//! Phase 1C end to end: read pages → product role → stored draft → API.
//!
//! Everything is real except the model: the HTTP API, PostgreSQL under row-level
//! security, the object store, the extraction worker and the understanding worker all
//! run as they do in the pilot. The model is a scripted provider
//! ([`otdel_llm::fake::FakeProvider`]), which is what lets these tests state the cases
//! that matter — a model that cites a source outside the material, one that invents a
//! quote, one that is not configured at all — without a key and without a network.
//!
//! The quotes the fake "model" returns are taken from the page text the extractor
//! really produced, so a test never asserts against a hand-written expectation of what
//! the parser should have said.

mod support;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use otdel_llm::fake::{FakeProvider, FakeReply};
use otdel_llm::{LlmError, LlmProvider, UnconfiguredProvider};
use serde_json::{json, Value};
use support::{TestApp, TestClient};
use uuid::Uuid;

// --- helpers --------------------------------------------------------------------------

async fn upload_and_read(app: &TestApp, client: &TestClient, partner: Uuid) -> Uuid {
    let response = app
        .send(client.upload_request(
            &format!("/api/partners/{partner}/materials"),
            "catalogue.pdf",
            Some("application/pdf"),
            &otdel_extract::fixtures::text_pdf(),
        ))
        .await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.text());
    let material_id = Uuid::parse_str(response.json()["id"].as_str().unwrap()).unwrap();

    let report = app.run_worker(&app.extractor()).await;
    assert_eq!(report.pages_read, 1, "the fixture has one readable page");
    material_id
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

/// The text the extractor really stored for page 1.
async fn page_text(app: &TestApp, client: &TestClient, partner: Uuid, material: Uuid) -> String {
    let detail = get(
        app,
        client,
        &format!("/api/partners/{partner}/materials/{material}/pages/1"),
    )
    .await;
    detail["text"].as_str().expect("page text").to_owned()
}

/// A fragment that really is on the page: the first line long enough to quote.
fn quotable(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| line.chars().count() >= 12)
        .expect("the fixture page has a quotable line")
        .to_owned()
}

/// A word that really appears in `quote` — a value the validator can confirm.
///
/// Derived from the quotation rather than written by hand: the server now requires a
/// fact's value to be in the fragment it cites, and a fixture that ignored that would
/// be testing a path the product no longer has.
fn value_from(quote: &str) -> String {
    quote
        .split_whitespace()
        .find(|word| word.chars().count() >= 4)
        .expect("the quotable line has a word to use as a value")
        .to_owned()
}

/// A complete, well-formed answer citing `S1` with `quote`.
fn answer(quote: &str) -> Value {
    json!({
        "categories": [{
            "ref": "c1", "kind": "direction",
            "name": "Монтажные системы", "summary": null,
        }],
        "products": [{
            "ref": "p1", "category_ref": "c1", "kind": "product",
            "name": "BP21", "summary": "профиль монтажный",
        }],
        "facts": [{
            "product_ref": "p1", "kind": "characteristic",
            "attribute": "обозначение", "value": value_from(quote),
            "unit": null, "conditions": null,
            "model_context": "формулировка модели, не цитата",
            "evidence": [{"source": "S1", "quote": quote}],
        }],
        "glossary": [{
            // The term has to occur in the fragment cited for it — a citation under a
            // term exists to show the term being used — so it is taken from the quote.
            "term": value_from(quote), "definition": "несущий элемент системы",
            "definition_from_source": false,
            "evidence": [{"source": "S1", "quote": quote}],
        }],
        "qa": [{
            "question": "Что описывает каталог?", "answer": "Профили и консоли.",
            "evidence": [{"source": "S1", "quote": quote}],
        }],
        "gaps": [{
            "product_ref": "p1", "topic": "price",
            "missing": "цена не указана в материале",
            "blocks": "коммерческое предложение",
            "question": "Какая отпускная цена профиля BP21?",
            "audience": "partner",
        }],
    })
}

/// The stored quotation of a fact, as the API returns it.
fn stored_quote_of(fact: &Value) -> String {
    fact["evidence"][0]["quote"].as_str().unwrap().to_owned()
}

fn scripted(value: Value) -> Arc<FakeProvider> {
    Arc::new(FakeProvider::new(vec![FakeReply::Json(value)]))
}

// --- the happy path ---------------------------------------------------------------------

