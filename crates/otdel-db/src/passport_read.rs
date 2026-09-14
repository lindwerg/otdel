//! Reading the product base back: passports, the page account, the explicit absences,
//! the application map and the uncertainties.
//!
//! The composition rule of this module, stated once: **a passport is assembled, never
//! stored**. Every part of [`ProductPassport`] comes from the table that owns it, so a
//! passport cannot drift from the candidates it describes, and there is no second copy to
//! keep in step.
//!
//! The second rule is what the assembly refuses to leave out. [`list_passports`] returns
//! the gaps, the uncertainties and the identity proposals alongside the facts, in one
//! call, because a caller that could fetch the facts on their own would eventually render
//! a product that looks complete and is not. That is the whole failure this package
//! answers, moved one layer up.

use chrono::{DateTime, Utc};
use otdel_core::knowledge::{ProductCategory, QuestionAudience};
use otdel_core::passport::{
    AliasRelation, ApplicationDetail, ApplicationDetailKind, DeclarationOrigin, DeclarationTopic,
    GlossarySense, GlossarySynonym, IdentityBasis, IdentityState, KnowledgeDeclaration,
    KnowledgeUncertainty, PageCoverage, PageDisposition, ProductAlias, ProductApplication,
    ProductIdentityLink, ProductPassport, RunPass, SynonymRelation, UncertaintyKind,
};
use sqlx::postgres::PgRow;
use sqlx::Row;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::knowledge_read::{self, take_owned};
use crate::tenancy::ScopedTx;

/// Columns every sourced R05 row carries, in one place so the readers below cannot
/// disagree about what provenance means. Only usable where the query has one table and
/// therefore needs no prefix.
const EVIDENCE_COLUMNS: &str = "page_id, page_number, quote, char_start, char_end";

// --- the page account ------------------------------------------------------------------

/// What happened to every page of a material during one run, in page order.
///
/// Returning the whole account rather than only the unhappy lines is deliberate: a reader
/// who sees six explained pages and no others cannot tell whether the material has six
/// pages or forty-four.
pub async fn page_coverage(tx: &mut ScopedTx, run_id: Uuid) -> DbResult<Vec<PageCoverage>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT id, material_id, run_id, page_id, page_number, disposition, offered, \
                chars_sent, batch_index, reason, created_at \
           FROM otdel.knowledge_page_coverage \
          WHERE bureau_id = $1 AND run_id = $2 \
          ORDER BY page_number, id",
    )
    .bind(bureau_id)
    .bind(run_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            let disposition: String = row.try_get("disposition")?;
            Ok(PageCoverage {
                id: row.try_get("id")?,
                material_id: row.try_get("material_id")?,
                run_id: row.try_get("run_id")?,
                page_id: row.try_get("page_id")?,
                page_number: row.try_get("page_number")?,
                disposition: PageDisposition::parse(&disposition).ok_or_else(|| {
                    DbError::Decode(format!("unknown page disposition `{disposition}`"))
                })?,
                offered: row.try_get("offered")?,
                chars_sent: row.try_get("chars_sent")?,
                batch_index: row.try_get("batch_index")?,
                reason: row.try_get("reason")?,
                created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
            })
        })
        .collect()
}

/// What each purpose-specific pass of a run covered.
///
/// R05.2. Returned with the coverage report and never on its own: "44 of 44 pages
/// processed" was a true sentence that could not answer «почему 0 терминов», and the whole
/// point of these rows is that the two questions are now asked together.
pub async fn run_passes(tx: &mut ScopedTx, run_id: Uuid) -> DbResult<Vec<RunPass>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT id, run_id, material_id, purpose, requests_allowed, requests_made, \
                pages_total, pages_processed, pages_deferred, covered_everything, \
                truncated_retries, input_chars, created_at \
           FROM otdel.knowledge_run_passes \
          WHERE bureau_id = $1 AND run_id = $2 \
          ORDER BY created_at, id",
    )
    .bind(bureau_id)
    .bind(run_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            Ok(RunPass {
                id: row.try_get("id")?,
                run_id: row.try_get("run_id")?,
                material_id: row.try_get("material_id")?,
                purpose: row.try_get("purpose")?,
                requests_allowed: row.try_get("requests_allowed")?,
                requests_made: row.try_get("requests_made")?,
                pages_total: row.try_get("pages_total")?,
                pages_processed: row.try_get("pages_processed")?,
                pages_deferred: row.try_get("pages_deferred")?,
                covered_everything: row.try_get("covered_everything")?,
                truncated_retries: row.try_get("truncated_retries")?,
                input_chars: row.try_get("input_chars")?,
                created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
            })
        })
        .collect()
}

