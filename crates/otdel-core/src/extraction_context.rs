//! Structural context of a table cell, and the verdict on whether it may be read as a
//! value at all.
//!
//! Phase 1B already kept a cell's verbatim text. That turned out not to be enough. In the
//! real catalogue the string `безопасная рабочая нагрузка (Н)` — a *column label* — was
//! read as page text by the next phase and published as a characteristic. Nothing in the
//! stored shape of a cell said "this is a label", and nothing said which product, which
//! property, which unit or which loading scheme a number belonged to, so a consumer had
//! no choice but to fall back on the flat page text where all of that is lost.
//!
//! This module adds the missing half. Every cell carries:
//!
//! * a [`CellRole`] — a header is a header, and a header is never a value;
//! * a [`StructuralContext`] — the column header path (including multi-row headers), the
//!   row's own label, the product/section it sits under, the unit *and where the unit was
//!   written*, and the footnote conditions that apply;
//! * a [`CellVerdict`] — `usable`, `ambiguous` or `unusable`, with the reasons spelled
//!   out.
//!
//! Two rules govern the whole module and are worth stating before the types:
//!
//! 1. **No invented certainty.** [`CellUsability`] is a category the parser can defend,
//!    not a number. There is deliberately no `confidence: f64` anywhere here: the text
//!    extractor has no measurement that would justify one, and a fabricated `0.87` is
//!    worse than an honest "ambiguous, because the unit is not written anywhere".
//! 2. **Every piece of context names its origin.** A header that was inherited across a
//!    merged cell is marked as inherited ([`ContextOrigin::InheritedFromMergedHeader`]),
//!    because that inheritance is an inference and a reader is entitled to know.

use serde::{Deserialize, Serialize};

use crate::extraction::BoundingBox;

/// What a cell *is* within its table. Distinct from the value it carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellRole {
    /// Part of the table's header band: it names a column, it is not a measurement.
    ColumnHeader,
    /// The label cell of its row — usually the product designation.
    RowHeader,
    /// An ordinary body cell.
    Data,
}

impl CellRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ColumnHeader => "column_header",
            Self::RowHeader => "row_header",
            Self::Data => "data",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "column_header" => Some(Self::ColumnHeader),
            "row_header" => Some(Self::RowHeader),
            "data" => Some(Self::Data),
            _ => None,
        }
    }

    /// Header cells are labels. A label can never be offered as the value of anything.
    pub const fn is_header(self) -> bool {
        matches!(self, Self::ColumnHeader | Self::RowHeader)
    }
}

/// How far a cell may be trusted as a value. A category, never a score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellUsability {
    /// Product, property, value and unit are all present and attributed.
    Usable,
    /// Something needed is missing or was inferred. A consumer may show it, but must not
    /// turn it into a confirmed characteristic without resolving the reasons.
    Ambiguous,
    /// This cell is not a value at all, or cannot be read as one. It must never appear as
    /// a candidate value.
    Unusable,
}

impl CellUsability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Usable => "usable",
            Self::Ambiguous => "ambiguous",
            Self::Unusable => "unusable",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "usable" => Some(Self::Usable),
            "ambiguous" => Some(Self::Ambiguous),
            "unusable" => Some(Self::Unusable),
            _ => None,
        }
    }

    /// May a consumer propose this cell as the value of a characteristic?
    pub const fn is_candidate_value(self) -> bool {
        matches!(self, Self::Usable)
    }

    /// The stricter of two verdicts. Reasons accumulate; the verdict only ever worsens.
    pub fn worst(self, other: Self) -> Self {
        if self >= other {
            self
        } else {
            other
        }
    }
}

/// Why a cell is not plainly usable. Every reason is something a person can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AmbiguityReason {
    /// The cell belongs to the header band. This is the reason that closes the reported
    /// defect: `безопасная рабочая нагрузка (Н)` is a column label.
    HeaderIsNotAValue,
    /// The text reads like a label — words followed by a unit in brackets — even though
    /// the grid did not place it in a header row. A mis-detected grid must not turn a
    /// label into a measurement.
    HeaderShapedText,
    /// Blank in the source. Blank stays blank; it never becomes a zero.
    BlankCell,
    /// The cell holds several values at once (`4860 / 8470 / 12720`). Which one applies
    /// depends on a loading scheme that the cell alone does not state.
    MultipleValuesInOneCell,
    /// No column header could be proven for this column, so the property is unnamed.
    NoColumnHeader,
    /// No row label could be proven, so the cell is not attached to a product.
    NoRowContext,
    /// A number with no unit written in the cell, its column header or its row label.
    UnitUnresolved,
    /// The cell carries a footnote marker whose footnote was not found on the page.
    ConditionUnresolved,
    /// The product/property was carried over from a cell above or to the left across a
    /// merged (blank) cell. Usually right, but it is an inference — and for lookalike
    /// designations such as BP21 and BP21D, guessing is exactly what must not happen.
    ContextInheritedFromMergedCell,
}

