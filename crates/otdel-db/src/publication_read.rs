//! Reading versions, and gathering what the checker needs in order to build one.
//!
//! The interesting function here is [`load_candidates`]. It does not hand the checker the
//! quotations 1C and 1D stored; it hands it those quotations **together with the page and
//! snapshot text as they are stored right now**, so the checker can locate every citation
//! again and decide for itself. A `LEFT JOIN` is doing load-bearing work: a page row that
//! is gone yields a `NULL` text, which becomes [`otdel_core::publication::ClaimStatus::
//! Unknown`] — "the source cannot be read" — and is deliberately not the same answer as
//! "the source no longer says this".
//!
//! Everything else here is a plain read, scoped by bureau in SQL as well as by row-level
//! security.

use chrono::{DateTime, Utc};
use otdel_core::knowledge::FactKind;
use otdel_core::publication::{
    CandidateSummary, ClaimOrigin, ClaimScope, ClaimStatus, EvidenceSourceKind, KnowledgeVersion,
    ReadinessEntry, ReadinessState, ReadinessTopic, ValidationRun, ValidationRunStatus,
    VersionClaim, VersionEvidence, VersionGap, VersionStatus,
};
use otdel_publish::{CandidateClaim, CandidateEvidence, GapText};
use sqlx::postgres::PgRow;
use sqlx::Row;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::tenancy::ScopedTx;

/// Counts of the snapshot rows, computed at read time rather than stored — the same rule
/// 1C and 1D follow, so a counter can never disagree with the rows it counts.
const VERSION_COUNTS: &str = "\
    (SELECT count(*) FROM otdel.version_claims c WHERE c.version_id = v.id) AS claims_total, \
    (SELECT count(*) FROM otdel.version_claims c WHERE c.version_id = v.id \
       AND c.status = 'source_supported') AS claims_source_supported, \
    (SELECT count(*) FROM otdel.version_claims c WHERE c.version_id = v.id \
       AND c.status = 'hypothesis') AS claims_hypothesis, \
    (SELECT count(*) FROM otdel.version_claims c WHERE c.version_id = v.id \
       AND c.status = 'unknown') AS claims_unknown, \
    (SELECT count(*) FROM otdel.version_claims c WHERE c.version_id = v.id \
       AND c.status = 'conflicted') AS claims_conflicted, \
    (SELECT count(*) FROM otdel.version_claims c WHERE c.version_id = v.id \
       AND c.status = 'stale') AS claims_stale, \
    (SELECT count(*) FROM otdel.version_gaps g WHERE g.version_id = v.id) AS gaps_open, \
    (SELECT count(*) FROM otdel.version_chunks k WHERE k.version_id = v.id) AS chunks_total, \
    (SELECT count(*) FROM otdel.version_chunks k WHERE k.version_id = v.id \
       AND k.embedding_profile IS NOT NULL) AS chunks_embedded";

fn count(row: &PgRow, column: &str) -> DbResult<i32> {
    Ok(i32::try_from(row.try_get::<i64, _>(column)?).unwrap_or(i32::MAX))
}

// --- versions -----------------------------------------------------------------------

fn version_from_row(row: &PgRow, readiness: Vec<ReadinessEntry>) -> DbResult<KnowledgeVersion> {
    let status: String = row.try_get("status")?;
    Ok(KnowledgeVersion {
        id: row.try_get("id")?,
        partner_id: row.try_get("partner_id")?,
        number: row.try_get("number")?,
        status: VersionStatus::parse(&status)
            .ok_or_else(|| DbError::Decode(format!("unknown version status `{status}`")))?,
        validation_run_id: row.try_get("validation_run_id")?,
        input_fingerprint: row.try_get("input_fingerprint")?,
        claims_total: count(row, "claims_total")?,
        claims_source_supported: count(row, "claims_source_supported")?,
        claims_hypothesis: count(row, "claims_hypothesis")?,
        claims_unknown: count(row, "claims_unknown")?,
        claims_conflicted: count(row, "claims_conflicted")?,
        claims_stale: count(row, "claims_stale")?,
        gaps_open: count(row, "gaps_open")?,
        chunks_total: count(row, "chunks_total")?,
        chunks_embedded: count(row, "chunks_embedded")?,
        embedding_profile: row.try_get("embedding_profile")?,
        readiness,
        blocked_reasons: row.try_get("blocked_reasons")?,
        created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
        published_at: row.try_get::<Option<DateTime<Utc>>, _>("published_at")?,
        superseded_at: row.try_get::<Option<DateTime<Utc>>, _>("superseded_at")?,
        revoked_at: row.try_get::<Option<DateTime<Utc>>, _>("revoked_at")?,
        revoked_reason: row.try_get("revoked_reason")?,
    })
}

