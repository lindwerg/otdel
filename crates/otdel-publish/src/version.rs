//! What a version is built from, and whether it may be published.
//!
//! Two decisions live here, and both are deterministic.
//!
//! **The fingerprint** identifies the *input* a version was built from. It is what makes
//! two rules in `block-01-spec.md` checkable rather than hoped for: a repeated upload or
//! a repeated press of the button must not produce a second published result (§13.4),
//! and a run that finished late must not overwrite a newer revision (§7). Both become
//! one comparison of two hex strings.
//!
//! **The publication rules** decide whether the snapshot goes live. Publication is
//! automatic — `block-01-plan.md` 1E §5 says no manual approval step is required — and
//! it is automatic precisely *because* the rules are mechanical. A version that fails
//! them is not discarded and not quietly published either: it is stored as `blocked`,
//! with the rules it failed in words, which is what "честно остаются неполными" means.
//!
//! R05 added a rule before all of them: a partner whose materials are knowingly short of
//! what a passport needs does not publish *itself*. Automatic publication was always
//! conditional on the rules being able to see everything that matters, and until R05 they
//! could not see whether the draft underneath them was complete.

use otdel_core::passport::RequirementsState;
use otdel_core::publication::{ReadinessEntry, ReadinessState};
use sha2::{Digest, Sha256};

use crate::chunk::normalise;
use crate::claim::{CandidateClaim, CheckedClaim};

/// Upper bound on the per-material shortfall lines one blocked decision may carry. A
/// partner with forty materials must not turn one refusal into four hundred lines.
const MAX_SHORTFALL_LINES: usize = 20;

/// Fingerprint of the candidate set a version was built from.
///
/// Order-independent: the claims are hashed individually and the digests sorted, so the
/// same knowledge read in a different order is the same input. Content-sensitive: the
/// verdict is part of each claim's digest, so a source that changed underneath — turning
/// a supported claim into a stale one — produces a different fingerprint and therefore a
/// new version, even though the candidate rows did not move.
pub fn fingerprint(claims: &[CheckedClaim]) -> String {
    let mut digests: Vec<[u8; 32]> = claims
        .iter()
        .map(|claim| {
            let mut hasher = Sha256::new();
            hasher.update(claim.origin.as_str().as_bytes());
            hasher.update([0]);
            hasher.update(claim.origin_id.as_bytes());
            hasher.update([0]);
            hasher.update(claim.status.as_str().as_bytes());
            hasher.update([0]);
            hasher.update(normalise(claim.product_name.as_deref().unwrap_or_default()).as_bytes());
            hasher.update([0]);
            hasher.update(normalise(&claim.attribute).as_bytes());
            hasher.update([0]);
            hasher.update(normalise(&claim.value_text).as_bytes());
            hasher.update([0]);
            hasher.update(normalise(claim.unit.as_deref().unwrap_or_default()).as_bytes());
            hasher.update([0]);
            hasher.update(normalise(claim.conditions.as_deref().unwrap_or_default()).as_bytes());
            for evidence in &claim.evidence {
                hasher.update([0]);
                hasher.update(normalise(&evidence.quote).as_bytes());
            }
            hasher.finalize().into()
        })
        .collect();
    digests.sort_unstable();

    let mut outer = Sha256::new();
    for digest in digests {
        outer.update(digest);
    }
    hex::encode(outer.finalize())
}

