//! Failures of the reading pipeline.
//!
//! Every variant answers two questions the worker has to ask: *what should the operator
//! be told* (the `Display` text, which is written verbatim into the page or material
//! diagnostic, so it must never contain a path, a stack trace or SQL) and *can repeating
//! this help* ([`ExtractError::is_permanent`]).

/// Something went wrong while reading a document or one of its pages.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("файл не открывается как PDF: {0}")]
    NotAPdf(String),

    #[error("PDF зашифрован: без пароля документ прочитать нельзя")]
    Encrypted,

    #[error("в документе не найдено ни одной страницы")]
    NoPages,

    #[error("страница {page} отсутствует в документе")]
    PageMissing { page: u32 },

    #[error("в документе {found} страниц — больше допустимого предела {limit}")]
    TooManyPages { found: u32, limit: u32 },

    #[error("страница {page} не прочитана: {reason}")]
    Page { page: u32, reason: String },

    /// The third-party PDF parser aborted. It is written defensively enough to be
    /// usable, but not defensively enough to promise it never panics on a malformed
    /// file, so the worker runs it inside a catch and reports *this* instead of dying.
    #[error("разбор PDF прерван внутренней ошибкой парсера ({context})")]
    ParserCrashed { context: String },

    #[error("страница {page} не прочитана за отведённое время")]
    PageTimeout { page: u32 },

    #[error("не удалось прочитать файл: {0}")]
    Io(String),
}

impl ExtractError {
    /// `true` when repeating the exact same job cannot change the outcome.
    ///
    /// The distinction is what keeps the queue from grinding on a file that will never
    /// be a PDF, while still retrying a page that lost a race with a busy machine.
    pub fn is_permanent(&self) -> bool {
        match self {
            Self::NotAPdf(_)
            | Self::Encrypted
            | Self::NoPages
            | Self::PageMissing { .. }
            | Self::TooManyPages { .. }
            | Self::ParserCrashed { .. } => true,
            Self::Page { .. } | Self::PageTimeout { .. } | Self::Io(_) => false,
        }
    }
}

/// Failure of an external tool (OCR engine, page rasteriser).
///
/// Separate from [`ExtractError`] because "the tool is not installed" is a normal,
/// expected state of this system — not an error of the document — and must lead to an
/// honest `needs_ocr`, never to a failed job.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("{tool} недоступен: {reason}")]
    Unavailable { tool: String, reason: String },

    #[error("{tool} не ответил за {seconds} с")]
    Timeout { tool: String, seconds: u64 },

    #[error("{tool} завершился с ошибкой: {reason}")]
    Failed { tool: String, reason: String },

    #[error("{tool}: не удалось прочитать результат: {reason}")]
    Output { tool: String, reason: String },
}

pub type ExtractResult<T> = Result<T, ExtractError>;
pub type ToolResult<T> = Result<T, ToolError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permanent_failures_are_not_retried_forever() {
        assert!(ExtractError::NotAPdf("нет заголовка".into()).is_permanent());
        assert!(ExtractError::Encrypted.is_permanent());
        assert!(ExtractError::TooManyPages {
            found: 9000,
            limit: 500
        }
        .is_permanent());
        assert!(!ExtractError::Io("диск занят".into()).is_permanent());
        assert!(!ExtractError::PageTimeout { page: 3 }.is_permanent());
    }

    #[test]
    fn messages_are_written_for_a_person_and_carry_no_internals() {
        let message = ExtractError::Page {
            page: 7,
            reason: "текстовый слой повреждён".into(),
        }
        .to_string();
        assert!(message.contains('7'));
        assert!(!message.contains("panicked"));
        assert!(!message.contains('/'));
    }
}
