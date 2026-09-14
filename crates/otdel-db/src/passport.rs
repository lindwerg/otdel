//! Writing what R05 added: the page account, the explicit absences, surface forms,
//! senses, the application map, the uncertainties and the identity proposals.
//!
//! Split from [`crate::knowledge`] because the two answer different questions. That file
//! stores *what the material says*; this one stores *what the run covered, what it
//! refused to say, and what it could not settle* — and those are exactly the parts the
//! audited run had no way to record at all.
//!
//! Three properties hold throughout, and none of them is a convention this module has to
//! remember:
//!
//! * **Nothing here is storable without its source.** `page_id`, `quote` and the offsets
//!   are `NOT NULL` in `0009_product_passports.sql` for every table that makes a claim,
//!   tied to the page by a composite foreign key. An unsourced alias, sense or parameter
//!   is not a row this module declines to write — it is not representable.
//! * **A page leaves the account only with a reason.** [`record_page_coverage`] writes
//!   one row per page of the material, and the `disposition <> 'processed' → reason`
//!   constraint refuses the silent case.
//! * **An identity is proposed, never applied.** [`propose_identity_links`] writes rows
//!   that say "these two product rows may be the same product, and here is what that
//!   rests on". The product rows themselves are never merged, never rewritten, and never
//!   consulted for an answer as if they were one.

use otdel_core::knowledge::QuestionAudience;
use otdel_core::passport::{
    AliasRelation, ApplicationDetailKind, DeclarationOrigin, DeclarationTopic, PageDisposition,
    SynonymRelation, UncertaintyKind,
};
use sqlx::Row;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::knowledge::{normalised, DraftCounts, NewEvidence, Resolved, Scope};
use crate::tenancy::ScopedTx;

// --- the shapes a caller hands in ---------------------------------------------------

/// A surface form of a product, with the one fragment that shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAlias {
    pub surface: String,
    pub relation: AliasRelation,
    pub note: Option<String>,
    pub evidence: NewEvidence,
}

/// A surface form of a glossary term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSynonym {
    pub surface: String,
    pub relation: SynonymRelation,
    pub evidence: NewEvidence,
}

/// A further reading of one term inside one material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSense {
    pub label: String,
    pub definition: String,
    pub definition_is_model_context: bool,
    pub evidence: NewEvidence,
}

/// One parameter, constraint or question under an application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewApplicationDetail {
    pub kind: ApplicationDetailKind,
    pub label: String,
    pub value_text: Option<String>,
    pub unit: Option<String>,
    pub audience: Option<QuestionAudience>,
    /// Required for a parameter or a constraint; a question may carry none.
    pub evidence: Option<NewEvidence>,
}

/// A task the material says a product serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewApplication {
    pub product_ref: Option<String>,
    pub task: String,
    pub summary: Option<String>,
    pub model_context: Option<String>,
    pub evidence: NewEvidence,
    pub details: Vec<NewApplicationDetail>,
}

/// An explicit "there is none", with the words on the record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDeclaration {
    pub topic: DeclarationTopic,
    pub stated: String,
    pub origin: DeclarationOrigin,
}

/// One page's line in the run's account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPageCoverage {
    pub page_id: Uuid,
    pub page_number: i32,
    pub disposition: PageDisposition,
    pub offered: bool,
    pub chars_sent: i32,
    pub batch_index: Option<i32>,
    /// Required whenever the disposition is not `processed`.
    pub reason: Option<String>,
}

/// Something the material states in a form nobody may read as a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewUncertainty {
    /// The product this is about, when that is known.
    ///
    /// `None` is the normal answer and not a shortcut: an uncertainty raised by a table
    /// whose subject column could not be established is, by definition, about a product
    /// nobody can name. Attributing it to one anyway would be the guess the whole verdict
    /// exists to refuse.
    pub product_id: Option<Uuid>,
    pub kind: UncertaintyKind,
    pub subject: String,
    pub detail: String,
    pub reasons: Vec<String>,
    pub quote: Option<String>,
    pub page_id: Option<Uuid>,
    pub page_number: Option<i32>,
    pub region_id: Option<Uuid>,
}