#[tokio::test]
async fn a_read_material_becomes_products_facts_terms_and_gaps_with_exact_citations() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload_and_read(&app, &client, partner).await;

    let text = page_text(&app, &client, partner, material_id).await;
    let quote = quotable(&text);

    // Reading a material queues its understanding automatically: the owner does not
    // have to press anything for the normal path (`docs/block-01-spec.md` §11).
    let jobs = get(&app, &client, &format!("/api/partners/{partner}/jobs")).await;
    let understanding: Vec<&Value> = jobs["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|job| job["kind"] == "understand_material")
        .collect();
    assert_eq!(understanding.len(), 1, "{jobs}");
    assert_eq!(understanding[0]["status"], "queued");

    let provider = scripted(answer(&quote));
    let report = app
        .run_knowledge(&app.knowledge_worker(provider.clone()))
        .await;
    assert_eq!(report.jobs_claimed, 1);
    assert_eq!(report.jobs_completed, 1);
    assert_eq!(report.facts_stored, 1);
    assert_eq!(provider.call_count(), 1, "one bounded call for one page");

    // The prompt shows the model the page text under a label, never an identifier.
    let prompt = &provider.prompts()[0];
    assert!(prompt.contains("S1"));
    assert!(!prompt.contains(&material_id.to_string()));
    assert!(!prompt.contains(&partner.to_string()));

    let overview = get(&app, &client, &format!("/api/partners/{partner}/knowledge")).await;
    assert_eq!(overview["summary"]["products_total"], 1);
    assert_eq!(overview["summary"]["facts_total"], 1);
    assert_eq!(overview["summary"]["terms_total"], 1);
    assert_eq!(overview["summary"]["gaps_total"], 1);
    assert_eq!(overview["summary"]["questions_total"], 1);
    assert_eq!(overview["summary"]["materials_readable"], 1);
    assert_eq!(overview["summary"]["materials_understood"], 1);
    let run = &overview["runs"][0];
    assert_eq!(run["status"], "completed");
    assert_eq!(run["material_id"], material_id.to_string());
    assert_eq!(run["facts_accepted"], 1);
    assert_eq!(run["facts_rejected"], 0);
    assert_eq!(run["requests_made"], 1);
    assert_eq!(run["provider"], "fake");
    assert_eq!(run["model"], "fake/model-1");
    assert!(run["prompt_profile"]
        .as_str()
        .unwrap()
        .contains("productologist"));

    // The product, its fact, and the evidence the owner can check.
    let products = get(
        &app,
        &client,
        &format!("/api/partners/{partner}/knowledge/products"),
    )
    .await;
    let node = &products["items"][0];
    assert_eq!(node["product"]["name"], "BP21");
    assert_eq!(node["category"]["name"], "Монтажные системы");

    let fact = &node["facts"][0];
    assert_eq!(fact["attribute"], "обозначение");
    // The value is stored as written, and it really is in the quotation below it.
    assert_eq!(fact["value_text"], value_from(&quote));
    assert!(
        stored_quote_of(fact).contains(fact["value_text"].as_str().unwrap()),
        "the value must appear in the quotation: {fact}"
    );
    assert_eq!(fact["status"], "candidate", "1C never claims verification");
    assert_eq!(
        fact["model_context"], "формулировка модели, не цитата",
        "the model's own words stay in their own field"
    );

    let evidence = &fact["evidence"][0];
    assert_eq!(evidence["page_number"], 1);
    assert_eq!(evidence["material_id"], material_id.to_string());
    assert_eq!(evidence["material_filename"], "catalogue.pdf");
    // The stored quote is the page's own wording, and the offsets locate it there.
    let stored_quote = evidence["quote"].as_str().unwrap();
    assert!(
        text.contains(stored_quote),
        "quote must be on the page: {stored_quote:?}"
    );
    let start = evidence["char_start"].as_i64().unwrap() as usize;
    let end = evidence["char_end"].as_i64().unwrap() as usize;
    let by_offset: String = text.chars().skip(start).take(end - start).collect();
    assert_eq!(by_offset, stored_quote, "offsets must point at the quote");

    // The glossary marks a definition the model wrote as the model's, not the source's.
    let glossary = get(
        &app,
        &client,
        &format!("/api/partners/{partner}/knowledge/glossary"),
    )
    .await;
    // The term is the one the citation actually contains.
    assert_eq!(glossary["items"][0]["term"], value_from(&quote));
    assert!(
        stored_quote_of(&glossary["items"][0]).contains(value_from(&quote).as_str()),
        "a term is cited with a fragment that uses it"
    );
    assert_eq!(glossary["items"][0]["definition_is_model_context"], true);
    assert!(!glossary["items"][0]["evidence"]
        .as_array()
        .unwrap()
        .is_empty());

    // A missing price is a gap with a prepared question — never an invented number.
    let gaps = get(
        &app,
        &client,
        &format!("/api/partners/{partner}/knowledge/gaps"),
    )
    .await;
    let gap = &gaps["items"][0];
    assert_eq!(gap["topic"], "price");
    assert_eq!(gap["product_name"], "BP21");
    assert_eq!(gap["question"]["audience"], "partner");
    assert_eq!(
        gap["question"]["status"], "prepared",
        "nothing is sent in this phase"
    );

    let qa = get(
        &app,
        &client,
        &format!("/api/partners/{partner}/knowledge/qa"),
    )
    .await;
    assert_eq!(qa["items"].as_array().unwrap().len(), 1);
    assert!(!qa["items"][0]["evidence"].as_array().unwrap().is_empty());

    app.cleanup().await;
}

