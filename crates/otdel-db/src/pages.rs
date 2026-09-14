//! Pages, source regions and table cells — the phase 1B evidence layer.
//!
//! Two properties are enforced here rather than left to callers.
//!
//! **Re-reading is idempotent.** A page is keyed by `(material_id, page_number)` and
//! upserted; its regions and cells are deleted and rewritten inside the same transaction
//! as the page update. Pressing "retry" twice therefore leaves exactly one set of rows,
//! and an interrupted retry leaves either the old evidence or the new one, never a mix.
//!
//! **Text and its source agree.** Blank text is stored as `NULL` with `text_source =
//! 'none'`; a database constraint rejects any other combination, so a parser that gave
//! up cannot leave behind a page that looks read.

use chrono::{DateTime, Utc};
use otdel_core::extraction::{
    BoundingBox, CellValueKind, MaterialPage, PageDetail, PageRegion, PageStatus, RegionDetail,
    RegionKind, TableCell, TextSource,
};
use otdel_core::extraction_context::{
    AmbiguityReason, CellRole, CellUsability, CellVerdict, DiagramInterpretation, SourceSpan,
    StructuralContext,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use sqlx::postgres::PgRow;
use sqlx::Row;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::tenancy::ScopedTx;

const PAGE_COLUMNS: &str = "p.id, p.material_id, p.page_number, p.status, p.text_source, \
     p.char_count, p.word_count, p.image_count, p.width_pt, p.height_pt, p.rotation, \
     p.parser_name, p.parser_version, p.ocr_engine, p.ocr_version, p.ocr_language, \
     p.duration_ms, p.attempts, p.diagnostic, p.extracted_at, \
     p.extraction_revision, p.drawing_count, p.diagram_interpretation";

const PAGE_COUNTS: &str = "coalesce(counts.regions, 0) AS region_count, \
     coalesce(counts.tables, 0) AS table_count";

const PAGE_COUNT_JOIN: &str = "LEFT JOIN LATERAL ( \
         SELECT count(*) AS regions, \
                count(*) FILTER (WHERE r.kind = 'table') AS tables \
           FROM otdel.page_regions r \
          WHERE r.bureau_id = p.bureau_id AND r.page_id = p.id \
     ) counts ON true";

/// Geometry of a page, recorded by the inventory pass before anything is read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NewPage {
    pub page_number: i32,
    pub width_pt: Option<f64>,
    pub height_pt: Option<f64>,
    pub rotation: i32,
    pub image_count: i32,
}

/// The outcome of reading one page.
#[derive(Debug, Clone, PartialEq)]
pub struct PageOutcomeRow {
    pub page_number: i32,
    pub status: PageStatus,
    pub text_source: TextSource,
    pub text: Option<String>,
    pub char_count: i32,
    pub word_count: i32,
    pub image_count: i32,
    pub width_pt: Option<f64>,
    pub height_pt: Option<f64>,
    pub rotation: i32,
    pub parser_name: Option<String>,
    pub parser_version: Option<String>,
    pub ocr_engine: Option<String>,
    pub ocr_version: Option<String>,
    pub ocr_language: Option<String>,
    pub duration_ms: Option<i32>,
    pub diagnostic: Option<String>,
    /// Vector drawing operations on the page. Only ever used to tell a blank page from a
    /// page carrying a diagram.
    pub drawing_count: i32,
    /// Never more than `not_attempted` in this phase: reading the labels around a load
    /// diagram is not reading the diagram.
    pub diagram_interpretation: DiagramInterpretation,
}

