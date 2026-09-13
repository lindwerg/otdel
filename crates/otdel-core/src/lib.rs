//! Domain model, configuration and validation for the OTDEL backend (block 1).
//!
//! This crate deliberately contains no I/O: it is pure enough to unit-test without a
//! database, an object store or a network. The HTTP layer lives in `otdel-api`, the
//! PostgreSQL layer in `otdel-db`, the original-file layer in `otdel-storage` and the
//! document-reading adapters in `otdel-extract`.
//!
//! Phase 1A covers intake ([`model`]); phase 1B adds per-page reading of the stored
//! originals ([`extraction`]); phase 1C adds the structured product knowledge drafted
//! from those pages ([`knowledge`]) and the configuration of the model adapter that
//! drafts it ([`llm_config`]).

pub mod config;
pub mod error;
pub mod extraction;
pub mod extraction_config;
pub mod knowledge;
pub mod llm_config;
pub mod media;
pub mod model;
pub mod secret;
pub mod validate;

pub use error::{AppError, ErrorCode, Result};
pub use media::MediaType;
