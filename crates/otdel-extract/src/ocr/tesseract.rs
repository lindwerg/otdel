//! Tesseract as an OCR adapter.
//!
//! The engine is an ordinary local program; nothing of it is vendored or reimplemented
//! here. That has one consequence worth being explicit about: **on a machine without
//! Tesseract this adapter reports itself unavailable and no page is ever recognised.**
//! The availability probe also verifies that the configured language packs are actually
//! installed, because `tesseract -l rus` on an installation that only ships `eng`
//! produces an error, and discovering that per page would turn a configuration mistake
//! into dozens of failed pages instead of one clear reason.

use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;

use crate::error::{ToolError, ToolResult};
use crate::model::{RecognisedText, ToolAvailability};

use super::process::{self, check_argument_path};
use super::OcrEngine;

/// Page segmentation mode 3 — fully automatic, no orientation detection. Chosen over
/// mode 1 because mode 1 needs the `osd` data pack, which is a separate install.
const PSM: &str = "3";
/// The probe runs a trivial command; a long wait here means the host is unwell.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
pub struct TesseractEngine {
    binary: String,
    languages: String,
    timeout: Duration,
}

impl TesseractEngine {
    pub fn new(binary: impl Into<String>, languages: impl Into<String>, timeout: Duration) -> Self {
        Self {
            binary: binary.into(),
            languages: languages.into(),
            timeout,
        }
    }

    async fn version(&self) -> ToolResult<String> {
        let output = process::run(
            self.name(),
            &self.binary,
            &[OsStr::new("--version")],
            PROBE_TIMEOUT,
        )
        .await?;
        // Tesseract prints its banner on stdout in some builds and stderr in others.
        let banner = process::first_line(&output.stdout_text());
        let banner = if banner.is_empty() {
            output.stderr.clone()
        } else {
            banner
        };
        if banner.is_empty() {
            return Err(ToolError::Output {
                tool: self.name().to_owned(),
                reason: "версия не определена".to_owned(),
            });
        }
        Ok(banner)
    }

    /// Language packs the installation actually has.
    async fn installed_languages(&self) -> ToolResult<Vec<String>> {
        let output = process::run(
            self.name(),
            &self.binary,
            &[OsStr::new("--list-langs")],
            PROBE_TIMEOUT,
        )
        .await?;
        let text = format!("{}\n{}", output.stdout_text(), output.stderr);
        Ok(text
            .lines()
            .map(str::trim)
            .filter(|line| {
                !line.is_empty()
                    && !line.contains(' ')
                    && line
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
            })
            .map(str::to_owned)
            .collect())
    }

    fn requested_languages(&self) -> Vec<&str> {
        self.languages
            .split('+')
            .filter(|code| !code.is_empty())
            .collect()
    }
}

#[async_trait]
impl OcrEngine for TesseractEngine {
    fn name(&self) -> &str {
        "tesseract"
    }

    fn language(&self) -> &str {
        &self.languages
    }

    async fn availability(&self) -> ToolAvailability {
        let version = match self.version().await {
            Ok(version) => version,
            Err(error) => {
                return ToolAvailability::Unavailable {
                    reason: error.to_string(),
                }
            }
        };

        match self.installed_languages().await {
            Ok(installed) if !installed.is_empty() => {
                let missing: Vec<&str> = self
                    .requested_languages()
                    .into_iter()
                    .filter(|code| !installed.iter().any(|have| have == code))
                    .collect();
                if !missing.is_empty() {
                    return ToolAvailability::Unavailable {
                        reason: format!("не установлены языковые пакеты: {}", missing.join(", ")),
                    };
                }
                ToolAvailability::Available { version }
            }
            // The engine answered but the language list could not be read: report the
            // version and let a per-page failure speak for itself rather than blocking.
            _ => ToolAvailability::Available { version },
        }
    }

    async fn recognise(&self, image: &Path) -> ToolResult<RecognisedText> {
        let image = check_argument_path(image)?;
        let output = process::run(
            self.name(),
            &self.binary,
            &[
                image,
                // Write the recognised text to stdout instead of a file.
                OsStr::new("stdout"),
                OsStr::new("-l"),
                OsStr::new(&self.languages),
                OsStr::new("--psm"),
                OsStr::new(PSM),
            ],
            self.timeout,
        )
        .await?;

        if !output.success {
            return Err(ToolError::Failed {
                tool: self.name().to_owned(),
                reason: if output.stderr.is_empty() {
                    "ненулевой код возврата".to_owned()
                } else {
                    output.stderr
                },
            });
        }

        Ok(RecognisedText {
            text: output.stdout_text(),
            engine: self.name().to_owned(),
            engine_version: self
                .version()
                .await
                .unwrap_or_else(|_| "неизвестная версия".to_owned()),
            language: self.languages.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deliberately points at a name that cannot exist, so the test says the same thing
    /// on a machine with Tesseract and on one without: absence is reported, never
    /// silently treated as success.
    fn missing_engine() -> TesseractEngine {
        TesseractEngine::new(
            "otdel-absent-tesseract-4b71",
            "rus+eng",
            Duration::from_secs(5),
        )
    }

    #[tokio::test]
    async fn an_uninstalled_engine_reports_the_reason() {
        let availability = missing_engine().availability().await;
        assert!(!availability.is_available());
        assert!(
            availability.reason().unwrap().contains("не найден"),
            "{availability:?}"
        );
    }

    #[tokio::test]
    async fn an_uninstalled_engine_cannot_return_text() {
        let error = missing_engine()
            .recognise(Path::new("/tmp/otdel-extract/page-1.png"))
            .await
            .unwrap_err();
        assert!(matches!(error, ToolError::Unavailable { .. }), "{error}");
    }

    #[test]
    fn the_requested_language_list_is_split_on_plus() {
        let engine = TesseractEngine::new("tesseract", "rus+eng", Duration::from_secs(1));
        assert_eq!(engine.requested_languages(), vec!["rus", "eng"]);
        assert_eq!(engine.language(), "rus+eng");
    }
}
