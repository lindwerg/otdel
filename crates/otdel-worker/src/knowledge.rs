//! The understanding worker: a read material → a bounded model call → a stored draft.
//!
//! The shape of one job, and why:
//!
//! 1. **Claim** an `understand_material` job (and only that kind — the reader half
//!    claims the extraction kinds, so neither can pick up work it cannot run).
//! 2. **Collect the sources** — pages of *this* material that really carry text, read
//!    in a bureau-scoped transaction. A page awaiting recognition is never offered:
//!    asking a model about a page nobody could read is how invented content gets in.
//! 3. **Call the model outside any transaction**, bounded by the configured limits.
//!    With no key configured this step never happens: the run is recorded as
//!    `needs_provider` and *nothing* is stored.
//! 4. **Validate every candidate against the same pages** (`otdel-knowledge`), then
//!    **store the survivors and the refusals together** in one transaction that
//!    replaces this material's previous draft.
//!
//! Step 4 is the phase's whole point: what reaches the database is a fact whose quote
//! was found, character for character, in a page of this material. Everything else is
//! counted and explained, never stored.

use std::sync::Arc;
use std::time::Duration;

use otdel_core::config::Config;
use otdel_core::knowledge::KnowledgeRunStatus;
use otdel_core::model::{Job, JobKind};
use otdel_db::knowledge::{
    self, NewCategory, NewDraft, NewEvidence, NewFact, NewGap, NewProduct, NewQa, NewQuestion,
    NewTerm, RunOutcome,
};
use otdel_db::{jobs, materials, pages, partners, Database};
use otdel_knowledge::{
    draft_knowledge, CandidateDraft, DraftLimits, KnowledgeError, PromptContext, SourceCatalog,
    SourcePage, PROMPT_PROFILE,
};
use otdel_llm::{LlmProvider, ProviderDescription};
use tracing::{info, warn};
use uuid::Uuid;

use crate::error::WorkerError;

/// Delay before a transient model failure is tried again. Longer than the extraction
/// backoff: a rate-limited provider needs more than thirty seconds of patience.
const RETRY_BACKOFF: Duration = Duration::from_secs(120);

/// What one understanding pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KnowledgeReport {
    pub jobs_claimed: u32,
    pub jobs_completed: u32,
    pub jobs_failed: u32,
    pub facts_stored: u32,
    pub candidates_rejected: u32,
    /// Runs that stopped because the model adapter is not configured.
    pub runs_awaiting_provider: u32,
}

pub struct KnowledgeWorker {
    config: Arc<Config>,
    db: Database,
    provider: Arc<dyn LlmProvider>,
    owner: String,
    draft_limits: DraftLimits,
}