// --- declarations -------------------------------------------------------------------------

/// The explicit absences a partner's runs have on the record.
pub async fn list_declarations(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    run_id: Option<Uuid>,
) -> DbResult<Vec<KnowledgeDeclaration>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT id, material_id, run_id, topic, stated, origin, created_at \
           FROM otdel.knowledge_declarations \
          WHERE bureau_id = $1 AND partner_id = $2 AND ($3::uuid IS NULL OR run_id = $3) \
          ORDER BY topic, created_at, id",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .bind(run_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            let topic: String = row.try_get("topic")?;
            let origin: String = row.try_get("origin")?;
            Ok(KnowledgeDeclaration {
                id: row.try_get("id")?,
                material_id: row.try_get("material_id")?,
                run_id: row.try_get("run_id")?,
                topic: DeclarationTopic::parse(&topic).ok_or_else(|| {
                    DbError::Decode(format!("unknown declaration topic `{topic}`"))
                })?,
                stated: row.try_get("stated")?,
                origin: DeclarationOrigin::parse(&origin).ok_or_else(|| {
                    DbError::Decode(format!("unknown declaration origin `{origin}`"))
                })?,
                created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
            })
        })
        .collect()
}

// --- surface forms and senses ----------------------------------------------------------------

/// Aliases of many products at once, keyed by product.
pub(crate) async fn aliases_for(
    tx: &mut ScopedTx,
    product_ids: &[Uuid],
) -> DbResult<Vec<(Uuid, ProductAlias)>> {
    if product_ids.is_empty() {
        return Ok(Vec::new());
    }
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(&format!(
        "SELECT id, product_id, material_id, run_id, surface, relation, note, \
                {EVIDENCE_COLUMNS}, created_at \
           FROM otdel.product_aliases \
          WHERE bureau_id = $1 AND product_id = ANY($2) \
          ORDER BY relation, surface, id"
    ))
    .bind(bureau_id)
    .bind(product_ids)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            let relation: String = row.try_get("relation")?;
            Ok((
                row.try_get("product_id")?,
                ProductAlias {
                    id: row.try_get("id")?,
                    product_id: row.try_get("product_id")?,
                    material_id: row.try_get("material_id")?,
                    run_id: row.try_get("run_id")?,
                    surface: row.try_get("surface")?,
                    relation: AliasRelation::parse(&relation).ok_or_else(|| {
                        DbError::Decode(format!("unknown alias relation `{relation}`"))
                    })?,
                    note: row.try_get("note")?,
                    page_id: row.try_get("page_id")?,
                    page_number: row.try_get("page_number")?,
                    quote: row.try_get("quote")?,
                    char_start: row.try_get("char_start")?,
                    char_end: row.try_get("char_end")?,
                    created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
                },
            ))
        })
        .collect()
}

/// Further readings of many terms at once, keyed by term.
pub(crate) async fn senses_for(
    tx: &mut ScopedTx,
    term_ids: &[Uuid],
) -> DbResult<Vec<(Uuid, GlossarySense)>> {
    if term_ids.is_empty() {
        return Ok(Vec::new());
    }
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(&format!(
        "SELECT id, term_id, material_id, run_id, label, definition, \
                definition_is_model_context, {EVIDENCE_COLUMNS}, created_at \
           FROM otdel.glossary_senses \
          WHERE bureau_id = $1 AND term_id = ANY($2) \
          ORDER BY label, id"
    ))
    .bind(bureau_id)
    .bind(term_ids)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            Ok((
                row.try_get("term_id")?,
                GlossarySense {
                    id: row.try_get("id")?,
                    term_id: row.try_get("term_id")?,
                    material_id: row.try_get("material_id")?,
                    run_id: row.try_get("run_id")?,
                    label: row.try_get("label")?,
                    definition: row.try_get("definition")?,
                    definition_is_model_context: row.try_get("definition_is_model_context")?,
                    page_id: row.try_get("page_id")?,
                    page_number: row.try_get("page_number")?,
                    quote: row.try_get("quote")?,
                    char_start: row.try_get("char_start")?,
                    char_end: row.try_get("char_end")?,
                    created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
                },
            ))
        })
        .collect()
}

