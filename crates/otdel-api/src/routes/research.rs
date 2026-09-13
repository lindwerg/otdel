//! `/api/partners/{id}/research` — phase 1D: what was asked, what it cost, where the
//! answer came from.
//!
//! Every read is bounded to one partner inside a bureau-scoped transaction, so a partner
//! id from another workspace returns "not found" rather than somebody else's research
//! bill. Findings always travel with their evidence: there is no endpoint here that hands
//! out a conclusion without the external fragment, the URL and the date behind it.
//!
//! Two endpoints are not tenant data at all. `GET /api/research/provider` reports whether
//! the researcher is configured — naming the search host, the declared allowlist and the
//! model, never a key. `GET /api/research/budget` reports the bureau's money.
//!
//! **No handler in this module reaches the network.** Approving a question queues a job;
//! the searching, the paying and the reading happen in the worker, where they are bounded
//! by the plan's limits and can be stopped. An HTTP request that did the research itself
//! would be an unbounded, unstoppable request path with a credit card attached.

use axum::extract::State;
use axum::Json;
use otdel_core::research::{
    IndustryQuestion, ResearchBudget, ResearchFinding, ResearchPlan, ResearchQueryRecord,
    ResearchSource, ResearchSummary,
};
use otdel_core::research_config::SearchProviderKind;
use otdel_core::{AppError, ErrorCode};
use otdel_db::research::NewPlan;
use otdel_db::{jobs, partners, research, research_read};
use serde::{Deserialize, Serialize};
use tracing::info;
use uuid::Uuid;

use crate::auth::Session;
use crate::dto::ItemsResponse;
use crate::error::{ApiError, ApiResult};
use crate::extract::{ApiPath, ApiQuery};
use crate::routes::partners::partner_not_found;
use crate::state::AppState;

/// State of one adapter, as shown in the interface. Never contains a key.
#[derive(Debug, Serialize)]
pub struct AdapterView {
    /// `ready` | `needs_configuration` | `disabled`.
    pub state: String,
    pub provider: String,
    /// Host only, when the adapter has one.
    pub endpoint_host: Option<String>,
    pub model: Option<String>,
    pub message: String,
}

/// The bounds of one research plan, so the interface can state them before anything runs.
#[derive(Debug, Serialize)]
pub struct ResearchLimitsView {
    pub max_queries_per_plan: u32,
    pub max_results_per_query: u32,
    pub max_sources_per_plan: u32,
    pub max_page_bytes: u64,
    pub max_page_chars: u32,
    pub request_timeout_seconds: u64,
    pub plan_time_budget_seconds: u64,
    pub max_passes_per_plan: u32,
    /// Results one plan may accumulate in total. Only the OpenRouter adapter meters
    /// results, so only it sets this.
    pub max_total_results_per_plan: Option<u32>,
}

/// Which engine will run the search, and what it is expected to cost.
///
/// Present only for the OpenRouter adapter — it is the one whose price depends on a
/// choice. Everything here is a *declared tariff*: the number the ledger finally records
/// is whatever the provider reports having charged.
#[derive(Debug, Serialize)]
pub struct SearchEngineView {
    /// As configured: `auto`, `exa`, `parallel` or `native`.
    pub configured: String,
    /// What `auto` resolves to. For a model without built-in search this is `exa`.
    pub effective: String,
    /// `true` when `auto` had to fall back to Exa because the model cannot search itself.
    pub exa_fallback: bool,
    /// Model that runs the tool. Its tokens are part of the bill.
    pub model: String,
    pub max_results: u32,
    pub max_total_results_per_plan: u32,
    /// Forecast for one search call: the engine tariff plus the token allowance.
    pub forecast_micros: u64,
    pub search_base_micros: u64,
    pub included_results: u32,
    pub extra_result_micros: u64,
    pub token_allowance_micros: u64,
    /// The key came from `OTDEL_LLM_API_KEY` rather than one of the researcher's own.
    pub api_key_inherited: bool,
}

