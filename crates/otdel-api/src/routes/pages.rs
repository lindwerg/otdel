//! `/api/partners/{id}/materials/{material_id}/pages` — the phase 1B evidence.
//!
//! Three endpoints, and one rule behind all of them: a page is reachable only through
//! the partner it belongs to. Every lookup resolves the material *inside that partner*
//! first, so a page id guessed from another bureau's material is simply not found —
//! the same wording whether the page does not exist, belongs to another partner, or
//! belongs to another bureau.
//!
//! The page text and its regions are partner data, so they travel the same authenticated,
//! bureau-scoped path as the original file. Nothing here serves an image or a file: the
//! original is fetched from the existing download route, which the interface links to
//! with the page number.

use axum::extract::State;
use axum::Json;
use otdel_core::extraction::{MaterialPage, PageDetail, PageStatus, RegionKind};
use otdel_core::extraction_context::{DiagramInterpretation, SourceSpan};
use otdel_core::{AppError, ErrorCode};
use otdel_db::{jobs, materials, pages};
use serde::Serialize;
use tracing::info;
use uuid::Uuid;

use crate::auth::Session;
use crate::dto::ItemsResponse;
use crate::error::{ApiError, ApiResult};
use crate::extract::ApiPath;
use crate::state::AppState;

/// `GET .../pages` — one row per page, in page order, without the page text.
pub async fn list(
    State(state): State<AppState>,
    session: Session,
    ApiPath((partner_id, material_id)): ApiPath<(Uuid, Uuid)>,
) -> ApiResult<Json<ItemsResponse<MaterialPage>>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if materials::get_in_partner(&mut tx, partner_id, material_id)
        .await?
        .is_none()
    {
        return Err(material_not_found());
    }
    let items = pages::list_for_material(&mut tx, material_id).await?;
    tx.commit().await?;

    Ok(Json(ItemsResponse::new(items)))
}

/// `GET .../pages/{page_number}` — the page with its text and source regions.
pub async fn show(
    State(state): State<AppState>,
    session: Session,
    ApiPath((partner_id, material_id, page_number)): ApiPath<(Uuid, Uuid, i32)>,
) -> ApiResult<Json<PageDetail>> {
    let page_number = valid_page_number(page_number)?;

    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if materials::get_in_partner(&mut tx, partner_id, material_id)
        .await?
        .is_none()
    {
        return Err(material_not_found());
    }
    let detail = pages::page_detail(&mut tx, material_id, page_number).await?;
    tx.commit().await?;

    detail.map(Json).ok_or_else(page_not_found)
}

/// A region that can actually be drawn, in the page's own point coordinates.
#[derive(Debug, Clone, Serialize)]
pub struct PlacedRegion {
    pub region_id: Uuid,
    pub ordinal: i32,
    pub kind: RegionKind,
    pub span: SourceSpan,
}

/// A region that cannot be drawn, and the reason in words. Listed, never drawn.
#[derive(Debug, Clone, Serialize)]
pub struct UnplacedRegion {
    pub region_id: Uuid,
    pub ordinal: i32,
    pub kind: RegionKind,
    pub reason: String,
}

/// Where the regions of one page sit. Not an image of the page; see [`view`].
#[derive(Debug, Clone, Serialize)]
pub struct PageView {
    pub page_number: i32,
    /// `None` when the page never declared its size. The interface then draws no map at
    /// all rather than assuming A4.
    pub width_pt: Option<f64>,
    pub height_pt: Option<f64>,
    pub rotation: i32,
    pub original_url: String,
    pub diagram_interpretation: DiagramInterpretation,
    pub regions: Vec<PlacedRegion>,
    pub unplaced: Vec<UnplacedRegion>,
}

