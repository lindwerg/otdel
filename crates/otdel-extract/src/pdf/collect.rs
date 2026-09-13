//! Collecting *positioned* glyphs from a page.
//!
//! The parser hands over one decoded character at a time together with its text
//! rendering matrix. Keeping the position — rather than only the characters — is what
//! makes the rest of phase 1B possible: a stored fact can point at a rectangle on a
//! page, and a table can be recovered from the whitespace between columns.
//!
//! Coordinates are kept in **PDF user space** (origin bottom-left, units = points),
//! which is the space the media box is expressed in and the space stored in
//! `otdel.page_regions`. Nothing here converts to pixels.

use pdf_extract::{ColorSpace, MediaBox, OutputDev, OutputError, Path, Transform};

/// One decoded character with where it sits on the page.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Glyph {
    /// Left edge of the glyph on the baseline.
    pub x: f64,
    /// Baseline position, in user space (larger = higher on the page).
    pub y: f64,
    /// Horizontal advance in user-space units.
    pub advance: f64,
    /// Font size after the text matrix is applied.
    pub size: f64,
    pub text: String,
}

impl Glyph {
    pub fn right(&self) -> f64 {
        self.x + self.advance
    }
}

/// Fallback when a text matrix degenerates (a zero-scale `Tf`, a broken `Tm`). Keeping a
/// usable number here avoids dividing by zero in the layout pass; it only ever affects
/// grouping tolerances, never the characters themselves.
const FALLBACK_FONT_SIZE: f64 = 10.0;

/// Upper bound on glyphs kept for one page — a resource limit
/// (`docs/block-01-spec.md` §13.9). Reaching it is recorded, not hidden: the page is
/// then reported as incomplete rather than as fully read.
pub(crate) const MAX_GLYPHS_PER_PAGE: usize = 120_000;

/// [`OutputDev`] that records glyphs and counts drawing operations.
pub(crate) struct GlyphCollector {
    pub glyphs: Vec<Glyph>,
    /// `fill`/`stroke` operations. Only used to tell a blank page from a page that
    /// carries a diagram and no text.
    pub drawing_ops: u32,
    /// The glyph budget was reached and characters were dropped.
    pub truncated: bool,
    pub media_box: Option<(f64, f64, f64, f64)>,
    max_glyphs: usize,
}

impl GlyphCollector {
    pub fn new() -> Self {
        Self::with_limit(MAX_GLYPHS_PER_PAGE)
    }

    pub fn with_limit(max_glyphs: usize) -> Self {
        Self {
            glyphs: Vec::new(),
            drawing_ops: 0,
            truncated: false,
            media_box: None,
            max_glyphs,
        }
    }

    /// Page height in user space, when the page announced a media box.
    pub fn page_height(&self) -> Option<f64> {
        self.media_box.map(|(_, lly, _, ury)| ury - lly)
    }
}

impl OutputDev for GlyphCollector {
    fn begin_page(
        &mut self,
        _page_num: u32,
        media_box: &MediaBox,
        _art_box: Option<(f64, f64, f64, f64)>,
    ) -> Result<(), OutputError> {
        self.media_box = Some((media_box.llx, media_box.lly, media_box.urx, media_box.ury));
        Ok(())
    }

    fn end_page(&mut self) -> Result<(), OutputError> {
        Ok(())
    }

