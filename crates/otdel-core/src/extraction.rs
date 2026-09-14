//! Domain model of phase 1B — per-page reading of a stored original.
//!
//! Two rules from `docs/block-01-spec.md` §6.2 shape everything here:
//!
//! * **every page has an explicit outcome.** There is no "the document is done" status
//!   that can hide a page nobody could read: [`PageStatus`] is stored per page and the
//!   material-level status is *derived* from the page statuses by
//!   [`aggregate_material_status`], never set independently.
//! * **an unreadable page is said to be unreadable.** A page whose text layer is absent
//!   or unusable becomes [`PageStatus::NeedsOcr`] with a written reason when no
//!   recognition engine is available. It never becomes `empty`, and it never receives
//!   invented text.
//!
//! Table values follow the same discipline: [`TableCell::raw_text`] is the verbatim
//! fragment of the source, [`CellValueKind`] only *classifies* it, and an unclear cell
//! stays [`CellValueKind::Empty`] instead of turning into a zero.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::extraction_context::{
    CellRole, CellVerdict, DiagramInterpretation, SourceSpan, StructuralContext,
};

/// Outcome of reading one page. Stored per page; the material status is derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageStatus {
    /// Recorded by the inventory pass, not read yet.
    Pending,
    /// Usable content was obtained (from the text layer or from OCR).
    Extracted,
    /// The page genuinely carries nothing: no text, no image, no drawing.
    Empty,
    /// No usable text layer, and recognition did not run or produced nothing.
    /// The reason is always written to `diagnostic`.
    NeedsOcr,
    /// Some content was obtained but the page is knowingly incomplete.
    Partial,
    /// Reading this page failed. One such page never disappears behind a
    /// document-level "completed".
    Failed,
}

impl PageStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Extracted => "extracted",
            Self::Empty => "empty",
            Self::NeedsOcr => "needs_ocr",
            Self::Partial => "partial",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "extracted" => Some(Self::Extracted),
            "empty" => Some(Self::Empty),
            "needs_ocr" => Some(Self::NeedsOcr),
            "partial" => Some(Self::Partial),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }

    /// Whether re-running *this page alone* can plausibly change the outcome.
    ///
    /// `pending` is included: a run that was interrupted before reaching the page
    /// leaves it pending, and repeating it is exactly the right move. `extracted` and
    /// `empty` are settled; repeating them would only churn derived rows.
    pub const fn can_retry(self) -> bool {
        matches!(
            self,
            Self::NeedsOcr | Self::Partial | Self::Failed | Self::Pending
        )
    }

    /// Did this page yield content that later phases may use?
    pub const fn is_readable(self) -> bool {
        matches!(self, Self::Extracted | Self::Partial)
    }
}

/// Where a page's text came from. `None` means no text was obtained at all — which is
/// not the same as an empty string produced by a parser that silently gave up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextSource {
    None,
    TextLayer,
    Ocr,
}

impl TextSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::TextLayer => "text_layer",
            Self::Ocr => "ocr",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "text_layer" => Some(Self::TextLayer),
            "ocr" => Some(Self::Ocr),
            _ => None,
        }
    }
}

/// Structural kind of a source region on a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegionKind {
    Heading,
    Paragraph,
    Footnote,
    Table,
}

impl RegionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Heading => "heading",
            Self::Paragraph => "paragraph",
            Self::Footnote => "footnote",
            Self::Table => "table",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "heading" => Some(Self::Heading),
            "paragraph" => Some(Self::Paragraph),
            "footnote" => Some(Self::Footnote),
            "table" => Some(Self::Table),
            _ => None,
        }
    }
}

/// Classification of a table cell's verbatim text. Deliberately *not* a parsed value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellValueKind {
    /// The cell is blank in the source. Blank stays blank: it never becomes `0`.
    Empty,
    /// The text reads as a number (possibly with a unit). The digits are kept as text.
    Number,
    /// Anything else.
    Text,
}

impl CellValueKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Number => "number",
            Self::Text => "text",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "empty" => Some(Self::Empty),
            "number" => Some(Self::Number),
            "text" => Some(Self::Text),
            _ => None,
        }
    }
}

