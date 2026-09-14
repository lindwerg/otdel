//! End-to-end reading of synthetic PDFs.
//!
//! These build a real document in memory and read it back through the same code the
//! worker uses, so they cover the whole text-layer path — parsing, glyph positions,
//! layout, table recovery — without anything installed on the machine.
//!
//! The two cases that matter for the acceptance checks are here in their honest form:
//! a PDF **with** a text layer produces page records with text and coordinates, and a
//! PDF **without** one is never reported as read or as empty.

use std::path::Path;
use std::sync::Arc;

use otdel_core::extraction::{CellValueKind, PageStatus, RegionKind, TextSource};
use otdel_extract::{
    fixtures, Disabled, OcrPermission, PageProcessor, PageSource, PdfDocument, PARSER_NAME,
};

const MAX_PAGES: u32 = 500;

fn processor_without_recognition() -> PageProcessor {
    PageProcessor::new(
        Arc::new(Disabled::new(
            "tesseract",
            "исполняемый файл `tesseract` не найден",
        )),
        Arc::new(Disabled::new("pdftoppm", "рендер страниц недоступен")),
    )
}

fn source() -> PageSource<'static> {
    PageSource::Pdf {
        path: Path::new("/tmp/otdel-extract-tests/document.pdf"),
        page: 1,
        work_dir: Path::new("/tmp/otdel-extract-tests"),
    }
}

#[test]
fn a_pdf_with_a_text_layer_yields_text_and_structure() {
    let document = PdfDocument::load(&fixtures::text_pdf()).expect("the fixture is a valid PDF");
    let inventory = document.inventory(MAX_PAGES).expect("inventory");
    assert_eq!(inventory.page_count, 1);
    assert_eq!(inventory.pages.len(), 1);
    assert_eq!(inventory.pages[0].page_number, 1);
    assert!((inventory.pages[0].width_pt - 595.0).abs() < 1.0);
    assert_eq!(inventory.pages[0].image_count, 0);

    let page = document.read_page(1).expect("page 1 is readable");
    assert!(page.char_count > 100, "chars: {}", page.char_count);
    assert_eq!(page.garbled_ratio, 0.0);
    assert!(page.text.contains("BASIS mounting systems"));
    assert!(page.text.contains("certificate of conformity"));

    // The heading is recognised as one, and the body as paragraphs.
    let kinds: Vec<RegionKind> = page.regions.iter().map(|region| region.kind).collect();
    assert!(kinds.contains(&RegionKind::Heading), "{kinds:?}");
    assert!(kinds.contains(&RegionKind::Paragraph), "{kinds:?}");

    // Every region knows where it is, in page coordinates inside the media box.
    for region in &page.regions {
        let bbox = region.bbox.expect("a text-layer region has coordinates");
        assert!(bbox.is_usable(), "{bbox:?}");
        assert!(bbox.x0 >= 0.0 && bbox.x1 <= 595.0, "{bbox:?}");
        assert!(bbox.y0 >= 0.0 && bbox.y1 <= 900.0, "{bbox:?}");
    }
}

#[tokio::test]
async fn a_readable_page_is_extracted_without_touching_recognition() {
    let document = PdfDocument::load(&fixtures::text_pdf()).unwrap();
    let inventory = document.inventory(MAX_PAGES).unwrap();
    let text = document.read_page(1).unwrap();

    let outcome = processor_without_recognition()
        .finish_page(text, &inventory.pages[0], source(), &OcrPermission::Allowed)
        .await;

    assert_eq!(outcome.decision.status, PageStatus::Extracted);
    assert_eq!(outcome.decision.text_source, TextSource::TextLayer);
    assert_eq!(outcome.parser_name, PARSER_NAME);
    assert!(outcome.ocr.is_none());
    assert!(outcome.text.unwrap().contains("BASIS"));
}