    fn output_character(
        &mut self,
        trm: &Transform,
        width: f64,
        _spacing: f64,
        font_size: f64,
        text: &str,
    ) -> Result<(), OutputError> {
        if self.glyphs.len() >= self.max_glyphs {
            self.truncated = true;
            return Ok(());
        }
        if text.is_empty() {
            return Ok(());
        }

        // The text rendering matrix places the glyph; its translation is the origin on
        // the baseline. No flip is applied: user space is what the rest of the pipeline
        // and the database both speak.
        let x = trm.m31;
        let y = trm.m32;

        // Font size in page units after the matrix: the side of a square with the same
        // area as the transformed (font_size × font_size) box. Handles rotated and
        // non-uniformly scaled text without special cases.
        let scaled_x = font_size * (trm.m11 + trm.m21);
        let scaled_y = font_size * (trm.m12 + trm.m22);
        let size = (scaled_x * scaled_y).abs().sqrt();
        let size = if size.is_finite() && size > 0.01 {
            size
        } else {
            FALLBACK_FONT_SIZE
        };

        let advance = width * size;
        let advance = if advance.is_finite() && advance >= 0.0 {
            advance
        } else {
            0.0
        };

        if !x.is_finite() || !y.is_finite() {
            // A glyph nobody can place is not stored with invented coordinates.
            return Ok(());
        }

        self.glyphs.push(Glyph {
            x,
            y,
            advance,
            size,
            text: text.to_owned(),
        });
        Ok(())
    }

    fn begin_word(&mut self) -> Result<(), OutputError> {
        Ok(())
    }

    fn end_word(&mut self) -> Result<(), OutputError> {
        Ok(())
    }

    fn end_line(&mut self) -> Result<(), OutputError> {
        Ok(())
    }

    fn fill(
        &mut self,
        _ctm: &Transform,
        _colorspace: &ColorSpace,
        _color: &[f64],
        _path: &Path,
    ) -> Result<(), OutputError> {
        self.drawing_ops = self.drawing_ops.saturating_add(1);
        Ok(())
    }

    fn stroke(
        &mut self,
        _ctm: &Transform,
        _colorspace: &ColorSpace,
        _color: &[f64],
        _path: &Path,
    ) -> Result<(), OutputError> {
        self.drawing_ops = self.drawing_ops.saturating_add(1);
        Ok(())
    }
}

/// Characters that mean "the font could not be decoded", not "this is the text".
///
/// Counted separately so a PDF whose fonts have no usable `ToUnicode` mapping is caught
/// and sent to recognition instead of producing convincing gibberish.
///
/// The NUL case is not hypothetical: when a font names a glyph in a form the parser's
/// table does not know (`uni0442` and friends), the code point stays unmapped and decodes
/// to `U+0000`. Counting those keeps such a page out of "read cleanly".
pub(crate) fn is_garbled(ch: char) -> bool {
    ch == '\u{fffd}'
        || ('\u{e000}'..='\u{f8ff}').contains(&ch)
        || (ch.is_control() && !ch.is_whitespace())
}

/// May this character be part of stored text?
///
/// Control characters other than tab and newline are not text and cannot be stored:
/// PostgreSQL rejects `U+0000` in a `text` column outright, and a single such character
/// coming out of one font would otherwise fail the whole document's extraction. They are
/// dropped from what is *stored* while still being counted by [`is_garbled`] in what was
/// *found* — so the page's assessment still reflects the damage.
pub(crate) fn is_storable(ch: char) -> bool {
    !ch.is_control() || ch == '\n' || ch == '\t'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_and_private_use_characters_count_as_garbled() {
        assert!(is_garbled('\u{fffd}'));
        assert!(is_garbled('\u{e001}'));
        assert!(is_garbled('\u{0001}'));
        assert!(is_garbled('\u{0000}'));
        assert!(!is_garbled('п'));
        assert!(!is_garbled(' '));
        assert!(!is_garbled('\n'));
        assert!(!is_garbled('²'));
    }

    #[test]
    fn control_characters_are_never_stored_as_text() {
        assert!(!is_storable('\u{0000}'));
        assert!(!is_storable('\u{0007}'));
        assert!(!is_storable('\u{001b}'));
        assert!(is_storable('\n'));
        assert!(is_storable('\t'));
        assert!(is_storable('п'));
        // A replacement character is storable: it is honest about a decoding problem
        // rather than being a character the database cannot hold.
        assert!(is_storable('\u{fffd}'));
    }

    #[test]
    fn glyph_right_edge_uses_the_advance() {
        let glyph = Glyph {
            x: 10.0,
            y: 100.0,
            advance: 5.5,
            size: 11.0,
            text: "а".to_owned(),
        };
        assert!((glyph.right() - 15.5).abs() < f64::EPSILON);
    }
}
