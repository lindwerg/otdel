//! Reading phase 1D back, for the API.
//!
//! Every query is bounded to one partner *inside* a bureau-scoped transaction, so a
//! partner id guessed from elsewhere returns nothing rather than somebody else's
//! research. Findings always come back with their evidence attached — there is no call
//! here that returns a conclusion without the external fragment that supports it, because
//! a caller that forgot to fetch the evidence would render an unsourced claim about the
//! industry.
//!
//! The plan's counters come from the plan row (they describe one pass and are reset when
//! a new one starts); the *totals* that describe what exists right now — sources,
//! findings — are counted from the rows themselves, so they cannot claim more than is
//! stored.

use chrono::{DateTime, Utc};
use otdel_core::research::{
    ExternalEvidence, FindingScope, FindingStatus, IndustryQuestion, QueryOutcome, ResearchBudget,
    ResearchFinding, ResearchPlan, ResearchPlanStatus, ResearchQueryRecord, ResearchSource,
    ResearchSummary, SourceStatus,
};
use sqlx::postgres::PgRow;
use sqlx::Row;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::tenancy::ScopedTx;

/// The plan columns, plus the material's file name so a plan can be named without a
/// second request.
const PLAN_COLUMNS: &str = "p.*, \
     (SELECT m.filename FROM otdel.materials m \
       WHERE m.bureau_id = p.bureau_id AND m.id = p.material_id) AS material_filename";

fn plan_from_row(row: &PgRow) -> DbResult<ResearchPlan> {
    let status: String = row.try_get("status")?;
    let status = ResearchPlanStatus::parse(&status)
        .ok_or_else(|| DbError::Decode(format!("unknown research plan status `{status}`")))?;

    Ok(ResearchPlan {
        id: row.try_get("id")?,
        partner_id: row.try_get("partner_id")?,
        material_id: row.try_get("material_id")?,
        // A correlated subquery is typed as nullable even though the foreign key
        // guarantees the material exists; decoding defensively keeps one missing row from
        // failing the whole overview.
        material_filename: row
            .try_get::<Option<String>, _>("material_filename")?
            .unwrap_or_default(),
        question_id: row.try_get("question_id")?,
        question_text: row.try_get("question_text")?,
        topic: row.try_get("topic")?,
        status,
        provider: row.try_get("provider")?,
        model: row.try_get("model")?,
        prompt_profile: row.try_get("prompt_profile")?,
        passes: row.try_get("passes")?,
        max_passes: row.try_get("max_passes")?,
        budget_micros: row.try_get("budget_micros")?,
        reserved_micros: row.try_get("reserved_micros")?,
        spent_micros: row.try_get("spent_micros")?,
        queries_made: row.try_get("queries_made")?,
        results_seen: row.try_get("results_seen")?,
        sources_fetched: row.try_get("sources_fetched")?,
        sources_skipped: row.try_get("sources_skipped")?,
        bytes_fetched: row.try_get("bytes_fetched")?,
        // Filled from the stored rows by the caller, so it cannot claim more conclusions
        // than exist.
        findings_accepted: 0,
        findings_rejected: row.try_get("findings_rejected")?,
        duration_ms: row.try_get("duration_ms")?,
        rejections: row.try_get::<Vec<String>, _>("rejections")?,
        diagnostic: row.try_get("diagnostic")?,
        cancel_requested: row.try_get("cancel_requested")?,
        started_at: row.try_get::<Option<DateTime<Utc>>, _>("started_at")?,
        finished_at: row.try_get::<Option<DateTime<Utc>>, _>("finished_at")?,
        created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
        updated_at: row.try_get::<DateTime<Utc>, _>("updated_at")?,
    })
}

/// Fill in the counts that describe stored rows rather than the last pass.
async fn with_counts(tx: &mut ScopedTx, mut plan: ResearchPlan) -> DbResult<ResearchPlan> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(
        "SELECT (SELECT count(*) FROM otdel.research_findings f \
                  WHERE f.bureau_id = $1 AND f.plan_id = $2) AS findings",
    )
    .bind(bureau_id)
    .bind(plan.id)
    .fetch_one(tx.conn())
    .await?;
    plan.findings_accepted = count(&row, "findings")?;
    Ok(plan)
}

