//! R05 — the vocabulary of a product base: coverage, passports, senses, applications and
//! the things a document leaves unclear.
//!
//! This module holds *types only*. The rules that decide a coverage state or a set of
//! unmet requirements live in `otdel-knowledge`, where they can be unit-tested against
//! fixtures without a database; the storage layer in `otdel-db` reads and writes these
//! shapes; the interface renders them. Keeping the vocabulary here is what stops the
//! three from inventing three slightly different answers to "was this page read".
//!
//! Two conventions run through everything below, and both are the direct answer to the
//! audited run that reported 44 products, 13 facts and 0 of everything else as success:
//!
//! * **Absence has a reason attached or it is not absence.** [`PageDisposition`] never
//!   has a value meaning "gone"; [`CoverageState`] has no value meaning "good enough";
//!   an empty list of terms is only acceptable beside a [`KnowledgeDeclaration`].
//! * **Unclear is a first-class answer.** [`AliasRelation::Unclear`],
//!   [`IdentityState::Unclear`] and [`KnowledgeUncertainty`] exist so the pipeline can
//!   record "this looks related and the document does not settle it" without either
//!   inventing a merge or throwing the observation away.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Which kind of unknown a [`crate::knowledge::KnowledgeGap`] records.
///
/// The requirement check asks about the commercial unknowns and the technical ones
/// separately, so the run classifies its own gaps. Deriving this from the gap's free-text
/// topic would put a word list in the publication path, where a vocabulary miss becomes a
/// silent clearance. `Other` exists so nothing has to be forced into a box.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapNature {
    /// Price, lead time, minimum order, packaging, delivery.
    Commercial,
    /// Loads, materials, dimensions, tolerances, compatibility.
    Technical,
    /// Neither, and saying so beats guessing.
    #[default]
    Other,
}

impl GapNature {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Commercial => "commercial",
            Self::Technical => "technical",
            Self::Other => "other",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "commercial" => Some(Self::Commercial),
            "technical" => Some(Self::Technical),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

// --- where a number came from, structurally -----------------------------------------

/// Which structure a fact's value sat in.
///
/// Not a confidence and not a quality grade: provenance of a second kind, beside the
/// quotation that was already required. `PageText` is the honest answer when the
/// structure was never established, and it is the default — a fact does not become
/// table-derived by nobody having checked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuralSource {
    /// Read out of running text, or out of a table whose structure was not established.
    #[default]
    PageText,
    /// Read out of a table cell R03 marked usable, and the cell agrees with the value.
    TableCell,
}

impl StructuralSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PageText => "page_text",
            Self::TableCell => "table_cell",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "page_text" => Some(Self::PageText),
            "table_cell" => Some(Self::TableCell),
            _ => None,
        }
    }
}

/// What the table said a fact's value was about, copied at the moment the fact was
/// accepted.
///
/// Copied rather than joined on purpose: re-reading a page replaces its cells, and a fact
/// must not become unreadable — or silently change meaning — because the table it came
/// from was parsed again. `cell_id` is the live pointer and may go to `None`; the words
/// stay.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactOrigin {
    pub source: StructuralSource,
    pub cell_id: Option<Uuid>,
    /// The product the table row was about, in the table's words.
    pub subject: Option<String>,
    /// The property the column stated.
    pub property: Option<String>,
    pub unit: Option<String>,
    pub conditions: Vec<String>,
}

impl FactOrigin {
    /// Whether this fact names a table cell as its origin.
    pub fn is_from_a_table(&self) -> bool {
        self.source == StructuralSource::TableCell
    }
}

// --- coverage ---------------------------------------------------------------------

/// What became of one page of a material during one understanding run.
///
/// Every value except [`Self::Processed`] is a reason the page is not represented in the
/// draft, and no two of them mean the same thing. The audited run could not tell
/// "awaiting recognition" from "the request budget ran out" from "nobody looked",
/// because all three looked identical from the outside — absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageDisposition {
    /// Sent to the model in a request whose answer came back.
    Processed,
    /// The request or cost cap was reached. The page is queued for the next pass — this
    /// is the only disposition that is expected to change on its own.
    DeferredBudget,
    /// Awaiting recognition: there is no text to quote.
    UnreadableNeedsOcr,
    /// Reading the page failed.
    UnreadableFailed,
    /// The page really is blank.
    UnreadableEmpty,
    /// Inventoried by the reading phase and not reached yet. Not a failure and not a
    /// silence: the reader simply has not got there, and a later pass will.
    NotReadYet,
    /// Read, but the stored text is whitespace — nothing to quote from.
    NotOfferedNoText,
    /// The caller restricted this pass to other pages (a resume pass over the deferred
    /// remainder).
    ExcludedByRequest,
}

