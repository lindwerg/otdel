//! One real call to OpenRouter, run by hand and never by CI.
//!
//! Everything else in this repository is tested without a network. This file exists
//! because one question cannot be answered that way: *does the request we build actually
//! work against the live service?* A wire format copied from documentation can be wrong in
//! a way no fake will ever reveal, and "работает с настоящим поиском" is a claim that
//! needs one real call behind it.
//!
//! It is `#[ignore]`d, so `cargo test` — locally and in CI — does not run it. To run it:
//!
//! ```bash
//! set -a; . ./.local/openrouter.env; set +a
//! export OTDEL_RESEARCH_ALLOWED_HOSTS=docs.cntd.ru
//! cargo test -p otdel-search --test openrouter_smoke -- --ignored --nocapture
//! ```
//!
//! What it deliberately does **not** do: it does not fetch any page it finds, does not
//! store anything, does not touch the database, and does not publish. It asks for three
//! results on a neutral industry question — no partner is named, because no partner is
//! involved — and prints the public URLs and a short excerpt so a human can see that the
//! links are real. The key is never printed, never logged and never put in a URL.

use std::collections::BTreeMap;
use std::sync::Arc;

use otdel_core::research_config::ResearchSettings;
use otdel_search::{SearchProvider, SearchRequest};

/// A question about a published standard. It names no company and no product.
const SMOKE_QUERY: &str = "минимальная толщина горячего цинкового покрытия ГОСТ 9.307";

fn settings_from_environment() -> ResearchSettings {
    let mut source: BTreeMap<String, String> = BTreeMap::new();
    for name in [
        "OTDEL_LLM_API_KEY",
        "OTDEL_LLM_MODEL",
        "OTDEL_LLM_BASE_URL",
        "OTDEL_RESEARCH_ALLOWED_HOSTS",
        "OTDEL_RESEARCH_OPENROUTER_ENGINE",
        "OTDEL_RESEARCH_OPENROUTER_MAX_RESULTS",
    ] {
        if let Ok(value) = std::env::var(name) {
            source.insert(name.to_owned(), value);
        }
    }
    source.insert(
        "OTDEL_RESEARCH_PROVIDER".to_owned(),
        "openrouter".to_owned(),
    );
    // Three results, which is the smallest number that still shows a list.
    source
        .entry("OTDEL_RESEARCH_OPENROUTER_MAX_RESULTS".to_owned())
        .or_insert_with(|| "3".to_owned());

    ResearchSettings::load(&source).expect("the smoke configuration must be valid")
}

#[tokio::test]
#[ignore = "makes one real, paid request to OpenRouter; run by hand with a key present"]
async fn one_real_bounded_search_against_openrouter() {
    let settings = settings_from_environment();
    let availability = settings.availability();
    assert!(
        availability.is_ready(),
        "set OTDEL_LLM_API_KEY, OTDEL_LLM_MODEL and OTDEL_RESEARCH_ALLOWED_HOSTS first; \
         missing: {availability:?}"
    );

    let engine = settings.openrouter.effective_engine();
    println!(
        "engine={} (configured {}, exa fallback: {}), model={}, max_results={}, forecast={} micros",
        engine.as_str(),
        settings.openrouter.engine.as_str(),
        settings.openrouter.is_exa_fallback(),
        settings.openrouter.model,
        settings.openrouter.max_results,
        settings.openrouter.forecast_micros(),
    );

    let provider: Arc<dyn SearchProvider> = otdel_search::build_search_provider(&settings);
    assert_eq!(
        provider.describe().provider,
        "openrouter_web_search",
        "the OpenRouter adapter must be the one that was built"
    );

    let answer = provider
        .search(&SearchRequest {
            query: SMOKE_QUERY.to_owned(),
            max_results: settings.openrouter.max_results,
        })
        .await
        .expect("the live search must succeed");

    println!(
        "hits={} duration_ms={} reported_micros={:?} prompt_tokens={:?} completion_tokens={:?} \
         web_search_requests={:?}",
        answer.hits.len(),
        answer.duration.as_millis(),
        answer.billing.reported_micros,
        answer.billing.prompt_tokens,
        answer.billing.completion_tokens,
        answer.billing.search_requests,
    );
    for hit in &answer.hits {
        let snippet: String = hit
            .snippet
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(120)
            .collect();
        println!("  {} — {:?}\n      {snippet}", hit.url, hit.title);
    }

    assert!(
        !answer.hits.is_empty(),
        "a live search must return at least one link"
    );
    assert!(
        answer.hits.len() <= settings.openrouter.max_results as usize,
        "the provider must not be able to exceed the configured result count"
    );
    for hit in &answer.hits {
        assert!(
            hit.url.starts_with("https://"),
            "every lead must be a plain https URL: {}",
            hit.url
        );
    }
    // The point of the accounting half: a real call reports a real price.
    assert!(
        answer.billing.reported_micros.is_some(),
        "OpenRouter reports `usage.cost`; if this ever stops being true the ledger \
         silently falls back to the declared tariff and the owner should know"
    );
}
