//! Phase 1F — the rows behind the refresh status, the retention pass and the export.
//!
//! Nothing here decides anything about knowledge. It reads what exists, in one place, so
//! the API can turn it into sentences and the worker can act on it.
//!
//! The interesting query is [`source_states`]. It answers, per document, four separate
//! questions that used to be one blurred "is it up to date?":
//!
//! * has it been read at all, and how many times;
//! * has the product role drafted from it, and from **which** reading;
//! * how many candidates that draft produced;
//! * how many claims of the currently published version cite it.
//!
//! The third and fourth differ more often than one would like, and the difference is the
//! honest answer to "почему этого нет в опубликованной версии": a candidate that exists
//! and is not in the version was either checked and refused, or drafted after the version
//! was built.

use chrono::{DateTime, Utc};
use otdel_core::knowledge::FactKind;
use otdel_core::model::MaterialStatus;
use otdel_core::publication::{ClaimOrigin, EvidenceSourceKind};
use otdel_core::retention_config::RetentionHorizons;
use otdel_core::updates::RetentionPreview;
use otdel_publish::{CandidateClaim, CandidateEvidence};
use sqlx::Row;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::tenancy::ScopedTx;

/// One document and everything the refresh status needs to know about it.
#[derive(Debug, Clone)]
pub struct SourceRow {
    pub material_id: Uuid,
    pub filename: String,
    pub status: MaterialStatus,
    pub content_revision: i32,
    pub pages_with_text: i64,
    /// `None` when the product role has never run over this material.
    pub run_status: Option<String>,
    /// Which reading the current draft used. `None` for a run that predates the column,
    /// reported as unknown rather than as current.
    pub drafted_revision: Option<i32>,
    pub facts_drafted: i64,
    pub claims_in_published: i64,
}

/// Every material of a partner, with the state of the cycle around it.
///
/// One statement rather than four: the counters have to describe the same moment, and a
/// refresh status assembled from reads taken seconds apart could report a draft that has
/// no facts and facts that have no draft.
pub async fn source_states(tx: &mut ScopedTx, partner_id: Uuid) -> DbResult<Vec<SourceRow>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT m.id, m.filename, m.status, m.content_revision, \
                r.status AS run_status, r.source_revision, \
                coalesce(pages.with_text, 0) AS pages_with_text, \
                coalesce(facts.total, 0) AS facts_drafted, \
                coalesce(published.total, 0) AS claims_in_published \
           FROM otdel.materials m \
           LEFT JOIN otdel.knowledge_runs r \
                  ON r.bureau_id = m.bureau_id AND r.material_id = m.id \
           LEFT JOIN LATERAL ( \
                SELECT count(*) AS with_text FROM otdel.material_pages p \
                 WHERE p.bureau_id = m.bureau_id AND p.material_id = m.id \
                   AND p.status IN ('extracted', 'partial') \
           ) pages ON true \
           LEFT JOIN LATERAL ( \
                SELECT count(*) AS total FROM otdel.knowledge_facts f \
                 WHERE f.bureau_id = m.bureau_id AND f.material_id = m.id \
           ) facts ON true \
           LEFT JOIN LATERAL ( \
                SELECT count(DISTINCT e.claim_id) AS total \
                  FROM otdel.version_evidence e \
                  JOIN otdel.knowledge_versions v \
                       ON v.bureau_id = e.bureau_id AND v.id = e.version_id \
                 WHERE e.bureau_id = m.bureau_id AND e.material_id = m.id \
                   AND v.partner_id = m.partner_id AND v.status = 'published' \
           ) published ON true \
          WHERE m.bureau_id = $1 AND m.partner_id = $2 \
          ORDER BY m.created_at, m.id",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            let status: String = row.try_get("status")?;
            Ok(SourceRow {
                material_id: row.try_get("id")?,
                filename: row.try_get("filename")?,
                status: MaterialStatus::parse(&status).ok_or_else(|| {
                    DbError::Decode(format!("unknown material status `{status}`"))
                })?,
                content_revision: row.try_get("content_revision")?,
                pages_with_text: row.try_get("pages_with_text")?,
                run_status: row.try_get("run_status")?,
                drafted_revision: row.try_get("source_revision")?,
                facts_drafted: row.try_get("facts_drafted")?,
                claims_in_published: row.try_get("claims_in_published")?,
            })
        })
        .collect()
}