impl PageDisposition {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Processed => "processed",
            Self::DeferredBudget => "deferred_budget",
            Self::UnreadableNeedsOcr => "unreadable_needs_ocr",
            Self::UnreadableFailed => "unreadable_failed",
            Self::UnreadableEmpty => "unreadable_empty",
            Self::NotReadYet => "not_read_yet",
            Self::NotOfferedNoText => "not_offered_no_text",
            Self::ExcludedByRequest => "excluded_by_request",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "processed" => Some(Self::Processed),
            "deferred_budget" => Some(Self::DeferredBudget),
            "unreadable_needs_ocr" => Some(Self::UnreadableNeedsOcr),
            "unreadable_failed" => Some(Self::UnreadableFailed),
            "unreadable_empty" => Some(Self::UnreadableEmpty),
            "not_read_yet" => Some(Self::NotReadYet),
            "not_offered_no_text" => Some(Self::NotOfferedNoText),
            "excluded_by_request" => Some(Self::ExcludedByRequest),
            _ => None,
        }
    }

    /// Whether a later pass could still turn this page into knowledge without anything
    /// else changing. Only a budget deferral qualifies: a page awaiting OCR needs the
    /// reader to run again first.
    pub const fn is_resumable(self) -> bool {
        matches!(self, Self::DeferredBudget | Self::ExcludedByRequest)
    }

    /// Whether nobody could read this page at all.
    pub const fn is_unreadable(self) -> bool {
        matches!(
            self,
            Self::UnreadableNeedsOcr
                | Self::UnreadableFailed
                | Self::UnreadableEmpty
                | Self::NotReadYet
                | Self::NotOfferedNoText
        )
    }

    /// One sentence for the interface and for the run record, in Russian.
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Processed => "страница разобрана",
            Self::DeferredBudget => {
                "страница отложена: исчерпан бюджет запросов, она встанет в очередь на следующий проход"
            }
            Self::UnreadableNeedsOcr => "страница ждёт распознавания: текстового слоя нет",
            Self::UnreadableFailed => "страницу не удалось прочитать",
            Self::UnreadableEmpty => "страница пустая",
            Self::NotReadYet => "страница ещё не прочитана: чтение документа не дошло до неё",
            Self::NotOfferedNoText => "страница прочитана, но текста на ней не оказалось",
            Self::ExcludedByRequest => "страница не входила в этот проход",
        }
    }
}

/// The verdict over a run's page account.
///
/// There is deliberately no value meaning "good enough". A caller that wants to know
/// whether it may proceed asks [`Self::allows_automatic_publication`] and gets an answer
/// it can explain to the owner.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageState {
    /// Not computed: a run that predates this package, or one that failed before it got
    /// as far as planning. Never treated as either good or bad news.
    #[default]
    Unknown,
    /// Every page of the material was processed.
    Complete,
    /// Some pages were not processed, each with a stated reason, and none of them is
    /// waiting on budget. The material is as covered as it can be until it is read again.
    PartialAccounted,
    /// Pages remain deferred for budget, or the account does not add up.
    Incomplete,
}

impl CoverageState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Complete => "complete",
            Self::PartialAccounted => "partial_accounted",
            Self::Incomplete => "incomplete",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "unknown" => Some(Self::Unknown),
            "complete" => Some(Self::Complete),
            "partial_accounted" => Some(Self::PartialAccounted),
            "incomplete" => Some(Self::Incomplete),
            _ => None,
        }
    }

    /// Whether the coverage half of the automatic-publication gate is satisfied.
    ///
    /// `Unknown` is refused on purpose: a run nobody judged is not a run that passed.
    pub const fn allows_automatic_publication(self) -> bool {
        matches!(self, Self::Complete | Self::PartialAccounted)
    }
}

