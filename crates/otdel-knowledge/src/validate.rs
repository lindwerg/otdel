//! Turning a model's answer into candidates that carry a real source — or into a
//! counted, explained refusal.
//!
//! Every rule here exists because of a specific way a draft could otherwise be wrong:
//!
//! | Rule | What it prevents |
//! |---|---|
//! | the cited label must resolve in this run's catalogue | a fact attributed to another material, partner or bureau |
//! | the quote must be found literally in that page | an invented, translated or rounded "quote" |
//! | the stored quote is the page's wording, not the model's | a citation that drifts from the document |
//! | a fact with no accepted evidence is refused | a claim nobody can check |
//! | **the value must appear in the quotation** | "нагрузка 10 kN" under a citation that reads 3.5 kN |
//! | a unit must appear in a quotation (never in the model's own value) | `3.5` silently becoming `3.5 kN`, or `кгс` vouching for itself |
//! | conditions must appear in a quote, or they become model context | an invented "при опирании на две опоры" |
//! | an unknown product reference refuses the fact | a property attached to the wrong product |
//!
//! The *attribute* is deliberately not required to be quoted: it is the model's name
//! for the property ("нагрузка" for a column headed `Load (kN)`), and demanding it
//! literally would refuse almost every real table. What it labels — the value, the
//! unit, the conditions — is what must be in the document, and the quotation is shown
//! next to it so the labelling itself can be judged.
//!
//! Refusals are never silent: each one increments a counter and adds a sentence the
//! interface shows next to the run.

use otdel_core::knowledge::{CategoryKind, FactKind, ProductKind, QuestionAudience};

use crate::candidate::{
    normalise_name, CandidateCategory, CandidateDraft, CandidateFact, CandidateGap,
    CandidateProduct, CandidateQa, CandidateQuestion, CandidateTerm, ResolvedEvidence,
};
use crate::quote;
use crate::schema::{DraftEvidence, DraftLimits, DraftResponse};
use crate::source::SourceCatalog;

/// Longest unit the database accepts (`0004_knowledge.sql`: `char_length(unit) <= 40`).
///
/// Clipping to the shared "short text" limit instead would let a 200-character unit
/// through validation and abort the whole draft at INSERT — a transaction failure the
/// queue would then retry, paying for the model call again each time.
const MAX_UNIT_CHARS: usize = 40;

/// Validate one response against the sources it was built from.
///
/// `prefix` namespaces the response-local references (`b1`, `b2`, …) so several
/// requests of one run can be merged without their `p1`s colliding.
pub fn validate_response(
    response: &DraftResponse,
    catalog: &SourceCatalog,
    limits: &DraftLimits,
    prefix: &str,
) -> CandidateDraft {
    let mut draft = CandidateDraft::default();

    validate_categories(response, limits, prefix, &mut draft);
    validate_products(response, limits, prefix, &mut draft);
    validate_facts(response, catalog, limits, prefix, &mut draft);
    validate_terms(response, catalog, limits, &mut draft);
    validate_qa(response, catalog, limits, &mut draft);
    validate_gaps(response, limits, prefix, &mut draft);

    draft
}

fn validate_categories(
    response: &DraftResponse,
    limits: &DraftLimits,
    prefix: &str,
    draft: &mut CandidateDraft,
) {
    for (index, category) in response.categories.iter().enumerate() {
        if index >= limits.max_categories {
            draft.reject(format!(
                "принято не более {} направлений, остальные отброшены",
                limits.max_categories
            ));
            break;
        }
        let Some(kind) = CategoryKind::parse(category.kind.trim()) else {
            draft.reject(format!(
                "направление «{}» отклонено: неизвестный вид «{}»",
                short(&category.name),
                short(&category.kind)
            ));
            continue;
        };
        let Some(name) = text(&category.name, limits.max_short_text_chars) else {
            draft.reject("направление отклонено: пустое или слишком длинное название");
            continue;
        };
        draft.categories.push(CandidateCategory {
            reference: category_ref(prefix, &category.reference),
            kind,
            name,
            summary: optional_text(category.summary.as_deref(), limits.max_long_text_chars),
        });
    }
}