/// Plans of one partner, newest activity first.
pub async fn list_plans(tx: &mut ScopedTx, partner_id: Uuid) -> DbResult<Vec<ResearchPlan>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(&format!(
        "SELECT {PLAN_COLUMNS} FROM otdel.research_plans p \
          WHERE p.bureau_id = $1 AND p.partner_id = $2 \
          ORDER BY p.updated_at DESC, p.id"
    ))
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_all(tx.conn())
    .await?;

    let mut plans = Vec::with_capacity(rows.len());
    for row in &rows {
        let plan = plan_from_row(row)?;
        plans.push(with_counts(tx, plan).await?);
    }
    Ok(plans)
}

/// One plan of one partner.
pub async fn find_plan(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    plan_id: Uuid,
) -> DbResult<Option<ResearchPlan>> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(&format!(
        "SELECT {PLAN_COLUMNS} FROM otdel.research_plans p \
          WHERE p.bureau_id = $1 AND p.partner_id = $2 AND p.id = $3"
    ))
    .bind(bureau_id)
    .bind(partner_id)
    .bind(plan_id)
    .fetch_optional(tx.conn())
    .await?;

    match row {
        Some(row) => {
            let plan = plan_from_row(&row)?;
            Ok(Some(with_counts(tx, plan).await?))
        }
        None => Ok(None),
    }
}

/// The plan approved from one 1C question, if there is one.
pub async fn find_plan_by_question(
    tx: &mut ScopedTx,
    question_id: Uuid,
) -> DbResult<Option<ResearchPlan>> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(&format!(
        "SELECT {PLAN_COLUMNS} FROM otdel.research_plans p \
          WHERE p.bureau_id = $1 AND p.question_id = $2"
    ))
    .bind(bureau_id)
    .bind(question_id)
    .fetch_optional(tx.conn())
    .await?;

    match row {
        Some(row) => {
            let plan = plan_from_row(&row)?;
            Ok(Some(with_counts(tx, plan).await?))
        }
        None => Ok(None),
    }
}

/// A plan by id alone, for the worker, which already knows the job is its own.
pub async fn find_plan_unscoped(
    tx: &mut ScopedTx,
    plan_id: Uuid,
) -> DbResult<Option<ResearchPlan>> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(&format!(
        "SELECT {PLAN_COLUMNS} FROM otdel.research_plans p \
          WHERE p.bureau_id = $1 AND p.id = $2"
    ))
    .bind(bureau_id)
    .bind(plan_id)
    .fetch_optional(tx.conn())
    .await?;

    match row {
        Some(row) => {
            let plan = plan_from_row(&row)?;
            Ok(Some(with_counts(tx, plan).await?))
        }
        None => Ok(None),
    }
}

/// The plan a research job belongs to.
///
/// The column lives on the job row rather than in the shared `Job` type: phases 1A–1C
/// have no use for it, and widening their wire contract to carry a field only this phase
/// reads would be a change to a frozen document for no reason.
pub async fn plan_id_for_job(tx: &mut ScopedTx, job_id: Uuid) -> DbResult<Option<Uuid>> {
    let bureau_id = tx.bureau_id();
    let row =
        sqlx::query("SELECT research_plan_id FROM otdel.jobs WHERE bureau_id = $1 AND id = $2")
            .bind(bureau_id)
            .bind(job_id)
            .fetch_optional(tx.conn())
            .await?;

    Ok(row
        .map(|row| row.try_get::<Option<Uuid>, _>("research_plan_id"))
        .transpose()?
        .flatten())
}

/// The bureau's research money, with the ceilings from the configuration.
///
/// The limits are parameters rather than columns on purpose (see `0005_research.sql`):
/// the row records what happened, the configuration decides what is allowed.
pub async fn budget(
    tx: &mut ScopedTx,
    currency: &str,
    limit_micros: i64,
    plan_budget_micros: i64,
    cost_per_search_micros: i64,
    cost_per_fetch_micros: i64,
    cost_per_model_call_micros: i64,
) -> DbResult<ResearchBudget> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(
        "SELECT reserved_micros, spent_micros, unknown_micros, updated_at \
           FROM otdel.research_budgets WHERE bureau_id = $1",
    )
    .bind(bureau_id)
    .fetch_optional(tx.conn())
    .await?;

    let (reserved, spent, unknown, updated_at) = match row {
        Some(row) => (
            row.try_get::<i64, _>("reserved_micros")?,
            row.try_get::<i64, _>("spent_micros")?,
            row.try_get::<i64, _>("unknown_micros")?,
            row.try_get::<DateTime<Utc>, _>("updated_at")?,
        ),
        // No row yet means nothing has ever been reserved. Reporting zeros is the truth;
        // creating the row from a read would make a GET a write.
        None => (0, 0, 0, Utc::now()),
    };

    Ok(ResearchBudget {
        currency: currency.to_owned(),
        limit_micros,
        reserved_micros: reserved,
        spent_micros: spent,
        unknown_micros: unknown,
        available_micros: ResearchBudget::available(limit_micros, spent, reserved),
        plan_budget_micros,
        cost_per_search_micros,
        cost_per_fetch_micros,
        cost_per_model_call_micros,
        updated_at,
    })
}

