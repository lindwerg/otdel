//! Rendering one PDF page to an image with `pdftoppm` (poppler).
//!
//! Recognition needs pixels, and the pure-Rust reader in [`crate::pdf`] deliberately does
//! not rasterise. This adapter is the bridge; like the engine itself it is a local
//! binary, and its absence is reported rather than worked around.
//!
//! The rendered image is a *transient* artefact: the caller writes it into a private
//! temporary directory and deletes it once the page is read. Phase 1B stores no page
//! snapshots, and the interface links to the original document instead of pretending
//! that stored page images exist.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;

use crate::error::{ToolError, ToolResult};
use crate::model::ToolAvailability;

use super::process::{self, check_argument_path};
use super::PageRasteriser;

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// Fixed stem of the produced file. `pdftoppm` appends the page number and `.png`, and
/// the exact padding it uses varies by version — so the file is found by scanning the
/// (freshly created, server-owned) output directory rather than by guessing the name.
const OUTPUT_STEM: &str = "page";

#[derive(Debug, Clone)]
pub struct PopplerRasteriser {
    binary: String,
    dpi: u32,
    timeout: Duration,
}

impl PopplerRasteriser {
    pub fn new(binary: impl Into<String>, dpi: u32, timeout: Duration) -> Self {
        Self {
            binary: binary.into(),
            dpi,
            timeout,
        }
    }
}

#[async_trait]
impl PageRasteriser for PopplerRasteriser {
    fn name(&self) -> &str {
        "pdftoppm"
    }

    async fn availability(&self) -> ToolAvailability {
        match process::run(
            self.name(),
            &self.binary,
            &[OsStr::new("-v")],
            PROBE_TIMEOUT,
        )
        .await
        {
            // `pdftoppm -v` prints the banner on stderr and exits non-zero on some
            // builds; the banner, not the exit code, is the evidence that it is there.
            Ok(output) => {
                let banner = if output.stderr.is_empty() {
                    process::first_line(&output.stdout_text())
                } else {
                    output.stderr.clone()
                };
                if banner.is_empty() {
                    ToolAvailability::Unavailable {
                        reason: "версия не определена".to_owned(),
                    }
                } else {
                    ToolAvailability::Available { version: banner }
                }
            }
            Err(error) => ToolAvailability::Unavailable {
                reason: error.to_string(),
            },
        }
    }

    async fn render(&self, pdf: &Path, page: u32, out_dir: &Path) -> ToolResult<PathBuf> {
        let pdf_arg = check_argument_path(pdf)?;
        let prefix = out_dir.join(OUTPUT_STEM);
        let prefix_arg = check_argument_path(&prefix)?.to_owned();

        let page_arg = page.to_string();
        let dpi_arg = self.dpi.to_string();

        let output = process::run(
            self.name(),
            &self.binary,
            &[
                OsStr::new("-png"),
                OsStr::new("-r"),
                OsStr::new(&dpi_arg),
                // First and last page: exactly one page is ever rendered per call.
                OsStr::new("-f"),
                OsStr::new(&page_arg),
                OsStr::new("-l"),
                OsStr::new(&page_arg),
                pdf_arg,
                &prefix_arg,
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

        find_rendered_image(out_dir).await.ok_or(ToolError::Output {
            tool: self.name().to_owned(),
            reason: "изображение страницы не создано".to_owned(),
        })
    }
}

/// The single PNG in a directory the server created for exactly this render.
async fn find_rendered_image(out_dir: &Path) -> Option<PathBuf> {
    let mut entries = tokio::fs::read_dir(out_dir).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let is_png = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("png"));
        let is_ours = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(OUTPUT_STEM));
        if is_png && is_ours && entry.metadata().await.is_ok_and(|meta| meta.is_file()) {
            return Some(path);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn missing_rasteriser() -> PopplerRasteriser {
        PopplerRasteriser::new("otdel-absent-pdftoppm-8c02", 200, Duration::from_secs(5))
    }

    #[tokio::test]
    async fn an_uninstalled_rasteriser_reports_the_reason() {
        let availability = missing_rasteriser().availability().await;
        assert!(!availability.is_available());
        assert!(availability.reason().unwrap().contains("не найден"));
    }

    #[tokio::test]
    async fn an_uninstalled_rasteriser_produces_no_image() {
        let error = missing_rasteriser()
            .render(
                Path::new("/tmp/otdel-extract/catalogue.pdf"),
                3,
                Path::new("/tmp/otdel-extract"),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ToolError::Unavailable { .. }), "{error}");
    }

    #[tokio::test]
    async fn a_relative_pdf_path_is_refused_before_anything_is_executed() {
        let error = PopplerRasteriser::new("pdftoppm", 200, Duration::from_secs(5))
            .render(Path::new("relative.pdf"), 1, Path::new("/tmp"))
            .await
            .unwrap_err();
        assert!(matches!(error, ToolError::Failed { .. }), "{error}");
    }

    #[tokio::test]
    async fn an_empty_output_directory_yields_no_image() {
        let dir = std::env::temp_dir().join(format!("otdel-poppler-test-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        assert!(find_rendered_image(&dir).await.is_none());
        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }
}
