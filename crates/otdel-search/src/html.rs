//! Turning a downloaded page into the plain text that becomes its stored snapshot.
//!
//! This is a *reader*, not a renderer, and the difference decides every choice here. It
//! never executes anything, never resolves a reference, never follows a link and never
//! asks the network for a stylesheet, a script or an image — the bytes that arrived are
//! all it ever sees. What comes out is text with collapsed whitespace, bounded length,
//! and no markup at all, because that text is what a finding's quotation is matched
//! against character by character (`otdel_research::validate`).
//!
//! Script and style bodies are dropped rather than flattened: a `<script>` full of JSON
//! is not something the page *says*, and letting a model quote it would attach evidence
//! to code. Comments go the same way.
//!
//! Two small pieces of metadata are read, and only when the page states them literally:
//! the `<title>`, and a licence declared as `<link rel="license">` or
//! `<meta name="license">`. A missing licence stays missing — `null` here means "not
//! stated", never "free to use".

use chrono::{DateTime, Utc};

/// What a page turned out to say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractedPage {
    pub text: String,
    pub title: Option<String>,
    /// Only when the document declares one. Never inferred from the host or the text.
    pub license: Option<String>,
    /// Where the licence was read from, or why none is recorded. Shown as-is.
    pub license_note: Option<String>,
    /// Only when the document states one in a machine-readable field.
    pub published_at: Option<DateTime<Utc>>,
    /// `true` when the text hit `max_chars` and the rest was not kept.
    pub truncated: bool,
}

/// Tags whose content is not prose and must not end up quotable.
const DROPPED_ELEMENTS: [&str; 6] = ["script", "style", "noscript", "template", "svg", "iframe"];

/// Tags that separate blocks of text; each becomes a line break.
const BLOCK_ELEMENTS: [&str; 24] = [
    "p",
    "div",
    "br",
    "li",
    "tr",
    "td",
    "th",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "section",
    "article",
    "header",
    "footer",
    "table",
    "ul",
    "ol",
    "dl",
    "dt",
    "dd",
    "blockquote",
];

/// Read a page. `max_chars` bounds the stored text, in characters.
pub fn extract(body: &str, max_chars: usize) -> ExtractedPage {
    let title = tag_text(body, "title").map(|value| clip(&collapse(&decode_entities(&value)), 300));
    let license = find_license(body);
    let published_at = find_published_at(body);

    let stripped = strip_markup(body);
    let text = collapse(&decode_entities(&stripped));
    let truncated = text.chars().count() > max_chars;
    let text = if truncated {
        text.chars().take(max_chars).collect()
    } else {
        text
    };

    ExtractedPage {
        text,
        title: title.filter(|value| !value.is_empty()),
        license_note: Some(match &license {
            Some(_) => "лицензия объявлена в самой странице (link rel=\"license\" или meta name=\"license\")".to_owned(),
            None => "страница не объявляет лицензию; условия использования не установлены".to_owned(),
        }),
        license,
        published_at,
        truncated,
    }
}

/// Plain text that is not HTML at all: the same collapsing and the same bound.
pub fn extract_plain(body: &str, max_chars: usize) -> ExtractedPage {
    let text = collapse(body);
    let truncated = text.chars().count() > max_chars;
    ExtractedPage {
        text: if truncated {
            text.chars().take(max_chars).collect()
        } else {
            text
        },
        title: None,
        license: None,
        license_note: Some(
            "текстовый документ не объявляет лицензию; условия использования не установлены"
                .to_owned(),
        ),
        published_at: None,
        truncated,
    }
}

