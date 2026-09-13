//! Material repository (original uploads).
//!
//! Two rules from the contract are enforced here rather than in the handler:
//!
//! * a material is always looked up *together with its partner* — a material id from
//!   another partner (or another bureau) simply does not match;
//! * deduplication is `(partner_id, sha256)`; the database constraint is the source of
//!   truth, so two concurrent uploads of the same bytes cannot both insert.

use chrono::{DateTime, Utc};
use otdel_core::extraction::ExtractionSummary;
use otdel_core::model::{Material, MaterialStatus};
use sqlx::postgres::PgRow;
use sqlx::Row;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::tenancy::ScopedTx;

const COLUMNS: &str =
    "id, partner_id, filename, media_type, size_bytes, sha256, status, page_count, created_at, error";

/// Phase 1B bookkeeping of the extraction run, prefixed for the joined query below.
const EXTRACTION_COLUMNS: &str = "m.extraction_started_at, m.extraction_finished_at, \
     m.parser_name, m.parser_version, m.ocr_engine, m.ocr_version, m.extraction_diagnostic";

/// A material plus the storage key, which is server-internal and never serialised.
#[derive(Debug, Clone)]
pub struct StoredMaterial {
    pub material: Material,
    pub storage_key: String,
}

/// Values needed to record a freshly uploaded original.
#[derive(Debug, Clone)]
pub struct NewMaterial {
    pub partner_id: Uuid,
    pub filename: String,
    pub media_type: String,
    pub size_bytes: i64,
    pub sha256: String,
    pub storage_key: String,
}

/// Result of an insert attempt: the upload may have raced with an identical one.
#[derive(Debug, Clone)]
pub enum InsertOutcome {
    Created(Material),
    Duplicate(Material),
}

fn material_from_row(row: &PgRow) -> DbResult<Material> {
    let status: String = row.try_get("status")?;
    let status = MaterialStatus::parse(&status)
        .ok_or_else(|| DbError::Decode(format!("unknown material status `{status}`")))?;

    Ok(Material {
        id: row.try_get("id")?,
        partner_id: row.try_get("partner_id")?,
        filename: row.try_get("filename")?,
        media_type: row.try_get("media_type")?,
        size_bytes: row.try_get("size_bytes")?,
        sha256: row.try_get("sha256")?,
        status,
        page_count: row.try_get("page_count")?,
        created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
        error: row.try_get("error")?,
        // Filled in only by the queries that join the page counts: a material read
        // without them reports "no summary", never an empty one that would read as
        // "zero pages, nothing wrong".
        extraction: None,
    })
}

pub async fn list_for_partner(tx: &mut ScopedTx, partner_id: Uuid) -> DbResult<Vec<Material>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM otdel.materials WHERE bureau_id = $1 AND partner_id = $2 \
         ORDER BY created_at DESC, id DESC"
    ))
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter().map(material_from_row).collect()
}

/// Per-partner deduplication lookup.
pub async fn find_by_digest(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    sha256: &str,
) -> DbResult<Option<Material>> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM otdel.materials \
          WHERE bureau_id = $1 AND partner_id = $2 AND sha256 = $3"
    ))
    .bind(bureau_id)
    .bind(partner_id)
    .bind(sha256)
    .fetch_optional(tx.conn())
    .await?;

    row.as_ref().map(material_from_row).transpose()
}

/// Material together with its storage key, scoped to the partner in the route.
pub async fn get_in_partner(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    material_id: Uuid,
) -> DbResult<Option<StoredMaterial>> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS}, storage_key FROM otdel.materials \
          WHERE bureau_id = $1 AND partner_id = $2 AND id = $3"
    ))
    .bind(bureau_id)
    .bind(partner_id)
    .bind(material_id)
    .fetch_optional(tx.conn())
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(StoredMaterial {
        material: material_from_row(&row)?,
        storage_key: row.try_get("storage_key")?,
    }))
}

