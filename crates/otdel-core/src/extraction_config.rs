//! Configuration of the phase 1B extraction worker and its OCR adapters.
//!
//! Two things deserve attention.
//!
//! **The external tools are named by the operator, never by a document.** The OCR engine
//! and the page rasteriser are separate processes (`tesseract`, `pdftoppm`); their
//! executable names come from the environment and are validated here, and every argument
//! the worker later passes to them is a server-generated temporary path or an integer.
//! Nothing from inside a PDF — a file name, a piece of text, an embedded instruction —
//! can reach a command line.
//!
//! **Absence is a configuration state, not an error to paper over.** When OCR is turned
//! off, or the binary is simply not installed, the worker does not fabricate text: the
//! affected pages are recorded as `needs_ocr` with the reason. See
//! [`crate::extraction::PageStatus`].

use std::path::PathBuf;
use std::time::Duration;

use crate::config::{duration_secs_or, parse_bool, parse_u64_or, string_or, ConfigSource};
use crate::error::AppError;

/// Below this a worker would spin; above it a retried job would look stuck.
const POLL_INTERVAL_RANGE: (u64, u64) = (1, 300);
/// A lease shorter than a page takes would be reclaimed under the running worker.
const LEASE_RANGE: (u64, u64) = (30, 3600);
const PAGE_TIMEOUT_RANGE: (u64, u64) = (5, 1800);
const OCR_TIMEOUT_RANGE: (u64, u64) = (5, 1800);
/// Rendering below ~120 dpi loses small type; above 600 the memory cost is not worth it.
const OCR_DPI_RANGE: (u64, u64) = (72, 600);
const MAX_PAGES_RANGE: (u64, u64) = (1, 20_000);

/// How the worker reads documents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractionSettings {
    /// Delay between polls when the queue is empty.
    pub poll_interval: Duration,
    /// How long a claimed job stays claimed without a heartbeat.
    pub lease_duration: Duration,
    /// Hard ceiling on pages per document — a resource limit, per
    /// `docs/block-01-spec.md` §13.9.
    pub max_pages_per_document: u32,
    /// Wall-clock budget for one page, including recognition.
    pub page_timeout: Duration,
    /// Scratch directory for page renders. Rendered images are transient: they are
    /// deleted after the page is read and are never published through the API.
    pub work_dir: Option<PathBuf>,
    pub ocr: OcrSettings,
}

/// OCR adapter configuration. See the module docs for why the binaries are validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcrSettings {
    /// `false` disables recognition entirely; pages that need it say so.
    pub enabled: bool,
    /// Executable that recognises an image (default `tesseract`).
    pub engine_bin: String,
    /// Executable that renders a PDF page to an image (default `pdftoppm`).
    pub renderer_bin: String,
    /// Language pack list passed to the engine, e.g. `rus+eng`.
    pub languages: String,
    pub dpi: u32,
    pub timeout: Duration,
    /// Upper bound on recognised pages in a single job, so one scanned catalogue
    /// cannot occupy the worker indefinitely.
    pub max_pages_per_run: u32,
}

impl Default for OcrSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            engine_bin: "tesseract".to_owned(),
            renderer_bin: "pdftoppm".to_owned(),
            languages: "rus+eng".to_owned(),
            dpi: 220,
            timeout: Duration::from_secs(120),
            max_pages_per_run: 200,
        }
    }
}

impl Default for ExtractionSettings {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(2),
            lease_duration: Duration::from_secs(120),
            max_pages_per_document: 500,
            page_timeout: Duration::from_secs(60),
            work_dir: None,
            ocr: OcrSettings::default(),
        }
    }
}

