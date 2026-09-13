//! Is this page's text layer good enough — and what does the page become if it is not.
//!
//! Everything that decides a page's fate lives here, in two pure functions, because this
//! is exactly the place where a document-reading system is tempted to lie. The two
//! temptations, and what is done instead:
//!
//! * a page with no text layer is *not* "empty". It is `needs_ocr` unless the page is
//!   demonstrably blank — no glyphs, no images, no vector drawing;
//! * a text layer that decodes to replacement characters is *not* text. Storing it would
//!   produce confident nonsense, so it is treated as absent.
//!
//! When recognition cannot run, that is reported as the page's reason. It never becomes
//! a successful outcome, and it never becomes invented text.

use otdel_core::extraction::{PageStatus, TextSource};

use crate::model::{PageInventory, PageText};

/// Tunables of the judgement below. Defaults are deliberately conservative: the cost of
/// sending a readable page to OCR is some time, the cost of accepting an unreadable one
/// is a document that claims to have been read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SuitabilityThresholds {
    /// Below this, a page that also carries images or drawings is only "thin" — the
    /// few characters are kept, but the page is not called fully read.
    pub min_chars_with_graphics: u32,
    pub min_words_with_graphics: u32,
    /// Share of undecodable characters above which the layer is nonsense.
    pub max_garbled_ratio: f64,
}

impl Default for SuitabilityThresholds {
    fn default() -> Self {
        Self {
            min_chars_with_graphics: 60,
            min_words_with_graphics: 8,
            max_garbled_ratio: 0.2,
        }
    }
}

/// What the text layer of one page turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextLayerVerdict {
    /// Good enough to be the page's content.
    Usable,
    /// Present but not trustworthy as the whole page; the text is still kept.
    Thin { reason: String },
    /// Nothing usable. Whatever text there was (if any) is nonsense and is discarded.
    Unusable { reason: String },
    /// The page really is blank.
    Blank,
}

/// Outcome of the recognition step for one page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcrAttempt {
    /// The text layer was good; recognition was never needed.
    NotNeeded,
    /// Recognition did not run. `reason` says why — not installed, switched off, budget
    /// spent. This is the case that must never look like success.
    Skipped { reason: String },
    /// Recognition ran and errored.
    Failed { reason: String },
    /// Recognition ran and found no text on the page.
    Empty,
    /// Recognition produced text.
    Recognised,
}

/// The stored outcome of a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageDecision {
    pub status: PageStatus,
    pub text_source: TextSource,
    /// Written to the page row so the interface can explain the status in words.
    pub diagnostic: Option<String>,
}

/// Judge a page's text layer from what was actually observed on it.
pub fn assess_text_layer(
    text: &PageText,
    inventory: &PageInventory,
    thresholds: &SuitabilityThresholds,
) -> TextLayerVerdict {
    let has_graphics = inventory.image_count > 0 || text.drawing_ops > 0;

    // A layer that decodes to replacement characters is worse than no layer: it looks
    // like content. Checked first, before any "there is text here" reasoning.
    if text.char_count > 0 && text.garbled_ratio > thresholds.max_garbled_ratio {
        return TextLayerVerdict::Unusable {
            reason: format!(
                "текстовый слой не декодируется: {}% символов не распознаны как текст",
                (text.garbled_ratio * 100.0).round() as i64
            ),
        };
    }

    // A page cut short by the glyph budget kept real text, but not all of it.
    if text.truncated {
        return TextLayerVerdict::Thin {
            reason: "на странице слишком много символов: сохранена только часть текста".to_owned(),
        };
    }

    if text.char_count == 0 {
        return match (inventory.image_count > 0, text.drawing_ops > 0) {
            (true, _) => TextLayerVerdict::Unusable {
                reason: format!(
                    "страница без текстового слоя, содержит изображений: {}",
                    inventory.image_count
                ),
            },
            (false, true) => TextLayerVerdict::Unusable {
                reason: "страница без текстового слоя, содержит векторную графику".to_owned(),
            },
            (false, false) => TextLayerVerdict::Blank,
        };
    }

    if has_graphics
        && (text.char_count < thresholds.min_chars_with_graphics
            || text.word_count < thresholds.min_words_with_graphics)
    {
        return TextLayerVerdict::Thin {
            reason: format!(
                "на странице с изображением найдено только {} символов текстового слоя; \
                 остальное содержимое может быть графикой",
                text.char_count
            ),
        };
    }

    TextLayerVerdict::Usable
}

