//! Database errors.
//!
//! These are server-side diagnostics. The API layer logs them and answers with a generic
//! contract error: SQL text, constraint names and connection strings never reach a client.

use otdel_core::{AppError, ErrorCode};

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database query failed: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration failed: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("unexpected row shape: {0}")]
    Decode(String),
    /// The pool is connected as a role that would defeat tenant isolation.
    #[error("database role `{role}` is not safe for the API: {}", problems.join("; "))]
    UnsafeRuntimeRole { role: String, problems: Vec<String> },
}

impl DbError {
    /// Name of the violated unique constraint, if this is a unique violation.
    pub fn unique_violation(&self) -> Option<String> {
        let Self::Sqlx(sqlx::Error::Database(db_error)) = self else {
            return None;
        };
        if db_error.code().as_deref() != Some("23505") {
            return None;
        }
        db_error.constraint().map(str::to_owned)
    }

    /// True when the failure means “this row is not visible to the current bureau”
    /// (row-level security rejected a write).
    pub fn is_rls_violation(&self) -> bool {
        let Self::Sqlx(sqlx::Error::Database(db_error)) = self else {
            return false;
        };
        // 42501 insufficient_privilege is what a WITH CHECK failure surfaces as.
        db_error.code().as_deref() == Some("42501")
    }

    /// Client-safe translation. Details stay in the log, not in the response.
    pub fn to_app_error(&self) -> AppError {
        match self {
            Self::Sqlx(sqlx::Error::PoolTimedOut) | Self::Sqlx(sqlx::Error::Io(_)) => {
                AppError::unavailable("the database is not available right now")
            }
            _ => AppError::new(ErrorCode::Internal, "internal server error"),
        }
    }
}

pub type DbResult<T> = Result<T, DbError>;
