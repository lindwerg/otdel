//! The response contract: what the product role is allowed to return.
//!
//! Two halves that must agree:
//!
//! * [`response_schema`] is the JSON Schema sent to the provider in structured-output
//!   mode. It is deliberately restricted to the keywords strict structured output
//!   actually supports (`type`, `enum`, `properties`, `required`,
//!   `additionalProperties: false`, `items`) — lengths and counts are **not** expressed
//!   there, because a schema keyword a provider silently ignores is worse than no
//!   keyword at all;
//! * [`DraftResponse`] is what the server accepts, with `deny_unknown_fields` and with
//!   every bound enforced in Rust afterwards ([`crate::validate`]).
//!
//! The schema is the request; the Rust types are the enforcement. A provider that
//! ignores the schema cannot widen what gets stored.

use serde::Deserialize;
use serde_json::{json, Value};

/// Name sent to the provider alongside the schema.
pub const SCHEMA_NAME: &str = "otdel_product_knowledge_draft";

/// Version of the prompt + schema pair, stored on every run.
///
/// A later run with a different profile is distinguishable from an older one, which is
/// what `docs/block-01-spec.md` §6.1 asks for when a processing profile changes.
///
/// R05 raises it: the response now carries applications, aliases, senses, synonyms, a
/// classification on every gap and — the part that matters most — the declarations that
/// make "there are none" a statement instead of an empty array.
/// R05.2 raises it again: the run is now five purpose-specific passes instead of one
/// omnibus request, so a draft made by this profile is not comparable with one made by
/// the previous.
/// R05.3 raises it once more: per-purpose page budgets, a stated response bound, and a
/// scheduler that recovers from a truncated answer instead of failing the run.
pub const PROMPT_PROFILE: &str = "productologist/2026-09-14.r05.3";

/// Upper bounds on what one response may contain. Anything beyond is refused (and
/// counted), never silently truncated into "success".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DraftLimits {
    pub max_categories: usize,
    pub max_products: usize,
    pub max_facts: usize,
    pub max_terms: usize,
    pub max_qa: usize,
    pub max_gaps: usize,
    pub max_evidence_per_item: usize,
    pub max_short_text_chars: usize,
    pub max_long_text_chars: usize,
    /// R05 — tasks a response may propose.
    pub max_applications: usize,
    /// Parameters, constraints and questions under one application, each counted apart.
    pub max_details_per_application: usize,
    /// Recorded surface forms under one product or one term.
    pub max_surface_forms_per_item: usize,
    /// Readings of one term.
    pub max_senses_per_term: usize,
}

impl Default for DraftLimits {
    fn default() -> Self {
        Self {
            max_categories: 20,
            max_products: 60,
            max_facts: 200,
            max_terms: 24,
            max_qa: 40,
            max_gaps: 60,
            max_evidence_per_item: 4,
            // Names, attributes, units, topics.
            max_short_text_chars: 200,
            // Summaries, definitions, answers, conditions, model context.
            max_long_text_chars: 1_000,
            // R05.3 — lowered from 40/12. These are per *response*, and the run now
            // makes several small applications requests instead of one large one, so the
            // old ceilings only ever authorised an answer too long to come back whole.
            max_applications: 12,
            max_details_per_application: 6,
            max_surface_forms_per_item: 6,
            max_senses_per_term: 4,
        }
    }
}

/// One cited fragment: which source, and the words the model claims are there.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DraftEvidence {
    /// A label from the prompt's source list (`S1`, `S2`, …).
    pub source: String,
    /// The words the model says appear on that page. Checked literally.
    pub quote: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DraftCategory {
    /// Local reference used by products in this same response. Never an identifier of
    /// anything stored: the server assigns real ids.
    #[serde(rename = "ref")]
    pub reference: String,
    /// `direction` | `family`.
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub summary: Option<String>,
}