fn validate_products(
    response: &DraftResponse,
    limits: &DraftLimits,
    prefix: &str,
    draft: &mut CandidateDraft,
) {
    for (index, product) in response.products.iter().enumerate() {
        if index >= limits.max_products {
            draft.reject(format!(
                "принято не более {} изделий, остальные отброшены",
                limits.max_products
            ));
            break;
        }
        let Some(kind) = ProductKind::parse(product.kind.trim()) else {
            draft.reject(format!(
                "изделие «{}» отклонено: неизвестный вид «{}»",
                short(&product.name),
                short(&product.kind)
            ));
            continue;
        };
        let Some(name) = text(&product.name, limits.max_short_text_chars) else {
            draft.reject("изделие отклонено: пустое или слишком длинное название");
            continue;
        };

        // A category reference that points at nothing loses the link, but the product
        // itself is still real: it was named in the material.
        let category_ref = match product.category_ref.as_deref().map(str::trim) {
            Some(reference) if !reference.is_empty() => {
                match resolve_category(draft, prefix, reference) {
                    Some(found) => Some(found),
                    None => {
                        draft.note(format!(
                            "изделие «{}» сохранено без направления: ссылка «{}» не найдена",
                            short(&name),
                            short(reference)
                        ));
                        None
                    }
                }
            }
            _ => None,
        };

        draft.products.push(CandidateProduct {
            reference: product_ref(prefix, &product.reference),
            category_ref,
            kind,
            name,
            summary: optional_text(product.summary.as_deref(), limits.max_long_text_chars),
        });
    }
}

fn validate_facts(
    response: &DraftResponse,
    catalog: &SourceCatalog,
    limits: &DraftLimits,
    prefix: &str,
    draft: &mut CandidateDraft,
) {
    let mut accepted = 0usize;
    let mut seen: Vec<String> = Vec::new();

    for fact in &response.facts {
        if accepted >= limits.max_facts {
            draft.reject(format!(
                "принято не более {} фактов, остальные отброшены",
                limits.max_facts
            ));
            break;
        }

        let label = short(&fact.attribute);
        let Some(kind) = FactKind::parse(fact.kind.trim()) else {
            draft.reject(format!(
                "факт «{label}» отклонён: неизвестный вид «{}»",
                short(&fact.kind)
            ));
            continue;
        };
        let Some(attribute) = text(&fact.attribute, limits.max_short_text_chars) else {
            draft.reject("факт отклонён: не указано свойство");
            continue;
        };
        let Some(value_text) = text(&fact.value, limits.max_short_text_chars) else {
            draft.reject(format!("факт «{label}» отклонён: не указано значение"));
            continue;
        };

        // The subject must be one the model itself introduced in this response —
        // addressed either by its draft-local `ref` or by its own name.
        let product_ref = match fact.product_ref.as_deref().map(str::trim) {
            Some(reference) if !reference.is_empty() => {
                let Some(found) = resolve_product(draft, prefix, reference) else {
                    draft.reject(format!(
                        "факт «{label}» отклонён: изделие «{}» не описано в ответе",
                        short(reference)
                    ));
                    continue;
                };
                Some(found)
            }
            _ => None,
        };

        let resolved = resolve_evidence(
            &fact.evidence,
            catalog,
            limits,
            &format!("факт «{label}»"),
            draft,
        );
        if resolved.is_empty() {
            draft.reject(format!(
                "факт «{label}» отклонён: нет ни одной подтверждённой цитаты из этого материала"
            ));
            continue;
        }

        // The value itself must be in the quotation. This is the rule the whole phase
        // rests on: a quote that merely comes from the right page proves nothing about
        // the number written next to it, and "нагрузка 10 kN" under a citation reading
        // "3.5 kN" is exactly the failure a citation is supposed to make impossible.
        //
        // Every *kept* citation has to contain the value, not just one of them. A real
        // run produced a certificate fact with two genuine quotations, only one of
        // which mentioned that certificate — the other named a different one. Both
        // were stored, so the owner opening the fact saw a citation that did not
        // support what it sat under. Rounding, converting or restating a value refuses
        // the fact outright; an extra quotation that supports nothing is dropped.
        let dropped = resolved.len();
        let evidence: Vec<ResolvedEvidence> = resolved
            .into_iter()
            .filter(|item| quote::contains_token(&item.quote, &value_text))
            .collect();
        if evidence.is_empty() {
            draft.reject(format!(
                "факт «{label}» отклонён: значение «{}» не найдено дословно в цитате",
                short(&value_text)
            ));
            continue;
        }
        if dropped > evidence.len() {
            draft.note(format!(
                "у факта «{label}» отброшены цитаты, в которых нет значения «{}»: \
                 под фактом остаются только подтверждающие его фрагменты",
                short(&value_text)
            ));
        }

        // A duplicate is not a second observation, it is the same one twice.
        let fingerprint = format!(
            "{}|{}|{}",
            product_ref.clone().unwrap_or_default(),
            normalise_name(&attribute),
            normalise_name(&value_text)
        );
        if seen.contains(&fingerprint) {
            draft.reject(format!(
                "факт «{label}» отклонён: повторяет уже принятый факт"
            ));
            continue;
        }
        seen.push(fingerprint);

        let mut model_context =
            optional_text(fact.model_context.as_deref(), limits.max_long_text_chars);

        // A unit is only real if the *source* writes it. It is checked against the
        // quotations alone — never against the model's own `value`, which would let
        // the model confirm its own unit by repeating it.
        let unit = match optional_text(fact.unit.as_deref(), MAX_UNIT_CHARS) {
            Some(unit) if unit_is_quoted(&unit, &evidence) => Some(unit),
            Some(unit) => {
                draft.note(format!(
                    "у факта «{label}» единица «{}» не найдена в цитате — сохранено без единицы",
                    short(&unit)
                ));
                None
            }
            None => None,
        };

        // Conditions decide when a number is true; an invented condition is as bad as
        // an invented number. Unconfirmed text is kept, but as the model's words.
        let conditions = match optional_text(fact.conditions.as_deref(), limits.max_long_text_chars)
        {
            Some(conditions) if quoted_anywhere(&conditions, &evidence) => Some(conditions),
            Some(conditions) => {
                draft.note(format!(
                    "у факта «{label}» условия не найдены дословно в цитате — перенесены в \
                     пояснение модели"
                ));
                model_context = Some(match model_context {
                    Some(existing) => {
                        format!("{existing} Условия по формулировке модели: {conditions}")
                    }
                    None => format!("Условия по формулировке модели: {conditions}"),
                });
                None
            }
            None => None,
        };

        accepted += 1;
        draft.facts.push(CandidateFact {
            product_ref,
            kind,
            attribute,
            value_text,
            unit,
            conditions,
            model_context: model_context.map(|context| clip(&context, limits.max_long_text_chars)),
            evidence,
        });
    }
}