/// Whether the run produced what a passport needs, and — when it did not — what is
/// missing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementsState {
    /// Not judged yet.
    #[default]
    Unknown,
    Met,
    Unmet,
}

impl RequirementsState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Met => "met",
            Self::Unmet => "unmet",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "unknown" => Some(Self::Unknown),
            "met" => Some(Self::Met),
            "unmet" => Some(Self::Unmet),
            _ => None,
        }
    }

    pub const fn is_met(self) -> bool {
        matches!(self, Self::Met)
    }
}

/// The page account and the cost of one run, as stored on the run row.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCoverage {
    /// Pages the material has at all — the denominator the old record never had.
    pub pages_total: i32,
    /// Pages carrying quotable text, and therefore offerable.
    pub pages_offered: i32,
    /// Pages actually sent in a request whose answer came back.
    pub pages_processed: i32,
    /// Pages queued for a later pass because a budget was reached.
    pub pages_deferred: i32,
    /// Pages nobody could read.
    pub pages_unreadable: i32,
    pub state: CoverageState,
    /// Why the state is not `complete`, in words, shown as-is.
    pub notes: Vec<String>,
    pub requirements: RequirementsState,
    /// Named requirements the run did not satisfy.
    pub requirements_missing: Vec<String>,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    /// What the provider said the pass cost, in micro-dollars. `None` means the provider
    /// did not report a cost — never "free".
    pub cost_micro_usd: Option<i64>,
}

impl RunCoverage {
    /// Whether both halves of the automatic-publication gate are satisfied.
    ///
    /// Both, not either: a fully covered material that produced no passport content is
    /// as unready as a half-read one that produced plenty.
    pub fn allows_automatic_publication(&self) -> bool {
        self.state.allows_automatic_publication() && self.requirements.is_met()
    }

    /// Pages that are neither processed nor explained. Should always be zero; a non-zero
    /// value means the account itself is broken, which is worth surfacing rather than
    /// hiding behind a state.
    pub fn unaccounted(&self) -> i32 {
        (self.pages_total - self.pages_processed - self.pages_deferred - self.pages_unreadable)
            .max(0)
    }
}

/// One page's line in the coverage report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageCoverage {
    pub id: Uuid,
    pub material_id: Uuid,
    pub run_id: Uuid,
    pub page_id: Uuid,
    pub page_number: i32,
    pub disposition: PageDisposition,
    pub offered: bool,
    pub chars_sent: i32,
    /// Which request of the run carried it, 1-based.
    pub batch_index: Option<i32>,
    /// Required whenever the page was not processed.
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

// --- declarations -----------------------------------------------------------------

/// A topic on which a run may have to say "there is none" out loud.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclarationTopic {
    Glossary,
    Questions,
    Applications,
    CommercialUnknowns,
    TechnicalUnknowns,
}

impl DeclarationTopic {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Glossary => "glossary",
            Self::Questions => "questions",
            Self::Applications => "applications",
            Self::CommercialUnknowns => "commercial_unknowns",
            Self::TechnicalUnknowns => "technical_unknowns",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "glossary" => Some(Self::Glossary),
            "questions" => Some(Self::Questions),
            "applications" => Some(Self::Applications),
            "commercial_unknowns" => Some(Self::CommercialUnknowns),
            "technical_unknowns" => Some(Self::TechnicalUnknowns),
            _ => None,
        }
    }

    /// Every topic the requirement check consults, in the order it reports them.
    pub const ALL: [Self; 5] = [
        Self::Glossary,
        Self::Questions,
        Self::Applications,
        Self::CommercialUnknowns,
        Self::TechnicalUnknowns,
    ];
}

/// Who stated the absence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclarationOrigin {
    /// The model said so in its answer, and the words are its own.
    Model,
    /// The server said so because the material settled it (a page with no table cannot
    /// leave an ambiguous cell behind).
    Server,
}

impl DeclarationOrigin {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Server => "server",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "model" => Some(Self::Model),
            "server" => Some(Self::Server),
            _ => None,
        }
    }
}

