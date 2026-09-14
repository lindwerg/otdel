//! Domain model of phase 1D — bounded industry research, its money and its sources.
//!
//! The shape of this phase follows from three sentences in the specification.
//!
//! *«Вход — конкретные вопросы продуктолога»* (`docs/block-01-spec.md` §6.5). A
//! [`ResearchPlan`] is created from **one** question that phase 1C prepared with
//! `audience = industry`, and only when the owner approves it. There is no "research
//! everything" entry point, and no plan exists without a question behind it.
//!
//! *«Поисковый результат — средство обнаружения источника»*. A search hit becomes a
//! [`ResearchSource`] with status `discovered`; a snippet is never evidence. Only a page
//! that was really downloaded from an allowed host carries text a finding may quote, and
//! [`ExternalEvidence`] points at the exact characters of that stored snapshot.
//!
//! *«Отраслевой контекст отделяется от возможностей партнёра»*. A
//! [`ResearchFinding`] is `scope = industry` — the enumeration has no other value, and
//! neither does the database column. A statement about the industry cannot become a
//! characteristic of the partner's product by being written to the wrong table, because
//! there is no path from this type to [`crate::knowledge::KnowledgeFact`].
//!
//! Everything here is a **candidate**, exactly as in 1C: verification and publication
//! are 1E.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Lifecycle of one research plan.
///
/// The vocabulary is the one `docs/block-01-spec.md` §7 fixes for processing runs, plus
/// the phase-1C style `needs_provider`: a plan that stopped because nothing is
/// configured did not *fail*, and calling it a failure would send the owner looking for
/// a problem in the question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchPlanStatus {
    Queued,
    Running,
    /// Every query ran, every allowed source was read, and nothing was refused.
    Completed,
    /// Some part of the plan did not happen: a limit was reached, a host was outside the
    /// allowlist, a candidate finding was refused. The counters and `rejections` say
    /// which.
    Partial,
    Failed,
    /// No search endpoint, no key, no allowlist, or no model. **Nothing was called and
    /// nothing was spent** — a configuration state, not a failure.
    NeedsProvider,
    /// The bureau's or the plan's money ran out. Research stops; it does not continue
    /// "just a little further" (`docs/block-01-spec.md` §10).
    BudgetExhausted,
    /// The owner pressed stop.
    Cancelled,
}

impl ResearchPlanStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::NeedsProvider => "needs_provider",
            Self::BudgetExhausted => "budget_exhausted",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "partial" => Some(Self::Partial),
            "failed" => Some(Self::Failed),
            "needs_provider" => Some(Self::NeedsProvider),
            "budget_exhausted" => Some(Self::BudgetExhausted),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    /// Is the plan still expected to move on its own?
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }

    /// Whether running the plan again could plausibly change anything.
    ///
    /// A plan that exhausted the budget is included: raising the budget is exactly the
    /// thing the owner can do about it.
    pub const fn can_repeat(self) -> bool {
        !self.is_active()
    }
}

/// What happened to one outbound search request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryOutcome {
    Ok,
    /// The provider answered with an error, or the answer was unusable.
    Failed,
    /// The request left this machine and the outcome is not known (a timeout after the
    /// bytes were sent). It is charged as spent **and** flagged for reconciliation:
    /// `docs/block-01-spec.md` §10 refuses to let an unknown outcome quietly become "no
    /// cost".
    Unknown,
    /// The request was never made — the query named the partner, or a limit was already
    /// reached. Nothing was spent.
    Refused,
}

impl QueryOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
            Self::Refused => "refused",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ok" => Some(Self::Ok),
            "failed" => Some(Self::Failed),
            "unknown" => Some(Self::Unknown),
            "refused" => Some(Self::Refused),
            _ => None,
        }
    }
}

