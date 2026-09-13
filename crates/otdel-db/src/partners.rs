//! Partner repository. Every statement runs inside a [`ScopedTx`], so row-level
//! security restricts it to the caller's bureau even though the SQL below also filters
//! by `bureau_id` explicitly (defence in depth, and it keeps the indexes useful).

use chrono::{DateTime, Utc};
use otdel_core::model::Partner;
use sqlx::postgres::PgRow;
use sqlx::Row;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::tenancy::ScopedTx;

const COLUMNS: &str = "id, name, note, created_at, updated_at";

pub(crate) fn partner_from_row(row: &PgRow) -> DbResult<Partner> {
    Ok(Partner {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        note: row.try_get("note")?,
        created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
        updated_at: row.try_get::<DateTime<Utc>, _>("updated_at")?,
    })
}

/// Newest first — the intake screen shows the most recently added partners on top.
pub async fn list(tx: &mut ScopedTx) -> DbResult<Vec<Partner>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM otdel.partners WHERE bureau_id = $1 \
         ORDER BY created_at DESC, id DESC"
    ))
    .bind(bureau_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter().map(partner_from_row).collect()
}

pub async fn create(tx: &mut ScopedTx, name: &str, note: Option<&str>) -> DbResult<Partner> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(&format!(
        "INSERT INTO otdel.partners (bureau_id, name, note) VALUES ($1, $2, $3) \
         RETURNING {COLUMNS}"
    ))
    .bind(bureau_id)
    .bind(name)
    .bind(note)
    .fetch_one(tx.conn())
    .await?;

    partner_from_row(&row)
}

pub async fn get(tx: &mut ScopedTx, partner_id: Uuid) -> DbResult<Option<Partner>> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM otdel.partners WHERE bureau_id = $1 AND id = $2"
    ))
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_optional(tx.conn())
    .await?;

    row.as_ref().map(partner_from_row).transpose()
}

/// Fields the caller actually sent. `note: Some(None)` clears the note, `None` leaves it.
#[derive(Debug, Clone, Default)]
pub struct PartnerPatch {
    pub name: Option<String>,
    pub note: Option<Option<String>>,
}

impl PartnerPatch {
    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.note.is_none()
    }
}

pub async fn update(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    patch: &PartnerPatch,
) -> DbResult<Option<Partner>> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(&format!(
        "UPDATE otdel.partners \
            SET name = COALESCE($3::text, name), \
                note = CASE WHEN $4::boolean THEN $5::text ELSE note END, \
                updated_at = now() \
          WHERE bureau_id = $1 AND id = $2 \
      RETURNING {COLUMNS}"
    ))
    .bind(bureau_id)
    .bind(partner_id)
    .bind(patch.name.as_deref())
    .bind(patch.note.is_some())
    .bind(patch.note.clone().flatten())
    .fetch_optional(tx.conn())
    .await?;

    row.as_ref().map(partner_from_row).transpose()
}

/// Existence check used before touching materials/jobs of a partner, so a request for
/// an unknown or foreign partner fails with 404 before any storage work happens.
pub async fn exists(tx: &mut ScopedTx, partner_id: Uuid) -> DbResult<bool> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(
        "SELECT EXISTS (SELECT 1 FROM otdel.partners WHERE bureau_id = $1 AND id = $2) AS present",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_one(tx.conn())
    .await?;
    row.try_get::<bool, _>("present").map_err(DbError::from)
}
