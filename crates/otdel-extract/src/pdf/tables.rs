//! Recovering tables from the whitespace between columns.
//!
//! PDF catalogues draw tables as text at coordinates; there is no table object to read.
//! The columns are recovered from the one thing that is genuinely there: a vertical
//! corridor of white space that **no row crosses**. That is a strict test, and it is
//! meant to be — the alternative failure mode is inventing a grid and then filling it
//! with values from the wrong columns, which would silently corrupt exactly the numbers
//! the acceptance checks care about (profile, length, thickness, load).
//!
//! When the test fails, no table is produced and the lines stay ordinary paragraphs. A
//! missing table is a visible gap; a wrong one is a lie.

use otdel_core::extraction::BoundingBox;

use crate::model::{ExtractedCell, ExtractedTable};
use crate::units;

use super::layout::{median, Line, Word};

/// At least this many consecutive rows must line up before anything is called a table.
const MIN_TABLE_ROWS: usize = 3;
const MIN_TABLE_COLUMNS: usize = 2;
/// A gap this wide (relative to the page's median font size) separates columns.
/// Ordinary word spacing is a quarter of that, so justified prose does not qualify.
const COLUMN_GAP_RATIO: f64 = 1.2;
/// Rows further apart than this are not the same table.
const ROW_GAP_RATIO: f64 = 3.0;
/// Resource limit: a "table" larger than this is not stored as a grid.
const MAX_TABLE_CELLS: usize = 4_000;

/// A table found in a consecutive range of lines.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DetectedTable {
    /// `lines[start..end]` were consumed by this table.
    pub start: usize,
    pub end: usize,
    pub table: ExtractedTable,
    pub bbox: BoundingBox,
}

/// Horizontal run of words that belong together (one cell's worth of text).
#[derive(Debug, Clone)]
struct Group {
    text: String,
    x0: f64,
    x1: f64,
}

/// Text of a cell plus the horizontal extent it occupies, or `None` for a blank cell.
type Cell = Option<(String, f64, f64)>;

/// Find the tables on a page. Ranges never overlap and are returned in reading order.
pub(crate) fn detect_tables(lines: &[Line]) -> Vec<DetectedTable> {
    if lines.len() < MIN_TABLE_ROWS {
        return Vec::new();
    }

    let page_size = median(&mut lines.iter().map(|line| line.size).collect::<Vec<_>>());
    if page_size <= 0.0 {
        return Vec::new();
    }
    let column_gap = page_size * COLUMN_GAP_RATIO;

    let rows: Vec<Vec<Group>> = lines
        .iter()
        .map(|line| group_words(&line.words, column_gap))
        .collect();

    let mut tables = Vec::new();
    let mut index = 0usize;
    while index < lines.len() {
        if rows[index].len() < MIN_TABLE_COLUMNS {
            index += 1;
            continue;
        }

        // Extend while the rows stay multi-column and vertically adjacent.
        let mut end = index + 1;
        while end < lines.len()
            && rows[end].len() >= MIN_TABLE_COLUMNS
            && lines[end - 1].y - lines[end].y
                <= lines[end - 1].size.max(lines[end].size) * ROW_GAP_RATIO
        {
            end += 1;
        }

        if end - index >= MIN_TABLE_ROWS {
            if let Some(table) = build_table(&lines[index..end], &rows[index..end], column_gap) {
                tables.push(DetectedTable {
                    start: index,
                    end,
                    table: table.0,
                    bbox: table.1,
                });
            }
        }
        index = end.max(index + 1);
    }

    tables
}

/// Merge words separated by less than `column_gap` into one cell-sized group.
fn group_words(words: &[Word], column_gap: f64) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for word in words {
        match groups.last_mut() {
            Some(last) if word.x0 - last.x1 < column_gap => {
                last.text.push(' ');
                last.text.push_str(&word.text);
                last.x1 = last.x1.max(word.x1);
            }
            _ => groups.push(Group {
                text: word.text.clone(),
                x0: word.x0,
                x1: word.x1,
            }),
        }
    }
    groups
}

/// Column bands: the x-intervals left over once every corridor wider than `column_gap`
/// that **no row crosses** is removed.
fn column_bands(rows: &[Vec<Group>], column_gap: f64) -> Option<Vec<(f64, f64)>> {
    let mut intervals: Vec<(f64, f64)> = rows
        .iter()
        .flat_map(|row| row.iter().map(|group| (group.x0, group.x1)))
        .collect();
    if intervals.len() < MIN_TABLE_COLUMNS {
        return None;
    }
    intervals.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut bands: Vec<(f64, f64)> = Vec::new();
    for (x0, x1) in intervals {
        match bands.last_mut() {
            // Separated by less than a column gap: the corridor is not clear, so these
            // belong to the same band.
            Some(last) if x0 - last.1 < column_gap => last.1 = last.1.max(x1),
            _ => bands.push((x0, x1)),
        }
    }

    (bands.len() >= MIN_TABLE_COLUMNS).then_some(bands)
}

