//! One real call to OpenRouter's Perplexity search, run by hand and never by CI.
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
//! **Bounds this run holds itself to.** One request. `engine=perplexity`, asserted on the
//! wire rather than assumed. `max_uses=1`, so the request may run the search tool exactly
//! once — a result count alone would not stop a model from searching repeatedly, and each
//! search is a separate charge. Three results. `max_characters` bounded. At OpenRouter's
//! published Perplexity price of $0.005 per search plus the tokens of a small model, one
//! run is worth well under a cent, and the test prints the reported figure rather than
//! asserting the forecast was right.
//!
//! What it deliberately does **not** do: it does not fetch any page it finds, does not
//! store anything, does not touch the database, and does not publish. It asks a neutral
//! industry question about a published standard — no partner is named, because no partner
//! is involved — and prints the public URLs and a short excerpt so a human can see that
//! the links are real. The key is never printed, never logged and never put in a URL.

use std::collections::BTreeMap;
use std::sync::Arc;

use otdel_core::research_config::{ResearchSettings, SearchEngine};
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
        "OTDEL_RESEARCH_OPENROUTER_MAX_USES",
        "OTDEL_RESEARCH_OPENROUTER_MAX_CHARACTERS",
        "OTDEL_RESEARCH_OPENROUTER_DOMAIN_FILTER",
    ] {
        if let Ok(value) = std::env::var(name) {
            source.insert(name.to_owned(), value);
        }
    }
    source.insert(
        "OTDEL_RESEARCH_PROVIDER".to_owned(),
        "openrouter".to_owned(),
    );
    // The engine this installation chose. Set here rather than left to the environment so
    // that a forgotten variable cannot turn a Perplexity acceptance run into an Exa one.
    source.insert(
        "OTDEL_RESEARCH_OPENROUTER_ENGINE".to_owned(),
        "perplexity".to_owned(),
    );
    // Three results, which is the smallest number that still shows a list.
    source
        .entry("OTDEL_RESEARCH_OPENROUTER_MAX_RESULTS".to_owned())
        .or_insert_with(|| "3".to_owned());
    // One search per request, on the provider's side.
    source
        .entry("OTDEL_RESEARCH_OPENROUTER_MAX_USES".to_owned())
        .or_insert_with(|| "1".to_owned());
    source
        .entry("OTDEL_RESEARCH_OPENROUTER_MAX_TOTAL_RESULTS".to_owned())
        .or_insert_with(|| "3".to_owned());

    ResearchSettings::load(&source).expect("the smoke configuration must be valid")
}

#[tokio::test]
#[ignore = "makes one real, paid request to OpenRouter; run by hand with a key present"]
async fn one_real_bounded_perplexity_search_against_openrouter() {
    let settings = settings_from_environment();
    let availability = settings.availability();
    assert!(
        availability.is_ready(),
        "set OTDEL_LLM_API_KEY, OTDEL_LLM_MODEL and OTDEL_RESEARCH_ALLOWED_HOSTS first; \
         missing: {availability:?}"
    );

    let engine = settings.openrouter.effective_engine();
    assert_eq!(
        engine,
        SearchEngine::Perplexity,
        "this acceptance run is about Perplexity; any other engine is a different bill"
    );
    assert_eq!(settings.openrouter.max_uses_per_request, 1);

    println!(
        "engine={} (configured {}), model={}, max_results={}, max_uses={}, max_characters={}, \
         forecast={} micros ({})",
        engine.as_str(),
        settings.openrouter.engine.as_str(),
        settings.openrouter.model,
        settings.openrouter.max_results,
        settings.openrouter.max_uses_per_request,
        settings.openrouter.max_characters_per_result,
        settings.openrouter.forecast_micros(),
        settings.costs.currency,
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

    // Requested and observed are printed as two separate facts. A provider that does not
    // name the engine it used leaves `observed_engine` empty, and that is reported as
    // "не сообщён" rather than quietly echoing what we asked for.
    println!(
        "request_id={:?} requested_engine={:?} observed_engine={:?} hits={} duration_ms={} \
         reported_micros={:?} prompt_tokens={:?} completion_tokens={:?} web_search_requests={:?}",
        answer.billing.request_id,
        answer.billing.engine,
        answer.billing.observed_engine,
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

    // The call ceiling is the point of `max_uses`: if the provider reports having searched
    // more than once, the bound did not hold and the forecast was wrong by that factor.
    if let Some(searches) = answer.billing.search_requests {
        assert!(
            searches <= settings.openrouter.max_uses_per_request,
            "max_uses={} was sent, but the provider reports {searches} searches",
            settings.openrouter.max_uses_per_request,
        );
    }

    // The point of the accounting half: a real call reports a real price.
    assert!(
        answer.billing.reported_micros.is_some(),
        "OpenRouter reports `usage.cost`; if this ever stops being true the ledger \
         silently falls back to the declared tariff and the owner should know"
    );
    assert_eq!(
        answer.billing.engine.as_deref(),
        Some("perplexity"),
        "the adapter must report the engine it asked for"
    );
    assert!(
        !answer.billing.exa_fallback,
        "an explicitly chosen engine is never an Exa fallback"
    );
}
