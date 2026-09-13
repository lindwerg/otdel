//! Reading the phase 1C draft back, for the API.
//!
//! Every query is bounded to one partner *inside* a bureau-scoped transaction, so a
//! partner id guessed from elsewhere returns nothing rather than somebody else's draft.
//! Facts, terms and answers always come back with their evidence attached: there is no
//! call here that returns a statement without the fragment that supports it, because a
//! caller that forgot to fetch the evidence would render an unsourced claim.

use chrono::{DateTime, Utc};
use otdel_core::knowledge::{
    CategoryKind, DraftableMaterial, FactEvidence, FactKind, FactStatus, GlossaryTerm,
    KnowledgeFact, KnowledgeGap, KnowledgeRun, KnowledgeRunStatus, KnowledgeSummary,
    PreparedQuestion, Product, ProductCategory, ProductKind, QaEntry, QuestionAudience,
};
use sqlx::postgres::PgRow;
use sqlx::Row;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::tenancy::ScopedTx;

/// Which parent an evidence row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvidenceOwner {
    Fact,
    Term,
    Qa,
}

impl EvidenceOwner {
    const fn column(self) -> &'static str {
        match self {
            Self::Fact => "fact_id",
            Self::Term => "term_id",
            Self::Qa => "qa_id",
        }
    }
}

pub(crate) fn run_from_row(row: &PgRow) -> DbResult<KnowledgeRun> {
    let status: String = row.try_get("status")?;
    let status = KnowledgeRunStatus::parse(&status)
        .ok_or_else(|| DbError::Decode(format!("unknown knowledge run status `{status}`")))?;

    Ok(KnowledgeRun {
        id: row.try_get("id")?,
        partner_id: row.try_get("partner_id")?,
        material_id: row.try_get("material_id")?,
        // A correlated subquery can be typed as nullable even though the foreign key
        // guarantees the material exists; decoding it defensively keeps one missing row
        // from failing the whole overview.
        material_filename: row
            .try_get::<Option<String>, _>("material_filename")?
            .unwrap_or_default(),
        status,
        provider: row.try_get("provider")?,
        model: row.try_get("model")?,
        prompt_profile: row.try_get("prompt_profile")?,
        pages_considered: row.try_get("pages_considered")?,
        requests_made: row.try_get("requests_made")?,
        input_chars: row.try_get("input_chars")?,
        categories_created: 0,
        products_created: 0,
        facts_accepted: row.try_get("facts_accepted")?,
        facts_rejected: row.try_get("facts_rejected")?,
        terms_created: 0,
        qa_created: 0,
        gaps_created: 0,
        questions_created: 0,
        rejections: row.try_get::<Vec<String>, _>("rejections")?,
        diagnostic: row.try_get("diagnostic")?,
        started_at: row.try_get::<Option<DateTime<Utc>>, _>("started_at")?,
        finished_at: row.try_get::<Option<DateTime<Utc>>, _>("finished_at")?,
        created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
    })
}

/// Runs of one partner, newest first, with the per-kind counts filled in from the
/// stored candidates (so the numbers cannot drift from the rows they describe).
pub async fn list_runs(tx: &mut ScopedTx, partner_id: Uuid) -> DbResult<Vec<KnowledgeRun>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(&format!(
        "SELECT r.*, {RUN_COUNTS} \
           FROM otdel.knowledge_runs r \
          WHERE r.bureau_id = $1 AND r.partner_id = $2 \
          ORDER BY r.updated_at DESC, r.id"
    ))
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter().map(run_with_counts).collect()
}

