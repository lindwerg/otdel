//! The two adapter contracts of phase 1D, and the vocabulary of their failures.
//!
//! They are narrow on purpose. A search adapter takes one query string and a result
//! count and returns a list of URLs; a fetcher takes one already-validated
//! [`NormalisedUrl`] and returns text. Neither decides what to look for, neither follows
//! a link, and neither retries: the plan, the budget, the limits and the retry policy
//! all live outside, where they can be seen.
//!
//! The failure enumerations exist mostly so that one distinction survives: a request
//! that never left this machine costs nothing, and a request that left with an unknown
//! outcome costs money that has to be reconciled (`docs/block-01-spec.md` §10). Collapsing
//! those two into "error" is how a budget silently stops being a budget.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::url::{NormalisedUrl, UrlRejection};

/// One bounded search request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    /// Exactly the text that will be sent. The caller has already checked it is safe to
    /// send (`otdel_research::query`), and the same string is stored in the journal.
    pub query: String,
    pub max_results: u32,
}

/// One result. A hit is a *lead*, not a source: nothing here may be quoted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// As the provider wrote it. Validated by the caller before anything is opened.
    pub url: String,
    pub title: Option<String>,
    /// The provider's own summary. Recorded for the owner to read, never usable as
    /// evidence: it is the search engine's sentence about a page, not the page.
    pub snippet: Option<String>,
}

/// What one search call produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchAnswer {
    pub hits: Vec<SearchHit>,
    pub duration: Duration,
}

/// Why a search call did not produce results.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SearchError {
    /// No endpoint, no key or no allowlist. **Nothing was sent.**
    #[error("поиск не настроен: {0}")]
    NotConfigured(String),

    #[error("поисковый провайдер ответил ошибкой {status}")]
    Http { status: u16, retryable: bool },

    #[error("превышен лимит запросов поискового провайдера")]
    RateLimited,

    /// The connection was never established: nothing was sent, nothing was spent.
    #[error("не удалось соединиться с поисковым провайдером")]
    Transport,

    /// The request left this machine and no answer came back. Charged as spent and
    /// flagged for reconciliation — it may well have been billed.
    #[error("ответ поискового провайдера не получен: исход запроса неизвестен")]
    UnknownOutcome,

    #[error("ответ поискового провайдера не разбирается: {0}")]
    InvalidResponse(String),
}

impl SearchError {
    /// Whether repeating the same call later can plausibly succeed.
    ///
    /// A missing configuration is *not* transient: the queue must stop and say so rather
    /// than burn attempts against something only the owner can fix.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Http { retryable, .. } => *retryable,
            Self::RateLimited | Self::Transport | Self::UnknownOutcome => true,
            Self::NotConfigured(_) | Self::InvalidResponse(_) => false,
        }
    }

    /// Did the request leave this machine? Only then is there money to account for.
    pub fn was_sent(&self) -> bool {
        match self {
            // The provider answered, so it certainly received the request.
            Self::Http { .. } | Self::RateLimited | Self::InvalidResponse(_) => true,
            Self::UnknownOutcome => true,
            Self::NotConfigured(_) | Self::Transport => false,
        }
    }

    /// One bounded line for the journal. Never contains a key or a prompt.
    pub fn diagnostic(&self) -> String {
        bounded_line(&self.to_string())
    }
}

/// A page that was really downloaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedDocument {
    pub url: NormalisedUrl,
    pub http_status: u16,
    pub content_type: Option<String>,
    /// Size of the body as received, before any extraction.
    pub bytes_len: u64,
    /// SHA-256 of those bytes. Two reads with the same hash are the same document.
    pub content_hash: String,
    /// The stored snapshot: plain text, whitespace collapsed, bounded.
    pub text: String,
    pub title: Option<String>,
    /// Only when the page declares one. `None` means "not stated".
    pub license: Option<String>,
    pub license_note: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub retrieved_at: DateTime<Utc>,
    pub duration: Duration,
    /// `true` when the text hit the character limit.
    pub truncated: bool,
}

