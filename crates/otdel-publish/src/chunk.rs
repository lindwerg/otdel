//! The searchable rendering of a claim, and the folding used to compare names.
//!
//! One chunk per claim, and that is a decision rather than a simplification waiting to
//! be fixed. `block-01-spec.md` §6.4 asks for meaningful chunks carrying the chain
//! partner → product → document → version → region; a claim already *is* that chain, and
//! what a claim needs in order to be **found** — its product, its property, its value,
//! its unit, its conditions and the words of its citations — is exactly what a claim
//! contains. Splitting prose into 400–800-token windows is the hypothesis the same
//! section calls a hypothesis, and it buys nothing here: there is no prose to split.
//!
//! The chunk text is what both halves of the keyword search read: PostgreSQL's
//! `to_tsvector('russian', …)` is computed from it by a generated column, and the exact
//! half looks up the folded value and attribute stored beside it.

use crate::claim::CheckedClaim;

/// Fold a name for comparison and for the exact half of the search.
///
/// Whitespace collapses, case folds, and the dash and quote variants that a text layer
/// and an OCR result legitimately disagree about are unified. Latin and Cyrillic
/// look-alikes are **not** folded, for the reason 1C gives: `BC-21` and `ВС-21` are
/// different designations, and quietly making them the same would merge two products.
pub fn normalise(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut pending_space = false;
    for ch in value.chars() {
        if ch.is_whitespace() || ch.is_control() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        match ch {
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2212}' => {
                out.push('-')
            }
            '«' | '»' | '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{2018}' | '\u{2019}' => {
                out.push('"')
            }
            other => {
                for folded in other.to_lowercase() {
                    out.push(folded);
                }
            }
        }
    }
    out
}

/// Clip to a character count without splitting a character.
pub fn clip(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

/// Fold and bound a name for a database column that accepts 1..200 characters.
///
/// Folding first and bounding second matters: `normalise` removes control characters, so
/// a value made only of them becomes empty here and is reported by the caller, instead
/// of passing a `char_length(btrim(...)) >= 1` check in Rust and then violating it in
/// PostgreSQL — which would abort the whole version rather than refuse one claim. 1D
/// learned that the hard way (`docs/research-1d.md`, defect 10).
pub fn normalised_column(value: &str) -> Option<String> {
    let folded = clip(&normalise(value), 200);
    (!folded.is_empty()).then_some(folded)
}

/// The searchable text of one claim.
///
/// Product, property, value, unit, conditions, then the citations' own words. The
/// citations are included because a question is far more often phrased in the document's
/// vocabulary than in the attribute name a model chose — "при опирании на две опоры"
/// finds the claim through its quotation, not through the word «нагрузка».
///
/// `model_context` is deliberately **excluded**. It is the drafting model's paraphrase;
/// letting it into the search index would let a model's own wording decide what the
/// published knowledge appears to say.
pub fn chunk_text(claim: &CheckedClaim, max_chars: usize) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(product) = &claim.product_name {
        parts.push(product.clone());
    }
    parts.push(claim.attribute.clone());
    parts.push(claim.value_text.clone());
    if let Some(unit) = &claim.unit {
        parts.push(unit.clone());
    }
    if let Some(conditions) = &claim.conditions {
        parts.push(conditions.clone());
    }
    for evidence in &claim.evidence {
        parts.push(evidence.quote.clone());
    }

    let joined = parts
        .into_iter()
        .map(|part| part.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");

    clip(&joined, max_chars)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claim::CheckedEvidence;
    use otdel_core::knowledge::FactKind;
    use otdel_core::publication::{ClaimOrigin, ClaimStatus, EvidenceSourceKind};
    use uuid::Uuid;

    fn evidence(quote: &str) -> CheckedEvidence {
        CheckedEvidence {
            source_kind: EvidenceSourceKind::Material,
            material_id: Some(Uuid::from_u128(1)),
            material_filename: Some("catalogue.pdf".to_owned()),
            page_number: Some(3),
            region_id: None,
            url: None,
            host: None,
            retrieved_at: None,
            content_hash: None,
            quote: quote.to_owned(),
            char_start: 0,
            char_end: 10,
        }
    }

    fn claim() -> CheckedClaim {
        CheckedClaim {
            origin: ClaimOrigin::PartnerMaterial,
            origin_id: Uuid::from_u128(1),
            product_name: Some("BP21".to_owned()),
            kind: FactKind::Characteristic,
            status: ClaimStatus::SourceSupported,
            attribute: "нагрузка".to_owned(),
            value_text: "3.5".to_owned(),
            unit: Some("kN".to_owned()),
            conditions: Some("при опирании на две опоры".to_owned()),
            model_context: Some("модель считает, что это предельная нагрузка".to_owned()),
            check_note: None,
            evidence: vec![evidence("BP21 1200 3.5 kN при опирании на две опоры")],
        }
    }

    #[test]
    fn a_chunk_carries_the_product_the_property_the_value_and_the_citation() {
        let text = chunk_text(&claim(), 2_000);
        assert!(text.contains("BP21"), "{text}");
        assert!(text.contains("нагрузка"), "{text}");
        assert!(text.contains("3.5"), "{text}");
        assert!(text.contains("kN"), "{text}");
        assert!(text.contains("при опирании на две опоры"), "{text}");
    }

    #[test]
    fn a_chunk_never_carries_the_models_own_words() {
        // Indexing the paraphrase would let a model's wording decide what the published
        // knowledge appears to say.
        let text = chunk_text(&claim(), 2_000);
        assert!(!text.contains("модель считает"), "{text}");
    }

    #[test]
    fn a_chunk_is_bounded_and_the_bound_does_not_split_a_character() {
        let mut long = claim();
        long.evidence = vec![evidence(&"я".repeat(5_000))];
        let text = chunk_text(&long, 300);
        assert_eq!(text.chars().count(), 300);
    }

    #[test]
    fn folding_unifies_spacing_case_and_dash_variants_but_never_scripts() {
        assert_eq!(normalise("  BP\u{2013}21 "), "bp-21");
        assert_eq!(normalise("«Базис»"), "\"базис\"");
        // Latin C and Cyrillic С stay different: they are different designations.
        assert_ne!(normalise("BC-21"), normalise("ВС-21"));
    }

    #[test]
    fn a_name_made_only_of_control_characters_is_refused_before_it_reaches_the_column() {
        // `str::trim` does not remove U+0001, so a check done before folding would pass
        // it and the database CHECK would then abort the whole version.
        assert_eq!(normalised_column("\u{1}\u{1}"), None);
        assert_eq!(normalised_column("   "), None);
        assert_eq!(normalised_column(" BP21 "), Some("bp21".to_owned()));
    }

    #[test]
    fn a_folded_name_is_bounded_to_what_the_column_accepts() {
        let folded = normalised_column(&"a".repeat(500)).unwrap();
        assert_eq!(folded.chars().count(), 200);
    }
}
