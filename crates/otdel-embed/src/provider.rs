//! The adapter contract: a batch of texts in, the same number of vectors out, in order —
//! or a stated reason and nothing at all.
//!
//! Two things are deliberately absent from this trait. There is no "embed one text"
//! convenience, because every caller in this system embeds a batch and a per-item helper
//! would quietly turn one paced request into a hundred. And there is no fallback: an
//! implementation that cannot reach its service returns an error, never a substitute
//! vector. A hashed or zeroed stand-in would satisfy the type, populate the column and
//! make every distance computed against it meaningless while looking exactly like
//! semantic search.
//!
//! The ordering rule is the other half of that. A batch of `n` texts must come back as
//! `n` vectors, positionally matched, or as [`EmbedError::CountMismatch`]. The caller
//! zips the answer with the claims it sent; a provider that drops or reorders an entry
//! would attach a vector to the wrong claim, and nothing downstream could ever notice.

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;

/// One bounded batch.
///
/// `inputs` are the texts as the caller wants them embedded; the client clips each one to
/// the configured ceiling before sending, and the clipping is a property of the call, not
/// a silent edit of what the caller holds.
#[derive(Debug, Clone)]
pub struct EmbedRequest {
    /// Short label of what this batch is for; appears in logs, never the content.
    pub purpose: &'static str,
    pub inputs: Vec<String>,
}

impl EmbedRequest {
    /// Characters submitted, for the run's bookkeeping.
    pub fn input_chars(&self) -> usize {
        self.inputs.iter().map(|input| input.chars().count()).sum()
    }
}

/// Vectors for one batch, positionally matched to the inputs that produced them.
#[derive(Debug, Clone)]
pub struct EmbedResponse {
    /// One vector per input, in the order the inputs were given. Guaranteed by the
    /// client: a response that did not satisfy this is an error, not a shorter list.
    pub vectors: Vec<Vec<f32>>,
    /// Model the service says answered — not necessarily the one asked for, which is why
    /// it is recorded rather than assumed.
    pub model: String,
    /// Length shared by every vector in this response.
    pub dimensions: usize,
    pub duration: Duration,
}

/// Why a batch did not produce usable vectors.
#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    /// No key, no model or no endpoint. Nothing was sent.
    #[error("сервис эмбеддингов не настроен: {0}")]
    NotConfigured(String),

    #[error("сервис эмбеддингов ответил ошибкой {status}")]
    Http { status: u16, retryable: bool },

    #[error("превышен лимит запросов сервиса эмбеддингов")]
    RateLimited,

    #[error("превышено время ожидания ответа сервиса эмбеддингов")]
    Timeout,

    #[error("не удалось соединиться с сервисом эмбеддингов")]
    Transport,

    /// Body too large, not JSON, no `data`, or a vector containing something that is not
    /// a finite number.
    #[error("ответ сервиса эмбеддингов не разбирается: {0}")]
    InvalidResponse(String),

    /// Vectors of unequal length arrived in one batch. They cannot all belong to the same
    /// space, so none of them is trusted.
    #[error("размерность вектора не совпадает: ожидалось {expected}, получено {got}")]
    DimensionMismatch { expected: usize, got: usize },

    /// The service returned a different number of vectors than there were inputs.
    ///
    /// Never recoverable by zipping what did arrive: the surplus or the gap shifts every
    /// following vector onto the wrong text, and the result would look perfectly normal
    /// in the database.
    #[error("получено {got} векторов вместо {expected}")]
    CountMismatch { expected: usize, got: usize },
}

impl EmbedError {
    /// Whether repeating the same batch later can plausibly succeed.
    ///
    /// A missing key is *not* transient, and neither is a service that answers with the
    /// wrong number of vectors: repeating either burns attempts against a problem that
    /// only a human can fix.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Http { retryable, .. } => *retryable,
            Self::RateLimited | Self::Timeout | Self::Transport => true,
            Self::NotConfigured(_)
            | Self::InvalidResponse(_)
            | Self::DimensionMismatch { .. }
            | Self::CountMismatch { .. } => false,
        }
    }

    /// One bounded line for the run record. Never contains input text or a key.
    pub fn diagnostic(&self) -> String {
        self.to_string()
            .chars()
            .map(|ch| if ch.is_control() { ' ' } else { ch })
            .take(300)
            .collect::<String>()
            .trim()
            .to_owned()
    }
}

