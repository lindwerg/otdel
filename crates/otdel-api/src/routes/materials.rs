//! `/api/partners/{id}/materials` — intake, listing, download and retry.
//!
//! Ordering guarantee of the upload endpoint (contract, “Хранение и безопасность 1A”):
//! the object is written and fsynced first, then the material row and its queue entry
//! are published in **one** transaction. If that transaction cannot commit, the client
//! gets a real error (no silent half-success), the request's staging file is removed by
//! the object store, and the finalized object is retained and reported rather than
//! deleted — see [`crate::upload::report_orphaned_object`] for why deleting it would be
//! unsafe next to a concurrent upload of the same bytes.

use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{body::Body, Json};
use otdel_core::model::{media_type_string, Material};
use otdel_core::updates::{EventActor, EventKind};
use otdel_core::{validate, AppError};
use otdel_db::materials::{self, InsertOutcome, NewMaterial};
use otdel_db::{events, jobs, partners};
use otdel_storage::{ObjectKey, ObjectNamespace};
use tracing::{error, info};
use uuid::Uuid;

use crate::auth::Session;
use crate::dto::ItemsResponse;
use crate::error::{ApiError, ApiResult};
use crate::extract::{ApiMultipart, ApiPath};
use crate::routes::partners::partner_not_found;
use crate::state::AppState;
use crate::upload;

pub async fn list(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
) -> ApiResult<Json<ItemsResponse<Material>>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }
    // With the page roll-up: the listing is where the owner sees how far reading got,
    // and the counters come from the page rows themselves.
    let items = materials::list_for_partner_with_extraction(&mut tx, partner_id).await?;
    tx.commit().await?;

    Ok(Json(ItemsResponse::new(items)))
}

/// `GET /api/partners/{id}/materials/{material_id}` — one material with its page summary.
pub async fn show(
    State(state): State<AppState>,
    session: Session,
    ApiPath((partner_id, material_id)): ApiPath<(Uuid, Uuid)>,
) -> ApiResult<Json<Material>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    let material =
        materials::get_in_partner_with_extraction(&mut tx, partner_id, material_id).await?;
    tx.commit().await?;

    material.map(Json).ok_or_else(material_not_found)
}

/// `POST /api/partners/{id}/materials` — one file per request.
///
/// 201 for a new original, 200 when the exact same bytes were already uploaded for this
/// partner (deduplication by `partner_id` + SHA-256). A new *version* of a document is a
/// different digest and therefore a new row: originals are never overwritten.
pub async fn upload(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
    ApiMultipart(mut multipart): ApiMultipart,
) -> ApiResult<Response> {
    // Fail before touching storage if the partner is unknown or belongs elsewhere.
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    let partner_exists = partners::exists(&mut tx, partner_id).await?;
    tx.commit().await?;
    if !partner_exists {
        return Err(partner_not_found());
    }

    let namespace = ObjectNamespace::new(session.bureau_id, partner_id);
    let received = upload::receive_file(&state, &namespace, &mut multipart).await?;

    let new_material = NewMaterial {
        partner_id,
        filename: received.filename.clone(),
        media_type: media_type_string(received.media_type),
        size_bytes: i64::try_from(received.stored.size_bytes).map_err(|_| {
            ApiError::new(AppError::new(
                otdel_core::ErrorCode::PayloadTooLarge,
                "file is too large to record",
            ))
        })?,
        sha256: received.stored.sha256.clone(),
        storage_key: received.stored.key.as_str().to_owned(),
    };

    // Metadata and queue entry are published together; either both are visible or
    // neither is.
    let published = async {
        let mut tx = state.db.begin_scoped(session.bureau_id).await?;
        let outcome = materials::insert(&mut tx, &new_material).await?;
        let material = match &outcome {
            InsertOutcome::Created(material) | InsertOutcome::Duplicate(material) => material,
        };
        let job = jobs::enqueue_extraction(&mut tx, partner_id, material.id).await?;

        // Phase 1F: the history line, in the same transaction as the row it describes.
        // A duplicate is recorded too, and as its own kind: "я загрузил файл и ничего не
        // произошло" has an answer, and the answer is that these bytes were already here.
        let (kind, summary) = match &outcome {
            InsertOutcome::Created(material) => (
                EventKind::MaterialUploaded,
                format!(
                    "загружен материал «{}» ({} байт); поставлен в очередь на чтение",
                    material.filename, material.size_bytes
                ),
            ),
            InsertOutcome::Duplicate(material) => (
                EventKind::MaterialDuplicate,
                format!(
                    "повторная загрузка «{}»: файл с тем же содержимым уже есть у этого \
                     партнёра, новая копия не создана",
                    material.filename
                ),
            ),
        };
        events::record(
            &mut tx,
            &events::NewEvent::new(kind, EventActor::Owner, summary)
                .for_partner(partner_id)
                .about_material(material.id)
                .about_job(job.id)
                .with_detail(serde_json::json!({
                    "media_type": material.media_type,
                    "size_bytes": material.size_bytes,
                    "sha256": material.sha256,
                })),
        )
        .await?;

        tx.commit().await?;
        Ok::<_, otdel_db::DbError>(outcome)
    }
    .await;

    let outcome = match published {
        Ok(outcome) => outcome,
        Err(error) => {
            error!(error = %error, "could not publish the uploaded material");
            // The object stays: another request may already have adopted the identical
            // content. See `upload::report_orphaned_object`.
            upload::report_orphaned_object(&received.stored);
            return Err(ApiError::from(error));
        }
    };

    match outcome {
        InsertOutcome::Created(material) => {
            info!(
                material_id = %material.id,
                partner_id = %partner_id,
                size_bytes = material.size_bytes,
                "original stored and queued for extraction"
            );
            Ok((StatusCode::CREATED, Json(material)).into_response())
        }
        InsertOutcome::Duplicate(material) => {
            info!(
                material_id = %material.id,
                partner_id = %partner_id,
                "upload matched an existing original for this partner"
            );
            Ok((StatusCode::OK, Json(material)).into_response())
        }
    }
}