impl ExtractionSettings {
    pub fn load(source: &dyn ConfigSource) -> Result<Self, AppError> {
        let defaults = Self::default();

        let poll_interval = bounded_duration(
            source,
            "OTDEL_WORKER_POLL_INTERVAL_SECONDS",
            defaults.poll_interval,
            POLL_INTERVAL_RANGE,
        )?;
        let lease_duration = bounded_duration(
            source,
            "OTDEL_WORKER_LEASE_SECONDS",
            defaults.lease_duration,
            LEASE_RANGE,
        )?;
        let page_timeout = bounded_duration(
            source,
            "OTDEL_EXTRACT_PAGE_TIMEOUT_SECONDS",
            defaults.page_timeout,
            PAGE_TIMEOUT_RANGE,
        )?;

        // A page that may legitimately run for `page_timeout` must not lose its lease
        // while it is still running: the maintenance worker would hand the same job to
        // another worker and both would write the same pages.
        if page_timeout >= lease_duration {
            return Err(AppError::validation(
                "OTDEL_EXTRACT_PAGE_TIMEOUT_SECONDS must be smaller than \
                 OTDEL_WORKER_LEASE_SECONDS, otherwise a page that uses its whole budget \
                 loses the job lease while it is still running",
            ));
        }

        let max_pages_per_document = bounded_u32(
            source,
            "OTDEL_EXTRACT_MAX_PAGES",
            u64::from(defaults.max_pages_per_document),
            MAX_PAGES_RANGE,
        )?;

        let work_dir = match source.get("OTDEL_EXTRACT_WORKDIR") {
            Some(value) if !value.trim().is_empty() => Some(PathBuf::from(value.trim())),
            _ => None,
        };

        Ok(Self {
            poll_interval,
            lease_duration,
            max_pages_per_document,
            page_timeout,
            work_dir,
            ocr: OcrSettings::load(source)?,
        })
    }
}

impl OcrSettings {
    pub fn load(source: &dyn ConfigSource) -> Result<Self, AppError> {
        let defaults = Self::default();

        let enabled = match source.get("OTDEL_OCR_ENABLED") {
            Some(value) => parse_bool(&value, "OTDEL_OCR_ENABLED")?,
            None => defaults.enabled,
        };

        let engine_bin = executable(source, "OTDEL_OCR_ENGINE_BIN", &defaults.engine_bin)?;
        let renderer_bin = executable(source, "OTDEL_OCR_RENDERER_BIN", &defaults.renderer_bin)?;

        let languages = string_or(source, "OTDEL_OCR_LANGUAGES", &defaults.languages);
        // `-` is allowed inside a code (`chi-sim`) but never at the start: the value is
        // passed as an argument and a leading `-` would be read as an option.
        if languages.is_empty()
            || languages.starts_with('-')
            || !languages
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '_' || c == '-')
        {
            return Err(AppError::validation(
                "OTDEL_OCR_LANGUAGES must be language codes joined with `+`, e.g. `rus+eng` \
                 (it is passed to the recognition engine as a command-line argument)",
            ));
        }

        let dpi = bounded_u32(
            source,
            "OTDEL_OCR_DPI",
            u64::from(defaults.dpi),
            OCR_DPI_RANGE,
        )?;
        let timeout = bounded_duration(
            source,
            "OTDEL_OCR_TIMEOUT_SECONDS",
            defaults.timeout,
            OCR_TIMEOUT_RANGE,
        )?;
        let max_pages_per_run = bounded_u32(
            source,
            "OTDEL_OCR_MAX_PAGES_PER_RUN",
            u64::from(defaults.max_pages_per_run),
            MAX_PAGES_RANGE,
        )?;

        Ok(Self {
            enabled,
            engine_bin,
            renderer_bin,
            languages,
            dpi,
            timeout,
            max_pages_per_run,
        })
    }
}

/// Validate an executable name/path that will be spawned.
///
/// A bare name is resolved through `PATH` by the operating system; a value containing a
/// separator must be an absolute path, so a relative `./tesseract` picked up from the
/// process's working directory cannot be substituted. A leading `-` is refused because
/// it would be read as an option by whatever ends up being executed.
fn executable(source: &dyn ConfigSource, key: &str, default: &str) -> Result<String, AppError> {
    let value = string_or(source, key, default);
    if value.is_empty() {
        return Err(AppError::validation(format!("{key} must not be empty")));
    }
    if value.starts_with('-') {
        return Err(AppError::validation(format!(
            "{key} must not start with `-`"
        )));
    }
    if value.chars().any(|c| c.is_control()) {
        return Err(AppError::validation(format!(
            "{key} must not contain control characters"
        )));
    }
    if value.contains(std::path::MAIN_SEPARATOR) && !std::path::Path::new(&value).is_absolute() {
        return Err(AppError::validation(format!(
            "{key} must be either a bare executable name resolved through PATH or an \
             absolute path, not a relative path"
        )));
    }
    Ok(value)
}