/// What became of one discovered URL.
///
/// Every value except `Fetched` means *no content was obtained*, and each names a
/// different reason, because "источник не прочитан" without a reason is what makes a
/// research journal useless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceStatus {
    /// Returned by the search provider; nothing has been downloaded.
    Discovered,
    /// The host is not in `OTDEL_RESEARCH_ALLOWED_HOSTS`. This is the common case and it
    /// is not an error: the owner decides which publishers are acceptable.
    SkippedHost,
    /// `robots.txt` of that host disallows this path for our user agent.
    SkippedRobots,
    /// The plan's page limit, time budget or money ran out before this URL's turn.
    SkippedLimit,
    /// The response was not a kind of document this phase reads (a PDF, an image, an
    /// archive). Recorded rather than parsed with a guess.
    SkippedType,
    /// Downloaded; `content_hash`, `content_chars` and the stored text are the snapshot.
    Fetched,
    /// The download itself failed (refused connection, redirect, oversized body, …).
    Failed,
}

impl SourceStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Discovered => "discovered",
            Self::SkippedHost => "skipped_host",
            Self::SkippedRobots => "skipped_robots",
            Self::SkippedLimit => "skipped_limit",
            Self::SkippedType => "skipped_type",
            Self::Fetched => "fetched",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "discovered" => Some(Self::Discovered),
            "skipped_host" => Some(Self::SkippedHost),
            "skipped_robots" => Some(Self::SkippedRobots),
            "skipped_limit" => Some(Self::SkippedLimit),
            "skipped_type" => Some(Self::SkippedType),
            "fetched" => Some(Self::Fetched),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }

    /// Only a fetched source has text, and only text can be quoted.
    pub const fn is_quotable(self) -> bool {
        matches!(self, Self::Fetched)
    }
}

/// What a finding is about.
///
/// One value, on purpose. A researcher that could write `scope = partner` would be one
/// prompt away from copying a competitor's specification into BASIS's product card,
/// which `docs/block-01-plan.md` (1D §4) forbids in so many words. The database column
/// carries the same single-valued check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingScope {
    Industry,
}

impl FindingScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Industry => "industry",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "industry" => Some(Self::Industry),
            _ => None,
        }
    }
}

/// Status of a candidate finding. As in 1C, exactly one value exists in this phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus {
    Candidate,
}

impl FindingStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "candidate" => Some(Self::Candidate),
            _ => None,
        }
    }
}

/// Stage of one chargeable operation in the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpendState {
    /// Money is held against the budget; the call has not returned yet.
    Reserved,
    /// The call returned and the amount moved from reserved to spent.
    Settled,
    /// The call was never made; the reservation was given back.
    Released,
    /// The call left the machine and its outcome is unknown. Counted as spent, and kept
    /// separately so a person can reconcile it against the provider's own record.
    Unknown,
}

impl SpendState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Settled => "settled",
            Self::Released => "released",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "reserved" => Some(Self::Reserved),
            "settled" => Some(Self::Settled),
            "released" => Some(Self::Released),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

/// Which kind of call a ledger entry paid for.
///
/// Interpreting the sources is one of them. A model call is a paid call like any other,
/// and leaving it out of the ledger would make "израсходовано" a number that omits the
/// most expensive part of a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpendKind {
    Search,
    Fetch,
    Model,
}

impl SpendKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Fetch => "fetch",
            Self::Model => "model",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "search" => Some(Self::Search),
            "fetch" => Some(Self::Fetch),
            "model" => Some(Self::Model),
            _ => None,
        }
    }
}

// --- stored entities --------------------------------------------------------------

/// The bureau's research money, in millionths of one currency unit.
///
/// `reserved` is money held for calls that are in flight right now, across every worker;
/// `spent` is money that has been accounted for; `unknown` is the part of `spent` whose
/// outcome nobody could confirm. What a new call may use is `limit - spent - reserved`,
/// and that subtraction happens inside a single UPDATE so two concurrent plans cannot
/// both pass the same ceiling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchBudget {
    /// Display label only. No amount is ever converted between currencies.
    pub currency: String,
    pub limit_micros: i64,
    pub reserved_micros: i64,
    pub spent_micros: i64,
    /// Part of `spent_micros` recorded with an unknown outcome, awaiting reconciliation.
    pub unknown_micros: i64,
    /// `limit - spent - reserved`, never negative.
    pub available_micros: i64,
    /// Ceiling applied to a single plan.
    pub plan_budget_micros: i64,
    /// The declared tariff. Not an invoice from a provider.
    pub cost_per_search_micros: i64,
    pub cost_per_fetch_micros: i64,
    /// Per model request made while interpreting the sources of a plan.
    pub cost_per_model_call_micros: i64,
    pub updated_at: DateTime<Utc>,
}

