//! The response contract: what the researcher role is allowed to return.
//!
//! Two halves that must agree, exactly as in 1C: [`response_schema`] is the JSON Schema
//! sent to the provider in structured-output mode, and [`FindingsResponse`] is what the
//! server accepts, with `deny_unknown_fields` and with every bound enforced in Rust
//! afterwards ([`crate::validate`]). The schema is the request; the Rust types are the
//! enforcement. A provider that ignores the schema cannot widen what gets stored.
//!
//! One field exists that 1C has no equivalent of: `not_found`. The researcher is allowed
//! — and asked — to say that the sources do not answer the question. That sentence is
//! more useful than a finding assembled out of nothing, and `docs/block-01-spec.md` §13.5
//! requires exactly it: no data means an honest gap, not a market guess.

use serde::Deserialize;
use serde_json::{json, Value};

/// Name sent to the provider alongside the schema.
pub const SCHEMA_NAME: &str = "otdel_industry_findings";

/// Version of the prompt + schema pair, stored on every plan.
///
/// A later pass with a different profile is distinguishable from an older one, which is
/// what `docs/block-01-spec.md` §6.1 asks for when a processing profile changes.
pub const PROMPT_PROFILE: &str = "researcher/2026-09-13.1";

/// Upper bounds on what one response may contain. Anything beyond is refused (and
/// counted), never silently truncated into "success".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FindingLimits {
    pub max_findings: usize,
    pub max_evidence_per_finding: usize,
    /// Topics, attributes, values, units.
    pub max_short_text_chars: usize,
    /// Conditions, model context, the not-found sentence.
    pub max_long_text_chars: usize,
    /// Sources shown to the model in one request.
    pub max_sources_per_request: usize,
    /// Characters of source text put into one request.
    pub max_input_chars: usize,
}

impl Default for FindingLimits {
    fn default() -> Self {
        Self {
            max_findings: 30,
            max_evidence_per_finding: 4,
            max_short_text_chars: 200,
            max_long_text_chars: 1_000,
            max_sources_per_request: 4,
            max_input_chars: 24_000,
        }
    }
}

/// One cited fragment: which source, and the words the model claims are there.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FindingEvidence {
    /// A label from the prompt's source list (`E1`, `E2`, …).
    pub source: String,
    /// The words the model says appear on that page. Checked literally.
    pub quote: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DraftFinding {
    /// Short area label — `покрытие`, `нагрузка`, `сертификация`.
    pub topic: String,
    /// The property being stated.
    pub attribute: String,
    /// The value as written in the source. Not parsed, not converted.
    pub value: String,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub conditions: Option<String>,
    /// Anything the model wants to add in its own words. Stored separately and shown as
    /// not-a-quote.
    #[serde(default)]
    pub model_context: Option<String>,
    #[serde(default)]
    pub evidence: Vec<FindingEvidence>,
}

/// The whole response. A missing array is empty; unknown fields are not accepted.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FindingsResponse {
    #[serde(default)]
    pub findings: Vec<DraftFinding>,
    /// Why the sources do not answer the question, when they do not.
    #[serde(default)]
    pub not_found: Option<String>,
}

impl FindingsResponse {
    /// Parse a provider's JSON object into the response contract.
    pub fn parse(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| {
            // serde's message names the offending field, which is a schema detail rather
            // than source content — useful in the plan record, and safe there.
            format!("ответ модели не соответствует схеме: {error}")
        })
    }
}

fn nullable_string() -> Value {
    json!({"type": ["string", "null"]})
}

/// The JSON Schema sent to the provider.
pub fn response_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["findings", "not_found"],
        "properties": {
            "findings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": [
                        "topic", "attribute", "value", "unit", "conditions",
                        "model_context", "evidence",
                    ],
                    "properties": {
                        "topic": {
                            "type": "string",
                            "description": "Короткая тема вывода: покрытие, нагрузка, сертификация.",
                        },
                        "attribute": {
                            "type": "string",
                            "description": "Название характеристики отрасли, о которой сделан вывод.",
                        },
                        "value": {
                            "type": "string",
                            "description": "Значение дословно из источника. Оно должно встречаться в приложенной цитате, иначе вывод отбрасывается.",
                        },
                        "unit": nullable_string(),
                        "conditions": nullable_string(),
                        "model_context": nullable_string(),
                        "evidence": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "required": ["source", "quote"],
                                "properties": {
                                    "source": {
                                        "type": "string",
                                        "description": "Метка источника из запроса: E1, E2, …",
                                    },
                                    "quote": {
                                        "type": "string",
                                        "description": "Дословный отрывок этого источника, символ в символ.",
                                    },
                                },
                            },
                        },
                    },
                },
            },
            "not_found": {
                "type": ["string", "null"],
                "description": "Если источники не отвечают на вопрос — напиши это здесь одной фразой и оставь findings пустым. Это правильный ответ, а не неудача.",
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
            "findings": [{
                "topic": "покрытие",
                "attribute": "минимальная толщина цинкового покрытия",
                "value": "55",
                "unit": "мкм",
                "conditions": "для изделий толщиной до 1,5 мм",
                "model_context": null,
                "evidence": [{"source": "E1", "quote": "минимальная толщина покрытия 55 мкм"}],
            }],
            "not_found": null,
        });

        let parsed = FindingsResponse::parse(&value).unwrap();
        assert_eq!(parsed.findings.len(), 1);
        assert_eq!(parsed.findings[0].unit.as_deref(), Some("мкм"));
        assert!(parsed.not_found.is_none());
    }

    #[test]
    fn saying_the_sources_do_not_answer_is_a_valid_response() {
        let value = json!({
            "findings": [],
            "not_found": "в найденных источниках это значение не приводится",
        });
        let parsed = FindingsResponse::parse(&value).unwrap();
        assert!(parsed.findings.is_empty());
        assert!(parsed.not_found.is_some());
    }

    #[test]
    fn missing_arrays_are_empty_but_unknown_fields_are_refused() {
        let sparse = FindingsResponse::parse(&json!({"findings": []})).unwrap();
        assert_eq!(sparse, FindingsResponse::default());

        // A verdict this phase is not entitled to reach.
        let error =
            FindingsResponse::parse(&json!({"findings": [], "verdict": "confirmed"})).unwrap_err();
        assert!(error.contains("схеме"), "{error}");

        // A confidence score nobody measured.
        let error = FindingsResponse::parse(&json!({
            "findings": [{
                "topic": "a", "attribute": "b", "value": "c",
                "confidence": 0.9, "evidence": [],
            }],
        }))
        .unwrap_err();
        assert!(error.contains("схеме"), "{error}");

        // A finding that names a partner's product is not representable: there is no
        // such field in the contract at all.
        let error = FindingsResponse::parse(&json!({
            "findings": [{
                "topic": "a", "attribute": "b", "value": "c",
                "product": "BP21", "evidence": [],
            }],
        }))
        .unwrap_err();
        assert!(error.contains("схеме"), "{error}");
    }

    #[test]
    fn a_finding_missing_a_required_field_is_refused() {
        let error =
            FindingsResponse::parse(&json!({"findings": [{"topic": "покрытие"}]})).unwrap_err();
        assert!(error.contains("схеме"), "{error}");
    }

    #[test]
    fn limits_have_sane_defaults() {
        let limits = FindingLimits::default();
        assert!(limits.max_evidence_per_finding >= 1);
        assert!(limits.max_short_text_chars < limits.max_long_text_chars);
        assert!(limits.max_sources_per_request >= 1);
    }
}
