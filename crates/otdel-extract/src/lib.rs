//! Reading documents: the phase 1B adapters.
//!
//! The crate is split along the line that matters for honesty:
//!
//! * [`pdf`] reads the **text layer** in process, in pure Rust. It needs nothing
//!   installed, which is why the whole text half of the pipeline is covered by ordinary
//!   unit tests that build a PDF in memory and read it back;
//! * [`ocr`] **drives an external engine**. It is a set of adapters over local binaries
//!   (`tesseract`, `pdftoppm`) with an availability probe, and when the binary is not
//!   there the adapters say so. Nothing in this crate can produce recognised text without
//!   a real engine having produced it;
//! * [`assess`] holds the decision — which status a page ends up with — in two pure
//!   functions, so the rule is readable and testable in isolation.
//!
//! Threading note. [`pdf::PdfDocument::load`] and [`pdf::PdfDocument::read_page`] are
//! synchronous and CPU-bound; the async half ([`PageProcessor::finish_page`]) only waits
//! on external processes. Callers are expected to run the synchronous half off the async
//! scheduler — see `otdel-worker`.

pub mod assess;
pub mod error;
pub mod model;
pub mod ocr;
pub mod pdf;
pub mod table_context;
pub mod units;

#[cfg(feature = "fixtures")]
pub mod fixtures;

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use otdel_core::extraction::RegionKind;

pub use assess::{
    assess_text_layer, decide, OcrAttempt, PageDecision, SuitabilityThresholds, TextLayerVerdict,
};
pub use error::{ExtractError, ExtractResult, ToolError, ToolResult};
pub use model::{
    DocumentInventory, ExtractedCell, ExtractedRegion, ExtractedTable, PageInventory, PageText,
    RecognisedText, ToolAvailability,
};
pub use ocr::{Disabled, OcrEngine, PageRasteriser, PopplerRasteriser, TesseractEngine};
pub use pdf::{PdfDocument, PARSER_NAME, PARSER_VERSION};
pub use table_context::annotate_page;

/// The reading half must be usable from a blocking task, so the worker can keep the
/// async scheduler free while a large page is parsed.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<PdfDocument>();
};

/// Upper bound on regions kept from recognised text — a resource limit, matching the
/// spirit of the per-page glyph budget.
const MAX_OCR_REGIONS: usize = 400;

/// Where the image for recognition comes from, when recognition is needed at all.
#[derive(Debug, Clone, Copy)]
pub enum PageSource<'a> {
    /// One page of a PDF, rendered on demand into `work_dir`.
    Pdf {
        path: &'a Path,
        page: u32,
        work_dir: &'a Path,
    },
    /// A material that is itself an image: no rendering step.
    Image { path: &'a Path },
}

/// Whether this page may use recognition on this run.
///
/// The caller (the worker) decides: it knows whether the engine probed as available and
/// how much of the per-run budget is left. `Denied` always carries a reason, because that
/// reason is what the page will say to the user.
#[derive(Debug, Clone)]
pub enum OcrPermission {
    Allowed,
    Denied { reason: String },
}

/// Which engine produced a page's text, recorded on the page row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcrStamp {
    pub engine: String,
    pub version: String,
    pub language: String,
}

/// Everything decided about one page, ready to be stored.
#[derive(Debug, Clone)]
pub struct PageOutcome {
    pub decision: PageDecision,
    /// `None` whenever no trustworthy text was obtained — never an empty string that
    /// would look like a successfully read blank page.
    pub text: Option<String>,
    pub regions: Vec<ExtractedRegion>,
    pub char_count: u32,
    pub word_count: u32,
    pub parser_name: &'static str,
    pub parser_version: &'static str,
    /// Present only when an engine really ran and produced the text.
    pub ocr: Option<OcrStamp>,
    pub duration_ms: u32,
    /// Vector drawing operations seen on the page.
    ///
    /// Carried so a page with a diagram is *recorded as having one*. Nothing here
    /// interprets it: the letters around a load diagram are text, and reading them is not
    /// reading the diagram.
    pub drawing_count: u32,
}