#[tokio::test]
async fn a_scanned_pdf_is_neither_empty_nor_read_when_no_engine_exists() {
    let document = PdfDocument::load(&fixtures::scanned_pdf(12)).unwrap();
    let inventory = document.inventory(MAX_PAGES).unwrap();

    // All twelve pages are accounted for before anything is read.
    assert_eq!(inventory.page_count, 12);
    assert_eq!(inventory.pages.len(), 12);
    assert!(inventory.pages.iter().all(|page| page.image_count >= 1));

    let processor = processor_without_recognition();
    for page_inventory in &inventory.pages {
        let text = document.read_page(page_inventory.page_number).unwrap();
        assert_eq!(text.char_count, 0, "a scan has no text layer");

        let outcome = processor
            .finish_page(text, page_inventory, source(), &OcrPermission::Allowed)
            .await;

        assert_eq!(
            outcome.decision.status,
            PageStatus::NeedsOcr,
            "page {} must ask for recognition",
            page_inventory.page_number
        );
        assert_ne!(outcome.decision.status, PageStatus::Empty);
        assert_ne!(outcome.decision.status, PageStatus::Extracted);
        assert_eq!(outcome.text, None);
        assert!(outcome.ocr.is_none());

        // The reason names the tool that was missing — here the page rasteriser, which
        // is reached before the engine. Either way the page says why, in words.
        let diagnostic = outcome.decision.diagnostic.as_deref().unwrap();
        assert!(
            diagnostic.contains("распознавание недоступно"),
            "{diagnostic}"
        );
        assert!(diagnostic.contains("pdftoppm"), "{diagnostic}");
        assert!(
            diagnostic.contains("изображен"),
            "the reason must mention what was found on the page: {diagnostic}"
        );
    }
}

#[test]
fn a_specification_table_keeps_its_cells_units_and_blanks() {
    let document = PdfDocument::load(&fixtures::table_pdf()).unwrap();
    let page = document.read_page(1).unwrap();

    let table_region = page
        .regions
        .iter()
        .find(|region| region.kind == RegionKind::Table)
        .expect("the grid must be recovered as a table");
    let table = table_region
        .table
        .as_ref()
        .expect("a table region has a grid");

    assert_eq!(table.column_count, 3);
    assert_eq!(table.row_count, 5, "header + four profiles");

    let cell = |row: u32, column: u32| {
        table
            .cells
            .iter()
            .find(|cell| cell.row_index == row && cell.column_index == column)
            .unwrap_or_else(|| panic!("cell {row}/{column} is missing"))
    };

    assert_eq!(cell(0, 0).raw_text, "Profile");
    assert!(cell(0, 0).is_header);

    // Values are kept verbatim, with the unit taken from the header that really says it.
    assert_eq!(cell(1, 1).raw_text, "1200");
    assert_eq!(cell(1, 1).value_kind, CellValueKind::Number);
    assert_eq!(cell(1, 1).unit.as_deref(), Some("mm"));
    assert_eq!(cell(1, 1).column_header.as_deref(), Some("Length, mm"));

    assert_eq!(cell(2, 2).raw_text, "4.2");
    assert_eq!(cell(2, 2).unit.as_deref(), Some("kN"));

    // A designation column gets no unit invented for it.
    assert_eq!(cell(1, 0).raw_text, "BP21");
    assert_eq!(cell(1, 0).unit, None);
    assert_eq!(cell(1, 0).value_kind, CellValueKind::Text);

    // The missing load stays missing: present as a blank cell, never as a zero.
    assert_eq!(cell(4, 0).raw_text, "BP40");
    assert_eq!(cell(4, 2).raw_text, "");
    assert_eq!(cell(4, 2).value_kind, CellValueKind::Empty);
    assert_ne!(cell(4, 2).value_kind, CellValueKind::Number);

    // And the table can be pointed at on the page.
    assert!(table_region.bbox.expect("a table is placed").is_usable());
}

#[test]
fn every_page_of_a_mixed_document_is_accounted_for() {
    let document = PdfDocument::load(&fixtures::mixed_pdf()).unwrap();
    let inventory = document.inventory(MAX_PAGES).unwrap();
    assert_eq!(inventory.page_count, 3);

    let readable = document.read_page(1).unwrap();
    assert!(readable.char_count > 0);

    let scanned = document.read_page(2).unwrap();
    assert_eq!(scanned.char_count, 0);
    assert_eq!(inventory.pages[1].image_count, 1);

    let tabular = document.read_page(3).unwrap();
    assert!(tabular
        .regions
        .iter()
        .any(|region| region.kind == RegionKind::Table));
}

#[test]
fn a_page_outside_the_document_is_reported_not_invented() {
    let document = PdfDocument::load(&fixtures::text_pdf()).unwrap();
    let error = document.read_page(99).unwrap_err();
    assert!(error.is_permanent(), "{error}");
    assert!(error.to_string().contains("99"));
}

#[test]
fn a_page_limit_stops_an_oversized_document_before_it_is_read() {
    let document = PdfDocument::load(&fixtures::scanned_pdf(6)).unwrap();
    let error = document.inventory(3).unwrap_err();
    assert!(error.to_string().contains('6'));
    assert!(error.is_permanent());
}

#[test]
fn a_file_that_only_claims_to_be_a_pdf_is_rejected() {
    let error = PdfDocument::load(&fixtures::not_a_pdf()).unwrap_err();
    assert!(error.is_permanent(), "{error}");
}
