//! Durable job queue.
//!
//! Phase 1A only *records* work: uploading a material enqueues one `extract_document`
//! job and the retry endpoint puts an existing job back into `queued`. Nothing in this
//! repository claims to read documents — the 1B worker will lease these rows.
//!
//! The queue row carries everything a worker needs to be restart-safe: an idempotency
//! key (unique per bureau), the attempt counter with a bound, a lease owner/expiry and
//! the earliest time it may run again.

use chrono::{DateTime, Utc};
use otdel_core::model::{extraction_idempotency_key, Job, JobKind, JobStatus};
use sqlx::postgres::PgRow;
use sqlx::Row;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::tenancy::ScopedTx;

const COLUMNS: &str =
    "id, partner_id, material_id, kind, status, stage, attempts, created_at, updated_at, error";

fn job_from_row(row: &PgRow) -> DbResult<Job> {
    let kind: String = row.try_get("kind")?;
    let kind = JobKind::parse(&kind)
        .ok_or_else(|| DbError::Decode(format!("unknown job kind `{kind}`")))?;
    let status: String = row.try_get("status")?;
    let status = JobStatus::parse(&status)
        .ok_or_else(|| DbError::Decode(format!("unknown job status `{status}`")))?;

    Ok(Job {
        id: row.try_get("id")?,
        partner_id: row.try_get("partner_id")?,
        material_id: row.try_get("material_id")?,
        kind,
        status,
        stage: row.try_get("stage")?,
        attempts: row.try_get("attempts")?,
        created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
        updated_at: row.try_get::<DateTime<Utc>, _>("updated_at")?,
        error: row.try_get("error")?,
    })
}

pub async fn list_for_partner(tx: &mut ScopedTx, partner_id: Uuid) -> DbResult<Vec<Job>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM otdel.jobs WHERE bureau_id = $1 AND partner_id = $2 \
         ORDER BY created_at DESC, id DESC"
    ))
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter().map(job_from_row).collect()
}

/// Enqueue extraction for a material, or return the job that already exists.
///
/// Idempotent by `(bureau_id, idempotency_key)`: re-uploading identical bytes or
/// retrying twice never produces a second queue entry for the same material.
pub async fn enqueue_extraction(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    material_id: Uuid,
) -> DbResult<Job> {
    let bureau_id = tx.bureau_id();
    let kind = JobKind::ExtractDocument;
    let key = extraction_idempotency_key(material_id, kind);

    let inserted = sqlx::query(&format!(
        "INSERT INTO otdel.jobs \
             (bureau_id, partner_id, material_id, kind, status, idempotency_key) \
         VALUES ($1, $2, $3, $4, 'queued', $5) \
         ON CONFLICT (bureau_id, idempotency_key) DO NOTHING \
         RETURNING {COLUMNS}"
    ))
    .bind(bureau_id)
    .bind(partner_id)
    .bind(material_id)
    .bind(kind.as_str())
    .bind(&key)
    .fetch_optional(tx.conn())
    .await?;

    if let Some(row) = inserted {
        return job_from_row(&row);
    }

    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM otdel.jobs WHERE bureau_id = $1 AND idempotency_key = $2"
    ))
    .bind(bureau_id)
    .bind(&key)
    .fetch_optional(tx.conn())
    .await?
    .ok_or_else(|| {
        DbError::Decode("job insert conflicted but the existing job is not visible".to_owned())
    })?;

    job_from_row(&row)
}

/// Put the extraction job of a material back into `queued`.
///
/// The attempt counter is deliberately *not* reset: it records how many times the job
/// actually ran, and `max_attempts` still bounds automatic retries by the worker.
pub async fn requeue_extraction(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    material_id: Uuid,
) -> DbResult<Job> {
    let bureau_id = tx.bureau_id();
    let kind = JobKind::ExtractDocument;
    let key = extraction_idempotency_key(material_id, kind);

    let updated = sqlx::query(&format!(
        "UPDATE otdel.jobs \
            SET status = 'queued', \
                stage = NULL, \
                error = NULL, \
                run_after = now(), \
                lease_owner = NULL, \
                lease_expires_at = NULL, \
                max_attempts = GREATEST(max_attempts, attempts + 1), \
                updated_at = now() \
          WHERE bureau_id = $1 AND partner_id = $2 AND material_id = $3 AND idempotency_key = $4 \
      RETURNING {COLUMNS}"
    ))
    .bind(bureau_id)
    .bind(partner_id)
    .bind(material_id)
    .bind(&key)
    .fetch_optional(tx.conn())
    .await?;

    match updated {
        Some(row) => job_from_row(&row),
        // The material exists but has no job row (e.g. it predates the queue): create it.
        None => enqueue_extraction(tx, partner_id, material_id).await,
    }
}

/// Jobs whose lease expired: a worker died mid-run and the work must become runnable
/// again. Returns the number of rows recovered.
///
/// In 1A nothing leases jobs, so this is normally a no-op; it exists because the
/// maintenance worker must be able to recover state after a crash without a human.
pub async fn reclaim_expired_leases(tx: &mut ScopedTx) -> DbResult<u64> {
    let bureau_id = tx.bureau_id();
    let result = sqlx::query(
        "UPDATE otdel.jobs \
            SET status = CASE WHEN attempts >= max_attempts THEN 'failed' ELSE 'queued' END, \
                error = CASE WHEN attempts >= max_attempts \
                             THEN 'attempt limit reached after an interrupted run' \
                             ELSE error END, \
                lease_owner = NULL, \
                lease_expires_at = NULL, \
                run_after = now(), \
                updated_at = now() \
          WHERE bureau_id = $1 AND status = 'running' AND lease_expires_at < now()",
    )
    .bind(bureau_id)
    .execute(tx.conn())
    .await?;

    Ok(result.rows_affected())
}

/// Queue depth by status, for the maintenance log and future dashboards.
pub async fn count_by_status(tx: &mut ScopedTx) -> DbResult<Vec<(JobStatus, i64)>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT status, count(*) AS total FROM otdel.jobs WHERE bureau_id = $1 GROUP BY status",
    )
    .bind(bureau_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            let status: String = row.try_get("status")?;
            let status = JobStatus::parse(&status)
                .ok_or_else(|| DbError::Decode(format!("unknown job status `{status}`")))?;
            Ok((status, row.try_get::<i64, _>("total")?))
        })
        .collect()
}
