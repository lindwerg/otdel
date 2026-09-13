//! The extraction worker: queued job → pages → regions and tables → settled material.
//!
//! The shape of a run, and why it is shaped that way:
//!
//! 1. **Claim** the job in a short transaction (`FOR UPDATE SKIP LOCKED`). The document
//!    is read *outside* any transaction, so a 32-page catalogue never holds one open.
//! 2. **Inventory** the pages and write one row per page before reading any of them.
//!    That is what makes "all 44 pages accounted for" true even if the run is
//!    interrupted: the missing pages are visibly `pending`, not absent.
//! 3. **Read each page**, decide its outcome, store its text and regions, and renew the
//!    lease. A page that fails is recorded as failed and the run continues — one bad
//!    page must not cost the other thirty-one.
//! 4. **Settle** the material with the status *derived* from the page rows
//!    ([`aggregate_material_status`]). The worker never sets `completed` directly.
//!
//! If the lease is lost mid-run the worker stops immediately rather than writing results
//! another worker may already be replacing (`docs/block-01-spec.md` §7: a late finish of
//! an old run must not overwrite a newer one).

use std::sync::Arc;
use std::time::Duration;

use otdel_core::config::Config;
use otdel_core::extraction::{
    aggregate_material_status, ExtractionSummary, PageStatus, TextSource,
};
use otdel_core::model::{Job, JobKind, MaterialStatus};
use otdel_db::materials::StoredMaterial;
use otdel_db::{jobs, materials, pages, Database};
use otdel_extract::{
    ExtractError, OcrPermission, PageProcessor, PageSource, PageText, PdfDocument,
    ToolAvailability, PARSER_NAME, PARSER_VERSION,
};
use otdel_storage::{ObjectKey, ObjectStore};
use tracing::{info, warn};
use uuid::Uuid;

use crate::error::WorkerError;
use crate::material_of;
use crate::pagemap;
use crate::workspace::{original_file_name, JobWorkspace};

/// Delay before a transient failure is retried. Deliberately modest: the local pilot has
/// one worker, and a long backoff would look like a hang.
const RETRY_BACKOFF: Duration = Duration::from_secs(30);

/// Availability of the external tools, probed once per worker.
#[derive(Debug, Clone)]
pub struct ToolReport {
    pub engine: ToolAvailability,
    pub rasteriser: ToolAvailability,
}

impl ToolReport {
    /// Why recognition cannot run, or `None` when it can.
    ///
    /// `needs_render` is false for a material that is itself an image: no page has to be
    /// rasterised, so a missing `pdftoppm` is irrelevant there.
    fn blocked_reason(&self, needs_render: bool) -> Option<String> {
        if let ToolAvailability::Unavailable { reason } = &self.engine {
            return Some(reason.clone());
        }
        if needs_render {
            if let ToolAvailability::Unavailable { reason } = &self.rasteriser {
                return Some(reason.clone());
            }
        }
        None
    }
}

/// What one extraction pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExtractionReport {
    pub jobs_claimed: u32,
    pub jobs_completed: u32,
    pub jobs_failed: u32,
    pub pages_read: u32,
    pub pages_recognised: u32,
    pub pages_needing_recognition: u32,
    pub pages_failed: u32,
}

pub struct Extractor {
    config: Arc<Config>,
    db: Database,
    store: Arc<dyn ObjectStore>,
    processor: Arc<PageProcessor>,
    /// Identifies this worker in the job lease.
    owner: String,
    tools: ToolReport,
}