/// Insert the metadata of an already-written object.
///
/// A unique violation on `(partner_id, sha256)` means a concurrent request stored the
/// same bytes first; the existing row is returned instead of an error, which keeps the
/// endpoint idempotent for identical uploads.
pub async fn insert(tx: &mut ScopedTx, new_material: &NewMaterial) -> DbResult<InsertOutcome> {
    let bureau_id = tx.bureau_id();
    // `ON CONFLICT DO NOTHING` rather than catching the error: a failed statement would
    // poison the transaction, and the object has already been written at this point.
    let inserted = sqlx::query(&format!(
        "INSERT INTO otdel.materials \
             (bureau_id, partner_id, filename, media_type, size_bytes, sha256, storage_key, status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'queued') \
         ON CONFLICT (partner_id, sha256) DO NOTHING \
         RETURNING {COLUMNS}"
    ))
    .bind(bureau_id)
    .bind(new_material.partner_id)
    .bind(&new_material.filename)
    .bind(&new_material.media_type)
    .bind(new_material.size_bytes)
    .bind(&new_material.sha256)
    .bind(&new_material.storage_key)
    .fetch_optional(tx.conn())
    .await?;

    if let Some(row) = inserted {
        return Ok(InsertOutcome::Created(material_from_row(&row)?));
    }

    let existing = find_by_digest(tx, new_material.partner_id, &new_material.sha256)
        .await?
        .ok_or_else(|| {
            DbError::Decode(
                "insert reported a duplicate but the existing material is not visible".to_owned(),
            )
        })?;
    Ok(InsertOutcome::Duplicate(existing))
}

/// Move a material back into the queue (used by the retry endpoint).
///
/// The recorded pages are deliberately **not** deleted here. A retry re-reads the
/// document and updates each page in place; keeping the old rows means that if the
/// retry is interrupted the material still shows what was known before, instead of
/// briefly having no pages at all.
pub async fn requeue(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    material_id: Uuid,
) -> DbResult<Option<Material>> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(&format!(
        "UPDATE otdel.materials \
            SET status = 'queued', error = NULL, extraction_diagnostic = NULL, updated_at = now() \
          WHERE bureau_id = $1 AND partner_id = $2 AND id = $3 \
      RETURNING {COLUMNS}"
    ))
    .bind(bureau_id)
    .bind(partner_id)
    .bind(material_id)
    .fetch_optional(tx.conn())
    .await?;

    row.as_ref().map(material_from_row).transpose()
}

// --- phase 1B: extraction bookkeeping ------------------------------------------------

/// Materials of a partner, each with the roll-up of its page outcomes.
///
/// The counters come from `otdel.material_pages` in the same statement, so they are a
/// view of the pages rather than a second, independently written record that could
/// disagree with them.
pub async fn list_for_partner_with_extraction(
    tx: &mut ScopedTx,
    partner_id: Uuid,
) -> DbResult<Vec<Material>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(&format!(
        "SELECT {prefixed}, {EXTRACTION_COLUMNS}, {COUNT_COLUMNS} \
           FROM otdel.materials m {COUNT_JOIN} \
          WHERE m.bureau_id = $1 AND m.partner_id = $2 \
          ORDER BY m.created_at DESC, m.id DESC",
        prefixed = prefixed_columns(),
    ))
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter().map(material_with_extraction).collect()
}

/// One material of a partner, with the same roll-up.
pub async fn get_in_partner_with_extraction(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    material_id: Uuid,
) -> DbResult<Option<Material>> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(&format!(
        "SELECT {prefixed}, {EXTRACTION_COLUMNS}, {COUNT_COLUMNS} \
           FROM otdel.materials m {COUNT_JOIN} \
          WHERE m.bureau_id = $1 AND m.partner_id = $2 AND m.id = $3",
        prefixed = prefixed_columns(),
    ))
    .bind(bureau_id)
    .bind(partner_id)
    .bind(material_id)
    .fetch_optional(tx.conn())
    .await?;

    row.as_ref().map(material_with_extraction).transpose()
}

/// Mark the start of an extraction run.
pub async fn begin_extraction(
    tx: &mut ScopedTx,
    material_id: Uuid,
    parser_name: &str,
    parser_version: &str,
) -> DbResult<()> {
    let bureau_id = tx.bureau_id();
    sqlx::query(
        "UPDATE otdel.materials \
            SET status = 'processing', \
                error = NULL, \
                extraction_diagnostic = NULL, \
                extraction_started_at = now(), \
                extraction_finished_at = NULL, \
                parser_name = $3, \
                parser_version = $4, \
                updated_at = now() \
          WHERE bureau_id = $1 AND id = $2",
    )
    .bind(bureau_id)
    .bind(material_id)
    .bind(parser_name)
    .bind(parser_version)
    .execute(tx.conn())
    .await?;
    Ok(())
}

