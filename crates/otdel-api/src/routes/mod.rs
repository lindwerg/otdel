//! Route table.
//!
//! The paths here are exactly the ones written in `docs/implementation-contract.md`.
//! Endpoints for later phases (pages, products, glossary, research, publishing) are
//! deliberately absent — a stub that answers plausibly would be indistinguishable from a
//! feature that works.

pub mod health;
pub mod jobs;
pub mod materials;
pub mod partners;
pub mod session;

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
            "/partners/{partner_id}/materials/{material_id}/original",
            get(materials::download),
        )
        .route(
            "/partners/{partner_id}/materials/{material_id}/retry",
            post(materials::retry),
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