/// Turn the two observations into the page's stored status.
///
/// The table in one sentence: text-layer text wins when it is trustworthy, recognised
/// text wins when it is not, and when neither is available the page says `needs_ocr`
/// with the reason rather than `empty` or `completed`.
pub fn decide(verdict: &TextLayerVerdict, ocr: &OcrAttempt) -> PageDecision {
    match verdict {
        TextLayerVerdict::Usable => PageDecision {
            status: PageStatus::Extracted,
            text_source: TextSource::TextLayer,
            diagnostic: None,
        },

        TextLayerVerdict::Blank => PageDecision {
            status: PageStatus::Empty,
            text_source: TextSource::None,
            diagnostic: Some(
                "страница пуста: нет ни текстового слоя, ни изображений, ни графики".to_owned(),
            ),
        },

        TextLayerVerdict::Unusable { reason } => match ocr {
            OcrAttempt::Recognised => PageDecision {
                status: PageStatus::Extracted,
                text_source: TextSource::Ocr,
                diagnostic: Some(format!("{reason}; текст получен распознаванием")),
            },
            OcrAttempt::Empty => PageDecision {
                status: PageStatus::NeedsOcr,
                text_source: TextSource::None,
                diagnostic: Some(format!(
                    "{reason}; распознавание выполнено, текст не найден"
                )),
            },
            OcrAttempt::Failed { reason: why } => PageDecision {
                status: PageStatus::NeedsOcr,
                text_source: TextSource::None,
                diagnostic: Some(format!("{reason}; распознавание не выполнено: {why}")),
            },
            // Including `NotNeeded`, which for an unusable layer means the pipeline
            // never even tried — reported as such instead of quietly passing.
            OcrAttempt::Skipped { reason: why } => PageDecision {
                status: PageStatus::NeedsOcr,
                text_source: TextSource::None,
                diagnostic: Some(format!("{reason}; распознавание недоступно: {why}")),
            },
            OcrAttempt::NotNeeded => PageDecision {
                status: PageStatus::NeedsOcr,
                text_source: TextSource::None,
                diagnostic: Some(format!("{reason}; распознавание не запускалось")),
            },
        },

        TextLayerVerdict::Thin { reason } => match ocr {
            OcrAttempt::Recognised => PageDecision {
                status: PageStatus::Extracted,
                text_source: TextSource::Ocr,
                diagnostic: Some(format!("{reason}; страница дочитана распознаванием")),
            },
            OcrAttempt::NotNeeded => PageDecision {
                status: PageStatus::Partial,
                text_source: TextSource::TextLayer,
                diagnostic: Some(reason.clone()),
            },
            OcrAttempt::Empty => PageDecision {
                status: PageStatus::Partial,
                text_source: TextSource::TextLayer,
                diagnostic: Some(format!(
                    "{reason}; распознавание выполнено, дополнительного текста не найдено"
                )),
            },
            OcrAttempt::Failed { reason: why } => PageDecision {
                status: PageStatus::Partial,
                text_source: TextSource::TextLayer,
                diagnostic: Some(format!("{reason}; распознавание не выполнено: {why}")),
            },
            OcrAttempt::Skipped { reason: why } => PageDecision {
                status: PageStatus::Partial,
                text_source: TextSource::TextLayer,
                diagnostic: Some(format!("{reason}; распознавание недоступно: {why}")),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inventory(images: u32) -> PageInventory {
        PageInventory {
            page_number: 1,
            width_pt: 595.0,
            height_pt: 842.0,
            rotation: 0,
            image_count: images,
        }
    }

    fn text(chars: u32, words: u32) -> PageText {
        PageText {
            text: "x".repeat(chars as usize),
            regions: Vec::new(),
            char_count: chars,
            word_count: words,
            garbled_ratio: 0.0,
            drawing_ops: 0,
            truncated: false,
        }
    }

    #[test]
    fn a_normal_technical_page_uses_its_text_layer() {
        let verdict = assess_text_layer(&text(2000, 300), &inventory(2), &Default::default());
        assert_eq!(verdict, TextLayerVerdict::Usable);
        let decision = decide(&verdict, &OcrAttempt::NotNeeded);
        assert_eq!(decision.status, PageStatus::Extracted);
        assert_eq!(decision.text_source, TextSource::TextLayer);
    }

    #[test]
    fn an_image_only_page_is_never_called_empty() {
        let verdict = assess_text_layer(&text(0, 0), &inventory(1), &Default::default());
        assert!(matches!(verdict, TextLayerVerdict::Unusable { .. }));
        assert_ne!(verdict, TextLayerVerdict::Blank);
    }

    #[test]
    fn a_scan_without_a_recognition_engine_is_needs_ocr_with_the_reason() {
        let verdict = assess_text_layer(&text(0, 0), &inventory(1), &Default::default());
        let decision = decide(
            &verdict,
            &OcrAttempt::Skipped {
                reason: "tesseract не установлен".to_owned(),
            },
        );
        assert_eq!(decision.status, PageStatus::NeedsOcr);
        assert_eq!(decision.text_source, TextSource::None);
        let diagnostic = decision.diagnostic.unwrap();
        assert!(
            diagnostic.contains("tesseract не установлен"),
            "{diagnostic}"
        );
        // The crucial negative: nothing here may read as success.
        assert_ne!(decision.status, PageStatus::Extracted);
        assert_ne!(decision.status, PageStatus::Empty);
    }

    #[test]
    fn recognition_that_runs_and_finds_nothing_is_not_success_either() {
        let verdict = assess_text_layer(&text(0, 0), &inventory(1), &Default::default());
        let decision = decide(&verdict, &OcrAttempt::Empty);
        assert_eq!(decision.status, PageStatus::NeedsOcr);
        assert_eq!(decision.text_source, TextSource::None);
    }

    #[test]
    fn recognised_text_is_recorded_as_recognised_not_as_a_text_layer() {
        let verdict = assess_text_layer(&text(0, 0), &inventory(1), &Default::default());
        let decision = decide(&verdict, &OcrAttempt::Recognised);
        assert_eq!(decision.status, PageStatus::Extracted);
        assert_eq!(decision.text_source, TextSource::Ocr);
    }

    #[test]
    fn a_truly_blank_page_is_empty_and_needs_no_recognition() {
        let verdict = assess_text_layer(&text(0, 0), &inventory(0), &Default::default());
        assert_eq!(verdict, TextLayerVerdict::Blank);
        let decision = decide(&verdict, &OcrAttempt::NotNeeded);
        assert_eq!(decision.status, PageStatus::Empty);
    }

    #[test]
    fn a_garbled_text_layer_is_treated_as_absent() {
        let mut garbled = text(500, 80);
        garbled.garbled_ratio = 0.75;
        let verdict = assess_text_layer(&garbled, &inventory(0), &Default::default());
        assert!(matches!(verdict, TextLayerVerdict::Unusable { .. }));
        let decision = decide(
            &verdict,
            &OcrAttempt::Skipped {
                reason: "распознавание отключено".to_owned(),
            },
        );
        // The nonsense is not stored as the page's text.
        assert_eq!(decision.text_source, TextSource::None);
        assert_eq!(decision.status, PageStatus::NeedsOcr);
    }

    #[test]
    fn a_caption_under_a_drawing_keeps_its_text_but_stays_partial() {
        let mut thin = text(30, 4);
        thin.drawing_ops = 40;
        let verdict = assess_text_layer(&thin, &inventory(0), &Default::default());
        assert!(matches!(verdict, TextLayerVerdict::Thin { .. }));

        let decision = decide(
            &verdict,
            &OcrAttempt::Skipped {
                reason: "tesseract не установлен".to_owned(),
            },
        );
        assert_eq!(decision.status, PageStatus::Partial);
        // The few characters that *were* readable are kept, not thrown away.
        assert_eq!(decision.text_source, TextSource::TextLayer);
        assert!(decision.diagnostic.unwrap().contains("tesseract"));
    }

    #[test]
    fn a_page_cut_short_by_the_glyph_budget_is_not_reported_as_fully_read() {
        let mut huge = text(120_000, 20_000);
        huge.truncated = true;
        let verdict = assess_text_layer(&huge, &inventory(0), &Default::default());
        assert!(matches!(verdict, TextLayerVerdict::Thin { .. }));
        assert_eq!(
            decide(&verdict, &OcrAttempt::NotNeeded).status,
            PageStatus::Partial
        );
    }

    #[test]
    fn a_sparse_page_without_graphics_is_still_a_normal_page() {
        // A title page with five words and nothing else is not suspicious.
        let verdict = assess_text_layer(&text(20, 3), &inventory(0), &Default::default());
        assert_eq!(verdict, TextLayerVerdict::Usable);
    }
}