/// Turns observations about a page into its stored outcome, running recognition when
/// the text layer is not good enough and permission allows it.
pub struct PageProcessor {
    engine: Arc<dyn OcrEngine>,
    rasteriser: Arc<dyn PageRasteriser>,
    thresholds: SuitabilityThresholds,
}

impl PageProcessor {
    pub fn new(engine: Arc<dyn OcrEngine>, rasteriser: Arc<dyn PageRasteriser>) -> Self {
        Self {
            engine,
            rasteriser,
            thresholds: SuitabilityThresholds::default(),
        }
    }

    pub fn with_thresholds(mut self, thresholds: SuitabilityThresholds) -> Self {
        self.thresholds = thresholds;
        self
    }

    pub fn engine_name(&self) -> &str {
        self.engine.name()
    }

    pub async fn engine_availability(&self) -> ToolAvailability {
        self.engine.availability().await
    }

    pub async fn rasteriser_availability(&self) -> ToolAvailability {
        self.rasteriser.availability().await
    }

    /// Decide a page, given what its text layer produced.
    ///
    /// `text` is the result of [`PdfDocument::read_page`] — or an empty [`PageText`] for
    /// a material that is an image and has no text layer at all.
    pub async fn finish_page(
        &self,
        text: PageText,
        inventory: &PageInventory,
        source: PageSource<'_>,
        permission: &OcrPermission,
    ) -> PageOutcome {
        let started = Instant::now();
        let drawing_count = text.drawing_ops;
        let verdict = assess::assess_text_layer(&text, inventory, &self.thresholds);

        let needs_recognition = matches!(
            verdict,
            TextLayerVerdict::Unusable { .. } | TextLayerVerdict::Thin { .. }
        );

        let (attempt, recognised) = if !needs_recognition {
            (OcrAttempt::NotNeeded, None)
        } else {
            match permission {
                OcrPermission::Denied { reason } => (
                    OcrAttempt::Skipped {
                        reason: reason.clone(),
                    },
                    None,
                ),
                OcrPermission::Allowed => self.recognise(source).await,
            }
        };

        let decision = assess::decide(&verdict, &attempt);
        let duration_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);

        match decision.text_source {
            otdel_core::extraction::TextSource::TextLayer => PageOutcome {
                char_count: text.char_count,
                word_count: text.word_count,
                text: non_empty(text.text),
                regions: text.regions,
                decision,
                parser_name: PARSER_NAME,
                parser_version: PARSER_VERSION,
                ocr: None,
                duration_ms,
                drawing_count: text.drawing_ops,
            },
            otdel_core::extraction::TextSource::Ocr => {
                let recognised = recognised.expect("an `Ocr` source implies recognised text");
                let (char_count, word_count) = count(&recognised.text);
                PageOutcome {
                    regions: ocr_regions(&recognised.text),
                    text: non_empty(recognised.text.clone()),
                    char_count,
                    word_count,
                    decision,
                    parser_name: PARSER_NAME,
                    parser_version: PARSER_VERSION,
                    ocr: Some(OcrStamp {
                        engine: recognised.engine,
                        version: recognised.engine_version,
                        language: recognised.language,
                    }),
                    duration_ms,
                    // Recognition reads an image of the page; it says nothing about the
                    // vector drawings the text layer reported.
                    drawing_count,
                }
            }
            otdel_core::extraction::TextSource::None => PageOutcome {
                decision,
                text: None,
                regions: Vec::new(),
                // The counts describe what was *kept*, and nothing was kept.
                char_count: 0,
                word_count: 0,
                parser_name: PARSER_NAME,
                parser_version: PARSER_VERSION,
                ocr: None,
                duration_ms,
                drawing_count,
            },
        }
    }

    /// Render (if needed) and recognise. Any failure becomes a stated reason.
    async fn recognise(&self, source: PageSource<'_>) -> (OcrAttempt, Option<RecognisedText>) {
        let rendered;
        let image = match source {
            PageSource::Image { path } => path,
            PageSource::Pdf {
                path,
                page,
                work_dir,
            } => match self.rasteriser.render(path, page, work_dir).await {
                Ok(image) => {
                    rendered = image;
                    rendered.as_path()
                }
                Err(error) => return (attempt_from(error), None),
            },
        };

        match self.engine.recognise(image).await {
            Ok(recognised) if recognised.text.trim().is_empty() => (OcrAttempt::Empty, None),
            Ok(recognised) => (OcrAttempt::Recognised, Some(recognised)),
            Err(error) => (attempt_from(error), None),
        }
    }
}

