//! Phase 1F — the update cycle, the history of it, and what may be pruned.
//!
//! Everything here is vocabulary and wire shape. No decision is taken in this module; the
//! rules live in `otdel-publish` (the version diff), `otdel-db` (what the database can
//! actually say) and the worker (what it actually did).
//!
//! The one idea worth stating once, because four types below depend on it: **a refresh
//! status is computed, never stored.** A stored "актуально / устарело" flag is a claim
//! about the world that some code has to remember to update, and the moment it is wrong
//! it is wrong silently. Every state here is derived from rows that exist — the readings
//! a document has had, the drafts made from them, and the fingerprint of the candidate
//! set the published version was built from — so a stale answer is impossible rather than
//! unlikely.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::MaterialStatus;
use crate::publication::{ReadinessState, ReadinessTopic};

/// A version, named the way a caller needs it in order to ask for more.
///
/// Deliberately not the full [`crate::publication::KnowledgeVersion`]: the refresh status
/// and the change list refer to versions constantly, and carrying nine counters into each
/// reference would make the interesting part hard to find.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionRef {
    pub id: Uuid,
    pub number: i32,
    pub status: String,
    pub published_at: Option<DateTime<Utc>>,
}

// --- the event log --------------------------------------------------------------------

/// What happened. A closed vocabulary, matching the CHECK in `0007_updates.sql`.
///
/// The list is short on purpose. An event exists here when a person reading the partner's
/// history would ask "and then what?" without it — not for every state a row can be in.
/// Progress within a job is the job's `stage`, and inventing an event for each would turn
/// the log into a second, worse copy of the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    MaterialUploaded,
    /// The same bytes were uploaded again. Recorded because "I uploaded it and nothing
    /// happened" has to have an answer, and "it was already here" is that answer.
    MaterialDuplicate,
    MaterialReprocessRequested,
    MaterialExtractionFinished,
    UnderstandingQueued,
    UnderstandingFinished,
    ValidationQueued,
    ValidationFinished,
    VersionPublished,
    VersionBlocked,
    VersionSuperseded,
    VersionRetracted,
    RefreshRequested,
    ExportRead,
    JobFailed,
    RetentionApplied,
}

impl EventKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MaterialUploaded => "material_uploaded",
            Self::MaterialDuplicate => "material_duplicate",
            Self::MaterialReprocessRequested => "material_reprocess_requested",
            Self::MaterialExtractionFinished => "material_extraction_finished",
            Self::UnderstandingQueued => "understanding_queued",
            Self::UnderstandingFinished => "understanding_finished",
            Self::ValidationQueued => "validation_queued",
            Self::ValidationFinished => "validation_finished",
            Self::VersionPublished => "version_published",
            Self::VersionBlocked => "version_blocked",
            Self::VersionSuperseded => "version_superseded",
            Self::VersionRetracted => "version_retracted",
            Self::RefreshRequested => "refresh_requested",
            Self::ExportRead => "export_read",
            Self::JobFailed => "job_failed",
            Self::RetentionApplied => "retention_applied",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "material_uploaded" => Self::MaterialUploaded,
            "material_duplicate" => Self::MaterialDuplicate,
            "material_reprocess_requested" => Self::MaterialReprocessRequested,
            "material_extraction_finished" => Self::MaterialExtractionFinished,
            "understanding_queued" => Self::UnderstandingQueued,
            "understanding_finished" => Self::UnderstandingFinished,
            "validation_queued" => Self::ValidationQueued,
            "validation_finished" => Self::ValidationFinished,
            "version_published" => Self::VersionPublished,
            "version_blocked" => Self::VersionBlocked,
            "version_superseded" => Self::VersionSuperseded,
            "version_retracted" => Self::VersionRetracted,
            "refresh_requested" => Self::RefreshRequested,
            "export_read" => Self::ExportRead,
            "job_failed" => Self::JobFailed,
            "retention_applied" => Self::RetentionApplied,
            _ => return None,
        })
    }
}