impl Extractor {
    pub fn new(
        config: Arc<Config>,
        db: Database,
        store: Arc<dyn ObjectStore>,
        processor: Arc<PageProcessor>,
        tools: ToolReport,
    ) -> Self {
        Self {
            config,
            db,
            store,
            processor,
            owner: format!("otdel-worker/{}", Uuid::new_v4()),
            tools,
        }
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn tools(&self) -> &ToolReport {
        &self.tools
    }

    /// Drain the queue for this bureau, up to `max_jobs`.
    pub async fn run_pass(
        &self,
        bureau_id: Uuid,
        max_jobs: u32,
    ) -> Result<ExtractionReport, WorkerError> {
        let mut report = ExtractionReport::default();
        for _ in 0..max_jobs {
            let Some(job) = self.claim(bureau_id).await? else {
                break;
            };
            report.jobs_claimed += 1;

            match self.run_job(bureau_id, &job).await {
                Ok(outcome) => {
                    report.pages_read += outcome.pages_read;
                    report.pages_recognised += outcome.pages_recognised;
                    report.pages_needing_recognition += outcome.pages_needing_recognition;
                    report.pages_failed += outcome.pages_failed;
                    self.settle_job(bureau_id, &job, None).await?;
                    report.jobs_completed += 1;
                }
                Err(error) => {
                    warn!(
                        job_id = %job.id,
                        material_id = ?job.material_id,
                        permanent = error.is_permanent(),
                        error = %error,
                        "extraction job failed"
                    );
                    self.settle_job(bureau_id, &job, Some(&error)).await?;
                    report.jobs_failed += 1;
                }
            }
        }
        Ok(report)
    }

    async fn claim(&self, bureau_id: Uuid) -> Result<Option<Job>, WorkerError> {
        let mut tx = self.db.begin_scoped(bureau_id).await?;
        // Only the reading kinds: an `understand_material` row belongs to the phase 1C
        // half, which knows what to do with it.
        let job = jobs::claim_next(
            &mut tx,
            &self.owner,
            self.settings().lease_duration,
            &JobKind::extraction_kinds(),
        )
        .await?;
        tx.commit().await?;
        Ok(job)
    }

    /// Record the job's end. A lost lease is *not* reported as our failure: the row now
    /// belongs to whoever reclaimed it.
    async fn settle_job(
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
                // A job that will not be retried must leave the material saying why,
                // rather than sitting in `processing` forever.
                if error.is_permanent() {
                    self.settle_material(bureau_id, job, Some(&error.diagnostic()))
                        .await?;
                }
            }
        }
        tx.commit().await?;
        Ok(())
    }

    // --- one job -------------------------------------------------------------------

    async fn run_job(&self, bureau_id: Uuid, job: &Job) -> Result<JobOutcome, WorkerError> {
        let stored = self.load_material(bureau_id, job).await?;
        let workspace = JobWorkspace::create(self.settings().work_dir.as_deref()).await?;

        let result = self
            .read_document(bureau_id, job, &stored, &workspace)
            .await;

        if let Err(error) = workspace.cleanup().await {
            warn!(error = %error, "could not remove the extraction scratch directory");
        }
        result
    }

    async fn load_material(
        &self,
        bureau_id: Uuid,
        job: &Job,
    ) -> Result<StoredMaterial, WorkerError> {
        let material_id = material_of(job)?;
        let mut tx = self.db.begin_scoped(bureau_id).await?;
        let stored = materials::get_in_partner(&mut tx, job.partner_id, material_id).await?;
        tx.commit().await?;
        stored.ok_or(WorkerError::MaterialMissing)
    }

    async fn read_document(
        &self,
        bureau_id: Uuid,
        job: &Job,
        stored: &StoredMaterial,
        workspace: &JobWorkspace,
    ) -> Result<JobOutcome, WorkerError> {
        let key = ObjectKey::parse(&stored.storage_key)?;
        let (path, bytes) = workspace
            .materialise(
                self.store.as_ref(),
                &key,
                original_file_name(&stored.material.media_type),
            )
            .await?;

        match stored.material.media_type.as_str() {
            "application/pdf" => {
                self.read_pdf(bureau_id, job, stored, workspace, &path, bytes)
                    .await
            }
            "image/png" | "image/jpeg" => self.read_image(bureau_id, job, stored, &path).await,
            other => Err(WorkerError::UnsupportedMediaType(other.to_owned())),
        }
    }

    async fn read_pdf(
        &self,
        bureau_id: Uuid,
        job: &Job,
        stored: &StoredMaterial,
        workspace: &JobWorkspace,
        path: &std::path::Path,
        bytes: Vec<u8>,
    ) -> Result<JobOutcome, WorkerError> {
        // Parsing is CPU-bound and synchronous; it belongs off the async scheduler.
        let document = tokio::task::spawn_blocking(move || PdfDocument::load(&bytes))
            .await
            .map_err(|_| {
                WorkerError::Extract(ExtractError::ParserCrashed {
                    context: "открытие документа".to_owned(),
                })
            })??;
        let document = Arc::new(document);

        let inventory = document.inventory(self.settings().max_pages_per_document)?;

        let whole_document = job.kind == JobKind::ExtractDocument;
        if whole_document {
            let mut tx = self.db.begin_scoped(bureau_id).await?;
            materials::begin_extraction(&mut tx, stored.material.id, PARSER_NAME, PARSER_VERSION)
                .await?;
            materials::set_page_count(
                &mut tx,
                stored.material.id,
                i32::try_from(inventory.page_count).unwrap_or(i32::MAX),
            )
            .await?;
            pages::record_inventory(
                &mut tx,
                job.partner_id,
                stored.material.id,
                &pagemap::inventory_rows(&inventory),
            )
            .await?;
            tx.commit().await?;
        }

        let targets: Vec<_> = match job.page_number {
            Some(page_number) => {
                let wanted = u32::try_from(page_number).unwrap_or(0);
                vec![inventory
                    .pages
                    .iter()
                    .find(|page| page.page_number == wanted)
                    .cloned()
                    .ok_or(ExtractError::PageMissing { page: wanted })?]
            }
            None => inventory.pages.clone(),
        };

        let mut outcome = JobOutcome::default();
        for page_inventory in &targets {
            self.renew_lease(
                bureau_id,
                job,
                &format!(
                    "page {}/{}",
                    page_inventory.page_number, inventory.page_count
                ),
            )
            .await?;

            let render_dir = workspace
                .subdirectory(&format!("page-{}", page_inventory.page_number))
                .await?;
            let permission = self.permission(true, outcome.pages_recognised);

            let page_result = self
                .read_one_page(
                    Arc::clone(&document),
                    page_inventory.page_number,
                    page_inventory,
                    PageSource::Pdf {
                        path,
                        page: page_inventory.page_number,
                        work_dir: &render_dir,
                    },
                    &permission,
                )
                .await;

            let (row, regions) = match page_result {
                Ok(processed) => {
                    if processed.decision.text_source == TextSource::Ocr {
                        outcome.pages_recognised += 1;
                    }
                    if processed.decision.status == PageStatus::NeedsOcr {
                        outcome.pages_needing_recognition += 1;
                    }
                    pagemap::outcome_row(page_inventory, &processed)
                }
                // A lease we no longer hold stops the run; anything else is this page's
                // own problem and must not cost the other thirty-one.
                Err(WorkerError::LeaseLost) => return Err(WorkerError::LeaseLost),
                Err(error) => {
                    warn!(
                        material_id = %stored.material.id,
                        page = page_inventory.page_number,
                        error = %error,
                        "page could not be read"
                    );
                    outcome.pages_failed += 1;
                    pagemap::failed_row(page_inventory, &error.diagnostic())
                }
            };

            let mut tx = self.db.begin_scoped(bureau_id).await?;
            pages::record_outcome(&mut tx, job.partner_id, stored.material.id, &row, &regions)
                .await?;
            tx.commit().await?;
            outcome.pages_read += 1;
        }

        self.settle_material(bureau_id, job, None).await?;
        Ok(outcome)
    }

    /// A material that is itself an image: one page, no text layer, recognition only.
    async fn read_image(
        &self,
        bureau_id: Uuid,
        job: &Job,
        stored: &StoredMaterial,
        path: &std::path::Path,
    ) -> Result<JobOutcome, WorkerError> {
        let page_inventory = pagemap::image_inventory();

        let mut tx = self.db.begin_scoped(bureau_id).await?;
        materials::begin_extraction(&mut tx, stored.material.id, PARSER_NAME, PARSER_VERSION)
            .await?;
        materials::set_page_count(&mut tx, stored.material.id, 1).await?;
        pages::record_inventory(
            &mut tx,
            job.partner_id,
            stored.material.id,
            &[pagemap::inventory_row(&page_inventory)],
        )
        .await?;
        tx.commit().await?;

        self.renew_lease(bureau_id, job, "image 1/1").await?;

        // No rasteriser is involved, so its absence must not block recognition here.
        let permission = self.permission(false, 0);
        let processed = self
            .processor
            .finish_page(
                PageText::default(),
                &page_inventory,
                PageSource::Image { path },
                &permission,
            )
            .await;

        let mut outcome = JobOutcome {
            pages_read: 1,
            ..JobOutcome::default()
        };
        if processed.decision.text_source == TextSource::Ocr {
            outcome.pages_recognised = 1;
        }
        if processed.decision.status == PageStatus::NeedsOcr {
            outcome.pages_needing_recognition = 1;
        }

        let (row, regions) = pagemap::outcome_row(&page_inventory, &processed);
        let mut tx = self.db.begin_scoped(bureau_id).await?;
        pages::record_outcome(&mut tx, job.partner_id, stored.material.id, &row, &regions).await?;
        tx.commit().await?;

        self.settle_material(bureau_id, job, None).await?;
        Ok(outcome)
    }

    async fn read_one_page(
        &self,
        document: Arc<PdfDocument>,
        page_number: u32,
        page_inventory: &otdel_extract::PageInventory,
        source: PageSource<'_>,
        permission: &OcrPermission,
    ) -> Result<otdel_extract::PageOutcome, WorkerError> {
        let text = tokio::task::spawn_blocking(move || document.read_page(page_number))
            .await
            .map_err(|_| {
                WorkerError::Extract(ExtractError::ParserCrashed {
                    context: format!("чтение страницы {page_number}"),
                })
            })??;

        // The whole page, including any external tool, is bounded.
        let processed = tokio::time::timeout(
            self.settings().page_timeout,
            self.processor
                .finish_page(text, page_inventory, source, permission),
        )
        .await
        .map_err(|_| WorkerError::Extract(ExtractError::PageTimeout { page: page_number }))?;

        Ok(processed)
    }

    // --- shared steps ---------------------------------------------------------------

    /// Recompute the material's status from its page rows and store it.
    async fn settle_material(
        &self,
        bureau_id: Uuid,
        job: &Job,
        failure: Option<&str>,
    ) -> Result<(), WorkerError> {
        let material_id = material_of(job)?;
        let mut tx = self.db.begin_scoped(bureau_id).await?;
        let material =
            materials::get_in_partner_with_extraction(&mut tx, job.partner_id, material_id).await?;

        let summary = material
            .as_ref()
            .and_then(|material| material.extraction.clone())
            .unwrap_or_default();
        let status = derive_status(&summary, failure);

        let (engine, version) = match &self.tools.engine {
            ToolAvailability::Available { version } if summary.pages_extracted > 0 => {
                (Some(self.processor.engine_name()), Some(version.as_str()))
            }
            _ => (None, None),
        };

        materials::finish_extraction(
            &mut tx,
            material_id,
            status,
            failure.or(summary.diagnostic.as_deref()),
            engine,
            version,
        )
        .await?;

        // A material that now has readable pages is handed to the product role
        // (phase 1C). Queueing is idempotent and is skipped while a run is already
        // queued or running, so finishing a single-page retry cannot start a second
        // draft of the same material.
        let readable = summary.pages_extracted + summary.pages_partial > 0;
        if failure.is_none() && readable {
            if jobs::understanding_pending(&mut tx, material_id).await? {
                info!(
                    material_id = %material_id,
                    "understanding of this material is already queued; not queueing again"
                );
            } else {
                let queued =
                    jobs::enqueue_understanding(&mut tx, job.partner_id, material_id).await?;
                otdel_db::knowledge::enqueue_run(
                    &mut tx,
                    job.partner_id,
                    material_id,
                    otdel_knowledge::PROMPT_PROFILE,
                )
                .await?;
                info!(
                    material_id = %material_id,
                    job_id = %queued.id,
                    "material queued for product understanding"
                );
            }
        }

        tx.commit().await?;

        info!(
            material_id = %material_id,
            status = status.as_str(),
            pages_total = summary.pages_total,
            pages_extracted = summary.pages_extracted,
            pages_needs_ocr = summary.pages_needs_ocr,
            pages_failed = summary.pages_failed,
            "material status recomputed from its pages"
        );
        Ok(())
    }

    async fn renew_lease(
        &self,
        bureau_id: Uuid,
        job: &Job,
        stage: &str,
    ) -> Result<(), WorkerError> {
        let mut tx = self.db.begin_scoped(bureau_id).await?;
        let held = jobs::heartbeat(
            &mut tx,
            job.id,
            &self.owner,
            self.settings().lease_duration,
            Some(stage),
        )
        .await?;
        tx.commit().await?;

        if held {
            Ok(())
        } else {
            Err(WorkerError::LeaseLost)
        }
    }

    /// May this page use recognition right now?
    fn permission(&self, needs_render: bool, already_recognised: u32) -> OcrPermission {
        if !self.settings().ocr.enabled {
            return OcrPermission::Denied {
                reason: "распознавание отключено настройкой OTDEL_OCR_ENABLED".to_owned(),
            };
        }
        if let Some(reason) = self.tools.blocked_reason(needs_render) {
            return OcrPermission::Denied { reason };
        }
        let budget = self.settings().ocr.max_pages_per_run;
        if already_recognised >= budget {
            return OcrPermission::Denied {
                reason: format!("исчерпан лимит распознавания на один запуск ({budget} стр.)"),
            };
        }
        OcrPermission::Allowed
    }

    fn settings(&self) -> &otdel_core::extraction_config::ExtractionSettings {
        &self.config.extraction
    }
}

