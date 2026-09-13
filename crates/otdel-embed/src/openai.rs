//! The OpenAI-compatible `/embeddings` client (OpenRouter, and anything speaking the
//! same API, including a model hosted on loopback).
//!
//! Everything that could go wrong quietly is made explicit here.
//!
//! * **The key only ever travels in the `Authorization` header**, is stored in
//!   [`otdel_core::llm_config::ApiKey`] (whose `Debug` is `<redacted>`), and never
//!   reaches a log line, an error message or a URL.
//! * **The body ceiling is real.** `Content-Length` is a claim by the other side, not a
//!   bound: a chunked response, or one that simply lies, would stream into memory until
//!   the process dies. The body is therefore read chunk by chunk and abandoned the
//!   instant it passes the configured ceiling — before anything is parsed
//!   (`docs/research-1d.md`, defect 9).
//! * **The batch comes back whole or not at all.** `data` shorter or longer than the
//!   input list is [`EmbedError::CountMismatch`]; vectors of unequal length are
//!   [`EmbedError::DimensionMismatch`]. Neither is patched up by zipping what arrived,
//!   because a vector attached to the wrong claim is invisible afterwards — every
//!   distance still computes, and every neighbour is wrong.
//! * **Order is taken from `index`, not from the array.** The API documents `index` on
//!   each entry precisely because the array order is not guaranteed. Trusting position
//!   would be the same off-by-one as above, only intermittent.
//! * **Nothing is unbounded.** Request timeout, connect timeout, response size, input
//!   length and a minimum interval between calls are all configuration-validated
//!   ceilings.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use otdel_core::llm_config::ApiKey;
use otdel_core::retrieval_config::EmbeddingSettings;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::body::read_bounded;
use crate::provider::{
    EmbedError, EmbedRequest, EmbedResponse, EmbedResult, EmbeddingDescription, EmbeddingProvider,
};

/// HTTP client against one configured endpoint and embedding model.
pub struct HttpEmbeddingProvider {
    client: reqwest::Client,
    endpoint: String,
    model: String,
    api_key: ApiKey,
    description: EmbeddingDescription,
    max_input_chars: usize,
    max_response_bytes: u64,
    min_request_interval: Duration,
    /// Time of the last request, so the pacing limit holds across concurrent callers.
    last_request: Mutex<Option<Instant>>,
}

impl std::fmt::Debug for HttpEmbeddingProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpEmbeddingProvider")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("api_key", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl HttpEmbeddingProvider {
    /// Build the client. Fails when the configuration is incomplete — the caller then
    /// uses [`crate::UnconfiguredEmbeddings`] instead, and no client exists at all.
    pub fn new(settings: &EmbeddingSettings) -> EmbedResult<Self> {
        let availability = settings.availability();
        if !availability.is_ready() {
            return Err(EmbedError::NotConfigured(format!(
                "состояние адаптера: {}",
                availability.as_str()
            )));
        }
        let api_key = settings
            .api_key
            .clone()
            .ok_or_else(|| EmbedError::NotConfigured("не задан ключ".to_owned()))?;

        let client = reqwest::Client::builder()
            .timeout(settings.limits.timeout)
            .connect_timeout(Duration::from_secs(10))
            // A response that redirects is not an API answer. Following one could send
            // the Authorization header to a host the operator never configured.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("otdel/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| {
                warn!(error = %error, "could not build the embedding HTTP client");
                EmbedError::Transport
            })?;

        Ok(Self {
            client,
            endpoint: settings.embeddings_url(),
            model: settings.model.clone(),
            api_key,
            description: EmbeddingDescription {
                provider: settings.provider.as_str().to_owned(),
                model: settings.model.clone(),
                endpoint_host: settings.endpoint_host(),
                state: "ready",
                missing: Vec::new(),
                message: format!(
                    "Векторы строятся моделью {} через {}.",
                    settings.model,
                    settings.endpoint_host().unwrap_or_else(|| "—".to_owned())
                ),
                profile: settings.profile(),
            },
            max_input_chars: settings.limits.max_input_chars as usize,
            max_response_bytes: settings.limits.max_response_bytes,
            min_request_interval: settings.limits.min_request_interval,
            last_request: Mutex::new(None),
        })
    }

    /// Hold the courtesy rate limit. Held across the whole request so two concurrent
    /// runs cannot both slip past it.
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

    fn body(&self, request: &EmbedRequest) -> Value {
        let inputs: Vec<String> = request
            .inputs
            .iter()
            .map(|input| clip(input, self.max_input_chars))
            .collect();
        json!({
            "model": self.model,
            "input": inputs,
        })
    }
}

