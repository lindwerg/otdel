//! Turning validated candidates into what the storage layer stores.
//!
//! Split from [`crate::knowledge`] because the two do different jobs: that file runs one
//! understanding job — claim, read, call, store, settle — and this one is the translation
//! layer between what `otdel-knowledge` validated and what `otdel-db` writes.
//!
//! The translation is deliberately plain. Every rule has already been applied by the time
//! a candidate reaches here, and a mapping that decided anything would be a second place
//! to look for the rules. There are exactly two exceptions, and both delegate:
//!
//! * [`fact_origin`] asks [`tables::confirm_against_cells`] whether a usable table cell
//!   says the same thing as an accepted fact. `None` is its common and truthful answer.
//! * [`collect_uncertainties`] gathers what nobody may read as a value, from two sources
//!   kept apart on purpose — a cell R03 could not settle, and a page nobody could read.

use otdel_core::passport::{FactOrigin, StructuralSource, UncertaintyKind};
use otdel_db::knowledge::{
    NewCategory, NewDraft, NewEvidence, NewFact, NewGap, NewProduct, NewQa, NewQuestion, NewTerm,
};
use otdel_db::passport::{
    NewAlias, NewApplication, NewApplicationDetail, NewDeclaration, NewPageCoverage, NewSense,
    NewSynonym, NewUncertainty,
};
use otdel_knowledge::{tables, CandidateDraft, CoveragePlan, StructuredCell, TableReading};
use uuid::Uuid;

/// Map validated candidates onto the storage layer's input.
///
/// A plain translation on purpose: every rule has already been applied, and a mapping
/// that decided anything would be a second place to look for the rules. The one thing it
/// *does* do is ask each fact whether a usable table cell says the same thing —
/// [`tables::confirm_against_cells`] owns that rule, and `None` is its common and
/// truthful answer.
pub(crate) fn to_new_draft(draft: &CandidateDraft, structured: &[StructuredCell]) -> NewDraft {
    let product_name = |reference: &Option<String>| -> Option<String> {
        let reference = reference.as_deref()?;
        draft
            .products
            .iter()
            .find(|product| product.reference == reference)
            .map(|product| product.name.clone())
    };

    NewDraft {
        categories: draft
            .categories
            .iter()
            .map(|category| NewCategory {
                reference: category.reference.clone(),
                kind: category.kind,
                name: category.name.clone(),
                summary: category.summary.clone(),
            })
            .collect(),
        products: draft
            .products
            .iter()
            .map(|product| NewProduct {
                reference: product.reference.clone(),
                category_ref: product.category_ref.clone(),
                kind: product.kind,
                name: product.name.clone(),
                summary: product.summary.clone(),
                aliases: product
                    .aliases
                    .iter()
                    .map(|alias| NewAlias {
                        surface: alias.surface.clone(),
                        relation: alias.relation,
                        note: alias.note.clone(),
                        evidence: evidence(&alias.evidence),
                    })
                    .collect(),
            })
            .collect(),
        facts: draft
            .facts
            .iter()
            .map(|fact| NewFact {
                product_ref: fact.product_ref.clone(),
                kind: fact.kind,
                attribute: fact.attribute.clone(),
                value_text: fact.value_text.clone(),
                unit: fact.unit.clone(),
                conditions: fact.conditions.clone(),
                model_context: fact.model_context.clone(),
                evidence: fact.evidence.iter().map(evidence).collect(),
                origin: fact_origin(
                    product_name(&fact.product_ref).as_deref(),
                    &fact.attribute,
                    &fact.value_text,
                    structured,
                ),
            })
            .collect(),
        terms: draft
            .terms
            .iter()
            .map(|term| NewTerm {
                term: term.term.clone(),
                definition: term.definition.clone(),
                definition_is_model_context: term.definition_is_model_context,
                evidence: term.evidence.iter().map(evidence).collect(),
                senses: term
                    .senses
                    .iter()
                    .map(|sense| NewSense {
                        label: sense.label.clone(),
                        definition: sense.definition.clone(),
                        definition_is_model_context: sense.definition_is_model_context,
                        evidence: evidence(&sense.evidence),
                    })
                    .collect(),
                synonyms: term
                    .synonyms
                    .iter()
                    .map(|synonym| NewSynonym {
                        surface: synonym.surface.clone(),
                        relation: synonym.relation,
                        evidence: evidence(&synonym.evidence),
                    })
                    .collect(),
            })
            .collect(),
        qa: draft
            .qa
            .iter()
            .map(|entry| NewQa {
                question: entry.question.clone(),
                answer: entry.answer.clone(),
                answer_is_model_context: entry.answer_is_model_context,
                evidence: entry.evidence.iter().map(evidence).collect(),
            })
            .collect(),
        gaps: draft
            .gaps
            .iter()
            .map(|gap| NewGap {
                product_ref: gap.product_ref.clone(),
                topic: gap.topic.clone(),
                missing: gap.missing.clone(),
                blocks: gap.blocks.clone(),
                nature: gap.nature,
                question: gap.question.as_ref().map(|question| NewQuestion {
                    audience: question.audience,
                    text: question.text.clone(),
                }),
            })
            .collect(),
        applications: draft
            .applications
            .iter()
            .map(|application| NewApplication {
                product_ref: application.product_ref.clone(),
                task: application.task.clone(),
                summary: application.summary.clone(),
                model_context: application.model_context.clone(),
                evidence: evidence(&application.evidence),
                details: application
                    .details
                    .iter()
                    .map(|detail| NewApplicationDetail {
                        kind: detail.kind,
                        label: detail.label.clone(),
                        value_text: detail.value_text.clone(),
                        unit: detail.unit.clone(),
                        audience: detail.audience,
                        evidence: detail.evidence.as_ref().map(evidence),
                    })
                    .collect(),
            })
            .collect(),
        declarations: draft
            .declarations
            .iter()
            .map(|declaration| NewDeclaration {
                topic: declaration.topic,
                stated: declaration.stated.clone(),
                origin: declaration.origin,
            })
            .collect(),
    }
}

