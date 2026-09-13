//! A scripted provider for tests. Behind the `fake` feature, off by default.
//!
//! The point of this adapter is that the *rest* of the pipeline — prompt building,
//! validation against real sources, persistence, the queue — can be exercised end to
//! end without a key and without a network. It answers from a script the test wrote, so
//! a test can also express the cases that matter most: a model that cites a source that
//! does not exist, one that invents a quote, one that returns prose, one that fails.
//!
//! It is never built by `build_provider` in a production binary: the feature is only
//! enabled by dev-dependencies.

use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use crate::provider::{
    LlmError, LlmProvider, LlmRequest, LlmResponse, LlmResult, ProviderDescription, Usage,
};

/// What the fake does on the next call.
#[derive(Debug)]
pub enum FakeReply {
    /// Answer with this JSON object.
    Json(Value),
    /// Answer with this raw string, as if the model ignored the schema.
    Raw(String),
    /// Fail with this error.
    Fail(LlmError),
}

impl FakeReply {
    pub fn json(value: Value) -> Self {
        Self::Json(value)
    }
}

/// Records the prompts it was given and replies from a script.
pub struct FakeProvider {
    replies: Mutex<Vec<FakeReply>>,
    prompts: Mutex<Vec<String>>,
    model: String,
}

impl std::fmt::Debug for FakeProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeProvider")
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl FakeProvider {
    /// Replies are consumed in order; running out is itself a failure, so a test that
    /// expects two calls and gets three finds out.
    pub fn new(replies: Vec<FakeReply>) -> Self {
        Self {
            replies: Mutex::new(replies.into_iter().rev().collect()),
            prompts: Mutex::new(Vec::new()),
            model: "fake/model-1".to_owned(),
        }
    }

    /// One reply, the common case.
    pub fn answering(value: Value) -> Self {
        Self::new(vec![FakeReply::Json(value)])
    }

    pub fn failing(error: LlmError) -> Self {
        Self::new(vec![FakeReply::Fail(error)])
    }

    /// Every user prompt this provider was given, in order — so a test can assert what
    /// the model was and was not shown.
    pub fn prompts(&self) -> Vec<String> {
        self.prompts.lock().expect("fake provider lock").clone()
    }

    pub fn call_count(&self) -> usize {
        self.prompts.lock().expect("fake provider lock").len()
    }
}

#[async_trait]
impl LlmProvider for FakeProvider {
    fn describe(&self) -> ProviderDescription {
        ProviderDescription {
            provider: "fake".to_owned(),
            model: self.model.clone(),
            endpoint_host: None,
            state: "ready",
            missing: Vec::new(),
            message: "Тестовый провайдер: сетевые вызовы не выполняются.".to_owned(),
        }
    }

    async fn complete_json(&self, request: &LlmRequest) -> LlmResult<LlmResponse> {
        self.prompts
            .lock()
            .expect("fake provider lock")
            .push(request.user_prompt.clone());

        let reply = self
            .replies
            .lock()
            .expect("fake provider lock")
            .pop()
            .ok_or_else(|| {
                LlmError::InvalidResponse("тестовый провайдер: ответы закончились".to_owned())
            })?;

        match reply {
            FakeReply::Json(json) => {
                let response_chars = json.to_string().chars().count();
                Ok(LlmResponse {
                    json,
                    model: self.model.clone(),
                    usage: Usage::default(),
                    duration: Duration::from_millis(1),
                    response_chars,
                })
            }
            FakeReply::Raw(raw) => {
                let json: Value = serde_json::from_str(&raw).map_err(|_| {
                    LlmError::InvalidResponse(
                        "содержимое ответа не разбирается как JSON".to_owned(),
                    )
                })?;
                if !json.is_object() {
                    return Err(LlmError::InvalidResponse(
                        "содержимое ответа не является JSON-объектом".to_owned(),
                    ));
                }
                let response_chars = raw.chars().count();
                Ok(LlmResponse {
                    json,
                    model: self.model.clone(),
                    usage: Usage::default(),
                    duration: Duration::from_millis(1),
                    response_chars,
                })
            }
            FakeReply::Fail(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(user: &str) -> LlmRequest {
        LlmRequest {
            purpose: "test",
            system_prompt: "s".to_owned(),
            user_prompt: user.to_owned(),
            schema_name: "test",
            schema: json!({"type": "object"}),
            max_output_tokens: 10,
        }
    }

    #[tokio::test]
    async fn replies_are_consumed_in_order_and_prompts_are_recorded() {
        let provider = FakeProvider::new(vec![
            FakeReply::Json(json!({"first": true})),
            FakeReply::Fail(LlmError::RateLimited),
        ]);

        let first = provider.complete_json(&request("one")).await.unwrap();
        assert_eq!(first.json["first"], true);

        let second = provider.complete_json(&request("two")).await.unwrap_err();
        assert!(matches!(second, LlmError::RateLimited));

        assert_eq!(provider.prompts(), vec!["one".to_owned(), "two".to_owned()]);
        assert_eq!(provider.call_count(), 2);

        // Running out of script is a loud failure, not an empty answer.
        assert!(provider.complete_json(&request("three")).await.is_err());
    }

    #[tokio::test]
    async fn raw_prose_is_rejected_exactly_as_the_real_client_rejects_it() {
        let provider = FakeProvider::new(vec![FakeReply::Raw("вот факты:".to_owned())]);
        assert!(matches!(
            provider.complete_json(&request("x")).await.unwrap_err(),
            LlmError::InvalidResponse(_)
        ));
    }
}
