//! Domain entities of phases 1A/1B and their wire representation.
//!
//! The `serde` shape of these structs *is* the API contract
//! (`docs/implementation-contract.md`), so field names and nullability are not
//! changed without changing that document first.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AppError, ErrorCode};
use crate::extraction::ExtractionSummary;
use crate::media::MediaType;

/// `{id,name,note,created_at,updated_at}`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Partner {
    pub id: Uuid,
    pub name: String,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `{id,partner_id,filename,media_type,size_bytes,sha256,status,page_count,created_at,
/// error,extraction}`
///
/// `extraction` is the phase 1B addition: `null` until the worker has recorded pages for
/// this material, and afterwards a roll-up computed from the page rows themselves (see
/// [`crate::extraction::ExtractionSummary`]). It is never written independently of the
/// pages, so the counters cannot claim more than the pages actually say.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Material {
    pub id: Uuid,
    pub partner_id: Uuid,
    pub filename: String,
    pub media_type: String,
    pub size_bytes: i64,
    pub sha256: String,
    pub status: MaterialStatus,
    pub page_count: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub error: Option<String>,
    pub extraction: Option<ExtractionSummary>,
    /// Phase 1F: how many times this stored original has been **read**.
    ///
    /// Not how many times it was uploaded — a changed file is a different material,
    /// because deduplication is by content. `0` means "never read"; anything above `1`
    /// means the text behind this id has been produced again, which is what makes a
    /// draft taken from an earlier reading identifiable as such.
    pub content_revision: i32,
}

/// `{id,partner_id,material_id,page_number,kind,status,stage,attempts,created_at,
/// updated_at,error}`
///
/// `page_number` is the 1B addition: `null` for a whole-document run, and the page a
/// single-page retry targets otherwise.
///
/// `material_id` became nullable in 1E, and it is the one non-additive change that phase
/// made to an existing wire type. `validate_partner` is the first job that is not about
/// a single document — the checker looks at everything a partner has, because a
/// contradiction between two catalogues is only visible from there. Giving that job some
/// arbitrary material id would have kept the type simpler by writing down something
/// untrue; a database CHECK now ties the two together, so `material_id` is absent
/// exactly for that kind and present for every other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: Uuid,
    pub partner_id: Uuid,
    pub material_id: Option<Uuid>,
    pub page_number: Option<i32>,
    pub kind: JobKind,
    pub status: JobStatus,
    pub stage: Option<String>,
    pub attempts: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub error: Option<String>,
}

/// Lifecycle of an intake material.
///
/// Upload produces `Queued`. Everything after that is written by the 1B extraction
/// worker and is *derived from the page outcomes*
/// ([`crate::extraction::aggregate_material_status`]) rather than decided on its own —
/// so `Completed` means "every page settled cleanly", not "the run finished".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialStatus {
    Queued,
    Processing,
    Completed,
    Partial,
    Failed,
    Quarantined,
}

