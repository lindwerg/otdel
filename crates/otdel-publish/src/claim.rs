//! What the checker is given, and what it produces.
//!
//! The split between [`CandidateClaim`] and [`CheckedClaim`] carries the phase's central
//! rule the way `otdel_knowledge::candidate` carries 1C's: a [`CheckedClaim`] cannot be
//! constructed without a [`ClaimStatus`], and a [`CheckedEvidence`] cannot be
//! constructed from the candidate's stored quotation — only from a fragment the checker
//! located in the source text *as it is stored today*. There is therefore no path from
//! "1C wrote this down once" to "the published version says so".
//!
//! Note what a candidate deliberately does **not** carry into the checker's reasoning:
//! `model_context`. The drafting model's own explanation of why a fact is right is the
//! one thing a checker must not be persuaded by (`block-01-plan.md`, 1E §1 — "без
//! доверия к объяснениям продуктолога"). It is copied into the published claim, because
//! a reader should still see it labelled as the model's words, and it is never shown to
//! the reviewing model and never affects a verdict.

use chrono::{DateTime, Utc};
use otdel_core::knowledge::FactKind;
use otdel_core::publication::{ClaimOrigin, ClaimStatus, EvidenceSourceKind};
use uuid::Uuid;

/// One candidate statement of 1C or 1D, together with the source text its citations
/// point into **right now**.
///
/// The source text is supplied by the caller rather than fetched here, because this
/// crate does no I/O. That is also what makes every rule in [`crate::check`] testable
/// with a string literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateClaim {
    pub origin: ClaimOrigin,
    /// Identifier of the 1C fact or 1D finding this came from.
    pub origin_id: Uuid,
    /// The partner product this is about. Always `None` for an industry conclusion —
    /// there is no column for it, in the candidate or in the snapshot.
    pub product_name: Option<String>,
    pub kind: FactKind,
    pub attribute: String,
    pub value_text: String,
    pub unit: Option<String>,
    pub conditions: Option<String>,
    /// The drafting model's words. Carried through, never weighed.
    pub model_context: Option<String>,
    pub evidence: Vec<CandidateEvidence>,
}

/// One citation of a candidate, with the text it indexes as stored today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateEvidence {
    pub source_kind: EvidenceSourceKind,
    pub material_id: Option<Uuid>,
    pub material_filename: Option<String>,
    pub page_number: Option<i32>,
    pub region_id: Option<Uuid>,
    pub url: Option<String>,
    pub host: Option<String>,
    pub retrieved_at: Option<DateTime<Utc>>,
    pub content_hash: Option<String>,
    /// The fragment 1C or 1D stored when it drafted the claim.
    pub quote: String,
    pub char_start: i32,
    pub char_end: i32,
    /// The page or snapshot text **as it is stored now**.
    ///
    /// `None` means the row is gone, or holds no text — the source cannot be read, so
    /// nothing about this citation can be checked either way. That is
    /// [`ClaimStatus::Unknown`], and it is deliberately not the same as "the source no
    /// longer says this", which is [`ClaimStatus::Stale`].
    pub source_text: Option<String>,
}

/// A statement with a verdict, ready to be frozen into a version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedClaim {
    pub origin: ClaimOrigin,
    pub origin_id: Uuid,
    pub product_name: Option<String>,
    pub kind: FactKind,
    pub status: ClaimStatus,
    pub attribute: String,
    pub value_text: String,
    pub unit: Option<String>,
    pub conditions: Option<String>,
    pub model_context: Option<String>,
    /// Why this verdict, in words. Shown verbatim by the interface.
    pub check_note: Option<String>,
    /// Never empty: a claim whose citations all failed to verify is either refused or
    /// carries the citations that did verify. The database refuses an empty one anyway.
    pub evidence: Vec<CheckedEvidence>,
}

impl CheckedClaim {
    /// The subject this claim makes a statement about, for contradiction detection.
    ///
    /// `None` when there is no product to attribute it to, which is every industry
    /// conclusion. Two statements can only contradict each other when they are about the
    /// same product and the same property; see [`crate::check`] for why that is narrower
    /// than it could be.
    pub fn subject(&self) -> Option<(String, String)> {
        let product = self.product_name.as_ref()?;
        Some((
            crate::chunk::normalise(product),
            crate::chunk::normalise(&self.attribute),
        ))
    }
}