impl AmbiguityReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HeaderIsNotAValue => "header_is_not_a_value",
            Self::HeaderShapedText => "header_shaped_text",
            Self::BlankCell => "blank_cell",
            Self::MultipleValuesInOneCell => "multiple_values_in_one_cell",
            Self::NoColumnHeader => "no_column_header",
            Self::NoRowContext => "no_row_context",
            Self::UnitUnresolved => "unit_unresolved",
            Self::ConditionUnresolved => "condition_unresolved",
            Self::ContextInheritedFromMergedCell => "context_inherited_from_merged_cell",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "header_is_not_a_value" => Some(Self::HeaderIsNotAValue),
            "header_shaped_text" => Some(Self::HeaderShapedText),
            "blank_cell" => Some(Self::BlankCell),
            "multiple_values_in_one_cell" => Some(Self::MultipleValuesInOneCell),
            "no_column_header" => Some(Self::NoColumnHeader),
            "no_row_context" => Some(Self::NoRowContext),
            "unit_unresolved" => Some(Self::UnitUnresolved),
            "condition_unresolved" => Some(Self::ConditionUnresolved),
            "context_inherited_from_merged_cell" => Some(Self::ContextInheritedFromMergedCell),
            _ => None,
        }
    }

    /// Reasons that make a cell not a value at all, as opposed to a doubtful one.
    pub const fn is_disqualifying(self) -> bool {
        matches!(
            self,
            Self::HeaderIsNotAValue
                | Self::HeaderShapedText
                | Self::BlankCell
                | Self::MultipleValuesInOneCell
                | Self::NoColumnHeader
                | Self::NoRowContext
        )
    }

    pub const fn describe(self) -> &'static str {
        match self {
            Self::HeaderIsNotAValue => "это заголовок таблицы, а не значение",
            Self::HeaderShapedText => "текст выглядит как подпись столбца, а не как величина",
            Self::BlankCell => "в источнике ячейка пуста",
            Self::MultipleValuesInOneCell => {
                "в ячейке несколько значений сразу — какое применимо, из неё не следует"
            }
            Self::NoColumnHeader => "для столбца не удалось доказать заголовок",
            Self::NoRowContext => "строка не привязана к изделию",
            Self::UnitUnresolved => "единица измерения нигде не написана",
            Self::ConditionUnresolved => "сноска указана, но её текст не найден на странице",
            Self::ContextInheritedFromMergedCell => {
                "контекст перенесён из объединённой ячейки — это предположение"
            }
        }
    }
}

/// Where a piece of context was written. Kept so that an inference is never mistaken for
/// something that stood in the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextOrigin {
    /// Written in the cell itself.
    CellItself,
    /// Written in the table's header band, directly above this column.
    HeaderRow,
    /// The header cell above this column was blank and the label to its left was carried
    /// across — the usual shape of a merged header. An inference, and marked as one.
    InheritedFromMergedHeader,
    /// Written in the row's own label column.
    RowLabel,
    /// The row's label cell was blank and the label above was carried down.
    InheritedFromMergedRowLabel,
    /// Written in a heading on the page, above the table.
    PageHeading,
    /// Written in a footnote on the page.
    Footnote,
}

