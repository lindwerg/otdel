//! Error codes and the single error type carried across layers.
//!
//! Wire shape (see `docs/implementation-contract.md`):
//! `{ "error": { "code": "...", "message": "...", "retryable": false } }`.
//!
//! Messages are written for the owner of the local pilot, never contain secrets,
//! connection strings, file-system paths or SQL text. Internal details are logged
//! separately with `tracing`, not returned to the client.

use std::fmt;

pub type Result<T> = std::result::Result<T, AppError>;

/// Stable, machine-readable error codes used by the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// Request body/parameters failed validation.
    ValidationFailed,
    /// Malformed request (bad JSON, malformed multipart, missing field).
    BadRequest,
    /// No valid session cookie.
    Unauthorized,
    /// Session is valid but the CSRF token is missing or does not match.
    InvalidCsrfToken,
    /// Session is valid but the object does not belong to the caller's bureau.
    Forbidden,
    /// Object does not exist (or does not belong to the route's partner).
    NotFound,
    /// The path exists but not for this HTTP method.
    MethodNotAllowed,
    /// State conflict, e.g. retry requested for a status that cannot be retried.
    Conflict,
    /// Upload exceeded the configured size limit.
    PayloadTooLarge,
    /// File signature is not one of the formats accepted in phase 1A.
    UnsupportedMediaType,
    /// Too many attempts (login throttling).
    RateLimited,
    /// A dependency (database, object store) is unavailable.
    ServiceUnavailable,
    /// Unexpected server-side failure.
    Internal,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ValidationFailed => "validation_failed",
            Self::BadRequest => "bad_request",
            Self::Unauthorized => "unauthorized",
            Self::InvalidCsrfToken => "invalid_csrf_token",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::MethodNotAllowed => "method_not_allowed",
            Self::Conflict => "conflict",
            Self::PayloadTooLarge => "payload_too_large",
            Self::UnsupportedMediaType => "unsupported_media_type",
            Self::RateLimited => "rate_limited",
            Self::ServiceUnavailable => "service_unavailable",
            Self::Internal => "internal_error",
        }
    }

    /// HTTP status as a plain number: this crate stays free of HTTP dependencies.
    pub const fn http_status(self) -> u16 {
        match self {
            Self::ValidationFailed => 422,
            Self::BadRequest => 400,
            Self::Unauthorized => 401,
            Self::InvalidCsrfToken => 403,
            Self::Forbidden => 403,
            Self::NotFound => 404,
            Self::MethodNotAllowed => 405,
            Self::Conflict => 409,
            Self::PayloadTooLarge => 413,
            Self::UnsupportedMediaType => 415,
            Self::RateLimited => 429,
            Self::ServiceUnavailable => 503,
            Self::Internal => 500,
        }
    }

    /// Whether repeating the exact same request can plausibly succeed later.
    pub const fn retryable(self) -> bool {
        matches!(
            self,
            Self::RateLimited | Self::ServiceUnavailable | Self::Internal
        )
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An error that is safe to render to the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: code.retryable(),
        }
    }

    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ValidationFailed, message)
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::BadRequest, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unauthorized, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Forbidden, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Conflict, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ServiceUnavailable, message)
    }

    pub fn http_status(&self) -> u16 {
        self.code.http_status()
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AppError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_map_to_expected_statuses() {
        assert_eq!(ErrorCode::Unauthorized.http_status(), 401);
        assert_eq!(ErrorCode::InvalidCsrfToken.http_status(), 403);
        assert_eq!(ErrorCode::PayloadTooLarge.http_status(), 413);
        assert_eq!(ErrorCode::UnsupportedMediaType.http_status(), 415);
        assert_eq!(ErrorCode::ValidationFailed.http_status(), 422);
    }

    #[test]
    fn client_errors_are_not_advertised_as_retryable() {
        for code in [
            ErrorCode::ValidationFailed,
            ErrorCode::BadRequest,
            ErrorCode::Unauthorized,
            ErrorCode::InvalidCsrfToken,
            ErrorCode::Forbidden,
            ErrorCode::NotFound,
            ErrorCode::Conflict,
            ErrorCode::PayloadTooLarge,
            ErrorCode::UnsupportedMediaType,
        ] {
            assert!(!code.retryable(), "{code} must not be retryable");
        }
        assert!(ErrorCode::RateLimited.retryable());
        assert!(ErrorCode::ServiceUnavailable.retryable());
    }
}
