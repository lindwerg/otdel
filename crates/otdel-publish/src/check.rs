//! The checker: every rule that turns a candidate into a verdict.
//!
//! Nothing here asks a model anything. Each verdict is reached by re-reading the source
//! text that is stored *now* and locating the citation in it, so a person with the
//! database and this file can reproduce every decision — which is what
//! `block-01-spec.md` §6.7 means by Rust checking "схемы, ссылки, область доступа,
//! обязательные условия, состояние источников и правила готовности", and why
//! "совпадение ответов двух моделей не является доказательством" costs this phase
//! nothing: no model was consulted to get here.
//!
//! | Rule | What it prevents | Verdict |
//! |---|---|---|
//! | the cited source must still be readable | publishing a claim whose source vanished, as if it were still checked | `unknown` |
//! | the quotation must still be found in that source, literally | a citation that says what the document used to say | `stale` |
//! | the stored quotation is re-extracted from today's text, at the offsets where it was found | a published citation pointing at the wrong place after a page was re-read | — |
//! | the value must appear in a surviving quotation, as a whole token | `10 kN` published under a citation that reads 3.5 kN; `5` confirmed by the `5` in `1500` | `hypothesis` |
//! | the unit must appear in a surviving quotation | `3,5` quietly becoming `3,5 мм` between drafting and publication | `hypothesis` |
//! | conditions must appear in a surviving quotation | an invented "при опирании на две опоры" surviving into a published version | `hypothesis` |
//! | two supported claims about one product and one property must agree | a confident numeric answer drawn from a document that contradicts another | `conflicted` |
//! | a claim whose every citation failed, or whose text cannot satisfy the column it goes in, is refused outright | an unsourced published claim, and a whole version aborted by one bad field | refused |
//!
//! Refusals and downgrades are never silent: each one adds a sentence the interface
//! shows verbatim, exactly as 1C and 1D do.

use otdel_core::publication::ClaimStatus;
use otdel_knowledge::quote::{self, SearchableText};

use crate::chunk::{clip, normalise, normalised_column};
use crate::claim::{
    CandidateClaim, CandidateEvidence, CheckOutcome, CheckedClaim, CheckedEvidence,
};

/// Longest unit the snapshot column accepts (`0006_publication.sql`).
const MAX_UNIT_CHARS: usize = 40;
/// Longest free-text column of a claim.
const MAX_LONG_TEXT_CHARS: usize = 1_000;
/// Longest short column (attribute, value, product).
const MAX_SHORT_TEXT_CHARS: usize = 200;

/// Check every candidate of one partner and decide what a version would carry.
///
/// The order matters: each claim is judged on its own evidence first, and contradiction
/// is decided afterwards over the claims that survived — a statement cannot contradict
/// one whose source has gone.
pub fn check_claims(candidates: &[CandidateClaim]) -> CheckOutcome {
    let mut outcome = CheckOutcome::default();

    for candidate in candidates {
        if let Some(checked) = check_one(candidate, &mut outcome) {
            outcome.claims.push(checked);
        }
    }

    mark_contradictions(&mut outcome);
    outcome
}

