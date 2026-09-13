//! Writing phase 1D: plans, money, the source journal and the conclusions.
//!
//! Four properties are implemented here and nowhere else.
//!
//! **Money is reserved before a call and settled after it.** [`reserve`] takes a row lock
//! on the bureau's budget *and* on the plan, in that fixed order, checks both ceilings and
//! only then writes. Two workers reserving at the same moment therefore queue behind one
//! lock instead of both reading "enough left" and both spending it
//! (`docs/block-01-spec.md` §10: "Конкурентные задачи не обходят общий лимит").
//!
//! **An unknown outcome is not free.** [`settle`] with [`SpendState::Unknown`] moves the
//! amount from reserved to spent *and* into the `unknown_micros` bucket, so a request
//! whose answer never arrived is counted against the budget and stays visible for
//! reconciliation. Treating it as "nothing happened" is how a budget stops being one.
//!
//! **A pass replaces its plan's journal.** [`start_pass`] deletes the queries, sources and
//! findings of the previous pass in the same transaction that marks the plan running, so
//! the counters always describe the rows that exist. The *ledger* is never cleared: money
//! that was spent does not stop having been spent.
//!
//! **A conclusion is checked by the database, not only by the caller.** An evidence row
//! names the plan and the source together, and the composite foreign keys of
//! `0005_research.sql` make a page of another plan — or another bureau — impossible to
//! reference. A finding with no evidence at all fails at commit through a deferred
//! constraint trigger.
//!
//! Reading it back lives in [`crate::research_read`].

use chrono::{DateTime, Utc};
use otdel_core::research::{
    QueryOutcome, ResearchPlan, ResearchPlanStatus, SourceStatus, SpendKind, SpendState,
};
use sqlx::Row;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::research_read;
use crate::tenancy::ScopedTx;

/// A plan the owner has approved, ready to be stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPlan {
    pub partner_id: Uuid,
    pub material_id: Uuid,
    pub question_id: Uuid,
    /// Copied verbatim at approval time: this is what gets researched, whatever 1C does
    /// to its own row afterwards.
    pub question_text: String,
    pub topic: Option<String>,
    pub prompt_profile: String,
    pub budget_micros: i64,
    pub max_passes: i32,
}

/// How a pass ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanOutcome {
    pub status: ResearchPlanStatus,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub queries_made: i32,
    pub results_seen: i32,
    pub sources_fetched: i32,
    pub sources_skipped: i32,
    pub bytes_fetched: i64,
    pub findings_rejected: i32,
    pub duration_ms: Option<i64>,
    pub rejections: Vec<String>,
    pub diagnostic: Option<String>,
}

/// One discovered URL, before anything has been downloaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSource {
    pub query_id: Option<Uuid>,
    pub url: String,
    pub url_hash: String,
    pub host: String,
    pub title: Option<String>,
    /// The search engine's summary. Recorded, never quotable.
    pub snippet: Option<String>,
    /// `Discovered` for a result nobody has opened yet, or a `skipped_*` value when the
    /// reason is already known (the host is not allowed, a limit was reached).
    pub status: SourceStatus,
    pub diagnostic: Option<String>,
}

/// What became of a source once the fetcher had its turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceOutcome {
    pub status: SourceStatus,
    pub http_status: Option<i32>,
    pub content_type: Option<String>,
    pub content_bytes: Option<i64>,
    pub content_hash: Option<String>,
    /// The stored snapshot. Present exactly when `status` is `Fetched`.
    pub text_content: Option<String>,
    pub license: Option<String>,
    pub license_note: Option<String>,
    pub retrieved_at: Option<DateTime<Utc>>,
    pub published_at: Option<DateTime<Utc>>,
    pub cost_micros: i64,
    pub diagnostic: Option<String>,
}

/// A fragment of one fetched source, already located there by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFindingEvidence {
    pub source_id: Uuid,
    pub quote: String,
    pub char_start: i32,
    pub char_end: i32,
}

/// A validated conclusion about the industry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFinding {
    pub topic: String,
    pub attribute: String,
    pub value_text: String,
    pub unit: Option<String>,
    pub conditions: Option<String>,
    pub model_context: Option<String>,
    /// Never empty — the database refuses a finding without it.
    pub evidence: Vec<NewFindingEvidence>,
}

