//! What survives validation — the only thing the storage layer ever sees.
//!
//! As in 1C, the decisive property is a type one: a [`ResolvedExternalEvidence`] cannot
//! be built from a model's words. It is produced by [`crate::validate`] from a source
//! that belongs to this plan, carrying the page's own wording and the offsets where it
//! sits in the stored snapshot. There is therefore no path from "the model said so" to a
//! stored finding.
//!
//! And one property this phase adds: a [`CandidateFinding`] has no product, no material
//! and no partner attribute. It is a statement about the industry, and the type simply
//! offers no way to make it a statement about the partner.

use uuid::Uuid;

/// A fragment of a real external source of this plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedExternalEvidence {
    pub source_id: Uuid,
    pub url: String,
    /// The source's own wording, not the model's.
    pub quote: String,
    /// Character offsets into the stored snapshot.
    pub char_start: i32,
    pub char_end: i32,
}

/// A candidate industry statement with the external sources behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateFinding {
    pub topic: String,
    pub attribute: String,
    pub value_text: String,
    /// Kept only when the unit is literally present in a cited fragment.
    pub unit: Option<String>,
    /// Kept only when the conditions are literally present in a cited fragment;
    /// otherwise the text is moved into `model_context` and labelled as the model's.
    pub conditions: Option<String>,
    pub model_context: Option<String>,
    /// Never empty: a finding without an external source does not reach this type.
    pub evidence: Vec<ResolvedExternalEvidence>,
}

/// Everything one interpretation pass produced, plus an honest account of what it
/// refused.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FindingsDraft {
    pub findings: Vec<CandidateFinding>,
    /// How many candidates were refused outright.
    pub rejected: u32,
    /// Why, in words, deduplicated and bounded. Shown to the owner as-is.
    pub rejections: Vec<String>,
    /// The model's statement that the sources do not answer the question. Not a failure
    /// — an answer, and a more useful one than an invented finding.
    pub not_found: Option<String>,
}

/// Upper bound on stored rejection lines: the reasons repeat, and a plan is not a log.
pub const MAX_REJECTION_LINES: usize = 40;

impl FindingsDraft {
    pub fn is_empty(&self) -> bool {
        self.findings.is_empty()
    }

    /// Record one refusal.
    pub fn reject(&mut self, reason: impl Into<String>) {
        self.rejected = self.rejected.saturating_add(1);
        self.note(reason);
    }

    /// Record something the owner should know that is not itself a refusal.
    pub fn note(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        if self.rejections.len() >= MAX_REJECTION_LINES || self.rejections.contains(&reason) {
            return;
        }
        self.rejections.push(reason);
    }

    /// Merge the result of another request of the same plan.
    ///
    /// The same statement found on a second page is not a second finding: it is the same
    /// finding with more evidence. Folding them keeps the plan readable and keeps both
    /// citations, which is the shape a checker in 1E needs.
    pub fn merge(&mut self, other: Self) {
        for finding in other.findings {
            match self
                .findings
                .iter_mut()
                .find(|kept| same_statement(kept, &finding))
            {
                Some(kept) => {
                    for evidence in finding.evidence {
                        let known = kept.evidence.iter().any(|existing| {
                            existing.source_id == evidence.source_id
                                && existing.char_start == evidence.char_start
                        });
                        if !known {
                            kept.evidence.push(evidence);
                        }
                    }
                }
                None => self.findings.push(finding),
            }
        }

        self.rejected = self.rejected.saturating_add(other.rejected);
        for reason in other.rejections {
            self.note(reason);
        }
        if self.not_found.is_none() {
            self.not_found = other.not_found;
        }
    }
}

/// Same property, same value, same unit, same conditions — the same statement.
///
/// The unit and the conditions are part of it: `55 мкм` and `55 мм` are not one finding,
/// and neither are two thicknesses that hold in different circumstances.
fn same_statement(left: &CandidateFinding, right: &CandidateFinding) -> bool {
    same_text(&left.attribute, &right.attribute)
        && same_text(&left.value_text, &right.value_text)
        && left.unit.as_deref().map(normalise) == right.unit.as_deref().map(normalise)
        && left.conditions.as_deref().map(normalise) == right.conditions.as_deref().map(normalise)
}

fn same_text(left: &str, right: &str) -> bool {
    normalise(left) == normalise(right)
}

pub(crate) fn normalise(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(source: u128, start: i32) -> ResolvedExternalEvidence {
        ResolvedExternalEvidence {
            source_id: Uuid::from_u128(source),
            url: format!("https://docs.example.org/{source}"),
            quote: "минимальная толщина покрытия 55 мкм".to_owned(),
            char_start: start,
            char_end: start + 35,
        }
    }

    fn finding(value: &str, unit: Option<&str>, source: u128) -> CandidateFinding {
        CandidateFinding {
            topic: "покрытие".to_owned(),
            attribute: "минимальная толщина".to_owned(),
            value_text: value.to_owned(),
            unit: unit.map(str::to_owned),
            conditions: None,
            model_context: None,
            evidence: vec![evidence(source, 0)],
        }
    }

    #[test]
    fn the_same_statement_from_two_sources_is_one_finding_with_two_citations() {
        let mut first = FindingsDraft {
            findings: vec![finding("55", Some("мкм"), 1)],
            ..FindingsDraft::default()
        };
        first.merge(FindingsDraft {
            findings: vec![finding(" 55 ", Some("МКМ"), 2)],
            ..FindingsDraft::default()
        });

        assert_eq!(first.findings.len(), 1, "one statement, not two");
        assert_eq!(first.findings[0].evidence.len(), 2, "both sources are kept");
    }

    #[test]
    fn a_different_unit_is_a_different_statement() {
        let mut first = FindingsDraft {
            findings: vec![finding("55", Some("мкм"), 1)],
            ..FindingsDraft::default()
        };
        first.merge(FindingsDraft {
            findings: vec![finding("55", Some("мм"), 2)],
            ..FindingsDraft::default()
        });
        assert_eq!(first.findings.len(), 2);
    }

    #[test]
    fn the_same_citation_twice_is_recorded_once() {
        let mut first = FindingsDraft {
            findings: vec![finding("55", Some("мкм"), 1)],
            ..FindingsDraft::default()
        };
        first.merge(FindingsDraft {
            findings: vec![finding("55", Some("мкм"), 1)],
            ..FindingsDraft::default()
        });
        assert_eq!(first.findings[0].evidence.len(), 1);
    }

    #[test]
    fn rejections_are_counted_once_and_bounded() {
        let mut draft = FindingsDraft::default();
        for _ in 0..5 {
            draft.reject("источник E9 не входит в это исследование");
        }
        assert_eq!(draft.rejected, 5, "every refused candidate is counted");
        assert_eq!(draft.rejections.len(), 1, "the reason is recorded once");

        for index in 0..100 {
            draft.note(format!("причина {index}"));
        }
        assert_eq!(draft.rejections.len(), MAX_REJECTION_LINES);
    }

    #[test]
    fn a_statement_that_the_sources_do_not_answer_survives_a_merge() {
        let mut first = FindingsDraft::default();
        first.merge(FindingsDraft {
            not_found: Some("источники не содержат этого значения".to_owned()),
            ..FindingsDraft::default()
        });
        assert_eq!(
            first.not_found.as_deref(),
            Some("источники не содержат этого значения")
        );
    }
}
