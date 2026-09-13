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
//! `probe` reports the state of those external tools and exits — useful before a long
//! run, and honest about a machine where they are not installed.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use otdel_core::config::Config;
use otdel_db::Database;
use otdel_extract::ToolAvailability;
use otdel_storage::{FilesystemObjectStore, ObjectStore};
use otdel_worker::{Maintenance, MaintenanceSettings};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

/// Jobs taken in one extraction pass before maintenance gets a turn.
const MAX_JOBS_PER_PASS: u32 = 8;
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
                 run   — extraction + maintenance until SIGINT/SIGTERM (default)\n\
                 once  — one extraction pass and one maintenance pass, then exit\n\
                 probe — report OCR/rasteriser availability and exit\n\n\
                 Extraction reads queued materials page by page. Pages without a usable\n\
                 text layer are recognised when an engine is installed, and recorded as\n\
                 `needs_ocr` with the reason when it is not."
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
        let recovery = maintenance.run_once().await.context("maintenance pass")?;
        println!(
            "jobs_claimed={} jobs_completed={} jobs_failed={} pages_read={} \
             pages_recognised={} pages_needing_recognition={} pages_failed={} \
             sessions_purged={} leases_reclaimed={} staging_files_removed={} \
             objects_scanned={} orphan_objects={} scan_truncated={}",
            extraction.jobs_claimed,
            extraction.jobs_completed,
            extraction.jobs_failed,
            extraction.pages_read,
            extraction.pages_recognised,
            extraction.pages_needing_recognition,
            extraction.pages_failed,
            recovery.sessions_purged,
            recovery.leases_reclaimed,
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
        maintenance,
        bureau_id,
        config.extraction.poll_interval,
    )
    .await;
    Ok(())
}

/// Alternate between draining the queue and the periodic recovery pass until stopped.
async fn serve(
    extractor: otdel_worker::Extractor,
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

                if last_maintenance.elapsed() >= MAINTENANCE_INTERVAL {
                    last_maintenance = Instant::now();
                    match maintenance.run_once().await {
                        Ok(report) => info!(
                            sessions_purged = report.sessions_purged,
                            leases_reclaimed = report.leases_reclaimed,
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
