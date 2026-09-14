//! Phase 1F end to end: a new document starts a new cycle, and nothing already published
//! is disturbed by it.
//!
//! These are the acceptance scenarios of `docs/uat-1f.md`, executed. Everything is real
//! except the model adapter, which is scripted ([`otdel_llm::fake`]) so a draft can be
//! produced without a key and without a network: the HTTP API, PostgreSQL with its
//! row-level security, its triggers and its retention functions, the object store, and
//! the extraction, understanding and validation workers as the pilot runs them.
//!
//! Every quotation asserted here comes from text the real extractor produced. Nothing in
//! this file hand-writes what a parser "should" have said.

mod support;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use otdel_embed::EmbeddingProvider;
use otdel_llm::fake::{FakeProvider, FakeReply};
use otdel_llm::{LlmProvider, UnconfiguredProvider};
use serde_json::{json, Value};
use sqlx::Row;
use support::{TestApp, TestClient, TestResponse};
use uuid::Uuid;

// --- helpers ------------------------------------------------------------------------------

fn unconfigured_llm() -> Arc<dyn LlmProvider> {
    Arc::new(UnconfiguredProvider::new(
        &otdel_core::llm_config::LlmSettings::default(),
    ))
}

fn unconfigured_embeddings() -> Arc<dyn EmbeddingProvider> {
    otdel_embed::build_provider(&otdel_core::retrieval_config::EmbeddingSettings::default())
}

async fn start() -> TestApp {
    TestApp::start_with_retrieval(unconfigured_llm(), unconfigured_embeddings()).await
}

async fn get(app: &TestApp, client: &TestClient, uri: &str) -> Value {
    let response = app.send(client.get(uri)).await;
    assert_eq!(
        response.status,
        StatusCode::OK,
        "{}: {}",
        uri,
        response.text()
    );
    response.json()
}

async fn post(app: &TestApp, client: &TestClient, uri: &str, body: Value) -> TestResponse {
    app.send(client.json_request(Method::POST, uri, body)).await
}

/// Upload one document and let the real extractor read it.
async fn upload_and_read(
    app: &TestApp,
    client: &TestClient,
    partner: Uuid,
    filename: &str,
    bytes: &[u8],
) -> Uuid {
    let response = app
        .send(client.upload_request(
            &format!("/api/partners/{partner}/materials"),
            filename,
            Some("application/pdf"),
            bytes,
        ))
        .await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.text());
    let material_id = Uuid::parse_str(response.json()["id"].as_str().unwrap()).unwrap();
    app.run_worker(&app.extractor()).await;
    material_id
}

async fn page_text(app: &TestApp, client: &TestClient, partner: Uuid, material: Uuid) -> String {
    let detail = get(
        app,
        client,
        &format!("/api/partners/{partner}/materials/{material}/pages/1"),
    )
    .await;
    detail["text"].as_str().expect("page text").to_owned()
}

fn quotable(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| line.chars().count() >= 12)
        .expect("the fixture page has a quotable line")
        .to_owned()
}

fn value_from(quote: &str) -> String {
    quote
        .split_whitespace()
        .find(|word| word.chars().count() >= 4)
        .expect("the quotable line has a word to use as a value")
        .to_owned()
}

/// A 1C draft with one product and the given facts.
fn draft(product: &str, facts: Vec<Value>) -> Value {
    json!({
        "categories": [{
            "ref": "c1", "kind": "direction",
            "name": "Монтажные системы", "summary": null,
        }],
        "products": [{
            "ref": "p1", "category_ref": "c1", "kind": "product",
            "name": product, "summary": "профиль монтажный",
        }],
        "facts": facts,
        "glossary": [],
        "qa": [],
        "gaps": [{
            "product_ref": "p1", "topic": "цена",
            "missing": "цена не указана в материале",
            "blocks": "коммерческое предложение",
            "question": "Какая отпускная цена профиля?",
            "audience": "partner",
            "nature": "commercial",
        }],
        "applications": [],
        // R05: this suite is about the *publication* rules, so its draft has to be one
        // R05 passes — otherwise every test below would be blocked by the new gate for a
        // reason that has nothing to do with what it is testing. One page and one product
        // is small enough for a sentence to answer for, and the run holds nothing that
        // contradicts one.
        "declarations": {
            "glossary": "лист не вводит терминов, требующих пояснения",
            "questions": null,
            "applications": "лист перечисляет обозначения и не описывает задач применения",
            "commercial_unknowns": null,
            "technical_unknowns": "технических величин, кроме приведённых, на листе нет",
        },
    })
}

fn fact(attribute: &str, value: &str, quote: &str) -> Value {
    json!({
        "product_ref": "p1", "kind": "characteristic",
        "attribute": attribute, "value": value,
        "unit": null, "conditions": null,
        "model_context": "формулировка модели, не цитата",
        "evidence": [{"source": "S1", "quote": quote}],
    })
}

/// Draft one material with a scripted reply, and run the understanding worker.
async fn draft_material(app: &TestApp, reply: Value) -> otdel_worker::KnowledgeReport {
    let provider: Arc<FakeProvider> = Arc::new(FakeProvider::new(vec![FakeReply::Json(reply)]));
    app.run_knowledge(&app.knowledge_worker(provider)).await
}

/// Ask for one material to be drafted again, the way the extraction worker does when a
/// document has been read.
///
/// Deliberately not through `POST .../understand`: that endpoint refuses while no model
/// adapter is configured — correctly, and its own test asserts it — and these tests run
/// with the unconfigured adapter on the request path and a scripted one in the worker.
async fn requeue_draft(app: &TestApp, partner: Uuid, material: Uuid) {
    let mut tx = app.state.db.begin_scoped(app.bureau_id).await.unwrap();
    otdel_db::jobs::enqueue_understanding(&mut tx, partner, material)
        .await
        .expect("queue the draft");
    otdel_db::knowledge::enqueue_run(&mut tx, partner, material, otdel_knowledge::PROMPT_PROFILE)
        .await
        .expect("arm the run row");
    tx.commit().await.unwrap();
}

/// Run the checker with both optional adapters absent — the normal configuration.
async fn check(app: &TestApp) -> otdel_worker::ValidationReport {
    let worker = app.validation_worker(unconfigured_llm(), unconfigured_embeddings());
    app.run_validation(&worker).await
}

