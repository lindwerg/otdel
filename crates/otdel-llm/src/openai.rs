//! The OpenAI-compatible client (OpenRouter and anything speaking the same API).
//!
//! Everything that could go wrong quietly is made explicit here.
//!
//! * **The key only ever travels in the `Authorization` header**, is stored in
//!   [`otdel_core::llm_config::ApiKey`] (whose `Debug` is `<redacted>`), and never
//!   reaches a log line, an error message or a URL.
//! * **Nothing is unbounded.** Request timeout, response size, output tokens and a
//!   minimum interval between calls are all configuration-validated ceilings. The body
//!   is read as bytes with the size checked *before* it is parsed.
//! * **A truncated answer is a failure, not a partial result.** `finish_reason=length`
//!   means the JSON object is cut in half; accepting it would mean persisting whatever
//!   half-parsed.
//! * **The response schema is sent and re-checked.** `response_format` asks for
//!   structured output, but the caller validates the returned object again — a provider
//!   that ignores the schema must not be able to widen what gets stored.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use otdel_core::llm_config::{ApiKey, LlmSettings};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::provider::{
    LlmError, LlmProvider, LlmRequest, LlmResponse, LlmResult, ProviderDescription, Usage,
};

/// HTTP client against one configured endpoint and model.
pub struct OpenAiCompatibleProvider {
    client: reqwest::Client,
    endpoint: String,
    model: String,
    api_key: ApiKey,
    description: ProviderDescription,
    max_response_bytes: u64,
    min_request_interval: Duration,
    /// Time of the last request, so the pacing limit holds across concurrent callers.
    last_request: Mutex<Option<Instant>>,
}

impl std::fmt::Debug for OpenAiCompatibleProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatibleProvider")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("api_key", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl OpenAiCompatibleProvider {
    /// Build the client. Fails when the configuration is incomplete — the caller then
    /// uses [`crate::UnconfiguredProvider`] instead, and no client exists at all.
    pub fn new(settings: &LlmSettings) -> LlmResult<Self> {
        let availability = settings.availability();
        if !availability.is_ready() {
            return Err(LlmError::NotConfigured(format!(
                "состояние адаптера: {}",
                availability.as_str()
            )));
        }
        let api_key = settings
            .api_key
            .clone()
            .ok_or_else(|| LlmError::NotConfigured("не задан ключ".to_owned()))?;

        let client = reqwest::Client::builder()
            .timeout(settings.limits.timeout)
            .connect_timeout(Duration::from_secs(10))
            // A response that redirects is not an API answer. Following one could send
            // the Authorization header to a host the operator never configured.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("otdel/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| {
                warn!(error = %error, "could not build the model HTTP client");
                LlmError::Transport
            })?;

        Ok(Self {
            client,
            endpoint: settings.chat_completions_url(),
            model: settings.model.clone(),
            api_key,
            description: ProviderDescription {
                provider: settings.provider.as_str().to_owned(),
                model: settings.model.clone(),
                endpoint_host: settings.endpoint_host(),
                state: "ready",
                missing: Vec::new(),
                message: format!(
                    "Продуктолог использует модель {} через {}.",
                    settings.model,
                    settings.endpoint_host().unwrap_or_else(|| "—".to_owned())
                ),
            },
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

    fn body(&self, request: &LlmRequest) -> Value {
        json!({
            "model": self.model,
            // Zero temperature: this is extraction, not composition. A creative
            // paraphrase of a load table is exactly what must not happen.
            "temperature": 0,
            "max_tokens": request.max_output_tokens,
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": request.schema_name,
                    "strict": true,
                    "schema": request.schema,
                },
            },
            "messages": [
                {"role": "system", "content": request.system_prompt},
                {"role": "user", "content": request.user_prompt},
            ],
        })
    }
}

#[async_trait]
impl LlmProvider for OpenAiCompatibleProvider {
    fn describe(&self) -> ProviderDescription {
        self.description.clone()
    }