/// A structural region to store, with its cells when it is a table.
#[derive(Debug, Clone, PartialEq)]
pub struct NewRegion {
    pub kind: RegionKind,
    pub text: String,
    pub source: TextSource,
    pub bbox: Option<BoundingBox>,
    pub row_count: Option<i32>,
    pub column_count: Option<i32>,
    pub cells: Vec<NewCell>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewCell {
    pub row_index: i32,
    pub column_index: i32,
    pub is_header: bool,
    pub raw_text: String,
    pub value_kind: CellValueKind,
    pub unit: Option<String>,
    pub column_header: Option<String>,
    pub bbox: Option<BoundingBox>,
    /// What the cell is within the table. A header is never a value.
    pub role: CellRole,
    /// Whether it may be read as a value, and why not when it may not.
    pub verdict: CellVerdict,
    /// Product, property, unit and conditions, each naming its origin.
    pub structural_context: StructuralContext,
}

/// Record the pages of a material. Existing rows keep their outcome — this pass only
/// establishes that the pages exist and how big they are.
pub async fn record_inventory(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    material_id: Uuid,
    pages: &[NewPage],
) -> DbResult<u64> {
    let bureau_id = tx.bureau_id();
    let mut written = 0u64;
    for page in pages {
        let result = sqlx::query(
            "INSERT INTO otdel.material_pages \
                 (bureau_id, partner_id, material_id, page_number, width_pt, height_pt, \
                  rotation, image_count) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (material_id, page_number) DO UPDATE \
                SET width_pt = EXCLUDED.width_pt, \
                    height_pt = EXCLUDED.height_pt, \
                    rotation = EXCLUDED.rotation, \
                    image_count = EXCLUDED.image_count, \
                    updated_at = now()",
        )
        .bind(bureau_id)
        .bind(partner_id)
        .bind(material_id)
        .bind(page.page_number)
        .bind(page.width_pt)
        .bind(page.height_pt)
        .bind(page.rotation)
        .bind(page.image_count)
        .execute(tx.conn())
        .await?;
        written += result.rows_affected();
    }
    Ok(written)
}

/// Store the outcome of one page and replace its regions in the same transaction.
///
/// Returns the page id.
pub async fn record_outcome(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    material_id: Uuid,
    outcome: &PageOutcomeRow,
    regions: &[NewRegion],
) -> DbResult<Uuid> {
    let bureau_id = tx.bureau_id();

    // Whitespace is not text. Normalised here as well as constrained in the database,
    // so the intent is visible at the call site too.
    let text = outcome
        .text
        .as_deref()
        .map(storable)
        .filter(|value| !value.trim().is_empty());
    let text_source = if text.is_none() {
        TextSource::None
    } else {
        outcome.text_source
    };

    let row = sqlx::query(
        "INSERT INTO otdel.material_pages \
             (bureau_id, partner_id, material_id, page_number, status, text_source, \
              text_content, char_count, word_count, image_count, width_pt, height_pt, \
              rotation, parser_name, parser_version, ocr_engine, ocr_version, ocr_language, \
              duration_ms, attempts, diagnostic, extracted_at, \
              extraction_revision, drawing_count, diagram_interpretation) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, \
                 $17, $18, $19, 1, $20, now(), gen_random_uuid(), $21, $22) \
         ON CONFLICT (material_id, page_number) DO UPDATE \
            SET status = EXCLUDED.status, \
                text_source = EXCLUDED.text_source, \
                text_content = EXCLUDED.text_content, \
                char_count = EXCLUDED.char_count, \
                word_count = EXCLUDED.word_count, \
                image_count = EXCLUDED.image_count, \
                width_pt = EXCLUDED.width_pt, \
                height_pt = EXCLUDED.height_pt, \
                rotation = EXCLUDED.rotation, \
                parser_name = EXCLUDED.parser_name, \
                parser_version = EXCLUDED.parser_version, \
                ocr_engine = EXCLUDED.ocr_engine, \
                ocr_version = EXCLUDED.ocr_version, \
                ocr_language = EXCLUDED.ocr_language, \
                duration_ms = EXCLUDED.duration_ms, \
                diagnostic = EXCLUDED.diagnostic, \
                extracted_at = now(), \
                drawing_count = EXCLUDED.drawing_count, \
                diagram_interpretation = EXCLUDED.diagram_interpretation, \
                -- A re-read is a new reading. Minting a fresh identifier here is what
                -- makes a stored piece of evidence able to notice that the coordinates it
                -- was checked against have been replaced.
                extraction_revision = gen_random_uuid(), \
                attempts = otdel.material_pages.attempts + 1, \
                updated_at = now() \
         RETURNING id",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .bind(material_id)
    .bind(outcome.page_number)
    .bind(outcome.status.as_str())
    .bind(text_source.as_str())
    .bind(text.as_deref())
    .bind(outcome.char_count)
    .bind(outcome.word_count)
    .bind(outcome.image_count)
    .bind(outcome.width_pt)
    .bind(outcome.height_pt)
    .bind(outcome.rotation)
    .bind(outcome.parser_name.as_deref())
    .bind(outcome.parser_version.as_deref())
    .bind(outcome.ocr_engine.as_deref())
    .bind(outcome.ocr_version.as_deref())
    .bind(outcome.ocr_language.as_deref())
    .bind(outcome.duration_ms)
    .bind(storable_opt(outcome.diagnostic.as_deref()))
    .bind(outcome.drawing_count)
    .bind(outcome.diagram_interpretation.as_str())
    .fetch_one(tx.conn())
    .await?;

    let page_id: Uuid = row.try_get("id")?;
    replace_regions(tx, material_id, page_id, outcome.page_number, regions).await?;
    Ok(page_id)
}

/// Delete this page's regions (cells cascade) and write the new ones.
async fn replace_regions(
    tx: &mut ScopedTx,
    material_id: Uuid,
    page_id: Uuid,
    page_number: i32,
    regions: &[NewRegion],
) -> DbResult<()> {
    let bureau_id = tx.bureau_id();
    sqlx::query("DELETE FROM otdel.page_regions WHERE bureau_id = $1 AND page_id = $2")
        .bind(bureau_id)
        .bind(page_id)
        .execute(tx.conn())
        .await?;

    for (ordinal, region) in regions.iter().enumerate() {
        let ordinal = i32::try_from(ordinal)
            .map_err(|_| DbError::Decode("too many regions on one page".to_owned()))?;
        let bbox = region.bbox.filter(BoundingBox::is_usable);
        let row = sqlx::query(
            "INSERT INTO otdel.page_regions \
                 (bureau_id, material_id, page_id, page_number, ordinal, kind, text_content, \
                  source, x0, y0, x1, y1, row_count, column_count) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) \
             RETURNING id",
        )
        .bind(bureau_id)
        .bind(material_id)
        .bind(page_id)
        .bind(page_number)
        .bind(ordinal)
        .bind(region.kind.as_str())
        .bind(storable(&region.text))
        .bind(region.source.as_str())
        .bind(bbox.map(|b| b.x0))
        .bind(bbox.map(|b| b.y0))
        .bind(bbox.map(|b| b.x1))
        .bind(bbox.map(|b| b.y1))
        .bind(region.row_count)
        .bind(region.column_count)
        .fetch_one(tx.conn())
        .await?;

        let region_id: Uuid = row.try_get("id")?;
        for cell in &region.cells {
            let bbox = cell.bbox.filter(BoundingBox::is_usable);
            sqlx::query(
                "INSERT INTO otdel.table_cells \
                     (bureau_id, region_id, row_index, column_index, is_header, raw_text, \
                      value_kind, unit, column_header, x0, y0, x1, y1, \
                      role, usability, ambiguity_reasons, column_header_path, \
                      row_header_path, subject, property, unit_ref, conditions) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, \
                         $14, $15, $16, $17, $18, $19, $20, $21, $22)",
            )
            .bind(bureau_id)
            .bind(region_id)
            .bind(cell.row_index)
            .bind(cell.column_index)
            .bind(cell.is_header)
            .bind(storable(&cell.raw_text))
            .bind(cell.value_kind.as_str())
            .bind(storable_opt(cell.unit.as_deref()))
            .bind(storable_opt(cell.column_header.as_deref()))
            .bind(bbox.map(|b| b.x0))
            .bind(bbox.map(|b| b.y0))
            .bind(bbox.map(|b| b.x1))
            .bind(bbox.map(|b| b.y1))
            .bind(cell.role.as_str())
            .bind(cell.verdict.usability.as_str())
            .bind(
                cell.verdict
                    .reasons
                    .iter()
                    .map(|reason| reason.as_str())
                    .collect::<Vec<_>>(),
            )
            .bind(json(&cell.structural_context.column_header_path)?)
            .bind(json(&cell.structural_context.row_header_path)?)
            .bind(json_opt(cell.structural_context.subject.as_ref())?)
            .bind(json_opt(cell.structural_context.property.as_ref())?)
            .bind(json_opt(cell.structural_context.unit.as_ref())?)
            .bind(json(&cell.structural_context.conditions)?)
            .execute(tx.conn())
            .await?;
        }
    }
    Ok(())
}

/// Pages of a material, in page order.
pub async fn list_for_material(
    tx: &mut ScopedTx,
    material_id: Uuid,
) -> DbResult<Vec<MaterialPage>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(&format!(
        "SELECT {PAGE_COLUMNS}, {PAGE_COUNTS} \
           FROM otdel.material_pages p {PAGE_COUNT_JOIN} \
          WHERE p.bureau_id = $1 AND p.material_id = $2 \
          ORDER BY p.page_number"
    ))
    .bind(bureau_id)
    .bind(material_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter().map(page_from_row).collect()
}

/// Pages of a material together with their stored text, in page order.
///
/// Phase 1C builds its prompt from this: only pages that really carry text are
/// returned, so a page awaiting recognition can never become a source to quote from.
/// The filter lives in SQL rather than in the caller because "readable" is a property
/// of the stored row, and every caller must agree on it.
pub async fn readable_with_text(
    tx: &mut ScopedTx,
    material_id: Uuid,
) -> DbResult<Vec<(MaterialPage, String)>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(&format!(
        "SELECT {PAGE_COLUMNS}, {PAGE_COUNTS}, p.text_content \
           FROM otdel.material_pages p {PAGE_COUNT_JOIN} \
          WHERE p.bureau_id = $1 AND p.material_id = $2 \
            AND p.status IN ('extracted', 'partial') \
            AND btrim(coalesce(p.text_content, '')) <> '' \
          ORDER BY p.page_number"
    ))
    .bind(bureau_id)
    .bind(material_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            let page = page_from_row(row)?;
            let text: String = row.try_get("text_content")?;
            Ok((page, text))
        })
        .collect()
}

