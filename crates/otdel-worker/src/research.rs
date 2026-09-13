//! The research worker: one approved question → bounded external work → candidate
//! conclusions.
//!
//! The shape of one pass, and why each step is where it is:
//!
//! 1. **Claim** a `research_plan` job (and only that kind — this is the half that spends
//!    money and opens sockets, and the document reader must never be able to reach it).
//! 2. **Refuse early.** No search endpoint, no allowlist, no key or no model means the
//!    plan is recorded as `needs_provider` and *nothing* is reserved or sent. Same for a
//!    plan that has used every pass it was allowed.
//! 3. **Build the queries deterministically**, and refuse outright if the question names
//!    the partner. Nothing has left the machine at this point, and nothing has been paid.
//! 4. **Search**, one query at a time: reserve → call → settle → journal. Between every
//!    two chargeable steps the worker checks the clock, the budget and the stop flag, so
//!    a run can always be ended at a boundary rather than in the middle of a payment.
//! 5. **Fetch** the results whose host the owner declared, under the same discipline.
//!    Everything else is recorded with the reason it was not read.
//! 6. **Interpret** the snapshots with the model, validate every conclusion against those
//!    same snapshots, and store the survivors with their exact citations.
//!
//! Step 6 is the point of the phase: what reaches the database is a statement whose quote
//! was found, character for character, in a page that was really downloaded from a host
//! the owner approved, at a time that is recorded. Everything else is counted and
//! explained, never stored.

use std::sync::Arc;
use std::time::{Duration, Instant};

use otdel_core::config::Config;
use otdel_core::model::{Job, JobKind};
use otdel_core::research::{
    QueryOutcome, ResearchPlan, ResearchPlanStatus, SourceStatus, SpendKind, SpendState,
};
use otdel_db::research::{
    self, NewFinding, NewFindingEvidence, NewSource, PlanOutcome, Reservation, ReserveOutcome,
    SourceOutcome,
};
use otdel_db::{jobs, partners, research_read, Database};
use otdel_research::{
    interpret_sources, CostModel, ExternalCatalog, ExternalSource, FindingLimits, ResearchContext,
    ResearchError,
};
use otdel_search::{DocumentFetcher, FetchRefusal, NormalisedUrl, SearchProvider, SearchRequest};
use tracing::{info, warn};
use uuid::Uuid;

use crate::error::WorkerError;

/// Delay before a transient research failure is tried again. Long: a rate-limited search
/// provider or a slow site needs more patience than a local PDF.
const RETRY_BACKOFF: Duration = Duration::from_secs(180);

/// What one research pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResearchReport {
    pub jobs_claimed: u32,
    pub jobs_completed: u32,
    pub jobs_failed: u32,
    pub queries_made: u32,
    pub sources_fetched: u32,
    pub findings_stored: u32,
    pub findings_rejected: u32,
    /// Plans that stopped because nothing is configured.
    pub plans_awaiting_provider: u32,
    /// Plans that stopped because the money ran out.
    pub plans_budget_exhausted: u32,
    pub micros_spent: i64,
}

pub struct ResearchWorker {
    config: Arc<Config>,
    db: Database,
    search: Arc<dyn SearchProvider>,
    fetcher: Arc<dyn DocumentFetcher>,
    model: Arc<dyn otdel_llm::LlmProvider>,
    owner: String,
    finding_limits: FindingLimits,
}