fn validate_terms(
    response: &DraftResponse,
    catalog: &SourceCatalog,
    limits: &DraftLimits,
    draft: &mut CandidateDraft,
) {
    for (index, term) in response.glossary.iter().enumerate() {
        if index >= limits.max_terms {
            draft.reject(format!(
                "принято не более {} терминов, остальные отброшены",
                limits.max_terms
            ));
            break;
        }
        let Some(name) = text(&term.term, limits.max_short_text_chars) else {
            draft.reject("термин отклонён: пустое название");
            continue;
        };
        let Some(definition) = text(&term.definition, limits.max_long_text_chars) else {
            draft.reject(format!(
                "термин «{}» отклонён: пустое определение",
                short(&name)
            ));
            continue;
        };

        let resolved = resolve_evidence(
            &term.evidence,
            catalog,
            limits,
            &format!("термин «{}»", short(&name)),
            draft,
        );
        // A quotation under a term has one job: to show the term being used in the
        // material. One that does not contain it shows something else.
        let evidence: Vec<ResolvedEvidence> = resolved
            .into_iter()
            .filter(|item| quote::contains_token(&item.quote, &name))
            .collect();
        if evidence.is_empty() {
            draft.reject(format!(
                "термин «{}» отклонён: нет цитаты, в которой он действительно встречается",
                short(&name)
            ));
            continue;
        }

        // "From the source" is only believable when the definition really is there.
        let from_source = term.definition_from_source && quoted_anywhere(&definition, &evidence);
        if term.definition_from_source && !from_source {
            draft.note(format!(
                "определение термина «{}» помечено как формулировка модели: дословно в \
                 источнике оно не найдено",
                short(&name)
            ));
        }

        draft.terms.push(CandidateTerm {
            term: name,
            definition,
            definition_is_model_context: !from_source,
            evidence,
        });
    }
}

