//! Shared harness for the database-backed API tests.
//!
//! These tests need a real PostgreSQL: row-level security, the composite foreign keys
//! and the `SECURITY DEFINER` session functions are the things being verified, and none
//! of them exist in a mock. The connection details come from the environment:
//!
//! ```text
//! OTDEL_TEST_DATABASE_URL        restricted runtime role
//! OTDEL_TEST_ADMIN_DATABASE_URL  migration role (schema owner)
//! OTDEL_TEST_SUPERUSER_URL       optional: only the “privileged role is refused” test
//! ```
//!
//! `scripts/dev-test-db.sh` prints them; `make test-db` sets them and runs the suite.
//! When they are absent the tests **fail with that explanation** instead of reporting
//! success — a database test that silently skips is worse than no test.

#![allow(dead_code)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use otdel_api::state::AppState;
use otdel_core::config::Config;
use otdel_core::secret;
use otdel_db::Database;
use otdel_extract::{OcrEngine, PageProcessor, PageRasteriser, ToolAvailability};
use otdel_llm::LlmProvider;
use otdel_search::{DocumentFetcher, SearchProvider};
use otdel_storage::{FilesystemObjectStore, ObjectStore};
use otdel_worker::{Extractor, KnowledgeWorker, ResearchWorker, ToolReport};
use serde_json::Value;
use sqlx::{Executor, PgPool};
use tower::ServiceExt;
use uuid::Uuid;

pub const OWNER_PASSWORD: &str = "local-owner-password-1a";

/// A running application under test, with its own storage directory and bureau.
pub struct TestApp {
    pub router: Router,
    pub state: AppState,
    pub bureau_id: Uuid,
    pub bureau_slug: String,
    pub storage_root: std::path::PathBuf,
    pub admin_pool: PgPool,
}

impl TestApp {
    /// Build an application bound to a freshly provisioned bureau.
    ///
    /// Every test gets its own bureau (and its own storage namespace), so tests can run
    /// concurrently against one database and still make statements about isolation.
    pub async fn start() -> Self {
        // No research configuration: the researcher is unconfigured, which is both the
        // default state of the product and one of the things the 1D suite asserts.
        Self::start_with_settings(ResearchOverrides::default()).await
    }

    /// Send a request and read the whole response.
    pub async fn send(&self, request: Request<Body>) -> TestResponse {
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("router must not fail");

        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("read the response body")
            .to_bytes();

        TestResponse {
            status,
            headers,
            body: bytes.to_vec(),
        }
    }

    /// Sign in and return a client that carries the session cookie and CSRF token.
    pub async fn sign_in(&self) -> TestClient {
        let response = self
            .send(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/session")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({ "password": OWNER_PASSWORD }).to_string(),
                    ))
                    .unwrap(),
            )
            .await;

        assert_eq!(
            response.status,
            StatusCode::OK,
            "sign-in failed: {}",
            response.text()
        );

        let cookie = response
            .headers
            .get(SET_COOKIE)
            .expect("session cookie")
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let csrf_token = response.json()["csrf_token"].as_str().unwrap().to_owned();

        TestClient { cookie, csrf_token }
    }

    /// Create a partner directly through the API (as an authenticated owner would).
    pub async fn create_partner(&self, client: &TestClient, name: &str) -> Uuid {
        let response = self
            .send(client.json_request(
                Method::POST,
                "/api/partners",
                serde_json::json!({ "name": name }),
            ))
            .await;
        assert_eq!(response.status, StatusCode::CREATED, "{}", response.text());
        Uuid::parse_str(response.json()["id"].as_str().unwrap()).unwrap()
    }

    /// Open a transaction on the migration role **with this bureau's row-level-security
    /// context set**, the way the server does it.
    ///
    /// The tenant tables use `FORCE ROW LEVEL SECURITY`, so the policies apply to the
    /// schema owner too. A fixture that forgets the context does not quietly do nothing:
    /// writes fail with `42501` and reads match zero rows. Tests therefore go through
    /// this helper instead of weakening the policies.
    pub async fn admin_tx(&self) -> sqlx::Transaction<'_, sqlx::Postgres> {
        set_bureau_context(&self.admin_pool, self.bureau_id).await
    }

    /// Run one statement as the migration role inside this bureau's context and assert
    /// how many rows it touched.
    pub async fn admin_update(&self, sql: &str, bind: Uuid, expected_rows: u64) {
        let mut tx = self.admin_tx().await;
        let result = sqlx::query(sql)
            .bind(bind)
            .execute(&mut *tx)
            .await
            .unwrap_or_else(|error| panic!("fixture statement failed: {error}\n{sql}"));
        assert_eq!(
            result.rows_affected(),
            expected_rows,
            "fixture statement touched the wrong number of rows (bureau context missing?)\n{sql}"
        );
        tx.commit().await.expect("commit the fixture transaction");
    }

    /// Remove this test's rows and storage directory.
    ///
    /// Failures are reported, not swallowed: a cleanup that silently does nothing would
    /// leave rows behind and make later runs confusing.
    pub async fn cleanup(self) {
        delete_bureau_as_admin(&self.admin_pool, self.bureau_id).await;
        if let Err(error) = tokio::fs::remove_dir_all(&self.storage_root).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                panic!("could not remove the test storage directory: {error}");
            }
        }
        self.admin_pool.close().await;
    }
}