/// An explicit "there is none", on the record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeDeclaration {
    pub id: Uuid,
    pub material_id: Uuid,
    pub run_id: Uuid,
    pub topic: DeclarationTopic,
    /// The reason, in the run's own words.
    pub stated: String,
    pub origin: DeclarationOrigin,
    pub created_at: DateTime<Utc>,
}

// --- aliases, senses, identity ------------------------------------------------------

/// How far a recorded surface form claims to go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AliasRelation {
    /// The same thing written differently, and one page shows both forms.
    Alias,
    /// A narrower reading used in one context of this material.
    Sense,
    /// It looks related and the document does not settle it. Never a merge.
    Unclear,
}

impl AliasRelation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Alias => "alias",
            Self::Sense => "sense",
            Self::Unclear => "unclear",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "alias" => Some(Self::Alias),
            "sense" => Some(Self::Sense),
            "unclear" => Some(Self::Unclear),
            _ => None,
        }
    }

    /// Whether this relation may be used to answer as if the two names were the same
    /// product. `Unclear` never may — that is what it is for.
    pub const fn is_safe_to_follow(self) -> bool {
        matches!(self, Self::Alias)
    }
}

/// A recorded surface form of a product, with the fragment that shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductAlias {
    pub id: Uuid,
    pub product_id: Uuid,
    pub material_id: Uuid,
    pub run_id: Uuid,
    pub surface: String,
    pub relation: AliasRelation,
    pub note: Option<String>,
    pub page_id: Uuid,
    pub page_number: i32,
    pub quote: String,
    pub char_start: i32,
    pub char_end: i32,
    pub created_at: DateTime<Utc>,
}

/// How far an identity proposal between two product rows goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityState {
    /// The same product, and a page on each side shows the designation.
    Linked,
    /// They may be the same and nothing proves it. The rows stay separate.
    Unclear,
}

impl IdentityState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Linked => "linked",
            Self::Unclear => "unclear",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "linked" => Some(Self::Linked),
            "unclear" => Some(Self::Unclear),
            _ => None,
        }
    }
}

/// What an identity proposal rests on. Named, never scored — nothing here measures a
/// confidence, and a fabricated 0.87 would be worse than the name of the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityBasis {
    /// The identical designation is quoted on a page of each material.
    IdenticalDesignationQuoted,
    /// An alias recorded on one side is quoted on the other.
    AliasQuotedInBoth,
    /// The names merely resemble each other. Never enough for a link.
    NameSimilarityOnly,
}

impl IdentityBasis {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IdenticalDesignationQuoted => "identical_designation_quoted",
            Self::AliasQuotedInBoth => "alias_quoted_in_both",
            Self::NameSimilarityOnly => "name_similarity_only",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "identical_designation_quoted" => Some(Self::IdenticalDesignationQuoted),
            "alias_quoted_in_both" => Some(Self::AliasQuotedInBoth),
            "name_similarity_only" => Some(Self::NameSimilarityOnly),
            _ => None,
        }
    }

    /// Whether this basis can support [`IdentityState::Linked`]. The database enforces
    /// the same rule; this is the half a caller can consult before writing.
    pub const fn can_support_a_link(self) -> bool {
        !matches!(self, Self::NameSimilarityOnly)
    }
}

/// A proposal that two product rows of different materials describe one product.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductIdentityLink {
    pub id: Uuid,
    pub product_id: Uuid,
    pub other_product_id: Uuid,
    pub state: IdentityState,
    pub basis: IdentityBasis,
    pub note: Option<String>,
    pub material_id: Option<Uuid>,
    pub page_id: Option<Uuid>,
    pub page_number: Option<i32>,
    pub other_material_id: Option<Uuid>,
    pub other_page_id: Option<Uuid>,
    pub other_page_number: Option<i32>,
    pub created_at: DateTime<Utc>,
}

/// One reading of a glossary term, with the fragment it was read from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlossarySense {
    pub id: Uuid,
    pub term_id: Uuid,
    pub material_id: Uuid,
    pub run_id: Uuid,
    /// Short disambiguator: «в контексте кабельных лотков». Not a number.
    pub label: String,
    pub definition: String,
    pub definition_is_model_context: bool,
    pub page_id: Uuid,
    pub page_number: i32,
    pub quote: String,
    pub char_start: i32,
    pub char_end: i32,
    pub created_at: DateTime<Utc>,
}