/// What one purpose-specific pass covered, ready to be stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRunPass {
    /// `inventory` | `facts` | `glossary` | `applications` | `inquiry`.
    pub purpose: String,
    pub requests_allowed: i32,
    pub requests_made: i32,
    pub pages_total: i32,
    pub pages_processed: i32,
    pub pages_deferred: i32,
    pub covered_everything: bool,
    pub truncated_retries: i32,
    pub input_chars: i32,
}

/// What [`propose_identity_links`] found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IdentityReport {
    /// Proposals resting on a designation quoted on a page of each material.
    pub linked: u32,
    /// Pairs whose names match and whose identity nothing proves.
    pub unclear: u32,
}

// --- the page account ----------------------------------------------------------------

/// Replace this run's page account.
///
/// Called on every finished pass, including one that stored no candidates: a run that
/// could not reach the model still knows which pages the material has and why none of
/// them was offered, and that record is the difference between "nothing was found" and
/// "nothing was looked at".
///
/// The delete is scoped to the run rather than the material because a run row survives a
/// re-draft (`ON CONFLICT (material_id)`), so its previous account would otherwise
/// collide with the new one on `UNIQUE (run_id, page_id)`.
pub async fn record_page_coverage(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    material_id: Uuid,
    run_id: Uuid,
    pages: &[NewPageCoverage],
) -> DbResult<u32> {
    let bureau_id = tx.bureau_id();
    sqlx::query("DELETE FROM otdel.knowledge_page_coverage WHERE bureau_id = $1 AND run_id = $2")
        .bind(bureau_id)
        .bind(run_id)
        .execute(tx.conn())
        .await?;

    let mut written = 0_u32;
    for page in pages {
        // Defence in depth beside the CHECK: a caller that lost a reason somewhere must
        // not turn a page into an unexplained absence, which is the exact shape of the
        // defect this table exists to make impossible.
        let reason = page
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty());
        if page.disposition != PageDisposition::Processed && reason.is_none() {
            return Err(DbError::Decode(format!(
                "refusing to record page {} as `{}` without a reason",
                page.page_number,
                page.disposition.as_str()
            )));
        }

        sqlx::query(
            "INSERT INTO otdel.knowledge_page_coverage \
                 (bureau_id, partner_id, material_id, run_id, page_id, page_number, \
                  disposition, offered, chars_sent, batch_index, reason) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(bureau_id)
        .bind(partner_id)
        .bind(material_id)
        .bind(run_id)
        .bind(page.page_id)
        .bind(page.page_number)
        .bind(page.disposition.as_str())
        .bind(page.offered)
        .bind(page.chars_sent)
        .bind(page.batch_index)
        .bind(reason.map(|r| clip(r, 500)))
        .execute(tx.conn())
        .await?;
        written += 1;
    }

    Ok(written)
}