pub async fn list_versions(tx: &mut ScopedTx, partner_id: Uuid) -> DbResult<Vec<KnowledgeVersion>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(&format!(
        "SELECT v.*, {VERSION_COUNTS} FROM otdel.knowledge_versions v \
          WHERE v.bureau_id = $1 AND v.partner_id = $2 \
          ORDER BY v.number DESC"
    ))
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_all(tx.conn())
    .await?;

    let ids: Vec<Uuid> = rows
        .iter()
        .map(|row| row.try_get("id"))
        .collect::<Result<_, _>>()?;
    let mut readiness = readiness_for(tx, &ids).await?;

    rows.iter()
        .map(|row| {
            let id: Uuid = row.try_get("id")?;
            version_from_row(row, take_readiness(&mut readiness, id))
        })
        .collect()
}

pub async fn find_version(
    tx: &mut ScopedTx,
    partner_id: Uuid,
    version_id: Uuid,
) -> DbResult<Option<KnowledgeVersion>> {
    let bureau_id = tx.bureau_id();
    let Some(row) = sqlx::query(&format!(
        "SELECT v.*, {VERSION_COUNTS} FROM otdel.knowledge_versions v \
          WHERE v.bureau_id = $1 AND v.partner_id = $2 AND v.id = $3"
    ))
    .bind(bureau_id)
    .bind(partner_id)
    .bind(version_id)
    .fetch_optional(tx.conn())
    .await?
    else {
        return Ok(None);
    };

    let mut readiness = readiness_for(tx, &[version_id]).await?;
    Ok(Some(version_from_row(
        &row,
        take_readiness(&mut readiness, version_id),
    )?))
}

/// The partner's current published version, if there is one.
pub async fn find_published(
    tx: &mut ScopedTx,
    partner_id: Uuid,
) -> DbResult<Option<KnowledgeVersion>> {
    let bureau_id = tx.bureau_id();
    let Some(row) = sqlx::query(&format!(
        "SELECT v.*, {VERSION_COUNTS} FROM otdel.knowledge_versions v \
          WHERE v.bureau_id = $1 AND v.partner_id = $2 AND v.status = 'published'"
    ))
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_optional(tx.conn())
    .await?
    else {
        return Ok(None);
    };
    let id: Uuid = row.try_get("id")?;
    let mut readiness = readiness_for(tx, &[id]).await?;
    Ok(Some(version_from_row(
        &row,
        take_readiness(&mut readiness, id),
    )?))
}

/// Fingerprint of what is published now, for the "do not republish the same input" rule.
pub async fn published_fingerprint(
    tx: &mut ScopedTx,
    partner_id: Uuid,
) -> DbResult<Option<String>> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(
        "SELECT input_fingerprint FROM otdel.knowledge_versions \
          WHERE bureau_id = $1 AND partner_id = $2 AND status = 'published'",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_optional(tx.conn())
    .await?;
    row.map(|row| row.try_get::<String, _>("input_fingerprint"))
        .transpose()
        .map_err(DbError::from)
}

async fn readiness_for(
    tx: &mut ScopedTx,
    version_ids: &[Uuid],
) -> DbResult<Vec<(Uuid, ReadinessEntry)>> {
    if version_ids.is_empty() {
        return Ok(Vec::new());
    }
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT version_id, topic, state, reason FROM otdel.version_readiness \
          WHERE bureau_id = $1 AND version_id = ANY($2) ORDER BY topic",
    )
    .bind(bureau_id)
    .bind(version_ids)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            let topic: String = row.try_get("topic")?;
            let state: String = row.try_get("state")?;
            Ok((
                row.try_get("version_id")?,
                ReadinessEntry {
                    topic: ReadinessTopic::parse(&topic).ok_or_else(|| {
                        DbError::Decode(format!("unknown readiness topic `{topic}`"))
                    })?,
                    state: ReadinessState::parse(&state).ok_or_else(|| {
                        DbError::Decode(format!("unknown readiness state `{state}`"))
                    })?,
                    reason: row.try_get("reason")?,
                },
            ))
        })
        .collect()
}