/// Whether the researcher can run at all, and what is missing when it cannot.
#[derive(Debug, Serialize)]
pub struct ResearchProviderResponse {
    /// `ready` only when the search endpoint, the host allowlist *and* the model are all
    /// configured. A researcher that could search but not read, or read but not
    /// interpret, would spend money to produce nothing.
    pub state: String,
    pub search: AdapterView,
    pub fetcher: AdapterView,
    pub model: AdapterView,
    /// Environment variables the owner still has to set, across all three.
    pub missing: Vec<String>,
    /// Hosts the researcher is allowed to read, exactly as declared.
    pub allowed_hosts: Vec<String>,
    pub limits: ResearchLimitsView,
    /// Present when the configured adapter is OpenRouter's `openrouter:web_search`.
    pub engine: Option<SearchEngineView>,
    pub message: String,
}

/// Overview of a partner's research: the adapters, the money, the totals, the plans, and
/// the questions nobody has approved yet.
#[derive(Debug, Serialize)]
pub struct ResearchOverview {
    pub provider: ResearchProviderResponse,
    pub budget: ResearchBudget,
    pub summary: ResearchSummary,
    pub plans: Vec<ResearchPlan>,
    /// The approval queue: 1C questions addressed to industry research. Nothing happens
    /// to one until the owner approves it.
    pub questions: Vec<IndustryQuestion>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingQuery {
    pub plan_id: Option<Uuid>,
}

/// `GET /api/research/provider`
pub async fn provider(
    State(state): State<AppState>,
    _session: Session,
) -> ApiResult<Json<ResearchProviderResponse>> {
    Ok(Json(describe_research(&state)))
}

/// `GET /api/research/budget`
pub async fn budget(
    State(state): State<AppState>,
    session: Session,
) -> ApiResult<Json<ResearchBudget>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    let budget = read_budget(&state, &mut tx).await?;
    tx.commit().await?;
    Ok(Json(budget))
}

/// `GET /api/partners/{id}/research`
pub async fn overview(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
) -> ApiResult<Json<ResearchOverview>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }
    let budget = read_budget(&state, &mut tx).await?;
    let summary = research_read::summary(&mut tx, partner_id).await?;
    let plans = research_read::list_plans(&mut tx, partner_id).await?;
    let questions = research_read::industry_questions(&mut tx, partner_id).await?;
    tx.commit().await?;

    Ok(Json(ResearchOverview {
        provider: describe_research(&state),
        budget,
        summary,
        plans,
        questions,
    }))
}

/// `GET /api/partners/{id}/research/plans/{plan_id}/sources` — the source journal.
pub async fn sources(
    State(state): State<AppState>,
    session: Session,
    ApiPath((partner_id, plan_id)): ApiPath<(Uuid, Uuid)>,
) -> ApiResult<Json<ItemsResponse<ResearchSource>>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    // Resolving the plan through the partner is what keeps a plan id from another
    // workspace from returning its journal.
    research_read::find_plan(&mut tx, partner_id, plan_id)
        .await?
        .ok_or_else(plan_not_found)?;
    let items = research_read::list_sources(&mut tx, plan_id).await?;
    tx.commit().await?;
    Ok(Json(ItemsResponse::new(items)))
}

/// `GET /api/partners/{id}/research/plans/{plan_id}/queries` — what was really asked.
pub async fn queries(
    State(state): State<AppState>,
    session: Session,
    ApiPath((partner_id, plan_id)): ApiPath<(Uuid, Uuid)>,
) -> ApiResult<Json<ItemsResponse<ResearchQueryRecord>>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    research_read::find_plan(&mut tx, partner_id, plan_id)
        .await?
        .ok_or_else(plan_not_found)?;
    let items = research_read::list_queries(&mut tx, plan_id).await?;
    tx.commit().await?;
    Ok(Json(ItemsResponse::new(items)))
}

/// `GET /api/partners/{id}/research/findings` — candidate industry conclusions.
pub async fn findings(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<FindingQuery>,
) -> ApiResult<Json<ItemsResponse<ResearchFinding>>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }
    let items = research_read::list_findings(&mut tx, partner_id, query.plan_id).await?;
    tx.commit().await?;
    Ok(Json(ItemsResponse::new(items)))
}