impl KnowledgeWorker {
    pub fn new(config: Arc<Config>, db: Database, provider: Arc<dyn LlmProvider>) -> Self {
        Self {
            config,
            db,
            provider,
            owner: format!("otdel-productologist/{}", Uuid::new_v4()),
            draft_limits: DraftLimits::default(),
        }
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// What the interface and the startup log say about the model adapter.
    pub fn provider(&self) -> ProviderDescription {
        self.provider.describe()
    }

    /// Drain the understanding queue for this bureau, up to `max_jobs`.
    pub async fn run_pass(
        &self,
        bureau_id: Uuid,
        max_jobs: u32,
    ) -> Result<KnowledgeReport, WorkerError> {
        let mut report = KnowledgeReport::default();

        for _ in 0..max_jobs {
            let Some(job) = self.claim(bureau_id).await? else {
                break;
            };
            report.jobs_claimed += 1;

            match self.run_job(bureau_id, &job).await {
                Ok(outcome) => {
                    report.facts_stored += outcome.facts_stored;
                    report.candidates_rejected += outcome.rejected;
                    self.settle(bureau_id, &job, None).await?;
                    report.jobs_completed += 1;
                }
                Err(error) => {
                    if matches!(error, WorkerError::ProviderNotConfigured(_)) {
                        report.runs_awaiting_provider += 1;
                    }
                    warn!(
                        job_id = %job.id,
                        material_id = %job.material_id,
                        permanent = error.is_permanent(),
                        error = %error,
                        "understanding job did not finish"
                    );
                    self.settle(bureau_id, &job, Some(&error)).await?;
                    report.jobs_failed += 1;
                }
            }
        }

        Ok(report)
    }

    async fn claim(&self, bureau_id: Uuid) -> Result<Option<Job>, WorkerError> {
        let mut tx = self.db.begin_scoped(bureau_id).await?;
        let job = jobs::claim_next(
            &mut tx,
            &self.owner,
            self.config.extraction.lease_duration,
            &JobKind::knowledge_kinds(),
        )
        .await?;
        tx.commit().await?;
        Ok(job)
    }

    async fn settle(
        &self,
        bureau_id: Uuid,
        job: &Job,
        failure: Option<&WorkerError>,
    ) -> Result<(), WorkerError> {
        if matches!(failure, Some(WorkerError::LeaseLost)) {
            return Ok(());
        }

        let mut tx = self.db.begin_scoped(bureau_id).await?;
        match failure {
            None => {
                jobs::complete(&mut tx, job.id, &self.owner).await?;
            }
            Some(error) => {
                jobs::fail(
                    &mut tx,
                    job.id,
                    &self.owner,
                    &error.diagnostic(),
                    error.is_permanent(),
                    RETRY_BACKOFF,
                )
                .await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    async fn run_job(&self, bureau_id: Uuid, job: &Job) -> Result<JobOutcome, WorkerError> {
        // The material and its pages, inside this bureau and this partner. A job that
        // named somebody else's material would find nothing here.
        let mut tx = self.db.begin_scoped(bureau_id).await?;
        let stored = materials::get_in_partner(&mut tx, job.partner_id, job.material_id).await?;
        let Some(stored) = stored else {
            tx.commit().await?;
            return Err(WorkerError::MaterialMissing);
        };
        let partner = partners::get(&mut tx, job.partner_id)
            .await?
            .map_or_else(|| "партнёр".to_owned(), |partner| partner.name);
        let readable = pages::readable_with_text(&mut tx, job.material_id).await?;
        let run =
            knowledge::start_run(&mut tx, job.partner_id, job.material_id, PROMPT_PROFILE).await?;
        tx.commit().await?;

        let catalog = SourceCatalog::build(
            readable
                .into_iter()
                .filter_map(|(page, text)| {
                    SourcePage::from_page(&page, &stored.material.filename, Some(text))
                })
                .collect(),
        );
        let context = PromptContext {
            partner_name: partner,
            material_filename: stored.material.filename.clone(),
            pages_with_text: catalog.len(),
        };

        let description = self.provider.describe();
        let result = draft_knowledge(
            self.provider.as_ref(),
            &catalog,
            &context,
            &self.config.llm.limits,
            &self.draft_limits,
        )
        .await;

        let drafted = match result {
            Ok(drafted) => drafted,
            Err(error) => {
                // Every failure is recorded on the run, so the owner sees why the
                // material has no draft — and the previous draft, if any, is left
                // untouched rather than replaced by nothing.
                let (status, worker_error) = match &error {
                    KnowledgeError::ProviderNotConfigured(message) => (
                        KnowledgeRunStatus::NeedsProvider,
                        WorkerError::ProviderNotConfigured(message.clone()),
                    ),
                    KnowledgeError::NoReadableSources => {
                        (KnowledgeRunStatus::Failed, WorkerError::NothingToUnderstand)
                    }
                    KnowledgeError::ProviderFailed {
                        diagnostic,
                        retryable,
                    } => (
                        KnowledgeRunStatus::Failed,
                        WorkerError::ModelCallFailed {
                            diagnostic: diagnostic.clone(),
                            retryable: *retryable,
                        },
                    ),
                };

                let mut tx = self.db.begin_scoped(bureau_id).await?;
                knowledge::finish_run(
                    &mut tx,
                    run.id,
                    &RunOutcome {
                        status,
                        provider: Some(description.provider.clone()),
                        model: None,
                        pages_considered: 0,
                        pages_skipped: 0,
                        requests_made: 0,
                        input_chars: 0,
                        facts_rejected: 0,
                        rejections: Vec::new(),
                        diagnostic: Some(error.to_string()),
                        counts: knowledge::DraftCounts::default(),
                    },
                )
                .await?;
                tx.commit().await?;

                return Err(worker_error);
            }
        };

        // Storing the draft and closing the run happen together — a reader can never
        // see candidates from one run described by the counters of another — and only
        // if this worker still holds the job.
        //
        // The model call takes as long as it takes; if the lease expired meanwhile,
        // the maintenance pass has returned the job to the queue and somebody else may
        // already be drafting the same material. Writing now would replace their draft
        // with ours, which is the "late result of an old run overwrites a newer one"
        // failure `docs/block-01-spec.md` §7 forbids. Stopping is the safe move: the
        // job belongs to whoever reclaimed it.
        let new_draft = to_new_draft(&drafted.draft);
        let mut tx = self.db.begin_scoped(bureau_id).await?;
        let still_ours = jobs::heartbeat(
            &mut tx,
            job.id,
            &self.owner,
            self.config.extraction.lease_duration,
            Some("storing the draft"),
        )
        .await?;
        if !still_ours {
            tx.rollback().await?;
            return Err(WorkerError::LeaseLost);
        }

        let counts =
            knowledge::replace_draft(&mut tx, job.partner_id, job.material_id, run.id, &new_draft)
                .await?;

        let status = if drafted.draft.rejected > 0 || drafted.pages_skipped > 0 {
            KnowledgeRunStatus::Partial
        } else {
            KnowledgeRunStatus::Completed
        };

        knowledge::finish_run(
            &mut tx,
            run.id,
            &RunOutcome {
                status,
                provider: Some(description.provider.clone()),
                model: drafted.model.clone(),
                pages_considered: i32::try_from(drafted.pages_considered).unwrap_or(i32::MAX),
                pages_skipped: i32::try_from(drafted.pages_skipped).unwrap_or(i32::MAX),
                requests_made: i32::try_from(drafted.requests_made).unwrap_or(i32::MAX),
                input_chars: i32::try_from(drafted.input_chars).unwrap_or(i32::MAX),
                facts_rejected: i32::try_from(drafted.draft.rejected).unwrap_or(i32::MAX),
                rejections: drafted.draft.rejections.clone(),
                diagnostic: None,
                counts,
            },
        )
        .await?;
        tx.commit().await?;

        info!(
            material_id = %job.material_id,
            status = status.as_str(),
            products = counts.products,
            facts = counts.facts,
            terms = counts.terms,
            gaps = counts.gaps,
            rejected = drafted.draft.rejected,
            requests = drafted.requests_made,
            "product knowledge drafted"
        );

        Ok(JobOutcome {
            facts_stored: u32::try_from(counts.facts).unwrap_or(0),
            rejected: drafted.draft.rejected,
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct JobOutcome {
    facts_stored: u32,
    rejected: u32,
}

/// Map validated candidates onto the storage layer's input.
///
/// A plain translation on purpose: every rule has already been applied, and a mapping
/// that decided anything would be a second place to look for the rules.
fn to_new_draft(draft: &CandidateDraft) -> NewDraft {
    NewDraft {
        categories: draft
            .categories
            .iter()
            .map(|category| NewCategory {
                reference: category.reference.clone(),
                kind: category.kind,
                name: category.name.clone(),
                summary: category.summary.clone(),
            })
            .collect(),
        products: draft
            .products
            .iter()
            .map(|product| NewProduct {
                reference: product.reference.clone(),
                category_ref: product.category_ref.clone(),
                kind: product.kind,
                name: product.name.clone(),
                summary: product.summary.clone(),
            })
            .collect(),
        facts: draft
            .facts
            .iter()
            .map(|fact| NewFact {
                product_ref: fact.product_ref.clone(),
                kind: fact.kind,
                attribute: fact.attribute.clone(),
                value_text: fact.value_text.clone(),
                unit: fact.unit.clone(),
                conditions: fact.conditions.clone(),
                model_context: fact.model_context.clone(),
                evidence: fact.evidence.iter().map(evidence).collect(),
            })
            .collect(),
        terms: draft
            .terms
            .iter()
            .map(|term| NewTerm {
                term: term.term.clone(),
                definition: term.definition.clone(),
                definition_is_model_context: term.definition_is_model_context,
                evidence: term.evidence.iter().map(evidence).collect(),
            })
            .collect(),
        qa: draft
            .qa
            .iter()
            .map(|entry| NewQa {
                question: entry.question.clone(),
                answer: entry.answer.clone(),
                answer_is_model_context: entry.answer_is_model_context,
                evidence: entry.evidence.iter().map(evidence).collect(),
            })
            .collect(),
        gaps: draft
            .gaps
            .iter()
            .map(|gap| NewGap {
                product_ref: gap.product_ref.clone(),
                topic: gap.topic.clone(),
                missing: gap.missing.clone(),
                blocks: gap.blocks.clone(),
                question: gap.question.as_ref().map(|question| NewQuestion {
                    audience: question.audience,
                    text: question.text.clone(),
                }),
            })
            .collect(),
    }
}

fn evidence(resolved: &otdel_knowledge::ResolvedEvidence) -> NewEvidence {
    NewEvidence {
        page_id: resolved.page_id,
        page_number: resolved.page_number,
        quote: resolved.quote.clone(),
        char_start: resolved.char_start,
        char_end: resolved.char_end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use otdel_core::knowledge::{CategoryKind, FactKind, ProductKind, QuestionAudience};
    use otdel_knowledge::{
        CandidateCategory, CandidateFact, CandidateGap, CandidateProduct, CandidateQuestion,
        ResolvedEvidence,
    };

    #[test]
    fn mapping_preserves_the_quote_its_offsets_and_the_separated_model_context() {
        let draft = CandidateDraft {
            categories: vec![CandidateCategory {
                reference: "b1:c1".to_owned(),
                kind: CategoryKind::Direction,
                name: "Монтажные системы".to_owned(),
                summary: None,
            }],
            products: vec![CandidateProduct {
                reference: "b1:p1".to_owned(),
                category_ref: Some("b1:c1".to_owned()),
                kind: ProductKind::Product,
                name: "BP21".to_owned(),
                summary: None,
            }],
            facts: vec![CandidateFact {
                product_ref: Some("b1:p1".to_owned()),
                kind: FactKind::Characteristic,
                attribute: "нагрузка".to_owned(),
                value_text: "3.5".to_owned(),
                unit: Some("kN".to_owned()),
                conditions: Some("две опоры".to_owned()),
                model_context: Some("пояснение модели".to_owned()),
                evidence: vec![ResolvedEvidence {
                    page_id: Uuid::from_u128(5),
                    material_id: Uuid::from_u128(6),
                    page_number: 3,
                    quote: "BP21 1200 3.5 kN".to_owned(),
                    char_start: 12,
                    char_end: 28,
                }],
            }],
            gaps: vec![CandidateGap {
                product_ref: Some("b1:p1".to_owned()),
                topic: "price".to_owned(),
                missing: "цена не указана".to_owned(),
                blocks: None,
                question: Some(CandidateQuestion {
                    audience: QuestionAudience::Partner,
                    text: "Какая цена?".to_owned(),
                }),
            }],
            ..CandidateDraft::default()
        };

        let mapped = to_new_draft(&draft);

        assert_eq!(mapped.categories.len(), 1);
        assert_eq!(mapped.products[0].category_ref.as_deref(), Some("b1:c1"));
        let fact = &mapped.facts[0];
        assert_eq!(fact.unit.as_deref(), Some("kN"));
        assert_eq!(fact.conditions.as_deref(), Some("две опоры"));
        assert_eq!(fact.model_context.as_deref(), Some("пояснение модели"));
        assert_eq!(fact.evidence[0].quote, "BP21 1200 3.5 kN");
        assert_eq!(fact.evidence[0].char_start, 12);
        assert_eq!(fact.evidence[0].char_end, 28);
        assert_eq!(
            mapped.gaps[0].question.as_ref().unwrap().audience,
            QuestionAudience::Partner
        );
    }

    #[test]
    fn an_empty_draft_maps_to_an_empty_draft() {
        let mapped = to_new_draft(&CandidateDraft::default());
        assert_eq!(mapped, NewDraft::default());
    }
}
