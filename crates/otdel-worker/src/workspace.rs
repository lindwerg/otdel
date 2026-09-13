//! A private scratch directory for one job.
//!
//! Two things happen here that are worth being explicit about.
//!
//! **The original is copied out of the object store.** The store is an interface
//! (`ObjectStore`), and the external tools need a real file, so the bytes are streamed
//! into a temporary file. Going through `open()` rather than reaching into the
//! filesystem backend is what keeps the S3 implementation named in the specification a
//! drop-in change instead of a rewrite.
//!
//! **Nothing here is published.** The directory lives outside the object store — so the
//! maintenance orphan scan never sees it — is created with owner-only permissions, and is
//! removed when the job ends, including on failure. Rendered page images are transient by
//! design: phase 1B stores no page snapshots and the interface links to the original
//! instead of pretending that it does.

use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use otdel_storage::{ObjectKey, ObjectStore};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::error::WorkerError;

/// Scratch space for one job, deleted on drop-time cleanup.
pub struct JobWorkspace {
    root: PathBuf,
}

impl JobWorkspace {
    /// Create `<base>/otdel-extract-<uuid>/`.
    pub async fn create(base: Option<&Path>) -> Result<Self, WorkerError> {
        let base = base.map_or_else(std::env::temp_dir, Path::to_path_buf);
        let root = base.join(format!("otdel-extract-{}", Uuid::new_v4()));
        create_private_dir(&root).await?;
        Ok(Self { root })
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    /// A fresh empty subdirectory, so a rendered image can be found without guessing
    /// the name a particular tool version produces.
    pub async fn subdirectory(&self, name: &str) -> Result<PathBuf, WorkerError> {
        let path = self.root.join(name);
        create_private_dir(&path).await?;
        Ok(path)
    }

    /// Stream a stored original into this workspace and return both the bytes and the
    /// file path. The bytes are needed for in-process parsing, the path for the external
    /// tools.
    pub async fn materialise(
        &self,
        store: &dyn ObjectStore,
        key: &ObjectKey,
        file_name: &str,
    ) -> Result<(PathBuf, Vec<u8>), WorkerError> {
        let path = self.root.join(file_name);
        let body = store.open(key).await?;
        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(|error| WorkerError::Workspace(error.to_string()))?;

        let mut bytes = Vec::with_capacity(usize::try_from(body.size_bytes).unwrap_or(0));
        let mut stream = body.stream;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| WorkerError::Workspace(error.to_string()))?;
            file.write_all(&chunk)
                .await
                .map_err(|error| WorkerError::Workspace(error.to_string()))?;
            bytes.extend_from_slice(&chunk);
        }
        file.flush()
            .await
            .map_err(|error| WorkerError::Workspace(error.to_string()))?;
        drop(file);

        Ok((path, bytes))
    }

    /// Remove the whole directory. Failure is reported by the caller's log, never
    /// silently swallowed — a scratch directory that survives is a disk leak.
    pub async fn cleanup(self) -> Result<(), WorkerError> {
        match tokio::fs::remove_dir_all(&self.root).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(WorkerError::Workspace(error.to_string())),
        }
    }
}

async fn create_private_dir(path: &Path) -> Result<(), WorkerError> {
    tokio::fs::create_dir_all(path)
        .await
        .map_err(|error| WorkerError::Workspace(error.to_string()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|error| WorkerError::Workspace(error.to_string()))?;
    }
    Ok(())
}

/// File name for the materialised original: server-chosen, with an extension that only
/// depends on the recorded media type. The uploaded file name is never used — it is
/// partner-supplied text and has no business appearing in a path or on a command line.
pub fn original_file_name(media_type: &str) -> &'static str {
    match media_type {
        "image/png" => "original.png",
        "image/jpeg" => "original.jpg",
        _ => "original.pdf",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_workspace_is_private_and_removable() {
        let workspace = JobWorkspace::create(None).await.unwrap();
        let root = workspace.path().to_path_buf();
        assert!(root.is_dir());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&root).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "scratch space must be owner-only");
        }

        let sub = workspace.subdirectory("page-3").await.unwrap();
        assert!(sub.is_dir());

        workspace.cleanup().await.unwrap();
        assert!(!root.exists());
    }

    #[test]
    fn the_stored_file_name_never_comes_from_the_upload() {
        assert_eq!(original_file_name("application/pdf"), "original.pdf");
        assert_eq!(original_file_name("image/png"), "original.png");
        assert_eq!(original_file_name("image/jpeg"), "original.jpg");
        // Anything unexpected still yields a fixed, safe name.
        assert_eq!(original_file_name("../../etc/passwd"), "original.pdf");
    }
}