/// The candidates of one partner, **without the source text** their citations point into.
///
/// [`crate::publication_read::load_candidates`] exists for the checker and deliberately
/// carries every cited page and snapshot in full, because re-reading them is what a check
/// *is*. The refresh status needs the same rows and none of that text: the candidate
/// fingerprint is computed over the values and the quotations, not over the documents.
///
/// Using the checker's loader here would pull every page of every material through the
/// connection on a plain `GET` that the interface polls every four seconds while a check
/// runs — megabytes per poll, to hash a few hundred bytes of it.
///
/// `source_text` is therefore `None` on every evidence row, which is correct rather than
/// merely cheap: [`otdel_publish::candidate_fingerprint`] does not look at it, and nothing
/// else in this path may.
pub async fn load_candidate_digest(
    tx: &mut ScopedTx,
    partner_id: Uuid,
) -> DbResult<Vec<CandidateClaim>> {
    let bureau_id = tx.bureau_id();
    let mut claims: Vec<CandidateClaim> = Vec::new();

    let rows = sqlx::query(
        "SELECT f.id, f.kind, f.attribute, f.value_text, f.unit, f.conditions, \
                p.name AS product_name, e.quote \
           FROM otdel.knowledge_facts f \
           LEFT JOIN otdel.products p ON p.bureau_id = f.bureau_id AND p.id = f.product_id \
           JOIN otdel.knowledge_evidence e \
                ON e.bureau_id = f.bureau_id AND e.fact_id = f.id \
          WHERE f.bureau_id = $1 AND f.partner_id = $2 \
          ORDER BY f.created_at, f.id, e.created_at, e.id",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_all(tx.conn())
    .await?;

    for row in &rows {
        let fact_id: Uuid = row.try_get("id")?;
        let evidence = quote_only(row.try_get("quote")?, EvidenceSourceKind::Material);
        match claims.iter_mut().find(|claim| claim.origin_id == fact_id) {
            Some(claim) => claim.evidence.push(evidence),
            None => {
                let kind: String = row.try_get("kind")?;
                claims.push(CandidateClaim {
                    origin: ClaimOrigin::PartnerMaterial,
                    origin_id: fact_id,
                    product_name: row.try_get("product_name")?,
                    kind: FactKind::parse(&kind)
                        .ok_or_else(|| DbError::Decode(format!("unknown fact kind `{kind}`")))?,
                    attribute: row.try_get("attribute")?,
                    value_text: row.try_get("value_text")?,
                    unit: row.try_get("unit")?,
                    conditions: row.try_get("conditions")?,
                    model_context: None,
                    evidence: vec![evidence],
                });
            }
        }
    }

    let rows = sqlx::query(
        "SELECT f.id, f.attribute, f.value_text, f.unit, f.conditions, e.quote \
           FROM otdel.research_findings f \
           JOIN otdel.research_evidence e \
                ON e.bureau_id = f.bureau_id AND e.finding_id = f.id \
          WHERE f.bureau_id = $1 AND f.partner_id = $2 \
          ORDER BY f.created_at, f.id, e.created_at, e.id",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_all(tx.conn())
    .await?;

    for row in &rows {
        let finding_id: Uuid = row.try_get("id")?;
        let evidence = quote_only(row.try_get("quote")?, EvidenceSourceKind::External);
        match claims
            .iter_mut()
            .find(|claim| claim.origin_id == finding_id)
        {
            Some(claim) => claim.evidence.push(evidence),
            None => claims.push(CandidateClaim {
                origin: ClaimOrigin::IndustryResearch,
                origin_id: finding_id,
                product_name: None,
                kind: FactKind::Characteristic,
                attribute: row.try_get("attribute")?,
                value_text: row.try_get("value_text")?,
                unit: row.try_get("unit")?,
                conditions: row.try_get("conditions")?,
                model_context: None,
                evidence: vec![evidence],
            }),
        }
    }

    Ok(claims)
}

/// An evidence row carrying only what the fingerprint hashes.
///
/// Every other field is `None` on purpose: this shape must never be handed to the checker,
/// and a row that cannot pretend to have a source is one that cannot be mistaken for one.
fn quote_only(quote: String, source_kind: EvidenceSourceKind) -> CandidateEvidence {
    CandidateEvidence {
        source_kind,
        material_id: None,
        material_filename: None,
        page_number: None,
        region_id: None,
        url: None,
        host: None,
        retrieved_at: None,
        content_hash: None,
        quote,
        char_start: 0,
        char_end: 0,
        source_text: None,
    }
}

/// The version published before this one, by number.
///
/// Used as the default other side of a comparison. Numbers, not timestamps: they are the
/// partner's own sequence, they never repeat, and a version that was blocked keeps its
/// number, so "the previous one" is unambiguous even when some of them never went live.
pub async fn previous_version(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    number: i32,
) -> DbResult<Option<Uuid>> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(
        "SELECT id FROM otdel.knowledge_versions \
          WHERE bureau_id = $1 AND partner_id = $2 AND number < $3 \
            AND published_at IS NOT NULL \
          ORDER BY number DESC LIMIT 1",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .bind(number)
    .fetch_optional(tx.conn())
    .await?;
    row.map(|row| row.try_get::<Uuid, _>("id"))
        .transpose()
        .map_err(DbError::from)
}

/// Record which reading of the document a draft was made from.
///
/// Called when the understanding run starts, with the material's revision as it is at
/// that moment. A run that finishes after the document has been re-read therefore records
/// the older number, and the refresh status reports it as a draft of an older reading —
/// which is exactly what it is.
pub async fn set_draft_source_revision(
    tx: &mut ScopedTx,
    run_id: Uuid,
    revision: i32,
) -> DbResult<()> {
    let bureau_id = tx.bureau_id();
    sqlx::query(
        "UPDATE otdel.knowledge_runs SET source_revision = $3, updated_at = now() \
          WHERE bureau_id = $1 AND id = $2",
    )
    .bind(bureau_id)
    .bind(run_id)
    .bind(revision)
    .execute(tx.conn())
    .await?;
    Ok(())
}

/// The material's current reading number, for the caller that is about to draft from it.
pub async fn content_revision(tx: &mut ScopedTx, material_id: Uuid) -> DbResult<Option<i32>> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(
        "SELECT content_revision FROM otdel.materials WHERE bureau_id = $1 AND id = $2",
    )
    .bind(bureau_id)
    .bind(material_id)
    .fetch_optional(tx.conn())
    .await?;
    row.map(|row| row.try_get::<i32, _>("content_revision"))
        .transpose()
        .map_err(DbError::from)
}

// --- retention ------------------------------------------------------------------------

/// What a sweep would remove right now, without removing it.
pub async fn retention_preview(
    tx: &mut ScopedTx,
    horizons: Option<RetentionHorizons>,
) -> DbResult<RetentionPreview> {
    let bureau_id = tx.bureau_id();
    let (event_days, job_days, keep_per_kind) = match horizons {
        Some(horizons) => (
            Some(i32::try_from(horizons.event_days).unwrap_or(i32::MAX)),
            horizons
                .job_days
                .map(|days| i32::try_from(days).unwrap_or(i32::MAX)),
            i32::try_from(horizons.keep_per_kind).unwrap_or(i32::MAX),
        ),
        // Nothing is configured: the totals are still worth showing, and the "would be
        // removed" halves are honestly zero because nothing would be.
        None => (None, None, 0),
    };

    let row = sqlx::query("SELECT * FROM otdel.retention_preview($1, $2, $3, $4)")
        .bind(bureau_id)
        .bind(event_days)
        .bind(job_days)
        .bind(keep_per_kind)
        .fetch_one(tx.conn())
        .await?;

    Ok(RetentionPreview {
        events_prunable: row.try_get("events_prunable")?,
        jobs_prunable: row.try_get("jobs_prunable")?,
        events_total: row.try_get("events_total")?,
        jobs_total: row.try_get("jobs_total")?,
        oldest_event: row.try_get::<Option<DateTime<Utc>>, _>("oldest_event")?,
    })
}

/// What one sweep removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetentionOutcome {
    pub events_removed: i64,
    pub jobs_removed: i64,
}

