//! Session records.
//!
//! The runtime role has **no** privileges on `otdel.sessions`; everything here goes
//! through `SECURITY DEFINER` functions created by the migration. That is deliberate:
//!
//! * authentication happens before any bureau context exists, so it cannot be expressed
//!   as a row-level-security policy;
//! * a SQL injection or logic bug in the application still cannot read or forge session
//!   rows, because the role simply cannot select from the table.
//!
//! Only the SHA-256 fingerprint of the opaque token is ever sent to the database.

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::{DbError, DbResult};

/// Server-side session state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub session_id: Uuid,
    pub bureau_id: Uuid,
    pub csrf_token: String,
    pub expires_at: DateTime<Utc>,
}

fn record_from_row(row: &sqlx::postgres::PgRow) -> DbResult<SessionRecord> {
    Ok(SessionRecord {
        session_id: row.try_get("session_id")?,
        bureau_id: row.try_get("bureau_id")?,
        csrf_token: row.try_get("csrf_token")?,
        expires_at: row.try_get::<DateTime<Utc>, _>("expires_at")?,
    })
}

/// Create a session for the bureau identified by `bureau_slug`.
pub async fn open(
    pool: &PgPool,
    bureau_slug: &str,
    token_fingerprint: &str,
    csrf_token: &str,
    ttl_seconds: i32,
) -> DbResult<SessionRecord> {
    let row = sqlx::query(
        "SELECT session_id, bureau_id, csrf_token, expires_at \
           FROM otdel.session_open($1, $2, $3, $4)",
    )
    .bind(bureau_slug)
    .bind(token_fingerprint)
    .bind(csrf_token)
    .bind(ttl_seconds)
    .fetch_one(pool)
    .await?;

    record_from_row(&row)
}

/// Validate a token fingerprint and refresh the idle timer.
///
/// `Ok(None)` means unknown, expired or idle for too long — all indistinguishable to
/// the client, which always sees a plain 401.
pub async fn touch(
    pool: &PgPool,
    token_fingerprint: &str,
    idle_timeout_seconds: i32,
) -> DbResult<Option<SessionRecord>> {
    let row = sqlx::query(
        "SELECT session_id, bureau_id, csrf_token, expires_at \
           FROM otdel.session_touch($1, $2)",
    )
    .bind(token_fingerprint)
    .bind(idle_timeout_seconds)
    .fetch_optional(pool)
    .await?;

    row.as_ref().map(record_from_row).transpose()
}

/// End a session. Returns `true` if a record was removed.
pub async fn close(pool: &PgPool, token_fingerprint: &str) -> DbResult<bool> {
    let row = sqlx::query("SELECT otdel.session_close($1) AS removed")
        .bind(token_fingerprint)
        .fetch_one(pool)
        .await?;
    Ok(row.try_get::<i32, _>("removed")? > 0)
}

/// Delete expired/idle sessions (maintenance worker).
pub async fn purge_expired(pool: &PgPool, idle_timeout_seconds: i32) -> DbResult<i64> {
    let row = sqlx::query("SELECT otdel.session_purge_expired($1) AS removed")
        .bind(idle_timeout_seconds)
        .fetch_one(pool)
        .await?;
    let removed: i32 = row.try_get("removed").map_err(DbError::from)?;
    Ok(i64::from(removed))
}