/// A rectangle in PDF user space (origin bottom-left, units = points).
///
/// Optional throughout: an adapter that cannot place a region says so instead of
/// reporting a made-up rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BoundingBox {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl BoundingBox {
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self {
            x0: x0.min(x1),
            y0: y0.min(y1),
            x1: x0.max(x1),
            y1: y0.max(y1),
        }
    }

    pub fn union(self, other: Self) -> Self {
        Self {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    /// All four coordinates are finite and the rectangle is not degenerate in a way
    /// that would be meaningless to highlight.
    pub fn is_usable(&self) -> bool {
        [self.x0, self.y0, self.x1, self.y1]
            .iter()
            .all(|value| value.is_finite())
            && self.x1 >= self.x0
            && self.y1 >= self.y0
    }
}

/// One page of a material, exactly as stored. The serde shape is the 1B API contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialPage {
    pub id: Uuid,
    pub material_id: Uuid,
    pub page_number: i32,
    pub status: PageStatus,
    pub text_source: TextSource,
    pub char_count: i32,
    pub word_count: i32,
    pub image_count: i32,
    pub width_pt: Option<f64>,
    pub height_pt: Option<f64>,
    pub rotation: i32,
    pub parser_name: Option<String>,
    pub parser_version: Option<String>,
    pub ocr_engine: Option<String>,
    pub ocr_version: Option<String>,
    pub ocr_language: Option<String>,
    pub duration_ms: Option<i32>,
    pub attempts: i32,
    /// Honest, human-readable reason for the status. Never a stack trace or a path.
    pub diagnostic: Option<String>,
    pub extracted_at: Option<DateTime<Utc>>,
    pub region_count: i32,
    pub table_count: i32,

    /// Identifier of the reading that produced the current regions and cells.
    ///
    /// A fresh value is minted every time the page is read, so a stored piece of evidence
    /// can name the revision its coordinates came from and a later re-read is recognisable
    /// as a different one rather than silently replacing it. `None` for a page that has
    /// only been inventoried. (R02 generalises this to a document-level revision line;
    /// this is the per-page anchor the source evidence needs now.)
    pub extraction_revision: Option<Uuid>,
    /// Vector drawing operations seen on the page. Used only to tell a blank page from a
    /// page carrying a diagram.
    pub drawing_count: i32,
    /// How far the page's drawings have been understood — never more than `not_attempted`
    /// in this phase. Recognising labels around a load diagram is not reading the diagram.
    pub diagram_interpretation: DiagramInterpretation,
}

/// A structural region of a page, with its source coordinates when they are known.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageRegion {
    pub id: Uuid,
    pub page_id: Uuid,
    pub page_number: i32,
    pub ordinal: i32,
    pub kind: RegionKind,
    pub text: String,
    pub source: TextSource,
    pub bbox: Option<BoundingBox>,
    /// Table shape; `None` for every non-table region.
    pub row_count: Option<i32>,
    pub column_count: Option<i32>,
    /// Where this region is on the page, or the stated reason it cannot be placed. A
    /// region read by an engine that returns no word boxes is `unavailable` with that
    /// reason — never a rectangle covering the page.
    pub span: SourceSpan,
}

/// One cell of an extracted table.
///
/// The first group of fields is the verbatim record of what stood in the source and is
/// never derived from anything. The second group — `role`, `verdict`, `structural_context`
/// and `span` — is what makes the cell *usable as evidence*: see
/// [`crate::extraction_context`] for why a verbatim cell on its own was not enough.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableCell {
    pub id: Uuid,
    pub region_id: Uuid,
    pub row_index: i32,
    pub column_index: i32,
    pub is_header: bool,
    /// Verbatim source fragment. Not normalised, not rounded, not parsed.
    pub raw_text: String,
    pub value_kind: CellValueKind,
    /// Only set when a unit is literally present in the cell or in its column header.
    pub unit: Option<String>,
    /// Verbatim header text of this column, when the table has a header row.
    pub column_header: Option<String>,
    pub bbox: Option<BoundingBox>,

    /// What this cell is within the table. A header is never a value.
    pub role: CellRole,
    /// Whether this cell may be read as a value at all, and why not when it may not.
    pub verdict: CellVerdict,
    /// Product, property, unit and conditions, each with its origin.
    pub structural_context: StructuralContext,
    /// Where on the page this cell was read, or why that cannot be said.
    pub span: SourceSpan,
}

/// Everything needed to render one page with its evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageDetail {
    pub page: MaterialPage,
    pub text: Option<String>,
    pub regions: Vec<RegionDetail>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegionDetail {
    #[serde(flatten)]
    pub region: PageRegion,
    pub cells: Vec<TableCell>,
}

/// Per-material roll-up of the page outcomes, plus the versions that produced them.
///
/// The counters are computed from the page rows on read, so they cannot drift away
/// from the pages they summarise.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractionSummary {
    pub pages_total: i32,
    pub pages_extracted: i32,
    pub pages_empty: i32,
    pub pages_needs_ocr: i32,
    pub pages_partial: i32,
    pub pages_failed: i32,
    pub pages_pending: i32,
    pub parser_name: Option<String>,
    pub parser_version: Option<String>,
    pub ocr_engine: Option<String>,
    pub ocr_version: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Document-level reason (e.g. "the file could not be opened as a PDF").
    pub diagnostic: Option<String>,
}

impl ExtractionSummary {
    /// Pages whose outcome a person may still be able to improve by retrying.
    pub const fn pages_needing_attention(&self) -> i32 {
        self.pages_needs_ocr + self.pages_partial + self.pages_failed + self.pages_pending
    }
}

