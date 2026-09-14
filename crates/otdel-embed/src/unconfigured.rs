//! The adapter used when there is no embedding service — the normal state of this pilot
//! today.
//!
//! It holds no HTTP client. Not "an unused one", not "one that is never called": the
//! struct has a single field, a description, and therefore there is no code path from
//! here to a socket. Every call returns [`EmbedError::NotConfigured`] naming the
//! variables the owner has to set.
//!
//! What makes this worth a file of its own is the alternative. An embedding adapter is
//! the one place in the system where a fallback is easy to write and impossible to
//! notice: hash the text into 1536 floats, return them, and everything downstream works
//! — chunks get vectors, distances compute, neighbours come back ranked. They would just
//! be wrong, silently, for as long as the fallback lived. So there is no fallback. Search
//! runs in `keyword` mode and reports the reason, and the profile stays `None` so nothing
//! stored can claim to belong to a vector space that was never built.

use async_trait::async_trait;
use otdel_core::retrieval_config::{EmbeddingAvailability, EmbeddingSettings};

use crate::provider::{
    EmbedError, EmbedRequest, EmbedResponse, EmbedResult, EmbeddingDescription, EmbeddingProvider,
};

/// An embedding adapter that is honest about not being one.
#[derive(Debug, Clone)]
pub struct UnconfiguredEmbeddings {
    description: EmbeddingDescription,
}

impl UnconfiguredEmbeddings {
    pub fn new(settings: &EmbeddingSettings) -> Self {
        let availability = settings.availability();
        let (state, missing, message) = match &availability {
            EmbeddingAvailability::Disabled => (
                "disabled",
                Vec::new(),
                "Векторный поиск отключён настройкой OTDEL_EMBEDDING_PROVIDER=disabled. \
                 Поиск работает по ключевым словам."
                    .to_owned(),
            ),
            EmbeddingAvailability::NeedsConfiguration { missing } => (
                "needs_configuration",
                missing.clone(),
                format!(
                    "Векторный поиск ожидает настройки: не заданы {}. \
                     Пока их нет, векторы не строятся, а поиск идёт по ключевым словам.",
                    missing.join(", ")
                ),
            ),
            // Defensive: only reachable if a caller builds this adapter for a ready
            // configuration. Saying "ready" while refusing every call would be a lie, so
            // the description states what actually happens.
            // Reached when the configuration is complete but the HTTP client could not
            // be built (no TLS backend, for instance). `missing` is empty here and there
            // is nothing the owner could add to it — so the message has to carry the
            // whole explanation, or they are told "waiting on configuration" with
            // nothing listed to configure.
            EmbeddingAvailability::Ready => (
                "needs_configuration",
                Vec::new(),
                "Адаптер эмбеддингов настроен, но HTTP-клиент для него создать не \
                 удалось, поэтому векторы не строятся. Переменные окружения здесь ни при \
                 чём — причина в журнале сервера при запуске. Поиск работает по точным \
                 значениям и тексту."
                    .to_owned(),
            ),
        };

        Self {
            description: EmbeddingDescription {
                provider: settings.provider.as_str().to_owned(),
                model: settings.model.clone(),
                endpoint_host: settings.endpoint_host(),
                state,
                missing,
                message,
                // Never the configured profile, even when the settings could name one:
                // a profile is a promise that vectors of that space exist.
                profile: None,
            },
        }
    }
}

#[async_trait]
impl EmbeddingProvider for UnconfiguredEmbeddings {
    fn describe(&self) -> EmbeddingDescription {
        self.description.clone()
    }

    async fn embed(&self, _request: &EmbedRequest) -> EmbedResult<EmbedResponse> {
        Err(EmbedError::NotConfigured(self.description.message.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn settings(pairs: &[(&str, &str)]) -> EmbeddingSettings {
        let source: BTreeMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        EmbeddingSettings::load(&source).unwrap()
    }

    fn request() -> EmbedRequest {
        EmbedRequest {
            purpose: "test",
            inputs: vec!["длина проёма 2100 мм".to_owned()],
        }
    }

    #[tokio::test]
    async fn without_a_key_every_batch_is_refused_and_says_what_is_missing() {
        let provider = UnconfiguredEmbeddings::new(&settings(&[]));
        let description = provider.describe();
        assert_eq!(description.state, "needs_configuration");
        assert!(description.missing.contains(&"OTDEL_EMBEDDING_API_KEY"));
        assert!(description.missing.contains(&"OTDEL_EMBEDDING_MODEL"));
        assert!(description.message.contains("OTDEL_EMBEDDING_API_KEY"));
        assert!(!description.is_ready());

        let error = provider.embed(&request()).await.unwrap_err();
        assert!(matches!(error, EmbedError::NotConfigured(_)));
        assert!(!error.is_retryable(), "a missing key must not be retried");
    }

    #[tokio::test]
    async fn the_unconfigured_adapter_carries_nothing_that_could_reach_a_network() {
        let provider = UnconfiguredEmbeddings::new(&settings(&[]));
        // The whole struct is one description; there is no client to call, and no
        // response to fabricate one from.
        assert_eq!(
            std::mem::size_of::<UnconfiguredEmbeddings>(),
            std::mem::size_of::<EmbeddingDescription>(),
            "the unconfigured adapter must hold nothing but its description"
        );
        assert!(provider.embed(&request()).await.is_err());
    }

    #[tokio::test]
    async fn no_vector_is_produced_rather_than_a_substitute_one() {
        let provider = UnconfiguredEmbeddings::new(&settings(&[]));
        let outcome = provider.embed(&request()).await;
        assert!(
            outcome.is_err(),
            "an unconfigured adapter must not return vectors of any kind"
        );
    }

    #[tokio::test]
    async fn there_is_no_profile_so_nothing_stored_can_claim_a_vector_space() {
        for pairs in [
            &[][..],
            &[("OTDEL_EMBEDDING_PROVIDER", "disabled")][..],
            // Even a complete configuration: this adapter did not build the vectors.
            &[
                ("OTDEL_EMBEDDING_API_KEY", "sk-or-v1-testkeyvalue0123"),
                ("OTDEL_EMBEDDING_MODEL", "openai/text-embedding-3-small"),
            ][..],
        ] {
            let provider = UnconfiguredEmbeddings::new(&settings(pairs));
            assert_eq!(provider.describe().profile, None);
            assert!(!provider.describe().is_ready());
        }
    }

    #[tokio::test]
    async fn disabled_is_described_as_a_deliberate_choice() {
        let provider =
            UnconfiguredEmbeddings::new(&settings(&[("OTDEL_EMBEDDING_PROVIDER", "disabled")]));
        let description = provider.describe();
        assert_eq!(description.state, "disabled");
        assert!(description.missing.is_empty());
        assert!(description.message.contains("ключевым словам"));
        assert!(provider.embed(&request()).await.is_err());
    }
}