impl ContextOrigin {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CellItself => "cell_itself",
            Self::HeaderRow => "header_row",
            Self::InheritedFromMergedHeader => "inherited_from_merged_header",
            Self::RowLabel => "row_label",
            Self::InheritedFromMergedRowLabel => "inherited_from_merged_row_label",
            Self::PageHeading => "page_heading",
            Self::Footnote => "footnote",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "cell_itself" => Some(Self::CellItself),
            "header_row" => Some(Self::HeaderRow),
            "inherited_from_merged_header" => Some(Self::InheritedFromMergedHeader),
            "row_label" => Some(Self::RowLabel),
            "inherited_from_merged_row_label" => Some(Self::InheritedFromMergedRowLabel),
            "page_heading" => Some(Self::PageHeading),
            "footnote" => Some(Self::Footnote),
            _ => None,
        }
    }

    /// Was this carried across a blank cell rather than read where it stands?
    pub const fn is_inferred(self) -> bool {
        matches!(
            self,
            Self::InheritedFromMergedHeader | Self::InheritedFromMergedRowLabel
        )
    }
}

/// Whether the source coordinates of something are known.
///
/// An OCR engine that returns text without word boxes produces `Unavailable` with the
/// reason written out. It never produces a rectangle covering the whole page, which would
/// look like evidence and point at nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum GeometryState {
    /// The rectangle is the one the parser measured.
    Exact,
    /// No rectangle. The reason is shown to the user instead of a highlight.
    Unavailable { reason: String },
}

impl GeometryState {
    pub fn is_exact(&self) -> bool {
        matches!(self, Self::Exact)
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Exact => None,
            Self::Unavailable { reason } => Some(reason),
        }
    }
}

/// Exactly where on which page something was read, or why that cannot be said.
///
/// `bbox` and `geometry` agree by construction: use [`SourceSpan::located`] and
/// [`SourceSpan::unlocated`] rather than building one by hand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceSpan {
    pub page_number: i32,
    pub bbox: Option<BoundingBox>,
    #[serde(flatten)]
    pub geometry: GeometryState,
}

impl SourceSpan {
    /// A span with measured coordinates. A rectangle that is not usable as a highlight is
    /// refused rather than stored as a misleading one.
    pub fn located(page_number: i32, bbox: BoundingBox) -> Self {
        if bbox.is_usable() {
            Self {
                page_number,
                bbox: Some(bbox),
                geometry: GeometryState::Exact,
            }
        } else {
            Self::unlocated(page_number, "координаты области непригодны для показа")
        }
    }

    pub fn unlocated(page_number: i32, reason: impl Into<String>) -> Self {
        Self {
            page_number,
            bbox: None,
            geometry: GeometryState::Unavailable {
                reason: reason.into(),
            },
        }
    }

    /// Build from an optional rectangle, naming the reason when there is none.
    pub fn from_bbox(page_number: i32, bbox: Option<BoundingBox>, reason: &str) -> Self {
        match bbox {
            Some(bbox) => Self::located(page_number, bbox),
            None => Self::unlocated(page_number, reason),
        }
    }

    /// Can this span be drawn on the page?
    pub fn is_highlightable(&self) -> bool {
        self.bbox.is_some() && self.geometry.is_exact()
    }
}

/// A piece of context with the text that was written and where it came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextRef {
    /// Verbatim, exactly as it stands in the source.
    pub text: String,
    pub origin: ContextOrigin,
}

impl ContextRef {
    pub fn new(text: impl Into<String>, origin: ContextOrigin) -> Self {
        Self {
            text: text.into(),
            origin,
        }
    }
}

/// A unit, and where it was written. A unit with no origin cannot exist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnitRef {
    pub unit: String,
    pub origin: ContextOrigin,
}

/// A condition that qualifies a value: a footnote, with the marker that pointed at it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionRef {
    /// The footnote's verbatim text.
    pub text: String,
    /// The marker as printed in the cell (`*`, `**`, `1)`), when there was one.
    pub marker: Option<String>,
    pub span: SourceSpan,
}

/// Everything a consumer needs in order to say what a cell means — or to conclude that it
/// cannot say.
///
/// The fields are deliberately separate. "Product identity", "property", "value", "unit"
/// and "conditions" are different questions with different evidence, and collapsing them
/// into one string is how a column label became a characteristic in the first place.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StructuralContext {
    /// Column labels from the outermost header row inwards. `["Нагрузка", "кН"]` for a
    /// two-row header. Empty when no header band could be proven.
    pub column_header_path: Vec<ContextRef>,
    /// The row's own label cells, left to right.
    pub row_header_path: Vec<ContextRef>,
    /// The product or section this cell belongs to: the row label when there is one,
    /// otherwise the nearest heading above the table.
    pub subject: Option<ContextRef>,
    /// The property being measured — the innermost column label.
    pub property: Option<ContextRef>,
    pub unit: Option<UnitRef>,
    /// Footnote conditions that apply to this cell.
    pub conditions: Vec<ConditionRef>,
}