#[async_trait]
impl EmbeddingProvider for HttpEmbeddingProvider {
    fn describe(&self) -> EmbeddingDescription {
        self.description.clone()
    }

    async fn embed(&self, request: &EmbedRequest) -> EmbedResult<EmbedResponse> {
        let expected = request.inputs.len();
        // An empty batch is answered without a socket: there is nothing to ask about,
        // and sending it would spend the pacing budget on a question with no content.
        if expected == 0 {
            return Ok(EmbedResponse {
                vectors: Vec::new(),
                model: self.model.clone(),
                dimensions: 0,
                duration: Duration::ZERO,
            });
        }

        self.pace().await;
        let started = Instant::now();

        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(self.api_key.expose())
            .json(&self.body(request))
            .send()
            .await
            .map_err(|error| {
                // `error` can contain the URL but never the header, so it is safe to log
                // at debug level; the returned error stays generic.
                debug!(error = %error, purpose = request.purpose, "embedding request failed");
                if error.is_timeout() {
                    EmbedError::Timeout
                } else {
                    EmbedError::Transport
                }
            })?;

        let status = response.status();
        if !status.is_success() {
            // The service's own body may quote the request; it is not returned to the
            // caller and not logged.
            return Err(http_error(status.as_u16()));
        }

        let bytes = read_bounded(response, self.max_response_bytes).await?;
        let envelope = decode_envelope(&bytes)?;
        let parsed = parse_embeddings(&envelope, expected)?;
        let duration = started.elapsed();

        debug!(
            purpose = request.purpose,
            model = %parsed.model,
            duration_ms = duration.as_millis() as u64,
            vectors = parsed.vectors.len(),
            dimensions = parsed.dimensions,
            input_chars = request.input_chars(),
            "embedding batch finished"
        );

        Ok(EmbedResponse {
            vectors: parsed.vectors,
            model: parsed.model,
            dimensions: parsed.dimensions,
            duration,
        })
    }
}

/// Decode the body as JSON, saying so when it is not.
///
/// A body cut short by a dropped connection parses as nothing, not as a partial batch —
/// which is the point: half a vector list is no vector list.
fn decode_envelope(bytes: &[u8]) -> EmbedResult<Value> {
    serde_json::from_slice(bytes)
        .map_err(|_| EmbedError::InvalidResponse("тело ответа не является JSON".to_owned()))
}