/// The queries of one plan, in the order they were made.
pub async fn list_queries(tx: &mut ScopedTx, plan_id: Uuid) -> DbResult<Vec<ResearchQueryRecord>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT id, plan_id, ordinal, query_text, provider, results_count, cost_micros, \
                outcome, diagnostic, created_at \
           FROM otdel.research_queries \
          WHERE bureau_id = $1 AND plan_id = $2 \
          ORDER BY ordinal, created_at",
    )
    .bind(bureau_id)
    .bind(plan_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            let outcome: String = row.try_get("outcome")?;
            Ok(ResearchQueryRecord {
                id: row.try_get("id")?,
                plan_id: row.try_get("plan_id")?,
                ordinal: row.try_get("ordinal")?,
                query_text: row.try_get("query_text")?,
                provider: row.try_get("provider")?,
                results_count: row.try_get("results_count")?,
                cost_micros: row.try_get("cost_micros")?,
                outcome: QueryOutcome::parse(&outcome)
                    .ok_or_else(|| DbError::Decode(format!("unknown query outcome `{outcome}`")))?,
                diagnostic: row.try_get("diagnostic")?,
                created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
            })
        })
        .collect()
}

/// The source journal of one plan.
///
/// `text_content` is deliberately not selected: the snapshot can be tens of thousands of
/// characters, the interface shows quotations rather than whole pages, and an endpoint
/// that returned every page would turn a plan overview into a megabyte.
pub async fn list_sources(tx: &mut ScopedTx, plan_id: Uuid) -> DbResult<Vec<ResearchSource>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT id, plan_id, query_id, url, host, title, snippet, status, http_status, \
                content_type, content_bytes, content_chars, content_hash, license, \
                license_note, retrieved_at, published_at, cost_micros, diagnostic, created_at \
           FROM otdel.research_sources \
          WHERE bureau_id = $1 AND plan_id = $2 \
          ORDER BY created_at, id",
    )
    .bind(bureau_id)
    .bind(plan_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter().map(source_from_row).collect()
}

fn source_from_row(row: &PgRow) -> DbResult<ResearchSource> {
    let status: String = row.try_get("status")?;
    Ok(ResearchSource {
        id: row.try_get("id")?,
        plan_id: row.try_get("plan_id")?,
        query_id: row.try_get("query_id")?,
        url: row.try_get("url")?,
        host: row.try_get("host")?,
        title: row.try_get("title")?,
        snippet: row.try_get("snippet")?,
        status: SourceStatus::parse(&status)
            .ok_or_else(|| DbError::Decode(format!("unknown source status `{status}`")))?,
        http_status: row.try_get("http_status")?,
        content_type: row.try_get("content_type")?,
        content_bytes: row.try_get("content_bytes")?,
        content_chars: row.try_get("content_chars")?,
        content_hash: row.try_get("content_hash")?,
        license: row.try_get("license")?,
        license_note: row.try_get("license_note")?,
        retrieved_at: row.try_get::<Option<DateTime<Utc>>, _>("retrieved_at")?,
        published_at: row.try_get::<Option<DateTime<Utc>>, _>("published_at")?,
        cost_micros: row.try_get("cost_micros")?,
        diagnostic: row.try_get("diagnostic")?,
        created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
    })
}

/// The sources of one plan that really carry text, with their snapshots.
///
/// Only the worker uses this — it is what the quotation checker is run against.
pub async fn fetched_sources_with_text(
    tx: &mut ScopedTx,
    plan_id: Uuid,
) -> DbResult<Vec<(ResearchSource, String)>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT id, plan_id, query_id, url, host, title, snippet, status, http_status, \
                content_type, content_bytes, content_chars, content_hash, license, \
                license_note, retrieved_at, published_at, cost_micros, diagnostic, created_at, \
                text_content \
           FROM otdel.research_sources \
          WHERE bureau_id = $1 AND plan_id = $2 AND status = 'fetched' \
            AND btrim(coalesce(text_content, '')) <> '' \
          ORDER BY created_at, id",
    )
    .bind(bureau_id)
    .bind(plan_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            Ok((
                source_from_row(row)?,
                row.try_get::<String, _>("text_content")?,
            ))
        })
        .collect()
}

