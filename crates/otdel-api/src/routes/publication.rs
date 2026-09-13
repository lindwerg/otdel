//! Phase 1E — the check, the versions it produces, and withdrawing one.
//!
//! No handler here decides anything about knowledge. Starting a check queues a job;
//! the rules live in `otdel-publish` and run in the worker, where they can be bounded and
//! where a lease protects them from a second copy of themselves. What the API does is
//! scope, refuse and report.
//!
//! The one thing worth reading twice is [`retract`]. Retraction is the only operation in
//! block 1 that takes something away from other agents, and it takes effect the moment
//! the transaction commits: search resolves the published pointer on every request, so a
//! withdrawn version stops answering immediately rather than at the next rebuild. The
//! snapshot itself is kept — the database refuses to delete a version that was ever
//! published — because "откат не воскрешает отозванные источники" needs the retracted
//! version to still exist to point at.

use axum::extract::State;
use otdel_core::error::{AppError, ErrorCode};
use otdel_core::publication::{
    CandidateSummary, KnowledgeVersion, ValidationRun, VersionClaim, VersionGap,
};
use otdel_db::{jobs, partners, publication, publication_read};
use serde::{Deserialize, Serialize};
use tracing::info;
use uuid::Uuid;

use crate::auth::Session;
use crate::dto::ItemsResponse;
use crate::error::{ApiError, ApiResult};
use crate::extract::{ApiJson, ApiPath};
use crate::state::AppState;

use super::retrieval_state::{apply_vector_state, describe_retrieval, RetrievalProviderResponse};

/// `GET /api/partners/{id}/validation` — everything the interface needs on one screen.
#[derive(Debug, Serialize)]
pub struct ValidationOverview {
    pub provider: RetrievalProviderResponse,
    pub published: Option<KnowledgeVersion>,
    pub runs: Vec<ValidationRun>,
    pub versions: Vec<KnowledgeVersion>,
    pub candidates: CandidateSummary,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetractRequest {
    pub reason: String,
}

pub async fn overview(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
) -> ApiResult<axum::Json<ValidationOverview>> {
    let mut provider = describe_retrieval(&state);

    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }
    // `search_mode` is the actual mode, not the intention: without pgvector a configured
    // embedding provider still cannot produce a hybrid search, and this page must not say
    // otherwise.
    apply_vector_state(&mut tx, &mut provider).await?;
    let published = publication_read::find_published(&mut tx, partner_id).await?;
    let versions = publication_read::list_versions(&mut tx, partner_id).await?;
    let candidates = publication_read::candidate_summary(&mut tx, partner_id).await?;
    // One run row per partner, as in 1C. The list shape is kept so the interface does not
    // have to special-case "no check has ever been made".
    let runs = publication_read::find_run(&mut tx, partner_id)
        .await?
        .into_iter()
        .collect();
    tx.commit().await?;

    Ok(axum::Json(ValidationOverview {
        provider,
        published,
        runs,
        versions,
        candidates,
    }))
}

pub async fn list(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
) -> ApiResult<axum::Json<ItemsResponse<KnowledgeVersion>>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }
    let items = publication_read::list_versions(&mut tx, partner_id).await?;
    tx.commit().await?;
    Ok(axum::Json(ItemsResponse::new(items)))
}

pub async fn get(
    State(state): State<AppState>,
    session: Session,
    ApiPath((partner_id, version_id)): ApiPath<(Uuid, Uuid)>,
) -> ApiResult<axum::Json<KnowledgeVersion>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    // Resolving the version *through* the partner is what keeps a version id from
    // another workspace from returning anything.
    let version = publication_read::find_version(&mut tx, partner_id, version_id)
        .await?
        .ok_or_else(version_not_found)?;
    tx.commit().await?;
    Ok(axum::Json(version))
}

pub async fn claims(
    State(state): State<AppState>,
    session: Session,
    ApiPath((partner_id, version_id)): ApiPath<(Uuid, Uuid)>,
) -> ApiResult<axum::Json<ItemsResponse<VersionClaim>>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    publication_read::find_version(&mut tx, partner_id, version_id)
        .await?
        .ok_or_else(version_not_found)?;
    let items = publication_read::list_claims(&mut tx, version_id).await?;
    tx.commit().await?;
    Ok(axum::Json(ItemsResponse::new(items)))
}

