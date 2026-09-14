//! Phase 1D — bounded industry research, as a pure pipeline.
//!
//! ```text
//! one approved 1C question
//!        │
//!        ├─ plan_queries ──→ 1..N strings that do NOT name the partner
//!        │                        │
//!        │                   (the worker searches, pays, fetches, stores)
//!        │                        │
//!        ▼                        ▼
//! ExternalCatalog (labels E1…En, quote index over the stored snapshots)
//!        │
//!        ├─ plan_batches ──→ bounded prompts ──→ LlmProvider
//!        │                                          │
//!        └────────── validate_response ←────────────┘
//!                        │
//!                 FindingsDraft (+ counted refusals)
//! ```
//!
//! Nothing in this crate touches a database, a socket or a budget row: it takes a
//! question, some downloaded text and a model, and returns candidates. That is what makes
//! the rules testable — "a finding whose quote is not on the page is refused" and "a
//! query naming the partner is never sent" are unit tests here, not integration tests
//! somewhere else.
//!
//! The parts that *do* have consequences live where they can be seen: the network in
//! `otdel-search`, the money and the journal in `otdel-db`, the loop that ties them
//! together in `otdel-worker`.
//!
//! Two adapters have to be ready before any of this runs: a search endpoint (with its
//! host allowlist) and the model. With either missing, the worker records the plan as
//! `needs_provider` — **no request, no reservation, nothing stored.**

pub mod budget;
pub mod candidate;
pub mod catalog;
pub mod prompt;
pub mod query;
pub mod schema;
pub mod validate;

use otdel_llm::{LlmError, LlmProvider, LlmRequest};
use tracing::{debug, warn};

pub use budget::{format_micros, Allowance, CostModel};
pub use candidate::{CandidateFinding, FindingsDraft, ResolvedExternalEvidence};
pub use catalog::{CatalogEntry, ExternalCatalog, ExternalSource};
pub use prompt::ResearchContext;
pub use query::{names_partner, plan_queries, QueryRefusal};
pub use schema::{FindingLimits, FindingsResponse, PROMPT_PROFILE, SCHEMA_NAME};

/// Why a whole interpretation pass could not be produced.
///
/// Refused *findings* are not errors — they are counted inside [`FindingsDraft`]. These
/// are the cases where there is no draft at all.
#[derive(Debug, thiserror::Error)]
pub enum ResearchError {
    /// No model configured. Nothing was called.
    #[error("{0}")]
    ProviderNotConfigured(String),

    /// The model was called and failed. `retryable` decides whether the queue should try
    /// again or stop with the reason.
    #[error("{diagnostic}")]
    ProviderFailed { diagnostic: String, retryable: bool },

    /// Not one source was readable: there is nothing to interpret.
    #[error("ни один источник не прочитан: интерпретировать нечего")]
    NoReadableSources,
}

/// What one interpretation pass did, beyond the findings themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterpretOutcome {
    pub draft: FindingsDraft,
    pub requests_made: u32,
    /// Characters of source text actually sent to the model.
    pub input_chars: u32,
    pub sources_considered: u32,
    /// Sources left out because the request budget was reached.
    pub sources_skipped: u32,
    pub model: Option<String>,
}

/// How many model requests [`interpret_sources`] will make for this catalogue.
///
/// Exact, not a worst case: it runs the same batching the interpretation itself runs, on
/// the same inputs. The caller reserves money for this many calls, and a reservation that
/// guessed `max_requests` instead would refuse work that fits comfortably — a plan with
/// one source would have to afford eight calls to make one.
pub fn planned_requests(
    catalog: &ExternalCatalog,
    limits: &FindingLimits,
    max_requests: u32,
) -> u32 {
    if catalog.is_empty() {
        return 0;
    }
    let per_source_chars: Vec<usize> = catalog
        .entries()
        .iter()
        .map(|entry| entry.source.text.chars().count())
        .collect();
    let (batches, _) = prompt::plan_batches(
        catalog.len(),
        &per_source_chars,
        limits,
        max_requests.max(1) as usize,
    );
    u32::try_from(batches.len()).unwrap_or(max_requests)
}

