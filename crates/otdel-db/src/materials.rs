//! Material repository (original uploads).
//!
//! Two rules from the contract are enforced here rather than in the handler:
//!
//! * a material is always looked up *together with its partner* — a material id from
//!   another partner (or another bureau) simply does not match;
//! * deduplication is `(partner_id, sha256)`; the database constraint is the source of
//!   truth, so two concurrent uploads of the same bytes cannot both insert.

use chrono::{DateTime, Utc};
use otdel_core::model::{Material, MaterialStatus};
use sqlx::postgres::PgRow;
use sqlx::Row;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::tenancy::ScopedTx;

const COLUMNS: &str =
    "id, partner_id, filename, media_type, size_bytes, sha256, status, page_count, created_at, error";

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
pub async fn requeue(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    material_id: Uuid,
) -> DbResult<Option<Material>> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(&format!(
        "UPDATE otdel.materials \
            SET status = 'queued', error = NULL, updated_at = now() \
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
