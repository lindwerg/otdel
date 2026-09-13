//! Original-file storage for OTDEL.
//!
//! Phase 1A ships a local filesystem backend ([`FilesystemObjectStore`]) behind the
//! [`ObjectStore`] trait. The trait exists so the S3-compatible backend named in
//! `docs/block-01-spec.md` §3 can be added later without touching the HTTP layer; it is
//! not implemented here and nothing pretends that it is.
//!
//! Properties the API layer relies on:
//!
//! * writes are streamed — the whole file is never buffered in memory;
//! * the content hash is computed while streaming, so the caller learns the digest
//!   without a second pass;
//! * the object is written and fsynced *before* database metadata is published, and the
//!   staging file is removed on every failure path (plus swept later, see
//!   [`ObjectStore::sweep_staging`]);
//! * the storage key is derived from bureau/partner ids and the digest only.

pub mod error;
pub mod fs;
pub mod key;

use std::pin::Pin;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::Stream;

pub use error::{StorageError, StorageResult, StreamError, StreamErrorKind};
pub use fs::FilesystemObjectStore;
pub use key::{ObjectKey, ObjectNamespace};

/// Body of an upload: a stream of chunks that may fail mid-way.
///
/// The lifetime lets callers pass a stream that borrows from the request body (an
/// `axum` multipart field) instead of copying it into an owned buffer first.
pub type ByteStream<'a> = Pin<Box<dyn Stream<Item = Result<Bytes, StreamError>> + Send + 'a>>;

/// Body of a download.
pub type ReadStream = Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>;

/// Result of a successful upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredObject {
    pub key: ObjectKey,
    pub sha256: String,
    pub size_bytes: u64,
    /// `true` when the identical object already existed under this key.
    pub already_present: bool,
}

/// A readable object.
pub struct ObjectBody {
    pub size_bytes: u64,
    pub stream: ReadStream,
}

/// A bounded page of stored object keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObjectListing {
    pub keys: Vec<ObjectKey>,
    /// `true` when the listing stopped at the limit and more objects exist.
    pub truncated: bool,
}

/// Outcome of a staging-area sweep.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StagingSweep {
    pub removed_files: u64,
    pub removed_bytes: u64,
}

#[async_trait]
pub trait ObjectStore: Send + Sync + 'static {
    /// Stream `body` into the namespace, returning the content-addressed key.
    ///
    /// The key depends on the digest, so uploading identical bytes twice converges on
    /// the same object instead of duplicating it (`already_present` reports that).
    async fn put_stream(
        &self,
        namespace: &ObjectNamespace,
        body: ByteStream<'_>,
    ) -> StorageResult<StoredObject>;

    /// Open an object for reading. The key is re-validated before use.
    async fn open(&self, key: &ObjectKey) -> StorageResult<ObjectBody>;

    /// Remove an object. Missing objects are not an error (idempotent cleanup).
    async fn delete(&self, key: &ObjectKey) -> StorageResult<()>;

    /// Readiness probe: verifies the store exists and is writable.
    async fn health(&self) -> StorageResult<()>;

    /// Remove staging files left behind by interrupted uploads (crash recovery).
    async fn sweep_staging(&self, older_than: Duration) -> StorageResult<StagingSweep>;

    /// List stored objects, at most `limit` of them.
    ///
    /// Used by maintenance to compare what is on disk with what the database knows
    /// about (orphan reconciliation). The bound keeps a single pass predictable.
    async fn list_objects(&self, limit: usize) -> StorageResult<ObjectListing>;

    /// Short, non-secret description for logs and `/ready`.
    fn describe(&self) -> String;
}