impl MaterialStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Processing => "processing",
            Self::Completed => "completed",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::Quarantined => "quarantined",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "processing" => Some(Self::Processing),
            "completed" => Some(Self::Completed),
            "partial" => Some(Self::Partial),
            "failed" => Some(Self::Failed),
            "quarantined" => Some(Self::Quarantined),
            _ => None,
        }
    }

    /// Retry is only meaningful for an extraction that ran and did not finish cleanly.
    ///
    /// * `queued` / `processing` — already scheduled or running, retry is a no-op
    ///   (reported as a conflict so the caller does not believe a new run started);
    /// * `completed` — nothing to repeat;
    /// * `quarantined` — the content itself was rejected, repeating cannot change that.
    pub const fn can_retry(self) -> bool {
        matches!(self, Self::Failed | Self::Partial)
    }

    /// Whether a **reprocess** may be started — phase 1F's "read this document again".
    ///
    /// Wider than [`Self::can_retry`] on purpose, and the difference is the whole point
    /// of having two. A retry is for work that did not finish. A reprocess is for work
    /// that finished and should be done again: a better OCR engine, a fixed parser, or
    /// simply doubt about what was read. `block-01-spec.md` §6.1 allows exactly that —
    /// «явный запуск новой версии обработчика допускается» — while re-running the same
    /// profile over an unchanged file is expected to reuse what is stored.
    ///
    /// `Queued` and `Processing` are refused because the reading they would repeat has
    /// not happened yet; `Quarantined` is refused because the file never passed intake,
    /// and re-reading it would be re-reading something this system declined to open.
    pub const fn can_reprocess(self) -> bool {
        matches!(self, Self::Completed | Self::Partial | Self::Failed)
    }

    /// Explains why a reprocess was refused, in the owner's terms.
    pub fn reprocess_refusal(self) -> AppError {
        let message = match self {
            Self::Queued => "материал уже стоит в очереди на чтение",
            Self::Processing => "материал читается прямо сейчас: дождитесь окончания",
            Self::Quarantined => {
                "файл не прошёл приём и не читался; повторное чтение нечего повторять"
            }
            Self::Completed | Self::Partial | Self::Failed => "материал можно перечитать",
        };
        AppError::new(ErrorCode::Conflict, message)
    }

    /// Explains, without leaking internals, why a retry was refused.
    pub fn retry_refusal(self) -> AppError {
        let message = match self {
            Self::Queued => "material is already queued for extraction",
            Self::Processing => "material is being processed right now",
            Self::Completed => "material has already been extracted",
            Self::Quarantined => "quarantined material cannot be retried",
            Self::Failed | Self::Partial => "material can be retried",
        };
        AppError::new(ErrorCode::Conflict, message)
    }
}

/// Kind of queued work.
///
/// 1A enqueued whole-document extraction; 1B adds the single-page repeat, which exists
/// so a problem page can be retried without re-reading the other 31
/// (`docs/block-01-plan.md`, 1B §2); 1C adds the understanding run that drafts product
/// knowledge from the pages a material already has; 1D adds the research plan, which is
/// the only kind that reaches outside this machine; 1E adds the partner-wide check that
/// turns candidates into a published version, and it is the only kind with no material
/// of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    ExtractDocument,
    ExtractPage,
    UnderstandMaterial,
    ResearchPlan,
    ValidatePartner,
}

impl JobKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExtractDocument => "extract_document",
            Self::ExtractPage => "extract_page",
            Self::UnderstandMaterial => "understand_material",
            Self::ResearchPlan => "research_plan",
            Self::ValidatePartner => "validate_partner",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "extract_document" => Some(Self::ExtractDocument),
            "extract_page" => Some(Self::ExtractPage),
            "understand_material" => Some(Self::UnderstandMaterial),
            "research_plan" => Some(Self::ResearchPlan),
            "validate_partner" => Some(Self::ValidatePartner),
            _ => None,
        }
    }

    /// A page job carries a page number; every other kind never does.
    pub const fn needs_page_number(self) -> bool {
        matches!(self, Self::ExtractPage)
    }

    /// Every kind but the partner-wide check is about one material.
    ///
    /// The database says the same thing (`jobs_material_matches_kind` in
    /// `0006_publication.sql`); this is the Rust side of it, so a worker can turn
    /// `Option<Uuid>` into a `Uuid` at one place with a real reason for the unwrap.
    pub const fn needs_material(self) -> bool {
        !matches!(self, Self::ValidatePartner)
    }

    /// Kinds the document-reading worker half claims. The understanding half claims
    /// its own, so neither can pick up work it does not know how to run.
    pub const fn extraction_kinds() -> [Self; 2] {
        [Self::ExtractDocument, Self::ExtractPage]
    }

    /// Kinds the phase 1C worker half claims.
    pub const fn knowledge_kinds() -> [Self; 1] {
        [Self::UnderstandMaterial]
    }

    /// Kinds the phase 1D worker half claims.
    ///
    /// Kept disjoint from the other two for the same reason, and for one more: this is
    /// the half that spends money and opens sockets to the outside world. A document
    /// reader that could claim one of these jobs would be a path from "a PDF arrived"
    /// to "a paid external request was made".
    pub const fn research_kinds() -> [Self; 1] {
        [Self::ResearchPlan]
    }

    /// Kinds the phase 1E worker half claims.
    ///
    /// Disjoint from the other three, and for a reason of its own: this is the half that
    /// decides what may be published and answered. A document reader or a researcher
    /// that could claim one of these would be a path from "a file arrived" to "a version
    /// went live".
    pub const fn validation_kinds() -> [Self; 1] {
        [Self::ValidatePartner]
    }
}

