//! The OTDEL background worker.
//!
//! Two halves, deliberately separate:
//!
//! * **extraction** ([`extraction`]) — the phase 1B pipeline that turns a queued
//!   material into per-page records, source regions and tables. This is the half that
//!   actually reads documents;
//! * **understanding** ([`knowledge`]) — the phase 1C pipeline that turns those pages
//!   into a structured product draft, every fact tied to a verbatim source. It runs
//!   only when the model adapter is configured; otherwise the run is recorded as
//!   `needs_provider` and nothing is stored;
//! * **research** ([`research`]) — the phase 1D pipeline that turns an approved industry
//!   question into bounded external work. It is the only half that opens a socket to
//!   somebody else's machine and the only one that spends money, which is why it claims
//!   its own job kind and refuses to start until a search endpoint, a host allowlist and
//!   a model are all configured;
//! * **verification and publication** ([`publication`]) — the phase 1E pipeline that
//!   re-checks every candidate against the source text as it is stored now, freezes what
//!   survives into an immutable version and switches the published pointer to it. Unlike
//!   the two above it needs **nothing configured**: the rules are deterministic, so a
//!   bureau with no keys at all still gets checked, published, searchable knowledge;
//! * **maintenance** ([`Maintenance`]) — the recovery work that must not require a
//!   human: expired sessions, jobs whose lease died with their worker, runs and plans
//!   whose worker is gone, money reserved for calls that will never happen, staging files
//!   of interrupted uploads, and orphan objects.
//!
//! Orphans are reported, never deleted: an object without a row can also be a request
//! that is committing right now, and silently deleting an original is worse than a log
//! line an operator can act on.

pub mod error;
pub mod extraction;
pub mod knowledge;
pub mod pagemap;
pub mod publication;
pub mod research;
pub mod workspace;

/// The material a job is about.
///
/// `validate_partner` is the only kind without one, and no half that calls this ever
/// claims that kind — the claim lists are disjoint (`JobKind::extraction_kinds()`,
/// `knowledge_kinds()`, `research_kinds()`, `validation_kinds()`). A `None` here would
/// therefore mean a queue row contradicting its own kind, which the database also
/// refuses (`jobs_material_matches_kind`). It is reported as a permanent failure of that
/// one job rather than unwrapped: a panic would take the whole worker down with it.
pub(crate) fn material_of(job: &otdel_core::model::Job) -> Result<Uuid, error::WorkerError> {
    job.material_id.ok_or(error::WorkerError::MaterialMissing)
}

use std::sync::Arc;
use std::time::Duration;

use otdel_core::config::Config;
use otdel_db::{jobs, materials, Database};
use otdel_extract::{
    Disabled, OcrEngine, PageProcessor, PageRasteriser, PopplerRasteriser, TesseractEngine,
};
use otdel_llm::LlmProvider;
use otdel_search::{DocumentFetcher, SearchProvider};
use otdel_storage::ObjectStore;
use tracing::{info, warn};
use uuid::Uuid;

pub use error::WorkerError;
pub use extraction::{ExtractionReport, Extractor, ToolReport};
pub use knowledge::{KnowledgeReport, KnowledgeWorker};
pub use publication::{ValidationReport, ValidationWorker};
pub use research::{ResearchReport, ResearchWorker};

/// Build the page processor from configuration.
///
/// When recognition is switched off the adapters are [`Disabled`] — not "the real ones
/// that we promise not to call". There is then no code path that could produce
/// recognised text, which is the property the acceptance checks depend on.
pub fn build_processor(config: &Config) -> PageProcessor {
    let ocr = &config.extraction.ocr;
    if !ocr.enabled {
        let reason = "распознавание отключено настройкой OTDEL_OCR_ENABLED".to_owned();
        return PageProcessor::new(
            Arc::new(Disabled::new("tesseract", reason.clone())),
            Arc::new(Disabled::new("pdftoppm", reason)),
        );
    }

    PageProcessor::new(
        Arc::new(TesseractEngine::new(
            &ocr.engine_bin,
            &ocr.languages,
            ocr.timeout,
        )),
        Arc::new(PopplerRasteriser::new(
            &ocr.renderer_bin,
            ocr.dpi,
            ocr.timeout,
        )),
    )
}

/// Probe the external tools once, so the reason for `needs_ocr` is the same on every
/// page of a run and the operator sees it in the startup log.
pub async fn probe_tools(processor: &PageProcessor) -> ToolReport {
    ToolReport {
        engine: processor.engine_availability().await,
        rasteriser: processor.rasteriser_availability().await,
    }
}

/// Assemble the understanding half from configuration.
///
/// The provider comes from [`otdel_llm::build_provider`], which returns an adapter with
/// no HTTP client at all when there is no key. The worker is therefore built and
/// started in either case — it has to be, so it can record "нужен ключ" on the run
/// instead of leaving the material silently unprocessed.
pub fn build_knowledge_worker(
    config: Arc<Config>,
    db: Database,
    provider: Arc<dyn LlmProvider>,
) -> KnowledgeWorker {
    KnowledgeWorker::new(config, db, provider)
}

