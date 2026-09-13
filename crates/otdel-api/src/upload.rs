//! Streaming intake of one original file.
//!
//! What this module guarantees, in order:
//!
//! 1. the body is never buffered — only the first few bytes are held back, long enough
//!    to decide the format;
//! 2. the format is decided by the **content signature**; a declared `Content-Type` that
//!    contradicts it is rejected rather than trusted;
//! 3. the size limit is enforced while streaming, so an oversized upload is cut off
//!    instead of filling the disk first;
//! 4. the client's file name is used for display only — the storage key comes from the
//!    bureau/partner ids and the content hash;
//! 5. exactly one file per request.

use axum::extract::multipart::{Field, Multipart};
use bytes::Bytes;
use futures_util::{stream, StreamExt, TryStreamExt};
use otdel_core::media::{MediaType, SIGNATURE_PREFIX_LEN};
use otdel_core::{validate, AppError, ErrorCode};
use otdel_storage::{ByteStream, ObjectNamespace, StoredObject, StreamError};
use tracing::{debug, warn};

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// Bytes allowed for non-file fields before the request is rejected: the endpoint takes
/// a file, not arbitrary form data.
const MAX_OTHER_FIELD_BYTES: u64 = 64 * 1024;

/// Outcome of a successful intake.
#[derive(Debug)]
pub struct ReceivedFile {
    /// Sanitised display name (never a path).
    pub filename: String,
    pub media_type: MediaType,
    pub stored: StoredObject,
}

/// Read the `file` part of a multipart request into the object store.
pub async fn receive_file(
    state: &AppState,
    namespace: &ObjectNamespace,
    multipart: &mut Multipart,
) -> ApiResult<ReceivedFile> {
    let mut received: Option<ReceivedFile> = None;
    let mut other_field_bytes: u64 = 0;

    while let Some(mut field) = multipart.next_field().await.map_err(|error| {
        debug!(error = %error, "multipart parsing failed");
        ApiError::new(AppError::bad_request(
            "request body is not valid multipart form data",
        ))
    })? {
        let is_file_part = field.name() == Some("file");

        if !is_file_part {
            // Drain and ignore, but do not let a client stream unlimited “other” data.
            while let Some(chunk) = field.chunk().await.map_err(|_| interrupted())? {
                other_field_bytes += chunk.len() as u64;
                if other_field_bytes > MAX_OTHER_FIELD_BYTES {
                    return Err(ApiError::new(AppError::new(
                        ErrorCode::PayloadTooLarge,
                        "unexpected form fields are too large; send only the `file` part",
                    )));
                }
            }
            continue;
        }

        if let Some(previous) = received.as_ref() {
            // The first file is already published at this point. It is reported, not
            // deleted — see `report_orphaned_object`.
            report_orphaned_object(&previous.stored);
            return Err(ApiError::new(AppError::validation(
                "send exactly one file per request",
            )));
        }

        received = Some(read_file_field(state, namespace, field).await?);
    }

    received.ok_or_else(|| {
        ApiError::new(AppError::validation(
            "multipart request must contain a `file` part",
        ))
    })
}