fn bounded_u32(
    source: &dyn ConfigSource,
    key: &str,
    default: u64,
    (min, max): (u64, u64),
) -> Result<u32, AppError> {
    let value = parse_u64_or(source, key, default)?;
    if value < min || value > max {
        return Err(AppError::validation(format!(
            "{key} must be between {min} and {max}"
        )));
    }
    u32::try_from(value).map_err(|_| AppError::validation(format!("{key} is too large")))
}

fn bounded_duration(
    source: &dyn ConfigSource,
    key: &str,
    default: Duration,
    (min, max): (u64, u64),
) -> Result<Duration, AppError> {
    let value = duration_secs_or(source, key, default.as_secs())?;
    if value.as_secs() < min || value.as_secs() > max {
        return Err(AppError::validation(format!(
            "{key} must be between {min} and {max} seconds"
        )));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn defaults_are_usable_and_bounded() {
        let settings = ExtractionSettings::load(&env(&[])).unwrap();
        assert_eq!(settings, ExtractionSettings::default());
        assert!(settings.page_timeout < settings.lease_duration);
        assert!(settings.ocr.enabled);
        assert_eq!(settings.ocr.languages, "rus+eng");
    }

    #[test]
    fn ocr_can_be_turned_off_explicitly() {
        let settings = ExtractionSettings::load(&env(&[("OTDEL_OCR_ENABLED", "false")])).unwrap();
        assert!(!settings.ocr.enabled);
    }

    #[test]
    fn a_page_budget_may_not_outlast_the_lease() {
        let error = ExtractionSettings::load(&env(&[
            ("OTDEL_WORKER_LEASE_SECONDS", "60"),
            ("OTDEL_EXTRACT_PAGE_TIMEOUT_SECONDS", "60"),
        ]))
        .unwrap_err();
        assert!(error.message.contains("OTDEL_WORKER_LEASE_SECONDS"));
    }

    #[test]
    fn executables_that_could_be_hijacked_are_refused() {
        for value in ["-rf", "./tesseract", "bin/tesseract", "../../tesseract"] {
            assert!(
                ExtractionSettings::load(&env(&[("OTDEL_OCR_ENGINE_BIN", value)])).is_err(),
                "must reject engine binary `{value}`"
            );
        }
        // A bare name and an absolute path are both fine.
        assert_eq!(
            ExtractionSettings::load(&env(&[("OTDEL_OCR_ENGINE_BIN", "tesseract5")]))
                .unwrap()
                .ocr
                .engine_bin,
            "tesseract5"
        );
        assert_eq!(
            ExtractionSettings::load(&env(&[("OTDEL_OCR_ENGINE_BIN", "/usr/bin/tesseract")]))
                .unwrap()
                .ocr
                .engine_bin,
            "/usr/bin/tesseract"
        );
    }

    #[test]
    fn language_lists_are_restricted_to_safe_tokens() {
        for value in ["rus eng", "rus;rm -rf /", "--oem", "rus/../eng"] {
            assert!(
                ExtractionSettings::load(&env(&[("OTDEL_OCR_LANGUAGES", value)])).is_err(),
                "must reject language list `{value}`"
            );
        }
        assert_eq!(
            ExtractionSettings::load(&env(&[("OTDEL_OCR_LANGUAGES", "rus+eng+deu")]))
                .unwrap()
                .ocr
                .languages,
            "rus+eng+deu"
        );
        // A blank override is "not configured", so the default stands rather than
        // producing an empty argument.
        assert_eq!(
            ExtractionSettings::load(&env(&[("OTDEL_OCR_LANGUAGES", "  ")]))
                .unwrap()
                .ocr
                .languages,
            OcrSettings::default().languages
        );
    }

    #[test]
    fn numeric_limits_are_range_checked() {
        assert!(ExtractionSettings::load(&env(&[("OTDEL_OCR_DPI", "10")])).is_err());
        assert!(ExtractionSettings::load(&env(&[("OTDEL_OCR_DPI", "5000")])).is_err());
        assert!(ExtractionSettings::load(&env(&[("OTDEL_EXTRACT_MAX_PAGES", "0")])).is_err());
        assert_eq!(
            ExtractionSettings::load(&env(&[("OTDEL_EXTRACT_MAX_PAGES", "64")]))
                .unwrap()
                .max_pages_per_document,
            64
        );
    }
}