/// Read the downloaded sources and propose findings, bounded by `max_requests`.
pub async fn interpret_sources(
    provider: &dyn LlmProvider,
    catalog: &ExternalCatalog,
    context: &ResearchContext,
    limits: &FindingLimits,
    max_requests: u32,
    max_output_tokens: u32,
) -> Result<InterpretOutcome, ResearchError> {
    if catalog.is_empty() {
        return Err(ResearchError::NoReadableSources);
    }

    let description = provider.describe();
    if !description.is_ready() {
        return Err(ResearchError::ProviderNotConfigured(description.message));
    }

    let per_source_chars: Vec<usize> = catalog
        .entries()
        .iter()
        .map(|entry| entry.source.text.chars().count())
        .collect();
    let (batches, sources_skipped) = prompt::plan_batches(
        catalog.len(),
        &per_source_chars,
        limits,
        max_requests.max(1) as usize,
    );
    let system_prompt = prompt::system_prompt();

    let mut outcome = InterpretOutcome {
        draft: FindingsDraft::default(),
        requests_made: 0,
        input_chars: 0,
        sources_considered: 0,
        sources_skipped: u32::try_from(sources_skipped).unwrap_or(u32::MAX),
        model: None,
    };

    if sources_skipped > 0 {
        outcome.draft.note(format!(
            "источников не вошло в разбор из-за лимита запросов к модели: {sources_skipped}"
        ));
    }

    for (index, batch) in batches.iter().enumerate() {
        let entries: Vec<&CatalogEntry> = batch
            .entry_indices
            .iter()
            .map(|position| &catalog.entries()[*position])
            .collect();

        let request = LlmRequest {
            purpose: "industry_research",
            system_prompt: system_prompt.clone(),
            user_prompt: prompt::user_prompt(context, &entries, limits),
            schema_name: SCHEMA_NAME,
            schema: schema::response_schema(),
            max_output_tokens,
        };
        let input_chars = request.input_chars();

        let response = match provider.complete_json(&request).await {
            Ok(response) => response,
            Err(LlmError::NotConfigured(message)) => {
                return Err(ResearchError::ProviderNotConfigured(message))
            }
            Err(error) => {
                // The whole pass fails rather than storing half a plan's conclusions:
                // persistence replaces a plan's findings as one set, and a partial set
                // would look like a complete one.
                warn!(
                    batch = index,
                    retryable = error.is_retryable(),
                    error = %error,
                    "model call failed while interpreting external sources"
                );
                return Err(ResearchError::ProviderFailed {
                    diagnostic: error.diagnostic(),
                    retryable: error.is_retryable(),
                });
            }
        };

        outcome.requests_made += 1;
        outcome.input_chars = outcome
            .input_chars
            .saturating_add(u32::try_from(input_chars).unwrap_or(u32::MAX));
        outcome.sources_considered += u32::try_from(entries.len()).unwrap_or(0);
        outcome.model = Some(response.model.clone());

        match FindingsResponse::parse(&response.json) {
            Ok(parsed) => {
                let validated = validate::validate_response(&parsed, catalog, limits);
                debug!(
                    batch = index,
                    findings = validated.findings.len(),
                    rejected = validated.rejected,
                    "validated one research response"
                );
                outcome.draft.merge(validated);
            }
            // A response that does not match the schema is a refusal with a reason, not a
            // crash: the other batches may still be usable and the plan says what happened.
            Err(reason) => outcome.draft.reject(reason),
        }
    }

    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use otdel_llm::fake::{FakeProvider, FakeReply};
    use otdel_llm::UnconfiguredProvider;
    use serde_json::json;
    use uuid::Uuid;

    fn source(id: u128, text: &str) -> ExternalSource {
        ExternalSource {
            source_id: Uuid::from_u128(id),
            url: format!("https://docs.example.org/page-{id}"),
            host: "docs.example.org".to_owned(),
            title: None,
            retrieved_at: None,
            content_hash: Some("a".repeat(64)),
            license: None,
            text: text.to_owned(),
        }
    }

    fn context() -> ResearchContext {
        ResearchContext {
            question: "Какая минимальная толщина цинкового покрытия?".to_owned(),
            topic: Some("покрытие".to_owned()),
            sources_total: 2,
        }
    }

    fn answer(label: &str, quote: &str, value: &str) -> serde_json::Value {
        json!({
            "findings": [{
                "topic": "покрытие",
                "attribute": "минимальная толщина цинкового покрытия",
                "value": value,
                "unit": null, "conditions": null, "model_context": null,
                "evidence": [{"source": label, "quote": quote}],
            }],
            "not_found": null,
        })
    }

    #[tokio::test]
    async fn a_pass_without_a_configured_model_calls_nothing_and_says_why() {
        let settings = otdel_core::llm_config::LlmSettings::default();
        let provider = UnconfiguredProvider::new(&settings);
        let catalog = ExternalCatalog::build(vec![source(1, "толщина покрытия 55 мкм")]);

        let error = interpret_sources(
            &provider,
            &catalog,
            &context(),
            &FindingLimits::default(),
            4,
            4_000,
        )
        .await
        .unwrap_err();

        assert!(matches!(error, ResearchError::ProviderNotConfigured(_)));
        assert!(error.to_string().contains("OTDEL_LLM_API_KEY"));
    }

    #[tokio::test]
    async fn a_plan_with_no_readable_source_is_not_sent_anywhere() {
        let provider = FakeProvider::answering(json!({}));
        let error = interpret_sources(
            &provider,
            &ExternalCatalog::default(),
            &context(),
            &FindingLimits::default(),
            4,
            4_000,
        )
        .await
        .unwrap_err();

        assert!(matches!(error, ResearchError::NoReadableSources));
        assert_eq!(provider.call_count(), 0);
    }

    #[tokio::test]
    async fn two_sources_are_interpreted_in_bounded_requests_and_merged() {
        let catalog = ExternalCatalog::build(vec![
            source(
                1,
                "Минимальная толщина цинкового покрытия 55 мкм по стандарту",
            ),
            source(
                2,
                "Для тонких изделий минимальная толщина 55 мкм также допускается",
            ),
        ]);
        let provider = FakeProvider::new(vec![
            FakeReply::Json(answer(
                "E1",
                "Минимальная толщина цинкового покрытия 55 мкм",
                "55",
            )),
            FakeReply::Json(answer(
                "E2",
                "Для тонких изделий минимальная толщина 55 мкм",
                "55",
            )),
        ]);
        let limits = FindingLimits {
            max_sources_per_request: 1,
            ..FindingLimits::default()
        };

        let outcome = interpret_sources(&provider, &catalog, &context(), &limits, 4, 4_000)
            .await
            .unwrap();

        assert_eq!(outcome.requests_made, 2);
        assert_eq!(outcome.sources_considered, 2);
        assert_eq!(outcome.sources_skipped, 0);
        assert!(outcome.input_chars > 0);
        assert_eq!(outcome.model.as_deref(), Some("fake/model-1"));
        // One statement, two independent external citations.
        assert_eq!(outcome.draft.findings.len(), 1);
        assert_eq!(outcome.draft.findings[0].evidence.len(), 2);
        let sources: Vec<Uuid> = outcome.draft.findings[0]
            .evidence
            .iter()
            .map(|evidence| evidence.source_id)
            .collect();
        assert_eq!(sources, vec![Uuid::from_u128(1), Uuid::from_u128(2)]);
    }

    #[tokio::test]
    async fn a_source_outside_the_plan_is_refused_rather_than_stored() {
        let catalog = ExternalCatalog::build(vec![source(
            1,
            "Минимальная толщина цинкового покрытия 55 мкм",
        )]);
        let provider = FakeProvider::new(vec![FakeReply::Json(answer(
            "E9",
            "Минимальная толщина цинкового покрытия 55 мкм",
            "55",
        ))]);

        let outcome = interpret_sources(
            &provider,
            &catalog,
            &context(),
            &FindingLimits::default(),
            4,
            4_000,
        )
        .await
        .unwrap();

        assert!(outcome.draft.findings.is_empty());
        assert_eq!(outcome.draft.rejected, 1);
    }

    #[tokio::test]
    async fn a_transient_model_failure_fails_the_whole_pass() {
        let catalog = ExternalCatalog::build(vec![source(1, "толщина покрытия 55 мкм")]);
        let provider = FakeProvider::failing(LlmError::RateLimited);

        let error = interpret_sources(
            &provider,
            &catalog,
            &context(),
            &FindingLimits::default(),
            4,
            4_000,
        )
        .await
        .unwrap_err();

        match error {
            ResearchError::ProviderFailed {
                retryable,
                diagnostic,
            } => {
                assert!(retryable);
                assert!(!diagnostic.is_empty());
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn prose_instead_of_the_schema_is_counted_as_a_refusal_not_a_crash() {
        let catalog = ExternalCatalog::build(vec![source(1, "толщина покрытия 55 мкм")]);
        let provider = FakeProvider::new(vec![FakeReply::Json(
            json!({"answer": "вот что удалось найти"}),
        )]);

        let outcome = interpret_sources(
            &provider,
            &catalog,
            &context(),
            &FindingLimits::default(),
            4,
            4_000,
        )
        .await
        .unwrap();

        assert!(outcome.draft.is_empty());
        assert_eq!(outcome.draft.rejected, 1);
        assert!(outcome.draft.rejections[0].contains("схеме"));
    }

    #[tokio::test]
    async fn more_sources_than_the_request_budget_are_reported_not_hidden() {
        let catalog = ExternalCatalog::build(
            (1..=6)
                .map(|index| source(index, "Минимальная толщина цинкового покрытия 55 мкм"))
                .collect(),
        );
        let provider = FakeProvider::new(vec![FakeReply::Json(answer(
            "E1",
            "Минимальная толщина цинкового покрытия 55 мкм",
            "55",
        ))]);
        let limits = FindingLimits {
            max_sources_per_request: 1,
            ..FindingLimits::default()
        };

        let outcome = interpret_sources(&provider, &catalog, &context(), &limits, 1, 4_000)
            .await
            .unwrap();

        assert_eq!(outcome.requests_made, 1);
        assert_eq!(outcome.sources_skipped, 5);
        assert!(outcome
            .draft
            .rejections
            .iter()
            .any(|reason| reason.contains("не вошло в разбор")));
    }

    #[tokio::test]
    async fn the_model_is_never_told_the_partner_or_the_url_of_a_source() {
        let catalog = ExternalCatalog::build(vec![source(
            1,
            "Минимальная толщина цинкового покрытия 55 мкм",
        )]);
        let provider = FakeProvider::new(vec![FakeReply::Json(answer(
            "E1",
            "Минимальная толщина цинкового покрытия 55 мкм",
            "55",
        ))]);

        interpret_sources(
            &provider,
            &catalog,
            &context(),
            &FindingLimits::default(),
            4,
            4_000,
        )
        .await
        .unwrap();

        let prompt = provider.prompts().join("\n");
        assert!(!prompt.contains("https://"), "no URL is shown to the model");
        assert!(!prompt.contains(&Uuid::from_u128(1).to_string()));
        assert!(prompt.contains("E1"));
        assert!(prompt.contains("docs.example.org"));
    }
}