/// Discovered sources of one plan that nobody has tried to read yet.
pub async fn pending_sources(tx: &mut ScopedTx, plan_id: Uuid) -> DbResult<Vec<ResearchSource>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT id, plan_id, query_id, url, host, title, snippet, status, http_status, \
                content_type, content_bytes, content_chars, content_hash, license, \
                license_note, retrieved_at, published_at, cost_micros, diagnostic, created_at \
           FROM otdel.research_sources \
          WHERE bureau_id = $1 AND plan_id = $2 AND status = 'discovered' \
          ORDER BY created_at, id",
    )
    .bind(bureau_id)
    .bind(plan_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter().map(source_from_row).collect()
}

/// Conclusions of one partner, each with its external evidence.
pub async fn list_findings(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    plan_id: Option<Uuid>,
) -> DbResult<Vec<ResearchFinding>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT id, partner_id, plan_id, scope, status, topic, attribute, value_text, unit, \
                conditions, model_context, created_at \
           FROM otdel.research_findings \
          WHERE bureau_id = $1 AND partner_id = $2 \
            AND ($3::uuid IS NULL OR plan_id = $3) \
          ORDER BY topic, attribute, created_at",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .bind(plan_id)
    .fetch_all(tx.conn())
    .await?;

    let ids: Vec<Uuid> = rows
        .iter()
        .map(|row| row.try_get::<Uuid, _>("id"))
        .collect::<Result<_, _>>()?;
    let mut evidence = evidence_for(tx, &ids).await?;

    rows.iter()
        .map(|row| {
            let id: Uuid = row.try_get("id")?;
            let scope: String = row.try_get("scope")?;
            let status: String = row.try_get("status")?;
            Ok(ResearchFinding {
                id,
                partner_id: row.try_get("partner_id")?,
                plan_id: row.try_get("plan_id")?,
                scope: FindingScope::parse(&scope)
                    .ok_or_else(|| DbError::Decode(format!("unknown finding scope `{scope}`")))?,
                status: FindingStatus::parse(&status)
                    .ok_or_else(|| DbError::Decode(format!("unknown finding status `{status}`")))?,
                topic: row.try_get("topic")?,
                attribute: row.try_get("attribute")?,
                value_text: row.try_get("value_text")?,
                unit: row.try_get("unit")?,
                conditions: row.try_get("conditions")?,
                model_context: row.try_get("model_context")?,
                evidence: take_evidence(&mut evidence, id),
                created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
            })
        })
        .collect()
}

/// Evidence of many findings at once, joined to the source it points at so the interface
/// can show the URL, the date it was read and the licence beside the quotation.
async fn evidence_for(tx: &mut ScopedTx, ids: &[Uuid]) -> DbResult<Vec<(Uuid, ExternalEvidence)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let bureau_id = tx.bureau_id();

    let rows = sqlx::query(
        "SELECT e.id, e.finding_id, e.source_id, s.url, s.host, s.retrieved_at, \
                s.content_hash, s.license, e.quote, e.char_start, e.char_end \
           FROM otdel.research_evidence e \
           JOIN otdel.research_sources s \
             ON s.bureau_id = e.bureau_id AND s.id = e.source_id \
          WHERE e.bureau_id = $1 AND e.finding_id = ANY($2) \
          ORDER BY s.url, e.char_start, e.id",
    )
    .bind(bureau_id)
    .bind(ids)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            let finding_id: Uuid = row.try_get("finding_id")?;
            Ok((
                finding_id,
                ExternalEvidence {
                    id: row.try_get("id")?,
                    source_id: row.try_get("source_id")?,
                    url: row.try_get("url")?,
                    host: row.try_get("host")?,
                    retrieved_at: row.try_get::<Option<DateTime<Utc>>, _>("retrieved_at")?,
                    content_hash: row.try_get("content_hash")?,
                    license: row.try_get("license")?,
                    quote: row.try_get("quote")?,
                    char_start: row.try_get("char_start")?,
                    char_end: row.try_get("char_end")?,
                },
            ))
        })
        .collect()
}