/// Spellings of many terms at once, keyed by term.
pub(crate) async fn synonyms_for(
    tx: &mut ScopedTx,
    term_ids: &[Uuid],
) -> DbResult<Vec<(Uuid, GlossarySynonym)>> {
    if term_ids.is_empty() {
        return Ok(Vec::new());
    }
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(&format!(
        "SELECT id, term_id, material_id, run_id, surface, relation, {EVIDENCE_COLUMNS}, \
                created_at \
           FROM otdel.glossary_synonyms \
          WHERE bureau_id = $1 AND term_id = ANY($2) \
          ORDER BY relation, surface, id"
    ))
    .bind(bureau_id)
    .bind(term_ids)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            let relation: String = row.try_get("relation")?;
            Ok((
                row.try_get("term_id")?,
                GlossarySynonym {
                    id: row.try_get("id")?,
                    term_id: row.try_get("term_id")?,
                    material_id: row.try_get("material_id")?,
                    run_id: row.try_get("run_id")?,
                    surface: row.try_get("surface")?,
                    relation: SynonymRelation::parse(&relation).ok_or_else(|| {
                        DbError::Decode(format!("unknown synonym relation `{relation}`"))
                    })?,
                    page_id: row.try_get("page_id")?,
                    page_number: row.try_get("page_number")?,
                    quote: row.try_get("quote")?,
                    char_start: row.try_get("char_start")?,
                    char_end: row.try_get("char_end")?,
                    created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
                },
            ))
        })
        .collect()
}

// --- the application map ---------------------------------------------------------------------

/// Tasks a partner's materials say the products serve, each with its details.
///
/// `product_id` narrows to one product; `None` returns the whole map, including the tasks
/// the material states for the offering as a whole.
pub async fn list_applications(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    product_id: Option<Uuid>,
) -> DbResult<Vec<ProductApplication>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT a.id, a.partner_id, a.material_id, a.run_id, a.product_id, \
                p.name AS product_name, a.task, a.summary, a.model_context, \
                a.page_id, a.page_number, a.quote, a.char_start, a.char_end, a.created_at \
           FROM otdel.product_applications a \
           LEFT JOIN otdel.products p ON p.bureau_id = a.bureau_id AND p.id = a.product_id \
          WHERE a.bureau_id = $1 AND a.partner_id = $2 \
            AND ($3::uuid IS NULL OR a.product_id = $3) \
          ORDER BY p.name NULLS FIRST, a.task, a.id",
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
    let mut details = details_for(tx, &ids).await?;

    rows.iter()
        .map(|row| {
            let id: Uuid = row.try_get("id")?;
            Ok(ProductApplication {
                id,
                partner_id: row.try_get("partner_id")?,
                material_id: row.try_get("material_id")?,
                run_id: row.try_get("run_id")?,
                product_id: row.try_get("product_id")?,
                product_name: row.try_get("product_name")?,
                task: row.try_get("task")?,
                summary: row.try_get("summary")?,
                model_context: row.try_get("model_context")?,
                page_id: row.try_get("page_id")?,
                page_number: row.try_get("page_number")?,
                quote: row.try_get("quote")?,
                char_start: row.try_get("char_start")?,
                char_end: row.try_get("char_end")?,
                details: take_owned(&mut details, id),
                created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
            })
        })
        .collect()
}

