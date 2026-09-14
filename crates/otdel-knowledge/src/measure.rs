//! Reading a written quantity conservatively: what counts as a number, what counts as a
//! unit, and when two written values may be compared at all.
//!
//! This module is deliberately small and deliberately *pessimistic*. It exists because of
//! a real failure in the first BASIS run: a table heading — «безопасная рабочая нагрузка
//! (Н)» — was published as the **value** of the characteristic of the same name, and bare
//! numbers like `2,0/2,5` were published with no unit and no context. Both passed every
//! rule the phase had, because every rule asked "is this text in the quotation?" and both
//! texts were.
//!
//! So the questions asked here are different ones:
//!
//! | Question | Function | Why it is asked |
//! |---|---|---|
//! | is this value a restatement of its own property name? | [`restates_attribute`] | a column heading is not a measurement |
//! | does this value contain a real number? | [`has_number`] | a number is what needs a unit and a context |
//! | does the value carry its unit inside itself? | [`inline_unit`] | `3,5 кН` is complete; `3,5` is not |
//! | may these two written values be compared? | [`value_key`], [`units_comparable`] | `3,5` and `3.5` are one value; `кН` and `Н` are not one unit |
//! | which token identifies the product? | [`designation`] | `BP21` and `BP21D` must never merge |
//!
//! **What this is not.** It is not a units library, it converts nothing, and it
//! understands no physics. `кН` and `Н` are reported as *not comparable* rather than
//! converted, because a conversion nobody wrote down is a guess. Every uncertainty
//! resolves towards "cannot be compared", which downstream means "no confident numeric
//! answer" — never towards agreement.
//!
//! **Grouping separators are not read.** `1 200` is read as two numbers, not as `1200`,
//! because in a table row `3 500 250` is three columns. The cost is a comparison that
//! declines to conclude; the alternative cost is a wrong quantity.

/// Longest trailing fragment of a value still read as that value's unit.
///
/// A unit is short. Reading a whole sentence after a number as "the unit" would let any
/// prose vouch for a bare number being a complete measurement.
const MAX_INLINE_UNIT_CHARS: usize = 12;

/// Collapse whitespace, fold case and unify the dash and quote variants a text layer and
/// an OCR result legitimately disagree about.
///
/// Latin and Cyrillic look-alikes are **not** folded, for the reason 1C gives about
/// designations: `BC-21` and `ВС-21` are different products.
pub fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() || ch.is_control() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        match ch {
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2212}' => {
                out.push('-')
            }
            '«' | '»' | '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{2018}' | '\u{2019}' => {
                out.push('"')
            }
            other => {
                for folded in other.to_lowercase() {
                    out.push(folded);
                }
            }
        }
    }
    out
}

