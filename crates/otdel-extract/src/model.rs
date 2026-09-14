//! What the reading adapters produce, before anything is stored.
//!
//! These types are deliberately *descriptions of what was found*, not decisions. The
//! decision — which [`otdel_core::extraction::PageStatus`] a page ends up with — is made
//! in [`crate::assess`] from these observations, in one place, so the rule can be read
//! and tested on its own.

use otdel_core::extraction::{BoundingBox, CellValueKind, RegionKind};
use otdel_core::extraction_context::{CellRole, CellVerdict, StructuralContext};

/// Page-level facts gathered without reading any text: geometry and what the page
/// contains structurally.
#[derive(Debug, Clone, PartialEq)]
pub struct PageInventory {
    pub page_number: u32,
    pub width_pt: f64,
    pub height_pt: f64,
    /// Normalised to 0/90/180/270.
    pub rotation: i32,
    /// Image XObjects referenced by the page's resources. A page with images and no
    /// text is the signature of a scan.
    pub image_count: u32,
}

/// Inventory of a whole document.
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentInventory {
    pub page_count: u32,
    pub pages: Vec<PageInventory>,
}

/// Text recovered from a page's text layer, with the structure that could be recovered
/// from the glyph positions.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PageText {
    /// Reading-order plain text of the page.
    pub text: String,
    pub regions: Vec<ExtractedRegion>,
    /// Non-whitespace characters.
    pub char_count: u32,
    pub word_count: u32,
    /// Share of characters that decoded to a replacement/unassigned code point. A high
    /// value means the text layer exists but is unusable — the classic broken-encoding
    /// PDF that would otherwise yield convincing nonsense.
    pub garbled_ratio: f64,
    /// Vector drawing operations. Used only to tell "blank page" from "page with a
    /// diagram and no text".
    pub drawing_ops: u32,
    /// The per-page glyph budget was reached and characters were dropped. Recorded
    /// because a page that was cut short must not be reported as fully read.
    pub truncated: bool,
}

/// One structural block of a page.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedRegion {
    pub kind: RegionKind,
    pub text: String,
    pub bbox: Option<BoundingBox>,
    /// Present exactly when `kind == RegionKind::Table`.
    pub table: Option<ExtractedTable>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedTable {
    pub row_count: u32,
    pub column_count: u32,
    pub cells: Vec<ExtractedCell>,
    /// How many leading rows form the header band. `0` means no header could be proven —
    /// which is a stated gap, not a licence to treat the first row as one.
    pub header_rows: u32,
    /// How many leading columns hold the rows' own labels (the product designation, as a
    /// rule). `0` means none could be proven.
    pub label_columns: u32,
}

/// A cell, kept verbatim, with the context that makes it readable as evidence.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedCell {
    pub row_index: u32,
    pub column_index: u32,
    pub is_header: bool,
    /// Exactly the characters that stood there, joined with single spaces where the
    /// source had gaps. Never rounded, never reformatted, never parsed into a number.
    pub raw_text: String,
    pub value_kind: CellValueKind,
    /// Only when a unit literally appears in the cell or in its column header.
    pub unit: Option<String>,
    pub column_header: Option<String>,
    pub bbox: Option<BoundingBox>,
    /// What the cell is within the table. Filled by [`crate::table_context`].
    pub role: CellRole,
    /// Whether it may be read as a value, and why not when it may not.
    pub verdict: CellVerdict,
    /// Product, property, unit and conditions, each naming its origin.
    pub structural_context: StructuralContext,
}

impl ExtractedCell {
    /// A cell as the grid detector leaves it: verbatim text and geometry, with the
    /// semantic layer still unset. [`crate::table_context::annotate_page`] fills the rest.
    pub fn unannotated(
        row_index: u32,
        column_index: u32,
        raw_text: String,
        value_kind: CellValueKind,
        bbox: Option<BoundingBox>,
    ) -> Self {
        Self {
            row_index,
            column_index,
            is_header: false,
            raw_text,
            value_kind,
            unit: None,
            column_header: None,
            bbox,
            role: CellRole::Data,
            verdict: CellVerdict::default(),
            structural_context: StructuralContext::default(),
        }
    }
}

/// Text produced by an OCR engine.
#[derive(Debug, Clone, PartialEq)]
pub struct RecognisedText {
    pub text: String,
    pub engine: String,
    pub engine_version: String,
    pub language: String,
}

/// Whether an external tool can be used, and if not, why not — in words a person can act
/// on ("tesseract is not installed"), never a silent `false`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolAvailability {
    Available { version: String },
    Unavailable { reason: String },
}

impl ToolAvailability {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available { .. })
    }

    pub fn version(&self) -> Option<&str> {
        match self {
            Self::Available { version } => Some(version),
            Self::Unavailable { .. } => None,
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Available { .. } => None,
            Self::Unavailable { reason } => Some(reason),
        }
    }
}

impl PageText {
    /// A page whose text layer produced nothing at all.
    pub fn is_textless(&self) -> bool {
        self.char_count == 0
    }
}