/// Queue state. The 1B worker leases a `Queued` row into `Running` and settles it as
/// `Completed` or `Failed`; `Cancelled` is reserved for an operator action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// Stable idempotency key for the whole-document extraction job of a material.
///
/// Re-uploading the same bytes (deduplicated) or pressing “retry” twice therefore
/// cannot create a second queue entry for the same material. The per-page variant is
/// [`crate::extraction::page_extraction_idempotency_key`].
pub fn extraction_idempotency_key(material_id: Uuid, kind: JobKind) -> String {
    format!("{}:{material_id}", kind.as_str())
}

/// Convenience for building the wire `media_type` string from a detected signature.
pub fn media_type_string(media_type: MediaType) -> String {
    media_type.as_str().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn material_status_round_trips() {
        for status in [
            MaterialStatus::Queued,
            MaterialStatus::Processing,
            MaterialStatus::Completed,
            MaterialStatus::Partial,
            MaterialStatus::Failed,
            MaterialStatus::Quarantined,
        ] {
            assert_eq!(MaterialStatus::parse(status.as_str()), Some(status));
            assert_eq!(
                serde_json::to_value(status).unwrap(),
                serde_json::Value::String(status.as_str().to_owned())
            );
        }
        assert_eq!(MaterialStatus::parse("read"), None);
    }

    #[test]
    fn only_failed_and_partial_can_be_retried() {
        assert!(MaterialStatus::Failed.can_retry());
        assert!(MaterialStatus::Partial.can_retry());
        for status in [
            MaterialStatus::Queued,
            MaterialStatus::Processing,
            MaterialStatus::Completed,
            MaterialStatus::Quarantined,
        ] {
            assert!(!status.can_retry());
            assert_eq!(status.retry_refusal().code, ErrorCode::Conflict);
        }
    }

    #[test]
    fn a_finished_material_can_be_reread_even_though_it_cannot_be_retried() {
        // The 1F distinction: a retry resumes work that did not finish, a reprocess
        // repeats work that did.
        assert!(MaterialStatus::Completed.can_reprocess());
        assert!(!MaterialStatus::Completed.can_retry());
        assert!(MaterialStatus::Partial.can_reprocess());
        assert!(MaterialStatus::Failed.can_reprocess());

        for status in [
            MaterialStatus::Queued,
            MaterialStatus::Processing,
            MaterialStatus::Quarantined,
        ] {
            assert!(!status.can_reprocess(), "{}", status.as_str());
            assert_eq!(status.reprocess_refusal().code, ErrorCode::Conflict);
            assert!(!status.reprocess_refusal().message.is_empty());
        }
    }

    #[test]
    fn idempotency_key_is_stable_per_material() {
        let material = Uuid::from_u128(7);
        assert_eq!(
            extraction_idempotency_key(material, JobKind::ExtractDocument),
            extraction_idempotency_key(material, JobKind::ExtractDocument)
        );
        assert_ne!(
            extraction_idempotency_key(material, JobKind::ExtractDocument),
            extraction_idempotency_key(Uuid::from_u128(8), JobKind::ExtractDocument)
        );
    }

    #[test]
    fn job_serialises_with_contract_field_names() {
        let now = DateTime::<Utc>::from_timestamp(0, 0).unwrap();
        let job = Job {
            id: Uuid::from_u128(1),
            partner_id: Uuid::from_u128(2),
            material_id: Some(Uuid::from_u128(3)),
            page_number: None,
            kind: JobKind::ExtractDocument,
            status: JobStatus::Queued,
            stage: None,
            attempts: 0,
            created_at: now,
            updated_at: now,
            error: None,
        };
        let value = serde_json::to_value(&job).unwrap();
        let object = value.as_object().unwrap();
        let mut keys: Vec<_> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "attempts",
                "created_at",
                "error",
                "id",
                "kind",
                "material_id",
                "page_number",
                "partner_id",
                "stage",
                "status",
                "updated_at"
            ]
        );
        assert_eq!(object["kind"], "extract_document");
        assert!(object["page_number"].is_null());
    }

    #[test]
    fn job_kinds_round_trip_and_only_page_jobs_carry_a_page() {
        for kind in [
            JobKind::ExtractDocument,
            JobKind::ExtractPage,
            JobKind::UnderstandMaterial,
            JobKind::ResearchPlan,
        ] {
            assert_eq!(JobKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(JobKind::parse("publish"), None);
        assert!(JobKind::ExtractPage.needs_page_number());
        assert!(!JobKind::ExtractDocument.needs_page_number());
        assert!(!JobKind::UnderstandMaterial.needs_page_number());
        assert!(!JobKind::ResearchPlan.needs_page_number());
    }

    #[test]
    fn the_four_worker_halves_claim_disjoint_kinds() {
        let halves = [
            ("extraction", JobKind::extraction_kinds().to_vec()),
            ("knowledge", JobKind::knowledge_kinds().to_vec()),
            ("research", JobKind::research_kinds().to_vec()),
            ("validation", JobKind::validation_kinds().to_vec()),
        ];

        for (name, kinds) in &halves {
            for (other_name, other_kinds) in &halves {
                if name == other_name {
                    continue;
                }
                for kind in kinds {
                    assert!(
                        !other_kinds.contains(kind),
                        "{kind:?} must not be claimable by both `{name}` and `{other_name}`: \
                         a half would pick up work it cannot run, and the reader could \
                         reach the half that spends money"
                    );
                }
            }
        }

        // Every kind belongs to exactly one half: a kind claimed by nobody would sit in
        // the queue for ever, looking queued and never running.
        let claimed: Vec<JobKind> = halves
            .iter()
            .flat_map(|(_, kinds)| kinds.iter().copied())
            .collect();
        for kind in [
            JobKind::ExtractDocument,
            JobKind::ExtractPage,
            JobKind::UnderstandMaterial,
            JobKind::ResearchPlan,
            JobKind::ValidatePartner,
        ] {
            assert!(
                claimed.contains(&kind),
                "{kind:?} is claimed by no worker half"
            );
        }
    }

    #[test]
    fn only_the_partner_wide_check_has_no_material() {
        assert!(!JobKind::ValidatePartner.needs_material());
        for kind in [
            JobKind::ExtractDocument,
            JobKind::ExtractPage,
            JobKind::UnderstandMaterial,
            JobKind::ResearchPlan,
        ] {
            assert!(kind.needs_material(), "{kind:?} must name its material");
        }
    }

    #[test]
    fn material_serialises_with_the_extraction_summary_field() {
        let now = DateTime::<Utc>::from_timestamp(0, 0).unwrap();
        let material = Material {
            id: Uuid::from_u128(1),
            partner_id: Uuid::from_u128(2),
            filename: "каталог.pdf".to_owned(),
            media_type: "application/pdf".to_owned(),
            size_bytes: 10,
            sha256: "a".repeat(64),
            status: MaterialStatus::Queued,
            page_count: None,
            created_at: now,
            error: None,
            extraction: None,
            content_revision: 0,
        };
        let value = serde_json::to_value(&material).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(object.len(), 12);
        // A queued material must not look read: no pages, no summary, no page count, and
        // a reading count of zero rather than a default of one.
        assert!(object["extraction"].is_null());
        assert!(object["page_count"].is_null());
        assert_eq!(object["status"], "queued");
        assert_eq!(object["content_revision"], 0);
    }
}