/// Replace this run's per-purpose account.
///
/// R05.2. Scoped to the run for the same reason the page account is: the run row outlives
/// a re-draft, so yesterday's passes must not describe today's.
///
/// `covered_everything` is written, not derived on read. The requirement check consults it
/// to decide whether a topic may be declared empty, and a reader looking at the run has to
/// be able to see the value the check saw — a recomputation is a second opinion, and two
/// opinions about "did anything look for a term" is exactly one too many.
pub async fn record_run_passes(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    material_id: Uuid,
    run_id: Uuid,
    passes: &[NewRunPass],
) -> DbResult<u32> {
    let bureau_id = tx.bureau_id();
    sqlx::query("DELETE FROM otdel.knowledge_run_passes WHERE bureau_id = $1 AND run_id = $2")
        .bind(bureau_id)
        .bind(run_id)
        .execute(tx.conn())
        .await?;

    let mut written = 0_u32;
    for pass in passes {
        // Defence in depth beside the CHECK: a pass that made no request cannot have
        // covered anything, and letting that pair through would make "0 terms" clearable
        // by a sentence all over again.
        if pass.covered_everything && (pass.requests_made == 0 || pass.pages_deferred > 0) {
            return Err(DbError::Decode(format!(
                "refusing to record the `{}` pass as complete: {} requests, {} pages left",
                pass.purpose, pass.requests_made, pass.pages_deferred
            )));
        }
        // A retry is a request; recording more of the first than the second would make
        // the recovery look free.
        if pass.truncated_retries > pass.requests_made {
            return Err(DbError::Decode(format!(
                "the `{}` pass reports {} truncation retries out of {} requests",
                pass.purpose, pass.truncated_retries, pass.requests_made
            )));
        }

        sqlx::query(
            "INSERT INTO otdel.knowledge_run_passes \
                 (bureau_id, partner_id, material_id, run_id, purpose, requests_allowed, \
                  requests_made, pages_total, pages_processed, pages_deferred, \
                  covered_everything, truncated_retries, input_chars) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(bureau_id)
        .bind(partner_id)
        .bind(material_id)
        .bind(run_id)
        .bind(&pass.purpose)
        .bind(pass.requests_allowed)
        .bind(pass.requests_made)
        .bind(pass.pages_total)
        .bind(pass.pages_processed)
        .bind(pass.pages_deferred)
        .bind(pass.covered_everything)
        .bind(pass.truncated_retries)
        .bind(pass.input_chars)
        .execute(tx.conn())
        .await?;
        written += 1;
    }

    Ok(written)
}

// --- surface forms, senses ------------------------------------------------------------

pub(crate) async fn insert_aliases(
    tx: &mut ScopedTx,
    scope: &Scope,
    product_id: Uuid,
    aliases: &[NewAlias],
    counts: &mut DraftCounts,
) -> DbResult<()> {
    for alias in aliases {
        let result = sqlx::query(
            "INSERT INTO otdel.product_aliases \
                 (bureau_id, partner_id, material_id, run_id, product_id, surface, \
                  normalised_surface, relation, note, page_id, page_number, quote, \
                  char_start, char_end) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) \
             ON CONFLICT (product_id, normalised_surface) DO NOTHING",
        )
        .bind(scope.bureau_id)
        .bind(scope.partner_id)
        .bind(scope.material_id)
        .bind(scope.run_id)
        .bind(product_id)
        .bind(&alias.surface)
        .bind(normalised(&alias.surface))
        .bind(alias.relation.as_str())
        .bind(alias.note.as_deref())
        .bind(alias.evidence.page_id)
        .bind(alias.evidence.page_number)
        .bind(&alias.evidence.quote)
        .bind(alias.evidence.char_start)
        .bind(alias.evidence.char_end)
        .execute(tx.conn())
        .await?;
        counts.aliases += i32::try_from(result.rows_affected()).unwrap_or(0);
    }
    Ok(())
}

pub(crate) async fn insert_synonyms(
    tx: &mut ScopedTx,
    scope: &Scope,
    term_id: Uuid,
    synonyms: &[NewSynonym],
    counts: &mut DraftCounts,
) -> DbResult<()> {
    for synonym in synonyms {
        let result = sqlx::query(
            "INSERT INTO otdel.glossary_synonyms \
                 (bureau_id, partner_id, material_id, run_id, term_id, surface, \
                  normalised_surface, relation, page_id, page_number, quote, char_start, \
                  char_end) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
             ON CONFLICT (term_id, normalised_surface) DO NOTHING",
        )
        .bind(scope.bureau_id)
        .bind(scope.partner_id)
        .bind(scope.material_id)
        .bind(scope.run_id)
        .bind(term_id)
        .bind(&synonym.surface)
        .bind(normalised(&synonym.surface))
        .bind(synonym.relation.as_str())
        .bind(synonym.evidence.page_id)
        .bind(synonym.evidence.page_number)
        .bind(&synonym.evidence.quote)
        .bind(synonym.evidence.char_start)
        .bind(synonym.evidence.char_end)
        .execute(tx.conn())
        .await?;
        counts.synonyms += i32::try_from(result.rows_affected()).unwrap_or(0);
    }
    Ok(())
}

pub(crate) async fn insert_senses(
    tx: &mut ScopedTx,
    scope: &Scope,
    term_id: Uuid,
    senses: &[NewSense],
    counts: &mut DraftCounts,
) -> DbResult<()> {
    for sense in senses {
        let result = sqlx::query(
            "INSERT INTO otdel.glossary_senses \
                 (bureau_id, partner_id, material_id, run_id, term_id, label, \
                  normalised_label, definition, definition_is_model_context, page_id, \
                  page_number, quote, char_start, char_end) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) \
             ON CONFLICT (term_id, normalised_label) DO NOTHING",
        )
        .bind(scope.bureau_id)
        .bind(scope.partner_id)
        .bind(scope.material_id)
        .bind(scope.run_id)
        .bind(term_id)
        .bind(&sense.label)
        .bind(normalised(&sense.label))
        .bind(&sense.definition)
        .bind(sense.definition_is_model_context)
        .bind(sense.evidence.page_id)
        .bind(sense.evidence.page_number)
        .bind(&sense.evidence.quote)
        .bind(sense.evidence.char_start)
        .bind(sense.evidence.char_end)
        .execute(tx.conn())
        .await?;
        counts.senses += i32::try_from(result.rows_affected()).unwrap_or(0);
    }
    Ok(())
}

// --- the application map ---------------------------------------------------------------

pub(crate) async fn insert_applications(
    tx: &mut ScopedTx,
    scope: &Scope,
    applications: &[NewApplication],
    products: &Resolved,
    counts: &mut DraftCounts,
) -> DbResult<()> {
    // Two responses describing the same task for the same product are one task. Counting
    // the stored identities rather than the inputs is what keeps the run's own number
    // from claiming more tasks than the passport can show.
    let mut stored: Vec<Uuid> = Vec::with_capacity(applications.len());

    for application in applications {
        let product_id = application
            .product_ref
            .as_deref()
            .and_then(|reference| crate::knowledge::resolve(products, reference));

        // `DO UPDATE` rather than `DO NOTHING`: two requests that both described the same
        // task keep the richer of the two summaries instead of whichever arrived first.
        let row = sqlx::query(
            "INSERT INTO otdel.product_applications \
                 (bureau_id, partner_id, material_id, run_id, product_id, task, \
                  normalised_task, summary, model_context, page_id, page_number, quote, \
                  char_start, char_end) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) \
             ON CONFLICT (material_id, \
                          coalesce(product_id, '00000000-0000-0000-0000-000000000000'::uuid), \
                          normalised_task) \
             DO UPDATE SET summary = coalesce(EXCLUDED.summary, otdel.product_applications.summary), \
                           model_context = coalesce(EXCLUDED.model_context, \
                                                    otdel.product_applications.model_context) \
             RETURNING id",
        )
        .bind(scope.bureau_id)
        .bind(scope.partner_id)
        .bind(scope.material_id)
        .bind(scope.run_id)
        .bind(product_id)
        .bind(&application.task)
        .bind(normalised(&application.task))
        .bind(application.summary.as_deref())
        .bind(application.model_context.as_deref())
        .bind(application.evidence.page_id)
        .bind(application.evidence.page_number)
        .bind(&application.evidence.quote)
        .bind(application.evidence.char_start)
        .bind(application.evidence.char_end)
        .fetch_one(tx.conn())
        .await?;

        let application_id: Uuid = row.try_get("id")?;
        if !stored.contains(&application_id) {
            counts.applications += 1;
            stored.push(application_id);
        }
        insert_details(tx, scope, application_id, &application.details, counts).await?;
    }
    Ok(())
}

async fn insert_details(
    tx: &mut ScopedTx,
    scope: &Scope,
    application_id: Uuid,
    details: &[NewApplicationDetail],
    counts: &mut DraftCounts,
) -> DbResult<()> {
    for detail in details {
        // The database says the same thing, from the other direction: a parameter or a
        // constraint without a page is not insertable. Refusing here names the caller's
        // mistake instead of aborting the whole draft on a constraint.
        if detail.kind.is_a_claim() && detail.evidence.is_none() {
            return Err(DbError::Decode(format!(
                "refusing to store `{}` as a {} without the fragment it rests on",
                detail.label,
                detail.kind.as_str()
            )));
        }
        if (detail.kind == ApplicationDetailKind::Question) != detail.audience.is_some() {
            return Err(DbError::Decode(format!(
                "`{}`: an addressee belongs to a question and to nothing else",
                detail.label
            )));
        }

        let evidence = detail.evidence.as_ref();
        sqlx::query(
            "INSERT INTO otdel.application_details \
                 (bureau_id, partner_id, material_id, run_id, application_id, kind, label, \
                  value_text, unit, audience, page_id, page_number, quote, char_start, char_end) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
        )
        .bind(scope.bureau_id)
        .bind(scope.partner_id)
        .bind(scope.material_id)
        .bind(scope.run_id)
        .bind(application_id)
        .bind(detail.kind.as_str())
        .bind(&detail.label)
        .bind(detail.value_text.as_deref())
        .bind(detail.unit.as_deref())
        .bind(detail.audience.map(|audience| audience.as_str()))
        .bind(evidence.map(|item| item.page_id))
        .bind(evidence.map(|item| item.page_number))
        .bind(evidence.map(|item| item.quote.as_str()))
        .bind(evidence.map(|item| item.char_start))
        .bind(evidence.map(|item| item.char_end))
        .execute(tx.conn())
        .await?;
        counts.application_details += 1;
    }
    Ok(())
}

// --- declarations -----------------------------------------------------------------------

pub(crate) async fn insert_declarations(
    tx: &mut ScopedTx,
    scope: &Scope,
    declarations: &[NewDeclaration],
    counts: &mut DraftCounts,
) -> DbResult<()> {
    for declaration in declarations {
        let stated = declaration.stated.trim();
        if stated.is_empty() {
            return Err(DbError::Decode(format!(
                "refusing to record an empty statement about `{}`: a declaration without \
                 words is the silence it is supposed to replace",
                declaration.topic.as_str()
            )));
        }

        sqlx::query(
            "INSERT INTO otdel.knowledge_declarations \
                 (bureau_id, partner_id, material_id, run_id, topic, stated, origin) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (run_id, topic) DO UPDATE \
                SET stated = EXCLUDED.stated, origin = EXCLUDED.origin",
        )
        .bind(scope.bureau_id)
        .bind(scope.partner_id)
        .bind(scope.material_id)
        .bind(scope.run_id)
        .bind(declaration.topic.as_str())
        .bind(clip(stated, 1_000))
        .bind(declaration.origin.as_str())
        .execute(tx.conn())
        .await?;
        counts.declarations += 1;
    }
    Ok(())
}

// --- uncertainties -----------------------------------------------------------------------

/// Replace this run's uncertainties.
///
/// Scoped to the run for the same reason the page account is: the run row outlives a
/// re-draft, and yesterday's unreadable page must not be reported beside today's reading.
pub async fn record_uncertainties(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    material_id: Uuid,
    run_id: Uuid,
    uncertainties: &[NewUncertainty],
) -> DbResult<u32> {
    let bureau_id = tx.bureau_id();
    sqlx::query("DELETE FROM otdel.knowledge_uncertainties WHERE bureau_id = $1 AND run_id = $2")
        .bind(bureau_id)
        .bind(run_id)
        .execute(tx.conn())
        .await?;

    let mut written = 0_u32;
    for item in uncertainties {
        let reasons: Vec<String> = item
            .reasons
            .iter()
            .map(|reason| clip(reason, 100))
            .take(20)
            .collect();

        sqlx::query(
            "INSERT INTO otdel.knowledge_uncertainties \
                 (bureau_id, partner_id, material_id, run_id, product_id, kind, subject, \
                  detail, reasons, quote, page_id, page_number, region_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(bureau_id)
        .bind(partner_id)
        .bind(material_id)
        .bind(run_id)
        .bind(item.product_id)
        .bind(item.kind.as_str())
        .bind(clip(&item.subject, 300))
        .bind(clip(&item.detail, 1_000))
        .bind(&reasons)
        .bind(item.quote.as_deref().map(|quote| clip(quote, 600)))
        .bind(item.page_id)
        .bind(item.page_number)
        .bind(item.region_id)
        .execute(tx.conn())
        .await?;
        written += 1;
    }

    Ok(written)
}

// --- identity across materials -------------------------------------------------------------

/// Re-derive this partner's identity proposals from the candidates that exist now.
///
/// Two materials of one partner produce two product rows for one profile, by design
/// (`0004_knowledge.sql` scopes a product to its material). A passport that ignores that
/// shows the reader two half-products; one that merges them on a matching string invents
/// an identity nobody proved. A proposal is the third option, and the rules below are the
/// whole of it:
///
/// * a **designation** is a name that is genuinely quoted on a page — either the product's
///   own name, found inside the quotation of one of its facts, or a recorded alias, whose
///   quotation is required to contain it;
/// * two products of different materials that share a quoted designation are proposed as
///   `linked`, with the page on each side. The basis distinguishes a shared *name* from a
///   shared *alias*, because they are different amounts of evidence;
/// * two products whose names match and which share no quoted designation are proposed as
///   `unclear` on the basis `name_similarity_only`, with no pages. The database refuses to
///   let that combination be a link, so this is not a rule that can be forgotten.
///
/// The partner's proposals are rebuilt rather than amended: they are derived entirely from
/// rows that exist right now, carry no human decision, and an amended set could keep a
/// proposal whose evidence was deleted by a re-draft.
pub async fn propose_identity_links(
    tx: &mut ScopedTx,
    partner_id: Uuid,
) -> DbResult<IdentityReport> {
    let bureau_id = tx.bureau_id();

    sqlx::query(
        "DELETE FROM otdel.product_identity_links WHERE bureau_id = $1 AND partner_id = $2",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .execute(tx.conn())
    .await?;

    // `DISTINCT ON` keeps one page per (product, designation): the proposal needs *a*
    // page on each side, not every page the name occurs on.
    let rows = sqlx::query(
        "WITH designations AS ( \
             (SELECT DISTINCT ON (p.id, p.normalised_name) \
                     p.id AS product_id, p.material_id, p.normalised_name AS form, \
                     true AS is_own_name, e.page_id, e.page_number \
                FROM otdel.products p \
                JOIN otdel.knowledge_facts f \
                  ON f.bureau_id = p.bureau_id AND f.product_id = p.id \
                JOIN otdel.knowledge_evidence e \
                  ON e.bureau_id = f.bureau_id AND e.fact_id = f.id \
               WHERE p.bureau_id = $1 AND p.partner_id = $2 \
                 AND position(lower(p.name) IN lower(e.quote)) > 0 \
               ORDER BY p.id, p.normalised_name, e.page_number, e.char_start) \
             UNION ALL \
             (SELECT DISTINCT ON (a.product_id, a.normalised_surface) \
                     a.product_id, a.material_id, a.normalised_surface AS form, \
                     false AS is_own_name, a.page_id, a.page_number \
                FROM otdel.product_aliases a \
               WHERE a.bureau_id = $1 AND a.partner_id = $2 AND a.relation = 'alias' \
               ORDER BY a.product_id, a.normalised_surface, a.page_number) \
         ), \
         matched AS ( \
             SELECT DISTINCT ON (l.product_id, r.product_id) \
                    l.product_id, r.product_id AS other_product_id, \
                    CASE WHEN l.is_own_name AND r.is_own_name \
                         THEN 'identical_designation_quoted' \
                         ELSE 'alias_quoted_in_both' END AS basis, \
                    l.form, l.material_id, l.page_id, l.page_number, \
                    r.material_id AS other_material_id, r.page_id AS other_page_id, \
                    r.page_number AS other_page_number \
               FROM designations l \
               JOIN designations r \
                 ON r.form = l.form AND r.material_id <> l.material_id \
                AND r.product_id > l.product_id \
              ORDER BY l.product_id, r.product_id, (l.is_own_name AND r.is_own_name) DESC \
         ), \
         resembling AS ( \
             SELECT l.id AS product_id, r.id AS other_product_id, l.normalised_name AS form \
               FROM otdel.products l \
               JOIN otdel.products r \
                 ON r.bureau_id = l.bureau_id AND r.partner_id = l.partner_id \
                AND r.normalised_name = l.normalised_name \
                AND r.material_id <> l.material_id AND r.id > l.id \
              WHERE l.bureau_id = $1 AND l.partner_id = $2 \
                AND NOT EXISTS (SELECT 1 FROM matched m \
                                 WHERE m.product_id = l.id AND m.other_product_id = r.id) \
         ) \
         INSERT INTO otdel.product_identity_links \
             (bureau_id, partner_id, product_id, other_product_id, state, basis, note, \
              material_id, page_id, other_material_id, other_page_id) \
         SELECT $1, $2, product_id, other_product_id, 'linked', basis, \
                'общее обозначение «' || form || '», процитировано на странице ' \
                    || page_number || ' и на странице ' || other_page_number, \
                material_id, page_id, other_material_id, other_page_id \
           FROM matched \
         UNION ALL \
         SELECT $1, $2, product_id, other_product_id, 'unclear', 'name_similarity_only', \
                'названия совпадают («' || form || '»), но ни на одной странице это \
                 обозначение не процитировано с обеих сторон', \
                NULL, NULL, NULL, NULL \
           FROM resembling \
         RETURNING state",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_all(tx.conn())
    .await?;

    let mut report = IdentityReport::default();
    for row in &rows {
        match row.try_get::<String, _>("state")?.as_str() {
            "linked" => report.linked += 1,
            _ => report.unclear += 1,
        }
    }
    Ok(report)
}

/// Bounded to the column's own limit, on a character boundary.
fn clip(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_is_cut_on_a_character_boundary_not_a_byte_one() {
        assert_eq!(clip("нагрузка", 3), "наг");
        assert_eq!(clip("нагрузка", 100), "нагрузка");
        assert_eq!(clip("", 10), "");
    }

    #[test]
    fn a_claim_without_its_fragment_is_named_as_the_callers_mistake() {
        // Both halves of the application-detail rule, as a pure check on the input shape:
        // what the database refuses, this refuses first and says which line it was.
        let parameter = NewApplicationDetail {
            kind: ApplicationDetailKind::Parameter,
            label: "глубина анкеровки".to_owned(),
            value_text: Some("60 мм".to_owned()),
            unit: None,
            audience: None,
            evidence: None,
        };
        assert!(parameter.kind.is_a_claim() && parameter.evidence.is_none());

        let question = NewApplicationDetail {
            kind: ApplicationDetailKind::Question,
            label: "какой бетон?".to_owned(),
            value_text: None,
            unit: None,
            audience: Some(QuestionAudience::Partner),
            evidence: None,
        };
        // A question asserts nothing, so it is storable without a fragment — and it is
        // the only kind that is.
        assert!(!question.kind.is_a_claim());
        assert!(question.audience.is_some());
    }
}