/// Which structure this fact's value sat in.
///
/// A cell that agrees with the value, the product and the property makes the fact
/// table-derived and copies what the table said it was about. Nothing else does — a fact
/// read out of running prose stays `page_text`, which is not a lesser answer but a
/// different and equally checkable one.
pub(crate) fn fact_origin(
    product_name: Option<&str>,
    attribute: &str,
    value: &str,
    structured: &[StructuredCell],
) -> FactOrigin {
    match tables::confirm_against_cells(product_name, attribute, value, structured) {
        Some(confirmation) => FactOrigin {
            source: StructuralSource::TableCell,
            cell_id: Some(confirmation.cell_id),
            subject: Some(confirmation.subject),
            property: Some(confirmation.property),
            unit: confirmation.unit,
            conditions: confirmation.conditions,
        },
        None => FactOrigin::default(),
    }
}

/// The run's page account as storable rows.
pub(crate) fn page_coverage_rows(plan: &CoveragePlan) -> Vec<NewPageCoverage> {
    plan.pages()
        .iter()
        .map(|page| NewPageCoverage {
            page_id: page.page_id,
            page_number: page.page_number,
            disposition: page.disposition,
            offered: page.offered,
            chars_sent: page.chars_sent,
            batch_index: page.batch_index,
            // Never synthesised here: the plan already attaches the disposition's own
            // sentence to every page it did not process, and inventing a second wording
            // at the storage boundary would make two answers to "why is page 37 missing".
            reason: page.reason.clone(),
        })
        .collect()
}

