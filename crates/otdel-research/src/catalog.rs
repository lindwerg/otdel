//! The set of external sources one plan is allowed to cite.
//!
//! The same shape as phase 1C's [`otdel_knowledge::SourceCatalog`], and for the same
//! reason. A plan is built from pages the server downloaded itself, from hosts the owner
//! declared, inside one bureau. The model never sees a URL it could ask for and never
//! sees an identifier: it is shown short labels (`E1`, `E2`, …) and must cite one. The
//! mapping from label to source exists only on the server, so a cited label either
//! resolves to a page of *this plan* or the finding is refused. There is no third
//! possibility — which is what makes "the model invented a source" a non-event rather
//! than a check somebody has to remember.
//!
//! Only a source that was really fetched has text, and only text can be quoted. A search
//! snippet never enters this catalogue: it is the search engine's sentence about a page,
//! not the page (`docs/block-01-spec.md` §6.5, "сниппет … сам по себе не заменяет
//! документ").

use chrono::{DateTime, Utc};
use otdel_knowledge::quote::{QuoteMatch, QuoteRejection, SearchableText};
use uuid::Uuid;

/// One downloaded page, as it is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSource {
    /// The stored row this label resolves to.
    pub source_id: Uuid,
    pub url: String,
    pub host: String,
    pub title: Option<String>,
    /// When the snapshot was taken.
    pub retrieved_at: Option<DateTime<Utc>>,
    /// SHA-256 of the bytes that produced `text`.
    pub content_hash: Option<String>,
    /// Only when the page declared one.
    pub license: Option<String>,
    /// The stored snapshot. Never empty in a catalogue entry.
    pub text: String,
}

/// A source with its label and its text prepared for quote matching.
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub label: String,
    pub source: ExternalSource,
    searchable: SearchableText,
}

impl CatalogEntry {
    /// Locate a model-supplied quote in this source.
    pub fn locate(&self, quote: &str) -> Result<QuoteMatch, QuoteRejection> {
        self.searchable.locate(quote)
    }

    /// Is this value written in the given fragment, as a whole token?
    pub fn quote_contains_value(quote: &str, value: &str) -> bool {
        otdel_knowledge::quote::contains_token(quote, value)
    }
}

/// Every external source a plan may cite, and nothing else.
#[derive(Debug, Clone, Default)]
pub struct ExternalCatalog {
    entries: Vec<CatalogEntry>,
}

impl ExternalCatalog {
    /// Label the sources `E1…En`, in the order they were read.
    ///
    /// A source with no usable text is dropped rather than labelled: offering the model a
    /// label it cannot quote from is how an empty citation gets invented.
    pub fn build(sources: Vec<ExternalSource>) -> Self {
        let entries = sources
            .into_iter()
            .filter(|source| !source.text.trim().is_empty())
            .enumerate()
            .map(|(index, source)| CatalogEntry {
                label: format!("E{}", index + 1),
                searchable: SearchableText::new(&source.text),
                source,
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

    /// Resolve a label the model cited. Anything else — a URL, an identifier, a label
    /// from another plan — resolves to nothing.
    pub fn resolve(&self, label: &str) -> Option<&CatalogEntry> {
        let wanted = label.trim();
        self.entries
            .iter()
            .find(|entry| entry.label.eq_ignore_ascii_case(wanted))
    }

    pub fn total_chars(&self) -> usize {
        self.entries
            .iter()
            .map(|entry| entry.source.text.chars().count())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(id: u128, url: &str, text: &str) -> ExternalSource {
        ExternalSource {
            source_id: Uuid::from_u128(id),
            url: url.to_owned(),
            host: url
                .trim_start_matches("https://")
                .split('/')
                .next()
                .unwrap_or_default()
                .to_owned(),
            title: None,
            retrieved_at: Some(Utc::now()),
            content_hash: Some("a".repeat(64)),
            license: None,
            text: text.to_owned(),
        }
    }

    #[test]
    fn labels_are_assigned_in_order_and_resolve_back_to_their_source() {
        let catalog = ExternalCatalog::build(vec![
            source(
                1,
                "https://docs.example.org/a",
                "минимальная толщина 55 мкм",
            ),
            source(2, "https://docs.example.org/b", "класс покрытия 2"),
        ]);

        assert_eq!(catalog.len(), 2);
        assert_eq!(catalog.entries()[0].label, "E1");
        assert_eq!(
            catalog.resolve("E2").unwrap().source.url,
            "https://docs.example.org/b"
        );
        assert_eq!(
            catalog.resolve("e2").unwrap().source.source_id,
            Uuid::from_u128(2)
        );
        assert!(catalog.total_chars() > 0);
    }

    #[test]
    fn a_label_outside_the_plan_resolves_to_nothing() {
        let catalog = ExternalCatalog::build(vec![source(
            1,
            "https://docs.example.org/a",
            "минимальная толщина 55 мкм",
        )]);

        for spoofed in [
            "E2",
            "E404",
            "",
            // The two shapes a model reaches for when it wants to cite something real.
            "https://docs.example.org/a",
            "00000000-0000-0000-0000-000000000001",
            "S1",
        ] {
            assert!(
                catalog.resolve(spoofed).is_none(),
                "{spoofed:?} must not resolve to a source"
            );
        }
    }

    #[test]
    fn a_source_with_no_text_is_never_offered_for_quoting() {
        let catalog = ExternalCatalog::build(vec![
            source(1, "https://docs.example.org/empty", "   \n  "),
            source(
                2,
                "https://docs.example.org/real",
                "настоящий текст страницы",
            ),
        ]);

        assert_eq!(catalog.len(), 1);
        // The surviving source takes the first label: labels describe this catalogue,
        // not the list it was built from.
        assert_eq!(catalog.entries()[0].label, "E1");
        assert_eq!(catalog.entries()[0].source.source_id, Uuid::from_u128(2));
    }

    #[test]
    fn a_quote_is_located_in_the_sources_own_wording() {
        let catalog = ExternalCatalog::build(vec![source(
            1,
            "https://docs.example.org/gost",
            "Минимальная   толщина покрытия  55 мкм для изделий толщиной до 1,5 мм",
        )]);
        let entry = catalog.resolve("E1").unwrap();

        // The model writes single spaces; the page does not. What is stored is the page's.
        let located = entry.locate("Минимальная толщина покрытия 55 мкм").unwrap();
        assert_eq!(located.text, "Минимальная   толщина покрытия  55 мкм");
        assert!(CatalogEntry::quote_contains_value(&located.text, "55"));
        // …and a value that is only part of another number is not confirmed.
        assert!(!CatalogEntry::quote_contains_value(&located.text, "5"));

        // An invented sentence is not found at all.
        assert!(entry.locate("Минимальная толщина покрытия 80 мкм").is_err());
    }
}