/// Why a page was not read. Every value maps onto a
/// [`otdel_core::research::SourceStatus`] the owner sees in the journal.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FetchRefusal {
    /// The adapter has no HTTP client at all.
    #[error("загрузка источников не настроена: {0}")]
    NotConfigured(String),

    #[error("{0}")]
    InvalidUrl(#[from] UrlRejection),

    #[error("хост `{0}` не входит в список разрешённых источников")]
    HostNotAllowed(String),

    #[error("robots.txt этого сайта запрещает читать эту страницу")]
    RobotsDisallowed,

    /// `robots.txt` could not be read, so permission could not be established.
    #[error("не удалось прочитать robots.txt сайта: без него страница не читается")]
    RobotsUnavailable,

    #[error("тип содержимого `{0}` не читается на этом этапе")]
    UnsupportedType(String),

    #[error("ответ больше разрешённых {limit} байт")]
    TooLarge { limit: u64 },

    /// A redirect is never followed: it is the classic way past a host allowlist.
    #[error("сайт ответил перенаправлением ({status}); переадресация не выполняется")]
    Redirected { status: u16 },

    #[error("сайт ответил ошибкой {status}")]
    Http { status: u16, retryable: bool },

    #[error("сайт не ответил вовремя")]
    Timeout,

    #[error("не удалось соединиться с сайтом")]
    Transport,

    #[error("страница не содержит текста")]
    Empty,
}

impl FetchRefusal {
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Http { retryable, .. } => *retryable,
            Self::Timeout | Self::Transport | Self::RobotsUnavailable => true,
            Self::NotConfigured(_)
            | Self::InvalidUrl(_)
            | Self::HostNotAllowed(_)
            | Self::RobotsDisallowed
            | Self::UnsupportedType(_)
            | Self::TooLarge { .. }
            | Self::Redirected { .. }
            | Self::Empty => false,
        }
    }

    /// Was the page this reservation paid for actually requested?
    ///
    /// The priced operation is "fetch this document". The refusals decided before that
    /// request — allowlist, URL shape, missing configuration — cost nothing. So do the two
    /// robots outcomes: checking `robots.txt` is an unpriced courtesy request, and a page
    /// that was never asked for must not be billed as if it had been.
    pub fn was_sent(&self) -> bool {
        !matches!(
            self,
            Self::NotConfigured(_)
                | Self::InvalidUrl(_)
                | Self::HostNotAllowed(_)
                | Self::RobotsDisallowed
                | Self::RobotsUnavailable
                | Self::Transport
        )
    }

    pub fn diagnostic(&self) -> String {
        bounded_line(&self.to_string())
    }
}

/// What the interface tells the owner about an adapter. Contains no secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterDescription {
    /// `http_json`, `disabled` or `fake`.
    pub provider: String,
    /// Host only, e.g. `search.example.com`. Never a key, never a full URL with a query.
    pub endpoint_host: Option<String>,
    /// `ready`, `needs_configuration` or `disabled`.
    pub state: &'static str,
    /// Environment variables the owner still has to set.
    pub missing: Vec<&'static str>,
    /// Hosts this adapter may contact, as configured.
    pub allowed_hosts: Vec<String>,
    /// One sentence for the interface, in Russian.
    pub message: String,
}

impl AdapterDescription {
    pub fn is_ready(&self) -> bool {
        self.state == "ready"
    }
}

/// An adapter that finds candidate sources.
#[async_trait]
pub trait SearchProvider: Send + Sync {
    fn describe(&self) -> AdapterDescription;

    /// Perform one search. Implementations must not retry internally.
    async fn search(&self, request: &SearchRequest) -> Result<SearchAnswer, SearchError>;
}

/// An adapter that reads one page of one allowed host.
#[async_trait]
pub trait DocumentFetcher: Send + Sync {
    fn describe(&self) -> AdapterDescription;

    /// Download and extract one document. The URL has already been validated; the
    /// allowlist, `robots.txt` and every size limit are enforced inside.
    async fn fetch(&self, url: &NormalisedUrl) -> Result<FetchedDocument, FetchRefusal>;
}

impl std::fmt::Debug for dyn SearchProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let description = self.describe();
        f.debug_struct("SearchProvider")
            .field("provider", &description.provider)
            .field("state", &description.state)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for dyn DocumentFetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let description = self.describe();
        f.debug_struct("DocumentFetcher")
            .field("provider", &description.provider)
            .field("state", &description.state)
            .field("allowed_hosts", &description.allowed_hosts.len())
            .finish_non_exhaustive()
    }
}

/// Why a response body could not be read to the end.
///
/// Shared by both clients so neither can forget the bound: a server that declares no
/// `Content-Length`, or lies about it, must not be able to make this process allocate
/// until it dies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyReadError {
    TooLarge,
    Timeout,
    Transport,
}