/// "Not installed" is a skip with a reason; anything else is a failure with a reason.
/// Both are visible, neither is success.
fn attempt_from(error: ToolError) -> OcrAttempt {
    match error {
        ToolError::Unavailable { .. } => OcrAttempt::Skipped {
            reason: error.to_string(),
        },
        other => OcrAttempt::Failed {
            reason: other.to_string(),
        },
    }
}

fn non_empty(text: String) -> Option<String> {
    (!text.trim().is_empty()).then_some(text)
}

fn count(text: &str) -> (u32, u32) {
    let chars = text
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .count()
        .try_into()
        .unwrap_or(u32::MAX);
    let words = text
        .split_whitespace()
        .count()
        .try_into()
        .unwrap_or(u32::MAX);
    (chars, words)
}

/// Paragraph regions from recognised text.
///
/// No coordinates: the engine is given a rendered image and returns text, so where a
/// paragraph sat on the page is not known. `None` is recorded rather than a rectangle
/// derived from nothing.
fn ocr_regions(text: &str) -> Vec<ExtractedRegion> {
    text.split("\n\n")
        .map(str::trim)
        .filter(|block| !block.is_empty())
        .take(MAX_OCR_REGIONS)
        .map(|block| ExtractedRegion {
            kind: RegionKind::Paragraph,
            text: block.to_owned(),
            bbox: None,
            table: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use otdel_core::extraction::{PageStatus, TextSource};
    use std::path::PathBuf;

    /// An engine that always succeeds — used to prove the *opposite* case is what the
    /// real absence of an engine produces.
    struct FakeEngine(&'static str);

    #[async_trait]
    impl OcrEngine for FakeEngine {
        fn name(&self) -> &str {
            "fake"
        }
        fn language(&self) -> &str {
            "rus"
        }
        async fn availability(&self) -> ToolAvailability {
            ToolAvailability::Available {
                version: "fake 1.0".to_owned(),
            }
        }
        async fn recognise(&self, _image: &Path) -> ToolResult<RecognisedText> {
            Ok(RecognisedText {
                text: self.0.to_owned(),
                engine: "fake".to_owned(),
                engine_version: "fake 1.0".to_owned(),
                language: "rus".to_owned(),
            })
        }
    }

    struct FakeRasteriser;

    #[async_trait]
    impl PageRasteriser for FakeRasteriser {
        fn name(&self) -> &str {
            "fake"
        }
        async fn availability(&self) -> ToolAvailability {
            ToolAvailability::Available {
                version: "fake 1.0".to_owned(),
            }
        }
        async fn render(&self, _pdf: &Path, page: u32, out_dir: &Path) -> ToolResult<PathBuf> {
            Ok(out_dir.join(format!("page-{page}.png")))
        }
    }

    fn scan_inventory() -> PageInventory {
        PageInventory {
            page_number: 4,
            width_pt: 864.0,
            height_pt: 618.0,
            rotation: 0,
            image_count: 1,
        }
    }

    fn source() -> PageSource<'static> {
        PageSource::Pdf {
            path: Path::new("/tmp/otdel/presentation.pdf"),
            page: 4,
            work_dir: Path::new("/tmp/otdel"),
        }
    }

    fn processor(engine: Arc<dyn OcrEngine>) -> PageProcessor {
        PageProcessor::new(engine, Arc::new(FakeRasteriser))
    }

    #[tokio::test]
    async fn a_scanned_page_without_an_engine_is_needs_ocr_and_stores_no_text() {
        let processor = processor(Arc::new(Disabled::new(
            "tesseract",
            "исполняемый файл `tesseract` не найден",
        )));
        let outcome = processor
            .finish_page(
                PageText::default(),
                &scan_inventory(),
                source(),
                &OcrPermission::Allowed,
            )
            .await;

        assert_eq!(outcome.decision.status, PageStatus::NeedsOcr);
        assert_eq!(outcome.decision.text_source, TextSource::None);
        assert_eq!(outcome.text, None);
        assert!(outcome.regions.is_empty());
        assert_eq!(outcome.char_count, 0);
        assert!(outcome.ocr.is_none(), "no engine may be credited");
        assert!(outcome.decision.diagnostic.unwrap().contains("не найден"));
    }

    #[tokio::test]
    async fn denied_permission_is_reported_with_its_reason() {
        let processor = processor(Arc::new(FakeEngine("распознанный текст")));
        let outcome = processor
            .finish_page(
                PageText::default(),
                &scan_inventory(),
                source(),
                &OcrPermission::Denied {
                    reason: "исчерпан лимит распознавания на один запуск".to_owned(),
                },
            )
            .await;

        assert_eq!(outcome.decision.status, PageStatus::NeedsOcr);
        assert!(outcome
            .decision
            .diagnostic
            .unwrap()
            .contains("исчерпан лимит"));
        assert!(outcome.ocr.is_none());
    }

    #[tokio::test]
    async fn a_real_engine_result_is_stored_as_recognised_text() {
        let processor = processor(Arc::new(FakeEngine(
            "Надёжная основа крепления\n\nинженерных систем",
        )));
        let outcome = processor
            .finish_page(
                PageText::default(),
                &scan_inventory(),
                source(),
                &OcrPermission::Allowed,
            )
            .await;

        assert_eq!(outcome.decision.status, PageStatus::Extracted);
        assert_eq!(outcome.decision.text_source, TextSource::Ocr);
        assert_eq!(outcome.regions.len(), 2);
        // Recognised text has no coordinates, and none are invented.
        assert!(outcome.regions.iter().all(|region| region.bbox.is_none()));
        let stamp = outcome.ocr.expect("the engine must be recorded");
        assert_eq!(stamp.engine, "fake");
        assert_eq!(stamp.language, "rus");
        assert!(outcome.char_count > 0);
    }

    #[tokio::test]
    async fn an_engine_that_finds_nothing_does_not_produce_a_read_page() {
        let processor = processor(Arc::new(FakeEngine("   \n  \n")));
        let outcome = processor
            .finish_page(
                PageText::default(),
                &scan_inventory(),
                source(),
                &OcrPermission::Allowed,
            )
            .await;
        assert_eq!(outcome.decision.status, PageStatus::NeedsOcr);
        assert_eq!(outcome.text, None);
    }

    #[tokio::test]
    async fn a_good_text_layer_never_invokes_recognition() {
        let text = PageText {
            text: "Профиль BP 21 применяется для монтажа инженерных систем".to_owned(),
            char_count: 500,
            word_count: 90,
            ..PageText::default()
        };
        let processor = processor(Arc::new(Disabled::new("tesseract", "не установлен")));
        let outcome = processor
            .finish_page(text, &scan_inventory(), source(), &OcrPermission::Allowed)
            .await;

        assert_eq!(outcome.decision.status, PageStatus::Extracted);
        assert_eq!(outcome.decision.text_source, TextSource::TextLayer);
        assert!(outcome.ocr.is_none());
        assert!(outcome.text.is_some());
    }
}
