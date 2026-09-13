//! Hybrid search over one pinned version.
//!
//! `block-01-spec.md` §9: exact articles and parameters are looked up structurally,
//! terms by full text, meaning by vectors, and the three are merged. Two properties of
//! this module are not negotiable and are enforced by its shape rather than by care:
//!
//! **One version, always.** Every query takes a `version_id` and filters on it. There is
//! no function here that searches "a partner" — the caller resolves the pinned or
//! published version first, and a result set therefore cannot contain two versions
//! ("в один ответ не смешиваются разные версии").
//!
//! **Profiles never meet.** The vector half filters by `embedding_profile` before it
//! computes a distance, so a version embedded by one model is never compared with a query
//! embedded by another. When the version has no vectors of the current profile, the
//! vector half simply returns nothing and the caller reports `keyword` mode with the
//! reason — which is the honest degradation the phase promises.
//!
//! The three halves run as three statements and are merged in Rust. One clever SQL
//! statement would be faster and would make "why did this rank first" unanswerable; at
//! the corpus size this phase is specified for (`block-01-spec.md` §9 allows exact vector
//! search precisely because the corpus is small) the trade is not worth making.

use sqlx::Row;
use uuid::Uuid;

use crate::error::DbResult;
use crate::tenancy::ScopedTx;

/// Weight of each half in the combined score.
///
/// An exact hit on an article number or a parameter name outranks everything, because
/// that is the query where the user knows exactly what they are asking for. The other two
/// contribute on top, so a claim found by two halves ranks above one found by either.
const EXACT_WEIGHT: f32 = 1.0;
const KEYWORD_WEIGHT: f32 = 0.6;
const VECTOR_WEIGHT: f32 = 0.6;

/// One claim matched by at least one half.
#[derive(Debug, Clone, PartialEq)]
pub struct RetrievalHit {
    pub claim_id: Uuid,
    pub chunk_id: Uuid,
    pub score: f32,
    pub exact: bool,
    pub keyword: bool,
    pub vector: bool,
}

/// Merge the three halves into one ranking.
///
/// The score is only comparable **inside one response** — it is a sum of weights, not a
/// probability and not a confidence. The wire contract says so, and the interface repeats
/// it.
fn merge(mut hits: Vec<RetrievalHit>) -> Vec<RetrievalHit> {
    let mut merged: Vec<RetrievalHit> = Vec::new();
    for hit in hits.drain(..) {
        match merged.iter_mut().find(|kept| kept.claim_id == hit.claim_id) {
            Some(kept) => {
                kept.score += hit.score;
                kept.exact |= hit.exact;
                kept.keyword |= hit.keyword;
                kept.vector |= hit.vector;
            }
            None => merged.push(hit),
        }
    }
    merged.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            // A stable tiebreak, so two runs of the same query return the same order.
            .then_with(|| a.claim_id.cmp(&b.claim_id))
    });
    merged
}

/// The exact half: the query names a value, a property or a product as written.
///
/// The query is split into folded tokens and each is compared for equality against the
/// folded columns. This is what answers «BP21» and «нагрузка» — a stemmer would turn the
/// first into something else entirely, and an article number is not a word to be stemmed.
pub async fn search_exact(
    tx: &mut ScopedTx,
    version_id: Uuid,
    tokens: &[String],
    product_filter: Option<&str>,
    limit: i64,
) -> DbResult<Vec<RetrievalHit>> {
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT id AS chunk_id, claim_id FROM otdel.version_chunks \
          WHERE bureau_id = $1 AND version_id = $2 \
            AND ($4::text IS NULL OR normalised_product = $4) \
            AND ( normalised_value = ANY($3) \
               OR normalised_attribute = ANY($3) \
               OR normalised_product = ANY($3) ) \
          ORDER BY created_at, id LIMIT $5",
    )
    .bind(bureau_id)
    .bind(version_id)
    .bind(tokens)
    .bind(product_filter)
    .bind(limit)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            Ok(RetrievalHit {
                claim_id: row.try_get("claim_id")?,
                chunk_id: row.try_get("chunk_id")?,
                score: EXACT_WEIGHT,
                exact: true,
                keyword: false,
                vector: false,
            })
        })
        .collect()
}

