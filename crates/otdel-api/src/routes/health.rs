//! `GET /health` (liveness) and `GET /ready` (dependencies).

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use otdel_core::AppError;
use tracing::warn;

use crate::dto::{HealthResponse, ReadyChecks, ReadyResponse};
use crate::error::ApiError;
use crate::state::AppState;

/// Liveness only: the process is running and can answer. No dependency is touched, so a
/// database outage does not make the process look dead.
pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

/// Readiness: the database *and* the object store must be usable, and the configured
/// bureau must exist. Anything else is reported as not ready rather than assumed.
pub async fn ready(State(state): State<AppState>) -> Result<Response, ApiError> {
    if let Err(error) = state.db.health().await {
        warn!(error = %error, "readiness: database check failed");
        return Err(ApiError::new(AppError::unavailable(
            "the database is not available",
        )));
    }

    if let Err(error) = state.store.health().await {
        warn!(error = %error, "readiness: object store check failed");
        return Err(ApiError::new(AppError::unavailable(
            "file storage is not available",
        )));
    }

    let bureau = state
        .db
        .bureau_id_by_slug(&state.config.bureau_slug)
        .await?;
    if bureau.is_none() {
        warn!(
            bureau = %state.config.bureau_slug,
            "readiness: configured bureau is not provisioned"
        );
        return Err(ApiError::new(AppError::unavailable(
            "the configured bureau is not provisioned; run migrations and bootstrap",
        )));
    }

    // Not a readiness condition: phase 1A stores no embeddings. Reported so the state of
    // the extension is visible instead of assumed.
    let pgvector = match state.db.pgvector_status().await {
        Ok(status) => status.as_str(),
        Err(error) => {
            warn!(error = %error, "readiness: pgvector probe failed");
            "unknown"
        }
    };

    Ok(Json(ReadyResponse {
        status: "ready",
        checks: ReadyChecks {
            database: "ok",
            object_store: "ok",
            bureau: "provisioned",
            pgvector,
        },
    })
    .into_response())
}