/// Who caused an event.
///
/// There is one human account in the local pilot, so `Owner` is as precise as this can
/// honestly be. A per-user identity belongs with the accounts that would need it, and
/// writing a user id column now would mean writing the same constant into every row while
/// implying it could be different.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventActor {
    Owner,
    Worker,
    System,
}

impl EventActor {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Worker => "worker",
            Self::System => "system",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "owner" => Self::Owner,
            "worker" => Self::Worker,
            "system" => Self::System,
            _ => return None,
        })
    }
}

/// One line of the partner's history, as the API returns it.
#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub id: Uuid,
    pub partner_id: Option<Uuid>,
    pub kind: EventKind,
    pub actor: EventActor,
    pub material_id: Option<Uuid>,
    pub version_id: Option<Uuid>,
    pub job_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    /// Shown verbatim. The server writes the sentence because the server is what knows
    /// what happened; the interface does not reassemble one from the kind.
    pub summary: String,
    /// Small structured payload — counters, a version number, a fingerprint. Never a
    /// secret: nothing that writes an event has access to one.
    pub detail: serde_json::Value,
    pub occurred_at: DateTime<Utc>,
}

// --- refresh status -------------------------------------------------------------------

/// Where the partner's published knowledge stands relative to their documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefreshState {
    /// Nothing has ever been published for this partner.
    NeverPublished,
    /// The published version was built from exactly the candidates that exist now.
    Current,
    /// Something downstream of the published version has moved: a new document, a
    /// re-reading of an old one, or a changed candidate set. A new check would produce a
    /// different version. `reasons` says which, and names the source.
    RevalidationRequired,
    /// A check is queued or running right now. Whatever is published is what the previous
    /// check produced; this state exists so the interface does not offer to start a
    /// second one.
    Checking,
    /// The published version was withdrawn and nothing replaced it. Search answers
    /// `no_published_version` until a new check publishes one.
    Retracted,
}

impl RefreshState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NeverPublished => "never_published",
            Self::Current => "current",
            Self::RevalidationRequired => "revalidation_required",
            Self::Checking => "checking",
            Self::Retracted => "retracted",
        }
    }
}

/// Why a revalidation is required, as a code the interface can branch on next to the
/// sentence it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefreshReasonCode {
    /// A document is uploaded and has not been read yet.
    MaterialNotRead,
    /// A document has been read and never handed to the product role.
    MaterialNotDrafted,
    /// A document was re-read after the draft that is in the published version was made.
    /// The published citations still say what they said — they are copies — but the
    /// current reading of the file may say something else.
    SourceReread,
    /// Today's candidate set is not the one the published version was built from.
    CandidatesChanged,
    /// The published version predates the candidate fingerprint, so the cheap comparison
    /// cannot be made. Not "unchanged": unknown, and said so.
    ComparisonUnavailable,
    /// The last check produced a version that did not pass the publication rules.
    LastCheckBlocked,
    /// The last check failed outright.
    LastCheckFailed,
    /// The published version was withdrawn.
    VersionRetracted,
    /// Nothing has ever been published.
    NothingPublished,
}

impl RefreshReasonCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MaterialNotRead => "material_not_read",
            Self::MaterialNotDrafted => "material_not_drafted",
            Self::SourceReread => "source_reread",
            Self::CandidatesChanged => "candidates_changed",
            Self::ComparisonUnavailable => "comparison_unavailable",
            Self::LastCheckBlocked => "last_check_blocked",
            Self::LastCheckFailed => "last_check_failed",
            Self::VersionRetracted => "version_retracted",
            Self::NothingPublished => "nothing_published",
        }
    }
}