/// Every number written in `text`, in a normalised form that compares equal across the
/// spellings one document legitimately mixes.
///
/// `3,5` and `3.5` produce the same string; so do `2,0` and `2`. A number glued to the
/// **end** of a letter run is not a number here — the `21` of `BP21` and the `12` of `M12`
/// are parts of a designation, and treating them as quantities would make an answer
/// naming `BP21` look like an answer asserting the number 21.
pub fn numbers_in(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut index = 0usize;

    while index < chars.len() {
        if !chars[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < chars.len() {
            if chars[index].is_ascii_digit() {
                index += 1;
                continue;
            }
            // A decimal separator continues a number only when a digit follows it.
            if matches!(chars[index], '.' | ',')
                && chars
                    .get(index + 1)
                    .is_some_and(|next| next.is_ascii_digit())
            {
                index += 1;
                continue;
            }
            break;
        }
        let preceded_by_letter = start > 0 && chars[start - 1].is_alphabetic();
        if !preceded_by_letter {
            let raw: String = chars[start..index].iter().collect();
            out.push(normalise_number(&raw));
        }
    }

    out
}

/// Does this text state a number at all?
pub fn has_number(text: &str) -> bool {
    !numbers_in(text).is_empty()
}

/// One number, written the one way.
fn normalise_number(raw: &str) -> String {
    let unified = raw.replace(',', ".");
    let mut parts = unified.split('.');
    let (Some(integer), Some(fraction), None) = (parts.next(), parts.next(), parts.next()) else {
        // No separator, or several of them (a grouped spelling this module does not
        // claim to read). Left exactly as written, folded.
        return unified;
    };

    let integer = integer.trim_start_matches('0');
    let integer = if integer.is_empty() { "0" } else { integer };
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() {
        integer.to_owned()
    } else {
        format!("{integer}.{fraction}")
    }
}

/// The unit written *inside* the value, when there is one.
///
/// `3,5 кН` carries its unit and is a complete measurement even with an empty unit
/// column; `2,0/2,5` carries none and is a bare number whatever column it sits in.
pub fn inline_unit(value: &str) -> Option<String> {
    let chars: Vec<char> = value.chars().collect();
    let end_of_last_number = last_number_end(&chars)?;

    let tail: String = chars[end_of_last_number..].iter().collect();
    let tail = fold(tail.trim());
    if tail.is_empty() || tail.chars().count() > MAX_INLINE_UNIT_CHARS {
        return None;
    }
    let carries_a_symbol = tail
        .chars()
        .any(|ch| ch.is_alphabetic() || matches!(ch, '%' | '°' | '₽' | '$' | '€'));
    carries_a_symbol.then_some(tail)
}

/// Character index just past the last number of `chars`, if there is one.
fn last_number_end(chars: &[char]) -> Option<usize> {
    let mut index = 0usize;
    let mut last_end: Option<usize> = None;
    while index < chars.len() {
        if !chars[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        while index < chars.len() {
            if chars[index].is_ascii_digit() {
                index += 1;
                continue;
            }
            if matches!(chars[index], '.' | ',')
                && chars
                    .get(index + 1)
                    .is_some_and(|next| next.is_ascii_digit())
            {
                index += 1;
                continue;
            }
            break;
        }
        last_end = Some(index);
    }
    last_end
}

/// A value folded so two spellings of one written value compare equal.
///
/// Numbers are normalised (`2,0` = `2`), everything else is folded and its spacing
/// dropped, so `2,0 / 2,5` and `2.0/2.5` are one value. Nothing is converted: this
/// answers "is this the same thing written differently?", never "does this mean the same
/// quantity?".
pub fn value_key(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::with_capacity(chars.len());
    let mut index = 0usize;

    while index < chars.len() {
        if chars[index].is_ascii_digit() && !(index > 0 && chars[index - 1].is_alphabetic()) {
            let start = index;
            while index < chars.len() {
                if chars[index].is_ascii_digit() {
                    index += 1;
                    continue;
                }
                if matches!(chars[index], '.' | ',')
                    && chars
                        .get(index + 1)
                        .is_some_and(|next| next.is_ascii_digit())
                {
                    index += 1;
                    continue;
                }
                break;
            }
            let raw: String = chars[start..index].iter().collect();
            out.push_str(&normalise_number(&raw));
            continue;
        }

        let ch = chars[index];
        index += 1;
        if ch.is_whitespace() || ch.is_control() {
            continue;
        }
        out.push_str(&fold(&ch.to_string()));
    }

    out
}

/// May two units be compared without a conversion nobody wrote down?
///
/// Only identical spellings, once folded. `кН` and `Н` are **not** comparable, and that
/// is the point: the caller must then refuse to conclude that two values agree, rather
/// than convert them itself. A missing unit is comparable only with another missing one.
pub fn units_comparable(left: Option<&str>, right: Option<&str>) -> bool {
    fold(left.unwrap_or_default()) == fold(right.unwrap_or_default())
}

/// Is this "value" only the property's own name written again?
///
/// The failure this names: a table heading «безопасная рабочая нагрузка (Н)» stored as the
/// value of the characteristic «безопасная рабочая нагрузка (Н)». A parenthesised unit in
/// either of them changes nothing — `нагрузка (Н)` under `нагрузка` is the same heading.
pub fn restates_attribute(attribute: &str, value: &str) -> bool {
    let attribute = without_parentheses(attribute);
    let value = without_parentheses(value);
    !attribute.is_empty() && attribute == value
}

/// Fold, and drop parenthesised fragments, which is where a heading keeps its unit.
fn without_parentheses(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    for ch in text.chars() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            other if depth == 0 => out.push(other),
            _ => {}
        }
    }
    fold(&out)
}

/// Does `haystack` use a word that begins with `stem`?
///
/// A plain substring test is wrong for this and was a real defect: «вес» is inside
/// «известно», so «неизвестно нечто» read as a statement about weight. A stem has to start
/// a word, which is what a stem *is*. Multi-word stems ("lead time") already carry their
/// own boundaries and are matched whole.
///
/// `haystack` is expected to be [`fold`]ed already; the caller usually folds once and asks
/// many times.
pub fn mentions(haystack: &str, stem: &str) -> bool {
    if stem.contains(' ') {
        return haystack.contains(stem);
    }
    haystack
        .split(|ch: char| !ch.is_alphanumeric())
        .any(|word| word.starts_with(stem))
}