/// Remove comments, dropped elements and every remaining tag.
fn strip_markup(body: &str) -> String {
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::with_capacity(chars.len() / 2);
    let mut index = 0usize;

    while index < chars.len() {
        if chars[index] != '<' {
            out.push(chars[index]);
            index += 1;
            continue;
        }

        // A comment, a CDATA section or a doctype — never content.
        if starts_with_at(&chars, index, "<!--") {
            index =
                find_from(&chars, index + 4, "-->").map_or(chars.len(), |position| position + 3);
            continue;
        }
        if starts_with_at(&chars, index, "<!") || starts_with_at(&chars, index, "<?") {
            index = find_from(&chars, index, ">").map_or(chars.len(), |position| position + 1);
            continue;
        }

        let Some(tag_end) = find_from(&chars, index, ">") else {
            // An unterminated `<` at the end of the body: the remainder is not markup
            // and not prose either. Dropping it is the choice that cannot invent text.
            break;
        };
        let tag: String = chars[index + 1..tag_end].iter().collect();
        let name = tag_name(&tag);

        if let Some(name) = name.as_deref() {
            if DROPPED_ELEMENTS.contains(&name) && !tag.starts_with('/') {
                // Skip to the matching close tag, or to the end if there is none.
                let close = format!("</{name}");
                index = match find_from_ignore_case(&chars, tag_end, &close) {
                    Some(position) => {
                        find_from(&chars, position, ">").map_or(chars.len(), |end| end + 1)
                    }
                    None => chars.len(),
                };
                out.push('\n');
                continue;
            }
            if BLOCK_ELEMENTS.contains(&name) {
                out.push('\n');
            } else {
                // An inline tag still separates words: `a<b>b</b>` is two tokens in the
                // rendered page, and gluing them would create a word nobody wrote.
                out.push(' ');
            }
        }

        index = tag_end + 1;
    }

    out
}

/// The content of the first `<tag>…</tag>`, with markup left in place.
fn tag_text(body: &str, tag: &str) -> Option<String> {
    // ASCII-only folding: it is length-preserving byte for byte, which is what makes the
    // offsets below safe to apply to `body`. Tag and attribute names are ASCII by
    // definition, so nothing is lost.
    let lower = body.to_ascii_lowercase();
    let open = lower.find(&format!("<{tag}"))?;
    let content_start = open + lower[open..].find('>')?;
    let close = lower[content_start..].find(&format!("</{tag}"))? + content_start;
    Some(body[content_start + 1..close].to_owned())
}

/// A licence the document declares about itself.
fn find_license(body: &str) -> Option<String> {
    for tag in tags_named(body, "link") {
        if attribute(&tag, "rel").is_some_and(|rel| {
            rel.to_ascii_lowercase()
                .split_whitespace()
                .any(|token| token == "license" || token == "licence")
        }) {
            if let Some(href) = attribute(&tag, "href").filter(|value| !value.trim().is_empty()) {
                return Some(clip(&collapse(&decode_entities(&href)), 300));
            }
        }
    }
    for tag in tags_named(body, "meta") {
        let names = [attribute(&tag, "name"), attribute(&tag, "property")];
        let declares_license = names.iter().flatten().any(|value| {
            let value = value.to_ascii_lowercase();
            value == "license" || value == "licence" || value == "dc.rights"
        });
        if declares_license {
            if let Some(content) = attribute(&tag, "content").filter(|v| !v.trim().is_empty()) {
                return Some(clip(&collapse(&decode_entities(&content)), 300));
            }
        }
    }
    None
}

/// A publication date the document states in a machine-readable field.
///
/// Only an explicit, parseable timestamp counts. A date guessed from a URL or from prose
/// would be this phase inventing provenance.
fn find_published_at(body: &str) -> Option<DateTime<Utc>> {
    for tag in tags_named(body, "meta") {
        let names = [attribute(&tag, "property"), attribute(&tag, "name")];
        let is_published = names.iter().flatten().any(|value| {
            let value = value.to_ascii_lowercase();
            value == "article:published_time" || value == "datepublished" || value == "date"
        });
        if is_published {
            if let Some(parsed) = attribute(&tag, "content").and_then(|value| parse_date(&value)) {
                return Some(parsed);
            }
        }
    }
    for tag in tags_named(body, "time") {
        if let Some(parsed) = attribute(&tag, "datetime").and_then(|value| parse_date(&value)) {
            return Some(parsed);
        }
    }
    None
}

fn parse_date(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed.with_timezone(&Utc));
    }
    // A bare `2024-05-17` is a date the page really states; midnight UTC is the only
    // reading that adds nothing.
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|naive| naive.and_utc())
}

