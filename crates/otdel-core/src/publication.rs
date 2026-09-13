//! Domain model of phase 1E — verification, the immutable knowledge version, and what
//! other agents are allowed to read.
//!
//! Phases 1C and 1D produce **candidates**. This phase decides, by rules a person can
//! re-run by hand, which of them a version may carry and what each one is worth, then
//! freezes the result. The rules the types enforce come from `docs/block-01-spec.md`
//! §6.7, §7 and §9:
//!
//! * **a verdict is not a certificate.** [`ClaimStatus::SourceSupported`] means the
//!   cited fragment really says this, in the document that was really read. It is not a
//!   manufacturer's confirmation and not a guarantee of truth, and nothing in this
//!   module is allowed to imply otherwise;
//! * **a version is immutable and there is at most one published.** [`KnowledgeVersion`]
//!   is a snapshot, not a view over the candidates; migration `0006_publication.sql`
//!   refuses to change it and refuses a second published row;
//! * **readiness is availability of knowledge, never permission.** [`ReadinessTopic`] is
//!   decided per topic so that a missing price limits commercial answers without
//!   blocking a product description — and so that partial readiness cannot be mistaken
//!   for a full commercial clearance (`block-01-spec.md` §13.7);
//! * **an answer cites or admits it cannot.** [`AnswerState`] has no variant that means
//!   "answered without sources".

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Lifecycle of one verification run over one partner's candidates.
///
/// There is deliberately no `needs_provider` here, unlike [`crate::knowledge::
/// KnowledgeRunStatus`]. Verification is deterministic: it re-reads the stored page
/// text, re-checks every quotation by its offsets and decides. A model may add a second
/// opinion when one is configured, and it may only raise doubt — "совпадение ответов
/// двух моделей не является доказательством" (`block-01-spec.md` §6.7). So the checker
/// runs, and a version publishes, with no key configured at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationRunStatus {
    Queued,
    Running,
    /// Every candidate was checked and the version was published.
    Completed,
    /// Checked, but something is not whole: candidates were refused, the version was
    /// blocked by a readiness rule, or a model review could not be made.
    Partial,
    Failed,
}

impl ValidationRunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Partial => "partial",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "partial" => Some(Self::Partial),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }

    /// Whether a run in this state is still expected to do something.
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}

/// State of one knowledge version (`block-01-spec.md` §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionStatus {
    Draft,
    Validating,
    /// The current version. At most one per partner, enforced by a partial unique index.
    Published,
    /// The rules were not met. The snapshot exists and can be inspected, so the owner can
    /// see what *would* be published, but nothing answers a customer from it.
    Blocked,
    /// Was published; a newer version took its place. Still readable when pinned.
    Superseded,
    /// Withdrawn by the owner. Removed from search at once, and kept as history —
    /// "откат не воскрешает отозванные источники".
    Revoked,
}

impl VersionStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Validating => "validating",
            Self::Published => "published",
            Self::Blocked => "blocked",
            Self::Superseded => "superseded",
            Self::Revoked => "revoked",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "draft" => Some(Self::Draft),
            "validating" => Some(Self::Validating),
            "published" => Some(Self::Published),
            "blocked" => Some(Self::Blocked),
            "superseded" => Some(Self::Superseded),
            "revoked" => Some(Self::Revoked),
            _ => None,
        }
    }

    /// May a search or an answer read this version when it is pinned by id?
    ///
    /// `published` is the current one and `superseded` is a version that *was* published
    /// and has since been replaced — pinning it is the whole point of pinning, and its
    /// snapshot is immutable, so it still says exactly what it said. Everything else is
    /// refused: a draft was never published, a blocked version failed the rules, and a
    /// revoked one was withdrawn.
    pub const fn is_readable(self) -> bool {
        matches!(self, Self::Published | Self::Superseded)
    }
}