impl RetentionOutcome {
    pub const fn is_empty(self) -> bool {
        self.events_removed == 0 && self.jobs_removed == 0
    }
}

/// Run the retention pass.
///
/// The horizons are handed to a `SECURITY DEFINER` function that carries the floor, so a
/// caller cannot ask for "delete everything" and a bug in this crate cannot produce one.
/// The runtime role has no DELETE privilege on either table, which is what makes that
/// statement true rather than a convention.
///
/// `job_days: None` means the queue is not pruned at all; the event horizon is passed for
/// both because the function requires a job horizon no longer than the event one, and the
/// pass then skips the queue by keeping every finished job of every kind.
pub async fn apply_retention(
    tx: &mut ScopedTx,
    horizons: RetentionHorizons,
) -> DbResult<RetentionOutcome> {
    let bureau_id = tx.bureau_id();
    let event_days = i32::try_from(horizons.event_days).unwrap_or(i32::MAX);
    // No job horizon: hand the function the event horizon (it refuses a longer one) and
    // a per-kind floor large enough that no finished job is eligible. "Not pruned" then
    // holds for the same reason the rest of this does — because the query says so, not
    // because a branch elsewhere remembered to skip a call.
    let (job_days, keep_per_kind) = match horizons.job_days {
        Some(days) => (
            i32::try_from(days).unwrap_or(i32::MAX),
            i32::try_from(horizons.keep_per_kind).unwrap_or(i32::MAX),
        ),
        None => (event_days, i32::MAX),
    };

    let row = sqlx::query("SELECT * FROM otdel.apply_retention($1, $2, $3, $4)")
        .bind(bureau_id)
        .bind(event_days)
        .bind(job_days)
        .bind(keep_per_kind)
        .fetch_one(tx.conn())
        .await?;

    Ok(RetentionOutcome {
        events_removed: row.try_get("events_removed")?,
        jobs_removed: row.try_get("jobs_removed")?,
    })
}