/// One page with its text and evidence regions.
pub async fn page_detail(
    tx: &mut ScopedTx,
    material_id: Uuid,
    page_number: i32,
) -> DbResult<Option<PageDetail>> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(&format!(
        "SELECT {PAGE_COLUMNS}, {PAGE_COUNTS}, p.text_content \
           FROM otdel.material_pages p {PAGE_COUNT_JOIN} \
          WHERE p.bureau_id = $1 AND p.material_id = $2 AND p.page_number = $3"
    ))
    .bind(bureau_id)
    .bind(material_id)
    .bind(page_number)
    .fetch_optional(tx.conn())
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };
    let page = page_from_row(&row)?;
    let text: Option<String> = row.try_get("text_content")?;
    let regions = regions_for_page(tx, page.id).await?;

    Ok(Some(PageDetail {
        page,
        text,
        regions,
    }))
}

/// Regions of a page, each with its cells when it is a table.
pub async fn regions_for_page(tx: &mut ScopedTx, page_id: Uuid) -> DbResult<Vec<RegionDetail>> {
    let bureau_id = tx.bureau_id();
    let region_rows = sqlx::query(
        "SELECT id, page_id, page_number, ordinal, kind, text_content, source, \
                x0, y0, x1, y1, row_count, column_count \
           FROM otdel.page_regions \
          WHERE bureau_id = $1 AND page_id = $2 \
          ORDER BY ordinal",
    )
    .bind(bureau_id)
    .bind(page_id)
    .fetch_all(tx.conn())
    .await?;

    let mut details = Vec::with_capacity(region_rows.len());
    for row in &region_rows {
        let region = region_from_row(row)?;
        let cells = if region.kind == RegionKind::Table {
            cells_for_region(tx, region.id, region.page_number, region.source).await?
        } else {
            Vec::new()
        };
        details.push(RegionDetail { region, cells });
    }
    Ok(details)
}

