//! Attaching a table cell to the thing it describes.
//!
//! [`super::pdf::tables`] recovers a *grid*: which text stood in which row and column.
//! That is geometry. This module adds the only part a consumer can actually reason
//! with — which product a cell belongs to, which property it measures, in which unit, and
//! under which conditions — and, just as importantly, says so when it cannot.
//!
//! The defect this exists to close: `безопасная рабочая нагрузка (Н)` is a *column label*.
//! It reached the knowledge base as a characteristic because a stored cell said nothing
//! about being a label, so the next phase read the page as flat text where the difference
//! does not survive. Here a label is [`CellRole::ColumnHeader`], a header is never a
//! value, and a cell that cannot name its product, property and unit is `unusable` rather
//! than quietly available.
//!
//! Everything is derived from what is written. Where something has to be carried across a
//! merged cell, the origin says so ([`ContextOrigin::InheritedFromMergedHeader`]) instead
//! of presenting an inference as a reading.

use std::collections::BTreeMap;

use otdel_core::extraction::{BoundingBox, CellValueKind, RegionKind};
use otdel_core::extraction_context::{
    AmbiguityReason, CellRole, CellVerdict, ConditionRef, ContextOrigin, ContextRef, SourceSpan,
    StructuralContext, UnitRef,
};

use crate::model::{ExtractedCell, ExtractedRegion, ExtractedTable};
use crate::units;

/// A footnote found on the page, with the marker it is introduced by.
#[derive(Debug, Clone)]
struct PageFootnote {
    marker: Option<String>,
    text: String,
    bbox: Option<BoundingBox>,
}

/// Annotate every table on a page, in place.
///
/// `regions` must be in reading order — the nearest heading *above* a table is what gives
/// its cells a subject when the rows carry no designation of their own.
pub fn annotate_page(regions: &mut [ExtractedRegion], page_number: i32) {
    let footnotes = collect_footnotes(regions);

    // The heading in force at each position, walking the page top to bottom.
    let mut heading: Option<String> = None;
    for region in regions.iter_mut() {
        match region.kind {
            RegionKind::Heading => {
                let text = region.text.trim();
                if !text.is_empty() {
                    heading = Some(collapse(text));
                }
                continue;
            }
            RegionKind::Table => {}
            _ => continue,
        }

        let section = heading.clone();
        if let Some(table) = region.table.as_mut() {
            annotate_table(table, page_number, section.as_deref(), &footnotes);
        }
    }
}

fn collect_footnotes(regions: &[ExtractedRegion]) -> Vec<PageFootnote> {
    regions
        .iter()
        .filter(|region| region.kind == RegionKind::Footnote)
        .flat_map(|region| {
            region.text.lines().filter_map(|line| {
                let line = line.trim();
                if line.is_empty() {
                    return None;
                }
                Some(PageFootnote {
                    marker: leading_marker(line),
                    text: collapse(line),
                    bbox: region.bbox,
                })
            })
        })
        .collect()
}

