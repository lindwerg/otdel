//! Domain model of phase 1C — the structured product draft built from read pages.
//!
//! Everything here is a **candidate**. Nothing in this phase is published, verified or
//! allowed to answer a customer: verification and publication are 1E
//! (`docs/block-01-plan.md`). The rules the types enforce come from
//! `docs/block-01-spec.md` §6.4 and §6.6:
//!
//! * **a fact without a source is not a fact.** [`KnowledgeFact`] always travels with
//!   at least one [`FactEvidence`], and the database refuses a fact whose evidence is
//!   missing at commit time (migration `0004_knowledge.sql`);
//! * **a quote is a quote.** [`FactEvidence::quote`] is a verbatim fragment of the
//!   stored page text, located by character offsets into that page. Anything the model
//!   added in its own words lives in [`KnowledgeFact::model_context`], which the
//!   interface labels as *not* a quote;
//! * **a value keeps its unit and its conditions.** They are separate fields, stored as
//!   written, and an unknown value becomes a [`KnowledgeGap`] rather than a guess.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// What a knowledge object describes.
///
/// `direction` is the broad offering ("монтажные системы"), `family` a group inside it.
/// Both are categories; the sellable things are [`ProductKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CategoryKind {
    Direction,
    Family,
}

impl CategoryKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Direction => "direction",
            Self::Family => "family",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "direction" => Some(Self::Direction),
            "family" => Some(Self::Family),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductKind {
    Product,
    Service,
}

impl ProductKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Product => "product",
            Self::Service => "service",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "product" => Some(Self::Product),
            "service" => Some(Self::Service),
            _ => None,
        }
    }
}

/// What kind of statement a fact makes about its subject.
///
/// `commercial` exists so a price or a lead time found *in the partner's own document*
/// can be recorded with its source — not so one can be inferred. A commercial value
/// that is not written anywhere becomes a gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactKind {
    /// A measurable or descriptive property ("длина", "нагрузка", "материал").
    Characteristic,
    /// A restriction ("не для наружного применения").
    Limitation,
    /// Where the product is used ("монтаж трубопроводов").
    Application,
    /// Price, lead time, packaging — only ever when literally present in the source.
    Commercial,
}

impl FactKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Characteristic => "characteristic",
            Self::Limitation => "limitation",
            Self::Application => "application",
            Self::Commercial => "commercial",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "characteristic" => Some(Self::Characteristic),
            "limitation" => Some(Self::Limitation),
            "application" => Some(Self::Application),
            "commercial" => Some(Self::Commercial),
            _ => None,
        }
    }
}

/// Status of a candidate statement.
///
/// Phase 1C produces exactly one value: [`FactStatus::Candidate`]. The other statuses
/// named in `docs/block-01-spec.md` §6.7 (`source_supported`, `conflicted`, …) are
/// assigned by the checker in 1E, and inventing them here would claim a verification
/// that has not happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactStatus {
    Candidate,
}

impl FactStatus {
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

/// Who a prepared question is addressed to. Nothing is sent in this phase: delivery
/// belongs to the common communication channel (`docs/block-01-spec.md` §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionAudience {
    /// To be asked of the partner/manufacturer.
    Partner,
    /// To be answered by the bounded industry research of phase 1D.
    Industry,
}

impl QuestionAudience {
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

/// Lifecycle of one understanding run over one material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeRunStatus {
    Queued,
    Running,
    /// Everything the model returned was accepted.
    Completed,
    /// Some candidates were refused (no real source, quote not found, …) and the rest
    /// were kept. The counters say how many.
    Partial,
    Failed,
    /// The model adapter is not configured (no key/model/endpoint). Nothing was called
    /// and nothing was stored — this is a configuration state, not a failure of the
    /// material.
    NeedsProvider,
}

impl KnowledgeRunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::NeedsProvider => "needs_provider",
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
            _ => None,
        }
    }

    /// Whether the owner pressing "разобрать ещё раз" can plausibly change anything.
    pub const fn can_retry(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Partial | Self::Failed | Self::NeedsProvider
        )
    }
}

// --- stored entities ------------------------------------------------------------------