async fn cells_for_region(
    tx: &mut ScopedTx,
    region_id: Uuid,
    page_number: i32,
    source: TextSource,
) -> DbResult<Vec<TableCell>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT id, region_id, row_index, column_index, is_header, raw_text, value_kind, \
                unit, column_header, x0, y0, x1, y1, \
                role, usability, ambiguity_reasons, column_header_path, \
                row_header_path, subject, property, unit_ref, conditions \
           FROM otdel.table_cells \
          WHERE bureau_id = $1 AND region_id = $2 \
          ORDER BY row_index, column_index",
    )
    .bind(bureau_id)
    .bind(region_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| cell_from_row(row, page_number, source))
        .collect()
}

/// Why a stored thing has no rectangle. An engine that returns text without word boxes
/// says so; it never receives a rectangle covering the page, which would look like
/// evidence and point at nothing.
fn no_geometry_reason(source: TextSource, blank: bool) -> &'static str {
    match (source, blank) {
        (_, true) => "в источнике ячейка пуста — выделять нечего",
        (TextSource::Ocr, false) => "движок распознавания вернул текст без координат",
        _ => "разборщик не определил область на странице",
    }
}

/// Put a single page back into `pending` so the worker reads it again.
///
/// Returns `None` when the page does not exist for this material.
pub async fn reset_page(
    tx: &mut ScopedTx,
    material_id: Uuid,
    page_number: i32,
) -> DbResult<Option<MaterialPage>> {
    let bureau_id = tx.bureau_id();
    sqlx::query(
        "UPDATE otdel.material_pages \
            SET status = 'pending', \
                text_source = 'none', \
                text_content = NULL, \
                char_count = 0, \
                word_count = 0, \
                ocr_engine = NULL, \
                ocr_version = NULL, \
                ocr_language = NULL, \
                diagnostic = NULL, \
                updated_at = now() \
          WHERE bureau_id = $1 AND material_id = $2 AND page_number = $3",
    )
    .bind(bureau_id)
    .bind(material_id)
    .bind(page_number)
    .execute(tx.conn())
    .await?;

    find_page(tx, material_id, page_number).await
}