/// The checker's verdict on one statement (`block-01-spec.md` §6.7).
///
/// Each value answers a different question, and the differences are what keep the
/// interface honest:
///
/// | Verdict | What was found |
/// |---|---|
/// | `source_supported` | the citation is still there, word for word, and it contains the value |
/// | `hypothesis` | the citation is real, but it does not state this value |
/// | `unknown` | the cited source can no longer be read at all, so nothing can be checked |
/// | `conflicted` | another statement about the same subject and property says something else |
/// | `stale` | the source was re-read and no longer says what was quoted |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    /// Supported **by its source**. Not independently verified, not guaranteed true.
    SourceSupported,
    Hypothesis,
    Unknown,
    Conflicted,
    Stale,
}

impl ClaimStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceSupported => "source_supported",
            Self::Hypothesis => "hypothesis",
            Self::Unknown => "unknown",
            Self::Conflicted => "conflicted",
            Self::Stale => "stale",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "source_supported" => Some(Self::SourceSupported),
            "hypothesis" => Some(Self::Hypothesis),
            "unknown" => Some(Self::Unknown),
            "conflicted" => Some(Self::Conflicted),
            "stale" => Some(Self::Stale),
            _ => None,
        }
    }

    /// May this statement be used as the basis of a confident answer?
    ///
    /// Only one verdict qualifies. A hypothesis, an unreadable source, a contradiction
    /// and a source that has moved on are all shown — with their verdict — and none of
    /// them carries an answer (`block-01-spec.md` §13.6).
    pub const fn is_answerable(self) -> bool {
        matches!(self, Self::SourceSupported)
    }
}

/// Where a claim in a version came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimOrigin {
    /// A fact drafted by phase 1C from the partner's own material.
    PartnerMaterial,
    /// A conclusion of phase 1D's bounded industry research.
    IndustryResearch,
}

impl ClaimOrigin {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PartnerMaterial => "partner_material",
            Self::IndustryResearch => "industry_research",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "partner_material" => Some(Self::PartnerMaterial),
            "industry_research" => Some(Self::IndustryResearch),
            _ => None,
        }
    }

    /// The scope that goes with this origin. They cannot disagree: a database CHECK ties
    /// them together, because an industry conclusion that became a partner claim is the
    /// exact failure `block-01-plan.md` 1D §4 forbids.
    pub const fn scope(self) -> ClaimScope {
        match self {
            Self::PartnerMaterial => ClaimScope::Partner,
            Self::IndustryResearch => ClaimScope::Industry,
        }
    }
}

/// What a claim is about: this partner's goods, or the industry around them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimScope {
    Partner,
    Industry,
}

impl ClaimScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Partner => "partner",
            Self::Industry => "industry",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "partner" => Some(Self::Partner),
            "industry" => Some(Self::Industry),
            _ => None,
        }
    }
}

/// Which kind of source a citation points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSourceKind {
    /// A page of the partner's own material. Opens at `.../original#page=N`.
    Material,
    /// A page downloaded by phase 1D, identified by URL, hash and retrieval time.
    External,
}

impl EvidenceSourceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Material => "material",
            Self::External => "external",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "material" => Some(Self::Material),
            "external" => Some(Self::External),
            _ => None,
        }
    }
}

/// The four things readiness is decided about, separately (`block-01-spec.md` §7).
///
/// Separately is the point. A catalogue that states loads but no prices is ready to
/// answer about characteristics and not ready to answer about commercial terms, and
/// collapsing that into one "готово" would either hide real knowledge or promise terms
/// nobody wrote down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessTopic {
    /// Describing what the partner offers.
    ProductDescription,
    /// Proposing who might buy it and where it is used.
    AudienceHypotheses,
    /// Answering about measurable properties and limits.
    CharacteristicAnswers,
    /// Answering about price, lead time and packaging.
    CommercialAnswers,
}

impl ReadinessTopic {
    pub const ALL: [Self; 4] = [
        Self::ProductDescription,
        Self::AudienceHypotheses,
        Self::CharacteristicAnswers,
        Self::CommercialAnswers,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProductDescription => "product_description",
            Self::AudienceHypotheses => "audience_hypotheses",
            Self::CharacteristicAnswers => "characteristic_answers",
            Self::CommercialAnswers => "commercial_answers",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "product_description" => Some(Self::ProductDescription),
            "audience_hypotheses" => Some(Self::AudienceHypotheses),
            "characteristic_answers" => Some(Self::CharacteristicAnswers),
            "commercial_answers" => Some(Self::CommercialAnswers),
            _ => None,
        }
    }
}