/// Fingerprint of the **candidates alone**, without the verdicts the checker gave them.
///
/// [`fingerprint`] is verdict-sensitive on purpose: a source that changed underneath a
/// claim produces a different published version even though no candidate row moved. That
/// is exactly right for deciding what to publish, and useless for the question phase 1F
/// has to answer on a plain GET — *is what we published still built from what the
/// partner's documents currently say?* Answering that with [`fingerprint`] would mean
/// running the whole check, which is the thing the owner is deciding whether to start.
///
/// So this covers what 1C and 1D actually stored: origin, product, property, value, unit,
/// conditions and the quotations. Order-independent, folded the same way, and computed
/// from the same rows the checker would read. Two consequences, both intended:
///
/// * a new document, a re-draft or a withdrawn fact changes it, and the refresh status
///   says a new check would produce something different;
/// * a source that changed *under* an unchanged candidate does **not** change it. That
///   case is caught by the source-revision comparison instead, which is why the refresh
///   status carries both and does not pretend one subsumes the other.
pub fn candidate_fingerprint(claims: &[CandidateClaim]) -> String {
    let mut digests: Vec<[u8; 32]> = claims
        .iter()
        .map(|claim| {
            let mut hasher = Sha256::new();
            hasher.update(claim.origin.as_str().as_bytes());
            hasher.update([0]);
            hasher.update(claim.origin_id.as_bytes());
            hasher.update([0]);
            hasher.update(normalise(claim.product_name.as_deref().unwrap_or_default()).as_bytes());
            hasher.update([0]);
            hasher.update(normalise(&claim.attribute).as_bytes());
            hasher.update([0]);
            hasher.update(normalise(&claim.value_text).as_bytes());
            hasher.update([0]);
            hasher.update(normalise(claim.unit.as_deref().unwrap_or_default()).as_bytes());
            hasher.update([0]);
            hasher.update(normalise(claim.conditions.as_deref().unwrap_or_default()).as_bytes());
            for evidence in &claim.evidence {
                hasher.update([0]);
                hasher.update(normalise(&evidence.quote).as_bytes());
            }
            hasher.finalize().into()
        })
        .collect();
    digests.sort_unstable();

    let mut outer = Sha256::new();
    for digest in digests {
        outer.update(digest);
    }
    hex::encode(outer.finalize())
}

/// What the publication rules concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublicationDecision {
    /// The rules pass. No human click is required (`block-01-plan.md`, 1E §5).
    Publish,
    /// The rules do not pass. The snapshot is stored and marked, with these reasons.
    Blocked { reasons: Vec<String> },
    /// The input is byte-for-byte the one already published. Publishing again would
    /// create a second version saying the same thing and a new "current" pointer for no
    /// reason (`block-01-spec.md` §13.4).
    Unchanged,
}

/// What R05 concluded about one material this partner's knowledge is drawn from.
///
/// Only the verdict travels here, never the counters: the publication rules ask one
/// question — "has a person still got to look at this material?" — and a rule that could
/// see page counts would eventually start weighing them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceReadiness {
    pub material_filename: String,
    pub requirements: RequirementsState,
    /// `name: explanation` lines from the run, shown as-is.
    pub missing: Vec<String>,
}

impl SourceReadiness {
    /// Whether this material is knowingly short of what a passport needs.
    ///
    /// `Unknown` is **not** included, and that asymmetry is deliberate: it means the run
    /// predates the requirement check or never got as far as making it, and blocking on it
    /// would stop every partner drafted before R05 from ever publishing again. Blocking on
    /// `Unmet` is blocking on a judgement somebody actually made.
    fn needs_a_person(&self) -> bool {
        self.requirements == RequirementsState::Unmet
    }
}

