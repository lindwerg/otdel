//! Storage errors.
//!
//! `Display` here is aimed at the server log, not at the client: it may contain file
//! system details. The API layer maps these into contract errors and only forwards the
//! *stream* errors, which carry a client-safe reason (too large, unsupported content).

use std::io;

/// Why an upload stream stopped before the body was fully read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamErrorKind {
    /// Body exceeded the configured limit.
    TooLarge,
    /// Content signature is not accepted.
    UnsupportedContent,
    /// The client or the HTTP layer failed mid-body.
    Upstream,
}

/// An error produced by the byte stream handed to the object store.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct StreamError {
    pub kind: StreamErrorKind,
    pub message: String,
}

impl StreamError {
    pub fn new(kind: StreamErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn too_large(message: impl Into<String>) -> Self {
        Self::new(StreamErrorKind::TooLarge, message)
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(StreamErrorKind::UnsupportedContent, message)
    }

    pub fn upstream(message: impl Into<String>) -> Self {
        Self::new(StreamErrorKind::Upstream, message)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("invalid object key: {0}")]
    InvalidKey(String),
    #[error("object not found: {0}")]
    NotFound(String),
    #[error("upload stream failed: {0}")]
    Stream(#[from] StreamError),
    #[error("storage i/o failure during {operation}: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("storage is not usable: {0}")]
    Unavailable(String),
}

impl StorageError {
    pub fn io(operation: &'static str, source: io::Error) -> Self {
        Self::Io { operation, source }
    }

    /// The stream reason, if this failure came from the uploaded body rather than disk.
    pub fn stream_error(&self) -> Option<&StreamError> {
        match self {
            Self::Stream(error) => Some(error),
            _ => None,
        }
    }
}

pub type StorageResult<T> = Result<T, StorageError>;