/// Counters of one job run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JobOutcome {
    pub pages_read: u32,
    pub pages_recognised: u32,
    pub pages_needing_recognition: u32,
    pub pages_failed: u32,
}

/// Material status: derived from the pages, unless the document itself could not be
/// opened — in which case there are no pages to derive anything from.
pub fn derive_status(summary: &ExtractionSummary, failure: Option<&str>) -> MaterialStatus {
    if failure.is_some() && summary.pages_total == 0 {
        return MaterialStatus::Failed;
    }
    aggregate_material_status(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(extracted: i32, needs_ocr: i32, failed: i32) -> ExtractionSummary {
        ExtractionSummary {
            pages_total: extracted + needs_ocr + failed,
            pages_extracted: extracted,
            pages_needs_ocr: needs_ocr,
            pages_failed: failed,
            ..ExtractionSummary::default()
        }
    }

    #[test]
    fn a_scanned_document_without_recognition_is_never_completed() {
        assert_eq!(
            derive_status(&summary(0, 12, 0), None),
            MaterialStatus::Failed
        );
        assert_eq!(
            derive_status(&summary(20, 12, 0), None),
            MaterialStatus::Partial
        );
        assert_eq!(
            derive_status(&summary(32, 0, 0), None),
            MaterialStatus::Completed
        );
    }

    #[test]
    fn an_unopenable_document_is_failed_not_completed() {
        assert_eq!(
            derive_status(
                &ExtractionSummary::default(),
                Some("файл не открывается как PDF")
            ),
            MaterialStatus::Failed
        );
    }

    #[test]
    fn a_missing_rasteriser_only_blocks_pages_that_need_rendering() {
        let tools = ToolReport {
            engine: ToolAvailability::Available {
                version: "tesseract 5.5".to_owned(),
            },
            rasteriser: ToolAvailability::Unavailable {
                reason: "исполняемый файл `pdftoppm` не найден".to_owned(),
            },
        };
        assert!(tools.blocked_reason(true).is_some());
        assert!(tools.blocked_reason(false).is_none());
    }

    #[test]
    fn a_missing_engine_blocks_everything() {
        let tools = ToolReport {
            engine: ToolAvailability::Unavailable {
                reason: "исполняемый файл `tesseract` не найден".to_owned(),
            },
            rasteriser: ToolAvailability::Available {
                version: "pdftoppm 24".to_owned(),
            },
        };
        assert!(tools.blocked_reason(true).is_some());
        assert!(tools.blocked_reason(false).is_some());
    }
}
