//! Phase 1F — what changed between two published versions.
//!
//! `block-01-plan.md` §1F asks for a comparison of versions; `docs/publication-1e.md`
//! lists its absence as known limitation 9. This module is that comparison, and it is a
//! pure function over two snapshots — no database, no model, no guessing.
//!
//! **The hard part is deciding that two claims are the same claim.** `origin_id` cannot
//! do it: re-running the product role over a material writes new candidate rows with new
//! identifiers, so every claim of the new version would have an origin nobody has seen
//! and the diff would report "everything removed, everything added" for a re-draft that
//! changed one number. The identity used here is therefore the *subject* of the claim —
//! its scope, its product and its property, folded the same way search folds them.
//!
//! That choice has one consequence and it is stated in the result rather than hidden:
//! renaming a property reads as one removal plus one addition. Deciding that «нагрузка»
//! and «предельная нагрузка» are the same property is a judgement, and 1E already
//! declined to make it when detecting contradictions (known limitation 2). Making it here
//! — where the output is a sentence claiming a value *changed* — would be worse: a wrong
//! match does not merely omit a caveat, it invents a change that never happened.

use std::collections::{BTreeMap, BTreeSet};

use otdel_core::publication::{
    EvidenceSourceKind, ReadinessEntry, ReadinessState, ReadinessTopic, VersionClaim,
    VersionEvidence, VersionGap,
};
use otdel_core::updates::{
    ChangeCounts, ChangeKind, ClaimChange, ClaimSide, GapChange, ReadinessChange,
};
use uuid::Uuid;

use crate::chunk::normalise;

/// The caveats that travel with every comparison.
pub fn limitations() -> Vec<String> {
    vec![
        "Сравнение идёт по области, изделию и названию свойства. Переименованное свойство \
         выглядит как удалённое и добавленное — решать, что «нагрузка» и «предельная \
         нагрузка» одно и то же, система не берётся."
            .to_owned(),
        "Сравниваются снимки версий, а не документы. Строка «значение изменилось» \
         означает, что две опубликованные версии говорят разное, и называет источник \
         каждой."
            .to_owned(),
    ]
}

/// Compare two snapshots, older first.
///
/// `from` may be empty — that is a partner's first version, and every claim in it is an
/// addition. Reporting "no changes" for a first version would be true of the comparison
/// and false about the world.
pub fn compare_claims(
    from: &[VersionClaim],
    to: &[VersionClaim],
) -> (Vec<ClaimChange>, ChangeCounts) {
    // A subject maps to a **list**, not to one claim.
    //
    // Two claims of one version can share a subject, and routinely do: that is exactly
    // the shape `check.rs::mark_contradictions` exists to detect, and nothing between
    // `load_candidates` and `write_version` collapses them. Collecting into a
    // `BTreeMap<_, &VersionClaim>` silently kept the *last* of each group — so a version
    // holding two disagreeing loads for one profile was compared by whichever of them
    // sorted last, and a claim that really disappeared was reported as a value change
    // that never happened. That is the precise failure this module's header promises not
    // to make, so the grouping is a list and the pairing below is explicit.
    let before = group_by_subject(from);
    let after = group_by_subject(to);

    let mut changes: Vec<ClaimChange> = Vec::new();
    let mut counts = ChangeCounts::default();

    let subjects: BTreeSet<&SubjectKey> = before.keys().chain(after.keys()).collect();
    for subject in subjects {
        let olds = before.get(subject).map(Vec::as_slice).unwrap_or_default();
        let news = after.get(subject).map(Vec::as_slice).unwrap_or_default();
        compare_group(olds, news, &mut changes, &mut counts);
    }

    // Changes first, then additions, then removals, and alphabetically within each — a
    // stable order, so two reads of the same pair of versions look the same.
    changes.sort_by(|a, b| {
        rank(a.kind)
            .cmp(&rank(b.kind))
            .then_with(|| a.product_name.cmp(&b.product_name))
            .then_with(|| a.attribute.cmp(&b.attribute))
            .then_with(|| {
                claim_side_value(a.after.as_ref().or(a.before.as_ref()))
                    .cmp(&claim_side_value(b.after.as_ref().or(b.before.as_ref())))
            })
    });
    (changes, counts)
}