impl ResearchBudget {
    /// Money a new call may use.
    pub fn available(limit: i64, spent: i64, reserved: i64) -> i64 {
        limit.saturating_sub(spent).saturating_sub(reserved).max(0)
    }
}

/// One approved question, turned into bounded research.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchPlan {
    pub id: Uuid,
    pub partner_id: Uuid,
    /// The material whose gap raised the question. Kept so the plan stays traceable to
    /// the document that caused it.
    pub material_id: Uuid,
    pub material_filename: String,
    /// The 1C question this plan was approved from.
    ///
    /// `null` after phase 1C re-drafted that material: re-running an understanding
    /// replaces its questions, and the link is dropped rather than the research. What
    /// was actually researched is `question_text`, copied verbatim at approval time.
    pub question_id: Option<Uuid>,
    /// The question as it stood when the owner approved it. This — not the current row
    /// in 1C — is what the plan researched.
    pub question_text: String,
    /// The gap's topic label, copied at approval time for the same reason.
    pub topic: Option<String>,
    pub status: ResearchPlanStatus,
    /// Which search adapter and model produced this plan's sources and findings.
    pub provider: Option<String>,
    pub model: Option<String>,
    pub prompt_profile: String,
    /// Passes already run, and the ceiling from the configuration.
    pub passes: i32,
    pub max_passes: i32,
    pub budget_micros: i64,
    pub reserved_micros: i64,
    pub spent_micros: i64,
    pub queries_made: i32,
    pub results_seen: i32,
    pub sources_fetched: i32,
    pub sources_skipped: i32,
    pub bytes_fetched: i64,
    pub findings_accepted: i32,
    /// Candidate findings refused by validation: source outside this plan, quote not
    /// found in the stored snapshot, value absent from the quotation.
    pub findings_rejected: i32,
    pub duration_ms: Option<i64>,
    /// Reasons, in words, deduplicated and bounded. The interface shows them as-is.
    pub rejections: Vec<String>,
    pub diagnostic: Option<String>,
    /// The owner asked the run to stop; the worker settles at the next checkpoint.
    pub cancel_requested: bool,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One search request that was made — or refused before it was made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchQueryRecord {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub ordinal: i32,
    /// Exactly the text that was sent. Shown to the owner, so "what did it ask?" has an
    /// answer that is not a reconstruction.
    pub query_text: String,
    pub provider: String,
    pub results_count: i32,
    pub cost_micros: i64,
    pub outcome: QueryOutcome,
    pub diagnostic: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One external URL: what it is, whether it was read, and what came back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchSource {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub query_id: Option<Uuid>,
    /// Normalised absolute URL. Always `https`, never with credentials or a fragment.
    pub url: String,
    pub host: String,
    pub title: Option<String>,
    /// The search provider's snippet. **Discovery, not evidence**: no finding may quote
    /// it, and the interface labels it as the search engine's text.
    pub snippet: Option<String>,
    pub status: SourceStatus,
    pub http_status: Option<i32>,
    pub content_type: Option<String>,
    pub content_bytes: Option<i64>,
    pub content_chars: Option<i32>,
    /// SHA-256 of the bytes as received. Two runs that produce the same hash read the
    /// same document; a changed hash is a changed source.
    pub content_hash: Option<String>,
    /// Filled only when the page states a licence in a way the fetcher recognises.
    /// `null` means "not stated", never "free to use".
    pub license: Option<String>,
    /// Where that licence was read from, or why none is recorded.
    pub license_note: Option<String>,
    pub retrieved_at: Option<DateTime<Utc>>,
    /// Only when the source itself states a date. Never inferred.
    pub published_at: Option<DateTime<Utc>>,
    pub cost_micros: i64,
    pub diagnostic: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A verbatim fragment of a fetched page, with the offsets that locate it in the stored
/// snapshot.
///
/// As in 1C, `quote` is the *source's* wording: the server stores the substring the
/// model's quote matched, not the model's rendering of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalEvidence {
    pub id: Uuid,
    pub source_id: Uuid,
    pub url: String,
    pub host: String,
    pub retrieved_at: Option<DateTime<Utc>>,
    pub content_hash: Option<String>,
    pub license: Option<String>,
    pub quote: String,
    pub char_start: i32,
    pub char_end: i32,
}

/// A candidate statement about the **industry**, with the external sources behind it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchFinding {
    pub id: Uuid,
    pub partner_id: Uuid,
    pub plan_id: Uuid,
    /// Always `industry`. Never a claim about this partner's products.
    pub scope: FindingScope,
    /// Always `candidate` in this phase.
    pub status: FindingStatus,
    pub topic: String,
    /// The property being stated ("минимальная толщина цинкового покрытия").
    pub attribute: String,
    /// The value exactly as the source writes it.
    pub value_text: String,
    pub unit: Option<String>,
    pub conditions: Option<String>,
    /// The model's own words. **Not a quote** — the interface says so, and no evidence
    /// points at it.
    pub model_context: Option<String>,
    /// Never empty: a finding without an external source is not stored.
    pub evidence: Vec<ExternalEvidence>,
    pub created_at: DateTime<Utc>,
}

