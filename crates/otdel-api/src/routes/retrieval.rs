//! Phase 1E — search over a published version, and answering from it.
//!
//! `block-01-spec.md` §9. Three properties are enforced here rather than hoped for:
//!
//! **Only a published version is readable.** [`resolve_version`] is the single place a
//! version is chosen, and it accepts `published` and `superseded` only. A draft was never
//! published, a `blocked` one failed the rules, and a `revoked` one was withdrawn — none
//! of them can be pinned into an answer. A partner with nothing published gets the named
//! state `no_published_version`, not an empty result that looks like "nothing matches".
//!
//! **An answer cites the version or admits it cannot.** The model is shown claims the
//! server selected, labelled `C1…Cn`, and the citations the caller receives are this
//! server's own evidence rows for the claims the model cited. A label that does not
//! resolve is dropped; an answer with no surviving citation is not returned as an answer.
//! There is no path by which a model's output becomes a citation.
//!
//! **Degradation is reported, never hidden.** Without an embedding provider, without
//! pgvector, or on a version with no vectors of the current profile, the vector half
//! simply does not run and `mode` is `keyword` with the reason in `degraded[]`.
//!
//! Two calls leave this machine from a request path, and both are deliberate exceptions
//! with the same shape: embedding the query, and composing prose. Each is bounded by its
//! own timeout, each happens only when its adapter is configured, and each **degrades**
//! on failure — to keyword search, and to `evidence_only` — instead of failing the
//! request. A search that returned 503 because somebody else's service was slow would be
//! worse than a search that says which half it used.

use std::collections::HashMap;

use axum::extract::State;
use otdel_core::error::{AppError, ErrorCode};
use otdel_core::publication::{
    AnswerState, MatchKind, ReadinessEntry, SearchMode, SearchState, VersionClaim, VersionEvidence,
    VersionGap, VersionStatus,
};
use otdel_db::{partners, publication_read, retrieval};
use otdel_embed::EmbedRequest;
use otdel_publish::{answer, claim::CheckedClaim};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use uuid::Uuid;

use crate::auth::Session;
use crate::error::{ApiError, ApiResult};
use crate::extract::{ApiJson, ApiPath};
use crate::state::AppState;

// The adapter description lives next door; it answers "is this configured", not "what
// does the published version say".
pub use super::retrieval_state::{describe_retrieval, provider, RetrievalProviderResponse};

/// Most tokens the exact half of the search compares. A question is a question, not a
/// vocabulary.
const MAX_QUERY_TOKENS: usize = 16;