/// Bound and flatten a string that came from somebody else's service.
///
/// It ends up inside an error that is both shown to the owner and written to a log, and
/// `Display` does neither of these things — only `diagnostic()` does, and not every
/// caller reaches for it. A service that answers with newlines in `error.type` would
/// otherwise write its own lines into the log.
fn sanitise_remote(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(80)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// What an unsuccessful HTTP status means for the queue.
///
/// Only a status the service itself can stop returning is retryable: a rate limit and the
/// 5xx family. A 400 or a 401 means the request or the key is wrong, and repeating it
/// spends the budget on a problem only a human can fix.
fn http_error(status: u16) -> EmbedError {
    match status {
        429 => EmbedError::RateLimited,
        code => EmbedError::Http {
            status: code,
            retryable: (500..600).contains(&code),
        },
    }
}

/// Cut to a number of *characters*, never bytes.
///
/// Slicing by byte offset would panic in the middle of a Cyrillic letter, which is most
/// of the corpus this system reads.
fn clip(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    text.chars().take(max_chars).collect()
}

#[derive(Debug)]
struct ParsedEmbeddings {
    vectors: Vec<Vec<f32>>,
    model: String,
    dimensions: usize,
}

/// Pull exactly `expected` vectors out of an OpenAI-compatible embeddings envelope.
///
/// Separate from the HTTP call so it can be tested against real-shaped payloads without a
/// network, which is also why every failure mode here is named rather than smoothed over.
fn parse_embeddings(envelope: &Value, expected: usize) -> EmbedResult<ParsedEmbeddings> {
    // Some gateways answer 200 with an `error` object instead of an HTTP error code.
    if let Some(error) = envelope.get("error") {
        let kind = sanitise_remote(
            error
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
        );
        return Err(EmbedError::InvalidResponse(format!(
            "сервис вернул ошибку в теле ответа ({kind})"
        )));
    }

    let data = envelope
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| EmbedError::InvalidResponse("в ответе нет массива data".to_owned()))?;

    if data.len() != expected {
        return Err(EmbedError::CountMismatch {
            expected,
            got: data.len(),
        });
    }

    let order = reading_order(data)?;

    let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(expected);
    for position in order {
        vectors.push(read_vector(&data[position])?);
    }

    // `expected == 0` is short-circuited by `embed`, but this function is the one that
    // would panic if that ever stopped being true. A parser is not the place to depend on
    // a caller's invariant.
    let Some(first) = vectors.first() else {
        return Ok(ParsedEmbeddings {
            vectors,
            model: envelope
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
            dimensions: 0,
        });
    };
    let dimensions = first.len();
    if let Some(other) = vectors.iter().find(|vector| vector.len() != dimensions) {
        return Err(EmbedError::DimensionMismatch {
            expected: dimensions,
            got: other.len(),
        });
    }

    Ok(ParsedEmbeddings {
        vectors,
        model: envelope
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        dimensions,
    })
}

/// Positions of `data`, in the order the inputs were sent.
///
/// When every entry carries an `index`, that is the order — and the indices must form the
/// complete set `0..n`, because a duplicate or a gap would otherwise silently map two
/// texts onto one vector. When no entry carries one, array order is all there is, and it
/// is used as given. A mixture is refused: half an ordering is not an ordering.
fn reading_order(data: &[Value]) -> EmbedResult<Vec<usize>> {
    let indices: Vec<Option<usize>> = data
        .iter()
        .map(|entry| {
            entry
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
        })
        .collect();

    if indices.iter().all(Option::is_none) {
        return Ok((0..data.len()).collect());
    }
    if indices.iter().any(Option::is_none) {
        return Err(EmbedError::InvalidResponse(
            "часть векторов в ответе без index — порядок восстановить нельзя".to_owned(),
        ));
    }

    // `position_of[i]` = which entry of `data` holds the vector for input `i`.
    let mut position_of = vec![usize::MAX; data.len()];
    for (position, index) in indices.iter().enumerate() {
        let index = index.expect("checked above that every index is present");
        if index >= data.len() || position_of[index] != usize::MAX {
            return Err(EmbedError::InvalidResponse(
                "индексы векторов в ответе не образуют полный набор".to_owned(),
            ));
        }
        position_of[index] = position;
    }
    Ok(position_of)
}