/// Money held for a call that has not happened yet.
///
/// Deliberately not `Copy`: it is a claim on a budget, and a value that can be settled
/// twice by accident is not one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reservation {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub kind: SpendKind,
    pub amount_micros: i64,
}

/// What [`reserve`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReserveOutcome {
    Granted(Reservation),
    /// Not enough money, with the reason naming which ceiling stopped it. Nothing was
    /// written, so the caller may commit the transaction either way.
    Refused(String),
}

// --- plans ---------------------------------------------------------------------------

/// Create the plan for one approved question, or return the one that already exists.
///
/// Idempotent by `question_id`: approving twice is one plan, and a second click cannot
/// research the same question in parallel with itself.
pub async fn create_plan(tx: &mut ScopedTx, plan: &NewPlan) -> DbResult<ResearchPlan> {
    let bureau_id = tx.bureau_id();

    sqlx::query(
        "INSERT INTO otdel.research_plans \
             (bureau_id, partner_id, material_id, question_id, question_text, topic, \
              prompt_profile, budget_micros, max_passes) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (question_id) DO NOTHING",
    )
    .bind(bureau_id)
    .bind(plan.partner_id)
    .bind(plan.material_id)
    .bind(plan.question_id)
    .bind(&plan.question_text)
    .bind(plan.topic.as_deref())
    .bind(&plan.prompt_profile)
    .bind(plan.budget_micros)
    .bind(plan.max_passes)
    .execute(tx.conn())
    .await?;

    research_read::find_plan_by_question(tx, plan.question_id)
        .await?
        .ok_or_else(|| {
            DbError::Decode("the research plan disappeared after it was written".to_owned())
        })
}

/// Put a settled plan back into the queue.
///
/// The pass counter is *not* reset — it records how many times this question has really
/// been researched, and `max_passes` still bounds it (`docs/block-01-plan.md`, 1D §3:
/// "предел проходов"). A plan that is already queued or running is left exactly as it
/// is, and `false` comes back: re-arming it would hand the same question to a second
/// worker while the first still holds the lease.
pub async fn requeue_plan(tx: &mut ScopedTx, plan_id: Uuid) -> DbResult<bool> {
    let bureau_id = tx.bureau_id();
    let result = sqlx::query(
        "UPDATE otdel.research_plans \
            SET status = 'queued', \
                diagnostic = NULL, \
                cancel_requested = false, \
                finished_at = NULL, \
                updated_at = now() \
          WHERE bureau_id = $1 AND id = $2 \
            AND status NOT IN ('queued', 'running') \
            AND passes < max_passes",
    )
    .bind(bureau_id)
    .bind(plan_id)
    .execute(tx.conn())
    .await?;

    Ok(result.rows_affected() == 1)
}

/// Mark the plan running, count the pass, and clear the journal of the previous one.
///
/// Returns `false` when the plan has already used every pass it is allowed — the worker
/// then settles it with that reason instead of researching again.
pub async fn start_pass(tx: &mut ScopedTx, plan_id: Uuid) -> DbResult<bool> {
    let bureau_id = tx.bureau_id();

    let updated = sqlx::query(
        "UPDATE otdel.research_plans \
            SET status = 'running', \
                passes = passes + 1, \
                started_at = now(), \
                finished_at = NULL, \
                diagnostic = NULL, \
                rejections = '{}', \
                queries_made = 0, \
                results_seen = 0, \
                sources_fetched = 0, \
                sources_skipped = 0, \
                bytes_fetched = 0, \
                findings_rejected = 0, \
                duration_ms = NULL, \
                updated_at = now() \
          WHERE bureau_id = $1 AND id = $2 AND passes < max_passes",
    )
    .bind(bureau_id)
    .bind(plan_id)
    .execute(tx.conn())
    .await?;

    if updated.rows_affected() != 1 {
        return Ok(false);
    }

    // The previous pass's journal goes with it, so the counters above describe exactly
    // the rows that exist. Findings cascade to their evidence; sources are deleted last
    // because evidence points at them.
    clear_pass_results(tx, plan_id).await?;
    Ok(true)
}

/// Remove the queries, sources and conclusions of a plan's previous pass.
pub async fn clear_pass_results(tx: &mut ScopedTx, plan_id: Uuid) -> DbResult<()> {
    let bureau_id = tx.bureau_id();
    for table in [
        "otdel.research_findings",
        "otdel.research_sources",
        "otdel.research_queries",
    ] {
        sqlx::query(&format!(
            "DELETE FROM {table} WHERE bureau_id = $1 AND plan_id = $2"
        ))
        .bind(bureau_id)
        .bind(plan_id)
        .execute(tx.conn())
        .await?;
    }
    Ok(())
}