/// Annotate one table: roles first, then context, then the verdict.
fn annotate_table(
    table: &mut ExtractedTable,
    page_number: i32,
    section: Option<&str>,
    footnotes: &[PageFootnote],
) {
    let header_rows = table.header_rows as usize;
    let label_columns = table.label_columns as usize;

    // Text of every cell by position, so header and label lookups do not depend on the
    // order the cells happen to be stored in.
    let text_at: BTreeMap<(u32, u32), String> = table
        .cells
        .iter()
        .map(|cell| {
            (
                (cell.row_index, cell.column_index),
                cell.raw_text.trim().to_owned(),
            )
        })
        .collect();

    let column_paths: Vec<Vec<ContextRef>> = (0..table.column_count)
        .map(|column| column_header_path(&text_at, header_rows, column))
        .collect();
    let row_paths: BTreeMap<u32, Vec<ContextRef>> = (0..table.row_count)
        .map(|row| (row, row_label_path(&text_at, label_columns, row)))
        .collect();

    for cell in &mut table.cells {
        let role = role_of(cell, header_rows, label_columns);
        cell.role = role;
        cell.is_header = role == CellRole::ColumnHeader;

        let column_path = column_paths
            .get(cell.column_index as usize)
            .cloned()
            .unwrap_or_default();
        let row_path = row_paths.get(&cell.row_index).cloned().unwrap_or_default();

        // A header cell is not described *by* the header band it belongs to.
        let (column_path, row_path) = match role {
            CellRole::ColumnHeader => (Vec::new(), Vec::new()),
            CellRole::RowHeader => (Vec::new(), row_path),
            CellRole::Data => (column_path, row_path),
        };

        let property = innermost_property(&column_path);
        let unit = resolve_unit(&cell.raw_text, &column_path, &row_path);
        let subject = resolve_subject(&row_path, section);
        let (conditions, unresolved_marker) =
            resolve_conditions(cell, &column_path, &row_path, footnotes, page_number);

        // The pre-existing flat fields keep their old meaning for existing consumers.
        cell.unit = unit.as_ref().map(|it| it.unit.clone());
        cell.column_header = column_path.last().map(|it| it.text.clone());

        let context = StructuralContext {
            column_header_path: column_path,
            row_header_path: row_path,
            subject,
            property,
            unit,
            conditions,
        };
        cell.verdict = judge(cell, role, &context, unresolved_marker);
        cell.structural_context = context;
    }
}

/// Annotate a single detached table, for tests that exercise the grid and the semantic
/// layer together without building a whole page around them.
#[cfg(test)]
pub(crate) fn annotate_table_for_tests(table: &mut ExtractedTable) {
    annotate_table(table, 1, None, &[]);
}

fn role_of(cell: &ExtractedCell, header_rows: usize, label_columns: usize) -> CellRole {
    if (cell.row_index as usize) < header_rows {
        CellRole::ColumnHeader
    } else if (cell.column_index as usize) < label_columns {
        CellRole::RowHeader
    } else {
        CellRole::Data
    }
}

/// Labels above a column, outermost first.
///
/// A blank header cell is the signature of a merged label spanning to the right, so the
/// nearest label to its left is carried across — and marked
/// [`ContextOrigin::InheritedFromMergedHeader`], because carrying it is an inference.
fn column_header_path(
    text_at: &BTreeMap<(u32, u32), String>,
    header_rows: usize,
    column: u32,
) -> Vec<ContextRef> {
    let mut path: Vec<ContextRef> = Vec::new();
    for row in 0..header_rows as u32 {
        let own = text_at.get(&(row, column)).filter(|text| !text.is_empty());
        let entry = match own {
            Some(text) => ContextRef::new(collapse(text), ContextOrigin::HeaderRow),
            None => {
                let Some(inherited) = (0..column)
                    .rev()
                    .find_map(|left| text_at.get(&(row, left)).filter(|text| !text.is_empty()))
                else {
                    continue;
                };
                ContextRef::new(
                    collapse(inherited),
                    ContextOrigin::InheritedFromMergedHeader,
                )
            }
        };
        // A label repeated down the band adds nothing.
        if path.last().is_some_and(|last| last.text == entry.text) {
            continue;
        }
        path.push(entry);
    }
    path
}

/// The row's own label cells, left to right, inheriting down a merged designation.
fn row_label_path(
    text_at: &BTreeMap<(u32, u32), String>,
    label_columns: usize,
    row: u32,
) -> Vec<ContextRef> {
    let mut path: Vec<ContextRef> = Vec::new();
    for column in 0..label_columns as u32 {
        let own = text_at.get(&(row, column)).filter(|text| !text.is_empty());
        let entry = match own {
            Some(text) => ContextRef::new(collapse(text), ContextOrigin::RowLabel),
            None => {
                let Some(inherited) = (0..row).rev().find_map(|above| {
                    text_at
                        .get(&(above, column))
                        .filter(|text| !text.is_empty())
                }) else {
                    continue;
                };
                ContextRef::new(
                    collapse(inherited),
                    ContextOrigin::InheritedFromMergedRowLabel,
                )
            }
        };
        if path.last().is_some_and(|last| last.text == entry.text) {
            continue;
        }
        path.push(entry);
    }
    path
}