/// Parameters, constraints and questions of many applications at once.
///
/// Ordered by kind so the interface shows what is known before what is missing, without
/// having to sort it again and risk sorting it differently.
async fn details_for(
    tx: &mut ScopedTx,
    application_ids: &[Uuid],
) -> DbResult<Vec<(Uuid, ApplicationDetail)>> {
    if application_ids.is_empty() {
        return Ok(Vec::new());
    }
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(&format!(
        "SELECT id, application_id, kind, label, value_text, unit, audience, \
                {EVIDENCE_COLUMNS}, created_at \
           FROM otdel.application_details \
          WHERE bureau_id = $1 AND application_id = ANY($2) \
          ORDER BY CASE kind WHEN 'parameter' THEN 0 WHEN 'constraint' THEN 1 ELSE 2 END, \
                   label, id"
    ))
    .bind(bureau_id)
    .bind(application_ids)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            let kind: String = row.try_get("kind")?;
            let audience: Option<String> = row.try_get("audience")?;
            let audience = match audience.as_deref() {
                Some(value) => Some(QuestionAudience::parse(value).ok_or_else(|| {
                    DbError::Decode(format!("unknown question audience `{value}`"))
                })?),
                None => None,
            };
            Ok((
                row.try_get("application_id")?,
                ApplicationDetail {
                    id: row.try_get("id")?,
                    application_id: row.try_get("application_id")?,
                    kind: ApplicationDetailKind::parse(&kind).ok_or_else(|| {
                        DbError::Decode(format!("unknown application detail kind `{kind}`"))
                    })?,
                    label: row.try_get("label")?,
                    value_text: row.try_get("value_text")?,
                    unit: row.try_get("unit")?,
                    audience,
                    page_id: row.try_get("page_id")?,
                    page_number: row.try_get("page_number")?,
                    quote: row.try_get("quote")?,
                    char_start: row.try_get("char_start")?,
                    char_end: row.try_get("char_end")?,
                    created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
                },
            ))
        })
        .collect()
}

// --- uncertainties ---------------------------------------------------------------------------

/// What a partner's materials state in a form nobody may read as a value.
pub async fn list_uncertainties(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    product_id: Option<Uuid>,
) -> DbResult<Vec<KnowledgeUncertainty>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT id, partner_id, material_id, run_id, product_id, kind, subject, detail, \
                reasons, quote, page_id, page_number, region_id, status, created_at \
           FROM otdel.knowledge_uncertainties \
          WHERE bureau_id = $1 AND partner_id = $2 AND status = 'open' \
            AND ($3::uuid IS NULL OR product_id = $3) \
          ORDER BY page_number NULLS LAST, kind, id",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .bind(product_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter().map(uncertainty_from_row).collect()
}

fn uncertainty_from_row(row: &PgRow) -> DbResult<KnowledgeUncertainty> {
    let kind: String = row.try_get("kind")?;
    Ok(KnowledgeUncertainty {
        id: row.try_get("id")?,
        partner_id: row.try_get("partner_id")?,
        material_id: row.try_get("material_id")?,
        run_id: row.try_get("run_id")?,
        product_id: row.try_get("product_id")?,
        kind: UncertaintyKind::parse(&kind)
            .ok_or_else(|| DbError::Decode(format!("unknown uncertainty kind `{kind}`")))?,
        subject: row.try_get("subject")?,
        detail: row.try_get("detail")?,
        reasons: row.try_get::<Vec<String>, _>("reasons")?,
        quote: row.try_get("quote")?,
        page_id: row.try_get("page_id")?,
        page_number: row.try_get("page_number")?,
        region_id: row.try_get("region_id")?,
        status: row.try_get("status")?,
        created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
    })
}

// --- identity across materials -----------------------------------------------------------------