/// A 1C question addressed to industry research, with the plan approved from it (if any).
///
/// This is the approval queue: a question with `plan = None` is one the owner has not
/// turned into research yet, and nothing happens to it until they do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndustryQuestion {
    pub id: Uuid,
    pub partner_id: Uuid,
    pub material_id: Uuid,
    pub material_filename: String,
    pub gap_id: Uuid,
    pub gap_topic: String,
    pub gap_missing: String,
    pub text: String,
    /// `prepared` in 1C; this phase does not rewrite it.
    pub status: String,
    /// The plan approved from this question, when there is one.
    pub plan_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// Partner-level roll-up for the research tab.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchSummary {
    pub plans_total: i32,
    pub plans_active: i32,
    pub questions_open: i32,
    pub sources_fetched: i32,
    pub sources_skipped: i32,
    pub findings_total: i32,
    pub spent_micros: i64,
}

/// Stable idempotency key for the job that runs one plan.
///
/// Approving the same question twice, or pressing "исследовать ещё раз", reuses one
/// queue row instead of researching the same question in parallel with itself.
pub fn research_plan_idempotency_key(plan_id: Uuid) -> String {
    format!("research_plan:{plan_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerations_round_trip_through_their_wire_form() {
        for status in [
            ResearchPlanStatus::Queued,
            ResearchPlanStatus::Running,
            ResearchPlanStatus::Completed,
            ResearchPlanStatus::Partial,
            ResearchPlanStatus::Failed,
            ResearchPlanStatus::NeedsProvider,
            ResearchPlanStatus::BudgetExhausted,
            ResearchPlanStatus::Cancelled,
        ] {
            assert_eq!(ResearchPlanStatus::parse(status.as_str()), Some(status));
            assert_eq!(
                serde_json::to_value(status).unwrap(),
                serde_json::Value::String(status.as_str().to_owned())
            );
        }
        assert_eq!(ResearchPlanStatus::parse("published"), None);

        for status in [
            SourceStatus::Discovered,
            SourceStatus::SkippedHost,
            SourceStatus::SkippedRobots,
            SourceStatus::SkippedLimit,
            SourceStatus::SkippedType,
            SourceStatus::Fetched,
            SourceStatus::Failed,
        ] {
            assert_eq!(SourceStatus::parse(status.as_str()), Some(status));
        }
        for outcome in [
            QueryOutcome::Ok,
            QueryOutcome::Failed,
            QueryOutcome::Unknown,
            QueryOutcome::Refused,
        ] {
            assert_eq!(QueryOutcome::parse(outcome.as_str()), Some(outcome));
        }
        for state in [
            SpendState::Reserved,
            SpendState::Settled,
            SpendState::Released,
            SpendState::Unknown,
        ] {
            assert_eq!(SpendState::parse(state.as_str()), Some(state));
        }
    }

    #[test]
    fn a_finding_can_only_ever_be_about_the_industry() {
        assert_eq!(FindingScope::Industry.as_str(), "industry");
        // The one thing this phase must never be able to say.
        assert_eq!(FindingScope::parse("partner"), None);
        assert_eq!(FindingScope::parse("product"), None);
        // And it is never verified here: the checker is 1E.
        assert_eq!(FindingStatus::parse("source_supported"), None);
        assert_eq!(FindingStatus::parse("published"), None);
    }

    #[test]
    fn only_a_fetched_source_can_be_quoted() {
        assert!(SourceStatus::Fetched.is_quotable());
        for status in [
            SourceStatus::Discovered,
            SourceStatus::SkippedHost,
            SourceStatus::SkippedRobots,
            SourceStatus::SkippedLimit,
            SourceStatus::SkippedType,
            SourceStatus::Failed,
        ] {
            assert!(
                !status.is_quotable(),
                "{status:?} has no stored text and must not be quotable"
            );
        }
    }

    #[test]
    fn a_plan_that_ran_out_of_money_can_be_repeated_once_there_is_more() {
        assert!(ResearchPlanStatus::BudgetExhausted.can_repeat());
        assert!(ResearchPlanStatus::NeedsProvider.can_repeat());
        assert!(ResearchPlanStatus::Cancelled.can_repeat());
        assert!(!ResearchPlanStatus::Queued.can_repeat());
        assert!(!ResearchPlanStatus::Running.can_repeat());
        assert!(ResearchPlanStatus::Queued.is_active());
        assert!(!ResearchPlanStatus::Completed.is_active());
    }

    #[test]
    fn available_money_never_goes_below_zero() {
        assert_eq!(ResearchBudget::available(1_000, 200, 300), 500);
        assert_eq!(ResearchBudget::available(1_000, 1_000, 0), 0);
        // Over-spend (an unknown outcome settled after the limit was lowered) must read
        // as "nothing left", not as a negative allowance a caller could add to.
        assert_eq!(ResearchBudget::available(1_000, 900, 500), 0);
        assert_eq!(ResearchBudget::available(0, 0, 0), 0);
    }

    #[test]
    fn the_plan_key_is_stable_per_plan() {
        let plan = Uuid::from_u128(21);
        assert_eq!(
            research_plan_idempotency_key(plan),
            research_plan_idempotency_key(plan)
        );
        assert_ne!(
            research_plan_idempotency_key(plan),
            research_plan_idempotency_key(Uuid::from_u128(22))
        );
    }

    #[test]
    fn a_finding_serialises_with_its_external_evidence_and_separated_model_context() {
        let now = DateTime::<Utc>::from_timestamp(0, 0).unwrap();
        let finding = ResearchFinding {
            id: Uuid::from_u128(1),
            partner_id: Uuid::from_u128(2),
            plan_id: Uuid::from_u128(3),
            scope: FindingScope::Industry,
            status: FindingStatus::Candidate,
            topic: "покрытие".to_owned(),
            attribute: "минимальная толщина цинкового покрытия".to_owned(),
            value_text: "55".to_owned(),
            unit: Some("мкм".to_owned()),
            conditions: Some("для изделий толщиной до 1,5 мм".to_owned()),
            model_context: Some("значение относится к отраслевому стандарту".to_owned()),
            evidence: vec![ExternalEvidence {
                id: Uuid::from_u128(4),
                source_id: Uuid::from_u128(5),
                url: "https://docs.example.org/gost".to_owned(),
                host: "docs.example.org".to_owned(),
                retrieved_at: Some(now),
                content_hash: Some("a".repeat(64)),
                license: None,
                quote: "минимальная толщина покрытия 55 мкм".to_owned(),
                char_start: 10,
                char_end: 45,
            }],
            created_at: now,
        };

        let value = serde_json::to_value(&finding).unwrap();
        assert_eq!(value["scope"], "industry");
        assert_eq!(value["status"], "candidate");
        assert_eq!(value["evidence"][0]["url"], "https://docs.example.org/gost");
        // The external source is named exactly, with the date it was read.
        assert!(!value["evidence"][0]["retrieved_at"].is_null());
        // The model's wording is a separate field, never merged into the quotation.
        assert_ne!(value["model_context"], value["evidence"][0]["quote"]);
        // A licence that was not stated stays null rather than becoming "free".
        assert!(value["evidence"][0]["license"].is_null());
    }
}
