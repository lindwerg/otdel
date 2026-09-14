//! Writing the phase 1C draft: runs and candidates.
//!
//! Two properties are implemented here and nowhere else.
//!
//! **Re-running a material replaces its draft.** Everything a run produces is scoped to
//! one material; a new run deletes that material's previous candidates inside the same
//! transaction that writes the new ones. Pressing "разобрать ещё раз" therefore cannot
//! duplicate a product or a fact, and an interrupted run leaves either the old draft or
//! the new one — never a mixture. It mirrors how 1B replaces a page's regions.
//!
//! **Evidence is checked by the database, not only by the caller.** An evidence row
//! names the page it quotes, and the composite foreign keys of `0004_knowledge.sql`
//! make a page of another material — or another partner, or another bureau —
//! impossible to reference. A fact with no evidence at all fails at commit through a
//! deferred constraint trigger. The validation in `otdel-knowledge` is the first line;
//! this is the one that holds regardless of the caller.
//!
//! The run row itself — queueing it, closing it, and the page account written with its
//! status — lives in [`crate::knowledge_run`]; reading the draft back lives in
//! [`crate::knowledge_read`].

use otdel_core::knowledge::{
    CategoryKind, FactKind, KnowledgeRunStatus, ProductKind, QuestionAudience,
};
use otdel_core::passport::{FactOrigin, GapNature, RunCoverage};
use sqlx::Row;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::passport::{self, NewAlias, NewApplication, NewDeclaration, NewSense, NewSynonym};
use crate::tenancy::ScopedTx;

// The run lifecycle lives next door but is re-exported here, so every caller keeps
// naming one module for "writing a draft and closing its run".
pub use crate::knowledge_run::{enqueue_run, finish_run, reclaim_stalled_runs, start_run};