/// Compare the claims of **one** subject in the old version with those in the new one.
///
/// Three passes, in this order, because each one is more certain than the next:
///
///  1. **identical claims pair off first.** A version stating a value twice, unchanged,
///     must not produce a spurious "changed" merely because a second claim exists;
///  2. **what is left pairs positionally**, in a deterministic order, and each pair is a
///     change. With one claim on each side — the ordinary case — this is exactly the old
///     behaviour;
///  3. **leftovers are additions and removals.** The counters therefore always sum to the
///     number of claims on each side, which the previous version did not.
fn compare_group(
    olds: &[&VersionClaim],
    news: &[&VersionClaim],
    changes: &mut Vec<ClaimChange>,
    counts: &mut ChangeCounts,
) {
    let mut olds: Vec<&VersionClaim> = olds.to_vec();
    let mut news: Vec<&VersionClaim> = news.to_vec();
    olds.sort_by_key(|claim| ordering_key(claim));
    news.sort_by_key(|claim| ordering_key(claim));

    let mut old_taken = vec![false; olds.len()];
    let mut new_taken = vec![false; news.len()];

    // 1 — unchanged pairs.
    for (new_index, new_claim) in news.iter().enumerate() {
        if let Some(old_index) = olds.iter().enumerate().position(|(index, old_claim)| {
            !old_taken[index] && changed_fields(old_claim, new_claim).is_empty()
        }) {
            old_taken[old_index] = true;
            new_taken[new_index] = true;
            counts.unchanged += 1;
        }
    }

    // 2 — the rest, paired in order.
    let mut remaining_old: Vec<&VersionClaim> = olds
        .iter()
        .enumerate()
        .filter(|(index, _)| !old_taken[*index])
        .map(|(_, claim)| *claim)
        .collect();
    let mut remaining_new: Vec<&VersionClaim> = news
        .iter()
        .enumerate()
        .filter(|(index, _)| !new_taken[*index])
        .map(|(_, claim)| *claim)
        .collect();

    let paired = remaining_old.len().min(remaining_new.len());
    for index in 0..paired {
        let old_claim = remaining_old[index];
        let new_claim = remaining_new[index];
        let fields = changed_fields(old_claim, new_claim);
        counts.changed += 1;
        changes.push(ClaimChange {
            kind: ChangeKind::Changed,
            scope: new_claim.scope.as_str().to_owned(),
            product_name: new_claim.product_name.clone(),
            attribute: new_claim.attribute.clone(),
            before: Some(side(old_claim)),
            after: Some(side(new_claim)),
            message: describe_change(old_claim, new_claim, &fields),
            fields,
        });
    }

    // 3 — leftovers.
    for new_claim in remaining_new.drain(paired..) {
        counts.added += 1;
        changes.push(ClaimChange {
            kind: ChangeKind::Added,
            scope: new_claim.scope.as_str().to_owned(),
            product_name: new_claim.product_name.clone(),
            attribute: new_claim.attribute.clone(),
            before: None,
            after: Some(side(new_claim)),
            fields: Vec::new(),
            message: format!(
                "новое утверждение: {} — {}{} ({})",
                new_claim.attribute,
                new_claim.value_text,
                unit_suffix(new_claim),
                verdict_word(new_claim)
            ),
        });
    }
    for old_claim in remaining_old.drain(paired..) {
        counts.removed += 1;
        changes.push(ClaimChange {
            kind: ChangeKind::Removed,
            scope: old_claim.scope.as_str().to_owned(),
            product_name: old_claim.product_name.clone(),
            attribute: old_claim.attribute.clone(),
            before: Some(side(old_claim)),
            after: None,
            fields: Vec::new(),
            message: format!(
                "утверждения больше нет в версии: {} — {}{}. Это не опровержение: \
                 источник мог перестать подтверждать его, быть перечитан или исчезнуть",
                old_claim.attribute,
                old_claim.value_text,
                unit_suffix(old_claim)
            ),
        });
    }
}

