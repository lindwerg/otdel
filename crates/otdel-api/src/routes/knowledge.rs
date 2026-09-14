//! `/api/partners/{id}/knowledge` — the phase 1C draft, and the button that produces it.
//!
//! Every read here is bounded to one partner inside a bureau-scoped transaction, so a
//! partner id from another workspace returns "not found" rather than somebody else's
//! products. Facts, terms and answers always travel with their evidence: there is no
//! endpoint that hands out a statement without the fragment that supports it.
//!
//! One endpoint is not tenant data at all: `GET /api/knowledge/provider` reports
//! whether the model adapter is configured. It names the provider, the model and the
//! endpoint host — never the key, which no layer above the configuration can read.

use axum::extract::State;
use axum::Json;
use otdel_core::knowledge::{
    DraftableMaterial, GlossaryTerm, KnowledgeFact, KnowledgeGap, KnowledgeRun, KnowledgeRunStatus,
    KnowledgeSummary, Product, ProductCategory, QaEntry,
};
use otdel_core::{AppError, ErrorCode};
use otdel_db::{knowledge, knowledge_read, materials, partners};
use serde::{Deserialize, Serialize};
use tracing::info;
use uuid::Uuid;

use crate::auth::Session;
use crate::dto::ItemsResponse;
use crate::error::{ApiError, ApiResult};
use crate::extract::{ApiPath, ApiQuery};
use crate::routes::partners::partner_not_found;
use crate::state::AppState;

/// State of the model adapter, as shown in the interface.
#[derive(Debug, Serialize)]
pub struct ProviderStateResponse {
    /// `ready` | `needs_configuration` | `disabled`.
    pub state: String,
    pub provider: String,
    pub model: Option<String>,
    /// Host only. The key is never part of any response.
    pub endpoint_host: Option<String>,
    /// Environment variables the owner still has to set.
    pub missing: Vec<String>,
    pub message: String,
}

/// Overview of a partner's draft: the adapter's state, the totals, and the runs.
#[derive(Debug, Serialize)]
pub struct KnowledgeOverview {
    pub provider: ProviderStateResponse,
    pub summary: KnowledgeSummary,
    pub runs: Vec<KnowledgeRun>,
    /// Read materials that have never been drafted. Without these the interface could
    /// only offer "разобрать заново" and a material read before this phase would have
    /// no way in at all.
    pub pending_materials: Vec<DraftableMaterial>,
}

/// A product with the facts drafted about it. `product` is `null` for facts that were
/// stated about the partner's offering as a whole.
#[derive(Debug, Serialize)]
pub struct ProductNode {
    pub product: Option<Product>,
    pub category: Option<ProductCategory>,
    pub facts: Vec<KnowledgeFact>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactQuery {
    pub product_id: Option<Uuid>,
}

/// `GET /api/knowledge/provider` — is the product role configured at all?
pub async fn provider(
    State(state): State<AppState>,
    _session: Session,
) -> ApiResult<Json<ProviderStateResponse>> {
    Ok(Json(describe_provider(&state)))
}

/// `GET /api/partners/{id}/knowledge`
pub async fn overview(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
) -> ApiResult<Json<KnowledgeOverview>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }
    let summary = knowledge_read::summary(&mut tx, partner_id).await?;
    let runs = knowledge_read::list_runs(&mut tx, partner_id).await?;
    let pending_materials = knowledge_read::draftable_materials(&mut tx, partner_id).await?;
    tx.commit().await?;

    Ok(Json(KnowledgeOverview {
        provider: describe_provider(&state),
        summary,
        runs,
        pending_materials,
    }))
}

/// `GET /api/partners/{id}/knowledge/products` — products with their facts.
pub async fn products(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<FactQuery>,
) -> ApiResult<Json<ItemsResponse<ProductNode>>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }
    let categories = knowledge_read::list_categories(&mut tx, partner_id).await?;
    let products = knowledge_read::list_products(&mut tx, partner_id).await?;
    let facts = knowledge_read::list_facts(&mut tx, partner_id, query.product_id).await?;
    tx.commit().await?;

    Ok(Json(ItemsResponse::new(group(categories, products, facts))))
}

