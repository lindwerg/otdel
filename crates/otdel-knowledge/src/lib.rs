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

use std::collections::VecDeque;

use otdel_core::llm_config::LlmLimits;
use otdel_llm::{LlmError, LlmProvider, LlmRequest};
use tracing::{debug, warn};

pub use candidate::{
    CandidateAlias, CandidateApplication, CandidateApplicationDetail, CandidateCategory,
    CandidateDeclaration, CandidateDraft, CandidateFact, CandidateGap, CandidateProduct,
    CandidateQa, CandidateQuestion, CandidateSense, CandidateSynonym, CandidateTerm, KnownProducts,
    ResolvedEvidence,
};
pub use coverage::{
    evaluate_requirements, CoveragePlan, DraftSnapshot, PlannedPage, ProcessedPage, Requirement,
    RequirementsOutcome, RunContext, REQUIREMENTS, TECHNICAL_REQUIREMENT,
};
pub use prompt::{PromptContext, TableContext};
pub use schema::{DraftLimits, DraftPurpose, DraftResponse, PROMPT_PROFILE, SCHEMA_NAME};
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
    ///
    /// The union across the passes: a page is deferred only when *no* pass reached it.
    pub deferred: Vec<uuid::Uuid>,
    /// R05.2 — what each purpose-specific pass covered, in the order they ran.
    ///
    /// The material-level account above is their union; this is the breakdown, and it is
    /// what makes "0 terms" answerable. A glossary pass that covered every page and found
    /// nothing is a finding; one that never ran is an unanswered question, and only this
    /// list can tell them apart.
    pub passes: Vec<PurposePass>,
    pub provider: String,
    pub model: Option<String>,
}

/// What one purpose-specific pass covered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurposePass {
    pub purpose: DraftPurpose,
    /// Requests this pass was allowed by the fair share, before it ran.
    pub requests_allowed: u32,
    pub requests_made: u32,
    pub input_chars: u32,
    /// Pages this pass put in front of the model.
    pub processed: Vec<ProcessedPage>,
    /// Pages this pass never reached, for this purpose.
    pub deferred: Vec<uuid::Uuid>,
    /// R05.3 — extra requests this pass spent recovering from a truncated answer.
    ///
    /// Counted apart from `requests_made` (which includes them) so the cost of the
    /// recovery is visible rather than blended into the pass's normal spend. A pass that
    /// needed six retries is telling the operator to lower this purpose's page budget.
    pub truncated_retries: u32,
}

impl PurposePass {
    /// Whether this pass saw the whole material.
    ///
    /// The question the requirement check asks. A pass that covered everything and
    /// produced nothing has established an absence; a pass that ran out of budget has
    /// established nothing at all, and the difference is the whole of R05.2.
    pub fn covered_everything(&self) -> bool {
        self.deferred.is_empty() && !self.processed.is_empty()
    }
}

/// One request's worth of pages, waiting to be sent for one purpose.
///
/// R05.3. The scheduler works off a queue of these rather than a fixed list of batches,
/// because a truncated answer has to put *smaller* work back on the queue. A plain `for`
/// over the plan could not: it had nowhere to put the halves.
#[derive(Debug, Clone)]
struct PendingBatch {
    /// Indices into the catalogue.
    entries: Vec<usize>,
    /// Output envelope for this attempt. Raised once before the pages are split, because
    /// a batch that overran slightly is cheaper to re-ask than to halve.
    max_output_tokens: u32,
    /// How many times this work has already come back truncated.
    attempt: u32,
}

/// Ceiling on the output envelope a truncation retry may ask for.
///
/// Two doublings from a 4 000-token base. Past that the answer is not "the envelope was
/// small", it is "these pages hold more than one request should carry", and the pages get
/// split instead.
const MAX_TRUNCATION_RETRIES: u32 = 2;