async fn read_file_field(
    state: &AppState,
    namespace: &ObjectNamespace,
    mut field: Field<'_>,
) -> ApiResult<ReceivedFile> {
    let declared_name = field.file_name().map(str::to_owned);
    let declared_type = field.content_type().map(str::to_owned);

    // Hold back just enough bytes to recognise the signature.
    let mut prefix: Vec<u8> = Vec::with_capacity(SIGNATURE_PREFIX_LEN);
    let mut head: Vec<Bytes> = Vec::new();
    while prefix.len() < SIGNATURE_PREFIX_LEN {
        match field.chunk().await.map_err(|_| interrupted())? {
            Some(chunk) => {
                prefix.extend_from_slice(&chunk);
                head.push(chunk);
            }
            None => break,
        }
    }

    if prefix.is_empty() {
        return Err(ApiError::new(AppError::validation(
            "the uploaded file is empty",
        )));
    }

    let media_type = MediaType::detect(&prefix).ok_or_else(|| {
        ApiError::new(AppError::new(
            ErrorCode::UnsupportedMediaType,
            format!(
                "file content is not one of the formats accepted in this phase ({})",
                MediaType::accepted_list()
            ),
        ))
    })?;

    // A declared type that names a *different* accepted format is a contradiction and is
    // refused. An unknown declaration (e.g. application/octet-stream) is simply ignored:
    // the signature decides.
    if let Some(declared) = declared_type.as_deref() {
        if let Some(parsed) = MediaType::parse(declared) {
            if parsed != media_type {
                return Err(ApiError::new(AppError::new(
                    ErrorCode::UnsupportedMediaType,
                    format!(
                        "declared content type `{}` does not match the file content ({})",
                        parsed.as_str(),
                        media_type.as_str()
                    ),
                )));
            }
        }
    }

    let filename =
        validate::display_filename(declared_name.as_deref().unwrap_or_default(), media_type);

    let max_bytes = state.config.max_upload_bytes;
    let body: ByteStream<'_> = Box::pin(limit_size(
        stream::iter(head.into_iter().map(Ok)).chain(field_stream(field)),
        max_bytes,
    ));

    let stored = state.store.put_stream(namespace, body).await?;

    Ok(ReceivedFile {
        filename,
        media_type,
        stored,
    })
}

/// Turn the remaining part of a multipart field into a byte stream.
fn field_stream(
    field: Field<'_>,
) -> impl stream::Stream<Item = Result<Bytes, StreamError>> + Send + '_ {
    stream::try_unfold(field, |mut field| async move {
        match field.chunk().await {
            Ok(Some(chunk)) => Ok(Some((chunk, field))),
            Ok(None) => Ok(None),
            Err(error) => {
                debug!(error = %error, "upload stream failed");
                Err(StreamError::upstream(
                    "the upload was interrupted before the file was fully received",
                ))
            }
        }
    })
}

/// Stop the stream with a `TooLarge` error as soon as the limit is passed, so the writer
/// never stores more than the configured maximum.
fn limit_size<S>(
    inner: S,
    max_bytes: u64,
) -> impl stream::Stream<Item = Result<Bytes, StreamError>> + Send
where
    S: stream::Stream<Item = Result<Bytes, StreamError>> + Send,
{
    stream::try_unfold(
        (Box::pin(inner), 0u64),
        move |(mut inner, written)| async move {
            let Some(chunk) = inner.try_next().await? else {
                return Ok(None);
            };
            let written = written + chunk.len() as u64;
            if written > max_bytes {
                return Err(StreamError::too_large(format!(
                    "file exceeds the {max_bytes}-byte upload limit"
                )));
            }
            Ok(Some((chunk, (inner, written))))
        },
    )
}

/// Report — never delete — an object that was published while the request then failed.
///
/// A published object is *content-addressed*: its key is derived from the bytes, so two
/// requests uploading the same file for the same partner converge on the same object.
/// That makes “delete it because my request failed” unsafe at any distance:
///
/// 1. request A stores the object and then fails (second file part, database outage);
/// 2. request B uploads the identical file, finds the object already present, and
///    commits its material row;
/// 3. A's cleanup deletes the object — and B's accepted original is gone.
///
/// A “check the metadata, then delete” sequence does not fix this either: B can commit
/// between A's check and A's delete. Deleting finalized originals would require a lock
/// held across the whole publish-then-commit protocol, which is not worth introducing in
/// 1A. So the rule here is: the *staging* file is always cleaned up (that one belongs to
/// this request alone, and the object store removes it on every failure path), while a
/// finalized object is retained and reported. The maintenance worker likewise reports
/// orphans instead of collecting them.
///
/// The cost is bounded: an orphan is one copy of a file the owner did try to upload,
/// visible in the log and in the worker's orphan count.
pub fn report_orphaned_object(stored: &StoredObject) {
    if stored.already_present {
        debug!("upload failed after storing, but those bytes were already stored before");
        return;
    }
    warn!(
        key = %stored.key,
        size_bytes = stored.size_bytes,
        "stored original was not recorded; it is retained (never deleted from an error \
         path, a concurrent upload may legitimately adopt the same content) and reported \
         by the maintenance worker"
    );
}

fn interrupted() -> ApiError {
    ApiError::new(AppError::bad_request(
        "the upload was interrupted before the file was fully received",
    ))
}