fn group_by_subject<'a>(claims: &'a [VersionClaim]) -> BTreeMap<SubjectKey, Vec<&'a VersionClaim>> {
    let mut grouped: BTreeMap<SubjectKey, Vec<&'a VersionClaim>> = BTreeMap::new();
    for claim in claims {
        grouped.entry(subject(claim)).or_default().push(claim);
    }
    grouped
}

/// A total order within one subject, so pairing is deterministic across two reads.
fn ordering_key(claim: &VersionClaim) -> (String, String, String, Uuid) {
    (
        normalise(&claim.value_text),
        folded_opt(claim.unit.as_deref()),
        claim.status.as_str().to_owned(),
        claim.id,
    )
}

fn claim_side_value(side: Option<&ClaimSide>) -> String {
    side.map(|side| side.value_text.clone()).unwrap_or_default()
}

/// Readiness, topic by topic. All four are reported when either side has them, because
/// "commercial answers went from limited to blocked" is the single most consequential
/// thing a new version can do and it must not be something the reader has to notice.
pub fn compare_readiness(from: &[ReadinessEntry], to: &[ReadinessEntry]) -> Vec<ReadinessChange> {
    let mut changes = Vec::new();
    for topic in [
        ReadinessTopic::ProductDescription,
        ReadinessTopic::AudienceHypotheses,
        ReadinessTopic::CharacteristicAnswers,
        ReadinessTopic::CommercialAnswers,
    ] {
        let before = from.iter().find(|entry| entry.topic == topic);
        let after = to.iter().find(|entry| entry.topic == topic);
        let before_state = before.map(|entry| entry.state);
        let after_state = after.map(|entry| entry.state);
        if before_state == after_state {
            continue;
        }
        changes.push(ReadinessChange {
            topic,
            before: before_state,
            after: after_state,
            reason: after
                .map(|entry| entry.reason.clone())
                .unwrap_or_else(|| "готовность по этой теме не записана в новой версии".to_owned()),
        });
    }
    changes
}

/// Gaps, matched by product and topic. A gap that closed is as important as one that
/// opened: it is the reason an answer that used to be refused is now available.
pub fn compare_gaps(from: &[VersionGap], to: &[VersionGap]) -> Vec<GapChange> {
    let key = |gap: &VersionGap| {
        (
            normalise(gap.product_name.as_deref().unwrap_or_default()),
            normalise(&gap.topic),
        )
    };
    // Counted, not keyed — same reason as the claims above: two gaps of one version may
    // share a product and a topic, and a map would keep one of them and report the others
    // as neither present nor removed.
    let mut before: BTreeMap<(String, String), Vec<&VersionGap>> = BTreeMap::new();
    for gap in from {
        before.entry(key(gap)).or_default().push(gap);
    }
    let mut after: BTreeMap<(String, String), Vec<&VersionGap>> = BTreeMap::new();
    for gap in to {
        after.entry(key(gap)).or_default().push(gap);
    }

    let mut changes = Vec::new();
    for (k, gaps) in &after {
        let was = before.get(k).map_or(0, Vec::len);
        for gap in gaps.iter().skip(was) {
            changes.push(GapChange {
                kind: ChangeKind::Added,
                topic: gap.topic.clone(),
                missing: gap.missing.clone(),
                product_name: gap.product_name.clone(),
            });
        }
    }
    for (k, gaps) in &before {
        let now = after.get(k).map_or(0, Vec::len);
        for gap in gaps.iter().skip(now) {
            changes.push(GapChange {
                kind: ChangeKind::Removed,
                topic: gap.topic.clone(),
                missing: gap.missing.clone(),
                product_name: gap.product_name.clone(),
            });
        }
    }
    changes.sort_by(|a, b| {
        rank(a.kind)
            .cmp(&rank(b.kind))
            .then_with(|| a.topic.cmp(&b.topic))
    });
    changes
}

