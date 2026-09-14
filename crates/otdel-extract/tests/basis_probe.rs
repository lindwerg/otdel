//! Opt-in structural probe against a real catalogue.
//!
//! Real partner originals are tenant data and are never committed. This test therefore
//! reads nothing by default: it runs only when `OTDEL_PROBE_PDF` names a file that is
//! already on the machine, and it prints a structural census rather than the document's
//! contents. It exists so a claim like "the table header is reachable as a value" can be
//! checked against the real source instead of against a fixture that was written to
//! agree with the code.
//!
//! ```text
//! OTDEL_PROBE_PDF=/path/to/original cargo test -p otdel-extract --test basis_probe -- --nocapture --ignored
//! ```

use std::collections::BTreeMap;

use otdel_core::extraction::{CellValueKind, RegionKind};
use otdel_core::extraction_context::CellUsability;
use otdel_extract::pdf::PdfDocument;
use otdel_extract::units;

const MAX_PAGES: u32 = 400;

#[test]
#[ignore = "needs OTDEL_PROBE_PDF pointing at a local original"]
fn structural_census_of_a_real_catalogue() {
    let Ok(path) = std::env::var("OTDEL_PROBE_PDF") else {
        eprintln!("OTDEL_PROBE_PDF is not set; nothing to probe");
        return;
    };

    let bytes = std::fs::read(&path).expect("the probe file must be readable");
    let doc = PdfDocument::load(&bytes).expect("the probe file must parse as a PDF");
    let inventory = doc.inventory(MAX_PAGES).expect("inventory");

    let mut pages_with_text = 0u32;
    let mut pages_without_text = 0u32;
    let mut region_kinds: BTreeMap<&'static str, u32> = BTreeMap::new();
    let mut tables = 0u32;
    let mut cells = 0u32;
    let mut header_cells = 0u32;
    let mut numeric_cells = 0u32;
    let mut empty_cells = 0u32;
    let mut cells_with_unit = 0u32;
    let mut cells_with_column_header = 0u32;
    let mut cells_without_bbox = 0u32;
    let mut usable = 0u32;
    let mut ambiguous = 0u32;
    let mut unusable = 0u32;
    let mut by_reason: BTreeMap<&'static str, u32> = BTreeMap::new();
    // A cell offered as a value while its text reads as a label is the defect under
    // review. This must stay empty.
    let mut labels_offered_as_values: Vec<String> = Vec::new();

    for page in &inventory.pages {
        let Ok(text) = doc.read_page(page.page_number) else {
            eprintln!("page {}: read failed", page.page_number);
            continue;
        };
        if text.char_count == 0 {
            pages_without_text += 1;
        } else {
            pages_with_text += 1;
        }

        for region in &text.regions {
            *region_kinds.entry(region.kind.as_str()).or_default() += 1;
            if region.kind != RegionKind::Table {
                continue;
            }
            let Some(table) = &region.table else { continue };
            tables += 1;
            for cell in &table.cells {
                cells += 1;
                if cell.is_header {
                    header_cells += 1;
                }
                match cell.verdict.usability {
                    CellUsability::Usable => usable += 1,
                    CellUsability::Ambiguous => ambiguous += 1,
                    CellUsability::Unusable => unusable += 1,
                }
                for reason in &cell.verdict.reasons {
                    *by_reason.entry(reason.as_str()).or_default() += 1;
                }
                if cell.verdict.is_candidate_value() && units::is_header_shaped(&cell.raw_text) {
                    labels_offered_as_values.push(cell.raw_text.clone());
                }
                match cell.value_kind {
                    CellValueKind::Number => numeric_cells += 1,
                    CellValueKind::Empty => empty_cells += 1,
                    CellValueKind::Text => {}
                }
                if cell.unit.is_some() {
                    cells_with_unit += 1;
                }
                if cell.column_header.is_some() {
                    cells_with_column_header += 1;
                }
                if cell.bbox.is_none() {
                    cells_without_bbox += 1;
                }
            }
        }
    }

    println!("--- structural census of {} ---", redact(&path));
    println!("pages declared:            {}", inventory.page_count);
    println!("pages enumerated:          {}", inventory.pages.len());
    println!("pages with text layer:     {pages_with_text}");
    println!("pages without text layer:  {pages_without_text}");
    println!("regions by kind:           {region_kinds:?}");
    println!("tables detected:           {tables}");
    println!("table cells:               {cells}");
    println!("  header cells:            {header_cells}");
    println!("  numeric cells:           {numeric_cells}");
    println!("  blank cells:             {empty_cells}");
    println!("  cells carrying a unit:   {cells_with_unit}");
    println!("  cells with col header:   {cells_with_column_header}");
    println!("  cells without a bbox:    {cells_without_bbox}");
    println!("verdicts:  usable {usable}  ambiguous {ambiguous}  unusable {unusable}");
    println!("reasons:                   {by_reason:?}");
    println!(
        "labels still offered as values (must be empty): {}",
        labels_offered_as_values.len()
    );
    for label in labels_offered_as_values.iter().take(20) {
        println!("  {label}");
    }

    // The defect under review — a column label published as a product characteristic.
    // Listing it is not enough: a regression has to fail the probe, not merely print.
    assert!(
        labels_offered_as_values.is_empty(),
        "{} label(s) are still offered as values, e.g. {:?}",
        labels_offered_as_values.len(),
        labels_offered_as_values.iter().take(5).collect::<Vec<_>>()
    );
}