/// `POST /api/partners/{id}/research/questions/{question_id}/plan`
///
/// Approve one 1C question for bounded research. This is the *only* way a plan comes into
/// existence: there is no endpoint that researches a free-text question, and none that
/// researches everything.
///
/// Idempotent: while a plan is queued or running, pressing the button again returns that
/// plan instead of starting a second one. A settled plan is put back in the queue, which
/// is what "исследовать заново" means — bounded by `max_passes`, so the button cannot be
/// turned into an unbounded spend.
pub async fn approve(
    State(state): State<AppState>,
    session: Session,
    ApiPath((partner_id, question_id)): ApiPath<(Uuid, Uuid)>,
) -> ApiResult<Json<ResearchPlan>> {
    let description = describe_research(&state);
    if description.state != "ready" {
        return Err(ApiError::new(
            AppError::new(ErrorCode::Conflict, description.message)
                // Repeating helps as soon as the adapters are configured, and the
                // interface uses this to offer the button again rather than hiding it.
                .with_retryable(true),
        ));
    }

    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    let question = research_read::find_industry_question(&mut tx, partner_id, question_id)
        .await?
        .ok_or_else(|| ApiError::not_found("industry question not found for this partner"))?;

    // Already scheduled: return the plan that exists rather than arming a second one.
    let existing = research_read::find_plan_by_question(&mut tx, question_id).await?;
    if let Some(existing) = &existing {
        if existing.status.is_active() {
            let existing = existing.clone();
            tx.commit().await?;
            return Ok(Json(existing));
        }
    }

    // Enough money for at least one *search*? A plan that cannot make one finds nothing
    // to read, so it is better refused with the reason than queued to report the same
    // thing later. Checked before *both* branches: re-arming a plan with an empty budget
    // is exactly as pointless as creating one.
    //
    // The search price is the right threshold rather than the cheapest of the two: with
    // the default tariff a fetch costs nothing, and `min` would make this check vacuous.
    let budget = read_budget(&state, &mut tx).await?;
    if budget.available_micros < budget.cost_per_search_micros {
        return Err(ApiError::new(AppError::new(
            ErrorCode::Conflict,
            format!(
                "бюджет бюро на исследования исчерпан: доступно {} из {} (в миллионных \
                 долях {}). Поднимите OTDEL_RESEARCH_BUDGET_MICROS, чтобы продолжить",
                budget.available_micros, budget.limit_micros, budget.currency
            ),
        )));
    }

    if let Some(existing) = existing {
        if !research::requeue_plan(&mut tx, existing.id).await? {
            return Err(ApiError::new(AppError::new(
                ErrorCode::Conflict,
                format!(
                    "предел проходов исследования исчерпан ({} из {}): поднимите \
                     OTDEL_RESEARCH_MAX_PASSES_PER_PLAN или сформулируйте новый вопрос",
                    existing.passes, existing.max_passes
                ),
            )));
        }
        let job =
            jobs::enqueue_research(&mut tx, partner_id, question.material_id, existing.id).await?;
        let plan = research_read::find_plan(&mut tx, partner_id, existing.id)
            .await?
            .ok_or_else(plan_not_found)?;
        tx.commit().await?;
        info!(plan_id = %plan.id, job_id = %job.id, "research plan queued again by the owner");
        return Ok(Json(plan));
    }

    let plan = research::create_plan(
        &mut tx,
        &NewPlan {
            partner_id,
            material_id: question.material_id,
            question_id,
            // Copied verbatim: re-drafting this material in 1C replaces its questions,
            // and what was researched must not change underneath the research.
            question_text: question.text.clone(),
            topic: Some(question.gap_topic.clone()),
            prompt_profile: otdel_research::PROMPT_PROFILE.to_owned(),
            budget_micros: i64::try_from(state.config.research.costs.plan_budget_micros)
                .unwrap_or(i64::MAX),
            max_passes: i32::try_from(state.config.research.limits.max_passes_per_plan)
                .unwrap_or(i32::MAX),
        },
    )
    .await?;

    let job = jobs::enqueue_research(&mut tx, partner_id, question.material_id, plan.id).await?;
    tx.commit().await?;

    info!(
        plan_id = %plan.id,
        job_id = %job.id,
        "industry question approved for bounded research by the owner"
    );
    Ok(Json(plan))
}

