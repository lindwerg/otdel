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
}

/// `{id,partner_id,material_id,page_number,kind,status,stage,attempts,created_at,
/// updated_at,error}`
///
/// `page_number` is the 1B addition: `null` for a whole-document run, and the page a
/// single-page retry targets otherwise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: Uuid,
    pub partner_id: Uuid,
    pub material_id: Uuid,
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
/// (`docs/block-01-plan.md`, 1B §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    ExtractDocument,
    ExtractPage,
}

impl JobKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExtractDocument => "extract_document",
            Self::ExtractPage => "extract_page",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "extract_document" => Some(Self::ExtractDocument),
            "extract_page" => Some(Self::ExtractPage),
            _ => None,
        }
    }

    /// A page job carries a page number; a document job never does.
    pub const fn needs_page_number(self) -> bool {
        matches!(self, Self::ExtractPage)
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
            material_id: Uuid::from_u128(3),
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
        for kind in [JobKind::ExtractDocument, JobKind::ExtractPage] {
            assert_eq!(JobKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(JobKind::parse("publish"), None);
        assert!(JobKind::ExtractPage.needs_page_number());
        assert!(!JobKind::ExtractDocument.needs_page_number());
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
        };
        let value = serde_json::to_value(&material).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(object.len(), 11);
        // A queued material must not look read: no pages, no summary, no page count.
        assert!(object["extraction"].is_null());
        assert!(object["page_count"].is_null());
        assert_eq!(object["status"], "queued");
    }
}