/// What the material states that nobody may read as a value.
///
/// Two sources, kept apart because they are different problems: a table cell R03 could
/// not settle, and a page nobody could read at all. The second is *also* in the coverage
/// account, and deliberately so — the account answers "was this page processed", and this
/// answers "what is on it", and a reader of a passport needs the second one.
pub(crate) fn collect_uncertainties(
    readings: &[(Uuid, i32, TableReading)],
    plan: &CoveragePlan,
) -> Vec<NewUncertainty> {
    let mut out: Vec<NewUncertainty> = readings
        .iter()
        .flat_map(|(_, _, reading)| reading.uncertainties.iter())
        .map(|item| NewUncertainty {
            product_id: None,
            kind: item.kind,
            subject: item.subject.clone(),
            detail: format!("{} (ячеек: {})", item.detail, item.cell_count),
            reasons: item.reasons.clone(),
            quote: item.example.clone(),
            page_id: Some(item.page_id),
            page_number: Some(item.page_number),
            region_id: Some(item.region_id),
        })
        .collect();

    out.extend(
        plan.pages()
            .iter()
            .filter(|page| page.disposition.is_unreadable())
            .map(|page| NewUncertainty {
                product_id: None,
                kind: UncertaintyKind::UnreadablePage,
                subject: format!("страница {}", page.page_number),
                detail: format!(
                    "{} — что написано на этой странице, по этому материалу неизвестно",
                    page.disposition.describe()
                ),
                reasons: vec![page.disposition.as_str().to_owned()],
                quote: None,
                page_id: Some(page.page_id),
                page_number: Some(page.page_number),
                region_id: None,
            }),
    );

    out
}

