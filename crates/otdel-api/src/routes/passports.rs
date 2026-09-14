//! `/api/partners/{id}/passports` and the R05 views beside it — the product base as a
//! person reads it.
//!
//! Every endpoint here is bounded to one partner inside a bureau-scoped transaction, like
//! the rest of the API. What is specific to this file is *what it refuses to omit*.
//!
//! A passport returns the product, its facts and its tasks **together with** the gaps, the
//! uncertainties and the identity proposals. There is no query parameter that drops the
//! second half, and no endpoint that returns only the first. An interface can of course
//! choose not to render them — but it cannot fail to receive them, and a reviewer reading
//! this file can see that in one place rather than inferring it from five call sites.
//!
//! The coverage endpoint follows the same rule from the other direction: it returns the
//! whole page account, not only the pages that went wrong, because six explained pages
//! beside an unstated total is exactly the report the audit could not act on.

use axum::extract::State;
use axum::Json;
use otdel_core::passport::{
    KnowledgeDeclaration, KnowledgeUncertainty, PageCoverage, ProductApplication,
    ProductIdentityLink, ProductPassport, RunCoverage, RunPass,
};
use otdel_db::{knowledge_read, partners, passport_read};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::Session;
use crate::dto::ItemsResponse;
use crate::error::{ApiError, ApiResult};
use crate::extract::{ApiPath, ApiQuery};
use crate::routes::partners::partner_not_found;
use crate::state::AppState;

/// Narrowing shared by the views that can be asked about one product.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductQuery {
    pub product_id: Option<Uuid>,
}

/// What one run covered, judged, and what it cost — with the per-page account behind it.
#[derive(Debug, Serialize)]
pub struct CoverageReport {
    pub run_id: Uuid,
    pub material_id: Uuid,
    pub material_filename: String,
    pub status: String,
    #[serde(flatten)]
    pub coverage: RunCoverage,
    /// `true` only when both halves of the gate are satisfied. Serialised rather than
    /// left to the client so the interface and the worker cannot come to different
    /// conclusions about the same run.
    pub allows_automatic_publication: bool,
    /// Pages this run could still turn into knowledge without anything else changing.
    pub resumable_pages: Vec<i32>,
    /// One line per page of the material.
    pub pages: Vec<PageCoverage>,
    /// The topics this run stated are empty, in its own words.
    pub declarations: Vec<KnowledgeDeclaration>,
    /// R05.2 — what each purpose-specific pass covered.
    ///
    /// Beside the page account rather than instead of it: the first answers "was this page
    /// read", these answer "read for what". «44 из 44» with an empty glossary is only
    /// readable when both are on the page.
    pub passes: Vec<RunPass>,
}

/// `GET /api/partners/{id}/passports` — every product, assembled for reading.
pub async fn list(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<ProductQuery>,
) -> ApiResult<Json<ItemsResponse<ProductPassport>>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }
    let items = passport_read::list_passports(&mut tx, partner_id, query.product_id).await?;
    tx.commit().await?;
    Ok(Json(ItemsResponse::new(items)))
}

/// `GET /api/partners/{id}/passports/{product_id}`
pub async fn show(
    State(state): State<AppState>,
    session: Session,
    ApiPath((partner_id, product_id)): ApiPath<(Uuid, Uuid)>,
) -> ApiResult<Json<ProductPassport>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }
    let passport = passport_read::find_passport(&mut tx, partner_id, product_id).await?;
    tx.commit().await?;

    passport
        .map(Json)
        .ok_or_else(|| ApiError::not_found("product not found for this partner"))
}

/// `GET /api/partners/{id}/coverage` — the page account of every run of this partner.
///
/// Returned for every run, including the ones that never reached the model. A run with
/// `coverage_state = "unknown"` is a run nobody judged, and hiding it would restore the
/// silence this package removes.
pub async fn coverage(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
) -> ApiResult<Json<ItemsResponse<CoverageReport>>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }

    let runs = knowledge_read::list_runs(&mut tx, partner_id).await?;
    let mut reports = Vec::with_capacity(runs.len());
    for run in runs {
        let pages = passport_read::page_coverage(&mut tx, run.id).await?;
        let declarations =
            passport_read::list_declarations(&mut tx, partner_id, Some(run.id)).await?;
        let passes = passport_read::run_passes(&mut tx, run.id).await?;
        reports.push(CoverageReport {
            run_id: run.id,
            material_id: run.material_id,
            material_filename: run.material_filename.clone(),
            status: run.status.as_str().to_owned(),
            allows_automatic_publication: run.coverage.allows_automatic_publication(),
            resumable_pages: pages
                .iter()
                .filter(|page| page.disposition.is_resumable())
                .map(|page| page.page_number)
                .collect(),
            pages,
            declarations,
            passes,
            coverage: run.coverage,
        });
    }
    tx.commit().await?;

    Ok(Json(ItemsResponse::new(reports)))
}