/// One page row without its text or regions.
pub async fn find_page(
    tx: &mut ScopedTx,
    material_id: Uuid,
    page_number: i32,
) -> DbResult<Option<MaterialPage>> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(&format!(
        "SELECT {PAGE_COLUMNS}, {PAGE_COUNTS} \
           FROM otdel.material_pages p {PAGE_COUNT_JOIN} \
          WHERE p.bureau_id = $1 AND p.material_id = $2 AND p.page_number = $3"
    ))
    .bind(bureau_id)
    .bind(material_id)
    .bind(page_number)
    .fetch_optional(tx.conn())
    .await?;

    row.as_ref().map(page_from_row).transpose()
}

/// Text that PostgreSQL can actually store.
///
/// A `text` column rejects `U+0000` outright, and one such character — which a PDF font
/// with glyph names the parser does not recognise really does produce — would otherwise
/// fail the whole document's extraction with a database error instead of costing the one
/// character it affects. The adapter already drops these before they get here; this is
/// the boundary that makes it impossible to get it wrong from any caller, including the
/// OCR path whose output comes from an external program.
fn storable(text: &str) -> String {
    if text
        .chars()
        .all(|ch| !ch.is_control() || ch == '\n' || ch == '\t')
    {
        return text.to_owned();
    }
    text.chars()
        .filter(|ch| !ch.is_control() || *ch == '\n' || *ch == '\t')
        .collect()
}