/// The property a column measures: the innermost label that is not merely a unit.
///
/// For a two-row header `Нагрузка` over `кН`, the property is `Нагрузка` — `кН` is the
/// dimension, and treating it as the name of the characteristic is how a value ends up
/// described by its own unit.
fn innermost_property(column_path: &[ContextRef]) -> Option<ContextRef> {
    column_path
        .iter()
        .rev()
        .find(|entry| units::exact_unit(&entry.text).is_none())
        .cloned()
}

/// The unit, and where it was written. Never guessed from what a column "looks like".
fn resolve_unit(
    raw_text: &str,
    column_path: &[ContextRef],
    row_path: &[ContextRef],
) -> Option<UnitRef> {
    if let Some(unit) = units::trailing_unit(raw_text) {
        return Some(UnitRef {
            unit,
            origin: ContextOrigin::CellItself,
        });
    }
    // Innermost header first: `Нагрузка` / `кН` puts the dimension on the inner row.
    for entry in column_path.iter().rev() {
        if let Some(unit) =
            units::exact_unit(&entry.text).or_else(|| units::header_unit(&entry.text))
        {
            return Some(UnitRef {
                unit,
                origin: entry.origin,
            });
        }
    }
    for entry in row_path.iter().rev() {
        if let Some(unit) = units::header_unit(&entry.text) {
            return Some(UnitRef {
                unit,
                origin: entry.origin,
            });
        }
    }
    None
}

/// The product a cell belongs to: its own row's designation, or failing that the heading
/// the table sits under. Never the row above's designation — see the BP21/BP21D case.
///
/// A row label that reads as a column label is not a product. On the real page a collapsed
/// grid put `безопасная рабочая нагрузка (Н)` into the label column, and taking it as a
/// product identity would have re-created the original defect one field to the left.
fn resolve_subject(row_path: &[ContextRef], section: Option<&str>) -> Option<ContextRef> {
    if let Some(label) = row_path
        .iter()
        .find(|entry| !units::is_header_shaped(&entry.text))
    {
        return Some(label.clone());
    }
    section.map(|text| ContextRef::new(text, ContextOrigin::PageHeading))
}

/// Footnotes that this cell's markers point at.
///
/// Only marker matches are attached. A footnote printed under a table very often applies
/// to all of it, but "very often" is not evidence, and silently attaching conditions to
/// values they may not govern is the same class of error as inventing a unit.
fn resolve_conditions(
    cell: &ExtractedCell,
    column_path: &[ContextRef],
    row_path: &[ContextRef],
    footnotes: &[PageFootnote],
    page_number: i32,
) -> (Vec<ConditionRef>, bool) {
    let mut markers: Vec<String> = Vec::new();
    for text in std::iter::once(cell.raw_text.as_str())
        .chain(column_path.iter().map(|it| it.text.as_str()))
        .chain(row_path.iter().map(|it| it.text.as_str()))
    {
        for marker in trailing_markers(text) {
            if !markers.contains(&marker) {
                markers.push(marker);
            }
        }
    }

    let mut conditions = Vec::new();
    let mut unresolved = false;
    for marker in markers {
        match footnotes
            .iter()
            .find(|note| note.marker.as_deref() == Some(marker.as_str()))
        {
            Some(note) => conditions.push(ConditionRef {
                text: note.text.clone(),
                marker: Some(marker),
                span: SourceSpan::from_bbox(
                    page_number,
                    note.bbox,
                    "сноска найдена в тексте, но её область на странице не определена",
                ),
            }),
            None => unresolved = true,
        }
    }
    (conditions, unresolved)
}