// --- the searchable half of a withdrawn version -----------------------------------------

/// Remove the search index of one version.
///
/// Called when a version is retracted, in the same transaction as the retraction. The
/// snapshot — claims, citations, gaps, readiness — stays, because it is history and the
/// database refuses to delete it. The chunks are not history: they are a rebuildable
/// rendering whose only purpose is to make the claims findable, and a withdrawn version
/// must not be findable.
///
/// Retrieval already refuses a `revoked` version by resolving the published pointer per
/// request, so this is defence in depth rather than the mechanism. It is worth having
/// because the two failures are different: the pointer protects the *query path*, and
/// this removes the *data* — including any vectors, which is the half a future index or a
/// future query path would be most likely to reach without asking about status first.
pub async fn purge_version_chunks(tx: &mut ScopedTx, version_id: Uuid) -> DbResult<u64> {
    let bureau_id = tx.bureau_id();
    let result =
        sqlx::query("DELETE FROM otdel.version_chunks WHERE bureau_id = $1 AND version_id = $2")
            .bind(bureau_id)
            .bind(version_id)
            .execute(tx.conn())
            .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sweep_that_removed_nothing_says_so() {
        assert!(RetentionOutcome::default().is_empty());
        assert!(!RetentionOutcome {
            events_removed: 1,
            jobs_removed: 0,
        }
        .is_empty());
        assert!(!RetentionOutcome {
            events_removed: 0,
            jobs_removed: 3,
        }
        .is_empty());
    }
}