/// `POST /api/partners/{id}/research/plans/{plan_id}/stop`
///
/// Ask a running plan to stop. The flag is all this does: the worker reads it before
/// every chargeable step and settles the plan there, so nothing is left half-paid and no
/// reservation is stranded.
pub async fn stop(
    State(state): State<AppState>,
    session: Session,
    ApiPath((partner_id, plan_id)): ApiPath<(Uuid, Uuid)>,
) -> ApiResult<Json<ResearchPlan>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    let plan = research_read::find_plan(&mut tx, partner_id, plan_id)
        .await?
        .ok_or_else(plan_not_found)?;

    if !plan.status.is_active() {
        return Err(ApiError::new(AppError::new(
            ErrorCode::Conflict,
            "это исследование уже завершено: останавливать нечего",
        )));
    }

    research::request_cancel(&mut tx, plan_id).await?;
    let plan = research_read::find_plan(&mut tx, partner_id, plan_id)
        .await?
        .ok_or_else(plan_not_found)?;
    tx.commit().await?;

    info!(plan_id = %plan_id, "owner asked a research plan to stop");
    Ok(Json(plan))
}

/// The bureau's money, with the ceilings and the tariff from the configuration.
async fn read_budget(
    state: &AppState,
    tx: &mut otdel_db::ScopedTx,
) -> Result<ResearchBudget, ApiError> {
    let costs = &state.config.research.costs;
    let budget = research_read::budget(
        tx,
        &costs.currency,
        i64::try_from(costs.bureau_budget_micros).unwrap_or(i64::MAX),
        i64::try_from(costs.plan_budget_micros).unwrap_or(i64::MAX),
        i64::try_from(costs.search_micros).unwrap_or(i64::MAX),
        i64::try_from(costs.fetch_micros).unwrap_or(i64::MAX),
        i64::try_from(costs.model_call_micros).unwrap_or(i64::MAX),
    )
    .await?;
    Ok(budget)
}