/// How far a recorded surface form of a term claims to go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SynonymRelation {
    Synonym,
    Abbreviation,
    Unclear,
}

impl SynonymRelation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Synonym => "synonym",
            Self::Abbreviation => "abbreviation",
            Self::Unclear => "unclear",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "synonym" => Some(Self::Synonym),
            "abbreviation" => Some(Self::Abbreviation),
            "unclear" => Some(Self::Unclear),
            _ => None,
        }
    }

    pub const fn is_safe_to_follow(self) -> bool {
        matches!(self, Self::Synonym | Self::Abbreviation)
    }
}

/// A recorded surface form of a term, with the fragment that shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlossarySynonym {
    pub id: Uuid,
    pub term_id: Uuid,
    pub material_id: Uuid,
    pub run_id: Uuid,
    pub surface: String,
    pub relation: SynonymRelation,
    pub page_id: Uuid,
    pub page_number: i32,
    pub quote: String,
    pub char_start: i32,
    pub char_end: i32,
    pub created_at: DateTime<Utc>,
}

// --- the application map ------------------------------------------------------------

/// What a line under an application is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationDetailKind {
    /// Something that has to be known to choose correctly, with the source's value.
    Parameter,
    /// Something that limits the application, with the source's wording.
    Constraint,
    /// Something the material does not settle and somebody has to be asked.
    Question,
}

impl ApplicationDetailKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Parameter => "parameter",
            Self::Constraint => "constraint",
            Self::Question => "question",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "parameter" => Some(Self::Parameter),
            "constraint" => Some(Self::Constraint),
            "question" => Some(Self::Question),
            _ => None,
        }
    }

    /// Whether this kind asserts something about the product, and therefore has to carry
    /// a quotation. A question asserts nothing.
    pub const fn is_a_claim(self) -> bool {
        matches!(self, Self::Parameter | Self::Constraint)
    }
}

/// One parameter, constraint or question under an application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationDetail {
    pub id: Uuid,
    pub application_id: Uuid,
    pub kind: ApplicationDetailKind,
    pub label: String,
    /// The source's value. `None` only for a question.
    pub value_text: Option<String>,
    pub unit: Option<String>,
    /// Who a question is for. `None` for the other kinds.
    pub audience: Option<crate::knowledge::QuestionAudience>,
    pub page_id: Option<Uuid>,
    pub page_number: Option<i32>,
    pub quote: Option<String>,
    pub char_start: Option<i32>,
    pub char_end: Option<i32>,
    pub created_at: DateTime<Utc>,
}

/// A task the material says this product serves, with everything needed to act on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductApplication {
    pub id: Uuid,
    pub partner_id: Uuid,
    pub material_id: Uuid,
    pub run_id: Uuid,
    pub product_id: Option<Uuid>,
    pub product_name: Option<String>,
    /// The task in the buyer's terms: «закрепить кабельный лоток к бетону».
    pub task: String,
    pub summary: Option<String>,
    /// The model's own framing. Not a quote.
    pub model_context: Option<String>,
    pub page_id: Uuid,
    pub page_number: i32,
    pub quote: String,
    pub char_start: i32,
    pub char_end: i32,
    pub details: Vec<ApplicationDetail>,
    pub created_at: DateTime<Utc>,
}

impl ProductApplication {
    pub fn parameters(&self) -> impl Iterator<Item = &ApplicationDetail> {
        self.details
            .iter()
            .filter(|detail| detail.kind == ApplicationDetailKind::Parameter)
    }

    pub fn constraints(&self) -> impl Iterator<Item = &ApplicationDetail> {
        self.details
            .iter()
            .filter(|detail| detail.kind == ApplicationDetailKind::Constraint)
    }

    pub fn questions(&self) -> impl Iterator<Item = &ApplicationDetail> {
        self.details
            .iter()
            .filter(|detail| detail.kind == ApplicationDetailKind::Question)
    }
}

// --- uncertainties -------------------------------------------------------------------