/// Counts taken from the stored candidates themselves, so a run's numbers can never
/// claim more than the rows it left behind — including after a failed re-run, which
/// leaves the previous draft in place.
const RUN_COUNTS: &str = "\
     (SELECT m.filename FROM otdel.materials m \
       WHERE m.bureau_id = r.bureau_id AND m.id = r.material_id) AS material_filename, \
     (SELECT count(*) FROM otdel.product_categories c \
       WHERE c.bureau_id = r.bureau_id AND c.material_id = r.material_id) AS categories, \
     (SELECT count(*) FROM otdel.products p \
       WHERE p.bureau_id = r.bureau_id AND p.material_id = r.material_id) AS products, \
     (SELECT count(*) FROM otdel.knowledge_facts f \
       WHERE f.bureau_id = r.bureau_id AND f.material_id = r.material_id) AS facts, \
     (SELECT count(*) FROM otdel.glossary_terms t \
       WHERE t.bureau_id = r.bureau_id AND t.material_id = r.material_id) AS terms, \
     (SELECT count(*) FROM otdel.knowledge_qa q \
       WHERE q.bureau_id = r.bureau_id AND q.material_id = r.material_id) AS qa, \
     (SELECT count(*) FROM otdel.knowledge_gaps g \
       WHERE g.bureau_id = r.bureau_id AND g.material_id = r.material_id) AS gaps, \
     (SELECT count(*) FROM otdel.knowledge_questions qq \
       WHERE qq.bureau_id = r.bureau_id AND qq.material_id = r.material_id) AS questions";

fn run_with_counts(row: &PgRow) -> DbResult<KnowledgeRun> {
    let mut run = run_from_row(row)?;
    run.categories_created = count(row, "categories")?;
    run.products_created = count(row, "products")?;
    run.facts_accepted = count(row, "facts")?;
    run.terms_created = count(row, "terms")?;
    run.qa_created = count(row, "qa")?;
    run.gaps_created = count(row, "gaps")?;
    run.questions_created = count(row, "questions")?;
    Ok(run)
}

/// The run of one material, if it has ever been queued.
pub async fn find_run(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    material_id: Uuid,
) -> DbResult<Option<KnowledgeRun>> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(&format!(
        "SELECT r.*, {RUN_COUNTS} FROM otdel.knowledge_runs r \
          WHERE r.bureau_id = $1 AND r.partner_id = $2 AND r.material_id = $3"
    ))
    .bind(bureau_id)
    .bind(partner_id)
    .bind(material_id)
    .fetch_optional(tx.conn())
    .await?;

    row.as_ref().map(run_with_counts).transpose()
}

/// Materials with readable pages that have never been queued for a draft.
///
/// The interface needs these to offer a first draft; a material that already has a run
/// is represented by that run instead.
pub async fn draftable_materials(
    tx: &mut ScopedTx,
    partner_id: Uuid,
) -> DbResult<Vec<DraftableMaterial>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT m.id, m.filename, count(p.id) AS pages \
           FROM otdel.materials m \
           JOIN otdel.material_pages p \
             ON p.bureau_id = m.bureau_id AND p.material_id = m.id \
            AND p.status IN ('extracted', 'partial') \
            AND btrim(coalesce(p.text_content, '')) <> '' \
          WHERE m.bureau_id = $1 AND m.partner_id = $2 \
            AND NOT EXISTS ( \
                SELECT 1 FROM otdel.knowledge_runs r \
                 WHERE r.bureau_id = m.bureau_id AND r.material_id = m.id \
            ) \
          GROUP BY m.id, m.filename, m.created_at \
          ORDER BY m.created_at DESC, m.id",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            Ok(DraftableMaterial {
                material_id: row.try_get("id")?,
                filename: row.try_get("filename")?,
                pages_with_text: count(row, "pages")?,
            })
        })
        .collect()
}

