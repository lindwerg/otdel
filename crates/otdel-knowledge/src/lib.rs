//! Phase 1C — the product role, as a pure pipeline.
//!
//! ```text
//! pages of one material ─→ SourceCatalog (labels S1…Sn, quote index)
//!                            │
//!                            ├─ plan_batches ─→ bounded prompts ─→ LlmProvider
//!                            │                                        │
//!                            └────────── validate_response ←──────────┘
//!                                          │
//!                                   CandidateDraft (+ counted refusals)
//! ```
//!
//! Nothing in this crate touches a database, a file or an HTTP client: it takes pages
//! and a provider, and returns candidates. That is what makes the rules testable —
//! "a fact whose quote is not on the page is refused" is a unit test here, not an
//! integration test somewhere else.
//!
//! The storage and queueing half lives in `otdel-db`/`otdel-worker`; the model adapter
//! in `otdel-llm`. With no key configured, the provider passed in is the unconfigured
//! one, [`draft_knowledge`] returns [`KnowledgeError::ProviderNotConfigured`], and the
//! caller records the run as `needs_provider` — storing nothing.

pub mod candidate;
/// R05 — the page account, and the requirement check that stands before publication.
pub mod coverage;
/// How a written quantity is read: numbers, units, and what may be compared with what.
pub mod measure;
pub mod prompt;
pub mod quote;
pub mod schema;
pub mod source;
/// R05 — reading a table as a table: structured context in, uncertainties out.
pub mod tables;
pub mod validate;

use otdel_core::llm_config::LlmLimits;
use otdel_llm::{LlmError, LlmProvider, LlmRequest};
use tracing::{debug, warn};

pub use candidate::{
    CandidateAlias, CandidateApplication, CandidateApplicationDetail, CandidateCategory,
    CandidateDeclaration, CandidateDraft, CandidateFact, CandidateGap, CandidateProduct,
    CandidateQa, CandidateQuestion, CandidateSense, CandidateSynonym, CandidateTerm,
    ResolvedEvidence,
};
pub use coverage::{
    evaluate_requirements, CoveragePlan, DraftSnapshot, PlannedPage, ProcessedPage, Requirement,
    RequirementsOutcome, REQUIREMENTS, TECHNICAL_REQUIREMENT,
};
pub use prompt::{PromptContext, TableContext};
pub use schema::{DraftLimits, DraftResponse, PROMPT_PROFILE, SCHEMA_NAME};
pub use source::{SourceCatalog, SourcePage};
pub use tables::{CellUncertainty, StructuralConfirmation, StructuredCell, TableReading};

/// Why a whole run could not be produced.
///
/// Refused *candidates* are not errors — they are counted inside
/// [`CandidateDraft`]. These are the cases where there is no draft at all.
#[derive(Debug, thiserror::Error)]
pub enum KnowledgeError {
    /// No key, no model, or the role is switched off. Nothing was called.
    #[error("{0}")]
    ProviderNotConfigured(String),

    /// The provider was called and failed. `retryable` decides whether the queue
    /// should try again or stop with the reason.
    #[error("{diagnostic}")]
    ProviderFailed { diagnostic: String, retryable: bool },

    /// The material has no page with usable text: there is nothing to understand yet.
    #[error("в материале нет ни одной прочитанной страницы с текстом")]
    NoReadableSources,
}

/// What one run did, beyond the candidates themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftOutcome {
    pub draft: CandidateDraft,
    pub requests_made: u32,
    /// Characters of source text actually sent.
    pub input_chars: u32,
    pub pages_considered: u32,
    /// Pages left out because the run's request budget was reached.
    pub pages_skipped: u32,
    /// R05 — every page whose request came back, with which request carried it and how
    /// much of it was sent. This is what [`CoveragePlan::settle`] needs, and it is
    /// recorded per request rather than counted at the end: a run that made three calls
    /// and lost one must not be able to describe the pages of the lost call as processed.
    pub processed: Vec<ProcessedPage>,
    /// R05 — the pages the request budget stopped, by identity rather than by count, so
    /// the next pass can be given exactly them.
    pub deferred: Vec<uuid::Uuid>,
    pub provider: String,
    pub model: Option<String>,
}