/// Why something the document *does* say may not be read as a value.
///
/// A [`crate::knowledge::KnowledgeGap`] records what the material does **not** say; an
/// uncertainty records what it says in a form nobody may safely read. They are different
/// questions, and neither is ever a fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UncertaintyKind {
    /// A table cell R03 marked `ambiguous` or `unusable`.
    AmbiguousTableCell,
    /// A page nobody could read, so whatever it holds is unknown rather than absent.
    UnreadablePage,
    /// A number whose unit is written nowhere.
    UnresolvedUnit,
    /// A value whose product cannot be determined from the table's structure.
    UnresolvedSubject,
    /// A load diagram the reader recognised and did not interpret.
    UninterpretedDiagram,
}

impl UncertaintyKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AmbiguousTableCell => "ambiguous_table_cell",
            Self::UnreadablePage => "unreadable_page",
            Self::UnresolvedUnit => "unresolved_unit",
            Self::UnresolvedSubject => "unresolved_subject",
            Self::UninterpretedDiagram => "uninterpreted_diagram",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ambiguous_table_cell" => Some(Self::AmbiguousTableCell),
            "unreadable_page" => Some(Self::UnreadablePage),
            "unresolved_unit" => Some(Self::UnresolvedUnit),
            "unresolved_subject" => Some(Self::UnresolvedSubject),
            "uninterpreted_diagram" => Some(Self::UninterpretedDiagram),
            _ => None,
        }
    }
}

/// Something the material states in a form nobody may read as a value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeUncertainty {
    pub id: Uuid,
    pub partner_id: Uuid,
    pub material_id: Uuid,
    pub run_id: Uuid,
    pub product_id: Option<Uuid>,
    pub kind: UncertaintyKind,
    /// What is unclear, in one line.
    pub subject: String,
    /// Why it cannot be read.
    pub detail: String,
    /// The machine-readable reasons, straight from R03's vocabulary where there is one.
    pub reasons: Vec<String>,
    /// The cell's own text when there is one — shown as *not* a claim.
    pub quote: Option<String>,
    pub page_id: Option<Uuid>,
    pub page_number: Option<i32>,
    pub region_id: Option<Uuid>,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

// --- the passport ---------------------------------------------------------------------

/// Everything known about one product, assembled for reading.
///
/// This is a *composition*, not a stored row: every field below comes from a table that
/// owns it, so a passport can never drift from the candidates it describes. What makes it
/// a passport rather than a product listing is what it refuses to leave out — the gaps,
/// the uncertainties and the identity proposals are as much part of the answer as the
/// facts, and a reader who sees only the facts has been told half the truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductPassport {
    pub product: crate::knowledge::Product,
    pub category: Option<crate::knowledge::ProductCategory>,
    /// The material this product row was drafted from, named so the reader can see which
    /// catalogue is speaking.
    pub material_filename: String,
    pub aliases: Vec<ProductAlias>,
    pub facts: Vec<crate::knowledge::KnowledgeFact>,
    pub applications: Vec<ProductApplication>,
    pub gaps: Vec<crate::knowledge::KnowledgeGap>,
    pub uncertainties: Vec<KnowledgeUncertainty>,
    /// Proposals that a product row of another material is the same product. Never a
    /// merge: both rows stay where they are.
    pub identity_links: Vec<ProductIdentityLink>,
}

