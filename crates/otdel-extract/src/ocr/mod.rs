//! Recognition adapters.
//!
//! Phase 1B does not implement OCR; it *drives* an OCR engine that exists on the host,
//! and the whole point of this module is that the difference is visible. Two traits, two
//! real implementations backed by local binaries, and two "unavailable" implementations
//! used when recognition is switched off.
//!
//! [`Disabled`] matters as much as the real adapters: with it in place, a configuration
//! that has recognition turned off *cannot* produce recognised text, because there is no
//! code path that would. A page then becomes `needs_ocr` with the reason — which is the
//! behaviour the acceptance checks require of a scanned presentation on a machine with
//! no engine installed.

pub(crate) mod poppler;
pub(crate) mod process;
pub(crate) mod tesseract;

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::error::{ToolError, ToolResult};
use crate::model::{RecognisedText, ToolAvailability};

pub use poppler::PopplerRasteriser;
pub use tesseract::TesseractEngine;

/// Turns an image into text.
#[async_trait]
pub trait OcrEngine: Send + Sync {
    /// Short name recorded on every page this engine produced.
    fn name(&self) -> &str;
    /// Language pack list the engine is configured with.
    fn language(&self) -> &str;
    /// Can it be used right now, and if not, why not.
    async fn availability(&self) -> ToolAvailability;
    /// Recognise one image.
    async fn recognise(&self, image: &Path) -> ToolResult<RecognisedText>;
}

/// Turns one page of a PDF into an image the engine can read.
#[async_trait]
pub trait PageRasteriser: Send + Sync {
    fn name(&self) -> &str;
    async fn availability(&self) -> ToolAvailability;
    /// Render `page` of `pdf` into `out_dir` and return the image that was written.
    async fn render(&self, pdf: &Path, page: u32, out_dir: &Path) -> ToolResult<PathBuf>;
}

/// The adapter used when recognition is switched off or deliberately absent.
///
/// It reports itself unavailable and refuses to run. There is no mode in which it
/// returns text.
#[derive(Debug, Clone)]
pub struct Disabled {
    name: String,
    reason: String,
}

impl Disabled {
    pub fn new(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            reason: reason.into(),
        }
    }

    fn unavailable(&self) -> ToolError {
        ToolError::Unavailable {
            tool: self.name.clone(),
            reason: self.reason.clone(),
        }
    }
}

#[async_trait]
impl OcrEngine for Disabled {
    fn name(&self) -> &str {
        &self.name
    }

    fn language(&self) -> &str {
        ""
    }

    async fn availability(&self) -> ToolAvailability {
        ToolAvailability::Unavailable {
            reason: self.reason.clone(),
        }
    }

    async fn recognise(&self, _image: &Path) -> ToolResult<RecognisedText> {
        Err(self.unavailable())
    }
}

#[async_trait]
impl PageRasteriser for Disabled {
    fn name(&self) -> &str {
        &self.name
    }

    async fn availability(&self) -> ToolAvailability {
        ToolAvailability::Unavailable {
            reason: self.reason.clone(),
        }
    }

    async fn render(&self, _pdf: &Path, _page: u32, _out_dir: &Path) -> ToolResult<PathBuf> {
        Err(self.unavailable())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_disabled_adapter_never_produces_text() {
        let disabled = Disabled::new("tesseract", "распознавание отключено настройкой");

        let availability = OcrEngine::availability(&disabled).await;
        assert!(!availability.is_available());
        assert_eq!(
            availability.reason(),
            Some("распознавание отключено настройкой")
        );

        let error = disabled
            .recognise(Path::new("/tmp/otdel/page.png"))
            .await
            .unwrap_err();
        assert!(matches!(error, ToolError::Unavailable { .. }));
    }

    #[tokio::test]
    async fn the_disabled_rasteriser_refuses_to_render() {
        let disabled = Disabled::new("pdftoppm", "рендер страниц отключён");
        assert!(!PageRasteriser::availability(&disabled).await.is_available());
        assert!(disabled
            .render(Path::new("/tmp/otdel/a.pdf"), 1, Path::new("/tmp/otdel"))
            .await
            .is_err());
    }
}