    async fn complete_json(&self, request: &LlmRequest) -> LlmResult<LlmResponse> {
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
                // `error` can contain the URL but never the header, so it is safe to
                // log at debug level; the returned error stays generic.
                debug!(error = %error, purpose = request.purpose, "model request failed");
                if error.is_timeout() {
                    LlmError::Timeout
                } else {
                    LlmError::Transport
                }
            })?;

        let status = response.status();
        if !status.is_success() {
            // The provider's own body may quote the request; it is not returned to the
            // client and not logged.
            return Err(match status.as_u16() {
                429 => LlmError::RateLimited,
                code => LlmError::Http {
                    status: code,
                    retryable: (500..600).contains(&code),
                },
            });
        }

        if let Some(length) = response.content_length() {
            if length > self.max_response_bytes {
                return Err(LlmError::InvalidResponse(format!(
                    "ответ больше разрешённых {} байт",
                    self.max_response_bytes
                )));
            }
        }

        let bytes = response.bytes().await.map_err(|_| LlmError::Transport)?;
        if bytes.len() as u64 > self.max_response_bytes {
            return Err(LlmError::InvalidResponse(format!(
                "ответ больше разрешённых {} байт",
                self.max_response_bytes
            )));
        }

        let envelope: Value = serde_json::from_slice(&bytes)
            .map_err(|_| LlmError::InvalidResponse("тело ответа не является JSON".to_owned()))?;

        let parsed = parse_completion(&envelope)?;
        let duration = started.elapsed();

        debug!(
            purpose = request.purpose,
            model = %parsed.model,
            duration_ms = duration.as_millis() as u64,
            prompt_tokens = parsed.usage.prompt_tokens,
            completion_tokens = parsed.usage.completion_tokens,
            response_chars = parsed.content.chars().count(),
            "model call finished"
        );

        Ok(LlmResponse {
            response_chars: parsed.content.chars().count(),
            json: parsed.json,
            model: parsed.model,
            usage: parsed.usage,
            duration,
        })
    }
}

#[derive(Debug)]
struct ParsedCompletion {
    json: Value,
    content: String,
    model: String,
    usage: Usage,
}

