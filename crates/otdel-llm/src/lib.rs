//! The single model adapter of OTDEL (`docs/block-01-spec.md` §3: "Единый адаптер
//! вызовов моделей").
//!
//! One entry point, [`build_provider`], decides from the configuration whether there is
//! a model to call at all:
//!
//! ```text
//! LlmSettings ──ready──────→ OpenAiCompatibleProvider   (HTTP client exists)
//!             └─anything else→ UnconfiguredProvider     (no client, no socket)
//! ```
//!
//! That branch is the whole safety property of this phase. With no key the returned
//! adapter has no HTTP client inside it, so there is no code path from the worker to
//! the network, and every call comes back as [`LlmError::NotConfigured`] — which the
//! caller records as a *configuration state* of the run, storing nothing.
//!
//! Test builds enable the `fake` feature and use [`fake::FakeProvider`] instead, so the
//! whole pipeline can be exercised without a key and without a network.

pub mod openai;
pub mod provider;
pub mod unconfigured;

#[cfg(feature = "fake")]
pub mod fake;

use std::sync::Arc;

use otdel_core::llm_config::LlmSettings;
use tracing::{info, warn};

pub use openai::OpenAiCompatibleProvider;
pub use provider::{
    LlmError, LlmProvider, LlmRequest, LlmResponse, LlmResult, ProviderDescription, Usage,
};
pub use unconfigured::UnconfiguredProvider;

/// Build the adapter described by the configuration.
///
/// Never fails: an unusable configuration yields the unconfigured adapter, because the
/// server and the worker must still start and still be able to *say* what is missing.
pub fn build_provider(settings: &LlmSettings) -> Arc<dyn LlmProvider> {
    if !settings.availability().is_ready() {
        let provider = UnconfiguredProvider::new(settings);
        let description = provider.describe();
        info!(
            state = description.state,
            missing = ?description.missing,
            "model adapter is not configured; no request will be made"
        );
        return Arc::new(provider);
    }

    match OpenAiCompatibleProvider::new(settings) {
        Ok(provider) => {
            info!(
                provider = settings.provider.as_str(),
                model = %settings.model,
                endpoint_host = settings.endpoint_host().unwrap_or_default(),
                "model adapter ready"
            );
            Arc::new(provider)
        }
        // Only reachable when the HTTP client itself cannot be built (no TLS backend,
        // for instance). Falling back keeps the process alive and honest rather than
        // crashing at startup over an optional capability.
        Err(error) => {
            warn!(error = %error, "could not build the model client; продуктолог остаётся ненастроенным");
            Arc::new(UnconfiguredProvider::new(settings))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn settings(pairs: &[(&str, &str)]) -> LlmSettings {
        let source: BTreeMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        LlmSettings::load(&source).unwrap()
    }

    #[test]
    fn no_key_yields_an_adapter_that_cannot_reach_the_network() {
        let provider = build_provider(&settings(&[("OTDEL_LLM_MODEL", "openai/gpt-4o-mini")]));
        let description = provider.describe();
        assert_eq!(description.state, "needs_configuration");
        assert_eq!(description.missing, vec!["OTDEL_LLM_API_KEY"]);
    }

    #[test]
    fn a_complete_configuration_yields_a_ready_adapter() {
        let provider = build_provider(&settings(&[
            ("OTDEL_LLM_API_KEY", "sk-or-v1-0123456789abcdef"),
            ("OTDEL_LLM_MODEL", "openai/gpt-4o-mini"),
        ]));
        let description = provider.describe();
        assert!(description.is_ready());
        assert_eq!(description.endpoint_host.as_deref(), Some("openrouter.ai"));
        // Even a ready adapter never exposes the key through its description.
        assert!(!format!("{description:?}").contains("sk-or-v1"));
    }

    #[test]
    fn disabled_stays_disabled_even_with_a_key_present() {
        let provider = build_provider(&settings(&[
            ("OTDEL_LLM_PROVIDER", "disabled"),
            ("OTDEL_LLM_API_KEY", "sk-or-v1-0123456789abcdef"),
            ("OTDEL_LLM_MODEL", "openai/gpt-4o-mini"),
        ]));
        assert_eq!(provider.describe().state, "disabled");
    }
}
