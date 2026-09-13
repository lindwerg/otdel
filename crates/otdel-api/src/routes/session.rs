//! `POST|GET|DELETE /api/session`.
//!
//! One local owner, no registration. The password is checked against an Argon2 hash from
//! the configuration; it is never logged, never echoed and never stored by the client.

use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use otdel_core::{secret, AppError, ErrorCode};
use tracing::{info, warn};

use crate::auth::{ClientKey, Session};
use crate::cookie;
use crate::dto::{LoginRequest, SessionEndedResponse, SessionResponse};
use crate::error::{ApiError, ApiResult};
use crate::extract::ApiJson;
use crate::state::AppState;

/// `POST /api/session` — exchange the owner password for a session cookie.
pub async fn login(
    State(state): State<AppState>,
    ClientKey(throttle_key): ClientKey,
    ApiJson(payload): ApiJson<LoginRequest>,
) -> ApiResult<Response> {
    // The attempt is counted here, before the expensive verification, so a burst of
    // concurrent requests cannot slip past a counter that is only updated on failure.
    if let Err(retry_after) = state.throttle.reserve(&throttle_key) {
        warn!("login attempt refused: client is throttled");
        return Err(ApiError::new(
            AppError::new(
                ErrorCode::RateLimited,
                "too many failed sign-in attempts; try again later",
            )
            .with_retryable(true),
        )
        .with_retry_after(retry_after));
    }

    // Argon2 is deliberately expensive: verify it on a blocking thread (so the async
    // runtime stays responsive) and only as many at a time as the throttle allows.
    //
    // The permit moves *into* the blocking closure. If it stayed in this future, a client
    // that disconnects would drop the future and release the slot while the hashing
    // thread keeps running — letting the next request start another hash on top of it.
    let expected_hash = state.config.owner_password_hash.clone();
    let password = payload.password;
    let permit = state.throttle.hash_permit().await;
    let verified = tokio::task::spawn_blocking(move || {
        let verified = secret::verify_password(&expected_hash, &password);
        drop(permit);
        verified
    })
    .await
    .map_err(|_| ApiError::new(AppError::internal("internal server error")))?;

    if !verified {
        // The attempt was already counted by `reserve`; nothing to add here.
        warn!("failed sign-in attempt");
        // One message for every failure mode: no hint about what was wrong.
        return Err(ApiError::new(AppError::unauthorized("sign-in failed")));
    }
    state.throttle.record_success(&throttle_key);

    let token = secret::generate_token();
    let csrf_token = secret::generate_token();
    let fingerprint = secret::token_fingerprint(&token);
    let ttl_seconds = i32::try_from(state.config.session_ttl.as_secs())
        .map_err(|_| ApiError::new(AppError::internal("internal server error")))?;

    let record = match otdel_db::sessions::open(
        state.db.pool(),
        &state.config.bureau_slug,
        &fingerprint,
        &csrf_token,
        ttl_seconds,
    )
    .await
    {
        Ok(record) => record,
        Err(error) => {
            // The most likely cause is a database that was migrated but never
            // bootstrapped for this bureau slug; that is configuration, not a secret.
            warn!(error = %error, "could not open a session");
            return Err(ApiError::new(AppError::unavailable(
                "sessions are not available; check that the database is migrated and the bureau is provisioned",
            )));
        }
    };

    info!(session_id = %record.session_id, "owner signed in");

    let cookie_value = cookie::session_cookie(
        &state.config.cookie_name,
        &token,
        state.config.session_ttl.as_secs(),
        state.config.cookie_secure,
    );

    Ok(with_cookie(
        (
            StatusCode::OK,
            Json(SessionResponse {
                authenticated: true,
                csrf_token: record.csrf_token,
            }),
        )
            .into_response(),
        &cookie_value,
    ))
}

/// `GET /api/session` — restore the CSRF token after a page reload.
pub async fn show(session: Session) -> Json<SessionResponse> {
    Json(SessionResponse {
        authenticated: true,
        csrf_token: session.csrf_token,
    })
}

/// `DELETE /api/session` — end the session (requires the CSRF header, like every other
/// state-changing request).
pub async fn logout(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    session: Session,
) -> ApiResult<Response> {
    let token = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|header| cookie::read_cookie(header, &state.config.cookie_name))
        .unwrap_or_default();

    let fingerprint = secret::token_fingerprint(token);
    let removed = otdel_db::sessions::close(state.db.pool(), &fingerprint).await?;
    info!(session_id = %session.session_id, removed, "session ended");

    let cleared = cookie::cleared_cookie(&state.config.cookie_name, state.config.cookie_secure);
    Ok(with_cookie(
        (
            StatusCode::OK,
            Json(SessionEndedResponse {
                authenticated: false,
            }),
        )
            .into_response(),
        &cleared,
    ))
}

fn with_cookie(mut response: Response, cookie_value: &str) -> Response {
    match HeaderValue::from_str(cookie_value) {
        Ok(value) => {
            response.headers_mut().insert(header::SET_COOKIE, value);
        }
        Err(_) => {
            // Tokens are base64url and the cookie name is validated at startup, so this
            // cannot happen; failing loudly in the log beats silently not setting it.
            warn!("could not build the session cookie header");
        }
    }
    response
}