/// How many requests each purpose may spend.
///
/// Fair share, not first-come. `max_requests_per_purpose` bounds each pass, and the
/// per-run total is divided by the passes still to run — so a bureau that lowers the total
/// starves every purpose a little rather than starving the last ones completely. That is
/// the arithmetic answer to the defect: the inventory pass cannot take the material's
/// whole budget, because it is handed a share and not a pool.
fn share_of_budget(limits: &LlmLimits, remaining_total: u32, passes_left: u32) -> u32 {
    let per_purpose = limits.max_requests_per_purpose.max(1);
    let fair = remaining_total / passes_left.max(1);
    per_purpose.min(fair)
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

    let system_prompt = prompt::system_prompt();
    let mut outcome = DraftOutcome {
        draft: CandidateDraft::default(),
        requests_made: 0,
        input_chars: 0,
        pages_considered: 0,
        pages_skipped: 0,
        processed: Vec::new(),
        deferred: Vec::new(),
        passes: Vec::new(),
        provider: description.provider.clone(),
        model: None,
    };

    let total_budget = limits.max_requests_per_run.max(1);
    let mut spent = 0_u32;

    for (position, purpose) in DraftPurpose::ALL.into_iter().enumerate() {
        let passes_left = u32::try_from(DraftPurpose::ALL.len() - position).unwrap_or(1);
        let allowed = share_of_budget(limits, total_budget.saturating_sub(spent), passes_left);

        // The products this pass may cite. Taken from the draft as it stands, so the
        // inventory pass feeds the four that follow and none of them has to re-list.
        let known = KnownProducts::from_draft(&outcome.draft);
        // R05.3: how many pages this purpose can carry is the purpose's business. An
        // applications request that takes six pages of a technical catalogue produces
        // more than the envelope holds — which is exactly how a live pass came back
        // truncated on its third batch.
        let (batches, deferred_indices) = prompt::plan_batches_for(
            catalog,
            limits,
            purpose.pages_per_request(limits.max_pages_per_request),
            allowed as usize,
        );

        let mut pass = PurposePass {
            purpose,
            requests_allowed: allowed,
            requests_made: 0,
            input_chars: 0,
            processed: Vec::new(),
            deferred: deferred_indices
                .iter()
                .map(|index| catalog.entries()[*index].page.page_id)
                .collect(),
            truncated_retries: 0,
        };

        if !pass.deferred.is_empty() {
            // Named per purpose. «Страниц не вошло» without saying into *what* is the
            // report that made "0 terms" unreadable.
            outcome.draft.note(format!(
                "проход «{}»: страниц не вошло из-за лимита запросов: {}",
                purpose.as_str(),
                pass.deferred.len()
            ));
        }

        // A queue, not a fixed list: a truncated answer puts *smaller* work back on it.
        let mut queue: VecDeque<PendingBatch> = batches
            .iter()
            .map(|batch| PendingBatch {
                entries: batch.entry_indices.clone(),
                max_output_tokens: limits.max_output_tokens,
                attempt: 0,
            })
            .collect();
        let mut batch_index = 0_i32;

        while let Some(pending) = queue.pop_front() {
            // The share is a hard stop, retries included. Without this a pathological
            // page could split its way through the whole run's budget.
            if pass.requests_made >= allowed || spent >= total_budget {
                defer_pending(catalog, &pending, &mut pass);
                outcome.draft.note(format!(
                    "проход «{}»: страниц не вошло — доля запросов исчерпана, в том числе \
                     повторами после обрыва: {}",
                    purpose.as_str(),
                    pending.entries.len()
                ));
                continue;
            }

            let entries: Vec<&source::CatalogEntry> = pending
                .entries
                .iter()
                .map(|position| &catalog.entries()[*position])
                .collect();
            batch_index += 1;

            let request = LlmRequest {
                purpose: "knowledge_draft",
                system_prompt: system_prompt.clone(),
                user_prompt: prompt::user_prompt(
                    context, &entries, tables, purpose, &known, limits,
                ),
                schema_name: SCHEMA_NAME,
                // Only this purpose's sections. `additionalProperties: false` makes
                // spending an applications pass on products unrepresentable rather than
                // merely discouraged.
                schema: schema::response_schema_for(purpose),
                max_output_tokens: pending.max_output_tokens,
            };
            let input_chars = request.input_chars();

            let response = match provider.complete_json(&request).await {
                Ok(response) => response,
                Err(LlmError::NotConfigured(message)) => {
                    return Err(KnowledgeError::ProviderNotConfigured(message))
                }
                // R05.3 — the answer was cut off by the output limit. This is *not* a run
                // failure: it says this request asked for more than one answer can hold,
                // and the fix is to ask for less. Failing the run here is what threw away
                // three completed passes in a live UAT and left the job permanently
                // failed with no way back.
                Err(LlmError::Truncated) => {
                    spent += 1;
                    pass.requests_made += 1;
                    pass.truncated_retries += 1;
                    pass.input_chars = pass
                        .input_chars
                        .saturating_add(u32::try_from(input_chars).unwrap_or(u32::MAX));

                    match recover_from_truncation(&pending, limits) {
                        Some(smaller) => {
                            warn!(
                                purpose = purpose.as_str(),
                                pages = pending.entries.len(),
                                attempt = pending.attempt,
                                "answer truncated; retrying this batch smaller"
                            );
                            // Front of the queue: finish recovering this work before
                            // moving on, so the pages stay together in the account.
                            for piece in smaller.into_iter().rev() {
                                queue.push_front(piece);
                            }
                        }
                        None => {
                            // One page, at the largest envelope, still overran. The page
                            // is deferred *for this purpose* with the reason stated —
                            // never silently, and never as a failure of the whole run.
                            defer_pending(catalog, &pending, &mut pass);
                            outcome.draft.reject(format!(
                                "проход «{}»: ответ обрывается лимитом длины даже на одной \
                                 странице — страниц отложено: {}",
                                purpose.as_str(),
                                pending.entries.len()
                            ));
                        }
                    }
                    continue;
                }
                Err(error) => {
                    // Everything else still fails the run: persistence replaces a
                    // material's candidates as one set, and a partial set produced by a
                    // provider that is down would look like a complete one.
                    warn!(
                        purpose = purpose.as_str(),
                        batch = batch_index,
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

            pass.requests_made += 1;
            spent += 1;
            pass.input_chars = pass
                .input_chars
                .saturating_add(u32::try_from(input_chars).unwrap_or(u32::MAX));
            outcome.model = Some(response.model.clone());

            // The answer came back, so every page of this request was genuinely put in
            // front of the model — including when the answer turns out not to match the
            // schema below. That refusal is counted as a refusal; calling the pages unread
            // would be a second, wrong story about the same event.
            let page_budget = prompt::per_page_budget(limits);
            for entry in &entries {
                // A page recovered after a split was already marked deferred by the
                // attempt that failed; reaching it now supersedes that.
                pass.deferred.retain(|page| *page != entry.page.page_id);
                if !pass
                    .processed
                    .iter()
                    .any(|done| done.page_id == entry.page.page_id)
                {
                    pass.processed.push(ProcessedPage {
                        page_id: entry.page.page_id,
                        batch_index,
                        chars_sent: i32::try_from(entry.page.text.chars().count().min(page_budget))
                            .unwrap_or(i32::MAX),
                    });
                }
            }

            match DraftResponse::parse(&response.json) {
                Ok(parsed) => {
                    let prefix = format!("{}{batch_index}", purpose.as_str());
                    let validated = validate::validate_response(
                        &parsed,
                        catalog,
                        draft_limits,
                        &prefix,
                        &known,
                    );
                    debug!(
                        purpose = purpose.as_str(),
                        batch = batch_index,
                        facts = validated.facts.len(),
                        rejected = validated.rejected,
                        "validated one response"
                    );
                    outcome.draft.merge(validated);
                }
                // A response that does not match the schema is a refusal with a reason,
                // not a crash: the other batches may still be usable and the run says
                // what happened.
                Err(reason) => outcome
                    .draft
                    .reject(format!("проход «{}»: {reason}", purpose.as_str())),
            }
        }

        outcome.requests_made += pass.requests_made;
        outcome.input_chars = outcome.input_chars.saturating_add(pass.input_chars);
        outcome.passes.push(pass);
    }

    // Every pass has run, so the declarations can finally be checked against the whole
    // draft rather than against the one response that carried them.
    outcome.draft.reconcile_declarations();

    // The material-level account is the union of the passes. A page is processed when any
    // pass put it in front of the model, and deferred only when none of them did — the
    // page account answers "was this page read at all", and the per-purpose breakdown
    // above answers "read for what".
    outcome.processed = union_of_processed(&outcome.passes);
    outcome.deferred = catalog
        .entries()
        .iter()
        .map(|entry| entry.page.page_id)
        .filter(|page| {
            !outcome
                .processed
                .iter()
                .any(|processed| processed.page_id == *page)
        })
        .collect();
    outcome.pages_considered = u32::try_from(outcome.processed.len()).unwrap_or(u32::MAX);
    outcome.pages_skipped = u32::try_from(outcome.deferred.len()).unwrap_or(u32::MAX);

    Ok(outcome)
}

/// What to try instead, after an answer came back cut off by the output limit.
///
/// Two levers, in order of cost:
///
/// 1. **The same pages, a bigger envelope.** A batch that overran slightly is cheaper to
///    re-ask than to halve, and halving it would double the requests for nothing.
/// 2. **Half the pages, at the base envelope.** Once the envelope has been doubled twice
///    and the answer is still cut off, the honest reading is not "the envelope is small",
///    it is "these pages hold more than one answer should carry".
///
/// `None` means neither lever is left: a single page, at the largest envelope, still
/// overran. The caller defers that page for this purpose with the reason stated — which is
/// a bounded, recorded outcome rather than a failed run.
fn recover_from_truncation(
    pending: &PendingBatch,
    limits: &LlmLimits,
) -> Option<Vec<PendingBatch>> {
    if pending.attempt < MAX_TRUNCATION_RETRIES {
        return Some(vec![PendingBatch {
            entries: pending.entries.clone(),
            max_output_tokens: pending.max_output_tokens.saturating_mul(2),
            attempt: pending.attempt + 1,
        }]);
    }

    if pending.entries.len() <= 1 {
        return None;
    }

    // Split, and start each half back at the base envelope: the pages are what was too
    // much, so a doubled envelope would only make the halves expensive.
    let middle = pending.entries.len() / 2;
    Some(vec![
        PendingBatch {
            entries: pending.entries[..middle].to_vec(),
            max_output_tokens: limits.max_output_tokens,
            attempt: 0,
        },
        PendingBatch {
            entries: pending.entries[middle..].to_vec(),
            max_output_tokens: limits.max_output_tokens,
            attempt: 0,
        },
    ])
}

/// Record every page of a batch the pass could not send as deferred for this purpose.
///
/// Deferred rather than dropped: the requirement check reads `covered_everything`, so a
/// page that never got through has to be visible there or the pass would claim to have
/// examined the material it did not.
fn defer_pending(catalog: &SourceCatalog, pending: &PendingBatch, pass: &mut PurposePass) {
    for index in &pending.entries {
        let page_id = catalog.entries()[*index].page.page_id;
        let reached = pass.processed.iter().any(|done| done.page_id == page_id);
        if !reached && !pass.deferred.contains(&page_id) {
            pass.deferred.push(page_id);
        }
    }
}

/// Every page any pass reached, once, keeping the first pass that carried it.
fn union_of_processed(passes: &[PurposePass]) -> Vec<ProcessedPage> {
    let mut union: Vec<ProcessedPage> = Vec::new();
    for pass in passes {
        for page in &pass.processed {
            if !union.iter().any(|kept| kept.page_id == page.page_id) {
                union.push(*page);
            }
        }
    }
    union
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

    fn provider_of(replies: Vec<FakeReply>) -> FakeProvider {
        FakeProvider::new(replies)
    }

    fn context() -> PromptContext {
        PromptContext {
            partner_name: "BASIS".to_owned(),
            material_filename: "catalogue.pdf".to_owned(),
            pages_with_text: 2,
        }
    }

    /// What an inventory pass returns: the product, and nothing else.
    fn inventory_answer(source: &str, quote: &str) -> serde_json::Value {
        let _ = (source, quote);
        json!({
            "categories": [],
            "products": [{
                "ref": "p1", "category_ref": null, "kind": "product",
                "name": "BP21", "summary": "профиль монтажный", "aliases": [],
            }],
        })
    }

    /// What a facts pass returns: the fact, citing the product by the server's label.
    fn facts_answer(source: &str, quote: &str, value: &str) -> serde_json::Value {
        json!({
            "facts": [{
                "product_ref": "P1", "kind": "characteristic", "attribute": "нагрузка",
                "value": value, "unit": null, "conditions": null, "model_context": null,
                "evidence": [{"source": source, "quote": quote}],
            }],
        })
    }

    /// The budget policy, stated as arithmetic.
    ///
    /// This is the number the owner inspects before a live run, so it is pinned rather
    /// than left to be read out of the loop.
    #[test]
    fn the_budget_is_shared_between_the_passes_and_never_starves_one_to_nothing() {
        let limits = |total: u32, per: u32| LlmLimits {
            max_requests_per_run: total,
            max_requests_per_purpose: per,
            ..LlmLimits::default()
        };

        // The default: 40 across five passes, capped at 8 each — every pass gets 8.
        let default = LlmLimits::default();
        assert_eq!(default.max_requests_per_run, 40);
        assert_eq!(default.max_requests_per_purpose, 8);
        for position in 0..5 {
            let spent = 8 * position;
            assert_eq!(
                share_of_budget(&default, 40 - spent, 5 - position),
                8,
                "pass {position} did not get its share"
            );
        }

        // A lowered total is divided, not consumed first-come.
        assert_eq!(share_of_budget(&limits(10, 8), 10, 5), 2);
        // The per-purpose cap still binds when the total is generous.
        assert_eq!(share_of_budget(&limits(100, 3), 100, 5), 3);

        // A total too small to give every pass a request leaves the later ones with
        // nothing — which the run then reports as unread pages per purpose, rather than
        // letting the first pass quietly take all of it.
        assert_eq!(share_of_budget(&limits(1, 1), 1, 5), 0);
        assert_eq!(share_of_budget(&limits(1, 1), 1, 1), 1);
    }

    // --- R05.3: recovering from a truncated answer -------------------------------

    /// The ladder, stated as arithmetic: envelope twice, then split, then defer.
    #[test]
    fn a_truncated_batch_is_re_asked_bigger_then_split_then_given_up_on() {
        let limits = LlmLimits::default();
        let four = |attempt: u32, tokens: u32| PendingBatch {
            entries: vec![0, 1, 2, 3],
            max_output_tokens: tokens,
            attempt,
        };

        // 1. Same pages, twice the envelope.
        let first = recover_from_truncation(&four(0, 4_000), &limits).expect("a first retry");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].entries, vec![0, 1, 2, 3]);
        assert_eq!(first[0].max_output_tokens, 8_000);
        assert_eq!(first[0].attempt, 1);

        // 2. Once more.
        let second = recover_from_truncation(&four(1, 8_000), &limits).expect("a second retry");
        assert_eq!(second[0].max_output_tokens, 16_000);

        // 3. Out of envelope: split, each half back at the base.
        let split = recover_from_truncation(&four(2, 16_000), &limits).expect("a split");
        assert_eq!(split.len(), 2);
        assert_eq!(split[0].entries, vec![0, 1]);
        assert_eq!(split[1].entries, vec![2, 3]);
        assert!(
            split
                .iter()
                .all(|piece| piece.max_output_tokens == limits.max_output_tokens
                    && piece.attempt == 0)
        );

        // 4. One page, nothing left to try.
        let single = PendingBatch {
            entries: vec![7],
            max_output_tokens: 16_000,
            attempt: MAX_TRUNCATION_RETRIES,
        };
        assert!(recover_from_truncation(&single, &limits).is_none());
    }

    /// The live failure: one purpose's batch comes back truncated, and the run survives.
    ///
    /// The applications pass overran on its own request. Before R05.3 that failed the
    /// whole run — discarding three completed passes and leaving the job permanently
    /// failed with nothing to resume from. Now the batch is re-asked smaller and the
    /// applications arrive.
    #[tokio::test]
    async fn a_truncated_applications_batch_is_retried_smaller_and_still_produces_tasks() {
        let catalog = SourceCatalog::build(vec![
            page(1, "BP21 1200 3.5 kN профиль монтажный"),
            page(2, "BP21 крепится к бетонному перекрытию"),
        ]);

        let provider = FakeProvider::new(vec![
            // inventory, facts, glossary — all fine, and they must survive.
            FakeReply::Json(inventory_answer("S1", "BP21 1200 3.5 kN")),
            FakeReply::Json(facts_answer("S1", "BP21 1200 3.5 kN", "3.5")),
            FakeReply::Json(json!({"glossary": []})),
            // applications: the answer is cut off by the output limit…
            FakeReply::Fail(LlmError::Truncated),
            // …and the retry, with a bigger envelope, comes back whole.
            FakeReply::Json(json!({
                "applications": [{
                    "product_ref": "P1",
                    "task": "закрепить лоток к перекрытию",
                    "summary": null, "model_context": null,
                    "evidence": [{"source": "S1", "quote": "BP21 1200 3.5 kN"}],
                    "details": [],
                }],
            })),
            FakeReply::Json(json!({"qa": [], "gaps": []})),
        ]);

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

        // The applications the truncated request would have lost.
        assert_eq!(outcome.draft.applications.len(), 1);
        assert_eq!(
            outcome.draft.applications[0].task,
            "закрепить лоток к перекрытию"
        );

        // The passes before it are untouched — this is the checkpoint that used to be
        // thrown away.
        assert_eq!(outcome.draft.products.len(), 1);
        assert_eq!(outcome.draft.facts.len(), 1);
        assert_eq!(outcome.passes.len(), 5);

        let applications = outcome
            .passes
            .iter()
            .find(|pass| pass.purpose == DraftPurpose::Applications)
            .expect("the applications pass");
        // The recovery is counted and visible, not hidden inside the ordinary spend.
        assert_eq!(applications.truncated_retries, 1);
        assert_eq!(applications.requests_made, 2);
        // …and the pages still count as covered: the retry reached them.
        assert!(
            applications.covered_everything(),
            "the retry covered the material: {applications:?}"
        );
        // The whole run made one extra call and no more.
        assert_eq!(outcome.requests_made, 6);
    }

    /// A page that overruns even alone is deferred for that purpose — never a run failure.
    #[tokio::test]
    async fn a_page_that_truncates_at_every_size_is_deferred_for_that_purpose_alone() {
        let catalog = SourceCatalog::build(vec![page(1, "BP21 1200 3.5 kN профиль монтажный")]);

        let mut replies = vec![
            FakeReply::Json(inventory_answer("S1", "BP21 1200 3.5 kN")),
            FakeReply::Json(facts_answer("S1", "BP21 1200 3.5 kN", "3.5")),
            FakeReply::Json(json!({"glossary": []})),
        ];
        // The applications pass never gets a whole answer, however small the ask.
        for _ in 0..=MAX_TRUNCATION_RETRIES {
            replies.push(FakeReply::Fail(LlmError::Truncated));
        }
        replies.push(FakeReply::Json(json!({"qa": [], "gaps": []})));

        let outcome = draft_knowledge(
            &provider_of(replies),
            &catalog,
            &context(),
            &TableContext::default(),
            &LlmLimits::default(),
            &DraftLimits::default(),
        )
        .await
        .unwrap();

        // The run finished. The earlier passes kept what they found.
        assert_eq!(outcome.draft.products.len(), 1);
        assert_eq!(outcome.draft.facts.len(), 1);

        let applications = outcome
            .passes
            .iter()
            .find(|pass| pass.purpose == DraftPurpose::Applications)
            .expect("the applications pass");
        assert_eq!(applications.truncated_retries, 3);
        assert!(applications.processed.is_empty());
        assert_eq!(applications.deferred.len(), 1);
        // Which means the topic cannot be cleared by a declaration: nothing examined it.
        assert!(!applications.covered_everything());
        assert!(
            outcome
                .draft
                .rejections
                .iter()
                .any(|note| note.contains("обрыв") || note.contains("лимитом длины")),
            "the refusal says what happened: {:?}",
            outcome.draft.rejections
        );
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

    /// Every pass runs, each with its own share, and the products the inventory found
    /// are the ones the later passes attach to.
    #[tokio::test]
    async fn each_purpose_gets_its_own_pass_and_later_passes_attach_to_the_known_products() {
        let catalog = SourceCatalog::build(vec![
            page(1, "BP21 1200 3.5 kN профиль монтажный"),
            page(2, "BP21 поставляется с крепежом"),
        ]);
        // One request per pass covers both pages at the default page budget.
        let provider = FakeProvider::new(vec![
            FakeReply::Json(inventory_answer("S1", "BP21 1200 3.5 kN")),
            FakeReply::Json(facts_answer("S1", "BP21 1200 3.5 kN", "3.5")),
            FakeReply::Json(json!({"glossary": []})),
            FakeReply::Json(json!({"applications": []})),
            FakeReply::Json(json!({"qa": [], "gaps": [], "declarations": {
                "glossary": null, "questions": null, "applications": null,
                "commercial_unknowns": null, "technical_unknowns": null,
            }})),
        ]);

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

        assert_eq!(outcome.requests_made, 5, "one request per purpose");
        assert_eq!(outcome.passes.len(), 5);
        assert_eq!(
            outcome
                .passes
                .iter()
                .map(|pass| pass.purpose)
                .collect::<Vec<_>>(),
            DraftPurpose::ALL.to_vec()
        );
        // Every pass read the whole material, so every topic could be answered.
        assert!(outcome.passes.iter().all(PurposePass::covered_everything));
        assert_eq!(outcome.pages_considered, 2);
        assert_eq!(outcome.pages_skipped, 0);
        assert!(outcome.input_chars > 0);
        assert_eq!(outcome.model.as_deref(), Some("fake/model-1"));

        assert_eq!(outcome.draft.products.len(), 1);
        assert_eq!(outcome.draft.facts.len(), 1);
        // The facts pass never re-declared the product; it cited the server's label, and
        // the fact still landed on the product the inventory pass created.
        assert_eq!(
            outcome.draft.facts[0].product_ref,
            Some(outcome.draft.products[0].reference.clone())
        );
    }

    /// The defect, as a unit test: one pass cannot take the whole run.
    #[tokio::test]
    async fn no_single_purpose_may_spend_the_whole_run_budget() {
        let catalog = SourceCatalog::build(
            (1..=12)
                .map(|number| page(number, "BP21 1200 3.5 kN профиль монтажный"))
                .collect(),
        );
        // One page per request and twelve pages: each pass *wants* twelve requests.
        let limits = LlmLimits {
            max_pages_per_request: 1,
            max_requests_per_run: 10,
            max_requests_per_purpose: 8,
            ..LlmLimits::default()
        };
        let provider = FakeProvider::new(
            (0..10)
                .map(|_| FakeReply::Json(json!({})))
                .collect::<Vec<_>>(),
        );

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

        // Ten requests over five purposes: two each, and none starved to nothing.
        assert_eq!(outcome.requests_made, 10);
        for pass in &outcome.passes {
            assert_eq!(
                pass.requests_made,
                2,
                "{} took {} of the run",
                pass.purpose.as_str(),
                pass.requests_made
            );
            assert!(
                !pass.covered_everything(),
                "ten pages are still unread for {}",
                pass.purpose.as_str()
            );
        }
        // And every purpose says which pages it did not reach, under its own name.
        for purpose in DraftPurpose::ALL {
            assert!(
                outcome
                    .draft
                    .rejections
                    .iter()
                    .any(|note| note.contains(purpose.as_str())),
                "{} did not report its unread pages: {:?}",
                purpose.as_str(),
                outcome.draft.rejections
            );
        }
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
        let provider = FakeProvider::new(vec![
            FakeReply::Json(inventory_answer("S404", "BP21 1200 3.5 kN")),
            FakeReply::Json(facts_answer("S404", "BP21 1200 3.5 kN", "3.5")),
            FakeReply::Json(json!({})),
            FakeReply::Json(json!({})),
            FakeReply::Json(json!({})),
        ]);

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
        let provider = FakeProvider::new(
            (0..5)
                .map(|_| FakeReply::Json(json!({"answer": "вот характеристики"})))
                .collect::<Vec<_>>(),
        );

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
        // One refusal per pass, each naming the pass it happened in.
        assert_eq!(outcome.draft.rejected, 5);
        assert!(outcome
            .draft
            .rejections
            .iter()
            .all(|reason| reason.contains("схеме")));
    }

    #[tokio::test]
    async fn a_material_larger_than_the_budget_reports_the_pages_it_did_not_read() {
        let catalog = SourceCatalog::build(
            (1..=6)
                .map(|number| page(number, "BP21 1200 3.5 kN профиль монтажный"))
                .collect(),
        );
        let provider = FakeProvider::new(vec![FakeReply::Json(json!({}))]);
        // One request for the whole *run*. Integer division gives the first four passes a
        // share of zero; the remainder is not lost, it falls to the last pass, which is
        // the only one that gets to make a call. Worth stating because it is the opposite
        // of the defect: when the budget is desperate, it is the inventory that goes
        // without, not the sections that used to be crowded out by it.
        let limits = LlmLimits {
            max_pages_per_request: 1,
            max_requests_per_run: 1,
            max_requests_per_purpose: 1,
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
        let spender = outcome
            .passes
            .iter()
            .find(|pass| pass.requests_made > 0)
            .expect("one pass made the single call");
        assert_eq!(spender.purpose, DraftPurpose::Inquiry);
        assert_eq!(outcome.pages_skipped, 5);
        assert!(outcome
            .draft
            .rejections
            .iter()
            .any(|reason| reason.contains("страниц не вошло")));

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