/// Cookie + CSRF token of a signed-in owner.
#[derive(Debug, Clone)]
pub struct TestClient {
    pub cookie: String,
    pub csrf_token: String,
}

impl TestClient {
    pub fn get(&self, uri: &str) -> Request<Body> {
        Request::builder()
            .method(Method::GET)
            .uri(uri)
            .header(COOKIE, &self.cookie)
            .body(Body::empty())
            .unwrap()
    }

    pub fn json_request(&self, method: Method, uri: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(COOKIE, &self.cookie)
            .header(CONTENT_TYPE, "application/json")
            .header("x-csrf-token", &self.csrf_token)
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// Multipart upload request with one `file` part.
    pub fn upload_request(
        &self,
        uri: &str,
        filename: &str,
        content_type: Option<&str>,
        bytes: &[u8],
    ) -> Request<Body> {
        let body = multipart_body(&[MultipartPart {
            name: "file",
            filename: Some(filename),
            content_type,
            bytes: bytes.to_vec(),
        }]);
        Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(COOKIE, &self.cookie)
            .header("x-csrf-token", &self.csrf_token)
            .header(
                CONTENT_TYPE,
                format!("multipart/form-data; boundary={MULTIPART_BOUNDARY}"),
            )
            .body(Body::from(body))
            .unwrap()
    }

    pub fn raw_multipart_request(&self, uri: &str, parts: &[MultipartPart<'_>]) -> Request<Body> {
        Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(COOKIE, &self.cookie)
            .header("x-csrf-token", &self.csrf_token)
            .header(
                CONTENT_TYPE,
                format!("multipart/form-data; boundary={MULTIPART_BOUNDARY}"),
            )
            .body(Body::from(multipart_body(parts)))
            .unwrap()
    }
}

pub struct TestResponse {
    pub status: StatusCode,
    pub headers: axum::http::HeaderMap,
    pub body: Vec<u8>,
}

impl TestResponse {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|_| {
            panic!(
                "response body is not JSON (status {}): {}",
                self.status,
                self.text()
            )
        })
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// Assert the contract error envelope and return its `code`.
    pub fn error_code(&self) -> String {
        let json = self.json();
        let error = json
            .get("error")
            .unwrap_or_else(|| panic!("no `error` object in: {}", self.text()));
        assert!(error.get("message").is_some(), "error has no message");
        assert!(
            error.get("retryable").is_some(),
            "error has no retryable flag"
        );
        assert_eq!(
            self.headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(|value| value.starts_with("application/json")),
            Some(true),
            "errors must be JSON, got: {}",
            self.text()
        );
        error["code"].as_str().unwrap().to_owned()
    }
}

// --- multipart building ----------------------------------------------------------

pub const MULTIPART_BOUNDARY: &str = "otdeltestboundary9d3f";

pub struct MultipartPart<'a> {
    pub name: &'a str,
    pub filename: Option<&'a str>,
    pub content_type: Option<&'a str>,
    pub bytes: Vec<u8>,
}