/// `GET /api/partners/{id}/knowledge/glossary`
pub async fn glossary(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
) -> ApiResult<Json<ItemsResponse<GlossaryTerm>>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }
    let items = knowledge_read::list_terms(&mut tx, partner_id).await?;
    tx.commit().await?;
    Ok(Json(ItemsResponse::new(items)))
}

/// `GET /api/partners/{id}/knowledge/qa`
pub async fn qa(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
) -> ApiResult<Json<ItemsResponse<QaEntry>>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }
    let items = knowledge_read::list_qa(&mut tx, partner_id).await?;
    tx.commit().await?;
    Ok(Json(ItemsResponse::new(items)))
}

/// `GET /api/partners/{id}/knowledge/gaps` — what the materials do not say.
pub async fn gaps(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
) -> ApiResult<Json<ItemsResponse<KnowledgeGap>>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }
    let items = knowledge_read::list_gaps(&mut tx, partner_id).await?;
    tx.commit().await?;
    Ok(Json(ItemsResponse::new(items)))
}

/// `POST /api/partners/{id}/materials/{material_id}/understand`
///
/// Queues the product role over one material. Idempotent: while a run is queued or
/// running, pressing the button again returns that run instead of starting a second
/// one. Refused with a stated reason when the material has not been read yet or when
/// the model adapter is not configured — a queued job that could only fail would tell
/// the owner nothing.
pub async fn understand(
    State(state): State<AppState>,
    session: Session,
    ApiPath((partner_id, material_id)): ApiPath<(Uuid, Uuid)>,
) -> ApiResult<Json<KnowledgeRun>> {
    let description = describe_provider(&state);
    if description.state != "ready" {
        return Err(ApiError::new(
            AppError::new(ErrorCode::Conflict, description.message)
                // Repeating helps as soon as the key is configured, and the interface
                // uses this to offer the button again rather than hiding it forever.
                .with_retryable(true),
        ));
    }

    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    let material = materials::get_in_partner_with_extraction(&mut tx, partner_id, material_id)
        .await?
        .ok_or_else(material_not_found)?;

    let readable = material
        .extraction
        .as_ref()
        .is_some_and(|summary| summary.pages_extracted + summary.pages_partial > 0);
    if !readable {
        return Err(ApiError::new(AppError::new(
            ErrorCode::Conflict,
            "материал ещё не прочитан: страниц с текстом нет, разбирать нечего",
        )));
    }

    // Already scheduled: return the run that exists rather than arming a second one.
    if let Some(run) = knowledge_read::find_run(&mut tx, partner_id, material_id).await? {
        if matches!(
            run.status,
            KnowledgeRunStatus::Queued | KnowledgeRunStatus::Running
        ) {
            tx.commit().await?;
            return Ok(Json(run));
        }
    }

    let job = otdel_db::jobs::enqueue_understanding(&mut tx, partner_id, material_id).await?;
    let run = knowledge::enqueue_run(
        &mut tx,
        partner_id,
        material_id,
        otdel_knowledge::PROMPT_PROFILE,
    )
    .await?;
    tx.commit().await?;

    info!(
        material_id = %material_id,
        job_id = %job.id,
        "material queued for product understanding by the owner"
    );
    Ok(Json(run))
}

/// Group facts under their product, keeping partner-level facts in a node of their own.
fn group(
    categories: Vec<ProductCategory>,
    products: Vec<Product>,
    facts: Vec<KnowledgeFact>,
) -> Vec<ProductNode> {
    let mut facts = facts;
    let mut nodes: Vec<ProductNode> = Vec::with_capacity(products.len() + 1);

    // Facts about no particular product come first: they describe the offering itself.
    let loose: Vec<KnowledgeFact> = drain(&mut facts, |fact| fact.product_id.is_none());
    if !loose.is_empty() {
        nodes.push(ProductNode {
            product: None,
            category: None,
            facts: loose,
        });
    }

    for product in products {
        let category = product.category_id.and_then(|id| {
            categories
                .iter()
                .find(|category| category.id == id)
                .cloned()
        });
        let owned = drain(&mut facts, |fact| fact.product_id == Some(product.id));
        nodes.push(ProductNode {
            product: Some(product),
            category,
            facts: owned,
        });
    }

    nodes
}