/// A fragment of a page, already located there by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEvidence {
    pub page_id: Uuid,
    pub page_number: i32,
    pub quote: String,
    pub char_start: i32,
    pub char_end: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCategory {
    /// Draft-local reference used by [`NewProduct::category_ref`].
    pub reference: String,
    pub kind: CategoryKind,
    pub name: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProduct {
    pub reference: String,
    pub category_ref: Option<String>,
    pub kind: ProductKind,
    pub name: String,
    pub summary: Option<String>,
    /// R05 — other names this material uses for the same product, each with the fragment
    /// that shows it. Recorded, never merged.
    pub aliases: Vec<NewAlias>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFact {
    pub product_ref: Option<String>,
    pub kind: FactKind,
    pub attribute: String,
    pub value_text: String,
    pub unit: Option<String>,
    pub conditions: Option<String>,
    pub model_context: Option<String>,
    /// Never empty — the database refuses a fact without it.
    pub evidence: Vec<NewEvidence>,
    /// R05 — which structure the value sat in. Defaults to `page_text`, which is the
    /// truthful answer whenever no cell was matched.
    pub origin: FactOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTerm {
    pub term: String,
    pub definition: String,
    pub definition_is_model_context: bool,
    pub evidence: Vec<NewEvidence>,
    /// R05 — further readings of the same word in this material.
    pub senses: Vec<NewSense>,
    /// R05 — other spellings of the same term.
    pub synonyms: Vec<NewSynonym>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewQa {
    pub question: String,
    pub answer: String,
    pub answer_is_model_context: bool,
    pub evidence: Vec<NewEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewQuestion {
    pub audience: QuestionAudience,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewGap {
    pub product_ref: Option<String>,
    pub topic: String,
    pub missing: String,
    pub blocks: Option<String>,
    /// R05 — commercial, technical, or neither. Classified by the run.
    pub nature: GapNature,
    pub question: Option<NewQuestion>,
}

/// Everything one run produced, ready to be stored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewDraft {
    pub categories: Vec<NewCategory>,
    pub products: Vec<NewProduct>,
    pub facts: Vec<NewFact>,
    pub terms: Vec<NewTerm>,
    pub qa: Vec<NewQa>,
    pub gaps: Vec<NewGap>,
    /// R05 — the tasks the material says its products serve.
    pub applications: Vec<NewApplication>,
    /// R05 — the topics this run stated are empty, in its own words. The one thing here
    /// that has no rows of its own to justify it, and the reason an empty array can ever
    /// be an answer.
    pub declarations: Vec<NewDeclaration>,
}

/// What was actually written.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DraftCounts {
    pub categories: i32,
    pub products: i32,
    pub facts: i32,
    pub terms: i32,
    pub qa: i32,
    pub gaps: i32,
    pub questions: i32,
    /// R05.
    pub applications: i32,
    pub application_details: i32,
    pub declarations: i32,
    pub aliases: i32,
    pub senses: i32,
    pub synonyms: i32,
}

/// Counters and provenance of a finished run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutcome {
    pub status: KnowledgeRunStatus,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub pages_considered: i32,
    pub pages_skipped: i32,
    pub requests_made: i32,
    pub input_chars: i32,
    pub facts_rejected: i32,
    pub rejections: Vec<String>,
    pub diagnostic: Option<String>,
    pub counts: DraftCounts,
    /// R05 — the page account, the requirement verdict and what the pass cost.
    ///
    /// Written in the same statement as the status, so no reader can ever see a run
    /// described as finished without the account of what it covered. A run that failed
    /// before planning stores [`RunCoverage::default`], whose state is `unknown` — which
    /// is the truth and is refused by the publication gate.
    pub coverage: RunCoverage,
}

/// Replace a material's candidates with this run's.
///
/// The delete comes first and covers every candidate table of this material. Facts,
/// terms and Q&A cascade to their evidence; gaps cascade to their questions.
pub async fn replace_draft(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    material_id: Uuid,
    run_id: Uuid,
    draft: &NewDraft,
) -> DbResult<DraftCounts> {
    clear_material_draft(tx, material_id).await?;

    let mut counts = DraftCounts::default();
    let scope = Scope {
        bureau_id: tx.bureau_id(),
        partner_id,
        material_id,
        run_id,
    };

    let categories = insert_categories(tx, &scope, &draft.categories, &mut counts).await?;
    let products = insert_products(tx, &scope, &draft.products, &categories, &mut counts).await?;
    insert_facts(tx, &scope, &draft.facts, &products, &mut counts).await?;
    insert_terms(tx, &scope, &draft.terms, &mut counts).await?;
    insert_qa(tx, &scope, &draft.qa, &mut counts).await?;
    insert_gaps(tx, &scope, &draft.gaps, &products, &mut counts).await?;
    // R05, after the products so an application can name one, and inside the same
    // transaction so a passport can never show a task whose product was rolled back.
    passport::insert_applications(tx, &scope, &draft.applications, &products, &mut counts).await?;
    passport::insert_declarations(tx, &scope, &draft.declarations, &mut counts).await?;

    Ok(counts)
}

/// Remove every candidate produced from this material.
pub async fn clear_material_draft(tx: &mut ScopedTx, material_id: Uuid) -> DbResult<()> {
    let bureau_id = tx.bureau_id();
    // Order matters only for readability: the foreign keys cascade. Facts are deleted
    // before their products so the cascade does the same work either way.
    //
    // The R05 tables are listed explicitly rather than left to the cascade for one of
    // them: `knowledge_declarations` hangs off the *run*, not off a product, and a run
    // row survives a re-draft. A stale declaration would keep satisfying a requirement
    // for a draft that no longer exists, which is precisely the inference this package
    // was built to remove.
    for table in [
        "otdel.application_details",
        "otdel.product_applications",
        "otdel.knowledge_declarations",
        "otdel.knowledge_uncertainties",
        "otdel.glossary_senses",
        "otdel.glossary_synonyms",
        "otdel.product_aliases",
        "otdel.knowledge_questions",
        "otdel.knowledge_gaps",
        "otdel.knowledge_qa",
        "otdel.glossary_terms",
        "otdel.knowledge_facts",
        "otdel.products",
        "otdel.product_categories",
    ] {
        sqlx::query(&format!(
            "DELETE FROM {table} WHERE bureau_id = $1 AND material_id = $2"
        ))
        .bind(bureau_id)
        .bind(material_id)
        .execute(tx.conn())
        .await?;
    }
    Ok(())
}

/// The bureau, partner, material and run every candidate row of one draft belongs to.
pub(crate) struct Scope {
    pub(crate) bureau_id: Uuid,
    pub(crate) partner_id: Uuid,
    pub(crate) material_id: Uuid,
    pub(crate) run_id: Uuid,
}

/// Draft-local reference → stored identifier.
pub(crate) type Resolved = Vec<(String, Uuid)>;

pub(crate) fn resolve(map: &Resolved, reference: &str) -> Option<Uuid> {
    map.iter()
        .find(|(key, _)| key == reference)
        .map(|(_, id)| *id)
}

async fn insert_categories(
    tx: &mut ScopedTx,
    scope: &Scope,
    categories: &[NewCategory],
    counts: &mut DraftCounts,
) -> DbResult<Resolved> {
    let mut resolved: Resolved = Vec::with_capacity(categories.len());

    for category in categories {
        let row = sqlx::query(
            "INSERT INTO otdel.product_categories \
                 (bureau_id, partner_id, material_id, run_id, kind, name, normalised_name, summary) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (material_id, kind, normalised_name) DO UPDATE \
                SET summary = coalesce(EXCLUDED.summary, otdel.product_categories.summary) \
             RETURNING id",
        )
        .bind(scope.bureau_id)
        .bind(scope.partner_id)
        .bind(scope.material_id)
        .bind(scope.run_id)
        .bind(category.kind.as_str())
        .bind(&category.name)
        .bind(normalised(&category.name))
        .bind(category.summary.as_deref())
        .fetch_one(tx.conn())
        .await?;

        let id: Uuid = row.try_get("id")?;
        // Two references to the same name are one row; counting both would report
        // more categories than exist.
        if !resolved.iter().any(|(_, kept)| *kept == id) {
            counts.categories += 1;
        }
        resolved.push((category.reference.clone(), id));
    }

    Ok(resolved)
}

async fn insert_products(
    tx: &mut ScopedTx,
    scope: &Scope,
    products: &[NewProduct],
    categories: &Resolved,
    counts: &mut DraftCounts,
) -> DbResult<Resolved> {
    let mut resolved: Resolved = Vec::with_capacity(products.len());

    for product in products {
        let category_id = product
            .category_ref
            .as_deref()
            .and_then(|reference| resolve(categories, reference));

        let row = sqlx::query(
            "INSERT INTO otdel.products \
                 (bureau_id, partner_id, material_id, run_id, category_id, kind, name, \
                  normalised_name, summary) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             ON CONFLICT (material_id, kind, normalised_name) DO UPDATE \
                SET category_id = coalesce(EXCLUDED.category_id, otdel.products.category_id), \
                    summary = coalesce(EXCLUDED.summary, otdel.products.summary) \
             RETURNING id",
        )
        .bind(scope.bureau_id)
        .bind(scope.partner_id)
        .bind(scope.material_id)
        .bind(scope.run_id)
        .bind(category_id)
        .bind(product.kind.as_str())
        .bind(&product.name)
        .bind(normalised(&product.name))
        .bind(product.summary.as_deref())
        .fetch_one(tx.conn())
        .await?;

        let id: Uuid = row.try_get("id")?;
        if !resolved.iter().any(|(_, kept)| *kept == id) {
            counts.products += 1;
        }
        passport::insert_aliases(tx, scope, id, &product.aliases, counts).await?;
        resolved.push((product.reference.clone(), id));
    }

    Ok(resolved)
}

async fn insert_facts(
    tx: &mut ScopedTx,
    scope: &Scope,
    facts: &[NewFact],
    products: &Resolved,
    counts: &mut DraftCounts,
) -> DbResult<()> {
    for fact in facts {
        // Defence in depth next to the deferred trigger: a caller that lost its
        // evidence somewhere between validation and storage must not write the fact.
        if fact.evidence.is_empty() {
            return Err(DbError::Decode(
                "refusing to store a fact without evidence".to_owned(),
            ));
        }

        let product_id = fact
            .product_ref
            .as_deref()
            .and_then(|reference| resolve(products, reference));

        // Claiming a table origin means naming what the table said the value was about.
        // The database enforces the same rule; refusing here keeps the caller's mistake
        // from becoming a constraint violation that aborts the whole draft.
        let origin = &fact.origin;
        if origin.is_from_a_table()
            && (origin
                .subject
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
                || origin
                    .property
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty())
        {
            return Err(DbError::Decode(
                "refusing to store a fact claiming a table origin without the cell's \
                 subject and property"
                    .to_owned(),
            ));
        }

        let row = sqlx::query(
            "INSERT INTO otdel.knowledge_facts \
                 (bureau_id, partner_id, material_id, run_id, product_id, kind, attribute, \
                  value_text, unit, conditions, model_context, structural_source, \
                  source_cell_id, structural_subject, structural_property, structural_unit, \
                  structural_conditions) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17) \
             RETURNING id",
        )
        .bind(scope.bureau_id)
        .bind(scope.partner_id)
        .bind(scope.material_id)
        .bind(scope.run_id)
        .bind(product_id)
        .bind(fact.kind.as_str())
        .bind(&fact.attribute)
        .bind(&fact.value_text)
        .bind(fact.unit.as_deref())
        .bind(fact.conditions.as_deref())
        .bind(fact.model_context.as_deref())
        .bind(origin.source.as_str())
        .bind(origin.cell_id)
        .bind(origin.subject.as_deref())
        .bind(origin.property.as_deref())
        .bind(origin.unit.as_deref())
        .bind(&origin.conditions)
        .fetch_one(tx.conn())
        .await?;

        let fact_id: Uuid = row.try_get("id")?;
        insert_evidence(tx, scope, EvidenceParent::Fact(fact_id), &fact.evidence).await?;
        counts.facts += 1;
    }
    Ok(())
}

async fn insert_terms(
    tx: &mut ScopedTx,
    scope: &Scope,
    terms: &[NewTerm],
    counts: &mut DraftCounts,
) -> DbResult<()> {
    for term in terms {
        if term.evidence.is_empty() {
            return Err(DbError::Decode(
                "refusing to store a glossary term without evidence".to_owned(),
            ));
        }

        let row = sqlx::query(
            "INSERT INTO otdel.glossary_terms \
                 (bureau_id, partner_id, material_id, run_id, term, normalised_term, \
                  definition, definition_is_model_context) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (material_id, normalised_term) DO UPDATE \
                SET definition = EXCLUDED.definition, \
                    definition_is_model_context = EXCLUDED.definition_is_model_context \
             RETURNING id",
        )
        .bind(scope.bureau_id)
        .bind(scope.partner_id)
        .bind(scope.material_id)
        .bind(scope.run_id)
        .bind(&term.term)
        .bind(normalised(&term.term))
        .bind(&term.definition)
        .bind(term.definition_is_model_context)
        .fetch_one(tx.conn())
        .await?;

        let term_id: Uuid = row.try_get("id")?;
        insert_evidence(tx, scope, EvidenceParent::Term(term_id), &term.evidence).await?;
        passport::insert_senses(tx, scope, term_id, &term.senses, counts).await?;
        passport::insert_synonyms(tx, scope, term_id, &term.synonyms, counts).await?;
        counts.terms += 1;
    }
    Ok(())
}

async fn insert_qa(
    tx: &mut ScopedTx,
    scope: &Scope,
    entries: &[NewQa],
    counts: &mut DraftCounts,
) -> DbResult<()> {
    for entry in entries {
        if entry.evidence.is_empty() {
            return Err(DbError::Decode(
                "refusing to store an answer without evidence".to_owned(),
            ));
        }

        let row = sqlx::query(
            "INSERT INTO otdel.knowledge_qa \
                 (bureau_id, partner_id, material_id, run_id, question, answer, \
                  answer_is_model_context) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
        )
        .bind(scope.bureau_id)
        .bind(scope.partner_id)
        .bind(scope.material_id)
        .bind(scope.run_id)
        .bind(&entry.question)
        .bind(&entry.answer)
        .bind(entry.answer_is_model_context)
        .fetch_one(tx.conn())
        .await?;

        let qa_id: Uuid = row.try_get("id")?;
        insert_evidence(tx, scope, EvidenceParent::Qa(qa_id), &entry.evidence).await?;
        counts.qa += 1;
    }
    Ok(())
}

async fn insert_gaps(
    tx: &mut ScopedTx,
    scope: &Scope,
    gaps: &[NewGap],
    products: &Resolved,
    counts: &mut DraftCounts,
) -> DbResult<()> {
    for gap in gaps {
        let product_id = gap
            .product_ref
            .as_deref()
            .and_then(|reference| resolve(products, reference));

        let row = sqlx::query(
            "INSERT INTO otdel.knowledge_gaps \
                 (bureau_id, partner_id, material_id, run_id, product_id, topic, missing, \
                  blocks, nature) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
        )
        .bind(scope.bureau_id)
        .bind(scope.partner_id)
        .bind(scope.material_id)
        .bind(scope.run_id)
        .bind(product_id)
        .bind(&gap.topic)
        .bind(&gap.missing)
        .bind(gap.blocks.as_deref())
        .bind(gap.nature.as_str())
        .fetch_one(tx.conn())
        .await?;

        let gap_id: Uuid = row.try_get("id")?;
        counts.gaps += 1;

        if let Some(question) = &gap.question {
            sqlx::query(
                "INSERT INTO otdel.knowledge_questions \
                     (bureau_id, partner_id, material_id, run_id, gap_id, audience, text_content) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) \
                 ON CONFLICT (gap_id, audience) DO UPDATE SET text_content = EXCLUDED.text_content",
            )
            .bind(scope.bureau_id)
            .bind(scope.partner_id)
            .bind(scope.material_id)
            .bind(scope.run_id)
            .bind(gap_id)
            .bind(question.audience.as_str())
            .bind(&question.text)
            .execute(tx.conn())
            .await?;
            counts.questions += 1;
        }
    }
    Ok(())
}

enum EvidenceParent {
    Fact(Uuid),
    Term(Uuid),
    Qa(Uuid),
}

async fn insert_evidence(
    tx: &mut ScopedTx,
    scope: &Scope,
    parent: EvidenceParent,
    evidence: &[NewEvidence],
) -> DbResult<()> {
    let (fact_id, term_id, qa_id) = match parent {
        EvidenceParent::Fact(id) => (Some(id), None, None),
        EvidenceParent::Term(id) => (None, Some(id), None),
        EvidenceParent::Qa(id) => (None, None, Some(id)),
    };

    for item in evidence {
        sqlx::query(
            "INSERT INTO otdel.knowledge_evidence \
                 (bureau_id, partner_id, material_id, page_id, page_number, fact_id, term_id, \
                  qa_id, quote, char_start, char_end) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(scope.bureau_id)
        .bind(scope.partner_id)
        // The material of the run: the composite foreign key then requires the page to
        // be a page of *this* material, so a page id from anywhere else is rejected by
        // the database itself.
        .bind(scope.material_id)
        .bind(item.page_id)
        .bind(item.page_number)
        .bind(fact_id)
        .bind(term_id)
        .bind(qa_id)
        .bind(&item.quote)
        .bind(item.char_start)
        .bind(item.char_end)
        .execute(tx.conn())
        .await?;
    }
    Ok(())
}

/// Case- and whitespace-folded name used by the uniqueness constraints.
pub fn normalised(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        .chars()
        .take(200)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_folded_for_uniqueness_but_not_altered_for_storage() {
        assert_eq!(normalised("  BP 21D  "), "bp 21d");
        assert_eq!(normalised("Профиль\tМонтажный"), "профиль монтажный");
        assert_eq!(normalised(&"я".repeat(500)).chars().count(), 200);
    }

    #[test]
    fn a_reference_map_resolves_only_what_it_holds() {
        let map: Resolved = vec![("b1:p1".to_owned(), Uuid::from_u128(3))];
        assert_eq!(resolve(&map, "b1:p1"), Some(Uuid::from_u128(3)));
        assert_eq!(resolve(&map, "b1:p2"), None);
        assert_eq!(resolve(&map, ""), None);
    }
}