#[tokio::test]
async fn facts_can_be_filtered_by_product_and_a_bad_filter_stays_in_the_error_envelope() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload_and_read(&app, &client, partner).await;
    let text = page_text(&app, &client, partner, material_id).await;
    app.run_knowledge(&app.knowledge_worker(scripted(answer(&quotable(&text)))))
        .await;

    let products = get(
        &app,
        &client,
        &format!("/api/partners/{partner}/knowledge/products"),
    )
    .await;
    let product_id = products["items"][0]["product"]["id"].as_str().unwrap();

    let filtered = get(
        &app,
        &client,
        &format!("/api/partners/{partner}/knowledge/products?product_id={product_id}"),
    )
    .await;
    assert_eq!(filtered["items"][0]["facts"].as_array().unwrap().len(), 1);

    // A filter naming a product of nobody returns an empty node, not somebody else's.
    let empty = get(
        &app,
        &client,
        &format!(
            "/api/partners/{partner}/knowledge/products?product_id={}",
            Uuid::new_v4()
        ),
    )
    .await;
    assert!(empty["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|node| node["facts"].as_array().unwrap().is_empty()));

    // Malformed and unknown parameters answer in the contract's envelope, not in
    // axum's plain text.
    for query in ["product_id=not-a-uuid", "verdict=published"] {
        let response = app
            .send(client.get(&format!(
                "/api/partners/{partner}/knowledge/products?{query}"
            )))
            .await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY, "{query}");
        assert_eq!(response.error_code(), "validation_failed");
    }

    app.cleanup().await;
}

#[tokio::test]
async fn a_material_read_before_this_phase_can_still_be_drafted() {
    let app = TestApp::start_with_provider(scripted(json!({}))).await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload_and_read(&app, &client, partner).await;

    // The state of a material read by an earlier version: pages, no run row.
    app.admin_update(
        "DELETE FROM otdel.knowledge_runs WHERE material_id = $1",
        material_id,
        1,
    )
    .await;
    app.admin_update(
        "DELETE FROM otdel.jobs WHERE material_id = $1 AND kind = 'understand_material'",
        material_id,
        1,
    )
    .await;

    // The overview offers it explicitly instead of leaving it invisible.
    let overview = get(&app, &client, &format!("/api/partners/{partner}/knowledge")).await;
    assert!(overview["runs"].as_array().unwrap().is_empty());
    let pending = &overview["pending_materials"][0];
    assert_eq!(pending["material_id"], material_id.to_string());
    assert_eq!(pending["filename"], "catalogue.pdf");
    assert_eq!(pending["pages_with_text"], 1);

    // And drafting it works from there.
    let response = app
        .send(client.json_request(
            Method::POST,
            &format!("/api/partners/{partner}/materials/{material_id}/understand"),
            json!(null),
        ))
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text());
    assert_eq!(response.json()["status"], "queued");

    let after = get(&app, &client, &format!("/api/partners/{partner}/knowledge")).await;
    assert!(
        after["pending_materials"].as_array().unwrap().is_empty(),
        "once queued it is a run, not a pending material"
    );
    assert_eq!(after["runs"][0]["material_id"], material_id.to_string());

    app.cleanup().await;
}

#[tokio::test]
async fn a_run_left_running_by_a_dead_worker_is_settled_by_maintenance() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload_and_read(&app, &client, partner).await;

    // A worker that started and never came back: the run says `running`, and its job
    // has been given up on.
    app.admin_update(
        "UPDATE otdel.knowledge_runs SET status = 'running' WHERE material_id = $1",
        material_id,
        1,
    )
    .await;
    app.admin_update(
        "UPDATE otdel.jobs SET status = 'failed', lease_owner = NULL, \
         lease_expires_at = NULL WHERE material_id = $1 AND kind = 'understand_material'",
        material_id,
        1,
    )
    .await;

    let before = get(&app, &client, &format!("/api/partners/{partner}/knowledge")).await;
    assert_eq!(before["runs"][0]["status"], "running");

    let mut tx = app.state.db.begin_scoped(app.bureau_id).await.unwrap();
    let settled = otdel_db::knowledge::reclaim_stalled_runs(&mut tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(settled, 1);

    let after = get(&app, &client, &format!("/api/partners/{partner}/knowledge")).await;
    assert_eq!(
        after["runs"][0]["status"], "failed",
        "a run cannot stay `running` with nothing behind it"
    );
    assert!(after["runs"][0]["diagnostic"]
        .as_str()
        .unwrap()
        .contains("прерван"));

    app.cleanup().await;
}

