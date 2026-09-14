//! Route table.
//!
//! The paths here are exactly the ones written in `docs/implementation-contract.md`.
//! Endpoints for later phases (pages, products, glossary, research, publishing) are
//! deliberately absent — a stub that answers plausibly would be indistinguishable from a
//! feature that works.

pub mod health;
pub mod history;
pub mod jobs;
pub mod knowledge;
pub mod materials;
pub mod pages;
pub mod partners;
/// R05 — the product base as a person reads it: passports, coverage, the application
/// map, the uncertainties and the identity proposals.
pub mod passports;
pub mod publication;
pub mod research;
pub mod retrieval;
pub mod retrieval_state;
pub mod session;
pub mod updates;

use axum::extract::DefaultBodyLimit;
use axum::http::{header, HeaderName, HeaderValue, Method};
use axum::routing::{get, post};
use axum::Router;
use otdel_core::AppError;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::error::ApiError;
use crate::state::AppState;

/// Headroom over the configured file limit for multipart boundaries and headers.
/// The precise per-file limit is enforced while streaming (see [`crate::upload`]); this
/// is the outer guard that stops a body from being read at all.
const MULTIPART_OVERHEAD_BYTES: usize = 1024 * 1024;

pub fn router(state: AppState) -> Router {
    let upload_limit = state
        .config
        .max_upload_bytes
        .try_into()
        .unwrap_or(usize::MAX)
        .saturating_add(MULTIPART_OVERHEAD_BYTES);

    let api = Router::new()
        .route(
            "/session",
            post(session::login)
                .get(session::show)
                .delete(session::logout),
        )
        .route("/partners", get(partners::list).post(partners::create))
        .route(
            "/partners/{partner_id}",
            get(partners::show).patch(partners::update),
        )
        .route(
            "/partners/{partner_id}/materials",
            get(materials::list).post(materials::upload),
        )
        .route(
            "/partners/{partner_id}/materials/{material_id}",
            get(materials::show),
        )
        .route(
            "/partners/{partner_id}/materials/{material_id}/original",
            get(materials::download),
        )
        .route(
            "/partners/{partner_id}/materials/{material_id}/retry",
            post(materials::retry),
        )
        // Phase 1B: the per-page evidence of a material.
        .route(
            "/partners/{partner_id}/materials/{material_id}/pages",
            get(pages::list),
        )
        .route(
            "/partners/{partner_id}/materials/{material_id}/pages/{page_number}",
            get(pages::show),
        )
        .route(
            "/partners/{partner_id}/materials/{material_id}/pages/{page_number}/view",
            get(pages::view),
        )
        .route(
            "/partners/{partner_id}/materials/{material_id}/pages/{page_number}/retry",
            post(pages::retry),
        )
        // Phase 1C: the product draft and the state of the model adapter.
        .route("/knowledge/provider", get(knowledge::provider))
        .route("/partners/{partner_id}/knowledge", get(knowledge::overview))
        .route(
            "/partners/{partner_id}/knowledge/products",
            get(knowledge::products),
        )
        .route(
            "/partners/{partner_id}/knowledge/glossary",
            get(knowledge::glossary),
        )
        .route("/partners/{partner_id}/knowledge/qa", get(knowledge::qa))
        .route(
            "/partners/{partner_id}/knowledge/gaps",
            get(knowledge::gaps),
        )
        .route(
            "/partners/{partner_id}/materials/{material_id}/understand",
            post(knowledge::understand),
        )
        // R05: the product base. A passport travels with its gaps, its uncertainties and
        // its identity proposals — there is no parameter here that drops them.
        .route("/partners/{partner_id}/passports", get(passports::list))
        .route(
            "/partners/{partner_id}/passports/{product_id}",
            get(passports::show),
        )
        .route("/partners/{partner_id}/coverage", get(passports::coverage))
        .route(
            "/partners/{partner_id}/applications",
            get(passports::applications),
        )
        .route(
            "/partners/{partner_id}/uncertainties",
            get(passports::uncertainties),
        )
        .route("/partners/{partner_id}/identity", get(passports::identity))
        .route(
            "/partners/{partner_id}/declarations",
            get(passports::declarations),
        )
        // Phase 1D: bounded industry research, its money and its sources. No handler
        // here reaches the network — approving a question queues a job.
        .route("/research/provider", get(research::provider))
        .route("/research/budget", get(research::budget))
        .route("/partners/{partner_id}/research", get(research::overview))
        .route(
            "/partners/{partner_id}/research/findings",
            get(research::findings),
        )
        .route(
            "/partners/{partner_id}/research/plans/{plan_id}/sources",
            get(research::sources),
        )
        .route(
            "/partners/{partner_id}/research/plans/{plan_id}/queries",
            get(research::queries),
        )
        .route(
            "/partners/{partner_id}/research/plans/{plan_id}/stop",
            post(research::stop),
        )
        .route(
            "/partners/{partner_id}/research/questions/{question_id}/plan",
            post(research::approve),
        )
        // Phase 1E: the check, the immutable versions it produces, and reading them.
        // Verification and publication need no adapter at all — `/retrieval/provider`
        // describes only the two optional halves (vectors, prose answers).
        .route("/retrieval/provider", get(retrieval::provider))
        .route(
            "/partners/{partner_id}/validation",
            get(publication::overview),
        )
        .route(
            "/partners/{partner_id}/validate",
            post(publication::validate),
        )
        .route("/partners/{partner_id}/versions", get(publication::list))
        .route(
            "/partners/{partner_id}/versions/{version_id}",
            get(publication::get),
        )
        .route(
            "/partners/{partner_id}/versions/{version_id}/claims",
            get(publication::claims),
        )
        .route(
            "/partners/{partner_id}/versions/{version_id}/gaps",
            get(publication::gaps),
        )
        .route(
            "/partners/{partner_id}/versions/{version_id}/retract",
            post(publication::retract),
        )
        // Search and answering are POST because the request carries the text of a
        // question. A question does not belong in a URL, a log or a browser history.
        .route(
            "/partners/{partner_id}/retrieval/search",
            post(retrieval::search),
        )
        .route(
            "/partners/{partner_id}/retrieval/answer",
            post(retrieval::ask),
        )
        // Phase 1F: the cycle around a published version. What is out of date and why,
        // what starts a new cycle, what happened, what changed, and the read-only copy a
        // downstream agent takes.
        .route("/retention", get(history::retention))
        .route(
            "/partners/{partner_id}/refresh",
            get(updates::status).post(updates::refresh),
        )
        .route("/partners/{partner_id}/events", get(history::events))
        .route(
            "/partners/{partner_id}/materials/{material_id}/reprocess",
            post(updates::reprocess),
        )
        .route(
            "/partners/{partner_id}/versions/{version_id}/changes",
            get(publication::changes),
        )
        .route(
            "/partners/{partner_id}/versions/{version_id}/export",
            get(publication::export),
        )
        .route("/partners/{partner_id}/jobs", get(jobs::list))
        .layer(DefaultBodyLimit::max(upload_limit));

    Router::new()
        .route("/health", get(health::health))
        .route("/ready", get(health::ready))
        .nest("/api", api)
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(cors_layer(&state))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// CORS for exactly the configured local frontend origin(s), with credentials.
///
/// `Access-Control-Allow-Origin: *` is impossible here: the configuration rejects `*`,
/// and a wildcard cannot be combined with credentialed requests anyway.
fn cors_layer(state: &AppState) -> CorsLayer {
    let origins: Vec<HeaderValue> = state
        .config
        .cors_allowed_origins
        .iter()
        .filter_map(|origin| HeaderValue::from_str(origin).ok())
        .collect();

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_credentials(true)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::CONTENT_TYPE,
            HeaderName::from_static("x-csrf-token"),
        ])
        .max_age(std::time::Duration::from_secs(600))
}

async fn not_found() -> ApiError {
    ApiError::new(AppError::not_found("no such endpoint"))
}

async fn method_not_allowed() -> ApiError {
    ApiError::new(AppError::new(
        otdel_core::ErrorCode::MethodNotAllowed,
        "this method is not allowed for this endpoint",
    ))
}
