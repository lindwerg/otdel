//! OTDEL background worker binary.
//!
//! `run` (default) keeps working until the process is stopped; `once` does a single pass
//! and prints what it did, which is what the acceptance checks and a cron-style
//! invocation need.
//!
//! Since phase 1B this worker **does** read documents: it leases `extract_document` and
//! `extract_page` jobs, writes one record per page, and settles each material with the
//! status derived from those pages. Recognition is delegated to local binaries; their
//! availability is probed once at startup and printed, so "why is page 7 waiting for
//! OCR" is answerable from the log rather than by guessing.
//!
//! Since phase 1C it also drafts product knowledge: it leases `understand_material`
//! jobs, sends bounded prompts built from the pages that were read, and stores only the
//! candidates whose quotes were found in those pages. Without a configured model key it
//! calls nothing and records each run as `needs_provider`.
//!
//! Since phase 1D it also researches approved industry questions: it leases
//! `research_plan` jobs, reserves money before every external call, searches, reads only
//! pages of hosts the owner declared, and stores conclusions whose quotes were found in
//! those pages. This is the only part of OTDEL that reaches outside the machine. Without
//! a configured search endpoint, host allowlist and model it calls nothing, reserves
//! nothing and records each plan as `needs_provider`.
//!
//! `probe` reports the state of the external tools, the model adapter and the research
//! adapters, then exits — useful before a long run, and honest about a machine where
//! nothing is installed and no key is set.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use otdel_core::config::Config;
use otdel_db::Database;
use otdel_extract::ToolAvailability;
use otdel_storage::{FilesystemObjectStore, ObjectStore};
use otdel_worker::{KnowledgeWorker, Maintenance, MaintenanceSettings, ResearchWorker};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

/// Jobs taken in one extraction pass before maintenance gets a turn.
const MAX_JOBS_PER_PASS: u32 = 8;
/// Understanding runs taken in one pass. Lower than the extraction budget: each one
/// can make several model calls, and a pass should stay short enough to be stopped.
const MAX_KNOWLEDGE_JOBS_PER_PASS: u32 = 4;
/// Research plans taken in one pass. Lower again: each one can search, download several
/// pages and call a model, and each one spends real money.
const MAX_RESEARCH_JOBS_PER_PASS: u32 = 2;
/// How often the recovery half runs while the worker is up.
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(300);

