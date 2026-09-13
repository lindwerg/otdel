//! `/api/partners` — the partner card of phase 1A.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use otdel_core::model::Partner;
use otdel_core::{validate, AppError};
use otdel_db::partners::{self, PartnerPatch};
use uuid::Uuid;

use crate::auth::Session;
use crate::dto::{CreatePartnerRequest, ItemsResponse, PatchPartnerRequest};
use crate::error::{ApiError, ApiResult};
use crate::extract::{ApiJson, ApiPath};
use crate::state::AppState;

pub async fn list(
    State(state): State<AppState>,
    session: Session,
) -> ApiResult<Json<ItemsResponse<Partner>>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    let items = partners::list(&mut tx).await?;
    tx.commit().await?;
    Ok(Json(ItemsResponse::new(items)))
}

pub async fn create(
    State(state): State<AppState>,
    session: Session,
    ApiJson(payload): ApiJson<CreatePartnerRequest>,
) -> ApiResult<Response> {
    let name = validate::partner_name(&payload.name)?;
    let note = validate::partner_note(payload.note.as_deref())?;

    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    let partner = partners::create(&mut tx, &name, note.as_deref()).await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(partner)).into_response())
}

pub async fn show(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
) -> ApiResult<Json<Partner>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    let partner = partners::get(&mut tx, partner_id).await?;
    tx.commit().await?;

    partner.map(Json).ok_or_else(partner_not_found)
}

pub async fn update(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
    ApiJson(payload): ApiJson<PatchPartnerRequest>,
) -> ApiResult<Json<Partner>> {
    let mut patch = PartnerPatch::default();
    if let Some(name) = payload.name.as_deref() {
        patch.name = Some(validate::partner_name(name)?);
    }
    if let Some(note) = payload.note {
        // `null` clears the note, a blank string is treated the same way.
        patch.note = Some(validate::partner_note(note.as_deref())?);
    }
    if patch.is_empty() {
        return Err(ApiError::new(AppError::validation(
            "patch must contain at least one of `name` or `note`",
        )));
    }

    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    let partner = partners::update(&mut tx, partner_id, &patch).await?;
    tx.commit().await?;

    partner.map(Json).ok_or_else(partner_not_found)
}

/// One wording for “does not exist” and “belongs to another bureau”: a caller must not
/// be able to probe which partner ids exist elsewhere.
pub(crate) fn partner_not_found() -> ApiError {
    ApiError::not_found("partner not found")
}