// --- refusals ---------------------------------------------------------------------------

#[tokio::test]
async fn a_fact_citing_a_source_outside_the_material_is_refused_and_stored_nowhere() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload_and_read(&app, &client, partner).await;
    let text = page_text(&app, &client, partner, material_id).await;

    let mut spoofed = answer(&quotable(&text));
    spoofed["facts"][0]["evidence"][0]["source"] = json!("S404");
    spoofed["glossary"] = json!([]);
    spoofed["qa"] = json!([]);

    let report = app
        .run_knowledge(&app.knowledge_worker(scripted(spoofed)))
        .await;
    assert_eq!(report.jobs_completed, 1);
    assert_eq!(report.facts_stored, 0, "a spoofed source stores nothing");
    assert!(report.candidates_rejected >= 1);

    let overview = get(&app, &client, &format!("/api/partners/{partner}/knowledge")).await;
    assert_eq!(overview["summary"]["facts_total"], 0);
    let run = &overview["runs"][0];
    assert_eq!(run["status"], "partial", "a refusal is not a clean run");
    assert_eq!(run["facts_rejected"], 1);
    let reasons = run["rejections"].as_array().unwrap();
    assert!(
        reasons
            .iter()
            .any(|reason| reason.as_str().unwrap().contains("S404")),
        "the reason must name the source that does not exist: {reasons:?}"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn an_invented_quote_is_refused_even_when_the_source_label_is_real() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    upload_and_read(&app, &client, partner).await;

    let invented = answer("Профиль BP21 выдерживает 10 кН при любой схеме опирания");
    let report = app
        .run_knowledge(&app.knowledge_worker(scripted(invented)))
        .await;
    assert_eq!(report.facts_stored, 0);

    let overview = get(&app, &client, &format!("/api/partners/{partner}/knowledge")).await;
    assert_eq!(overview["summary"]["facts_total"], 0);
    assert_eq!(overview["summary"]["terms_total"], 0);
    assert_eq!(overview["summary"]["qa_total"], 0);
    // The product itself was named in the answer and is kept; the unsupported claims
    // about it are not.
    assert_eq!(overview["summary"]["products_total"], 1);
    let reasons = overview["runs"][0]["rejections"].as_array().unwrap();
    assert!(reasons
        .iter()
        .any(|reason| reason.as_str().unwrap().contains("дословно")));

    app.cleanup().await;
}

#[tokio::test]
async fn a_response_that_ignores_the_schema_stores_nothing_and_says_so() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    upload_and_read(&app, &client, partner).await;

    let provider = scripted(json!({"answer": "Каталог описывает профили и консоли."}));
    let report = app.run_knowledge(&app.knowledge_worker(provider)).await;
    assert_eq!(report.facts_stored, 0);

    let overview = get(&app, &client, &format!("/api/partners/{partner}/knowledge")).await;
    assert_eq!(overview["summary"]["facts_total"], 0);
    assert_eq!(overview["runs"][0]["status"], "partial");
    assert!(overview["runs"][0]["rejections"][0]
        .as_str()
        .unwrap()
        .contains("схеме"));

    app.cleanup().await;
}

// --- no key ------------------------------------------------------------------------------

