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
//! drafts it ([`llm_config`]); phase 1D adds bounded industry research over external
//! sources ([`research`]) and the configuration that bounds it ([`research_config`]);
//! phase 1E adds verification, the immutable published version and the search over it
//! ([`publication`]), with the optional embedding adapter and the bounds on reading it
//! ([`retrieval_config`]); phase 1F adds the update cycle around a published version —
//! its history, its refresh status, the comparison of two versions and the export a
//! downstream agent reads ([`updates`]) — together with how long operational history is
//! kept ([`retention_config`]).

pub mod config;
pub mod error;
pub mod extraction;
pub mod extraction_config;
pub mod extraction_context;
pub mod knowledge;
pub mod llm_config;
pub mod media;
pub mod model;
pub mod publication;
pub mod research;
pub mod research_config;
pub mod retention_config;
pub mod retrieval_config;
pub mod secret;
pub mod updates;
pub mod validate;

pub use error::{AppError, ErrorCode, Result};
pub use media::MediaType;