fn take_readiness(
    entries: &mut Vec<(Uuid, ReadinessEntry)>,
    version_id: Uuid,
) -> Vec<ReadinessEntry> {
    let mut taken = Vec::new();
    entries.retain(|(owner, entry)| {
        if *owner == version_id {
            taken.push(entry.clone());
            false
        } else {
            true
        }
    });
    taken.sort_by_key(|entry| entry.topic);
    taken
}

// --- claims ---------------------------------------------------------------------------

pub async fn list_claims(tx: &mut ScopedTx, version_id: Uuid) -> DbResult<Vec<VersionClaim>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT * FROM otdel.version_claims \
          WHERE bureau_id = $1 AND version_id = $2 \
          ORDER BY product_name NULLS LAST, attribute, created_at, id",
    )
    .bind(bureau_id)
    .bind(version_id)
    .fetch_all(tx.conn())
    .await?;

    let ids: Vec<Uuid> = rows
        .iter()
        .map(|row| row.try_get("id"))
        .collect::<Result<_, _>>()?;
    let mut evidence = evidence_for(tx, &ids).await?;

    rows.iter()
        .map(|row| {
            let id: Uuid = row.try_get("id")?;
            claim_from_row(row, take_evidence(&mut evidence, id))
        })
        .collect()
}

fn claim_from_row(row: &PgRow, evidence: Vec<VersionEvidence>) -> DbResult<VersionClaim> {
    let origin: String = row.try_get("origin")?;
    let scope: String = row.try_get("scope")?;
    let kind: String = row.try_get("kind")?;
    let status: String = row.try_get("status")?;

    Ok(VersionClaim {
        id: row.try_get("id")?,
        version_id: row.try_get("version_id")?,
        origin: ClaimOrigin::parse(&origin)
            .ok_or_else(|| DbError::Decode(format!("unknown claim origin `{origin}`")))?,
        origin_id: row.try_get("origin_id")?,
        scope: ClaimScope::parse(&scope)
            .ok_or_else(|| DbError::Decode(format!("unknown claim scope `{scope}`")))?,
        product_name: row.try_get("product_name")?,
        kind: FactKind::parse(&kind)
            .ok_or_else(|| DbError::Decode(format!("unknown fact kind `{kind}`")))?,
        status: ClaimStatus::parse(&status)
            .ok_or_else(|| DbError::Decode(format!("unknown claim status `{status}`")))?,
        attribute: row.try_get("attribute")?,
        value_text: row.try_get("value_text")?,
        unit: row.try_get("unit")?,
        conditions: row.try_get("conditions")?,
        model_context: row.try_get("model_context")?,
        check_note: row.try_get("check_note")?,
        evidence,
        created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
    })
}

async fn evidence_for(
    tx: &mut ScopedTx,
    claim_ids: &[Uuid],
) -> DbResult<Vec<(Uuid, VersionEvidence)>> {
    if claim_ids.is_empty() {
        return Ok(Vec::new());
    }
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT * FROM otdel.version_evidence \
          WHERE bureau_id = $1 AND claim_id = ANY($2) ORDER BY created_at, id",
    )
    .bind(bureau_id)
    .bind(claim_ids)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            let source_kind: String = row.try_get("source_kind")?;
            Ok((
                row.try_get("claim_id")?,
                VersionEvidence {
                    id: row.try_get("id")?,
                    claim_id: row.try_get("claim_id")?,
                    source_kind: EvidenceSourceKind::parse(&source_kind).ok_or_else(|| {
                        DbError::Decode(format!("unknown evidence source kind `{source_kind}`"))
                    })?,
                    material_id: row.try_get("material_id")?,
                    material_filename: row.try_get("material_filename")?,
                    page_number: row.try_get("page_number")?,
                    region_id: row.try_get("region_id")?,
                    url: row.try_get("url")?,
                    host: row.try_get("host")?,
                    retrieved_at: row.try_get::<Option<DateTime<Utc>>, _>("retrieved_at")?,
                    content_hash: row.try_get("content_hash")?,
                    quote: row.try_get("quote")?,
                    char_start: row.try_get("char_start")?,
                    char_end: row.try_get("char_end")?,
                },
            ))
        })
        .collect()
}