// --- internals ------------------------------------------------------------------------

/// Scope, product and property, folded. See the module documentation for why this and not
/// `origin_id`.
type SubjectKey = (String, String, String);

fn subject(claim: &VersionClaim) -> SubjectKey {
    (
        claim.scope.as_str().to_owned(),
        normalise(claim.product_name.as_deref().unwrap_or_default()),
        normalise(&claim.attribute),
    )
}

const fn rank(kind: ChangeKind) -> u8 {
    match kind {
        ChangeKind::Changed => 0,
        ChangeKind::Added => 1,
        ChangeKind::Removed => 2,
    }
}

fn changed_fields(before: &VersionClaim, after: &VersionClaim) -> Vec<String> {
    let mut fields = Vec::new();
    if normalise(&before.value_text) != normalise(&after.value_text) {
        fields.push("value_text".to_owned());
    }
    if folded_opt(before.unit.as_deref()) != folded_opt(after.unit.as_deref()) {
        fields.push("unit".to_owned());
    }
    if folded_opt(before.conditions.as_deref()) != folded_opt(after.conditions.as_deref()) {
        fields.push("conditions".to_owned());
    }
    if before.status != after.status {
        fields.push("status".to_owned());
    }
    // Sources are compared as the version recorded them. A claim whose value is identical
    // but which is now supported by a different document is a change worth seeing: it is
    // how "the new catalogue says the same thing" looks.
    if sources(&before.evidence) != sources(&after.evidence) {
        fields.push("sources".to_owned());
    }
    fields
}

fn folded_opt(value: Option<&str>) -> String {
    normalise(value.unwrap_or_default())
}

fn describe_change(before: &VersionClaim, after: &VersionClaim, fields: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if fields.iter().any(|field| field == "value_text") {
        parts.push(format!(
            "значение: {}{} → {}{}",
            before.value_text,
            unit_suffix(before),
            after.value_text,
            unit_suffix(after)
        ));
    } else if fields.iter().any(|field| field == "unit") {
        parts.push(format!(
            "единица: {} → {}",
            before.unit.as_deref().unwrap_or("не указана"),
            after.unit.as_deref().unwrap_or("не указана")
        ));
    }
    if fields.iter().any(|field| field == "status") {
        parts.push(format!(
            "проверка: {} → {}",
            verdict_word(before),
            verdict_word(after)
        ));
    }
    if fields.iter().any(|field| field == "conditions") {
        parts.push(format!(
            "условия: {} → {}",
            before.conditions.as_deref().unwrap_or("не записаны"),
            after.conditions.as_deref().unwrap_or("не записаны")
        ));
    }
    if fields.iter().any(|field| field == "sources") {
        parts.push("изменился состав источников".to_owned());
    }
    format!("{}: {}", after.attribute, parts.join("; "))
}

fn unit_suffix(claim: &VersionClaim) -> String {
    claim
        .unit
        .as_deref()
        .map(|unit| format!(" {unit}"))
        .unwrap_or_default()
}

/// The checker's verdict in the words the interface uses, so a diff line does not need a
/// legend. `source_supported` is deliberately not called "проверено".
fn verdict_word(claim: &VersionClaim) -> &'static str {
    use otdel_core::publication::ClaimStatus;
    match claim.status {
        ClaimStatus::SourceSupported => "подтверждено источником",
        ClaimStatus::Hypothesis => "гипотеза",
        ClaimStatus::Unknown => "источник не прочитан",
        ClaimStatus::Conflicted => "противоречие",
        ClaimStatus::Stale => "источник изменился",
    }
}

