//! HTTP rendering of errors.
//!
//! Every failure leaves this server in the shape promised by the contract:
//! `{ "error": { "code", "message", "retryable" } }`. Internal detail (SQL state,
//! file-system paths, connection strings) is logged, never serialised.

use std::time::Duration;

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use otdel_core::{AppError, ErrorCode};
use otdel_db::DbError;
use otdel_storage::{StorageError, StreamErrorKind};
use serde::Serialize;
use tracing::{error, warn};

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: ErrorPayload,
}

#[derive(Debug, Serialize)]
struct ErrorPayload {
    code: &'static str,
    message: String,
    retryable: bool,
}

/// A contract error, optionally with a `Retry-After` hint.
#[derive(Debug)]
pub struct ApiError {
    inner: AppError,
    retry_after: Option<Duration>,
}

impl ApiError {
    pub fn new(inner: AppError) -> Self {
        Self {
            inner,
            retry_after: None,
        }
    }

    pub fn with_retry_after(mut self, retry_after: Duration) -> Self {
        self.retry_after = Some(retry_after);
        self
    }

    pub fn code(&self) -> ErrorCode {
        self.inner.code
    }

    pub fn unauthorized() -> Self {
        Self::new(AppError::unauthorized("authentication required"))
    }

    pub fn not_found(message: &str) -> Self {
        Self::new(AppError::not_found(message))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.inner.http_status())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let body = ErrorBody {
            error: ErrorPayload {
                code: self.inner.code.as_str(),
                message: self.inner.message,
                retryable: self.inner.retryable,
            },
        };

        let mut response = (status, Json(body)).into_response();
        if let Some(retry_after) = self.retry_after {
            if let Ok(value) = HeaderValue::from_str(&retry_after.as_secs().max(1).to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
        }
        response
    }
}

impl From<AppError> for ApiError {
    fn from(value: AppError) -> Self {
        Self::new(value)
    }
}

impl From<DbError> for ApiError {
    fn from(value: DbError) -> Self {
        // The full error (including SQL state) goes to the log only.
        if value.is_rls_violation() {
            warn!(error = %value, "database refused an out-of-scope write");
        } else {
            error!(error = %value, "database failure");
        }
        Self::new(value.to_app_error())
    }
}

impl From<StorageError> for ApiError {
    fn from(value: StorageError) -> Self {
        if let Some(stream_error) = value.stream_error() {
            // These describe the client's own upload and are safe to return verbatim.
            let app_error = match stream_error.kind {
                StreamErrorKind::TooLarge => {
                    AppError::new(ErrorCode::PayloadTooLarge, stream_error.message.clone())
                }
                StreamErrorKind::UnsupportedContent => AppError::new(
                    ErrorCode::UnsupportedMediaType,
                    stream_error.message.clone(),
                ),
                StreamErrorKind::Upstream => AppError::new(
                    ErrorCode::BadRequest,
                    "the upload was interrupted before the file was fully received",
                ),
            };
            return Self::new(app_error);
        }

        match value {
            StorageError::NotFound(_) => {
                error!(error = %value, "stored original is missing from the object store");
                Self::new(AppError::internal(
                    "the stored original is currently unavailable",
                ))
            }
            StorageError::InvalidKey(_) => {
                error!(error = %value, "rejected an object key that failed validation");
                Self::new(AppError::internal("internal server error"))
            }
            StorageError::Unavailable(_) | StorageError::Io { .. } => {
                error!(error = %value, "object store failure");
                Self::new(AppError::unavailable(
                    "file storage is not available right now",
                ))
            }
            StorageError::Stream(_) => unreachable!("stream errors are handled above"),
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