/// One `embedding` array, as finite `f32`.
///
/// A `null`, a string or a NaN inside a vector is refused rather than coerced to zero:
/// a zeroed coordinate is a real point in the space, and would quietly move the claim.
fn read_vector(entry: &Value) -> EmbedResult<Vec<f32>> {
    let raw = entry
        .get("embedding")
        .and_then(Value::as_array)
        .ok_or_else(|| EmbedError::InvalidResponse("в элементе ответа нет embedding".to_owned()))?;

    if raw.is_empty() {
        return Err(EmbedError::InvalidResponse(
            "вектор нулевой длины".to_owned(),
        ));
    }

    raw.iter()
        .map(|value| {
            let number = value.as_f64().ok_or_else(|| {
                EmbedError::InvalidResponse("координата вектора не является числом".to_owned())
            })?;
            let number = number as f32;
            if !number.is_finite() {
                return Err(EmbedError::InvalidResponse(
                    "координата вектора не является конечным числом".to_owned(),
                ));
            }
            Ok(number)
        })
        .collect()
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

    fn ready_settings() -> EmbeddingSettings {
        settings(&[
            ("OTDEL_EMBEDDING_API_KEY", "sk-or-v1-testkeyvalue0123"),
            ("OTDEL_EMBEDDING_MODEL", "openai/text-embedding-3-small"),
        ])
    }

    fn request(inputs: &[&str]) -> EmbedRequest {
        EmbedRequest {
            purpose: "claim_chunks",
            inputs: inputs.iter().map(|input| (*input).to_owned()).collect(),
        }
    }

    fn entry(index: Option<u64>, embedding: Vec<f64>) -> Value {
        match index {
            Some(index) => json!({"index": index, "embedding": embedding}),
            None => json!({"embedding": embedding}),
        }
    }

    #[test]
    fn an_incomplete_configuration_never_produces_a_client() {
        let error = HttpEmbeddingProvider::new(&settings(&[])).unwrap_err();
        assert!(matches!(error, EmbedError::NotConfigured(_)));

        let disabled = settings(&[
            ("OTDEL_EMBEDDING_PROVIDER", "disabled"),
            ("OTDEL_EMBEDDING_API_KEY", "sk-or-v1-testkeyvalue0123"),
            ("OTDEL_EMBEDDING_MODEL", "openai/text-embedding-3-small"),
        ]);
        assert!(matches!(
            HttpEmbeddingProvider::new(&disabled).unwrap_err(),
            EmbedError::NotConfigured(_)
        ));
    }

    #[test]
    fn the_request_body_carries_the_model_and_the_inputs_and_no_key() {
        let provider = HttpEmbeddingProvider::new(&ready_settings()).unwrap();
        let body = provider.body(&request(&["длина проёма", "ширина проёма"]));
        let rendered = serde_json::to_string(&body).unwrap();

        assert_eq!(body["model"], "openai/text-embedding-3-small");
        assert_eq!(body["input"][0], "длина проёма");
        assert_eq!(body["input"][1], "ширина проёма");
        assert_eq!(body["input"].as_array().unwrap().len(), 2);
        assert!(
            !rendered.contains("sk-or-v1"),
            "the key must travel in the Authorization header only"
        );
    }

    #[test]
    fn a_long_input_is_clipped_to_the_configured_ceiling_before_it_is_sent() {
        let provider = HttpEmbeddingProvider::new(&settings(&[
            ("OTDEL_EMBEDDING_API_KEY", "sk-or-v1-testkeyvalue0123"),
            ("OTDEL_EMBEDDING_MODEL", "openai/text-embedding-3-small"),
            ("OTDEL_EMBEDDING_MAX_INPUT_CHARS", "200"),
        ]))
        .unwrap();
        let long = "я".repeat(5_000);
        let body = provider.body(&request(&[long.as_str()]));
        assert_eq!(body["input"][0].as_str().unwrap().chars().count(), 200);
    }

    #[test]
    fn debug_output_of_the_client_hides_the_key() {
        let provider = HttpEmbeddingProvider::new(&ready_settings()).unwrap();
        let rendered = format!("{provider:?}");
        assert!(!rendered.contains("sk-or-v1"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");

        let description = provider.describe();
        assert!(description.is_ready());
        assert_eq!(description.endpoint_host.as_deref(), Some("openrouter.ai"));
        assert_eq!(
            description.profile.as_deref(),
            Some("openai_compatible:openai/text-embedding-3-small")
        );
        assert!(!format!("{description:?}").contains("sk-or-v1"));
    }

    #[tokio::test]
    async fn an_empty_batch_is_answered_without_touching_the_network() {
        let provider = HttpEmbeddingProvider::new(&ready_settings()).unwrap();
        let response = provider.embed(&request(&[])).await.unwrap();
        assert!(response.vectors.is_empty());
        assert_eq!(response.dimensions, 0);
    }

    #[test]
    fn clipping_cuts_on_character_boundaries_of_multi_byte_text() {
        assert_eq!(clip("длина", 3), "дли");
        assert_eq!(clip("длина", 5), "длина");
        assert_eq!(clip("длина", 50), "длина");
        assert_eq!(clip("", 4), "");
        // Emoji are several bytes and one character each; a byte slice would panic here.
        assert_eq!(clip("📐📏🧱", 2).chars().count(), 2);
        assert_eq!(clip("📐📏🧱", 2), "📐📏");
    }

    #[test]
    fn a_well_formed_batch_is_parsed_in_input_order() {
        let envelope = json!({
            "model": "openai/text-embedding-3-small",
            "data": [
                entry(Some(0), vec![0.1, 0.2]),
                entry(Some(1), vec![0.3, 0.4]),
            ],
        });
        let parsed = parse_embeddings(&envelope, 2).unwrap();
        assert_eq!(parsed.model, "openai/text-embedding-3-small");
        assert_eq!(parsed.dimensions, 2);
        assert_eq!(parsed.vectors.len(), 2);
        assert!((parsed.vectors[0][0] - 0.1).abs() < f32::EPSILON);
        assert!((parsed.vectors[1][1] - 0.4).abs() < f32::EPSILON);
    }

    #[test]
    fn data_arriving_out_of_order_is_sorted_by_index() {
        let envelope = json!({
            "data": [
                entry(Some(2), vec![2.0]),
                entry(Some(0), vec![0.0]),
                entry(Some(1), vec![1.0]),
            ],
        });
        let parsed = parse_embeddings(&envelope, 3).unwrap();
        assert_eq!(
            parsed.vectors,
            vec![vec![0.0_f32], vec![1.0_f32], vec![2.0_f32]],
            "vectors must come back matched to the inputs, not to the array positions"
        );
    }

    #[test]
    fn an_envelope_without_indices_keeps_the_array_order() {
        let envelope = json!({
            "data": [entry(None, vec![7.0]), entry(None, vec![8.0])],
        });
        let parsed = parse_embeddings(&envelope, 2).unwrap();
        assert_eq!(parsed.vectors, vec![vec![7.0_f32], vec![8.0_f32]]);
    }

    #[test]
    fn indices_that_do_not_form_a_complete_set_are_refused() {
        for data in [
            // A duplicate: one input would silently get two vectors and another none.
            json!([entry(Some(0), vec![1.0]), entry(Some(0), vec![2.0])]),
            // Out of range.
            json!([entry(Some(0), vec![1.0]), entry(Some(9), vec![2.0])]),
            // Half an ordering is not an ordering.
            json!([entry(Some(0), vec![1.0]), entry(None, vec![2.0])]),
        ] {
            let envelope = json!({ "data": data });
            assert!(
                matches!(
                    parse_embeddings(&envelope, 2).unwrap_err(),
                    EmbedError::InvalidResponse(_)
                ),
                "{data:?} must not be accepted"
            );
        }
    }

    #[test]
    fn fewer_vectors_than_inputs_is_a_count_mismatch_not_a_silent_zip() {
        let envelope = json!({
            "data": [entry(Some(0), vec![0.1]), entry(Some(1), vec![0.2])],
        });
        let error = parse_embeddings(&envelope, 3).unwrap_err();
        assert!(
            matches!(
                error,
                EmbedError::CountMismatch {
                    expected: 3,
                    got: 2
                }
            ),
            "{error:?}"
        );

        // And one too many is equally refused.
        assert!(matches!(
            parse_embeddings(&envelope, 1).unwrap_err(),
            EmbedError::CountMismatch {
                expected: 1,
                got: 2
            }
        ));
    }

    #[test]
    fn vectors_of_different_lengths_cannot_belong_to_one_space_and_are_refused() {
        let envelope = json!({
            "data": [
                entry(Some(0), vec![0.1, 0.2, 0.3]),
                entry(Some(1), vec![0.4, 0.5]),
            ],
        });
        let error = parse_embeddings(&envelope, 2).unwrap_err();
        assert!(
            matches!(
                error,
                EmbedError::DimensionMismatch {
                    expected: 3,
                    got: 2
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn a_body_that_is_not_a_batch_of_vectors_is_refused() {
        for envelope in [
            // An error object returned with status 200.
            json!({"error": {"type": "insufficient_quota", "message": "no credit"}}),
            // No data at all.
            json!({"model": "m"}),
            json!({"data": "не массив"}),
            // An entry without an embedding, or with one that is not numbers.
            json!({"data": [{"index": 0, "object": "embedding"}]}),
            json!({"data": [entry(Some(0), vec![]) ]}),
            json!({"data": [{"index": 0, "embedding": ["0.1"]}]}),
            json!({"data": [{"index": 0, "embedding": [null]}]}),
            // Finite as f64, infinite as f32 — a coordinate that cannot be stored.
            json!({"data": [{"index": 0, "embedding": [1e300]}]}),
        ] {
            assert!(
                matches!(
                    parse_embeddings(&envelope, 1).unwrap_err(),
                    EmbedError::InvalidResponse(_)
                ),
                "{envelope} must not be accepted"
            );
        }
    }

    #[test]
    fn an_error_body_returned_with_status_200_names_its_kind() {
        let envelope = json!({"error": {"type": "insufficient_quota"}});
        let error = parse_embeddings(&envelope, 1).unwrap_err();
        assert!(error.diagnostic().contains("insufficient_quota"));
    }

    #[test]
    fn a_truncated_or_non_json_body_is_an_invalid_response() {
        for bytes in [
            &br#"{"data": [{"index": 0, "embed"#[..],
            &b"<html><body>502 Bad Gateway</body></html>"[..],
            &b""[..],
        ] {
            assert!(
                matches!(
                    decode_envelope(bytes).unwrap_err(),
                    EmbedError::InvalidResponse(_)
                ),
                "{:?} must not decode",
                String::from_utf8_lossy(bytes)
            );
        }

        assert!(decode_envelope(br#"{"data": []}"#).is_ok());
    }

    #[tokio::test]
    async fn pacing_returns_immediately_when_no_interval_is_configured() {
        let provider = HttpEmbeddingProvider::new(&settings(&[
            ("OTDEL_EMBEDDING_API_KEY", "sk-or-v1-testkeyvalue0123"),
            ("OTDEL_EMBEDDING_MODEL", "openai/text-embedding-3-small"),
            ("OTDEL_EMBEDDING_MIN_REQUEST_INTERVAL_MS", "0"),
        ]))
        .unwrap();
        provider.pace().await;
        assert!(
            provider.last_request.lock().await.is_none(),
            "a zero interval must not even record a timestamp"
        );
    }

    #[tokio::test]
    async fn pacing_records_the_moment_of_the_call_so_concurrent_callers_queue() {
        let provider = HttpEmbeddingProvider::new(&ready_settings()).unwrap();
        provider.pace().await;
        assert!(provider.last_request.lock().await.is_some());
    }

    #[test]
    fn a_rate_limit_is_retryable_and_a_bad_request_is_not() {
        assert!(matches!(http_error(429), EmbedError::RateLimited));
        assert!(http_error(429).is_retryable());

        assert!(http_error(500).is_retryable());
        assert!(http_error(503).is_retryable());

        for status in [400, 401, 403, 404, 422] {
            let error = http_error(status);
            assert!(
                matches!(error, EmbedError::Http { retryable: false, status: code } if code == status),
                "{status} must not be retried: {error:?}"
            );
        }
    }
}
