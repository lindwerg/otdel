//! The configured search adapter.
//!
//! There is no vendor in this file. The owner points `OTDEL_RESEARCH_SEARCH_URL` at an
//! endpoint that accepts
//!
//! ```json
//! { "query": "минимальная толщина цинкового покрытия ГОСТ", "max_results": 8 }
//! ```
//!
//! and answers
//!
//! ```json
//! { "results": [ { "url": "https://…", "title": "…", "snippet": "…" } ] }
//! ```
//!
//! That endpoint can be a vendor API, a self-hosted SearxNG, or a few lines of proxy in
//! front of either. Choosing a search provider is a decision `docs/block-01-spec.md` §3
//! leaves open, and this phase implements the *shape* rather than pretending the decision
//! was made.
//!
//! Everything that could go wrong quietly is explicit:
//!
//! * **the key travels only in a header**, never in the URL, and `Debug` prints
//!   `<redacted>`;
//! * **nothing is unbounded**: request timeout, response size, result count and a
//!   minimum interval between calls are all configuration-validated ceilings;
//! * **redirects are not followed**, so the key cannot be sent to a host the owner never
//!   configured;
//! * **the endpoint's own host is resolved through the guarded resolver**, so even a
//!   misconfigured endpoint cannot be used to reach an internal address;
//! * **a timeout after the request was sent is not "no cost"** — it comes back as
//!   [`SearchError::UnknownOutcome`], which the ledger records as spent and flags for
//!   reconciliation.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use otdel_core::llm_config::ApiKey;
use otdel_core::research_config::ResearchSettings;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::guard::{GuardedResolver, HostPolicy};
use crate::provider::{
    AdapterDescription, SearchAnswer, SearchError, SearchHit, SearchProvider, SearchRequest,
};

/// A search answer larger than this is not parsed. Results are a list of links, not a
/// document.
const MAX_SEARCH_RESPONSE_BYTES: u64 = 1024 * 1024;
/// Upper bound on results taken from one answer, whatever the provider returns.
const MAX_HITS: usize = 50;
/// Longest snippet kept. It is a lead, not a source, and a long one is not more of one.
const MAX_SNIPPET_CHARS: usize = 600;
const MAX_TITLE_CHARS: usize = 300;

/// HTTP client against one configured search endpoint.
pub struct HttpSearchProvider {
    client: reqwest::Client,
    endpoint: String,
    api_key: ApiKey,
    api_key_header: Option<String>,
    description: AdapterDescription,
    min_request_interval: Duration,
    /// Time of the last request, so pacing holds across concurrent plans.
    last_request: Mutex<Option<Instant>>,
}

impl std::fmt::Debug for HttpSearchProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpSearchProvider")
            .field("endpoint", &self.endpoint)
            .field("api_key", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl HttpSearchProvider {
    /// Build the client. Fails when the configuration is incomplete — the caller then
    /// uses the unconfigured adapter, and no client exists at all.
    pub fn new(settings: &ResearchSettings) -> Result<Self, SearchError> {
        let availability = settings.availability();
        if !availability.is_ready() {
            return Err(SearchError::NotConfigured(format!(
                "состояние адаптера: {}",
                availability.as_str()
            )));
        }
        let api_key = settings
            .api_key
            .clone()
            .ok_or_else(|| SearchError::NotConfigured("не задан ключ".to_owned()))?;
        let host = settings
            .search_host()
            .ok_or_else(|| SearchError::NotConfigured("не задан адрес поиска".to_owned()))?;
        // The authority may carry a port, and an IPv6 literal carries colons of its own;
        // the resolver matches on the name alone.
        let host_name = otdel_core::research_config::authority_host(&host).to_owned();
        let loopback = host_name == "localhost"
            || host_name
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback());

        let client = reqwest::Client::builder()
            .timeout(settings.limits.request_timeout)
            .connect_timeout(Duration::from_secs(10))
            // A redirect would send the key somewhere the owner never configured.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(crate::USER_AGENT)
            // Even the configured endpoint resolves through the guard: a search URL
            // pointed at an internal name must not become a way to reach it.
            .dns_resolver(GuardedResolver::shared(
                HostPolicy::Exact(vec![host_name]),
                loopback,
            ))
            .build()
            .map_err(|error| {
                warn!(error = %error, "could not build the search HTTP client");
                SearchError::Transport
            })?;

        Ok(Self {
            client,
            endpoint: settings.search_url.clone(),
            api_key,
            api_key_header: settings.api_key_header.clone(),
            description: AdapterDescription {
                provider: settings.provider.as_str().to_owned(),
                endpoint_host: Some(host.clone()),
                state: "ready",
                missing: Vec::new(),
                message: format!("Исследователь ищет источники через {host}."),
                allowed_hosts: settings.allowed_hosts.entries().to_vec(),
            },
            min_request_interval: settings.limits.min_request_interval,
            last_request: Mutex::new(None),
        })
    }

    /// Hold the courtesy rate limit. Held across the whole request so two concurrent
    /// plans cannot both slip past it.
    async fn pace(&self) {
        if self.min_request_interval.is_zero() {
            return;
        }
        let mut last = self.last_request.lock().await;
        if let Some(previous) = *last {
            let elapsed = previous.elapsed();
            if elapsed < self.min_request_interval {
                tokio::time::sleep(self.min_request_interval - elapsed).await;
            }
        }
        *last = Some(Instant::now());
    }

    fn body(&self, request: &SearchRequest) -> Value {
        json!({
            "query": request.query,
            "max_results": request.max_results,
        })
    }

    fn authorised(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key_header {
            Some(header) => builder.header(header.as_str(), self.api_key.expose()),
            None => builder.bearer_auth(self.api_key.expose()),
        }
    }
}