fn storable_opt(text: Option<&str>) -> Option<String> {
    text.map(storable)
}

// --- row decoding -------------------------------------------------------------------

fn page_from_row(row: &PgRow) -> DbResult<MaterialPage> {
    let status: String = row.try_get("status")?;
    let status = PageStatus::parse(&status)
        .ok_or_else(|| DbError::Decode(format!("unknown page status `{status}`")))?;
    let text_source: String = row.try_get("text_source")?;
    let text_source = TextSource::parse(&text_source)
        .ok_or_else(|| DbError::Decode(format!("unknown text source `{text_source}`")))?;
    let diagram_interpretation: String = row.try_get("diagram_interpretation")?;
    let diagram_interpretation =
        DiagramInterpretation::parse(&diagram_interpretation).ok_or_else(|| {
            DbError::Decode(format!(
                "unknown diagram interpretation `{diagram_interpretation}`"
            ))
        })?;

    Ok(MaterialPage {
        id: row.try_get("id")?,
        material_id: row.try_get("material_id")?,
        page_number: row.try_get("page_number")?,
        status,
        text_source,
        char_count: row.try_get("char_count")?,
        word_count: row.try_get("word_count")?,
        image_count: row.try_get("image_count")?,
        width_pt: row.try_get("width_pt")?,
        height_pt: row.try_get("height_pt")?,
        rotation: row.try_get("rotation")?,
        parser_name: row.try_get("parser_name")?,
        parser_version: row.try_get("parser_version")?,
        ocr_engine: row.try_get("ocr_engine")?,
        ocr_version: row.try_get("ocr_version")?,
        ocr_language: row.try_get("ocr_language")?,
        duration_ms: row.try_get("duration_ms")?,
        attempts: row.try_get("attempts")?,
        diagnostic: row.try_get("diagnostic")?,
        extracted_at: row.try_get::<Option<DateTime<Utc>>, _>("extracted_at")?,
        region_count: i32::try_from(row.try_get::<i64, _>("region_count")?).unwrap_or(i32::MAX),
        table_count: i32::try_from(row.try_get::<i64, _>("table_count")?).unwrap_or(i32::MAX),
        extraction_revision: row.try_get("extraction_revision")?,
        drawing_count: row.try_get("drawing_count")?,
        diagram_interpretation,
    })
}

fn region_from_row(row: &PgRow) -> DbResult<PageRegion> {
    let kind: String = row.try_get("kind")?;
    let kind = RegionKind::parse(&kind)
        .ok_or_else(|| DbError::Decode(format!("unknown region kind `{kind}`")))?;
    let source: String = row.try_get("source")?;
    let source = TextSource::parse(&source)
        .ok_or_else(|| DbError::Decode(format!("unknown region source `{source}`")))?;

    let page_number: i32 = row.try_get("page_number")?;
    let bbox = bbox_from_row(row)?;

    Ok(PageRegion {
        id: row.try_get("id")?,
        page_id: row.try_get("page_id")?,
        page_number,
        ordinal: row.try_get("ordinal")?,
        kind,
        text: row.try_get("text_content")?,
        source,
        bbox,
        row_count: row.try_get("row_count")?,
        column_count: row.try_get("column_count")?,
        span: SourceSpan::from_bbox(page_number, bbox, no_geometry_reason(source, false)),
    })
}

