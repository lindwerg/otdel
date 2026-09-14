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
use otdel_core::passport::{CoverageState, RequirementsState};
use otdel_core::updates::{EventActor, EventKind};
use otdel_db::knowledge::{self, RunOutcome};
use otdel_db::passport;
use otdel_db::{
    events, jobs, materials, pages, partners, publication, publication_read, updates, Database,
};
use otdel_knowledge::{
    draft_knowledge, evaluate_requirements, tables, CoveragePlan, DraftLimits, KnowledgeError,
    PromptContext, RunContext, SourceCatalog, SourcePage, StructuredCell, TableContext,
    TableReading, PROMPT_PROFILE,
};
use otdel_llm::{LlmProvider, ProviderDescription};
use tracing::{info, warn};
use uuid::Uuid;

use crate::error::WorkerError;
use crate::knowledge_draft::{
    collect_uncertainties, page_coverage_rows, run_pass_rows, to_new_draft,
    topics_whose_pass_covered_the_material,
};
use crate::material_of;

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
    /// R05 — pages this pass left for a later one because a budget was reached. Reported
    /// beside the successes: a pass that "completed" ten jobs and deferred two hundred
    /// pages has not read the partner's catalogues, and the report should say so.
    pub pages_deferred: u32,
    /// R05 — runs whose draft does not carry what a passport needs. These are exactly the
    /// runs a person still has to look at before anything is published from them.
    pub runs_below_requirements: u32,
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
                    report.pages_deferred += outcome.pages_deferred;
                    if !outcome.requirements_met {
                        report.runs_below_requirements += 1;
                    }
                    self.settle(bureau_id, &job, None).await?;
                    report.jobs_completed += 1;
                }
                Err(error) => {
                    if matches!(error, WorkerError::ProviderNotConfigured(_)) {
                        report.runs_awaiting_provider += 1;
                    }
                    warn!(
                        job_id = %job.id,
                        material_id = ?job.material_id,
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
        let material_id = material_of(job)?;
        // The material and its pages, inside this bureau and this partner. A job that
        // named somebody else's material would find nothing here.
        let mut tx = self.db.begin_scoped(bureau_id).await?;
        let stored = materials::get_in_partner(&mut tx, job.partner_id, material_id).await?;
        let Some(stored) = stored else {
            tx.commit().await?;
            return Err(WorkerError::MaterialMissing);
        };
        let partner = partners::get(&mut tx, job.partner_id)
            .await?
            .map_or_else(|| "партнёр".to_owned(), |partner| partner.name);
        let readable = pages::readable_with_text(&mut tx, material_id).await?;
        // R05: *every* page of the material, not only the offerable ones. This list is
        // the denominator — the thing the audited run never had, which is how eight pages
        // of a forty-four page catalogue left the account without anybody noticing.
        let inventory = pages::list_for_material(&mut tx, material_id).await?;
        // R05: R03's table cells, at last consumed. Established rows become structured
        // context in the prompt; everything else becomes an uncertainty and never a fact.
        let table_cells = pages::table_cells_for_material(&mut tx, material_id).await?;
        let run =
            knowledge::start_run(&mut tx, job.partner_id, material_id, PROMPT_PROFILE).await?;
        // Phase 1F: which reading of the document this draft is about to be made from.
        // Recorded before the model is called, so a document re-read while this run is in
        // flight leaves the draft carrying the older number — which is what it is, and
        // what makes "перечитан после разбора" detectable afterwards.
        updates::set_draft_source_revision(&mut tx, run.id, stored.material.content_revision)
            .await?;
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

        // The plan is built before the model is called and settled after, so a run that
        // fails halfway still leaves an account naming every page and why it is not in
        // the draft. A run with no account is refused by the publication gate, but a run
        // whose account says «страница 37 ждёт распознавания» can be acted on.
        let offerable: Vec<Uuid> = catalog
            .entries()
            .iter()
            .map(|entry| entry.page.page_id)
            .collect();
        let mut plan = CoveragePlan::build(&inventory, &offerable);

        let readings: Vec<(Uuid, i32, TableReading)> = table_cells
            .iter()
            .map(|(page_id, page_number, cells)| {
                (
                    *page_id,
                    *page_number,
                    tables::partition(*page_id, *page_number, cells),
                )
            })
            .collect();
        let table_context = TableContext::from_readings(
            readings
                .iter()
                .map(|(page_id, _, reading)| (*page_id, reading)),
        );
        let structured: Vec<StructuredCell> = readings
            .iter()
            .flat_map(|(_, _, reading)| reading.structured.iter().cloned())
            .collect();

        let description = self.provider.describe();
        let result = draft_knowledge(
            self.provider.as_ref(),
            &catalog,
            &context,
            &table_context,
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

                // R05: a run that never reached the model still knows what the material
                // is made of. Settling with nothing processed leaves every page carrying
                // the reason it was not, which is the difference between "нечего было
                // разбирать" and "разбор не состоялся, вот что осталось непрочитанным".
                plan.settle(&[], &[]);
                let mut coverage = plan.summarise();
                coverage.requirements = RequirementsState::Unknown;

                let mut tx = self.db.begin_scoped(bureau_id).await?;
                passport::record_page_coverage(
                    &mut tx,
                    job.partner_id,
                    material_id,
                    run.id,
                    &page_coverage_rows(&plan),
                )
                .await?;
                // The tables were read by R03 and the unreadable pages are unreadable
                // whatever the model did, so both findings survive a failed pass. A run
                // that could not reach the provider still knows that page 37 holds a load
                // table nobody could read, and that is worth more than a clean slate.
                passport::record_uncertainties(
                    &mut tx,
                    job.partner_id,
                    material_id,
                    run.id,
                    &collect_uncertainties(&readings, &plan),
                )
                .await?;
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
                        coverage,
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
        // R05, all before the transaction opens because none of it needs one:
        //
        //   * settle the page account against what the run actually did;
        //   * judge the draft against the requirements a passport has to carry;
        //   * ask each accepted fact whether a usable table cell says the same thing.
        //
        // The requirement verdict is computed from the draft in memory rather than from
        // the rows after storage on purpose: it has to be the *same* rule in both places,
        // and `CandidateDraft::snapshot` is the only way to build the rule's input from a
        // draft that has not been stored yet.
        plan.settle(&drafted.processed, &drafted.deferred);
        let mut coverage = plan.summarise();
        let new_draft = to_new_draft(&drafted.draft, &structured);
        let uncertainties = collect_uncertainties(&readings, &plan);

        // The requirement check is given the run's unsettled readings and — R05.2 — which
        // purpose-specific passes actually got through the material. The second is what
        // makes "0 terms" answerable: a glossary pass that read every page and found none
        // has observed something, and one that ran out of budget has not. Without it the
        // only way to close the topic was a sentence, which is how a self-serving
        // declaration came to clear a catalogue nobody had examined for terms.
        let context = RunContext {
            pages_processed: usize::try_from(coverage.pages_processed).unwrap_or(0),
            open_uncertainties: uncertainties.len(),
            purposes_covered: topics_whose_pass_covered_the_material(&drafted.passes),
        };
        let requirements = evaluate_requirements(&drafted.draft.snapshot(context));
        coverage.requirements = requirements.state;
        coverage.requirements_missing = requirements.missing.clone();

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
            knowledge::replace_draft(&mut tx, job.partner_id, material_id, run.id, &new_draft)
                .await?;
        passport::record_page_coverage(
            &mut tx,
            job.partner_id,
            material_id,
            run.id,
            &page_coverage_rows(&plan),
        )
        .await?;
        passport::record_uncertainties(
            &mut tx,
            job.partner_id,
            material_id,
            run.id,
            &uncertainties,
        )
        .await?;
        // R05.2: what each purpose-specific pass covered, stored beside the
        // material-level account. The union answers "was this page read at all"; these
        // answer "read for what", which is the question «0 терминов» needs.
        passport::record_run_passes(
            &mut tx,
            job.partner_id,
            material_id,
            run.id,
            &run_pass_rows(&drafted.passes, offerable.len()),
        )
        .await?;
        // Derived last, from the candidates that exist after this draft replaced the
        // previous one. A proposal made from the old rows would point at products the
        // same transaction has just deleted.
        let identity = passport::propose_identity_links(&mut tx, job.partner_id).await?;

        // `completed` is unavailable while a page is still queued — the database says the
        // same thing, and this says it first so the run is not written twice. That rule
        // is exactly what the audited run broke: eight pages were never offered and the
        // run still reported success.
        let status = if drafted.draft.rejected > 0
            || drafted.pages_skipped > 0
            || coverage.state != CoverageState::Complete
        {
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
                coverage: coverage.clone(),
            },
        )
        .await?;

        events::record(
            &mut tx,
            &events::NewEvent::new(
                EventKind::UnderstandingFinished,
                EventActor::Worker,
                // R05: the denominator is in the sentence. «фактов 13» was reported as
                // success over a catalogue whose pages nobody had counted; «страниц
                // 36/44» in the same line makes that unreportable.
                format!(
                    "разбор материала «{}» завершён ({}): страниц {}/{}, фактов {}, задач {}, \
                     отклонено {}{}",
                    stored.material.filename,
                    status.as_str(),
                    coverage.pages_processed,
                    coverage.pages_total,
                    counts.facts,
                    counts.applications,
                    drafted.draft.rejected,
                    if requirements.state.is_met() {
                        String::new()
                    } else {
                        format!("; паспорту не хватает: {}", requirements.missing.len())
                    },
                ),
            )
            .for_partner(job.partner_id)
            .about_material(material_id)
            .about_run(run.id)
            .with_detail(serde_json::json!({
                "status": status.as_str(),
                "facts": counts.facts,
                "rejected": drafted.draft.rejected,
                "source_revision": stored.material.content_revision,
                "coverage_state": coverage.state.as_str(),
                "pages_total": coverage.pages_total,
                "pages_processed": coverage.pages_processed,
                "pages_deferred": coverage.pages_deferred,
                "pages_unreadable": coverage.pages_unreadable,
                "requirements_state": coverage.requirements.as_str(),
                "requirements_missing": coverage.requirements_missing,
                "applications": counts.applications,
                "declarations": counts.declarations,
                "uncertainties": uncertainties.len(),
                "identity_linked": identity.linked,
                "identity_unclear": identity.unclear,
            })),
        )
        .await?;

        // Phase 1F: the draft is the last stage a person had to start by hand.
        //
        // `block-01-spec.md` §11 — «нормальный путь не требует ручной работы между
        // этапами» — and until now the chain stopped here: extraction queued the draft,
        // and the draft queued nothing. A partner who uploaded a new catalogue got new
        // candidates and an unchanged published version, with no indication that the
        // remaining step was a button.
        //
        // Three things keep this from being a loop or a surprise:
        //
        //   * the check is queued only when there is something to check. A draft that
        //     stored nothing leaves the published version alone;
        //   * `validation_pending` means a check already queued or running is joined
        //     rather than re-armed, so two materials finishing together produce one
        //     check over both — which is also the only way a contradiction between them
        //     can be seen;
        //   * a check queues nothing in turn. The chain ends here, on purpose.
        //
        // The check itself decides whether anything is published. An unchanged candidate
        // set produces no new version (`version::decide`), so this cannot manufacture
        // versions out of repeated drafting.
        self.queue_check(&mut tx, job.partner_id, material_id)
            .await?;

        tx.commit().await?;

        info!(
            material_id = %material_id,
            status = status.as_str(),
            products = counts.products,
            facts = counts.facts,
            terms = counts.terms,
            gaps = counts.gaps,
            applications = counts.applications,
            declarations = counts.declarations,
            uncertainties = uncertainties.len(),
            coverage = coverage.state.as_str(),
            pages = format!("{}/{}", coverage.pages_processed, coverage.pages_total),
            requirements = coverage.requirements.as_str(),
            rejected = drafted.draft.rejected,
            requests = drafted.requests_made,
            "product knowledge drafted"
        );

        Ok(JobOutcome {
            facts_stored: u32::try_from(counts.facts).unwrap_or(0),
            rejected: drafted.draft.rejected,
            pages_deferred: u32::try_from(coverage.pages_deferred).unwrap_or(0),
            requirements_met: coverage.requirements.is_met(),
        })
    }

    /// Hand the partner to the checker, unless there is nothing to check or a check is
    /// already on its way.
    ///
    /// Runs in the caller's transaction, so the draft and the check that will read it
    /// commit together: a check queued for a draft that rolled back would read the
    /// previous candidates and publish a version the owner never asked for.
    async fn queue_check(
        &self,
        tx: &mut otdel_db::ScopedTx,
        partner_id: Uuid,
        material_id: Uuid,
    ) -> Result<(), WorkerError> {
        let candidates = publication_read::candidate_summary(tx, partner_id).await?;
        if candidates.is_empty() {
            info!(
                material_id = %material_id,
                "draft produced no candidates; the published version is left alone"
            );
            return Ok(());
        }

        // A check that is queued but not started will read this draft too, so joining it
        // is right. A check that is already *running* has read its candidates already and
        // cannot see this one — but its job row is leased and cannot be re-armed from
        // here. That case is handled where it can be: `ValidationWorker::follow_up` queues
        // another check when the run it just finished turns out to have read a different
        // candidate set than the one that exists now.
        if jobs::validation_pending(tx, partner_id).await? {
            info!(
                partner_id = %partner_id,
                "a check is already queued or running; the checker queues a follow-up if \
                 this draft arrived too late for it"
            );
            return Ok(());
        }

        let run = publication::enqueue_run(tx, partner_id, otdel_publish::PROMPT_PROFILE).await?;
        let queued = jobs::enqueue_validation(tx, partner_id, run.id).await?;
        events::record(
            tx,
            &events::NewEvent::new(
                EventKind::ValidationQueued,
                EventActor::Worker,
                "проверка поставлена в очередь автоматически: появились новые кандидаты \
                 после разбора материала"
                    .to_owned(),
            )
            .for_partner(partner_id)
            .about_material(material_id)
            .about_job(queued.id)
            .about_run(run.id)
            .with_detail(serde_json::json!({ "trigger": "understanding_finished" })),
        )
        .await?;

        info!(
            partner_id = %partner_id,
            job_id = %queued.id,
            "partner queued for verification after a new draft"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct JobOutcome {
    facts_stored: u32,
    rejected: u32,
    /// Pages the request budget left for a later pass.
    pages_deferred: u32,
    /// Whether the draft carries what a passport has to have.
    requirements_met: bool,
}
