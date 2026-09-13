//! Every rule the product role's validator enforces, stated as a test.
//!
//! These live outside `src/validate.rs` on purpose: they exercise the crate's public
//! surface (`validate_response`) exactly as the worker calls it, and keeping them here
//! keeps the module itself readable. What is checked is the phase's central promise —
//! a candidate reaches storage only if a real page of this run really says so.

use otdel_core::extraction::{PageStatus, TextSource};
use otdel_core::knowledge::QuestionAudience;
use otdel_knowledge::validate::validate_response;
use otdel_knowledge::{CandidateDraft, DraftLimits, DraftResponse, SourceCatalog, SourcePage};
use serde_json::{json, Value};
use uuid::Uuid;

const PAGE_ONE: &str = "BASIS mounting systems\nProfile load table\nBP21 1200 3.5 kN при опирании на две опоры\nКонсоль — опорный элемент крепления.";

fn catalog() -> SourceCatalog {
    SourceCatalog::build(vec![SourcePage {
        page_id: Uuid::from_u128(7),
        material_id: Uuid::from_u128(70),
        material_filename: "catalogue.pdf".to_owned(),
        page_number: 3,
        status: PageStatus::Extracted,
        text_source: TextSource::TextLayer,
        text: PAGE_ONE.to_owned(),
    }])
}

fn validate(value: Value) -> CandidateDraft {
    let response = DraftResponse::parse(&value).expect("fixture must match the schema");
    validate_response(&response, &catalog(), &DraftLimits::default(), "b1")
}

fn base_product() -> Value {
    json!({"ref": "p1", "category_ref": null, "kind": "product", "name": "BP21", "summary": null})
}

fn fact(extra: Value) -> Value {
    let mut base = json!({
        "product_ref": "p1",
        "kind": "characteristic",
        "attribute": "нагрузка",
        "value": "3.5",
        "unit": "kN",
        "conditions": "при опирании на две опоры",
        "model_context": null,
        "evidence": [{"source": "S1", "quote": "BP21 1200 3.5 kN при опирании на две опоры"}],
    });
    if let (Value::Object(base), Value::Object(extra)) = (&mut base, extra) {
        for (key, value) in extra {
            base.insert(key, value);
        }
    }
    base
}

#[test]
fn a_well_sourced_fact_is_accepted_with_the_pages_own_wording() {
    let draft = validate(json!({"products": [base_product()], "facts": [fact(json!({}))]}));

    assert_eq!(draft.rejected, 0, "{:?}", draft.rejections);
    assert_eq!(draft.facts.len(), 1);
    let stored = &draft.facts[0];
    assert_eq!(stored.attribute, "нагрузка");
    assert_eq!(stored.value_text, "3.5");
    assert_eq!(stored.unit.as_deref(), Some("kN"));
    assert_eq!(
        stored.conditions.as_deref(),
        Some("при опирании на две опоры")
    );
    assert_eq!(stored.evidence.len(), 1);
    assert_eq!(stored.evidence[0].page_number, 3);
    assert_eq!(stored.evidence[0].page_id, Uuid::from_u128(7));
    assert_eq!(
        stored.evidence[0].quote,
        "BP21 1200 3.5 kN при опирании на две опоры"
    );
    assert_eq!(stored.product_ref.as_deref(), Some("b1:p:p1"));
}

#[test]
fn a_fact_citing_a_source_outside_this_run_is_refused() {
    let draft = validate(json!({
        "products": [base_product()],
        "facts": [fact(json!({
            "evidence": [{"source": "S9", "quote": "BP21 1200 3.5 kN при опирании на две опоры"}],
        }))],
    }));

    assert!(
        draft.facts.is_empty(),
        "a spoofed source must store nothing"
    );
    assert_eq!(draft.rejected, 1);
    assert!(draft
        .rejections
        .iter()
        .any(|reason| reason.contains("S9") && reason.contains("не входит")));
}

#[test]
fn a_fact_with_an_invented_quote_is_refused() {
    let draft = validate(json!({
        "products": [base_product()],
        "facts": [fact(json!({
            "value": "10",
            "evidence": [{"source": "S1", "quote": "BP21 выдерживает 10 kN в любых условиях"}],
        }))],
    }));

    assert!(draft.facts.is_empty());
    assert_eq!(draft.rejected, 1);
    assert!(draft
        .rejections
        .iter()
        .any(|reason| reason.contains("дословно")));
}

#[test]
fn a_fact_with_no_evidence_at_all_is_refused() {
    let draft = validate(json!({
        "products": [base_product()],
        "facts": [fact(json!({"evidence": []}))],
    }));
    assert!(draft.facts.is_empty());
    assert_eq!(draft.rejected, 1);
    assert!(draft
        .rejections
        .iter()
        .any(|reason| reason.contains("нет ни одной подтверждённой цитаты")));
}

