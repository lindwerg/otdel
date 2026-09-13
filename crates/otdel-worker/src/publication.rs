//! The phase 1E worker half: check a partner's candidates, freeze a version, publish it.
//!
//! Four steps, and the boundaries between them are where the safety lives:
//!
//! 1. **Claim.** `validate_partner` is its own job kind, claimed by nobody else. The
//!    document reader and the researcher cannot pick up the job that decides what may be
//!    published.
//! 2. **Read and check**, with no model and no network. Every candidate is re-checked
//!    against the source text as it is stored now ([`otdel_publish::check_claims`]).
//!    This step alone is enough to publish: a bureau with no keys at all gets verified,
//!    published, searchable knowledge.
//! 3. **Optionally ask a model** — for a second opinion that may only lower a verdict,
//!    and for vectors. Both happen **outside any transaction**, and a failure of either
//!    is recorded and carried on from, never fatal. An optional dependency that could
//!    block publication would not be optional.
//! 4. **Write and switch**, in one transaction, with the lease re-checked inside it. A
//!    run whose lease died cannot write, and a run whose input is older than what is
//!    already published does not replace it (`docs/block-01-spec.md` §7).
//!
//! Nothing here decides a rule. The rules are in `otdel-publish`, where they can be
//! tested against a string literal; this module is the part that talks to the database.

use std::sync::Arc;
use std::time::Duration;

use otdel_core::config::Config;
use otdel_core::model::{Job, JobKind};
use otdel_core::publication::{ClaimStatus, ValidationRunStatus};
use otdel_core::updates::{EventActor, EventKind};
use otdel_db::publication::{
    self, NewClaim, NewEvidence, NewGap, NewVersion, PublishOutcome, RunOutcome,
};
use otdel_db::{events, jobs, partners, publication_read, Database};
use otdel_embed::{EmbedRequest, EmbeddingProvider};
use otdel_llm::LlmProvider;
use otdel_publish::{check_claims, chunk, readiness, version, CheckedClaim, PublicationDecision};
use tracing::{info, warn};
use uuid::Uuid;

use crate::error::WorkerError;

/// A check that failed for a transient reason is worth repeating, but not at once.
const RETRY_BACKOFF: Duration = Duration::from_secs(120);
/// Requests one check may spend on the optional second opinion.
const MAX_REVIEW_REQUESTS: u32 = 4;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ValidationReport {
    pub jobs_claimed: u32,
    pub jobs_completed: u32,
    pub jobs_failed: u32,
    pub versions_published: u32,
    pub versions_blocked: u32,
    pub claims_checked: u32,
    pub claims_rejected: u32,
    pub chunks_embedded: u32,
    /// Phase 1F: checks queued again because candidates appeared while one was running.
    pub checks_requeued: u32,
}

pub struct ValidationWorker {
    config: Arc<Config>,
    db: Database,
    llm: Arc<dyn LlmProvider>,
    embeddings: Arc<dyn EmbeddingProvider>,
    owner: String,
}

