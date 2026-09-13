//! Translating what the reader found into what the database stores.
//!
//! Deliberately dull and side-effect free, so the interesting properties are easy to
//! assert: a failed page keeps its geometry and loses its text, recognised text is
//! attributed to the engine that produced it, and a page with no trustworthy text is
//! stored with `text_source = none` rather than with an empty string.

use otdel_core::extraction::{PageStatus, TextSource};
use otdel_db::pages::{NewCell, NewPage, NewRegion, PageOutcomeRow};
use otdel_extract::{DocumentInventory, ExtractedRegion, PageInventory, PageOutcome};

/// Page rows for the inventory pass.
pub fn inventory_rows(inventory: &DocumentInventory) -> Vec<NewPage> {
    inventory.pages.iter().map(inventory_row).collect()
}

pub fn inventory_row(page: &PageInventory) -> NewPage {
    NewPage {
        page_number: page_number(page),
        width_pt: finite(page.width_pt),
        height_pt: finite(page.height_pt),
        rotation: page.rotation,
        image_count: i32::try_from(page.image_count).unwrap_or(i32::MAX),
    }
}

/// The single "page" of a material that is itself an image.
///
/// The dimensions are unknown at this level — the file has not been decoded, only
/// hashed and stored — so they are recorded as unknown instead of guessed.
pub fn image_inventory() -> PageInventory {
    PageInventory {
        page_number: 1,
        width_pt: f64::NAN,
        height_pt: f64::NAN,
        rotation: 0,
        image_count: 1,
    }
}

/// Row and regions for a page that was read.
pub fn outcome_row(
    inventory: &PageInventory,
    outcome: &PageOutcome,
) -> (PageOutcomeRow, Vec<NewRegion>) {
    let source = outcome.decision.text_source;
    let regions = if source == TextSource::None {
        Vec::new()
    } else {
        outcome
            .regions
            .iter()
            .map(|region| new_region(region, source))
            .collect()
    };

    let row = PageOutcomeRow {
        page_number: page_number(inventory),
        status: outcome.decision.status,
        text_source: source,
        text: outcome.text.clone(),
        char_count: i32::try_from(outcome.char_count).unwrap_or(i32::MAX),
        word_count: i32::try_from(outcome.word_count).unwrap_or(i32::MAX),
        image_count: i32::try_from(inventory.image_count).unwrap_or(i32::MAX),
        width_pt: finite(inventory.width_pt),
        height_pt: finite(inventory.height_pt),
        rotation: inventory.rotation,
        parser_name: Some(outcome.parser_name.to_owned()),
        parser_version: Some(outcome.parser_version.to_owned()),
        // Only a page whose text really came from an engine names one.
        ocr_engine: outcome.ocr.as_ref().map(|stamp| stamp.engine.clone()),
        ocr_version: outcome.ocr.as_ref().map(|stamp| stamp.version.clone()),
        ocr_language: outcome.ocr.as_ref().map(|stamp| stamp.language.clone()),
        duration_ms: i32::try_from(outcome.duration_ms).ok(),
        diagnostic: outcome.decision.diagnostic.clone(),
    };

    (row, regions)
}

/// Row for a page that could not be read at all.
///
/// The page still exists, still knows its size, and states the reason — it does not
/// disappear and it is not silently empty.
pub fn failed_row(inventory: &PageInventory, reason: &str) -> (PageOutcomeRow, Vec<NewRegion>) {
    (
        PageOutcomeRow {
            page_number: page_number(inventory),
            status: PageStatus::Failed,
            text_source: TextSource::None,
            text: None,
            char_count: 0,
            word_count: 0,
            image_count: i32::try_from(inventory.image_count).unwrap_or(i32::MAX),
            width_pt: finite(inventory.width_pt),
            height_pt: finite(inventory.height_pt),
            rotation: inventory.rotation,
            parser_name: Some(otdel_extract::PARSER_NAME.to_owned()),
            parser_version: Some(otdel_extract::PARSER_VERSION.to_owned()),
            ocr_engine: None,
            ocr_version: None,
            ocr_language: None,
            duration_ms: None,
            diagnostic: Some(reason.chars().take(2000).collect()),
        },
        Vec::new(),
    )
}