fn validate_qa(
    response: &DraftResponse,
    catalog: &SourceCatalog,
    limits: &DraftLimits,
    draft: &mut CandidateDraft,
) {
    for (index, entry) in response.qa.iter().enumerate() {
        if index >= limits.max_qa {
            draft.reject(format!(
                "принято не более {} пар вопрос-ответ, остальные отброшены",
                limits.max_qa
            ));
            break;
        }
        let Some(question) = text(&entry.question, limits.max_long_text_chars) else {
            draft.reject("вопрос-ответ отклонён: пустой вопрос");
            continue;
        };
        let Some(answer) = text(&entry.answer, limits.max_long_text_chars) else {
            draft.reject(format!(
                "вопрос-ответ «{}» отклонён: пустой ответ",
                short(&question)
            ));
            continue;
        };

        let evidence = resolve_evidence(
            &entry.evidence,
            catalog,
            limits,
            &format!("ответ на «{}»", short(&question)),
            draft,
        );
        if evidence.is_empty() {
            draft.reject(format!(
                "ответ на «{}» отклонён: нет подтверждённой цитаты из этого материала",
                short(&question)
            ));
            continue;
        }

        // An answer is a synthesis: even when it repeats the source, it is the model's
        // sentence, not the document's. It is stored and shown as such, next to the
        // fragment it was built from — the same discipline as a glossary definition,
        // which is demoted to "the model's wording" unless it is literally in the page.
        let answer_is_model_context = !quoted_anywhere(&answer, &evidence);

        draft.qa.push(CandidateQa {
            question,
            answer,
            answer_is_model_context,
            evidence,
        });
    }
}

fn validate_gaps(
    response: &DraftResponse,
    limits: &DraftLimits,
    prefix: &str,
    draft: &mut CandidateDraft,
) {
    for (index, gap) in response.gaps.iter().enumerate() {
        if index >= limits.max_gaps {
            draft.reject(format!(
                "принято не более {} пробелов, остальные отброшены",
                limits.max_gaps
            ));
            break;
        }
        let Some(topic) = text(&gap.topic, limits.max_short_text_chars) else {
            draft.reject("пробел отклонён: не указана тема");
            continue;
        };
        let Some(missing) = text(&gap.missing, limits.max_long_text_chars) else {
            draft.reject(format!(
                "пробел «{}» отклонён: не сказано, чего не хватает",
                short(&topic)
            ));
            continue;
        };

        let product_ref = match gap.product_ref.as_deref().map(str::trim) {
            Some(reference) if !reference.is_empty() => {
                match resolve_product(draft, prefix, reference) {
                    Some(found) => Some(found),
                    None => {
                        draft.note(format!(
                            "пробел «{}» сохранён без привязки к изделию: ссылка не найдена",
                            short(&topic)
                        ));
                        None
                    }
                }
            }
            _ => None,
        };

        // A question without a stated addressee cannot be routed: 1D researches the
        // industry ones, the partner ones wait for a channel. Guessing would send the
        // wrong question to the wrong place.
        let question = match (
            optional_text(gap.question.as_deref(), limits.max_long_text_chars),
            gap.audience.as_deref().map(str::trim),
        ) {
            (Some(text), Some(audience)) => match QuestionAudience::parse(audience) {
                Some(audience) => Some(CandidateQuestion { audience, text }),
                None => {
                    draft.note(format!(
                        "вопрос по пробелу «{}» не сохранён: неизвестный адресат «{}»",
                        short(&topic),
                        short(audience)
                    ));
                    None
                }
            },
            (Some(_), None) => {
                draft.note(format!(
                    "вопрос по пробелу «{}» не сохранён: не указано, кому он адресован",
                    short(&topic)
                ));
                None
            }
            _ => None,
        };

        draft.gaps.push(CandidateGap {
            product_ref,
            topic,
            missing,
            blocks: optional_text(gap.blocks.as_deref(), limits.max_long_text_chars),
            question,
        });
    }
}