/// Record how a pass ended, including one that produced nothing.
pub async fn finish_plan(tx: &mut ScopedTx, plan_id: Uuid, outcome: &PlanOutcome) -> DbResult<()> {
    let bureau_id = tx.bureau_id();
    let rejections: Vec<String> = outcome
        .rejections
        .iter()
        .map(|reason| reason.chars().take(500).collect())
        .take(100)
        .collect();

    sqlx::query(
        "UPDATE otdel.research_plans \
            SET status = $3, \
                provider = coalesce($4, provider), \
                model = coalesce($5, model), \
                queries_made = $6, \
                results_seen = $7, \
                sources_fetched = $8, \
                sources_skipped = $9, \
                bytes_fetched = $10, \
                findings_rejected = $11, \
                duration_ms = $12, \
                rejections = $13, \
                diagnostic = $14, \
                finished_at = now(), \
                updated_at = now() \
          WHERE bureau_id = $1 AND id = $2",
    )
    .bind(bureau_id)
    .bind(plan_id)
    .bind(outcome.status.as_str())
    .bind(outcome.provider.as_deref())
    .bind(outcome.model.as_deref())
    .bind(outcome.queries_made)
    .bind(outcome.results_seen)
    .bind(outcome.sources_fetched)
    .bind(outcome.sources_skipped)
    .bind(outcome.bytes_fetched)
    .bind(outcome.findings_rejected)
    .bind(outcome.duration_ms)
    .bind(&rejections)
    .bind(outcome.diagnostic.as_deref())
    .execute(tx.conn())
    .await?;

    Ok(())
}

/// Ask a running plan to stop.
///
/// The flag is all this does. The worker reads it before every chargeable step and
/// settles the plan as `cancelled` there — stopping a run from outside by killing it
/// would leave money reserved and a source half-written.
pub async fn request_cancel(tx: &mut ScopedTx, plan_id: Uuid) -> DbResult<bool> {
    let bureau_id = tx.bureau_id();
    let result = sqlx::query(
        "UPDATE otdel.research_plans \
            SET cancel_requested = true, updated_at = now() \
          WHERE bureau_id = $1 AND id = $2 AND status IN ('queued', 'running')",
    )
    .bind(bureau_id)
    .bind(plan_id)
    .execute(tx.conn())
    .await?;

    Ok(result.rows_affected() == 1)
}

/// Has the owner asked this plan to stop?
pub async fn cancel_requested(tx: &mut ScopedTx, plan_id: Uuid) -> DbResult<bool> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(
        "SELECT cancel_requested FROM otdel.research_plans WHERE bureau_id = $1 AND id = $2",
    )
    .bind(bureau_id)
    .bind(plan_id)
    .fetch_optional(tx.conn())
    .await?;

    Ok(row
        .map(|row| row.try_get::<bool, _>("cancel_requested"))
        .transpose()?
        .unwrap_or(false))
}

/// Settle plans that say `running` but have no job behind them any more.
///
/// A worker killed mid-pass leaves the plan claiming to be in progress; the queue
/// recovers the *job*, but nothing would ever correct the plan, and the interface would
/// show a spinner over work that is not happening while the owner's own buttons stay
/// disabled. Returns how many rows were corrected.
pub async fn reclaim_stalled_plans(tx: &mut ScopedTx) -> DbResult<u64> {
    let bureau_id = tx.bureau_id();
    let result = sqlx::query(
        "UPDATE otdel.research_plans p \
            SET status = 'failed', \
                diagnostic = coalesce(p.diagnostic, \
                    'исследование прервано: обработчик остановился, задание больше не выполняется'), \
                finished_at = now(), \
                updated_at = now() \
          WHERE p.bureau_id = $1 AND p.status = 'running' \
            AND NOT EXISTS ( \
                SELECT 1 FROM otdel.jobs j \
                 WHERE j.bureau_id = p.bureau_id \
                   AND j.research_plan_id = p.id \
                   AND j.status IN ('queued', 'running') \
            )",
    )
    .bind(bureau_id)
    .execute(tx.conn())
    .await?;

    Ok(result.rows_affected())
}

// --- money ---------------------------------------------------------------------------