/// Run the product role over one material's pages.
///
/// `tables` carries the server's reading of R03's table cells for the pages of this run.
/// Passing [`prompt::TableContext::default`] is legitimate and means the material has no
/// established table structure — it is not a way to opt out of anything, because the
/// uncertainties the same reading produced are stored by the caller either way.
pub async fn draft_knowledge(
    provider: &dyn LlmProvider,
    catalog: &SourceCatalog,
    context: &PromptContext,
    tables: &prompt::TableContext,
    limits: &LlmLimits,
    draft_limits: &DraftLimits,
) -> Result<DraftOutcome, KnowledgeError> {
    if catalog.is_empty() {
        return Err(KnowledgeError::NoReadableSources);
    }

    let description = provider.describe();
    if !description.is_ready() {
        return Err(KnowledgeError::ProviderNotConfigured(description.message));
    }

    let (batches, deferred_indices) = prompt::plan_batches(catalog, limits);
    let system_prompt = prompt::system_prompt();

    let deferred: Vec<uuid::Uuid> = deferred_indices
        .iter()
        .map(|position| catalog.entries()[*position].page.page_id)
        .collect();
    let pages_skipped = deferred.len();

    let mut outcome = DraftOutcome {
        draft: CandidateDraft::default(),
        requests_made: 0,
        input_chars: 0,
        pages_considered: 0,
        pages_skipped: u32::try_from(pages_skipped).unwrap_or(u32::MAX),
        processed: Vec::new(),
        deferred,
        provider: description.provider.clone(),
        model: None,
    };

    if pages_skipped > 0 {
        outcome.draft.note(format!(
            "страниц не вошло в разбор из-за лимита запросов: {pages_skipped}"
        ));
    }

    for (index, batch) in batches.iter().enumerate() {
        let entries: Vec<&source::CatalogEntry> = batch
            .entry_indices
            .iter()
            .map(|position| &catalog.entries()[*position])
            .collect();

        let request = LlmRequest {
            purpose: "knowledge_draft",
            system_prompt: system_prompt.clone(),
            user_prompt: prompt::user_prompt(context, &entries, tables, limits),
            schema_name: SCHEMA_NAME,
            schema: schema::response_schema(),
            max_output_tokens: limits.max_output_tokens,
        };
        let input_chars = request.input_chars();

        let response = match provider.complete_json(&request).await {
            Ok(response) => response,
            Err(LlmError::NotConfigured(message)) => {
                return Err(KnowledgeError::ProviderNotConfigured(message))
            }
            Err(error) => {
                // The whole run fails rather than storing half a material's knowledge:
                // persistence replaces a material's candidates as one set, and a
                // partial set would look like a complete one.
                warn!(
                    batch = index,
                    retryable = error.is_retryable(),
                    error = %error,
                    "model call failed during a knowledge run"
                );
                return Err(KnowledgeError::ProviderFailed {
                    diagnostic: error.diagnostic(),
                    retryable: error.is_retryable(),
                });
            }
        };

        outcome.requests_made += 1;
        outcome.input_chars = outcome
            .input_chars
            .saturating_add(u32::try_from(input_chars).unwrap_or(u32::MAX));
        outcome.pages_considered += u32::try_from(entries.len()).unwrap_or(0);
        outcome.model = Some(response.model.clone());

        // The answer came back, so every page of this request was genuinely put in front
        // of the model — including when the answer turns out not to match the schema
        // below. That refusal is counted as a refusal; calling the pages unread would be
        // a second, wrong story about the same event.
        let batch_index = i32::try_from(index + 1).unwrap_or(i32::MAX);
        let page_budget = prompt::per_page_budget(limits);
        outcome
            .processed
            .extend(entries.iter().map(|entry| {
                ProcessedPage {
                    page_id: entry.page.page_id,
                    batch_index,
                    chars_sent: i32::try_from(entry.page.text.chars().count().min(page_budget))
                        .unwrap_or(i32::MAX),
                }
            }));

        match DraftResponse::parse(&response.json) {
            Ok(parsed) => {
                let prefix = format!("b{}", index + 1);
                let validated =
                    validate::validate_response(&parsed, catalog, draft_limits, &prefix);
                debug!(
                    batch = index,
                    facts = validated.facts.len(),
                    rejected = validated.rejected,
                    "validated one response"
                );
                outcome.draft.merge(validated);
            }
            // A response that does not match the schema is a refusal with a reason,
            // not a crash: the other batches may still be usable and the run says what
            // happened.
            Err(reason) => outcome.draft.reject(reason),
        }
    }

    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use otdel_core::extraction::{PageStatus, TextSource};
    use otdel_llm::fake::{FakeProvider, FakeReply};
    use otdel_llm::UnconfiguredProvider;
    use serde_json::json;
    use uuid::Uuid;

    fn page(number: i32, text: &str) -> SourcePage {
        SourcePage {
            page_id: Uuid::from_u128(u128::try_from(number).unwrap()),
            material_id: Uuid::from_u128(900),
            material_filename: "catalogue.pdf".to_owned(),
            page_number: number,
            status: PageStatus::Extracted,
            text_source: TextSource::TextLayer,
            text: text.to_owned(),
        }
    }

    fn context() -> PromptContext {
        PromptContext {
            partner_name: "BASIS".to_owned(),
            material_filename: "catalogue.pdf".to_owned(),
            pages_with_text: 2,
        }
    }

    fn answer_with_value(source: &str, quote: &str, value: &str) -> serde_json::Value {
        json!({
            "categories": [],
            "products": [{
                "ref": "p1", "category_ref": null, "kind": "product",
                "name": "BP21", "summary": null,
            }],
            "facts": [{
                "product_ref": "p1", "kind": "characteristic", "attribute": "нагрузка",
                "value": value, "unit": null, "conditions": null, "model_context": null,
                "evidence": [{"source": source, "quote": quote}],
            }],
            "glossary": [],
            "qa": [],
            "gaps": [],
        })
    }

    /// The common case: a value that really is in the quoted fragment.
    fn answer(source: &str, quote: &str) -> serde_json::Value {
        answer_with_value(source, quote, "3.5")
    }

    #[tokio::test]
    async fn a_run_without_a_configured_provider_calls_nothing_and_says_why() {
        let settings = otdel_core::llm_config::LlmSettings::default();
        let provider = UnconfiguredProvider::new(&settings);
        let catalog = SourceCatalog::build(vec![page(1, "BP21 1200 3.5 kN")]);

        let error = draft_knowledge(
            &provider,
            &catalog,
            &context(),
            &TableContext::default(),
            &LlmLimits::default(),
            &DraftLimits::default(),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, KnowledgeError::ProviderNotConfigured(_)));
        assert!(error.to_string().contains("OTDEL_LLM_API_KEY"));
    }

    #[tokio::test]
    async fn a_material_with_no_readable_page_is_not_sent_anywhere() {
        let provider = FakeProvider::answering(json!({}));
        let error = draft_knowledge(
            &provider,
            &SourceCatalog::default(),
            &context(),
            &TableContext::default(),
            &LlmLimits::default(),
            &DraftLimits::default(),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, KnowledgeError::NoReadableSources));
        assert_eq!(provider.call_count(), 0);
    }

    #[tokio::test]
    async fn a_two_page_material_is_drafted_in_bounded_requests_and_merged() {
        let catalog = SourceCatalog::build(vec![
            page(1, "BP21 1200 3.5 kN профиль монтажный"),
            page(2, "BP21 поставляется с крепежом"),
        ]);
        let provider = FakeProvider::new(vec![
            FakeReply::Json(answer("S1", "BP21 1200 3.5 kN")),
            // A second, differently-sourced statement about the same product.
            FakeReply::Json(answer_with_value(
                "S2",
                "BP21 поставляется с крепежом",
                "с крепежом",
            )),
        ]);
        let limits = LlmLimits {
            max_pages_per_request: 1,
            ..LlmLimits::default()
        };

        let outcome = draft_knowledge(
            &provider,
            &catalog,
            &context(),
            &TableContext::default(),
            &limits,
            &DraftLimits::default(),
        )
        .await
        .unwrap();

        assert_eq!(outcome.requests_made, 2);
        assert_eq!(outcome.pages_considered, 2);
        assert_eq!(outcome.pages_skipped, 0);
        assert!(outcome.input_chars > 0);
        assert_eq!(outcome.model.as_deref(), Some("fake/model-1"));
        // The same product named in both responses is one product…
        assert_eq!(outcome.draft.products.len(), 1);
        // …with both statements attached to it, each citing its own page.
        assert_eq!(outcome.draft.facts.len(), 2);
        let pages: Vec<i32> = outcome
            .draft
            .facts
            .iter()
            .map(|fact| fact.evidence[0].page_number)
            .collect();
        assert_eq!(pages, vec![1, 2]);
        assert!(outcome
            .draft
            .facts
            .iter()
            .all(|fact| fact.product_ref.as_deref() == Some("b1:p:p1")));
    }

    #[tokio::test]
    async fn a_response_citing_a_page_of_another_request_still_has_to_be_in_the_run() {
        // S2 exists in the catalogue but was not shown in this batch. It is still a
        // page of this material, so quoting it correctly is legitimate — what must
        // fail is a label that is not in the catalogue at all.
        let catalog = SourceCatalog::build(vec![
            page(1, "BP21 1200 3.5 kN профиль монтажный"),
            page(2, "BP21 поставляется с крепежом"),
        ]);
        let provider = FakeProvider::new(vec![FakeReply::Json(answer("S404", "BP21 1200 3.5 kN"))]);

        let outcome = draft_knowledge(
            &provider,
            &catalog,
            &context(),
            &TableContext::default(),
            &LlmLimits::default(),
            &DraftLimits::default(),
        )
        .await
        .unwrap();

        assert!(outcome.draft.facts.is_empty());
        assert_eq!(outcome.draft.rejected, 1);
    }

    #[tokio::test]
    async fn a_transient_provider_failure_fails_the_whole_run() {
        let catalog = SourceCatalog::build(vec![page(1, "BP21 1200 3.5 kN")]);
        let provider = FakeProvider::failing(LlmError::RateLimited);

        let error = draft_knowledge(
            &provider,
            &catalog,
            &context(),
            &TableContext::default(),
            &LlmLimits::default(),
            &DraftLimits::default(),
        )
        .await
        .unwrap_err();

        match error {
            KnowledgeError::ProviderFailed {
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
        let catalog = SourceCatalog::build(vec![page(1, "BP21 1200 3.5 kN")]);
        let provider = FakeProvider::new(vec![FakeReply::Json(
            json!({"answer": "вот характеристики"}),
        )]);

        let outcome = draft_knowledge(
            &provider,
            &catalog,
            &context(),
            &TableContext::default(),
            &LlmLimits::default(),
            &DraftLimits::default(),
        )
        .await
        .unwrap();

        assert!(outcome.draft.is_empty());
        assert_eq!(outcome.draft.rejected, 1);
        assert!(outcome.draft.rejections[0].contains("схеме"));
    }

    #[tokio::test]
    async fn a_material_larger_than_the_budget_reports_the_pages_it_did_not_read() {
        let catalog = SourceCatalog::build(
            (1..=6)
                .map(|number| page(number, "BP21 1200 3.5 kN профиль монтажный"))
                .collect(),
        );
        let provider = FakeProvider::new(vec![FakeReply::Json(answer("S1", "BP21 1200 3.5 kN"))]);
        let limits = LlmLimits {
            max_pages_per_request: 1,
            max_requests_per_run: 1,
            ..LlmLimits::default()
        };

        let outcome = draft_knowledge(
            &provider,
            &catalog,
            &context(),
            &TableContext::default(),
            &limits,
            &DraftLimits::default(),
        )
        .await
        .unwrap();

        assert_eq!(outcome.requests_made, 1);
        assert_eq!(outcome.pages_skipped, 5);
        assert!(outcome
            .draft
            .rejections
            .iter()
            .any(|reason| reason.contains("не вошло в разбор")));

        // R05: the five are named, not counted. This is the account the audited run
        // could not produce — and it is what lets the next pass resume exactly here.
        assert_eq!(outcome.processed.len(), 1);
        assert_eq!(outcome.processed[0].batch_index, 1);
        assert!(outcome.processed[0].chars_sent > 0);
        let deferred: Vec<Uuid> = catalog.entries()[1..]
            .iter()
            .map(|entry| entry.page.page_id)
            .collect();
        assert_eq!(outcome.deferred, deferred);
        // No page is in both accounts: a page is either read or waiting, never both.
        assert!(!outcome
            .processed
            .iter()
            .any(|done| outcome.deferred.contains(&done.page_id)));
    }
}