pub fn multipart_body(parts: &[MultipartPart<'_>]) -> Vec<u8> {
    let mut body = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{MULTIPART_BOUNDARY}\r\n").as_bytes());
        let mut disposition = format!("Content-Disposition: form-data; name=\"{}\"", part.name);
        if let Some(filename) = part.filename {
            disposition.push_str(&format!("; filename=\"{filename}\""));
        }
        body.extend_from_slice(disposition.as_bytes());
        body.extend_from_slice(b"\r\n");
        if let Some(content_type) = part.content_type {
            body.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
        }
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(&part.bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{MULTIPART_BOUNDARY}--\r\n").as_bytes());
    body
}

// --- tiny synthetic fixtures ------------------------------------------------------
// Generated in-process: no document is committed to this repository, and the real
// catalogues stay private (see docs/development.md).

/// Minimal but structurally valid one-page PDF.
pub fn tiny_pdf(marker: &str) -> Vec<u8> {
    format!(
        "%PDF-1.4\n\
         1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n\
         2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n\
         3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]>>endobj\n\
         % marker {marker}\n\
         trailer<</Root 1 0 R>>\n\
         %%EOF\n"
    )
    .into_bytes()
}

/// 1×1 PNG (signature + minimal chunks).
pub fn tiny_png() -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x0d]);
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
    bytes.extend_from_slice(&[0x1f, 0x15, 0xc4, 0x89]);
    bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
    bytes.extend_from_slice(b"IEND");
    bytes.extend_from_slice(&[0xae, 0x42, 0x60, 0x82]);
    bytes
}

/// JPEG signature followed by filler — enough for signature detection.
pub fn tiny_jpeg() -> Vec<u8> {
    let mut bytes = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10];
    bytes.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
    bytes.extend_from_slice(&[0xff, 0xd9]);
    bytes
}

// --- environment ------------------------------------------------------------------

pub fn require_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!(
            "{name} is not set. The database-backed tests need a real PostgreSQL; run \
             `make test-db`, or `eval \"$(scripts/dev-test-db.sh --export)\"` and then \
             `cargo test`. See docs/backend-1a.md. These tests deliberately fail instead \
             of silently passing without a database."
        )
    })
}

pub fn optional_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn unique_slug() -> String {
    format!("test-{}", Uuid::new_v4().simple())
        .chars()
        .take(64)
        .collect()
}

async fn provision_bureau(admin_pool: &PgPool, slug: &str) -> Uuid {
    let row: (Uuid,) =
        sqlx::query_as("INSERT INTO otdel.bureaus (slug, name) VALUES ($1, $2) RETURNING id")
            .bind(slug)
            .bind("Test bureau")
            .fetch_one(admin_pool)
            .await
            .expect("provision the test bureau");
    row.0
}

/// Insert a partner directly with the migration role — used to build a *foreign*
/// bureau's data that the tested session must never see.
///
/// The insert runs inside that bureau's row-level-security context: the policies are
/// forced for the table owner as well, so this is the only way to write the row without
/// weakening them.
pub async fn insert_partner_as_admin(admin_pool: &PgPool, bureau_id: Uuid, name: &str) -> Uuid {
    let mut tx = set_bureau_context(admin_pool, bureau_id).await;
    let row: (Uuid,) =
        sqlx::query_as("INSERT INTO otdel.partners (bureau_id, name) VALUES ($1, $2) RETURNING id")
            .bind(bureau_id)
            .bind(name)
            .fetch_one(&mut *tx)
            .await
            .expect("insert a partner as the migration role inside the bureau context");
    tx.commit().await.expect("commit the partner insert");
    row.0
}

pub async fn new_bureau_as_admin(admin_pool: &PgPool) -> (Uuid, String) {
    let slug = unique_slug();
    let id = provision_bureau(admin_pool, &slug).await;
    (id, slug)
}

/// Delete everything belonging to one bureau.
///
/// Tenant tables are deleted inside that bureau's context (forced row-level security
/// applies to the owner too); `sessions` and `bureaus` are not force-protected and are
/// deleted afterwards. Errors are propagated — a cleanup helper that ignores them hides
/// exactly the kind of permission problem these tests exist to catch.
pub async fn delete_bureau_as_admin(admin_pool: &PgPool, bureau_id: Uuid) {
    let mut tx = set_bureau_context(admin_pool, bureau_id).await;
    for statement in [
        "DELETE FROM otdel.jobs WHERE bureau_id = $1",
        // Materials cascade to everything 1B/1C/1D derived from them, including research
        // plans and their journals. The budget hangs off the bureau instead and is
        // removed explicitly rather than relying on the cascade of the final DELETE.
        "DELETE FROM otdel.materials WHERE bureau_id = $1",
        "DELETE FROM otdel.partners WHERE bureau_id = $1",
        "DELETE FROM otdel.research_budgets WHERE bureau_id = $1",
    ] {
        sqlx::query(statement)
            .bind(bureau_id)
            .execute(&mut *tx)
            .await
            .unwrap_or_else(|error| panic!("cleanup failed ({statement}): {error}"));
    }
    tx.commit().await.expect("commit the cleanup transaction");

    for statement in [
        "DELETE FROM otdel.sessions WHERE bureau_id = $1",
        "DELETE FROM otdel.bureaus WHERE id = $1",
    ] {
        sqlx::query(statement)
            .bind(bureau_id)
            .execute(admin_pool)
            .await
            .unwrap_or_else(|error| panic!("cleanup failed ({statement}): {error}"));
    }
}

