//! Reading a table as a table — the consumer R03 was missing.
//!
//! R03 stopped guessing about table cells: every cell got a role, a verdict and a
//! structural context (subject, property, unit, conditions), each piece carrying whether
//! it was *read* or *inferred*. Nothing then used any of it. The understanding phase kept
//! building its prompt from the page's flat text, where `BP21`, `3,5` and `кН` are three
//! unrelated tokens on three different lines, and the ambiguous cells — the ones a person
//! would have to settle — simply never became anything.
//!
//! This module closes both halves:
//!
//! * [`partition`] splits a page's cells into the ones whose structure is established and
//!   the ones that are not. The first set becomes structured context in the prompt, so the
//!   model is told *which product* and *which property* a number belongs to instead of
//!   inferring it from layout. The second set becomes uncertainties — never facts. That
//!   is the rule stated from the negative side: an unclear cell has exactly one legal
//!   destination, and it is not `knowledge_facts`.
//! * [`confirm_against_cells`] takes a fact that survived quotation checking and asks
//!   whether a *usable* cell says the same thing. When one does, the fact records which
//!   cell, with the subject, property, unit and conditions that cell carried. That is what
//!   makes "a structured table-derived fact with identity, unit and condition context" a
//!   checkable claim rather than a description.
//!
//! Uncertainties are grouped rather than emitted per cell. A load table with forty
//! unit-less numbers is one problem on one page, and forty identical rows would bury it.

use std::collections::BTreeSet;

use otdel_core::extraction::TableCell;
use otdel_core::extraction_context::{CellRole, CellUsability};
use otdel_core::passport::UncertaintyKind;
use uuid::Uuid;

use crate::candidate::normalise_name;

/// Upper bound on structured rows shown for one page. A page of a parts catalogue can
/// hold hundreds; the prompt has a character budget and the point is to establish the
/// shape of the table, not to resend it.
const MAX_STRUCTURED_ROWS_PER_PAGE: usize = 40;

/// A cell whose structure is established: what it is about, which property, what value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuredCell {
    pub cell_id: Uuid,
    pub page_id: Uuid,
    pub page_number: i32,
    pub region_id: Uuid,
    /// The product the row is about, as the table states it.
    pub subject: String,
    /// The property the column states.
    pub property: String,
    /// The value, verbatim. Never parsed, never rounded — the same discipline as
    /// everywhere else.
    pub value: String,
    pub unit: Option<String>,
    pub conditions: Vec<String>,
    /// `true` when any part of the context above was inferred (carried over a merged
    /// cell) rather than read. Shown to the model, and to the reader, as such.
    pub context_inferred: bool,
}

impl StructuredCell {
    /// One line of structured context for the prompt.
    ///
    /// Deliberately not a quotation: it is the server's reading of the grid, and the
    /// model is told so. Evidence still has to be a verbatim fragment of the page.
    pub fn prompt_line(&self) -> String {
        let mut line = format!(
            "изделие: {} | свойство: {} | значение: {}",
            clean(&self.subject),
            clean(&self.property),
            clean(&self.value)
        );
        if let Some(unit) = &self.unit {
            line.push_str(&format!(" | единица: {}", clean(unit)));
        }
        if !self.conditions.is_empty() {
            let conditions: Vec<String> = self.conditions.iter().map(|c| clean(c)).collect();
            line.push_str(&format!(" | условия: {}", conditions.join("; ")));
        }
        if self.context_inferred {
            line.push_str(" | внимание: часть контекста перенесена из объединённой ячейки");
        }
        line
    }
}

/// A group of cells nobody may read as a value, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellUncertainty {
    pub kind: UncertaintyKind,
    pub page_id: Uuid,
    pub page_number: i32,
    pub region_id: Uuid,
    /// What is unclear: the column, or the table, in one line.
    pub subject: String,
    /// Why, in words a person can act on.
    pub detail: String,
    /// R03's own reason identifiers, so the interface can group without re-deriving them.
    pub reasons: Vec<String>,
    /// How many cells share this problem.
    pub cell_count: usize,
    /// One example cell's text, shown as *not* a claim.
    pub example: Option<String>,
}

/// What a page's table cells amount to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableReading {
    /// Cells whose structure is established, ready to be shown as context.
    pub structured: Vec<StructuredCell>,
    /// Everything a person still has to settle, grouped.
    pub uncertainties: Vec<CellUncertainty>,
    /// Cells skipped as labels or blanks. Counted rather than reported: a column heading
    /// being a heading is not a finding.
    pub labels_skipped: usize,
}