/// A product direction or family drafted from one material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductCategory {
    pub id: Uuid,
    pub partner_id: Uuid,
    /// The material whose run produced this candidate. Re-running that material
    /// replaces its candidates; candidates of other materials are untouched.
    pub material_id: Uuid,
    pub run_id: Uuid,
    pub kind: CategoryKind,
    pub name: String,
    pub summary: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Product {
    pub id: Uuid,
    pub partner_id: Uuid,
    pub material_id: Uuid,
    pub run_id: Uuid,
    pub category_id: Option<Uuid>,
    pub kind: ProductKind,
    pub name: String,
    pub summary: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One verbatim fragment of a page, with the offsets that locate it there.
///
/// `quote` is not what the model wrote: it is the substring of the stored page text the
/// model's quote was matched against, so what the owner reads is what the document
/// says. `char_start`/`char_end` are character (not byte) offsets into
/// `material_pages.text_content`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactEvidence {
    pub id: Uuid,
    pub material_id: Uuid,
    pub material_filename: String,
    pub page_id: Uuid,
    pub page_number: i32,
    pub region_id: Option<Uuid>,
    pub quote: String,
    pub char_start: i32,
    pub char_end: i32,
}

/// A candidate statement about a product, with its unit, its conditions and its sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeFact {
    pub id: Uuid,
    pub partner_id: Uuid,
    pub material_id: Uuid,
    pub run_id: Uuid,
    pub product_id: Option<Uuid>,
    pub product_name: Option<String>,
    pub kind: FactKind,
    pub status: FactStatus,
    /// The property being stated ("длина", "нагрузка").
    pub attribute: String,
    /// The value exactly as the source writes it. Never rounded, never converted.
    pub value_text: String,
    /// Only set when the unit is written in the source next to the value.
    pub unit: Option<String>,
    /// When the value holds ("при опирании на две опоры"), as written.
    pub conditions: Option<String>,
    /// The model's own explanation. **Not a quote** — the interface says so, and no
    /// evidence points at it.
    pub model_context: Option<String>,
    pub evidence: Vec<FactEvidence>,
    /// R05 — which structure the value sat in, and what that structure said it was about.
    /// Never a replacement for the quotation: both are shown.
    #[serde(default)]
    pub origin: crate::passport::FactOrigin,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlossaryTerm {
    pub id: Uuid,
    pub partner_id: Uuid,
    pub material_id: Uuid,
    pub run_id: Uuid,
    pub term: String,
    pub definition: String,
    /// `true` when the definition is the model's wording rather than the source's. The
    /// evidence still points at the fragment the term was read from.
    pub definition_is_model_context: bool,
    pub evidence: Vec<FactEvidence>,
    /// R05 — further readings of the same word in the same material. One definition for a
    /// word the catalogue uses two ways makes the second usage wrong or invisible.
    #[serde(default)]
    pub senses: Vec<crate::passport::GlossarySense>,
    /// R05 — other spellings, recorded and never merged.
    #[serde(default)]
    pub synonyms: Vec<crate::passport::GlossarySynonym>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QaEntry {
    pub id: Uuid,
    pub partner_id: Uuid,
    pub material_id: Uuid,
    pub run_id: Uuid,
    pub question: String,
    pub answer: String,
    /// `true` when the answer is the model's own wording rather than a phrase found in
    /// the source. An answer is a synthesis, so this is normally true — and the
    /// interface says so instead of letting the quotation below it vouch for the
    /// sentence above it.
    pub answer_is_model_context: bool,
    pub evidence: Vec<FactEvidence>,
    pub created_at: DateTime<Utc>,
}

/// Something the material does not say. A gap is the honest alternative to a guess.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeGap {
    pub id: Uuid,
    pub partner_id: Uuid,
    pub material_id: Uuid,
    pub run_id: Uuid,
    pub product_id: Option<Uuid>,
    pub product_name: Option<String>,
    /// Short machine-ish label of the missing area (`price`, `lead_time`, …).
    pub topic: String,
    /// What exactly is missing.
    pub missing: String,
    /// Which answers this gap prevents.
    pub blocks: Option<String>,
    /// R05 — which kind of unknown this is, classified by the run rather than guessed
    /// from the topic's wording. The requirement check asks about the commercial and the
    /// technical unknowns separately, and a lexicon in that path would turn a vocabulary
    /// miss into a silent clearance.
    #[serde(default)]
    pub nature: crate::passport::GapNature,
    /// The question prepared from this gap, when there is one.
    pub question: Option<PreparedQuestion>,
    pub created_at: DateTime<Utc>,
}

