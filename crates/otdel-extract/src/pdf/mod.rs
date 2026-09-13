//! Reading a PDF: inventory first, then one page at a time.
//!
//! The parser is a third-party, pure-Rust one. That choice is deliberate — the
//! text-layer half of phase 1B must work, and must be testable, on a machine with no
//! external binary installed. Recognition is the opposite case and lives in
//! [`crate::ocr`], behind adapters that report their own absence.
//!
//! **Panics are handled, not ignored.** The parser is defensive enough to be useful but
//! not defensive enough to promise it never aborts on a malformed file, so each call is
//! made inside a catch and a panic becomes [`ExtractError::ParserCrashed`] — a page that
//! failed for a stated reason, not a worker that died mid-document.

pub(crate) mod collect;
pub(crate) mod inventory;
pub(crate) mod layout;
pub(crate) mod tables;

use std::panic::{catch_unwind, AssertUnwindSafe};

use otdel_core::extraction::RegionKind;
use pdf_extract::Document;

use crate::error::{ExtractError, ExtractResult};
use crate::model::{DocumentInventory, ExtractedRegion, PageInventory, PageText};

use collect::{is_garbled, GlyphCollector};
use layout::{build_blocks, build_lines, plain_text, Line};

/// Name recorded on every page this module produces.
pub const PARSER_NAME: &str = "pdf-extract";
/// Version recorded alongside it. Bumping the dependency must bump this string: a page
/// read by a different parser version has to be distinguishable from the old result
/// (`docs/block-01-spec.md` §6.1).
pub const PARSER_VERSION: &str = "0.12";

/// A loaded PDF. Cheap to keep around; every page read borrows it.
pub struct PdfDocument {
    doc: Document,
}

/// Deliberately terse: the object graph of a real catalogue is megabytes of tenant data
/// and must never end up in a log line or a test failure message.
impl std::fmt::Debug for PdfDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdfDocument")
            .field("pages", &self.doc.get_pages().len())
            .finish_non_exhaustive()
    }
}

impl PdfDocument {
    /// Parse the bytes of an original.
    pub fn load(bytes: &[u8]) -> ExtractResult<Self> {
        let loaded =
            catch_unwind(AssertUnwindSafe(|| Document::load_mem(bytes))).map_err(|_| {
                ExtractError::ParserCrashed {
                    context: "открытие документа".to_owned(),
                }
            })?;

        let doc = loaded.map_err(|error| ExtractError::NotAPdf(sanitise(&error.to_string())))?;
        if doc.is_encrypted() {
            return Err(ExtractError::Encrypted);
        }
        Ok(Self { doc })
    }

    /// Enumerate the pages without reading any of them.
    pub fn inventory(&self, max_pages: u32) -> ExtractResult<DocumentInventory> {
        catch_unwind(AssertUnwindSafe(|| {
            inventory::document_inventory(&self.doc, max_pages)
        }))
        .map_err(|_| ExtractError::ParserCrashed {
            context: "перечисление страниц".to_owned(),
        })?
    }

    /// Facts about one page, without reading its content stream.
    pub fn page_inventory(&self, page_number: u32) -> ExtractResult<PageInventory> {
        let pages = self.doc.get_pages();
        let page_id = *pages
            .get(&page_number)
            .ok_or(ExtractError::PageMissing { page: page_number })?;
        catch_unwind(AssertUnwindSafe(|| {
            inventory::page_inventory(&self.doc, page_number, page_id)
        }))
        .map_err(|_| ExtractError::ParserCrashed {
            context: format!("разбор страницы {page_number}"),
        })
    }

    /// Read the text layer of one page and recover its structure.
    pub fn read_page(&self, page_number: u32) -> ExtractResult<PageText> {
        if !self.doc.get_pages().contains_key(&page_number) {
            return Err(ExtractError::PageMissing { page: page_number });
        }

        let mut collector = GlyphCollector::new();
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            pdf_extract::output_doc_page(&self.doc, &mut collector, page_number)
        }))
        .map_err(|_| ExtractError::ParserCrashed {
            context: format!("чтение текстового слоя страницы {page_number}"),
        })?;

        outcome.map_err(|error| ExtractError::Page {
            page: page_number,
            reason: sanitise(&error.to_string()),
        })?;

        Ok(assemble(collector))
    }
}