// --- search and answer ------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default)]
    pub version_id: Option<Uuid>,
    #[serde(default)]
    pub product: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnswerRequest {
    pub question: String,
    #[serde(default)]
    pub version_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct VersionRef {
    pub id: Uuid,
    pub number: i32,
    pub status: String,
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Serialize)]
pub struct SearchHit {
    pub claim: VersionClaim,
    pub score: f32,
    pub matched_by: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct SearchResponseBody {
    pub state: String,
    pub version: Option<VersionRef>,
    pub mode: String,
    pub degraded: Vec<String>,
    pub items: Vec<SearchHit>,
    pub gaps: Vec<VersionGap>,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct AnswerResponseBody {
    pub state: String,
    pub version: Option<VersionRef>,
    pub mode: String,
    pub degraded: Vec<String>,
    pub text: Option<String>,
    /// `true` whenever `text` is present: prose is always the model's wording, never a
    /// quotation, and the interface must label it.
    pub answer_is_model_context: bool,
    pub claims: Vec<VersionClaim>,
    pub citations: Vec<VersionEvidence>,
    pub conditions: Vec<String>,
    pub gaps: Vec<VersionGap>,
    pub readiness: Vec<ReadinessEntry>,
    pub limitations: Vec<String>,
    pub rejections: Vec<String>,
    pub message: String,
}

/// `POST /api/partners/{id}/retrieval/search`
pub async fn search(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
    ApiJson(payload): ApiJson<SearchRequest>,
) -> ApiResult<axum::Json<SearchResponseBody>> {
    let limits = state.config.retrieval.limits;
    let query = check_query(&payload.query, limits.max_query_chars)?;
    let limit = i64::from(
        payload
            .limit
            .unwrap_or(limits.max_results)
            .min(limits.max_results),
    )
    .max(1);

    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }
    let Some(version) = resolve_version(&mut tx, partner_id, payload.version_id).await? else {
        tx.commit().await?;
        return Ok(axum::Json(SearchResponseBody {
            state: SearchState::NoPublishedVersion.as_str().to_owned(),
            version: None,
            mode: SearchMode::Keyword.as_str().to_owned(),
            degraded: Vec::new(),
            items: Vec::new(),
            gaps: Vec::new(),
            message: NO_PUBLISHED_MESSAGE.to_owned(),
        }));
    };

    let product = payload
        .product
        .as_deref()
        .and_then(otdel_publish::chunk::normalised_column);
    let tokens = retrieval::query_tokens(&query, MAX_QUERY_TOKENS);

    let (profile, mut degraded) = vector_readiness(&state, &mut tx, version.id).await;
    // The first transaction ends here, before any external call. See `embed_query`.
    tx.commit().await?;

    let vector = match &profile {
        Some(profile) => {
            let (vector, reasons) = embed_query(&state, profile, &query).await;
            degraded.extend(reasons);
            vector
        }
        None => None,
    };
    let mode = if vector.is_some() {
        SearchMode::Hybrid
    } else {
        SearchMode::Keyword
    };

    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    let hits = retrieval::search(
        &mut tx,
        version.id,
        &query,
        &tokens,
        product.as_deref(),
        vector
            .as_ref()
            .map(|(profile, v)| (profile.as_str(), v.as_slice())),
        limit,
    )
    .await?;

    let claim_ids: Vec<Uuid> = hits.iter().map(|hit| hit.claim_id).collect();
    let claims = publication_read::claims_by_id(&mut tx, version.id, &claim_ids).await?;
    let gaps = if hits.is_empty() {
        publication_read::list_version_gaps(&mut tx, version.id).await?
    } else {
        Vec::new()
    };
    tx.commit().await?;

    let by_id: HashMap<Uuid, VersionClaim> =
        claims.into_iter().map(|claim| (claim.id, claim)).collect();
    let items: Vec<SearchHit> = hits
        .iter()
        .filter_map(|hit| {
            let claim = by_id.get(&hit.claim_id)?.clone();
            let mut matched_by = Vec::new();
            if hit.exact {
                matched_by.push(MatchKind::Exact.as_str().to_owned());
            }
            if hit.keyword {
                matched_by.push(MatchKind::Keyword.as_str().to_owned());
            }
            if hit.vector {
                matched_by.push(MatchKind::Vector.as_str().to_owned());
            }
            Some(SearchHit {
                claim,
                score: hit.score,
                matched_by,
            })
        })
        .collect();

    let (state_label, message) = if items.is_empty() {
        (
            SearchState::InsufficientEvidence,
            "В опубликованной версии нет утверждений, подходящих под запрос. Это не ошибка \
             и не пустой ответ модели: искали по точным значениям и тексту, и совпадений \
             нет."
                .to_owned(),
        )
    } else {
        (
            SearchState::Ok,
            format!(
                "Найдено {} утверждение(й) в версии №{}.",
                items.len(),
                version.number
            ),
        )
    };

    Ok(axum::Json(SearchResponseBody {
        state: state_label.as_str().to_owned(),
        version: Some(version_ref(&version)),
        mode: mode.as_str().to_owned(),
        degraded,
        items,
        gaps,
        message,
    }))
}

/// `POST /api/partners/{id}/retrieval/answer`
///
/// The whole of `block-01-spec.md` §9 in one handler: retrieve from the pinned version,
/// put only answerable claims in front of the model, and return the *server's* citations
/// for whatever it cited — or say honestly that there is no answer.
pub async fn ask(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
    ApiJson(payload): ApiJson<AnswerRequest>,
) -> ApiResult<axum::Json<AnswerResponseBody>> {
    let limits = state.config.retrieval.limits;
    let question = check_query(&payload.question, limits.max_query_chars)?;

    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }
    let Some(version) = resolve_version(&mut tx, partner_id, payload.version_id).await? else {
        tx.commit().await?;
        return Ok(axum::Json(empty_answer(
            AnswerState::NoPublishedVersion,
            NO_PUBLISHED_MESSAGE,
        )));
    };