/// Material status derived from the page outcomes — the only way it is ever decided.
///
/// * every page settled as `extracted`/`empty` → `completed`;
/// * no page produced anything usable → `failed`;
/// * anything in between → `partial`.
///
/// A single failed page therefore prevents `completed`, which is the whole point:
/// "ошибка одной страницы не исчезает за общим статусом «готово»"
/// (`docs/block-01-spec.md` §6.2).
pub fn aggregate_material_status(summary: &ExtractionSummary) -> crate::model::MaterialStatus {
    use crate::model::MaterialStatus;

    if summary.pages_total <= 0 {
        // Pages were never recorded: the document itself could not be opened.
        return MaterialStatus::Failed;
    }

    let settled_clean = summary.pages_extracted + summary.pages_empty;
    if settled_clean == summary.pages_total {
        return MaterialStatus::Completed;
    }
    if summary.pages_extracted == 0 && summary.pages_partial == 0 {
        return MaterialStatus::Failed;
    }
    MaterialStatus::Partial
}

/// Stable idempotency key for the extraction job of a single page.
pub fn page_extraction_idempotency_key(material_id: Uuid, page_number: i32) -> String {
    format!("extract_page:{material_id}:{page_number}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MaterialStatus;

    fn summary(
        extracted: i32,
        empty: i32,
        needs_ocr: i32,
        partial: i32,
        failed: i32,
    ) -> ExtractionSummary {
        ExtractionSummary {
            pages_total: extracted + empty + needs_ocr + partial + failed,
            pages_extracted: extracted,
            pages_empty: empty,
            pages_needs_ocr: needs_ocr,
            pages_partial: partial,
            pages_failed: failed,
            ..ExtractionSummary::default()
        }
    }

    #[test]
    fn page_statuses_round_trip() {
        for status in [
            PageStatus::Pending,
            PageStatus::Extracted,
            PageStatus::Empty,
            PageStatus::NeedsOcr,
            PageStatus::Partial,
            PageStatus::Failed,
        ] {
            assert_eq!(PageStatus::parse(status.as_str()), Some(status));
            assert_eq!(
                serde_json::to_value(status).unwrap(),
                serde_json::Value::String(status.as_str().to_owned())
            );
        }
        assert_eq!(PageStatus::parse("read"), None);
    }

    #[test]
    fn a_single_unreadable_page_prevents_completed() {
        assert_eq!(
            aggregate_material_status(&summary(31, 0, 1, 0, 0)),
            MaterialStatus::Partial
        );
        assert_eq!(
            aggregate_material_status(&summary(31, 0, 0, 0, 1)),
            MaterialStatus::Partial
        );
        assert_eq!(
            aggregate_material_status(&summary(31, 1, 0, 0, 0)),
            MaterialStatus::Completed
        );
    }

    #[test]
    fn a_scan_without_recognition_is_never_completed() {
        // The presentation: 12 pages, all image-only, no OCR engine available.
        let scanned = summary(0, 0, 12, 0, 0);
        assert_eq!(aggregate_material_status(&scanned), MaterialStatus::Failed);
        assert_ne!(
            aggregate_material_status(&scanned),
            MaterialStatus::Completed
        );
        assert_eq!(scanned.pages_needing_attention(), 12);
    }

    #[test]
    fn a_document_with_no_pages_is_failed_not_completed() {
        assert_eq!(
            aggregate_material_status(&ExtractionSummary::default()),
            MaterialStatus::Failed
        );
    }

    #[test]
    fn pending_pages_keep_the_material_out_of_completed() {
        let interrupted = ExtractionSummary {
            pages_total: 32,
            pages_extracted: 10,
            pages_pending: 22,
            ..ExtractionSummary::default()
        };
        assert_eq!(
            aggregate_material_status(&interrupted),
            MaterialStatus::Partial
        );
    }

    #[test]
    fn only_unsettled_pages_can_be_retried() {
        assert!(PageStatus::NeedsOcr.can_retry());
        assert!(PageStatus::Partial.can_retry());
        assert!(PageStatus::Failed.can_retry());
        assert!(PageStatus::Pending.can_retry());
        assert!(!PageStatus::Extracted.can_retry());
        assert!(!PageStatus::Empty.can_retry());
    }

    #[test]
    fn page_job_keys_are_stable_and_page_specific() {
        let material = Uuid::from_u128(9);
        assert_eq!(
            page_extraction_idempotency_key(material, 3),
            page_extraction_idempotency_key(material, 3)
        );
        assert_ne!(
            page_extraction_idempotency_key(material, 3),
            page_extraction_idempotency_key(material, 4)
        );
    }

    #[test]
    fn bounding_box_normalises_and_unions() {
        let a = BoundingBox::new(10.0, 20.0, 5.0, 8.0);
        assert_eq!((a.x0, a.y0, a.x1, a.y1), (5.0, 8.0, 10.0, 20.0));
        let b = BoundingBox::new(0.0, 0.0, 1.0, 1.0);
        let union = a.union(b);
        assert_eq!(
            (union.x0, union.y0, union.x1, union.y1),
            (0.0, 0.0, 10.0, 20.0)
        );
        assert!(union.is_usable());
        assert!(!BoundingBox {
            x0: f64::NAN,
            y0: 0.0,
            x1: 1.0,
            y1: 1.0
        }
        .is_usable());
    }
}