impl ResearchWorker {
    pub fn new(
        config: Arc<Config>,
        db: Database,
        search: Arc<dyn SearchProvider>,
        fetcher: Arc<dyn DocumentFetcher>,
        model: Arc<dyn otdel_llm::LlmProvider>,
    ) -> Self {
        let finding_limits = FindingLimits {
            max_input_chars: config.llm.limits.max_input_chars as usize,
            ..FindingLimits::default()
        };
        Self {
            config,
            db,
            search,
            fetcher,
            model,
            owner: format!("otdel-researcher/{}", Uuid::new_v4()),
            finding_limits,
        }
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// Can this worker do anything at all?
    ///
    /// All three halves are required. The model is part of it on purpose: a researcher
    /// that could search and fetch but not interpret would spend the budget to produce a
    /// list of pages and no answer.
    fn adapters_ready(&self) -> bool {
        self.search.describe().is_ready()
            && self.fetcher.describe().is_ready()
            && self.model.describe().is_ready()
    }

    /// One sentence naming what the owner still has to do.
    fn not_configured_message(&self) -> String {
        let search = self.search.describe();
        let model = self.model.describe();

        if !search.is_ready() {
            return search.message;
        }
        if !self.fetcher.describe().is_ready() {
            return self.fetcher.describe().message;
        }
        format!(
            "исследование требует модели для интерпретации источников: {}",
            model.message
        )
    }

    /// Drain the research queue for this bureau, up to `max_jobs`.
    pub async fn run_pass(
        &self,
        bureau_id: Uuid,
        max_jobs: u32,
    ) -> Result<ResearchReport, WorkerError> {
        let mut report = ResearchReport::default();

        for _ in 0..max_jobs {
            let Some(job) = self.claim(bureau_id).await? else {
                break;
            };
            report.jobs_claimed += 1;

            match self.run_job(bureau_id, &job).await {
                Ok(outcome) => {
                    report.queries_made += outcome.queries_made;
                    report.sources_fetched += outcome.sources_fetched;
                    report.findings_stored += outcome.findings_stored;
                    report.findings_rejected += outcome.findings_rejected;
                    report.micros_spent = report.micros_spent.saturating_add(outcome.micros_spent);
                    if outcome.status == ResearchPlanStatus::BudgetExhausted {
                        report.plans_budget_exhausted += 1;
                    }
                    self.settle(bureau_id, &job, None).await?;
                    report.jobs_completed += 1;
                }
                Err(error) => {
                    if matches!(error, WorkerError::ProviderNotConfigured(_)) {
                        report.plans_awaiting_provider += 1;
                    }
                    warn!(
                        job_id = %job.id,
                        permanent = error.is_permanent(),
                        error = %error,
                        "research job did not finish"
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
            &JobKind::research_kinds(),
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

    async fn heartbeat(&self, bureau_id: Uuid, job: &Job, stage: &str) -> Result<(), WorkerError> {
        let mut tx = self.db.begin_scoped(bureau_id).await?;
        let still_ours = jobs::heartbeat(
            &mut tx,
            job.id,
            &self.owner,
            self.config.extraction.lease_duration,
            Some(stage),
        )
        .await?;
        tx.commit().await?;
        if still_ours {
            Ok(())
        } else {
            Err(WorkerError::LeaseLost)
        }
    }

    async fn run_job(&self, bureau_id: Uuid, job: &Job) -> Result<JobOutcome, WorkerError> {
        let started = Instant::now();

        // The plan the job names. The column lives on the job row; phases 1A–1C have no
        // use for it and their wire contract is left alone.
        let mut tx = self.db.begin_scoped(bureau_id).await?;
        let plan_id = research_read::plan_id_for_job(&mut tx, job.id).await?;
        let Some(plan_id) = plan_id else {
            tx.commit().await?;
            return Err(WorkerError::ResearchPlanMissing);
        };
        let plan = research_read::find_plan_unscoped(&mut tx, plan_id).await?;
        let Some(plan) = plan else {
            tx.commit().await?;
            return Err(WorkerError::ResearchPlanMissing);
        };
        let partner_name = partners::get(&mut tx, plan.partner_id)
            .await?
            .map_or_else(|| "партнёр".to_owned(), |partner| partner.name);
        tx.commit().await?;

        // Nothing configured: no reservation, no request, no stored conclusion. This is
        // the state of every installation that has not been given a search endpoint.
        if !self.adapters_ready() {
            let message = self.not_configured_message();
            self.finish(
                bureau_id,
                job,
                plan_id,
                PlanOutcome {
                    status: ResearchPlanStatus::NeedsProvider,
                    provider: Some(self.search.describe().provider),
                    model: None,
                    queries_made: 0,
                    results_seen: 0,
                    sources_fetched: 0,
                    sources_skipped: 0,
                    bytes_fetched: 0,
                    findings_rejected: 0,
                    duration_ms: Some(elapsed_ms(started)),
                    rejections: Vec::new(),
                    diagnostic: Some(message.clone()),
                },
            )
            .await?;
            return Err(WorkerError::ProviderNotConfigured(message));
        }

        // A pass is counted here, which also bounds automatic retries of a transient
        // failure: `max_passes` is the ceiling on how often this question is researched
        // at all (`docs/block-01-plan.md`, 1D §3).
        //
        // The lease is checked in the same transaction, for a sharper reason than usual:
        // `start_pass` *deletes* the previous pass's queries, sources and conclusions. A
        // worker resuming after its lease expired would otherwise wipe the results a
        // second worker had just finished writing.
        let mut tx = self.db.begin_scoped(bureau_id).await?;
        let still_ours = jobs::heartbeat(
            &mut tx,
            job.id,
            &self.owner,
            self.config.extraction.lease_duration,
            Some("starting a pass"),
        )
        .await?;
        if !still_ours {
            tx.rollback().await?;
            return Err(WorkerError::LeaseLost);
        }
        let started_pass = research::start_pass(&mut tx, plan_id).await?;
        tx.commit().await?;
        if !started_pass {
            let reason = format!(
                "предел проходов исследования исчерпан ({} из {}): повторный запуск не выполняется",
                plan.passes, plan.max_passes
            );
            self.finish(
                bureau_id,
                job,
                plan_id,
                PlanOutcome {
                    status: ResearchPlanStatus::Partial,
                    provider: None,
                    model: None,
                    queries_made: 0,
                    results_seen: 0,
                    sources_fetched: 0,
                    sources_skipped: 0,
                    bytes_fetched: 0,
                    findings_rejected: 0,
                    duration_ms: Some(elapsed_ms(started)),
                    rejections: vec![reason.clone()],
                    diagnostic: Some(reason),
                },
            )
            .await?;
            return Ok(JobOutcome {
                status: ResearchPlanStatus::Partial,
                ..JobOutcome::default()
            });
        }

        let mut pass = Pass::new(
            started,
            self.config.research.limits.plan_time_budget,
            CostModel::from_settings(&self.config.research.costs),
            Some(self.search.describe().provider),
        );

        // --- search -------------------------------------------------------------------
        self.heartbeat(bureau_id, job, "building queries").await?;
        let queries = match otdel_research::plan_queries(
            &plan.question_text,
            plan.topic.as_deref(),
            &partner_name,
            self.config.research.limits.max_queries_per_plan as usize,
        ) {
            Ok(queries) => queries,
            Err(refusal) => {
                // Nothing has been reserved or sent. The refusal is journalled as a query
                // that was never made, so "why did this plan search nothing?" is visible.
                let reason = refusal.to_string();
                let mut tx = self.db.begin_scoped(bureau_id).await?;
                research::record_query(
                    &mut tx,
                    plan_id,
                    1,
                    &plan.question_text,
                    &self.search.describe().provider,
                    0,
                    0,
                    QueryOutcome::Refused,
                    Some(&reason),
                )
                .await?;
                tx.commit().await?;

                self.finish(
                    bureau_id,
                    job,
                    plan_id,
                    PlanOutcome {
                        status: ResearchPlanStatus::Failed,
                        provider: Some(self.search.describe().provider),
                        model: None,
                        queries_made: 0,
                        results_seen: 0,
                        sources_fetched: 0,
                        sources_skipped: 0,
                        bytes_fetched: 0,
                        findings_rejected: 0,
                        duration_ms: Some(elapsed_ms(started)),
                        rejections: vec![reason.clone()],
                        diagnostic: Some(reason),
                    },
                )
                .await?;
                return Ok(JobOutcome {
                    status: ResearchPlanStatus::Failed,
                    ..JobOutcome::default()
                });
            }
        };

        let max_sources = self.config.research.limits.max_sources_per_plan;
        for (index, query) in queries.iter().enumerate() {
            // Enough already found to fill the page budget: searching further would pay
            // for results nobody will ever be allowed to open
            // (`docs/block-01-spec.md` §6.5, "останавливается при достаточном покрытии").
            if pass.sources_discovered >= max_sources {
                pass.note(format!(
                    "поиск остановлен: найдено достаточно источников для лимита в {max_sources} \
                     страниц"
                ));
                break;
            }
            // Money stops *searching* here: a search is always chargeable. Reading is
            // governed by its own reservation, so a free fetch of a page already found is
            // not cancelled by a budget that ran out of searches.
            if pass.stopped_for_budget || self.should_stop(bureau_id, plan_id, &mut pass).await? {
                break;
            }
            self.heartbeat(bureau_id, job, "searching").await?;
            self.run_one_query(bureau_id, &plan, plan_id, index + 1, query, &mut pass)
                .await?;
        }

        // --- fetch --------------------------------------------------------------------
        let mut tx = self.db.begin_scoped(bureau_id).await?;
        let discovered = research_read::pending_sources(&mut tx, plan_id).await?;
        tx.commit().await?;

        for source in discovered {
            if pass.sources_fetched >= self.config.research.limits.max_sources_per_plan {
                self.skip_source(
                    bureau_id,
                    source.id,
                    SourceStatus::SkippedLimit,
                    "достигнут предел числа читаемых страниц на одно исследование",
                    &mut pass,
                )
                .await?;
                continue;
            }
            if self.should_stop(bureau_id, plan_id, &mut pass).await? {
                self.skip_source(
                    bureau_id,
                    source.id,
                    SourceStatus::SkippedLimit,
                    &pass.stop_reason.clone().unwrap_or_else(|| {
                        "исследование остановлено до чтения этой страницы".to_owned()
                    }),
                    &mut pass,
                )
                .await?;
                continue;
            }

            self.heartbeat(bureau_id, job, "reading a source").await?;
            self.fetch_one_source(bureau_id, plan_id, &source.id, &source.url, &mut pass)
                .await?;
        }

        // --- interpret ----------------------------------------------------------------
        let mut tx = self.db.begin_scoped(bureau_id).await?;
        let fetched = research_read::fetched_sources_with_text(&mut tx, plan_id).await?;
        tx.commit().await?;

        let catalog = ExternalCatalog::build(
            fetched
                .into_iter()
                .map(|(source, text)| ExternalSource {
                    source_id: source.id,
                    url: source.url,
                    host: source.host,
                    title: source.title,
                    retrieved_at: source.retrieved_at,
                    content_hash: source.content_hash,
                    license: source.license,
                    text,
                })
                .collect(),
        );

        if catalog.is_empty() {
            let status = if pass.stopped_for_budget {
                ResearchPlanStatus::BudgetExhausted
            } else if pass.cancelled {
                ResearchPlanStatus::Cancelled
            } else {
                ResearchPlanStatus::Partial
            };
            pass.note(
                "ни один источник не прочитан: выводов по этому вопросу не сформировано".to_owned(),
            );
            self.finish(
                bureau_id,
                job,
                plan_id,
                pass.outcome(status, None, None, started),
            )
            .await?;
            return Ok(pass.job_outcome(status, 0));
        }

        // Interpretation is the most expensive step of the pass — several model calls —
        // so it gets the same checkpoint every other chargeable step gets. Without this,
        // pressing "Остановить" while the last page was downloading would stop the
        // *cheap* half and then run the expensive one anyway, and a pass already past its
        // wall-clock budget would ignore it here.
        if self.should_stop(bureau_id, plan_id, &mut pass).await? {
            let status = if pass.cancelled {
                ResearchPlanStatus::Cancelled
            } else {
                ResearchPlanStatus::Partial
            };
            pass.note(
                "источники прочитаны, но выводы не формировались: исследование остановлено \
                 до обращения к модели"
                    .to_owned(),
            );
            self.finish(
                bureau_id,
                job,
                plan_id,
                pass.outcome(status, None, None, started),
            )
            .await?;
            return Ok(pass.job_outcome(status, 0));
        }

        // Interpreting costs money too, and it is reserved *before* the first call like
        // everything else — not reserving would make "израсходовано" a number that omits
        // the most expensive part of a pass. The amount is the number of requests this
        // catalogue will really produce, computed by the same batching the interpretation
        // runs: reserving `max_requests_per_run` instead would refuse a plan with one
        // source unless it could afford eight calls.
        let planned_calls = otdel_research::planned_requests(
            &catalog,
            &self.finding_limits,
            self.config.llm.limits.max_requests_per_run,
        );
        let model_reservation = self
            .reserve(
                bureau_id,
                plan_id,
                SpendKind::Model,
                pass.costs
                    .model_call_micros
                    .saturating_mul(i64::from(planned_calls)),
                &mut pass,
            )
            .await?;
        let Some(model_reservation) = model_reservation else {
            // The sources stay: they were paid for and really read, and the next pass
            // starts from a question that already has a journal.
            pass.note(
                "источники прочитаны, но выводы не формировались: бюджета на обращение к \
                 модели не осталось"
                    .to_owned(),
            );
            self.finish(
                bureau_id,
                job,
                plan_id,
                pass.outcome(ResearchPlanStatus::BudgetExhausted, None, None, started),
            )
            .await?;
            return Ok(pass.job_outcome(ResearchPlanStatus::BudgetExhausted, 0));
        };

        self.heartbeat(bureau_id, job, "interpreting sources")
            .await?;
        let context = ResearchContext {
            question: plan.question_text.clone(),
            // The topic is 1C's own wording of a gap found in the *partner's* document, so
            // it can carry the partner's name. It never goes to the search provider
            // (`plan_queries` refuses such a query) and it must not go to the model
            // either: a third party learning which company this bureau works on is the
            // same disclosure whichever provider learns it.
            topic: plan
                .topic
                .clone()
                .filter(|topic| otdel_research::names_partner(topic, &partner_name).is_none()),
            sources_total: catalog.len(),
        };

        let interpreted = interpret_sources(
            self.model.as_ref(),
            &catalog,
            &context,
            &self.finding_limits,
            self.config.llm.limits.max_requests_per_run,
            self.config.llm.limits.max_output_tokens,
        )
        .await;

        let interpreted = match interpreted {
            Ok(interpreted) => interpreted,
            Err(error) => {
                let (status, worker_error) = match &error {
                    ResearchError::ProviderNotConfigured(message) => (
                        ResearchPlanStatus::NeedsProvider,
                        WorkerError::ProviderNotConfigured(message.clone()),
                    ),
                    ResearchError::NoReadableSources => (
                        ResearchPlanStatus::Partial,
                        WorkerError::ResearchStopped {
                            reason: error.to_string(),
                            permanent: true,
                        },
                    ),
                    ResearchError::ProviderFailed {
                        diagnostic,
                        retryable,
                    } => (
                        ResearchPlanStatus::Failed,
                        WorkerError::ModelCallFailed {
                            diagnostic: diagnostic.clone(),
                            retryable: *retryable,
                        },
                    ),
                };
                // The reservation is settled either way: a failed pass may still have
                // made calls before it failed, and money held for calls that will never
                // happen has to go back rather than wait for the maintenance sweep.
                self.settle_model(bureau_id, &model_reservation, 0, &mut pass)
                    .await?;
                // The sources stay: they were paid for and really read, and the next pass
                // starts from a question that already has a journal.
                self.finish(
                    bureau_id,
                    job,
                    plan_id,
                    pass.outcome(status, None, Some(error.to_string()), started),
                )
                .await?;
                return Err(worker_error);
            }
        };

        self.settle_model(
            bureau_id,
            &model_reservation,
            interpreted.requests_made,
            &mut pass,
        )
        .await?;

        for reason in &interpreted.draft.rejections {
            pass.note(reason.clone());
        }
        if let Some(not_found) = &interpreted.draft.not_found {
            pass.note(format!("модель: {not_found}"));
        }
        pass.findings_rejected += interpreted.draft.rejected;

        // Storing the conclusions and closing the plan happen together, and only if this
        // worker still holds the job. The model call takes as long as it takes; if the
        // lease expired meanwhile, somebody else may already be researching the same
        // question, and writing now would replace their result with ours — the "late
        // result of an old run overwrites a newer one" failure `docs/block-01-spec.md` §7
        // forbids.
        let findings: Vec<NewFinding> = interpreted
            .draft
            .findings
            .iter()
            .map(|finding| NewFinding {
                topic: finding.topic.clone(),
                attribute: finding.attribute.clone(),
                value_text: finding.value_text.clone(),
                unit: finding.unit.clone(),
                conditions: finding.conditions.clone(),
                model_context: finding.model_context.clone(),
                evidence: finding
                    .evidence
                    .iter()
                    .map(|evidence| NewFindingEvidence {
                        source_id: evidence.source_id,
                        quote: evidence.quote.clone(),
                        char_start: evidence.char_start,
                        char_end: evidence.char_end,
                    })
                    .collect(),
            })
            .collect();

        let mut tx = self.db.begin_scoped(bureau_id).await?;
        let still_ours = jobs::heartbeat(
            &mut tx,
            job.id,
            &self.owner,
            self.config.extraction.lease_duration,
            Some("storing the conclusions"),
        )
        .await?;
        if !still_ours {
            tx.rollback().await?;
            return Err(WorkerError::LeaseLost);
        }

        let stored =
            research::replace_findings(&mut tx, plan.partner_id, plan_id, &findings).await?;

        // `completed` has to mean "this pass did everything it set out to do". A pass cut
        // short by the clock did not, even if nothing was skipped after the cut — so
        // `timed_out` belongs here next to the other two stop conditions.
        let status = if pass.cancelled {
            ResearchPlanStatus::Cancelled
        } else if pass.stopped_for_budget {
            ResearchPlanStatus::BudgetExhausted
        } else if pass.timed_out
            || pass.findings_rejected > 0
            || pass.sources_skipped > 0
            || interpreted.sources_skipped > 0
        {
            ResearchPlanStatus::Partial
        } else {
            ResearchPlanStatus::Completed
        };

        research::finish_plan(
            &mut tx,
            plan_id,
            &pass.outcome(status, interpreted.model.clone(), None, started),
        )
        .await?;
        tx.commit().await?;

        info!(
            plan_id = %plan_id,
            status = status.as_str(),
            queries = pass.queries_made,
            sources_fetched = pass.sources_fetched,
            sources_skipped = pass.sources_skipped,
            findings = stored,
            rejected = pass.findings_rejected,
            micros_spent = pass.micros_spent,
            "industry research finished"
        );

        Ok(pass.job_outcome(status, u32::try_from(stored).unwrap_or(0)))
    }

    /// One search request, with its reservation, its settlement and its journal entry.
    async fn run_one_query(
        &self,
        bureau_id: Uuid,
        plan: &ResearchPlan,
        plan_id: Uuid,
        ordinal: usize,
        query: &str,
        pass: &mut Pass,
    ) -> Result<(), WorkerError> {
        let cost = pass.costs.search_micros;
        let Some(reservation) = self
            .reserve(bureau_id, plan_id, SpendKind::Search, cost, pass)
            .await?
        else {
            return Ok(());
        };

        let answer = self
            .search
            .search(&SearchRequest {
                query: query.to_owned(),
                max_results: self.config.research.limits.max_results_per_query,
            })
            .await;

        let (state, note, outcome, hits, diagnostic) = match answer {
            Ok(answer) => (
                SpendState::Settled,
                None,
                QueryOutcome::Ok,
                answer.hits,
                None,
            ),
            Err(error) => {
                let diagnostic = error.diagnostic();
                let (state, outcome) = if !error.was_sent() {
                    (SpendState::Released, QueryOutcome::Failed)
                } else if matches!(error, otdel_search::SearchError::UnknownOutcome) {
                    // Sent, no answer: charged, and flagged for reconciliation.
                    (SpendState::Unknown, QueryOutcome::Unknown)
                } else {
                    (SpendState::Settled, QueryOutcome::Failed)
                };
                (
                    state,
                    Some(diagnostic.clone()),
                    outcome,
                    Vec::new(),
                    Some(diagnostic),
                )
            }
        };

        let charged = matches!(state, SpendState::Settled | SpendState::Unknown);
        let mut tx = self.db.begin_scoped(bureau_id).await?;
        research::settle(&mut tx, &reservation, state, note.as_deref()).await?;
        let query_id = research::record_query(
            &mut tx,
            plan_id,
            i32::try_from(ordinal).unwrap_or(i32::MAX),
            query,
            &self.search.describe().provider,
            i32::try_from(hits.len()).unwrap_or(i32::MAX),
            if charged { cost } else { 0 },
            outcome,
            diagnostic.as_deref(),
        )
        .await?;

        pass.queries_made += 1;
        pass.results_seen += u32::try_from(hits.len()).unwrap_or(0);
        if charged {
            pass.micros_spent = pass.micros_spent.saturating_add(cost);
        }
        if let Some(diagnostic) = &diagnostic {
            pass.note(format!("поиск: {diagnostic}"));
        }

        // Every result becomes a row, including the ones that will never be opened. A
        // journal that silently drops them would leave the owner believing the search
        // found nothing.
        for hit in hits {
            // The guard runs here, on the provider's raw string, and its result is the
            // only URL that is ever stored: `NormalisedUrl` cannot be built from anything
            // that is not a plain public `https://host/path`.
            let url = match NormalisedUrl::parse(&hit.url) {
                Ok(url) => url,
                // Not a URL this system will ever open — a `file://`, an IP literal, a
                // port, a control character. There is nothing to journal about it except
                // that it was refused, and that reason goes on the plan.
                Err(rejection) => {
                    pass.note(format!("ссылка отклонена: {rejection}"));
                    continue;
                }
            };

            let host = url.host().to_owned();
            let (status, reason) = if self.config.research.allowed_hosts.allows(&host) {
                (SourceStatus::Discovered, None)
            } else {
                (
                    SourceStatus::SkippedHost,
                    Some(format!(
                        "хост `{host}` не входит в список разрешённых источников"
                    )),
                )
            };
            let url_hash = url.digest();
            let url = url.as_str().to_owned();

            if status != SourceStatus::Discovered {
                pass.sources_skipped += 1;
                if let Some(reason) = &reason {
                    pass.note(reason.clone());
                }
            }

            let stored = research::record_source(
                &mut tx,
                plan.partner_id,
                plan_id,
                &NewSource {
                    query_id: Some(query_id),
                    url,
                    url_hash,
                    host,
                    title: hit.title,
                    snippet: hit.snippet,
                    status,
                    diagnostic: reason,
                },
            )
            .await?;

            // A result this plan may still open. `None` means the same document came
            // back from an earlier query and is already counted.
            if stored.is_some() && status == SourceStatus::Discovered {
                pass.sources_discovered += 1;
            }
        }
        tx.commit().await?;

        Ok(())
    }

    /// One document, with its reservation, its settlement and its journal entry.
    async fn fetch_one_source(
        &self,
        bureau_id: Uuid,
        plan_id: Uuid,
        source_id: &Uuid,
        url: &str,
        pass: &mut Pass,
    ) -> Result<(), WorkerError> {
        let Ok(parsed) = NormalisedUrl::parse(url) else {
            self.skip_source(
                bureau_id,
                *source_id,
                SourceStatus::Failed,
                "сохранённая ссылка больше не проходит проверку",
                pass,
            )
            .await?;
            return Ok(());
        };

        let cost = pass.costs.fetch_micros;
        let Some(reservation) = self
            .reserve(bureau_id, plan_id, SpendKind::Fetch, cost, pass)
            .await?
        else {
            self.skip_source(
                bureau_id,
                *source_id,
                SourceStatus::SkippedLimit,
                &pass
                    .stop_reason
                    .clone()
                    .unwrap_or_else(|| "бюджет исчерпан".to_owned()),
                pass,
            )
            .await?;
            return Ok(());
        };

        let fetched = self.fetcher.fetch(&parsed).await;

        let (state, outcome) = match fetched {
            Ok(document) => {
                pass.sources_fetched += 1;
                pass.bytes_fetched = pass
                    .bytes_fetched
                    .saturating_add(i64::try_from(document.bytes_len).unwrap_or(i64::MAX));
                if document.truncated {
                    pass.note(format!(
                        "страница {} сохранена не полностью: достигнут предел символов",
                        parsed.host()
                    ));
                }
                (
                    SpendState::Settled,
                    SourceOutcome {
                        status: SourceStatus::Fetched,
                        http_status: Some(i32::from(document.http_status)),
                        content_type: document.content_type,
                        content_bytes: Some(i64::try_from(document.bytes_len).unwrap_or(i64::MAX)),
                        content_hash: Some(document.content_hash),
                        text_content: Some(document.text),
                        license: document.license,
                        license_note: document.license_note,
                        retrieved_at: Some(document.retrieved_at),
                        published_at: document.published_at,
                        cost_micros: cost,
                        diagnostic: None,
                    },
                )
            }
            Err(refusal) => {
                let diagnostic = refusal.diagnostic();
                pass.sources_skipped += 1;
                pass.note(format!("источник {}: {diagnostic}", parsed.host()));

                let status = match &refusal {
                    FetchRefusal::HostNotAllowed(_) => SourceStatus::SkippedHost,
                    // Only the site actually saying no is `skipped_robots`. A robots.txt
                    // that could not be read is a *failure to check*, and labelling it
                    // "запрещено robots.txt" would put words in the publisher's mouth.
                    FetchRefusal::RobotsDisallowed => SourceStatus::SkippedRobots,
                    FetchRefusal::UnsupportedType(_) => SourceStatus::SkippedType,
                    _ => SourceStatus::Failed,
                };
                let state = if refusal.was_sent() {
                    SpendState::Settled
                } else {
                    SpendState::Released
                };
                let http_status = match &refusal {
                    FetchRefusal::Http { status, .. } | FetchRefusal::Redirected { status } => {
                        Some(i32::from(*status))
                    }
                    _ => None,
                };

                (
                    state,
                    SourceOutcome {
                        status,
                        http_status,
                        content_type: None,
                        content_bytes: None,
                        content_hash: None,
                        text_content: None,
                        license: None,
                        license_note: None,
                        retrieved_at: None,
                        published_at: None,
                        cost_micros: if refusal.was_sent() { cost } else { 0 },
                        diagnostic: Some(diagnostic),
                    },
                )
            }
        };

        if matches!(state, SpendState::Settled | SpendState::Unknown) {
            pass.micros_spent = pass.micros_spent.saturating_add(cost);
        }

        let mut tx = self.db.begin_scoped(bureau_id).await?;
        research::settle(&mut tx, &reservation, state, outcome.diagnostic.as_deref()).await?;
        research::finish_source(&mut tx, *source_id, &outcome).await?;
        tx.commit().await?;

        Ok(())
    }

    /// Record a source that was never opened, with the reason.
    async fn skip_source(
        &self,
        bureau_id: Uuid,
        source_id: Uuid,
        status: SourceStatus,
        reason: &str,
        pass: &mut Pass,
    ) -> Result<(), WorkerError> {
        pass.sources_skipped += 1;
        pass.note(reason.to_owned());

        let mut tx = self.db.begin_scoped(bureau_id).await?;
        research::finish_source(
            &mut tx,
            source_id,
            &SourceOutcome {
                status,
                http_status: None,
                content_type: None,
                content_bytes: None,
                content_hash: None,
                text_content: None,
                license: None,
                license_note: None,
                retrieved_at: None,
                published_at: None,
                cost_micros: 0,
                diagnostic: Some(reason.to_owned()),
            },
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Close the interpretation phase's reservation at what it really used.
    ///
    /// The reservation covered `max_requests_per_run` calls; `requests_made` is how many
    /// happened. The difference goes back to the budget in the same statement that records
    /// the spend, so no pass can leave money held for calls it decided not to make.
    async fn settle_model(
        &self,
        bureau_id: Uuid,
        reservation: &Reservation,
        requests_made: u32,
        pass: &mut Pass,
    ) -> Result<(), WorkerError> {
        let actual = pass
            .costs
            .model_call_micros
            .saturating_mul(i64::from(requests_made))
            .clamp(0, reservation.amount_micros);

        let mut tx = self.db.begin_scoped(bureau_id).await?;
        research::settle_amount(
            &mut tx,
            reservation,
            actual,
            SpendState::Settled,
            Some(&format!("обращений к модели: {requests_made}")),
        )
        .await?;
        tx.commit().await?;

        pass.micros_spent = pass.micros_spent.saturating_add(actual);
        Ok(())
    }

    /// Hold money for one call, or record why it cannot be held.
    async fn reserve(
        &self,
        bureau_id: Uuid,
        plan_id: Uuid,
        kind: SpendKind,
        amount: i64,
        pass: &mut Pass,
    ) -> Result<Option<Reservation>, WorkerError> {
        let limit =
            i64::try_from(self.config.research.costs.bureau_budget_micros).unwrap_or(i64::MAX);

        let mut tx = self.db.begin_scoped(bureau_id).await?;
        let outcome = research::reserve(&mut tx, plan_id, kind, amount, limit).await?;
        tx.commit().await?;

        match outcome {
            ReserveOutcome::Granted(reservation) => Ok(Some(reservation)),
            ReserveOutcome::Refused(reason) => {
                pass.stopped_for_budget = true;
                pass.stop_reason = Some(reason.clone());
                pass.note(reason);
                Ok(None)
            }
        }
    }

    /// Is this pass over? Checked before every step, never in the middle of one.
    ///
    /// Money is deliberately *not* one of the conditions here. Affordability is decided
    /// per call by [`research::reserve`], which knows what that particular call costs; a
    /// blanket "the budget ran out" would also stop the steps that cost nothing, and
    /// throw away pages this plan has already paid a search to find.
    async fn should_stop(
        &self,
        bureau_id: Uuid,
        plan_id: Uuid,
        pass: &mut Pass,
    ) -> Result<bool, WorkerError> {
        if pass.cancelled {
            return Ok(true);
        }
        if pass.out_of_time() {
            let reason = format!(
                "исчерпано время на одно исследование ({} с)",
                pass.time_budget.as_secs()
            );
            pass.stop_reason = Some(reason.clone());
            pass.note(reason);
            pass.timed_out = true;
            return Ok(true);
        }

        let mut tx = self.db.begin_scoped(bureau_id).await?;
        let cancelled = research::cancel_requested(&mut tx, plan_id).await?;
        tx.commit().await?;
        if cancelled {
            pass.cancelled = true;
            let reason = "исследование остановлено владельцем".to_owned();
            pass.stop_reason = Some(reason.clone());
            pass.note(reason);
        }
        Ok(cancelled)
    }

    /// Settle the plan — but only while this worker still holds the job.
    ///
    /// The lease check and the write are in **one transaction**, so there is no window
    /// between them. Without this, a worker that stalled past its lease could wake up
    /// after another worker had already finished the same plan and overwrite a
    /// `completed` run with its own zeroed counters — the "late result of an old run
    /// overwrites a newer one" failure `docs/block-01-spec.md` §7 forbids, and the reason
    /// the storing path below takes the same precaution.
    async fn finish(
        &self,
        bureau_id: Uuid,
        job: &Job,
        plan_id: Uuid,
        outcome: PlanOutcome,
    ) -> Result<(), WorkerError> {
        let mut tx = self.db.begin_scoped(bureau_id).await?;
        let still_ours = jobs::heartbeat(
            &mut tx,
            job.id,
            &self.owner,
            self.config.extraction.lease_duration,
            Some("settling the plan"),
        )
        .await?;
        if !still_ours {
            tx.rollback().await?;
            return Err(WorkerError::LeaseLost);
        }
        research::finish_plan(&mut tx, plan_id, &outcome).await?;
        tx.commit().await?;
        Ok(())
    }
}

/// Counters and stop conditions of one pass.
#[derive(Debug)]
struct Pass {
    deadline: Instant,
    time_budget: Duration,
    costs: CostModel,
    /// Which search adapter this pass used, recorded on the plan when it settles.
    provider: Option<String>,
    queries_made: u32,
    results_seen: u32,
    sources_fetched: u32,
    sources_skipped: u32,
    /// Results that became a source this plan may still open. Once this reaches the page
    /// limit there is nothing more to search for.
    sources_discovered: u32,
    bytes_fetched: i64,
    findings_rejected: u32,
    micros_spent: i64,
    cancelled: bool,
    stopped_for_budget: bool,
    timed_out: bool,
    stop_reason: Option<String>,
    notes: Vec<String>,
}

/// Upper bound on the reasons kept on a plan. They repeat, and a plan is not a log file.
const MAX_NOTES: usize = 40;

impl Pass {
    fn new(
        started: Instant,
        time_budget: Duration,
        costs: CostModel,
        provider: Option<String>,
    ) -> Self {
        Self {
            deadline: started + time_budget,
            time_budget,
            costs,
            provider,
            queries_made: 0,
            results_seen: 0,
            sources_fetched: 0,
            sources_skipped: 0,
            sources_discovered: 0,
            bytes_fetched: 0,
            findings_rejected: 0,
            micros_spent: 0,
            cancelled: false,
            stopped_for_budget: false,
            timed_out: false,
            stop_reason: None,
            notes: Vec::new(),
        }
    }

    fn out_of_time(&self) -> bool {
        Instant::now() >= self.deadline
    }

    fn note(&mut self, reason: String) {
        if self.notes.len() >= MAX_NOTES || self.notes.contains(&reason) {
            return;
        }
        self.notes.push(reason);
    }

    fn outcome(
        &self,
        status: ResearchPlanStatus,
        model: Option<String>,
        diagnostic: Option<String>,
        started: Instant,
    ) -> PlanOutcome {
        PlanOutcome {
            status,
            // Which search adapter and which model produced this plan's sources and
            // conclusions: a later pass with a different provider must be
            // distinguishable from an older one (`docs/block-01-spec.md` §6.1).
            provider: self.provider.clone(),
            model,
            queries_made: i32::try_from(self.queries_made).unwrap_or(i32::MAX),
            results_seen: i32::try_from(self.results_seen).unwrap_or(i32::MAX),
            sources_fetched: i32::try_from(self.sources_fetched).unwrap_or(i32::MAX),
            sources_skipped: i32::try_from(self.sources_skipped).unwrap_or(i32::MAX),
            bytes_fetched: self.bytes_fetched,
            findings_rejected: i32::try_from(self.findings_rejected).unwrap_or(i32::MAX),
            duration_ms: Some(elapsed_ms(started)),
            rejections: self.notes.clone(),
            diagnostic: diagnostic.or_else(|| self.stop_reason.clone()),
        }
    }

    fn job_outcome(&self, status: ResearchPlanStatus, findings_stored: u32) -> JobOutcome {
        JobOutcome {
            status,
            queries_made: self.queries_made,
            sources_fetched: self.sources_fetched,
            findings_stored,
            findings_rejected: self.findings_rejected,
            micros_spent: self.micros_spent,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct JobOutcome {
    status: ResearchPlanStatus,
    queries_made: u32,
    sources_fetched: u32,
    findings_stored: u32,
    findings_rejected: u32,
    micros_spent: i64,
}

impl Default for JobOutcome {
    fn default() -> Self {
        Self {
            status: ResearchPlanStatus::Partial,
            queries_made: 0,
            sources_fetched: 0,
            findings_stored: 0,
            findings_rejected: 0,
            micros_spent: 0,
        }
    }
}

fn elapsed_ms(started: Instant) -> i64 {
    i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass() -> Pass {
        Pass::new(
            Instant::now(),
            Duration::from_secs(300),
            CostModel {
                search_micros: 5_000,
                fetch_micros: 0,
                model_call_micros: 2_000,
            },
            Some("fake".to_owned()),
        )
    }

    #[test]
    fn notes_are_deduplicated_and_bounded() {
        let mut pass = pass();
        for _ in 0..5 {
            pass.note("хост не входит в список разрешённых источников".to_owned());
        }
        assert_eq!(pass.notes.len(), 1, "the same reason is recorded once");

        for index in 0..100 {
            pass.note(format!("причина {index}"));
        }
        assert_eq!(pass.notes.len(), MAX_NOTES);
    }

    #[test]
    fn a_pass_with_no_time_left_is_over() {
        let expired = Pass::new(
            Instant::now() - Duration::from_secs(600),
            Duration::from_secs(300),
            CostModel {
                search_micros: 0,
                fetch_micros: 0,
                model_call_micros: 0,
            },
            Some("fake".to_owned()),
        );
        assert!(expired.out_of_time());
        assert!(!pass().out_of_time());
    }

    #[test]
    fn the_outcome_carries_the_stop_reason_when_there_is_no_other_diagnostic() {
        let mut pass = pass();
        pass.stopped_for_budget = true;
        pass.stop_reason = Some("бюджет бюро исчерпан".to_owned());
        pass.queries_made = 2;
        pass.sources_skipped = 3;

        let started = Instant::now();
        let outcome = pass.outcome(ResearchPlanStatus::BudgetExhausted, None, None, started);
        assert_eq!(outcome.status, ResearchPlanStatus::BudgetExhausted);
        assert_eq!(outcome.diagnostic.as_deref(), Some("бюджет бюро исчерпан"));
        assert_eq!(outcome.queries_made, 2);
        assert_eq!(outcome.sources_skipped, 3);

        // An explicit diagnostic wins over the stop reason.
        let explicit = pass.outcome(
            ResearchPlanStatus::Failed,
            None,
            Some("модель ответила ошибкой".to_owned()),
            started,
        );
        assert_eq!(
            explicit.diagnostic.as_deref(),
            Some("модель ответила ошибкой")
        );
    }

    #[test]
    fn a_report_of_one_pass_starts_empty() {
        let report = ResearchReport::default();
        assert_eq!(report.jobs_claimed, 0);
        assert_eq!(report.micros_spent, 0);
        assert_eq!(report.plans_awaiting_provider, 0);
    }
}
