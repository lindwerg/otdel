//! Phase 1E — what the optional halves of retrieval are, and whether they are configured.
//!
//! Split out of [`super::retrieval`] because it answers a different question. That module
//! reads a published version; this one only describes the two adapters that make reading
//! it *better* — embeddings for the semantic half of search, and a model for a prose
//! answer.
//!
//! The wording here is load-bearing and is chosen to stop one specific misreading:
//! `needs_configuration` must never be taken to mean that knowledge is not being checked
//! or published. It is, always, with nothing configured at all. Verification and
//! publication are deterministic (`docs/block-01-spec.md` §6.7), and that is why
//! [`ValidationView`] has a `mode` and no `state`: it cannot be switched off and cannot
//! be waiting for anything.

use axum::extract::State;
use otdel_core::publication::SearchMode;
use otdel_core::retrieval_config::EmbeddingAvailability;
use otdel_db::publication_read;
use serde::Serialize;

use crate::auth::Session;
use crate::error::ApiResult;
use crate::state::AppState;

// --- provider state -------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct AdapterView {
    pub state: String,
    pub provider: String,
    pub endpoint_host: Option<String>,
    pub model: Option<String>,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct ValidationView {
    /// Always `deterministic`. There is no `state` here on purpose: verification cannot
    /// be switched off, and cannot be waiting for anything.
    pub mode: &'static str,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct VectorView {
    /// `ready` | `no_embeddings` | `extension_missing`
    pub state: String,
    pub profile: Option<String>,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct RetrievalLimitsView {
    pub max_query_chars: u32,
    pub max_results: u32,
    pub max_answer_claims: u32,
    pub max_answer_chars: u32,
    pub chunk_max_chars: u32,
}

#[derive(Debug, Serialize)]
pub struct RetrievalProviderResponse {
    pub state: String,
    pub validation: ValidationView,
    pub embedding: AdapterView,
    pub answer: AdapterView,
    pub vector: VectorView,
    pub search_mode: String,
    pub missing: Vec<String>,
    pub limits: RetrievalLimitsView,
    pub message: String,
}

/// Describe the optional halves of 1E.
///
/// The wording is chosen to stop one specific misreading: "не настроено" here must never
/// be taken to mean that knowledge is not being checked or published. It is, always, with
/// nothing configured.
pub fn describe_retrieval(state: &AppState) -> RetrievalProviderResponse {
    let embedding = state.embeddings.describe();
    let model = state.llm.describe();
    let limits = state.config.retrieval.limits;

    let mut missing: Vec<String> = Vec::new();
    for name in &embedding.missing {
        let name = (*name).to_owned();
        if !missing.contains(&name) {
            missing.push(name);
        }
    }
    for name in &model.missing {
        let name = (*name).to_owned();
        if !missing.contains(&name) {
            missing.push(name);
        }
    }

    let embedding_ready = embedding.is_ready();
    // Without a provider there is nothing to embed with, whatever the database has.
    // `extension_missing` is filled in by `provider` below, which can ask the database.
    let vector_state = if embedding_ready {
        "ready"
    } else {
        "no_embeddings"
    };

    let overall = if embedding_ready && model.is_ready() {
        "ready"
    } else if matches!(
        state.config.retrieval.embedding.availability(),
        EmbeddingAvailability::Disabled
    ) && !model.is_ready()
    {
        "disabled"
    } else {
        "needs_configuration"
    };

    let message = if overall == "ready" {
        "Проверка и публикация работают всегда. Дополнительно настроены: семантический \
         поиск и ответы в свободной форме."
            .to_owned()
    } else {
        "Проверка знаний и публикация версий работают без каких-либо ключей — правила \
         детерминированы. Не настроены только две надстройки: семантическая половина \
         поиска и ответ в свободной форме. Без них поиск работает по точным значениям и \
         тексту, а на вопрос возвращаются найденные утверждения с цитатами."
            .to_owned()
    };

    RetrievalProviderResponse {
        state: overall.to_owned(),
        validation: ValidationView {
            mode: "deterministic",
            message: "Проверяющий сверяет каждое утверждение с сохранённым текстом источника \
                      по смещениям. Модель может только понизить статус, но не подтвердить \
                      его: совпадение ответов двух моделей не является доказательством."
                .to_owned(),
        },
        embedding: AdapterView {
            state: embedding.state.to_owned(),
            provider: embedding.provider.clone(),
            endpoint_host: embedding.endpoint_host.clone(),
            model: Some(embedding.model.clone()).filter(|model| !model.is_empty()),
            message: embedding.message.clone(),
        },
        answer: AdapterView {
            state: model.state.to_owned(),
            provider: model.provider.clone(),
            endpoint_host: model.endpoint_host.clone(),
            model: Some(model.model.clone()).filter(|model| !model.is_empty()),
            message: model.message.clone(),
        },
        vector: VectorView {
            state: vector_state.to_owned(),
            profile: embedding.profile.clone(),
            message: if embedding_ready {
                "Векторы строятся при публикации версии.".to_owned()
            } else {
                "Векторы не строятся: провайдер embeddings не настроен. Псевдовекторы не \
                 создаются — поиск честно работает без векторной половины."
                    .to_owned()
            },
        },
        search_mode: if embedding_ready {
            SearchMode::Hybrid.as_str().to_owned()
        } else {
            SearchMode::Keyword.as_str().to_owned()
        },
        missing,
        limits: RetrievalLimitsView {
            max_query_chars: limits.max_query_chars,
            max_results: limits.max_results,
            max_answer_claims: limits.max_answer_claims,
            max_answer_chars: limits.max_answer_chars,
            chunk_max_chars: limits.chunk_max_chars,
        },
        message,
    }
}

/// Apply the one thing only the database knows: whether the `embedding` column exists and
/// is reachable by this role.
///
/// `search_mode` is contractually "фактический режим, а не намерение", so every endpoint
/// that reports it has to go through here. Describing the adapters alone would let
/// `/api/partners/{id}/validation` say `hybrid` on the same page load where
/// `/api/retrieval/provider` correctly says `keyword`.
pub async fn apply_vector_state(
    tx: &mut otdel_db::ScopedTx,
    description: &mut RetrievalProviderResponse,
) -> ApiResult<()> {
    if publication_read::vector_column_exists(tx).await? {
        return Ok(());
    }

    description.vector.state = "extension_missing".to_owned();
    description.vector.message =
        "Расширение pgvector не установлено (или недоступно рабочей роли), поэтому векторы \
         негде хранить. Точный и текстовый поиск работают как обычно. Установить: \
         scripts/dev-extensions.sh (требуются права суперпользователя), затем make migrate."
            .to_owned();
    description.search_mode = SearchMode::Keyword.as_str().to_owned();
    Ok(())
}

pub async fn provider(
    State(state): State<AppState>,
    session: Session,
) -> ApiResult<axum::Json<RetrievalProviderResponse>> {
    let mut description = describe_retrieval(&state);

    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    apply_vector_state(&mut tx, &mut description).await?;
    tx.commit().await?;

    Ok(axum::Json(description))
}
