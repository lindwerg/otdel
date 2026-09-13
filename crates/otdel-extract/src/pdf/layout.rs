//! Turning positioned glyphs into words, lines and blocks.
//!
//! A PDF has no paragraphs — it has glyphs at coordinates. Everything structural is
//! reconstructed from geometry here, and the tolerances are expressed as fractions of the
//! local font size so they hold for a title page and a specification table alike.
//!
//! Known limitation, stated rather than hidden: lines are grouped by baseline across the
//! full page width, so a genuinely two-column page interleaves its columns in the plain
//! text. The *regions* still carry correct coordinates, and tables (which is what the
//! technical catalogues are made of) are recovered separately in [`super::tables`].

use otdel_core::extraction::{BoundingBox, RegionKind};

use super::collect::{is_storable, Glyph};

/// A run of glyphs with no significant gap between them.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Word {
    pub text: String,
    pub x0: f64,
    pub x1: f64,
    pub y: f64,
    pub size: f64,
}

/// Words sharing a baseline.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Line {
    pub words: Vec<Word>,
    pub y: f64,
    pub size: f64,
    pub x0: f64,
    pub x1: f64,
}

impl Line {
    /// The line as text, words separated by a single space.
    pub fn text(&self) -> String {
        self.words
            .iter()
            .map(|word| word.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn bbox(&self) -> BoundingBox {
        BoundingBox::new(
            self.x0,
            self.y - self.size * DESCENDER,
            self.x1,
            self.y + self.size * ASCENDER,
        )
    }
}

/// Fractions of the font size used to approximate a glyph box from a baseline.
const ASCENDER: f64 = 0.85;
const DESCENDER: f64 = 0.25;

/// Glyphs closer than this (relative to the font size) belong to the same word.
const WORD_GAP: f64 = 0.28;
/// Baselines within this fraction of the font size are the same line.
const LINE_TOLERANCE: f64 = 0.45;
/// Lines further apart than this (relative to the font size) start a new block.
const BLOCK_GAP: f64 = 1.9;
/// A block whose text is this much larger than the page median reads as a heading.
const HEADING_SIZE_RATIO: f64 = 1.18;
const HEADING_MAX_CHARS: usize = 200;
const HEADING_MAX_LINES: usize = 2;
/// A small-print block inside the bottom band of the page reads as a footnote.
const FOOTNOTE_BAND: f64 = 0.12;
const FOOTNOTE_SIZE_RATIO: f64 = 0.88;

/// Group glyphs into lines of words, in reading order (top to bottom, left to right).
pub(crate) fn build_lines(glyphs: &[Glyph]) -> Vec<Line> {
    if glyphs.is_empty() {
        return Vec::new();
    }

    let mut ordered: Vec<&Glyph> = glyphs.iter().collect();
    // Descending y: user space counts upwards, reading order goes downwards.
    ordered.sort_by(|a, b| {
        b.y.partial_cmp(&a.y)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
    });

    let mut rows: Vec<Vec<&Glyph>> = Vec::new();
    for glyph in ordered {
        match rows.last_mut() {
            Some(row) => {
                let reference = row[0];
                let tolerance = reference.size.max(glyph.size) * LINE_TOLERANCE;
                if (reference.y - glyph.y).abs() <= tolerance {
                    row.push(glyph);
                } else {
                    rows.push(vec![glyph]);
                }
            }
            None => rows.push(vec![glyph]),
        }
    }

    rows.into_iter().filter_map(build_line).collect()
}

fn build_line(mut row: Vec<&Glyph>) -> Option<Line> {
    row.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));

    let mut words: Vec<Word> = Vec::new();
    let mut cursor: Option<Word> = None;
    let mut previous_right = f64::NEG_INFINITY;

    for glyph in row {
        let is_space = glyph.text.chars().all(char::is_whitespace);
        if is_space {
            // An explicit space closes the current word without contributing text.
            if let Some(word) = cursor.take() {
                words.push(word);
            }
            previous_right = glyph.right();
            continue;
        }

        // A glyph whose font could not be decoded still occupies its place on the line —
        // the advance is kept so neighbouring words do not slide together — but the
        // undecodable characters themselves are not carried into stored text.
        let text: String = glyph.text.chars().filter(|ch| is_storable(*ch)).collect();
        if text.is_empty() {
            previous_right = glyph.right();
            continue;
        }

        let gap = glyph.x - previous_right;
        let starts_new_word = cursor.is_none() || gap > glyph.size * WORD_GAP;

        if starts_new_word {
            if let Some(word) = cursor.take() {
                words.push(word);
            }
            cursor = Some(Word {
                text,
                x0: glyph.x,
                x1: glyph.right(),
                y: glyph.y,
                size: glyph.size,
            });
        } else if let Some(word) = cursor.as_mut() {
            word.text.push_str(&text);
            word.x1 = word.x1.max(glyph.right());
            word.size = word.size.max(glyph.size);
        }
        previous_right = glyph.right();
    }
    if let Some(word) = cursor.take() {
        words.push(word);
    }

