//! The adapter contract: one bounded request, one JSON object back, or a stated reason.
//!
//! The trait is deliberately narrow. A role in this system never gets a free-form
//! conversation, tools, or the ability to decide what to fetch next: it gets one
//! prompt built from material the server chose, and must answer with one JSON object
//! matching a schema the server wrote. Everything else — retries, persistence, access
//! control — happens outside.

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

/// One bounded call.
///
/// `schema` is sent to the provider (structured-output mode) *and* enforced again on
/// the way back by the caller: a provider that ignores it must not be able to slip an
/// unexpected shape through.
#[derive(Debug, Clone)]
pub struct LlmRequest {
    /// Short label of what this call is for; appears in logs, never the content.
    pub purpose: &'static str,
    pub system_prompt: String,
    pub user_prompt: String,
    /// Name of the schema, sent to the provider with the schema itself.
    pub schema_name: &'static str,
    pub schema: Value,
    pub max_output_tokens: u32,
}

impl LlmRequest {
    /// Characters of prompt, for the run's bookkeeping.
    pub fn input_chars(&self) -> usize {
        self.system_prompt.chars().count() + self.user_prompt.chars().count()
    }
}

/// Token accounting as reported by the provider, when it reports any.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
}

/// A parsed, syntactically valid JSON object from the model.
#[derive(Debug, Clone)]
pub struct LlmResponse {
    /// The decoded JSON. Validation against the domain rules is the caller's job.
    pub json: Value,
    /// Model the provider says answered — not necessarily the one asked for, which is
    /// why it is recorded rather than assumed.
    pub model: String,
    pub usage: Usage,
    pub duration: Duration,
    pub response_chars: usize,
}

/// Why a call did not produce usable JSON.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// No key, no model or no endpoint. Nothing was sent.
    #[error("модель не настроена: {0}")]
    NotConfigured(String),

    #[error("провайдер ответил ошибкой {status}")]
    Http { status: u16, retryable: bool },

    #[error("превышен лимит запросов провайдера")]
    RateLimited,

    #[error("превышено время ожидания ответа модели")]
    Timeout,

    #[error("не удалось соединиться с провайдером")]
    Transport,

    /// The answer was cut off by `max_tokens`: a truncated JSON object is not a partial
    /// result, it is no result.
    #[error("ответ модели оборван лимитом длины")]
    Truncated,

    /// Body too large, not JSON, no content, or JSON that is not an object.
    #[error("ответ модели не является корректным JSON-объектом: {0}")]
    InvalidResponse(String),

    /// The run's own limit on requests was reached.
    #[error("исчерпан лимит запросов к модели на один запуск ({limit})")]
    RequestBudgetExhausted { limit: u32 },
}

impl LlmError {
    /// Whether repeating the same call later can plausibly succeed.
    ///
    /// A missing key is *not* transient: the queue must stop and say so instead of
    /// burning attempts against a configuration problem.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Http { retryable, .. } => *retryable,
            Self::RateLimited | Self::Timeout | Self::Transport => true,
            Self::NotConfigured(_)
            | Self::Truncated
            | Self::InvalidResponse(_)
            | Self::RequestBudgetExhausted { .. } => false,
        }
    }

    /// One bounded line for the run record. Never contains prompt text or a key.
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

pub type LlmResult<T> = Result<T, LlmError>;

/// What the interface tells the owner about the model adapter.
///
/// Contains no secret: the key is never part of a description, and the endpoint is
/// reduced to its host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDescription {
    /// `openrouter`, `openai_compatible`, `disabled` or `fake`.
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
}

impl ProviderDescription {
    pub fn is_ready(&self) -> bool {
        self.state == "ready"
    }
}

/// A model adapter. Object-safe on purpose: the worker holds `Arc<dyn LlmProvider>`,
/// so the unconfigured, the real and the test adapters are interchangeable.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn describe(&self) -> ProviderDescription;

    /// Perform one call. Implementations must not retry internally: retry policy
    /// belongs to the queue, which can see the attempt counter.
    async fn complete_json(&self, request: &LlmRequest) -> LlmResult<LlmResponse>;
}

impl fmt::Debug for dyn LlmProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let description = self.describe();
        f.debug_struct("LlmProvider")
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
    fn a_missing_key_is_not_a_transient_failure() {
        assert!(!LlmError::NotConfigured("нет ключа".to_owned()).is_retryable());
        assert!(!LlmError::Truncated.is_retryable());
        assert!(!LlmError::InvalidResponse("не объект".to_owned()).is_retryable());
        assert!(!LlmError::RequestBudgetExhausted { limit: 8 }.is_retryable());

        assert!(LlmError::RateLimited.is_retryable());
        assert!(LlmError::Timeout.is_retryable());
        assert!(LlmError::Transport.is_retryable());
        assert!(LlmError::Http {
            status: 503,
            retryable: true
        }
        .is_retryable());
        assert!(!LlmError::Http {
            status: 401,
            retryable: false
        }
        .is_retryable());
    }

    #[test]
    fn diagnostics_are_one_bounded_line() {
        let message = LlmError::InvalidResponse("строка\nс переводом".to_owned()).diagnostic();
        assert!(!message.contains('\n'));
        assert!(message.chars().count() <= 300);
    }
}