/// How a citation is named in a diff: a filename with its page, or a host. Sorted, so two
/// versions listing the same sources in a different order do not read as a change.
fn sources(evidence: &[VersionEvidence]) -> Vec<String> {
    let mut names: Vec<String> = evidence
        .iter()
        .map(|item| match item.source_kind {
            EvidenceSourceKind::Material => format!(
                "{}#{}",
                item.material_filename.as_deref().unwrap_or("документ"),
                item.page_number.unwrap_or(0)
            ),
            EvidenceSourceKind::External => item
                .host
                .clone()
                .or_else(|| item.url.clone())
                .unwrap_or_else(|| "внешний источник".to_owned()),
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

fn side(claim: &VersionClaim) -> ClaimSide {
    ClaimSide {
        claim_id: claim.id,
        status: claim.status.as_str().to_owned(),
        value_text: claim.value_text.clone(),
        unit: claim.unit.clone(),
        conditions: claim.conditions.clone(),
        sources: sources(&claim.evidence),
    }
}

/// A short sentence for the whole comparison, so the interface has something to show
/// before anybody expands a list.
pub fn summarise(counts: ChangeCounts, first_version: bool) -> String {
    if first_version {
        return format!(
            "Первая версия партнёра: {} утверждени(й) опубликовано, сравнивать не с чем.",
            counts.added
        );
    }
    if counts.added == 0 && counts.removed == 0 && counts.changed == 0 {
        return "Утверждения не изменились: обе версии говорят одно и то же.".to_owned();
    }
    format!(
        "Изменилось {}, добавлено {}, исчезло {}, без изменений {}.",
        counts.changed, counts.added, counts.removed, counts.unchanged
    )
}

/// Readiness across a version pair, in one sentence — a downgrade is named first because
/// it is the one that takes an answer away.
pub fn summarise_readiness(changes: &[ReadinessChange]) -> Option<String> {
    let downgraded: Vec<&ReadinessChange> = changes
        .iter()
        .filter(|change| {
            matches!(
                (change.before, change.after),
                (Some(ReadinessState::Ready), Some(ReadinessState::Limited))
                    | (Some(ReadinessState::Ready), Some(ReadinessState::Blocked))
                    | (Some(ReadinessState::Limited), Some(ReadinessState::Blocked))
            )
        })
        .collect();
    if downgraded.is_empty() {
        return None;
    }
    Some(format!(
        "Готовность понизилась по темам: {}.",
        downgraded
            .iter()
            .map(|change| change.topic.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use otdel_core::knowledge::FactKind;
    use otdel_core::publication::{ClaimOrigin, ClaimScope, ClaimStatus};
    use uuid::Uuid;

    fn evidence(filename: &str, page: i32) -> VersionEvidence {
        VersionEvidence {
            id: Uuid::new_v4(),
            claim_id: Uuid::nil(),
            source_kind: EvidenceSourceKind::Material,
            material_id: Some(Uuid::from_u128(7)),
            material_filename: Some(filename.to_owned()),
            page_number: Some(page),
            region_id: None,
            url: None,
            host: None,
            retrieved_at: None,
            content_hash: None,
            quote: "нагрузка 3,5 кН".to_owned(),
            char_start: 0,
            char_end: 15,
        }
    }

    fn claim(attribute: &str, value: &str, status: ClaimStatus) -> VersionClaim {
        VersionClaim {
            id: Uuid::new_v4(),
            version_id: Uuid::nil(),
            origin: ClaimOrigin::PartnerMaterial,
            origin_id: Uuid::new_v4(),
            scope: ClaimScope::Partner,
            product_name: Some("BP21".to_owned()),
            kind: FactKind::Characteristic,
            status,
            attribute: attribute.to_owned(),
            value_text: value.to_owned(),
            unit: Some("кН".to_owned()),
            conditions: None,
            model_context: None,
            check_note: None,
            evidence: vec![evidence("catalogue.pdf", 3)],
            created_at: Utc::now(),
        }
    }

    #[test]
    fn a_new_candidate_row_for_the_same_property_is_a_change_not_a_replacement() {
        // Re-drafting writes a new fact id. Matching on `origin_id` would report this as
        // one removal and one addition, which is exactly the wrong story.
        let before = vec![claim("нагрузка", "3.5", ClaimStatus::SourceSupported)];
        let after = vec![claim("нагрузка", "4.0", ClaimStatus::SourceSupported)];
        assert_ne!(before[0].origin_id, after[0].origin_id);

        let (changes, counts) = compare_claims(&before, &after);
        assert_eq!(counts.changed, 1);
        assert_eq!(counts.added, 0);
        assert_eq!(counts.removed, 0);
        assert_eq!(changes[0].kind, ChangeKind::Changed);
        assert!(changes[0].fields.contains(&"value_text".to_owned()));
        assert!(changes[0].message.contains("3.5"), "{}", changes[0].message);
        assert!(changes[0].message.contains("4.0"), "{}", changes[0].message);
        assert_eq!(changes[0].before.as_ref().unwrap().value_text, "3.5");
        assert_eq!(changes[0].after.as_ref().unwrap().value_text, "4.0");
    }

    #[test]
    fn two_claims_sharing_one_subject_are_both_compared() {
        // A version may hold two claims about one product and one property — that is
        // exactly what `mark_contradictions` detects, and nothing collapses them before
        // publication. Keying a map by subject kept only the last of them, so the claim
        // that really disappeared was reported as a value change that never happened.
        let before = vec![
            claim("нагрузка", "3.5", ClaimStatus::Conflicted),
            claim("нагрузка", "4.0", ClaimStatus::Conflicted),
        ];
        let after = vec![claim("нагрузка", "3.5", ClaimStatus::SourceSupported)];

        let (changes, counts) = compare_claims(&before, &after);
        assert_eq!(counts.removed, 1, "one of the two really disappeared");
        assert_eq!(counts.added, 0);
        assert_eq!(
            counts.changed + counts.unchanged,
            1,
            "the surviving claim is accounted for exactly once: {counts:?}"
        );
        // The counters describe every claim on both sides, which a map could not.
        assert_eq!(counts.unchanged + counts.changed + counts.removed, 2);
        assert_eq!(counts.unchanged + counts.changed + counts.added, 1);

        // And nothing claims the value moved from 4.0 to 3.5.
        assert!(
            !changes.iter().any(|change| {
                change.kind == ChangeKind::Changed
                    && change
                        .before
                        .as_ref()
                        .is_some_and(|side| side.value_text == "4.0")
                    && change
                        .after
                        .as_ref()
                        .is_some_and(|side| side.value_text == "3.5")
            }),
            "a disappearance must not be rendered as a value change: {changes:?}"
        );
    }

    #[test]
    fn a_value_stated_twice_and_still_stated_twice_is_not_a_change() {
        let twice = vec![
            claim("нагрузка", "3.5", ClaimStatus::SourceSupported),
            claim("нагрузка", "3.5", ClaimStatus::SourceSupported),
        ];
        let (changes, counts) = compare_claims(&twice.clone(), &twice);
        assert!(changes.is_empty(), "{changes:?}");
        assert_eq!(counts.unchanged, 2);
    }

    #[test]
    fn a_duplicated_subject_that_grew_reports_an_addition_not_a_rewrite() {
        let before = vec![claim("нагрузка", "3.5", ClaimStatus::SourceSupported)];
        let after = vec![
            claim("нагрузка", "3.5", ClaimStatus::SourceSupported),
            claim("нагрузка", "4.0", ClaimStatus::Conflicted),
        ];
        let (changes, counts) = compare_claims(&before, &after);
        assert_eq!(counts.unchanged, 1);
        assert_eq!(counts.added, 1);
        assert_eq!(counts.changed, 0);
        assert_eq!(counts.removed, 0);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, ChangeKind::Added);
        assert_eq!(changes[0].after.as_ref().unwrap().value_text, "4.0");
    }

    #[test]
    fn two_gaps_on_one_topic_are_both_counted() {
        let gap = |topic: &str, missing: &str| VersionGap {
            id: Uuid::new_v4(),
            version_id: Uuid::nil(),
            origin_id: Uuid::new_v4(),
            product_name: Some("BP21".to_owned()),
            topic: topic.to_owned(),
            missing: missing.to_owned(),
            blocks: None,
            blocks_topics: Vec::new(),
            created_at: Utc::now(),
        };
        let before = vec![gap("цена", "нет прайса"), gap("цена", "нет условий")];
        let after = vec![gap("цена", "нет прайса")];

        let changes = compare_gaps(&before, &after);
        assert_eq!(changes.len(), 1, "one of the two gaps closed: {changes:?}");
        assert_eq!(changes[0].kind, ChangeKind::Removed);
    }

    #[test]
    fn an_identical_pair_of_versions_reports_no_changes() {
        let before = vec![claim("нагрузка", "3.5", ClaimStatus::SourceSupported)];
        let after = before.clone();
        let (changes, counts) = compare_claims(&before, &after);
        assert!(changes.is_empty());
        assert_eq!(counts.unchanged, 1);
        assert_eq!(
            summarise(counts, false),
            "Утверждения не изменились: обе версии говорят одно и то же."
        );
    }

    #[test]
    fn a_verdict_that_dropped_is_reported_even_when_the_value_is_identical() {
        // The source changed under an unchanged number: the value reads the same and the
        // claim is no longer supported. A diff that only compared values would show this
        // pair as identical.
        let before = vec![claim("нагрузка", "3.5", ClaimStatus::SourceSupported)];
        let after = vec![claim("нагрузка", "3.5", ClaimStatus::Stale)];
        let (changes, counts) = compare_claims(&before, &after);
        assert_eq!(counts.changed, 1);
        assert_eq!(changes[0].fields, vec!["status".to_owned()]);
        assert!(
            changes[0].message.contains("источник изменился"),
            "{}",
            changes[0].message
        );
    }

    #[test]
    fn a_renamed_property_is_an_addition_and_a_removal_and_the_result_says_so() {
        let before = vec![claim("нагрузка", "3.5", ClaimStatus::SourceSupported)];
        let after = vec![claim(
            "предельная нагрузка",
            "3.5",
            ClaimStatus::SourceSupported,
        )];
        let (_changes, counts) = compare_claims(&before, &after);
        assert_eq!(counts.added, 1);
        assert_eq!(counts.removed, 1);
        assert_eq!(counts.changed, 0);
        assert!(
            limitations()
                .iter()
                .any(|line| line.contains("Переименованное свойство")),
            "the limitation has to be stated next to the result"
        );
    }

    #[test]
    fn a_disappeared_claim_is_not_called_a_refutation() {
        let before = vec![claim("нагрузка", "3.5", ClaimStatus::SourceSupported)];
        let (changes, counts) = compare_claims(&before, &[]);
        assert_eq!(counts.removed, 1);
        assert_eq!(changes[0].kind, ChangeKind::Removed);
        assert!(
            changes[0].message.contains("не опровержение"),
            "{}",
            changes[0].message
        );
        assert!(changes[0].after.is_none());
    }

    #[test]
    fn the_first_version_of_a_partner_is_all_additions_and_says_so() {
        let after = vec![claim("нагрузка", "3.5", ClaimStatus::SourceSupported)];
        let (changes, counts) = compare_claims(&[], &after);
        assert_eq!(counts.added, 1);
        assert_eq!(changes[0].kind, ChangeKind::Added);
        assert!(summarise(counts, true).contains("сравнивать не с чем"));
    }

    #[test]
    fn a_different_supporting_document_is_a_change_of_sources() {
        let before = vec![claim("нагрузка", "3.5", ClaimStatus::SourceSupported)];
        let mut after = before.clone();
        after[0].evidence = vec![evidence("catalogue-2027.pdf", 5)];
        let (changes, counts) = compare_claims(&before, &after);
        assert_eq!(counts.changed, 1);
        assert_eq!(changes[0].fields, vec!["sources".to_owned()]);
        assert_eq!(
            changes[0].after.as_ref().unwrap().sources,
            vec!["catalogue-2027.pdf#5".to_owned()]
        );
    }

    #[test]
    fn the_same_sources_in_another_order_are_not_a_change() {
        let mut before = vec![claim("нагрузка", "3.5", ClaimStatus::SourceSupported)];
        before[0].evidence = vec![evidence("b.pdf", 1), evidence("a.pdf", 2)];
        let mut after = before.clone();
        after[0].evidence = vec![evidence("a.pdf", 2), evidence("b.pdf", 1)];
        let (changes, counts) = compare_claims(&before, &after);
        assert!(changes.is_empty(), "{changes:?}");
        assert_eq!(counts.unchanged, 1);
    }

    #[test]
    fn readiness_reports_all_four_topics_and_names_a_downgrade() {
        let before = vec![
            ReadinessEntry {
                topic: ReadinessTopic::CharacteristicAnswers,
                state: ReadinessState::Ready,
                reason: "есть подтверждённая характеристика".to_owned(),
            },
            ReadinessEntry {
                topic: ReadinessTopic::CommercialAnswers,
                state: ReadinessState::Blocked,
                reason: "цен нет".to_owned(),
            },
        ];
        let after = vec![
            ReadinessEntry {
                topic: ReadinessTopic::CharacteristicAnswers,
                state: ReadinessState::Limited,
                reason: "среди найденного есть противоречие".to_owned(),
            },
            ReadinessEntry {
                topic: ReadinessTopic::CommercialAnswers,
                state: ReadinessState::Blocked,
                reason: "цен нет".to_owned(),
            },
        ];
        let changes = compare_readiness(&before, &after);
        assert_eq!(changes.len(), 1, "only the topic that moved is reported");
        assert_eq!(changes[0].topic, ReadinessTopic::CharacteristicAnswers);
        assert_eq!(changes[0].before, Some(ReadinessState::Ready));
        assert_eq!(changes[0].after, Some(ReadinessState::Limited));
        assert_eq!(changes[0].reason, "среди найденного есть противоречие");

        let summary = summarise_readiness(&changes).expect("a downgrade must be summarised");
        assert!(summary.contains("понизилась"), "{summary}");
    }

    #[test]
    fn a_gap_that_closed_and_one_that_opened_are_both_reported() {
        let gap = |topic: &str| VersionGap {
            id: Uuid::new_v4(),
            version_id: Uuid::nil(),
            origin_id: Uuid::new_v4(),
            product_name: Some("BP21".to_owned()),
            topic: topic.to_owned(),
            missing: "нет данных".to_owned(),
            blocks: None,
            blocks_topics: Vec::new(),
            created_at: Utc::now(),
        };
        let changes = compare_gaps(&[gap("цена")], &[gap("срок поставки")]);
        assert_eq!(changes.len(), 2);
        let added: Vec<&GapChange> = changes
            .iter()
            .filter(|change| change.kind == ChangeKind::Added)
            .collect();
        let removed: Vec<&GapChange> = changes
            .iter()
            .filter(|change| change.kind == ChangeKind::Removed)
            .collect();
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].topic, "срок поставки");
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].topic, "цена");
    }

    #[test]
    fn an_industry_claim_never_matches_a_partner_claim_with_the_same_property() {
        // The scope is part of the identity, so an industry conclusion about «нагрузка»
        // cannot be reported as a change to the partner's own «нагрузка».
        let mut industry = claim("нагрузка", "3.5", ClaimStatus::SourceSupported);
        industry.scope = ClaimScope::Industry;
        industry.product_name = None;
        let partner = claim("нагрузка", "3.5", ClaimStatus::SourceSupported);

        let (changes, counts) = compare_claims(&[partner], &[industry]);
        assert_eq!(counts.added, 1);
        assert_eq!(counts.removed, 1);
        assert_eq!(counts.changed, 0);
        assert!(changes.iter().any(|change| change.scope == "industry"));
    }
}