fn take_evidence(
    evidence: &mut Vec<(Uuid, VersionEvidence)>,
    claim_id: Uuid,
) -> Vec<VersionEvidence> {
    let mut taken = Vec::new();
    evidence.retain(|(owner, item)| {
        if *owner == claim_id {
            taken.push(item.clone());
            false
        } else {
            true
        }
    });
    taken
}

/// Claims of a version by id, with their evidence — used by the answer endpoint to turn
/// a retrieval result into a bounded context.
pub async fn claims_by_id(
    tx: &mut ScopedTx,
    version_id: Uuid,
    claim_ids: &[Uuid],
) -> DbResult<Vec<VersionClaim>> {
    if claim_ids.is_empty() {
        return Ok(Vec::new());
    }
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT * FROM otdel.version_claims \
          WHERE bureau_id = $1 AND version_id = $2 AND id = ANY($3)",
    )
    .bind(bureau_id)
    .bind(version_id)
    .bind(claim_ids)
    .fetch_all(tx.conn())
    .await?;

    let ids: Vec<Uuid> = rows
        .iter()
        .map(|row| row.try_get("id"))
        .collect::<Result<_, _>>()?;
    let mut evidence = evidence_for(tx, &ids).await?;

    // Preserve the order the caller asked for: it is the ranking of the search.
    let mut claims: Vec<VersionClaim> = rows
        .iter()
        .map(|row| {
            let id: Uuid = row.try_get("id")?;
            claim_from_row(row, take_evidence(&mut evidence, id))
        })
        .collect::<DbResult<_>>()?;
    claims.sort_by_key(|claim| {
        claim_ids
            .iter()
            .position(|wanted| *wanted == claim.id)
            .unwrap_or(usize::MAX)
    });
    Ok(claims)
}

// --- gaps -------------------------------------------------------------------------------

pub async fn list_version_gaps(tx: &mut ScopedTx, version_id: Uuid) -> DbResult<Vec<VersionGap>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT * FROM otdel.version_gaps \
          WHERE bureau_id = $1 AND version_id = $2 ORDER BY topic, created_at, id",
    )
    .bind(bureau_id)
    .bind(version_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            let topics: Vec<String> = row.try_get("blocks_topics")?;
            Ok(VersionGap {
                id: row.try_get("id")?,
                version_id: row.try_get("version_id")?,
                origin_id: row.try_get("origin_id")?,
                product_name: row.try_get("product_name")?,
                topic: row.try_get("topic")?,
                missing: row.try_get("missing")?,
                blocks: row.try_get("blocks")?,
                blocks_topics: topics
                    .iter()
                    .filter_map(|topic| ReadinessTopic::parse(topic))
                    .collect(),
                created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
            })
        })
        .collect()
}

// --- the run -------------------------------------------------------------------------