/// A question waiting for a channel (partner) or for the 1D researcher (industry).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedQuestion {
    pub id: Uuid,
    pub audience: QuestionAudience,
    pub text: String,
    /// Always `prepared` in this phase: nothing is sent and nothing is researched yet.
    pub status: String,
    pub created_at: DateTime<Utc>,
}

/// One run of the product role over one material, with what it produced and refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeRun {
    pub id: Uuid,
    pub partner_id: Uuid,
    pub material_id: Uuid,
    /// File name of the material, so the interface can name it without a second
    /// request.
    pub material_filename: String,
    pub status: KnowledgeRunStatus,
    /// Provider and model that produced the draft, recorded so a later re-run with a
    /// different model is distinguishable (`docs/block-01-spec.md` §6.1).
    pub provider: Option<String>,
    pub model: Option<String>,
    pub prompt_profile: String,
    pub pages_considered: i32,
    /// R05 — the page account of this run: how many pages the material has, how many were
    /// offered, processed, deferred and unreadable, and the verdict over that account.
    ///
    /// It lives on the run rather than beside it because the audited failure was exactly
    /// a reader who saw `pages_considered` and had no way to ask "out of how many". A
    /// caller holding a run now cannot avoid seeing the denominator.
    pub coverage: crate::passport::RunCoverage,
    /// R05 — tasks, explicit absences and unsettled readings this run left behind.
    ///
    /// Beside the counters above rather than below them, because that is how they have to
    /// be read: «44 изделия, 13 фактов» was reported as success, and the same line with
    /// «0 задач, 0 заявлений, 8 неясностей» beside it is not.
    #[serde(default)]
    pub applications_created: i32,
    #[serde(default)]
    pub declarations_made: i32,
    #[serde(default)]
    pub uncertainties_open: i32,
    pub requests_made: i32,
    pub input_chars: i32,
    pub categories_created: i32,
    pub products_created: i32,
    pub facts_accepted: i32,
    /// Candidates refused by validation: unknown source, quote not found in the page,
    /// missing value. The reasons are in `rejections`.
    pub facts_rejected: i32,
    pub terms_created: i32,
    pub qa_created: i32,
    pub gaps_created: i32,
    pub questions_created: i32,
    /// Human-readable reasons, deduplicated and bounded. Shown as-is.
    pub rejections: Vec<String>,
    pub diagnostic: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// A material that can be drafted but has no run yet.
///
/// Exists so the interface can offer the *first* draft of a material. Without it the
/// only button would be "разобрать заново" next to an existing run, and a material
/// read before this phase — every material in the current pilot — would have no way in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftableMaterial {
    pub material_id: Uuid,
    pub filename: String,
    /// Pages with usable text; the only pages a draft can be built from.
    pub pages_with_text: i32,
}

/// Partner-level roll-up for the knowledge tab.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeSummary {
    pub categories_total: i32,
    pub products_total: i32,
    pub facts_total: i32,
    pub terms_total: i32,
    pub qa_total: i32,
    pub gaps_total: i32,
    pub questions_total: i32,
    /// R05 — tasks the materials say the products serve.
    pub applications_total: i32,
    /// R05 — things the materials state in a form nobody may read as a value. Shown
    /// beside the totals above rather than below them: a base with many facts and many
    /// open uncertainties is not the same base as one with many facts and none.
    pub uncertainties_total: i32,
    /// R05 — products whose passport carries something a reader could act on.
    pub passports_substantive: i32,
    /// Materials that have been read and could be drafted from.
    pub materials_readable: i32,
    /// Materials that have a completed or partial run.
    pub materials_understood: i32,
    /// R05 — materials whose run may be published without a person reading it first.
    pub materials_ready: i32,
}

