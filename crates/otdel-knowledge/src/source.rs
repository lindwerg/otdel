//! The set of sources one understanding run is allowed to use.
//!
//! A run is built from pages the server selected: pages of **one material**, of **one
//! partner**, read in a bureau-scoped transaction. The model never sees an identifier
//! it could use to reach anything else — it is shown short labels (`S1`, `S2`, …) and
//! must cite one of them. The mapping from label to page exists only on the server, so
//! a cited label either resolves to a page inside this run's scope or the candidate is
//! refused. There is no third possibility, which is what makes source spoofing a
//! non-event rather than a check that has to be remembered.

use otdel_core::extraction::{MaterialPage, PageStatus, TextSource};
use uuid::Uuid;

use crate::quote::{QuoteMatch, QuoteRejection, SearchableText};

/// One page offered to the model, with the text the server has stored for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePage {
    pub page_id: Uuid,
    pub material_id: Uuid,
    pub material_filename: String,
    pub page_number: i32,
    pub status: PageStatus,
    pub text_source: TextSource,
    /// The stored page text. Never empty in a catalogue entry.
    pub text: String,
}

impl SourcePage {
    /// Build from a stored page record plus its text, or `None` when the page carries
    /// nothing usable.
    ///
    /// Pages that are `needs_ocr`, `empty`, `pending` or `failed` have no text to quote
    /// from and are therefore not offered at all: asking a model to describe a page
    /// nobody could read is how invented content gets in.
    pub fn from_page(
        page: &MaterialPage,
        material_filename: &str,
        text: Option<String>,
    ) -> Option<Self> {
        let text = text?;
        if text.trim().is_empty() || !page.status.is_readable() {
            return None;
        }
        Some(Self {
            page_id: page.id,
            material_id: page.material_id,
            material_filename: material_filename.to_owned(),
            page_number: page.page_number,
            status: page.status,
            text_source: page.text_source,
            text,
        })
    }

    /// How the page's provenance is described in the prompt and the interface.
    pub fn origin_note(&self) -> &'static str {
        match (self.text_source, self.status) {
            (TextSource::Ocr, PageStatus::Partial) => "распознано, прочитано частично",
            (TextSource::Ocr, _) => "распознано (OCR), возможны ошибки распознавания",
            (_, PageStatus::Partial) => "текстовый слой, страница прочитана частично",
            _ => "текстовый слой",
        }
    }
}

/// A page with its label and its text prepared for quote matching.
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub label: String,
    pub page: SourcePage,
    searchable: SearchableText,
}

impl CatalogEntry {
    /// Locate a model-supplied quote in this page.
    pub fn locate(&self, quote: &str) -> Result<QuoteMatch, QuoteRejection> {
        self.searchable.locate(quote)
    }
}

/// Every source a run may cite, and nothing else.
#[derive(Debug, Clone, Default)]
pub struct SourceCatalog {
    entries: Vec<CatalogEntry>,
}

impl SourceCatalog {
    /// Label the pages `S1…Sn` in page order.
    pub fn build(pages: Vec<SourcePage>) -> Self {
        let mut pages = pages;
        pages.sort_by_key(|page| (page.material_id, page.page_number));

        let entries = pages
            .into_iter()
            .enumerate()
            .map(|(index, page)| CatalogEntry {
                label: format!("S{}", index + 1),
                searchable: SearchableText::new(&page.text),
                page,
            })
            .collect();

        Self { entries }
    }

    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Resolve a label the model cited. Unknown labels — including a plausible-looking
    /// page UUID — resolve to nothing.
    pub fn resolve(&self, label: &str) -> Option<&CatalogEntry> {
        let wanted = label.trim();
        self.entries
            .iter()
            .find(|entry| entry.label.eq_ignore_ascii_case(wanted))
    }