/// SQLSTATE of a database error, for tests that must distinguish “foreign key” from
/// “row-level security refused it”.
pub fn sqlstate(error: &sqlx::Error) -> Option<String> {
    match error {
        sqlx::Error::Database(db_error) => db_error.code().map(|code| code.into_owned()),
        _ => None,
    }
}

/// Set the row-level-security context on a raw connection, the way the server does.
pub async fn set_bureau_context(
    pool: &PgPool,
    bureau_id: Uuid,
) -> sqlx::Transaction<'_, sqlx::Postgres> {
    let mut tx = pool.begin().await.expect("begin");
    tx.execute(sqlx::query("SELECT set_config('otdel.bureau_id', $1::text, true)").bind(bureau_id))
        .await
        .expect("set the bureau context");
    tx
}

/// The environment every test application is built from.
///
/// Returned as a map rather than a `Config` so a suite can override individual variables
/// (see [`ResearchOverrides`]) and still go through the same validation the server does
/// at startup — a test that hand-built a `Config` could give itself a combination the
/// real configuration loader would refuse.
fn test_config_source(
    runtime_url: &str,
    bureau_slug: &str,
    storage_root: &std::path::Path,
) -> std::collections::BTreeMap<String, String> {
    let mut source = std::collections::BTreeMap::new();
    source.insert("OTDEL_DATABASE_URL".to_owned(), runtime_url.to_owned());
    source.insert(
        "OTDEL_OWNER_PASSWORD_HASH".to_owned(),
        secret::hash_password(OWNER_PASSWORD).expect("hash the test password"),
    );
    source.insert("OTDEL_BUREAU_SLUG".to_owned(), bureau_slug.to_owned());
    source.insert(
        "OTDEL_STORAGE_ROOT".to_owned(),
        storage_root.to_string_lossy().into_owned(),
    );
    // Small limit so the “too large” test does not have to stream 25 MiB.
    source.insert("OTDEL_MAX_UPLOAD_BYTES".to_owned(), "4096".to_owned());
    // Recognition is off by default in tests **on purpose**: the suite must produce the
    // same result on a machine with Tesseract installed and on one without. The tests
    // that exercise a working engine install a fake one explicitly
    // (`TestApp::extractor_with`), which is also the only way any of them can produce
    // recognised text.
    source.insert("OTDEL_OCR_ENABLED".to_owned(), "false".to_owned());
    source
}

// --- phase 1B: driving the real worker ------------------------------------------------

impl TestApp {
    /// An extractor that models a machine where recognition is **switched on but not
    /// installed** — the situation the "a scan is not reported as read" checks are about.
    ///
    /// Recognition is enabled in the configuration (so nothing is skipped for the wrong
    /// reason) while both tools probe as missing, and the adapters themselves are the
    /// [`otdel_extract::Disabled`] ones, so there is no path that could return text even
    /// if the permission logic were wrong.
    pub fn extractor(&self) -> Extractor {
        let mut config = (*self.state.config).clone();
        config.extraction.ocr.enabled = true;

        let processor = Arc::new(PageProcessor::new(
            Arc::new(otdel_extract::Disabled::new(
                "tesseract",
                "исполняемый файл `tesseract` не найден",
            )),
            Arc::new(otdel_extract::Disabled::new(
                "pdftoppm",
                "исполняемый файл `pdftoppm` не найден",
            )),
        ));

        Extractor::new(
            Arc::new(config),
            self.state.db.clone(),
            Arc::clone(&self.state.store),
            processor,
            ToolReport {
                engine: ToolAvailability::Unavailable {
                    reason: "исполняемый файл `tesseract` не найден".to_owned(),
                },
                rasteriser: ToolAvailability::Unavailable {
                    reason: "исполняемый файл `pdftoppm` не найден".to_owned(),
                },
            },
        )
    }