/// One reason, tied to the exact source or version it is about.
#[derive(Debug, Clone, Serialize)]
pub struct RefreshReason {
    pub code: RefreshReasonCode,
    /// Shown verbatim.
    pub message: String,
    pub material_id: Option<Uuid>,
    pub material_filename: Option<String>,
    pub version_id: Option<Uuid>,
    pub version_number: Option<i32>,
    /// The reading of the document that exists now.
    pub content_revision: Option<i32>,
    /// The reading the current draft was made from, when they differ.
    pub drafted_revision: Option<i32>,
}

/// Where one document stands in the cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
    /// Uploaded, not read yet (or being read now).
    Reading,
    /// Read, but nothing could be extracted from it.
    Unreadable,
    /// Read and not usefully drafted: never attempted, being drafted right now, or a
    /// drafting run that failed or stopped for want of a model.
    ///
    /// The three are one state on purpose — they need the same thing done about them —
    /// and `SourceRefresh::draft_status` says which it is.
    NotDrafted,
    /// Read, and a **finished** drafting run used the reading that exists now.
    Drafted,
    /// Drafted, then re-read. The draft describes an older reading of this file.
    RereadAfterDraft,
}

impl SourceState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reading => "reading",
            Self::Unreadable => "unreadable",
            Self::NotDrafted => "not_drafted",
            Self::Drafted => "drafted",
            Self::RereadAfterDraft => "reread_after_draft",
        }
    }
}

/// One document, and what the published version knows about it.
#[derive(Debug, Clone, Serialize)]
pub struct SourceRefresh {
    pub material_id: Uuid,
    pub filename: String,
    pub material_status: MaterialStatus,
    pub state: SourceState,
    /// How many times this document has been read. A changed file is a different
    /// material (deduplication is by content), so this only ever counts re-readings.
    pub content_revision: i32,
    /// Which reading the current draft used. `null` for a draft made before this was
    /// recorded, which is reported as unknown rather than as current.
    pub drafted_revision: Option<i32>,
    /// The 1C run's own status (`queued`, `running`, `completed`, `partial`, `failed`,
    /// `needs_provider`), or `null` when the product role has never run over this
    /// document.
    ///
    /// It is on the wire because `drafted_revision` alone cannot tell a finished draft
    /// from a failed one: the revision is recorded when a run *starts*, so a run that
    /// then failed leaves a number that looks exactly like success. Reading only that
    /// number reported a document with no facts as "разобран" and left the owner with no
    /// button that would do anything about it.
    pub draft_status: Option<String>,
    pub facts_drafted: i32,
    /// How many claims of the published version cite this document. Zero for a document
    /// that contributed nothing — which is a fact about the version, not about the file.
    pub claims_in_published: i32,
    pub message: String,
}

/// The whole answer of `GET /api/partners/{id}/refresh`.
#[derive(Debug, Clone, Serialize)]
pub struct RefreshStatus {
    pub state: RefreshState,
    pub published: Option<VersionRef>,
    /// The newest version of any status, so a blocked one is visible without listing.
    pub latest: Option<VersionRef>,
    pub reasons: Vec<RefreshReason>,
    pub sources: Vec<SourceRefresh>,
    /// Fingerprint of the candidates that exist right now.
    pub candidate_fingerprint: String,
    /// Fingerprint of the candidates the published version was built from, when it
    /// recorded one.
    pub published_candidate_fingerprint: Option<String>,
    /// Whether a check is queued or running.
    pub checking: bool,
    pub message: String,
    pub computed_at: DateTime<Utc>,
}

// --- starting a new cycle -------------------------------------------------------------

/// Which stage of the pipeline a refresh step is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefreshStepKind {
    Extraction,
    Understanding,
    Validation,
}

impl RefreshStepKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Extraction => "extraction",
            Self::Understanding => "understanding",
            Self::Validation => "validation",
        }
    }
}