/// Make sure this bureau has a budget row, and return nothing but success.
///
/// The row holds balances only; the ceiling is configuration
/// (`OTDEL_RESEARCH_BUDGET_MICROS`), passed into every statement that needs it, so
/// raising the limit takes effect at once and no stored copy can disagree with it.
pub async fn ensure_budget(tx: &mut ScopedTx) -> DbResult<()> {
    let bureau_id = tx.bureau_id();
    sqlx::query(
        "INSERT INTO otdel.research_budgets (bureau_id) VALUES ($1) \
         ON CONFLICT (bureau_id) DO NOTHING",
    )
    .bind(bureau_id)
    .execute(tx.conn())
    .await?;
    Ok(())
}

/// Hold `amount_micros` against both ceilings, or say why not.
///
/// The two rows are locked in a fixed order — the bureau's budget first, then the plan —
/// so concurrent reservations queue instead of deadlocking. Nothing is written on a
/// refusal, so the caller may commit either way.
pub async fn reserve(
    tx: &mut ScopedTx,
    plan_id: Uuid,
    kind: SpendKind,
    amount_micros: i64,
    bureau_limit_micros: i64,
) -> DbResult<ReserveOutcome> {
    let bureau_id = tx.bureau_id();
    ensure_budget(tx).await?;

    // `FOR UPDATE` is what serialises two workers: the second one blocks here until the
    // first has committed its reservation, and then sees the new balance.
    let budget = sqlx::query(
        "SELECT reserved_micros, spent_micros FROM otdel.research_budgets \
          WHERE bureau_id = $1 FOR UPDATE",
    )
    .bind(bureau_id)
    .fetch_one(tx.conn())
    .await?;
    let bureau_available = bureau_limit_micros
        .saturating_sub(budget.try_get::<i64, _>("spent_micros")?)
        .saturating_sub(budget.try_get::<i64, _>("reserved_micros")?)
        .max(0);

    let plan = sqlx::query(
        "SELECT budget_micros, reserved_micros, spent_micros FROM otdel.research_plans \
          WHERE bureau_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(bureau_id)
    .bind(plan_id)
    .fetch_optional(tx.conn())
    .await?
    .ok_or_else(|| DbError::Decode("research plan not found while reserving".to_owned()))?;
    let plan_available = plan
        .try_get::<i64, _>("budget_micros")?
        .saturating_sub(plan.try_get::<i64, _>("spent_micros")?)
        .saturating_sub(plan.try_get::<i64, _>("reserved_micros")?)
        .max(0);

    if amount_micros > bureau_available {
        return Ok(ReserveOutcome::Refused(format!(
            "бюджет бюро на исследования исчерпан: доступно {bureau_available}, \
             требуется {amount_micros} (в миллионных долях)"
        )));
    }
    if amount_micros > plan_available {
        return Ok(ReserveOutcome::Refused(format!(
            "бюджет этого исследования исчерпан: доступно {plan_available}, \
             требуется {amount_micros} (в миллионных долях)"
        )));
    }

    sqlx::query(
        "UPDATE otdel.research_budgets \
            SET reserved_micros = reserved_micros + $2, updated_at = now() \
          WHERE bureau_id = $1",
    )
    .bind(bureau_id)
    .bind(amount_micros)
    .execute(tx.conn())
    .await?;

    sqlx::query(
        "UPDATE otdel.research_plans \
            SET reserved_micros = reserved_micros + $3, updated_at = now() \
          WHERE bureau_id = $1 AND id = $2",
    )
    .bind(bureau_id)
    .bind(plan_id)
    .bind(amount_micros)
    .execute(tx.conn())
    .await?;

    let row = sqlx::query(
        "INSERT INTO otdel.research_spend (bureau_id, plan_id, kind, state, amount_micros) \
         VALUES ($1, $2, $3, 'reserved', $4) RETURNING id",
    )
    .bind(bureau_id)
    .bind(plan_id)
    .bind(kind.as_str())
    .bind(amount_micros)
    .fetch_one(tx.conn())
    .await?;

    Ok(ReserveOutcome::Granted(Reservation {
        id: row.try_get("id")?,
        plan_id,
        kind,
        amount_micros,
    }))
}

/// Close a reservation.
///
/// * [`SpendState::Settled`] — the call happened and was paid for;
/// * [`SpendState::Released`] — the call never happened, so the money goes back;
/// * [`SpendState::Unknown`] — the request left the machine and no answer came. The
///   amount is counted as **spent** and also recorded in `unknown_micros`, because the
///   provider may well have billed it and pretending otherwise would silently raise the
///   budget.
pub async fn settle(
    tx: &mut ScopedTx,
    reservation: &Reservation,
    state: SpendState,
    note: Option<&str>,
) -> DbResult<()> {
    settle_amount(tx, reservation, reservation.amount_micros, state, note).await
}

/// Close a reservation at what the call really cost.
///
/// One reservation can cover several calls whose number is not known in advance — the
/// model requests of one interpretation phase are reserved at their worst case, because a
/// reservation that guessed low would be a ceiling that does not hold. `actual_micros` is
/// what was really used; the difference goes back to the budget, and the ledger row is
/// rewritten to the real amount so it explains the balance rather than the intention.
///
/// **An amount above the reservation is recorded, not trimmed.** A provider that reports
/// its own cost is stating an invoice, and a search that turned out to cost more than the
/// forecast has already cost it — clamping the number would make the ledger disagree with
/// the account it is supposed to track, and would hide exactly the case the owner needs to
/// see. The ceiling still does its job: the next [`reserve`] sees the larger balance and
/// refuses.
pub async fn settle_amount(
    tx: &mut ScopedTx,
    reservation: &Reservation,
    actual_micros: i64,
    state: SpendState,
    note: Option<&str>,
) -> DbResult<()> {
    let bureau_id = tx.bureau_id();
    let actual = actual_micros.max(0);

    // Only a reservation that is still open can be closed: a second settlement of the
    // same row would move the money twice.
    let closed = sqlx::query(
        "UPDATE otdel.research_spend \
            SET state = $3, note = $4, amount_micros = $5, updated_at = now() \
          WHERE bureau_id = $1 AND id = $2 AND state = 'reserved'",
    )
    .bind(bureau_id)
    .bind(reservation.id)
    .bind(state.as_str())
    .bind(note.map(|note| note.chars().take(500).collect::<String>()))
    // The row records what was really used, not what was held for it.
    .bind(match state {
        SpendState::Released | SpendState::Reserved => 0,
        _ => actual,
    })
    .execute(tx.conn())
    .await?;
    if closed.rows_affected() != 1 {
        return Ok(());
    }

    let (spent, unknown) = match state {
        SpendState::Settled => (actual, 0),
        SpendState::Unknown => (actual, actual),
        // `Reserved` cannot reach here (the UPDATE above changed the state), and
        // `Released` gives everything back.
        SpendState::Released | SpendState::Reserved => (0, 0),
    };

    sqlx::query(
        "UPDATE otdel.research_budgets \
            SET reserved_micros = GREATEST(reserved_micros - $2, 0), \
                spent_micros = spent_micros + $3, \
                unknown_micros = unknown_micros + $4, \
                updated_at = now() \
          WHERE bureau_id = $1",
    )
    .bind(bureau_id)
    .bind(reservation.amount_micros)
    .bind(spent)
    .bind(unknown)
    .execute(tx.conn())
    .await?;

    sqlx::query(
        "UPDATE otdel.research_plans \
            SET reserved_micros = GREATEST(reserved_micros - $3, 0), \
                spent_micros = spent_micros + $4, \
                updated_at = now() \
          WHERE bureau_id = $1 AND id = $2",
    )
    .bind(bureau_id)
    .bind(reservation.plan_id)
    .bind(reservation.amount_micros)
    .bind(spent)
    .execute(tx.conn())
    .await?;

    Ok(())
}

/// Give back reservations of plans that are no longer running.
///
/// A worker killed between reserving and settling would otherwise hold that money for
/// ever, and the bureau's budget would shrink every time a process died. The call itself
/// is assumed not to have happened: the plan never recorded a result for it, and a
/// reservation is not evidence of a request. Returns how many were released.
pub async fn release_orphan_reservations(tx: &mut ScopedTx) -> DbResult<u64> {
    let bureau_id = tx.bureau_id();

    let rows = sqlx::query(
        "UPDATE otdel.research_spend s \
            SET state = 'released', \
                note = coalesce(s.note, 'резерв освобождён: запуск исследования прерван'), \
                updated_at = now() \
          FROM otdel.research_plans p \
         WHERE s.bureau_id = $1 AND s.state = 'reserved' \
           AND p.bureau_id = s.bureau_id AND p.id = s.plan_id \
           AND p.status NOT IN ('queued', 'running') \
        RETURNING s.plan_id, s.amount_micros",
    )
    .bind(bureau_id)
    .fetch_all(tx.conn())
    .await?;

    if rows.is_empty() {
        return Ok(0);
    }

    let mut total: i64 = 0;
    for row in &rows {
        let plan_id: Uuid = row.try_get("plan_id")?;
        let amount: i64 = row.try_get("amount_micros")?;
        total = total.saturating_add(amount);

        sqlx::query(
            "UPDATE otdel.research_plans \
                SET reserved_micros = GREATEST(reserved_micros - $3, 0), updated_at = now() \
              WHERE bureau_id = $1 AND id = $2",
        )
        .bind(bureau_id)
        .bind(plan_id)
        .bind(amount)
        .execute(tx.conn())
        .await?;
    }

    sqlx::query(
        "UPDATE otdel.research_budgets \
            SET reserved_micros = GREATEST(reserved_micros - $2, 0), updated_at = now() \
          WHERE bureau_id = $1",
    )
    .bind(bureau_id)
    .bind(total)
    .execute(tx.conn())
    .await?;

    Ok(rows.len() as u64)
}

// --- the journal ----------------------------------------------------------------------

/// Record one search request — including one that was refused before it was sent.
#[allow(clippy::too_many_arguments)]
pub async fn record_query(
    tx: &mut ScopedTx,
    plan_id: Uuid,
    ordinal: i32,
    query_text: &str,
    provider: &str,
    results_count: i32,
    cost_micros: i64,
    outcome: QueryOutcome,
    diagnostic: Option<&str>,
) -> DbResult<Uuid> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(
        "INSERT INTO otdel.research_queries \
             (bureau_id, plan_id, ordinal, query_text, provider, results_count, cost_micros, \
              outcome, diagnostic) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(bureau_id)
    .bind(plan_id)
    .bind(ordinal)
    .bind(clip(query_text, 500))
    .bind(clip(provider, 100))
    .bind(results_count)
    .bind(cost_micros)
    .bind(outcome.as_str())
    .bind(diagnostic.map(|value| clip(value, 1000)))
    .fetch_one(tx.conn())
    .await?;

    Ok(row.try_get("id")?)
}

/// Record one discovered URL, or `None` when this plan has already seen it.
///
/// Deduplication is by the hash of the normalised URL inside one plan, so two queries
/// returning the same document produce one source — and one fetch.
pub async fn record_source(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    plan_id: Uuid,
    source: &NewSource,
) -> DbResult<Option<Uuid>> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(
        "INSERT INTO otdel.research_sources \
             (bureau_id, partner_id, plan_id, query_id, url, url_hash, host, title, snippet, \
              status, diagnostic) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
         ON CONFLICT (plan_id, url_hash) DO NOTHING \
         RETURNING id",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .bind(plan_id)
    .bind(source.query_id)
    .bind(clip(&source.url, 2_000))
    .bind(&source.url_hash)
    .bind(clip(&source.host, 253))
    .bind(source.title.as_deref().map(|value| clip(value, 300)))
    .bind(source.snippet.as_deref().map(|value| clip(value, 600)))
    .bind(source.status.as_str())
    .bind(source.diagnostic.as_deref().map(|value| clip(value, 1000)))
    .fetch_optional(tx.conn())
    .await?;

    row.map(|row| row.try_get::<Uuid, _>("id"))
        .transpose()
        .map_err(DbError::from)
}

/// Record what became of a source once the fetcher had its turn.
pub async fn finish_source(
    tx: &mut ScopedTx,
    source_id: Uuid,
    outcome: &SourceOutcome,
) -> DbResult<()> {
    let bureau_id = tx.bureau_id();

    // The schema requires text and a retrieval time exactly when the status is
    // `fetched`; clearing them here for every other outcome keeps a caller from writing
    // a half-state that the CHECK would reject at the end of a long transaction.
    let fetched = outcome.status == SourceStatus::Fetched;

    sqlx::query(
        "UPDATE otdel.research_sources \
            SET status = $3, \
                http_status = $4, \
                content_type = $5, \
                content_bytes = $6, \
                content_chars = $7, \
                content_hash = $8, \
                text_content = $9, \
                license = $10, \
                license_note = $11, \
                retrieved_at = $12, \
                published_at = $13, \
                cost_micros = $14, \
                diagnostic = $15 \
          WHERE bureau_id = $1 AND id = $2",
    )
    .bind(bureau_id)
    .bind(source_id)
    .bind(outcome.status.as_str())
    .bind(outcome.http_status)
    .bind(
        outcome
            .content_type
            .as_deref()
            .map(|value| clip(value, 200)),
    )
    .bind(outcome.content_bytes)
    .bind(
        outcome
            .text_content
            .as_deref()
            .filter(|_| fetched)
            .map(|text| i32::try_from(text.chars().count()).unwrap_or(i32::MAX)),
    )
    .bind(outcome.content_hash.as_deref())
    .bind(outcome.text_content.as_deref().filter(|_| fetched))
    .bind(outcome.license.as_deref().map(|value| clip(value, 300)))
    .bind(
        outcome
            .license_note
            .as_deref()
            .map(|value| clip(value, 300)),
    )
    .bind(outcome.retrieved_at.filter(|_| fetched))
    .bind(outcome.published_at)
    .bind(outcome.cost_micros)
    .bind(outcome.diagnostic.as_deref().map(|value| clip(value, 1000)))
    .execute(tx.conn())
    .await?;

    Ok(())
}

/// Store this pass's conclusions, replacing anything a previous call left behind.
///
/// Returns how many were written. Every evidence row names the plan as well as the
/// source, so the composite foreign key refuses a citation of another plan's page before
/// the application would have to notice.
pub async fn replace_findings(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    plan_id: Uuid,
    findings: &[NewFinding],
) -> DbResult<i32> {
    let bureau_id = tx.bureau_id();

    sqlx::query("DELETE FROM otdel.research_findings WHERE bureau_id = $1 AND plan_id = $2")
        .bind(bureau_id)
        .bind(plan_id)
        .execute(tx.conn())
        .await?;

    let mut stored = 0i32;
    for finding in findings {
        // Defence in depth next to the deferred trigger: a caller that lost its evidence
        // between validation and storage must not write the conclusion.
        if finding.evidence.is_empty() {
            return Err(DbError::Decode(
                "refusing to store a research finding without an external source".to_owned(),
            ));
        }

        let row = sqlx::query(
            "INSERT INTO otdel.research_findings \
                 (bureau_id, partner_id, plan_id, topic, attribute, value_text, unit, \
                  conditions, model_context) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
        )
        .bind(bureau_id)
        .bind(partner_id)
        .bind(plan_id)
        .bind(clip(&finding.topic, 200))
        .bind(clip(&finding.attribute, 200))
        .bind(clip(&finding.value_text, 200))
        .bind(finding.unit.as_deref().map(|value| clip(value, 40)))
        .bind(finding.conditions.as_deref().map(|value| clip(value, 1000)))
        .bind(
            finding
                .model_context
                .as_deref()
                .map(|value| clip(value, 1000)),
        )
        .fetch_one(tx.conn())
        .await?;
        let finding_id: Uuid = row.try_get("id")?;

        for evidence in &finding.evidence {
            sqlx::query(
                "INSERT INTO otdel.research_evidence \
                     (bureau_id, plan_id, finding_id, source_id, quote, char_start, char_end) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(bureau_id)
            .bind(plan_id)
            .bind(finding_id)
            .bind(evidence.source_id)
            .bind(clip(&evidence.quote, 600))
            .bind(evidence.char_start)
            .bind(evidence.char_end)
            .execute(tx.conn())
            .await?;
        }

        stored += 1;
    }

    Ok(stored)
}

fn clip(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_clipped_on_a_character_boundary_not_a_byte_one() {
        assert_eq!(clip("абвгд", 3), "абв");
        assert_eq!(clip("abc", 10), "abc");
        assert_eq!(clip("", 10), "");
        // A multi-byte string cut to its character limit stays valid UTF-8.
        assert_eq!(clip(&"я".repeat(500), 200).chars().count(), 200);
    }

    #[test]
    fn a_reservation_carries_everything_needed_to_settle_it_exactly_once() {
        let reservation = Reservation {
            id: Uuid::from_u128(1),
            plan_id: Uuid::from_u128(2),
            kind: SpendKind::Search,
            amount_micros: 5_000,
        };
        // Not `Copy`: settling it is a move-shaped operation at the call site, so the
        // same claim is not accidentally released twice.
        let cloned = reservation.clone();
        assert_eq!(cloned, reservation);
        assert_eq!(reservation.kind.as_str(), "search");
    }
}