/// Resolve the cited fragments, dropping every one that cannot be proven.
fn resolve_evidence(
    cited: &[DraftEvidence],
    catalog: &SourceCatalog,
    limits: &DraftLimits,
    subject: &str,
    draft: &mut CandidateDraft,
) -> Vec<ResolvedEvidence> {
    let mut resolved: Vec<ResolvedEvidence> = Vec::new();

    for evidence in cited.iter().take(limits.max_evidence_per_item) {
        let Some(entry) = catalog.resolve(&evidence.source) else {
            draft.note(format!(
                "{subject}: источник «{}» не входит в этот материал",
                short(&evidence.source)
            ));
            continue;
        };

        match entry.locate(&evidence.quote) {
            Ok(found) => {
                let char_start = i32::try_from(found.char_start).unwrap_or(i32::MAX);
                let already = resolved.iter().any(|kept| {
                    kept.page_id == entry.page.page_id && kept.char_start == char_start
                });
                if already {
                    continue;
                }
                resolved.push(ResolvedEvidence {
                    page_id: entry.page.page_id,
                    material_id: entry.page.material_id,
                    page_number: entry.page.page_number,
                    quote: found.text,
                    char_start,
                    char_end: i32::try_from(found.char_end).unwrap_or(i32::MAX),
                });
            }
            Err(rejection) => draft.note(format!(
                "{subject}: {} (страница {})",
                rejection.reason(),
                entry.page.page_number
            )),
        }
    }

    resolved
}

/// Is the unit literally written in one of the accepted quotations?
///
/// Only the quotations count. Checking the model's own `value` would let a model
/// confirm its unit by writing it twice — "3.5 кгс" would vouch for "кгс" even though
/// the document says kN.
fn unit_is_quoted(unit: &str, evidence: &[ResolvedEvidence]) -> bool {
    quoted_anywhere(unit, evidence)
}

/// Is this text present, as whole tokens, in one of the accepted quotations?
///
/// Token-aware rather than a bare substring test: the quotation is short, and `5`
/// must not be confirmed by the `5` inside `1500`.
fn quoted_anywhere(text: &str, evidence: &[ResolvedEvidence]) -> bool {
    evidence
        .iter()
        .any(|item| quote::contains_token(&item.quote, text))
}

/// Resolve what a fact or a gap says its subject is.
///
/// Two spellings are accepted, and both name something the model itself described in
/// this very response:
///
/// * the draft-local `ref` it declared (`p3`) — the documented convention;
/// * the product's own **name** (`Подвес ВР 41`) — what a model actually tends to
///   write. Observed against the real catalogue: gpt-4o-mini referenced products by
///   name for a whole batch, and refusing those cost ~30 well-sourced facts. Matching
///   the name is no weaker a rule: the subject still has to be one of the products in
///   this response, and every fact still needs its own verbatim quotation.
///
/// An ambiguous name (two products sharing it) resolves to nothing: silently picking
/// one would attach a property to the wrong product.
fn resolve_product(draft: &CandidateDraft, prefix: &str, reference: &str) -> Option<String> {
    let namespaced = product_ref(prefix, reference);
    if draft
        .products
        .iter()
        .any(|product| product.reference == namespaced)
    {
        return Some(namespaced);
    }

    let wanted = normalise_name(reference);
    let mut matches = draft
        .products
        .iter()
        .filter(|product| normalise_name(&product.name) == wanted);
    let first = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(first.reference.clone())
}

/// The same two spellings for a product's category.
fn resolve_category(draft: &CandidateDraft, prefix: &str, reference: &str) -> Option<String> {
    let namespaced = category_ref(prefix, reference);
    if draft
        .categories
        .iter()
        .any(|category| category.reference == namespaced)
    {
        return Some(namespaced);
    }

    let wanted = normalise_name(reference);
    let mut matches = draft
        .categories
        .iter()
        .filter(|category| normalise_name(&category.name) == wanted);
    let first = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(first.reference.clone())
}

/// Draft-local reference of a category, namespaced per request.
///
/// Categories and products have separate namespaces: a model that reuses `x1` for a
/// category and a product in one response would otherwise make a fact about `x1`
/// resolvable to the category, and the fact would silently lose its subject.
fn category_ref(prefix: &str, reference: &str) -> String {
    format!("{prefix}:c:{}", reference.trim())
}

fn product_ref(prefix: &str, reference: &str) -> String {
    format!("{prefix}:p:{}", reference.trim())
}

/// Trim, refuse empty, clip to the limit.
fn text(value: &str, max_chars: usize) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(clip(trimmed, max_chars))
}

fn optional_text(value: Option<&str>, max_chars: usize) -> Option<String> {
    value.and_then(|value| text(value, max_chars))
}

fn clip(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    value.chars().take(max_chars).collect()
}

/// Short, control-free label for a message shown to the owner.
fn short(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(60)
        .collect::<String>()
        .trim()
        .to_owned()
}
