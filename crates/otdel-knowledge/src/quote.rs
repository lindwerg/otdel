//! Locating a quote in the page it claims to come from.
//!
//! This is the mechanism behind the phase's central rule: *a citation is a citation*.
//! The model returns a quote; the server does not store it. It stores the fragment of
//! the **page text** that the quote matched, together with the character offsets where
//! it sits. A model that paraphrases, translates, rounds a number or invents a sentence
//! produces no match, and the candidate is refused.
//!
//! Matching is done on a normalised copy of both strings — whitespace runs collapsed,
//! case folded, dash and quote variants unified — because a PDF text layer and an OCR
//! result legitimately differ from the visible page in exactly those ways. The
//! normalisation keeps a one-to-one character mapping back to the original, which is
//! what lets the *unmodified* source fragment be recovered afterwards.

/// A fragment of a page, as the page itself writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuoteMatch {
    /// The verbatim substring of the page text.
    pub text: String,
    /// Character offsets into the page text (not bytes).
    pub char_start: usize,
    pub char_end: usize,
}

/// Length above which a quote is accepted on sight.
///
/// The danger a floor guards against is a *trivial* match: "12" occurs somewhere on
/// almost any technical page, so accepting it would make the citation meaningless.
/// Length is only a proxy for that, though, and a blunt floor refuses real evidence —
/// against the BASIS catalogue it threw away short table values like "300 мм" that
/// the model had quoted correctly.
///
/// So a shorter quote is not refused outright: it is accepted when it occurs **exactly
/// once** on the page ([`SearchableText::locate`]). Uniqueness is the property that
/// actually makes a citation identifiable, and it is strictly stronger than length for
/// the case the floor was written for — "12" appearing twice is refused, "300 мм"
/// appearing once is not.
pub const MIN_QUOTE_CHARS: usize = 8;
/// Longest quote stored. A whole page is not a citation.
pub const MAX_QUOTE_CHARS: usize = 600;

/// Why a quote could not be accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuoteRejection {
    TooShort,
    TooLong,
    NoLetters,
    NotFound,
}

impl QuoteRejection {
    pub const fn reason(self) -> &'static str {
        match self {
            Self::TooShort => {
                "цитата слишком короткая и встречается на странице не один раз — \
                 по ней нельзя однозначно указать место"
            }
            Self::TooLong => "цитата длиннее допустимой",
            Self::NoLetters => "в цитате нет ни одной буквы или цифры",
            Self::NotFound => "цитата не найдена дословно на указанной странице",
        }
    }
}

/// A page text prepared once, so many quotes can be checked against it cheaply.
#[derive(Debug, Clone)]
pub struct SearchableText {
    /// The page text exactly as stored.
    original: Vec<char>,
    /// Normalised form used for matching.
    normalised: String,
    /// `offsets[i]` is the character index in `original` that produced
    /// `normalised`'s `i`-th character.
    offsets: Vec<usize>,
}