/// Record the page count discovered by the inventory pass.
pub async fn set_page_count(tx: &mut ScopedTx, material_id: Uuid, page_count: i32) -> DbResult<()> {
    let bureau_id = tx.bureau_id();
    sqlx::query(
        "UPDATE otdel.materials SET page_count = $3, updated_at = now() \
          WHERE bureau_id = $1 AND id = $2",
    )
    .bind(bureau_id)
    .bind(material_id)
    .bind(page_count)
    .execute(tx.conn())
    .await?;
    Ok(())
}

/// Settle a material after a run.
///
/// `status` is always the value derived from the page outcomes
/// ([`otdel_core::extraction::aggregate_material_status`]) — this function does not
/// decide it, it stores it.
pub async fn finish_extraction(
    tx: &mut ScopedTx,
    material_id: Uuid,
    status: MaterialStatus,
    diagnostic: Option<&str>,
    ocr_engine: Option<&str>,
    ocr_version: Option<&str>,
) -> DbResult<()> {
    let bureau_id = tx.bureau_id();
    sqlx::query(
        "UPDATE otdel.materials \
            SET status = $3, \
                extraction_finished_at = now(), \
                extraction_diagnostic = $4, \
                error = $4, \
                ocr_engine = coalesce($5, ocr_engine), \
                ocr_version = coalesce($6, ocr_version), \
                updated_at = now() \
          WHERE bureau_id = $1 AND id = $2",
    )
    .bind(bureau_id)
    .bind(material_id)
    .bind(status.as_str())
    .bind(diagnostic)
    .bind(ocr_engine)
    .bind(ocr_version)
    .execute(tx.conn())
    .await?;
    Ok(())
}

/// Per-status page counters, computed next to the material row.
const COUNT_COLUMNS: &str = "coalesce(pages.total, 0) AS pages_total, \
     coalesce(pages.extracted, 0) AS pages_extracted, \
     coalesce(pages.empty, 0) AS pages_empty, \
     coalesce(pages.needs_ocr, 0) AS pages_needs_ocr, \
     coalesce(pages.partial, 0) AS pages_partial, \
     coalesce(pages.failed, 0) AS pages_failed, \
     coalesce(pages.pending, 0) AS pages_pending";

const COUNT_JOIN: &str = "LEFT JOIN LATERAL ( \
         SELECT count(*) AS total, \
                count(*) FILTER (WHERE p.status = 'extracted') AS extracted, \
                count(*) FILTER (WHERE p.status = 'empty')     AS empty, \
                count(*) FILTER (WHERE p.status = 'needs_ocr') AS needs_ocr, \
                count(*) FILTER (WHERE p.status = 'partial')   AS partial, \
                count(*) FILTER (WHERE p.status = 'failed')    AS failed, \
                count(*) FILTER (WHERE p.status = 'pending')   AS pending \
           FROM otdel.material_pages p \
          WHERE p.bureau_id = m.bureau_id AND p.material_id = m.id \
     ) pages ON true";

fn prefixed_columns() -> String {
    COLUMNS
        .split(", ")
        .map(|column| format!("m.{column}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn material_with_extraction(row: &PgRow) -> DbResult<Material> {
    let mut material = material_from_row(row)?;
    let pages_total: i64 = row.try_get("pages_total")?;
    let started_at: Option<DateTime<Utc>> = row.try_get("extraction_started_at")?;

    // No pages and no run: the material has genuinely not been read, and says so with
    // `null` rather than with a summary full of zeros.
    if pages_total == 0 && started_at.is_none() {
        return Ok(material);
    }

    material.extraction = Some(ExtractionSummary {
        pages_total: count(row, "pages_total")?,
        pages_extracted: count(row, "pages_extracted")?,
        pages_empty: count(row, "pages_empty")?,
        pages_needs_ocr: count(row, "pages_needs_ocr")?,
        pages_partial: count(row, "pages_partial")?,
        pages_failed: count(row, "pages_failed")?,
        pages_pending: count(row, "pages_pending")?,
        parser_name: row.try_get("parser_name")?,
        parser_version: row.try_get("parser_version")?,
        ocr_engine: row.try_get("ocr_engine")?,
        ocr_version: row.try_get("ocr_version")?,
        started_at,
        finished_at: row.try_get("extraction_finished_at")?,
        diagnostic: row.try_get("extraction_diagnostic")?,
    });
    Ok(material)
}

fn count(row: &PgRow, column: &str) -> DbResult<i32> {
    let value: i64 = row.try_get(column)?;
    i32::try_from(value).map_err(|_| DbError::Decode(format!("page counter `{column}` overflowed")))
}