/// How ready one topic is.
///
/// This says what the knowledge base **can answer**, and nothing else. It is not a
/// permission to send a message, to promise technical compatibility or to take on an
/// obligation (`block-01-spec.md` §7); the interface is required to say that next to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessState {
    Ready,
    /// Answerable with stated reservations — a gap, a contradiction, a stale source.
    Limited,
    Blocked,
}

impl ReadinessState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Limited => "limited",
            Self::Blocked => "blocked",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ready" => Some(Self::Ready),
            "limited" => Some(Self::Limited),
            "blocked" => Some(Self::Blocked),
            _ => None,
        }
    }
}

/// How a search request was actually served.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    /// Exact values, full text **and** vectors.
    Hybrid,
    /// Exact values and full text only: there are no vectors to compare against. This is
    /// the honest state without an embedding provider, and it is reported, never hidden
    /// behind a result list that looks complete.
    Keyword,
}

impl SearchMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hybrid => "hybrid",
            Self::Keyword => "keyword",
        }
    }
}

/// Why a search hit was returned. Never empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    /// The query names the value or the attribute as written — an article, a parameter.
    Exact,
    /// Full-text match over the claim and its citations.
    Keyword,
    /// Vector neighbourhood within one embedding profile.
    Vector,
}

impl MatchKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Keyword => "keyword",
            Self::Vector => "vector",
        }
    }
}

/// Outcome of one search request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchState {
    Ok,
    /// This partner has nothing published: never validated, blocked by the rules, or
    /// retracted. A named state, not an empty result that looks like "nothing matches".
    NoPublishedVersion,
    /// There is a version, and nothing in it matches.
    InsufficientEvidence,
}

impl SearchState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::NoPublishedVersion => "no_published_version",
            Self::InsufficientEvidence => "insufficient_evidence",
        }
    }
}

/// Outcome of one question.
///
/// There is no variant meaning "answered, sources unavailable". An answer either carries
/// citations into the pinned version or it is not an answer — which is the whole of
/// `block-01-spec.md` §9 in one enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerState {
    /// A model composed prose **and** every sentence rests on claims of this version.
    /// `citations` is non-empty; the server, not the model, chose what goes in it.
    Answered,
    /// Supporting claims with their citations were found, and no prose was composed:
    /// no model is configured, or its answer did not survive checking. Still useful, and
    /// honest about what it is.
    EvidenceOnly,
    /// The version holds nothing that answers this. The gap is named if one is recorded;
    /// a market guess is never substituted (`block-01-spec.md` §13.5).
    InsufficientEvidence,
    NoPublishedVersion,
}

impl AnswerState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Answered => "answered",
            Self::EvidenceOnly => "evidence_only",
            Self::InsufficientEvidence => "insufficient_evidence",
            Self::NoPublishedVersion => "no_published_version",
        }
    }

    /// Does this outcome oblige the response to carry citations?
    pub const fn requires_citations(self) -> bool {
        matches!(self, Self::Answered)
    }
}

// --- stored entities ------------------------------------------------------------------

/// One immutable snapshot of a partner's knowledge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeVersion {
    pub id: Uuid,
    pub partner_id: Uuid,
    /// Sequential per partner, from 1, never reused.
    pub number: i32,
    pub status: VersionStatus,
    pub validation_run_id: Option<Uuid>,
    /// SHA-256 over the candidate set this version was built from. A run whose input is
    /// the fingerprint already published does not publish again — that is what keeps a
    /// repeated upload from producing a second published result (`block-01-spec.md`
    /// §13.4), and what keeps a late run from overwriting a newer one (§7).
    pub input_fingerprint: String,
    /// Phase 1F: SHA-256 over the same candidate set **without** the verdicts.
    ///
    /// `null` for a version published before this was recorded. That is reported as a
    /// comparison the refresh status cannot make, never as "ничего не изменилось" —
    /// answering a question nobody computed is the failure this field exists to avoid.
    pub candidate_fingerprint: Option<String>,
    pub claims_total: i32,
    pub claims_source_supported: i32,
    pub claims_hypothesis: i32,
    pub claims_unknown: i32,
    pub claims_conflicted: i32,
    pub claims_stale: i32,
    pub gaps_open: i32,
    pub chunks_total: i32,
    pub chunks_embedded: i32,
    pub embedding_profile: Option<String>,
    pub readiness: Vec<ReadinessEntry>,
    /// Why this version was not published, in words. Non-empty exactly when the status
    /// is `blocked`.
    pub blocked_reasons: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
    pub superseded_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_reason: Option<String>,
}