pub(crate) fn evidence(resolved: &otdel_knowledge::ResolvedEvidence) -> NewEvidence {
    NewEvidence {
        page_id: resolved.page_id,
        page_number: resolved.page_number,
        quote: resolved.quote.clone(),
        char_start: resolved.char_start,
        char_end: resolved.char_end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use otdel_core::extraction::{MaterialPage, PageStatus, TextSource};
    use otdel_core::extraction_context::DiagramInterpretation;
    use otdel_core::knowledge::{CategoryKind, FactKind, ProductKind, QuestionAudience};
    use otdel_core::passport::{
        AliasRelation, ApplicationDetailKind, DeclarationOrigin, DeclarationTopic, GapNature,
        PageDisposition,
    };
    use otdel_knowledge::{
        CandidateAlias, CandidateApplication, CandidateApplicationDetail, CandidateCategory,
        CandidateDeclaration, CandidateFact, CandidateGap, CandidateProduct, CandidateQuestion,
        ProcessedPage, ResolvedEvidence,
    };

    fn quote(page: u128, text: &str) -> ResolvedEvidence {
        ResolvedEvidence {
            page_id: Uuid::from_u128(page),
            material_id: Uuid::from_u128(600),
            page_number: i32::try_from(page).unwrap(),
            quote: text.to_owned(),
            char_start: 12,
            char_end: 12 + i32::try_from(text.chars().count()).unwrap(),
        }
    }

    fn page(number: i32, status: PageStatus) -> MaterialPage {
        MaterialPage {
            id: Uuid::from_u128(u128::try_from(number).unwrap()),
            material_id: Uuid::from_u128(600),
            page_number: number,
            status,
            text_source: TextSource::TextLayer,
            char_count: 120,
            word_count: 20,
            image_count: 0,
            width_pt: None,
            height_pt: None,
            rotation: 0,
            parser_name: None,
            parser_version: None,
            ocr_engine: None,
            ocr_version: None,
            ocr_language: None,
            duration_ms: None,
            attempts: 1,
            diagnostic: None,
            extracted_at: None,
            region_count: 0,
            table_count: 0,
            extraction_revision: None,
            drawing_count: 0,
            diagram_interpretation: DiagramInterpretation::None,
        }
    }

    fn full_draft() -> CandidateDraft {
        CandidateDraft {
            categories: vec![CandidateCategory {
                reference: "b1:c1".to_owned(),
                kind: CategoryKind::Direction,
                name: "Монтажные системы".to_owned(),
                summary: None,
            }],
            products: vec![CandidateProduct {
                reference: "b1:p1".to_owned(),
                category_ref: Some("b1:c1".to_owned()),
                kind: ProductKind::Product,
                name: "BP21".to_owned(),
                summary: Some("профиль монтажный".to_owned()),
                aliases: vec![CandidateAlias {
                    surface: "Профиль BP 21".to_owned(),
                    relation: AliasRelation::Unclear,
                    note: Some("в тексте раздела".to_owned()),
                    evidence: quote(4, "Профиль BP 21 монтажный"),
                }],
            }],
            facts: vec![CandidateFact {
                product_ref: Some("b1:p1".to_owned()),
                kind: FactKind::Characteristic,
                attribute: "нагрузка".to_owned(),
                value_text: "3.5".to_owned(),
                unit: Some("kN".to_owned()),
                conditions: Some("две опоры".to_owned()),
                model_context: Some("пояснение модели".to_owned()),
                evidence: vec![quote(3, "BP21 1200 3.5 kN")],
            }],
            gaps: vec![CandidateGap {
                product_ref: Some("b1:p1".to_owned()),
                topic: "price".to_owned(),
                missing: "цена не указана".to_owned(),
                blocks: None,
                nature: GapNature::Commercial,
                question: Some(CandidateQuestion {
                    audience: QuestionAudience::Partner,
                    text: "Какая цена?".to_owned(),
                }),
            }],
            applications: vec![CandidateApplication {
                product_ref: Some("b1:p1".to_owned()),
                task: "закрепить кабельный лоток к бетону".to_owned(),
                summary: None,
                model_context: Some("обобщение модели".to_owned()),
                evidence: quote(5, "применяется для крепления лотков к бетону"),
                details: vec![
                    CandidateApplicationDetail {
                        kind: ApplicationDetailKind::Parameter,
                        label: "глубина анкеровки".to_owned(),
                        value_text: Some("60 мм".to_owned()),
                        unit: None,
                        audience: None,
                        evidence: Some(quote(5, "глубина анкеровки 60 мм")),
                    },
                    CandidateApplicationDetail {
                        kind: ApplicationDetailKind::Question,
                        label: "какой класс бетона?".to_owned(),
                        value_text: None,
                        unit: None,
                        audience: Some(QuestionAudience::Partner),
                        evidence: None,
                    },
                ],
            }],
            declarations: vec![CandidateDeclaration {
                topic: DeclarationTopic::Glossary,
                stated: "каталог не вводит терминов, требующих пояснения".to_owned(),
                origin: DeclarationOrigin::Model,
            }],
            ..CandidateDraft::default()
        }
    }

    #[test]
    fn mapping_preserves_the_quote_its_offsets_and_the_separated_model_context() {
        let mapped = to_new_draft(&full_draft(), &[]);

        assert_eq!(mapped.categories.len(), 1);
        assert_eq!(mapped.products[0].category_ref.as_deref(), Some("b1:c1"));
        let fact = &mapped.facts[0];
        assert_eq!(fact.unit.as_deref(), Some("kN"));
        assert_eq!(fact.conditions.as_deref(), Some("две опоры"));
        assert_eq!(fact.model_context.as_deref(), Some("пояснение модели"));
        assert_eq!(fact.evidence[0].quote, "BP21 1200 3.5 kN");
        assert_eq!(fact.evidence[0].char_start, 12);
        assert_eq!(
            mapped.gaps[0].question.as_ref().unwrap().audience,
            QuestionAudience::Partner
        );
    }

    /// Everything R05 added survives the translation, including the parts whose whole
    /// purpose is to be an absence: the gap's classification and the declaration.
    #[test]
    fn mapping_carries_aliases_applications_gap_nature_and_declarations() {
        let mapped = to_new_draft(&full_draft(), &[]);

        let alias = &mapped.products[0].aliases[0];
        assert_eq!(alias.relation, AliasRelation::Unclear);
        assert!(!alias.relation.is_safe_to_follow());
        assert_eq!(alias.evidence.quote, "Профиль BP 21 монтажный");

        assert_eq!(mapped.gaps[0].nature, GapNature::Commercial);

        let application = &mapped.applications[0];
        assert_eq!(application.product_ref.as_deref(), Some("b1:p1"));
        assert_eq!(application.details.len(), 2);
        // A parameter carries its fragment; a question carries its addressee and no
        // fragment at all, because it asserts nothing.
        assert!(application.details[0].evidence.is_some());
        assert!(application.details[1].evidence.is_none());
        assert_eq!(
            application.details[1].audience,
            Some(QuestionAudience::Partner)
        );

        assert_eq!(mapped.declarations.len(), 1);
        assert_eq!(mapped.declarations[0].topic, DeclarationTopic::Glossary);
        assert!(!mapped.declarations[0].stated.is_empty());
    }

    /// A fact nobody matched to a cell says `page_text`, and that is not a lesser answer.
    #[test]
    fn a_fact_with_no_matching_cell_records_the_page_as_its_structure() {
        let mapped = to_new_draft(&full_draft(), &[]);
        assert_eq!(mapped.facts[0].origin.source, StructuralSource::PageText);
        assert!(!mapped.facts[0].origin.is_from_a_table());
        assert!(mapped.facts[0].origin.cell_id.is_none());
    }

    #[test]
    fn a_fact_read_out_of_an_established_cell_records_which_cell_and_what_it_was_about() {
        let cell = StructuredCell {
            cell_id: Uuid::from_u128(77),
            page_id: Uuid::from_u128(3),
            page_number: 3,
            region_id: Uuid::from_u128(78),
            subject: "BP21".to_owned(),
            property: "Безопасная рабочая нагрузка".to_owned(),
            value: "3.5".to_owned(),
            unit: Some("кН".to_owned()),
            conditions: vec!["две опоры".to_owned()],
            context_inferred: false,
        };

        let mapped = to_new_draft(&full_draft(), std::slice::from_ref(&cell));
        let origin = &mapped.facts[0].origin;
        assert_eq!(origin.source, StructuralSource::TableCell);
        assert_eq!(origin.cell_id, Some(cell.cell_id));
        assert_eq!(origin.subject.as_deref(), Some("BP21"));
        assert_eq!(
            origin.property.as_deref(),
            Some("Безопасная рабочая нагрузка")
        );
        assert_eq!(origin.conditions, vec!["две опоры".to_owned()]);
    }

    #[test]
    fn an_empty_draft_maps_to_an_empty_draft() {
        let mapped = to_new_draft(&CandidateDraft::default(), &[]);
        assert_eq!(mapped, NewDraft::default());
    }

    /// The audited shape: pages processed, pages nobody could read, and one page the
    /// budget stopped — every one of them in the account, each with its own words.
    #[test]
    fn every_page_reaches_the_account_and_no_unprocessed_page_leaves_without_a_reason() {
        let pages = vec![
            page(1, PageStatus::Extracted),
            page(2, PageStatus::Extracted),
            page(3, PageStatus::NeedsOcr),
        ];
        let offerable = vec![pages[0].id, pages[1].id];
        let mut plan = CoveragePlan::build(&pages, &offerable);
        plan.settle(
            &[ProcessedPage {
                page_id: pages[0].id,
                batch_index: 1,
                chars_sent: 900,
            }],
            &[pages[1].id],
        );

        let rows = page_coverage_rows(&plan);
        assert_eq!(rows.len(), 3, "the denominator is the whole material");
        assert!(rows
            .iter()
            .all(|row| row.disposition == PageDisposition::Processed || row.reason.is_some()));

        let deferred = rows.iter().find(|row| row.page_number == 2).unwrap();
        assert_eq!(deferred.disposition, PageDisposition::DeferredBudget);
        assert_eq!(deferred.batch_index, None);
        assert_eq!(deferred.chars_sent, 0);

        let processed = rows.iter().find(|row| row.page_number == 1).unwrap();
        assert_eq!(processed.batch_index, Some(1));
        assert!(processed.offered);
        assert!(processed.reason.is_none());
    }

    /// An unreadable page becomes an uncertainty as well as a coverage line: the first
    /// answers "was it processed", the second answers "what is on it".
    #[test]
    fn a_page_nobody_could_read_is_reported_as_an_unknown_not_as_an_absence() {
        let pages = vec![page(1, PageStatus::Extracted), page(2, PageStatus::Failed)];
        let plan = CoveragePlan::build(&pages, &[pages[0].id]);

        let uncertainties = collect_uncertainties(&[], &plan);
        assert_eq!(uncertainties.len(), 1);
        assert_eq!(uncertainties[0].kind, UncertaintyKind::UnreadablePage);
        assert_eq!(uncertainties[0].page_number, Some(2));
        assert!(
            uncertainties[0].quote.is_none(),
            "there is nothing to quote"
        );
        assert!(uncertainties[0].detail.contains("неизвестно"));
        // Nobody may attribute it to a product: that is the point.
        assert!(uncertainties[0].product_id.is_none());
    }
}