    /// Total characters of source text in the catalogue.
    pub fn total_chars(&self) -> usize {
        self.entries
            .iter()
            .map(|entry| entry.page.text.chars().count())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use otdel_core::extraction_context::DiagramInterpretation;

    fn page(number: i32, status: PageStatus, source: TextSource) -> MaterialPage {
        MaterialPage {
            id: Uuid::from_u128(u128::try_from(number).unwrap()),
            material_id: Uuid::from_u128(1000),
            page_number: number,
            status,
            text_source: source,
            char_count: 10,
            word_count: 2,
            image_count: 0,
            width_pt: None,
            height_pt: None,
            rotation: 0,
            parser_name: None,
            parser_version: None,
            ocr_engine: None,
            ocr_version: None,
            ocr_language: None,
            duration_ms: None,
            attempts: 1,
            diagnostic: None,
            extracted_at: Some(Utc::now()),
            region_count: 0,
            table_count: 0,
            extraction_revision: None,
            drawing_count: 0,
            diagram_interpretation: DiagramInterpretation::None,
        }
    }

    #[test]
    fn only_pages_with_real_text_become_sources() {
        let extracted = page(1, PageStatus::Extracted, TextSource::TextLayer);
        assert!(SourcePage::from_page(&extracted, "c.pdf", Some("BP21 1200".to_owned())).is_some());

        // No text at all.
        assert!(SourcePage::from_page(&extracted, "c.pdf", None).is_none());
        assert!(SourcePage::from_page(&extracted, "c.pdf", Some("   ".to_owned())).is_none());

        // A page waiting for recognition has nothing to quote, whatever else is true.
        let needs_ocr = page(2, PageStatus::NeedsOcr, TextSource::None);
        assert!(SourcePage::from_page(&needs_ocr, "c.pdf", Some("text".to_owned())).is_none());
        let empty = page(3, PageStatus::Empty, TextSource::None);
        assert!(SourcePage::from_page(&empty, "c.pdf", Some("text".to_owned())).is_none());
        let failed = page(4, PageStatus::Failed, TextSource::None);
        assert!(SourcePage::from_page(&failed, "c.pdf", Some("text".to_owned())).is_none());
    }

    #[test]
    fn a_partially_read_page_is_offered_with_that_stated() {
        let partial = page(5, PageStatus::Partial, TextSource::TextLayer);
        let source =
            SourcePage::from_page(&partial, "c.pdf", Some("BP21 1200".to_owned())).unwrap();
        assert!(source.origin_note().contains("частично"));
    }

    fn source_page(number: i32, text: &str) -> SourcePage {
        SourcePage {
            page_id: Uuid::from_u128(u128::try_from(number).unwrap()),
            material_id: Uuid::from_u128(1000),
            material_filename: "catalogue.pdf".to_owned(),
            page_number: number,
            status: PageStatus::Extracted,
            text_source: TextSource::TextLayer,
            text: text.to_owned(),
        }
    }

    #[test]
    fn labels_are_assigned_in_page_order_and_resolve_back() {
        let catalog = SourceCatalog::build(vec![
            source_page(3, "третья страница про BP30"),
            source_page(1, "первая страница про BP21"),
        ]);

        assert_eq!(catalog.len(), 2);
        assert_eq!(catalog.entries()[0].label, "S1");
        assert_eq!(catalog.entries()[0].page.page_number, 1);
        assert_eq!(catalog.resolve("S2").unwrap().page.page_number, 3);
        assert_eq!(catalog.resolve("s2").unwrap().page.page_number, 3);
    }

    #[test]
    fn a_label_outside_the_run_resolves_to_nothing() {
        let catalog = SourceCatalog::build(vec![source_page(1, "первая страница про BP21")]);
        for spoofed in [
            "S2",
            "S404",
            "",
            "00000000-0000-0000-0000-000000000001",
            "другой партнёр",
        ] {
            assert!(
                catalog.resolve(spoofed).is_none(),
                "{spoofed:?} must not resolve to a source"
            );
        }
    }
}
