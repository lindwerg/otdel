//! A scripted embedding adapter for tests. Behind the `fake` feature, off by default.
//!
//! The point of this adapter is that the *rest* of phase 1E — chunking, storing vectors
//! under a profile, the distance query, the fall back to keyword search — can be
//! exercised without a key and without a network.
//!
//! Its deterministic mode produces a stable vector per text, and that mode is exactly the
//! thing this crate refuses to do in production: the numbers come from a hash, they carry
//! no meaning, and two texts about the same thing are as far apart as two unrelated ones.
//! That is fine here, where a test asserts which row came back, and catastrophic in a
//! real index, where nobody would ever find out. Two guards keep the two apart:
//! [`FakeEmbeddings::describe`] reports `provider: "fake"` and a profile prefixed the
//! same way, so a vector built here can never be mistaken for one from a real space; and
//! the feature is only ever enabled by dev-dependencies, so `build_provider` in a
//! production binary cannot reach this file.
//!
//! The fake also enforces the contract the real client enforces: a scripted batch of the
//! wrong size, or of ragged vectors, comes back as the same error the HTTP client would
//! return, so no test can depend on a shape the real service could never deliver.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;

use crate::provider::{
    EmbedError, EmbedRequest, EmbedResponse, EmbedResult, EmbeddingDescription, EmbeddingProvider,
};

/// What the fake does on the next batch.
#[derive(Debug)]
pub enum FakeEmbedReply {
    /// Answer with these vectors, one per input.
    Vectors(Vec<Vec<f32>>),
    /// Fail with this error.
    Fail(EmbedError),
}

/// Records the texts it was asked to embed and replies from a script.
pub struct FakeEmbeddings {
    replies: Mutex<Vec<FakeEmbedReply>>,
    inputs: Mutex<Vec<String>>,
    calls: AtomicUsize,
    /// When set, any batch beyond the script is answered with stable vectors of this
    /// length instead of running out.
    deterministic_dimensions: Option<usize>,
    model: String,
}

impl std::fmt::Debug for FakeEmbeddings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeEmbeddings")
            .field("model", &self.model)
            .field("deterministic_dimensions", &self.deterministic_dimensions)
            .finish_non_exhaustive()
    }
}

impl FakeEmbeddings {
    /// Replies are consumed in order; running out is itself a failure, so a test that
    /// expects two batches and gets three finds out.
    pub fn new(replies: Vec<FakeEmbedReply>) -> Self {
        Self {
            replies: Mutex::new(replies.into_iter().rev().collect()),
            inputs: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            deterministic_dimensions: None,
            model: "fake/embedding-1".to_owned(),
        }
    }

    /// Answer every batch with stable vectors of `dimensions` coordinates.
    ///
    /// Same text, same vector, for as long as the test runs — enough to assert that a
    /// query found the row it should have, and nothing more than that.
    pub fn deterministic(dimensions: usize) -> Self {
        Self {
            deterministic_dimensions: Some(dimensions.max(1)),
            ..Self::new(Vec::new())
        }
    }

    pub fn failing(error: EmbedError) -> Self {
        Self::new(vec![FakeEmbedReply::Fail(error)])
    }

    /// Every text this adapter was asked to embed, in order — so a test can assert what
    /// was and was not sent for embedding.
    pub fn inputs(&self) -> Vec<String> {
        self.inputs.lock().expect("fake embeddings lock").clone()
    }

    /// Batches, not texts: the pacing and batching logic is what this counts.
    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn next_reply(&self, expected: usize) -> Option<FakeEmbedReply> {
        if let Some(reply) = self.replies.lock().expect("fake embeddings lock").pop() {
            return Some(reply);
        }
        self.deterministic_dimensions.map(|dimensions| {
            let inputs = self.inputs.lock().expect("fake embeddings lock");
            let batch = &inputs[inputs.len() - expected..];
            FakeEmbedReply::Vectors(
                batch
                    .iter()
                    .map(|input| deterministic_vector(input, dimensions))
                    .collect(),
            )
        })
    }
}

#[async_trait]
impl EmbeddingProvider for FakeEmbeddings {
    fn describe(&self) -> EmbeddingDescription {
        EmbeddingDescription {
            provider: "fake".to_owned(),
            model: self.model.clone(),
            endpoint_host: None,
            state: "ready",
            missing: Vec::new(),
            // Prefixed with the provider, like every other profile, so vectors built by a
            // test can never be compared with vectors from a real service.
            profile: Some(format!("fake:{}", self.model)),
            message: "Тестовый адаптер эмбеддингов: сетевые вызовы не выполняются.".to_owned(),
        }
    }

    async fn embed(&self, request: &EmbedRequest) -> EmbedResult<EmbedResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inputs
            .lock()
            .expect("fake embeddings lock")
            .extend(request.inputs.iter().cloned());

        let expected = request.inputs.len();
        let reply = self.next_reply(expected).ok_or_else(|| {
            EmbedError::InvalidResponse("тестовый адаптер: ответы закончились".to_owned())
        })?;