pub type EmbedResult<T> = Result<T, EmbedError>;

/// What the interface tells the owner about the embedding adapter.
///
/// Contains no secret: the key is never part of a description, and the endpoint is
/// reduced to its host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingDescription {
    /// `openai_compatible`, `disabled` or `fake`.
    pub provider: String,
    /// Model identifier, empty when none is configured.
    pub model: String,
    /// Host only, e.g. `openrouter.ai`.
    pub endpoint_host: Option<String>,
    /// `ready`, `needs_configuration` or `disabled`.
    pub state: &'static str,
    /// Environment variables the owner still has to set.
    pub missing: Vec<&'static str>,
    /// One sentence for the interface, in Russian.
    pub message: String,
    /// Identifier of the vector space this adapter writes into, when there is one.
    ///
    /// `None` is the whole point of the unconfigured state: a row can only carry a
    /// profile that some adapter actually produced, so nothing without vectors can claim
    /// to have them (`block-01-spec.md` §9).
    pub profile: Option<String>,
}

impl EmbeddingDescription {
    pub fn is_ready(&self) -> bool {
        self.state == "ready"
    }
}

/// An embedding adapter. Object-safe on purpose: the worker holds
/// `Arc<dyn EmbeddingProvider>`, so the unconfigured, the real and the test adapters are
/// interchangeable.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    fn describe(&self) -> EmbeddingDescription;

    /// Embed a batch of texts, in order. Implementations must not retry internally:
    /// retry policy belongs to the queue, which can see the attempt counter.
    async fn embed(&self, request: &EmbedRequest) -> EmbedResult<EmbedResponse>;
}

impl fmt::Debug for dyn EmbeddingProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let description = self.describe();
        f.debug_struct("EmbeddingProvider")
            .field("provider", &description.provider)
            .field("model", &description.model)
            .field("state", &description.state)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_key_and_a_miscounted_batch_are_not_transient_failures() {
        assert!(!EmbedError::NotConfigured("нет ключа".to_owned()).is_retryable());
        assert!(!EmbedError::InvalidResponse("не JSON".to_owned()).is_retryable());
        assert!(!EmbedError::DimensionMismatch {
            expected: 1536,
            got: 768
        }
        .is_retryable());
        assert!(!EmbedError::CountMismatch {
            expected: 8,
            got: 7
        }
        .is_retryable());

        assert!(EmbedError::RateLimited.is_retryable());
        assert!(EmbedError::Timeout.is_retryable());
        assert!(EmbedError::Transport.is_retryable());
        assert!(EmbedError::Http {
            status: 503,
            retryable: true
        }
        .is_retryable());
        assert!(!EmbedError::Http {
            status: 400,
            retryable: false
        }
        .is_retryable());
    }

    #[test]
    fn a_count_mismatch_states_both_numbers_so_the_record_is_readable() {
        let message = EmbedError::CountMismatch {
            expected: 32,
            got: 31,
        }
        .diagnostic();
        assert!(message.contains("32"), "{message}");
        assert!(message.contains("31"), "{message}");
    }

    #[test]
    fn diagnostics_are_one_bounded_line() {
        let message = EmbedError::InvalidResponse("строка\nс переводом".to_owned()).diagnostic();
        assert!(!message.contains('\n'));
        assert!(message.chars().count() <= 300);
    }

    #[test]
    fn a_request_counts_the_characters_of_the_whole_batch() {
        let request = EmbedRequest {
            purpose: "chunks",
            inputs: vec!["длина".to_owned(), "ширина".to_owned()],
        };
        assert_eq!(request.input_chars(), 11);
    }

    #[test]
    fn only_the_ready_state_is_ready() {
        let mut description = EmbeddingDescription {
            provider: "openai_compatible".to_owned(),
            model: "m".to_owned(),
            endpoint_host: None,
            state: "needs_configuration",
            missing: vec!["OTDEL_EMBEDDING_API_KEY"],
            message: String::new(),
            profile: None,
        };
        assert!(!description.is_ready());
        description.state = "ready";
        assert!(description.is_ready());
    }
}