#[async_trait]
impl SearchProvider for HttpSearchProvider {
    fn describe(&self) -> AdapterDescription {
        self.description.clone()
    }

    async fn search(&self, request: &SearchRequest) -> Result<SearchAnswer, SearchError> {
        self.pace().await;
        let started = Instant::now();

        let response = self
            .authorised(self.client.post(&self.endpoint))
            .json(&self.body(request))
            .send()
            .await
            .map_err(classify_transport)?;

        let status = response.status();
        if status.is_redirection() {
            return Err(SearchError::InvalidResponse(
                "поисковый провайдер ответил перенаправлением; переадресация не выполняется"
                    .to_owned(),
            ));
        }
        if !status.is_success() {
            // The provider's body may quote the query; it is neither returned nor logged.
            return Err(match status.as_u16() {
                429 => SearchError::RateLimited,
                code => SearchError::Http {
                    status: code,
                    retryable: (500..600).contains(&code),
                },
            });
        }

        // A declared length over the limit is refused before a byte of body is read…
        if let Some(length) = response.content_length() {
            if length > MAX_SEARCH_RESPONSE_BYTES {
                return Err(SearchError::InvalidResponse(format!(
                    "ответ больше разрешённых {MAX_SEARCH_RESPONSE_BYTES} байт"
                )));
            }
        }

        // …and the body is then *streamed* under the same bound, because a chunked
        // response declares no length and a lying one declares the wrong length. Calling
        // `bytes()` here would buffer whatever arrives, limited only by the timeout.
        let bytes = crate::provider::read_bounded(response, MAX_SEARCH_RESPONSE_BYTES)
            .await
            .map_err(|error| match error {
                crate::provider::BodyReadError::TooLarge => SearchError::InvalidResponse(format!(
                    "ответ больше разрешённых {MAX_SEARCH_RESPONSE_BYTES} байт"
                )),
                // Bytes had already been sent when it stalled: the provider received the
                // request, so the outcome is unknown rather than free.
                crate::provider::BodyReadError::Timeout => SearchError::UnknownOutcome,
                crate::provider::BodyReadError::Transport => SearchError::Transport,
            })?;

        let envelope: Value = serde_json::from_slice(&bytes)
            .map_err(|_| SearchError::InvalidResponse("тело ответа не является JSON".to_owned()))?;

        let hits = parse_hits(&envelope, request.max_results as usize)?;
        let duration = started.elapsed();

        debug!(
            hits = hits.len(),
            duration_ms = duration.as_millis() as u64,
            response_bytes = bytes.len(),
            "search call finished"
        );

        Ok(SearchAnswer { hits, duration })
    }
}

/// Did the request leave the machine?
///
/// A connect failure means it did not; a timeout once connected means it very likely
/// did, and the money has to be accounted for either way
/// (`docs/block-01-spec.md` §10).
fn classify_transport(error: reqwest::Error) -> SearchError {
    // `error` can name the URL but never a header, so debug-level logging is safe.
    debug!(error = %error, "search request failed");
    if error.is_connect() {
        SearchError::Transport
    } else if error.is_timeout() {
        SearchError::UnknownOutcome
    } else {
        SearchError::Transport
    }
}