async fn published(app: &TestApp, client: &TestClient, partner: Uuid) -> Value {
    get(app, client, &format!("/api/partners/{partner}/validation")).await["published"].clone()
}

async fn refresh_status(app: &TestApp, client: &TestClient, partner: Uuid) -> Value {
    get(app, client, &format!("/api/partners/{partner}/refresh")).await
}

async fn events(app: &TestApp, client: &TestClient, partner: Uuid) -> Vec<Value> {
    get(app, client, &format!("/api/partners/{partner}/events")).await["items"]
        .as_array()
        .expect("items")
        .clone()
}

fn kinds(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| event["kind"].as_str().unwrap_or_default().to_owned())
        .collect()
}

fn reason_codes(status: &Value) -> Vec<String> {
    status["reasons"]
        .as_array()
        .expect("reasons")
        .iter()
        .map(|reason| reason["code"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// Upload → read → draft → check → published version 1. Returns `(material, quote, value)`.
async fn publish_first_version(
    app: &TestApp,
    client: &TestClient,
    partner: Uuid,
) -> (Uuid, String, String) {
    let material = upload_and_read(
        app,
        client,
        partner,
        "catalogue.pdf",
        &otdel_extract::fixtures::text_pdf(),
    )
    .await;
    let text = page_text(app, client, partner, material).await;
    let quote = quotable(&text);
    let value = value_from(&quote);

    let report = draft_material(
        app,
        draft("BP21", vec![fact("обозначение", &value, &quote)]),
    )
    .await;
    assert_eq!(report.facts_stored, 1, "the draft must store one fact");

    let checked = check(app).await;
    assert_eq!(
        checked.versions_published, 1,
        "the first check must publish version 1"
    );
    (material, quote, value)
}

// --- the central scenario -------------------------------------------------------------------

/// UAT 1 — a new document starts a new cycle, and the published version it replaces stays
/// exactly as it was.
#[tokio::test]
async fn a_new_document_produces_a_newer_version_and_the_old_one_is_untouched() {
    let app = start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    publish_first_version(&app, &client, partner).await;

    let first = published(&app, &client, partner).await;
    let first_id = first["id"].as_str().unwrap().to_owned();
    let first_claims = get(
        &app,
        &client,
        &format!("/api/partners/{partner}/versions/{first_id}/claims"),
    )
    .await;
    let first_published_at = first["published_at"].as_str().unwrap().to_owned();

    // The partner sends a second catalogue. Nothing about the first one is touched: a
    // changed file is a different material, because deduplication is by content.
    let second = upload_and_read(
        &app,
        &client,
        partner,
        "loads.pdf",
        &otdel_extract::fixtures::table_pdf(),
    )
    .await;
    let second_text = page_text(&app, &client, partner, second).await;
    let second_quote = quotable(&second_text);
    let second_value = value_from(&second_quote);

    // Drafting the new document is enough: the worker chains the check itself, so no
    // button is required between the stages (`block-01-spec.md` §11).
    let report = draft_material(
        &app,
        draft("BP21", vec![fact("нагрузка", &second_value, &second_quote)]),
    )
    .await;
    assert_eq!(report.facts_stored, 1);

    let checked = check(&app).await;
    assert_eq!(
        checked.versions_published, 1,
        "the new candidate set must publish a newer version"
    );
    // The follow-up guard fires only when candidates really moved during a check.
    // Publishing does not move them, so a settled check must not queue another — that is
    // what keeps the guard from becoming a loop.
    assert_eq!(
        checked.checks_requeued, 0,
        "a check whose candidates did not change must not queue another: {checked:?}"
    );

    // The newer version is current and carries a higher number.
    let current = published(&app, &client, partner).await;
    assert_eq!(current["number"], 2);
    assert_ne!(current["id"], first["id"]);

    // The old version is superseded — not deleted, not altered, not re-timed.
    let old = get(
        &app,
        &client,
        &format!("/api/partners/{partner}/versions/{first_id}"),
    )
    .await;
    assert_eq!(old["status"], "superseded");
    assert_eq!(
        old["published_at"].as_str().unwrap(),
        first_published_at,
        "a published time, once set, is history"
    );
    assert!(old["superseded_at"].is_string());

    let old_claims = get(
        &app,
        &client,
        &format!("/api/partners/{partner}/versions/{first_id}/claims"),
    )
    .await;
    assert_eq!(
        old_claims, first_claims,
        "every claim, verdict and quotation of the replaced version must be byte-for-byte \
         what it was"
    );

    // And it is still readable when pinned: that is the whole point of pinning.
    let pinned = post(
        &app,
        &client,
        &format!("/api/partners/{partner}/retrieval/search"),
        json!({ "query": "обозначение", "version_id": first_id }),
    )
    .await;
    assert_eq!(pinned.status, StatusCode::OK, "{}", pinned.text());
    assert_eq!(pinned.json()["version"]["number"], 1);

    // The history says what happened, in order, and names both versions.
    let log = events(&app, &client, partner).await;
    let kinds = kinds(&log);
    for expected in [
        "material_uploaded",
        "material_extraction_finished",
        "understanding_queued",
        "understanding_finished",
        "validation_queued",
        "version_published",
        "version_superseded",
    ] {
        assert!(
            kinds.contains(&expected.to_owned()),
            "missing {expected}: {kinds:?}"
        );
    }
    let superseded = log
        .iter()
        .find(|event| event["kind"] == "version_superseded")
        .expect("the replacement is recorded");
    assert_eq!(superseded["detail"]["superseded_number"], 1);
    assert_eq!(superseded["detail"]["replaced_by_number"], 2);

    app.cleanup().await;
}

/// UAT 2 — the comparison of two versions names what changed and where it came from.
#[tokio::test]
async fn comparing_two_versions_names_the_changed_value_and_its_source() {
    let app = start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let (material, quote, value) = publish_first_version(&app, &client, partner).await;

    // The same property, re-drafted with a different value found in the same document.
    // A re-draft writes new candidate rows with new identifiers, so this also proves the
    // comparison does not match on `origin_id`.
    let other_value = quote
        .split_whitespace()
        .filter(|word| word.chars().count() >= 4)
        .nth(1)
        .expect("a second usable word")
        .to_owned();
    assert_ne!(other_value, value);

    requeue_draft(&app, partner, material).await;
    let report = draft_material(
        &app,
        draft("BP21", vec![fact("обозначение", &other_value, &quote)]),
    )
    .await;
    assert_eq!(report.facts_stored, 1);
    check(&app).await;

    let current = published(&app, &client, partner).await;
    assert_eq!(current["number"], 2);
    let version_id = current["id"].as_str().unwrap();

    let changes = get(
        &app,
        &client,
        &format!("/api/partners/{partner}/versions/{version_id}/changes"),
    )
    .await;
    assert_eq!(changes["from"]["number"], 1);
    assert_eq!(changes["to"]["number"], 2);
    assert_eq!(changes["counts"]["changed"], 1);
    assert_eq!(changes["counts"]["added"], 0);
    assert_eq!(changes["counts"]["removed"], 0);

    let change = &changes["claims"][0];
    assert_eq!(change["kind"], "changed");
    assert_eq!(change["before"]["value_text"], value);
    assert_eq!(change["after"]["value_text"], other_value);
    // The source is named on both sides, with the page.
    let source = change["after"]["sources"][0].as_str().unwrap();
    assert!(source.starts_with("catalogue.pdf#"), "{source}");
    assert!(
        changes["limitations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line
                .as_str()
                .unwrap_or_default()
                .contains("Переименованное")),
        "the comparison must state what it cannot see"
    );

    // The replaced version's own claims still point at the material they always did.
    assert_eq!(
        change["before"]["sources"][0].as_str().unwrap(),
        source,
        "both versions cite the same document in this scenario"
    );

    app.cleanup().await;
}

// --- withdrawal ------------------------------------------------------------------------------

/// UAT 3 — withdrawing a version stops every answer immediately, and removes its search
/// index while keeping the snapshot as history.
#[tokio::test]
async fn retracting_hides_the_version_from_retrieval_at_once_and_keeps_it_as_history() {
    let app = start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let (_, _, value) = publish_first_version(&app, &client, partner).await;

    let version = published(&app, &client, partner).await;
    let version_id = version["id"].as_str().unwrap().to_owned();

    // Before: the version answers.
    let before = post(
        &app,
        &client,
        &format!("/api/partners/{partner}/retrieval/search"),
        json!({ "query": value }),
    )
    .await;
    assert_eq!(before.json()["state"], "ok");
    assert!(!before.json()["items"].as_array().unwrap().is_empty());

    let retracted = post(
        &app,
        &client,
        &format!("/api/partners/{partner}/versions/{version_id}/retract"),
        json!({ "reason": "в каталоге нашли опечатку в нагрузке" }),
    )
    .await;
    assert_eq!(retracted.status, StatusCode::OK, "{}", retracted.text());
    assert_eq!(retracted.json()["status"], "revoked");

    // Immediately, with no rebuild in between.
    let after = post(
        &app,
        &client,
        &format!("/api/partners/{partner}/retrieval/search"),
        json!({ "query": value }),
    )
    .await;
    assert_eq!(after.json()["state"], "no_published_version");
    assert!(after.json()["items"].as_array().unwrap().is_empty());

    let asked = post(
        &app,
        &client,
        &format!("/api/partners/{partner}/retrieval/answer"),
        json!({ "question": "что это за профиль?" }),
    )
    .await;
    assert_eq!(asked.json()["state"], "no_published_version");

    // Pinning the withdrawn version is refused with its reason, not silently answered.
    let pinned = post(
        &app,
        &client,
        &format!("/api/partners/{partner}/retrieval/search"),
        json!({ "query": value, "version_id": version_id }),
    )
    .await;
    assert_eq!(pinned.status, StatusCode::CONFLICT, "{}", pinned.text());

    // The searchable rendering is gone; the snapshot is not.
    let mut tx = app.admin_tx().await;
    let chunks: i64 =
        sqlx::query("SELECT count(*) AS n FROM otdel.version_chunks WHERE version_id = $1")
            .bind(Uuid::parse_str(&version_id).unwrap())
            .fetch_one(&mut *tx)
            .await
            .expect("count chunks")
            .try_get("n")
            .unwrap();
    let claims: i64 =
        sqlx::query("SELECT count(*) AS n FROM otdel.version_claims WHERE version_id = $1")
            .bind(Uuid::parse_str(&version_id).unwrap())
            .fetch_one(&mut *tx)
            .await
            .expect("count claims")
            .try_get("n")
            .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        chunks, 0,
        "a withdrawn version leaves no search index behind"
    );
    assert!(claims > 0, "the snapshot stays as history");

    // The refresh status names the retraction and its reason rather than looking empty.
    let status = refresh_status(&app, &client, partner).await;
    assert_eq!(status["state"], "retracted");
    assert!(reason_codes(&status).contains(&"version_retracted".to_owned()));
    assert!(status["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reason| reason["message"]
            .as_str()
            .unwrap_or_default()
            .contains("опечатку")));

    // And the log carries it with the reason.
    let log = events(&app, &client, partner).await;
    let event = log
        .iter()
        .find(|event| event["kind"] == "version_retracted")
        .expect("the retraction is recorded");
    assert_eq!(event["actor"], "owner");
    assert_eq!(event["detail"]["number"], 1);

    app.cleanup().await;
}

/// UAT 4 — a check that publishes nothing does not take away what is published.
#[tokio::test]
async fn a_blocked_check_leaves_the_published_version_answering() {
    let app = start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let (material, _, value) = publish_first_version(&app, &client, partner).await;

    let first = published(&app, &client, partner).await;
    let first_id = first["id"].as_str().unwrap().to_owned();

    // The page is re-read and now says something else: every citation is stale, so the
    // next check has nothing supported to publish.
    app.admin_update(
        "UPDATE otdel.material_pages SET text_content = 'Совершенно другой документ.' \
         WHERE material_id = $1",
        material,
        1,
    )
    .await;

    let response = post(
        &app,
        &client,
        &format!("/api/partners/{partner}/validate"),
        json!({}),
    )
    .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text());
    let checked = check(&app).await;
    assert_eq!(checked.versions_published, 0);
    assert_eq!(checked.versions_blocked, 1);

    // Still published, still the same version, still answering.
    let current = published(&app, &client, partner).await;
    assert_eq!(current["id"], first["id"]);
    assert_eq!(current["status"], "published");

    let search = post(
        &app,
        &client,
        &format!("/api/partners/{partner}/retrieval/search"),
        json!({ "query": value }),
    )
    .await;
    assert_eq!(search.json()["state"], "ok");
    assert_eq!(search.json()["version"]["id"], first["id"]);

    // The refresh status explains it, and the log records the blocked version naming the
    // one that stays live.
    let status = refresh_status(&app, &client, partner).await;
    assert_eq!(status["state"], "revalidation_required");
    assert!(reason_codes(&status).contains(&"last_check_blocked".to_owned()));

    let log = events(&app, &client, partner).await;
    let blocked = log
        .iter()
        .find(|event| event["kind"] == "version_blocked")
        .expect("the blocked version is recorded");
    assert_eq!(blocked["detail"]["still_published_number"], 1);
    let _ = first_id;

    app.cleanup().await;
}

// --- staleness ---------------------------------------------------------------------------------

/// UAT 5 — re-reading a document marks everything drawn from the earlier reading as
/// needing a fresh check, and names the document.
#[tokio::test]
async fn re_reading_a_document_marks_its_draft_as_made_from_an_older_reading() {
    let app = start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let (material, _, _) = publish_first_version(&app, &client, partner).await;

    // Right after publishing, nothing is out of date.
    let status = refresh_status(&app, &client, partner).await;
    assert_eq!(status["state"], "current", "{}", status["message"]);
    assert!(status["reasons"].as_array().unwrap().is_empty());
    // The fingerprint the request path computes and the one the worker stored have to be
    // the same string. They come from two different loaders — the worker's carries the
    // source text a check needs, this one deliberately does not — and if they ever
    // disagreed every partner would permanently read as "нужна перепроверка".
    assert_eq!(
        status["candidate_fingerprint"], status["published_candidate_fingerprint"],
        "the digest loader and the checker's loader must hash to the same value"
    );
    assert_eq!(status["sources"][0]["state"], "drafted");
    assert_eq!(status["sources"][0]["content_revision"], 1);
    assert_eq!(status["sources"][0]["drafted_revision"], 1);
    assert_eq!(status["sources"][0]["claims_in_published"], 1);

    // The owner asks for the document to be read again — a finished material, which
    // `.../retry` refuses on purpose.
    let refused = post(
        &app,
        &client,
        &format!("/api/partners/{partner}/materials/{material}/retry"),
        json!({}),
    )
    .await;
    assert_eq!(refused.status, StatusCode::CONFLICT);

    let reprocess = post(
        &app,
        &client,
        &format!("/api/partners/{partner}/materials/{material}/reprocess"),
        json!({}),
    )
    .await;
    assert_eq!(reprocess.status, StatusCode::OK, "{}", reprocess.text());
    app.run_worker(&app.extractor()).await;

    // The reading counted, and the re-read chained a fresh draft by itself — so the
    // honest state right now is "не разобран (в очереди)", not "разобран". Nothing here
    // claims the document is up to date.
    let status = refresh_status(&app, &client, partner).await;
    assert_eq!(status["sources"][0]["content_revision"], 2);
    assert_eq!(status["sources"][0]["state"], "not_drafted");
    assert_eq!(status["sources"][0]["draft_status"], "queued");
    assert_eq!(status["state"], "revalidation_required");
    assert!(reason_codes(&status).contains(&"material_not_drafted".to_owned()));

    // Now the case the state `reread_after_draft` exists for: a draft that *started*
    // before the re-read finished records the older reading and then completes. The
    // worker writes exactly this row (`set_draft_source_revision` at run start, status at
    // the end), so installing it directly reproduces the race deterministically instead
    // of trying to win it.
    app.admin_update(
        "UPDATE otdel.knowledge_runs SET status = 'completed', source_revision = 1 \
          WHERE material_id = $1",
        material,
        1,
    )
    .await;

    let status = refresh_status(&app, &client, partner).await;
    assert_eq!(status["sources"][0]["state"], "reread_after_draft");
    assert_eq!(status["sources"][0]["content_revision"], 2);
    assert_eq!(status["sources"][0]["drafted_revision"], 1);
    assert_eq!(status["state"], "revalidation_required");

    let reasons = status["reasons"].as_array().unwrap();
    let reread = reasons
        .iter()
        .find(|reason| reason["code"] == "source_reread")
        .expect("the re-read document must be named");
    assert_eq!(reread["material_id"], material.to_string());
    assert_eq!(reread["material_filename"], "catalogue.pdf");
    assert_eq!(reread["content_revision"], 2);
    assert_eq!(reread["drafted_revision"], 1);

    // The published version is untouched by any of this: its citations are copies.
    let current = published(&app, &client, partner).await;
    assert_eq!(current["number"], 1);
    assert_eq!(current["status"], "published");

    // And the reprocess is in the log as an owner action.
    let log = events(&app, &client, partner).await;
    assert!(kinds(&log).contains(&"material_reprocess_requested".to_owned()));

    app.cleanup().await;
}

/// UAT 6 — a document that is read but never drafted is named as the reason, not hidden
/// behind a generic "up to date".
#[tokio::test]
async fn a_document_that_was_never_drafted_is_named_in_the_refresh_status() {
    let app = start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    publish_first_version(&app, &client, partner).await;

    let second = upload_and_read(
        &app,
        &client,
        partner,
        "loads.pdf",
        &otdel_extract::fixtures::table_pdf(),
    )
    .await;

    let status = refresh_status(&app, &client, partner).await;
    assert_eq!(status["state"], "revalidation_required");
    let reason = status["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|reason| reason["code"] == "material_not_drafted")
        .expect("the undrafted document must be named")
        .clone();
    assert_eq!(reason["material_id"], second.to_string());
    assert_eq!(reason["material_filename"], "loads.pdf");

    // And the published version has not changed while that is true.
    assert_eq!(status["published"]["number"], 1);

    app.cleanup().await;
}

/// UAT 6а — a drafting run that failed is not reported as a draft, and the owner has a
/// button that does something about it.
///
/// The revision a draft was made from is recorded when the run **starts**, before the
/// model is called. Reading only that number made a failed run indistinguishable from a
/// successful one: the document showed as «разобран … фактов 0», the partner read as
/// `current`, and `POST /refresh` answered `up_to_date` — leaving no way forward.
#[tokio::test]
async fn a_drafting_run_that_failed_is_not_reported_as_a_draft() {
    let app = start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;

    let material = upload_and_read(
        &app,
        &client,
        partner,
        "catalogue.pdf",
        &otdel_extract::fixtures::text_pdf(),
    )
    .await;

    // The model is reachable and refuses. The run records the revision it started from,
    // then fails and stores nothing.
    let provider: Arc<FakeProvider> = Arc::new(FakeProvider::new(vec![FakeReply::Fail(
        otdel_llm::LlmError::InvalidResponse("модель вернула мусор".to_owned()),
    )]));
    let report = app.run_knowledge(&app.knowledge_worker(provider)).await;
    assert_eq!(report.facts_stored, 0);
    assert_eq!(report.jobs_failed, 1, "the run really failed: {report:?}");

    let status = refresh_status(&app, &client, partner).await;
    let source = &status["sources"][0];
    assert_eq!(source["material_id"], material.to_string());
    assert_eq!(
        source["state"], "not_drafted",
        "a run that failed is not a draft: {source}"
    );
    assert_eq!(source["draft_status"], "failed");
    assert!(
        source["message"]
            .as_str()
            .unwrap()
            .contains("нужно разобрать заново"),
        "{source}"
    );
    // Nothing has ever been published for this partner, so that is the state — and the
    // failed draft is named among the reasons rather than hidden behind "всё в порядке".
    assert_eq!(status["state"], "never_published");
    assert!(reason_codes(&status).contains(&"material_not_drafted".to_owned()));

    // And the refresh button reports the real obstacle — the unconfigured model on the
    // request path — rather than «ничего делать не нужно».
    let plan = post(
        &app,
        &client,
        &format!("/api/partners/{partner}/refresh"),
        json!({}),
    )
    .await
    .json();
    let understanding = plan["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["kind"] == "understanding")
        .expect("the drafting stage must be reported")
        .clone();
    assert_ne!(
        understanding["outcome"], "up_to_date",
        "a failed draft must not be reported as needing nothing: {understanding}"
    );
    assert_eq!(understanding["outcome"], "needs_provider");

    app.cleanup().await;
}

/// UAT 7 — asking for a refresh with no model configured says which stage cannot run and
/// why, instead of reporting work it did not start.
#[tokio::test]
async fn a_refresh_names_the_unconfigured_stage_rather_than_skipping_it() {
    let app = start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;

    // Read, and the API's model adapter is the unconfigured one.
    //
    // Extraction chains a draft by itself, so an `understand_material` job really is
    // queued for this document. That job cannot succeed without a key — it will record
    // `needs_provider` and store nothing — so the plan must name the missing
    // configuration rather than answer "уже в очереди", which would be true and useless.
    upload_and_read(
        &app,
        &client,
        partner,
        "catalogue.pdf",
        &otdel_extract::fixtures::text_pdf(),
    )
    .await;

    let response = post(
        &app,
        &client,
        &format!("/api/partners/{partner}/refresh"),
        json!({}),
    )
    .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text());
    let plan = response.json();

    let understanding = plan["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["kind"] == "understanding")
        .expect("the drafting stage must be reported")
        .clone();
    assert_eq!(understanding["outcome"], "needs_provider");
    assert!(
        understanding["message"]
            .as_str()
            .unwrap()
            .contains("OTDEL_LLM_API_KEY"),
        "the message must name what is missing: {}",
        understanding["message"]
    );

    // Nothing was queued, and the plan says so rather than implying progress.
    assert_eq!(plan["queued"], 0);
    assert!(plan["steps"]
        .as_array()
        .unwrap()
        .iter()
        .all(|step| step["outcome"] != "queued"));
    assert!(
        !plan["message"].as_str().unwrap().contains('%'),
        "no percentage is ever reported for work nobody measured"
    );

    app.cleanup().await;
}

/// UAT 8 — pressing refresh twice does not arm a second check.
#[tokio::test]
async fn a_repeated_refresh_joins_the_work_already_queued() {
    let app = start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    publish_first_version(&app, &client, partner).await;

    let first = post(
        &app,
        &client,
        &format!("/api/partners/{partner}/refresh"),
        json!({}),
    )
    .await;
    assert_eq!(first.status, StatusCode::OK);
    let validation = |plan: &Value| {
        plan["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|step| step["kind"] == "validation")
            .expect("the check is always reported")
            .clone()
    };
    assert_eq!(validation(&first.json())["outcome"], "queued");

    let second = post(
        &app,
        &client,
        &format!("/api/partners/{partner}/refresh"),
        json!({}),
    )
    .await;
    assert_eq!(validation(&second.json())["outcome"], "already_running");
    assert_eq!(second.json()["queued"], 0);

    // One queued check, not two.
    let jobs = get(&app, &client, &format!("/api/partners/{partner}/jobs")).await;
    let queued = jobs["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|job| job["kind"] == "validate_partner" && job["status"] == "queued")
        .count();
    assert_eq!(queued, 1);

    app.cleanup().await;
}

// --- the log ----------------------------------------------------------------------------------

/// UAT 9 — the history is append-only for every writer, including the schema owner.
#[tokio::test]
async fn the_event_log_cannot_be_rewritten_or_quietly_deleted() {
    let app = start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    publish_first_version(&app, &client, partner).await;

    let log = events(&app, &client, partner).await;
    assert!(!log.is_empty());
    let event_id = Uuid::parse_str(log[0]["id"].as_str().unwrap()).unwrap();

    // The runtime role has no UPDATE or DELETE grant at all, and the trigger refuses both
    // for the schema owner as well — which is what this asserts.
    let mut tx = app.admin_tx().await;
    let updated = sqlx::query("UPDATE otdel.events SET summary = 'подделка' WHERE id = $1")
        .bind(event_id)
        .execute(&mut *tx)
        .await;
    assert!(updated.is_err(), "an event must not be editable");
    tx.rollback().await.unwrap();

    let mut tx = app.admin_tx().await;
    let deleted = sqlx::query("DELETE FROM otdel.events WHERE id = $1")
        .bind(event_id)
        .execute(&mut *tx)
        .await;
    assert!(
        deleted.is_err(),
        "an event must only leave through the retention pass, which records that it ran"
    );
    tx.rollback().await.unwrap();

    // Still there, unchanged.
    let log = events(&app, &client, partner).await;
    assert!(log
        .iter()
        .all(|event| event["summary"].as_str() != Some("подделка")));

    app.cleanup().await;
}

// --- export -----------------------------------------------------------------------------------

/// UAT 10 — the export carries the snapshot, its provenance and its caveats, and refuses
/// what was never published.
#[tokio::test]
async fn the_export_is_a_published_snapshot_with_its_caveats() {
    let app = start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let (_, quote, value) = publish_first_version(&app, &client, partner).await;

    let version = published(&app, &client, partner).await;
    let version_id = version["id"].as_str().unwrap().to_owned();

    let document = get(
        &app,
        &client,
        &format!("/api/partners/{partner}/versions/{version_id}/export"),
    )
    .await;

    assert_eq!(document["manifest"]["schema"], "otdel.knowledge-version.v1");
    assert_eq!(document["manifest"]["version_number"], 1);
    assert_eq!(document["manifest"]["version_status"], "published");
    assert_eq!(document["manifest"]["partner_name"], "BASIS");
    assert_eq!(document["manifest"]["claims_source_supported"], 1);
    assert_eq!(
        document["manifest"]["input_fingerprint"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    // Phase 1F writes the candidate fingerprint, so a version published now carries one.
    assert!(document["manifest"]["candidate_fingerprint"].is_string());

    let disclosure = document["manifest"]["disclosure"].as_array().unwrap();
    assert!(disclosure
        .iter()
        .any(|line| line.as_str().unwrap().contains("не независимая проверка")));
    assert!(disclosure
        .iter()
        .any(|line| line.as_str().unwrap().contains("не разрешение на рассылку")));

    // The claims travel with their quotations, copied into the version.
    let claim = &document["claims"][0];
    assert_eq!(claim["value_text"], value);
    assert_eq!(claim["status"], "source_supported");
    assert_eq!(claim["evidence"][0]["quote"], quote);
    assert_eq!(claim["evidence"][0]["material_filename"], "catalogue.pdf");

    // Reading it is recorded.
    let log = events(&app, &client, partner).await;
    assert!(kinds(&log).contains(&"export_read".to_owned()));

    // A withdrawn version is refused with its reason.
    post(
        &app,
        &client,
        &format!("/api/partners/{partner}/versions/{version_id}/retract"),
        json!({ "reason": "выгрузка больше не должна её отдавать" }),
    )
    .await;
    let refused = app
        .send(client.get(&format!(
            "/api/partners/{partner}/versions/{version_id}/export"
        )))
        .await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.text());
    assert!(refused.text().contains("отозвана"));

    app.cleanup().await;
}

/// UAT 11 — a version that was never published cannot be exported, even by its owner.
#[tokio::test]
async fn a_version_that_was_never_published_is_not_exportable() {
    let app = start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material = upload_and_read(
        &app,
        &client,
        partner,
        "catalogue.pdf",
        &otdel_extract::fixtures::text_pdf(),
    )
    .await;
    let text = page_text(&app, &client, partner, material).await;
    let quote = quotable(&text);
    let value = value_from(&quote);

    // A draft whose citation no longer matches the page: nothing is supported, so the
    // check stores a `blocked` version instead of publishing one.
    draft_material(
        &app,
        draft("BP21", vec![fact("обозначение", &value, &quote)]),
    )
    .await;
    app.admin_update(
        "UPDATE otdel.material_pages SET text_content = 'Совершенно другой документ.' \
         WHERE material_id = $1",
        material,
        1,
    )
    .await;
    let checked = check(&app).await;
    assert_eq!(checked.versions_published, 0);
    assert_eq!(checked.versions_blocked, 1);

    let versions = get(&app, &client, &format!("/api/partners/{partner}/versions")).await;
    let blocked = versions["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|version| version["status"] == "blocked")
        .expect("a blocked version is stored so its reasons can be read")
        .clone();
    let blocked_id = blocked["id"].as_str().unwrap();
    assert!(!blocked["blocked_reasons"].as_array().unwrap().is_empty());

    let response = app
        .send(client.get(&format!(
            "/api/partners/{partner}/versions/{blocked_id}/export"
        )))
        .await;
    assert_eq!(
        response.status,
        StatusCode::NOT_FOUND,
        "an unchecked snapshot must not leave the building looking like a checked one"
    );

    app.cleanup().await;
}

// --- retention ---------------------------------------------------------------------------------

/// UAT 12 — with no policy configured nothing is pruned, and the API says so.
#[tokio::test]
async fn retention_keeps_everything_until_it_is_configured() {
    let app = start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    publish_first_version(&app, &client, partner).await;

    let policy = get(&app, &client, "/api/retention").await;
    assert_eq!(policy["state"], "keep_everything");
    assert!(policy["event_days"].is_null());
    assert!(policy["job_days"].is_null());
    assert_eq!(policy["preview"]["events_prunable"], 0);
    assert_eq!(policy["preview"]["jobs_prunable"], 0);
    assert!(policy["preview"]["events_total"].as_i64().unwrap() > 0);
    assert!(policy["message"].as_str().unwrap().contains("выключена"));
    assert!(policy["protected"]
        .as_array()
        .unwrap()
        .iter()
        .any(|line| line.as_str().unwrap().contains("Опубликованная версия")));

    app.cleanup().await;
}

/// UAT 13 — a sweep prunes old operational history, never a published version, and
/// records that it ran.
#[tokio::test]
async fn a_retention_sweep_prunes_old_history_and_never_a_published_version() {
    let app = TestApp::start_with_retention(7, Some(7)).await;
    let state = app
        .state
        .clone()
        .with_provider(unconfigured_llm())
        .with_embeddings(unconfigured_embeddings());
    let app = TestApp {
        router: otdel_api::app(state.clone()),
        state,
        ..app
    };
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    publish_first_version(&app, &client, partner).await;

    let version = published(&app, &client, partner).await;
    let version_id = Uuid::parse_str(version["id"].as_str().unwrap()).unwrap();
    let fresh_events = events(&app, &client, partner).await.len();
    assert!(fresh_events > 0);

    // One event and one finished job, far enough in the past to be prunable. Inserted
    // rather than aged, because `occurred_at` cannot be updated: the log is append-only.
    let mut tx = support::set_bureau_context(&app.admin_pool, app.bureau_id).await;
    sqlx::query(
        "INSERT INTO otdel.events (bureau_id, partner_id, kind, actor, summary, occurred_at) \
         VALUES ($1, $2, 'export_read', 'owner', 'старая выгрузка', now() - interval '60 days')",
    )
    .bind(app.bureau_id)
    .bind(partner)
    .execute(&mut *tx)
    .await
    .expect("insert an old event");
    sqlx::query(
        "UPDATE otdel.jobs SET status = 'completed', updated_at = now() - interval '60 days' \
          WHERE bureau_id = $1 AND partner_id = $2 AND kind = 'extract_document'",
    )
    .bind(app.bureau_id)
    .bind(partner)
    .execute(&mut *tx)
    .await
    .expect("age one finished job");
    tx.commit().await.unwrap();

    let policy = get(&app, &client, "/api/retention").await;
    assert_eq!(policy["state"], "enabled");
    assert_eq!(policy["event_days"], 7);
    assert_eq!(policy["preview"]["events_prunable"], 1);

    // Run the sweep the way the maintenance pass does.
    let horizons = app
        .state
        .config
        .retention
        .horizons()
        .expect("a configured policy has horizons");
    let mut tx = app.state.db.begin_scoped(app.bureau_id).await.unwrap();
    let outcome = otdel_db::updates::apply_retention(&mut tx, horizons)
        .await
        .expect("the sweep must run");
    tx.commit().await.unwrap();
    assert_eq!(outcome.events_removed, 1);

    // Everything recent survives, and so does everything published.
    let remaining = events(&app, &client, partner).await;
    assert_eq!(remaining.len(), fresh_events);
    assert!(remaining
        .iter()
        .all(|event| event["summary"].as_str() != Some("старая выгрузка")));

    let current = published(&app, &client, partner).await;
    assert_eq!(current["id"], version["id"]);
    assert_eq!(current["status"], "published");
    let mut tx = app.admin_tx().await;
    let claims: i64 =
        sqlx::query("SELECT count(*) AS n FROM otdel.version_claims WHERE version_id = $1")
            .bind(version_id)
            .fetch_one(&mut *tx)
            .await
            .unwrap()
            .try_get("n")
            .unwrap();
    tx.commit().await.unwrap();
    assert!(
        claims > 0,
        "retention must never touch a published snapshot"
    );

    app.cleanup().await;
}

/// UAT 13а — the maintenance pass runs the sweep, records it, and does not run it again
/// on the next tick.
///
/// Separate from UAT 13 on purpose: that one calls the database function, this one goes
/// through `Maintenance`, which is what the worker actually runs. Two things only exist
/// here — the pacing, and the sweep's own record, which has to be written **outside** the
/// deleting transaction because the trigger that permits a retention delete is
/// transaction-local.
#[tokio::test]
async fn the_maintenance_pass_sweeps_once_and_writes_its_own_record() {
    let app = TestApp::start_with_retention(7, Some(7)).await;
    let state = app
        .state
        .clone()
        .with_provider(unconfigured_llm())
        .with_embeddings(unconfigured_embeddings());
    let app = TestApp {
        router: otdel_api::app(state.clone()),
        state,
        ..app
    };
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    publish_first_version(&app, &client, partner).await;

    let mut tx = support::set_bureau_context(&app.admin_pool, app.bureau_id).await;
    sqlx::query(
        "INSERT INTO otdel.events (bureau_id, partner_id, kind, actor, summary, occurred_at) \
         VALUES ($1, $2, 'export_read', 'owner', 'старая выгрузка', now() - interval '60 days')",
    )
    .bind(app.bureau_id)
    .bind(partner)
    .execute(&mut *tx)
    .await
    .expect("insert an old event");
    tx.commit().await.unwrap();

    let maintenance = otdel_worker::Maintenance::new(
        Arc::clone(&app.state.config),
        app.state.db.clone(),
        Arc::clone(&app.state.store),
        otdel_worker::MaintenanceSettings::default(),
    );

    let report = maintenance.run_once().await.expect("maintenance pass");
    assert!(report.retention_ran, "{report:?}");
    assert_eq!(report.events_pruned, 1, "{report:?}");

    // The sweep recorded itself, and that record survived its own horizon.
    let mut tx = app.state.db.begin_scoped(app.bureau_id).await.unwrap();
    let sweep =
        otdel_db::events::latest_of_kind(&mut tx, otdel_core::updates::EventKind::RetentionApplied)
            .await
            .expect("read the sweep record");
    tx.commit().await.unwrap();
    let sweep = sweep.expect("a sweep that removed something records that it ran");
    assert_eq!(sweep.actor, otdel_core::updates::EventActor::System);
    assert_eq!(sweep.detail["events_removed"], 1);
    assert!(
        sweep.summary.contains("не затрагиваются"),
        "the record must say what it did not touch: {}",
        sweep.summary
    );

    // The next tick is inside the sweep interval, so it does not sweep again. "Did not
    // run" and "ran and removed nothing" are different answers and the report keeps them
    // apart.
    let again = maintenance.run_once().await.expect("second pass");
    assert!(!again.retention_ran, "{again:?}");
    assert_eq!(again.events_pruned, 0);

    app.cleanup().await;
}

/// UAT 14 — the database refuses a horizon that would delete what just happened, whatever
/// the caller asks for.
#[tokio::test]
async fn the_retention_floor_is_enforced_by_the_database() {
    let app = start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    publish_first_version(&app, &client, partner).await;

    for (events, jobs) in [(0, 0), (-1, 1), (7, 30)] {
        let mut tx = app.state.db.begin_scoped(app.bureau_id).await.unwrap();
        let result = sqlx::query("SELECT * FROM otdel.apply_retention($1, $2, $3, $4)")
            .bind(app.bureau_id)
            .bind(events)
            .bind(jobs)
            .bind(10_i32)
            .fetch_one(tx.conn())
            .await;
        assert!(
            result.is_err(),
            "apply_retention({events}, {jobs}) must be refused"
        );
        tx.rollback().await.ok();
    }

    // And a caller cannot prune another bureau by naming it.
    let (other_bureau, _) = support::new_bureau_as_admin(&app.admin_pool).await;
    let mut tx = app.state.db.begin_scoped(app.bureau_id).await.unwrap();
    let result = sqlx::query("SELECT * FROM otdel.apply_retention($1, 30, 30, 10)")
        .bind(other_bureau)
        .fetch_one(tx.conn())
        .await;
    assert!(
        result.is_err(),
        "retention is scoped to the caller's bureau"
    );
    tx.rollback().await.ok();

    support::delete_bureau_as_admin(&app.admin_pool, other_bureau).await;
    app.cleanup().await;
}

// --- isolation ----------------------------------------------------------------------------------

/// UAT 15 — none of the 1F surfaces reach another bureau or another partner.
#[tokio::test]
async fn the_update_endpoints_are_invisible_across_bureaus_and_partners() {
    let app = start().await;
    let client = app.sign_in().await;
    let mine = app.create_partner(&client, "BASIS").await;
    publish_first_version(&app, &client, mine).await;
    let my_version = published(&app, &client, mine).await["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // A second partner in the same bureau: row-level security separates bureaus only, so
    // partner scoping lives in the handlers and needs its own test.
    let neighbour = app.create_partner(&client, "Сосед").await;
    // And a partner of another bureau entirely.
    let (other_bureau, _) = support::new_bureau_as_admin(&app.admin_pool).await;
    let theirs = support::insert_partner_as_admin(&app.admin_pool, other_bureau, "Чужой").await;

    for partner in [neighbour, theirs] {
        for uri in [
            format!("/api/partners/{partner}/versions/{my_version}/changes"),
            format!("/api/partners/{partner}/versions/{my_version}/export"),
        ] {
            let response = app.send(client.get(&uri)).await;
            assert_eq!(response.status, StatusCode::NOT_FOUND, "{uri}");
        }
    }

    // The foreign bureau's partner is not even addressable.
    for uri in [
        format!("/api/partners/{theirs}/refresh"),
        format!("/api/partners/{theirs}/events"),
    ] {
        let response = app.send(client.get(&uri)).await;
        assert_eq!(response.status, StatusCode::NOT_FOUND, "{uri}");
    }
    let response = post(
        &app,
        &client,
        &format!("/api/partners/{theirs}/refresh"),
        json!({}),
    )
    .await;
    assert_eq!(response.status, StatusCode::NOT_FOUND);

    // The neighbour exists, and its history is its own: none of the first partner's
    // events appear in it.
    let neighbour_events = events(&app, &client, neighbour).await;
    assert!(
        neighbour_events.is_empty(),
        "a partner's history must contain only its own events: {neighbour_events:?}"
    );
    let status = refresh_status(&app, &client, neighbour).await;
    assert_eq!(status["state"], "never_published");
    assert!(status["sources"].as_array().unwrap().is_empty());

    // A material of the first partner cannot be reprocessed through the second.
    let materials = get(&app, &client, &format!("/api/partners/{mine}/materials")).await;
    let material = materials["items"][0]["id"].as_str().unwrap();
    let response = post(
        &app,
        &client,
        &format!("/api/partners/{neighbour}/materials/{material}/reprocess"),
        json!({}),
    )
    .await;
    assert_eq!(response.status, StatusCode::NOT_FOUND);

    support::delete_bureau_as_admin(&app.admin_pool, other_bureau).await;
    app.cleanup().await;
}

/// UAT 16 — the mutating 1F endpoints are behind the session and the CSRF token, like
/// every other one.
#[tokio::test]
async fn the_update_endpoints_require_a_session_and_a_csrf_token() {
    let app = start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;

    // No session at all.
    let anonymous = app
        .send(
            axum::http::Request::builder()
                .method(Method::GET)
                .uri(format!("/api/partners/{partner}/refresh"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

    // A session without the CSRF header on a mutating request.
    let no_token = app
        .send(
            axum::http::Request::builder()
                .method(Method::POST)
                .uri(format!("/api/partners/{partner}/refresh"))
                .header(axum::http::header::COOKIE, &client.cookie)
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from("{}"))
                .unwrap(),
        )
        .await;
    assert_eq!(no_token.status, StatusCode::FORBIDDEN);

    app.cleanup().await;
}

/// UAT 17 — a document is read again only when that makes sense, and the refusal says why.
#[tokio::test]
async fn reprocessing_is_refused_while_a_document_is_still_being_read() {
    let app = start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;

    // Uploaded and queued, not read yet.
    let response = app
        .send(client.upload_request(
            &format!("/api/partners/{partner}/materials"),
            "catalogue.pdf",
            Some("application/pdf"),
            &otdel_extract::fixtures::text_pdf(),
        ))
        .await;
    let material = Uuid::parse_str(response.json()["id"].as_str().unwrap()).unwrap();

    let refused = post(
        &app,
        &client,
        &format!("/api/partners/{partner}/materials/{material}/reprocess"),
        json!({}),
    )
    .await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.text());
    assert!(refused.text().contains("очереди"), "{}", refused.text());

    // Once it has been read, the same request is accepted.
    app.run_worker(&app.extractor()).await;
    let accepted = post(
        &app,
        &client,
        &format!("/api/partners/{partner}/materials/{material}/reprocess"),
        json!({}),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.text());
    assert_eq!(accepted.json()["status"], "queued");

    app.cleanup().await;
}

/// UAT 18 — a repeated upload of the same bytes starts no second cycle, and the history
/// says why nothing happened.
#[tokio::test]
async fn re_uploading_identical_bytes_does_not_start_a_second_cycle() {
    let app = start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    publish_first_version(&app, &client, partner).await;

    let first = published(&app, &client, partner).await;

    let repeat = app
        .send(client.upload_request(
            &format!("/api/partners/{partner}/materials"),
            "catalogue.pdf",
            Some("application/pdf"),
            &otdel_extract::fixtures::text_pdf(),
        ))
        .await;
    assert_eq!(repeat.status, StatusCode::OK, "a duplicate is 200, not 201");

    let materials = get(&app, &client, &format!("/api/partners/{partner}/materials")).await;
    assert_eq!(
        materials["items"].as_array().unwrap().len(),
        1,
        "deduplication is by content within one partner"
    );

    // Checking again produces no second published version: the input is identical.
    post(
        &app,
        &client,
        &format!("/api/partners/{partner}/validate"),
        json!({}),
    )
    .await;
    let checked = check(&app).await;
    assert_eq!(checked.versions_published, 0);

    let current = published(&app, &client, partner).await;
    assert_eq!(current["id"], first["id"]);
    assert_eq!(current["number"], 1);

    let log = events(&app, &client, partner).await;
    let duplicate = log
        .iter()
        .find(|event| event["kind"] == "material_duplicate")
        .expect("the repeated upload is recorded rather than looking like nothing");
    assert!(duplicate["summary"].as_str().unwrap().contains("уже есть"));

    // And the refresh status agrees that there is nothing to redo.
    let status = refresh_status(&app, &client, partner).await;
    assert_eq!(status["state"], "current");

    app.cleanup().await;
}
