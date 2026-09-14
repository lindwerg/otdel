//! What survives validation — the only thing the database layer ever sees.
//!
//! These types differ from [`crate::schema`]'s in one decisive way: a
//! [`ResolvedEvidence`] cannot be constructed from a model's words. It is produced by
//! [`crate::validate`] from a page that belongs to this run, carrying the page's own
//! wording and the offsets where it sits. There is therefore no path from "the model
//! said so" to a stored fact — the type system carries the rule.

use otdel_core::knowledge::{CategoryKind, FactKind, ProductKind, QuestionAudience};
use otdel_core::passport::{
    AliasRelation, ApplicationDetailKind, DeclarationOrigin, DeclarationTopic, GapNature,
    SynonymRelation,
};
use uuid::Uuid;

/// A fragment of a real page of this run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEvidence {
    pub page_id: Uuid,
    pub material_id: Uuid,
    pub page_number: i32,
    /// The page's own wording, not the model's.
    pub quote: String,
    pub char_start: i32,
    pub char_end: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateCategory {
    /// Response-local reference, namespaced per request when several are merged.
    pub reference: String,
    pub kind: CategoryKind,
    pub name: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateProduct {
    pub reference: String,
    pub category_ref: Option<String>,
    pub kind: ProductKind,
    pub name: String,
    pub summary: Option<String>,
    /// Other names the material uses for this product. Recorded, never merged: the
    /// relation says how far the claim goes, and `unclear` keeps a resemblance from
    /// silently becoming an identity.
    pub aliases: Vec<CandidateAlias>,
}

/// A surface form of a product, with the fragment that shows it.
///
/// Evidence is a single resolved fragment rather than a list: a surface form is a claim
/// about one place in the document, and "it is written this way somewhere" is exactly the
/// unfalsifiable shape this package exists to remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateAlias {
    pub surface: String,
    pub relation: AliasRelation,
    pub note: Option<String>,
    pub evidence: ResolvedEvidence,
}

/// A surface form of a glossary term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateSynonym {
    pub surface: String,
    pub relation: SynonymRelation,
    pub evidence: ResolvedEvidence,
}

/// A further reading of the same term inside one material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateSense {
    /// Short disambiguator: «в контексте кабельных лотков». Not a number.
    pub label: String,
    pub definition: String,
    pub definition_is_model_context: bool,
    pub evidence: ResolvedEvidence,
}

/// One parameter, constraint or question under an application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateApplicationDetail {
    pub kind: ApplicationDetailKind,
    pub label: String,
    /// `None` only for a question, which asserts nothing.
    pub value_text: Option<String>,
    pub unit: Option<String>,
    /// `Some` only for a question.
    pub audience: Option<QuestionAudience>,
    /// Required for a parameter or a constraint; a question may carry none.
    pub evidence: Option<ResolvedEvidence>,
}

/// A task the material says a product serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateApplication {
    pub product_ref: Option<String>,
    /// The task in the buyer's words.
    pub task: String,
    pub summary: Option<String>,
    /// The model's own framing. Never presented as a quotation.
    pub model_context: Option<String>,
    pub evidence: ResolvedEvidence,
    pub details: Vec<CandidateApplicationDetail>,
}