    let tokens = retrieval::query_tokens(&question, MAX_QUERY_TOKENS);
    let (profile, mut degraded) = vector_readiness(&state, &mut tx, version.id).await;
    // Committed before the embedding call, and again before composing an answer: neither
    // external call may be made while a database transaction is open.
    tx.commit().await?;

    let vector = match &profile {
        Some(profile) => {
            let (vector, reasons) = embed_query(&state, profile, &question).await;
            degraded.extend(reasons);
            vector
        }
        None => None,
    };
    let mode = if vector.is_some() {
        SearchMode::Hybrid
    } else {
        SearchMode::Keyword
    };

    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    let hits = retrieval::search(
        &mut tx,
        version.id,
        &question,
        &tokens,
        None,
        vector
            .as_ref()
            .map(|(profile, v)| (profile.as_str(), v.as_slice())),
        i64::from(limits.max_results),
    )
    .await?;
    let claim_ids: Vec<Uuid> = hits.iter().map(|hit| hit.claim_id).collect();
    let matched = publication_read::claims_by_id(&mut tx, version.id, &claim_ids).await?;
    let gaps = publication_read::list_version_gaps(&mut tx, version.id).await?;
    tx.commit().await?;

    // Only a claim the checker found supported may carry an answer. The others are shown
    // to the owner with their verdict elsewhere; here they become reservations.
    let checked: Vec<CheckedClaim> = matched.iter().map(to_checked).collect();
    let limitations = answer::limitations(&checked);
    let answerable = answer::answerable(&checked);

    if answerable.is_empty() {
        return Ok(axum::Json(AnswerResponseBody {
            state: AnswerState::InsufficientEvidence.as_str().to_owned(),
            version: Some(version_ref(&version)),
            mode: mode.as_str().to_owned(),
            degraded,
            text: None,
            answer_is_model_context: false,
            claims: Vec::new(),
            citations: Vec::new(),
            conditions: Vec::new(),
            gaps,
            readiness: version.readiness.clone(),
            limitations,
            rejections: Vec::new(),
            message: "В опубликованной версии нет подтверждённого источником утверждения, \
                      отвечающего на этот вопрос. Догадка не подставляется; если пробел \
                      записан, он показан ниже."
                .to_owned(),
        }));
    }

    // The claims the model will be shown, in ranking order, bounded by configuration.
    let shown_count = usize::try_from(limits.max_answer_claims).unwrap_or(8);
    let shown_claims: Vec<VersionClaim> = matched
        .iter()
        .filter(|claim| claim.status.is_answerable())
        .take(shown_count)
        .cloned()
        .collect();
    let shown_checked: Vec<CheckedClaim> = shown_claims.iter().map(to_checked).collect();

    let mut rejections: Vec<String> = Vec::new();
    let mut text: Option<String> = None;
    let mut cited_indices: Vec<usize> = (0..shown_claims.len()).collect();
    let mut state_label = AnswerState::EvidenceOnly;