/// Decide whether this snapshot may be published.
///
/// `published_fingerprint` is the fingerprint of the version currently published for
/// this partner, when there is one. `sources` carries R05's verdict on each material the
/// candidates came from.
pub fn decide(
    claims: &[CheckedClaim],
    readiness: &[ReadinessEntry],
    sources: &[SourceReadiness],
    fingerprint_now: &str,
    published_fingerprint: Option<&str>,
) -> PublicationDecision {
    let mut reasons: Vec<String> = Vec::new();

    // Rule 0 — a material whose passport is knowingly incomplete does not publish itself.
    //
    // This rule exists because of a live pass that had every other rule's blessing: the
    // claims were supported, the readiness topics answered, and a version was built and
    // published from a catalogue whose own run recorded 0 terms, 0 applications and 123
    // readings it could not settle. `allows_automatic_publication` said no and nothing
    // consulted it. It does now, and it is checked *first*, so a partner is told the real
    // reason rather than one of the downstream ones.
    //
    // What this does not do: unpublish, replace or retract. A blocked decision leaves the
    // version that is already current exactly where it is — the owner keeps yesterday's
    // published answer while today's draft waits for them.
    let unready: Vec<&SourceReadiness> = sources
        .iter()
        .filter(|source| source.needs_a_person())
        .collect();
    if !unready.is_empty() {
        let named: Vec<String> = unready
            .iter()
            .map(|source| format!("«{}»", source.material_filename))
            .collect();
        reasons.push(format!(
            "автоматическая публикация остановлена: у {} из {} материалов паспорт собран \
             не полностью ({}). Это не ошибка разбора — по этим материалам осталось то, \
             что должен посмотреть человек; уже опубликованная версия не тронута",
            unready.len(),
            sources.len(),
            named.join(", ")
        ));
        // The named shortfalls, so the owner sees what to fix rather than only that
        // something is unfixed. Bounded: a partner with forty materials must not turn
        // one blocked decision into four hundred lines.
        reasons.extend(
            unready
                .iter()
                .flat_map(|source| {
                    source
                        .missing
                        .iter()
                        .map(|line| format!("{}: {line}", source.material_filename))
                })
                .take(MAX_SHORTFALL_LINES),
        );
    }

    // Rule 1 — something has to be supported. A version made entirely of hypotheses,
    // contradictions and unreadable sources would answer nothing and would still look
    // like published knowledge.
    if !claims.iter().any(|claim| claim.status.is_answerable()) {
        reasons.push(
            "ни одно утверждение не подтверждено источником: публиковать нечего. Проверьте, \
             что материалы прочитаны и разобраны, и повторите проверку"
                .to_owned(),
        );
    }

    // Rule 2 — at least one thing has to be answerable. This can fail while rule 1
    // passes: a supported claim whose kind bears on no topic (an industry conclusion
    // that is not an application) leaves every topic blocked.
    if readiness
        .iter()
        .all(|entry| entry.state == ReadinessState::Blocked)
    {
        reasons.push(
            "все четыре готовности заблокированы: опубликованная версия не смогла бы ответить \
             ни на один вопрос"
                .to_owned(),
        );
    }

    if !reasons.is_empty() {
        return PublicationDecision::Blocked { reasons };
    }

    // Rule 3 — do not republish the same input. Checked last, so a snapshot that would
    // have been blocked is reported as blocked rather than as unchanged.
    if published_fingerprint == Some(fingerprint_now) {
        return PublicationDecision::Unchanged;
    }

    PublicationDecision::Publish
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claim::CheckedEvidence;
    use otdel_core::knowledge::FactKind;
    use otdel_core::publication::{ClaimOrigin, ClaimStatus, EvidenceSourceKind, ReadinessTopic};
    use uuid::Uuid;

    fn evidence(quote: &str) -> CheckedEvidence {
        CheckedEvidence {
            source_kind: EvidenceSourceKind::Material,
            material_id: Some(Uuid::from_u128(1)),
            material_filename: Some("catalogue.pdf".to_owned()),
            page_number: Some(3),
            region_id: None,
            url: None,
            host: None,
            retrieved_at: None,
            content_hash: None,
            quote: quote.to_owned(),
            char_start: 0,
            char_end: 10,
        }
    }

    fn claim(id: u128, value: &str, status: ClaimStatus) -> CheckedClaim {
        CheckedClaim {
            origin: ClaimOrigin::PartnerMaterial,
            origin_id: Uuid::from_u128(id),
            product_name: Some("BP21".to_owned()),
            kind: FactKind::Characteristic,
            status,
            attribute: "нагрузка".to_owned(),
            value_text: value.to_owned(),
            unit: None,
            conditions: None,
            model_context: None,
            check_note: None,
            evidence: vec![evidence("BP21 3.5 kN")],
        }
    }

    fn ready() -> Vec<ReadinessEntry> {
        vec![ReadinessEntry {
            topic: ReadinessTopic::CharacteristicAnswers,
            state: ReadinessState::Ready,
            reason: "ok".to_owned(),
        }]
    }

    fn source(requirements: RequirementsState, missing: &[&str]) -> SourceReadiness {
        SourceReadiness {
            material_filename: "catalogue.pdf".to_owned(),
            requirements,
            missing: missing.iter().map(|line| (*line).to_owned()).collect(),
        }
    }

    /// A partner whose materials R05 judged complete — the precondition every rule below
    /// rule 0 was written against.
    fn ready_sources() -> Vec<SourceReadiness> {
        vec![source(RequirementsState::Met, &[])]
    }

    #[test]
    fn a_material_whose_passport_is_incomplete_stops_automatic_publication() {
        let claims = vec![claim(1, "3.5", ClaimStatus::SourceSupported)];
        let decision = decide(
            &claims,
            &ready(),
            &[source(
                RequirementsState::Unmet,
                &["glossary: заявление не принято", "applications: нет задач"],
            )],
            "aa",
            None,
        );

        match decision {
            PublicationDecision::Blocked { reasons } => {
                // The real reason first, not one of the downstream ones.
                assert!(reasons[0].contains("паспорт собран"), "{reasons:?}");
                assert!(reasons[0].contains("catalogue.pdf"), "{reasons:?}");
                // …and the named shortfalls, so the owner sees what to fix.
                assert!(
                    reasons.iter().any(|line| line.contains("glossary:")),
                    "{reasons:?}"
                );
                // Stated in the refusal itself: nothing already published is touched.
                assert!(reasons[0].contains("не тронута"), "{reasons:?}");
            }
            other => panic!("an incomplete passport must not publish itself: {other:?}"),
        }
    }

    /// A run nobody judged is not a run that failed.
    ///
    /// `unknown` means the pass predates the requirement check or never got as far as
    /// making it. Blocking on it would stop every partner drafted before R05 from ever
    /// publishing again, which is a different defect from the one this rule fixes.
    #[test]
    fn a_material_nobody_judged_does_not_block_publication() {
        let claims = vec![claim(1, "3.5", ClaimStatus::SourceSupported)];
        assert_eq!(
            decide(
                &claims,
                &ready(),
                &[source(RequirementsState::Unknown, &[])],
                "aa",
                None
            ),
            PublicationDecision::Publish
        );
    }

    /// One incomplete material among several is enough: a partner's published version
    /// answers as one body of knowledge, and half of it waiting on a person is not a
    /// state worth publishing around.
    #[test]
    fn one_incomplete_material_blocks_even_beside_complete_ones() {
        let claims = vec![claim(1, "3.5", ClaimStatus::SourceSupported)];
        let sources = vec![
            source(RequirementsState::Met, &[]),
            SourceReadiness {
                material_filename: "presentation.pdf".to_owned(),
                requirements: RequirementsState::Unmet,
                missing: vec!["applications: нет задач применения".to_owned()],
            },
        ];
        match decide(&claims, &ready(), &sources, "aa", None) {
            PublicationDecision::Blocked { reasons } => {
                assert!(reasons[0].contains("1 из 2"), "{reasons:?}");
                assert!(reasons[0].contains("presentation.pdf"), "{reasons:?}");
            }
            other => panic!("unexpected decision: {other:?}"),
        }
    }

    /// Rule 0 runs before rule 3, so a partner who is blocked is told they are blocked
    /// rather than told nothing has changed.
    #[test]
    fn an_incomplete_passport_is_reported_as_blocked_and_not_as_unchanged() {
        let claims = vec![claim(1, "3.5", ClaimStatus::SourceSupported)];
        let decision = decide(
            &claims,
            &ready(),
            &[source(RequirementsState::Unmet, &[])],
            "aa",
            Some("aa"),
        );
        assert!(matches!(decision, PublicationDecision::Blocked { .. }));
    }

    #[test]
    fn the_fingerprint_does_not_depend_on_the_order_claims_arrive_in() {
        let a = claim(1, "3.5", ClaimStatus::SourceSupported);
        let b = claim(2, "1200", ClaimStatus::SourceSupported);
        assert_eq!(
            fingerprint(&[a.clone(), b.clone()]),
            fingerprint(&[b, a]),
            "the same knowledge is the same input"
        );
    }

    #[test]
    fn the_fingerprint_changes_when_a_verdict_changes() {
        // A source that moved under a claim turns `source_supported` into `stale`
        // without the candidate row changing. That is a different published version.
        let supported = fingerprint(&[claim(1, "3.5", ClaimStatus::SourceSupported)]);
        let stale = fingerprint(&[claim(1, "3.5", ClaimStatus::Stale)]);
        assert_ne!(supported, stale);
    }

    #[test]
    fn the_fingerprint_changes_when_a_value_changes() {
        assert_ne!(
            fingerprint(&[claim(1, "3.5", ClaimStatus::SourceSupported)]),
            fingerprint(&[claim(1, "9.9", ClaimStatus::SourceSupported)])
        );
    }

    #[test]
    fn the_fingerprint_is_a_sha256_hex_string_the_column_accepts() {
        let value = fingerprint(&[]);
        assert_eq!(value.len(), 64);
        assert!(value
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_uppercase()));
    }

    fn candidate(id: u128, value: &str) -> CandidateClaim {
        CandidateClaim {
            origin: ClaimOrigin::PartnerMaterial,
            origin_id: Uuid::from_u128(id),
            product_name: Some("BP21".to_owned()),
            kind: FactKind::Characteristic,
            attribute: "нагрузка".to_owned(),
            value_text: value.to_owned(),
            unit: None,
            conditions: None,
            model_context: None,
            evidence: vec![crate::claim::CandidateEvidence {
                source_kind: EvidenceSourceKind::Material,
                material_id: Some(Uuid::from_u128(1)),
                material_filename: Some("catalogue.pdf".to_owned()),
                page_number: Some(3),
                region_id: None,
                url: None,
                host: None,
                retrieved_at: None,
                content_hash: None,
                quote: "BP21 3.5 kN".to_owned(),
                char_start: 0,
                char_end: 10,
                source_text: Some("BP21 3.5 kN".to_owned()),
            }],
        }
    }

    #[test]
    fn the_candidate_fingerprint_ignores_order_and_notices_a_new_candidate() {
        let a = candidate(1, "3.5");
        let b = candidate(2, "1200");
        assert_eq!(
            candidate_fingerprint(&[a.clone(), b.clone()]),
            candidate_fingerprint(&[b.clone(), a.clone()])
        );
        assert_ne!(
            candidate_fingerprint(std::slice::from_ref(&a)),
            candidate_fingerprint(&[a, b])
        );
    }

    #[test]
    fn the_candidate_fingerprint_is_not_the_published_one() {
        // They answer different questions and must not be compared with each other: one
        // includes verdicts, the other cannot, because computing a verdict is the check.
        let checked = fingerprint(&[claim(1, "3.5", ClaimStatus::SourceSupported)]);
        let candidates = candidate_fingerprint(&[candidate(1, "3.5")]);
        assert_ne!(checked, candidates);
        assert_eq!(candidates.len(), 64);
        assert!(candidates.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn a_changed_value_changes_the_candidate_fingerprint() {
        assert_ne!(
            candidate_fingerprint(&[candidate(1, "3.5")]),
            candidate_fingerprint(&[candidate(1, "9.9")])
        );
    }

    #[test]
    fn a_snapshot_with_nothing_supported_is_blocked_and_says_why() {
        let claims = vec![claim(1, "3.5", ClaimStatus::Hypothesis)];
        let decision = decide(&claims, &ready(), &ready_sources(), "aa", None);
        let PublicationDecision::Blocked { reasons } = decision else {
            panic!("a version with nothing supported must not publish");
        };
        assert!(
            reasons[0].contains("не подтверждено источником"),
            "{reasons:?}"
        );
    }

    #[test]
    fn a_snapshot_whose_every_topic_is_blocked_does_not_publish() {
        let claims = vec![claim(1, "3.5", ClaimStatus::SourceSupported)];
        let blocked = vec![ReadinessEntry {
            topic: ReadinessTopic::CharacteristicAnswers,
            state: ReadinessState::Blocked,
            reason: "нет".to_owned(),
        }];
        assert!(matches!(
            decide(&claims, &blocked, &ready_sources(), "aa", None),
            PublicationDecision::Blocked { .. }
        ));
    }

    #[test]
    fn a_supported_snapshot_publishes_without_anybody_pressing_anything() {
        let claims = vec![claim(1, "3.5", ClaimStatus::SourceSupported)];
        assert_eq!(
            decide(&claims, &ready(), &ready_sources(), "aa", None),
            PublicationDecision::Publish
        );
    }

    #[test]
    fn re_checking_unchanged_input_does_not_produce_a_second_published_version() {
        // `block-01-spec.md` §13.4: a repeated upload or a repeated press must not create
        // a second published result.
        let claims = vec![claim(1, "3.5", ClaimStatus::SourceSupported)];
        assert_eq!(
            decide(&claims, &ready(), &ready_sources(), "aa", Some("aa")),
            PublicationDecision::Unchanged
        );
        assert_eq!(
            decide(&claims, &ready(), &ready_sources(), "bb", Some("aa")),
            PublicationDecision::Publish
        );
    }

    #[test]
    fn a_blocked_snapshot_is_reported_as_blocked_even_when_its_input_is_unchanged() {
        // Otherwise the owner would be told "ничего не изменилось" about a version that
        // failed the rules, and would never learn which rule.
        let claims = vec![claim(1, "3.5", ClaimStatus::Unknown)];
        assert!(matches!(
            decide(&claims, &ready(), &ready_sources(), "aa", Some("aa")),
            PublicationDecision::Blocked { .. }
        ));
    }
}