/// A citation that the checker located in the source text itself.
///
/// `quote`, `char_start` and `char_end` are what the checker *found*, not what the
/// candidate claimed. When a page was re-read and its text moved, the offsets are
/// repaired here — which is 1C's known limitation 8 being closed at the moment it
/// matters, because a published version must not carry a citation that points at the
/// wrong place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedEvidence {
    pub source_kind: EvidenceSourceKind,
    pub material_id: Option<Uuid>,
    pub material_filename: Option<String>,
    pub page_number: Option<i32>,
    pub region_id: Option<Uuid>,
    pub url: Option<String>,
    pub host: Option<String>,
    pub retrieved_at: Option<DateTime<Utc>>,
    pub content_hash: Option<String>,
    pub quote: String,
    pub char_start: i32,
    pub char_end: i32,
}

/// Everything one check of one partner concluded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckOutcome {
    pub claims: Vec<CheckedClaim>,
    /// Candidates that are not in the version at all, and how many.
    pub rejected: u32,
    pub rejections: Vec<String>,
}

/// Upper bound on the refusal sentences kept, matching 1C and 1D.
pub const MAX_REJECTION_LINES: usize = 40;

impl CheckOutcome {
    /// Record one refusal: the candidate is not in the version.
    pub fn reject(&mut self, reason: impl Into<String>) {
        self.rejected = self.rejected.saturating_add(1);
        self.note(reason);
    }

    /// Record one sentence without counting a refusal — a claim that was kept with a
    /// lowered verdict is not a rejection, and counting it as one would make the run
    /// look worse than it is.
    pub fn note(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        if self.rejections.len() >= MAX_REJECTION_LINES || self.rejections.contains(&reason) {
            return;
        }
        self.rejections.push(reason);
    }

    pub fn count(&self, status: ClaimStatus) -> i32 {
        i32::try_from(self.claims.iter().filter(|c| c.status == status).count()).unwrap_or(i32::MAX)
    }

    /// Is anything in here usable as the basis of an answer?
    pub fn has_supported(&self) -> bool {
        self.claims.iter().any(|c| c.status.is_answerable())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(product: Option<&str>, attribute: &str) -> CheckedClaim {
        CheckedClaim {
            origin: ClaimOrigin::PartnerMaterial,
            origin_id: Uuid::from_u128(1),
            product_name: product.map(str::to_owned),
            kind: FactKind::Characteristic,
            status: ClaimStatus::SourceSupported,
            attribute: attribute.to_owned(),
            value_text: "3.5".to_owned(),
            unit: None,
            conditions: None,
            model_context: None,
            check_note: None,
            evidence: Vec::new(),
        }
    }

    #[test]
    fn a_subject_folds_case_and_spacing_so_one_product_is_one_subject() {
        let a = claim(Some("BP21"), "Нагрузка");
        let b = claim(Some("  bp21 "), "нагрузка");
        assert_eq!(a.subject(), b.subject());
    }

    #[test]
    fn an_industry_conclusion_has_no_subject_to_contradict() {
        // It names no product, so there is nothing for it to disagree with *about a
        // product* — which is the only kind of contradiction this phase claims to find.
        assert_eq!(claim(None, "толщина").subject(), None);
    }

    #[test]
    fn a_repeated_refusal_is_recorded_once_and_counted_twice() {
        let mut outcome = CheckOutcome::default();
        outcome.reject("цитата не найдена");
        outcome.reject("цитата не найдена");
        assert_eq!(outcome.rejected, 2, "both candidates really were refused");
        assert_eq!(
            outcome.rejections.len(),
            1,
            "the owner does not need the same sentence twice"
        );
    }

    #[test]
    fn a_lowered_verdict_is_a_note_and_not_a_rejection() {
        // The claim is still published, with its verdict on it. Counting it as rejected
        // would make "отклонено" mean two different things.
        let mut outcome = CheckOutcome::default();
        outcome.note("значение не найдено в цитате — понижено до гипотезы");
        assert_eq!(outcome.rejected, 0);
        assert_eq!(outcome.rejections.len(), 1);
    }

    #[test]
    fn refusal_sentences_are_bounded() {
        let mut outcome = CheckOutcome::default();
        for index in 0..(MAX_REJECTION_LINES * 2) {
            outcome.reject(format!("причина {index}"));
        }
        assert_eq!(outcome.rejections.len(), MAX_REJECTION_LINES);
        assert_eq!(outcome.rejected, (MAX_REJECTION_LINES * 2) as u32);
    }

    #[test]
    fn only_a_supported_claim_makes_an_outcome_answerable() {
        let mut outcome = CheckOutcome::default();
        let mut hypothesis = claim(Some("BP21"), "нагрузка");
        hypothesis.status = ClaimStatus::Hypothesis;
        outcome.claims.push(hypothesis);
        assert!(!outcome.has_supported());

        outcome.claims.push(claim(Some("BP21"), "длина"));
        assert!(outcome.has_supported());
    }
}