fn build_table(
    lines: &[Line],
    rows: &[Vec<Group>],
    column_gap: f64,
) -> Option<(ExtractedTable, BoundingBox)> {
    let bands = column_bands(rows, column_gap)?;
    let column_count = bands.len();
    let row_count = rows.len();
    if row_count.saturating_mul(column_count) > MAX_TABLE_CELLS {
        return None;
    }

    // Text of each cell, built by placing every group into the band that contains its
    // centre. Two groups in one band are joined with a single space — that is a cell
    // whose content had an internal gap, not two columns.
    let mut grid: Vec<Vec<Cell>> = vec![vec![None; column_count]; row_count];
    for (row_index, row) in rows.iter().enumerate() {
        for group in row {
            let centre = (group.x0 + group.x1) / 2.0;
            let Some(column) = band_of(&bands, centre) else {
                continue;
            };
            match &mut grid[row_index][column] {
                Some((text, x0, x1)) => {
                    text.push(' ');
                    text.push_str(&group.text);
                    *x0 = x0.min(group.x0);
                    *x1 = x1.max(group.x1);
                }
                cell @ None => *cell = Some((group.text.clone(), group.x0, group.x1)),
            }
        }
    }

    // A grid where most rows have a single filled column is prose that happened to be
    // indented, not a table.
    let populated_rows = grid
        .iter()
        .filter(|row| row.iter().filter(|cell| cell.is_some()).count() >= MIN_TABLE_COLUMNS)
        .count();
    if populated_rows < MIN_TABLE_ROWS {
        return None;
    }

    let header_row = detect_header_row(&grid);
    let headers: Vec<Option<String>> = (0..column_count)
        .map(|column| {
            if !header_row {
                return None;
            }
            grid[0][column]
                .as_ref()
                .map(|(text, _, _)| text.trim().to_owned())
                .filter(|text| !text.is_empty())
        })
        .collect();

    let mut cells = Vec::with_capacity(row_count * column_count);
    for (row_index, row) in grid.iter().enumerate() {
        let line = &lines[row_index];
        let is_header = header_row && row_index == 0;
        for (column_index, cell) in row.iter().enumerate() {
            let (raw_text, bbox) = match cell {
                Some((text, x0, x1)) => (
                    text.trim().to_owned(),
                    Some(BoundingBox::new(*x0, line.bbox().y0, *x1, line.bbox().y1)),
                ),
                None => (String::new(), None),
            };
            let column_header = if is_header {
                None
            } else {
                headers[column_index].clone()
            };
            let unit = units::detect_unit(&raw_text, column_header.as_deref());
            cells.push(ExtractedCell {
                row_index: row_index as u32,
                column_index: column_index as u32,
                is_header,
                value_kind: units::classify(&raw_text),
                raw_text,
                unit,
                column_header,
                bbox,
            });
        }
    }

    let bbox = lines
        .iter()
        .map(Line::bbox)
        .reduce(BoundingBox::union)
        .unwrap_or(BoundingBox::new(0.0, 0.0, 0.0, 0.0));

    Some((
        ExtractedTable {
            row_count: row_count as u32,
            column_count: column_count as u32,
            cells,
        },
        bbox,
    ))
}

fn band_of(bands: &[(f64, f64)], x: f64) -> Option<usize> {
    bands
        .iter()
        .position(|(x0, x1)| x >= *x0 && x <= *x1)
        // A centre that falls inside a corridor (possible when a group straddles a band
        // edge) is attributed to the nearest band rather than dropped.
        .or_else(|| {
            bands
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    distance(**a, x)
                        .partial_cmp(&distance(**b, x))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(index, _)| index)
        })
}

fn distance((x0, x1): (f64, f64), x: f64) -> f64 {
    if x < x0 {
        x0 - x
    } else if x > x1 {
        x - x1
    } else {
        0.0
    }
}

