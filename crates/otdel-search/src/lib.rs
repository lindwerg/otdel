//! The outward-facing adapters of OTDEL phase 1D (`docs/block-01-spec.md` §3: "Адаптер
//! поиска и отдельный загрузчик источников").
//!
//! Two entry points decide, from the configuration alone, whether anything can leave
//! this machine at all:
//!
//! ```text
//! ResearchSettings ──ready──────→ HttpSearchProvider / HttpsFetcher   (HTTP clients exist)
//!                  └─anything else→ UnconfiguredSearch / UnconfiguredFetcher
//!                                                                     (no client, no socket)
//! ```
//!
//! That branch is the safety property of this phase, exactly as it was for the model
//! adapter in 1C. With no search endpoint, no key or no host allowlist, the adapters that
//! come back have no HTTP client inside them: there is no code path from the worker to
//! the network, every call returns "not configured", and the plan is recorded as
//! `needs_provider` with **no money reserved and nothing stored**.
//!
//! Readiness deliberately includes the *allowlist*. A researcher able to search but not
//! allowed to read anything would spend money to produce a list of links nobody can open,
//! and the acceptance criterion for this phase is not "it searched" but "the conclusion
//! points at an open source".
//!
//! Test builds enable the `fake` feature and use [`fake`] instead, so the whole pipeline
//! runs without a key and without a network.

pub mod fetch;
pub mod guard;
pub mod html;
pub mod http;
pub mod openrouter;
pub mod provider;
pub mod robots;
pub mod url;

#[cfg(feature = "fake")]
pub mod fake;

use std::sync::Arc;

use otdel_core::research_config::{ResearchSettings, SearchProviderKind};
use tracing::{info, warn};

pub use fetch::{HttpsFetcher, UnconfiguredFetcher};
pub use guard::{GuardedResolver, HostPolicy};
pub use html::ExtractedPage;
pub use http::{HttpSearchProvider, UnconfiguredSearch};
pub use openrouter::{ChatTransport, OpenRouterSearch};
pub use provider::{
    AdapterDescription, DocumentFetcher, FetchRefusal, FetchedDocument, SearchAnswer,
    SearchBilling, SearchError, SearchHit, SearchProvider, SearchRequest,
};
pub use url::{NormalisedUrl, UrlRejection};

/// How this crawler identifies itself to every site it reads.
///
/// A real name and a real contact hint, because a site operator who wants to block this
/// reader should be able to, and because a crawler that hides is not one a serious
/// publisher would tolerate.
pub const USER_AGENT: &str = concat!(
    "otdel-research/",
    env!("CARGO_PKG_VERSION"),
    " (+bounded industry research; respects robots.txt)"
);

/// The token `robots.txt` groups are matched against.
pub const ROBOTS_TOKEN: &str = "otdel-research";

/// Build the search adapter described by the configuration.
///
/// Never fails: an unusable configuration yields the unconfigured adapter, because the
/// server and the worker must still start and must still be able to *say* what is
/// missing.
pub fn build_search_provider(settings: &ResearchSettings) -> Arc<dyn SearchProvider> {
    if !settings.availability().is_ready() {
        let provider = UnconfiguredSearch::new(settings);
        let description = provider.describe();
        info!(
            state = description.state,
            missing = ?description.missing,
            "search adapter is not configured; no external request will be made"
        );
        return Arc::new(provider);
    }

    let built: Result<Arc<dyn SearchProvider>, SearchError> = match settings.provider {
        SearchProviderKind::OpenRouterWebSearch => {
            OpenRouterSearch::new(settings).map(|provider| Arc::new(provider) as Arc<_>)
        }
        // `Disabled` cannot reach here: it is never `ready`.
        SearchProviderKind::HttpJson | SearchProviderKind::Disabled => {
            HttpSearchProvider::new(settings).map(|provider| Arc::new(provider) as Arc<_>)
        }
    };

    match built {
        Ok(provider) => {
            info!(
                provider = provider.describe().provider,
                endpoint_host = settings.search_host().unwrap_or_default(),
                allowed_hosts = settings.allowed_hosts.len(),
                "search adapter ready"
            );
            provider
        }
        // Only reachable when the HTTP client itself cannot be built (no TLS backend,
        // for instance). Falling back keeps the process alive and honest rather than
        // crashing at startup over an optional capability.
        Err(error) => {
            warn!(error = %error, "could not build the search client; исследователь остаётся ненастроенным");
            Arc::new(UnconfiguredSearch::new(settings))
        }
    }
}

