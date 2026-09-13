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
use axum::Json;
use chrono::Utc;
use otdel_core::error::{AppError, ErrorCode};
use otdel_core::publication::{
    CandidateSummary, KnowledgeVersion, ValidationRun, VersionClaim, VersionGap, VersionStatus,
};
use otdel_core::updates::{
    export_disclosure, EventActor, EventKind, ExportManifest, VersionChanges, VersionRef,
    EXPORT_SCHEMA,
};
use otdel_db::{events, jobs, partners, publication, publication_read, updates};
use otdel_publish::diff;
use serde::{Deserialize, Serialize};
use tracing::info;
use uuid::Uuid;

use crate::auth::Session;
use crate::dto::ItemsResponse;
use crate::error::{ApiError, ApiResult};
use crate::extract::{ApiJson, ApiPath, ApiQuery};
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangesQuery {
    /// The older version to compare with. Defaults to the previously published one.
    pub against: Option<Uuid>,
}

/// A version named the way a 1F response refers to one.
fn version_ref(version: &KnowledgeVersion) -> VersionRef {
    VersionRef {
        id: version.id,
        number: version.number,
        status: version.status.as_str().to_owned(),
        published_at: version.published_at,
    }
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
    events::record(
        &mut tx,
        &events::NewEvent::new(
            EventKind::ValidationQueued,
            EventActor::Owner,
            format!(
                "владелец запустил проверку: кандидатов — фактов {}, отраслевых выводов {}",
                candidates.facts, candidates.findings
            ),
        )
        .for_partner(partner_id)
        .about_job(job.id)
        .about_run(run.id)
        .with_detail(serde_json::json!({
            "trigger": "owner",
            "facts": candidates.facts,
            "findings": candidates.findings,
        })),
    )
    .await?;
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

    events::record(
        &mut tx,
        &events::NewEvent::new(
            EventKind::VersionRetracted,
            EventActor::Owner,
            format!(
                "версия {} отозвана: {reason}. Снимок сохранён как история; поиск и ответы \
                 по ней прекращены немедленно",
                version.number
            ),
        )
        .for_partner(partner_id)
        .about_version(version_id)
        .with_detail(serde_json::json!({
            "number": version.number,
            "reason": reason,
        })),
    )
    .await?;

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

// --- phase 1F: what a version changed, and a copy of it ---------------------------------
//
// Both endpoints are addressed by version id and belong beside `claims`, `gaps` and
// `retract` above rather than in a module of their own: the rules about *which* versions a
// reader may see are the same ones, and keeping them together is what stops the two sets
// from drifting apart.

// --- comparing versions ----------------------------------------------------------------

/// `GET /api/partners/{id}/versions/{version_id}/changes`
///
/// What this version says that the previous one did not.
///
/// The default other side is the previously **published** version, by number. A blocked
/// draft is not a thing anybody read, so comparing against one would answer a question
/// nobody asked; `?against=` accepts any version of this partner for the cases where it
/// is the question.
pub async fn changes(
    State(state): State<AppState>,
    session: Session,
    ApiPath((partner_id, version_id)): ApiPath<(Uuid, Uuid)>,
    ApiQuery(query): ApiQuery<ChangesQuery>,
) -> ApiResult<Json<VersionChanges>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    let version = publication_read::find_version(&mut tx, partner_id, version_id)
        .await?
        .ok_or_else(version_not_found)?;

    let previous_id = match query.against {
        Some(against) => Some(against),
        None => updates::previous_version(&mut tx, partner_id, version.number).await?,
    };

    let previous = match previous_id {
        Some(id) => Some(
            publication_read::find_version(&mut tx, partner_id, id)
                .await?
                .ok_or_else(version_not_found)?,
        ),
        None => None,
    };
    if previous
        .as_ref()
        .is_some_and(|other| other.id == version.id)
    {
        return Err(ApiError::new(AppError::validation(
            "сравнивать версию с самой собой нечего",
        )));
    }

    let to_claims = publication_read::list_claims(&mut tx, version.id).await?;
    let to_gaps = publication_read::list_version_gaps(&mut tx, version.id).await?;
    let (from_claims, from_gaps): (Vec<VersionClaim>, Vec<VersionGap>) = match &previous {
        Some(other) => (
            publication_read::list_claims(&mut tx, other.id).await?,
            publication_read::list_version_gaps(&mut tx, other.id).await?,
        ),
        None => (Vec::new(), Vec::new()),
    };
    tx.commit().await?;

    let (claims, counts) = diff::compare_claims(&from_claims, &to_claims);
    let readiness = diff::compare_readiness(
        previous
            .as_ref()
            .map(|other| other.readiness.as_slice())
            .unwrap_or_default(),
        &version.readiness,
    );
    let gaps = diff::compare_gaps(&from_gaps, &to_gaps);

    let mut message = diff::summarise(counts, previous.is_none());
    if let Some(extra) = diff::summarise_readiness(&readiness) {
        message.push(' ');
        message.push_str(&extra);
    }

    Ok(Json(VersionChanges {
        from: previous.as_ref().map(version_ref),
        to: version_ref(&version),
        counts,
        claims,
        readiness,
        gaps,
        limitations: diff::limitations(),
        message,
    }))
}

