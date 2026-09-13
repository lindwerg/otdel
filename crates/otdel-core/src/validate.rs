//! Input validation and normalisation.
//!
//! Two different jobs live here and must not be confused:
//!
//! * partner fields are *validated* — bad input is rejected with a clear message;
//! * the uploaded file name is *normalised for display only*. It never becomes part
//!   of a path: storage keys are derived from identifiers and the content hash
//!   (see `otdel-storage`), so even a hostile name cannot influence where bytes land.

use crate::error::AppError;
use crate::media::MediaType;

pub const PARTNER_NAME_MAX_CHARS: usize = 200;
pub const PARTNER_NOTE_MAX_CHARS: usize = 10_000;
pub const FILENAME_MAX_CHARS: usize = 200;

/// Trim, then require 1..=200 characters and no control characters.
pub fn partner_name(raw: &str) -> Result<String, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::validation("partner name must not be empty"));
    }
    let length = trimmed.chars().count();
    if length > PARTNER_NAME_MAX_CHARS {
        return Err(AppError::validation(format!(
            "partner name must be at most {PARTNER_NAME_MAX_CHARS} characters, got {length}"
        )));
    }
    if trimmed.chars().any(is_forbidden_control) {
        return Err(AppError::validation(
            "partner name must not contain control characters",
        ));
    }
    Ok(trimmed.to_owned())
}

/// Trim, allow up to 10000 characters; an empty note is stored as `null`.
pub fn partner_note(raw: Option<&str>) -> Result<Option<String>, AppError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let length = trimmed.chars().count();
    if length > PARTNER_NOTE_MAX_CHARS {
        return Err(AppError::validation(format!(
            "partner note must be at most {PARTNER_NOTE_MAX_CHARS} characters, got {length}"
        )));
    }
    if trimmed
        .chars()
        .any(|c| is_forbidden_control(c) && c != '\n' && c != '\t')
    {
        return Err(AppError::validation(
            "partner note must not contain control characters",
        ));
    }
    Ok(Some(trimmed.to_owned()))
}

/// Reduce a client-supplied file name to a safe *display* name.
///
/// Directory separators (both `/` and `\`), NUL and other control characters, and the
/// special names `.`/`..` are removed. If nothing usable is left, a neutral name based
/// on the detected media type is returned.
pub fn display_filename(raw: &str, media_type: MediaType) -> String {
    let last_segment = raw.rsplit(['/', '\\']).next().unwrap_or_default();

    let cleaned: String = last_segment
        .chars()
        .map(|c| if is_forbidden_control(c) { ' ' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').trim();

    let truncated: String = cleaned.chars().take(FILENAME_MAX_CHARS).collect();
    let truncated = truncated.trim().to_owned();

    if truncated.is_empty() {
        format!("upload.{}", media_type.extension())
    } else {
        truncated
    }
}

/// Escape a display name for the quoted-string form of `Content-Disposition`.
///
/// Only US-ASCII visible characters survive; the full name is transported separately
/// in the RFC 5987 `filename*` parameter.
pub fn content_disposition_ascii_fallback(name: &str, media_type: MediaType) -> String {
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ' | '(' | ')' | '+') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let ascii = ascii.trim().to_owned();
    if ascii.is_empty() || ascii.chars().all(|c| c == '_' || c == ' ') {
        format!("original.{}", media_type.extension())
    } else {
        ascii
    }
}

/// RFC 5987 / RFC 6266 `filename*` value (`UTF-8''<percent-encoded>`).
pub fn content_disposition_utf8(name: &str) -> String {
    const UNRESERVED: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut out = String::from("UTF-8''");
    for byte in name.as_bytes() {
        if UNRESERVED.contains(byte) {
            out.push(*byte as char);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

fn is_forbidden_control(c: char) -> bool {
    c.is_control()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partner_name_is_trimmed_and_bounded() {
        assert_eq!(partner_name("  BASIS  ").unwrap(), "BASIS");
        assert!(partner_name("   ").is_err());
        assert!(partner_name("\u{0}name").is_err());
        let long = "a".repeat(PARTNER_NAME_MAX_CHARS);
        assert!(partner_name(&long).is_ok());
        assert!(partner_name(&format!("{long}a")).is_err());
    }

    #[test]
    fn partner_note_empty_becomes_null() {
        assert_eq!(partner_note(None).unwrap(), None);
        assert_eq!(partner_note(Some("  ")).unwrap(), None);
        assert_eq!(partner_note(Some(" hi ")).unwrap(), Some("hi".to_owned()));
        let long = "n".repeat(PARTNER_NOTE_MAX_CHARS + 1);
        assert!(partner_note(Some(&long)).is_err());
        assert!(partner_note(Some("line\nbreak\tok")).is_ok());
    }

    #[test]
    fn display_filename_strips_paths_and_traversal() {
        assert_eq!(
            display_filename("../../etc/passwd", MediaType::Pdf),
            "passwd"
        );
        assert_eq!(
            display_filename("C:\\Windows\\System32\\catalog.pdf", MediaType::Pdf),
            "catalog.pdf"
        );
        assert_eq!(display_filename("..", MediaType::Pdf), "upload.pdf");
        assert_eq!(display_filename("   ", MediaType::Png), "upload.png");
        assert_eq!(
            display_filename("bad\u{0}name.pdf", MediaType::Pdf),
            "bad name.pdf"
        );
        assert_eq!(
            display_filename("каталог.pdf", MediaType::Pdf),
            "каталог.pdf"
        );
        let long = format!("{}.pdf", "x".repeat(500));
        assert_eq!(
            display_filename(&long, MediaType::Pdf).chars().count(),
            FILENAME_MAX_CHARS
        );
    }

    #[test]
    fn content_disposition_parts_are_header_safe() {
        let fallback = content_disposition_ascii_fallback("катало\"г.pdf", MediaType::Pdf);
        assert!(!fallback.contains('"'));
        assert!(!fallback.contains('\n'));
        assert_eq!(
            content_disposition_utf8("a b.pdf"),
            "UTF-8''a%20b.pdf".to_owned()
        );
        assert_eq!(
            content_disposition_ascii_fallback("привет", MediaType::Png),
            "original.png"
        );
    }
}