fn check_one(candidate: &CandidateClaim, outcome: &mut CheckOutcome) -> Option<CheckedClaim> {
    let label = label_of(candidate);

    // Every text that goes into a bounded column is folded and bounded *here*, so a
    // field made of control characters refuses one claim instead of aborting the
    // transaction that writes the whole version.
    let attribute = bounded(&candidate.attribute, MAX_SHORT_TEXT_CHARS).or_else(|| {
        outcome.reject(format!(
            "{label}: свойство пустое после очистки — не опубликовано"
        ));
        None
    })?;
    let value_text = bounded(&candidate.value_text, MAX_SHORT_TEXT_CHARS).or_else(|| {
        outcome.reject(format!(
            "{label}: значение пустое после очистки — не опубликовано"
        ));
        None
    })?;
    let product_name = match &candidate.product_name {
        Some(name) => match bounded(name, MAX_SHORT_TEXT_CHARS) {
            Some(name) => Some(name),
            None => {
                outcome.reject(format!(
                    "{label}: название изделия пустое после очистки — не опубликовано"
                ));
                return None;
            }
        },
        None => None,
    };

    // An industry conclusion may not name a product. The database enforces it too; this
    // refuses the row rather than letting the CHECK abort the version.
    if candidate.origin.scope() != otdel_core::publication::ClaimScope::Partner
        && product_name.is_some()
    {
        outcome.reject(format!(
            "{label}: отраслевой вывод не может относиться к изделию партнёра — не опубликовано"
        ));
        return None;
    }

    if candidate.evidence.is_empty() {
        outcome.reject(format!(
            "{label}: нет ни одного источника — не опубликовано"
        ));
        return None;
    }

    // --- locate every citation in the source as it is stored today ---------------
    let mut verified: Vec<CheckedEvidence> = Vec::new();
    let mut unreadable = 0usize;
    let mut lost = 0usize;
    let mut moved = 0usize;

    for evidence in &candidate.evidence {
        match locate(evidence) {
            Located::Verified { checked, moved: m } => {
                if m {
                    moved += 1;
                }
                verified.push(checked);
            }
            Located::Unreadable => unreadable += 1,
            Located::Lost => lost += 1,
        }
    }

    if verified.is_empty() {
        // Two different facts, kept apart. A source that cannot be read tells us
        // nothing; a source that was read and no longer says this tells us a lot.
        let status = if lost == 0 {
            ClaimStatus::Unknown
        } else {
            ClaimStatus::Stale
        };
        let note = if lost == 0 {
            format!(
                "источник недоступен: проверить утверждение сейчас нельзя ({unreadable} \
                 ссылок без текста)"
            )
        } else {
            "источник перечитан и больше не содержит процитированного фрагмента".to_owned()
        };
        outcome.note(format!("{label}: {note}"));
        // The claim is still published, with its verdict and its citations as they were
        // recorded, because hiding it would lose the trail to the document. The
        // citations kept here are the candidate's own, which is the only thing left.
        return Some(CheckedClaim {
            origin: candidate.origin,
            origin_id: candidate.origin_id,
            product_name,
            kind: candidate.kind,
            status,
            attribute,
            value_text,
            unit: candidate
                .unit
                .as_deref()
                .and_then(|unit| bounded(unit, MAX_UNIT_CHARS)),
            conditions: candidate.conditions.as_deref().and_then(bounded_long),
            model_context: candidate
                .model_context
                .as_deref()
                .and_then(bounded_long_ref),
            check_note: Some(clip(&note, MAX_LONG_TEXT_CHARS)),
            evidence: candidate.evidence.iter().map(carry_over).collect(),
        });
    }

    let mut notes: Vec<String> = Vec::new();
    if moved > 0 {
        notes.push(
            "положение цитаты в источнике изменилось — ссылка исправлена при публикации".to_owned(),
        );
    }
    if lost > 0 {
        notes.push(format!(
            "{lost} цитат(ы) больше не найдено в источнике — оставлены только подтверждённые"
        ));
    }
    if unreadable > 0 {
        notes.push(format!("{unreadable} источник(ов) недоступно для проверки"));
    }

    let mut status = ClaimStatus::SourceSupported;

    // --- the value must be in a surviving quotation, as a whole token -------------
    let supporting: Vec<&CheckedEvidence> = verified
        .iter()
        .filter(|item| quote::contains_token(&item.quote, &value_text))
        .collect();
    if supporting.is_empty() {
        status = ClaimStatus::Hypothesis;
        notes.push(format!(
            "значение «{}» не найдено в подтверждённой цитате — утверждение оставлено как \
             гипотеза",
            short(&value_text)
        ));
    }

    // --- the unit may not vouch for itself ----------------------------------------
    let unit = candidate
        .unit
        .as_deref()
        .and_then(|unit| bounded(unit, MAX_UNIT_CHARS));
    if let Some(unit) = &unit {
        if !verified
            .iter()
            .any(|item| quote::contains_token(&item.quote, unit))
        {
            status = lower(status, ClaimStatus::Hypothesis);
            notes.push(format!(
                "единица «{}» не найдена в подтверждённой цитате",
                short(unit)
            ));
        }
    }

    // --- conditions decide when a value is true ------------------------------------
    let conditions = candidate.conditions.as_deref().and_then(bounded_long_ref);
    if let Some(conditions) = &conditions {
        if !verified
            .iter()
            .any(|item| quote::contains_token(&item.quote, conditions))
        {
            status = lower(status, ClaimStatus::Hypothesis);
            notes.push("условия применимости не найдены в подтверждённой цитате".to_owned());
        }
    }

    if status != ClaimStatus::SourceSupported {
        outcome.note(format!("{label}: {}", notes.join("; ")));
    }

    Some(CheckedClaim {
        origin: candidate.origin,
        origin_id: candidate.origin_id,
        product_name,
        kind: candidate.kind,
        status,
        attribute,
        value_text,
        unit,
        conditions,
        model_context: candidate
            .model_context
            .as_deref()
            .and_then(bounded_long_ref),
        check_note: (!notes.is_empty()).then(|| clip(&notes.join("; "), MAX_LONG_TEXT_CHARS)),
        evidence: verified,
    })
}