pub async fn gaps(
    State(state): State<AppState>,
    session: Session,
    ApiPath((partner_id, version_id)): ApiPath<(Uuid, Uuid)>,
) -> ApiResult<axum::Json<ItemsResponse<VersionGap>>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    publication_read::find_version(&mut tx, partner_id, version_id)
        .await?
        .ok_or_else(version_not_found)?;
    let items = publication_read::list_version_gaps(&mut tx, version_id).await?;
    tx.commit().await?;
    Ok(axum::Json(ItemsResponse::new(items)))
}

/// `POST /api/partners/{id}/validate`
///
/// Queues the checker over everything this partner has. Idempotent: while a check is
/// queued or running, pressing the button again returns that run instead of starting a
/// second one.
///
/// There is **no provider gate here**, and its absence is the point. Verification and
/// publication are deterministic (`block-01-spec.md` §6.7), so unlike 1C's
/// `.../understand` and 1D's `.../plan`, this button works with nothing configured. What
/// it refuses is a partner with no candidates: a check of nothing would produce a version
/// of nothing and tell the owner nothing about why.
pub async fn validate(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
) -> ApiResult<axum::Json<ValidationRun>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }

    let candidates = publication_read::candidate_summary(&mut tx, partner_id).await?;
    if candidates.is_empty() {
        return Err(ApiError::new(AppError::new(
            ErrorCode::Conflict,
            "проверять нечего: у партнёра нет ни одного кандидата. Загрузите материал, \
             дождитесь чтения и разбора, затем повторите",
        )));
    }

    // Already scheduled: return the run that exists rather than arming a second one.
    if let Some(run) = publication_read::find_run(&mut tx, partner_id).await? {
        if run.status.is_active() {
            tx.commit().await?;
            return Ok(axum::Json(run));
        }
    }

    let run = publication::enqueue_run(&mut tx, partner_id, otdel_publish::PROMPT_PROFILE).await?;
    let job = jobs::enqueue_validation(&mut tx, partner_id, run.id).await?;
    let run = publication_read::find_run(&mut tx, partner_id)
        .await?
        .ok_or_else(|| ApiError::new(AppError::internal("validation run disappeared")))?;
    tx.commit().await?;

    info!(
        partner_id = %partner_id,
        job_id = %job.id,
        "partner queued for verification and publication by the owner"
    );
    Ok(axum::Json(run))
}

/// `POST /api/partners/{id}/versions/{version_id}/retract`
///
/// Withdraw the published version. A reason is required: a retraction without one is
/// indistinguishable from a malfunction, and the version keeps that sentence for ever.
///
/// Takes effect immediately for every reader, because search resolves the published
/// pointer per request rather than caching it.
pub async fn retract(
    State(state): State<AppState>,
    session: Session,
    ApiPath((partner_id, version_id)): ApiPath<(Uuid, Uuid)>,
    ApiJson(payload): ApiJson<RetractRequest>,
) -> ApiResult<axum::Json<KnowledgeVersion>> {
    let reason = payload.reason.trim();
    if reason.is_empty() || reason.chars().count() > 1_000 {
        return Err(ApiError::new(AppError::validation(
            "причина отзыва обязательна и не длиннее 1000 символов: отзыв без причины \
             невозможно отличить от сбоя",
        )));
    }

    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    let version = publication_read::find_version(&mut tx, partner_id, version_id)
        .await?
        .ok_or_else(version_not_found)?;

    if !publication::can_retract(version.status) {
        return Err(ApiError::new(AppError::new(
            ErrorCode::Conflict,
            format!(
                "версия в состоянии `{}` не может быть отозвана: отзывается только текущая \
                 опубликованная версия",
                version.status.as_str()
            ),
        )));
    }

    if !publication::retract_version(&mut tx, partner_id, version_id, reason).await? {
        return Err(ApiError::new(AppError::new(
            ErrorCode::Conflict,
            "версия перестала быть опубликованной, пока выполнялся запрос",
        )));
    }

    let version = publication_read::find_version(&mut tx, partner_id, version_id)
        .await?
        .ok_or_else(version_not_found)?;
    tx.commit().await?;

    info!(
        partner_id = %partner_id,
        version_id = %version_id,
        "published knowledge version retracted by the owner; it is out of search now"
    );
    Ok(axum::Json(version))
}

fn partner_not_found() -> ApiError {
    ApiError::not_found("partner not found in this workspace")
}

fn version_not_found() -> ApiError {
    ApiError::not_found("knowledge version not found for this partner")
}