/// A surface form the material uses for a product or a term.
///
/// Recorded, never merged. `relation` carries how far the claim goes, and `unclear` is a
/// legal answer — it is the one that keeps a resemblance from becoming an identity.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DraftSurfaceForm {
    pub surface: String,
    /// For a product: `alias` | `sense` | `unclear`. For a term: `synonym` |
    /// `abbreviation` | `unclear`.
    pub relation: String,
    #[serde(default)]
    pub note: Option<String>,
    /// One fragment showing this form in the document. Required: a surface form nobody
    /// can point at is not recorded.
    #[serde(default)]
    pub evidence: Vec<DraftEvidence>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DraftProduct {
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default)]
    pub category_ref: Option<String>,
    /// `product` | `service`.
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub summary: Option<String>,
    /// Other names the material uses for this product.
    #[serde(default)]
    pub aliases: Vec<DraftSurfaceForm>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DraftFact {
    #[serde(default)]
    pub product_ref: Option<String>,
    /// `characteristic` | `limitation` | `application` | `commercial`.
    pub kind: String,
    pub attribute: String,
    /// The value as written in the source. Not parsed, not converted.
    pub value: String,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub conditions: Option<String>,
    /// Anything the model wants to add in its own words. Stored separately and shown
    /// as not-a-quote.
    #[serde(default)]
    pub model_context: Option<String>,
    #[serde(default)]
    pub evidence: Vec<DraftEvidence>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DraftTerm {
    pub term: String,
    pub definition: String,
    /// `true` when the definition is copied from the source, `false` when the model
    /// wrote it. Either way the evidence must point at a real fragment.
    #[serde(default)]
    pub definition_from_source: bool,
    #[serde(default)]
    pub evidence: Vec<DraftEvidence>,
    /// Further readings of the same word inside this material. A catalogue that uses
    /// «консоль» for a bracket in one section and for a load scheme in another has two,
    /// and one definition would make the second usage either wrong or invisible.
    #[serde(default)]
    pub senses: Vec<DraftSense>,
    /// Other ways the material writes this term.
    #[serde(default)]
    pub synonyms: Vec<DraftSurfaceForm>,
}

/// One reading of a term, with its own evidence.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DraftSense {
    /// Short disambiguator: «в контексте кабельных лотков». Not a number.
    pub label: String,
    pub definition: String,
    #[serde(default)]
    pub definition_from_source: bool,
    #[serde(default)]
    pub evidence: Vec<DraftEvidence>,
}

/// One line under an application.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DraftApplicationDetail {
    /// `parameter` | `constraint` | `question`.
    pub kind: String,
    pub label: String,
    /// The source's value. Required for a parameter or a constraint; omitted for a
    /// question, which asserts nothing.
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub unit: Option<String>,
    /// `partner` | `industry`. Only for a question, and required there.
    #[serde(default)]
    pub audience: Option<String>,
    #[serde(default)]
    pub evidence: Vec<DraftEvidence>,
}

/// A task the material says a product serves.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DraftApplication {
    #[serde(default)]
    pub product_ref: Option<String>,
    /// The task in the buyer's words: «закрепить кабельный лоток к бетонному перекрытию».
    pub task: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub model_context: Option<String>,
    #[serde(default)]
    pub evidence: Vec<DraftEvidence>,
    #[serde(default)]
    pub details: Vec<DraftApplicationDetail>,
}