/// The verdict, built only from reasons — so the two can never disagree.
fn judge(
    cell: &ExtractedCell,
    role: CellRole,
    context: &StructuralContext,
    unresolved_marker: bool,
) -> CellVerdict {
    if role.is_header() {
        // The reported defect, closed at its source: a label is not a measurement, and
        // there is no combination of other facts that can make it one.
        return CellVerdict::from_reasons([AmbiguityReason::HeaderIsNotAValue]);
    }

    let mut reasons = Vec::new();

    if cell.value_kind == CellValueKind::Empty {
        reasons.push(AmbiguityReason::BlankCell);
    }
    // A mis-detected grid can drop a column label into a body row. Text shaped like
    // `слова (единица)` is a label wherever it lands.
    if units::is_header_shaped(&cell.raw_text) {
        reasons.push(AmbiguityReason::HeaderShapedText);
    }
    if units::holds_several_values(&cell.raw_text) {
        reasons.push(AmbiguityReason::MultipleValuesInOneCell);
    }
    if context.column_header_path.is_empty() {
        reasons.push(AmbiguityReason::NoColumnHeader);
    }
    if context.subject.is_none() {
        reasons.push(AmbiguityReason::NoRowContext);
    }
    if cell.value_kind == CellValueKind::Number && context.unit.is_none() {
        reasons.push(AmbiguityReason::UnitUnresolved);
    }
    if unresolved_marker {
        reasons.push(AmbiguityReason::ConditionUnresolved);
    }
    if context.has_inferred_context() {
        reasons.push(AmbiguityReason::ContextInheritedFromMergedCell);
    }

    CellVerdict::from_reasons(reasons)
}

/// Footnote markers a piece of text carries, e.g. the `**` in `3,5**`.
fn trailing_markers(text: &str) -> Vec<String> {
    let trimmed = text.trim_end();
    let mut stars = String::new();
    for ch in trimmed.chars().rev() {
        if ch == '*' {
            stars.insert(0, ch);
        } else {
            break;
        }
    }
    if stars.is_empty() {
        Vec::new()
    } else {
        vec![stars]
    }
}

/// The marker a footnote line opens with, when it opens with one.
fn leading_marker(line: &str) -> Option<String> {
    let stars: String = line.chars().take_while(|ch| *ch == '*').collect();
    (!stars.is_empty()).then_some(stars)
}