#[test]
fn a_value_that_is_not_in_the_quotation_is_refused() {
    // The citation is real and comes from the right page — but it says 3.5, and the
    // fact says 10. Exactly the case a citation exists to prevent.
    let draft = validate(json!({
        "products": [base_product()],
        "facts": [fact(json!({
            "value": "10",
            "unit": null,
            "conditions": null,
            "evidence": [{"source": "S1", "quote": "BP21 1200 3.5 kN при опирании на две опоры"}],
        }))],
    }));

    assert!(draft.facts.is_empty(), "{:?}", draft.facts);
    assert_eq!(draft.rejected, 1);
    assert!(draft
        .rejections
        .iter()
        .any(|reason| reason.contains("значение") && reason.contains("не найдено")));
}

#[test]
fn a_value_is_matched_as_a_whole_token_not_as_a_substring() {
    // "5" occurs inside "1500" on the page; that must not confirm a load of 5.
    let draft = validate(json!({
        "products": [base_product()],
        "facts": [fact(json!({
            "value": "5",
            "unit": null,
            "conditions": null,
            "evidence": [{"source": "S1", "quote": "BP21 1200 3.5 kN при опирании"}],
        }))],
    }));
    assert!(draft.facts.is_empty());

    // The real value in the same quotation is accepted.
    let draft = validate(json!({
        "products": [base_product()],
        "facts": [fact(json!({
            "value": "3.5",
            "unit": null,
            "conditions": null,
            "evidence": [{"source": "S1", "quote": "BP21 1200 3.5 kN при опирании"}],
        }))],
    }));
    assert_eq!(draft.facts.len(), 1);
    assert_eq!(draft.facts[0].value_text, "3.5");
}

#[test]
fn a_citation_that_does_not_contain_the_value_is_not_kept_under_the_fact() {
    // Found on real data: the model attached two genuine quotations to one
    // certificate fact, and only one of them named that certificate. Both were
    // stored, so the owner opening the fact saw a citation that did not support it.
    let draft = validate(json!({
        "products": [base_product()],
        "facts": [fact(json!({
            "value": "3.5",
            "unit": null,
            "conditions": null,
            "evidence": [
                {"source": "S1", "quote": "BP21 1200 3.5 kN при опирании"},
                {"source": "S1", "quote": "Консоль — опорный элемент крепления."},
            ],
        }))],
    }));

    assert_eq!(draft.facts.len(), 1);
    assert_eq!(
        draft.facts[0].evidence.len(),
        1,
        "only the quotation that carries the value stays"
    );
    assert!(draft.facts[0].evidence[0].quote.contains("3.5"));
    assert!(draft
        .rejections
        .iter()
        .any(|reason| reason.contains("отброшены цитаты")));
}

#[test]
fn a_term_needs_a_citation_that_actually_contains_it() {
    let draft = validate(json!({
        "glossary": [{
            "term": "консоль",
            "definition": "опорный элемент крепления",
            "definition_from_source": false,
            // A real fragment of the page — about something else entirely.
            "evidence": [{"source": "S1", "quote": "BP21 1200 3.5 kN при опирании"}],
        }],
    }));

    assert!(draft.terms.is_empty());
    assert!(draft
        .rejections
        .iter()
        .any(|reason| reason.contains("действительно встречается")));
}

#[test]
fn a_unit_cannot_confirm_itself_through_the_models_own_value() {
    // The model writes the unit into the value and into the unit field; the page says
    // kN. Repeating a claim is not evidence for it.
    let draft = validate(json!({
        "products": [base_product()],
        "facts": [fact(json!({
            "value": "3.5",
            "unit": "кгс",
            "conditions": null,
            "evidence": [{"source": "S1", "quote": "BP21 1200 3.5 kN при опирании"}],
        }))],
    }));

    assert_eq!(draft.facts.len(), 1);
    assert_eq!(
        draft.facts[0].unit, None,
        "an unconfirmed unit is not stored"
    );
    assert!(draft
        .rejections
        .iter()
        .any(|reason| reason.contains("без единицы")));
}

#[test]
fn a_unit_longer_than_the_column_allows_is_dropped_rather_than_failing_the_insert() {
    let long_unit = "килоньютон на квадратный метр несущей поверхности профиля";
    assert!(long_unit.chars().count() > 40);

    let draft = validate(json!({
        "products": [base_product()],
        "facts": [fact(json!({
            "value": "3.5",
            "unit": long_unit,
            "conditions": null,
            "evidence": [{"source": "S1", "quote": "BP21 1200 3.5 kN при опирании"}],
        }))],
    }));

    // The fact survives; the unit does not reach a column that would reject it.
    assert_eq!(draft.facts.len(), 1);
    assert!(draft.facts[0]
        .unit
        .as_ref()
        .is_none_or(|unit| unit.chars().count() <= 40));
}