/// The token of a name that identifies the product, for looking it up in a quotation.
///
/// A catalogue writes «Профиль монтажный BP21» in a section heading and `BP21` in the
/// table row. The designation is the part that survives both: the last token carrying
/// letters *and* digits. When a name has no such token, the whole folded name is the
/// designation — a name is never shortened into something less specific, because
/// `BP21` and `BP21D` becoming one product is exactly the failure this guards.
pub fn designation(name: &str) -> String {
    let folded = fold(name);
    folded
        .split(|ch: char| !ch.is_alphanumeric() && ch != '-')
        .rfind(|token| {
            token.chars().any(|ch| ch.is_alphabetic()) && token.chars().any(|ch| ch.is_numeric())
        })
        .map(str::to_owned)
        .unwrap_or(folded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_number_written_two_ways_is_one_number() {
        assert_eq!(numbers_in("3,5"), vec!["3.5".to_owned()]);
        assert_eq!(numbers_in("3.50"), vec!["3.5".to_owned()]);
        assert_eq!(numbers_in("2,0"), vec!["2".to_owned()]);
        assert_eq!(numbers_in("03.5"), vec!["3.5".to_owned()]);
    }

    #[test]
    fn a_designations_digits_are_not_a_quantity() {
        // Otherwise an answer naming BP21 would look like an answer asserting 21.
        assert!(numbers_in("BP21").is_empty());
        assert!(numbers_in("M12").is_empty());
        assert_eq!(numbers_in("BP21 1200 3.5 kN"), vec!["1200", "3.5"]);
    }

    #[test]
    fn a_grouped_number_is_not_silently_joined() {
        // In a table row `3 500 250` is three columns, not one number.
        assert_eq!(numbers_in("1 200"), vec!["1".to_owned(), "200".to_owned()]);
    }

    #[test]
    fn a_value_carrying_its_own_unit_is_told_apart_from_a_bare_number() {
        assert_eq!(inline_unit("3,5 кН").as_deref(), Some("кн"));
        assert_eq!(inline_unit("60 °C").as_deref(), Some("°c"));
        assert_eq!(inline_unit("12мм").as_deref(), Some("мм"));
        // The BASIS case: a bare pair of numbers with nothing saying what they are.
        assert_eq!(inline_unit("2,0/2,5"), None);
        assert_eq!(inline_unit("1200"), None);
        // A whole sentence after a number is not a unit.
        assert_eq!(inline_unit("5 в зависимости от схемы опирания"), None);
    }

    #[test]
    fn two_spellings_of_one_value_share_a_key_and_two_values_do_not() {
        assert_eq!(value_key("2,0/2,5"), value_key("2.0 / 2.5"));
        assert_eq!(value_key(" 3,50 "), value_key("3.5"));
        assert_ne!(value_key("3.5"), value_key("4.2"));
        assert_ne!(value_key("BP21"), value_key("BP21D"));
    }

    #[test]
    fn units_are_never_converted_only_recognised_as_the_same_spelling() {
        assert!(units_comparable(Some("кН"), Some("кн")));
        assert!(units_comparable(None, None));
        assert!(!units_comparable(Some("кН"), Some("Н")));
        assert!(!units_comparable(Some("кН"), None));
    }

    #[test]
    fn a_heading_repeated_as_its_own_value_is_recognised() {
        assert!(restates_attribute(
            "безопасная рабочая нагрузка (Н)",
            "безопасная рабочая нагрузка (Н)"
        ));
        assert!(restates_attribute("нагрузка", "Нагрузка (Н)"));
        // A real value that merely starts with the property name is not a heading.
        assert!(!restates_attribute(
            "покрытие",
            "покрытие горячим цинкованием"
        ));
        assert!(!restates_attribute("нагрузка", "3,5 кН"));
    }

    #[test]
    fn a_designation_is_the_specific_token_and_is_never_shortened() {
        assert_eq!(designation("Профиль монтажный BP21"), "bp21");
        assert_eq!(designation("BP21D"), "bp21d");
        assert_ne!(designation("BP21"), designation("BP21D"));
        // Nothing letter-and-digit in the name: the whole name stays the designation.
        assert_eq!(designation("Консоль опорная"), "консоль опорная");
    }
}