    /// An extractor with the supplied adapters, used by the tests that need a working
    /// engine without depending on one being installed.
    pub fn extractor_with(
        &self,
        engine: Arc<dyn OcrEngine>,
        rasteriser: Arc<dyn PageRasteriser>,
    ) -> Extractor {
        let mut config = (*self.state.config).clone();
        config.extraction.ocr.enabled = true;
        let engine_version = "fake-engine 1.0".to_owned();

        Extractor::new(
            Arc::new(config),
            self.state.db.clone(),
            Arc::clone(&self.state.store),
            Arc::new(PageProcessor::new(engine, rasteriser)),
            ToolReport {
                engine: ToolAvailability::Available {
                    version: engine_version.clone(),
                },
                rasteriser: ToolAvailability::Available {
                    version: engine_version,
                },
            },
        )
    }

    /// Drain the queue once, the way `otdel-worker once` does.
    pub async fn run_worker(&self, extractor: &Extractor) -> otdel_worker::ExtractionReport {
        extractor
            .run_pass(self.bureau_id, 16)
            .await
            .expect("the extraction pass must not fail as a whole")
    }
}

// --- phase 1C: driving the product role -----------------------------------------------

impl TestApp {
    /// An application whose model adapter is the supplied one.
    ///
    /// The configuration of a test has no key, so the adapter built from it refuses
    /// every call — which is exactly what the "no key" checks want, and exactly what
    /// the positive checks cannot use. Those pass a scripted provider here; no test
    /// ever reaches a network.
    pub async fn start_with_provider(provider: Arc<dyn LlmProvider>) -> Self {
        let app = Self::start().await;
        let state = app.state.clone().with_provider(provider);
        let router = otdel_api::app(state.clone());
        Self {
            router,
            state,
            ..app
        }
    }

    /// The phase 1C worker, with the supplied provider.
    pub fn knowledge_worker(&self, provider: Arc<dyn LlmProvider>) -> KnowledgeWorker {
        KnowledgeWorker::new(
            Arc::clone(&self.state.config),
            self.state.db.clone(),
            provider,
        )
    }

    /// One understanding pass, the way `otdel-worker once` does it.
    pub async fn run_knowledge(&self, worker: &KnowledgeWorker) -> otdel_worker::KnowledgeReport {
        worker
            .run_pass(self.bureau_id, 8)
            .await
            .expect("the understanding pass must not fail as a whole")
    }
}

// --- phase 1D: driving the researcher ---------------------------------------------------

impl TestApp {
    /// An application whose research adapters are the supplied ones.
    ///
    /// A real researcher needs a search key, a network *and* somebody's money. No test
    /// may acquire any of the three, so the 1D suites pass scripted adapters here and
    /// drive the whole path — route, queue, budget, allowlist, quotation checking,
    /// storage — without them. A test that forgets to do this gets the unconfigured
    /// adapters, which is itself one of the cases worth asserting.
    pub async fn start_with_research(
        search: Arc<dyn SearchProvider>,
        fetcher: Arc<dyn DocumentFetcher>,
        llm: Arc<dyn LlmProvider>,
        settings: ResearchOverrides,
    ) -> Self {
        let app = Self::start_with_settings(settings).await;
        let state = app
            .state
            .clone()
            .with_provider(llm)
            .with_research_adapters(search, fetcher);
        let router = otdel_api::app(state.clone());
        Self {
            router,
            state,
            ..app
        }
    }

    /// An application whose researcher is the **real** OpenRouter adapter, with only its
    /// socket replaced.
    ///
    /// The difference from [`Self::start_with_research`] matters: there the whole search
    /// adapter is a fake and nothing about the OpenRouter one is exercised, while here the
    /// tool arguments, the citation parsing, the cost arithmetic and every refusal are the
    /// production code, and only the `POST` is scripted. The settings come from the loaded
    /// configuration, so the engine, the tariff and the result counts are the ones the
    /// application really has.
    pub async fn start_with_openrouter(
        transport: Arc<dyn otdel_search::ChatTransport>,
        fetcher: Arc<dyn DocumentFetcher>,
        llm: Arc<dyn LlmProvider>,
        settings: ResearchOverrides,
    ) -> (Self, Arc<otdel_search::OpenRouterSearch>) {
        let app = Self::start_with_settings(settings).await;
        let search = Arc::new(otdel_search::OpenRouterSearch::with_transport(
            &app.state.config.research,
            transport,
        ));
        let state = app
            .state
            .clone()
            .with_provider(llm)
            .with_research_adapters(Arc::clone(&search) as Arc<dyn SearchProvider>, fetcher);
        let router = otdel_api::app(state.clone());
        (
            Self {
                router,
                state,
                ..app
            },
            search,
        )
    }