#[tokio::test]
async fn without_a_configured_model_nothing_is_called_and_nothing_is_stored() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload_and_read(&app, &client, partner).await;

    // The interface is told plainly what is missing.
    let provider_state = get(&app, &client, "/api/knowledge/provider").await;
    assert_eq!(provider_state["state"], "needs_configuration");
    assert_eq!(provider_state["provider"], "openrouter");
    let missing: Vec<&str> = provider_state["missing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    assert!(missing.contains(&"OTDEL_LLM_API_KEY"), "{missing:?}");
    assert!(provider_state["message"]
        .as_str()
        .unwrap()
        .contains("OTDEL_LLM_API_KEY"));
    // No response ever carries a key, not even an empty field for one.
    assert!(provider_state.get("api_key").is_none());

    // Asking for a draft is refused with that reason rather than queued to fail later.
    let response = app
        .send(client.json_request(
            Method::POST,
            &format!("/api/partners/{partner}/materials/{material_id}/understand"),
            json!(null),
        ))
        .await;
    assert_eq!(response.status, StatusCode::CONFLICT, "{}", response.text());
    let error = response.json();
    assert_eq!(error["error"]["code"], "conflict");
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("OTDEL_LLM_API_KEY"));

    // And the worker, given the same unconfigured adapter, records the state on the
    // run instead of inventing a draft.
    let unconfigured: Arc<dyn LlmProvider> =
        Arc::new(UnconfiguredProvider::new(&app.state.config.llm));
    let report = app.run_knowledge(&app.knowledge_worker(unconfigured)).await;
    assert_eq!(report.jobs_claimed, 1);
    assert_eq!(report.jobs_failed, 1);
    assert_eq!(report.runs_awaiting_provider, 1);
    assert_eq!(report.facts_stored, 0);

    let overview = get(&app, &client, &format!("/api/partners/{partner}/knowledge")).await;
    assert_eq!(overview["runs"][0]["status"], "needs_provider");
    assert_eq!(overview["summary"]["facts_total"], 0);
    assert_eq!(overview["summary"]["products_total"], 0);

    // The extraction half is unaffected: the material is still read.
    let material = get(
        &app,
        &client,
        &format!("/api/partners/{partner}/materials/{material_id}"),
    )
    .await;
    assert_eq!(material["status"], "completed");

    app.cleanup().await;
}

#[tokio::test]
async fn a_model_failure_is_recorded_and_leaves_the_previous_draft_alone() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload_and_read(&app, &client, partner).await;
    let text = page_text(&app, &client, partner, material_id).await;

    // First run succeeds.
    app.run_knowledge(&app.knowledge_worker(scripted(answer(&quotable(&text)))))
        .await;
    let before = get(&app, &client, &format!("/api/partners/{partner}/knowledge")).await;
    assert_eq!(before["summary"]["facts_total"], 1);

    // Second run is asked for and the provider fails.
    let response = app
        .send(client.json_request(
            Method::POST,
            &format!("/api/partners/{partner}/materials/{material_id}/understand"),
            json!(null),
        ))
        .await;
    assert_eq!(
        response.status,
        StatusCode::CONFLICT,
        "no key configured yet"
    );

    let failing: Arc<dyn LlmProvider> = Arc::new(FakeProvider::failing(LlmError::RateLimited));
    // Re-arm the job directly: the API refuses while no key is configured, and this
    // test is about what the worker does when a call fails, not about the button.
    app.admin_update(
        "UPDATE otdel.jobs SET status = 'queued', lease_owner = NULL, \
         lease_expires_at = NULL, run_after = now() \
         WHERE material_id = $1 AND kind = 'understand_material'",
        material_id,
        1,
    )
    .await;

    let report = app.run_knowledge(&app.knowledge_worker(failing)).await;
    assert_eq!(report.jobs_failed, 1);

    let after = get(&app, &client, &format!("/api/partners/{partner}/knowledge")).await;
    assert_eq!(
        after["summary"]["facts_total"], 1,
        "a failed re-run must not delete the draft that was already there"
    );
    assert_eq!(after["runs"][0]["status"], "failed");
    assert!(after["runs"][0]["diagnostic"].is_string());

    app.cleanup().await;
}

// --- idempotency ---------------------------------------------------------------------------