    if state.llm.describe().is_ready() {
        match otdel_publish::compose_answer(&state.llm, &question, &shown_checked, &limits).await {
            Ok(validated) => {
                rejections.extend(validated.rejections.clone());
                if let Some(note) = validated.note.clone() {
                    rejections.push(format!("модель сообщила: {note}"));
                }
                if validated.state == AnswerState::Answered {
                    state_label = AnswerState::Answered;
                    text = validated.text.clone();
                    cited_indices = validated.cited.clone();
                } else {
                    cited_indices = if validated.cited.is_empty() {
                        (0..shown_claims.len()).collect()
                    } else {
                        validated.cited.clone()
                    };
                }
            }
            Err(diagnostic) => {
                // Degrades rather than fails: the claims and their citations are real and
                // useful without a sentence wrapped around them.
                warn!(diagnostic = %diagnostic, "answer composition failed; returning evidence only");
                rejections.push(format!(
                    "ответ в свободной форме не получен: {diagnostic}. Ниже — найденные \
                     утверждения с цитатами"
                ));
            }
        }
    } else {
        rejections.push(
            "модель для ответов не настроена: ниже найденные утверждения с цитатами, без \
             формулировки в свободной форме"
                .to_owned(),
        );
    }

    // The citations are **this server's** rows for the claims that were cited. Nothing a
    // model wrote becomes a citation.
    let cited: Vec<VersionClaim> = cited_indices
        .iter()
        .filter_map(|index| shown_claims.get(*index).cloned())
        .collect();
    let citations: Vec<VersionEvidence> = cited
        .iter()
        .flat_map(|claim| claim.evidence.iter().cloned())
        .collect();
    let conditions: Vec<String> = cited
        .iter()
        .filter_map(|claim| claim.conditions.clone())
        .collect();

    // The invariant the contract states, enforced rather than assumed: `answered`
    // requires citations. If the two ever disagreed, the honest answer is the lower one.
    if state_label == AnswerState::Answered && citations.is_empty() {
        state_label = AnswerState::EvidenceOnly;
        text = None;
        rejections.push(
            "ответ не сопровождается ни одной цитатой и поэтому не выдан как ответ".to_owned(),
        );
    }

    let message = match state_label {
        AnswerState::Answered => "Ответ составлен моделью по показанным утверждениям. Это \
                                  формулировка модели, а не цитата; цитаты приложены."
            .to_owned(),
        _ => "Ответ в свободной форме не составлен. Найденные подтверждённые утверждения и \
              их цитаты показаны как есть."
            .to_owned(),
    };

    info!(
        partner_id = %partner_id,
        version_id = %version.id,
        state = state_label.as_str(),
        citations = citations.len(),
        "question answered from the published version"
    );

    Ok(axum::Json(AnswerResponseBody {
        state: state_label.as_str().to_owned(),
        version: Some(version_ref(&version)),
        mode: mode.as_str().to_owned(),
        degraded,
        answer_is_model_context: text.is_some(),
        text,
        claims: cited,
        citations,
        conditions,
        gaps,
        readiness: version.readiness.clone(),
        limitations,
        rejections,
        message,
    }))
}

// --- helpers ------------------------------------------------------------------------------

const NO_PUBLISHED_MESSAGE: &str =
    "У партнёра нет опубликованной версии знаний. Это состояние, а не ошибка: проверка \
     ещё не проходила, не прошла правила готовности, либо версия была отозвана. Черновики \
     1C и 1D через поиск не видны.";

/// Resolve the version a request reads, and refuse everything that was never published.
async fn resolve_version(
    tx: &mut otdel_db::ScopedTx,
    partner_id: Uuid,
    pinned: Option<Uuid>,
) -> ApiResult<Option<otdel_core::publication::KnowledgeVersion>> {
    let Some(pinned) = pinned else {
        return Ok(publication_read::find_published(tx, partner_id).await?);
    };

    let version = publication_read::find_version(tx, partner_id, pinned)
        .await?
        .ok_or_else(|| ApiError::not_found("knowledge version not found for this partner"))?;

    match version.status {
        // Pinning a version that has since been replaced is the point of pinning: it was
        // published, its snapshot is immutable, and it still says what it said.
        VersionStatus::Published | VersionStatus::Superseded => Ok(Some(version)),
        VersionStatus::Revoked => Err(ApiError::new(AppError::new(
            ErrorCode::Conflict,
            "эта версия отозвана и больше не отвечает на вопросы. Причина отзыва указана в \
             карточке версии",
        ))),
        // Never published: for a reader it does not exist.
        VersionStatus::Draft | VersionStatus::Validating | VersionStatus::Blocked => Err(
            ApiError::not_found("knowledge version is not published and cannot be searched"),
        ),
    }
}