/// The run's explicit statements that a topic has nothing in it.
///
/// Every field is nullable, and `null` means "not declared" — which is *not* the same as
/// "there is none". A topic with no rows and no declaration fails its requirement, and
/// that asymmetry is the entire reason this object exists: the audited run produced empty
/// arrays for four topics and nothing could tell that apart from four honest absences.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DraftDeclarations {
    #[serde(default)]
    pub glossary: Option<String>,
    #[serde(default)]
    pub questions: Option<String>,
    #[serde(default)]
    pub applications: Option<String>,
    #[serde(default)]
    pub commercial_unknowns: Option<String>,
    #[serde(default)]
    pub technical_unknowns: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DraftQa {
    pub question: String,
    pub answer: String,
    #[serde(default)]
    pub evidence: Vec<DraftEvidence>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DraftGap {
    #[serde(default)]
    pub product_ref: Option<String>,
    /// Short area label; free text bounded by the limits, e.g. `price`, `lead_time`.
    pub topic: String,
    /// What the material does not say.
    pub missing: String,
    #[serde(default)]
    pub blocks: Option<String>,
    /// The question to ask, when the model proposes one.
    #[serde(default)]
    pub question: Option<String>,
    /// `partner` | `industry`. Only meaningful together with `question`.
    #[serde(default)]
    pub audience: Option<String>,
    /// `commercial` | `technical` | `other`.
    ///
    /// The requirement check asks separately whether the commercial unknowns and the
    /// technical ones were recorded, so the run classifies its own gaps. The alternative
    /// — matching words against the free-text topic — would put a lexicon in the
    /// publication path, where a vocabulary miss silently becomes a clearance.
    #[serde(default)]
    pub nature: Option<String>,
}

/// The whole response. Missing arrays are accepted as empty; unknown fields are not.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DraftResponse {
    #[serde(default)]
    pub categories: Vec<DraftCategory>,
    #[serde(default)]
    pub products: Vec<DraftProduct>,
    #[serde(default)]
    pub facts: Vec<DraftFact>,
    #[serde(default)]
    pub glossary: Vec<DraftTerm>,
    #[serde(default)]
    pub qa: Vec<DraftQa>,
    #[serde(default)]
    pub gaps: Vec<DraftGap>,
    #[serde(default)]
    pub applications: Vec<DraftApplication>,
    /// The statements that make "there are none" an answer. Defaulted rather than
    /// required on the Rust side so an older stored response still parses; the JSON
    /// schema does require it, which is where the model is actually held to it.
    #[serde(default)]
    pub declarations: DraftDeclarations,
}

impl DraftResponse {
    /// Parse a provider's JSON object into the response contract.
    pub fn parse(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| {
            // serde's message names the offending field, which is a schema detail, not
            // partner data — it is safe (and useful) to keep in the run record.
            format!("ответ модели не соответствует схеме: {error}")
        })
    }
}

fn nullable_string() -> Value {
    json!({"type": ["string", "null"]})
}

fn evidence_schema() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "additionalProperties": false,
            "required": ["source", "quote"],
            "properties": {
                "source": {
                    "type": "string",
                    "description": "Метка фрагмента из запроса: S1, S2, …",
                },
                "quote": {
                    "type": "string",
                    "description": "Дословный отрывок этого фрагмента, символ в символ.",
                },
            },
        },
    })
}

/// An array of surface forms, parameterised by the relations this position allows.
///
/// Products and terms describe different kinds of resemblance (`alias`/`sense` versus
/// `synonym`/`abbreviation`), but both admit `unclear` and both require evidence — so the
/// shape is shared and only the enum differs. `evidence` is required here rather than
/// optional: [`crate::validate`] drops a surface form whose fragment does not resolve, and
/// asking for it in the schema makes that refusal rare instead of routine.
fn surface_form_schema(relations: &[&str], description: &str) -> Value {
    json!({
        "type": "array",
        "description": description,
        "items": {
            "type": "object",
            "additionalProperties": false,
            "required": ["surface", "relation", "note", "evidence"],
            "properties": {
                "surface": {
                    "type": "string",
                    "description": "Написание дословно из материала.",
                },
                "relation": {"type": "string", "enum": relations},
                "note": nullable_string(),
                "evidence": evidence_schema(),
            },
        },
    })
}