/// Readiness of one topic, with the sentence that explains it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessEntry {
    pub topic: ReadinessTopic,
    pub state: ReadinessState,
    /// Shown verbatim. The interface does not paraphrase it.
    pub reason: String,
}

/// One statement inside a version, with the checker's verdict and its own copy of the
/// evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionClaim {
    pub id: Uuid,
    pub version_id: Uuid,
    pub origin: ClaimOrigin,
    /// The candidate this was copied from. For tracing only — the snapshot does not
    /// depend on that row still existing.
    pub origin_id: Uuid,
    pub scope: ClaimScope,
    /// Always `None` when the scope is `industry`: a database CHECK makes it impossible
    /// for an industry conclusion to name a partner's product.
    pub product_name: Option<String>,
    pub kind: crate::knowledge::FactKind,
    pub status: ClaimStatus,
    pub attribute: String,
    /// The value exactly as the source writes it. Never reformatted, never rounded.
    pub value_text: String,
    pub unit: Option<String>,
    pub conditions: Option<String>,
    /// The drafting model's own words, carried over. Never a quotation.
    pub model_context: Option<String>,
    /// Why the checker returned this verdict.
    pub check_note: Option<String>,
    pub evidence: Vec<VersionEvidence>,
    pub created_at: DateTime<Utc>,
}

/// A citation frozen into a version: the fragment, where it was, and when it was read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionEvidence {
    pub id: Uuid,
    pub claim_id: Uuid,
    pub source_kind: EvidenceSourceKind,
    pub material_id: Option<Uuid>,
    pub material_filename: Option<String>,
    pub page_number: Option<i32>,
    pub region_id: Option<Uuid>,
    pub url: Option<String>,
    pub host: Option<String>,
    pub retrieved_at: Option<DateTime<Utc>>,
    pub content_hash: Option<String>,
    /// The source's own wording, copied into the version at publication time.
    pub quote: String,
    pub char_start: i32,
    pub char_end: i32,
}

/// A gap carried into the version, with the answers it limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionGap {
    pub id: Uuid,
    pub version_id: Uuid,
    pub origin_id: Uuid,
    pub product_name: Option<String>,
    pub topic: String,
    pub missing: String,
    pub blocks: Option<String>,
    /// Which readiness topics this gap limits. Derived by a lexical rule over a small
    /// fixed vocabulary — useful, and not an understanding of the sentence.
    pub blocks_topics: Vec<ReadinessTopic>,
    pub created_at: DateTime<Utc>,
}

/// State of the checker over one partner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationRun {
    pub id: Uuid,
    pub partner_id: Uuid,
    pub status: ValidationRunStatus,
    pub prompt_profile: String,
    pub version_id: Option<Uuid>,
    pub version_number: Option<i32>,
    pub claims_considered: i32,
    pub claims_source_supported: i32,
    pub claims_hypothesis: i32,
    pub claims_unknown: i32,
    pub claims_conflicted: i32,
    pub claims_stale: i32,
    pub claims_rejected: i32,
    pub gaps_carried: i32,
    pub chunks_created: i32,
    pub chunks_embedded: i32,
    /// How many claims a model looked at. `0` is the normal state with no key, and it
    /// does not lower the run's status.
    pub model_reviewed: i32,
    pub published: bool,
    pub rejections: Vec<String>,
    pub blocked_reasons: Vec<String>,
    pub diagnostic: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// How many candidates a partner has right now.
///
/// The interface uses it to avoid offering a check where there is nothing to check, and
/// the API uses it to refuse one for the same reason.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateSummary {
    pub facts: i32,
    pub findings: i32,
    pub gaps_open: i32,
    pub materials_drafted: i32,
}