/// Partner-level roll-up shown on the knowledge tab.
pub async fn summary(tx: &mut ScopedTx, partner_id: Uuid) -> DbResult<KnowledgeSummary> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(
        "SELECT \
            (SELECT count(*) FROM otdel.product_categories WHERE bureau_id = $1 AND partner_id = $2) AS categories, \
            (SELECT count(*) FROM otdel.products WHERE bureau_id = $1 AND partner_id = $2) AS products, \
            (SELECT count(*) FROM otdel.knowledge_facts WHERE bureau_id = $1 AND partner_id = $2) AS facts, \
            (SELECT count(*) FROM otdel.glossary_terms WHERE bureau_id = $1 AND partner_id = $2) AS terms, \
            (SELECT count(*) FROM otdel.knowledge_qa WHERE bureau_id = $1 AND partner_id = $2) AS qa, \
            (SELECT count(*) FROM otdel.knowledge_gaps WHERE bureau_id = $1 AND partner_id = $2 AND status = 'open') AS gaps, \
            (SELECT count(*) FROM otdel.knowledge_questions WHERE bureau_id = $1 AND partner_id = $2 AND status = 'prepared') AS questions, \
            (SELECT count(DISTINCT p.material_id) FROM otdel.material_pages p \
               JOIN otdel.materials m ON m.bureau_id = p.bureau_id AND m.id = p.material_id \
              WHERE p.bureau_id = $1 AND m.partner_id = $2 \
                AND p.status IN ('extracted', 'partial') AND btrim(coalesce(p.text_content, '')) <> '') AS readable, \
            (SELECT count(*) FROM otdel.knowledge_runs \
              WHERE bureau_id = $1 AND partner_id = $2 AND status IN ('completed', 'partial')) AS understood",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_one(tx.conn())
    .await?;

    Ok(KnowledgeSummary {
        categories_total: count(&row, "categories")?,
        products_total: count(&row, "products")?,
        facts_total: count(&row, "facts")?,
        terms_total: count(&row, "terms")?,
        qa_total: count(&row, "qa")?,
        gaps_total: count(&row, "gaps")?,
        questions_total: count(&row, "questions")?,
        materials_readable: count(&row, "readable")?,
        materials_understood: count(&row, "understood")?,
    })
}

pub async fn list_categories(
    tx: &mut ScopedTx,
    partner_id: Uuid,
) -> DbResult<Vec<ProductCategory>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT id, partner_id, material_id, run_id, kind, name, summary, created_at \
           FROM otdel.product_categories \
          WHERE bureau_id = $1 AND partner_id = $2 \
          ORDER BY name, id",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            let kind: String = row.try_get("kind")?;
            Ok(ProductCategory {
                id: row.try_get("id")?,
                partner_id: row.try_get("partner_id")?,
                material_id: row.try_get("material_id")?,
                run_id: row.try_get("run_id")?,
                kind: CategoryKind::parse(&kind)
                    .ok_or_else(|| DbError::Decode(format!("unknown category kind `{kind}`")))?,
                name: row.try_get("name")?,
                summary: row.try_get("summary")?,
                created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
            })
        })
        .collect()
}

pub async fn list_products(tx: &mut ScopedTx, partner_id: Uuid) -> DbResult<Vec<Product>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT id, partner_id, material_id, run_id, category_id, kind, name, summary, created_at \
           FROM otdel.products \
          WHERE bureau_id = $1 AND partner_id = $2 \
          ORDER BY name, id",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            let kind: String = row.try_get("kind")?;
            Ok(Product {
                id: row.try_get("id")?,
                partner_id: row.try_get("partner_id")?,
                material_id: row.try_get("material_id")?,
                run_id: row.try_get("run_id")?,
                category_id: row.try_get("category_id")?,
                kind: ProductKind::parse(&kind)
                    .ok_or_else(|| DbError::Decode(format!("unknown product kind `{kind}`")))?,
                name: row.try_get("name")?,
                summary: row.try_get("summary")?,
                created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
            })
        })
        .collect()
}