/// The full-text half, over the claim and the words of its citations.
pub async fn search_keyword(
    tx: &mut ScopedTx,
    version_id: Uuid,
    query: &str,
    product_filter: Option<&str>,
    limit: i64,
) -> DbResult<Vec<RetrievalHit>> {
    let bureau_id = tx.bureau_id();
    let rows = sqlx::query(
        "SELECT id AS chunk_id, claim_id, \
                ts_rank(search_vector, plainto_tsquery('russian', $3)) AS rank \
           FROM otdel.version_chunks \
          WHERE bureau_id = $1 AND version_id = $2 \
            AND ($4::text IS NULL OR normalised_product = $4) \
            AND search_vector @@ plainto_tsquery('russian', $3) \
          ORDER BY rank DESC, created_at, id LIMIT $5",
    )
    .bind(bureau_id)
    .bind(version_id)
    .bind(query)
    .bind(product_filter)
    .bind(limit)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            let rank: f32 = row.try_get("rank")?;
            Ok(RetrievalHit {
                claim_id: row.try_get("claim_id")?,
                chunk_id: row.try_get("chunk_id")?,
                // `ts_rank` is unbounded above; folding it into 0..1 keeps the weights
                // meaningful when two halves are added together.
                score: KEYWORD_WEIGHT * (rank / (1.0 + rank)),
                exact: false,
                keyword: true,
                vector: false,
            })
        })
        .collect()
}

/// The vector half, within one embedding profile.
///
/// Exact nearest-neighbour search: there is no ANN index, on purpose. `block-01-spec.md`
/// §9 allows exact vector search on a small corpus and requires a recall comparison —
/// with partner filters applied — before ANN is adopted. Creating the index would make
/// the planner answer approximately, which is the very thing that has to be measured
/// first.
pub async fn search_vector(
    tx: &mut ScopedTx,
    version_id: Uuid,
    profile: &str,
    query_vector: &[f32],
    product_filter: Option<&str>,
    limit: i64,
) -> DbResult<Vec<RetrievalHit>> {
    if query_vector.is_empty() {
        return Ok(Vec::new());
    }
    // The type *and the operator* are schema-qualified from the catalogue. The runtime
    // connection pins `search_path` to `public` (`otdel_db::MIGRATION_SEARCH_PATH`), and
    // the extension may live in `otdel` or in `public`, so neither `vector` nor `<=>`
    // resolves on its own. No reachable extension, no vector half — reported by the
    // caller, never guessed at.
    let Some(schema) = crate::publication_read::vector_schema(tx).await? else {
        return Ok(Vec::new());
    };
    let bureau_id = tx.bureau_id();
    let literal = crate::publication::vector_literal(query_vector);

    let rows = sqlx::query(&format!(
        "SELECT id AS chunk_id, claim_id, \
                (embedding OPERATOR({schema}.<=>) $4::text::{schema}.vector) AS distance \
           FROM otdel.version_chunks \
          WHERE bureau_id = $1 AND version_id = $2 \
            AND embedding_profile = $3 \
            AND embedding IS NOT NULL \
            AND embedding_dims = $6 \
            AND ($5::text IS NULL OR normalised_product = $5) \
          ORDER BY distance ASC LIMIT $7"
    ))
    .bind(bureau_id)
    .bind(version_id)
    .bind(profile)
    .bind(&literal)
    .bind(product_filter)
    .bind(i32::try_from(query_vector.len()).unwrap_or(i32::MAX))
    .bind(limit)
    .fetch_all(tx.conn())
    .await?;

    rows.iter()
        .map(|row| {
            let distance: f64 = row.try_get("distance")?;
            // Cosine distance runs 0..2; similarity is what a score wants.
            let similarity = (1.0 - distance / 2.0).clamp(0.0, 1.0) as f32;
            Ok(RetrievalHit {
                claim_id: row.try_get("claim_id")?,
                chunk_id: row.try_get("chunk_id")?,
                score: VECTOR_WEIGHT * similarity,
                exact: false,
                keyword: false,
                vector: true,
            })
        })
        .collect()
}

/// Does this version carry vectors of this profile?
///
/// Asked before the vector half runs, so "this version has no vectors of the current
/// profile" is reported as a named reason rather than silently returning fewer results.
pub async fn has_vectors(tx: &mut ScopedTx, version_id: Uuid, profile: &str) -> DbResult<bool> {
    let bureau_id = tx.bureau_id();
    let row = sqlx::query(
        "SELECT EXISTS ( \
             SELECT 1 FROM otdel.version_chunks \
              WHERE bureau_id = $1 AND version_id = $2 AND embedding_profile = $3 \
         ) AS present",
    )
    .bind(bureau_id)
    .bind(version_id)
    .bind(profile)
    .fetch_one(tx.conn())
    .await?;
    Ok(row.try_get::<bool, _>("present")?)
}

