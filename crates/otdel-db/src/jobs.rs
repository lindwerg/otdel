//! Durable job queue.
//!
//! Phase 1A recorded work; phase 1B runs it. The row carries everything a worker needs
//! to be restart-safe: an idempotency key (unique per bureau), the attempt counter with a
//! bound, a lease owner/expiry and the earliest time it may run again.
//!
//! Claiming uses `FOR UPDATE SKIP LOCKED` inside a **short** transaction that only marks
//! the row; the actual reading happens afterwards, outside any transaction, with the
//! lease renewed by heartbeats. That is what keeps a long document from holding a
//! database transaction open for minutes, and what lets a second worker pick up the job
//! if this one dies — the maintenance pass returns expired leases to the queue.

use std::time::Duration;

use chrono::{DateTime, Utc};
use otdel_core::extraction::page_extraction_idempotency_key;
use otdel_core::knowledge::understanding_idempotency_key;
use otdel_core::model::{extraction_idempotency_key, Job, JobKind, JobStatus};
use sqlx::postgres::PgRow;
use sqlx::Row;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::tenancy::ScopedTx;

const COLUMNS: &str = "id, partner_id, material_id, page_number, kind, status, stage, \
     attempts, created_at, updated_at, error";

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
        page_number: row.try_get("page_number")?,
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
                error_kind = NULL, \
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

// --- phase 1B: single-page jobs ------------------------------------------------------

/// Enqueue (or return) the job that re-reads one page of a material.
///
/// The idempotency key contains the page number, so retrying page 7 twice reuses one row
/// and retrying page 8 is a different job.
pub async fn enqueue_page_extraction(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    material_id: Uuid,
    page_number: i32,
) -> DbResult<Job> {
    let bureau_id = tx.bureau_id();
    let key = page_extraction_idempotency_key(material_id, page_number);

    // An existing row is *reset* rather than left as it was: a page job that already ran
    // and failed must become runnable again, which is exactly what the user asked for.
    let row = sqlx::query(&format!(
        "INSERT INTO otdel.jobs \
             (bureau_id, partner_id, material_id, page_number, kind, status, idempotency_key) \
         VALUES ($1, $2, $3, $4, 'extract_page', 'queued', $5) \
         ON CONFLICT (bureau_id, idempotency_key) DO UPDATE \
            SET status = 'queued', \
                stage = NULL, \
                error = NULL, \
                error_kind = NULL, \
                run_after = now(), \
                lease_owner = NULL, \
                lease_expires_at = NULL, \
                max_attempts = GREATEST(otdel.jobs.max_attempts, otdel.jobs.attempts + 1), \
                updated_at = now() \
         RETURNING {COLUMNS}"
    ))
    .bind(bureau_id)
    .bind(partner_id)
    .bind(material_id)
    .bind(page_number)
    .bind(&key)
    .fetch_one(tx.conn())
    .await?;

    job_from_row(&row)
}

// --- phase 1C: understanding jobs ------------------------------------------------------

/// Enqueue (or re-arm) the understanding run of a material.
///
/// Keyed by the material, so pressing "разобрать" twice, or the extraction worker
/// finishing twice, reuses one row. A settled row is reset to `queued` — that is what
/// the owner asked for when a material has already been drafted once and needs
/// drafting again (a new model, a re-read page, a key that has finally been
/// configured).
///
/// A row that is **currently running** is left exactly as it is, and the running job is
/// returned instead. Resetting it would hand the same material to a second worker while
/// the first still holds the lease, and both would write the same draft. The
/// `WHERE` clause on `DO UPDATE` is what makes that impossible rather than unlikely:
/// the decision is taken inside the statement, not between a read and a write.
pub async fn enqueue_understanding(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    material_id: Uuid,
) -> DbResult<Job> {
    let bureau_id = tx.bureau_id();
    let key = understanding_idempotency_key(material_id);

    let updated = sqlx::query(&format!(
        "INSERT INTO otdel.jobs \
             (bureau_id, partner_id, material_id, kind, status, idempotency_key) \
         VALUES ($1, $2, $3, 'understand_material', 'queued', $4) \
         ON CONFLICT (bureau_id, idempotency_key) DO UPDATE \
            SET status = 'queued', \
                stage = NULL, \
                error = NULL, \
                error_kind = NULL, \
                run_after = now(), \
                lease_owner = NULL, \
                lease_expires_at = NULL, \
                max_attempts = GREATEST(otdel.jobs.max_attempts, otdel.jobs.attempts + 1), \
                updated_at = now() \
          WHERE otdel.jobs.status <> 'running' \
         RETURNING {COLUMNS}"
    ))
    .bind(bureau_id)
    .bind(partner_id)
    .bind(material_id)
    .bind(&key)
    .fetch_optional(tx.conn())
    .await?;

    if let Some(row) = updated {
        return job_from_row(&row);
    }

    // The `WHERE` refused the update: the job is running right now.
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM otdel.jobs WHERE bureau_id = $1 AND idempotency_key = $2"
    ))
    .bind(bureau_id)
    .bind(&key)
    .fetch_optional(tx.conn())
    .await?
    .ok_or_else(|| {
        DbError::Decode(
            "understanding job conflicted but the existing job is not visible".to_owned(),
        )
    })?;

    job_from_row(&row)
}

/// Is there already an unfinished understanding job for this material?
///
/// Used by the extraction worker so finishing a re-read does not re-arm a run that is
/// about to happen anyway.
pub async fn understanding_pending(tx: &mut ScopedTx, material_id: Uuid) -> DbResult<bool> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(
        "SELECT EXISTS ( \
             SELECT 1 FROM otdel.jobs \
              WHERE bureau_id = $1 AND material_id = $2 \
                AND kind = 'understand_material' AND status IN ('queued', 'running') \
         ) AS pending",
    )
    .bind(bureau_id)
    .bind(material_id)
    .fetch_one(tx.conn())
    .await?;

    Ok(row.try_get::<bool, _>("pending")?)
}