#[test]
fn a_quotation_whose_source_span_is_too_long_is_refused() {
    // A column-padded line: short once whitespace is collapsed, enormous in the page
    // text. Storing it would violate the database's own 600-character limit and abort
    // the whole draft, so it is refused here with a reason.
    let padded = format!("BP21{}3.5 kN", " ".repeat(700));
    let catalog = SourceCatalog::build(vec![SourcePage {
        page_id: Uuid::from_u128(7),
        material_id: Uuid::from_u128(70),
        material_filename: "catalogue.pdf".to_owned(),
        page_number: 3,
        status: PageStatus::Extracted,
        text_source: TextSource::TextLayer,
        text: padded,
    }]);

    let response = DraftResponse::parse(&json!({
        "products": [base_product()],
        "facts": [fact(json!({
            "value": "3.5",
            "unit": null,
            "conditions": null,
            "evidence": [{"source": "S1", "quote": "BP21 3.5 kN"}],
        }))],
    }))
    .unwrap();
    let draft = validate_response(&response, &catalog, &DraftLimits::default(), "b1");

    assert!(draft.facts.is_empty());
    assert!(draft
        .rejections
        .iter()
        .any(|reason| reason.contains("длиннее допустимой")));
}

#[test]
fn an_unconfirmed_unit_is_dropped_and_the_value_is_kept_as_written() {
    let draft = validate(json!({
        "products": [base_product()],
        "facts": [fact(json!({
            "unit": "кгс",
            "evidence": [{"source": "S1", "quote": "BP21 1200 3.5 kN при опирании на две опоры"}],
        }))],
    }));

    assert_eq!(draft.facts.len(), 1);
    assert_eq!(draft.facts[0].unit, None);
    assert_eq!(draft.facts[0].value_text, "3.5");
    assert!(draft
        .rejections
        .iter()
        .any(|reason| reason.contains("без единицы")));
}

#[test]
fn invented_conditions_become_model_context_instead_of_a_condition() {
    let draft = validate(json!({
        "products": [base_product()],
        "facts": [fact(json!({"conditions": "при температуре до 80 °C"}))],
    }));

    assert_eq!(draft.facts.len(), 1);
    let stored = &draft.facts[0];
    assert_eq!(
        stored.conditions, None,
        "an unconfirmed condition is not a condition"
    );
    let context = stored.model_context.as_deref().unwrap();
    assert!(context.contains("80 °C"));
    assert!(context.contains("формулировке модели"));
}

#[test]
fn a_fact_may_name_its_product_instead_of_using_the_reference() {
    // What gpt-4o-mini actually did against the real BASIS catalogue: it declared
    // products with `ref` and then wrote the product's *name* in `product_ref`.
    // The subject is still one of this response's own products, so the fact stands.
    let draft = validate(json!({
        "products": [base_product()],
        "facts": [fact(json!({"product_ref": "BP21"}))],
    }));

    assert_eq!(draft.facts.len(), 1, "{:?}", draft.rejections);
    assert_eq!(draft.facts[0].product_ref.as_deref(), Some("b1:p:p1"));

    // Case and spacing do not matter; a different product does.
    let draft = validate(json!({
        "products": [base_product()],
        "facts": [fact(json!({"product_ref": "  bp21 "}))],
    }));
    assert_eq!(draft.facts.len(), 1);
    assert_eq!(draft.facts[0].product_ref.as_deref(), Some("b1:p:p1"));
}

#[test]
fn an_ambiguous_product_name_attaches_the_fact_to_nothing() {
    // Two products share a name: picking one would attach the property to the wrong
    // product, so the fact is refused instead.
    let twin = json!({
        "ref": "p2", "category_ref": null, "kind": "service",
        "name": "BP21", "summary": null,
    });
    let draft = validate(json!({
        "products": [base_product(), twin],
        "facts": [fact(json!({"product_ref": "BP21"}))],
    }));

    assert!(draft.facts.is_empty());
    assert!(draft
        .rejections
        .iter()
        .any(|reason| reason.contains("не описано в ответе")));
}

#[test]
fn a_product_may_name_its_category_instead_of_using_the_reference() {
    let draft = validate(json!({
        "categories": [{
            "ref": "c1", "kind": "direction",
            "name": "Монтажные системы", "summary": null,
        }],
        "products": [{
            "ref": "p1", "category_ref": "Монтажные системы", "kind": "product",
            "name": "BP21", "summary": null,
        }],
    }));

    assert_eq!(draft.products.len(), 1);
    assert_eq!(draft.products[0].category_ref.as_deref(), Some("b1:c:c1"));
}