impl CandidateSummary {
    /// Is there anything at all for the checker to look at?
    pub const fn is_empty(self) -> bool {
        self.facts == 0 && self.findings == 0
    }
}

/// Stable idempotency key of a partner's validation job.
///
/// Keyed by the partner, not by the run: pressing "проверить" twice, or a material
/// finishing twice, must reuse one queue row (`block-01-spec.md` §13.4).
pub fn validation_idempotency_key(partner_id: Uuid) -> String {
    format!("validate_partner:{partner_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_status_survives_a_round_trip_through_its_string() {
        for status in [
            ClaimStatus::SourceSupported,
            ClaimStatus::Hypothesis,
            ClaimStatus::Unknown,
            ClaimStatus::Conflicted,
            ClaimStatus::Stale,
        ] {
            assert_eq!(ClaimStatus::parse(status.as_str()), Some(status));
        }
        for status in [
            VersionStatus::Draft,
            VersionStatus::Validating,
            VersionStatus::Published,
            VersionStatus::Blocked,
            VersionStatus::Superseded,
            VersionStatus::Revoked,
        ] {
            assert_eq!(VersionStatus::parse(status.as_str()), Some(status));
        }
        for topic in ReadinessTopic::ALL {
            assert_eq!(ReadinessTopic::parse(topic.as_str()), Some(topic));
        }
    }

    #[test]
    fn only_a_source_supported_claim_may_carry_an_answer() {
        // The other four verdicts each mean something is wrong with the evidence, and a
        // confident answer built on any of them would be the failure of
        // `block-01-spec.md` §13.6.
        assert!(ClaimStatus::SourceSupported.is_answerable());
        for status in [
            ClaimStatus::Hypothesis,
            ClaimStatus::Unknown,
            ClaimStatus::Conflicted,
            ClaimStatus::Stale,
        ] {
            assert!(
                !status.is_answerable(),
                "{} must not answer",
                status.as_str()
            );
        }
    }

    #[test]
    fn a_draft_blocked_or_revoked_version_can_never_be_searched() {
        // Pinning a version that was published and has since been replaced is the point
        // of pinning; the other four states were never, or are no longer, publishable.
        assert!(VersionStatus::Published.is_readable());
        assert!(VersionStatus::Superseded.is_readable());
        for status in [
            VersionStatus::Draft,
            VersionStatus::Validating,
            VersionStatus::Blocked,
            VersionStatus::Revoked,
        ] {
            assert!(
                !status.is_readable(),
                "{} must not be reachable through search",
                status.as_str()
            );
        }
    }

    #[test]
    fn an_origin_fixes_its_scope_so_industry_cannot_become_a_partner_claim() {
        assert_eq!(ClaimOrigin::PartnerMaterial.scope(), ClaimScope::Partner);
        assert_eq!(ClaimOrigin::IndustryResearch.scope(), ClaimScope::Industry);
    }

    #[test]
    fn only_answered_obliges_the_response_to_carry_citations() {
        assert!(AnswerState::Answered.requires_citations());
        for state in [
            AnswerState::EvidenceOnly,
            AnswerState::InsufficientEvidence,
            AnswerState::NoPublishedVersion,
        ] {
            assert!(!state.requires_citations());
        }
    }

    #[test]
    fn the_validation_job_is_keyed_by_the_partner_so_two_presses_are_one_row() {
        let partner = Uuid::from_u128(7);
        assert_eq!(
            validation_idempotency_key(partner),
            validation_idempotency_key(partner)
        );
        assert_ne!(
            validation_idempotency_key(partner),
            validation_idempotency_key(Uuid::from_u128(8))
        );
    }

    #[test]
    fn a_partner_with_no_candidates_has_nothing_to_check() {
        assert!(CandidateSummary::default().is_empty());
        assert!(!CandidateSummary {
            facts: 1,
            ..CandidateSummary::default()
        }
        .is_empty());
        // Gaps alone are not something to verify: a gap is the absence of a statement.
        assert!(CandidateSummary {
            gaps_open: 3,
            ..CandidateSummary::default()
        }
        .is_empty());
    }
}