fn new_region(region: &ExtractedRegion, source: TextSource) -> NewRegion {
    let table = region.table.as_ref();
    NewRegion {
        kind: region.kind,
        text: region.text.clone(),
        source,
        bbox: region.bbox,
        row_count: table.map(|table| i32::try_from(table.row_count).unwrap_or(i32::MAX)),
        column_count: table.map(|table| i32::try_from(table.column_count).unwrap_or(i32::MAX)),
        cells: table
            .map(|table| {
                table
                    .cells
                    .iter()
                    .map(|cell| NewCell {
                        row_index: i32::try_from(cell.row_index).unwrap_or(i32::MAX),
                        column_index: i32::try_from(cell.column_index).unwrap_or(i32::MAX),
                        is_header: cell.is_header,
                        raw_text: cell.raw_text.clone(),
                        value_kind: cell.value_kind,
                        unit: cell.unit.clone(),
                        column_header: cell.column_header.clone(),
                        bbox: cell.bbox,
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn page_number(page: &PageInventory) -> i32 {
    i32::try_from(page.page_number).unwrap_or(i32::MAX).max(1)
}

/// Unknown geometry stays unknown: `NaN`/infinity would violate the column checks and,
/// worse, would read as a measurement.
fn finite(value: f64) -> Option<f64> {
    (value.is_finite() && value > 0.0).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use otdel_core::extraction::{CellValueKind, RegionKind};
    use otdel_extract::{ExtractedCell, ExtractedTable, OcrStamp, PageDecision};

    fn inventory() -> PageInventory {
        PageInventory {
            page_number: 7,
            width_pt: 595.0,
            height_pt: 842.0,
            rotation: 90,
            image_count: 2,
        }
    }

    fn outcome(decision: PageDecision) -> PageOutcome {
        PageOutcome {
            decision,
            text: Some("текст страницы".to_owned()),
            regions: Vec::new(),
            char_count: 13,
            word_count: 2,
            parser_name: "pdf-extract",
            parser_version: "0.12",
            ocr: None,
            duration_ms: 12,
        }
    }

    #[test]
    fn a_failed_page_keeps_its_geometry_and_states_the_reason() {
        let (row, regions) = failed_row(&inventory(), "текстовый слой повреждён");
        assert_eq!(row.page_number, 7);
        assert_eq!(row.status, PageStatus::Failed);
        assert_eq!(row.text, None);
        assert_eq!(row.text_source, TextSource::None);
        assert_eq!(row.width_pt, Some(595.0));
        assert_eq!(row.rotation, 90);
        assert_eq!(row.image_count, 2);
        assert!(row.diagnostic.unwrap().contains("повреждён"));
        assert!(regions.is_empty());
    }

    #[test]
    fn a_page_without_text_stores_no_regions_and_no_engine() {
        let (row, regions) = outcome_row(
            &inventory(),
            &PageOutcome {
                text: None,
                ..outcome(PageDecision {
                    status: PageStatus::NeedsOcr,
                    text_source: TextSource::None,
                    diagnostic: Some("распознавание недоступно".to_owned()),
                })
            },
        );
        assert_eq!(row.text, None);
        assert_eq!(row.text_source, TextSource::None);
        assert_eq!(row.ocr_engine, None);
        assert!(regions.is_empty());
    }

    #[test]
    fn recognised_text_is_attributed_to_the_engine_that_produced_it() {
        let mut source = outcome(PageDecision {
            status: PageStatus::Extracted,
            text_source: TextSource::Ocr,
            diagnostic: None,
        });
        source.ocr = Some(OcrStamp {
            engine: "tesseract".to_owned(),
            version: "tesseract 5.5.0".to_owned(),
            language: "rus+eng".to_owned(),
        });
        source.regions = vec![ExtractedRegion {
            kind: RegionKind::Paragraph,
            text: "распознанный абзац".to_owned(),
            bbox: None,
            table: None,
        }];

        let (row, regions) = outcome_row(&inventory(), &source);
        assert_eq!(row.ocr_engine.as_deref(), Some("tesseract"));
        assert_eq!(row.ocr_language.as_deref(), Some("rus+eng"));
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].source, TextSource::Ocr);
        assert_eq!(regions[0].bbox, None);
    }

    #[test]
    fn table_cells_survive_the_translation_unchanged() {
        let mut source = outcome(PageDecision {
            status: PageStatus::Extracted,
            text_source: TextSource::TextLayer,
            diagnostic: None,
        });
        source.regions = vec![ExtractedRegion {
            kind: RegionKind::Table,
            text: "таблица".to_owned(),
            bbox: None,
            table: Some(ExtractedTable {
                row_count: 2,
                column_count: 2,
                cells: vec![
                    ExtractedCell {
                        row_index: 1,
                        column_index: 1,
                        is_header: false,
                        raw_text: "1200".to_owned(),
                        value_kind: CellValueKind::Number,
                        unit: Some("мм".to_owned()),
                        column_header: Some("Длина, мм".to_owned()),
                        bbox: None,
                    },
                    ExtractedCell {
                        row_index: 1,
                        column_index: 0,
                        is_header: false,
                        raw_text: String::new(),
                        value_kind: CellValueKind::Empty,
                        unit: None,
                        column_header: None,
                        bbox: None,
                    },
                ],
            }),
        }];

        let (_, regions) = outcome_row(&inventory(), &source);
        assert_eq!(regions[0].row_count, Some(2));
        assert_eq!(regions[0].column_count, Some(2));
        assert_eq!(regions[0].cells.len(), 2);
        assert_eq!(regions[0].cells[0].raw_text, "1200");
        assert_eq!(regions[0].cells[0].unit.as_deref(), Some("мм"));
        assert_eq!(
            regions[0].cells[0].column_header.as_deref(),
            Some("Длина, мм")
        );
        // The blank cell is carried through as blank, not dropped and not zeroed.
        assert_eq!(regions[0].cells[1].raw_text, "");
        assert_eq!(regions[0].cells[1].value_kind, CellValueKind::Empty);
    }

    #[test]
    fn unknown_page_geometry_is_stored_as_unknown() {
        let row = inventory_row(&image_inventory());
        assert_eq!(row.width_pt, None);
        assert_eq!(row.height_pt, None);
        assert_eq!(row.page_number, 1);
        assert_eq!(row.image_count, 1);
    }
}