pub async fn find_run(tx: &mut ScopedTx, partner_id: Uuid) -> DbResult<Option<ValidationRun>> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(
        "SELECT r.*, \
                v.number AS version_number, \
                (SELECT count(*) FROM otdel.version_claims c WHERE c.version_id = r.version_id \
                   AND c.status = 'source_supported') AS claims_source_supported, \
                (SELECT count(*) FROM otdel.version_claims c WHERE c.version_id = r.version_id \
                   AND c.status = 'hypothesis') AS claims_hypothesis, \
                (SELECT count(*) FROM otdel.version_claims c WHERE c.version_id = r.version_id \
                   AND c.status = 'unknown') AS claims_unknown, \
                (SELECT count(*) FROM otdel.version_claims c WHERE c.version_id = r.version_id \
                   AND c.status = 'conflicted') AS claims_conflicted, \
                (SELECT count(*) FROM otdel.version_claims c WHERE c.version_id = r.version_id \
                   AND c.status = 'stale') AS claims_stale \
           FROM otdel.validation_runs r \
           LEFT JOIN otdel.knowledge_versions v \
                  ON v.bureau_id = r.bureau_id AND v.id = r.version_id \
          WHERE r.bureau_id = $1 AND r.partner_id = $2",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_optional(tx.conn())
    .await?;

    let Some(row) = row else { return Ok(None) };
    let status: String = row.try_get("status")?;

    Ok(Some(ValidationRun {
        id: row.try_get("id")?,
        partner_id: row.try_get("partner_id")?,
        status: ValidationRunStatus::parse(&status)
            .ok_or_else(|| DbError::Decode(format!("unknown validation status `{status}`")))?,
        prompt_profile: row.try_get("prompt_profile")?,
        version_id: row.try_get("version_id")?,
        version_number: row.try_get("version_number")?,
        claims_considered: row.try_get("claims_considered")?,
        claims_source_supported: count(&row, "claims_source_supported")?,
        claims_hypothesis: count(&row, "claims_hypothesis")?,
        claims_unknown: count(&row, "claims_unknown")?,
        claims_conflicted: count(&row, "claims_conflicted")?,
        claims_stale: count(&row, "claims_stale")?,
        claims_rejected: row.try_get("claims_rejected")?,
        gaps_carried: row.try_get("gaps_carried")?,
        chunks_created: row.try_get("chunks_created")?,
        chunks_embedded: row.try_get("chunks_embedded")?,
        model_reviewed: row.try_get("model_reviewed")?,
        published: row.try_get("published")?,
        rejections: row.try_get("rejections")?,
        blocked_reasons: row.try_get("blocked_reasons")?,
        diagnostic: row.try_get("diagnostic")?,
        started_at: row.try_get::<Option<DateTime<Utc>>, _>("started_at")?,
        finished_at: row.try_get::<Option<DateTime<Utc>>, _>("finished_at")?,
        created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
    }))
}

// --- what the checker reads -------------------------------------------------------------

/// How many candidates this partner has, so the API can refuse a check with nothing in it.
pub async fn candidate_summary(tx: &mut ScopedTx, partner_id: Uuid) -> DbResult<CandidateSummary> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(
        "SELECT \
            (SELECT count(*) FROM otdel.knowledge_facts f \
              WHERE f.bureau_id = $1 AND f.partner_id = $2) AS facts, \
            (SELECT count(*) FROM otdel.research_findings r \
              WHERE r.bureau_id = $1 AND r.partner_id = $2) AS findings, \
            (SELECT count(*) FROM otdel.knowledge_gaps g \
              WHERE g.bureau_id = $1 AND g.partner_id = $2 AND g.status = 'open') AS gaps_open, \
            (SELECT count(DISTINCT f.material_id) FROM otdel.knowledge_facts f \
              WHERE f.bureau_id = $1 AND f.partner_id = $2) AS materials_drafted",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_one(tx.conn())
    .await?;

    Ok(CandidateSummary {
        facts: count(&row, "facts")?,
        findings: count(&row, "findings")?,
        gaps_open: count(&row, "gaps_open")?,
        materials_drafted: count(&row, "materials_drafted")?,
    })
}

