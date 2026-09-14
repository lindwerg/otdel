//! Units and the classification of a table cell's text.
//!
//! The rule of this module is that it never *produces* a value. It reports what is
//! literally written:
//!
//! * a unit is recorded only when the unit string stands in the cell itself or in that
//!   column's header (`Длина, мм`, `Нагрузка (кН)`). A unit guessed from the meaning of
//!   a column — "this looks like a length, so millimetres" — is exactly the kind of
//!   invention the specification forbids (§6.4);
//! * [`classify`] answers "does this read as a single number", it does not parse one.
//!   A range (`40…60`), a designation (`BP 21/21D`) and a dash are all `Text`, and a
//!   blank cell is [`CellValueKind::Empty`] — never a zero.

use otdel_core::extraction::CellValueKind;

/// Unit tokens recognised in cells and headers.
///
/// Longest-first, so `кг/м²` is matched before `кг` and `мм²` before `мм`. The list is
/// intentionally small and explicit: an unknown token is simply not a unit here, which
/// leaves the text untouched instead of guessing.
const UNITS: &[&str] = &[
    "кгс/см²",
    "кгс/см2",
    "кН·м",
    "кН/м²",
    "кН/м2",
    "кН/м",
    "кг/м²",
    "кг/м2",
    "кг/м³",
    "кг/м3",
    "кг/м",
    "Н·м",
    "Н/мм²",
    "Н/мм2",
    "Н/м",
    "мм²",
    "мм2",
    "мм³",
    "мм3",
    "см²",
    "см2",
    "см³",
    "см3",
    "м²",
    "м2",
    "м³",
    "м3",
    "МПа",
    "ГПа",
    "кПа",
    "Па",
    "кгс",
    "кН",
    "МН",
    "мкм",
    "мм",
    "см",
    "дм",
    "км",
    "мин",
    "шт.",
    "шт",
    "кг",
    "°C",
    "°С",
    "%",
    "‰",
    "Н",
    "г",
    "т",
    "м",
    "ч",
    "°",
    "mm",
    "cm",
    "kg",
    "kN",
    "MPa",
];

/// Characters that may separate a number from its unit or act as a decimal/group mark.
const NUMBER_SEPARATORS: &[char] = &[',', '.', '\u{00a0}', '\u{202f}', '\u{2009}', ' ', '\''];

/// The unit written in this cell, or failing that in its column header.
///
/// Returns `None` whenever nothing is literally written — which is the common case and
/// is not a defect.
pub fn detect_unit(cell_text: &str, column_header: Option<&str>) -> Option<String> {
    if let Some(unit) = trailing_unit(cell_text) {
        return Some(unit);
    }
    column_header.and_then(header_unit)
}

/// A unit standing at the end of the text, after something that is not a letter.
///
/// `"120 мм"` yields `мм`; `"Длина"` yields nothing, and neither does `"размм"` — the
/// character before the unit must not be a letter, so a unit is never carved out of the
/// middle of a word.
pub fn trailing_unit(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    for unit in UNITS {
        let Some(head) = trimmed.strip_suffix(unit) else {
            continue;
        };
        if head.is_empty() {
            // The cell contains the unit alone (a header cell such as `мм`).
            return Some((*unit).to_owned());
        }
        let previous = head.chars().next_back()?;
        if previous.is_alphabetic() {
            continue;
        }
        return Some((*unit).to_owned());
    }
    None
}

/// A unit written in a header as `Длина, мм`, `Длина (мм)` or `Длина [мм]`.
pub fn header_unit(header: &str) -> Option<String> {
    let trimmed = header.trim().trim_end_matches(['.', ':', '*']);

    for (open, close) in [('(', ')'), ('[', ']'), ('{', '}')] {
        if let Some(end) = trimmed.strip_suffix(close) {
            if let Some(index) = end.rfind(open) {
                let inner = &end[index + open.len_utf8()..];
                if let Some(unit) = exact_unit(inner) {
                    return Some(unit);
                }
            }
        }
    }

    let tail = trimmed.rsplit(&[',', ';'][..]).next()?;
    if tail.len() == trimmed.len() {
        return None;
    }
    exact_unit(tail)
}

/// The whole (trimmed) text *is* a unit.
pub fn exact_unit(text: &str) -> Option<String> {
    let trimmed = text.trim();
    UNITS
        .iter()
        .find(|unit| **unit == trimmed)
        .map(|unit| (*unit).to_owned())
}

/// Does this text read as a *label* rather than a measurement?
///
/// The shape that matters is `слова (единица)` — words followed by a dimension in
/// brackets or after a comma. `безопасная рабочая нагрузка (Н)` is the column label that
/// was published as a characteristic; wherever a broken grid puts it, it is still a label.
///
/// Deliberately narrow. It requires a *written* unit, so an ordinary textual value such as
/// `оцинкованная сталь` is untouched and stays available as a value.
pub fn is_header_shaped(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || classify(trimmed) == CellValueKind::Number {
        return false;
    }
    if header_unit(trimmed).is_none() {
        return false;
    }
    // Enough letters to be words, not a designation such as `BP21 (мм)`.
    trimmed.chars().filter(|ch| ch.is_alphabetic()).count() >= MIN_LABEL_LETTERS
}

