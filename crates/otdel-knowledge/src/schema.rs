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
pub const PROMPT_PROFILE: &str = "productologist/2026-09-13.2";

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
}

impl Default for DraftLimits {
    fn default() -> Self {
        Self {
            max_categories: 20,
            max_products: 60,
            max_facts: 200,
            max_terms: 80,
            max_qa: 40,
            max_gaps: 60,
            max_evidence_per_item: 4,
            // Names, attributes, units, topics.
            max_short_text_chars: 200,
            // Summaries, definitions, answers, conditions, model context.
            max_long_text_chars: 1_000,
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

/// The JSON Schema sent to the provider.
pub fn response_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["categories", "products", "facts", "glossary", "qa", "gaps"],
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
                    "required": ["ref", "category_ref", "kind", "name", "summary"],
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
                    "required": ["term", "definition", "definition_from_source", "evidence"],
                    "properties": {
                        "term": {"type": "string"},
                        "definition": {"type": "string"},
                        "definition_from_source": {"type": "boolean"},
                        "evidence": evidence_schema(),
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
                    },
                },
            },
        },
    })
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
