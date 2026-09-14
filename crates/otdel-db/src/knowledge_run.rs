//! The run row: queueing it, starting it, closing it, and correcting it when a worker
//! disappears.
//!
//! Split from [`crate::knowledge`] because the two answer different questions. That file
//! stores *what the material says*; this one stores *what the pass over it did* — the
//! page account, the requirement verdict, the cost, and the status that ties them
//! together.
//!
//! One property is implemented here and nowhere else: **the status and the account are
//! written by one statement.** `0009_product_passports.sql` forbids the pair
//! (`completed`, `pages_deferred > 0`), and splitting the write in two would create a
//! transaction in which a run claims to have finished a material it has pages left over
//! from. That pair is exactly what the audited run reported.

use chrono::{DateTime, Utc};
use otdel_core::knowledge::{KnowledgeRun, KnowledgeRunStatus};
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::knowledge_read;
use crate::tenancy::ScopedTx;

use crate::knowledge::RunOutcome;

/// Create or reset the run row of a material, in `queued`.
///
/// Called when the work is enqueued (by the owner or by the extraction worker), so the
/// interface can show "материал поставлен в очередь на разбор" instead of nothing.
pub async fn enqueue_run(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    material_id: Uuid,
    prompt_profile: &str,
) -> DbResult<KnowledgeRun> {
    upsert_run(
        tx,
        partner_id,
        material_id,
        prompt_profile,
        KnowledgeRunStatus::Queued,
    )
    .await
}

/// Mark the run as running and clear the previous counters.
pub async fn start_run(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    material_id: Uuid,
    prompt_profile: &str,
) -> DbResult<KnowledgeRun> {
    upsert_run(
        tx,
        partner_id,
        material_id,
        prompt_profile,
        KnowledgeRunStatus::Running,
    )
    .await
}