/// Every candidate of one partner, each citation carrying the source text it points into
/// **as it is stored now**.
///
/// Two queries, one per origin, because the two kinds of citation index different tables:
/// a partner fact points into `material_pages.text_content`, an industry conclusion into
/// `research_sources.text_content`. Both are `LEFT JOIN`ed, so a source row that is gone
/// yields `NULL` and becomes "cannot be checked" rather than "no longer says this".
pub async fn load_candidates(tx: &mut ScopedTx, partner_id: Uuid) -> DbResult<Vec<CandidateClaim>> {
    let bureau_id = tx.bureau_id();
    let mut claims: Vec<CandidateClaim> = Vec::new();

    // --- partner facts -------------------------------------------------------------
    let rows = sqlx::query(
        "SELECT f.id, f.kind, f.attribute, f.value_text, f.unit, f.conditions, \
                f.model_context, p.name AS product_name, \
                e.id AS evidence_id, e.material_id, m.filename, e.page_number, e.region_id, \
                e.quote, e.char_start, e.char_end, mp.text_content \
           FROM otdel.knowledge_facts f \
           LEFT JOIN otdel.products p ON p.bureau_id = f.bureau_id AND p.id = f.product_id \
           JOIN otdel.knowledge_evidence e \
                ON e.bureau_id = f.bureau_id AND e.fact_id = f.id \
           LEFT JOIN otdel.materials m ON m.bureau_id = e.bureau_id AND m.id = e.material_id \
           LEFT JOIN otdel.material_pages mp \
                ON mp.bureau_id = e.bureau_id AND mp.id = e.page_id \
          WHERE f.bureau_id = $1 AND f.partner_id = $2 \
          ORDER BY f.created_at, f.id, e.created_at, e.id",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_all(tx.conn())
    .await?;

    for row in &rows {
        let fact_id: Uuid = row.try_get("id")?;
        let evidence = CandidateEvidence {
            source_kind: EvidenceSourceKind::Material,
            material_id: row.try_get("material_id")?,
            material_filename: row
                .try_get::<Option<String>, _>("filename")?
                .or_else(|| Some("документ".to_owned())),
            page_number: row.try_get("page_number")?,
            region_id: row.try_get("region_id")?,
            url: None,
            host: None,
            retrieved_at: None,
            content_hash: None,
            quote: row.try_get("quote")?,
            char_start: row.try_get("char_start")?,
            char_end: row.try_get("char_end")?,
            source_text: row.try_get("text_content")?,
        };

        match claims.iter_mut().find(|claim| claim.origin_id == fact_id) {
            Some(claim) => claim.evidence.push(evidence),
            None => {
                let kind: String = row.try_get("kind")?;
                claims.push(CandidateClaim {
                    origin: ClaimOrigin::PartnerMaterial,
                    origin_id: fact_id,
                    product_name: row.try_get("product_name")?,
                    kind: FactKind::parse(&kind)
                        .ok_or_else(|| DbError::Decode(format!("unknown fact kind `{kind}`")))?,
                    attribute: row.try_get("attribute")?,
                    value_text: row.try_get("value_text")?,
                    unit: row.try_get("unit")?,
                    conditions: row.try_get("conditions")?,
                    model_context: row.try_get("model_context")?,
                    evidence: vec![evidence],
                });
            }
        }
    }

    // --- industry conclusions --------------------------------------------------------
    let rows = sqlx::query(
        "SELECT f.id, f.attribute, f.value_text, f.unit, f.conditions, f.model_context, \
                e.id AS evidence_id, e.quote, e.char_start, e.char_end, \
                s.url, s.host, s.retrieved_at, s.content_hash, s.text_content \
           FROM otdel.research_findings f \
           JOIN otdel.research_evidence e \
                ON e.bureau_id = f.bureau_id AND e.finding_id = f.id \
           LEFT JOIN otdel.research_sources s \
                ON s.bureau_id = e.bureau_id AND s.id = e.source_id \
          WHERE f.bureau_id = $1 AND f.partner_id = $2 \
          ORDER BY f.created_at, f.id, e.created_at, e.id",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_all(tx.conn())
    .await?;

    for row in &rows {
        let finding_id: Uuid = row.try_get("id")?;
        let evidence = CandidateEvidence {
            source_kind: EvidenceSourceKind::External,
            material_id: None,
            material_filename: None,
            page_number: None,
            region_id: None,
            url: row.try_get("url")?,
            host: row.try_get("host")?,
            retrieved_at: row.try_get::<Option<DateTime<Utc>>, _>("retrieved_at")?,
            content_hash: row.try_get("content_hash")?,
            quote: row.try_get("quote")?,
            char_start: row.try_get("char_start")?,
            char_end: row.try_get("char_end")?,
            source_text: row.try_get("text_content")?,
        };

        match claims
            .iter_mut()
            .find(|claim| claim.origin_id == finding_id)
        {
            Some(claim) => claim.evidence.push(evidence),
            None => claims.push(CandidateClaim {
                origin: ClaimOrigin::IndustryResearch,
                origin_id: finding_id,
                // Structurally impossible to be anything else: `research_findings` has
                // no product column at all (`0005_research.sql`).
                product_name: None,
                // An industry conclusion states a property and its value, which is what
                // `characteristic` means. The thing that keeps it out of the partner's
                // own answers is its **scope**, not its kind.
                kind: FactKind::Characteristic,
                attribute: row.try_get("attribute")?,
                value_text: row.try_get("value_text")?,
                unit: row.try_get("unit")?,
                conditions: row.try_get("conditions")?,
                model_context: row.try_get("model_context")?,
                evidence: vec![evidence],
            }),
        }
    }

    Ok(claims)
}

/// Open gaps of a partner, as the readiness rules and the snapshot need them.
pub async fn load_gaps(
    tx: &mut ScopedTx,
    partner_id: Uuid,
) -> DbResult<Vec<(Uuid, Option<String>, GapText)>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT g.id, g.topic, g.missing, g.blocks, p.name AS product_name \
           FROM otdel.knowledge_gaps g \
           LEFT JOIN otdel.products p ON p.bureau_id = g.bureau_id AND p.id = g.product_id \
          WHERE g.bureau_id = $1 AND g.partner_id = $2 AND g.status = 'open' \
          ORDER BY g.created_at, g.id",
    )
    .bind(bureau_id)
    .bind(partner_id)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            Ok((
                row.try_get("id")?,
                row.try_get("product_name")?,
                GapText {
                    topic: row.try_get("topic")?,
                    missing: row.try_get("missing")?,
                    blocks: row.try_get("blocks")?,
                },
            ))
        })
        .collect()
}