/// `GET .../pages/{page_number}/view` — where this page's regions are, for the viewer.
///
/// Deliberately *not* a rendering. Phase 1B stores no page images, so there is nothing to
/// draw behind the rectangles, and the response says as much by shape: it carries the
/// page's own dimensions, the regions that have real coordinates, and — separately — the
/// regions that have none, each with the reason. A region whose coordinates are unknown
/// is listed in `unplaced`; it never receives an invented rectangle, because a highlight
/// that points at the wrong part of a document is worse than no highlight at all.
pub async fn view(
    State(state): State<AppState>,
    session: Session,
    ApiPath((partner_id, material_id, page_number)): ApiPath<(Uuid, Uuid, i32)>,
) -> ApiResult<Json<PageView>> {
    let page_number = valid_page_number(page_number)?;

    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    // Same containment gate as every other page route: the material is resolved inside
    // the partner before anything about the page is read.
    if materials::get_in_partner(&mut tx, partner_id, material_id)
        .await?
        .is_none()
    {
        return Err(material_not_found());
    }
    let Some(page) = pages::find_page(&mut tx, material_id, page_number).await? else {
        tx.commit().await?;
        return Err(page_not_found());
    };
    let regions = pages::regions_for_page(&mut tx, page.id).await?;
    tx.commit().await?;

    let mut placed = Vec::new();
    let mut unplaced = Vec::new();
    for detail in &regions {
        let region = &detail.region;
        if region.span.is_highlightable() {
            placed.push(PlacedRegion {
                region_id: region.id,
                ordinal: region.ordinal,
                kind: region.kind,
                span: region.span.clone(),
            });
        } else {
            unplaced.push(UnplacedRegion {
                region_id: region.id,
                ordinal: region.ordinal,
                kind: region.kind,
                reason: region
                    .span
                    .geometry
                    .reason()
                    .unwrap_or("координаты области не сохранены")
                    .to_owned(),
            });
        }
    }

    Ok(Json(PageView {
        page_number: page.page_number,
        width_pt: page.width_pt,
        height_pt: page.height_pt,
        rotation: page.rotation,
        original_url: crate::routes::materials::original_url(partner_id, material_id, page_number),
        diagram_interpretation: page.diagram_interpretation,
        regions: placed,
        unplaced,
    }))
}

/// `POST .../pages/{page_number}/retry` — read this one page again.
///
/// Allowed only for a page whose outcome repeating could change (`needs_ocr`, `partial`,
/// `failed`, or one the previous run never reached). Two presses reuse the same queue
/// row, so the button is idempotent: the second one does not create a second job and
/// does not duplicate the page.
pub async fn retry(
    State(state): State<AppState>,
    session: Session,
    ApiPath((partner_id, material_id, page_number)): ApiPath<(Uuid, Uuid, i32)>,
) -> ApiResult<Json<MaterialPage>> {
    let page_number = valid_page_number(page_number)?;

    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if materials::get_in_partner(&mut tx, partner_id, material_id)
        .await?
        .is_none()
    {
        return Err(material_not_found());
    }

    let Some(page) = pages::find_page(&mut tx, material_id, page_number).await? else {
        return Err(page_not_found());
    };
    if !page.status.can_retry() {
        return Err(ApiError::new(retry_refusal(page.status)));
    }

    let page = pages::reset_page(&mut tx, material_id, page_number)
        .await?
        .ok_or_else(page_not_found)?;
    let job = jobs::enqueue_page_extraction(&mut tx, partner_id, material_id, page_number).await?;
    tx.commit().await?;

    info!(
        material_id = %material_id,
        page_number,
        job_id = %job.id,
        "page queued for another read"
    );
    Ok(Json(page))
}

/// Why a page cannot be retried, without leaking internals.
fn retry_refusal(status: PageStatus) -> AppError {
    let message = match status {
        PageStatus::Extracted => "страница уже прочитана",
        PageStatus::Empty => "страница пуста, повторное чтение ничего не изменит",
        PageStatus::Pending | PageStatus::NeedsOcr | PageStatus::Partial | PageStatus::Failed => {
            "страницу можно перечитать"
        }
    };
    AppError::new(ErrorCode::Conflict, message)
}

/// Page numbers are 1-based and bounded before they reach a query.
fn valid_page_number(page_number: i32) -> Result<i32, ApiError> {
    if page_number >= 1 {
        Ok(page_number)
    } else {
        Err(ApiError::new(AppError::validation(
            "номер страницы должен быть положительным",
        )))
    }
}

fn material_not_found() -> ApiError {
    ApiError::not_found("material not found for this partner")
}

fn page_not_found() -> ApiError {
    ApiError::not_found("page not found for this material")
}