fn take_evidence(
    evidence: &mut Vec<(Uuid, ExternalEvidence)>,
    finding_id: Uuid,
) -> Vec<ExternalEvidence> {
    let mut taken = Vec::new();
    evidence.retain(|(id, item)| {
        if *id == finding_id {
            taken.push(item.clone());
            false
        } else {
            true
        }
    });
    taken
}

/// The 1C questions addressed to industry research, with the plan approved from each.
///
/// This is the approval queue. A question with no plan is one the owner has not turned
/// into research yet, and nothing happens to it until they do — which is the whole of
/// «превращать только утверждённые вопросы» (`docs/block-01-plan.md`, 1D §1).
pub async fn industry_questions(
    tx: &mut ScopedTx,
    partner_id: Uuid,
) -> DbResult<Vec<IndustryQuestion>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT q.id, q.partner_id, q.material_id, q.gap_id, q.text_content, q.status, \
                q.created_at, \
                g.topic AS gap_topic, g.missing AS gap_missing, \
                m.filename AS material_filename, \
                (SELECT p.id FROM otdel.research_plans p \
                  WHERE p.bureau_id = q.bureau_id AND p.question_id = q.id) AS plan_id \
           FROM otdel.knowledge_questions q \
           JOIN otdel.knowledge_gaps g ON g.bureau_id = q.bureau_id AND g.id = q.gap_id \
           JOIN otdel.materials m ON m.bureau_id = q.bureau_id AND m.id = q.material_id \
          WHERE q.bureau_id = $1 AND q.partner_id = $2 AND q.audience = 'industry' \
          ORDER BY q.created_at DESC, q.id",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            Ok(IndustryQuestion {
                id: row.try_get("id")?,
                partner_id: row.try_get("partner_id")?,
                material_id: row.try_get("material_id")?,
                material_filename: row.try_get("material_filename")?,
                gap_id: row.try_get("gap_id")?,
                gap_topic: row.try_get("gap_topic")?,
                gap_missing: row.try_get("gap_missing")?,
                text: row.try_get("text_content")?,
                status: row.try_get("status")?,
                plan_id: row.try_get("plan_id")?,
                created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
            })
        })
        .collect()
}

/// One industry question by id, for the endpoint that approves it.
pub async fn find_industry_question(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    question_id: Uuid,
) -> DbResult<Option<IndustryQuestion>> {
    Ok(industry_questions(tx, partner_id)
        .await?
        .into_iter()
        .find(|question| question.id == question_id))
}

/// Partner-level roll-up for the research tab.
pub async fn summary(tx: &mut ScopedTx, partner_id: Uuid) -> DbResult<ResearchSummary> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(
        "SELECT \
            (SELECT count(*) FROM otdel.research_plans \
              WHERE bureau_id = $1 AND partner_id = $2) AS plans, \
            (SELECT count(*) FROM otdel.research_plans \
              WHERE bureau_id = $1 AND partner_id = $2 AND status IN ('queued', 'running')) AS active, \
            (SELECT count(*) FROM otdel.knowledge_questions q \
              WHERE q.bureau_id = $1 AND q.partner_id = $2 AND q.audience = 'industry' \
                AND NOT EXISTS (SELECT 1 FROM otdel.research_plans p \
                                 WHERE p.bureau_id = q.bureau_id AND p.question_id = q.id)) AS open_questions, \
            (SELECT count(*) FROM otdel.research_sources \
              WHERE bureau_id = $1 AND partner_id = $2 AND status = 'fetched') AS fetched, \
            (SELECT count(*) FROM otdel.research_sources \
              WHERE bureau_id = $1 AND partner_id = $2 AND status <> 'fetched') AS skipped, \
            (SELECT count(*) FROM otdel.research_findings \
              WHERE bureau_id = $1 AND partner_id = $2) AS findings, \
            (SELECT coalesce(sum(spent_micros), 0)::bigint FROM otdel.research_plans \
              WHERE bureau_id = $1 AND partner_id = $2) AS spent",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_one(tx.conn())
    .await?;

    Ok(ResearchSummary {
        plans_total: count(&row, "plans")?,
        plans_active: count(&row, "active")?,
        questions_open: count(&row, "open_questions")?,
        sources_fetched: count(&row, "fetched")?,
        sources_skipped: count(&row, "skipped")?,
        findings_total: count(&row, "findings")?,
        spent_micros: row.try_get::<i64, _>("spent")?,
    })
}

fn count(row: &PgRow, column: &str) -> DbResult<i32> {
    Ok(i32::try_from(row.try_get::<i64, _>(column)?).unwrap_or(i32::MAX))
}