impl SearchableText {
    pub fn new(text: &str) -> Self {
        let original: Vec<char> = text.chars().collect();
        let mut normalised = String::with_capacity(original.len());
        let mut offsets = Vec::with_capacity(original.len());

        let mut pending_space: Option<usize> = None;
        for (index, ch) in original.iter().copied().enumerate() {
            if ch.is_whitespace() {
                // Remember one space; emit it only if real content follows, so a page
                // never ends with a trailing normalised space that offsets nothing.
                if !normalised.is_empty() && pending_space.is_none() {
                    pending_space = Some(index);
                }
                continue;
            }
            if let Some(space_index) = pending_space.take() {
                normalised.push(' ');
                offsets.push(space_index);
            }
            normalised.push(fold(ch));
            offsets.push(index);
        }

        Self {
            original,
            normalised,
            offsets,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.normalised.is_empty()
    }

    /// Find `quote` in this page and return the page's own wording of it.
    ///
    /// Returns the *first* occurrence: a value that appears twice on a page is still
    /// genuinely on that page, and picking the first keeps the result deterministic.
    pub fn locate(&self, quote: &str) -> Result<QuoteMatch, QuoteRejection> {
        let trimmed = quote.trim();
        if trimmed.chars().count() > MAX_QUOTE_CHARS {
            return Err(QuoteRejection::TooLong);
        }
        if !trimmed.chars().any(char::is_alphanumeric) {
            return Err(QuoteRejection::NoLetters);
        }

        // The length that matters is the *normalised* one: `"A      B"` is eight
        // characters but a three-character needle, and padding a short value with
        // spaces must not buy it past the floor.
        let needle = normalise(trimmed);
        if needle.is_empty() {
            return Err(QuoteRejection::NoLetters);
        }

        let byte_position = self
            .normalised
            .find(&needle)
            .ok_or(QuoteRejection::NotFound)?;

        // Below the floor, the quote has to be unambiguous instead: exactly one
        // occurrence on the page. A short fragment that appears twice does not point
        // at a place, and "цитата" that could be either is not a citation.
        if needle.chars().count() < MIN_QUOTE_CHARS && self.occurrences(&needle) != 1 {
            return Err(QuoteRejection::TooShort);
        }

        // `find` gives a byte offset; the offset table is indexed by characters.
        let char_position = self.normalised[..byte_position].chars().count();
        let needle_chars = needle.chars().count();
        let last = char_position + needle_chars - 1;

        let start = self.offsets[char_position];
        let end = self.offsets[last] + 1;

        // The span in the *original* can be much longer than the normalised needle —
        // a column-padded table line collapses to a fraction of its width. What is
        // stored is that original span, so it is what has to fit the limit (and the
        // database's own CHECK); refusing here keeps a legitimate-looking quote from
        // aborting the whole draft at INSERT time.
        if end - start > MAX_QUOTE_CHARS {
            return Err(QuoteRejection::TooLong);
        }

        let text: String = self.original[start..end].iter().collect();

        Ok(QuoteMatch {
            text,
            char_start: start,
            char_end: end,
        })
    }

    /// How many times the normalised needle occurs, counted without overlaps.
    fn occurrences(&self, needle: &str) -> usize {
        if needle.is_empty() {
            return 0;
        }
        self.normalised.matches(needle).count()
    }

    /// Is `needle` present in this text as a whole token?
    ///
    /// Used to confirm a *value* or a *unit* inside an already-accepted quotation,
    /// where [`Self::locate`]'s minimum length would be wrong — "3.5" is a perfectly
    /// good value — but a bare substring test would be too weak: `5` must not be
    /// confirmed by the `5` inside `1500`. A match therefore has to be flanked by
    /// something that is not a letter or a digit.
    pub fn contains_token(&self, needle: &str) -> bool {
        let needle = normalise(needle.trim());
        if needle.is_empty() {
            return false;
        }

        let haystack: Vec<char> = self.normalised.chars().collect();
        let needle: Vec<char> = needle.chars().collect();
        if needle.len() > haystack.len() {
            return false;
        }

        let first = needle[0];
        let last = needle[needle.len() - 1];

        (0..=haystack.len() - needle.len()).any(|start| {
            if haystack[start..start + needle.len()] != needle[..] {
                return false;
            }
            let after = start + needle.len();
            let before_ok = start == 0 || !continues_token(haystack[start - 1], first);
            let after_ok = after == haystack.len() || !continues_token(haystack[after], last);
            // The boundary rule only applies at an end where the needle itself has a
            // letter or a digit: a needle starting with "(" needs no separator before it.
            (before_ok || !first.is_alphanumeric()) && (after_ok || !last.is_alphanumeric())
        })
    }
}

/// Would `neighbour` extend a token that ends (or starts) with `edge`?
///
/// Letters and digits always continue a token. A decimal separator continues a
/// *number*, which is what keeps `5` from being confirmed by the `5` in `3.5`
/// — the case that matters most, because a load is a number.
fn continues_token(neighbour: char, edge: char) -> bool {
    if neighbour.is_alphanumeric() {
        return true;
    }
    edge.is_ascii_digit() && matches!(neighbour, '.' | ',')
}

/// Is `needle` written, as a whole token, inside `haystack`?
pub fn contains_token(haystack: &str, needle: &str) -> bool {
    SearchableText::new(haystack).contains_token(needle)
}

/// Collapse whitespace and fold the characters that legitimately vary between a
/// rendered page, its text layer and a recognition result.
fn normalise(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !out.is_empty() && !in_space {
                out.push(' ');
                in_space = true;
            }
            continue;
        }
        in_space = false;
        out.push(fold(ch));
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

/// One character in, one character out — the mapping back to the original depends on it.
fn fold(ch: char) -> char {
    match ch {
        // Dash variants: hyphen, non-breaking hyphen, figure/en/em dash, minus sign.
        '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2212}' => '-',
        // Quote variants, including the Russian « » and the typographic pairs.
        '«' | '»' | '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{2018}' | '\u{2019}' => '"',
        // Latin/Cyrillic homoglyph folding is deliberately *not* done: «С» and «C» are
        // different characters in a part number, and treating them as equal would let
        // a wrong designation match.
        other => other.to_lowercase().next().unwrap_or(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "BASIS mounting systems\n\nProfile load table\nBP21   1200   3.5 kN\nBP21D  1500   4.2 kN\nНагрузка приведена при опирании на две опоры.";

    #[test]
    fn a_real_quote_is_found_and_returned_in_the_pages_own_wording() {
        let page = SearchableText::new(PAGE);
        // The model writes the row with single spaces; the page uses several.
        let found = page.locate("BP21 1200 3.5 kN").unwrap();
        assert_eq!(found.text, "BP21   1200   3.5 kN");
        assert_eq!(
            &PAGE.chars().collect::<Vec<_>>()[found.char_start..found.char_end]
                .iter()
                .collect::<String>(),
            "BP21   1200   3.5 kN"
        );
    }

    #[test]
    fn case_and_line_breaks_do_not_prevent_a_match() {
        let page = SearchableText::new(PAGE);
        assert!(page.locate("basis mounting systems").is_ok());
        assert!(page.locate("Profile load table BP21").is_ok());
    }

    #[test]
    fn an_invented_quote_is_not_found() {
        let page = SearchableText::new(PAGE);
        for invented in [
            "BP21 выдерживает 10 kN",
            "Цена профиля 1200 рублей",
            "BP99   1200   3.5 kN",
        ] {
            assert_eq!(
                page.locate(invented),
                Err(QuoteRejection::NotFound),
                "{invented:?} must not be accepted as a quote"
            );
        }
    }

    #[test]
    fn a_rounded_number_is_a_different_number() {
        let page = SearchableText::new(PAGE);
        // 3.5 → 4 is exactly the kind of "helpful" change that must fail.
        assert_eq!(page.locate("BP21 1200 4 kN"), Err(QuoteRejection::NotFound));
    }

    #[test]
    fn quotes_that_are_too_short_or_meaningless_are_refused() {
        let page = SearchableText::new(PAGE);
        // "kN" ends both load rows: too short *and* ambiguous, so there is no place
        // it points at.
        assert_eq!(page.locate("kN"), Err(QuoteRejection::TooShort));
        // A short fragment that occurs once is fine — that is the whole distinction.
        assert_eq!(page.locate("BP21D").unwrap().text, "BP21D");
        assert_eq!(page.locate("  "), Err(QuoteRejection::NoLetters));
        assert_eq!(page.locate("--- ... ---"), Err(QuoteRejection::NoLetters));
        let long = "a".repeat(MAX_QUOTE_CHARS + 1);
        assert_eq!(page.locate(&long), Err(QuoteRejection::TooLong));
    }

    #[test]
    fn dash_and_quote_variants_are_folded_but_letters_are_not_transliterated() {
        let page = SearchableText::new("Профиль «BP21» — длина 1200 мм");
        assert!(page.locate("Профиль \"BP21\" - длина 1200 мм").is_ok());
        // The returned text is the page's own, with its original typography.
        let found = page.locate("\"BP21\" - длина").unwrap();
        assert_eq!(found.text, "«BP21» — длина");

        // A Latin `C` must not match a Cyrillic `С` in a designation.
        let designation = SearchableText::new("Артикул BC-21 доступен");
        assert_eq!(
            designation.locate("Артикул ВС-21 доступен"),
            Err(QuoteRejection::NotFound)
        );
    }

    #[test]
    fn offsets_point_at_the_quote_inside_the_original_text() {
        let page = SearchableText::new(PAGE);
        let found = page.locate("Нагрузка приведена").unwrap();
        let chars: Vec<char> = PAGE.chars().collect();
        assert_eq!(
            chars[found.char_start..found.char_end]
                .iter()
                .collect::<String>(),
            "Нагрузка приведена"
        );
        assert!(found.char_end <= chars.len());
    }

    #[test]
    fn a_token_search_confirms_a_value_without_confirming_a_fragment_of_one() {
        let quote = SearchableText::new("BP21   1200   3.5 kN при опирании");

        // The values really written there.
        assert!(quote.contains_token("3.5"));
        assert!(quote.contains_token("1200"));
        assert!(quote.contains_token("kN"));
        assert!(quote.contains_token("BP21"));
        // Whitespace differences do not matter, as everywhere else.
        assert!(quote.contains_token("1200 3.5"));

        // Pieces of a number are not the number.
        assert!(
            !quote.contains_token("5"),
            "`5` is part of `3.5`, not a value"
        );
        assert!(!quote.contains_token("3"));
        assert!(!quote.contains_token("120"));
        // Nor is a longer number that merely starts the same way.
        assert!(!quote.contains_token("3.55"));
        // Nor a word that only appears inside another.
        assert!(!quote.contains_token("опира"));
        assert!(!quote.contains_token("BP2"));
        assert!(!quote.contains_token(""));
    }

    #[test]
    fn a_short_quote_is_accepted_when_it_is_the_only_one_on_the_page() {
        // Real table values are short. Refusing them by length alone threw away
        // correctly quoted facts from the BASIS catalogue; what matters is whether the
        // fragment identifies a place.
        let page = SearchableText::new("Профиль BP21\nДлина 2000 мм\nНагрузка 3.5 kN");
        let found = page.locate("2000 мм").unwrap();
        assert_eq!(found.text, "2000 мм");
        assert_eq!(
            page.locate("3.5 kN").unwrap().text,
            "3.5 kN",
            "a unique short value is evidence"
        );

        // The same fragment twice points at nothing in particular.
        let repeated = SearchableText::new("BP21 300 мм\nBP41 300 мм");
        assert_eq!(repeated.locate("300 мм"), Err(QuoteRejection::TooShort));

        // A long quote never needs the uniqueness rule: the first occurrence is
        // deterministic and identifiable.
        let twice = SearchableText::new("Консоль монтажная BP\nКонсоль монтажная BP");
        assert!(twice.locate("Консоль монтажная BP").is_ok());
    }

    #[test]
    fn an_empty_page_matches_nothing() {
        let page = SearchableText::new("   \n\t  ");
        assert!(page.is_empty());
        assert_eq!(
            page.locate("anything at all"),
            Err(QuoteRejection::NotFound)
        );
    }
}