#[test]
fn a_fact_about_a_product_that_was_never_described_is_refused() {
    let draft = validate(json!({
        "products": [base_product()],
        "facts": [fact(json!({"product_ref": "p404"}))],
    }));
    assert!(draft.facts.is_empty());
    assert!(draft
        .rejections
        .iter()
        .any(|reason| reason.contains("не описано в ответе")));
}

#[test]
fn the_same_fact_twice_is_stored_once() {
    let draft = validate(json!({
        "products": [base_product()],
        "facts": [fact(json!({})), fact(json!({}))],
    }));
    assert_eq!(draft.facts.len(), 1);
    assert_eq!(draft.rejected, 1);
    assert!(draft
        .rejections
        .iter()
        .any(|reason| reason.contains("повторяет")));
}

#[test]
fn a_term_whose_definition_is_the_models_own_is_marked_as_such() {
    let draft = validate(json!({
        "glossary": [
            {
                "term": "консоль",
                "definition": "Консоль — опорный элемент крепления.",
                "definition_from_source": true,
                "evidence": [{"source": "S1", "quote": "Консоль — опорный элемент крепления."}],
            },
            {
                // The quotation really contains the term; the *definition* is the
                // model's own wording, and that is what gets marked.
                "term": "опоры",
                "definition": "элементы, на которые опирается профиль",
                "definition_from_source": true,
                "evidence": [{
                    "source": "S1",
                    "quote": "BP21 1200 3.5 kN при опирании на две опоры",
                }],
            },
        ],
    }));

    assert_eq!(draft.terms.len(), 2);
    assert!(!draft.terms[0].definition_is_model_context);
    assert!(
        draft.terms[1].definition_is_model_context,
        "a definition that is not in the source must not be presented as quoted"
    );
}

#[test]
fn a_gap_needs_no_quote_but_its_question_needs_an_addressee() {
    let draft = validate(json!({
        "gaps": [
            {
                "product_ref": null, "topic": "price", "missing": "цена не указана",
                "blocks": "коммерческое предложение",
                "question": "Какая отпускная цена профиля BP21?", "audience": "partner",
            },
            {
                "product_ref": null, "topic": "lead_time", "missing": "срок поставки не указан",
                "blocks": null, "question": "Какой срок поставки?", "audience": null,
            },
        ],
    }));

    assert_eq!(draft.gaps.len(), 2, "both gaps are real absences");
    assert_eq!(
        draft.gaps[0].question.as_ref().unwrap().audience,
        QuestionAudience::Partner
    );
    assert!(draft.gaps[1].question.is_none());
    assert!(draft
        .rejections
        .iter()
        .any(|reason| reason.contains("кому он адресован")));
}

#[test]
fn an_answer_without_evidence_is_refused() {
    let draft = validate(json!({
        "qa": [{"question": "Какая нагрузка у BP21?", "answer": "3.5 kN", "evidence": []}],
    }));
    assert!(draft.qa.is_empty());
    assert_eq!(draft.rejected, 1);
}

#[test]
fn evidence_beyond_the_per_item_limit_is_ignored_not_stored() {
    let many: Vec<Value> = (0..10)
        .map(|_| json!({"source": "S1", "quote": "BP21 1200 3.5 kN при опирании"}))
        .collect();
    let draft = validate(json!({
        "products": [base_product()],
        "facts": [fact(json!({"evidence": many}))],
    }));
    assert_eq!(draft.facts.len(), 1);
    // Identical fragments collapse to one; the limit caps what is even considered.
    assert_eq!(draft.facts[0].evidence.len(), 1);
}

#[test]
fn oversized_collections_are_capped_and_the_cap_is_reported() {
    let limits = DraftLimits {
        max_facts: 1,
        ..DraftLimits::default()
    };
    let value = json!({
        "products": [base_product()],
        "facts": [
            fact(json!({})),
            fact(json!({"attribute": "длина", "value": "1200", "unit": null,
                        "conditions": null,
                        "evidence": [{"source": "S1", "quote": "BP21 1200 3.5 kN"}]})),
        ],
    });
    let response = DraftResponse::parse(&value).unwrap();
    let draft = validate_response(&response, &catalog(), &limits, "b1");

    assert_eq!(draft.facts.len(), 1);
    assert!(draft
        .rejections
        .iter()
        .any(|reason| reason.contains("не более 1 фактов")));
}

#[test]
fn nothing_is_accepted_when_the_catalogue_is_empty() {
    let response = DraftResponse::parse(&json!({
        "products": [base_product()],
        "facts": [fact(json!({}))],
    }))
    .unwrap();
    let draft = validate_response(
        &response,
        &SourceCatalog::default(),
        &DraftLimits::default(),
        "b1",
    );
    assert!(draft.facts.is_empty());
    assert_eq!(draft.products.len(), 1, "the product itself is still named");
}