/// `GET /api/partners/{id}/materials/{material_id}/original`
///
/// The object store is never exposed as a directory: the only path that can be read is
/// the one recorded for a material that belongs to *this* partner and *this* bureau, and
/// the stored key is re-validated before it is resolved.
pub async fn download(
    State(state): State<AppState>,
    session: Session,
    ApiPath((partner_id, material_id)): ApiPath<(Uuid, Uuid)>,
) -> ApiResult<Response> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    let stored = materials::get_in_partner(&mut tx, partner_id, material_id).await?;
    tx.commit().await?;

    let Some(stored) = stored else {
        return Err(material_not_found());
    };

    let key = ObjectKey::parse(&stored.storage_key).map_err(|error| {
        error!(error = %error, material_id = %material_id, "stored key failed validation");
        ApiError::new(AppError::internal("internal server error"))
    })?;
    let body = state.store.open(&key).await?;
    let size_bytes = body.size_bytes;

    let mut response = Response::new(Body::from_stream(body.stream));
    let headers = response.headers_mut();

    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&stored.material.media_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&size_bytes.to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    if let Some(disposition) = content_disposition(&stored.material) {
        headers.insert(header::CONTENT_DISPOSITION, disposition);
    }
    // The body is user-supplied: never let a browser sniff it into something executable.
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );

    Ok(response)
}

/// `POST /api/partners/{id}/materials/{material_id}/retry`
///
/// Only a material that actually failed (or finished partially) can be retried; the job
/// row is reused, so pressing retry twice does not create a second queue entry.
pub async fn retry(
    State(state): State<AppState>,
    session: Session,
    ApiPath((partner_id, material_id)): ApiPath<(Uuid, Uuid)>,
) -> ApiResult<Json<Material>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;

    let Some(stored) = materials::get_in_partner(&mut tx, partner_id, material_id).await? else {
        return Err(material_not_found());
    };

    if !stored.material.status.can_retry() {
        return Err(ApiError::new(stored.material.status.retry_refusal()));
    }

    materials::requeue(&mut tx, partner_id, material_id)
        .await?
        .ok_or_else(material_not_found)?;
    let job = jobs::requeue_extraction(&mut tx, partner_id, material_id).await?;
    // Re-read with the page summary attached, so the client sees the same shape the
    // listing returns and knows what is still recorded from the previous run.
    let material = materials::get_in_partner_with_extraction(&mut tx, partner_id, material_id)
        .await?
        .ok_or_else(material_not_found)?;
    tx.commit().await?;

    info!(
        material_id = %material.id,
        job_id = %job.id,
        "material queued again for extraction"
    );
    Ok(Json(material))
}

/// Same wording whether the material does not exist, belongs to another partner or to
/// another bureau.
fn material_not_found() -> ApiError {
    ApiError::not_found("material not found for this partner")
}

fn content_disposition(material: &Material) -> Option<HeaderValue> {
    let media_type = otdel_core::MediaType::parse(&material.media_type)?;
    let ascii = validate::content_disposition_ascii_fallback(&material.filename, media_type);
    let utf8 = validate::content_disposition_utf8(&material.filename);
    HeaderValue::from_str(&format!(
        "attachment; filename=\"{ascii}\"; filename*={utf8}"
    ))
    .ok()
}