/// What actually happened to that step when the owner asked for a refresh.
///
/// `Queued` is the only outcome that means work will happen. Every other value is a
/// refusal with a reason — nothing here reports progress that was not started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefreshStepOutcome {
    Queued,
    /// Already queued or running: the existing job is returned rather than a second one
    /// armed.
    AlreadyRunning,
    /// Nothing to do at this stage.
    UpToDate,
    /// The stage needs an adapter that is not configured. Named, not skipped: a pipeline
    /// that silently stops at 1C looks identical to one that finished.
    NeedsProvider,
    /// The stage cannot run yet because the one before it has not produced anything.
    Waiting,
}

impl RefreshStepOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::AlreadyRunning => "already_running",
            Self::UpToDate => "up_to_date",
            Self::NeedsProvider => "needs_provider",
            Self::Waiting => "waiting",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RefreshStep {
    pub kind: RefreshStepKind,
    pub outcome: RefreshStepOutcome,
    pub material_id: Option<Uuid>,
    pub material_filename: Option<String>,
    pub job_id: Option<Uuid>,
    pub message: String,
}

/// What `POST /api/partners/{id}/refresh` did — one line per stage it touched.
#[derive(Debug, Clone, Serialize)]
pub struct RefreshPlan {
    pub steps: Vec<RefreshStep>,
    /// How many steps were really queued. The interface shows this rather than a
    /// percentage: nothing here knows how long any of it takes.
    pub queued: i32,
    pub message: String,
    pub requested_at: DateTime<Utc>,
}

// --- comparing two versions -----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Removed,
    Changed,
}

impl ChangeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Changed => "changed",
        }
    }
}

/// One side of a comparison: what a claim said in one of the two versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClaimSide {
    pub claim_id: Uuid,
    pub status: String,
    pub value_text: String,
    pub unit: Option<String>,
    pub conditions: Option<String>,
    /// Where it came from, as the version recorded it: a filename with a page, or a host.
    /// Copies, like everything else in a snapshot.
    pub sources: Vec<String>,
}

/// A property that appeared, disappeared or moved between two versions.
#[derive(Debug, Clone, Serialize)]
pub struct ClaimChange {
    pub kind: ChangeKind,
    pub scope: String,
    pub product_name: Option<String>,
    pub attribute: String,
    pub before: Option<ClaimSide>,
    pub after: Option<ClaimSide>,
    /// Which fields differ, for a `Changed` entry: `value_text`, `unit`, `conditions`,
    /// `status`, `sources`.
    pub fields: Vec<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadinessChange {
    pub topic: ReadinessTopic,
    pub before: Option<ReadinessState>,
    pub after: Option<ReadinessState>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GapChange {
    pub kind: ChangeKind,
    pub topic: String,
    pub missing: String,
    pub product_name: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ChangeCounts {
    pub added: i32,
    pub removed: i32,
    pub changed: i32,
    pub unchanged: i32,
}

/// The answer of `GET /api/partners/{id}/versions/{id}/changes`.
#[derive(Debug, Clone, Serialize)]
pub struct VersionChanges {
    /// The older version. `null` when this is the partner's first version — then every
    /// claim is an addition and saying so is more honest than comparing with nothing and
    /// calling the result "no changes".
    pub from: Option<VersionRef>,
    pub to: VersionRef,
    pub counts: ChangeCounts,
    pub claims: Vec<ClaimChange>,
    pub readiness: Vec<ReadinessChange>,
    pub gaps: Vec<GapChange>,
    /// What this comparison cannot see. Shown next to the result, not buried.
    pub limitations: Vec<String>,
    pub message: String,
}

// --- export ---------------------------------------------------------------------------

/// Identifier of the export format. A downstream agent that pins this string knows what
/// it is parsing; a format change gets a new one rather than a silent new field order.
pub const EXPORT_SCHEMA: &str = "otdel.knowledge-version.v1";

#[derive(Debug, Clone, Serialize)]
pub struct ExportManifest {
    pub schema: &'static str,
    pub generated_at: DateTime<Utc>,
    pub bureau_slug: String,
    pub partner_id: Uuid,
    pub partner_name: String,
    pub version_id: Uuid,
    pub version_number: i32,
    pub version_status: String,
    pub published_at: Option<DateTime<Utc>>,
    pub superseded_at: Option<DateTime<Utc>>,
    pub input_fingerprint: String,
    pub candidate_fingerprint: Option<String>,
    pub claims_total: i32,
    pub claims_source_supported: i32,
    pub gaps_total: i32,
    /// The caveats that travel with the document, in words, because an export is read
    /// away from every screen that would otherwise say them.
    pub disclosure: Vec<String>,
}

/// The disclosure block, written once so every export carries the same sentences.
///
/// These are not decoration. An export is the one artefact of this system that leaves the
/// interface, and `block-01-spec.md` §6.6 requires that it respect disclosure limits and
/// not add commercial terms nobody wrote down. A reader who has only the file has to be
/// able to see what `source_supported` does and does not mean.
pub fn export_disclosure() -> Vec<String> {
    vec![
        "«Подтверждено источником» означает, что фрагмент найден в прочитанном документе \
         по записанным смещениям. Это не независимая проверка, не подтверждение \
         производителя и не гарантия истинности документа."
            .to_owned(),
        "Готовность — это доступность знаний, а не разрешение на рассылку, сделку или \
         обещание технической совместимости."
            .to_owned(),
        "Коммерческие условия включены только в том виде, в каком они записаны в \
         источниках. Отсутствующая цена или срок остаются пробелом и не дополняются."
            .to_owned(),
        "Это снимок одной версии. Более новая версия могла быть опубликована после \
         выгрузки; версия и её отпечаток названы в манифесте."
            .to_owned(),
    ]
}

// --- retention ------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionState {
    /// Nothing is pruned. The default: a pilot that starts deleting history because a
    /// default said so is a pilot that loses the evidence of its own first month.
    KeepEverything,
    Enabled,
}

impl RetentionState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::KeepEverything => "keep_everything",
            Self::Enabled => "enabled",
        }
    }
}

