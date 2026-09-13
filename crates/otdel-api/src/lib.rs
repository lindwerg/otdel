//! HTTP API of OTDEL phase 1A.
//!
//! Scope, as fixed by `docs/implementation-contract.md`: sessions for the single local
//! owner, partner cards, streaming intake of original PDF/PNG/JPEG files, authorised
//! download of those originals, and the recorded extraction queue. Reading documents is
//! phase 1B, so an uploaded file stays `queued` and is never reported as read.
//!
//! Security properties that hold for every route in this crate:
//!
//! * tenant data is only reachable through the [`auth::Session`] extractor, which
//!   resolves the bureau from the server-side session record;
//! * state-changing requests additionally require a matching `X-CSRF-Token` and an
//!   allowed `Origin`;
//! * every query runs in a bureau-scoped transaction under row-level security;
//! * errors always render as `{ "error": { code, message, retryable } }` without
//!   internals.

pub mod auth;
pub mod cookie;
pub mod dto;
pub mod error;
pub mod extract;
pub mod routes;
pub mod state;
pub mod upload;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use otdel_core::config::Config;
use otdel_core::AppError;
use otdel_db::Database;
use otdel_storage::{FilesystemObjectStore, ObjectStore};
use tokio::net::TcpListener;
use tracing::info;

pub use error::{ApiError, ApiResult};
pub use state::AppState;

/// Build the application state from configuration: database pool, object store,
/// login throttle.
///
/// The runtime database role is verified here, before a single request is served: if
/// row-level security would not apply to it, the server refuses to start instead of
/// serving data that only *looks* isolated.
pub async fn build_state(config: Config) -> Result<AppState, AppError> {
    let config = Arc::new(config);

    let db = Database::connect(&config.database_url, config.database_max_connections)
        .await
        .map_err(|error| {
            AppError::internal(format!("could not connect to the database: {error}"))
        })?;

    db.verify_runtime_role().await.map_err(|error| {
        AppError::internal(format!(
            "refusing to start with this database role: {error}. Use the restricted runtime \
             role (OTDEL_DATABASE_URL), not the migration/admin role"
        ))
    })?;

    let store = FilesystemObjectStore::open_at(&config.storage_root)
        .await
        .map_err(|error| AppError::internal(format!("could not open the object store: {error}")))?;
    let store: Arc<dyn ObjectStore> = Arc::new(store);

    Ok(AppState::new(config, db, store))
}

/// Build the router for an existing state (used by tests as well as by the binary).
pub fn app(state: AppState) -> Router {
    routes::router(state)
}

/// Serve until the process is asked to stop.
pub async fn serve(state: AppState) -> Result<(), AppError> {
    let addr = state.config.bind_addr;
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|error| AppError::internal(format!("could not bind {addr}: {error}")))?;

    info!(
        address = %addr,
        environment = state.config.env.as_str(),
        storage = %state.store.describe(),
        "otdel api listening"
    );

    let app = app(state);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .map_err(|error| AppError::internal(format!("server stopped with an error: {error}")))
}

/// Ctrl-C (and SIGTERM on Unix) stop the server without cutting requests mid-flight.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => info!("shutdown requested (ctrl-c)"),
        () = terminate => info!("shutdown requested (sigterm)"),
    }
}