impl TableReading {
    pub fn is_empty(&self) -> bool {
        self.structured.is_empty() && self.uncertainties.is_empty()
    }

    /// The structured lines for one page's prompt block, bounded.
    ///
    /// Returns the lines and how many were left out, so the prompt can say so instead of
    /// silently showing part of a table — the same rule the page budget follows.
    pub fn prompt_lines(&self) -> (Vec<String>, usize) {
        let shown: Vec<String> = self
            .structured
            .iter()
            .take(MAX_STRUCTURED_ROWS_PER_PAGE)
            .map(StructuredCell::prompt_line)
            .collect();
        let omitted = self.structured.len().saturating_sub(shown.len());
        (shown, omitted)
    }
}

/// Split one page's cells into established structure and open questions.
///
/// `page_id`/`page_number` are the server's, never the cell's own claim about where it
/// lives — a cell carries a region, and the caller knows which page that region is on.
pub fn partition(page_id: Uuid, page_number: i32, cells: &[TableCell]) -> TableReading {
    let mut reading = TableReading::default();
    // (region, sorted reasons) → the group being accumulated.
    let mut groups: Vec<(Uuid, Vec<String>, CellUncertainty)> = Vec::new();

    for cell in cells {
        // A header is a label and a blank is a blank. Both are correctly refused by R03,
        // and neither is something a person has to settle: reporting them would drown the
        // cells that genuinely are unclear.
        if cell.role != CellRole::Data || cell.raw_text.trim().is_empty() {
            reading.labels_skipped += 1;
            continue;
        }

        let context = &cell.structural_context;
        let subject = context
            .subject
            .as_ref()
            .map(|reference| reference.text.clone());
        let property = context
            .property
            .as_ref()
            .map(|reference| reference.text.clone());

        match (cell.verdict.usability, subject, property) {
            (CellUsability::Usable, Some(subject), Some(property)) => {
                reading.structured.push(StructuredCell {
                    cell_id: cell.id,
                    page_id,
                    page_number,
                    region_id: cell.region_id,
                    subject,
                    property,
                    value: cell.raw_text.trim().to_owned(),
                    unit: context
                        .unit
                        .as_ref()
                        .map(|unit| unit.unit.clone())
                        .or_else(|| cell.unit.clone()),
                    conditions: context
                        .conditions
                        .iter()
                        .map(|condition| condition.text.clone())
                        .collect(),
                    context_inferred: context.has_inferred_context(),
                });
            }
            // Usable by R03's verdict, and the grid still could not say what the number
            // is about. Not a fact and not nothing: a question about page N.
            (CellUsability::Usable, subject, property) => {
                let missing = match (subject.is_some(), property.is_some()) {
                    (false, false) => "не удалось определить ни изделие, ни свойство",
                    (false, true) => "не удалось определить, к какому изделию относится значение",
                    _ => "не удалось определить, какое свойство измеряет столбец",
                };
                accumulate(
                    &mut groups,
                    cell,
                    page_id,
                    page_number,
                    UncertaintyKind::UnresolvedSubject,
                    missing,
                    &["structural_context_incomplete".to_owned()],
                );
            }
            (usability, _, _) => {
                let reasons: Vec<String> = cell
                    .verdict
                    .reasons
                    .iter()
                    .map(|reason| reason.as_str().to_owned())
                    .collect();
                let kind = if cell
                    .verdict
                    .reasons
                    .iter()
                    .any(|reason| reason.as_str() == "unit_unresolved")
                {
                    UncertaintyKind::UnresolvedUnit
                } else {
                    UncertaintyKind::AmbiguousTableCell
                };
                let detail = cell.verdict.describe().join("; ");
                let detail = if detail.is_empty() {
                    match usability {
                        CellUsability::Unusable => "ячейку нельзя читать как значение",
                        _ => "значение ячейки неоднозначно",
                    }
                    .to_owned()
                } else {
                    detail
                };
                accumulate(
                    &mut groups,
                    cell,
                    page_id,
                    page_number,
                    kind,
                    &detail,
                    &reasons,
                );
            }
        }
    }

    reading.uncertainties = groups.into_iter().map(|(_, _, group)| group).collect();
    reading
}