/// An explicit "there is none", in the run's own words.
///
/// The reason this is a row and not a flag: a flag records that somebody ticked a box,
/// while a sentence records *why*, and a reader can disagree with a sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateDeclaration {
    pub topic: DeclarationTopic,
    pub stated: String,
    pub origin: DeclarationOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateFact {
    pub product_ref: Option<String>,
    pub kind: FactKind,
    pub attribute: String,
    pub value_text: String,
    /// Kept only when the unit is literally present in the value or in a quote.
    pub unit: Option<String>,
    /// Kept only when the conditions are literally present in a quote; otherwise the
    /// text is moved into `model_context`, where it is shown as the model's wording.
    pub conditions: Option<String>,
    pub model_context: Option<String>,
    /// Never empty: a fact without evidence does not reach this type.
    pub evidence: Vec<ResolvedEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateTerm {
    pub term: String,
    pub definition: String,
    /// `true` when the definition is the model's wording rather than the source's.
    pub definition_is_model_context: bool,
    pub evidence: Vec<ResolvedEvidence>,
    /// Further readings of the same word in this material. A catalogue that uses
    /// «консоль» for a bracket in one section and for a load scheme in another has two,
    /// and one definition would make the second usage wrong or invisible.
    pub senses: Vec<CandidateSense>,
    pub synonyms: Vec<CandidateSynonym>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateQa {
    pub question: String,
    pub answer: String,
    /// `true` when the answer is the model's own sentence rather than the source's
    /// wording — which is the usual case, an answer being a synthesis.
    pub answer_is_model_context: bool,
    pub evidence: Vec<ResolvedEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateQuestion {
    pub audience: QuestionAudience,
    pub text: String,
}

/// A gap needs no evidence: it records what the material does *not* say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateGap {
    pub product_ref: Option<String>,
    pub topic: String,
    pub missing: String,
    pub blocks: Option<String>,
    pub question: Option<CandidateQuestion>,
    /// Which kind of unknown this is. Classified by the run, never inferred from the
    /// topic's wording.
    pub nature: GapNature,
}

/// Products an earlier pass accepted, offered to a later one by a stable label.
///
/// R05.2. The passes after the inventory need to say *which* product a fact, a task or a
/// gap is about. Making them re-declare the products would spend their budget on exactly
/// the thing that crowded everything else out of the single omnibus request; so the server
/// hands them a short list instead — `P1 — AP10`, `P2 — AP20` — and they cite the label.
///
/// The label is the server's, never the model's. A label the model invents resolves to
/// nothing and the candidate is attached to no product, which is the same refusal a made-up
/// source label already gets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KnownProducts {
    /// `(label, candidate reference, name)`.
    entries: Vec<(String, String, String)>,
}

impl KnownProducts {
    /// Label every product a draft has accepted so far, in a stable order.
    pub fn from_draft(draft: &CandidateDraft) -> Self {
        let entries = draft
            .products
            .iter()
            .enumerate()
            .map(|(index, product)| {
                (
                    format!("P{}", index + 1),
                    product.reference.clone(),
                    product.name.clone(),
                )
            })
            .collect();
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `(label, name)` pairs for the prompt.
    pub fn listing(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .iter()
            .map(|(label, _, name)| (label.as_str(), name.as_str()))
    }

    /// The candidate reference a label stands for.
    ///
    /// Matched on the label first and on the product's name second — a model that writes
    /// the name instead of the label has still named something real, and refusing that
    /// would lose a correct attribution over a formatting slip. An unknown string resolves
    /// to `None` and the candidate is stored attached to nothing.
    pub fn resolve(&self, reference: &str) -> Option<String> {
        let wanted = reference.trim();
        if let Some((_, candidate, _)) = self
            .entries
            .iter()
            .find(|(label, _, _)| label.eq_ignore_ascii_case(wanted))
        {
            return Some(candidate.clone());
        }

        let folded = normalise_name(wanted);
        let mut named = self
            .entries
            .iter()
            .filter(|(_, _, name)| normalise_name(name) == folded);
        let first = named.next()?;
        // Two products of the same name: naming one of them would be a coin toss.
        if named.next().is_some() {
            return None;
        }
        Some(first.1.clone())
    }
}

/// Everything one run produced, plus an honest account of what it refused.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CandidateDraft {
    pub categories: Vec<CandidateCategory>,
    pub products: Vec<CandidateProduct>,
    pub facts: Vec<CandidateFact>,
    pub terms: Vec<CandidateTerm>,
    pub qa: Vec<CandidateQa>,
    pub gaps: Vec<CandidateGap>,
    pub applications: Vec<CandidateApplication>,
    /// Topics this run stated are empty, with the words on the record. Never inferred
    /// from an empty array: that inference is the whole failure this package answers.
    pub declarations: Vec<CandidateDeclaration>,
    /// How many candidates were refused outright.
    pub rejected: u32,
    /// Why, in words, deduplicated and bounded. Shown to the owner as-is.
    pub rejections: Vec<String>,
}

/// Upper bound on stored rejection lines: the reasons repeat, and a run record is not
/// a log file.
pub const MAX_REJECTION_LINES: usize = 40;

impl CandidateDraft {
    pub fn is_empty(&self) -> bool {
        self.categories.is_empty()
            && self.products.is_empty()
            && self.facts.is_empty()
            && self.terms.is_empty()
            && self.qa.is_empty()
            && self.gaps.is_empty()
            && self.applications.is_empty()
            && self.declarations.is_empty()
    }

    /// Reduce the draft to the counts the requirement check asks about.
    ///
    /// Lives here rather than in `coverage` so the same draft can be judged before it is
    /// stored and after — the rule reads a [`crate::coverage::DraftSnapshot`], and this is
    /// the in-memory way to build one.
    ///
    /// `context` carries the two numbers a draft cannot know about itself: how much of the
    /// material this pass actually processed, and how many readings it could not settle.
    /// Both decide whether a declaration of absence may stand, so leaving them out would
    /// let "there is none" clear a topic it has no standing to clear.
    pub fn snapshot(&self, context: crate::coverage::RunContext) -> crate::coverage::DraftSnapshot {
        crate::coverage::DraftSnapshot {
            products_total: self.products.len(),
            products_with_summary: self
                .products
                .iter()
                .filter(|product| product.summary.is_some())
                .count(),
            applications_total: self.applications.len(),
            terms_total: self.terms.len(),
            // A prepared question reaches the record either on its own gap or under an
            // application; both are questions somebody has to be asked, and counting only
            // the first would let a run pass by moving them.
            questions_total: self
                .gaps
                .iter()
                .filter(|gap| gap.question.is_some())
                .count()
                + self
                    .applications
                    .iter()
                    .flat_map(|application| application.details.iter())
                    .filter(|detail| detail.kind == ApplicationDetailKind::Question)
                    .count(),
            commercial_gaps: self.count_gaps(GapNature::Commercial),
            technical_gaps: self.count_gaps(GapNature::Technical),
            // What the material *states* commercially. "Nothing commercial is missing"
            // is only sayable when something commercial is present, so the check needs
            // the facts and not only the gaps.
            commercial_facts: self
                .facts
                .iter()
                .filter(|fact| fact.kind == FactKind::Commercial)
                .count(),
            declared: self
                .declarations
                .iter()
                .map(|declaration| declaration.topic)
                .collect(),
            pages_processed: context.pages_processed,
            open_uncertainties: context.open_uncertainties,
            purposes_covered: context.purposes_covered.clone(),
        }
    }

    fn count_gaps(&self, nature: GapNature) -> usize {
        self.gaps.iter().filter(|gap| gap.nature == nature).count()
    }

    /// Record an explicit absence, keeping the first statement on a topic.
    ///
    /// First rather than last on purpose: a server-side declaration is only ever added
    /// after the model's, and the model's own words are the more informative of the two.
    pub fn declare(&mut self, declaration: CandidateDeclaration) {
        if self
            .declarations
            .iter()
            .any(|kept| kept.topic == declaration.topic)
        {
            return;
        }
        self.declarations.push(declaration);
    }

    /// Drop declarations the finished draft contradicts.
    ///
    /// R05.2 moved this check from the response to the run. When one omnibus request
    /// produced everything, "there are no terms" beside eleven terms was a contradiction
    /// visible inside that one response, and [`crate::validate`] caught it there. Now the
    /// terms arrive from the glossary pass and the declaration from the inquiry pass, so
    /// no single response contains both — and the only place the two can still be
    /// compared is here, once every pass has run.
    ///
    /// Called at the end of a run. The per-response check stays where it is: it is the
    /// same rule seen earlier, and catching a contradiction sooner costs nothing.
    pub fn reconcile_declarations(&mut self) {
        let contradicted: Vec<DeclarationTopic> = self
            .declarations
            .iter()
            .map(|declaration| declaration.topic)
            .filter(|topic| self.produced_rows_for(*topic))
            .collect();

        for topic in contradicted {
            self.declarations.retain(|kept| kept.topic != topic);
            self.reject(format!(
                "заявление «в материале этого нет» по теме «{}» отклонено: разбор в целом \
                 такие записи всё-таки дал",
                topic.as_str()
            ));
        }
    }

    /// Did this draft produce anything on the topic a declaration calls empty?
    fn produced_rows_for(&self, topic: DeclarationTopic) -> bool {
        match topic {
            DeclarationTopic::Glossary => !self.terms.is_empty(),
            DeclarationTopic::Applications => !self.applications.is_empty(),
            DeclarationTopic::Questions => self.gaps.iter().any(|gap| gap.question.is_some()),
            DeclarationTopic::CommercialUnknowns => self
                .gaps
                .iter()
                .any(|gap| gap.nature == GapNature::Commercial),
            DeclarationTopic::TechnicalUnknowns => self
                .gaps
                .iter()
                .any(|gap| gap.nature == GapNature::Technical),
        }
    }

    /// Record one refusal. Repeated reasons are counted once.
    pub fn reject(&mut self, reason: impl Into<String>) {
        self.rejected = self.rejected.saturating_add(1);
        self.note(reason);
    }

    /// Record something the owner should know that is not itself a refusal (a unit
    /// that could not be confirmed, a page that did not fit the budget).
    pub fn note(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        if self.rejections.len() >= MAX_REJECTION_LINES || self.rejections.contains(&reason) {
            return;
        }
        self.rejections.push(reason);
    }

    /// Merge the result of another request of the same run.
    ///
    /// References are response-local, so the caller namespaces them before merging
    /// (`b2:p1`). Categories and products that repeat by kind and normalised name are
    /// folded into the first occurrence and the later references are remapped — the
    /// same profile mentioned on two pages is one product, but two *different* names
    /// are never merged (`docs/block-01-spec.md` §6.6: synonyms do not merge products
    /// without proof).
    pub fn merge(&mut self, other: Self) {
        let mut remap: Vec<(String, String)> = Vec::new();

        for category in other.categories {
            match self
                .categories
                .iter()
                .find(|kept| kept.kind == category.kind && same_name(&kept.name, &category.name))
            {
                Some(kept) => remap.push((category.reference.clone(), kept.reference.clone())),
                None => self.categories.push(category),
            }
        }

        for mut product in other.products {
            if let Some(target) = product
                .category_ref
                .as_ref()
                .and_then(|reference| lookup(&remap, reference))
            {
                product.category_ref = Some(target);
            }
            match self
                .products
                .iter_mut()
                .find(|kept| kept.kind == product.kind && same_name(&kept.name, &product.name))
            {
                Some(kept) => {
                    // The same product named on two pages is one product with two pages'
                    // worth of aliases — dropping the second batch would lose a surface
                    // form purely because of which request happened to see it.
                    for alias in product.aliases {
                        if !kept
                            .aliases
                            .iter()
                            .any(|known| same_name(&known.surface, &alias.surface))
                        {
                            kept.aliases.push(alias);
                        }
                    }
                    if kept.summary.is_none() {
                        kept.summary = product.summary;
                    }
                    remap.push((product.reference.clone(), kept.reference.clone()));
                }
                None => self.products.push(product),
            }
        }

        for mut fact in other.facts {
            if let Some(target) = fact
                .product_ref
                .as_ref()
                .and_then(|reference| lookup(&remap, reference))
            {
                fact.product_ref = Some(target);
            }

            // The same statement found on a second page is not a second fact: it is
            // the same fact with more evidence. Folding them keeps the draft readable
            // and keeps both citations.
            match self
                .facts
                .iter_mut()
                .find(|kept| same_statement(kept, &fact))
            {
                Some(kept) => {
                    for evidence in fact.evidence {
                        let known = kept.evidence.iter().any(|existing| {
                            existing.page_id == evidence.page_id
                                && existing.char_start == evidence.char_start
                        });
                        if !known {
                            kept.evidence.push(evidence);
                        }
                    }
                }
                None => self.facts.push(fact),
            }
        }

        for mut gap in other.gaps {
            if let Some(target) = gap
                .product_ref
                .as_ref()
                .and_then(|reference| lookup(&remap, reference))
            {
                gap.product_ref = Some(target);
            }
            self.gaps.push(gap);
        }

        for mut application in other.applications {
            if let Some(target) = application
                .product_ref
                .as_ref()
                .and_then(|reference| lookup(&remap, reference))
            {
                application.product_ref = Some(target);
            }
            // Two requests describing the same task for the same product is one task.
            // Merged by product and task text only: the details differ per page, and
            // keeping both sets is the point.
            match self.applications.iter_mut().find(|kept| {
                kept.product_ref == application.product_ref
                    && same_name(&kept.task, &application.task)
            }) {
                Some(kept) => {
                    for detail in application.details {
                        if !kept.details.iter().any(|known| {
                            known.kind == detail.kind && same_name(&known.label, &detail.label)
                        }) {
                            kept.details.push(detail);
                        }
                    }
                }
                None => self.applications.push(application),
            }
        }

        // The same term drafted from two requests is one term carrying both readings.
        for term in other.terms {
            match self
                .terms
                .iter_mut()
                .find(|kept| same_name(&kept.term, &term.term))
            {
                Some(kept) => {
                    for sense in term.senses {
                        if !kept
                            .senses
                            .iter()
                            .any(|known| same_name(&known.label, &sense.label))
                        {
                            kept.senses.push(sense);
                        }
                    }
                    for synonym in term.synonyms {
                        if !kept
                            .synonyms
                            .iter()
                            .any(|known| same_name(&known.surface, &synonym.surface))
                        {
                            kept.synonyms.push(synonym);
                        }
                    }
                    for evidence in term.evidence {
                        let known = kept.evidence.iter().any(|existing| {
                            existing.page_id == evidence.page_id
                                && existing.char_start == evidence.char_start
                        });
                        if !known {
                            kept.evidence.push(evidence);
                        }
                    }
                }
                None => self.terms.push(term),
            }
        }

        for declaration in other.declarations {
            self.declare(declaration);
        }

        self.qa.extend(other.qa);
        self.rejected = self.rejected.saturating_add(other.rejected);
        for reason in other.rejections {
            self.note(reason);
        }
    }
}

/// Same subject, same property, same value — and therefore the same statement.
///
/// The unit and the conditions are part of the statement: `3.5 kN` and `3.5 т` are not
/// the same fact, and neither are two loads that hold under different conditions.
fn same_statement(left: &CandidateFact, right: &CandidateFact) -> bool {
    left.product_ref == right.product_ref
        && left.kind == right.kind
        && same_name(&left.attribute, &right.attribute)
        && same_name(&left.value_text, &right.value_text)
        && left.unit.as_deref().map(normalise_name) == right.unit.as_deref().map(normalise_name)
        && left.conditions.as_deref().map(normalise_name)
            == right.conditions.as_deref().map(normalise_name)
}

fn lookup(remap: &[(String, String)], reference: &str) -> Option<String> {
    remap
        .iter()
        .find(|(from, _)| from == reference)
        .map(|(_, to)| to.clone())
}

/// Case- and whitespace-insensitive name comparison. Nothing stronger: a different
/// spelling is a different product until something proves otherwise.
fn same_name(left: &str, right: &str) -> bool {
    normalise_name(left) == normalise_name(right)
}

pub(crate) fn normalise_name(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn product(reference: &str, name: &str) -> CandidateProduct {
        CandidateProduct {
            reference: reference.to_owned(),
            category_ref: None,
            kind: ProductKind::Product,
            name: name.to_owned(),
            summary: None,
            aliases: Vec::new(),
        }
    }

    fn fact(product_ref: &str, attribute: &str) -> CandidateFact {
        CandidateFact {
            product_ref: Some(product_ref.to_owned()),
            kind: FactKind::Characteristic,
            attribute: attribute.to_owned(),
            value_text: "3.5".to_owned(),
            unit: None,
            conditions: None,
            model_context: None,
            evidence: vec![ResolvedEvidence {
                page_id: Uuid::from_u128(1),
                material_id: Uuid::from_u128(2),
                page_number: 1,
                quote: "BP21 1200 3.5".to_owned(),
                char_start: 0,
                char_end: 13,
            }],
        }
    }

    #[test]
    fn merging_folds_the_same_product_and_remaps_its_facts() {
        let mut first = CandidateDraft {
            products: vec![product("b1:p1", "BP21")],
            facts: vec![fact("b1:p1", "длина")],
            ..CandidateDraft::default()
        };
        first.merge(CandidateDraft {
            products: vec![product("b2:p1", " bp21 ")],
            facts: vec![fact("b2:p1", "нагрузка")],
            ..CandidateDraft::default()
        });

        assert_eq!(first.products.len(), 1, "the same name is one product");
        assert_eq!(first.facts.len(), 2);
        assert!(first
            .facts
            .iter()
            .all(|fact| fact.product_ref.as_deref() == Some("b1:p1")));
    }

    #[test]
    fn two_different_names_stay_two_products() {
        let mut first = CandidateDraft {
            products: vec![product("b1:p1", "BP21")],
            ..CandidateDraft::default()
        };
        first.merge(CandidateDraft {
            products: vec![product("b2:p1", "BP21D")],
            ..CandidateDraft::default()
        });
        assert_eq!(first.products.len(), 2);
    }

    #[test]
    fn rejections_are_counted_once_and_bounded() {
        let mut draft = CandidateDraft::default();
        for _ in 0..5 {
            draft.reject("источник S9 не входит в этот материал");
        }
        assert_eq!(draft.rejected, 5, "every refused candidate is counted");
        assert_eq!(draft.rejections.len(), 1, "the reason is recorded once");

        for index in 0..100 {
            draft.note(format!("причина {index}"));
        }
        assert_eq!(draft.rejections.len(), MAX_REJECTION_LINES);
    }
}
