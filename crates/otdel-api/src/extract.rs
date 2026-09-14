//! Extractors that fail in the contract's error shape.
//!
//! `axum`'s built-in rejections answer with `text/plain` and their own wording, which
//! would break the promise that *every* error is
//! `{ "error": { code, message, retryable } }`. These thin wrappers keep the shape.

use axum::extract::multipart::MultipartRejection;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{FromRequest, FromRequestParts, Multipart, Path, Query, Request};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::Json;
use otdel_core::{AppError, ErrorCode};
use serde::de::DeserializeOwned;

use crate::error::ApiError;

/// JSON body with contract-shaped rejections.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(json_rejection(&rejection)),
        }
    }
}

fn json_rejection(rejection: &JsonRejection) -> ApiError {
    // Deliberately generic: serde's message can quote the offending body, which for the
    // login endpoint is the password.
    let message = match rejection {
        JsonRejection::MissingJsonContentType(_) => "request body must be sent as application/json",
        JsonRejection::BytesRejection(_) => "request body could not be read",
        _ => "request body is not valid JSON for this endpoint",
    };
    ApiError::new(AppError::bad_request(message))
}

/// Path parameters with contract-shaped rejections.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApiPath<T>(pub T);

impl<S, T> FromRequestParts<S> for ApiPath<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match Path::<T>::from_request_parts(parts, state).await {
            Ok(Path(value)) => Ok(Self(value)),
            Err(rejection) => Err(path_rejection(&rejection)),
        }
    }
}

/// Query parameters with contract-shaped rejections.
///
/// The bare `Query` extractor answers `text/plain` with serde's own wording, which for
/// a filter like `?product_id=…` would be the one place a client sees something other
/// than the agreed JSON envelope.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApiQuery<T>(pub T);

impl<S, T> FromRequestParts<S> for ApiQuery<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match Query::<T>::from_request_parts(parts, state).await {
            Ok(Query(value)) => Ok(Self(value)),
            Err(_) => Err(ApiError::new(AppError::validation(
                "query parameters of this endpoint could not be parsed",
            ))),
        }
    }
}

/// Multipart body with contract-shaped rejections.
///
/// The bare `Multipart` extractor answers `text/plain` with axum's own wording before
/// the handler runs (missing or invalid boundary, wrong content type, body limit). That
/// would be the one place where a client sees something other than the agreed JSON
/// envelope, so the rejection is translated here.
#[derive(Debug)]
pub struct ApiMultipart(pub Multipart);

impl<S> FromRequest<S> for ApiMultipart
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Multipart::from_request(request, state).await {
            Ok(multipart) => Ok(Self(multipart)),
            Err(rejection) => Err(multipart_rejection(&rejection)),
        }
    }
}

fn multipart_rejection(rejection: &MultipartRejection) -> ApiError {
    let code = match rejection.status() {
        StatusCode::PAYLOAD_TOO_LARGE => ErrorCode::PayloadTooLarge,
        StatusCode::UNSUPPORTED_MEDIA_TYPE => ErrorCode::UnsupportedMediaType,
        _ => ErrorCode::BadRequest,
    };
    ApiError::new(AppError::new(
        code,
        "request must be a multipart/form-data body with a `file` part",
    ))
}

fn path_rejection(rejection: &PathRejection) -> ApiError {
    match rejection {
        PathRejection::FailedToDeserializePathParams(_) => {
            ApiError::new(AppError::validation("path identifiers must be UUIDs"))
        }
        _ => ApiError::new(AppError::bad_request("request path could not be parsed")),
    }
}
