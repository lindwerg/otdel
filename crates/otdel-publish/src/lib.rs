//! Phase 1E — the checker, the immutable version, and answering from it.
//!
//! This crate contains no I/O. It is handed candidates together with the source text
//! their citations point into, and it returns verdicts, a readiness matrix, a
//! publication decision and — when a model is configured — a grounded answer. That is
//! what makes every rule in it testable with a string literal, and it is why the
//! database layer can stay a translation rather than a second place where rules live.
//!
//! ```text
//! candidates of 1C and 1D  +  the source text as it is stored now
//!    │
//!    ▼
//! check_claims             every citation re-located, every value re-checked
//!    │                     → source_supported | hypothesis | unknown | conflicted | stale
//!    ├─ (optional) review  a model may lower a verdict, never raise one
//!    ▼
//! readiness::assess        four topics, decided separately
//!    │
//!    ▼
//! version::decide          publish | blocked (with reasons) | unchanged
//!    │
//!    ▼
//! chunk_text               the searchable rendering, one per claim
//! ```
//!
//! **What needs no configuration:** everything above except the review. Verification is
//! deterministic, publication is automatic, and search works on exact values and full
//! text. `block-01-spec.md` §6.7 forbids treating two models agreeing as proof, so this
//! phase never depended on a model for a verdict in the first place.
//!
//! **What a model adds, and cannot do:** a second opinion that may only lower a verdict
//! ([`review`]), and prose over claims the server selected ([`answer`]). Neither can
//! create a claim, raise a verdict or invent a citation.

pub mod answer;
pub mod check;
pub mod chunk;
pub mod claim;
/// Phase 1F: what changed between two published snapshots.
pub mod diff;
pub mod prompt;
pub mod readiness;
pub mod review;
pub mod version;

use std::sync::Arc;

use otdel_core::retrieval_config::RetrievalLimits;
use otdel_llm::{LlmError, LlmProvider, LlmRequest};
use tracing::warn;

pub use check::check_claims;
pub use claim::{
    CandidateClaim, CandidateEvidence, CheckOutcome, CheckedClaim, CheckedEvidence,
    MAX_REJECTION_LINES,
};
pub use readiness::{assess, gap_blocks, GapText};
pub use review::PROMPT_PROFILE;
pub use version::{candidate_fingerprint, decide, fingerprint, PublicationDecision};

/// Purpose label of a review call. Appears in logs; never the content.
pub const REVIEW_PURPOSE: &str = "claim_review";
/// Purpose label of an answering call.
pub const ANSWER_PURPOSE: &str = "grounded_answer";

/// How many claims one review request carries.
///
/// Bounded so a partner with hundreds of claims cannot turn one check into an unbounded
/// spend. Claims that do not fit are simply not reviewed — which lowers nothing, because
/// a review can only lower.
const REVIEW_BATCH: usize = 8;

/// Ask a model for a second opinion on the supported claims, and apply what it lowers.
///
/// Returns how many claims were reviewed. Every failure here is **non-fatal by design**:
/// the deterministic verdicts stand, the version publishes, and the reason is recorded.
/// A checker that refused to publish because an optional reviewer timed out would make
/// an optional dependency a required one.
pub async fn review_claims(
    provider: &Arc<dyn LlmProvider>,
    claims: &mut [CheckedClaim],
    limits: &RetrievalLimits,
    max_requests: u32,
    notes: &mut Vec<String>,
) -> u32 {
    if !provider.describe().is_ready() {
        return 0;
    }

    let candidates = review::reviewable(claims);
    if candidates.is_empty() {
        return 0;
    }

    let mut reviewed = 0u32;

    for (requests, batch) in candidates.chunks(REVIEW_BATCH).enumerate() {
        if u32::try_from(requests).unwrap_or(u32::MAX) >= max_requests {
            notes.push(format!(
                "проверка моделью остановлена на лимите {max_requests} запросов: остальные \
                 утверждения сохранили статус, полученный детерминированной проверкой"
            ));
            break;
        }
        let (user_prompt, labels) = review::user_prompt(claims, batch, limits);
        let request = LlmRequest {
            purpose: REVIEW_PURPOSE,
            system_prompt: review::system_prompt(),
            user_prompt,
            schema_name: review::SCHEMA_NAME,
            schema: review::response_schema(),
            max_output_tokens: 2_000,
        };

        match provider.complete_json(&request).await {
            Ok(response) => match review::ReviewResponse::parse(&response.json) {
                Ok(parsed) => {
                    let outcome = review::apply(claims, batch, &labels, &parsed);
                    reviewed = reviewed.saturating_add(outcome.reviewed);
                    notes.extend(outcome.notes);
                }
                Err(reason) => {
                    notes.push(format!("второе мнение модели не принято: {reason}"));
                }
            },
            Err(LlmError::NotConfigured(message)) => {
                notes.push(format!("второе мнение модели не запрашивалось: {message}"));
                break;
            }
            Err(error) => {
                // Recorded, not fatal. The deterministic verdicts are the verdicts.
                warn!(error = %error, "claim review call failed; deterministic verdicts stand");
                notes.push(format!(
                    "второе мнение модели не получено: {}. Статусы получены \
                     детерминированной проверкой",
                    error.diagnostic()
                ));
                break;
            }
        }
    }

    reviewed
}

/// Ask a model to answer from claims the server chose, and validate what comes back.
///
/// `claims` is the bounded context and nothing else reaches the model. The returned
/// [`answer::ValidatedAnswer`] carries indices into that same slice, so the caller turns
/// them back into citations from its own rows — the model never supplies one.
pub async fn compose_answer(
    provider: &Arc<dyn LlmProvider>,
    question: &str,
    claims: &[CheckedClaim],
    limits: &RetrievalLimits,
) -> Result<answer::ValidatedAnswer, String> {
    let (user_prompt, labels) = answer::user_prompt(question, claims, limits);
    let request = LlmRequest {
        purpose: ANSWER_PURPOSE,
        system_prompt: answer::system_prompt(),
        user_prompt,
        schema_name: answer::SCHEMA_NAME,
        schema: answer::response_schema(),
        max_output_tokens: 2_000,
    };

    let response = provider
        .complete_json(&request)
        .await
        .map_err(|error| error.diagnostic())?;

    let parsed = answer::AnswerResponse::parse(&response.json)?;
    Ok(answer::validate(&parsed, &labels, limits))
}