/// One bounded pass over the material, asking for one kind of thing.
///
/// R05.2. A single omnibus request asking for products, facts, terms, tasks, questions and
/// gaps at once has a failure mode nothing downstream can correct: the model spends the
/// whole output budget on the cheapest section. A live pass over a technical catalogue
/// came back with 51 products, 6 facts and **0 terms, 0 applications** — not because the
/// material lacked them, but because the request was over before it got there.
///
/// Splitting the run into purpose-specific passes removes the competition. Each pass is
/// sent a schema that contains *only* its own sections, so spending an applications pass
/// on products is not a thing the model can do — `additionalProperties: false` makes the
/// other sections unrepresentable rather than merely discouraged.
///
/// The order is a dependency order, not a preference: everything after [`Self::Inventory`]
/// may refer to the products it accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftPurpose {
    /// Directions, families, products and their recorded surface forms.
    Inventory,
    /// Characteristics and limitations, with the table context R03 established.
    Facts,
    /// Terms, their further readings and their spellings.
    Glossary,
    /// Tasks the material says a product serves, with the parameters and constraints
    /// needed to choose, and the questions it does not settle.
    Applications,
    /// Answers the material supports, what it does not say, and the explicit absences.
    Inquiry,
}

impl DraftPurpose {
    /// Every pass, in dependency order.
    pub const ALL: [Self; 5] = [
        Self::Inventory,
        Self::Facts,
        Self::Glossary,
        Self::Applications,
        Self::Inquiry,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inventory => "inventory",
            Self::Facts => "facts",
            Self::Glossary => "glossary",
            Self::Applications => "applications",
            Self::Inquiry => "inquiry",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "inventory" => Some(Self::Inventory),
            "facts" => Some(Self::Facts),
            "glossary" => Some(Self::Glossary),
            "applications" => Some(Self::Applications),
            "inquiry" => Some(Self::Inquiry),
            _ => None,
        }
    }

    /// Which top-level sections this pass may return.
    ///
    /// Deliberately narrow. A pass that could also return products would be a pass that
    /// can spend its budget on them, which is the whole defect.
    pub const fn sections(self) -> &'static [&'static str] {
        match self {
            Self::Inventory => &["categories", "products"],
            Self::Facts => &["facts"],
            Self::Glossary => &["glossary"],
            Self::Applications => &["applications"],
            Self::Inquiry => &["qa", "gaps", "declarations"],
        }
    }

    /// Whether this pass needs to be told which products already exist.
    ///
    /// Everything except the inventory refers to products by a label the server assigns,
    /// so no later pass has to re-list them — re-listing is how the budget would be spent
    /// on products all over again.
    pub const fn needs_product_context(self) -> bool {
        !matches!(self, Self::Inventory)
    }

    /// Pages this purpose may put in one request, given the configured base.
    ///
    /// R05.3. The passes are not equally verbose per page. An inventory line is a name
    /// and a summary; one application is a task, a summary, and up to a dozen parameters
    /// and constraints, each with its own quotation. Six pages of a technical catalogue
    /// asked for applications is several thousand tokens of output, and a live run had
    /// exactly that batch come back truncated.
    ///
    /// So the verbose purposes get fewer pages per request. This is the cheap half of the
    /// fix — it makes truncation rare; [`crate::draft_knowledge`] handles the times it
    /// happens anyway.
    pub fn pages_per_request(self, base: u32) -> u32 {
        let ceiling = match self {
            // The most verbose: every task carries its own details and quotations.
            Self::Applications => 2,
            // A term carries senses and synonyms, each quoted.
            Self::Glossary => 3,
            Self::Inquiry => 4,
            Self::Inventory | Self::Facts => base,
        };
        base.min(ceiling).max(1)
    }

    /// How many items of this purpose one response should carry, told to the model.
    ///
    /// Strict structured output cannot express counts (see this module's header), so the
    /// bound is stated in words and enforced afterwards by [`crate::validate`]. Saying it
    /// is still worth doing: a model asked for "up to 8" writes 8 short entries where one
    /// asked for nothing in particular writes 40 long ones and runs out of room.
    pub const fn max_items_per_response(self) -> usize {
        match self {
            Self::Inventory => 24,
            Self::Facts => 40,
            Self::Glossary => 12,
            Self::Applications => 8,
            Self::Inquiry => 16,
        }
    }

    /// The schema name reported to the provider, for its logs and ours.
    pub fn schema_name(self) -> String {
        format!("{SCHEMA_NAME}_{}", self.as_str())
    }
}