/// Pull the list of results out of the answer.
///
/// Three spellings of the same list are accepted (`results`, `items`, `web.results`),
/// because the difference between them is a vendor's habit rather than a decision the
/// owner should have to write a proxy for. Anything else is a refusal with a reason: a
/// provider answering something unexpected must not silently produce zero results, which
/// would read as "the internet knows nothing about this".
fn parse_hits(envelope: &Value, max_results: usize) -> Result<Vec<SearchHit>, SearchError> {
    if let Some(error) = envelope.get("error") {
        let kind = error
            .get("type")
            .or_else(|| error.get("code"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(SearchError::InvalidResponse(format!(
            "провайдер вернул ошибку в теле ответа ({kind})"
        )));
    }

    let list = envelope
        .get("results")
        .or_else(|| envelope.get("items"))
        .or_else(|| envelope.pointer("/web/results"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            SearchError::InvalidResponse(
                "в ответе нет массива `results` (допустимо также `items` или `web.results`)"
                    .to_owned(),
            )
        })?;

    let hits = list
        .iter()
        .filter_map(|item| {
            let url = item
                .get("url")
                .or_else(|| item.get("link"))
                .and_then(Value::as_str)?;
            Some(SearchHit {
                url: url.trim().to_owned(),
                title: text_field(item, &["title", "name"], MAX_TITLE_CHARS),
                snippet: text_field(
                    item,
                    &["snippet", "description", "summary"],
                    MAX_SNIPPET_CHARS,
                ),
            })
        })
        .take(max_results.clamp(1, MAX_HITS))
        .collect();

    Ok(hits)
}

/// The first present field, cleaned of control characters and bounded.
fn text_field(item: &Value, names: &[&str], max_chars: usize) -> Option<String> {
    names
        .iter()
        .find_map(|name| item.get(*name).and_then(Value::as_str))
        .map(|value| {
            value
                .chars()
                .map(|ch| if ch.is_control() { ' ' } else { ch })
                .take(max_chars)
                .collect::<String>()
                .trim()
                .to_owned()
        })
        .filter(|value| !value.is_empty())
}

/// The unconfigured search adapter: it has no HTTP client, so there is no code path from
/// it to a socket.
///
/// This is the state of every installation that has not been given a search endpoint,
/// which is all of them today. It answers every call with the list of variables to set.
pub struct UnconfiguredSearch {
    description: AdapterDescription,
    reason: String,
}

impl UnconfiguredSearch {
    pub fn new(settings: &ResearchSettings) -> Self {
        let availability = settings.availability();
        let missing = match &availability {
            otdel_core::research_config::SearchAvailability::NeedsConfiguration { missing } => {
                missing.clone()
            }
            _ => Vec::new(),
        };

        let message = if missing.is_empty() {
            "Отраслевое исследование выключено настройкой OTDEL_RESEARCH_PROVIDER=disabled."
                .to_owned()
        } else {
            format!(
                "Исследователь не настроен: задайте {}. Пока этого нет, ни один внешний \
                 запрос не выполняется и бюджет не расходуется.",
                missing.join(", ")
            )
        };

        Self {
            description: AdapterDescription {
                provider: settings.provider.as_str().to_owned(),
                endpoint_host: settings.search_host(),
                state: availability.as_str(),
                missing,
                allowed_hosts: settings.allowed_hosts.entries().to_vec(),
                message: message.clone(),
            },
            reason: message,
        }
    }
}

#[async_trait]
impl SearchProvider for UnconfiguredSearch {
    fn describe(&self) -> AdapterDescription {
        self.description.clone()
    }

    async fn search(&self, _request: &SearchRequest) -> Result<SearchAnswer, SearchError> {
        Err(SearchError::NotConfigured(self.reason.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn settings(extra: &[(&str, &str)]) -> ResearchSettings {
        let mut source: BTreeMap<String, String> = [
            (
                "OTDEL_RESEARCH_SEARCH_URL",
                "https://search.example.com/v1/search",
            ),
            ("OTDEL_RESEARCH_API_KEY", "srch-testkeyvalue0123"),
            ("OTDEL_RESEARCH_ALLOWED_HOSTS", "docs.example.org"),
        ]
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
        for (key, value) in extra {
            source.insert((*key).to_owned(), (*value).to_owned());
        }
        ResearchSettings::load(&source).unwrap()
    }

    fn request() -> SearchRequest {
        SearchRequest {
            query: "минимальная толщина цинкового покрытия".to_owned(),
            max_results: 5,
        }
    }

    #[test]
    fn an_incomplete_configuration_never_produces_a_client() {
        let source: BTreeMap<String, String> = BTreeMap::new();
        let empty = ResearchSettings::load(&source).unwrap();
        let error = HttpSearchProvider::new(&empty).unwrap_err();
        assert!(matches!(error, SearchError::NotConfigured(_)));
    }

    #[test]
    fn the_request_body_carries_the_query_and_no_key() {
        let provider = HttpSearchProvider::new(&settings(&[])).unwrap();
        let body = provider.body(&request());
        let rendered = serde_json::to_string(&body).unwrap();

        assert_eq!(body["query"], "минимальная толщина цинкового покрытия");
        assert_eq!(body["max_results"], 5);
        assert!(
            !rendered.contains("srch-"),
            "the key must travel in a header only: {rendered}"
        );
    }

    #[test]
    fn debug_output_of_the_client_hides_the_key() {
        let provider = HttpSearchProvider::new(&settings(&[])).unwrap();
        let rendered = format!("{provider:?}");
        assert!(!rendered.contains("srch-testkeyvalue"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(provider.describe().is_ready());
        assert_eq!(
            provider.describe().endpoint_host.as_deref(),
            Some("search.example.com")
        );
    }

    #[test]
    fn the_three_common_result_shapes_are_understood() {
        let expected = vec![SearchHit {
            url: "https://docs.example.org/gost".to_owned(),
            title: Some("ГОСТ".to_owned()),
            snippet: Some("фрагмент".to_owned()),
        }];

        for envelope in [
            json!({"results": [{"url": "https://docs.example.org/gost", "title": "ГОСТ", "snippet": "фрагмент"}]}),
            json!({"items": [{"link": "https://docs.example.org/gost", "name": "ГОСТ", "description": "фрагмент"}]}),
            json!({"web": {"results": [{"url": "https://docs.example.org/gost", "title": "ГОСТ", "summary": "фрагмент"}]}}),
        ] {
            assert_eq!(parse_hits(&envelope, 5).unwrap(), expected, "{envelope}");
        }
    }

    #[test]
    fn an_unexpected_answer_is_a_refusal_not_an_empty_result_list() {
        // "Zero results" and "the provider answered something else" must not look the
        // same: the first would read as "nothing is published about this".
        for envelope in [
            json!({"answer": "вот что я нашёл"}),
            json!([]),
            json!({"results": "нет"}),
        ] {
            assert!(
                matches!(
                    parse_hits(&envelope, 5).unwrap_err(),
                    SearchError::InvalidResponse(_)
                ),
                "{envelope} must be refused"
            );
        }

        // A genuinely empty list is a genuinely empty list.
        assert_eq!(parse_hits(&json!({"results": []}), 5).unwrap(), Vec::new());
    }

    #[test]
    fn an_error_body_returned_with_status_200_is_still_an_error() {
        let envelope = json!({"error": {"type": "quota_exceeded", "message": "no credit"}});
        let error = parse_hits(&envelope, 5).unwrap_err();
        assert!(error.diagnostic().contains("quota_exceeded"));
    }

    #[test]
    fn results_are_bounded_and_cleaned() {
        let envelope = json!({
            "results": (0..100)
                .map(|index| json!({
                    "url": format!("https://docs.example.org/{index}"),
                    "title": "a\u{0}b",
                    "snippet": "я".repeat(5_000),
                }))
                .collect::<Vec<_>>(),
        });

        let hits = parse_hits(&envelope, 5).unwrap();
        assert_eq!(
            hits.len(),
            5,
            "the caller's limit decides, not the provider"
        );
        assert_eq!(hits[0].title.as_deref(), Some("a b"));
        assert!(hits[0].snippet.as_ref().unwrap().chars().count() <= MAX_SNIPPET_CHARS);

        // A result without a URL is not a result.
        let partial =
            json!({"results": [{"title": "без ссылки"}, {"url": "https://docs.example.org/a"}]});
        assert_eq!(parse_hits(&partial, 5).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_unconfigured_adapter_names_what_is_missing_and_calls_nothing() {
        let source: BTreeMap<String, String> = BTreeMap::new();
        let provider = UnconfiguredSearch::new(&ResearchSettings::load(&source).unwrap());

        let description = provider.describe();
        assert_eq!(description.state, "needs_configuration");
        assert!(description.missing.contains(&"OTDEL_RESEARCH_SEARCH_URL"));
        assert!(description
            .missing
            .contains(&"OTDEL_RESEARCH_ALLOWED_HOSTS"));

        let error = provider.search(&request()).await.unwrap_err();
        assert!(matches!(error, SearchError::NotConfigured(_)));
        assert!(!error.was_sent(), "nothing may be charged for this");
        assert!(error.to_string().contains("OTDEL_RESEARCH_SEARCH_URL"));
    }

    #[tokio::test]
    async fn a_disabled_researcher_says_so_rather_than_naming_variables() {
        let provider =
            UnconfiguredSearch::new(&settings(&[("OTDEL_RESEARCH_PROVIDER", "disabled")]));
        let description = provider.describe();
        assert_eq!(description.state, "disabled");
        assert!(description.missing.is_empty());
        assert!(description.message.contains("выключено"));
    }
}