/// Build the page result from the collected glyphs.
fn assemble(collector: GlyphCollector) -> PageText {
    let mut char_count = 0u32;
    let mut garbled = 0u32;
    for glyph in &collector.glyphs {
        for ch in glyph.text.chars() {
            if ch.is_whitespace() {
                continue;
            }
            char_count = char_count.saturating_add(1);
            if is_garbled(ch) {
                garbled = garbled.saturating_add(1);
            }
        }
    }

    let lines = build_lines(&collector.glyphs);
    let word_count = lines
        .iter()
        .map(|line| line.words.len())
        .sum::<usize>()
        .try_into()
        .unwrap_or(u32::MAX);

    let page_bottom = collector.media_box.map(|(_, lly, _, _)| lly);
    let page_height = collector.page_height();

    PageText {
        text: plain_text(&lines),
        regions: build_regions(&lines, page_bottom, page_height),
        char_count,
        word_count,
        garbled_ratio: if char_count == 0 {
            0.0
        } else {
            f64::from(garbled) / f64::from(char_count)
        },
        drawing_ops: collector.drawing_ops,
        truncated: collector.truncated,
    }
}

/// Regions in reading order: tables where a grid was proven, ordinary blocks elsewhere.
fn build_regions(
    lines: &[Line],
    page_bottom: Option<f64>,
    page_height: Option<f64>,
) -> Vec<ExtractedRegion> {
    let detected = tables::detect_tables(lines);
    let mut regions = Vec::new();
    let mut index = 0usize;
    let mut pending = detected.iter().peekable();

    while index < lines.len() {
        if pending.peek().is_some_and(|table| table.start == index) {
            let table = pending.next().expect("peeked");
            regions.push(ExtractedRegion {
                kind: RegionKind::Table,
                text: plain_text(&lines[table.start..table.end]),
                bbox: Some(table.bbox),
                table: Some(table.table.clone()),
            });
            index = table.end;
            continue;
        }

        let stop = pending.peek().map_or(lines.len(), |table| table.start);
        if stop <= index {
            // Defensive: never loop on an unexpected range.
            index += 1;
            continue;
        }
        regions.extend(
            build_blocks(&lines[index..stop], page_bottom, page_height)
                .into_iter()
                .map(|(kind, text, bbox)| ExtractedRegion {
                    kind,
                    text,
                    bbox: Some(bbox),
                    table: None,
                }),
        );
        index = stop;
    }

    regions
}

/// Keep a third-party error message fit for a user-facing diagnostic: one line, bounded,
/// and without anything that looks like a path.
fn sanitise(message: &str) -> String {
    let cleaned: String = message
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let cleaned = cleaned.trim();
    let cleaned = cleaned.split(" at /").next().unwrap_or(cleaned).trim();
    cleaned.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_that_is_not_a_pdf_is_refused_permanently() {
        let error = PdfDocument::load(b"this is a text file, not a PDF").unwrap_err();
        assert!(matches!(error, ExtractError::NotAPdf(_)));
        assert!(error.is_permanent());
    }

    #[test]
    fn truncated_pdf_bytes_do_not_crash_the_reader() {
        // A header and nothing else: the point is that this returns an error rather
        // than unwinding out of the crate.
        let error = PdfDocument::load(b"%PDF-1.7\n").unwrap_err();
        assert!(error.is_permanent(), "{error}");
    }

    #[test]
    fn diagnostics_are_one_bounded_line_without_paths() {
        let message = sanitise("broken\n\tobject at /Users/someone/secret/file.pdf");
        assert!(!message.contains('\n'));
        assert!(!message.contains("/Users"));
        assert!(message.len() <= 200);
    }
}