#[tokio::main]
async fn main() -> ExitCode {
    let command = std::env::args().nth(1).unwrap_or_else(|| "run".to_owned());

    let result = match command.as_str() {
        "run" => run(Mode::Continuous).await,
        "once" => run(Mode::SinglePass).await,
        "probe" => run(Mode::Probe).await,
        "-h" | "--help" | "help" => {
            eprintln!(
                "usage: otdel-worker [run|once|probe]\n\n\
                 run   — extraction + understanding + maintenance until SIGINT/SIGTERM (default)\n\
                 once  — one pass of each, then exit\n\
                 probe — report OCR/rasteriser and model adapter availability, then exit\n\n\
                 Extraction reads queued materials page by page. Pages without a usable\n\
                 text layer are recognised when an engine is installed, and recorded as\n\
                 `needs_ocr` with the reason when it is not.\n\
                 Understanding drafts product knowledge from the pages that were read.\n\
                 Without a configured model key nothing is called: each run is recorded\n\
                 as `needs_provider` and no knowledge is stored.\n\
                 Research answers approved industry questions from external sources,\n\
                 within a budget and a declared host allowlist. Without a configured\n\
                 search endpoint, allowlist and model, nothing leaves this machine and\n\
                 no money is reserved: each plan is recorded as `needs_provider`."
            );
            Ok(())
        }
        other => {
            eprintln!("unknown command `{other}` (expected `run`, `once` or `probe`)");
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("otdel-worker: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Continuous,
    SinglePass,
    Probe,
}

async fn run(mode: Mode) -> Result<()> {
    let config = Config::from_env().map_err(|error| anyhow::anyhow!("{}", error.message))?;

    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&config.log_filter))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .try_init();

    let processor = Arc::new(otdel_worker::build_processor(&config));
    let tools = otdel_worker::probe_tools(&processor).await;
    report_tools(&tools);

    // The model adapter is described before anything else runs. With no key this is
    // the whole story of phase 1C on this machine, and the operator should not have to
    // wait for a failed run to learn it.
    let provider = otdel_worker::build_llm_provider(&config);
    report_provider(&provider.describe());

    // The research adapters are described before anything runs too. Their absence is the
    // whole story of phase 1D on this machine, and the operator should not have to wait
    // for a plan to stop in order to learn it.
    let (search, fetcher) = otdel_worker::build_research_adapters(&config);
    report_research(&search.describe(), &fetcher.describe());

    if mode == Mode::Probe {
        return Ok(());
    }

    let db = Database::connect(&config.database_url, 4)
        .await
        .context("connecting to the database")?;
    // Same refusal as the API: the worker must not run with a role that bypasses
    // row-level security. It reads and writes partner documents.
    db.verify_runtime_role()
        .await
        .context("verifying the runtime database role")?;

    let store = FilesystemObjectStore::open_at(&config.storage_root)
        .await
        .context("opening the object store")?;
    let store: Arc<dyn ObjectStore> = Arc::new(store);

    let config = Arc::new(config);
    let bureau_id = db
        .bureau_id_by_slug(&config.bureau_slug)
        .await
        .context("resolving the configured bureau")?
        .with_context(|| {
            format!(
                "bureau `{}` is not provisioned; run `otdel-api bootstrap` first",
                config.bureau_slug
            )
        })?;

    let extractor = otdel_worker::build_extractor(
        Arc::clone(&config),
        db.clone(),
        Arc::clone(&store),
        Arc::clone(&processor),
        tools,
    );
    let knowledge = KnowledgeWorker::new(Arc::clone(&config), db.clone(), Arc::clone(&provider));
    let research = ResearchWorker::new(
        Arc::clone(&config),
        db.clone(),
        Arc::clone(&search),
        Arc::clone(&fetcher),
        Arc::clone(&provider),
    );
    let maintenance = Maintenance::new(
        Arc::clone(&config),
        db.clone(),
        Arc::clone(&store),
        MaintenanceSettings::default(),
    );

    if mode == Mode::SinglePass {
        let extraction = extractor
            .run_pass(bureau_id, MAX_JOBS_PER_PASS)
            .await
            .context("extraction pass")?;
        let understanding = knowledge
            .run_pass(bureau_id, MAX_KNOWLEDGE_JOBS_PER_PASS)
            .await
            .context("understanding pass")?;
        let researched = research
            .run_pass(bureau_id, MAX_RESEARCH_JOBS_PER_PASS)
            .await
            .context("research pass")?;
        let recovery = maintenance.run_once().await.context("maintenance pass")?;
        println!(
            "jobs_claimed={} jobs_completed={} jobs_failed={} pages_read={} \
             pages_recognised={} pages_needing_recognition={} pages_failed={} \
             knowledge_jobs_claimed={} knowledge_jobs_completed={} knowledge_jobs_failed={} \
             facts_stored={} candidates_rejected={} runs_awaiting_provider={} \
             research_jobs_claimed={} research_jobs_completed={} research_jobs_failed={} \
             research_queries={} research_sources_fetched={} research_findings={} \
             research_findings_rejected={} plans_awaiting_provider={} \
             plans_budget_exhausted={} research_micros_spent={} \
             sessions_purged={} leases_reclaimed={} stalled_runs_settled={} \
             stalled_plans_settled={} reservations_released={} \
             staging_files_removed={} \
             objects_scanned={} orphan_objects={} scan_truncated={}",
            extraction.jobs_claimed,
            extraction.jobs_completed,
            extraction.jobs_failed,
            extraction.pages_read,
            extraction.pages_recognised,
            extraction.pages_needing_recognition,
            extraction.pages_failed,
            understanding.jobs_claimed,
            understanding.jobs_completed,
            understanding.jobs_failed,
            understanding.facts_stored,
            understanding.candidates_rejected,
            understanding.runs_awaiting_provider,
            researched.jobs_claimed,
            researched.jobs_completed,
            researched.jobs_failed,
            researched.queries_made,
            researched.sources_fetched,
            researched.findings_stored,
            researched.findings_rejected,
            researched.plans_awaiting_provider,
            researched.plans_budget_exhausted,
            researched.micros_spent,
            recovery.sessions_purged,
            recovery.leases_reclaimed,
            recovery.stalled_runs_settled,
            recovery.stalled_plans_settled,
            recovery.reservations_released,
            recovery.staging_files_removed,
            recovery.objects_scanned,
            recovery.orphan_objects,
            recovery.scan_truncated
        );
        return Ok(());
    }

    info!(
        worker = extractor.owner(),
        bureau = %config.bureau_slug,
        poll_interval_seconds = config.extraction.poll_interval.as_secs(),
        "otdel worker started"
    );
    serve(
        extractor,
        knowledge,
        research,
        maintenance,
        bureau_id,
        config.extraction.poll_interval,
    )
    .await;
    Ok(())
}

/// Alternate between draining the queues and the periodic recovery pass until stopped.
async fn serve(
    extractor: otdel_worker::Extractor,
    knowledge: KnowledgeWorker,
    research: ResearchWorker,
    maintenance: Maintenance,
    bureau_id: uuid::Uuid,
    poll_interval: Duration,
) {
    let shutdown = otdel_api::shutdown_signal();
    tokio::pin!(shutdown);

    let mut ticker = tokio::time::interval(poll_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_maintenance = Instant::now() - MAINTENANCE_INTERVAL;

    loop {
        tokio::select! {
            () = &mut shutdown => {
                info!("worker stopping");
                return;
            }
            _ = ticker.tick() => {
                match extractor.run_pass(bureau_id, MAX_JOBS_PER_PASS).await {
                    Ok(report) if report.jobs_claimed > 0 => info!(
                        jobs_claimed = report.jobs_claimed,
                        jobs_completed = report.jobs_completed,
                        jobs_failed = report.jobs_failed,
                        pages_read = report.pages_read,
                        pages_recognised = report.pages_recognised,
                        pages_needing_recognition = report.pages_needing_recognition,
                        pages_failed = report.pages_failed,
                        "extraction pass finished"
                    ),
                    Ok(_) => {}
                    // A failed pass is logged and retried on the next tick: the worker
                    // must not die because the database blinked.
                    Err(error) => warn!(error = %error, "extraction pass failed"),
                }

                match knowledge.run_pass(bureau_id, MAX_KNOWLEDGE_JOBS_PER_PASS).await {
                    Ok(report) if report.jobs_claimed > 0 => info!(
                        jobs_claimed = report.jobs_claimed,
                        jobs_completed = report.jobs_completed,
                        jobs_failed = report.jobs_failed,
                        facts_stored = report.facts_stored,
                        candidates_rejected = report.candidates_rejected,
                        runs_awaiting_provider = report.runs_awaiting_provider,
                        "understanding pass finished"
                    ),
                    Ok(_) => {}
                    Err(error) => warn!(error = %error, "understanding pass failed"),
                }

                match research.run_pass(bureau_id, MAX_RESEARCH_JOBS_PER_PASS).await {
                    Ok(report) if report.jobs_claimed > 0 => info!(
                        jobs_claimed = report.jobs_claimed,
                        jobs_completed = report.jobs_completed,
                        jobs_failed = report.jobs_failed,
                        queries_made = report.queries_made,
                        sources_fetched = report.sources_fetched,
                        findings_stored = report.findings_stored,
                        findings_rejected = report.findings_rejected,
                        plans_awaiting_provider = report.plans_awaiting_provider,
                        plans_budget_exhausted = report.plans_budget_exhausted,
                        micros_spent = report.micros_spent,
                        "research pass finished"
                    ),
                    Ok(_) => {}
                    Err(error) => warn!(error = %error, "research pass failed"),
                }

                if last_maintenance.elapsed() >= MAINTENANCE_INTERVAL {
                    last_maintenance = Instant::now();
                    match maintenance.run_once().await {
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
                        Err(error) => warn!(error = %error, "maintenance pass failed"),
                    }
                }
            }
        }
    }
}

/// Say plainly whether recognition can happen on this machine.
fn report_tools(tools: &otdel_worker::ToolReport) {
    match &tools.engine {
        ToolAvailability::Available { version } => {
            info!(engine = %version, "OCR engine available");
        }
        ToolAvailability::Unavailable { reason } => warn!(
            reason = %reason,
            "OCR engine unavailable: pages without a usable text layer will be recorded \
             as `needs_ocr` with this reason, not as read"
        ),
    }
    match &tools.rasteriser {
        ToolAvailability::Available { version } => {
            info!(rasteriser = %version, "page rasteriser available");
        }
        ToolAvailability::Unavailable { reason } => warn!(
            reason = %reason,
            "page rasteriser unavailable: PDF pages cannot be rendered for recognition"
        ),
    }
}

/// Say plainly whether the product role can run on this machine.
fn report_provider(description: &otdel_llm::ProviderDescription) {
    if description.is_ready() {
        info!(
            provider = %description.provider,
            model = %description.model,
            endpoint_host = description.endpoint_host.clone().unwrap_or_default(),
            "model adapter ready: materials will be drafted into product knowledge"
        );
        return;
    }

    warn!(
        state = description.state,
        missing = ?description.missing,
        "model adapter not configured: materials are read as usual, and each \
         understanding run is recorded as `needs_provider` without calling anything \
         and without storing invented knowledge"
    );
}

/// Say plainly whether bounded industry research can happen on this machine.
///
/// This is the only half that leaves the machine, so its state is reported with the two
/// facts an operator actually needs: which service would be asked, and which hosts may be
/// read. A key is never printed; neither adapter can print one.
fn report_research(
    search: &otdel_search::AdapterDescription,
    fetcher: &otdel_search::AdapterDescription,
) {
    if search.is_ready() && fetcher.is_ready() {
        info!(
            endpoint_host = search.endpoint_host.clone().unwrap_or_default(),
            allowed_hosts = ?fetcher.allowed_hosts,
            "research adapters ready: approved industry questions will be researched \
             within their budget, reading only the hosts listed above"
        );
        return;
    }

    warn!(
        search_state = search.state,
        fetcher_state = fetcher.state,
        missing = ?search.missing,
        "research adapters not configured: nothing leaves this machine and no budget is \
         reserved. An approved question is recorded as `needs_provider` — reading and \
         understanding materials keep working as usual"
    );
}