/// Stable idempotency key for the understanding job of one material.
///
/// Pressing "разобрать" twice, or the extraction worker finishing twice, reuses the
/// same queue row instead of drafting the same material twice in parallel.
pub fn understanding_idempotency_key(material_id: Uuid) -> String {
    format!("understand_material:{material_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerations_round_trip_through_their_wire_form() {
        assert_eq!(CategoryKind::parse("family"), Some(CategoryKind::Family));
        assert_eq!(CategoryKind::parse("product"), None);
        assert_eq!(ProductKind::parse("service"), Some(ProductKind::Service));
        assert_eq!(FactKind::parse("commercial"), Some(FactKind::Commercial));
        assert_eq!(FactKind::parse("guess"), None);
        assert_eq!(
            QuestionAudience::parse("industry"),
            Some(QuestionAudience::Industry)
        );
        for status in [
            KnowledgeRunStatus::Queued,
            KnowledgeRunStatus::Running,
            KnowledgeRunStatus::Completed,
            KnowledgeRunStatus::Partial,
            KnowledgeRunStatus::Failed,
            KnowledgeRunStatus::NeedsProvider,
        ] {
            assert_eq!(KnowledgeRunStatus::parse(status.as_str()), Some(status));
            assert_eq!(
                serde_json::to_value(status).unwrap(),
                serde_json::Value::String(status.as_str().to_owned())
            );
        }
    }

    #[test]
    fn phase_1c_has_exactly_one_fact_status_and_it_is_not_verified() {
        assert_eq!(FactStatus::Candidate.as_str(), "candidate");
        // The checker's vocabulary belongs to 1E and must not be storable here.
        assert_eq!(FactStatus::parse("source_supported"), None);
        assert_eq!(FactStatus::parse("published"), None);
    }

    #[test]
    fn a_run_that_only_needs_a_key_can_be_repeated_once_it_has_one() {
        assert!(KnowledgeRunStatus::NeedsProvider.can_retry());
        assert!(KnowledgeRunStatus::Failed.can_retry());
        assert!(!KnowledgeRunStatus::Queued.can_retry());
        assert!(!KnowledgeRunStatus::Running.can_retry());
    }

    #[test]
    fn the_understanding_key_is_stable_per_material() {
        let material = Uuid::from_u128(11);
        assert_eq!(
            understanding_idempotency_key(material),
            understanding_idempotency_key(material)
        );
        assert_ne!(
            understanding_idempotency_key(material),
            understanding_idempotency_key(Uuid::from_u128(12))
        );
    }

    #[test]
    fn a_fact_serialises_with_its_evidence_and_its_separated_model_context() {
        let now = DateTime::<Utc>::from_timestamp(0, 0).unwrap();
        let fact = KnowledgeFact {
            id: Uuid::from_u128(1),
            partner_id: Uuid::from_u128(2),
            material_id: Uuid::from_u128(3),
            run_id: Uuid::from_u128(4),
            product_id: Some(Uuid::from_u128(5)),
            product_name: Some("BP21".to_owned()),
            kind: FactKind::Characteristic,
            status: FactStatus::Candidate,
            attribute: "нагрузка".to_owned(),
            value_text: "3.5".to_owned(),
            unit: Some("kN".to_owned()),
            conditions: Some("при опирании на две опоры".to_owned()),
            model_context: Some("значение приведено для профиля BP21".to_owned()),
            evidence: vec![FactEvidence {
                id: Uuid::from_u128(6),
                material_id: Uuid::from_u128(3),
                material_filename: "catalogue.pdf".to_owned(),
                page_id: Uuid::from_u128(7),
                page_number: 3,
                region_id: None,
                quote: "BP21 1200 3.5".to_owned(),
                char_start: 10,
                char_end: 23,
            }],
            origin: crate::passport::FactOrigin {
                source: crate::passport::StructuralSource::TableCell,
                cell_id: Some(Uuid::from_u128(8)),
                subject: Some("BP21".to_owned()),
                property: Some("Безопасная рабочая нагрузка".to_owned()),
                unit: Some("кН".to_owned()),
                conditions: vec!["две опоры".to_owned()],
            },
            created_at: now,
        };

        let value = serde_json::to_value(&fact).unwrap();
        assert_eq!(value["status"], "candidate");
        // R05: which structure the number sat in travels with the fact, beside the
        // quotation rather than instead of it.
        assert_eq!(value["origin"]["source"], "table_cell");
        assert_eq!(value["origin"]["property"], "Безопасная рабочая нагрузка");
        assert!(value["evidence"][0]["quote"].is_string());
        assert_eq!(value["unit"], "kN");
        assert_eq!(value["evidence"][0]["page_number"], 3);
        assert_eq!(value["evidence"][0]["quote"], "BP21 1200 3.5");
        // The model's wording is a separate field, never merged into the quote.
        assert_ne!(value["model_context"], value["evidence"][0]["quote"]);
    }
}
