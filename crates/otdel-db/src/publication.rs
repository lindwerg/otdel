//! Writing a knowledge version: the check that produced it, the snapshot it froze, and
//! the atomic moment it becomes the published one.
//!
//! **The snapshot is a copy, and the copy is the point.** Every claim, citation, gap and
//! readiness row is written once and never touched again — the runtime role has no
//! UPDATE or DELETE privilege on any of them, and `0006_publication.sql` refuses both
//! with a trigger regardless of who asks. Re-drafting a material afterwards changes the
//! candidate rows and leaves the published version exactly as it was.
//!
//! **Publication is one statement, not a sequence.** [`publish_version`] moves the
//! previous version to `superseded` and this one to `published` inside a single
//! transaction, and a partial unique index makes a second published row impossible. Two
//! workers racing do not produce two current versions; the loser's statement fails.
//!
//! **A late run cannot overwrite a newer one.** Publication is guarded twice: the job
//! lease is re-checked in the same transaction as the write (the caller does that), and
//! the version's `input_fingerprint` is compared with the one already published, so a run
//! that started before a newer one finished cannot replace it with older knowledge
//! (`docs/block-01-spec.md` §7).
//!
//! Reading it back lives in [`crate::publication_read`].

use chrono::{DateTime, Utc};
use otdel_core::publication::{
    ClaimStatus, ReadinessEntry, ValidationRun, ValidationRunStatus, VersionStatus,
};
use sqlx::Row;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::publication_read;
use crate::tenancy::ScopedTx;

// --- the run ---------------------------------------------------------------------

