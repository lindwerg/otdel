//! Maintenance worker.
//!
//! **This worker does not extract documents.** Reading PDFs and images is phase 1B; a
//! process that pretended to process the queue would make `queued` materials look
//! handled when nothing read them. What it does do is the recovery work that phase 1A
//! genuinely needs and that must not require a human:
//!
//! * delete expired and idle-timed-out sessions;
//! * return jobs whose worker lease expired to the queue (or fail them once the attempt
//!   limit is reached), so an interrupted run resumes instead of hanging;
//! * remove staging files left by uploads that were cut off;
//! * compare stored objects with the material rows and *report* orphans.
//!
//! Orphans are reported, never deleted: an object without a row can also be a request
//! that is committing right now, and silently deleting an original is worse than a log
//! line an operator can act on.

use std::sync::Arc;
use std::time::Duration;

use otdel_core::config::Config;
use otdel_db::{jobs, materials, Database};
use otdel_storage::ObjectStore;
use tracing::{info, warn};
use uuid::Uuid;

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