impl StructuralContext {
    /// Are product, property and unit all named?
    pub fn is_complete(&self) -> bool {
        self.subject.is_some() && self.property.is_some() && self.unit.is_some()
    }

    /// Context that was carried across a merged cell rather than read in place.
    pub fn has_inferred_context(&self) -> bool {
        self.subject
            .as_ref()
            .is_some_and(|it| it.origin.is_inferred())
            || self
                .property
                .as_ref()
                .is_some_and(|it| it.origin.is_inferred())
            || self
                .column_header_path
                .iter()
                .any(|it| it.origin.is_inferred())
            || self
                .row_header_path
                .iter()
                .any(|it| it.origin.is_inferred())
    }
}

/// The parser's judgement on a cell, with its reasons.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellVerdict {
    pub usability: CellUsability,
    /// Sorted and deduplicated, so the same cell always reports the same list.
    pub reasons: Vec<AmbiguityReason>,
}

impl Default for CellVerdict {
    fn default() -> Self {
        Self {
            usability: CellUsability::Usable,
            reasons: Vec::new(),
        }
    }
}

impl CellVerdict {
    /// Derive the verdict from the reasons, so the two can never disagree.
    ///
    /// * any disqualifying reason → `unusable`;
    /// * any other reason → `ambiguous`;
    /// * none → `usable`.
    pub fn from_reasons(reasons: impl IntoIterator<Item = AmbiguityReason>) -> Self {
        let mut reasons: Vec<AmbiguityReason> = reasons.into_iter().collect();
        reasons.sort_unstable();
        reasons.dedup();

        let usability = if reasons
            .iter()
            .copied()
            .any(AmbiguityReason::is_disqualifying)
        {
            CellUsability::Unusable
        } else if reasons.is_empty() {
            CellUsability::Usable
        } else {
            CellUsability::Ambiguous
        };

        Self { usability, reasons }
    }

    pub fn is_candidate_value(&self) -> bool {
        self.usability.is_candidate_value()
    }

    /// The reasons in words, for a person reading the source panel.
    pub fn describe(&self) -> Vec<&'static str> {
        self.reasons
            .iter()
            .copied()
            .map(AmbiguityReason::describe)
            .collect()
    }
}

/// How far a drawing or diagram on a page has been understood.
///
/// Recognising the *letters* around a load diagram is not understanding the diagram, and
/// this enum exists so the difference is recorded rather than implied. Nothing in phase 1B
/// may report anything but `NotAttempted` for a page that has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagramInterpretation {
    /// The page carries no vector drawing.
    None,
    /// The page carries a drawing and nothing has tried to interpret it. Any text picked
    /// up near it is text, not a reading of the diagram.
    NotAttempted,
}