    /// The phase 1D worker, with the supplied adapters.
    pub fn research_worker(
        &self,
        search: Arc<dyn SearchProvider>,
        fetcher: Arc<dyn DocumentFetcher>,
        llm: Arc<dyn LlmProvider>,
    ) -> ResearchWorker {
        ResearchWorker::new(
            Arc::clone(&self.state.config),
            self.state.db.clone(),
            search,
            fetcher,
            llm,
        )
    }

    /// Start with the phase 1E adapters replaced.
    ///
    /// Both are optional in production and both are optional here, which is the property
    /// worth exercising: a suite that passes the unconfigured ones still gets a checked,
    /// published, searchable version — only the vector half and the prose answer are
    /// missing, and the response says so.
    pub async fn start_with_retrieval(
        llm: Arc<dyn LlmProvider>,
        embeddings: Arc<dyn otdel_embed::EmbeddingProvider>,
    ) -> Self {
        let app = Self::start_with_settings(ResearchOverrides::default()).await;
        let state = app
            .state
            .clone()
            .with_provider(llm)
            .with_embeddings(embeddings);
        let router = otdel_api::app(state.clone());
        Self {
            router,
            state,
            ..app
        }
    }

    /// Phase 1F: an app whose retention policy is switched on.
    ///
    /// Retention is off by default — a pilot that pruned its own history because a
    /// default said so would lose the evidence of its first month — so the tests that
    /// assert what a sweep removes have to turn it on explicitly, and they go through the
    /// real `Config::load` in order to do it. A test cannot construct a policy the server
    /// would refuse.
    pub async fn start_with_retention(event_days: u32, job_days: Option<u32>) -> Self {
        let mut overrides = std::collections::BTreeMap::new();
        overrides.insert(
            "OTDEL_RETENTION_EVENT_DAYS".to_owned(),
            event_days.to_string(),
        );
        if let Some(days) = job_days {
            overrides.insert("OTDEL_RETENTION_JOB_DAYS".to_owned(), days.to_string());
        }
        Self::start_with_env(overrides).await
    }

    /// The phase 1E worker, with the supplied optional adapters.
    pub fn validation_worker(
        &self,
        llm: Arc<dyn LlmProvider>,
        embeddings: Arc<dyn otdel_embed::EmbeddingProvider>,
    ) -> otdel_worker::ValidationWorker {
        otdel_worker::ValidationWorker::new(
            Arc::clone(&self.state.config),
            self.state.db.clone(),
            llm,
            embeddings,
        )
    }

    /// One verification pass, the way `otdel-worker once` does it.
    pub async fn run_validation(
        &self,
        worker: &otdel_worker::ValidationWorker,
    ) -> otdel_worker::ValidationReport {
        worker
            .run_pass(self.bureau_id, 4)
            .await
            .expect("the verification pass must not fail as a whole")
    }

    /// Does this database have the pgvector column?
    ///
    /// The suite asserts different things depending on the answer, because both are real
    /// deployments: the extension is not `trusted`, so a database whose operator never
    /// installed it is the normal case, not a broken one.
    pub async fn has_vector_column(&self) -> bool {
        let mut tx = self
            .state
            .db
            .begin_scoped(self.bureau_id)
            .await
            .expect("scoped transaction");
        let present = otdel_db::publication_read::vector_column_exists(&mut tx)
            .await
            .expect("probing for the vector column");
        tx.commit().await.expect("commit");
        present
    }

    /// One research pass, the way `otdel-worker once` does it.
    pub async fn run_research(&self, worker: &ResearchWorker) -> otdel_worker::ResearchReport {
        worker
            .run_pass(self.bureau_id, 4)
            .await
            .expect("the research pass must not fail as a whole")
    }

    /// Start with research configuration applied on top of the standard test settings.
    /// Start with arbitrary extra environment applied on top of the test defaults.
    ///
    /// Everything still goes through the real `Config::load`, so an override that the
    /// server would reject fails the test at startup rather than producing an app in a
    /// state the product cannot be in.
    pub async fn start_with_env(extra: std::collections::BTreeMap<String, String>) -> Self {
        Self::start_with(ResearchOverrides::default(), extra).await
    }

