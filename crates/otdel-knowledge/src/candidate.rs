//! What survives validation — the only thing the database layer ever sees.
//!
//! These types differ from [`crate::schema`]'s in one decisive way: a
//! [`ResolvedEvidence`] cannot be constructed from a model's words. It is produced by
//! [`crate::validate`] from a page that belongs to this run, carrying the page's own
//! wording and the offsets where it sits. There is therefore no path from "the model
//! said so" to a stored fact — the type system carries the rule.

use otdel_core::knowledge::{CategoryKind, FactKind, ProductKind, QuestionAudience};
use uuid::Uuid;

/// A fragment of a real page of this run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEvidence {
    pub page_id: Uuid,
    pub material_id: Uuid,
    pub page_number: i32,
    /// The page's own wording, not the model's.
    pub quote: String,
    pub char_start: i32,
    pub char_end: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateCategory {
    /// Response-local reference, namespaced per request when several are merged.
    pub reference: String,
    pub kind: CategoryKind,
    pub name: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateProduct {
    pub reference: String,
    pub category_ref: Option<String>,
    pub kind: ProductKind,
    pub name: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateFact {
    pub product_ref: Option<String>,
    pub kind: FactKind,
    pub attribute: String,
    pub value_text: String,
    /// Kept only when the unit is literally present in the value or in a quote.
    pub unit: Option<String>,
    /// Kept only when the conditions are literally present in a quote; otherwise the
    /// text is moved into `model_context`, where it is shown as the model's wording.
    pub conditions: Option<String>,
    pub model_context: Option<String>,
    /// Never empty: a fact without evidence does not reach this type.
    pub evidence: Vec<ResolvedEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateTerm {
    pub term: String,
    pub definition: String,
    /// `true` when the definition is the model's wording rather than the source's.
    pub definition_is_model_context: bool,
    pub evidence: Vec<ResolvedEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateQa {
    pub question: String,
    pub answer: String,
    /// `true` when the answer is the model's own sentence rather than the source's
    /// wording — which is the usual case, an answer being a synthesis.
    pub answer_is_model_context: bool,
    pub evidence: Vec<ResolvedEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateQuestion {
    pub audience: QuestionAudience,
    pub text: String,
}

/// A gap needs no evidence: it records what the material does *not* say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateGap {
    pub product_ref: Option<String>,
    pub topic: String,
    pub missing: String,
    pub blocks: Option<String>,
    pub question: Option<CandidateQuestion>,
}

/// Everything one run produced, plus an honest account of what it refused.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CandidateDraft {
    pub categories: Vec<CandidateCategory>,
    pub products: Vec<CandidateProduct>,
    pub facts: Vec<CandidateFact>,
    pub terms: Vec<CandidateTerm>,
    pub qa: Vec<CandidateQa>,
    pub gaps: Vec<CandidateGap>,
    /// How many candidates were refused outright.
    pub rejected: u32,
    /// Why, in words, deduplicated and bounded. Shown to the owner as-is.
    pub rejections: Vec<String>,
}

/// Upper bound on stored rejection lines: the reasons repeat, and a run record is not
/// a log file.
pub const MAX_REJECTION_LINES: usize = 40;

impl CandidateDraft {
    pub fn is_empty(&self) -> bool {
        self.categories.is_empty()
            && self.products.is_empty()
            && self.facts.is_empty()
            && self.terms.is_empty()
            && self.qa.is_empty()
            && self.gaps.is_empty()
    }

    /// Record one refusal. Repeated reasons are counted once.
    pub fn reject(&mut self, reason: impl Into<String>) {
        self.rejected = self.rejected.saturating_add(1);
        self.note(reason);
    }

    /// Record something the owner should know that is not itself a refusal (a unit
    /// that could not be confirmed, a page that did not fit the budget).
    pub fn note(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        if self.rejections.len() >= MAX_REJECTION_LINES || self.rejections.contains(&reason) {
            return;
        }
        self.rejections.push(reason);
    }

    /// Merge the result of another request of the same run.
    ///
    /// References are response-local, so the caller namespaces them before merging
    /// (`b2:p1`). Categories and products that repeat by kind and normalised name are
    /// folded into the first occurrence and the later references are remapped — the
    /// same profile mentioned on two pages is one product, but two *different* names
    /// are never merged (`docs/block-01-spec.md` §6.6: synonyms do not merge products
    /// without proof).
    pub fn merge(&mut self, other: Self) {
        let mut remap: Vec<(String, String)> = Vec::new();

        for category in other.categories {
            match self
                .categories
                .iter()
                .find(|kept| kept.kind == category.kind && same_name(&kept.name, &category.name))
            {
                Some(kept) => remap.push((category.reference.clone(), kept.reference.clone())),
                None => self.categories.push(category),
            }
        }

        for mut product in other.products {
            if let Some(target) = product
                .category_ref
                .as_ref()
                .and_then(|reference| lookup(&remap, reference))
            {
                product.category_ref = Some(target);
            }
            match self
                .products
                .iter()
                .find(|kept| kept.kind == product.kind && same_name(&kept.name, &product.name))
            {
                Some(kept) => remap.push((product.reference.clone(), kept.reference.clone())),
                None => self.products.push(product),
            }
        }

        for mut fact in other.facts {
            if let Some(target) = fact
                .product_ref
                .as_ref()
                .and_then(|reference| lookup(&remap, reference))
            {
                fact.product_ref = Some(target);
            }

            // The same statement found on a second page is not a second fact: it is
            // the same fact with more evidence. Folding them keeps the draft readable
            // and keeps both citations.
            match self
                .facts
                .iter_mut()
                .find(|kept| same_statement(kept, &fact))
            {
                Some(kept) => {
                    for evidence in fact.evidence {
                        let known = kept.evidence.iter().any(|existing| {
                            existing.page_id == evidence.page_id
                                && existing.char_start == evidence.char_start
                        });
                        if !known {
                            kept.evidence.push(evidence);
                        }
                    }
                }
                None => self.facts.push(fact),
            }
        }

        for mut gap in other.gaps {
            if let Some(target) = gap
                .product_ref
                .as_ref()
                .and_then(|reference| lookup(&remap, reference))
            {
                gap.product_ref = Some(target);
            }
            self.gaps.push(gap);
        }

        self.terms.extend(other.terms);
        self.qa.extend(other.qa);
        self.rejected = self.rejected.saturating_add(other.rejected);
        for reason in other.rejections {
            self.note(reason);
        }
    }
}

/// Same subject, same property, same value — and therefore the same statement.
///
/// The unit and the conditions are part of the statement: `3.5 kN` and `3.5 т` are not
/// the same fact, and neither are two loads that hold under different conditions.
fn same_statement(left: &CandidateFact, right: &CandidateFact) -> bool {
    left.product_ref == right.product_ref
        && left.kind == right.kind
        && same_name(&left.attribute, &right.attribute)
        && same_name(&left.value_text, &right.value_text)
        && left.unit.as_deref().map(normalise_name) == right.unit.as_deref().map(normalise_name)
        && left.conditions.as_deref().map(normalise_name)
            == right.conditions.as_deref().map(normalise_name)
}

fn lookup(remap: &[(String, String)], reference: &str) -> Option<String> {
    remap
        .iter()
        .find(|(from, _)| from == reference)
        .map(|(_, to)| to.clone())
}

/// Case- and whitespace-insensitive name comparison. Nothing stronger: a different
/// spelling is a different product until something proves otherwise.
fn same_name(left: &str, right: &str) -> bool {
    normalise_name(left) == normalise_name(right)
}

pub(crate) fn normalise_name(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn product(reference: &str, name: &str) -> CandidateProduct {
        CandidateProduct {
            reference: reference.to_owned(),
            category_ref: None,
            kind: ProductKind::Product,
            name: name.to_owned(),
            summary: None,
        }
    }

    fn fact(product_ref: &str, attribute: &str) -> CandidateFact {
        CandidateFact {
            product_ref: Some(product_ref.to_owned()),
            kind: FactKind::Characteristic,
            attribute: attribute.to_owned(),
            value_text: "3.5".to_owned(),
            unit: None,
            conditions: None,
            model_context: None,
            evidence: vec![ResolvedEvidence {
                page_id: Uuid::from_u128(1),
                material_id: Uuid::from_u128(2),
                page_number: 1,
                quote: "BP21 1200 3.5".to_owned(),
                char_start: 0,
                char_end: 13,
            }],
        }
    }

    #[test]
    fn merging_folds_the_same_product_and_remaps_its_facts() {
        let mut first = CandidateDraft {
            products: vec![product("b1:p1", "BP21")],
            facts: vec![fact("b1:p1", "длина")],
            ..CandidateDraft::default()
        };
        first.merge(CandidateDraft {
            products: vec![product("b2:p1", " bp21 ")],
            facts: vec![fact("b2:p1", "нагрузка")],
            ..CandidateDraft::default()
        });

        assert_eq!(first.products.len(), 1, "the same name is one product");
        assert_eq!(first.facts.len(), 2);
        assert!(first
            .facts
            .iter()
            .all(|fact| fact.product_ref.as_deref() == Some("b1:p1")));
    }

    #[test]
    fn two_different_names_stay_two_products() {
        let mut first = CandidateDraft {
            products: vec![product("b1:p1", "BP21")],
            ..CandidateDraft::default()
        };
        first.merge(CandidateDraft {
            products: vec![product("b2:p1", "BP21D")],
            ..CandidateDraft::default()
        });
        assert_eq!(first.products.len(), 2);
    }

    #[test]
    fn rejections_are_counted_once_and_bounded() {
        let mut draft = CandidateDraft::default();
        for _ in 0..5 {
            draft.reject("источник S9 не входит в этот материал");
        }
        assert_eq!(draft.rejected, 5, "every refused candidate is counted");
        assert_eq!(draft.rejections.len(), 1, "the reason is recorded once");

        for index in 0..100 {
            draft.note(format!("причина {index}"));
        }
        assert_eq!(draft.rejections.len(), MAX_REJECTION_LINES);
    }
}