/// Chunks of a version that still have no vector of the wanted profile.
pub async fn chunks_to_embed(
    tx: &mut ScopedTx,
    version_id: Uuid,
    profile: &str,
) -> DbResult<Vec<(Uuid, String)>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT id, chunk_text FROM otdel.version_chunks \
          WHERE bureau_id = $1 AND version_id = $2 \
            AND (embedding_profile IS NULL OR embedding_profile <> $3) \
          ORDER BY created_at, id",
    )
    .bind(bureau_id)
    .bind(version_id)
    .bind(profile)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| Ok((row.try_get("id")?, row.try_get("chunk_text")?)))
        .collect()
}

/// Can **this connection** actually store and compare vectors, and under what type name?
///
/// Three things have to be true at once, and asking for all three together is the point:
///
///  1. pgvector is installed. `0006_publication.sql` adds the `embedding` column only
///     then, because the restricted migration role may not create a non-trusted
///     extension;
///  2. the column is really there;
///  3. **the runtime role can reach the schema the extension lives in.** This is the one
///     that is easy to miss and was missed once here: `0001_schema.sql` revokes
///     everything on `public` and grants `USAGE` only to the migration role, so a
///     `vector` type created in `public` is invisible to `otdel_app` — the column exists,
///     and every statement naming its type fails with `type "vector" does not exist`.
///     Installing the extension into `otdel` is what the provisioning script does; this
///     check is what makes either layout work, and makes an unusable one a *reported*
///     state rather than a runtime error.
///
/// Returns the **schema** pgvector lives in, or `None` when semantic search is genuinely
/// unavailable. The caller qualifies both the type (`{schema}.vector`) and the distance
/// operator (`OPERATOR({schema}.<=>)`) with it — the runtime connection pins
/// `search_path` to `public`, so neither resolves unqualified. The name comes from
/// `pg_namespace`, never from user input.
pub async fn vector_schema(tx: &mut ScopedTx) -> DbResult<Option<String>> {
    let row = sqlx::query(
        "SELECT n.nspname AS schema_name \
           FROM pg_extension e \
           JOIN pg_namespace n ON n.oid = e.extnamespace \
          WHERE e.extname = 'vector' \
            AND has_schema_privilege(n.nspname, 'USAGE') \
            AND EXISTS ( \
                SELECT 1 FROM information_schema.columns \
                 WHERE table_schema = 'otdel' AND table_name = 'version_chunks' \
                   AND column_name = 'embedding' \
            )",
    )
    .fetch_optional(tx.conn())
    .await?;

    let Some(row) = row else { return Ok(None) };
    let schema: String = row.try_get("schema_name")?;
    // Defence in depth: a schema name is an identifier from the catalogue, but nothing
    // outside this shape is ever interpolated into SQL.
    if !schema
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return Ok(None);
    }
    Ok(Some(schema))
}

/// Convenience for the callers that only need a yes or no.
pub async fn vector_column_exists(tx: &mut ScopedTx) -> DbResult<bool> {
    Ok(vector_schema(tx).await?.is_some())
}