impl ProductPassport {
    /// Whether this passport says anything a reader could act on. A product row with a
    /// name and nothing else is exactly what the audited run produced 44 of.
    pub fn is_substantive(&self) -> bool {
        self.product.summary.is_some()
            || !self.facts.is_empty()
            || !self.applications.is_empty()
            || !self.gaps.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_disposition_round_trips_and_explains_itself() {
        for disposition in [
            PageDisposition::Processed,
            PageDisposition::DeferredBudget,
            PageDisposition::UnreadableNeedsOcr,
            PageDisposition::UnreadableFailed,
            PageDisposition::UnreadableEmpty,
            PageDisposition::NotReadYet,
            PageDisposition::NotOfferedNoText,
            PageDisposition::ExcludedByRequest,
        ] {
            assert_eq!(
                PageDisposition::parse(disposition.as_str()),
                Some(disposition)
            );
            assert!(
                !disposition.describe().is_empty(),
                "a disposition without a sentence is the silence this package removes"
            );
        }
    }

    #[test]
    fn only_a_budget_deferral_resumes_on_its_own() {
        assert!(PageDisposition::DeferredBudget.is_resumable());
        assert!(PageDisposition::ExcludedByRequest.is_resumable());
        // A page awaiting recognition needs the *reader* to run again first; calling it
        // resumable here would make the understanding queue spin against it forever.
        assert!(!PageDisposition::UnreadableNeedsOcr.is_resumable());
        assert!(PageDisposition::UnreadableNeedsOcr.is_unreadable());
        assert!(!PageDisposition::Processed.is_unreadable());
    }

    #[test]
    fn an_unjudged_run_never_passes_the_publication_gate() {
        assert!(!CoverageState::Unknown.allows_automatic_publication());
        assert!(!CoverageState::Incomplete.allows_automatic_publication());
        assert!(CoverageState::Complete.allows_automatic_publication());
        assert!(CoverageState::PartialAccounted.allows_automatic_publication());
    }

    #[test]
    fn the_gate_needs_both_halves() {
        let mut coverage = RunCoverage {
            state: CoverageState::Complete,
            requirements: RequirementsState::Met,
            ..RunCoverage::default()
        };
        assert!(coverage.allows_automatic_publication());

        // Fully covered, nothing to show for it.
        coverage.requirements = RequirementsState::Unmet;
        assert!(!coverage.allows_automatic_publication());

        // Plenty to show, half the material unread.
        coverage.requirements = RequirementsState::Met;
        coverage.state = CoverageState::Incomplete;
        assert!(!coverage.allows_automatic_publication());
    }

    #[test]
    fn a_page_that_is_in_no_column_is_reported_as_unaccounted() {
        let coverage = RunCoverage {
            pages_total: 44,
            pages_processed: 30,
            pages_deferred: 0,
            pages_unreadable: 6,
            ..RunCoverage::default()
        };
        // Eight pages: exactly the silence the audit found.
        assert_eq!(coverage.unaccounted(), 8);
    }

    #[test]
    fn an_unclear_relation_is_never_safe_to_follow() {
        assert!(AliasRelation::Alias.is_safe_to_follow());
        assert!(!AliasRelation::Sense.is_safe_to_follow());
        assert!(!AliasRelation::Unclear.is_safe_to_follow());
        assert!(!SynonymRelation::Unclear.is_safe_to_follow());
    }

    #[test]
    fn a_resemblance_between_names_can_never_support_a_link() {
        assert!(!IdentityBasis::NameSimilarityOnly.can_support_a_link());
        assert!(IdentityBasis::IdenticalDesignationQuoted.can_support_a_link());
        assert!(IdentityBasis::AliasQuotedInBoth.can_support_a_link());
    }

    #[test]
    fn only_a_claim_has_to_carry_a_quotation() {
        assert!(ApplicationDetailKind::Parameter.is_a_claim());
        assert!(ApplicationDetailKind::Constraint.is_a_claim());
        assert!(!ApplicationDetailKind::Question.is_a_claim());
    }

    #[test]
    fn enumerations_round_trip_through_their_wire_form() {
        for topic in DeclarationTopic::ALL {
            assert_eq!(DeclarationTopic::parse(topic.as_str()), Some(topic));
        }
        assert_eq!(
            CoverageState::parse("partial_accounted"),
            Some(CoverageState::PartialAccounted)
        );
        assert_eq!(CoverageState::parse("good_enough"), None);
        assert_eq!(
            RequirementsState::parse("met"),
            Some(RequirementsState::Met)
        );
        assert_eq!(
            UncertaintyKind::parse("ambiguous_table_cell"),
            Some(UncertaintyKind::AmbiguousTableCell)
        );
        assert_eq!(IdentityState::parse("linked"), Some(IdentityState::Linked));
        assert_eq!(
            ApplicationDetailKind::parse("parameter"),
            Some(ApplicationDetailKind::Parameter)
        );
        assert_eq!(
            DeclarationOrigin::parse("server"),
            Some(DeclarationOrigin::Server)
        );
        assert_eq!(
            SynonymRelation::parse("abbreviation"),
            Some(SynonymRelation::Abbreviation)
        );
    }
}