/// Facts of a partner, each with its evidence.
pub async fn list_facts(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    product_id: Option<Uuid>,
) -> DbResult<Vec<KnowledgeFact>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT f.id, f.partner_id, f.material_id, f.run_id, f.product_id, p.name AS product_name, \
                f.kind, f.status, f.attribute, f.value_text, f.unit, f.conditions, \
                f.model_context, f.created_at \
           FROM otdel.knowledge_facts f \
           LEFT JOIN otdel.products p ON p.bureau_id = f.bureau_id AND p.id = f.product_id \
          WHERE f.bureau_id = $1 AND f.partner_id = $2 \
            AND ($3::uuid IS NULL OR f.product_id = $3) \
          ORDER BY p.name NULLS FIRST, f.attribute, f.created_at",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .bind(product_id)
    .fetch_all(tx.conn())
    .await?;

    let ids: Vec<Uuid> = rows
        .iter()
        .map(|row| row.try_get::<Uuid, _>("id"))
        .collect::<Result<_, _>>()?;
    let mut evidence = evidence_for(tx, EvidenceOwner::Fact, &ids).await?;

    rows.iter()
        .map(|row| {
            let id: Uuid = row.try_get("id")?;
            let kind: String = row.try_get("kind")?;
            let status: String = row.try_get("status")?;
            Ok(KnowledgeFact {
                id,
                partner_id: row.try_get("partner_id")?,
                material_id: row.try_get("material_id")?,
                run_id: row.try_get("run_id")?,
                product_id: row.try_get("product_id")?,
                product_name: row.try_get("product_name")?,
                kind: FactKind::parse(&kind)
                    .ok_or_else(|| DbError::Decode(format!("unknown fact kind `{kind}`")))?,
                status: FactStatus::parse(&status)
                    .ok_or_else(|| DbError::Decode(format!("unknown fact status `{status}`")))?,
                attribute: row.try_get("attribute")?,
                value_text: row.try_get("value_text")?,
                unit: row.try_get("unit")?,
                conditions: row.try_get("conditions")?,
                model_context: row.try_get("model_context")?,
                evidence: take_evidence(&mut evidence, id),
                created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
            })
        })
        .collect()
}

pub async fn list_terms(tx: &mut ScopedTx, partner_id: Uuid) -> DbResult<Vec<GlossaryTerm>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT id, partner_id, material_id, run_id, term, definition, \
                definition_is_model_context, created_at \
           FROM otdel.glossary_terms \
          WHERE bureau_id = $1 AND partner_id = $2 \
          ORDER BY term, id",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_all(tx.conn())
    .await?;

    let ids: Vec<Uuid> = rows
        .iter()
        .map(|row| row.try_get::<Uuid, _>("id"))
        .collect::<Result<_, _>>()?;
    let mut evidence = evidence_for(tx, EvidenceOwner::Term, &ids).await?;

    rows.iter()
        .map(|row| {
            let id: Uuid = row.try_get("id")?;
            Ok(GlossaryTerm {
                id,
                partner_id: row.try_get("partner_id")?,
                material_id: row.try_get("material_id")?,
                run_id: row.try_get("run_id")?,
                term: row.try_get("term")?,
                definition: row.try_get("definition")?,
                definition_is_model_context: row.try_get("definition_is_model_context")?,
                evidence: take_evidence(&mut evidence, id),
                created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
            })
        })
        .collect()
}

pub async fn list_qa(tx: &mut ScopedTx, partner_id: Uuid) -> DbResult<Vec<QaEntry>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT id, partner_id, material_id, run_id, question, answer, \
                answer_is_model_context, created_at \
           FROM otdel.knowledge_qa \
          WHERE bureau_id = $1 AND partner_id = $2 \
          ORDER BY created_at, id",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_all(tx.conn())
    .await?;

    let ids: Vec<Uuid> = rows
        .iter()
        .map(|row| row.try_get::<Uuid, _>("id"))
        .collect::<Result<_, _>>()?;
    let mut evidence = evidence_for(tx, EvidenceOwner::Qa, &ids).await?;

    rows.iter()
        .map(|row| {
            let id: Uuid = row.try_get("id")?;
            Ok(QaEntry {
                id,
                partner_id: row.try_get("partner_id")?,
                material_id: row.try_get("material_id")?,
                run_id: row.try_get("run_id")?,
                question: row.try_get("question")?,
                answer: row.try_get("answer")?,
                answer_is_model_context: row.try_get("answer_is_model_context")?,
                evidence: take_evidence(&mut evidence, id),
                created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
            })
        })
        .collect()
}