/// The JSON Schema for one pass: the full schema, narrowed to that pass's sections.
///
/// Narrowed rather than written out per purpose, so a change to how a fact is described
/// cannot apply to one pass and not another.
pub fn response_schema_for(purpose: DraftPurpose) -> Value {
    let full = response_schema();
    let properties = full
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let kept: serde_json::Map<String, Value> = purpose
        .sections()
        .iter()
        .filter_map(|name| {
            properties
                .get(*name)
                .map(|schema| ((*name).to_owned(), schema.clone()))
        })
        .collect();

    json!({
        "type": "object",
        "additionalProperties": false,
        // Required, not merely allowed: a pass that answers with an empty object has not
        // been asked a question it could dodge.
        "required": purpose.sections(),
        "properties": kept,
    })
}

/// The JSON Schema of the whole draft. The per-pass schemas are slices of it.
pub fn response_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "categories", "products", "facts", "glossary", "qa", "gaps",
            "applications", "declarations",
        ],
        "properties": {
            "categories": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["ref", "kind", "name", "summary"],
                    "properties": {
                        "ref": {
                            "type": "string",
                            "description": "Короткий ярлык этого направления внутри ответа, например \"c1\". На него ссылаются изделия через category_ref.",
                        },
                        "kind": {"type": "string", "enum": ["direction", "family"]},
                        "name": {"type": "string"},
                        "summary": nullable_string(),
                    },
                },
            },
            "products": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["ref", "category_ref", "kind", "name", "summary", "aliases"],
                    "properties": {
                        "ref": {
                            "type": "string",
                            "description": "Короткий ярлык этого изделия внутри ответа, например \"p1\". Факты и пробелы ссылаются на него через product_ref.",
                        },
                        "category_ref": {
                            "type": ["string", "null"],
                            "description": "Ярлык направления (поле ref из categories), не его название.",
                        },
                        "kind": {"type": "string", "enum": ["product", "service"]},
                        "name": {"type": "string"},
                        "summary": nullable_string(),
                        "aliases": surface_form_schema(
                            &["alias", "sense", "unclear"],
                            "Другое написание этого же изделия в материале. alias — то же изделие другими словами; sense — более узкое значение в одном разделе; unclear — похоже, но материал этого не подтверждает. Форма должна встречаться в цитате дословно.",
                        ),
                    },
                },
            },
            "facts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": [
                        "product_ref", "kind", "attribute", "value", "unit",
                        "conditions", "model_context", "evidence",
                    ],
                    "properties": {
                        "product_ref": {
                            "type": ["string", "null"],
                            "description": "Ярлык изделия (поле ref из products), например \"p1\", а не его название. null — факт о предложении в целом.",
                        },
                        "kind": {
                            "type": "string",
                            "enum": ["characteristic", "limitation", "application", "commercial"],
                        },
                        "attribute": {"type": "string"},
                        "value": {
                            "type": "string",
                            "description": "Значение дословно из источника. Оно должно встречаться в приложенной цитате, иначе факт отбрасывается.",
                        },
                        "unit": nullable_string(),
                        "conditions": nullable_string(),
                        "model_context": nullable_string(),
                        "evidence": evidence_schema(),
                    },
                },
            },
            "glossary": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": [
                        "term", "definition", "definition_from_source", "evidence",
                        "senses", "synonyms",
                    ],
                    "properties": {
                        "term": {"type": "string"},
                        "definition": {"type": "string"},
                        "definition_from_source": {"type": "boolean"},
                        "evidence": evidence_schema(),
                        "senses": {
                            "type": "array",
                            "description": "Другие значения этого же слова в этом материале. Заполняется, только если материал действительно употребляет слово по-разному.",
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "required": [
                                    "label", "definition", "definition_from_source", "evidence",
                                ],
                                "properties": {
                                    "label": {
                                        "type": "string",
                                        "description": "Короткое уточнение значения, например \"в контексте кабельных лотков\". Не номер.",
                                    },
                                    "definition": {"type": "string"},
                                    "definition_from_source": {"type": "boolean"},
                                    "evidence": evidence_schema(),
                                },
                            },
                        },
                        "synonyms": surface_form_schema(
                            &["synonym", "abbreviation", "unclear"],
                            "Другое написание этого же термина в материале. synonym — то же понятие другими словами; abbreviation — сокращение; unclear — похоже, но материал этого не подтверждает.",
                        ),
                    },
                },
            },
            "qa": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["question", "answer", "evidence"],
                    "properties": {
                        "question": {"type": "string"},
                        "answer": {"type": "string"},
                        "evidence": evidence_schema(),
                    },
                },
            },
            "gaps": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": [
                        "product_ref", "topic", "missing", "blocks", "question", "audience",
                        "nature",
                    ],
                    "properties": {
                        "product_ref": {
                            "type": ["string", "null"],
                            "description": "Ярлык изделия (поле ref из products), не название.",
                        },
                        "topic": {"type": "string"},
                        "missing": {"type": "string"},
                        "blocks": nullable_string(),
                        "question": nullable_string(),
                        "audience": {
                            "type": ["string", "null"],
                            "enum": ["partner", "industry", null],
                            "description": "Кому адресован вопрос. Обязателен, если question заполнен: вопрос без адресата не сохраняется.",
                        },
                        "nature": {
                            "type": ["string", "null"],
                            "enum": ["commercial", "technical", "other", null],
                            "description": "Характер пробела: commercial — цена, срок, партия, условия поставки; technical — нагрузки, размеры, материалы, совместимость; other — всё остальное.",
                        },
                    },
                },
            },
            "applications": {
                "type": "array",
                "description": "Задачи, которые материал прямо связывает с изделием: не рекламные обещания, а работа, которую изделие выполняет.",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": [
                        "product_ref", "task", "summary", "model_context", "evidence", "details",
                    ],
                    "properties": {
                        "product_ref": {
                            "type": ["string", "null"],
                            "description": "Ярлык изделия (поле ref из products), не название.",
                        },
                        "task": {
                            "type": "string",
                            "description": "Задача словами покупателя: «закрепить кабельный лоток к бетонному перекрытию».",
                        },
                        "summary": nullable_string(),
                        "model_context": nullable_string(),
                        "evidence": evidence_schema(),
                        "details": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "required": [
                                    "kind", "label", "value", "unit", "audience", "evidence",
                                ],
                                "properties": {
                                    "kind": {
                                        "type": "string",
                                        "enum": ["parameter", "constraint", "question"],
                                    },
                                    "label": {"type": "string"},
                                    "value": {
                                        "type": ["string", "null"],
                                        "description": "Значение дословно из источника. Обязательно для parameter и constraint; для question — null, вопрос ничего не утверждает.",
                                    },
                                    "unit": nullable_string(),
                                    "audience": {
                                        "type": ["string", "null"],
                                        "enum": ["partner", "industry", null],
                                        "description": "Только для kind = question, и там обязателен.",
                                    },
                                    "evidence": evidence_schema(),
                                },
                            },
                        },
                    },
                },
            },
            // Not an array: a declaration is a sentence about a topic, and there is a
            // fixed list of topics. `null` means "not declared", which is deliberately
            // different from "there is none" — an empty array elsewhere in this response
            // satisfies no requirement unless the matching sentence appears here.
            "declarations": {
                "type": "object",
                "additionalProperties": false,
                "required": [
                    "glossary", "questions", "applications", "commercial_unknowns",
                    "technical_unknowns",
                ],
                "description": "Прямые утверждения «в материале этого нет». Заполняйте только то, что действительно проверили; null означает «не проверял», а не «нет».",
                "properties": {
                    "glossary": declaration_field(
                        "Почему в материале нет терминов, требующих пояснения.",
                    ),
                    "questions": declaration_field(
                        "Почему по материалу нечего спросить.",
                    ),
                    "applications": declaration_field(
                        "Почему материал не описывает задач применения.",
                    ),
                    "commercial_unknowns": declaration_field(
                        "Почему в материале не осталось коммерческих неизвестных (цена, срок, партия).",
                    ),
                    "technical_unknowns": declaration_field(
                        "Почему в материале не осталось технических неизвестных.",
                    ),
                },
            },
        },
    })
}