fn drain<T>(items: &mut Vec<T>, mut keep: impl FnMut(&T) -> bool) -> Vec<T> {
    let mut taken = Vec::new();
    let mut rest = Vec::with_capacity(items.len());
    for item in items.drain(..) {
        if keep(&item) {
            taken.push(item);
        } else {
            rest.push(item);
        }
    }
    *items = rest;
    taken
}

fn describe_provider(state: &AppState) -> ProviderStateResponse {
    let description = state.llm.describe();
    ProviderStateResponse {
        state: description.state.to_owned(),
        provider: description.provider,
        model: Some(description.model).filter(|model| !model.is_empty()),
        endpoint_host: description.endpoint_host,
        missing: description
            .missing
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
        message: description.message,
    }
}

fn material_not_found() -> ApiError {
    ApiError::not_found("material not found for this partner")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use otdel_core::knowledge::{FactKind, FactStatus, ProductKind};

    fn product(id: u128, name: &str, category: Option<Uuid>) -> Product {
        Product {
            id: Uuid::from_u128(id),
            partner_id: Uuid::from_u128(1),
            material_id: Uuid::from_u128(2),
            run_id: Uuid::from_u128(3),
            category_id: category,
            kind: ProductKind::Product,
            name: name.to_owned(),
            summary: None,
            created_at: Utc::now(),
        }
    }

    fn fact(id: u128, product_id: Option<Uuid>) -> KnowledgeFact {
        KnowledgeFact {
            id: Uuid::from_u128(id),
            partner_id: Uuid::from_u128(1),
            material_id: Uuid::from_u128(2),
            run_id: Uuid::from_u128(3),
            product_id,
            product_name: None,
            kind: FactKind::Characteristic,
            status: FactStatus::Candidate,
            attribute: "нагрузка".to_owned(),
            value_text: "3.5".to_owned(),
            unit: None,
            conditions: None,
            model_context: None,
            evidence: Vec::new(),
            origin: otdel_core::passport::FactOrigin::default(),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn facts_are_grouped_under_their_product_and_loose_ones_keep_their_own_node() {
        let category = ProductCategory {
            id: Uuid::from_u128(9),
            partner_id: Uuid::from_u128(1),
            material_id: Uuid::from_u128(2),
            run_id: Uuid::from_u128(3),
            kind: otdel_core::knowledge::CategoryKind::Direction,
            name: "Монтажные системы".to_owned(),
            summary: None,
            created_at: Utc::now(),
        };
        let first = product(10, "BP21", Some(category.id));
        let second = product(11, "BP30", None);

        let nodes = group(
            vec![category.clone()],
            vec![first.clone(), second.clone()],
            vec![
                fact(20, Some(first.id)),
                fact(21, None),
                fact(22, Some(first.id)),
            ],
        );

        assert_eq!(nodes.len(), 3);
        assert!(nodes[0].product.is_none());
        assert_eq!(
            nodes[0].facts.len(),
            1,
            "the partner-level fact stands alone"
        );
        assert_eq!(nodes[1].product.as_ref().unwrap().id, first.id);
        assert_eq!(nodes[1].facts.len(), 2);
        assert_eq!(nodes[1].category.as_ref().unwrap().id, category.id);
        assert_eq!(nodes[2].product.as_ref().unwrap().id, second.id);
        assert!(nodes[2].facts.is_empty());
        assert!(nodes[2].category.is_none());
    }

    #[test]
    fn a_product_without_facts_is_still_listed() {
        let nodes = group(Vec::new(), vec![product(10, "BP21", None)], Vec::new());
        assert_eq!(nodes.len(), 1);
        assert!(nodes[0].facts.is_empty());
    }
}