/// Identity proposals of a partner, with the page on each side where there is one.
pub async fn list_identity_links(
    tx: &mut ScopedTx,
    partner_id: Uuid,
) -> DbResult<Vec<ProductIdentityLink>> {
    let bureau_id = tx.bureau_id();
    // Both directions, so a passport can ask "what is proposed about *this* product"
    // without knowing which side of the pair it landed on when the proposal was derived.
    let rows = sqlx::query(
        "SELECT l.id, l.product_id, l.other_product_id, l.state, l.basis, l.note, \
                l.material_id, l.page_id, lp.page_number, \
                l.other_material_id, l.other_page_id, rp.page_number AS other_page_number, \
                l.created_at \
           FROM otdel.product_identity_links l \
           LEFT JOIN otdel.material_pages lp \
             ON lp.bureau_id = l.bureau_id AND lp.id = l.page_id \
           LEFT JOIN otdel.material_pages rp \
             ON rp.bureau_id = l.bureau_id AND rp.id = l.other_page_id \
          WHERE l.bureau_id = $1 AND l.partner_id = $2 \
          ORDER BY l.state, l.created_at, l.id",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter().map(identity_from_row).collect()
}

fn identity_from_row(row: &PgRow) -> DbResult<ProductIdentityLink> {
    let state: String = row.try_get("state")?;
    let basis: String = row.try_get("basis")?;
    Ok(ProductIdentityLink {
        id: row.try_get("id")?,
        product_id: row.try_get("product_id")?,
        other_product_id: row.try_get("other_product_id")?,
        state: IdentityState::parse(&state)
            .ok_or_else(|| DbError::Decode(format!("unknown identity state `{state}`")))?,
        basis: IdentityBasis::parse(&basis)
            .ok_or_else(|| DbError::Decode(format!("unknown identity basis `{basis}`")))?,
        note: row.try_get("note")?,
        material_id: row.try_get("material_id")?,
        page_id: row.try_get("page_id")?,
        page_number: row.try_get("page_number")?,
        other_material_id: row.try_get("other_material_id")?,
        other_page_id: row.try_get("other_page_id")?,
        other_page_number: row.try_get("other_page_number")?,
        created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
    })
}

/// Flip a proposal so it reads from `product_id`'s side.
///
/// A proposal is stored once, in a canonical order. Showing it unflipped on the second
/// product's passport would print "эта запись совпадает с …" beside the record the reader
/// is already looking at.
fn oriented(link: &ProductIdentityLink, product_id: Uuid) -> ProductIdentityLink {
    if link.product_id == product_id {
        return link.clone();
    }
    ProductIdentityLink {
        product_id: link.other_product_id,
        other_product_id: link.product_id,
        material_id: link.other_material_id,
        page_id: link.other_page_id,
        page_number: link.other_page_number,
        other_material_id: link.material_id,
        other_page_id: link.page_id,
        other_page_number: link.page_number,
        ..link.clone()
    }
}

// --- passports ------------------------------------------------------------------------------------

/// Everything known about every product of a partner, assembled for reading.
///
/// One pass over each table rather than one query per product: a partner with two hundred
/// products would otherwise make the interface choose between being slow and showing
/// less, and "showing less" is how a passport stops being one.
pub async fn list_passports(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    product_id: Option<Uuid>,
) -> DbResult<Vec<ProductPassport>> {
    let mut products = knowledge_read::list_products(tx, partner_id).await?;
    if let Some(wanted) = product_id {
        products.retain(|product| product.id == wanted);
    }
    if products.is_empty() {
        return Ok(Vec::new());
    }

    let categories = knowledge_read::list_categories(tx, partner_id).await?;
    let filenames = material_filenames(tx, partner_id).await?;
    let ids: Vec<Uuid> = products.iter().map(|product| product.id).collect();

    let mut aliases = aliases_for(tx, &ids).await?;
    let mut facts = by_product(
        knowledge_read::list_facts(tx, partner_id, None).await?,
        |fact| fact.product_id,
    );
    let mut applications = by_product(
        list_applications(tx, partner_id, None).await?,
        |application| application.product_id,
    );
    let mut gaps = by_product(knowledge_read::list_gaps(tx, partner_id).await?, |gap| {
        gap.product_id
    });
    let mut uncertainties = by_product(
        list_uncertainties(tx, partner_id, None).await?,
        |uncertainty| uncertainty.product_id,
    );
    let links = list_identity_links(tx, partner_id).await?;

    Ok(products
        .into_iter()
        .map(|product| {
            let id = product.id;
            ProductPassport {
                category: category_of(&categories, product.category_id),
                material_filename: filenames
                    .iter()
                    .find(|(material_id, _)| *material_id == product.material_id)
                    .map(|(_, filename)| filename.clone())
                    .unwrap_or_default(),
                aliases: take_owned(&mut aliases, id),
                facts: take_owned(&mut facts, id),
                applications: take_owned(&mut applications, id),
                gaps: take_owned(&mut gaps, id),
                uncertainties: take_owned(&mut uncertainties, id),
                identity_links: links
                    .iter()
                    .filter(|link| link.product_id == id || link.other_product_id == id)
                    .map(|link| oriented(link, id))
                    .collect(),
                product,
            }
        })
        .collect())
}

/// The passport of one product, or `None` when this partner has no such product.
pub async fn find_passport(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    product_id: Uuid,
) -> DbResult<Option<ProductPassport>> {
    Ok(list_passports(tx, partner_id, Some(product_id))
        .await?
        .into_iter()
        .next())
}

/// Rekey a partner-wide list by the product it belongs to, dropping the rows that belong
/// to no product.
///
/// Those rows are not lost: a fact or a task the material states about the offering as a
/// whole is still returned by its own endpoint. It simply has no passport to sit in, and
/// filing it under an arbitrary product would be an attribution nobody made.
fn by_product<T>(items: Vec<T>, key: impl Fn(&T) -> Option<Uuid>) -> Vec<(Uuid, T)> {
    items
        .into_iter()
        .filter_map(|item| key(&item).map(|id| (id, item)))
        .collect()
}

fn category_of(categories: &[ProductCategory], id: Option<Uuid>) -> Option<ProductCategory> {
    let id = id?;
    categories
        .iter()
        .find(|category| category.id == id)
        .cloned()
}

/// `material_id → filename`, so a passport can name the catalogue that is speaking.
async fn material_filenames(tx: &mut ScopedTx, partner_id: Uuid) -> DbResult<Vec<(Uuid, String)>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT id, filename FROM otdel.materials WHERE bureau_id = $1 AND partner_id = $2",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| Ok((row.try_get("id")?, row.try_get("filename")?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link() -> ProductIdentityLink {
        ProductIdentityLink {
            id: Uuid::from_u128(1),
            product_id: Uuid::from_u128(10),
            other_product_id: Uuid::from_u128(20),
            state: IdentityState::Linked,
            basis: IdentityBasis::IdenticalDesignationQuoted,
            note: Some("общее обозначение".to_owned()),
            material_id: Some(Uuid::from_u128(100)),
            page_id: Some(Uuid::from_u128(101)),
            page_number: Some(4),
            other_material_id: Some(Uuid::from_u128(200)),
            other_page_id: Some(Uuid::from_u128(201)),
            other_page_number: Some(9),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn a_proposal_reads_from_the_side_whose_passport_shows_it() {
        let stored = link();

        // The canonical side is shown unchanged.
        let near = oriented(&stored, stored.product_id);
        assert_eq!(near.product_id, stored.product_id);
        assert_eq!(near.page_number, Some(4));
        assert_eq!(near.other_page_number, Some(9));

        // The other product's passport sees itself first — otherwise it would print
        // "this matches …" beside the very record the reader is looking at.
        let far = oriented(&stored, stored.other_product_id);
        assert_eq!(far.product_id, stored.other_product_id);
        assert_eq!(far.other_product_id, stored.product_id);
        assert_eq!(far.page_number, Some(9));
        assert_eq!(far.other_page_number, Some(4));
        // Flipping changes the point of view and nothing else.
        assert_eq!(far.state, stored.state);
        assert_eq!(far.basis, stored.basis);
        assert_eq!(far.note, stored.note);
    }

    #[test]
    fn a_row_belonging_to_no_product_is_left_out_of_every_passport() {
        let rows = vec![(Some(Uuid::from_u128(5)), "о изделии"), (None, "об офере")];
        let keyed = by_product(rows, |(product_id, _)| *product_id);
        assert_eq!(keyed.len(), 1);
        assert_eq!(keyed[0].0, Uuid::from_u128(5));
    }
}