/// Every `<name …>` tag in the body, as raw tag text.
fn tags_named(body: &str, name: &str) -> Vec<String> {
    // Length-preserving, for the same reason as `tag_text`: these offsets index `body`.
    let lower = body.to_ascii_lowercase();
    let needle = format!("<{name}");
    let mut found = Vec::new();
    let mut cursor = 0usize;

    while let Some(offset) = lower[cursor..].find(&needle) {
        let start = cursor + offset;
        // `<linkage>` is not `<link>`.
        let after = lower[start + needle.len()..].chars().next();
        if !matches!(after, Some(ch) if ch.is_ascii_alphanumeric()) {
            if let Some(end) = lower[start..].find('>') {
                found.push(body[start..start + end].to_owned());
                cursor = start + end;
                if found.len() >= 200 {
                    break;
                }
                continue;
            }
            break;
        }
        cursor = start + needle.len();
    }

    found
}

/// The value of `name="…"` (or `name='…'`) inside one tag.
fn attribute(tag: &str, name: &str) -> Option<String> {
    // Length-preserving, for the same reason as `tag_text`: these offsets index `tag`.
    let lower = tag.to_ascii_lowercase();
    let mut cursor = 0usize;

    while let Some(offset) = lower[cursor..].find(name) {
        let start = cursor + offset;
        cursor = start + name.len();
        // Must be a whole attribute name: preceded by whitespace, followed by `=`.
        let before_ok = start == 0
            || lower[..start]
                .chars()
                .next_back()
                .is_some_and(|ch| ch.is_whitespace());
        let rest = lower[cursor..].trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let value_start = cursor + lower[cursor..].find('=')? + 1;
        let value = tag[value_start..].trim_start();
        let quote = value.chars().next()?;
        return if quote == '"' || quote == '\'' {
            value[1..]
                .find(quote)
                .map(|end| value[1..1 + end].to_owned())
        } else {
            Some(
                value
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
            )
        };
    }

    None
}

/// The handful of entities that actually appear in prose, plus numeric references.
fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0usize;

    while index < chars.len() {
        if chars[index] != '&' {
            out.push(chars[index]);
            index += 1;
            continue;
        }
        let Some(end) = chars[index..]
            .iter()
            .take(12)
            .position(|ch| *ch == ';')
            .map(|position| index + position)
        else {
            out.push('&');
            index += 1;
            continue;
        };

        let entity: String = chars[index + 1..end].iter().collect();
        let replacement = match entity.as_str() {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" | "#160" => Some(' '),
            "laquo" => Some('«'),
            "raquo" => Some('»'),
            "mdash" => Some('—'),
            "ndash" => Some('–'),
            "deg" => Some('°'),
            other => other
                .strip_prefix('#')
                .and_then(|digits| match digits.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => digits.parse::<u32>().ok(),
                })
                .and_then(char::from_u32)
                // A decoded control character would corrupt the snapshot the quote is
                // matched against, so it becomes a space.
                .map(|ch| if ch.is_control() { ' ' } else { ch }),
        };

        match replacement {
            Some(ch) => {
                out.push(ch);
                index = end + 1;
            }
            None => {
                out.push('&');
                index += 1;
            }
        }
    }

    out
}

/// Collapse runs of whitespace, keeping paragraph breaks as single newlines.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    let mut pending_newline = false;

    for ch in text.chars() {
        if ch == '\n' || ch == '\r' {
            pending_newline = true;
            continue;
        }
        if ch.is_whitespace() {
            pending_space = true;
            continue;
        }
        if ch.is_control() {
            continue;
        }
        if !out.is_empty() {
            if pending_newline {
                out.push('\n');
            } else if pending_space {
                out.push(' ');
            }
        }
        pending_space = false;
        pending_newline = false;
        out.push(ch);
    }

    out
}

