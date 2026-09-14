//! Phase 1F — writing and reading the append-only history.
//!
//! Two properties of this module are worth stating before the code, because both are
//! deliberate and both look like omissions:
//!
//! **An event is written in the caller's transaction.** [`record`] takes a [`ScopedTx`]
//! and never opens one of its own, so the record of a thing and the thing itself commit
//! together or not at all. A log that could survive a rolled-back upload would describe a
//! material that does not exist; a log written after the commit would lose the record
//! whenever the process died in between. There is exactly one exception and it is named:
//! [`record_standalone`], used by the maintenance pass, which has no other transaction to
//! join.
//!
//! **Nothing here updates or deletes.** The runtime role has no such privilege on
//! `otdel.events` and a trigger refuses both anyway (`0007_updates.sql`). Pruning is
//! [`crate::updates::apply_retention`], which goes through a database function carrying
//! its own floor.

use chrono::{DateTime, Utc};
use otdel_core::updates::{Event, EventActor, EventKind};
use sqlx::postgres::PgRow;
use sqlx::Row;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::tenancy::ScopedTx;

/// The longest sentence the column accepts. Clipped rather than refused: losing the tail
/// of a diagnostic is better than losing the event.
const MAX_SUMMARY_CHARS: usize = 1000;

/// One thing that happened, on its way to the log.
///
/// Built with [`NewEvent::new`] and the `with_*` methods so a call site reads as the
/// sentence it is recording, and so adding an identifier later does not break every
/// caller.
#[derive(Debug, Clone)]
pub struct NewEvent {
    pub partner_id: Option<Uuid>,
    pub kind: EventKind,
    pub actor: EventActor,
    pub material_id: Option<Uuid>,
    pub version_id: Option<Uuid>,
    pub job_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub summary: String,
    pub detail: serde_json::Value,
}

impl NewEvent {
    pub fn new(kind: EventKind, actor: EventActor, summary: impl Into<String>) -> Self {
        Self {
            partner_id: None,
            kind,
            actor,
            material_id: None,
            version_id: None,
            job_id: None,
            run_id: None,
            summary: summary.into(),
            detail: serde_json::Value::Object(serde_json::Map::new()),
        }
    }

    #[must_use]
    pub fn for_partner(mut self, partner_id: Uuid) -> Self {
        self.partner_id = Some(partner_id);
        self
    }

    #[must_use]
    pub fn about_material(mut self, material_id: Uuid) -> Self {
        self.material_id = Some(material_id);
        self
    }

    #[must_use]
    pub fn about_version(mut self, version_id: Uuid) -> Self {
        self.version_id = Some(version_id);
        self
    }

    #[must_use]
    pub fn about_job(mut self, job_id: Uuid) -> Self {
        self.job_id = Some(job_id);
        self
    }

    #[must_use]
    pub fn about_run(mut self, run_id: Uuid) -> Self {
        self.run_id = Some(run_id);
        self
    }

    /// Attach the structured half. Secrets never reach it: nothing that writes an event
    /// has access to one, and the column is bounded by the schema.
    #[must_use]
    pub fn with_detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = match detail {
            value @ serde_json::Value::Object(_) => value,
            // The column requires an object. Wrapping rather than refusing keeps a
            // careless caller from losing the whole event.
            other => serde_json::json!({ "value": other }),
        };
        self
    }
}

/// Append one event inside the caller's transaction.
///
/// Returns the identifier so a caller that wants to log it can, and so a test can assert
/// the row is there without guessing.
pub async fn record(tx: &mut ScopedTx, event: &NewEvent) -> DbResult<Uuid> {
    let bureau_id = tx.bureau_id();
    let summary = clip(&event.summary);
    if summary.is_empty() {
        return Err(DbError::Decode(
            "refusing to record an event with no summary: a history line that says nothing \
             is worse than none"
                .to_owned(),
        ));
    }

    let id: Uuid = sqlx::query(
        "INSERT INTO otdel.events \
             (bureau_id, partner_id, kind, actor, material_id, version_id, job_id, run_id, \
              summary, detail) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
         RETURNING id",
    )
    .bind(bureau_id)
    .bind(event.partner_id)
    .bind(event.kind.as_str())
    .bind(event.actor.as_str())
    .bind(event.material_id)
    .bind(event.version_id)
    .bind(event.job_id)
    .bind(event.run_id)
    .bind(&summary)
    .bind(&event.detail)
    .fetch_one(tx.conn())
    .await?
    .try_get("id")?;

    Ok(id)
}

/// Append an event that has no other transaction to belong to.
///
/// The maintenance pass is the only caller: a retention sweep is not part of anybody's
/// request, and the record of it must not be inside the transaction that did the
/// deleting — the trigger that lets a retention pass delete is transaction-local, and
/// writing the sweep's own record inside that transaction would put one event under a
/// flag that exists to permit removal.
pub async fn record_standalone(
    db: &crate::Database,
    bureau_id: Uuid,
    event: &NewEvent,
) -> DbResult<Uuid> {
    let mut tx = db.begin_scoped(bureau_id).await?;
    let id = record(&mut tx, event).await?;
    tx.commit().await?;
    Ok(id)
}