// --- phase 1B: leasing ---------------------------------------------------------------

/// Take the next runnable job **of one of the given kinds**, or `None` when there is
/// none.
///
/// The kind filter is what keeps the two worker halves apart: the document reader must
/// never claim an `understand_material` row (it would try to open a knowledge job as a
/// PDF), and the product role must never claim an extraction job. Passing the kinds
/// explicitly makes that a property of the query rather than of a later `match`.
///
/// `SKIP LOCKED` means two workers never fight over the same row and never block each
/// other. The attempt counter is incremented *here*, at claim time, so a worker that
/// dies mid-run still burns an attempt and a permanently poisonous document cannot be
/// retried forever.
pub async fn claim_next(
    tx: &mut ScopedTx,
    owner: &str,
    lease: Duration,
    kinds: &[JobKind],
) -> DbResult<Option<Job>> {
    if kinds.is_empty() {
        return Ok(None);
    }
    let bureau_id = tx.bureau_id();
    let lease_seconds = i32::try_from(lease.as_secs()).unwrap_or(i32::MAX);
    let kinds: Vec<String> = kinds.iter().map(|kind| kind.as_str().to_owned()).collect();

    let row = sqlx::query(&format!(
        "UPDATE otdel.jobs SET \
                status = 'running', \
                attempts = attempts + 1, \
                stage = 'claimed', \
                lease_owner = $2, \
                lease_expires_at = now() + make_interval(secs => $3), \
                updated_at = now() \
          WHERE id = ( \
              SELECT j.id FROM otdel.jobs j \
               WHERE j.bureau_id = $1 AND j.status = 'queued' AND j.run_after <= now() \
                 AND j.kind = ANY($4) \
               ORDER BY j.run_after, j.created_at, j.id \
               FOR UPDATE SKIP LOCKED \
               LIMIT 1 \
          ) \
      RETURNING {COLUMNS}"
    ))
    .bind(bureau_id)
    .bind(owner)
    .bind(f64::from(lease_seconds))
    .bind(&kinds)
    .fetch_optional(tx.conn())
    .await?;

    row.as_ref().map(job_from_row).transpose()
}

/// Extend the lease of a job this worker still holds, and record what it is doing.
///
/// Returns `false` when the job is no longer ours — the lease expired and somebody else
/// took it. The caller must then stop writing results for it rather than racing.
pub async fn heartbeat(
    tx: &mut ScopedTx,
    job_id: Uuid,
    owner: &str,
    lease: Duration,
    stage: Option<&str>,
) -> DbResult<bool> {
    let bureau_id = tx.bureau_id();
    let lease_seconds = i32::try_from(lease.as_secs()).unwrap_or(i32::MAX);

    let result = sqlx::query(
        "UPDATE otdel.jobs SET \
                lease_expires_at = now() + make_interval(secs => $4), \
                stage = coalesce($5, stage), \
                updated_at = now() \
          WHERE bureau_id = $1 AND id = $2 AND status = 'running' AND lease_owner = $3",
    )
    .bind(bureau_id)
    .bind(job_id)
    .bind(owner)
    .bind(f64::from(lease_seconds))
    .bind(stage)
    .execute(tx.conn())
    .await?;

    Ok(result.rows_affected() == 1)
}

/// Settle a job as done.
pub async fn complete(tx: &mut ScopedTx, job_id: Uuid, owner: &str) -> DbResult<bool> {
    let bureau_id = tx.bureau_id();
    let result = sqlx::query(
        "UPDATE otdel.jobs SET \
                status = 'completed', \
                stage = NULL, \
                error = NULL, \
                error_kind = NULL, \
                lease_owner = NULL, \
                lease_expires_at = NULL, \
                updated_at = now() \
          WHERE bureau_id = $1 AND id = $2 AND status = 'running' AND lease_owner = $3",
    )
    .bind(bureau_id)
    .bind(job_id)
    .bind(owner)
    .execute(tx.conn())
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Settle a job as failed.
///
/// A *transient* failure below the attempt limit goes back to `queued` with a delay; a
/// permanent one (the file is not a PDF) stops immediately, because repeating it would
/// only burn attempts and hide the real reason behind "still retrying".
pub async fn fail(
    tx: &mut ScopedTx,
    job_id: Uuid,
    owner: &str,
    message: &str,
    permanent: bool,
    backoff: Duration,
) -> DbResult<bool> {
    let bureau_id = tx.bureau_id();
    let backoff_seconds = i32::try_from(backoff.as_secs()).unwrap_or(i32::MAX);
    let message: String = message.chars().take(2000).collect();

    let result = sqlx::query(
        "UPDATE otdel.jobs SET \
                status = CASE WHEN $4 OR attempts >= max_attempts THEN 'failed' ELSE 'queued' END, \
                error = $5, \
                error_kind = CASE WHEN $4 THEN 'permanent' ELSE 'transient' END, \
                stage = NULL, \
                run_after = now() + make_interval(secs => $6), \
                lease_owner = NULL, \
                lease_expires_at = NULL, \
                updated_at = now() \
          WHERE bureau_id = $1 AND id = $2 AND status = 'running' AND lease_owner = $3",
    )
    .bind(bureau_id)
    .bind(job_id)
    .bind(owner)
    .bind(permanent)
    .bind(&message)
    .bind(f64::from(backoff_seconds))
    .execute(tx.conn())
    .await?;

    Ok(result.rows_affected() == 1)
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