/// Does this cell hold more than one value at once?
///
/// The load tables print `4860 / 8470 / 12720` — one number per loading scheme — in a
/// single cell. Which of the three applies is not stated by the cell, so no single value
/// may be taken from it.
pub fn holds_several_values(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || classify(trimmed) == CellValueKind::Number {
        return false;
    }
    number_runs(trimmed) >= 2
}

/// How many separate runs of digits the text contains.
fn number_runs(text: &str) -> usize {
    let mut runs = 0usize;
    let mut in_run = false;
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            if !in_run {
                runs += 1;
                in_run = true;
            }
        } else if !NUMBER_SEPARATORS.contains(&ch) {
            in_run = false;
        }
    }
    runs
}

/// Below this many letters, a bracketed unit reads as a designation, not a label.
const MIN_LABEL_LETTERS: usize = 4;

/// Classify a cell's verbatim text. See the module docs: this is a description, not a
/// conversion.
pub fn classify(raw_text: &str) -> CellValueKind {
    let trimmed = raw_text.trim();
    if trimmed.is_empty() {
        return CellValueKind::Empty;
    }

    // A cell may legitimately carry its own unit: `120 мм` still reads as a number.
    let without_unit = match trailing_unit(trimmed) {
        Some(unit) => trimmed[..trimmed.len() - unit.len()].trim(),
        None => trimmed,
    };

    if is_single_number(without_unit) {
        CellValueKind::Number
    } else {
        CellValueKind::Text
    }
}

/// Does the text read as exactly one number?
///
/// Ranges, lists, designations and dashes deliberately do not: `40…60` is not a value,
/// and pretending it is would put a number into the knowledge base that nobody wrote.
fn is_single_number(text: &str) -> bool {
    let body = text.strip_prefix(['+', '-', '\u{2212}']).unwrap_or(text);
    if body.is_empty() {
        return false;
    }

    let mut digits = 0usize;
    let mut decimal_marks = 0usize;
    let mut last_was_separator = true;
    for ch in body.chars() {
        if ch.is_ascii_digit() {
            digits += 1;
            last_was_separator = false;
            continue;
        }
        if NUMBER_SEPARATORS.contains(&ch) {
            // Two separators in a row, or a leading one, means this is not a plain
            // number but a list or a damaged cell.
            if last_was_separator {
                return false;
            }
            if ch == ',' || ch == '.' {
                decimal_marks += 1;
                // A second decimal mark means an enumeration (`1,2,3`) or a grouped
                // value in a convention this code does not get to guess about.
                if decimal_marks > 1 {
                    return false;
                }
            }
            last_was_separator = true;
            continue;
        }
        return false;
    }

    digits > 0 && !last_was_separator
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_unit_written_in_the_cell_is_recorded() {
        assert_eq!(detect_unit("120 мм", None).as_deref(), Some("мм"));
        assert_eq!(detect_unit("3,5 кН", None).as_deref(), Some("кН"));
        assert_eq!(detect_unit("1200 мм²", None).as_deref(), Some("мм²"));
    }

    #[test]
    fn a_unit_written_in_the_header_is_recorded_for_its_column() {
        assert_eq!(detect_unit("120", Some("Длина, мм")).as_deref(), Some("мм"));
        assert_eq!(
            detect_unit("3,5", Some("Нагрузка (кН)")).as_deref(),
            Some("кН")
        );
        assert_eq!(
            detect_unit("2", Some("Толщина [мм]")).as_deref(),
            Some("мм")
        );
    }

    #[test]
    fn no_unit_is_invented_when_none_is_written() {
        assert_eq!(detect_unit("120", None), None);
        assert_eq!(detect_unit("120", Some("Длина")), None);
        // "Length" without a unit does not silently become millimetres.
        assert_eq!(detect_unit("120", Some("Профиль")), None);
    }

    #[test]
    fn a_unit_is_never_carved_out_of_a_word() {
        // `Сумм` ends with `мм`, but the preceding character is a letter.
        assert_eq!(trailing_unit("Сумм"), None);
        assert_eq!(trailing_unit("Форм"), None);
    }

    #[test]
    fn single_numbers_are_recognised_with_either_decimal_mark() {
        for value in ["120", "3,5", "3.5", "-4", "1 200", "0,75"] {
            assert_eq!(
                classify(value),
                CellValueKind::Number,
                "`{value}` should read as a number"
            );
        }
        assert_eq!(classify("120 мм"), CellValueKind::Number);
    }

    #[test]
    fn ranges_designations_and_dashes_stay_text() {
        for value in ["40…60", "40-60", "BP 21/21D", "—", "н/д", "≥120", "1,2,3"] {
            assert_eq!(
                classify(value),
                CellValueKind::Text,
                "`{value}` must not be classified as a number"
            );
        }
    }

    #[test]
    fn a_blank_cell_stays_blank_and_never_becomes_zero() {
        for value in ["", "   ", "\t\n"] {
            assert_eq!(classify(value), CellValueKind::Empty);
            assert_ne!(classify(value), CellValueKind::Number);
        }
    }
}