/// Fold one cell into the group of cells that share its region and its reasons.
fn accumulate(
    groups: &mut Vec<(Uuid, Vec<String>, CellUncertainty)>,
    cell: &TableCell,
    page_id: Uuid,
    page_number: i32,
    kind: UncertaintyKind,
    detail: &str,
    reasons: &[String],
) {
    let key: Vec<String> = reasons
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let column = cell
        .structural_context
        .property
        .as_ref()
        .map(|reference| reference.text.clone())
        .or_else(|| cell.column_header.clone())
        .unwrap_or_else(|| format!("столбец {}", cell.column_index + 1));

    if let Some((_, _, group)) = groups
        .iter_mut()
        .find(|(region, existing, _)| *region == cell.region_id && *existing == key)
    {
        group.cell_count += 1;
        return;
    }

    groups.push((
        cell.region_id,
        key,
        CellUncertainty {
            kind,
            page_id,
            page_number,
            region_id: cell.region_id,
            subject: format!("таблица на стр. {page_number}, {}", clean(&column)),
            detail: detail.to_owned(),
            reasons: reasons.to_vec(),
            cell_count: 1,
            example: Some(clean(&cell.raw_text)).filter(|text| !text.is_empty()),
        },
    ));
}

/// What a usable cell confirms about an accepted fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuralConfirmation {
    pub cell_id: Uuid,
    pub subject: String,
    pub property: String,
    pub unit: Option<String>,
    pub conditions: Vec<String>,
}

/// Find the cell an accepted fact was read out of, if there is one.
///
/// The match is deliberately narrow and deliberately *not* fuzzy:
///
/// * the value has to be the cell's own text, folded only for case and whitespace —
///   `3,5` never matches `3.5`, because a table that writes one and a fact that claims
///   the other are not the same reading;
/// * the product name has to be the cell's subject, folded the same way;
/// * the property is checked in the looser direction only: the attribute is the model's
///   name for the column (`нагрузка` for `Безопасная рабочая нагрузка`), so containment
///   either way is accepted. It cannot widen anything on its own — a wrong column with a
///   matching value and subject is already implausible, and the fact keeps its own
///   quotation regardless.
///
/// Returning `None` is a perfectly good answer and the common one. A fact read out of
/// running prose has no cell, and saying `page_text` is the truthful record.
pub fn confirm_against_cells(
    product_name: Option<&str>,
    attribute: &str,
    value: &str,
    cells: &[StructuredCell],
) -> Option<StructuralConfirmation> {
    let wanted_value = normalise_name(value);
    let wanted_product = product_name.map(normalise_name);
    let wanted_attribute = normalise_name(attribute);

    cells
        .iter()
        .find(|cell| {
            if normalise_name(&cell.value) != wanted_value {
                return false;
            }
            match &wanted_product {
                // A fact about the offering as a whole cannot be attributed to a row of a
                // table about one product.
                None => false,
                Some(product) => {
                    if &normalise_name(&cell.subject) != product {
                        return false;
                    }
                    let property = normalise_name(&cell.property);
                    property.contains(&wanted_attribute) || wanted_attribute.contains(&property)
                }
            }
        })
        .map(|cell| StructuralConfirmation {
            cell_id: cell.cell_id,
            subject: cell.subject.clone(),
            property: cell.property.clone(),
            unit: cell.unit.clone(),
            conditions: cell.conditions.clone(),
        })
}