impl ValidationWorker {
    pub fn new(
        config: Arc<Config>,
        db: Database,
        llm: Arc<dyn LlmProvider>,
        embeddings: Arc<dyn EmbeddingProvider>,
    ) -> Self {
        Self {
            config,
            db,
            llm,
            embeddings,
            owner: format!("otdel-checker/{}", Uuid::new_v4()),
        }
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn embeddings(&self) -> otdel_embed::EmbeddingDescription {
        self.embeddings.describe()
    }

    pub async fn run_pass(
        &self,
        bureau_id: Uuid,
        max_jobs: u32,
    ) -> Result<ValidationReport, WorkerError> {
        let mut report = ValidationReport::default();

        for _ in 0..max_jobs {
            let Some(job) = self.claim(bureau_id).await? else {
                break;
            };
            report.jobs_claimed += 1;

            match self.run_job(bureau_id, &job).await {
                Ok(outcome) => {
                    report.claims_checked += outcome.claims_checked;
                    report.claims_rejected += outcome.claims_rejected;
                    report.chunks_embedded += outcome.chunks_embedded;
                    if outcome.published {
                        report.versions_published += 1;
                    }
                    if outcome.blocked {
                        report.versions_blocked += 1;
                    }
                    self.settle(bureau_id, &job, None).await?;
                    report.jobs_completed += 1;
                    if self
                        .follow_up(bureau_id, &job, outcome.inputs.as_deref())
                        .await?
                    {
                        report.checks_requeued += 1;
                    }
                }
                Err(error) => {
                    warn!(
                        job_id = %job.id,
                        partner_id = %job.partner_id,
                        permanent = error.is_permanent(),
                        error = %error,
                        "validation job did not finish"
                    );
                    self.record_failure(bureau_id, &job, &error).await?;
                    self.settle(bureau_id, &job, Some(&error)).await?;
                    report.jobs_failed += 1;
                }
            }
        }

        Ok(report)
    }

    /// Queue another check when candidates appeared **while this one was running**.
    ///
    /// The check reads its candidates once, in its first transaction. A draft that commits
    /// after that moment is not in the version this run produced — and `queue_check` in the
    /// understanding worker could not help, because it sees a check already `running` and
    /// returns rather than arming a second one it has no way to arm (the job row is leased).
    ///
    /// Uploading three documents at once is enough to hit it: the first draft starts a
    /// check, the other two commit while it works, and their facts would never be checked
    /// and never published — with nothing queued to ever fix it.
    ///
    /// So the run that just finished compares what it read with what exists now and, if
    /// they differ, queues one more. This cannot loop: publishing does not change the
    /// candidate set, so a repeat requires a real new candidate each time.
    ///
    /// Runs after `settle`, deliberately — the job row has to be `completed` before
    /// `enqueue_validation` can re-arm it.
    async fn follow_up(
        &self,
        bureau_id: Uuid,
        job: &Job,
        checked: Option<&str>,
    ) -> Result<bool, WorkerError> {
        let Some(checked) = checked else {
            return Ok(false);
        };

        let mut tx = self.db.begin_scoped(bureau_id).await?;
        let candidates = otdel_db::updates::load_candidate_digest(&mut tx, job.partner_id).await?;
        let now = otdel_publish::candidate_fingerprint(&candidates);
        if now == checked || jobs::validation_pending(&mut tx, job.partner_id).await? {
            tx.commit().await?;
            return Ok(false);
        }

        let run = publication::enqueue_run(&mut tx, job.partner_id, otdel_publish::PROMPT_PROFILE)
            .await?;
        let queued = jobs::enqueue_validation(&mut tx, job.partner_id, run.id).await?;
        events::record(
            &mut tx,
            &events::NewEvent::new(
                EventKind::ValidationQueued,
                EventActor::Worker,
                "проверка поставлена в очередь ещё раз: пока шла предыдущая, появились новые \
                 кандидаты, и в её версию они не вошли"
                    .to_owned(),
            )
            .for_partner(job.partner_id)
            .about_job(queued.id)
            .about_run(run.id)
            .with_detail(serde_json::json!({ "trigger": "candidates_changed_during_check" })),
        )
        .await?;
        tx.commit().await?;

        info!(
            partner_id = %job.partner_id,
            job_id = %queued.id,
            "candidates changed while the check was running; another check is queued"
        );
        Ok(true)
    }

    async fn claim(&self, bureau_id: Uuid) -> Result<Option<Job>, WorkerError> {
        let mut tx = self.db.begin_scoped(bureau_id).await?;
        let job = jobs::claim_next(
            &mut tx,
            &self.owner,
            self.config.extraction.lease_duration,
            &JobKind::validation_kinds(),
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
        // A lost lease means somebody else owns this job now. Touching the row would
        // undo their work.
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

    /// Leave the run row saying what happened, so the interface does not show a check
    /// that is stuck at "running" for ever.
    async fn record_failure(
        &self,
        bureau_id: Uuid,
        job: &Job,
        error: &WorkerError,
    ) -> Result<(), WorkerError> {
        if matches!(error, WorkerError::LeaseLost) {
            return Ok(());
        }
        let mut tx = self.db.begin_scoped(bureau_id).await?;
        publication::finish_run(
            &mut tx,
            job.partner_id,
            ValidationRunStatus::Failed,
            &RunOutcome {
                diagnostic: Some(error.diagnostic()),
                ..RunOutcome::default()
            },
        )
        .await?;
        // Phase 1F: a failed check is history, and it is the history that answers "почему
        // версия прежняя". The run row keeps the current state and is overwritten by the
        // next attempt; this line is not.
        events::record(
            &mut tx,
            &events::NewEvent::new(
                EventKind::JobFailed,
                EventActor::Worker,
                format!(
                    "проверка не выполнена: {}. Опубликованная версия не менялась",
                    error.diagnostic()
                ),
            )
            .for_partner(job.partner_id)
            .about_job(job.id)
            .with_detail(serde_json::json!({
                "kind": "validate_partner",
                "permanent": error.is_permanent(),
                "attempts": job.attempts,
            })),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn heartbeat(&self, bureau_id: Uuid, job: &Job, stage: &str) -> Result<(), WorkerError> {
        let mut tx = self.db.begin_scoped(bureau_id).await?;
        let ours = jobs::heartbeat(
            &mut tx,
            job.id,
            &self.owner,
            self.config.extraction.lease_duration,
            Some(stage),
        )
        .await?;
        tx.commit().await?;
        if ours {
            Ok(())
        } else {
            Err(WorkerError::LeaseLost)
        }
    }

    async fn run_job(&self, bureau_id: Uuid, job: &Job) -> Result<JobOutcome, WorkerError> {
        // --- 1. what there is to check -------------------------------------------
        // The moment the candidates are read. Publication compares this with the
        // publication time of whatever is current, so a run that read stale inputs and
        // finished late cannot replace newer knowledge (`block-01-spec.md` §7).
        let inputs_read_at = chrono::Utc::now();
        let mut tx = self.db.begin_scoped(bureau_id).await?;
        if partners::get(&mut tx, job.partner_id).await?.is_none() {
            tx.commit().await?;
            return Err(WorkerError::MaterialMissing);
        }
        let run = publication::start_run(&mut tx, job.partner_id).await?;
        let candidates = publication_read::load_candidates(&mut tx, job.partner_id).await?;
        let gaps = publication_read::load_gaps(&mut tx, job.partner_id).await?;
        let published_fingerprint =
            publication_read::published_fingerprint(&mut tx, job.partner_id).await?;
        tx.commit().await?;

        if candidates.is_empty() {
            let mut tx = self.db.begin_scoped(bureau_id).await?;
            publication::finish_run(
                &mut tx,
                job.partner_id,
                ValidationRunStatus::Partial,
                &RunOutcome {
                    blocked_reasons: vec![
                        "у партнёра нет ни одного кандидата: сначала прочитайте и разберите \
                         материалы"
                            .to_owned(),
                    ],
                    ..RunOutcome::default()
                },
            )
            .await?;
            events::record(
                &mut tx,
                &events::NewEvent::new(
                    EventKind::ValidationFinished,
                    EventActor::Worker,
                    "проверка завершена без результата: у партнёра нет ни одного кандидата"
                        .to_owned(),
                )
                .for_partner(job.partner_id)
                .about_run(run.id)
                .with_detail(serde_json::json!({ "outcome": "no_candidates" })),
            )
            .await?;
            tx.commit().await?;
            return Ok(JobOutcome {
                inputs: Some(otdel_publish::candidate_fingerprint(&candidates)),
                ..JobOutcome::default()
            });
        }

        // What this run is about to check. Compared after it finishes with what exists
        // then, because a draft committed while the check was running is not in it (see
        // `settle_and_follow_up`).
        let inputs = otdel_publish::candidate_fingerprint(&candidates);

        // --- 2. the deterministic check -------------------------------------------
        self.heartbeat(bureau_id, job, "checking claims").await?;
        let mut outcome = check_claims(&candidates);
        let claims_considered = i32::try_from(candidates.len()).unwrap_or(i32::MAX);

        // --- 3. the optional second opinion, outside any transaction ---------------
        let mut notes = outcome.rejections.clone();
        let model_reviewed = otdel_publish::review_claims(
            &self.llm,
            &mut outcome.claims,
            &self.config.retrieval.limits,
            MAX_REVIEW_REQUESTS,
            &mut notes,
        )
        .await;
        for note in notes {
            outcome.note(note);
        }

        // --- 4. readiness and the publication rules --------------------------------
        let gap_texts: Vec<otdel_publish::GapText> =
            gaps.iter().map(|(_, _, text)| text.clone()).collect();
        let readiness = readiness::assess(&outcome.claims, &gap_texts);
        let fingerprint = version::fingerprint(&outcome.claims);
        let decision = version::decide(
            &outcome.claims,
            &readiness,
            &fingerprint,
            published_fingerprint.as_deref(),
        );

        if decision == PublicationDecision::Unchanged {
            // Nothing new to publish — but the *existing* version may still be missing
            // its vectors, and this is the only path that can give them to it.
            //
            // The case is ordinary: a bureau publishes with no embedding provider, then
            // configures one. The candidates have not changed, so the fingerprint is
            // identical and no new version is warranted. Returning here would leave that
            // version permanently vector-less, with the only escape being to perturb a
            // candidate — and `0006_publication.sql` grants the runtime role UPDATE on
            // `version_chunks` precisely so that this is possible.
            let mut tx = self.db.begin_scoped(bureau_id).await?;
            let published = publication_read::find_published(&mut tx, job.partner_id).await?;
            tx.commit().await?;

            let mut embedded = 0i32;
            if let Some(published) = &published {
                embedded = self
                    .embed_version(bureau_id, job, published.id, &mut outcome)
                    .await;
            }

            let mut tx = self.db.begin_scoped(bureau_id).await?;
            publication::finish_run(
                &mut tx,
                job.partner_id,
                ValidationRunStatus::Completed,
                &RunOutcome {
                    version_id: published.as_ref().map(|version| version.id),
                    claims_considered,
                    claims_rejected: i32::try_from(outcome.rejected).unwrap_or(i32::MAX),
                    chunks_embedded: embedded,
                    model_reviewed: i32::try_from(model_reviewed).unwrap_or(i32::MAX),
                    published: published.is_some(),
                    rejections: outcome.rejections.clone(),
                    diagnostic: Some(if embedded > 0 {
                        format!(
                            "кандидаты не изменились с прошлой публикации: новая версия не \
                             создавалась. Опубликованной версии добавлено векторов: {embedded}"
                        )
                    } else {
                        "кандидаты не изменились с прошлой публикации: новая версия не \
                         создавалась"
                            .to_owned()
                    }),
                    ..RunOutcome::default()
                },
            )
            .await?;
            // The one outcome with no version to point at. Without this line the history
            // of a check that found nothing new would be silence, which is exactly what
            // the owner reads as "кажется, ничего не запустилось".
            events::record(
                &mut tx,
                &events::NewEvent::new(
                    EventKind::ValidationFinished,
                    EventActor::Worker,
                    if embedded > 0 {
                        format!(
                            "проверка завершена: кандидаты не изменились, новая версия не \
                             создавалась. Опубликованной версии добавлено векторов: {embedded}"
                        )
                    } else {
                        "проверка завершена: кандидаты не изменились с прошлой публикации, \
                         новая версия не создавалась"
                            .to_owned()
                    },
                )
                .for_partner(job.partner_id)
                .about_run(run.id)
                .with_detail(serde_json::json!({
                    "outcome": "unchanged",
                    "chunks_embedded": embedded,
                    "published_number": published.as_ref().map(|version| version.number),
                })),
            )
            .await?;
            tx.commit().await?;
            info!(
                partner_id = %job.partner_id,
                chunks_embedded = embedded,
                "candidates unchanged; no new version"
            );
            return Ok(JobOutcome {
                chunks_embedded: u32::try_from(embedded).unwrap_or(0),
                inputs: Some(inputs),
                ..JobOutcome::default()
            });
        }

        // --- 5. freeze the snapshot ------------------------------------------------
        self.heartbeat(bureau_id, job, "writing the version")
            .await?;
        let new_version = NewVersion {
            input_fingerprint: fingerprint,
            // Phase 1F: the same input without its verdicts, so `GET .../refresh` can say
            // whether a new check would produce something different without running one.
            candidate_fingerprint: inputs.clone(),
            validation_run_id: run.id,
            claims: outcome
                .claims
                .iter()
                .map(|claim| to_new_claim(claim, self.config.retrieval.limits.chunk_max_chars))
                .collect(),
            gaps: gaps
                .iter()
                .map(|(id, product, text)| NewGap {
                    origin_id: *id,
                    product_name: product.clone(),
                    topic: text.topic.clone(),
                    missing: text.missing.clone(),
                    blocks: text.blocks.clone(),
                    blocks_topics: readiness::gap_blocks(text)
                        .into_iter()
                        .map(|topic| topic.as_str().to_owned())
                        .collect(),
                })
                .collect(),
            readiness: readiness.clone(),
        };

        let mut tx = self.db.begin_scoped(bureau_id).await?;
        // The lease is re-checked inside the transaction that writes, so a run whose
        // lease died cannot publish over a newer one.
        let still_ours = jobs::heartbeat(
            &mut tx,
            job.id,
            &self.owner,
            self.config.extraction.lease_duration,
            Some("publishing"),
        )
        .await?;
        if !still_ours {
            tx.rollback().await?;
            return Err(WorkerError::LeaseLost);
        }

        // Which version is current *before* this one publishes. Read inside the writing
        // transaction, because it is the one this run is about to supersede, and the
        // event log has to name it — "версия 2 заменила версию 1" is the line that makes
        // the history readable, and afterwards there is no way to tell which it was.
        let superseded = publication_read::find_published(&mut tx, job.partner_id).await?;

        let (version_id, counts) =
            publication::write_version(&mut tx, job.partner_id, &new_version).await?;

        let mut blocked_reasons: Vec<String> = Vec::new();
        let published = match &decision {
            PublicationDecision::Publish => {
                match publication::publish_version(
                    &mut tx,
                    job.partner_id,
                    version_id,
                    inputs_read_at,
                )
                .await?
                {
                    PublishOutcome::Published => true,
                    PublishOutcome::Superseded { reason } => {
                        blocked_reasons.push(reason.clone());
                        publication::block_version(&mut tx, version_id, &blocked_reasons).await?;
                        false
                    }
                }
            }
            PublicationDecision::Blocked { reasons } => {
                blocked_reasons = reasons.clone();
                publication::block_version(&mut tx, version_id, &blocked_reasons).await?;
                false
            }
            PublicationDecision::Unchanged => unreachable!("handled above"),
        };

        // The history of the switch, written in the transaction that performs it. A log
        // that could survive a rolled-back publication would describe a version nobody
        // can open.
        let version_number = publication_read::find_version(&mut tx, job.partner_id, version_id)
            .await?
            .map(|version| version.number);
        if published {
            if let Some(previous) = &superseded {
                events::record(
                    &mut tx,
                    &events::NewEvent::new(
                        EventKind::VersionSuperseded,
                        EventActor::Worker,
                        format!(
                            "версия {} заменена: опубликована версия {}. Замещённая версия \
                             остаётся неизменной и открывается по закреплённой ссылке",
                            previous.number,
                            version_number.unwrap_or_default()
                        ),
                    )
                    .for_partner(job.partner_id)
                    .about_version(previous.id)
                    .with_detail(serde_json::json!({
                        "superseded_number": previous.number,
                        "replaced_by_number": version_number,
                    })),
                )
                .await?;
            }
            events::record(
                &mut tx,
                &events::NewEvent::new(
                    EventKind::VersionPublished,
                    EventActor::Worker,
                    format!(
                        "опубликована версия {}: утверждений {}, из них подтверждено \
                         источником {}",
                        version_number.unwrap_or_default(),
                        counts.claims,
                        outcome.count(ClaimStatus::SourceSupported)
                    ),
                )
                .for_partner(job.partner_id)
                .about_version(version_id)
                .about_run(run.id)
                .with_detail(serde_json::json!({
                    "number": version_number,
                    "claims": counts.claims,
                    "supported": outcome.count(ClaimStatus::SourceSupported),
                    "conflicted": outcome.count(ClaimStatus::Conflicted),
                    "gaps": counts.gaps,
                })),
            )
            .await?;
        } else {
            events::record(
                &mut tx,
                &events::NewEvent::new(
                    EventKind::VersionBlocked,
                    EventActor::Worker,
                    format!(
                        "версия {} не опубликована: {}",
                        version_number.unwrap_or_default(),
                        blocked_reasons
                            .first()
                            .cloned()
                            .unwrap_or_else(|| "правила публикации не выполнены".to_owned())
                    ),
                )
                .for_partner(job.partner_id)
                .about_version(version_id)
                .about_run(run.id)
                .with_detail(serde_json::json!({
                    "number": version_number,
                    "blocked_reasons": blocked_reasons,
                    // The version that stays live. A blocked check does not take away
                    // what is published (`block-01-spec.md` §7), and the log says which
                    // version that is rather than leaving the reader to assume.
                    "still_published_number": superseded.as_ref().map(|version| version.number),
                })),
            )
            .await?;
        }
        tx.commit().await?;

        // --- 6. vectors, if there is a provider ------------------------------------
        let chunks_embedded = self
            .embed_version(bureau_id, job, version_id, &mut outcome)
            .await;

        // --- 7. report -------------------------------------------------------------
        let status = if outcome.rejected > 0 || !blocked_reasons.is_empty() {
            ValidationRunStatus::Partial
        } else {
            ValidationRunStatus::Completed
        };

        let mut tx = self.db.begin_scoped(bureau_id).await?;
        publication::finish_run(
            &mut tx,
            job.partner_id,
            status,
            &RunOutcome {
                version_id: Some(version_id),
                claims_considered,
                claims_rejected: i32::try_from(outcome.rejected).unwrap_or(i32::MAX),
                gaps_carried: counts.gaps,
                chunks_created: counts.chunks,
                chunks_embedded,
                model_reviewed: i32::try_from(model_reviewed).unwrap_or(i32::MAX),
                published,
                rejections: outcome.rejections.clone(),
                blocked_reasons: blocked_reasons.clone(),
                diagnostic: None,
                ..RunOutcome::default()
            },
        )
        .await?;
        tx.commit().await?;

        info!(
            partner_id = %job.partner_id,
            version_id = %version_id,
            published,
            claims = counts.claims,
            supported = outcome.count(ClaimStatus::SourceSupported),
            conflicted = outcome.count(ClaimStatus::Conflicted),
            chunks_embedded,
            "knowledge version built"
        );

        Ok(JobOutcome {
            claims_checked: u32::try_from(counts.claims).unwrap_or(0),
            claims_rejected: outcome.rejected,
            chunks_embedded: u32::try_from(chunks_embedded).unwrap_or(0),
            published,
            blocked: !blocked_reasons.is_empty(),
            inputs: Some(inputs),
        })
    }

    /// Attach vectors, when there is somewhere to put them and something to make them.
    ///
    /// Returns how many were stored. Every reason for storing none is recorded on the run
    /// and none of them is a failure: a version without vectors is searched by exact value
    /// and full text, and the search says which mode it used.
    async fn embed_version(
        &self,
        bureau_id: Uuid,
        job: &Job,
        version_id: Uuid,
        outcome: &mut otdel_publish::CheckOutcome,
    ) -> i32 {
        let description = self.embeddings.describe();
        let Some(profile) = description.profile.clone() else {
            return 0;
        };
        if !description.is_ready() {
            return 0;
        }

        let mut tx = match self.db.begin_scoped(bureau_id).await {
            Ok(tx) => tx,
            Err(error) => {
                outcome.note(format!("векторы не построены: {error}"));
                return 0;
            }
        };
        // A database failure here must not be reported as "the extension is missing" or,
        // worse, as nothing at all: both are confident statements about a situation
        // nobody actually established.
        let has_column = match publication_read::vector_column_exists(&mut tx).await {
            Ok(present) => present,
            Err(error) => {
                let _ = tx.rollback().await;
                outcome.note(format!(
                    "не удалось проверить наличие pgvector: {error}. Векторы не строились"
                ));
                return 0;
            }
        };
        if !has_column {
            let _ = tx.commit().await;
            outcome.note(
                "расширение pgvector не установлено: семантический поиск недоступен, поиск \
                 работает по точным значениям и тексту"
                    .to_owned(),
            );
            return 0;
        }
        let pending = match publication_read::chunks_to_embed(&mut tx, version_id, &profile).await {
            Ok(pending) => pending,
            Err(error) => {
                let _ = tx.rollback().await;
                outcome.note(format!(
                    "не удалось прочитать фрагменты для векторов: {error}"
                ));
                return 0;
            }
        };
        let _ = tx.commit().await;

        if pending.is_empty() {
            return 0;
        }
        let wanted = pending.len();

        if self.heartbeat(bureau_id, job, "embedding").await.is_err() {
            return 0;
        }

        // Outside any transaction: an external call inside one would hold a database
        // transaction open for the length of somebody else's service
        // (`block-01-spec.md` §10).
        let batch = usize::try_from(self.config.retrieval.embedding.limits.batch_size)
            .unwrap_or(32)
            .max(1);
        let mut stored = 0i32;

        for group in pending.chunks(batch) {
            let request = EmbedRequest {
                purpose: "version_chunk",
                inputs: group.iter().map(|(_, text)| text.clone()).collect(),
            };
            let response = match self.embeddings.embed(&request).await {
                Ok(response) => response,
                Err(error) => {
                    // Recorded, not fatal: the version is already published.
                    outcome.note(format!(
                        "векторы построены не полностью: {}. Поиск работает по точным \
                         значениям и тексту",
                        error.diagnostic()
                    ));
                    break;
                }
            };

            // The profile string asserts which vector space these rows live in, and it
            // is derived from configuration. If the service answered as a *different*
            // model, storing under that profile would put two spaces in one — which is
            // the exact failure `block-01-spec.md` §9 forbids, and one that no dimension
            // check can catch when the widths happen to agree.
            // Compared against the *adapter's* declared model, because that is what the
            // profile string is built from (`EmbeddingSettings::profile`). Comparing
            // against configuration would be comparing against the wrong thing whenever
            // the adapter is not the one configuration describes.
            let declared = &description.model;
            if !response.model.is_empty()
                && !declared.is_empty()
                && !response.model.eq_ignore_ascii_case(declared)
            {
                outcome.note(format!(
                    "векторы не сохранены: провайдер ответил моделью «{}», а профиль \
                     объявлен для «{declared}». Смешивать пространства разных моделей \
                     нельзя — поиск остаётся по точным значениям и тексту",
                    response.model.chars().take(60).collect::<String>()
                ));
                break;
            }

            let vectors: Vec<(Uuid, Vec<f32>)> = group
                .iter()
                .map(|(id, _)| *id)
                .zip(response.vectors)
                .collect();
            let dimensions = i32::try_from(response.dimensions).unwrap_or(i32::MAX);

            let mut tx = match self.db.begin_scoped(bureau_id).await {
                Ok(tx) => tx,
                Err(error) => {
                    outcome.note(format!("векторы не сохранены: {error}"));
                    break;
                }
            };
            match publication::store_embeddings(&mut tx, version_id, &profile, dimensions, &vectors)
                .await
            {
                Ok(count) => {
                    stored += count;
                    let _ = tx.commit().await;
                }
                Err(error) => {
                    let _ = tx.rollback().await;
                    outcome.note(format!("векторы не сохранены: {error}"));
                    break;
                }
            }
        }

        if (stored as usize) < wanted {
            // Said out loud, because a version with one embedded chunk out of ten still
            // reports `hybrid` — `has_vectors` only asks whether *any* row carries the
            // profile. Without this line the owner has no way to learn that most of the
            // version is semantically unreachable.
            outcome.note(format!(
                "векторы построены не для всех утверждений: {stored} из {wanted}. \
                 Семантический поиск покрывает только их; остальное находится по точным \
                 значениям и тексту"
            ));
        }

        stored
    }
}

#[derive(Debug, Clone, Default)]
struct JobOutcome {
    claims_checked: u32,
    claims_rejected: u32,
    chunks_embedded: u32,
    published: bool,
    blocked: bool,
    /// Phase 1F: the candidate fingerprint this run actually read, so the pass can tell
    /// whether anything appeared while it was working. `None` only when the run never got
    /// as far as reading candidates.
    inputs: Option<String>,
}

/// A plain translation. Every rule has already been applied by `otdel-publish`, and a
/// mapping that decided anything would be a second place to look for the rules.
fn to_new_claim(claim: &CheckedClaim, chunk_max_chars: u32) -> NewClaim {
    let max = usize::try_from(chunk_max_chars).unwrap_or(2_000);
    NewClaim {
        origin: claim.origin.as_str(),
        origin_id: claim.origin_id,
        scope: claim.origin.scope().as_str(),
        product_name: claim.product_name.clone(),
        kind: claim.kind.as_str(),
        status: claim.status,
        attribute: claim.attribute.clone(),
        value_text: claim.value_text.clone(),
        unit: claim.unit.clone(),
        conditions: claim.conditions.clone(),
        model_context: claim.model_context.clone(),
        check_note: claim.check_note.clone(),
        evidence: claim
            .evidence
            .iter()
            .map(|item| NewEvidence {
                source_kind: item.source_kind.as_str(),
                material_id: item.material_id,
                material_filename: item.material_filename.clone(),
                page_number: item.page_number,
                region_id: item.region_id,
                url: item.url.clone(),
                host: item.host.clone(),
                retrieved_at: item.retrieved_at,
                content_hash: item.content_hash.clone(),
                quote: item.quote.clone(),
                char_start: item.char_start,
                char_end: item.char_end,
            })
            .collect(),
        chunk_text: chunk::chunk_text(claim, max),
        // The folded lookup keys can never be empty: `check` refused any claim whose
        // attribute or value folds away, so these are the same strings it kept.
        normalised_value: chunk::normalised_column(&claim.value_text)
            .unwrap_or_else(|| "-".to_owned()),
        normalised_attribute: chunk::normalised_column(&claim.attribute)
            .unwrap_or_else(|| "-".to_owned()),
        normalised_product: claim
            .product_name
            .as_deref()
            .and_then(chunk::normalised_column),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use otdel_core::knowledge::FactKind;
    use otdel_core::publication::{ClaimOrigin, EvidenceSourceKind};
    use otdel_publish::CheckedEvidence;

    fn claim() -> CheckedClaim {
        CheckedClaim {
            origin: ClaimOrigin::PartnerMaterial,
            origin_id: Uuid::from_u128(1),
            product_name: Some("  BP21 ".to_owned()),
            kind: FactKind::Characteristic,
            status: ClaimStatus::SourceSupported,
            attribute: "Нагрузка".to_owned(),
            value_text: "3.5".to_owned(),
            unit: Some("kN".to_owned()),
            conditions: None,
            model_context: None,
            check_note: None,
            evidence: vec![CheckedEvidence {
                source_kind: EvidenceSourceKind::Material,
                material_id: Some(Uuid::from_u128(2)),
                material_filename: Some("catalogue.pdf".to_owned()),
                page_number: Some(3),
                region_id: None,
                url: None,
                host: None,
                retrieved_at: None,
                content_hash: None,
                quote: "BP21 3.5 kN".to_owned(),
                char_start: 0,
                char_end: 11,
            }],
        }
    }

    #[test]
    fn the_scope_of_a_stored_claim_comes_from_its_origin_and_cannot_disagree_with_it() {
        // The database has the same CHECK. Deriving it rather than copying it means the
        // two can never be written out of step.
        let partner = to_new_claim(&claim(), 2_000);
        assert_eq!(partner.origin, "partner_material");
        assert_eq!(partner.scope, "partner");

        let mut industry = claim();
        industry.origin = ClaimOrigin::IndustryResearch;
        industry.product_name = None;
        let industry = to_new_claim(&industry, 2_000);
        assert_eq!(industry.scope, "industry");
        assert_eq!(industry.normalised_product, None);
    }

    #[test]
    fn the_lookup_keys_are_folded_so_an_article_is_found_however_it_was_written() {
        let stored = to_new_claim(&claim(), 2_000);
        assert_eq!(stored.normalised_product.as_deref(), Some("bp21"));
        assert_eq!(stored.normalised_attribute, "нагрузка");
        assert_eq!(stored.normalised_value, "3.5");
    }

    #[test]
    fn the_chunk_is_bounded_by_the_configured_limit() {
        let mut long = claim();
        long.evidence[0].quote = "я".repeat(9_000);
        let stored = to_new_claim(&long, 500);
        assert_eq!(stored.chunk_text.chars().count(), 500);
    }

    #[test]
    fn every_stored_claim_carries_its_citations() {
        let stored = to_new_claim(&claim(), 2_000);
        assert_eq!(stored.evidence.len(), 1);
        assert_eq!(stored.evidence[0].source_kind, "material");
        assert_eq!(stored.evidence[0].quote, "BP21 3.5 kN");
    }
}