enum Located {
    Verified {
        checked: CheckedEvidence,
        moved: bool,
    },
    /// The source row is gone or holds no text.
    Unreadable,
    /// The source is readable and no longer contains the quotation.
    Lost,
}

/// Find the candidate's quotation in the source text as it is stored now.
///
/// Finding it *somewhere* is what makes the claim supported: the document still says
/// this. Finding it at different offsets is not a failure — a page that was re-read can
/// shift every offset after it — so the located position is stored and the move is
/// reported. What is stored as the quotation is the source's own current wording, taken
/// at those offsets, never the candidate's copy of it.
fn locate(evidence: &CandidateEvidence) -> Located {
    let Some(source_text) = evidence.source_text.as_deref() else {
        return Located::Unreadable;
    };
    if source_text.trim().is_empty() {
        return Located::Unreadable;
    }

    let searchable = SearchableText::new(source_text);
    let Ok(found) = searchable.locate(&evidence.quote) else {
        return Located::Lost;
    };

    let char_start = i32::try_from(found.char_start).unwrap_or(i32::MAX);
    let char_end = i32::try_from(found.char_end).unwrap_or(i32::MAX);
    let moved = char_start != evidence.char_start;

    Located::Verified {
        checked: CheckedEvidence {
            source_kind: evidence.source_kind,
            material_id: evidence.material_id,
            material_filename: evidence.material_filename.clone(),
            page_number: evidence.page_number,
            region_id: evidence.region_id,
            url: evidence.url.clone(),
            host: evidence.host.clone(),
            retrieved_at: evidence.retrieved_at,
            content_hash: evidence.content_hash.clone(),
            quote: found.text,
            char_start,
            char_end,
        },
        moved,
    }
}

/// Carry a candidate's citation through unverified, for a claim whose source cannot be
/// read at all. It is the last record of what the document said, and dropping it would
/// leave a published claim with no trail — which the database refuses anyway.
fn carry_over(evidence: &CandidateEvidence) -> CheckedEvidence {
    CheckedEvidence {
        source_kind: evidence.source_kind,
        material_id: evidence.material_id,
        material_filename: evidence.material_filename.clone(),
        page_number: evidence.page_number,
        region_id: evidence.region_id,
        url: evidence.url.clone(),
        host: evidence.host.clone(),
        retrieved_at: evidence.retrieved_at,
        content_hash: evidence.content_hash.clone(),
        quote: clip(&evidence.quote, 600),
        char_start: evidence.char_start,
        char_end: evidence.char_end,
    }
}