/// Pull the single JSON object out of an OpenAI-compatible completion envelope.
///
/// Separate from the HTTP call so it can be tested against real-shaped payloads without
/// a network, which is also why every failure mode here is named.
fn parse_completion(envelope: &Value) -> LlmResult<ParsedCompletion> {
    // Some gateways answer 200 with an `error` object instead of an HTTP error code.
    if let Some(error) = envelope.get("error") {
        let kind = error
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(LlmError::InvalidResponse(format!(
            "провайдер вернул ошибку в теле ответа ({kind})"
        )));
    }

    let choice = envelope
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| LlmError::InvalidResponse("в ответе нет ни одного варианта".to_owned()))?;

    if choice.get("finish_reason").and_then(Value::as_str) == Some("length") {
        return Err(LlmError::Truncated);
    }

    let content = choice
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .ok_or_else(|| LlmError::InvalidResponse("в ответе нет текста сообщения".to_owned()))?;

    if content.trim().is_empty() {
        return Err(LlmError::InvalidResponse(
            "модель вернула пустой ответ".to_owned(),
        ));
    }

    let json: Value = serde_json::from_str(content).map_err(|_| {
        LlmError::InvalidResponse("содержимое ответа не разбирается как JSON".to_owned())
    })?;
    if !json.is_object() {
        return Err(LlmError::InvalidResponse(
            "содержимое ответа не является JSON-объектом".to_owned(),
        ));
    }

    let usage = envelope.get("usage");
    Ok(ParsedCompletion {
        json,
        content: content.to_owned(),
        model: envelope
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        usage: Usage {
            prompt_tokens: usage
                .and_then(|usage| usage.get("prompt_tokens"))
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok()),
            completion_tokens: usage
                .and_then(|usage| usage.get("completion_tokens"))
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok()),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn ready_settings() -> LlmSettings {
        let source: BTreeMap<String, String> = [
            ("OTDEL_LLM_API_KEY", "sk-or-v1-testkeyvalue0123"),
            ("OTDEL_LLM_MODEL", "openai/gpt-4o-mini"),
        ]
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
        LlmSettings::load(&source).unwrap()
    }

    fn request() -> LlmRequest {
        LlmRequest {
            purpose: "knowledge_draft",
            system_prompt: "система".to_owned(),
            user_prompt: "материал".to_owned(),
            schema_name: "otdel_knowledge_draft",
            schema: json!({"type": "object", "additionalProperties": false}),
            max_output_tokens: 1234,
        }
    }

    #[test]
    fn an_incomplete_configuration_never_produces_a_client() {
        let source: BTreeMap<String, String> = BTreeMap::new();
        let settings = LlmSettings::load(&source).unwrap();
        let error = OpenAiCompatibleProvider::new(&settings).unwrap_err();
        assert!(matches!(error, LlmError::NotConfigured(_)));
    }

    #[test]
    fn the_request_body_carries_the_schema_and_no_key() {
        let provider = OpenAiCompatibleProvider::new(&ready_settings()).unwrap();
        let body = provider.body(&request());
        let rendered = serde_json::to_string(&body).unwrap();

        assert_eq!(body["model"], "openai/gpt-4o-mini");
        assert_eq!(body["temperature"], 0);
        assert_eq!(body["max_tokens"], 1234);
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
        assert_eq!(
            body["response_format"]["json_schema"]["name"],
            "otdel_knowledge_draft"
        );
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "материал");
        assert!(
            !rendered.contains("sk-or-v1"),
            "the key must travel in the Authorization header only"
        );
    }

    #[test]
    fn debug_output_of_the_client_hides_the_key() {
        let provider = OpenAiCompatibleProvider::new(&ready_settings()).unwrap();
        let rendered = format!("{provider:?}");
        assert!(!rendered.contains("sk-or-v1"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(provider.describe().is_ready());
    }

    #[test]
    fn a_well_formed_completion_is_parsed_with_its_usage() {
        let envelope = json!({
            "model": "openai/gpt-4o-mini",
            "choices": [{
                "finish_reason": "stop",
                "message": {"role": "assistant", "content": "{\"facts\":[]}"},
            }],
            "usage": {"prompt_tokens": 120, "completion_tokens": 30},
        });
        let parsed = parse_completion(&envelope).unwrap();
        assert_eq!(parsed.json["facts"], json!([]));
        assert_eq!(parsed.model, "openai/gpt-4o-mini");
        assert_eq!(parsed.usage.prompt_tokens, Some(120));
        assert_eq!(parsed.usage.completion_tokens, Some(30));
    }

    #[test]
    fn a_truncated_answer_is_refused_rather_than_half_accepted() {
        let envelope = json!({
            "choices": [{
                "finish_reason": "length",
                "message": {"content": "{\"facts\":[{\"attribute\":\"дли"},
            }],
        });
        assert!(matches!(
            parse_completion(&envelope).unwrap_err(),
            LlmError::Truncated
        ));
    }

    #[test]
    fn prose_instead_of_json_is_refused() {
        for content in [
            "Конечно! Вот факты:",
            "",
            "   ",
            "[1, 2, 3]",
            "{\"facts\": [",
        ] {
            let envelope = json!({
                "choices": [{"finish_reason": "stop", "message": {"content": content}}],
            });
            assert!(
                matches!(
                    parse_completion(&envelope).unwrap_err(),
                    LlmError::InvalidResponse(_)
                ),
                "content {content:?} must not be accepted"
            );
        }
    }

    #[test]
    fn an_error_body_returned_with_status_200_is_still_an_error() {
        let envelope = json!({"error": {"type": "insufficient_quota", "message": "no credit"}});
        let error = parse_completion(&envelope).unwrap_err();
        assert!(matches!(error, LlmError::InvalidResponse(_)));
        assert!(error.diagnostic().contains("insufficient_quota"));
    }

    #[test]
    fn an_envelope_without_choices_is_refused() {
        assert!(matches!(
            parse_completion(&json!({"model": "m"})).unwrap_err(),
            LlmError::InvalidResponse(_)
        ));
        assert!(matches!(
            parse_completion(&json!({"choices": []})).unwrap_err(),
            LlmError::InvalidResponse(_)
        ));
    }
}