impl DiagramInterpretation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::NotAttempted => "not_attempted",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "not_attempted" => Some(Self::NotAttempted),
            _ => None,
        }
    }

    /// Never true in this phase. Present so that a later phase that really does interpret
    /// diagrams has to add a variant here, rather than quietly reusing `NotAttempted`.
    pub const fn is_understood(self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_can_never_be_a_candidate_value() {
        let verdict = CellVerdict::from_reasons([AmbiguityReason::HeaderIsNotAValue]);
        assert_eq!(verdict.usability, CellUsability::Unusable);
        assert!(!verdict.is_candidate_value());
    }

    #[test]
    fn a_missing_unit_makes_a_cell_doubtful_not_unusable() {
        let verdict = CellVerdict::from_reasons([AmbiguityReason::UnitUnresolved]);
        assert_eq!(verdict.usability, CellUsability::Ambiguous);
        assert!(!verdict.is_candidate_value());
    }

    #[test]
    fn a_fully_attributed_cell_is_usable() {
        let verdict = CellVerdict::from_reasons([]);
        assert_eq!(verdict.usability, CellUsability::Usable);
        assert!(verdict.is_candidate_value());
    }

    #[test]
    fn reasons_are_sorted_deduplicated_and_never_disagree_with_the_verdict() {
        let verdict = CellVerdict::from_reasons([
            AmbiguityReason::UnitUnresolved,
            AmbiguityReason::BlankCell,
            AmbiguityReason::UnitUnresolved,
        ]);
        assert_eq!(
            verdict.reasons,
            vec![AmbiguityReason::BlankCell, AmbiguityReason::UnitUnresolved]
        );
        // A disqualifying reason wins over a merely doubtful one.
        assert_eq!(verdict.usability, CellUsability::Unusable);
    }

    #[test]
    fn an_unlocated_span_says_why_instead_of_inventing_a_rectangle() {
        let span = SourceSpan::from_bbox(4, None, "движок распознавания не выдаёт координаты");
        assert!(span.bbox.is_none());
        assert!(!span.is_highlightable());
        assert_eq!(
            span.geometry.reason(),
            Some("движок распознавания не выдаёт координаты")
        );
    }

    #[test]
    fn a_degenerate_rectangle_is_refused_rather_than_highlighted() {
        let span = SourceSpan::located(
            1,
            BoundingBox {
                x0: f64::NAN,
                y0: 0.0,
                x1: 1.0,
                y1: 1.0,
            },
        );
        assert!(span.bbox.is_none());
        assert!(!span.is_highlightable());
    }

    #[test]
    fn every_enum_round_trips_through_its_string_form() {
        for role in [CellRole::ColumnHeader, CellRole::RowHeader, CellRole::Data] {
            assert_eq!(CellRole::parse(role.as_str()), Some(role));
        }
        for usability in [
            CellUsability::Usable,
            CellUsability::Ambiguous,
            CellUsability::Unusable,
        ] {
            assert_eq!(CellUsability::parse(usability.as_str()), Some(usability));
        }
        for reason in [
            AmbiguityReason::HeaderIsNotAValue,
            AmbiguityReason::HeaderShapedText,
            AmbiguityReason::BlankCell,
            AmbiguityReason::MultipleValuesInOneCell,
            AmbiguityReason::NoColumnHeader,
            AmbiguityReason::NoRowContext,
            AmbiguityReason::UnitUnresolved,
            AmbiguityReason::ConditionUnresolved,
            AmbiguityReason::ContextInheritedFromMergedCell,
        ] {
            assert_eq!(AmbiguityReason::parse(reason.as_str()), Some(reason));
        }
        for origin in [
            ContextOrigin::CellItself,
            ContextOrigin::HeaderRow,
            ContextOrigin::InheritedFromMergedHeader,
            ContextOrigin::RowLabel,
            ContextOrigin::InheritedFromMergedRowLabel,
            ContextOrigin::PageHeading,
            ContextOrigin::Footnote,
        ] {
            assert_eq!(ContextOrigin::parse(origin.as_str()), Some(origin));
        }
        for interpretation in [
            DiagramInterpretation::None,
            DiagramInterpretation::NotAttempted,
        ] {
            assert_eq!(
                DiagramInterpretation::parse(interpretation.as_str()),
                Some(interpretation)
            );
        }
    }

    #[test]
    fn no_phase_1b_diagram_state_claims_understanding() {
        assert!(!DiagramInterpretation::NotAttempted.is_understood());
        assert!(!DiagramInterpretation::None.is_understood());
    }

    #[test]
    fn inherited_context_is_distinguishable_from_context_read_in_place() {
        let inherited = StructuralContext {
            subject: Some(ContextRef::new(
                "BP21",
                ContextOrigin::InheritedFromMergedRowLabel,
            )),
            ..StructuralContext::default()
        };
        assert!(inherited.has_inferred_context());

        let written = StructuralContext {
            subject: Some(ContextRef::new("BP21D", ContextOrigin::RowLabel)),
            ..StructuralContext::default()
        };
        assert!(!written.has_inferred_context());
    }

    #[test]
    fn completeness_requires_product_property_and_unit_together() {
        let mut context = StructuralContext {
            subject: Some(ContextRef::new("BP21", ContextOrigin::RowLabel)),
            property: Some(ContextRef::new("Длина", ContextOrigin::HeaderRow)),
            ..StructuralContext::default()
        };
        assert!(
            !context.is_complete(),
            "a value with no unit is not complete"
        );

        context.unit = Some(UnitRef {
            unit: "мм".to_owned(),
            origin: ContextOrigin::HeaderRow,
        });
        assert!(context.is_complete());
    }
}