/// `GET /api/partners/{id}/applications` — task → product → parameters → questions.
pub async fn applications(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<ProductQuery>,
) -> ApiResult<Json<ItemsResponse<ProductApplication>>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }
    let items = passport_read::list_applications(&mut tx, partner_id, query.product_id).await?;
    tx.commit().await?;
    Ok(Json(ItemsResponse::new(items)))
}

/// `GET /api/partners/{id}/uncertainties` — what the materials say and nobody may read.
pub async fn uncertainties(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<ProductQuery>,
) -> ApiResult<Json<ItemsResponse<KnowledgeUncertainty>>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }
    let items = passport_read::list_uncertainties(&mut tx, partner_id, query.product_id).await?;
    tx.commit().await?;
    Ok(Json(ItemsResponse::new(items)))
}

/// `GET /api/partners/{id}/identity` — proposals that two product rows are one product.
pub async fn identity(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
) -> ApiResult<Json<ItemsResponse<ProductIdentityLink>>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }
    let items = passport_read::list_identity_links(&mut tx, partner_id).await?;
    tx.commit().await?;
    Ok(Json(ItemsResponse::new(items)))
}

/// `GET /api/partners/{id}/declarations` — the explicit absences on the record.
pub async fn declarations(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
) -> ApiResult<Json<ItemsResponse<KnowledgeDeclaration>>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }
    let items = passport_read::list_declarations(&mut tx, partner_id, None).await?;
    tx.commit().await?;
    Ok(Json(ItemsResponse::new(items)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use otdel_core::passport::{CoverageState, RequirementsState};

    fn report(coverage: RunCoverage, pages: Vec<PageCoverage>) -> CoverageReport {
        CoverageReport {
            run_id: Uuid::from_u128(1),
            material_id: Uuid::from_u128(2),
            material_filename: "catalogue.pdf".to_owned(),
            status: "partial".to_owned(),
            allows_automatic_publication: coverage.allows_automatic_publication(),
            resumable_pages: pages
                .iter()
                .filter(|page| page.disposition.is_resumable())
                .map(|page| page.page_number)
                .collect(),
            pages,
            declarations: Vec::new(),
            passes: Vec::new(),
            coverage,
        }
    }

    #[test]
    fn a_report_says_whether_the_run_may_publish_itself_rather_than_leaving_it_to_be_derived() {
        let ready = report(
            RunCoverage {
                pages_total: 4,
                pages_processed: 4,
                state: CoverageState::Complete,
                requirements: RequirementsState::Met,
                ..RunCoverage::default()
            },
            Vec::new(),
        );
        assert!(ready.allows_automatic_publication);

        // Fully covered, nothing to show for it: the gate needs both halves.
        let thin = report(
            RunCoverage {
                pages_total: 4,
                pages_processed: 4,
                state: CoverageState::Complete,
                requirements: RequirementsState::Unmet,
                requirements_missing: vec!["glossary: нет терминов".to_owned()],
                ..RunCoverage::default()
            },
            Vec::new(),
        );
        assert!(!thin.allows_automatic_publication);

        // A run nobody judged is not a run that passed.
        let unjudged = report(RunCoverage::default(), Vec::new());
        assert!(!unjudged.allows_automatic_publication);
    }

    #[test]
    fn the_report_serialises_the_account_beside_the_verdict() {
        let value = serde_json::to_value(report(
            RunCoverage {
                pages_total: 44,
                pages_processed: 36,
                pages_deferred: 8,
                state: CoverageState::Incomplete,
                notes: vec!["страница отложена: исчерпан бюджет запросов".to_owned()],
                requirements: RequirementsState::Unmet,
                requirements_missing: vec!["glossary: нет терминов".to_owned()],
                ..RunCoverage::default()
            },
            Vec::new(),
        ))
        .unwrap();

        // Flattened, so a client reads `pages_total` rather than `coverage.pages_total`.
        assert_eq!(value["pages_total"], 44);
        assert_eq!(value["pages_processed"], 36);
        assert_eq!(value["pages_deferred"], 8);
        assert_eq!(value["state"], "incomplete");
        assert_eq!(value["requirements"], "unmet");
        assert_eq!(value["allows_automatic_publication"], false);
        assert!(value["notes"][0].is_string());
    }
}