/// Build the document fetcher described by the configuration.
pub fn build_fetcher(settings: &ResearchSettings) -> Arc<dyn DocumentFetcher> {
    if !settings.availability().is_ready() {
        return Arc::new(UnconfiguredFetcher::new(settings));
    }

    match HttpsFetcher::new(settings) {
        Ok(fetcher) => Arc::new(fetcher),
        Err(error) => {
            warn!(error = %error, "could not build the document fetcher; источники не читаются");
            Arc::new(UnconfiguredFetcher::new(settings))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn settings(pairs: &[(&str, &str)]) -> ResearchSettings {
        let source: BTreeMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        ResearchSettings::load(&source).unwrap()
    }

    fn ready() -> ResearchSettings {
        settings(&[
            (
                "OTDEL_RESEARCH_SEARCH_URL",
                "https://search.example.com/v1/search",
            ),
            ("OTDEL_RESEARCH_API_KEY", "srch-0123456789abcdef"),
            ("OTDEL_RESEARCH_ALLOWED_HOSTS", "docs.example.org"),
        ])
    }

    #[tokio::test]
    async fn an_unconfigured_installation_gets_adapters_that_cannot_reach_the_network() {
        let empty = settings(&[]);
        let search = build_search_provider(&empty);
        let fetcher = build_fetcher(&empty);

        assert_eq!(search.describe().state, "needs_configuration");
        assert_eq!(fetcher.describe().state, "needs_configuration");

        let error = search
            .search(&SearchRequest {
                query: "что угодно".to_owned(),
                max_results: 5,
            })
            .await
            .unwrap_err();
        assert!(matches!(error, SearchError::NotConfigured(_)));
        assert!(
            !error.was_sent(),
            "an unconfigured adapter must not be able to spend anything"
        );

        let refusal = fetcher
            .fetch(&NormalisedUrl::parse("https://docs.example.org/a").unwrap())
            .await
            .unwrap_err();
        assert!(matches!(refusal, FetchRefusal::NotConfigured(_)));
        assert!(!refusal.was_sent());
    }

    #[test]
    fn a_complete_configuration_yields_ready_adapters_that_expose_no_key() {
        let search = build_search_provider(&ready());
        let fetcher = build_fetcher(&ready());

        assert!(search.describe().is_ready());
        assert!(fetcher.describe().is_ready());
        assert_eq!(
            search.describe().endpoint_host.as_deref(),
            Some("search.example.com")
        );
        assert_eq!(
            fetcher.describe().allowed_hosts,
            vec!["docs.example.org".to_owned()]
        );

        for rendered in [
            format!("{:?}", search.describe()),
            format!("{:?}", fetcher.describe()),
        ] {
            assert!(!rendered.contains("srch-"), "{rendered}");
        }
    }

    #[test]
    fn a_search_key_without_an_allowlist_yields_an_unconfigured_pair() {
        // The half-configuration that would otherwise search and then be unable to open
        // anything it found.
        let partial = settings(&[
            (
                "OTDEL_RESEARCH_SEARCH_URL",
                "https://search.example.com/v1/search",
            ),
            ("OTDEL_RESEARCH_API_KEY", "srch-0123456789abcdef"),
        ]);
        assert_eq!(
            build_search_provider(&partial).describe().state,
            "needs_configuration"
        );
        assert_eq!(
            build_fetcher(&partial).describe().missing,
            vec!["OTDEL_RESEARCH_ALLOWED_HOSTS"]
        );
    }

    #[test]
    fn a_disabled_researcher_stays_disabled_even_with_a_key_present() {
        let disabled = settings(&[
            ("OTDEL_RESEARCH_PROVIDER", "disabled"),
            (
                "OTDEL_RESEARCH_SEARCH_URL",
                "https://search.example.com/v1/search",
            ),
            ("OTDEL_RESEARCH_API_KEY", "srch-0123456789abcdef"),
            ("OTDEL_RESEARCH_ALLOWED_HOSTS", "docs.example.org"),
        ]);
        assert_eq!(
            build_search_provider(&disabled).describe().state,
            "disabled"
        );
        assert_eq!(build_fetcher(&disabled).describe().state, "disabled");
    }

    #[test]
    fn the_user_agent_names_the_reader_and_promises_robots() {
        assert!(USER_AGENT.starts_with("otdel-research/"));
        assert!(USER_AGENT.contains("robots.txt"));
        assert!(ROBOTS_TOKEN.starts_with("otdel"));
    }
}