// --- export ---------------------------------------------------------------------------

/// The whole of one version, as a downstream agent reads it.
#[derive(Debug, Serialize)]
pub struct ExportDocument {
    pub manifest: ExportManifest,
    pub version: KnowledgeVersion,
    pub claims: Vec<VersionClaim>,
    pub gaps: Vec<VersionGap>,
}

/// `GET /api/partners/{id}/versions/{version_id}/export`
///
/// A read-only snapshot for another agent, with its provenance and its caveats.
///
/// The status rules are the same as pinning a version for search, and for the same
/// reason: `published` and `superseded` are readable — a version that was once live is
/// what somebody's earlier answer cited — `revoked` is refused with its reason, and
/// `draft`, `validating` and `blocked` are 404, because for a reader they were never
/// published. Exporting a draft would put unchecked candidates into a file that looks
/// exactly like a checked one.
pub async fn export(
    State(state): State<AppState>,
    session: Session,
    ApiPath((partner_id, version_id)): ApiPath<(Uuid, Uuid)>,
) -> ApiResult<Json<ExportDocument>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    let partner = partners::get(&mut tx, partner_id)
        .await?
        .ok_or_else(partner_not_found)?;
    let version = publication_read::find_version(&mut tx, partner_id, version_id)
        .await?
        .ok_or_else(version_not_found)?;

    match version.status {
        VersionStatus::Published | VersionStatus::Superseded => {}
        VersionStatus::Revoked => {
            return Err(ApiError::new(AppError::new(
                ErrorCode::Conflict,
                format!(
                    "версия отозвана и не выгружается: {}",
                    version
                        .revoked_reason
                        .as_deref()
                        .unwrap_or("причина не записана")
                ),
            )));
        }
        // Never published: for a reader it does not exist, and the wording matches what
        // pinning an unpublished version returns.
        VersionStatus::Draft | VersionStatus::Validating | VersionStatus::Blocked => {
            return Err(version_not_found());
        }
    }

    let claims = publication_read::list_claims(&mut tx, version.id).await?;
    let gaps = publication_read::list_version_gaps(&mut tx, version.id).await?;

    events::record(
        &mut tx,
        &events::NewEvent::new(
            EventKind::ExportRead,
            EventActor::Owner,
            format!(
                "выгружена версия {} ({}): утверждений {}",
                version.number,
                version.status.as_str(),
                claims.len()
            ),
        )
        .for_partner(partner_id)
        .about_version(version.id)
        .with_detail(serde_json::json!({
            "number": version.number,
            "status": version.status.as_str(),
            "claims": claims.len(),
            "schema": EXPORT_SCHEMA,
        })),
    )
    .await?;
    tx.commit().await?;

    let manifest = ExportManifest {
        schema: EXPORT_SCHEMA,
        generated_at: Utc::now(),
        bureau_slug: state.config.bureau_slug.clone(),
        partner_id,
        partner_name: partner.name,
        version_id: version.id,
        version_number: version.number,
        version_status: version.status.as_str().to_owned(),
        published_at: version.published_at,
        superseded_at: version.superseded_at,
        input_fingerprint: version.input_fingerprint.clone(),
        candidate_fingerprint: version.candidate_fingerprint.clone(),
        claims_total: version.claims_total,
        claims_source_supported: version.claims_source_supported,
        gaps_total: i32::try_from(gaps.len()).unwrap_or(i32::MAX),
        disclosure: export_disclosure(),
    };

    info!(
        partner_id = %partner_id,
        version_id = %version.id,
        "knowledge version exported by the owner"
    );

    Ok(Json(ExportDocument {
        manifest,
        version,
        claims,
        gaps,
    }))
}

fn partner_not_found() -> ApiError {
    ApiError::not_found("partner not found in this workspace")
}

fn version_not_found() -> ApiError {
    ApiError::not_found("knowledge version not found for this partner")
}