    let words: Vec<Word> = words
        .into_iter()
        .filter(|word| !word.text.trim().is_empty())
        .collect();
    if words.is_empty() {
        return None;
    }

    let y = words.iter().map(|word| word.y).sum::<f64>() / words.len() as f64;
    let size = median(&mut words.iter().map(|word| word.size).collect::<Vec<_>>());
    let x0 = words.iter().map(|word| word.x0).fold(f64::MAX, f64::min);
    let x1 = words.iter().map(|word| word.x1).fold(f64::MIN, f64::max);

    Some(Line {
        words,
        y,
        size,
        x0,
        x1,
    })
}

/// Reading-order plain text: lines separated by a newline, blocks by a blank line.
pub(crate) fn plain_text(lines: &[Line]) -> String {
    let mut out = String::new();
    let mut previous: Option<&Line> = None;
    for line in lines {
        if let Some(previous) = previous {
            let gap = previous.y - line.y;
            out.push('\n');
            if gap > previous.size.max(line.size) * BLOCK_GAP {
                out.push('\n');
            }
        }
        out.push_str(&line.text());
        previous = Some(line);
    }
    out
}

/// Group consecutive lines into blocks and name what each block is.
///
/// `page_height`/`page_bottom` are only used to recognise the footnote band; when the
/// page never announced a media box they are `None` and no block is called a footnote
/// rather than one being guessed.
pub(crate) fn build_blocks(
    lines: &[Line],
    page_bottom: Option<f64>,
    page_height: Option<f64>,
) -> Vec<(RegionKind, String, BoundingBox)> {
    if lines.is_empty() {
        return Vec::new();
    }

    let median_size = median(&mut lines.iter().map(|line| line.size).collect::<Vec<_>>());

    let mut blocks: Vec<Vec<&Line>> = Vec::new();
    for line in lines {
        let start_new = match blocks.last().and_then(|block| block.last()) {
            None => true,
            Some(previous) => {
                let gap = previous.y - line.y;
                let reference = previous.size.max(line.size);
                let size_ratio = (previous.size / line.size).max(line.size / previous.size);
                gap > reference * BLOCK_GAP || size_ratio > 1.35
            }
        };
        if start_new {
            blocks.push(vec![line]);
        } else if let Some(block) = blocks.last_mut() {
            block.push(line);
        }
    }

    blocks
        .into_iter()
        .map(|block| {
            let text = block
                .iter()
                .map(|line| line.text())
                .collect::<Vec<_>>()
                .join("\n");
            let bbox = block
                .iter()
                .map(|line| line.bbox())
                .reduce(BoundingBox::union)
                .unwrap_or(BoundingBox::new(0.0, 0.0, 0.0, 0.0));
            let size = median(&mut block.iter().map(|line| line.size).collect::<Vec<_>>());
            let kind = classify_block(
                block.len(),
                &text,
                size,
                median_size,
                bbox.y0,
                page_bottom,
                page_height,
            );
            (kind, text, bbox)
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn classify_block(
    line_count: usize,
    text: &str,
    size: f64,
    median_size: f64,
    bottom: f64,
    page_bottom: Option<f64>,
    page_height: Option<f64>,
) -> RegionKind {
    if line_count <= HEADING_MAX_LINES
        && size >= median_size * HEADING_SIZE_RATIO
        && text.chars().count() <= HEADING_MAX_CHARS
    {
        return RegionKind::Heading;
    }

    if let (Some(page_bottom), Some(page_height)) = (page_bottom, page_height) {
        let band_top = page_bottom + page_height * FOOTNOTE_BAND;
        if bottom <= band_top && size <= median_size * FOOTNOTE_SIZE_RATIO {
            return RegionKind::Footnote;
        }
    }

    RegionKind::Paragraph
}

/// Median of a sample; `0.0` for an empty one. Used for font sizes, where the mean
/// would be dragged around by a single large title.
pub(crate) fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    values[values.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph(x: f64, y: f64, text: &str) -> Glyph {
        Glyph {
            x,
            y,
            advance: 6.0,
            size: 10.0,
            text: text.to_owned(),
        }
    }

    /// Lay out a string horizontally at `y`, 6pt per character.
    fn run(x: f64, y: f64, text: &str) -> Vec<Glyph> {
        text.chars()
            .enumerate()
            .map(|(index, ch)| glyph(x + index as f64 * 6.0, y, &ch.to_string()))
            .collect()
    }

    #[test]
    fn glyphs_on_one_baseline_become_one_line_of_words() {
        let mut glyphs = run(50.0, 700.0, "Профиль");
        glyphs.extend(run(120.0, 700.0, "BP21"));
        let lines = build_lines(&glyphs);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].words.len(), 2);
        assert_eq!(lines[0].text(), "Профиль BP21");
    }

    #[test]
    fn lines_are_ordered_top_to_bottom() {
        let mut glyphs = run(50.0, 600.0, "второй");
        glyphs.extend(run(50.0, 700.0, "первый"));
        let lines = build_lines(&glyphs);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text(), "первый");
        assert_eq!(lines[1].text(), "второй");
        assert_eq!(plain_text(&lines), "первый\n\nвторой");
    }

    #[test]
    fn characters_without_a_gap_stay_one_word() {
        let glyphs = run(10.0, 100.0, "МПа");
        let lines = build_lines(&glyphs);
        assert_eq!(lines[0].words.len(), 1);
        assert_eq!(lines[0].words[0].text, "МПа");
    }

    #[test]
    fn an_explicit_space_glyph_separates_words() {
        let mut glyphs = run(10.0, 100.0, "120");
        glyphs.push(glyph(28.0, 100.0, " "));
        glyphs.extend(run(34.0, 100.0, "мм"));
        let lines = build_lines(&glyphs);
        assert_eq!(lines[0].text(), "120 мм");
    }

    #[test]
    fn a_larger_single_line_reads_as_a_heading() {
        let mut glyphs: Vec<Glyph> = run(50.0, 780.0, "Каталог")
            .into_iter()
            .map(|mut g| {
                g.size = 22.0;
                g
            })
            .collect();
        for offset in 0..6 {
            glyphs.extend(run(
                50.0,
                700.0 - f64::from(offset) * 12.0,
                "обычный текст строки",
            ));
        }
        let lines = build_lines(&glyphs);
        let blocks = build_blocks(&lines, Some(0.0), Some(842.0));
        assert_eq!(blocks[0].0, RegionKind::Heading);
        assert_eq!(blocks[0].1, "Каталог");
        assert!(blocks
            .iter()
            .skip(1)
            .all(|(kind, _, _)| *kind == RegionKind::Paragraph));
    }

    #[test]
    fn small_print_at_the_bottom_reads_as_a_footnote() {
        let mut glyphs = Vec::new();
        for offset in 0..6 {
            glyphs.extend(run(
                50.0,
                700.0 - f64::from(offset) * 12.0,
                "основной текст страницы",
            ));
        }
        glyphs.extend(
            run(50.0, 40.0, "сноска о допусках")
                .into_iter()
                .map(|mut g| {
                    g.size = 7.0;
                    g
                })
                .collect::<Vec<_>>(),
        );
        let lines = build_lines(&glyphs);
        let blocks = build_blocks(&lines, Some(0.0), Some(842.0));
        assert_eq!(blocks.last().unwrap().0, RegionKind::Footnote);
    }

    #[test]
    fn without_a_media_box_nothing_is_called_a_footnote() {
        let glyphs = run(50.0, 40.0, "мелкий текст");
        let lines = build_lines(&glyphs);
        let blocks = build_blocks(&lines, None, None);
        assert_eq!(blocks[0].0, RegionKind::Paragraph);
    }

    #[test]
    fn undecodable_glyphs_are_dropped_from_stored_text_but_keep_the_line_intact() {
        // A font whose glyph names the parser does not understand yields U+0000 for the
        // affected characters. Real catalogues do this; PostgreSQL cannot store it.
        let mut glyphs = run(50.0, 700.0, "Pro");
        glyphs.push(glyph(68.0, 700.0, "\u{0}"));
        glyphs.extend(run(74.0, 700.0, "fil"));

        let lines = build_lines(&glyphs);
        let text = lines[0].text();
        assert!(!text.contains('\u{0}'), "{text:?}");
        assert_eq!(text, "Profil");
    }

    #[test]
    fn a_glyph_that_is_only_undecodable_does_not_create_an_empty_word() {
        let mut glyphs = run(50.0, 700.0, "AB");
        glyphs.push(glyph(200.0, 700.0, "\u{0}"));
        let lines = build_lines(&glyphs);
        assert_eq!(lines[0].words.len(), 1);
        assert_eq!(lines[0].text(), "AB");
    }

    #[test]
    fn median_of_an_empty_sample_is_zero() {
        assert_eq!(median(&mut []), 0.0);
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), 2.0);
    }
}
