//! `GET /api/partners/{id}/jobs` — the queue as it really is.
//!
//! Phase 1A records extraction jobs but does not run them: the rows returned here are
//! `queued` until the 1B worker exists. Nothing invents progress.

use axum::extract::State;
use axum::Json;
use otdel_core::model::Job;
use otdel_db::{jobs, partners};
use uuid::Uuid;

use crate::auth::Session;
use crate::dto::ItemsResponse;
use crate::error::ApiResult;
use crate::extract::ApiPath;
use crate::routes::partners::partner_not_found;
use crate::state::AppState;

pub async fn list(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
) -> ApiResult<Json<ItemsResponse<Job>>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }
    let items = jobs::list_for_partner(&mut tx, partner_id).await?;
    tx.commit().await?;

    Ok(Json(ItemsResponse::new(items)))
}
