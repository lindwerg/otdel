//! The adapter used when there is no key — the normal state of this pilot today.
//!
//! It holds no HTTP client, so there is no code path from here to a socket. Every call
//! returns [`LlmError::NotConfigured`] naming the variables the owner has to set. This
//! is the property the acceptance check "provider no-key state clear" rests on: the
//! system cannot accidentally call *something*, and it cannot invent an answer either —
//! the caller records the run as `needs_provider` and stores nothing.

use async_trait::async_trait;
use otdel_core::llm_config::{LlmAvailability, LlmSettings};

use crate::provider::{
    LlmError, LlmProvider, LlmRequest, LlmResponse, LlmResult, ProviderDescription,
};

/// A provider that is honest about not being one.
#[derive(Debug, Clone)]
pub struct UnconfiguredProvider {
    description: ProviderDescription,
}

impl UnconfiguredProvider {
    pub fn new(settings: &LlmSettings) -> Self {
        let availability = settings.availability();
        let (state, missing, message) = match &availability {
            LlmAvailability::Disabled => (
                "disabled",
                Vec::new(),
                "Продуктолог отключён настройкой OTDEL_LLM_PROVIDER=disabled. \
                 Чтение материалов работает, структурирование знаний не запускается."
                    .to_owned(),
            ),
            LlmAvailability::NeedsConfiguration { missing } => (
                "needs_configuration",
                missing.clone(),
                format!(
                    "Продуктолог ожидает настройки: не заданы {}. \
                     Пока ключа нет, обращений к модели не происходит и знания не создаются.",
                    missing.join(", ")
                ),
            ),
            // Defensive: only reachable if a caller builds this adapter for a ready
            // configuration. Saying "ready" while refusing every call would be a lie,
            // so the description states what actually happens.
            LlmAvailability::Ready => (
                "needs_configuration",
                Vec::new(),
                "Адаптер модели не создан, обращения к модели не выполняются.".to_owned(),
            ),
        };

        Self {
            description: ProviderDescription {
                provider: settings.provider.as_str().to_owned(),
                model: settings.model.clone(),
                endpoint_host: settings.endpoint_host(),
                state,
                missing,
                message,
            },
        }
    }
}

#[async_trait]
impl LlmProvider for UnconfiguredProvider {
    fn describe(&self) -> ProviderDescription {
        self.description.clone()
    }

    async fn complete_json(&self, _request: &LlmRequest) -> LlmResult<LlmResponse> {
        Err(LlmError::NotConfigured(self.description.message.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn settings(pairs: &[(&str, &str)]) -> LlmSettings {
        let source: BTreeMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        LlmSettings::load(&source).unwrap()
    }

    fn request() -> LlmRequest {
        LlmRequest {
            purpose: "test",
            system_prompt: "s".to_owned(),
            user_prompt: "u".to_owned(),
            schema_name: "test",
            schema: json!({"type": "object"}),
            max_output_tokens: 100,
        }
    }

    #[tokio::test]
    async fn without_a_key_every_call_is_refused_and_says_what_is_missing() {
        let provider = UnconfiguredProvider::new(&settings(&[]));
        let description = provider.describe();
        assert_eq!(description.state, "needs_configuration");
        assert!(description.missing.contains(&"OTDEL_LLM_API_KEY"));
        assert!(description.message.contains("OTDEL_LLM_API_KEY"));
        assert!(!description.is_ready());

        let error = provider.complete_json(&request()).await.unwrap_err();
        assert!(matches!(error, LlmError::NotConfigured(_)));
        assert!(!error.is_retryable(), "a missing key must not be retried");
    }

    #[tokio::test]
    async fn disabled_is_described_as_a_deliberate_choice() {
        let provider = UnconfiguredProvider::new(&settings(&[("OTDEL_LLM_PROVIDER", "disabled")]));
        let description = provider.describe();
        assert_eq!(description.state, "disabled");
        assert!(description.missing.is_empty());
        assert!(provider.complete_json(&request()).await.is_err());
    }
}