/// One line, control-free, bounded.
fn clean(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use otdel_core::extraction::CellValueKind;
    use otdel_core::extraction_context::{
        AmbiguityReason, CellVerdict, ConditionRef, ContextOrigin, ContextRef, SourceSpan,
        StructuralContext, UnitRef,
    };

    const PAGE: Uuid = Uuid::from_u128(70);
    const REGION: Uuid = Uuid::from_u128(71);

    fn span() -> SourceSpan {
        SourceSpan::unlocated(12, "фикстура без геометрии")
    }

    fn context(
        subject: Option<&str>,
        property: Option<&str>,
        unit: Option<&str>,
        condition: Option<&str>,
        inferred: bool,
    ) -> StructuralContext {
        let origin = if inferred {
            ContextOrigin::InheritedFromMergedRowLabel
        } else {
            ContextOrigin::CellItself
        };
        StructuralContext {
            column_header_path: Vec::new(),
            row_header_path: Vec::new(),
            subject: subject.map(|text| ContextRef::new(text, origin)),
            property: property.map(|text| ContextRef::new(text, ContextOrigin::HeaderRow)),
            unit: unit.map(|unit| UnitRef {
                unit: unit.to_owned(),
                origin: ContextOrigin::HeaderRow,
            }),
            conditions: condition
                .map(|text| {
                    vec![ConditionRef {
                        text: text.to_owned(),
                        marker: None,
                        span: span(),
                    }]
                })
                .unwrap_or_default(),
        }
    }

    fn cell(
        id: u128,
        column: i32,
        raw: &str,
        usability: CellUsability,
        reasons: &[AmbiguityReason],
        structural: StructuralContext,
    ) -> TableCell {
        TableCell {
            id: Uuid::from_u128(id),
            region_id: REGION,
            row_index: 1,
            column_index: column,
            is_header: false,
            raw_text: raw.to_owned(),
            value_kind: if raw.trim().is_empty() {
                CellValueKind::Empty
            } else {
                CellValueKind::Text
            },
            unit: None,
            column_header: Some("Нагрузка".to_owned()),
            bbox: None,
            role: CellRole::Data,
            verdict: if reasons.is_empty() {
                CellVerdict {
                    usability,
                    reasons: Vec::new(),
                }
            } else {
                CellVerdict::from_reasons(reasons.iter().copied())
            },
            structural_context: structural,
            span: span(),
        }
    }

    /// The positive half: an established cell becomes context the model can use, carrying
    /// identity, unit and condition together.
    #[test]
    fn an_established_cell_becomes_structured_context() {
        let cells = vec![cell(
            1,
            2,
            "3,5",
            CellUsability::Usable,
            &[],
            context(
                Some("BP21"),
                Some("безопасная рабочая нагрузка"),
                Some("кН"),
                Some("при опирании на две опоры"),
                false,
            ),
        )];

        let reading = partition(PAGE, 12, &cells);
        assert_eq!(reading.structured.len(), 1);
        assert!(reading.uncertainties.is_empty());

        let line = reading.structured[0].prompt_line();
        assert!(line.contains("BP21"), "{line}");
        assert!(line.contains("безопасная рабочая нагрузка"), "{line}");
        assert!(line.contains("3,5"), "{line}");
        assert!(line.contains("кН"), "{line}");
        assert!(line.contains("две опоры"), "{line}");
    }

    /// The negative half, and the rule the package rests on: an ambiguous cell has exactly
    /// one destination, and it is never a fact.
    #[test]
    fn an_ambiguous_cell_becomes_an_uncertainty_and_never_a_value() {
        let cells = vec![cell(
            2,
            3,
            "4860 / 8470",
            CellUsability::Ambiguous,
            &[AmbiguityReason::MultipleValuesInOneCell],
            context(Some("BP21"), Some("нагрузка"), None, None, false),
        )];

        let reading = partition(PAGE, 12, &cells);
        assert!(
            reading.structured.is_empty(),
            "an ambiguous cell must not reach the structured set"
        );
        assert_eq!(reading.uncertainties.len(), 1);
        let uncertainty = &reading.uncertainties[0];
        assert_eq!(uncertainty.kind, UncertaintyKind::AmbiguousTableCell);
        assert!(uncertainty
            .reasons
            .contains(&"multiple_values_in_one_cell".to_owned()));
        assert_eq!(uncertainty.example.as_deref(), Some("4860 / 8470"));
    }

    #[test]
    fn a_number_with_no_unit_anywhere_is_reported_as_exactly_that() {
        let cells = vec![cell(
            3,
            1,
            "1200",
            CellUsability::Ambiguous,
            &[AmbiguityReason::UnitUnresolved],
            context(Some("BP21"), Some("длина"), None, None, false),
        )];
        let reading = partition(PAGE, 12, &cells);
        assert_eq!(
            reading.uncertainties[0].kind,
            UncertaintyKind::UnresolvedUnit
        );
        assert!(reading.uncertainties[0].detail.contains("единица"));
    }

    #[test]
    fn cells_sharing_a_problem_are_one_finding_with_a_count() {
        let cells: Vec<TableCell> = (10..50)
            .map(|id| {
                cell(
                    id,
                    2,
                    "1200",
                    CellUsability::Ambiguous,
                    &[AmbiguityReason::UnitUnresolved],
                    context(Some("BP21"), Some("длина"), None, None, false),
                )
            })
            .collect();

        let reading = partition(PAGE, 12, &cells);
        assert_eq!(
            reading.uncertainties.len(),
            1,
            "forty identical rows would bury the finding"
        );
        assert_eq!(reading.uncertainties[0].cell_count, 40);
    }

    #[test]
    fn headers_and_blanks_are_counted_rather_than_reported() {
        let mut header = cell(
            4,
            0,
            "безопасная рабочая нагрузка (Н)",
            CellUsability::Unusable,
            &[AmbiguityReason::HeaderIsNotAValue],
            context(None, None, None, None, false),
        );
        header.role = CellRole::ColumnHeader;
        let blank = cell(
            5,
            1,
            "   ",
            CellUsability::Unusable,
            &[AmbiguityReason::BlankCell],
            context(None, None, None, None, false),
        );

        let reading = partition(PAGE, 12, &[header, blank]);
        assert!(
            reading.is_empty(),
            "a heading being a heading is not a finding"
        );
        assert_eq!(reading.labels_skipped, 2);
    }

    #[test]
    fn an_inherited_context_is_shown_as_an_assumption() {
        let cells = vec![cell(
            6,
            2,
            "3,5",
            CellUsability::Usable,
            &[],
            context(Some("BP21"), Some("нагрузка"), Some("кН"), None, true),
        )];
        let reading = partition(PAGE, 12, &cells);
        assert!(reading.structured[0].context_inferred);
        assert!(reading.structured[0]
            .prompt_line()
            .contains("объединённой ячейки"));
    }

    #[test]
    fn a_usable_cell_with_no_subject_is_a_question_not_a_fact() {
        let cells = vec![cell(
            7,
            2,
            "3,5",
            CellUsability::Usable,
            &[],
            context(None, Some("нагрузка"), Some("кН"), None, false),
        )];
        let reading = partition(PAGE, 12, &cells);
        assert!(reading.structured.is_empty());
        assert_eq!(
            reading.uncertainties[0].kind,
            UncertaintyKind::UnresolvedSubject
        );
    }

    #[test]
    fn only_a_page_worth_of_structured_rows_reaches_the_prompt_and_the_rest_is_counted() {
        let cells: Vec<TableCell> = (100..200)
            .map(|id| {
                cell(
                    id,
                    2,
                    &format!("{id}"),
                    CellUsability::Usable,
                    &[],
                    context(Some("BP21"), Some("нагрузка"), Some("кН"), None, false),
                )
            })
            .collect();
        let reading = partition(PAGE, 12, &cells);
        let (lines, omitted) = reading.prompt_lines();
        assert_eq!(lines.len(), MAX_STRUCTURED_ROWS_PER_PAGE);
        assert_eq!(omitted, 100 - MAX_STRUCTURED_ROWS_PER_PAGE);
    }

    // --- confirmation ----------------------------------------------------------------

    fn structured(subject: &str, property: &str, value: &str) -> StructuredCell {
        StructuredCell {
            cell_id: Uuid::from_u128(900),
            page_id: PAGE,
            page_number: 12,
            region_id: REGION,
            subject: subject.to_owned(),
            property: property.to_owned(),
            value: value.to_owned(),
            unit: Some("кН".to_owned()),
            conditions: vec!["при опирании на две опоры".to_owned()],
            context_inferred: false,
        }
    }

    #[test]
    fn a_fact_that_matches_a_cell_is_confirmed_with_its_context() {
        let cells = vec![structured("BP21", "безопасная рабочая нагрузка", "3,5")];
        let confirmation =
            confirm_against_cells(Some("bp21"), "нагрузка", "3,5", &cells).expect("confirmed");
        assert_eq!(confirmation.subject, "BP21");
        assert_eq!(confirmation.unit.as_deref(), Some("кН"));
        assert_eq!(confirmation.conditions.len(), 1);
    }

    #[test]
    fn a_different_spelling_of_the_number_is_not_the_same_reading() {
        let cells = vec![structured("BP21", "нагрузка", "3,5")];
        assert!(
            confirm_against_cells(Some("BP21"), "нагрузка", "3.5", &cells).is_none(),
            "3,5 and 3.5 are two readings; matching them would invent a conversion"
        );
    }

    #[test]
    fn a_value_belonging_to_another_product_is_never_confirmed() {
        let cells = vec![structured("BP21", "нагрузка", "3,5")];
        assert!(confirm_against_cells(Some("BP21D"), "нагрузка", "3,5", &cells).is_none());
        // …and a fact about the offering as a whole cannot borrow a product's row.
        assert!(confirm_against_cells(None, "нагрузка", "3,5", &cells).is_none());
    }
}