/// The model adapter described by the configuration.
pub fn build_llm_provider(config: &Config) -> Arc<dyn LlmProvider> {
    otdel_llm::build_provider(&config.llm)
}

/// Assemble the phase 1E half from configuration.
///
/// Both adapters it is handed are optional and may be the unconfigured ones. That is the
/// normal case and it changes nothing about whether a version is published: the model can
/// only lower a verdict it was never needed to reach, and the embedding provider only
/// adds the vector half of search.
pub fn build_validation_worker(
    config: Arc<Config>,
    db: Database,
    llm: Arc<dyn LlmProvider>,
    embeddings: Arc<dyn otdel_embed::EmbeddingProvider>,
) -> ValidationWorker {
    ValidationWorker::new(config, db, llm, embeddings)
}

/// The phase 1E embedding adapter described by the configuration.
///
/// Client-less when nothing is configured, exactly like the others — so there is no code
/// path from a published version to a network, and no pseudo-vector anywhere.
pub fn build_embedding_provider(config: &Config) -> Arc<dyn otdel_embed::EmbeddingProvider> {
    otdel_embed::build_provider(&config.retrieval.embedding)
}

/// The phase 1D adapters described by the configuration.
///
/// Both come from builders that return a *client-less* object when the configuration is
/// incomplete, so the research worker is built and started in either case — it has to be,
/// so it can record "нужен поисковый провайдер" on the plan instead of leaving an approved
/// question silently unprocessed. Returned as a pair because the binary also reports their
/// state at startup, and building them twice would be two different objects.
pub fn build_research_adapters(
    config: &Config,
) -> (Arc<dyn SearchProvider>, Arc<dyn DocumentFetcher>) {
    (
        otdel_search::build_search_provider(&config.research),
        otdel_search::build_fetcher(&config.research),
    )
}

/// Assemble the extraction half from configuration.
pub fn build_extractor(
    config: Arc<Config>,
    db: Database,
    store: Arc<dyn ObjectStore>,
    processor: Arc<PageProcessor>,
    tools: ToolReport,
) -> Extractor {
    Extractor::new(config, db, store, processor, tools)
}

/// Compile-time reminder that both adapter traits stay object-safe: the worker holds
/// them behind `Arc<dyn _>` so a different engine can be swapped in without touching it.
const _: fn() = || {
    fn accepts(_engine: &dyn OcrEngine, _rasteriser: &dyn PageRasteriser) {}
    let _ = accepts;
};

/// Tunables for one maintenance pass.
#[derive(Debug, Clone, Copy)]
pub struct MaintenanceSettings {
    /// Time between passes.
    pub interval: Duration,
    /// A staging file older than this belongs to an upload that will never finish.
    pub staging_max_age: Duration,
    /// Upper bound on objects inspected per pass, so a pass stays predictable.
    pub orphan_scan_limit: usize,
}

impl Default for MaintenanceSettings {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(300),
            staging_max_age: Duration::from_secs(3600),
            orphan_scan_limit: 5_000,
        }
    }
}