/// What the interface is told about the researcher.
///
/// The three adapters are reported separately *and* combined, because "почему кнопка
/// недоступна" has three possible answers and naming the wrong one wastes an afternoon.
fn describe_research(state: &AppState) -> ResearchProviderResponse {
    let search = state.search.describe();
    let fetcher = state.fetcher.describe();
    let model = state.llm.describe();

    let ready = search.is_ready() && fetcher.is_ready() && model.is_ready();
    let disabled = search.state == "disabled";

    let mut missing: Vec<String> = Vec::new();
    for name in search.missing.iter().chain(fetcher.missing.iter()) {
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

    let state_label = if ready {
        "ready"
    } else if disabled {
        "disabled"
    } else {
        "needs_configuration"
    };

    // The sentence the owner reads first. When something is missing it names *that*
    // thing: the search adapter's message already lists the variables it needs, and the
    // model's is only shown when the outside half is complete and the interpretation half
    // is not.
    let message = if ready {
        format!(
            "Исследователь готов: поиск через {}, чтение только с {} и модель {}.",
            search
                .endpoint_host
                .clone()
                .unwrap_or_else(|| "—".to_owned()),
            if fetcher.allowed_hosts.is_empty() {
                "—".to_owned()
            } else {
                fetcher.allowed_hosts.join(", ")
            },
            model.model
        )
    } else if disabled || !search.is_ready() || !fetcher.is_ready() {
        search.message.clone()
    } else {
        format!(
            "Исследователь не может интерпретировать источники без модели: {}",
            model.message
        )
    };

    let limits = &state.config.research.limits;
    let research = &state.config.research;
    let uses_openrouter = research.provider == SearchProviderKind::OpenRouterWebSearch;
    let engine = uses_openrouter.then(|| {
        let openrouter = &research.openrouter;
        SearchEngineView {
            configured: openrouter.engine.as_str().to_owned(),
            effective: openrouter.effective_engine().as_str().to_owned(),
            exa_fallback: openrouter.is_exa_fallback(),
            model: openrouter.model.clone(),
            max_results: openrouter.max_results,
            max_total_results_per_plan: openrouter.max_total_results_per_plan,
            forecast_micros: openrouter.forecast_micros(),
            search_base_micros: openrouter.base_micros,
            included_results: openrouter.included_results,
            extra_result_micros: openrouter.extra_result_micros,
            token_allowance_micros: openrouter.token_allowance_micros,
            api_key_inherited: openrouter.api_key_inherited,
        }
    });

    ResearchProviderResponse {
        state: state_label.to_owned(),
        search: AdapterView {
            state: search.state.to_owned(),
            provider: search.provider.clone(),
            endpoint_host: search.endpoint_host.clone(),
            model: None,
            message: search.message.clone(),
        },
        fetcher: AdapterView {
            state: fetcher.state.to_owned(),
            provider: fetcher.provider.clone(),
            endpoint_host: None,
            model: None,
            message: fetcher.message.clone(),
        },
        model: AdapterView {
            state: model.state.to_owned(),
            provider: model.provider.clone(),
            endpoint_host: model.endpoint_host.clone(),
            model: Some(model.model.clone()).filter(|value| !value.is_empty()),
            message: model.message.clone(),
        },
        missing,
        allowed_hosts: state.config.research.allowed_hosts.entries().to_vec(),
        limits: ResearchLimitsView {
            max_queries_per_plan: limits.max_queries_per_plan,
            max_results_per_query: limits.max_results_per_query,
            max_sources_per_plan: limits.max_sources_per_plan,
            max_page_bytes: limits.max_page_bytes,
            max_page_chars: limits.max_page_chars,
            request_timeout_seconds: limits.request_timeout.as_secs(),
            plan_time_budget_seconds: limits.plan_time_budget.as_secs(),
            max_passes_per_plan: limits.max_passes_per_plan,
            max_total_results_per_plan: uses_openrouter
                .then_some(research.openrouter.max_total_results_per_plan),
        },
        engine,
        message,
    }
}

fn plan_not_found() -> ApiError {
    ApiError::not_found("research plan not found for this partner")
}

#[cfg(test)]
mod tests {
    use super::*;
    use otdel_core::research::{FindingScope, FindingStatus};

    #[test]
    fn the_limits_view_mirrors_the_configuration() {
        // The interface states the bounds before anything runs, so "сколько страниц оно
        // прочитает" is answerable without starting a plan.
        let limits = otdel_core::research_config::ResearchLimits::default();
        let view = ResearchLimitsView {
            max_queries_per_plan: limits.max_queries_per_plan,
            max_results_per_query: limits.max_results_per_query,
            max_sources_per_plan: limits.max_sources_per_plan,
            max_page_bytes: limits.max_page_bytes,
            max_page_chars: limits.max_page_chars,
            request_timeout_seconds: limits.request_timeout.as_secs(),
            plan_time_budget_seconds: limits.plan_time_budget.as_secs(),
            max_passes_per_plan: limits.max_passes_per_plan,
            max_total_results_per_plan: None,
        };
        let rendered = serde_json::to_value(&view).unwrap();
        assert_eq!(
            rendered["max_sources_per_plan"],
            limits.max_sources_per_plan
        );
        assert_eq!(
            rendered["plan_time_budget_seconds"],
            limits.plan_time_budget.as_secs()
        );
    }

    #[test]
    fn a_finding_serialises_as_an_industry_candidate_and_nothing_else() {
        let finding = ResearchFinding {
            id: Uuid::from_u128(1),
            partner_id: Uuid::from_u128(2),
            plan_id: Uuid::from_u128(3),
            scope: FindingScope::Industry,
            status: FindingStatus::Candidate,
            topic: "покрытие".to_owned(),
            attribute: "минимальная толщина".to_owned(),
            value_text: "55".to_owned(),
            unit: Some("мкм".to_owned()),
            conditions: None,
            model_context: None,
            evidence: Vec::new(),
            created_at: chrono::Utc::now(),
        };

        let value = serde_json::to_value(&finding).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(object["scope"], "industry");
        assert_eq!(object["status"], "candidate");
        // The fields that would make this a claim about the partner's goods simply do
        // not exist on the wire.
        assert!(!object.contains_key("product_id"));
        assert!(!object.contains_key("product_name"));
        assert!(!object.contains_key("material_id"));
    }
}