/// The partner's history, newest first.
///
/// `limit` is clamped by the caller's contract rather than here; the API passes a bounded
/// value so one partner with a long history cannot be asked for in one response.
pub async fn list_for_partner(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    limit: i64,
    before: Option<DateTime<Utc>>,
) -> DbResult<Vec<Event>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT * FROM otdel.events \
          WHERE bureau_id = $1 AND partner_id = $2 \
            AND ($4::timestamptz IS NULL OR occurred_at < $4) \
          ORDER BY occurred_at DESC, id DESC \
          LIMIT $3",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .bind(limit)
    .bind(before)
    .fetch_all(tx.conn())
    .await?;

    rows.iter().map(event_from_row).collect()
}

/// The most recent event of one kind in the whole bureau.
///
/// Used for the retention sweep's own record: the policy view shows when the last sweep
/// ran and what it removed, taken from the log rather than from a counter somebody has to
/// remember to update.
pub async fn latest_of_kind(tx: &mut ScopedTx, kind: EventKind) -> DbResult<Option<Event>> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(
        "SELECT * FROM otdel.events \
          WHERE bureau_id = $1 AND kind = $2 \
          ORDER BY occurred_at DESC, id DESC LIMIT 1",
    )
    .bind(bureau_id)
    .bind(kind.as_str())
    .fetch_optional(tx.conn())
    .await?;

    row.as_ref().map(event_from_row).transpose()
}

/// How many events of one kind a partner has. The refresh status uses it to tell "no
/// check has ever run" from "a check ran and published nothing".
pub async fn count_for_partner(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    kind: EventKind,
) -> DbResult<i64> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(
        "SELECT count(*) AS total FROM otdel.events \
          WHERE bureau_id = $1 AND partner_id = $2 AND kind = $3",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .bind(kind.as_str())
    .fetch_one(tx.conn())
    .await?;
    Ok(row.try_get("total")?)
}

fn event_from_row(row: &PgRow) -> DbResult<Event> {
    let kind: String = row.try_get("kind")?;
    let actor: String = row.try_get("actor")?;
    Ok(Event {
        id: row.try_get("id")?,
        partner_id: row.try_get("partner_id")?,
        kind: EventKind::parse(&kind)
            .ok_or_else(|| DbError::Decode(format!("unknown event kind `{kind}`")))?,
        actor: EventActor::parse(&actor)
            .ok_or_else(|| DbError::Decode(format!("unknown event actor `{actor}`")))?,
        material_id: row.try_get("material_id")?,
        version_id: row.try_get("version_id")?,
        job_id: row.try_get("job_id")?,
        run_id: row.try_get("run_id")?,
        summary: row.try_get("summary")?,
        detail: row.try_get("detail")?,
        occurred_at: row.try_get::<DateTime<Utc>, _>("occurred_at")?,
    })
}

/// Trim to what the column accepts, counting characters rather than bytes.
fn clip(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= MAX_SUMMARY_CHARS {
        return trimmed.to_owned();
    }
    trimmed.chars().take(MAX_SUMMARY_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_summary_longer_than_the_column_is_clipped_by_characters() {
        // Cyrillic is two bytes per character in UTF-8; clipping by bytes would split one
        // and produce a string the database refuses.
        let long: String = "я".repeat(MAX_SUMMARY_CHARS + 50);
        let clipped = clip(&long);
        assert_eq!(clipped.chars().count(), MAX_SUMMARY_CHARS);
        assert!(clipped.is_char_boundary(clipped.len()));
    }

    #[test]
    fn surrounding_whitespace_is_not_part_of_the_record() {
        assert_eq!(clip("  версия опубликована  "), "версия опубликована");
        assert!(clip("   ").is_empty());
    }

    #[test]
    fn a_detail_that_is_not_an_object_is_wrapped_rather_than_dropped() {
        // The column requires an object. A caller passing an array should not lose the
        // event over it.
        let event = NewEvent::new(EventKind::ExportRead, EventActor::Owner, "выгрузка")
            .with_detail(serde_json::json!([1, 2, 3]));
        assert!(event.detail.is_object());
        assert_eq!(event.detail["value"], serde_json::json!([1, 2, 3]));

        let event = NewEvent::new(EventKind::ExportRead, EventActor::Owner, "выгрузка")
            .with_detail(serde_json::json!({"version": 2}));
        assert_eq!(event.detail["version"], 2);
    }

    #[test]
    fn a_new_event_carries_no_identifier_it_was_not_given() {
        let event = NewEvent::new(
            EventKind::ValidationQueued,
            EventActor::Owner,
            "проверка поставлена в очередь",
        );
        assert!(event.partner_id.is_none());
        assert!(event.material_id.is_none());
        assert!(event.version_id.is_none());
        assert!(event.job_id.is_none());
        assert!(event.run_id.is_none());
        assert!(event.detail.is_object());
    }
}