/// What one pass actually did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MaintenanceReport {
    pub sessions_purged: i64,
    pub leases_reclaimed: u64,
    /// Understanding runs that said `running` with no job behind them.
    pub stalled_runs_settled: u64,
    /// Research plans in the same situation.
    pub stalled_plans_settled: u64,
    /// Verification runs in the same situation. Without this a killed checker would
    /// leave the interface saying "проверка идёт" for ever.
    pub stalled_checks_settled: u64,
    /// Reservations of plans that are no longer running — money a dead worker was
    /// holding, given back to the budget.
    pub reservations_released: u64,
    pub staging_files_removed: u64,
    pub objects_scanned: usize,
    pub orphan_objects: usize,
    /// `true` when the object scan hit its limit and did not see everything.
    pub scan_truncated: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum MaintenanceError {
    #[error("database failure during maintenance: {0}")]
    Db(#[from] otdel_db::DbError),
    #[error("storage failure during maintenance: {0}")]
    Storage(#[from] otdel_storage::StorageError),
    #[error("bureau `{0}` is not provisioned")]
    BureauMissing(String),
}

/// Maintenance describes itself as phase 1A recovery; the extraction half lives in
/// [`extraction`] and is driven by the binary in `apps/worker`.
pub const MAINTENANCE_SCOPE: &str = "sessions, job leases, upload staging, orphan objects";

pub struct Maintenance {
    config: Arc<Config>,
    db: Database,
    store: Arc<dyn ObjectStore>,
    settings: MaintenanceSettings,
}

impl Maintenance {
    pub fn new(
        config: Arc<Config>,
        db: Database,
        store: Arc<dyn ObjectStore>,
        settings: MaintenanceSettings,
    ) -> Self {
        Self {
            config,
            db,
            store,
            settings,
        }
    }

    /// Run one pass and report what happened.
    pub async fn run_once(&self) -> Result<MaintenanceReport, MaintenanceError> {
        let mut report = MaintenanceReport::default();

        let idle_timeout =
            i32::try_from(self.config.session_idle_timeout.as_secs()).unwrap_or(7200);
        report.sessions_purged =
            otdel_db::sessions::purge_expired(self.db.pool(), idle_timeout).await?;

        let bureau_id = self
            .db
            .bureau_id_by_slug(&self.config.bureau_slug)
            .await?
            .ok_or_else(|| MaintenanceError::BureauMissing(self.config.bureau_slug.clone()))?;

        let mut tx = self.db.begin_scoped(bureau_id).await?;
        report.leases_reclaimed = jobs::reclaim_expired_leases(&mut tx).await?;
        // A run whose worker died is corrected here too: the job queue recovers the
        // job, and this recovers the record the owner is actually looking at.
        report.stalled_runs_settled = otdel_db::knowledge::reclaim_stalled_runs(&mut tx).await?;
        // The same correction for research, and then the money: a worker that died
        // between reserving and settling would otherwise hold that amount for ever, and
        // the bureau's research budget would shrink with every crash. The order matters —
        // plans are settled first, so their reservations become releasable in the same
        // pass rather than the next one.
        report.stalled_plans_settled = otdel_db::research::reclaim_stalled_plans(&mut tx).await?;
        report.reservations_released =
            otdel_db::research::release_orphan_reservations(&mut tx).await?;
        // And the same for verification. A check holds no money and writes nothing until
        // its final transaction, so a dead one costs only the row that says it is still
        // running — which is exactly the row the owner is watching.
        report.stalled_checks_settled =
            otdel_db::publication::reclaim_stalled_runs(&mut tx).await?;
        tx.commit().await?;

        let sweep = self
            .store
            .sweep_staging(self.settings.staging_max_age)
            .await?;
        report.staging_files_removed = sweep.removed_files;

        let (orphans, scanned, truncated) = self.report_orphans(bureau_id).await?;
        report.orphan_objects = orphans;
        report.objects_scanned = scanned;
        report.scan_truncated = truncated;

        Ok(report)
    }

    /// Compare stored objects with material rows of this bureau.
    async fn report_orphans(
        &self,
        bureau_id: Uuid,
    ) -> Result<(usize, usize, bool), MaintenanceError> {
        let listing = self
            .store
            .list_objects(self.settings.orphan_scan_limit)
            .await?;
        let scanned = listing.keys.len();
        let mut orphans = 0usize;

        let mut tx = self.db.begin_scoped(bureau_id).await?;
        for key in &listing.keys {
            let namespace = key.namespace()?;
            if namespace.bureau_id != bureau_id {
                // Another bureau's object: this worker runs for one bureau and must not
                // guess about rows it cannot see.
                continue;
            }
            let referenced =
                materials::find_by_digest(&mut tx, namespace.partner_id, key.digest()).await?;
            if referenced.is_none() {
                orphans += 1;
                warn!(
                    key = %key,
                    "stored original has no material row; kept for manual review"
                );
            }
        }
        tx.commit().await?;

        Ok((orphans, scanned, listing.truncated))
    }

    /// Run passes until `shutdown` resolves.
    pub async fn run_until<F>(&self, shutdown: F)
    where
        F: std::future::Future<Output = ()>,
    {
        let mut ticker = tokio::time::interval(self.settings.interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tokio::pin!(shutdown);

        loop {
            tokio::select! {
                () = &mut shutdown => {
                    info!("maintenance worker stopping");
                    return;
                }
                _ = ticker.tick() => {
                    match self.run_once().await {
                        Ok(report) => info!(
                            sessions_purged = report.sessions_purged,
                            leases_reclaimed = report.leases_reclaimed,
                            stalled_runs_settled = report.stalled_runs_settled,
                            stalled_plans_settled = report.stalled_plans_settled,
                            reservations_released = report.reservations_released,
                            staging_files_removed = report.staging_files_removed,
                            objects_scanned = report.objects_scanned,
                            orphan_objects = report.orphan_objects,
                            scan_truncated = report.scan_truncated,
                            "maintenance pass finished"
                        ),
                        // A failed pass is logged and retried at the next tick: the
                        // worker must not die because the database blinked.
                        Err(error) => warn!(error = %error, "maintenance pass failed"),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_are_bounded() {
        let settings = MaintenanceSettings::default();
        assert!(settings.interval >= Duration::from_secs(60));
        assert!(settings.staging_max_age >= Duration::from_secs(600));
        assert!(settings.orphan_scan_limit > 0);
    }
}