async fn upsert_run(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    material_id: Uuid,
    prompt_profile: &str,
    status: KnowledgeRunStatus,
) -> DbResult<KnowledgeRun> {
    let bureau_id = tx.bureau_id();
    let started_at: Option<DateTime<Utc>> = match status {
        KnowledgeRunStatus::Running => Some(Utc::now()),
        _ => None,
    };

    sqlx::query(
        "INSERT INTO otdel.knowledge_runs \
             (bureau_id, partner_id, material_id, status, prompt_profile, started_at) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (material_id) DO UPDATE \
            SET status = EXCLUDED.status, \
                prompt_profile = EXCLUDED.prompt_profile, \
                started_at = coalesce(EXCLUDED.started_at, otdel.knowledge_runs.started_at), \
                finished_at = NULL, \
                diagnostic = NULL, \
                rejections = '{}', \
                pages_considered = 0, \
                pages_skipped = 0, \
                requests_made = 0, \
                input_chars = 0, \
                facts_accepted = 0, \
                facts_rejected = 0, \
                pages_total = 0, \
                pages_offered = 0, \
                pages_processed = 0, \
                pages_deferred = 0, \
                pages_unreadable = 0, \
                coverage_state = 'unknown', \
                coverage_notes = '{}', \
                requirements_state = 'unknown', \
                requirements_missing = '{}', \
                prompt_tokens = NULL, \
                completion_tokens = NULL, \
                cost_micro_usd = NULL, \
                updated_at = now()",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .bind(material_id)
    .bind(status.as_str())
    .bind(prompt_profile)
    .bind(started_at)
    .execute(tx.conn())
    .await?;

    // R05: the run row survives a re-queue, so its previous account has to go with the
    // counters that were just zeroed. Leaving it would put a run reporting
    // `coverage_state = 'unknown'` beside a page-by-page report of the *previous* pass,
    // and leave yesterday's «в материале нет терминов» attached to a run that has not
    // said anything yet — which is exactly the stale-clearance this package removes.
    //
    // The candidates themselves are deliberately *not* cleared here: a re-run that fails
    // must leave the previous draft in place (`replace_draft` owns that), and only the
    // run's own claims about itself are reset.
    clear_run_account(tx, material_id).await?;

    // Read it back through the same query the API uses, so a run always carries the
    // live counts and the material's name rather than a second, divergent shape.
    knowledge_read::find_run(tx, partner_id, material_id)
        .await?
        .ok_or_else(|| DbError::Decode("the run row disappeared after it was written".to_owned()))
}

/// Drop everything a previous pass over this material claimed about *itself*.
///
/// Three tables, and all three hang off the run rather than off a product: the page
/// account, the unsettled readings and the explicit absences. A re-queued run has made
/// none of those statements yet, and the counters on the row now say so.
async fn clear_run_account(tx: &mut ScopedTx, material_id: Uuid) -> DbResult<()> {
    let bureau_id = tx.bureau_id();
    for table in [
        "otdel.knowledge_page_coverage",
        "otdel.knowledge_uncertainties",
        "otdel.knowledge_declarations",
    ] {
        sqlx::query(&format!(
            "DELETE FROM {table} WHERE bureau_id = $1 AND material_id = $2"
        ))
        .bind(bureau_id)
        .bind(material_id)
        .execute(tx.conn())
        .await?;
    }
    Ok(())
}

/// Record how a run ended, including a run that produced nothing.
///
/// The status and the page account are written by one statement. That is not a
/// convenience: `0009_product_passports.sql` forbids the pair (`completed`,
/// `pages_deferred > 0`), and splitting the write in two would let a transaction exist in
/// which a run claims to have finished a material it has pages left over from.
pub async fn finish_run(tx: &mut ScopedTx, run_id: Uuid, outcome: &RunOutcome) -> DbResult<()> {
    let bureau_id = tx.bureau_id();
    let rejections: Vec<String> = outcome
        .rejections
        .iter()
        .map(|reason| reason.chars().take(500).collect())
        .take(100)
        .collect();
    let coverage = &outcome.coverage;
    let notes: Vec<String> = bounded_lines(&coverage.notes, MAX_COVERAGE_NOTES);
    let missing: Vec<String> = bounded_lines(&coverage.requirements_missing, MAX_MISSING_LINES);

    sqlx::query(
        "UPDATE otdel.knowledge_runs \
            SET status = $3, \
                provider = $4, \
                model = $5, \
                pages_considered = $6, \
                pages_skipped = $7, \
                requests_made = $8, \
                input_chars = $9, \
                facts_accepted = $10, \
                facts_rejected = $11, \
                rejections = $12, \
                diagnostic = $13, \
                pages_total = $14, \
                pages_offered = $15, \
                pages_processed = $16, \
                pages_deferred = $17, \
                pages_unreadable = $18, \
                coverage_state = $19, \
                coverage_notes = $20, \
                requirements_state = $21, \
                requirements_missing = $22, \
                prompt_tokens = $23, \
                completion_tokens = $24, \
                cost_micro_usd = $25, \
                finished_at = now(), \
                updated_at = now() \
          WHERE bureau_id = $1 AND id = $2",
    )
    .bind(bureau_id)
    .bind(run_id)
    .bind(outcome.status.as_str())
    .bind(outcome.provider.as_deref())
    .bind(outcome.model.as_deref())
    .bind(outcome.pages_considered)
    .bind(outcome.pages_skipped)
    .bind(outcome.requests_made)
    .bind(outcome.input_chars)
    .bind(outcome.counts.facts)
    .bind(outcome.facts_rejected)
    .bind(&rejections)
    .bind(outcome.diagnostic.as_deref())
    .bind(coverage.pages_total)
    .bind(coverage.pages_offered)
    .bind(coverage.pages_processed)
    .bind(coverage.pages_deferred)
    .bind(coverage.pages_unreadable)
    .bind(coverage.state.as_str())
    .bind(&notes)
    .bind(coverage.requirements.as_str())
    .bind(&missing)
    .bind(coverage.prompt_tokens)
    .bind(coverage.completion_tokens)
    .bind(coverage.cost_micro_usd)
    .execute(tx.conn())
    .await?;

    Ok(())
}

/// Matches the column limits in `0009_product_passports.sql`. A run record is read by a
/// person; the reasons repeat, and a thousand of them would bury the first one.
const MAX_COVERAGE_NOTES: usize = 100;
const MAX_MISSING_LINES: usize = 50;
const MAX_LINE_CHARS: usize = 500;

fn bounded_lines(lines: &[String], max: usize) -> Vec<String> {
    lines
        .iter()
        .map(|line| line.chars().take(MAX_LINE_CHARS).collect())
        .take(max)
        .collect()
}

/// Settle runs that say `running` but have no job behind them any more.
///
/// A worker killed mid-run leaves the run row claiming to be in progress; the queue
/// recovers the *job* (lease reclaim, attempt limit), but nothing would ever correct
/// the run, and the interface would show a spinner forever while the owner's own
/// "разобрать" button stays hidden behind "уже выполняется". Returns how many rows
/// were corrected.
pub async fn reclaim_stalled_runs(tx: &mut ScopedTx) -> DbResult<u64> {
    let bureau_id = tx.bureau_id();
    let result = sqlx::query(
        "UPDATE otdel.knowledge_runs r \
            SET status = 'failed', \
                diagnostic = coalesce(r.diagnostic, \
                    'разбор прерван: обработчик остановился, задание больше не выполняется'), \
                finished_at = now(), \
                updated_at = now() \
          WHERE r.bureau_id = $1 AND r.status = 'running' \
            AND NOT EXISTS ( \
                SELECT 1 FROM otdel.jobs j \
                 WHERE j.bureau_id = r.bureau_id \
                   AND j.material_id = r.material_id \
                   AND j.kind = 'understand_material' \
                   AND j.status IN ('queued', 'running') \
            )",
    )
    .bind(bureau_id)
    .execute(tx.conn())
    .await?;

    Ok(result.rows_affected())
}