/// Mark every group of supported claims that disagree about one product's one property.
///
/// Scope is narrow on purpose, and the narrowness is the honest part. Two statements
/// contradict each other only when they are about the **same product** and the **same
/// property** and their values differ once folded. An industry conclusion names no
/// product, so it is never used to contradict a partner's own document — a competitor's
/// standard disagreeing with a catalogue is not evidence that the catalogue is wrong
/// (`block-01-plan.md`, 1D §4). Values that differ only in spacing, case or dash variant
/// are the same value and are not a contradiction.
///
/// What this does **not** catch is stated in the phase documentation: two attributes
/// that mean the same thing under different names ("нагрузка" and "предельная
/// нагрузка") are not compared, because deciding that they are the same property is a
/// judgement, not a rule.
fn mark_contradictions(outcome: &mut CheckOutcome) {
    let mut groups: Vec<((String, String), Vec<usize>)> = Vec::new();

    for (index, claim) in outcome.claims.iter().enumerate() {
        if claim.status != ClaimStatus::SourceSupported {
            continue;
        }
        let Some(subject) = claim.subject() else {
            continue;
        };
        match groups.iter_mut().find(|(key, _)| *key == subject) {
            Some((_, members)) => members.push(index),
            None => groups.push((subject, vec![index])),
        }
    }

    for ((product, attribute), members) in groups {
        if members.len() < 2 {
            continue;
        }
        let mut values: Vec<String> = members
            .iter()
            .map(|index| normalise(&outcome.claims[*index].value_text))
            .collect();
        values.sort();
        values.dedup();
        if values.len() < 2 {
            continue;
        }

        let listed = values
            .iter()
            .map(|value| format!("«{}»", short(value)))
            .collect::<Vec<_>>()
            .join(", ");
        for index in &members {
            let claim = &mut outcome.claims[*index];
            claim.status = ClaimStatus::Conflicted;
            let note = format!(
                "источники расходятся о свойстве «{}»: {listed}. Численный ответ по этому \
                 свойству не даётся, пока расхождение не разрешено",
                short(&attribute)
            );
            claim.check_note = Some(match claim.check_note.take() {
                Some(existing) => clip(&format!("{existing}; {note}"), MAX_LONG_TEXT_CHARS),
                None => clip(&note, MAX_LONG_TEXT_CHARS),
            });
        }
        outcome.note(format!(
            "«{}»: источники расходятся о свойстве «{}» ({listed})",
            short(&product),
            short(&attribute)
        ));
    }
}

/// Verdicts only ever move downwards during one check.
const fn lower(current: ClaimStatus, proposed: ClaimStatus) -> ClaimStatus {
    match current {
        ClaimStatus::SourceSupported => proposed,
        other => other,
    }
}

fn bounded(value: &str, max_chars: usize) -> Option<String> {
    let cleaned = clip(&sanitise(value), max_chars);
    let trimmed = cleaned.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn bounded_long(value: &str) -> Option<String> {
    bounded(value, MAX_LONG_TEXT_CHARS)
}

fn bounded_long_ref(value: &str) -> Option<String> {
    bounded_long(value)
}

/// Control characters become spaces; the column would otherwise refuse the row at INSERT
/// time and take the whole version with it.
fn sanitise(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

fn short(value: &str) -> String {
    clip(&sanitise(value), 60).trim().to_owned()
}

fn label_of(candidate: &CandidateClaim) -> String {
    let attribute = normalised_column(&candidate.attribute).unwrap_or_else(|| "?".to_owned());
    match &candidate.product_name {
        Some(product) => format!("«{}: {}»", short(product), short(&attribute)),
        None => format!("«{}»", short(&attribute)),
    }
}
