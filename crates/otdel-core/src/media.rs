//! Accepted media types for phase 1A and content-signature detection.
//!
//! The media type is decided by the *content signature*, never by the file name and
//! never by the client-declared `Content-Type` of the multipart part. A declared type
//! that contradicts the signature is rejected instead of silently overridden.

use serde::{Deserialize, Serialize};

/// Number of leading bytes that must be inspected before a decision can be made.
pub const SIGNATURE_PREFIX_LEN: usize = 12;

/// File formats accepted by the 1A intake. DOCX/XLSX/PPTX and web links are later phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaType {
    Pdf,
    Png,
    Jpeg,
}

impl MediaType {
    pub const ACCEPTED: [MediaType; 3] = [MediaType::Pdf, MediaType::Png, MediaType::Jpeg];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pdf => "application/pdf",
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
        }
    }

    /// Canonical extension, used for `Content-Disposition` fallbacks only — never to
    /// build a storage path.
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Png => "png",
            Self::Jpeg => "jpg",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        // Tolerate parameters such as `application/pdf; charset=binary`.
        let base = value
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match base.as_str() {
            "application/pdf" | "application/x-pdf" => Some(Self::Pdf),
            "image/png" => Some(Self::Png),
            "image/jpeg" | "image/jpg" => Some(Self::Jpeg),
            _ => None,
        }
    }

    /// Detect the media type from the leading bytes of the stream.
    ///
    /// Returns `None` for anything that is not an accepted 1A format, including a
    /// prefix that is too short to decide.
    pub fn detect(prefix: &[u8]) -> Option<Self> {
        const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

        if prefix.len() < 4 {
            return None;
        }
        if prefix.starts_with(b"%PDF-") {
            return Some(Self::Pdf);
        }
        if prefix.starts_with(&PNG) {
            return Some(Self::Png);
        }
        // JPEG: SOI marker followed by any JFIF/Exif/other application marker.
        if prefix.starts_with(&[0xff, 0xd8, 0xff]) {
            return Some(Self::Jpeg);
        }
        None
    }

    /// Human-readable list used in rejection messages.
    pub fn accepted_list() -> String {
        Self::ACCEPTED
            .iter()
            .map(|m| m.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_accepted_signatures() {
        assert_eq!(MediaType::detect(b"%PDF-1.7\n%..."), Some(MediaType::Pdf));
        assert_eq!(
            MediaType::detect(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00]),
            Some(MediaType::Png)
        );
        assert_eq!(
            MediaType::detect(&[0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10]),
            Some(MediaType::Jpeg)
        );
    }

    #[test]
    fn rejects_disguised_and_short_content() {
        // A ZIP (e.g. a renamed .docx) must not be accepted in 1A.
        assert_eq!(MediaType::detect(b"PK\x03\x04rest"), None);
        assert_eq!(MediaType::detect(b"<html><body>hi"), None);
        // `%PDF-` must be at the very beginning.
        assert_eq!(MediaType::detect(b"junk%PDF-1.4"), None);
        assert_eq!(MediaType::detect(b"%PD"), None);
        assert_eq!(MediaType::detect(b""), None);
    }

    #[test]
    fn parses_declared_content_types() {
        assert_eq!(MediaType::parse("application/pdf"), Some(MediaType::Pdf));
        assert_eq!(
            MediaType::parse("IMAGE/JPEG; charset=binary"),
            Some(MediaType::Jpeg)
        );
        assert_eq!(MediaType::parse("application/octet-stream"), None);
    }
}