/// One line, single spaces. Context is shown next to a value; it must not drag a
/// line break or a run of padding into the interface.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use otdel_core::extraction_context::CellUsability;

    /// Build a table from `&[&[&str]]`, with the bands stated explicitly.
    fn table(rows: &[&[&str]], header_rows: u32, label_columns: u32) -> ExtractedTable {
        let column_count = rows.iter().map(|row| row.len()).max().unwrap_or(0) as u32;
        let mut cells = Vec::new();
        for (row_index, row) in rows.iter().enumerate() {
            for column_index in 0..column_count {
                let raw = row
                    .get(column_index as usize)
                    .copied()
                    .unwrap_or("")
                    .to_owned();
                let kind = units::classify(&raw);
                cells.push(ExtractedCell::unannotated(
                    row_index as u32,
                    column_index,
                    raw,
                    kind,
                    Some(BoundingBox::new(
                        f64::from(column_index) * 100.0,
                        700.0 - row_index as f64 * 20.0,
                        f64::from(column_index) * 100.0 + 90.0,
                        710.0 - row_index as f64 * 20.0,
                    )),
                ));
            }
        }
        ExtractedTable {
            row_count: rows.len() as u32,
            column_count,
            cells,
            header_rows,
            label_columns,
        }
    }

    fn cell(table: &ExtractedTable, row: u32, column: u32) -> &ExtractedCell {
        table
            .cells
            .iter()
            .find(|cell| cell.row_index == row && cell.column_index == column)
            .expect("cell exists")
    }

    fn annotate(table: &mut ExtractedTable, section: Option<&str>, footnotes: &[PageFootnote]) {
        annotate_table(table, 7, section, footnotes);
    }

    #[test]
    fn a_column_label_is_a_header_and_can_never_be_a_value() {
        // Exactly the published defect: the load column's label.
        let mut grid = table(
            &[
                &["Профиль", "безопасная рабочая нагрузка (Н)"],
                &["BP21", "3500"],
                &["BP21D", "4200"],
            ],
            1,
            1,
        );
        annotate(&mut grid, None, &[]);

        let label = cell(&grid, 0, 1);
        assert_eq!(label.raw_text, "безопасная рабочая нагрузка (Н)");
        assert_eq!(label.role, CellRole::ColumnHeader);
        assert_eq!(label.verdict.usability, CellUsability::Unusable);
        assert!(!label.verdict.is_candidate_value());
        assert!(label
            .verdict
            .reasons
            .contains(&AmbiguityReason::HeaderIsNotAValue));
    }

    #[test]
    fn the_same_label_is_still_not_a_value_when_the_grid_misplaces_it() {
        // On the real page the grid collapses and the label lands in a body row. It must
        // not become a candidate value just because the geometry went wrong.
        let mut grid = table(
            &[
                &["Длина", "безопасная рабочая нагрузка (Н)"],
                &["профиля", "толщина металла 2 / 2.5"],
                &["250", "2074"],
            ],
            0,
            0,
        );
        annotate(&mut grid, None, &[]);

        let stray = cell(&grid, 0, 1);
        assert_eq!(
            stray.role,
            CellRole::Data,
            "the grid did place it in a body row"
        );
        assert_eq!(stray.verdict.usability, CellUsability::Unusable);
        assert!(stray
            .verdict
            .reasons
            .contains(&AmbiguityReason::HeaderShapedText));
    }

    #[test]
    fn a_fully_attributed_cell_exposes_product_property_value_and_unit_separately() {
        let mut grid = table(
            &[
                &["Профиль", "Длина, мм", "Нагрузка, кН"],
                &["BP21", "1200", "3,5"],
                &["BP21D", "1500", "4,2"],
            ],
            1,
            1,
        );
        annotate(&mut grid, Some("Профили BASIS"), &[]);

        let load = cell(&grid, 2, 2);
        let context = &load.structural_context;
        assert_eq!(load.raw_text, "4,2", "the value stays verbatim");
        assert_eq!(load.value_kind, CellValueKind::Number);
        assert_eq!(context.subject.as_ref().unwrap().text, "BP21D");
        assert_eq!(
            context.subject.as_ref().unwrap().origin,
            ContextOrigin::RowLabel
        );
        assert_eq!(context.property.as_ref().unwrap().text, "Нагрузка, кН");
        assert_eq!(context.unit.as_ref().unwrap().unit, "кН");
        assert_eq!(load.verdict.usability, CellUsability::Usable);
        assert!(load.verdict.is_candidate_value());
    }

    #[test]
    fn a_two_row_header_puts_the_property_and_its_unit_in_different_places() {
        let mut grid = table(
            &[
                &["Профиль", "Безопасная рабочая нагрузка", ""],
                &["", "кН", "кН"],
                &["BP21", "3,5", "4,1"],
            ],
            2,
            1,
        );
        annotate(&mut grid, None, &[]);

        let value = cell(&grid, 2, 1);
        let context = &value.structural_context;
        assert_eq!(
            context
                .column_header_path
                .iter()
                .map(|it| it.text.as_str())
                .collect::<Vec<_>>(),
            vec!["Безопасная рабочая нагрузка", "кН"]
        );
        // The property is the label, not the dimension.
        assert_eq!(
            context.property.as_ref().unwrap().text,
            "Безопасная рабочая нагрузка"
        );
        assert_eq!(context.unit.as_ref().unwrap().unit, "кН");

        // The third column's label was merged: it is carried across, and marked as such.
        let merged = cell(&grid, 2, 2);
        assert_eq!(
            merged.structural_context.column_header_path[0].origin,
            ContextOrigin::InheritedFromMergedHeader
        );
        assert_eq!(merged.verdict.usability, CellUsability::Ambiguous);
        assert!(merged
            .verdict
            .reasons
            .contains(&AmbiguityReason::ContextInheritedFromMergedCell));
    }

    #[test]
    fn lookalike_designations_never_borrow_each_others_rows() {
        let mut grid = table(
            &[
                &["Профиль", "Нагрузка, кН"],
                &["BP21", "3,5"],
                &["BP21D", "4,2"],
            ],
            1,
            1,
        );
        annotate(&mut grid, None, &[]);

        assert_eq!(
            cell(&grid, 1, 1)
                .structural_context
                .subject
                .as_ref()
                .unwrap()
                .text,
            "BP21"
        );
        assert_eq!(
            cell(&grid, 2, 1)
                .structural_context
                .subject
                .as_ref()
                .unwrap()
                .text,
            "BP21D"
        );
    }

    #[test]
    fn a_designation_carried_down_a_merged_row_label_is_marked_as_inferred() {
        let mut grid = table(
            &[
                &["Профиль", "Длина, мм", "Нагрузка, кН"],
                &["BP21", "1200", "3,5"],
                &["", "1500", "4,2"],
            ],
            1,
            1,
        );
        annotate(&mut grid, None, &[]);

        let inherited = cell(&grid, 2, 2);
        let subject = inherited.structural_context.subject.as_ref().unwrap();
        assert_eq!(subject.text, "BP21");
        assert_eq!(subject.origin, ContextOrigin::InheritedFromMergedRowLabel);
        // Right nine times out of ten — and still not something to publish unreviewed.
        assert_eq!(inherited.verdict.usability, CellUsability::Ambiguous);
    }

    #[test]
    fn a_number_with_no_unit_anywhere_is_ambiguous_not_usable() {
        let mut grid = table(
            &[&["Профиль", "Длина"], &["BP21", "1200"], &["BP21D", "1500"]],
            1,
            1,
        );
        annotate(&mut grid, None, &[]);

        let value = cell(&grid, 1, 1);
        assert!(value.structural_context.unit.is_none());
        assert_eq!(value.verdict.usability, CellUsability::Ambiguous);
        assert!(value
            .verdict
            .reasons
            .contains(&AmbiguityReason::UnitUnresolved));
    }

    #[test]
    fn a_blank_cell_is_unusable_and_never_a_zero() {
        let mut grid = table(
            &[
                &["Профиль", "Нагрузка, кН"],
                &["BP21", "3,5"],
                &["BP21D", ""],
            ],
            1,
            1,
        );
        annotate(&mut grid, None, &[]);

        let blank = cell(&grid, 2, 1);
        assert_eq!(blank.raw_text, "");
        assert_eq!(blank.value_kind, CellValueKind::Empty);
        assert_eq!(blank.verdict.usability, CellUsability::Unusable);
        assert!(blank.verdict.reasons.contains(&AmbiguityReason::BlankCell));
    }

    #[test]
    fn a_cell_holding_several_values_is_unusable_until_a_scheme_is_chosen() {
        // The real load tables print three values per cell, one per loading scheme.
        let mut grid = table(
            &[
                &["Длина, мм", "Нагрузка, кН"],
                &["1500", "4860 / 8470 / 12720"],
                &["2000", "2350 / 6361 / 10424"],
            ],
            1,
            1,
        );
        annotate(&mut grid, None, &[]);

        let triple = cell(&grid, 1, 1);
        assert_eq!(triple.raw_text, "4860 / 8470 / 12720");
        assert_eq!(triple.verdict.usability, CellUsability::Unusable);
        assert!(triple
            .verdict
            .reasons
            .contains(&AmbiguityReason::MultipleValuesInOneCell));
    }

    #[test]
    fn a_footnote_marker_brings_its_condition_with_the_value() {
        let footnotes = vec![PageFootnote {
            marker: Some("**".to_owned()),
            text: "** при схеме опирания по двум краям и толщине металла 2 мм".to_owned(),
            bbox: Some(BoundingBox::new(40.0, 60.0, 500.0, 75.0)),
        }];
        let mut grid = table(
            &[
                &["Профиль", "Нагрузка, кН**"],
                &["BP21", "3,5"],
                &["BP21D", "4,2"],
            ],
            1,
            1,
        );
        annotate(&mut grid, None, &footnotes);

        let value = cell(&grid, 1, 1);
        let conditions = &value.structural_context.conditions;
        assert_eq!(conditions.len(), 1);
        assert!(conditions[0].text.contains("схеме опирания"));
        assert_eq!(conditions[0].marker.as_deref(), Some("**"));
        assert!(conditions[0].span.is_highlightable());
        assert_eq!(value.verdict.usability, CellUsability::Usable);
    }

    #[test]
    fn a_marker_whose_footnote_is_missing_leaves_the_value_doubtful() {
        let mut grid = table(
            &[
                &["Профиль", "Нагрузка, кН*"],
                &["BP21", "3,5"],
                &["BP21D", "4,2"],
            ],
            1,
            1,
        );
        annotate(&mut grid, None, &[]);

        let value = cell(&grid, 1, 1);
        assert!(value.structural_context.conditions.is_empty());
        assert_eq!(value.verdict.usability, CellUsability::Ambiguous);
        assert!(value
            .verdict
            .reasons
            .contains(&AmbiguityReason::ConditionUnresolved));
    }

    #[test]
    fn a_cell_with_no_provable_header_is_unusable_however_numeric_it_looks() {
        // Stray digits from a load diagram: the real page is full of them.
        let mut grid = table(&[&["", "5"], &["", "2"], &["", "0"]], 0, 0);
        annotate(&mut grid, None, &[]);

        let stray = cell(&grid, 0, 1);
        assert_eq!(stray.value_kind, CellValueKind::Number);
        assert_eq!(stray.verdict.usability, CellUsability::Unusable);
        assert!(stray
            .verdict
            .reasons
            .contains(&AmbiguityReason::NoColumnHeader));
        assert!(stray
            .verdict
            .reasons
            .contains(&AmbiguityReason::NoRowContext));
    }

    #[test]
    fn a_column_label_that_lands_in_the_label_column_is_not_taken_as_a_product() {
        // Straight from the real page: the grid collapses and the load label ends up in
        // the row-label column. It must not become the product identity of its row.
        let mut grid = table(
            &[
                &["Длина", "250"],
                &["безопасная рабочая нагрузка (Н)", "2074"],
            ],
            0,
            1,
        );
        annotate(&mut grid, Some("Профиль BP21"), &[]);

        let value = cell(&grid, 1, 1);
        let subject = value.structural_context.subject.as_ref().unwrap();
        assert_ne!(subject.text, "безопасная рабочая нагрузка (Н)");
        assert_eq!(subject.text, "Профиль BP21");
        assert_eq!(subject.origin, ContextOrigin::PageHeading);

        // And the label cell itself is still not a value.
        assert_eq!(cell(&grid, 1, 0).verdict.usability, CellUsability::Unusable);
    }

    #[test]
    fn a_heading_above_the_table_names_the_product_when_the_rows_do_not() {
        let mut grid = table(
            &[
                &["Длина, мм", "Нагрузка, кН"],
                &["1200", "3,5"],
                &["1500", "4,2"],
            ],
            1,
            0,
        );
        annotate(&mut grid, Some("Профиль BP21D"), &[]);

        let subject = cell(&grid, 1, 1)
            .structural_context
            .subject
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(subject.text, "Профиль BP21D");
        assert_eq!(subject.origin, ContextOrigin::PageHeading);
    }

    #[test]
    fn annotate_page_uses_the_nearest_heading_above_each_table() {
        let mut regions = vec![
            ExtractedRegion {
                kind: RegionKind::Heading,
                text: "Профиль BP21".to_owned(),
                bbox: None,
                table: None,
            },
            ExtractedRegion {
                kind: RegionKind::Table,
                text: String::new(),
                bbox: None,
                table: Some(table(
                    &[
                        &["Длина, мм", "Нагрузка, кН"],
                        &["1200", "3,5"],
                        &["1500", "4,2"],
                    ],
                    1,
                    0,
                )),
            },
            ExtractedRegion {
                kind: RegionKind::Heading,
                text: "Профиль BP21D".to_owned(),
                bbox: None,
                table: None,
            },
            ExtractedRegion {
                kind: RegionKind::Table,
                text: String::new(),
                bbox: None,
                table: Some(table(
                    &[
                        &["Длина, мм", "Нагрузка, кН"],
                        &["1200", "4,0"],
                        &["1500", "4,8"],
                    ],
                    1,
                    0,
                )),
            },
        ];
        annotate_page(&mut regions, 3);

        let first = regions[1].table.as_ref().unwrap();
        let second = regions[3].table.as_ref().unwrap();
        assert_eq!(
            cell(first, 1, 1)
                .structural_context
                .subject
                .as_ref()
                .unwrap()
                .text,
            "Профиль BP21"
        );
        assert_eq!(
            cell(second, 1, 1)
                .structural_context
                .subject
                .as_ref()
                .unwrap()
                .text,
            "Профиль BP21D"
        );
    }
}
