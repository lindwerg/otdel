//! Local filesystem backend for [`ObjectStore`](crate::ObjectStore).
//!
//! Layout under the configured root (`.local/storage` by default, git-ignored):
//!
//! ```text
//! <root>/objects/bureau-<uuid>/partner-<uuid>/<shard>/<sha256>
//! <root>/staging/<uuid>.part
//! ```
//!
//! The tree is never exposed as a static directory: the only way to read a byte of it is
//! through the authenticated download route, which resolves a database-stored key.
//! Directories and files are created with owner-only permissions on Unix.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::fs::{self, File, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::error::{StorageError, StorageResult};
use crate::key::{ObjectKey, ObjectNamespace};
use crate::{ByteStream, ObjectBody, ObjectListing, ObjectStore, StagingSweep, StoredObject};

const OBJECTS_DIR: &str = "objects";
const STAGING_DIR: &str = "staging";

#[derive(Debug, Clone)]
pub struct FilesystemObjectStore {
    root: PathBuf,
    objects_root: PathBuf,
    staging_root: PathBuf,
}

impl FilesystemObjectStore {
    /// Create (if needed) and validate the storage tree.
    pub async fn open_at(root: impl Into<PathBuf>) -> StorageResult<Self> {
        let root = root.into();
        create_private_dir(&root).await?;
        let root = fs::canonicalize(&root)
            .await
            .map_err(|source| StorageError::io("canonicalize storage root", source))?;

        let objects_root = root.join(OBJECTS_DIR);
        let staging_root = root.join(STAGING_DIR);
        create_private_dir(&objects_root).await?;
        create_private_dir(&staging_root).await?;

        Ok(Self {
            root,
            objects_root,
            staging_root,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[async_trait]
impl ObjectStore for FilesystemObjectStore {
    async fn put_stream(
        &self,
        namespace: &ObjectNamespace,
        mut body: ByteStream<'_>,
    ) -> StorageResult<StoredObject> {
        let staging_path = self.staging_root.join(format!("{}.part", Uuid::new_v4()));
        // From this point the staging file is removed on every exit path, including
        // early `?` returns and panics.
        let guard = StagingGuard::new(staging_path.clone());

        let mut file = private_create(&staging_path).await?;
        let mut hasher = Sha256::new();
        let mut size_bytes: u64 = 0;

        while let Some(chunk) = body.next().await {
            let chunk = chunk?;
            hasher.update(&chunk);
            size_bytes += chunk.len() as u64;
            file.write_all(&chunk)
                .await
                .map_err(|source| StorageError::io("write staging file", source))?;
        }

        file.flush()
            .await
            .map_err(|source| StorageError::io("flush staging file", source))?;
        file.sync_all()
            .await
            .map_err(|source| StorageError::io("fsync staging file", source))?;
        drop(file);

        let sha256 = hex_lower(&hasher.finalize());
        let key = namespace.key_for_digest(&sha256)?;
        let final_path = key.resolve_within(&self.objects_root)?;

        if let Some(parent) = final_path.parent() {
            create_private_dir(parent).await?;
        }

        let existing = fs::symlink_metadata(&final_path).await;
        let already_present = match existing {
            Ok(metadata) if metadata.is_file() && metadata.len() == size_bytes => true,
            Ok(metadata) => {
                // Same digest but a different size on disk means the stored copy is
                // damaged or truncated; replace it with the complete upload.
                warn!(
                    key = %key,
                    stored_size = metadata.len(),
                    uploaded_size = size_bytes,
                    "replacing damaged stored object with a freshly uploaded copy"
                );
                false
            }
            Err(error) if error.kind() == ErrorKind::NotFound => false,
            Err(source) => return Err(StorageError::io("stat stored object", source)),
        };

        if already_present {
            debug!(key = %key, "object already stored, reusing it");
            drop(guard);
        } else {
            fs::rename(&staging_path, &final_path)
                .await
                .map_err(|source| StorageError::io("publish staging file", source))?;
            guard.disarm();
            sync_dir(final_path.parent().unwrap_or(&self.objects_root)).await;
        }

        Ok(StoredObject {
            key,
            sha256,
            size_bytes,
            already_present,
        })
    }

    async fn open(&self, key: &ObjectKey) -> StorageResult<ObjectBody> {
        let path = key.resolve_within(&self.objects_root)?;

        let metadata = match fs::symlink_metadata(&path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Err(StorageError::NotFound(key.to_string()))
            }
            Err(source) => return Err(StorageError::io("stat object", source)),
        };
        if !metadata.is_file() {
            // A symlink or directory under the objects tree is never something this
            // store wrote; refuse to follow it.
            return Err(StorageError::InvalidKey(
                "stored path is not a regular file".to_owned(),
            ));
        }

        let file = File::open(&path)
            .await
            .map_err(|source| StorageError::io("open object", source))?;

        Ok(ObjectBody {
            size_bytes: metadata.len(),
            stream: Box::pin(ReaderStream::new(file)),
        })
    }

    async fn delete(&self, key: &ObjectKey) -> StorageResult<()> {
        let path = key.resolve_within(&self.objects_root)?;
        match fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(source) => Err(StorageError::io("delete object", source)),
        }
    }

    async fn health(&self) -> StorageResult<()> {
        for dir in [&self.objects_root, &self.staging_root] {
            let metadata = fs::metadata(dir)
                .await
                .map_err(|source| StorageError::io("stat storage directory", source))?;
            if !metadata.is_dir() {
                return Err(StorageError::Unavailable(
                    "storage path is not a directory".to_owned(),
                ));
            }
        }

        let probe = self
            .staging_root
            .join(format!("health-{}.probe", Uuid::new_v4()));
        let guard = StagingGuard::new(probe.clone());
        let mut file = private_create(&probe).await?;
        file.write_all(b"ok")
            .await
            .map_err(|source| StorageError::io("write health probe", source))?;
        file.flush()
            .await
            .map_err(|source| StorageError::io("flush health probe", source))?;
        drop(file);
        drop(guard);
        Ok(())
    }

    async fn sweep_staging(&self, older_than: Duration) -> StorageResult<StagingSweep> {
        let mut sweep = StagingSweep::default();
        let mut entries = fs::read_dir(&self.staging_root)
            .await
            .map_err(|source| StorageError::io("read staging directory", source))?;

        let cutoff = SystemTime::now()
            .checked_sub(older_than)
            .unwrap_or(SystemTime::UNIX_EPOCH);

        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|source| StorageError::io("iterate staging directory", source))?
        {
            let metadata = match entry.metadata().await {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(source) => return Err(StorageError::io("stat staging entry", source)),
            };
            if !metadata.is_file() {
                continue;
            }
            let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if modified > cutoff {
                continue;
            }
            match fs::remove_file(entry.path()).await {
                Ok(()) => {
                    sweep.removed_files += 1;
                    sweep.removed_bytes += metadata.len();
                }
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(source) => return Err(StorageError::io("remove staging entry", source)),
            }
        }

        Ok(sweep)
    }

    async fn list_objects(&self, limit: usize) -> StorageResult<ObjectListing> {
        let mut listing = ObjectListing::default();
        if limit == 0 {
            return Ok(listing);
        }

        // Depth-first walk over bureau/partner/shard directories. Entries that do not
        // form a valid key are reported as such rather than silently skipped: anything
        // unexpected under the objects root deserves an operator's attention.
        let mut stack = vec![(self.objects_root.clone(), Vec::<String>::new())];
        while let Some((dir, prefix)) = stack.pop() {
            let mut entries = match fs::read_dir(&dir).await {
                Ok(entries) => entries,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(source) => return Err(StorageError::io("read objects directory", source)),
            };

            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|source| StorageError::io("iterate objects directory", source))?
            {
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    warn!("skipping an object entry with a non-UTF-8 name");
                    continue;
                };
                let metadata = match entry.metadata().await {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == ErrorKind::NotFound => continue,
                    Err(source) => return Err(StorageError::io("stat object entry", source)),
                };

                let mut segments = prefix.clone();
                segments.push(name);

                if metadata.is_dir() {
                    // Directories exist at three levels only: bureau, partner, shard.
                    // Objects themselves are the fourth segment.
                    if segments.len() <= 3 {
                        stack.push((entry.path(), segments));
                    } else {
                        warn!("unexpected directory below the object shard level");
                    }
                    continue;
                }
                if !metadata.is_file() {
                    warn!("skipping a non-regular file under the objects root");
                    continue;
                }

                match ObjectKey::parse(&segments.join("/")) {
                    Ok(key) => {
                        listing.keys.push(key);
                        if listing.keys.len() >= limit {
                            listing.truncated = true;
                            return Ok(listing);
                        }
                    }
                    Err(error) => {
                        warn!(error = %error, "file under the objects root is not a valid object")
                    }
                }
            }
        }

