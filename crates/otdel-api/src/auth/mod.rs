//! Authentication and CSRF.
//!
//! The session is opaque: the client holds a random token in an `HttpOnly`,
//! `SameSite=Strict` cookie and the server stores only its SHA-256 fingerprint. The CSRF
//! token is a second random value returned in the JSON body (never in a cookie), which
//! the frontend keeps in memory and echoes in `X-CSRF-Token` on every state-changing
//! request.
//!
//! [`Session`] is an extractor, so a handler that does not name it simply cannot see
//! tenant data — authentication is not something a route can forget to switch on, and
//! the CSRF check happens in the same place.

pub mod throttle;

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, Method};
use chrono::{DateTime, Utc};
use otdel_core::config::Config;
use otdel_core::{secret, AppError, ErrorCode};
use std::net::SocketAddr;
use uuid::Uuid;

use crate::cookie;
use crate::error::ApiError;
use crate::state::AppState;

pub use throttle::LoginThrottle;

/// An authenticated request, already checked for CSRF when it changes state.
#[derive(Debug, Clone)]
pub struct Session {
    pub session_id: Uuid,
    /// Tenant scope. Comes from the server-side record, never from the request.
    pub bureau_id: Uuid,
    pub csrf_token: String,
    pub expires_at: DateTime<Utc>,
}

impl FromRequestParts<AppState> for Session {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = parts
            .headers
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|header| cookie::read_cookie(header, &state.config.cookie_name))
            .filter(|token| !token.is_empty())
            .ok_or_else(ApiError::unauthorized)?;

        let fingerprint = secret::token_fingerprint(token);
        let idle_timeout = i32::try_from(state.config.session_idle_timeout.as_secs())
            .map_err(|_| ApiError::new(AppError::internal("internal server error")))?;

        // Unknown, expired and idle-timed-out sessions are indistinguishable to the
        // client: all three are a plain 401.
        let record = otdel_db::sessions::touch(state.db.pool(), &fingerprint, idle_timeout)
            .await?
            .ok_or_else(ApiError::unauthorized)?;

        if changes_state(&parts.method) {
            check_origin(&parts.headers, &state.config)?;
            check_csrf(&parts.headers, &record.csrf_token)?;
        }

        Ok(Self {
            session_id: record.session_id,
            bureau_id: record.bureau_id,
            csrf_token: record.csrf_token,
            expires_at: record.expires_at,
        })
    }
}

/// Methods that require a CSRF token. `GET`/`HEAD`/`OPTIONS` never change state here.
fn changes_state(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn check_csrf(headers: &HeaderMap, expected: &str) -> Result<(), ApiError> {
    let provided = headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();

    if provided.is_empty() {
        return Err(ApiError::new(AppError::new(
            ErrorCode::InvalidCsrfToken,
            "X-CSRF-Token header is required for this request",
        )));
    }
    if !secret::tokens_match(expected, provided) {
        return Err(ApiError::new(AppError::new(
            ErrorCode::InvalidCsrfToken,
            "X-CSRF-Token does not match this session",
        )));
    }
    Ok(())
}

/// Defence in depth next to `SameSite=Strict` and the CSRF token: when a browser tells
/// us where the request came from, that origin must be one we allow.
fn check_origin(headers: &HeaderMap, config: &Config) -> Result<(), ApiError> {
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return Ok(());
    };

    if config
        .cors_allowed_origins
        .iter()
        .any(|allowed| allowed == origin)
    {
        return Ok(());
    }

    Err(ApiError::new(AppError::forbidden(
        "this request origin is not allowed",
    )))
}

/// Throttling key for the login endpoint: the peer address when the server runs with
/// connection info, a constant otherwise (tests, or a future proxy setup — in which case
/// the limit simply becomes global rather than per-client, never absent).
#[derive(Debug, Clone)]
pub struct ClientKey(pub String);

impl<S> FromRequestParts<S> for ClientKey
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let key = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(addr)| addr.ip().to_string())
            .unwrap_or_else(|| "unknown-client".to_owned());
        Ok(Self(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn config_with_origin(origin: &str) -> Config {
        let mut source = std::collections::BTreeMap::new();
        source.insert(
            "OTDEL_DATABASE_URL".to_owned(),
            "postgres://otdel_app:pw@127.0.0.1:58432/otdel".to_owned(),
        );
        source.insert(
            "OTDEL_OWNER_PASSWORD_HASH".to_owned(),
            secret::hash_password("local-owner-password").unwrap(),
        );
        source.insert("OTDEL_CORS_ALLOWED_ORIGINS".to_owned(), origin.to_owned());
        Config::load(&source).unwrap()
    }

    #[test]
    fn state_changing_methods_require_csrf() {
        assert!(changes_state(&Method::POST));
        assert!(changes_state(&Method::PATCH));
        assert!(changes_state(&Method::DELETE));
        assert!(!changes_state(&Method::GET));
        assert!(!changes_state(&Method::HEAD));
        assert!(!changes_state(&Method::OPTIONS));
    }

    #[test]
    fn csrf_header_must_match_exactly() {
        let expected = secret::generate_token();
        let mut headers = HeaderMap::new();
        assert_eq!(
            check_csrf(&headers, &expected).unwrap_err().code(),
            ErrorCode::InvalidCsrfToken
        );

        headers.insert("x-csrf-token", HeaderValue::from_static("wrong"));
        assert_eq!(
            check_csrf(&headers, &expected).unwrap_err().code(),
            ErrorCode::InvalidCsrfToken
        );

        headers.insert("x-csrf-token", HeaderValue::from_str(&expected).unwrap());
        assert!(check_csrf(&headers, &expected).is_ok());
    }

    #[test]
    fn foreign_origins_are_refused_and_missing_origin_is_allowed() {
        let config = config_with_origin("http://127.0.0.1:15173");
        let mut headers = HeaderMap::new();
        assert!(check_origin(&headers, &config).is_ok());

        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://127.0.0.1:15173"),
        );
        assert!(check_origin(&headers, &config).is_ok());

        headers.insert(header::ORIGIN, HeaderValue::from_static("http://evil.test"));
        assert_eq!(
            check_origin(&headers, &config).unwrap_err().code(),
            ErrorCode::Forbidden
        );
    }
}