        let vectors = match reply {
            FakeEmbedReply::Fail(error) => return Err(error),
            FakeEmbedReply::Vectors(vectors) => vectors,
        };

        // The same two refusals the real client makes, so no test can rely on a shape the
        // service could never produce.
        if vectors.len() != expected {
            return Err(EmbedError::CountMismatch {
                expected,
                got: vectors.len(),
            });
        }
        let dimensions = vectors.first().map_or(0, Vec::len);
        if let Some(other) = vectors.iter().find(|vector| vector.len() != dimensions) {
            return Err(EmbedError::DimensionMismatch {
                expected: dimensions,
                got: other.len(),
            });
        }

        Ok(EmbedResponse {
            vectors,
            model: self.model.clone(),
            dimensions,
            duration: Duration::from_millis(1),
        })
    }
}

/// A stable, meaningless vector for a text.
///
/// FNV-1a over the text and the coordinate number, mapped into `[-1, 1]`. Deterministic
/// across runs and platforms, which is all a test needs; semantically worthless, which is
/// why nothing outside the `fake` feature may call it.
fn deterministic_vector(text: &str, dimensions: usize) -> Vec<f32> {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    (0..dimensions)
        .map(|position| {
            let mut hash = OFFSET;
            for byte in text
                .as_bytes()
                .iter()
                .copied()
                .chain((position as u64).to_le_bytes())
            {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(PRIME);
            }
            // Top 24 bits, which f32 represents exactly, mapped onto [-1, 1).
            const HALF_RANGE: f32 = 8_388_608.0; // 2^23
            (hash >> 40) as f32 / HALF_RANGE - 1.0
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(inputs: &[&str]) -> EmbedRequest {
        EmbedRequest {
            purpose: "test",
            inputs: inputs.iter().map(|input| (*input).to_owned()).collect(),
        }
    }

    #[tokio::test]
    async fn replies_are_consumed_in_order_and_inputs_are_recorded() {
        let provider = FakeEmbeddings::new(vec![
            FakeEmbedReply::Vectors(vec![vec![0.1, 0.2]]),
            FakeEmbedReply::Fail(EmbedError::RateLimited),
        ]);

        let first = provider.embed(&request(&["длина"])).await.unwrap();
        assert_eq!(first.dimensions, 2);
        assert_eq!(first.vectors.len(), 1);

        let second = provider.embed(&request(&["ширина"])).await.unwrap_err();
        assert!(matches!(second, EmbedError::RateLimited));

        assert_eq!(
            provider.inputs(),
            vec!["длина".to_owned(), "ширина".to_owned()]
        );
        assert_eq!(provider.call_count(), 2);

        // Running out of script is a loud failure, not an empty answer.
        assert!(provider.embed(&request(&["высота"])).await.is_err());
    }

    #[tokio::test]
    async fn nothing_can_mistake_the_fake_for_a_real_vector_space() {
        let provider = FakeEmbeddings::deterministic(4);
        let description = provider.describe();
        assert_eq!(description.provider, "fake");
        assert_eq!(
            description.profile.as_deref(),
            Some("fake:fake/embedding-1")
        );
        assert!(description.endpoint_host.is_none());
    }

    #[tokio::test]
    async fn deterministic_vectors_are_stable_and_of_the_requested_length() {
        let provider = FakeEmbeddings::deterministic(8);
        let first = provider
            .embed(&request(&["длина", "ширина"]))
            .await
            .unwrap();
        let second = provider.embed(&request(&["длина"])).await.unwrap();

        assert_eq!(first.dimensions, 8);
        assert_eq!(first.vectors.len(), 2);
        assert_eq!(second.vectors[0], first.vectors[0]);
        assert_ne!(first.vectors[0], first.vectors[1]);
        assert!(first.vectors[0].iter().all(|value| value.is_finite()));
        assert_eq!(provider.call_count(), 2);
        assert_eq!(provider.inputs().len(), 3);
    }

    #[tokio::test]
    async fn a_scripted_batch_of_the_wrong_size_is_refused_exactly_as_the_real_client_refuses_it() {
        let provider = FakeEmbeddings::new(vec![FakeEmbedReply::Vectors(vec![vec![0.1]])]);
        let error = provider
            .embed(&request(&["длина", "ширина"]))
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                EmbedError::CountMismatch {
                    expected: 2,
                    got: 1
                }
            ),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn scripted_vectors_of_ragged_length_are_refused() {
        let provider = FakeEmbeddings::new(vec![FakeEmbedReply::Vectors(vec![
            vec![0.1, 0.2],
            vec![0.3],
        ])]);
        let error = provider
            .embed(&request(&["длина", "ширина"]))
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                EmbedError::DimensionMismatch {
                    expected: 2,
                    got: 1
                }
            ),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_failing_fake_reports_the_error_it_was_given() {
        let provider = FakeEmbeddings::failing(EmbedError::Timeout);
        let error = provider.embed(&request(&["длина"])).await.unwrap_err();
        assert!(matches!(error, EmbedError::Timeout));
        assert!(error.is_retryable());
    }
}