        Ok(listing)
    }

    fn describe(&self) -> String {
        format!("filesystem:{}", self.root.display())
    }
}

/// Removes a staging file unless the upload succeeded and disarmed it.
struct StagingGuard {
    path: Option<PathBuf>,
}

impl StagingGuard {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn disarm(mut self) {
        self.path = None;
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            // Synchronous removal on purpose: this must also run when the future is
            // dropped (client disconnect) or unwound, where awaiting is impossible.
            match std::fs::remove_file(&path) {
                Ok(()) => debug!("removed staging file after an unfinished upload"),
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => warn!(
                    error = %error,
                    "could not remove staging file; it will be swept later"
                ),
            }
        }
    }
}

async fn create_private_dir(path: &Path) -> StorageResult<()> {
    fs::create_dir_all(path)
        .await
        .map_err(|source| StorageError::io("create storage directory", source))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|source| StorageError::io("restrict storage directory", source))?;
    }
    Ok(())
}

async fn private_create(path: &Path) -> StorageResult<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        // `tokio::fs::OpenOptions::mode` is the inherent unix extension.
        options.mode(0o600);
    }
    options
        .open(path)
        .await
        .map_err(|source| StorageError::io("create staging file", source))
}

/// Best-effort directory fsync so a published rename survives a crash.
async fn sync_dir(dir: &Path) {
    match File::open(dir).await {
        Ok(handle) => {
            if let Err(error) = handle.sync_all().await {
                debug!(error = %error, "directory fsync failed (non-fatal)");
            }
        }
        Err(error) => debug!(error = %error, "could not open directory for fsync (non-fatal)"),
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{StreamError, StreamErrorKind};
    use bytes::Bytes;

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("otdel-storage-test-{name}-{}", Uuid::new_v4()))
    }

    fn namespace() -> ObjectNamespace {
        ObjectNamespace::new(Uuid::new_v4(), Uuid::new_v4())
    }

    fn stream_of(chunks: Vec<Result<Bytes, StreamError>>) -> ByteStream<'static> {
        Box::pin(futures_util::stream::iter(chunks))
    }

    async fn collect(mut body: ObjectBody) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(chunk) = body.stream.next().await {
            out.extend_from_slice(&chunk.unwrap());
        }
        out
    }

    #[tokio::test]
    async fn stores_reads_and_deduplicates() {
        let root = temp_root("roundtrip");
        let store = FilesystemObjectStore::open_at(&root).await.unwrap();
        let namespace = namespace();

        let first = store
            .put_stream(
                &namespace,
                stream_of(vec![
                    Ok(Bytes::from_static(b"%PDF-1.7 ")),
                    Ok(Bytes::from_static(b"body")),
                ]),
            )
            .await
            .unwrap();
        assert!(!first.already_present);
        assert_eq!(first.size_bytes, 13);

        let again = store
            .put_stream(
                &namespace,
                stream_of(vec![Ok(Bytes::from_static(b"%PDF-1.7 body"))]),
            )
            .await
            .unwrap();
        assert!(again.already_present);
        assert_eq!(again.key, first.key);

        let body = store.open(&first.key).await.unwrap();
        assert_eq!(body.size_bytes, 13);
        assert_eq!(collect(body).await, b"%PDF-1.7 body");

        // Same bytes for a different partner get a different key: originals are never
        // shared across the tenant namespace.
        let other = store
            .put_stream(
                &ObjectNamespace::new(namespace.bureau_id, Uuid::new_v4()),
                stream_of(vec![Ok(Bytes::from_static(b"%PDF-1.7 body"))]),
            )
            .await
            .unwrap();
        assert_ne!(other.key, first.key);
        assert_eq!(other.sha256, first.sha256);

        store.delete(&first.key).await.unwrap();
        assert!(matches!(
            store.open(&first.key).await,
            Err(StorageError::NotFound(_))
        ));
        // Deleting twice is not an error.
        store.delete(&first.key).await.unwrap();

        fs::remove_dir_all(&root).await.unwrap();
    }

    #[tokio::test]
    async fn failed_stream_leaves_no_staging_file_and_no_object() {
        let root = temp_root("failure");
        let store = FilesystemObjectStore::open_at(&root).await.unwrap();

        let error = store
            .put_stream(
                &namespace(),
                stream_of(vec![
                    Ok(Bytes::from_static(b"%PDF-1.7 ")),
                    Err(StreamError::too_large("file exceeds the limit")),
                ]),
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.stream_error().map(|e| e.kind),
            Some(StreamErrorKind::TooLarge)
        );

        let mut staging = fs::read_dir(root.join(STAGING_DIR)).await.unwrap();
        assert!(staging.next_entry().await.unwrap().is_none());
        let mut objects = fs::read_dir(root.join(OBJECTS_DIR)).await.unwrap();
        assert!(objects.next_entry().await.unwrap().is_none());

        fs::remove_dir_all(&root).await.unwrap();
    }

    #[tokio::test]
    async fn sweep_removes_only_old_staging_files() {
        let root = temp_root("sweep");
        let store = FilesystemObjectStore::open_at(&root).await.unwrap();
        let leftover = root.join(STAGING_DIR).join("abandoned.part");
        fs::write(&leftover, b"partial").await.unwrap();

        let untouched = store
            .sweep_staging(Duration::from_secs(3600))
            .await
            .unwrap();
        assert_eq!(untouched, StagingSweep::default());
        assert!(fs::metadata(&leftover).await.is_ok());

        let swept = store.sweep_staging(Duration::ZERO).await.unwrap();
        assert_eq!(swept.removed_files, 1);
        assert_eq!(swept.removed_bytes, 7);
        assert!(fs::metadata(&leftover).await.is_err());

        fs::remove_dir_all(&root).await.unwrap();
    }

    #[tokio::test]
    async fn lists_stored_objects_within_the_limit() {
        let root = temp_root("listing");
        let store = FilesystemObjectStore::open_at(&root).await.unwrap();
        let namespace = namespace();

        let first = store
            .put_stream(
                &namespace,
                stream_of(vec![Ok(Bytes::from_static(b"%PDF-a"))]),
            )
            .await
            .unwrap();
        let second = store
            .put_stream(
                &namespace,
                stream_of(vec![Ok(Bytes::from_static(b"%PDF-b"))]),
            )
            .await
            .unwrap();

        let listing = store.list_objects(10).await.unwrap();
        assert!(!listing.truncated);
        assert_eq!(listing.keys.len(), 2);
        assert!(listing.keys.contains(&first.key));
        assert!(listing.keys.contains(&second.key));
        assert_eq!(
            listing.keys[0].namespace().unwrap().partner_id,
            namespace.partner_id
        );

        let truncated = store.list_objects(1).await.unwrap();
        assert!(truncated.truncated);
        assert_eq!(truncated.keys.len(), 1);

        fs::remove_dir_all(&root).await.unwrap();
    }

    #[tokio::test]
    async fn health_checks_a_writable_tree() {
        let root = temp_root("health");
        let store = FilesystemObjectStore::open_at(&root).await.unwrap();
        store.health().await.unwrap();
        assert!(store.describe().starts_with("filesystem:"));

        fs::remove_dir_all(&root).await.unwrap();
        assert!(store.health().await.is_err());
    }
}
