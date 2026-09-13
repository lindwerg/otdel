//! Phase 1F — the partner's history, and how long it is kept.
//!
//! Two reads and no writes. The log is append-only in the database — the runtime role has
//! no `UPDATE` or `DELETE` grant and a trigger refuses both for every writer — so there is
//! deliberately no endpoint that edits or removes a line. Entries leave only through the
//! retention sweep, which records that it ran.

use axum::extract::State;
use axum::Json;
use chrono::{DateTime, Utc};
use otdel_core::updates::{retention_protected, Event, EventKind, RetentionPolicyView};
use otdel_db::{events, partners, updates};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::Session;
use crate::dto::ItemsResponse;
use crate::error::ApiResult;
use crate::extract::{ApiPath, ApiQuery};
use crate::routes::partners::partner_not_found;
use crate::state::AppState;

/// How much history one request may ask for. A partner's log grows without bound, so the
/// endpoint is paged rather than "everything, ordered by time".
const MAX_EVENTS: i64 = 200;
const DEFAULT_EVENTS: i64 = 50;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventQuery {
    pub limit: Option<i64>,
    /// Page backwards: everything strictly older than this moment.
    pub before: Option<DateTime<Utc>>,
}

// --- history -------------------------------------------------------------------------

/// `GET /api/partners/{id}/events`
pub async fn events(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<EventQuery>,
) -> ApiResult<Json<ItemsResponse<Event>>> {
    let limit = query.limit.unwrap_or(DEFAULT_EVENTS).clamp(1, MAX_EVENTS);

    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }
    let items = events::list_for_partner(&mut tx, partner_id, limit, query.before).await?;
    tx.commit().await?;

    Ok(Json(ItemsResponse::new(items)))
}

// --- retention -------------------------------------------------------------------------

/// `GET /api/retention`
///
/// The policy, what it protects, and what a sweep would remove right now.
///
/// The preview is computed rather than remembered, so turning a horizon on is not a
/// button whose effect is visible only afterwards.
pub async fn retention(
    State(state): State<AppState>,
    session: Session,
) -> ApiResult<Json<RetentionPolicyView>> {
    let settings = &state.config.retention;
    let horizons = settings.horizons();

    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    let preview = updates::retention_preview(&mut tx, horizons).await?;
    let last_sweep = events::latest_of_kind(&mut tx, EventKind::RetentionApplied).await?;
    tx.commit().await?;

    Ok(Json(RetentionPolicyView {
        state: settings.state(),
        event_days: settings.event_days,
        job_days: settings.job_days,
        keep_per_kind: settings.keep_per_kind,
        sweep_interval_seconds: settings.sweep_interval.as_secs(),
        preview,
        protected: retention_protected(),
        last_sweep,
        message: settings.message(),
    }))
}
