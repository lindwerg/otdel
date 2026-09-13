//! Domain model, configuration and validation for the OTDEL backend (block 1, phase 1A).
//!
//! This crate deliberately contains no I/O: it is pure enough to unit-test without a
//! database, an object store or a network. The HTTP layer lives in `otdel-api`, the
//! PostgreSQL layer in `otdel-db` and the original-file layer in `otdel-storage`.

pub mod config;
pub mod error;
pub mod media;
pub mod model;
pub mod secret;
pub mod validate;

pub use error::{AppError, ErrorCode, Result};
pub use media::MediaType;