/// The probe prints the file's *name* only; the directory is the operator's business.
fn redact(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or("original")
}

/// Dump the grid of every table on the page(s) whose text contains `OTDEL_PROBE_NEEDLE`.
///
/// This is the microscope for a specific reported defect: it shows which cell a phrase
/// actually landed in, what `is_header`/`value_kind`/`unit`/`column_header` the extractor
/// gave it, and therefore what a consumer would have been able to read it as.
#[test]
#[ignore = "needs OTDEL_PROBE_PDF and OTDEL_PROBE_NEEDLE"]
fn grid_around_a_phrase() {
    let (Ok(path), Ok(needle)) = (
        std::env::var("OTDEL_PROBE_PDF"),
        std::env::var("OTDEL_PROBE_NEEDLE"),
    ) else {
        eprintln!("OTDEL_PROBE_PDF / OTDEL_PROBE_NEEDLE are not set; nothing to probe");
        return;
    };
    let needle = needle.to_lowercase();

    let bytes = std::fs::read(&path).expect("the probe file must be readable");
    let doc = PdfDocument::load(&bytes).expect("the probe file must parse as a PDF");
    let inventory = doc.inventory(MAX_PAGES).expect("inventory");

    for page in &inventory.pages {
        let Ok(text) = doc.read_page(page.page_number) else {
            continue;
        };
        if !text.text.to_lowercase().contains(&needle) {
            continue;
        }
        println!("=== page {} ===", page.page_number);
        for (ordinal, region) in text.regions.iter().enumerate() {
            let hit = region.text.to_lowercase().contains(&needle);
            let Some(table) = &region.table else {
                if hit {
                    println!(
                        "  region {ordinal} [{}] (no grid): {:?}",
                        region.kind.as_str(),
                        truncate(&region.text)
                    );
                }
                continue;
            };
            if !hit {
                continue;
            }
            println!(
                "  region {ordinal} [table {}x{}] bbox={:?}",
                table.row_count,
                table.column_count,
                region.bbox.is_some()
            );
            for cell in &table.cells {
                if cell.raw_text.trim().is_empty() {
                    continue;
                }
                println!(
                    "    r{:<2} c{:<2} role={:<13} verdict={:<9} unit={:<6} subject={:<18} reasons={:<60} text={:?}",
                    cell.row_index,
                    cell.column_index,
                    cell.role.as_str(),
                    cell.verdict.usability.as_str(),
                    cell.unit.as_deref().unwrap_or("-"),
                    truncate(
                        cell.structural_context
                            .subject
                            .as_ref()
                            .map_or("-", |it| it.text.as_str())
                    ),
                    cell.verdict
                        .reasons
                        .iter()
                        .map(|r| r.as_str())
                        .collect::<Vec<_>>()
                        .join(","),
                    truncate(&cell.raw_text),
                );
            }
        }
    }
}

fn truncate(text: &str) -> String {
    let flat = text.replace('\n', " ⏎ ");
    flat.chars().take(60).collect()
}