/// One declaration slot: a sentence, or `null` for "not declared".
fn declaration_field(description: &str) -> Value {
    json!({"type": ["string", "null"], "description": description})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_forbids_extra_properties_everywhere() {
        fn walk(value: &Value, path: &str) {
            if value.get("type") == Some(&json!("object")) {
                assert_eq!(
                    value.get("additionalProperties"),
                    Some(&json!(false)),
                    "object at {path} must forbid additional properties"
                );
                assert!(
                    value.get("required").is_some(),
                    "object at {path} must list its required properties"
                );
            }
            match value {
                Value::Object(map) => {
                    for (key, child) in map {
                        walk(child, &format!("{path}/{key}"));
                    }
                }
                Value::Array(items) => {
                    for (index, child) in items.iter().enumerate() {
                        walk(child, &format!("{path}/{index}"));
                    }
                }
                _ => {}
            }
        }
        walk(&response_schema(), "");
    }

    #[test]
    fn a_well_formed_response_parses() {
        let value = json!({
            "categories": [{"ref": "c1", "kind": "direction", "name": "Монтажные системы", "summary": null}],
            "products": [{
                "ref": "p1", "category_ref": "c1", "kind": "product",
                "name": "BP21", "summary": "профиль",
            }],
            "facts": [{
                "product_ref": "p1", "kind": "characteristic", "attribute": "нагрузка",
                "value": "3.5", "unit": "kN", "conditions": "две опоры",
                "model_context": null,
                "evidence": [{"source": "S1", "quote": "BP21 1200 3.5 kN"}],
            }],
            "glossary": [{
                "term": "консоль", "definition": "опорный элемент",
                "definition_from_source": false,
                "evidence": [{"source": "S1", "quote": "консоль крепится"}],
            }],
            "qa": [{"question": "Какая нагрузка?", "answer": "3.5 kN", "evidence": []}],
            "gaps": [{
                "product_ref": "p1", "topic": "price", "missing": "цена не указана",
                "blocks": "коммерческое предложение", "question": "Какая цена?",
                "audience": "partner",
            }],
        });

        let parsed = DraftResponse::parse(&value).unwrap();
        assert_eq!(parsed.facts.len(), 1);
        assert_eq!(parsed.facts[0].unit.as_deref(), Some("kN"));
        assert_eq!(parsed.gaps[0].audience.as_deref(), Some("partner"));
    }

    #[test]
    fn missing_arrays_are_empty_but_unknown_fields_are_refused() {
        let sparse = DraftResponse::parse(&json!({"facts": []})).unwrap();
        assert_eq!(sparse, DraftResponse::default());

        let error =
            DraftResponse::parse(&json!({"facts": [], "verdict": "published"})).unwrap_err();
        assert!(error.contains("схеме"), "{error}");

        // A fact that invents a field — say, a confidence score nobody measured.
        let error = DraftResponse::parse(&json!({
            "facts": [{
                "kind": "characteristic", "attribute": "a", "value": "b",
                "confidence": 0.9, "evidence": [],
            }],
        }))
        .unwrap_err();
        assert!(error.contains("схеме"), "{error}");
    }

    #[test]
    fn a_fact_missing_a_required_field_is_refused() {
        let error = DraftResponse::parse(&json!({
            "facts": [{"kind": "characteristic", "attribute": "нагрузка"}],
        }))
        .unwrap_err();
        assert!(error.contains("схеме"), "{error}");
    }

    #[test]
    fn limits_have_sane_defaults() {
        let limits = DraftLimits::default();
        assert!(limits.max_facts >= limits.max_products);
        assert!(limits.max_evidence_per_item >= 1);
        assert!(limits.max_short_text_chars < limits.max_long_text_chars);
    }
}