/// Phase one: can the vector half run at all, and in which profile?
///
/// Everything here is a database question, so it happens inside the caller's
/// transaction. Every branch that says "no" also says why, so the interface never has to
/// guess why a search was keyword-only.
async fn vector_readiness(
    state: &AppState,
    tx: &mut otdel_db::ScopedTx,
    version_id: Uuid,
) -> (Option<String>, Vec<String>) {
    let description = state.embeddings.describe();
    let Some(profile) = description.profile.clone() else {
        return (
            None,
            vec![
                "векторная половина поиска не работает: провайдер embeddings не настроен.                  Псевдовекторы не создаются"
                    .to_owned(),
            ],
        );
    };

    match publication_read::vector_column_exists(tx).await {
        Ok(true) => {}
        Ok(false) => {
            return (
                None,
                vec![
                    "векторная половина поиска не работает: расширение pgvector не                      установлено в этой базе"
                        .to_owned(),
                ],
            )
        }
        Err(error) => {
            warn!(error = %error, "could not probe for pgvector");
            return (None, vec!["векторная половина поиска недоступна".to_owned()]);
        }
    }

    match retrieval::has_vectors(tx, version_id, &profile).await {
        Ok(true) => (Some(profile), Vec::new()),
        Ok(false) => (
            None,
            vec![
                "в этой версии нет векторов текущего профиля: она была опубликована до                  настройки embeddings либо профиль изменился. Пространства разных                  моделей не смешиваются"
                    .to_owned(),
            ],
        ),
        Err(error) => {
            warn!(error = %error, "could not check for vectors");
            (None, vec!["векторная половина поиска недоступна".to_owned()])
        }
    }
}

/// Phase two: embed the query — **outside any transaction**.
///
/// This is the one external call block 1 makes from a request path, and it is kept off
/// the transaction on purpose. Holding one open across somebody else's service would pin
/// a pool connection *and* an idle-in-transaction backend for the whole timeout, so a
/// slow embedding endpoint would stall requests that have nothing to do with it
/// (`block-01-spec.md` §10 — external APIs are called outside a transaction).
///
/// It fails safe: any refusal downgrades this one request to keyword search with the
/// reason reported, rather than turning a search into an error.
async fn embed_query(
    state: &AppState,
    profile: &str,
    query: &str,
) -> (Option<(String, Vec<f32>)>, Vec<String>) {
    // A query longer than the adapter's input bound would be silently clipped before
    // embedding, and the response would then claim `hybrid` over a question the vector
    // half never actually saw. The bounds are configured independently, so this really
    // can happen; refusing the half and saying so is the honest answer.
    let max_input = state.config.retrieval.embedding.limits.max_input_chars;
    if query.chars().count() > usize::try_from(max_input).unwrap_or(usize::MAX) {
        return (
            None,
            vec![format!(
                "векторная половина поиска пропущена: запрос длиннее {max_input} символов,                  которые принимает адаптер embeddings (OTDEL_EMBEDDING_MAX_INPUT_CHARS).                  Поиск выполнен по точным значениям и тексту"
            )],
        );
    }

    let request = EmbedRequest {
        purpose: "search_query",
        inputs: vec![query.to_owned()],
    };
    match state.embeddings.embed(&request).await {
        Ok(response) => match response.vectors.into_iter().next() {
            Some(vector) => (Some((profile.to_owned(), vector)), Vec::new()),
            None => (
                None,
                vec![
                    "векторная половина поиска пропущена: провайдер embeddings не вернул                      вектор для запроса"
                        .to_owned(),
                ],
            ),
        },
        Err(error) => {
            // `diagnostic()` and not `Display`: the text can carry an upstream service's
            // own words, and only `diagnostic()` strips control characters and bounds
            // the length. A log line is not a place to interpolate a remote string.
            let diagnostic = error.diagnostic();
            warn!(
                diagnostic = %diagnostic,
                "embedding the query failed; falling back to keyword search"
            );
            (
                None,
                vec![format!(
                    "векторная половина поиска пропущена: {diagnostic}. Поиск выполнен по                      точным значениям и тексту"
                )],
            )
        }
    }
}

