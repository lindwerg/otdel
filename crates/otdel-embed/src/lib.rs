//! The embedding adapter of OTDEL (`docs/block-01-spec.md` §9: semantic search over
//! published knowledge, optional).
//!
//! One entry point, [`build_provider`], decides from the configuration whether there is a
//! service to call at all:
//!
//! ```text
//! EmbeddingSettings ──ready──────→ HttpEmbeddingProvider   (HTTP client exists)
//!                   └─anything else→ UnconfiguredEmbeddings (no client, no socket)
//! ```
//!
//! **Without configuration there is no vector at all — never a hashed, random or zero
//! pseudo-vector.** That sentence is the whole crate. It is stated here because the
//! opposite is the natural thing to write: an embedding function has a total signature,
//! `text → Vec<f32>`, and there is always *something* to return. Hash the string into
//! 1536 floats and every layer above keeps working — chunks get vectors, the distance
//! query returns rows, the interface shows neighbours, nothing logs an error. The result
//! is a search that looks semantic and ranks by noise, and no one downstream can tell,
//! because a wrong vector is indistinguishable from a right one.
//!
//! So the unconfigured adapter has no HTTP client inside it, there is no code path from
//! the worker to the network, and every call comes back as [`EmbedError::NotConfigured`].
//! The caller records the search as `keyword`-only with the stated reason rather than
//! storing something vector-shaped, and [`EmbeddingDescription::profile`] stays `None` so
//! no row can claim membership of a space that was never built.
//!
//! Two further properties are enforced below rather than assumed:
//!
//! * **A batch returns whole or not at all.** `n` inputs produce `n` vectors in input
//!   order, or [`EmbedError::CountMismatch`] / [`EmbedError::DimensionMismatch`]. Zipping
//!   a short answer onto the inputs would attach vectors to the wrong claims, and nothing
//!   afterwards could detect it.
//! * **Profiles never mix.** The profile identifier comes from
//!   [`otdel_core::retrieval_config::EmbeddingSettings::profile`] and names the model, so
//!   changing the model degrades honestly to keyword search instead of computing
//!   distances between two unrelated spaces.
//!
//! Test builds enable the `fake` feature and use [`fake::FakeEmbeddings`] instead, so the
//! whole pipeline can be exercised without a key and without a network.

pub mod body;
mod openai;
pub mod provider;
pub mod unconfigured;

#[cfg(feature = "fake")]
pub mod fake;

use std::sync::Arc;

use otdel_core::retrieval_config::EmbeddingSettings;
use tracing::{info, warn};

pub use openai::HttpEmbeddingProvider;
pub use provider::{
    EmbedError, EmbedRequest, EmbedResponse, EmbedResult, EmbeddingDescription, EmbeddingProvider,
};
pub use unconfigured::UnconfiguredEmbeddings;

/// Build the adapter described by the configuration.
///
/// Never fails: an unusable configuration yields the unconfigured adapter, because the
/// server and the worker must still start, still publish, still search by keyword, and
/// still be able to *say* what is missing.
pub fn build_provider(settings: &EmbeddingSettings) -> Arc<dyn EmbeddingProvider> {
    if !settings.availability().is_ready() {
        let provider = UnconfiguredEmbeddings::new(settings);
        let description = provider.describe();
        info!(
            state = description.state,
            missing = ?description.missing,
            "embedding adapter is not configured; no request will be made and no vector will be produced"
        );
        return Arc::new(provider);
    }

    match HttpEmbeddingProvider::new(settings) {
        Ok(provider) => {
            info!(
                provider = settings.provider.as_str(),
                model = %settings.model,
                endpoint_host = settings.endpoint_host().unwrap_or_default(),
                profile = settings.profile().unwrap_or_default(),
                "embedding adapter ready"
            );
            Arc::new(provider)
        }
        // Only reachable when the HTTP client itself cannot be built (no TLS backend, for
        // instance). Falling back keeps the process alive and honest rather than crashing
        // at startup over an optional capability — and keyword search still works.
        Err(error) => {
            warn!(error = %error, "could not build the embedding client; векторный поиск остаётся недоступным");
            Arc::new(UnconfiguredEmbeddings::new(settings))
        }
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
            purpose: "claim_chunks",
            inputs: vec!["длина проёма 2100 мм".to_owned()],
        }
    }

    #[tokio::test]
    async fn no_key_yields_an_adapter_that_cannot_reach_the_network() {
        let provider = build_provider(&settings(&[(
            "OTDEL_EMBEDDING_MODEL",
            "openai/text-embedding-3-small",
        )]));
        let description = provider.describe();
        assert_eq!(description.state, "needs_configuration");
        assert_eq!(description.missing, vec!["OTDEL_EMBEDDING_API_KEY"]);

        let error = provider.embed(&request()).await.unwrap_err();
        assert!(matches!(error, EmbedError::NotConfigured(_)));
    }

    #[tokio::test]
    async fn without_configuration_there_is_no_vector_at_all() {
        let provider = build_provider(&settings(&[]));
        assert_eq!(
            provider.describe().profile,
            None,
            "an unconfigured adapter must not name a vector space"
        );
        let outcome = provider.embed(&request()).await;
        assert!(
            outcome.is_err(),
            "the adapter must refuse rather than return a substitute vector"
        );
    }

    #[test]
    fn a_complete_configuration_yields_a_ready_adapter_naming_its_profile() {
        let provider = build_provider(&settings(&[
            ("OTDEL_EMBEDDING_API_KEY", "sk-or-v1-0123456789abcdef"),
            ("OTDEL_EMBEDDING_MODEL", "openai/text-embedding-3-small"),
        ]));
        let description = provider.describe();
        assert!(description.is_ready());
        assert_eq!(description.endpoint_host.as_deref(), Some("openrouter.ai"));
        assert_eq!(
            description.profile.as_deref(),
            Some("openai_compatible:openai/text-embedding-3-small")
        );
        // Even a ready adapter never exposes the key, through its description or its
        // `Debug` — the latter goes through `dyn EmbeddingProvider`.
        assert!(!format!("{description:?}").contains("sk-or-v1"));
        assert!(!format!("{:?}", provider.as_ref()).contains("sk-or-v1"));
    }

    #[tokio::test]
    async fn disabled_stays_disabled_even_with_a_key_present() {
        let provider = build_provider(&settings(&[
            ("OTDEL_EMBEDDING_PROVIDER", "disabled"),
            ("OTDEL_EMBEDDING_API_KEY", "sk-or-v1-0123456789abcdef"),
            ("OTDEL_EMBEDDING_MODEL", "openai/text-embedding-3-small"),
        ]));
        assert_eq!(provider.describe().state, "disabled");
        assert_eq!(provider.describe().profile, None);
        assert!(provider.embed(&request()).await.is_err());
    }
}