/// What a sweep would remove right now.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct RetentionPreview {
    pub events_prunable: i64,
    pub jobs_prunable: i64,
    pub events_total: i64,
    pub jobs_total: i64,
    pub oldest_event: Option<DateTime<Utc>>,
}

/// The answer of `GET /api/retention`.
#[derive(Debug, Clone, Serialize)]
pub struct RetentionPolicyView {
    pub state: RetentionState,
    pub event_days: Option<u32>,
    pub job_days: Option<u32>,
    pub keep_per_kind: u32,
    pub sweep_interval_seconds: u64,
    pub preview: RetentionPreview,
    /// What retention never touches, in words. The list is the policy's most important
    /// half and is returned rather than documented elsewhere.
    pub protected: Vec<String>,
    /// The last sweep that ran, from the log it wrote.
    pub last_sweep: Option<Event>,
    pub message: String,
}

/// What retention is not allowed to remove, stated once.
pub fn retention_protected() -> Vec<String> {
    vec![
        "Опубликованная версия знаний и её снимок (утверждения, цитаты, пробелы, \
         готовность) не удаляются никогда — ни после замены, ни после отзыва. Версия \
         отзывается, а не стирается."
            .to_owned(),
        "Оригиналы материалов и их страницы не удаляются: на них ссылаются цитаты.".to_owned(),
        "Незавершённые задания (в очереди или выполняющиеся) не удаляются: это работа, а \
         не история."
            .to_owned(),
        "Запись о самой очистке остаётся в журнале: она новее горизонта, который только \
         что применила."
            .to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_event_kind_round_trips_through_its_database_spelling() {
        for kind in [
            EventKind::MaterialUploaded,
            EventKind::MaterialDuplicate,
            EventKind::MaterialReprocessRequested,
            EventKind::MaterialExtractionFinished,
            EventKind::UnderstandingQueued,
            EventKind::UnderstandingFinished,
            EventKind::ValidationQueued,
            EventKind::ValidationFinished,
            EventKind::VersionPublished,
            EventKind::VersionBlocked,
            EventKind::VersionSuperseded,
            EventKind::VersionRetracted,
            EventKind::RefreshRequested,
            EventKind::ExportRead,
            EventKind::JobFailed,
            EventKind::RetentionApplied,
        ] {
            assert_eq!(EventKind::parse(kind.as_str()), Some(kind), "{kind:?}");
        }
        assert_eq!(EventKind::parse("something_else"), None);
    }

    #[test]
    fn an_event_kind_serialises_as_the_string_the_database_stores() {
        // The wire spelling and the CHECK constraint's spelling have to be the same, or a
        // value that round-trips through the database changes name on the way to the
        // interface.
        let rendered = serde_json::to_string(&EventKind::VersionRetracted).unwrap();
        assert_eq!(rendered, "\"version_retracted\"");
        assert_eq!(
            serde_json::from_str::<EventKind>(&rendered).unwrap(),
            EventKind::VersionRetracted
        );
    }

    #[test]
    fn actors_round_trip_and_reject_anything_else() {
        for actor in [EventActor::Owner, EventActor::Worker, EventActor::System] {
            assert_eq!(EventActor::parse(actor.as_str()), Some(actor));
        }
        assert_eq!(EventActor::parse("admin"), None);
    }

    #[test]
    fn refresh_states_and_reasons_have_distinct_wire_names() {
        let states = [
            RefreshState::NeverPublished,
            RefreshState::Current,
            RefreshState::RevalidationRequired,
            RefreshState::Checking,
            RefreshState::Retracted,
        ];
        let names: std::collections::BTreeSet<&str> =
            states.iter().map(|state| state.as_str()).collect();
        assert_eq!(names.len(), states.len(), "two states share a name");

        let codes = [
            RefreshReasonCode::MaterialNotRead,
            RefreshReasonCode::MaterialNotDrafted,
            RefreshReasonCode::SourceReread,
            RefreshReasonCode::CandidatesChanged,
            RefreshReasonCode::ComparisonUnavailable,
            RefreshReasonCode::LastCheckBlocked,
            RefreshReasonCode::LastCheckFailed,
            RefreshReasonCode::VersionRetracted,
            RefreshReasonCode::NothingPublished,
        ];
        let names: std::collections::BTreeSet<&str> =
            codes.iter().map(|code| code.as_str()).collect();
        assert_eq!(names.len(), codes.len(), "two reason codes share a name");
    }

    #[test]
    fn a_step_outcome_other_than_queued_never_reads_as_work_started() {
        // The interface counts `Queued` and nothing else; this test is what keeps a
        // future outcome from quietly joining that count.
        for outcome in [
            RefreshStepOutcome::AlreadyRunning,
            RefreshStepOutcome::UpToDate,
            RefreshStepOutcome::NeedsProvider,
            RefreshStepOutcome::Waiting,
        ] {
            assert_ne!(outcome, RefreshStepOutcome::Queued);
            assert_ne!(outcome.as_str(), RefreshStepOutcome::Queued.as_str());
        }
    }

    #[test]
    fn the_export_carries_its_caveats_and_names_its_format() {
        assert_eq!(EXPORT_SCHEMA, "otdel.knowledge-version.v1");
        let disclosure = export_disclosure();
        assert!(disclosure.len() >= 4);
        assert!(
            disclosure
                .iter()
                .any(|line| line.contains("не независимая проверка")),
            "an export must say what «подтверждено источником» does not mean"
        );
        assert!(
            disclosure
                .iter()
                .any(|line| line.contains("не разрешение на рассылку")),
            "an export must say that readiness authorises nothing"
        );
    }

    #[test]
    fn retention_states_what_it_will_never_remove() {
        let protected = retention_protected();
        assert!(protected
            .iter()
            .any(|line| line.contains("Опубликованная версия")));
        assert!(protected.iter().any(|line| line.contains("Оригиналы")));
    }
}