fn version_ref(version: &otdel_core::publication::KnowledgeVersion) -> VersionRef {
    VersionRef {
        id: version.id,
        number: version.number,
        status: version.status.as_str().to_owned(),
        published_at: version.published_at,
    }
}

fn empty_answer(state: AnswerState, message: &str) -> AnswerResponseBody {
    AnswerResponseBody {
        state: state.as_str().to_owned(),
        version: None,
        mode: SearchMode::Keyword.as_str().to_owned(),
        degraded: Vec::new(),
        text: None,
        answer_is_model_context: false,
        claims: Vec::new(),
        citations: Vec::new(),
        conditions: Vec::new(),
        gaps: Vec::new(),
        readiness: Vec::new(),
        limitations: Vec::new(),
        rejections: Vec::new(),
        message: message.to_owned(),
    }
}

/// Turn a stored claim back into the checker's shape, so the answering role reads exactly
/// what the checker wrote — including the verdict, which decides whether it may be shown
/// at all.
fn to_checked(claim: &VersionClaim) -> CheckedClaim {
    CheckedClaim {
        origin: claim.origin,
        origin_id: claim.origin_id,
        product_name: claim.product_name.clone(),
        kind: claim.kind,
        status: claim.status,
        attribute: claim.attribute.clone(),
        value_text: claim.value_text.clone(),
        unit: claim.unit.clone(),
        conditions: claim.conditions.clone(),
        model_context: claim.model_context.clone(),
        check_note: claim.check_note.clone(),
        evidence: claim
            .evidence
            .iter()
            .map(|item| otdel_publish::CheckedEvidence {
                source_kind: item.source_kind,
                material_id: item.material_id,
                material_filename: item.material_filename.clone(),
                page_number: item.page_number,
                region_id: item.region_id,
                url: item.url.clone(),
                host: item.host.clone(),
                retrieved_at: item.retrieved_at,
                content_hash: item.content_hash.clone(),
                quote: item.quote.clone(),
                char_start: item.char_start,
                char_end: item.char_end,
            })
            .collect(),
    }
}

fn check_query(raw: &str, max_chars: u32) -> ApiResult<String> {
    let query = raw.trim();
    if query.is_empty() {
        return Err(ApiError::new(AppError::validation(
            "запрос не может быть пустым",
        )));
    }
    if query.chars().count() > usize::try_from(max_chars).unwrap_or(500) {
        // Refused rather than clipped: answering a shortened version of the question
        // without saying so would be answering a different question.
        return Err(ApiError::new(AppError::validation(format!(
            "запрос длиннее допустимых {max_chars} символов"
        ))));
    }
    Ok(query.to_owned())
}

fn partner_not_found() -> ApiError {
    ApiError::not_found("partner not found in this workspace")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_or_over_long_query_is_refused_rather_than_silently_changed() {
        assert!(check_query("   ", 500).is_err());
        assert!(check_query(&"я".repeat(501), 500).is_err());
        assert_eq!(check_query("  нагрузка  ", 500).unwrap(), "нагрузка");
    }

    #[test]
    fn unanswered_states_never_claim_to_have_citations() {
        for state in [
            AnswerState::NoPublishedVersion,
            AnswerState::InsufficientEvidence,
            AnswerState::EvidenceOnly,
        ] {
            let body = empty_answer(state, "x");
            assert!(body.citations.is_empty());
            assert!(body.text.is_none());
            assert!(!body.answer_is_model_context);
        }
    }
}