/// Read a body, stopping the moment it exceeds `limit`.
///
/// Streaming rather than `bytes()`, which buffers whatever arrives regardless of any
/// declared length.
pub(crate) async fn read_bounded(
    mut response: reqwest::Response,
    limit: u64,
) -> Result<Vec<u8>, BodyReadError> {
    let mut body: Vec<u8> = Vec::with_capacity(8 * 1024);
    loop {
        let chunk = response.chunk().await.map_err(|error| {
            if error.is_timeout() {
                BodyReadError::Timeout
            } else {
                BodyReadError::Transport
            }
        })?;
        let Some(chunk) = chunk else { break };
        if body.len() as u64 + chunk.len() as u64 > limit {
            return Err(BodyReadError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// One line, bounded, no control characters — safe to store and to show.
fn bounded_line(message: &str) -> String {
    message
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(300)
        .collect::<String>()
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_that_never_left_the_machine_is_not_charged() {
        for error in [
            SearchError::NotConfigured("нет ключа".to_owned()),
            SearchError::Transport,
        ] {
            assert!(
                !error.was_sent(),
                "{error} must not be charged: nothing was sent"
            );
        }

        for error in [
            SearchError::RateLimited,
            SearchError::Http {
                status: 500,
                retryable: true,
            },
            SearchError::InvalidResponse("не JSON".to_owned()),
            // The one that matters most: the provider may well have billed it.
            SearchError::UnknownOutcome,
        ] {
            assert!(
                error.was_sent(),
                "{error} reached the provider and is charged"
            );
        }
    }

    #[test]
    fn a_missing_configuration_is_not_a_transient_failure() {
        assert!(!SearchError::NotConfigured("нет ключа".to_owned()).is_retryable());
        assert!(!SearchError::InvalidResponse("x".to_owned()).is_retryable());
        assert!(SearchError::RateLimited.is_retryable());
        assert!(SearchError::Transport.is_retryable());
        assert!(SearchError::UnknownOutcome.is_retryable());
        assert!(!SearchError::Http {
            status: 401,
            retryable: false
        }
        .is_retryable());
    }

    #[test]
    fn refusals_decided_before_the_page_was_requested_cost_nothing() {
        for refusal in [
            FetchRefusal::NotConfigured("нет списка хостов".to_owned()),
            FetchRefusal::HostNotAllowed("evil.example".to_owned()),
            FetchRefusal::RobotsDisallowed,
            // The site could not be asked for its rules, so the page was never requested
            // either — and this one is worth trying again, unlike the rest.
            FetchRefusal::RobotsUnavailable,
            FetchRefusal::InvalidUrl(UrlRejection::IpLiteralHost),
            FetchRefusal::Transport,
        ] {
            assert!(!refusal.was_sent(), "{refusal} must not be charged");
        }
        assert!(
            FetchRefusal::RobotsUnavailable.is_retryable(),
            "a blip while reading robots.txt is not the site refusing permission"
        );
        assert!(!FetchRefusal::RobotsDisallowed.is_retryable());
        // The two must not be confused: only one of them claims the site said no.
        assert!(FetchRefusal::RobotsDisallowed
            .diagnostic()
            .contains("запрещает"));
        assert!(FetchRefusal::RobotsUnavailable
            .diagnostic()
            .contains("не удалось прочитать"));

        assert!(FetchRefusal::Http {
            status: 404,
            retryable: false
        }
        .was_sent());
        assert!(FetchRefusal::TooLarge { limit: 1024 }.was_sent());
        assert!(FetchRefusal::Redirected { status: 302 }.was_sent());
    }

    #[test]
    fn a_redirect_is_a_refusal_and_is_never_worth_repeating() {
        let refusal = FetchRefusal::Redirected { status: 301 };
        assert!(!refusal.is_retryable());
        assert!(refusal
            .diagnostic()
            .contains("переадресация не выполняется"));
    }

    #[test]
    fn diagnostics_are_one_bounded_line() {
        let message = SearchError::InvalidResponse("строка\nс переводом".to_owned()).diagnostic();
        assert!(!message.contains('\n'));
        assert!(message.chars().count() <= 300);

        let long = FetchRefusal::UnsupportedType("x".repeat(1_000)).diagnostic();
        assert!(long.chars().count() <= 300);
    }
}