/// Is the first row a header?
///
/// Two conditions, both needed, and both learned from a real catalogue:
///
/// * **no cell reads as a number.** A row that starts with `250` is the first length of
///   a load table, not a label for the rows beneath it;
/// * **the cells read like words.** `– / 238 / 395` is not a number, but it is not a
///   label either — a row of such values was being promoted to a header and then
///   attached as `column_header` to every value below it, which is precisely the kind of
///   invented provenance this phase must not produce.
fn detect_header_row(grid: &[Vec<Cell>]) -> bool {
    use otdel_core::extraction::CellValueKind;

    let Some(first) = grid.first() else {
        return false;
    };
    let filled: Vec<&String> = first
        .iter()
        .filter_map(|cell| cell.as_ref().map(|(text, _, _)| text))
        .filter(|text| !text.trim().is_empty())
        .collect();
    if filled.len() < MIN_TABLE_COLUMNS {
        return false;
    }
    if filled
        .iter()
        .any(|text| units::classify(text) == CellValueKind::Number)
    {
        return false;
    }

    let wordy = filled
        .iter()
        .filter(|text| text.chars().any(char::is_alphabetic))
        .count();
    wordy * 2 >= filled.len()
}

#[cfg(test)]
mod tests {
    use super::super::collect::Glyph;
    use super::super::layout::build_lines;
    use super::*;
    use otdel_core::extraction::CellValueKind;

    const CHAR_WIDTH: f64 = 6.0;

    fn run(x: f64, y: f64, text: &str) -> Vec<Glyph> {
        text.chars()
            .enumerate()
            .map(|(index, ch)| Glyph {
                x: x + index as f64 * CHAR_WIDTH,
                y,
                advance: CHAR_WIDTH,
                size: 10.0,
                text: ch.to_string(),
            })
            .collect()
    }