    async fn start_with_settings(settings: ResearchOverrides) -> Self {
        Self::start_with(settings, std::collections::BTreeMap::new()).await
    }

    async fn start_with(
        settings: ResearchOverrides,
        extra: std::collections::BTreeMap<String, String>,
    ) -> Self {
        let runtime_url = require_env("OTDEL_TEST_DATABASE_URL");
        let admin_url = require_env("OTDEL_TEST_ADMIN_DATABASE_URL");

        let admin_pool = PgPool::connect(&admin_url)
            .await
            .expect("connect with the migration role");

        let slug = unique_slug();
        let bureau_id = provision_bureau(&admin_pool, &slug).await;

        let storage_root = std::env::temp_dir().join(format!("otdel-api-test-{}", Uuid::new_v4()));
        let mut source = test_config_source(&runtime_url, &slug, &storage_root);
        settings.apply(&mut source);
        source.extend(extra);
        let config = Config::load(&source).expect("test configuration");

        let db = Database::connect(&config.database_url, 5)
            .await
            .expect("connect with the runtime role");
        db.verify_runtime_role()
            .await
            .expect("the test runtime role must be the restricted one");

        let store = FilesystemObjectStore::open_at(&storage_root)
            .await
            .expect("open the test object store");
        let store: Arc<dyn ObjectStore> = Arc::new(store);

        let state = AppState::new(Arc::new(config), db, store);
        let router = otdel_api::app(state.clone());

        Self {
            router,
            state,
            bureau_id,
            bureau_slug: slug,
            storage_root,
            admin_pool,
        }
    }
}

/// Research configuration a test wants applied on top of the defaults.
///
/// Only the values whose *effects* a test asserts on: the allowlist (which hosts may be
/// read), the money (what stops a plan), and the two limits that decide when a pass ends.
/// Everything else stays at the shipped default, so the suites exercise the configuration
/// the pilot would really run.
#[derive(Debug, Clone, Default)]
pub struct ResearchOverrides {
    pub allowed_hosts: Option<String>,
    pub bureau_budget_micros: Option<u64>,
    pub plan_budget_micros: Option<u64>,
    pub cost_per_search_micros: Option<u64>,
    pub cost_per_fetch_micros: Option<u64>,
    pub cost_per_model_call_micros: Option<u64>,
    pub max_sources_per_plan: Option<u32>,
    pub max_queries_per_plan: Option<u32>,
    pub max_passes_per_plan: Option<u32>,
    /// Configure the researcher as OpenRouter's `openrouter:web_search` rather than the
    /// generic endpoint, with this engine.
    pub openrouter_engine: Option<String>,
    pub openrouter_max_results: Option<u32>,
    pub openrouter_max_total_results: Option<u32>,
    pub openrouter_model: Option<String>,
}

impl ResearchOverrides {
    /// The shape a working researcher has: a declared publisher, and enough money for a
    /// handful of calls.
    pub fn ready() -> Self {
        Self {
            allowed_hosts: Some("docs.example.org".to_owned()),
            ..Self::default()
        }
    }

    pub fn with_hosts(mut self, hosts: &str) -> Self {
        self.allowed_hosts = Some(hosts.to_owned());
        self
    }

    pub fn with_bureau_budget(mut self, micros: u64) -> Self {
        self.bureau_budget_micros = Some(micros);
        self
    }

    pub fn with_plan_budget(mut self, micros: u64) -> Self {
        self.plan_budget_micros = Some(micros);
        self
    }

    pub fn with_search_cost(mut self, micros: u64) -> Self {
        self.cost_per_search_micros = Some(micros);
        self
    }

    pub fn with_fetch_cost(mut self, micros: u64) -> Self {
        self.cost_per_fetch_micros = Some(micros);
        self
    }

    pub fn with_model_cost(mut self, micros: u64) -> Self {
        self.cost_per_model_call_micros = Some(micros);
        self
    }

    pub fn with_max_sources(mut self, sources: u32) -> Self {
        self.max_sources_per_plan = Some(sources);
        self
    }

    pub fn with_max_queries(mut self, queries: u32) -> Self {
        self.max_queries_per_plan = Some(queries);
        self
    }