/// Open gaps with the question prepared from each, if any.
pub async fn list_gaps(tx: &mut ScopedTx, partner_id: Uuid) -> DbResult<Vec<KnowledgeGap>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT g.id, g.partner_id, g.material_id, g.run_id, g.product_id, p.name AS product_name, \
                g.topic, g.missing, g.blocks, g.created_at, \
                q.id AS question_id, q.audience, q.text_content, q.status AS question_status, \
                q.created_at AS question_created_at \
           FROM otdel.knowledge_gaps g \
           LEFT JOIN otdel.products p ON p.bureau_id = g.bureau_id AND p.id = g.product_id \
           LEFT JOIN LATERAL ( \
                 SELECT qq.id, qq.audience, qq.text_content, qq.status, qq.created_at \
                   FROM otdel.knowledge_questions qq \
                  WHERE qq.bureau_id = g.bureau_id AND qq.gap_id = g.id \
                  ORDER BY qq.created_at, qq.id LIMIT 1 \
           ) q ON true \
          WHERE g.bureau_id = $1 AND g.partner_id = $2 AND g.status = 'open' \
          ORDER BY g.topic, g.created_at, g.id",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            let question = match row.try_get::<Option<Uuid>, _>("question_id")? {
                Some(id) => {
                    let audience: String = row.try_get("audience")?;
                    Some(PreparedQuestion {
                        id,
                        audience: QuestionAudience::parse(&audience).ok_or_else(|| {
                            DbError::Decode(format!("unknown question audience `{audience}`"))
                        })?,
                        text: row.try_get("text_content")?,
                        status: row.try_get("question_status")?,
                        created_at: row.try_get::<DateTime<Utc>, _>("question_created_at")?,
                    })
                }
                None => None,
            };

            Ok(KnowledgeGap {
                id: row.try_get("id")?,
                partner_id: row.try_get("partner_id")?,
                material_id: row.try_get("material_id")?,
                run_id: row.try_get("run_id")?,
                product_id: row.try_get("product_id")?,
                product_name: row.try_get("product_name")?,
                topic: row.try_get("topic")?,
                missing: row.try_get("missing")?,
                blocks: row.try_get("blocks")?,
                question,
                created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
            })
        })
        .collect()
}

/// Evidence of many statements at once, with the material's file name for the link.
async fn evidence_for(
    tx: &mut ScopedTx,
    owner: EvidenceOwner,
    ids: &[Uuid],
) -> DbResult<Vec<(Uuid, FactEvidence)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let bureau_id = tx.bureau_id();
    let column = owner.column();

    let rows = sqlx::query(&format!(
        "SELECT e.id, e.{column} AS owner_id, e.material_id, m.filename, e.page_id, \
                e.page_number, e.region_id, e.quote, e.char_start, e.char_end \
           FROM otdel.knowledge_evidence e \
           JOIN otdel.materials m ON m.bureau_id = e.bureau_id AND m.id = e.material_id \
          WHERE e.bureau_id = $1 AND e.{column} = ANY($2) \
          ORDER BY e.page_number, e.char_start, e.id"
    ))
    .bind(bureau_id)
    .bind(ids)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            let owner_id: Uuid = row.try_get("owner_id")?;
            Ok((
                owner_id,
                FactEvidence {
                    id: row.try_get("id")?,
                    material_id: row.try_get("material_id")?,
                    material_filename: row.try_get("filename")?,
                    page_id: row.try_get("page_id")?,
                    page_number: row.try_get("page_number")?,
                    region_id: row.try_get("region_id")?,
                    quote: row.try_get("quote")?,
                    char_start: row.try_get("char_start")?,
                    char_end: row.try_get("char_end")?,
                },
            ))
        })
        .collect()
}

fn take_evidence(evidence: &mut Vec<(Uuid, FactEvidence)>, owner_id: Uuid) -> Vec<FactEvidence> {
    let mut taken = Vec::new();
    evidence.retain(|(id, item)| {
        if *id == owner_id {
            taken.push(item.clone());
            false
        } else {
            true
        }
    });
    taken
}

fn count(row: &PgRow, column: &str) -> DbResult<i32> {
    Ok(i32::try_from(row.try_get::<i64, _>(column)?).unwrap_or(i32::MAX))
}