    /// Build a page of glyphs from `(y, [(x, text)])` rows.
    fn page(rows: &[(f64, &[(f64, &str)])]) -> Vec<Glyph> {
        rows.iter()
            .flat_map(|(y, cells)| {
                cells
                    .iter()
                    .flat_map(move |(x, text)| run(*x, *y, text))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn sample_table() -> Vec<Glyph> {
        page(&[
            (
                700.0,
                &[
                    (50.0, "Профиль"),
                    (200.0, "Длина, мм"),
                    (350.0, "Нагрузка, кН"),
                ],
            ),
            (680.0, &[(50.0, "BP21"), (200.0, "1200"), (350.0, "3,5")]),
            (660.0, &[(50.0, "BP21D"), (200.0, "1500"), (350.0, "4,2")]),
            (640.0, &[(50.0, "BP30"), (200.0, "1800"), (350.0, "6,0")]),
        ])
    }

    #[test]
    fn an_aligned_grid_is_recovered_with_its_cells() {
        let lines = build_lines(&sample_table());
        let tables = detect_tables(&lines);
        assert_eq!(tables.len(), 1, "expected exactly one table");

        let table = &tables[0].table;
        assert_eq!(table.row_count, 4);
        assert_eq!(table.column_count, 3);
        assert_eq!(table.cells.len(), 12);

        let cell = |row: u32, column: u32| {
            table
                .cells
                .iter()
                .find(|cell| cell.row_index == row && cell.column_index == column)
                .unwrap()
        };
        assert_eq!(cell(0, 0).raw_text, "Профиль");
        assert!(cell(0, 0).is_header);
        assert_eq!(cell(1, 0).raw_text, "BP21");
        assert_eq!(cell(1, 1).raw_text, "1200");
        assert_eq!(cell(3, 2).raw_text, "6,0");
    }

    #[test]
    fn units_and_headers_survive_into_every_body_cell() {
        let lines = build_lines(&sample_table());
        let table = &detect_tables(&lines)[0].table;

        let length = table
            .cells
            .iter()
            .find(|cell| cell.row_index == 1 && cell.column_index == 1)
            .unwrap();
        assert_eq!(length.raw_text, "1200");
        assert_eq!(length.value_kind, CellValueKind::Number);
        // The unit comes from the header, and the header itself is kept verbatim.
        assert_eq!(length.unit.as_deref(), Some("мм"));
        assert_eq!(length.column_header.as_deref(), Some("Длина, мм"));

        let load = table
            .cells
            .iter()
            .find(|cell| cell.row_index == 2 && cell.column_index == 2)
            .unwrap();
        assert_eq!(load.raw_text, "4,2");
        assert_eq!(load.unit.as_deref(), Some("кН"));

        // A designation column has no unit invented for it.
        let profile = table
            .cells
            .iter()
            .find(|cell| cell.row_index == 1 && cell.column_index == 0)
            .unwrap();
        assert_eq!(profile.unit, None);
        assert_eq!(profile.value_kind, CellValueKind::Text);
    }

    #[test]
    fn a_blank_cell_is_reported_blank_not_as_a_value() {
        let glyphs = page(&[
            (
                700.0,
                &[
                    (50.0, "Профиль"),
                    (200.0, "Длина, мм"),
                    (350.0, "Нагрузка, кН"),
                ],
            ),
            (680.0, &[(50.0, "BP21"), (200.0, "1200"), (350.0, "3,5")]),
            // The load of this profile is simply not printed in the catalogue.
            (660.0, &[(50.0, "BP21D"), (200.0, "1500")]),
            (640.0, &[(50.0, "BP30"), (200.0, "1800"), (350.0, "6,0")]),
        ]);
        let lines = build_lines(&glyphs);
        let table = &detect_tables(&lines)[0].table;

        let missing = table
            .cells
            .iter()
            .find(|cell| cell.row_index == 2 && cell.column_index == 2)
            .expect("the cell must exist as a blank, not be omitted");
        assert_eq!(missing.raw_text, "");
        assert_eq!(missing.value_kind, CellValueKind::Empty);
        assert_ne!(missing.value_kind, CellValueKind::Number);
    }

    #[test]
    fn ordinary_prose_is_not_turned_into_a_table() {
        let glyphs = page(&[
            (
                700.0,
                &[(50.0, "Компания BASIS выпускает профили и комплектующие")],
            ),
            (
                686.0,
                &[(50.0, "для светопрозрачных конструкций разного назначения")],
            ),
            (
                672.0,
                &[(50.0, "и поставляет их по всей территории страны")],
            ),
            (658.0, &[(50.0, "включая индивидуальные заказы и сервис")]),
        ]);
        let lines = build_lines(&glyphs);
        assert!(detect_tables(&lines).is_empty());
    }

    #[test]
    fn two_aligned_rows_are_not_enough_to_claim_a_table() {
        let glyphs = page(&[
            (700.0, &[(50.0, "Профиль"), (250.0, "Длина")]),
            (680.0, &[(50.0, "BP21"), (250.0, "1200")]),
        ]);
        let lines = build_lines(&glyphs);
        assert!(detect_tables(&lines).is_empty());
    }

    #[test]
    fn a_numeric_first_row_is_data_not_a_header() {
        let glyphs = page(&[
            (700.0, &[(50.0, "100"), (250.0, "200")]),
            (680.0, &[(50.0, "110"), (250.0, "210")]),
            (660.0, &[(50.0, "120"), (250.0, "220")]),
        ]);
        let lines = build_lines(&glyphs);
        let table = &detect_tables(&lines)[0].table;
        assert!(table.cells.iter().all(|cell| !cell.is_header));
        assert!(table.cells.iter().all(|cell| cell.column_header.is_none()));
    }

    #[test]
    fn a_row_of_load_values_is_not_promoted_to_a_header() {
        // Straight from the real catalogue: a length in the first column and
        // dash-separated load triplets in the rest. None of it is a label, and treating
        // it as one would attach a wrong `column_header` to every value below.
        let glyphs = page(&[
            (
                700.0,
                &[
                    (50.0, "250"),
                    (200.0, "- / 2074 / 2345"),
                    (400.0, "- / 4220 / 4600"),
                ],
            ),
            (
                680.0,
                &[
                    (50.0, "500"),
                    (200.0, "861 / 1104 / 1206"),
                    (400.0, "1560 / 1990 / 2430"),
                ],
            ),
            (
                660.0,
                &[
                    (50.0, "750"),
                    (200.0, "- / 700 / 662"),
                    (400.0, "- / 1115 / 1285"),
                ],
            ),
        ]);
        let lines = build_lines(&glyphs);
        let table = &detect_tables(&lines)[0].table;

        assert!(table.cells.iter().all(|cell| !cell.is_header));
        assert!(table.cells.iter().all(|cell| cell.column_header.is_none()));

        // The values themselves are kept exactly as printed, and a triplet is not a
        // number that anything downstream could use as one.
        let triplet = table
            .cells
            .iter()
            .find(|cell| cell.row_index == 1 && cell.column_index == 1)
            .unwrap();
        assert_eq!(triplet.raw_text, "861 / 1104 / 1206");
        assert_eq!(triplet.value_kind, CellValueKind::Text);
        assert_eq!(triplet.unit, None);
    }

    #[test]
    fn table_rows_report_where_they_are_on_the_page() {
        let lines = build_lines(&sample_table());
        let detected = &detect_tables(&lines)[0];
        assert!(detected.bbox.is_usable());
        assert!(detected.bbox.y0 < 640.0 && detected.bbox.y1 > 700.0);
        let cell = detected
            .table
            .cells
            .iter()
            .find(|cell| cell.row_index == 1 && cell.column_index == 1)
            .unwrap();
        let bbox = cell.bbox.expect("a filled cell knows where it is");
        assert!(bbox.x0 >= 190.0 && bbox.x1 <= 320.0, "{bbox:?}");
    }
}
