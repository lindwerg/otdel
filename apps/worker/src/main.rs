//! OTDEL maintenance worker binary.
//!
//! `run` (default) keeps doing maintenance passes until the process is stopped; `once`
//! does a single pass and prints the result, which is what the acceptance checks and a
//! cron-style invocation need.
//!
//! This worker does **not** extract documents: phase 1B owns that. Queued materials stay
//! queued, and nothing here writes a status that would suggest a file has been read.

use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result};
use otdel_core::config::Config;
use otdel_db::Database;
use otdel_storage::{FilesystemObjectStore, ObjectStore};
use otdel_worker::{Maintenance, MaintenanceSettings};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    let command = std::env::args().nth(1).unwrap_or_else(|| "run".to_owned());

    let result = match command.as_str() {
        "run" => run(false).await,
        "once" => run(true).await,
        "-h" | "--help" | "help" => {
            eprintln!(
                "usage: otdel-worker [run|once]\n\n\
                 run  — maintenance passes until SIGINT/SIGTERM (default)\n\
                 once — a single pass, then exit\n\n\
                 Phase 1A maintenance only: expired sessions, job lease recovery, staging\n\
                 sweep and orphan reporting. Document extraction is phase 1B."
            );
            Ok(())
        }
        other => {
            eprintln!("unknown command `{other}` (expected `run` or `once`)");
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

async fn run(single_pass: bool) -> Result<()> {
    let config = Config::from_env().map_err(|error| anyhow::anyhow!("{}", error.message))?;

    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&config.log_filter))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .try_init();

    let db = Database::connect(&config.database_url, 2)
        .await
        .context("connecting to the database")?;
    // Same refusal as the API: maintenance must not run with a role that bypasses
    // row-level security either.
    db.verify_runtime_role()
        .await
        .context("verifying the runtime database role")?;

    let store = FilesystemObjectStore::open_at(&config.storage_root)
        .await
        .context("opening the object store")?;
    let store: Arc<dyn ObjectStore> = Arc::new(store);

    let maintenance = Maintenance::new(Arc::new(config), db, store, MaintenanceSettings::default());

    if single_pass {
        let report = maintenance.run_once().await.context("maintenance pass")?;
        println!(
            "sessions_purged={} leases_reclaimed={} staging_files_removed={} \
             objects_scanned={} orphan_objects={} scan_truncated={}",
            report.sessions_purged,
            report.leases_reclaimed,
            report.staging_files_removed,
            report.objects_scanned,
            report.orphan_objects,
            report.scan_truncated
        );
        return Ok(());
    }

    maintenance.run_until(otdel_api::shutdown_signal()).await;
    Ok(())
}
