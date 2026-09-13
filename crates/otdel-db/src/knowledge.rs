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
//! Reading the draft back lives in [`crate::knowledge_read`].

use chrono::{DateTime, Utc};
use otdel_core::knowledge::{
    CategoryKind, FactKind, KnowledgeRun, KnowledgeRunStatus, ProductKind, QuestionAudience,
};
use sqlx::Row;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::knowledge_read;
use crate::tenancy::ScopedTx;

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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTerm {
    pub term: String,
    pub definition: String,
    pub definition_is_model_context: bool,
    pub evidence: Vec<NewEvidence>,
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
}

/// Create or reset the run row of a material, in `queued`.
///
/// Called when the work is enqueued (by the owner or by the extraction worker), so the
/// interface can show "материал поставлен в очередь на разбор" instead of nothing.
pub async fn enqueue_run(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    material_id: Uuid,
    prompt_profile: &str,
) -> DbResult<KnowledgeRun> {
    upsert_run(
        tx,
        partner_id,
        material_id,
        prompt_profile,
        KnowledgeRunStatus::Queued,
    )
    .await
}

/// Mark the run as running and clear the previous counters.
pub async fn start_run(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    material_id: Uuid,
    prompt_profile: &str,
) -> DbResult<KnowledgeRun> {
    upsert_run(
        tx,
        partner_id,
        material_id,
        prompt_profile,
        KnowledgeRunStatus::Running,
    )
    .await
}

async fn upsert_run(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    material_id: Uuid,
    prompt_profile: &str,
    status: KnowledgeRunStatus,
) -> DbResult<KnowledgeRun> {
    let bureau_id = tx.bureau_id();
    let started_at: Option<DateTime<Utc>> = match status {
        KnowledgeRunStatus::Running => Some(Utc::now()),
        _ => None,
    };

    sqlx::query(
        "INSERT INTO otdel.knowledge_runs \
             (bureau_id, partner_id, material_id, status, prompt_profile, started_at) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (material_id) DO UPDATE \
            SET status = EXCLUDED.status, \
                prompt_profile = EXCLUDED.prompt_profile, \
                started_at = coalesce(EXCLUDED.started_at, otdel.knowledge_runs.started_at), \
                finished_at = NULL, \
                diagnostic = NULL, \
                rejections = '{}', \
                pages_considered = 0, \
                pages_skipped = 0, \
                requests_made = 0, \
                input_chars = 0, \
                facts_accepted = 0, \
                facts_rejected = 0, \
                updated_at = now()",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .bind(material_id)
    .bind(status.as_str())
    .bind(prompt_profile)
    .bind(started_at)
    .execute(tx.conn())
    .await?;

    // Read it back through the same query the API uses, so a run always carries the
    // live counts and the material's name rather than a second, divergent shape.
    knowledge_read::find_run(tx, partner_id, material_id)
        .await?
        .ok_or_else(|| DbError::Decode("the run row disappeared after it was written".to_owned()))
}

/// Record how a run ended, including a run that produced nothing.
pub async fn finish_run(tx: &mut ScopedTx, run_id: Uuid, outcome: &RunOutcome) -> DbResult<()> {
    let bureau_id = tx.bureau_id();
    let rejections: Vec<String> = outcome
        .rejections
        .iter()
        .map(|reason| reason.chars().take(500).collect())
        .take(100)
        .collect();

    sqlx::query(
        "UPDATE otdel.knowledge_runs \
            SET status = $3, \
                provider = $4, \
                model = $5, \
                pages_considered = $6, \
                pages_skipped = $7, \
                requests_made = $8, \
                input_chars = $9, \
                facts_accepted = $10, \
                facts_rejected = $11, \
                rejections = $12, \
                diagnostic = $13, \
                finished_at = now(), \
                updated_at = now() \
          WHERE bureau_id = $1 AND id = $2",
    )
    .bind(bureau_id)
    .bind(run_id)
    .bind(outcome.status.as_str())
    .bind(outcome.provider.as_deref())
    .bind(outcome.model.as_deref())
    .bind(outcome.pages_considered)
    .bind(outcome.pages_skipped)
    .bind(outcome.requests_made)
    .bind(outcome.input_chars)
    .bind(outcome.counts.facts)
    .bind(outcome.facts_rejected)
    .bind(&rejections)
    .bind(outcome.diagnostic.as_deref())
    .execute(tx.conn())
    .await?;

    Ok(())
}

/// Settle runs that say `running` but have no job behind them any more.
///
/// A worker killed mid-run leaves the run row claiming to be in progress; the queue
/// recovers the *job* (lease reclaim, attempt limit), but nothing would ever correct
/// the run, and the interface would show a spinner forever while the owner's own
/// "разобрать" button stays hidden behind "уже выполняется". Returns how many rows
/// were corrected.
pub async fn reclaim_stalled_runs(tx: &mut ScopedTx) -> DbResult<u64> {
    let bureau_id = tx.bureau_id();
    let result = sqlx::query(
        "UPDATE otdel.knowledge_runs r \
            SET status = 'failed', \
                diagnostic = coalesce(r.diagnostic, \
                    'разбор прерван: обработчик остановился, задание больше не выполняется'), \
                finished_at = now(), \
                updated_at = now() \
          WHERE r.bureau_id = $1 AND r.status = 'running' \
            AND NOT EXISTS ( \
                SELECT 1 FROM otdel.jobs j \
                 WHERE j.bureau_id = r.bureau_id \
                   AND j.material_id = r.material_id \
                   AND j.kind = 'understand_material' \
                   AND j.status IN ('queued', 'running') \
            )",
    )
    .bind(bureau_id)
    .execute(tx.conn())
    .await?;

    Ok(result.rows_affected())
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

    Ok(counts)
}

/// Remove every candidate produced from this material.
pub async fn clear_material_draft(tx: &mut ScopedTx, material_id: Uuid) -> DbResult<()> {
    let bureau_id = tx.bureau_id();
    // Order matters only for readability: the foreign keys cascade. Facts are deleted
    // before their products so the cascade does the same work either way.
    for table in [
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

struct Scope {
    bureau_id: Uuid,
    partner_id: Uuid,
    material_id: Uuid,
    run_id: Uuid,
}

/// Draft-local reference → stored identifier.
type Resolved = Vec<(String, Uuid)>;

fn resolve(map: &Resolved, reference: &str) -> Option<Uuid> {
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

        let row = sqlx::query(
            "INSERT INTO otdel.knowledge_facts \
                 (bureau_id, partner_id, material_id, run_id, product_id, kind, attribute, \
                  value_text, unit, conditions, model_context) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
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
                 (bureau_id, partner_id, material_id, run_id, product_id, topic, missing, blocks) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
        )
        .bind(scope.bureau_id)
        .bind(scope.partner_id)
        .bind(scope.material_id)
        .bind(scope.run_id)
        .bind(product_id)
        .bind(&gap.topic)
        .bind(&gap.missing)
        .bind(gap.blocks.as_deref())
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