    pub fn with_max_passes(mut self, passes: u32) -> Self {
        self.max_passes_per_plan = Some(passes);
        self
    }

    /// Run this application as an OpenRouter researcher. The key and the model are the
    /// test ones; nothing reaches a network, because the transport is scripted.
    pub fn with_openrouter(mut self, engine: &str) -> Self {
        self.openrouter_engine = Some(engine.to_owned());
        self
    }

    pub fn with_openrouter_results(mut self, per_query: u32, per_plan: u32) -> Self {
        self.openrouter_max_results = Some(per_query);
        self.openrouter_max_total_results = Some(per_plan);
        self
    }

    pub fn with_openrouter_model(mut self, model: &str) -> Self {
        self.openrouter_model = Some(model.to_owned());
        self
    }

    fn apply(&self, source: &mut std::collections::BTreeMap<String, String>) {
        // The search endpoint and key are set whenever a test declares an allowlist:
        // the adapters injected afterwards are scripted, but the *configuration* has to
        // read as ready or the routes would refuse before the fakes are ever consulted.
        if let Some(hosts) = &self.allowed_hosts {
            if let Some(engine) = &self.openrouter_engine {
                // The OpenRouter adapter has no endpoint of its own; it needs a model, a
                // key and an engine. The key is a test string that never leaves the
                // process: the transport under the adapter is scripted.
                source.insert(
                    "OTDEL_RESEARCH_PROVIDER".to_owned(),
                    "openrouter".to_owned(),
                );
                source.insert(
                    "OTDEL_LLM_API_KEY".to_owned(),
                    "sk-or-v1-test-only-never-used".to_owned(),
                );
                source.insert(
                    "OTDEL_LLM_MODEL".to_owned(),
                    self.openrouter_model
                        .clone()
                        .unwrap_or_else(|| "openai/gpt-4o-mini".to_owned()),
                );
                source.insert(
                    "OTDEL_RESEARCH_OPENROUTER_ENGINE".to_owned(),
                    engine.clone(),
                );
                for (key, value) in [
                    (
                        "OTDEL_RESEARCH_OPENROUTER_MAX_RESULTS",
                        self.openrouter_max_results,
                    ),
                    (
                        "OTDEL_RESEARCH_OPENROUTER_MAX_TOTAL_RESULTS",
                        self.openrouter_max_total_results,
                    ),
                ] {
                    if let Some(value) = value {
                        source.insert(key.to_owned(), value.to_string());
                    }
                }
            } else {
                source.insert(
                    "OTDEL_RESEARCH_SEARCH_URL".to_owned(),
                    "https://search.invalid.test/v1/search".to_owned(),
                );
                source.insert(
                    "OTDEL_RESEARCH_API_KEY".to_owned(),
                    "srch-test-only-never-used".to_owned(),
                );
            }
            source.insert("OTDEL_RESEARCH_ALLOWED_HOSTS".to_owned(), hosts.clone());
        }
        for (key, value) in [
            ("OTDEL_RESEARCH_BUDGET_MICROS", self.bureau_budget_micros),
            ("OTDEL_RESEARCH_PLAN_BUDGET_MICROS", self.plan_budget_micros),
            (
                "OTDEL_RESEARCH_COST_PER_SEARCH_MICROS",
                // With OpenRouter the per-search price is computed from the engine
                // tariff, and setting a flat one is a configuration error rather than an
                // override.
                self.cost_per_search_micros
                    .filter(|_| self.openrouter_engine.is_none()),
            ),
            (
                "OTDEL_RESEARCH_COST_PER_FETCH_MICROS",
                self.cost_per_fetch_micros,
            ),
            (
                "OTDEL_RESEARCH_COST_PER_MODEL_CALL_MICROS",
                self.cost_per_model_call_micros,
            ),
        ] {
            if let Some(value) = value {
                source.insert(key.to_owned(), value.to_string());
            }
        }
        for (key, value) in [
            (
                "OTDEL_RESEARCH_MAX_SOURCES_PER_PLAN",
                self.max_sources_per_plan,
            ),
            (
                "OTDEL_RESEARCH_MAX_QUERIES_PER_PLAN",
                self.max_queries_per_plan,
            ),
            (
                "OTDEL_RESEARCH_MAX_PASSES_PER_PLAN",
                self.max_passes_per_plan,
            ),
        ] {
            if let Some(value) = value {
                source.insert(key.to_owned(), value.to_string());
            }
        }
    }
}