fn cell_from_row(row: &PgRow, page_number: i32, source: TextSource) -> DbResult<TableCell> {
    let value_kind: String = row.try_get("value_kind")?;
    let value_kind = CellValueKind::parse(&value_kind)
        .ok_or_else(|| DbError::Decode(format!("unknown cell value kind `{value_kind}`")))?;

    let role: String = row.try_get("role")?;
    let role = CellRole::parse(&role)
        .ok_or_else(|| DbError::Decode(format!("unknown cell role `{role}`")))?;

    let usability: String = row.try_get("usability")?;
    let usability = CellUsability::parse(&usability)
        .ok_or_else(|| DbError::Decode(format!("unknown cell usability `{usability}`")))?;
    let stored_reasons: Vec<String> = row.try_get("ambiguity_reasons")?;
    let mut reasons = Vec::with_capacity(stored_reasons.len());
    for reason in &stored_reasons {
        reasons.push(
            AmbiguityReason::parse(reason)
                .ok_or_else(|| DbError::Decode(format!("unknown ambiguity reason `{reason}`")))?,
        );
    }
    // Rebuilt from the reasons rather than trusted from the row, so a hand-edited
    // `usability` can never quietly promote a cell the reasons still disqualify.
    let verdict = CellVerdict::from_reasons(reasons);
    if verdict.usability != usability {
        return Err(DbError::Decode(format!(
            "stored cell verdict `{}` disagrees with its reasons",
            usability.as_str()
        )));
    }

    let bbox = bbox_from_row(row)?;
    let raw_text: String = row.try_get("raw_text")?;
    let blank = raw_text.trim().is_empty();

    Ok(TableCell {
        id: row.try_get("id")?,
        region_id: row.try_get("region_id")?,
        row_index: row.try_get("row_index")?,
        column_index: row.try_get("column_index")?,
        is_header: row.try_get("is_header")?,
        raw_text,
        value_kind,
        unit: row.try_get("unit")?,
        column_header: row.try_get("column_header")?,
        bbox,
        role,
        verdict,
        structural_context: StructuralContext {
            column_header_path: from_json(row, "column_header_path")?,
            row_header_path: from_json(row, "row_header_path")?,
            subject: from_json_opt(row, "subject")?,
            property: from_json_opt(row, "property")?,
            unit: from_json_opt(row, "unit_ref")?,
            conditions: from_json(row, "conditions")?,
        },
        span: SourceSpan::from_bbox(page_number, bbox, no_geometry_reason(source, blank)),
    })
}

/// Serialise a piece of structural context for storage.
fn json<T: Serialize>(value: &T) -> DbResult<serde_json::Value> {
    serde_json::to_value(value).map_err(|error| DbError::Decode(error.to_string()))
}

fn json_opt<T: Serialize>(value: Option<&T>) -> DbResult<Option<serde_json::Value>> {
    value.map(json).transpose()
}

fn from_json<T: DeserializeOwned>(row: &PgRow, column: &str) -> DbResult<T> {
    let value: serde_json::Value = row.try_get(column)?;
    serde_json::from_value(value)
        .map_err(|error| DbError::Decode(format!("column `{column}`: {error}")))
}

fn from_json_opt<T: DeserializeOwned>(row: &PgRow, column: &str) -> DbResult<Option<T>> {
    let value: Option<serde_json::Value> = row.try_get(column)?;
    value
        .map(|value| {
            serde_json::from_value(value)
                .map_err(|error| DbError::Decode(format!("column `{column}`: {error}")))
        })
        .transpose()
}

/// The database stores all four coordinates or none; this mirrors that.
fn bbox_from_row(row: &PgRow) -> DbResult<Option<BoundingBox>> {
    let x0: Option<f64> = row.try_get("x0")?;
    let y0: Option<f64> = row.try_get("y0")?;
    let x1: Option<f64> = row.try_get("x1")?;
    let y1: Option<f64> = row.try_get("y1")?;
    Ok(match (x0, y0, x1, y1) {
        (Some(x0), Some(y0), Some(x1), Some(y1)) => Some(BoundingBox { x0, y0, x1, y1 }),
        _ => None,
    })
}