/// Run the halves that are available and merge them.
///
/// `query_vector` is `None` whenever there is no embedding provider, no pgvector, or no
/// vectors of the current profile on this version. All three are the same thing to this
/// function — nothing to compare — and the caller turns each into its own sentence for
/// the interface.
#[allow(clippy::too_many_arguments)]
pub async fn search(
    tx: &mut ScopedTx,
    version_id: Uuid,
    query: &str,
    tokens: &[String],
    product_filter: Option<&str>,
    query_vector: Option<(&str, &[f32])>,
    limit: i64,
) -> DbResult<Vec<RetrievalHit>> {
    // Each half is given room for the full limit; merging is what decides the ranking,
    // and starving one half here would quietly make the hybrid less than its parts.
    let mut hits = search_exact(tx, version_id, tokens, product_filter, limit).await?;
    hits.extend(search_keyword(tx, version_id, query, product_filter, limit).await?);
    if let Some((profile, vector)) = query_vector {
        hits.extend(search_vector(tx, version_id, profile, vector, product_filter, limit).await?);
    }

    let mut merged = merge(hits);
    merged.truncate(usize::try_from(limit).unwrap_or(20));
    Ok(merged)
}

/// Fold a query into the tokens the exact half compares.
pub fn query_tokens(query: &str, max_tokens: usize) -> Vec<String> {
    let mut tokens: Vec<String> = otdel_publish::chunk::normalise(query)
        .split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ';' | '?' | '!' | '(' | ')'))
        .filter(|token| !token.is_empty())
        .map(|token| {
            token
                .trim_matches(|ch: char| matches!(ch, '.' | ':'))
                .to_owned()
        })
        .filter(|token| !token.is_empty())
        .collect();
    tokens.sort();
    tokens.dedup();
    tokens.truncate(max_tokens);
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(id: u128, score: f32, exact: bool, keyword: bool, vector: bool) -> RetrievalHit {
        RetrievalHit {
            claim_id: Uuid::from_u128(id),
            chunk_id: Uuid::from_u128(id + 100),
            score,
            exact,
            keyword,
            vector,
        }
    }

    #[test]
    fn a_claim_found_by_two_halves_outranks_one_found_by_either() {
        let merged = merge(vec![
            hit(1, 0.5, false, true, false),
            hit(2, 0.6, false, false, true),
            hit(1, 0.6, false, false, true),
        ]);
        assert_eq!(merged[0].claim_id, Uuid::from_u128(1));
        assert!(merged[0].keyword && merged[0].vector);
        assert!((merged[0].score - 1.1).abs() < 1e-6);
    }

    #[test]
    fn merging_records_every_half_that_matched() {
        let merged = merge(vec![
            hit(1, 1.0, true, false, false),
            hit(1, 0.3, false, true, false),
        ]);
        assert_eq!(merged.len(), 1);
        assert!(merged[0].exact && merged[0].keyword && !merged[0].vector);
    }

    #[test]
    fn equal_scores_break_ties_stably_so_one_query_ranks_the_same_twice() {
        let a = merge(vec![
            hit(2, 0.5, true, false, false),
            hit(1, 0.5, true, false, false),
        ]);
        let b = merge(vec![
            hit(1, 0.5, true, false, false),
            hit(2, 0.5, true, false, false),
        ]);
        assert_eq!(
            a.iter().map(|h| h.claim_id).collect::<Vec<_>>(),
            b.iter().map(|h| h.claim_id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn query_tokens_fold_case_and_punctuation_so_an_article_is_found_as_written() {
        assert_eq!(
            query_tokens("Какая нагрузка у BP21?", 20),
            vec![
                "bp21".to_owned(),
                "какая".to_owned(),
                "нагрузка".to_owned(),
                "у".to_owned()
            ]
        );
    }

    #[test]
    fn query_tokens_are_bounded_so_a_long_query_cannot_become_a_long_array() {
        let tokens = query_tokens(&(0..500).map(|n| format!("w{n} ")).collect::<String>(), 16);
        assert_eq!(tokens.len(), 16);
    }

    #[test]
    fn a_query_of_only_punctuation_produces_no_tokens_rather_than_an_empty_one() {
        assert!(query_tokens("?? !! ,,", 20).is_empty());
        assert!(query_tokens("   ", 20).is_empty());
    }
}