fn clip(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

fn starts_with_at(chars: &[char], index: usize, needle: &str) -> bool {
    needle
        .chars()
        .enumerate()
        .all(|(offset, ch)| chars.get(index + offset) == Some(&ch))
}

fn find_from(chars: &[char], from: usize, needle: &str) -> Option<usize> {
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() || from >= chars.len() {
        return None;
    }
    (from..=chars.len().saturating_sub(needle.len()))
        .find(|start| chars[*start..*start + needle.len()] == needle[..])
}

fn find_from_ignore_case(chars: &[char], from: usize, needle: &str) -> Option<usize> {
    // ASCII folding, matching the comparison below. (This one walks characters rather
    // than bytes, so it is not offset-sensitive — but a needle whose `to_lowercase` grew
    // a character would still never match, so the two halves are kept consistent.)
    let needle: Vec<char> = needle.to_ascii_lowercase().chars().collect();
    if needle.is_empty() || from >= chars.len() {
        return None;
    }
    (from..=chars.len().saturating_sub(needle.len())).find(|start| {
        chars[*start..*start + needle.len()]
            .iter()
            .zip(needle.iter())
            .all(|(left, right)| left.to_ascii_lowercase() == *right)
    })
}

/// The name of a tag written as `p`, `/p`, `p class="x"` or `br/`.
fn tag_name(tag: &str) -> Option<String> {
    let body = tag.strip_prefix('/').unwrap_or(tag);
    let name: String = body
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_becomes_the_text_it_shows() {
        let page = extract(
            "<html><head><title>ГОСТ 9.307</title></head><body>\
             <h1>Покрытия цинковые</h1>\
             <p>Минимальная толщина покрытия&nbsp;&mdash; 55&nbsp;мкм.</p>\
             </body></html>",
            10_000,
        );

        assert_eq!(page.title.as_deref(), Some("ГОСТ 9.307"));
        assert!(page.text.contains("Покрытия цинковые"));
        assert!(
            page.text.contains("Минимальная толщина покрытия — 55 мкм."),
            "{:?}",
            page.text
        );
        assert!(!page.truncated);
        // Nothing on this page declared a licence, so none is claimed.
        assert!(page.license.is_none());
        assert!(page.license_note.is_some());
    }

    #[test]
    fn scripts_styles_and_comments_never_become_quotable_text() {
        let page = extract(
            "<body><script>var secret = \"НЕ ЦИТАТА\";</script>\
             <style>.a{content:\"тоже не цитата\"}</style>\
             <!-- и это не цитата -->\
             <noscript>включите javascript</noscript>\
             <p>Настоящий текст страницы.</p></body>",
            10_000,
        );

        assert!(page.text.contains("Настоящий текст страницы."));
        for forbidden in [
            "НЕ ЦИТАТА",
            "тоже не цитата",
            "и это не цитата",
            "включите javascript",
            "var secret",
        ] {
            assert!(
                !page.text.contains(forbidden),
                "`{forbidden}` must not be quotable: {:?}",
                page.text
            );
        }
    }

    #[test]
    fn markup_never_glues_two_words_into_one() {
        // A model quoting "сталь" must not be confirmed by a "сталь" that only exists
        // because a tag between two words disappeared.
        let page = extract("<p>оцинкованная<b>сталь</b></p>", 10_000);
        assert!(page.text.contains("оцинкованная сталь"), "{:?}", page.text);
        assert!(!page.text.contains("оцинкованнаясталь"));
    }

    #[test]
    fn a_declared_licence_is_recorded_and_an_undeclared_one_is_not_invented() {
        let declared = extract(
            "<head><link rel=\"license\" href=\"https://creativecommons.org/licenses/by/4.0/\"></head>\
             <body><p>текст</p></body>",
            10_000,
        );
        assert_eq!(
            declared.license.as_deref(),
            Some("https://creativecommons.org/licenses/by/4.0/")
        );

        let meta = extract(
            "<head><meta name=\"license\" content=\"CC BY-SA 4.0\"></head><body>x</body>",
            10_000,
        );
        assert_eq!(meta.license.as_deref(), Some("CC BY-SA 4.0"));

        let silent = extract("<body><p>Все права защищены.</p></body>", 10_000);
        assert!(
            silent.license.is_none(),
            "prose about rights is not a declared licence"
        );
        assert!(silent.license_note.unwrap().contains("не объявляет"));
    }

    #[test]
    fn a_publication_date_is_read_only_when_the_page_states_one() {
        let stated = extract(
            "<head><meta property=\"article:published_time\" content=\"2024-05-17T10:00:00Z\"></head><body>x</body>",
            1_000,
        );
        assert_eq!(
            stated.published_at.map(|value| value.to_rfc3339()),
            Some("2024-05-17T10:00:00+00:00".to_owned())
        );

        let bare_date = extract(
            "<body><time datetime=\"2024-05-17\">17 мая</time></body>",
            1_000,
        );
        assert!(bare_date.published_at.is_some());

        // A date in prose is not a stated publication date.
        let prose = extract("<body><p>Опубликовано 17 мая 2024 года</p></body>", 1_000);
        assert!(prose.published_at.is_none());

        // Neither is an unparseable one.
        let broken = extract(
            "<head><meta name=\"date\" content=\"вчера\"></head><body>x</body>",
            1_000,
        );
        assert!(broken.published_at.is_none());
    }

    #[test]
    fn the_snapshot_is_bounded_and_says_when_it_was_cut() {
        let body = format!("<p>{}</p>", "а".repeat(5_000));
        let page = extract(&body, 100);
        assert_eq!(page.text.chars().count(), 100);
        assert!(page.truncated);
    }

    #[test]
    fn malformed_markup_does_not_produce_text_nobody_wrote() {
        // An unterminated tag at the end of the body.
        let page = extract("<p>настоящий текст</p><div class=\"x", 1_000);
        assert_eq!(page.text.trim(), "настоящий текст");

        // An unterminated script swallows the rest rather than exposing it.
        let script = extract("<p>до</p><script>var x = 1;", 1_000);
        assert!(script.text.contains("до"));
        assert!(!script.text.contains("var x"));
    }

    #[test]
    fn plain_text_documents_keep_their_words_and_their_bound() {
        let page = extract_plain("  Толщина\t\t покрытия   55 мкм \n\n подробнее ", 1_000);
        assert_eq!(page.text, "Толщина покрытия 55 мкм\nподробнее");
        assert!(page.title.is_none());
        assert!(page.license.is_none());

        let long = extract_plain(&"я".repeat(50), 10);
        assert_eq!(long.text.chars().count(), 10);
        assert!(long.truncated);
    }

    #[test]
    fn a_page_whose_case_folding_changes_its_length_does_not_derail_the_reader() {
        // `İ` (U+0130, two bytes) lowercases to `i` + U+0307 — three bytes. Any offset
        // computed in a `to_lowercase()` copy and applied to the original therefore
        // drifts, landing inside a multi-byte character and panicking. This is a page
        // from an allowed host, so it is a remotely-reachable input, not a curiosity.
        let page = extract(
            "<link rel=\"license\" href=\"İİ\"><title>İİ</title>\
             <meta property=\"article:published_time\" content=\"İ\">\
             <p>Ж настоящий текст</p>",
            10_000,
        );

        assert!(page.text.contains("настоящий текст"), "{:?}", page.text);
        // The title is the title, not the title plus whatever markup followed it.
        assert_eq!(page.title.as_deref(), Some("İİ"));
        assert_eq!(page.license.as_deref(), Some("İİ"));
        assert!(page.published_at.is_none(), "`İ` is not a date");

        // The same shape in the two other offset-using helpers.
        assert!(!tags_named("<meta name=\"İ\" content=\"Ж\">", "meta").is_empty());
        assert_eq!(
            attribute("meta name=\"İİ\" content=\"Ж\"", "content").as_deref(),
            Some("Ж")
        );
    }

    #[test]
    fn numeric_entities_decode_and_control_characters_do_not_survive() {
        assert_eq!(decode_entities("5&#176;C"), "5°C");
        assert_eq!(decode_entities("&#x41;&#x42;"), "AB");
        assert_eq!(decode_entities("a&#0;b"), "a b");
        // Something that is not an entity stays as written.
        assert_eq!(decode_entities("H&M"), "H&M");
        assert_eq!(decode_entities("&notanentity;"), "&notanentity;");
    }
}