/// Create or re-arm the partner's check row and put it in `queued`.
pub async fn enqueue_run(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    prompt_profile: &str,
) -> DbResult<ValidationRun> {
    let bureau_id = tx.bureau_id();
    sqlx::query(
        "INSERT INTO otdel.validation_runs \
             (bureau_id, partner_id, status, prompt_profile) \
         VALUES ($1, $2, 'queued', $3) \
         ON CONFLICT (partner_id) DO UPDATE \
            SET status = 'queued', \
                prompt_profile = EXCLUDED.prompt_profile, \
                version_id = NULL, \
                claims_considered = 0, \
                claims_rejected = 0, \
                gaps_carried = 0, \
                chunks_created = 0, \
                chunks_embedded = 0, \
                model_reviewed = 0, \
                published = false, \
                rejections = '{}', \
                blocked_reasons = '{}', \
                diagnostic = NULL, \
                started_at = NULL, \
                finished_at = NULL, \
                updated_at = now()",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .bind(prompt_profile)
    .execute(tx.conn())
    .await?;

    publication_read::find_run(tx, partner_id)
        .await?
        .ok_or_else(|| {
            DbError::Decode("validation run disappeared after it was written".to_owned())
        })
}

/// Mark the run as running. Returns the row the worker will report against.
pub async fn start_run(tx: &mut ScopedTx, partner_id: Uuid) -> DbResult<ValidationRun> {
    let bureau_id = tx.bureau_id();
    sqlx::query(
        "UPDATE otdel.validation_runs \
            SET status = 'running', \
                started_at = now(), \
                finished_at = NULL, \
                diagnostic = NULL, \
                rejections = '{}', \
                blocked_reasons = '{}', \
                updated_at = now() \
          WHERE bureau_id = $1 AND partner_id = $2",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .execute(tx.conn())
    .await?;

    publication_read::find_run(tx, partner_id)
        .await?
        .ok_or_else(|| DbError::Decode("validation run vanished while starting".to_owned()))
}

/// Everything a finished check reports.
#[derive(Debug, Clone, Default)]
pub struct RunOutcome {
    pub status_queued: Option<ValidationRunStatus>,
    pub version_id: Option<Uuid>,
    pub claims_considered: i32,
    pub claims_rejected: i32,
    pub gaps_carried: i32,
    pub chunks_created: i32,
    pub chunks_embedded: i32,
    pub model_reviewed: i32,
    pub published: bool,
    pub rejections: Vec<String>,
    pub blocked_reasons: Vec<String>,
    pub diagnostic: Option<String>,
}

pub async fn finish_run(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    status: ValidationRunStatus,
    outcome: &RunOutcome,
) -> DbResult<()> {
    let bureau_id = tx.bureau_id();
    sqlx::query(
        "UPDATE otdel.validation_runs \
            SET status = $3, \
                version_id = $4, \
                claims_considered = $5, \
                claims_rejected = $6, \
                gaps_carried = $7, \
                chunks_created = $8, \
                chunks_embedded = $9, \
                model_reviewed = $10, \
                published = $11, \
                rejections = $12, \
                blocked_reasons = $13, \
                diagnostic = $14, \
                finished_at = now(), \
                updated_at = now() \
          WHERE bureau_id = $1 AND partner_id = $2",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .bind(status.as_str())
    .bind(outcome.version_id)
    .bind(outcome.claims_considered)
    .bind(outcome.claims_rejected)
    .bind(outcome.gaps_carried)
    .bind(outcome.chunks_created)
    .bind(outcome.chunks_embedded)
    .bind(outcome.model_reviewed)
    .bind(outcome.published)
    .bind(&outcome.rejections)
    .bind(&outcome.blocked_reasons)
    .bind(outcome.diagnostic.as_deref())
    .execute(tx.conn())
    .await?;
    Ok(())
}

/// Settle checks whose worker is gone, so the interface stops showing work that is not
/// happening. Mirrors `reclaim_stalled_runs` of 1C.
pub async fn reclaim_stalled_runs(tx: &mut ScopedTx) -> DbResult<u64> {
    let result = sqlx::query(
        "UPDATE otdel.validation_runs r \
            SET status = 'failed', \
                diagnostic = 'проверка прервана: исполнитель не завершил работу', \
                finished_at = now(), \
                updated_at = now() \
          WHERE r.status IN ('queued', 'running') \
            AND NOT EXISTS ( \
                SELECT 1 FROM otdel.jobs j \
                 WHERE j.bureau_id = r.bureau_id \
                   AND j.partner_id = r.partner_id \
                   AND j.kind = 'validate_partner' \
                   AND j.status IN ('queued', 'running') \
            )",
    )
    .execute(tx.conn())
    .await?;
    Ok(result.rows_affected())
}

// --- the snapshot -----------------------------------------------------------------

/// One claim as it will be frozen, with its citations.
#[derive(Debug, Clone)]
pub struct NewClaim {
    pub origin: &'static str,
    pub origin_id: Uuid,
    pub scope: &'static str,
    pub product_name: Option<String>,
    pub kind: &'static str,
    pub status: ClaimStatus,
    pub attribute: String,
    pub value_text: String,
    pub unit: Option<String>,
    pub conditions: Option<String>,
    pub model_context: Option<String>,
    pub check_note: Option<String>,
    pub evidence: Vec<NewEvidence>,
    /// The searchable rendering and its folded lookup keys.
    pub chunk_text: String,
    pub normalised_value: String,
    pub normalised_attribute: String,
    pub normalised_product: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewEvidence {
    pub source_kind: &'static str,
    pub material_id: Option<Uuid>,
    pub material_filename: Option<String>,
    pub page_number: Option<i32>,
    pub region_id: Option<Uuid>,
    pub url: Option<String>,
    pub host: Option<String>,
    pub retrieved_at: Option<DateTime<Utc>>,
    pub content_hash: Option<String>,
    pub quote: String,
    pub char_start: i32,
    pub char_end: i32,
}

#[derive(Debug, Clone)]
pub struct NewGap {
    pub origin_id: Uuid,
    pub product_name: Option<String>,
    pub topic: String,
    pub missing: String,
    pub blocks: Option<String>,
    pub blocks_topics: Vec<String>,
}

/// Everything a version is made of.
#[derive(Debug, Clone)]
pub struct NewVersion {
    pub input_fingerprint: String,
    pub validation_run_id: Uuid,
    pub claims: Vec<NewClaim>,
    pub gaps: Vec<NewGap>,
    pub readiness: Vec<ReadinessEntry>,
}

/// Counts of what was actually written.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VersionCounts {
    pub claims: i32,
    pub evidence: i32,
    pub gaps: i32,
    pub chunks: i32,
}

/// Write one version and everything in it, as a `draft`.
///
/// Publishing is a separate step ([`publish_version`]) so that a snapshot which fails the
/// readiness rules can still be stored and inspected — "честно остаются неполными" needs
/// something to point at.
///
/// The whole snapshot is written in the caller's transaction. The deferred evidence
/// trigger therefore fires at commit, which is what allows a claim to be inserted before
/// the citations that justify it.
pub async fn write_version(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    version: &NewVersion,
) -> DbResult<(Uuid, VersionCounts)> {
    let bureau_id = tx.bureau_id();

    // The next number for this partner. `FOR UPDATE` on the partner row would be the
    // other way to serialise this; the unique constraint on (partner_id, number) is
    // enough, because a loser simply fails and its job is retried.
    let number: i32 = sqlx::query(
        "SELECT COALESCE(MAX(number), 0) + 1 AS next \
           FROM otdel.knowledge_versions WHERE bureau_id = $1 AND partner_id = $2",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_one(tx.conn())
    .await?
    .try_get("next")?;

    let version_id: Uuid = sqlx::query(
        "INSERT INTO otdel.knowledge_versions \
             (bureau_id, partner_id, number, status, validation_run_id, input_fingerprint) \
         VALUES ($1, $2, $3, 'draft', $4, $5) \
         RETURNING id",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .bind(number)
    .bind(version.validation_run_id)
    .bind(&version.input_fingerprint)
    .fetch_one(tx.conn())
    .await?
    .try_get("id")?;

    let mut counts = VersionCounts::default();

    for claim in &version.claims {
        // Defence in depth beside the database CHECK: a claim with no citation must not
        // be attempted at all, so the failure is a refusal here rather than an aborted
        // transaction at commit that takes the whole version with it.
        if claim.evidence.is_empty() {
            return Err(DbError::Decode(
                "refusing to store a published claim without evidence".to_owned(),
            ));
        }

        let claim_id: Uuid = sqlx::query(
            "INSERT INTO otdel.version_claims \
                 (bureau_id, partner_id, version_id, origin, origin_id, scope, product_name, \
                  kind, status, attribute, value_text, unit, conditions, model_context, \
                  check_note) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) \
             RETURNING id",
        )
        .bind(bureau_id)
        .bind(partner_id)
        .bind(version_id)
        .bind(claim.origin)
        .bind(claim.origin_id)
        .bind(claim.scope)
        .bind(claim.product_name.as_deref())
        .bind(claim.kind)
        .bind(claim.status.as_str())
        .bind(&claim.attribute)
        .bind(&claim.value_text)
        .bind(claim.unit.as_deref())
        .bind(claim.conditions.as_deref())
        .bind(claim.model_context.as_deref())
        .bind(claim.check_note.as_deref())
        .fetch_one(tx.conn())
        .await?
        .try_get("id")?;
        counts.claims += 1;

        for evidence in &claim.evidence {
            sqlx::query(
                "INSERT INTO otdel.version_evidence \
                     (bureau_id, version_id, claim_id, source_kind, material_id, \
                      material_filename, page_number, region_id, url, host, retrieved_at, \
                      content_hash, quote, char_start, char_end) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
            )
            .bind(bureau_id)
            .bind(version_id)
            .bind(claim_id)
            .bind(evidence.source_kind)
            .bind(evidence.material_id)
            .bind(evidence.material_filename.as_deref())
            .bind(evidence.page_number)
            .bind(evidence.region_id)
            .bind(evidence.url.as_deref())
            .bind(evidence.host.as_deref())
            .bind(evidence.retrieved_at)
            .bind(evidence.content_hash.as_deref())
            .bind(&evidence.quote)
            .bind(evidence.char_start)
            .bind(evidence.char_end)
            .execute(tx.conn())
            .await?;
            counts.evidence += 1;
        }

        sqlx::query(
            "INSERT INTO otdel.version_chunks \
                 (bureau_id, partner_id, version_id, claim_id, chunk_text, normalised_value, \
                  normalised_attribute, normalised_product) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(bureau_id)
        .bind(partner_id)
        .bind(version_id)
        .bind(claim_id)
        .bind(&claim.chunk_text)
        .bind(&claim.normalised_value)
        .bind(&claim.normalised_attribute)
        .bind(claim.normalised_product.as_deref())
        .execute(tx.conn())
        .await?;
        counts.chunks += 1;
    }

    for gap in &version.gaps {
        sqlx::query(
            "INSERT INTO otdel.version_gaps \
                 (bureau_id, partner_id, version_id, origin_id, product_name, topic, missing, \
                  blocks, blocks_topics) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(bureau_id)
        .bind(partner_id)
        .bind(version_id)
        .bind(gap.origin_id)
        .bind(gap.product_name.as_deref())
        .bind(&gap.topic)
        .bind(&gap.missing)
        .bind(gap.blocks.as_deref())
        .bind(&gap.blocks_topics)
        .execute(tx.conn())
        .await?;
        counts.gaps += 1;
    }

    for entry in &version.readiness {
        sqlx::query(
            "INSERT INTO otdel.version_readiness \
                 (bureau_id, partner_id, version_id, topic, state, reason) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(bureau_id)
        .bind(partner_id)
        .bind(version_id)
        .bind(entry.topic.as_str())
        .bind(entry.state.as_str())
        .bind(&entry.reason)
        .execute(tx.conn())
        .await?;
    }

    Ok((version_id, counts))
}

/// Mark a stored snapshot as blocked, with the rules it failed.
pub async fn block_version(
    tx: &mut ScopedTx,
    version_id: Uuid,
    reasons: &[String],
) -> DbResult<()> {
    let bureau_id = tx.bureau_id();
    sqlx::query(
        "UPDATE otdel.knowledge_versions \
            SET status = 'blocked', blocked_reasons = $3 \
          WHERE bureau_id = $1 AND id = $2 AND status IN ('draft', 'validating')",
    )
    .bind(bureau_id)
    .bind(version_id)
    .bind(reasons)
    .execute(tx.conn())
    .await?;
    Ok(())
}

/// What publication concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishOutcome {
    Published,
    /// A version with a newer or equal fingerprint is already published. The snapshot
    /// stays as a draft and nothing is switched — this is the guard against a late run
    /// overwriting a newer revision (`docs/block-01-spec.md` §7).
    Superseded {
        reason: String,
    },
}

/// Switch the published pointer to this version, atomically.
///
/// Everything happens in the caller's transaction:
///
/// 1. the currently published version, if any, is locked and read;
/// 2. if its fingerprint equals this one's, or it was published after this snapshot was
///    created, nothing is switched;
/// 3. otherwise it becomes `superseded` and this one becomes `published`.
///
/// Step 3 relies on the partial unique index for correctness under concurrency, not on
/// the ordering of these two statements: if another transaction published in between,
/// this one's UPDATE violates the index and the whole check is retried.
pub async fn publish_version(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    version_id: Uuid,
    inputs_read_at: DateTime<Utc>,
) -> DbResult<PublishOutcome> {
    let bureau_id = tx.bureau_id();

    let this = sqlx::query(
        "SELECT input_fingerprint FROM otdel.knowledge_versions \
          WHERE bureau_id = $1 AND partner_id = $2 AND id = $3 FOR UPDATE",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .bind(version_id)
    .fetch_optional(tx.conn())
    .await?
    .ok_or_else(|| DbError::Decode("version vanished before publication".to_owned()))?;
    let fingerprint: String = this.try_get("input_fingerprint")?;

    let current = sqlx::query(
        "SELECT id, input_fingerprint, published_at FROM otdel.knowledge_versions \
          WHERE bureau_id = $1 AND partner_id = $2 AND status = 'published' FOR UPDATE",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_optional(tx.conn())
    .await?;

    if let Some(row) = &current {
        let current_id: Uuid = row.try_get("id")?;
        let current_fingerprint: String = row.try_get("input_fingerprint")?;
        let current_published: Option<DateTime<Utc>> = row.try_get("published_at")?;

        if current_id == version_id {
            return Ok(PublishOutcome::Published);
        }
        if current_fingerprint == fingerprint {
            return Ok(PublishOutcome::Superseded {
                reason: "опубликованная версия построена из того же набора кандидатов".to_owned(),
            });
        }
        // A run that read its candidates before a newer version was published must not
        // put older knowledge back on top (`docs/block-01-spec.md` §7).
        //
        // The comparison is deliberately against `inputs_read_at` — the moment this run
        // *read* the candidates — and not against this version row's `created_at`. The
        // row is inserted inside this very transaction, so its creation time is always
        // later than anything already published, and comparing it could never refuse
        // anything. That was the first version of this check, and it was dead code
        // wearing the clothes of a guarantee.
        if current_published.is_some_and(|published| published > inputs_read_at) {
            return Ok(PublishOutcome::Superseded {
                reason: "за время проверки опубликована более новая версия — результат этого \
                         запуска не заменяет её"
                    .to_owned(),
            });
        }

        sqlx::query(
            "UPDATE otdel.knowledge_versions \
                SET status = 'superseded', superseded_at = now() \
              WHERE bureau_id = $1 AND id = $2 AND status = 'published'",
        )
        .bind(bureau_id)
        .bind(current_id)
        .execute(tx.conn())
        .await?;
    }

    let switched = sqlx::query(
        "UPDATE otdel.knowledge_versions \
            SET status = 'published', published_at = now(), blocked_reasons = '{}' \
          WHERE bureau_id = $1 AND partner_id = $2 AND id = $3 \
            AND status IN ('draft', 'validating')",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .bind(version_id)
    .execute(tx.conn())
    .await?;

    if switched.rows_affected() == 1 {
        Ok(PublishOutcome::Published)
    } else {
        Err(DbError::Decode(
            "version could not be published: it is no longer a draft".to_owned(),
        ))
    }
}

/// Withdraw the published version.
///
/// Returns `false` when there was nothing published to withdraw. The snapshot is kept —
/// retraction is history, not deletion, and the database refuses to delete a version that
/// was ever published. Search resolves the pointer on every request, so the version is
/// gone from search the moment this commits.
pub async fn retract_version(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    version_id: Uuid,
    reason: &str,
) -> DbResult<bool> {
    let bureau_id = tx.bureau_id();
    let result = sqlx::query(
        "UPDATE otdel.knowledge_versions \
            SET status = 'revoked', revoked_at = now(), revoked_reason = $4 \
          WHERE bureau_id = $1 AND partner_id = $2 AND id = $3 AND status = 'published'",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .bind(version_id)
    .bind(reason)
    .execute(tx.conn())
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Attach vectors to a version's chunks.
///
/// Separate from writing the version on purpose: embeddings are optional, they are
/// produced by an external call that must not happen inside the transaction that freezes
/// the knowledge, and a provider configured later must be able to add them to an already
/// published version without touching a single claim.
pub async fn store_embeddings(
    tx: &mut ScopedTx,
    version_id: Uuid,
    profile: &str,
    dimensions: i32,
    vectors: &[(Uuid, Vec<f32>)],
) -> DbResult<i32> {
    if vectors.is_empty() {
        return Ok(0);
    }
    let bureau_id = tx.bureau_id();

    // The column only exists when pgvector is installed *and* reachable by this role.
    // Without it there is nowhere to put a vector, and inventing a substitute is exactly
    // what this phase must not do.
    let Some(schema) = crate::publication_read::vector_schema(tx).await? else {
        return Err(DbError::Decode(
            "pgvector is not installed or not reachable by the runtime role: there is \
             nowhere to store an embedding"
                .to_owned(),
        ));
    };

    let mut stored = 0i32;
    for (chunk_id, vector) in vectors {
        if i32::try_from(vector.len()).unwrap_or(i32::MAX) != dimensions {
            return Err(DbError::Decode(format!(
                "embedding for chunk {chunk_id} has {} dimensions, expected {dimensions}",
                vector.len()
            )));
        }
        let literal = vector_literal(vector);
        let result = sqlx::query(&format!(
            "UPDATE otdel.version_chunks \
                SET embedding = $4::text::{schema}.vector, \
                    embedding_profile = $5, \
                    embedding_dims = $6 \
              WHERE bureau_id = $1 AND version_id = $2 AND id = $3"
        ))
        .bind(bureau_id)
        .bind(version_id)
        .bind(chunk_id)
        .bind(&literal)
        .bind(profile)
        .bind(dimensions)
        .execute(tx.conn())
        .await?;
        stored += i32::try_from(result.rows_affected()).unwrap_or(0);
    }

    sqlx::query(
        "UPDATE otdel.knowledge_versions SET embedding_profile = $3 \
          WHERE bureau_id = $1 AND id = $2",
    )
    .bind(bureau_id)
    .bind(version_id)
    .bind(profile)
    .execute(tx.conn())
    .await?;

    Ok(stored)
}

/// Render a vector as the `[1,2,3]` text pgvector parses.
///
/// Built from `f32` values that have already been checked to be finite by the embedding
/// adapter, and bound as a parameter rather than interpolated, so there is no path from
/// a provider's response into SQL.
pub(crate) fn vector_literal(vector: &[f32]) -> String {
    let mut out = String::with_capacity(vector.len() * 8 + 2);
    out.push('[');
    for (index, value) in vector.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&format!("{value}"));
    }
    out.push(']');
    out
}

/// Status transitions the version lifecycle allows, for the caller's own checks.
pub const fn can_retract(status: VersionStatus) -> bool {
    matches!(status, VersionStatus::Published)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vector_is_rendered_as_the_literal_pgvector_parses() {
        assert_eq!(vector_literal(&[1.0, -2.5, 0.0]), "[1,-2.5,0]");
        assert_eq!(vector_literal(&[]), "[]");
    }

    #[test]
    fn only_a_published_version_can_be_retracted() {
        assert!(can_retract(VersionStatus::Published));
        for status in [
            VersionStatus::Draft,
            VersionStatus::Validating,
            VersionStatus::Blocked,
            VersionStatus::Superseded,
            VersionStatus::Revoked,
        ] {
            assert!(!can_retract(status), "{}", status.as_str());
        }
    }
}