#[tokio::test]
async fn drafting_a_material_twice_replaces_the_draft_instead_of_duplicating_it() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload_and_read(&app, &client, partner).await;
    let text = page_text(&app, &client, partner, material_id).await;
    let quote = quotable(&text);

    app.run_knowledge(&app.knowledge_worker(scripted(answer(&quote))))
        .await;
    let first = get(&app, &client, &format!("/api/partners/{partner}/knowledge")).await;
    assert_eq!(first["summary"]["facts_total"], 1);
    assert_eq!(first["summary"]["products_total"], 1);

    // Queue it again through the same path the extraction worker uses, then run.
    app.admin_update(
        "UPDATE otdel.jobs SET status = 'queued', lease_owner = NULL, \
         lease_expires_at = NULL, run_after = now() \
         WHERE material_id = $1 AND kind = 'understand_material'",
        material_id,
        1,
    )
    .await;
    app.run_knowledge(&app.knowledge_worker(scripted(answer(&quote))))
        .await;

    let second = get(&app, &client, &format!("/api/partners/{partner}/knowledge")).await;
    assert_eq!(second["summary"]["facts_total"], 1, "no duplicate fact");
    assert_eq!(
        second["summary"]["products_total"], 1,
        "no duplicate product"
    );
    assert_eq!(second["summary"]["gaps_total"], 1);
    assert_eq!(second["summary"]["terms_total"], 1);
    assert_eq!(
        second["runs"].as_array().unwrap().len(),
        1,
        "one material has one run record"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn asking_for_a_draft_twice_reuses_one_queue_row() {
    let app = TestApp::start_with_provider(scripted(json!({}))).await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload_and_read(&app, &client, partner).await;

    let uri = format!("/api/partners/{partner}/materials/{material_id}/understand");
    for _ in 0..3 {
        let response = app
            .send(client.json_request(Method::POST, &uri, json!(null)))
            .await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.text());
        assert_eq!(response.json()["status"], "queued");
    }

    let jobs = get(&app, &client, &format!("/api/partners/{partner}/jobs")).await;
    let understanding = jobs["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|job| job["kind"] == "understand_material")
        .count();
    assert_eq!(understanding, 1, "pressing the button never queues twice");

    app.cleanup().await;
}

#[tokio::test]
async fn a_running_draft_is_never_re_armed_under_the_worker_that_holds_it() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload_and_read(&app, &client, partner).await;

    // A worker claims the understanding job the extraction queued.
    let mut tx = app.state.db.begin_scoped(app.bureau_id).await.unwrap();
    let claimed = otdel_db::jobs::claim_next(
        &mut tx,
        "worker-a",
        std::time::Duration::from_secs(120),
        &otdel_core::model::JobKind::knowledge_kinds(),
    )
    .await
    .unwrap()
    .expect("the understanding job is queued");
    tx.commit().await.unwrap();
    assert_eq!(claimed.kind, otdel_core::model::JobKind::UnderstandMaterial);

    // Finishing another page re-read now must NOT put the running job back into the
    // queue: a second worker would then draft the same material at the same time.
    let mut tx = app.state.db.begin_scoped(app.bureau_id).await.unwrap();
    let again = otdel_db::jobs::enqueue_understanding(&mut tx, partner, material_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(
        again.id, claimed.id,
        "one material has one understanding job"
    );
    assert_eq!(
        again.status,
        otdel_core::model::JobStatus::Running,
        "a running job stays running instead of being handed out twice"
    );

    // And nothing else can claim it.
    let mut tx = app.state.db.begin_scoped(app.bureau_id).await.unwrap();
    let stolen = otdel_db::jobs::claim_next(
        &mut tx,
        "worker-b",
        std::time::Duration::from_secs(120),
        &otdel_core::model::JobKind::knowledge_kinds(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(stolen.is_none(), "{stolen:?}");

    app.cleanup().await;
}

/// A provider that takes the job's lease away while it "thinks" — the shape of a slow
/// model call whose lease expired and was reclaimed by the maintenance pass.
struct LeaseStealingProvider {
    pool: sqlx::PgPool,
    bureau_id: Uuid,
    answer: Value,
}

#[async_trait::async_trait]
impl LlmProvider for LeaseStealingProvider {
    fn describe(&self) -> otdel_llm::ProviderDescription {
        otdel_llm::ProviderDescription {
            provider: "fake".to_owned(),
            model: "fake/model-1".to_owned(),
            endpoint_host: None,
            state: "ready",
            missing: Vec::new(),
            message: "тестовый провайдер".to_owned(),
        }
    }

    async fn complete_json(
        &self,
        _request: &otdel_llm::LlmRequest,
    ) -> Result<otdel_llm::LlmResponse, otdel_llm::LlmError> {
        let mut tx = support::set_bureau_context(&self.pool, self.bureau_id).await;
        sqlx::query(
            "UPDATE otdel.jobs SET lease_owner = 'another-worker', \
                    lease_expires_at = now() + interval '2 minutes' \
              WHERE kind = 'understand_material' AND status = 'running'",
        )
        .execute(&mut *tx)
        .await
        .expect("steal the lease");
        tx.commit().await.expect("commit the theft");

        Ok(otdel_llm::LlmResponse {
            json: self.answer.clone(),
            model: "fake/model-1".to_owned(),
            usage: otdel_llm::Usage::default(),
            duration: std::time::Duration::from_millis(1),
            response_chars: 0,
        })
    }
}

#[tokio::test]
async fn a_run_that_lost_its_lease_writes_nothing() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload_and_read(&app, &client, partner).await;
    let text = page_text(&app, &client, partner, material_id).await;

    let thief: Arc<dyn LlmProvider> = Arc::new(LeaseStealingProvider {
        pool: app.admin_pool.clone(),
        bureau_id: app.bureau_id,
        answer: answer(&quotable(&text)),
    });

    let report = app.run_knowledge(&app.knowledge_worker(thief)).await;
    assert_eq!(report.jobs_claimed, 1);
    assert_eq!(report.jobs_failed, 1);
    assert_eq!(report.facts_stored, 0);

    // The draft belongs to whoever holds the job now — this run stored nothing.
    let overview = get(&app, &client, &format!("/api/partners/{partner}/knowledge")).await;
    assert_eq!(overview["summary"]["facts_total"], 0);
    assert_eq!(overview["summary"]["products_total"], 0);

    app.cleanup().await;
}

#[tokio::test]
async fn a_material_that_was_not_read_cannot_be_drafted() {
    let app = TestApp::start_with_provider(scripted(json!({}))).await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;

    let response = app
        .send(client.upload_request(
            &format!("/api/partners/{partner}/materials"),
            "broken.pdf",
            Some("application/pdf"),
            &otdel_extract::fixtures::not_a_pdf(),
        ))
        .await;
    assert_eq!(response.status, StatusCode::CREATED);
    let material_id = Uuid::parse_str(response.json()["id"].as_str().unwrap()).unwrap();
    app.run_worker(&app.extractor()).await;

    let response = app
        .send(client.json_request(
            Method::POST,
            &format!("/api/partners/{partner}/materials/{material_id}/understand"),
            json!(null),
        ))
        .await;
    assert_eq!(response.status, StatusCode::CONFLICT, "{}", response.text());
    assert!(response.json()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("не прочитан"));

    app.cleanup().await;
}

// --- access -------------------------------------------------------------------------------

#[tokio::test]
async fn the_draft_of_another_bureau_is_not_reachable() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload_and_read(&app, &client, partner).await;
    let text = page_text(&app, &client, partner, material_id).await;
    app.run_knowledge(&app.knowledge_worker(scripted(answer(&quotable(&text)))))
        .await;

    // A partner of a different bureau, created directly in the database.
    let (other_bureau, _slug) = support::new_bureau_as_admin(&app.admin_pool).await;
    let foreign_partner =
        support::insert_partner_as_admin(&app.admin_pool, other_bureau, "Чужой партнёр").await;

    for uri in [
        format!("/api/partners/{foreign_partner}/knowledge"),
        format!("/api/partners/{foreign_partner}/knowledge/products"),
        format!("/api/partners/{foreign_partner}/knowledge/glossary"),
        format!("/api/partners/{foreign_partner}/knowledge/qa"),
        format!("/api/partners/{foreign_partner}/knowledge/gaps"),
    ] {
        let response = app.send(client.get(&uri)).await;
        assert_eq!(response.status, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(response.error_code(), "not_found");
    }

    // Nor can this session queue a draft of a material through a partner that is not
    // the material's own.
    let own_partner = app.create_partner(&client, "Второй партнёр").await;
    let response = app
        .send(client.json_request(
            Method::POST,
            &format!("/api/partners/{own_partner}/materials/{material_id}/understand"),
            json!(null),
        ))
        .await;
    assert!(
        response.status == StatusCode::NOT_FOUND || response.status == StatusCode::CONFLICT,
        "a material must not be reachable through another partner: {}",
        response.text()
    );

    support::delete_bureau_as_admin(&app.admin_pool, other_bureau).await;
    app.cleanup().await;
}

/// The two guarantees that must not depend on the application getting it right.
///
/// Written as raw SQL against the schema, with the bureau context set exactly as the
/// server sets it, because that is the only way to show that a *different* writer —
/// a later phase, a migration script, a mistake — cannot store an unsourced fact or
/// point evidence at another partner's page.
#[tokio::test]
async fn the_database_refuses_an_unsourced_fact_and_a_foreign_page() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;

    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload_and_read(&app, &client, partner).await;
    let other_partner = app.create_partner(&client, "Другой партнёр").await;
    let other_material = upload_and_read(&app, &client, other_partner).await;

    let page_id = page_id_of(&app, material_id).await;
    let foreign_page_id = page_id_of(&app, other_material).await;
    let run_id = create_run(&app, partner, material_id).await;

    // 1. A fact with no evidence: accepted by the INSERT, refused at commit by the
    //    deferred constraint trigger.
    let mut tx = app.admin_tx().await;
    sqlx::query(
        "INSERT INTO otdel.knowledge_facts \
             (bureau_id, partner_id, material_id, run_id, kind, attribute, value_text) \
         VALUES ($1, $2, $3, $4, 'characteristic', 'нагрузка', '3.5')",
    )
    .bind(app.bureau_id)
    .bind(partner)
    .bind(material_id)
    .bind(run_id)
    .execute(&mut *tx)
    .await
    .expect("the insert itself is legal; the commit is not");

    let error = tx
        .commit()
        .await
        .expect_err("committing a fact with no evidence must fail");
    assert!(
        error.to_string().contains("evidence"),
        "the error must name the missing evidence: {error}"
    );

    // 2. Evidence pointing at a page of another partner's material.
    let mut tx = app.admin_tx().await;
    let fact_id: (Uuid,) = sqlx::query_as(
        "INSERT INTO otdel.knowledge_facts \
             (bureau_id, partner_id, material_id, run_id, kind, attribute, value_text) \
         VALUES ($1, $2, $3, $4, 'characteristic', 'нагрузка', '3.5') RETURNING id",
    )
    .bind(app.bureau_id)
    .bind(partner)
    .bind(material_id)
    .bind(run_id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();

    let error = sqlx::query(
        "INSERT INTO otdel.knowledge_evidence \
             (bureau_id, partner_id, material_id, page_id, page_number, fact_id, quote, \
              char_start, char_end) \
         VALUES ($1, $2, $3, $4, 1, $5, 'украденная цитата', 0, 10)",
    )
    .bind(app.bureau_id)
    .bind(partner)
    .bind(material_id)
    // A real page — of a material this fact has nothing to do with.
    .bind(foreign_page_id)
    .bind(fact_id.0)
    .execute(&mut *tx)
    .await
    .expect_err("evidence must not be able to cite another material's page");
    assert_eq!(
        support::sqlstate(&error).as_deref(),
        Some("23503"),
        "expected a foreign-key violation, got: {error}"
    );
    tx.rollback().await.unwrap();

    // …and the same page cited with the other partner's identity is refused too.
    let mut tx = app.admin_tx().await;
    let error = sqlx::query(
        "INSERT INTO otdel.knowledge_evidence \
             (bureau_id, partner_id, material_id, page_id, page_number, fact_id, quote, \
              char_start, char_end) \
         VALUES ($1, $2, $3, $4, 1, $5, 'украденная цитата', 0, 10)",
    )
    .bind(app.bureau_id)
    .bind(other_partner)
    .bind(material_id)
    .bind(page_id)
    .bind(fact_id.0)
    .execute(&mut *tx)
    .await
    .expect_err("a material belongs to one partner, and the keys say so");
    assert_eq!(support::sqlstate(&error).as_deref(), Some("23503"));
    tx.rollback().await.unwrap();

    // Nothing of the above reached the draft.
    let overview = get(&app, &client, &format!("/api/partners/{partner}/knowledge")).await;
    assert_eq!(overview["summary"]["facts_total"], 0);

    app.cleanup().await;
}

/// The page id of a material's first page, read directly.
async fn page_id_of(app: &TestApp, material_id: Uuid) -> Uuid {
    let mut tx = app.admin_tx().await;
    let row: (Uuid,) = sqlx::query_as(
        "SELECT id FROM otdel.material_pages WHERE material_id = $1 ORDER BY page_number LIMIT 1",
    )
    .bind(material_id)
    .fetch_one(&mut *tx)
    .await
    .expect("the material has a page");
    tx.commit().await.unwrap();
    row.0
}

async fn create_run(app: &TestApp, partner: Uuid, material_id: Uuid) -> Uuid {
    let mut tx = app.admin_tx().await;
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO otdel.knowledge_runs \
             (bureau_id, partner_id, material_id, status, prompt_profile) \
         VALUES ($1, $2, $3, 'running', 'test') \
         ON CONFLICT (material_id) DO UPDATE SET status = 'running' RETURNING id",
    )
    .bind(app.bureau_id)
    .bind(partner)
    .bind(material_id)
    .fetch_one(&mut *tx)
    .await
    .expect("create the run row");
    tx.commit().await.unwrap();
    row.0
}

#[tokio::test]
async fn the_knowledge_endpoints_require_a_session_and_a_csrf_token() {
    let app = TestApp::start_with_provider(scripted(json!({}))).await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload_and_read(&app, &client, partner).await;

    // No session at all.
    for uri in [
        "/api/knowledge/provider".to_owned(),
        format!("/api/partners/{partner}/knowledge"),
        format!("/api/partners/{partner}/knowledge/products"),
        format!("/api/partners/{partner}/knowledge/gaps"),
    ] {
        let response = app
            .send(
                Request::builder()
                    .method(Method::GET)
                    .uri(&uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status, StatusCode::UNAUTHORIZED, "{uri}");
    }

    // A session, but no CSRF token on the state-changing request.
    let response = app
        .send(
            Request::builder()
                .method(Method::POST)
                .uri(format!(
                    "/api/partners/{partner}/materials/{material_id}/understand"
                ))
                .header(axum::http::header::COOKIE, &client.cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(response.status, StatusCode::FORBIDDEN);
    assert_eq!(response.error_code(), "invalid_csrf_token");

    app.cleanup().await;
}
