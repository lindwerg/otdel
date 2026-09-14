//! R05 end to end: the product base a productologist can act on.
//!
//! Everything is real except the model: the HTTP API, PostgreSQL under row-level
//! security, the object store, the reader and the understanding worker all run as they do
//! in the pilot. The model is a scripted provider, which is what lets these tests state
//! the case that matters most — **a run that produces products and nothing else must not
//! be reportable as success**, and the same run with its absences stated in words must.
//!
//! That pair is the whole package, and it is the first test below. The audited run over a
//! real 44-page catalogue produced 44 products, 13 facts, 0 terms, 0 Q&A, 0 gaps and left
//! 8 pages out of the account, and every check it had passed. Here it fails, by name, and
//! the reasons are readable.
//!
//! Every quotation these tests hand the fake model is taken from the page text the reader
//! really produced, so nothing asserts against a hand-written guess about the parser.

mod support;

use std::sync::Arc;

use axum::http::StatusCode;
use otdel_embed::EmbeddingProvider;
use otdel_llm::fake::FakeProvider;
use otdel_llm::{LlmProvider, UnconfiguredProvider};
use serde_json::{json, Value};
use support::{TestApp, TestClient};
use uuid::Uuid;

// --- fixtures ----------------------------------------------------------------------------

async fn upload_and_read(app: &TestApp, client: &TestClient, partner: Uuid) -> Uuid {
    upload_bytes_and_read(
        app,
        client,
        partner,
        "catalogue.pdf",
        &otdel_extract::fixtures::text_pdf(),
    )
    .await
}

async fn upload_bytes_and_read(
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

    let report = app.run_worker(&app.extractor()).await;
    assert_eq!(report.pages_read, 1, "the fixture has one readable page");
    material_id
}

/// A second, genuinely different catalogue that names the same maker on its first line.
///
/// Different bytes on purpose: uploading the identical file is deduplicated to the same
/// material, and this test needs two materials to have anything to propose a link
/// between.
fn second_catalogue() -> Vec<u8> {
    use otdel_extract::fixtures::{PageBuilder, PdfBuilder};
    PdfBuilder::new()
        .page(
            PageBuilder::new()
                .text(50.0, 780.0, 24.0, "BASIS mounting systems")
                .text(50.0, 700.0, 11.0, "Price list for profiles and consoles.")
                .text(50.0, 686.0, 11.0, "Lead times on request.")
                .text(50.0, 40.0, 7.0, "1/1 basisparts.ru"),
        )
        .build()
}

/// A multi-page technical document with several products and a table nobody can read.
///
/// Generic on purpose: the *shape* is what the live pass had — more pages than a sentence
/// can answer for, several products, and a load column whose unit is written nowhere — and
/// nothing here is a particular partner's text.
fn technical_catalogue() -> Vec<u8> {
    use otdel_extract::fixtures::{PageBuilder, PdfBuilder};
    let mut builder = PdfBuilder::new();
    for page in 1..=6 {
        builder = builder.page(
            PageBuilder::new()
                .text(50.0, 780.0, 18.0, "Mounting systems catalogue")
                .text(
                    50.0,
                    750.0,
                    11.0,
                    &format!("Section {page}: profiles and consoles"),
                )
                .text(
                    50.0,
                    730.0,
                    11.0,
                    "Every item ships with mounting hardware.",
                )
                // A load table with no unit anywhere: R03 marks the column unreadable and
                // R05 records one uncertainty per group.
                .row(
                    700.0,
                    &[(50.0, "Profile"), (220.0, "Length"), (400.0, "Load")],
                )
                .row(680.0, &[(50.0, "AP10"), (220.0, "1200"), (400.0, "3.5")])
                .row(660.0, &[(50.0, "AP20"), (220.0, "1500"), (400.0, "4.2")])
                .text(50.0, 40.0, 7.0, &format!("{page}/6")),
        );
    }
    builder.build()
}

/// Upload the multi-page technical catalogue and read it.
async fn upload_catalogue(app: &TestApp, client: &TestClient, partner: Uuid) -> Uuid {
    let response = app
        .send(client.upload_request(
            &format!("/api/partners/{partner}/materials"),
            "technical.pdf",
            Some("application/pdf"),
            &technical_catalogue(),
        ))
        .await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.text());
    let material = Uuid::parse_str(response.json()["id"].as_str().unwrap()).unwrap();
    app.run_worker(&app.extractor()).await;
    material
}

/// A draft in which every pass has something real to return.
///
/// Written once as an omnibus answer and sliced per pass by `support::scripted_run`, the
/// same way the server slices the schema — so the fixture reads as "what the model knows"
/// and no test has to know the pass order.
fn full_draft_answer(quote: &str, word: &str) -> Value {
    json!({
        "categories": [],
        "products": (1..=4).map(|n| json!({
            "ref": format!("p{n}"), "category_ref": null, "kind": "product",
            "name": format!("AP{n}0"), "summary": "профиль монтажный", "aliases": [],
        })).collect::<Vec<_>>(),
        "facts": [{
            // The later passes cite the server's label, never a ref of their own.
            "product_ref": "P1", "kind": "characteristic",
            "attribute": "обозначение", "value": word,
            "unit": null, "conditions": null, "model_context": null,
            "evidence": [{"source": "S1", "quote": quote}],
        }],
        "glossary": [{
            "term": word, "definition": "несущий элемент системы",
            "definition_from_source": false,
            "evidence": [{"source": "S1", "quote": quote}],
            "senses": [], "synonyms": [],
        }],
        "qa": [],
        "gaps": [{
            "product_ref": "P1", "topic": "цена",
            "missing": "цена не указана", "blocks": null,
            "question": "Какая отпускная цена?", "audience": "partner",
            "nature": "commercial",
        }, {
            "product_ref": "P1", "topic": "нагрузка",
            "missing": "единица нагрузки не написана", "blocks": null,
            "question": null, "audience": null, "nature": "technical",
        }],
        "applications": [{
            "product_ref": "P1",
            "task": "закрепить лоток к перекрытию",
            "summary": null, "model_context": null,
            "evidence": [{"source": "S1", "quote": quote}],
            "details": [{
                "kind": "parameter", "label": "обозначение",
                "value": word, "unit": null, "audience": null,
                "evidence": [{"source": "S1", "quote": quote}],
            }],
        }],
        "declarations": {
            "glossary": null, "questions": null, "applications": null,
            "commercial_unknowns": null, "technical_unknowns": null,
        },
    })
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

/// The text the reader really stored for page 1.
async fn page_text(app: &TestApp, client: &TestClient, partner: Uuid, material: Uuid) -> String {
    let detail = get(
        app,
        client,
        &format!("/api/partners/{partner}/materials/{material}/pages/1"),
    )
    .await;
    detail["text"].as_str().expect("page text").to_owned()
}

/// A fragment that really is on the page.
fn quotable(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| line.chars().count() >= 12)
        .expect("the fixture page has a quotable line")
        .to_owned()
}

/// A word that really appears in `quote`, so a value or a surface form built from it
/// survives the server's "is this in the fragment you cited" check.
fn word_from(quote: &str) -> String {
    quote
        .split_whitespace()
        .find(|word| word.chars().count() >= 4)
        .expect("the quotable line has a word long enough to use")
        .to_owned()
}

/// Exactly the audited draft: products and a fact, and silence everywhere else.
fn thin_answer(quote: &str) -> Value {
    json!({
        "categories": [],
        "products": [{
            "ref": "p1", "category_ref": null, "kind": "product",
            "name": "BP21", "summary": "профиль монтажный", "aliases": [],
        }],
        "facts": [{
            "product_ref": "p1", "kind": "characteristic",
            "attribute": "обозначение", "value": word_from(quote),
            "unit": null, "conditions": null, "model_context": null,
            "evidence": [{"source": "S1", "quote": quote}],
        }],
        "glossary": [],
        "qa": [],
        "gaps": [],
        "applications": [],
        "declarations": {
            "glossary": null, "questions": null, "applications": null,
            "commercial_unknowns": null, "technical_unknowns": null,
        },
    })
}

/// The same draft, with every absence answered instead of left as an empty array.
///
/// Note what is *not* declared. «Коммерческих неизвестных нет» is unavailable to this
/// material: saying nothing commercial is missing requires that something commercial is
/// present, and this draft states no price. The honest answer there is a gap, so the
/// fixture records one — which is the shape the rule is meant to push a run into.
fn declared_answer(quote: &str) -> Value {
    let mut answer = thin_answer(quote);
    answer["gaps"] = json!([{
        "product_ref": "p1", "topic": "цена",
        "missing": "цена в материале не указана",
        "blocks": "коммерческое предложение",
        "question": "Какая отпускная цена?",
        "audience": "partner", "nature": "commercial",
    }]);
    answer["declarations"] = json!({
        "glossary": "каталог не вводит терминов, требующих пояснения",
        "questions": null,
        "applications": "лист не описывает задач применения, только обозначения",
        "commercial_unknowns": null,
        "technical_unknowns": "технических величин на листе нет",
    });
    answer
}

/// A full R05 draft: a task with its parameters and its question, an alias, a term with a
/// second reading, and a gap the run classified itself.
fn rich_answer(quote: &str) -> Value {
    let word = word_from(quote);
    json!({
        "categories": [{
            "ref": "c1", "kind": "direction",
            "name": "Монтажные системы", "summary": null,
        }],
        "products": [{
            "ref": "p1", "category_ref": "c1", "kind": "product",
            "name": "BP21", "summary": "профиль монтажный",
            "aliases": [
                {
                    // Quoted verbatim on the page, so it is stored — as an observation,
                    // never as a merge.
                    "surface": word, "relation": "unclear",
                    "note": "встречается в этом же абзаце",
                    "evidence": [{"source": "S1", "quote": quote}],
                },
                {
                    // Nowhere in the fragment it cites: refused with a reason.
                    "surface": "Профиль-невидимка", "relation": "alias", "note": null,
                    "evidence": [{"source": "S1", "quote": quote}],
                },
            ],
        }],
        "facts": [{
            "product_ref": "p1", "kind": "characteristic",
            "attribute": "обозначение", "value": word,
            "unit": null, "conditions": null,
            "model_context": "формулировка модели, не цитата",
            "evidence": [{"source": "S1", "quote": quote}],
        }],
        "glossary": [{
            "term": word, "definition": "несущий элемент системы",
            "definition_from_source": false,
            "evidence": [{"source": "S1", "quote": quote}],
            "senses": [{
                "label": "в контексте кабельных лотков",
                "definition": "опора, на которую ложится лоток",
                "definition_from_source": false,
                "evidence": [{"source": "S1", "quote": quote}],
            }],
            "synonyms": [{
                "surface": word, "relation": "abbreviation", "note": null,
                "evidence": [{"source": "S1", "quote": quote}],
            }],
        }],
        "qa": [{
            "question": "Что описывает каталог?", "answer": "Профили и консоли.",
            "evidence": [{"source": "S1", "quote": quote}],
        }],
        "gaps": [
            {
                "product_ref": "p1", "topic": "price",
                "missing": "цена не указана в материале",
                "blocks": "коммерческое предложение",
                "question": "Какая отпускная цена профиля BP21?",
                "audience": "partner", "nature": "commercial",
            },
            {
                "product_ref": "p1", "topic": "load",
                "missing": "несущая способность не указана",
                "blocks": null, "question": null, "audience": null,
                "nature": "technical",
            },
        ],
        "applications": [{
            "product_ref": "p1",
            "task": "закрепить кабельный лоток к бетонному перекрытию",
            "summary": null,
            "model_context": "обобщение модели, не цитата",
            "evidence": [{"source": "S1", "quote": quote}],
            "details": [
                {
                    "kind": "parameter", "label": "обозначение профиля",
                    "value": word, "unit": null, "audience": null,
                    "evidence": [{"source": "S1", "quote": quote}],
                },
                {
                    // A claim with no fragment: refused, and the refusal is counted.
                    "kind": "constraint", "label": "только для сухих помещений",
                    "value": "сухие помещения", "unit": null, "audience": null,
                    "evidence": [],
                },
                {
                    // A question asserts nothing, so it needs no quotation — only an
                    // addressee, without which nobody could ask it.
                    "kind": "question", "label": "какой класс бетона допускается?",
                    "value": null, "unit": null, "audience": "partner",
                    "evidence": [],
                },
            ],
        }],
        "declarations": {
            "glossary": null, "questions": null, "applications": null,
            "commercial_unknowns": null, "technical_unknowns": null,
        },
    })
}

/// A provider that answers one whole run per omnibus answer.
///
/// R05.2: a run is five purpose-specific passes. A test still writes "what the model knows
/// about this material" once and this slices it per pass, the same way the server slices
/// the schema — so the fixtures stay readable and no test has to know the pass order.
fn scripted(answers: Vec<Value>) -> Arc<FakeProvider> {
    support::scripted_runs(&answers)
}

/// The coverage report of the only run of this partner.
async fn coverage_of(app: &TestApp, client: &TestClient, partner: Uuid) -> Value {
    let reports = get(app, client, &format!("/api/partners/{partner}/coverage")).await;
    reports["items"][0].clone()
}

// --- the requirement gate ------------------------------------------------------------------

/// The audited run, and the same run once it says what it checked.
///
/// Nothing about the *material* differs between the two halves below. The only difference
/// is whether the run stated its absences, and that alone decides whether anything may be
/// published from it without a person reading it first.
#[tokio::test]
async fn a_draft_of_products_and_nothing_else_is_not_success_until_the_absences_are_stated() {
    // The API refuses to queue a draft while the adapter is unconfigured, and this test
    // queues a second one by hand. The adapter it is given answers nothing — the worker
    // below brings its own scripted provider — it only has to report itself as ready.
    let app = TestApp::start_with_provider(Arc::new(FakeProvider::new(Vec::new()))).await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material = upload_and_read(&app, &client, partner).await;
    let quote = quotable(&page_text(&app, &client, partner, material).await);

    // --- half one: empty arrays --------------------------------------------------------
    let provider = scripted(vec![thin_answer(&quote)]);
    let report = app
        .run_knowledge(&app.knowledge_worker(provider.clone()))
        .await;
    assert_eq!(report.jobs_completed, 1);
    assert_eq!(
        report.runs_below_requirements, 1,
        "the pass itself reports that this draft is not publishable"
    );

    let thin = coverage_of(&app, &client, partner).await;
    assert_eq!(thin["requirements"], "unmet");
    assert_eq!(thin["allows_automatic_publication"], false);

    let missing: Vec<String> = thin["requirements_missing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line.as_str().unwrap().to_owned())
        .collect();
    // Five distinct problems, each named: this is what «0 терминов, 0 вопросов,
    // 0 пробелов» looks like once it is no longer reportable as success.
    assert_eq!(missing.len(), 5, "{missing:?}");
    for expected in [
        "applications:",
        "glossary:",
        "questions:",
        "commercial_unknowns:",
        "technical_unknowns:",
    ] {
        assert!(
            missing.iter().any(|line| line.starts_with(expected)),
            "{expected} is not among {missing:?}"
        );
    }

    // The page account is nevertheless complete: the material really was read end to end.
    // Coverage and content are separate verdicts, and a run can pass one and fail the
    // other — which is exactly why one boolean could never have expressed this.
    assert_eq!(thin["state"], "complete");
    assert_eq!(thin["pages_total"], 1);
    assert_eq!(thin["pages_processed"], 1);

    // --- half two: the same emptiness, stated ------------------------------------------
    let response = app
        .send(client.json_request(
            axum::http::Method::POST,
            &format!("/api/partners/{partner}/materials/{material}/understand"),
            json!({}),
        ))
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text());

    let provider = scripted(vec![declared_answer(&quote)]);
    let report = app
        .run_knowledge(&app.knowledge_worker(provider.clone()))
        .await;
    assert_eq!(report.jobs_completed, 1);
    assert_eq!(report.runs_below_requirements, 0);

    let declared = coverage_of(&app, &client, partner).await;
    assert_eq!(declared["requirements"], "met");
    assert_eq!(declared["allows_automatic_publication"], true);
    assert!(declared["requirements_missing"]
        .as_array()
        .unwrap()
        .is_empty());

    // The statements themselves are on the record, in the run's own words, and a person
    // can disagree with them. A flag could not have been disagreed with.
    // Three, not five. `questions` and `commercial_unknowns` are answered by rows —
    // a prepared question and a commercial gap — and the run is right not to claim in
    // words what it can show with records.
    let declarations = declared["declarations"].as_array().unwrap();
    assert_eq!(declarations.len(), 3, "{declarations:?}");
    let glossary = declarations
        .iter()
        .find(|item| item["topic"] == "glossary")
        .expect("the glossary declaration");
    assert_eq!(glossary["origin"], "model");
    assert!(glossary["stated"]
        .as_str()
        .unwrap()
        .contains("не вводит терминов"));
}

/// A declaration that contradicts the same response is refused rather than stored.
#[tokio::test]
async fn a_statement_that_a_topic_is_empty_beside_rows_on_that_topic_is_refused() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material = upload_and_read(&app, &client, partner).await;
    let quote = quotable(&page_text(&app, &client, partner, material).await);

    // Eleven terms and "there are no terms", in one answer.
    let mut answer = rich_answer(&quote);
    answer["declarations"]["glossary"] = json!("в материале нет терминов");

    let provider = scripted(vec![answer]);
    app.run_knowledge(&app.knowledge_worker(provider)).await;

    let declarations = get(
        &app,
        &client,
        &format!("/api/partners/{partner}/declarations"),
    )
    .await;
    assert!(
        declarations["items"].as_array().unwrap().is_empty(),
        "a contradiction is not an observation: {declarations}"
    );

    let overview = get(&app, &client, &format!("/api/partners/{partner}/knowledge")).await;
    let rejections: Vec<String> = overview["runs"][0]["rejections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line.as_str().unwrap().to_owned())
        .collect();
    assert!(
        rejections
            .iter()
            .any(|line| line.contains("заявление") && line.contains("glossary")),
        "the refusal is explained: {rejections:?}"
    );
}

// --- the page account -------------------------------------------------------------------

#[tokio::test]
async fn every_page_of_the_material_is_in_the_run_account_with_what_became_of_it() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material = upload_and_read(&app, &client, partner).await;
    let quote = quotable(&page_text(&app, &client, partner, material).await);

    let provider = scripted(vec![rich_answer(&quote)]);
    app.run_knowledge(&app.knowledge_worker(provider)).await;

    let report = coverage_of(&app, &client, partner).await;
    assert_eq!(report["material_filename"], "catalogue.pdf");
    assert_eq!(report["pages_total"], 1, "the denominator is the material");
    assert_eq!(report["pages_offered"], 1);
    assert_eq!(report["pages_processed"], 1);
    assert_eq!(report["pages_deferred"], 0);
    assert_eq!(report["pages_unreadable"], 0);
    assert_eq!(report["state"], "complete");
    assert!(report["resumable_pages"].as_array().unwrap().is_empty());

    let pages = report["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 1, "one row per page, always");
    assert_eq!(pages[0]["page_number"], 1);
    assert_eq!(pages[0]["disposition"], "processed");
    assert_eq!(pages[0]["offered"], true);
    assert_eq!(pages[0]["batch_index"], 1);
    assert!(pages[0]["chars_sent"].as_i64().unwrap() > 0);
    // A processed page needs no excuse; every other disposition does, and the database
    // refuses the pair (unprocessed, no reason) outright.
    assert!(pages[0]["reason"].is_null());
}

/// A run that never reached the model still leaves an account of the material.
#[tokio::test]
async fn a_run_that_could_not_call_the_model_still_says_what_the_material_is_made_of() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    upload_and_read(&app, &client, partner).await;

    // No replies at all: the provider is ready but the first call fails, so the run ends
    // with a diagnostic and nothing stored.
    let provider = Arc::new(FakeProvider::new(Vec::new()));
    let report = app.run_knowledge(&app.knowledge_worker(provider)).await;
    assert_eq!(report.jobs_failed, 1);

    let coverage = coverage_of(&app, &client, partner).await;
    assert_eq!(coverage["pages_total"], 1, "the pages are still counted");
    assert_eq!(coverage["pages_processed"], 0);
    assert_eq!(coverage["state"], "incomplete");
    assert_eq!(coverage["requirements"], "unknown");
    assert_eq!(
        coverage["allows_automatic_publication"], false,
        "a run nobody judged never passes the gate"
    );

    let pages = coverage["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0]["disposition"], "excluded_by_request");
    assert!(
        pages[0]["reason"].as_str().is_some_and(|r| !r.is_empty()),
        "no page leaves the account without a reason: {pages:?}"
    );
}

/// The audited failure itself, reproduced and then made unreportable.
///
/// A three-page material, a budget that allows one request, and a page nobody could read.
/// The old record had one number — `pages_considered` — and no denominator, so this run
/// and a run over the whole document were indistinguishable. Here the three pages are in
/// three different states, each with its own words, and the run cannot call itself
/// `completed` while one of them is still queued.
#[tokio::test]
async fn a_run_stopped_by_its_budget_names_the_pages_it_left_and_is_never_completed() {
    let app = TestApp::start_with_env(std::collections::BTreeMap::from([
        ("OTDEL_LLM_MAX_PAGES_PER_REQUEST".to_owned(), "1".to_owned()),
        ("OTDEL_LLM_MAX_REQUESTS_PER_RUN".to_owned(), "1".to_owned()),
    ]))
    .await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;

    let response = app
        .send(client.upload_request(
            &format!("/api/partners/{partner}/materials"),
            "mixed.pdf",
            Some("application/pdf"),
            &otdel_extract::fixtures::mixed_pdf(),
        ))
        .await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.text());
    let material = Uuid::parse_str(response.json()["id"].as_str().unwrap()).unwrap();
    app.run_worker(&app.extractor()).await;

    let quote = quotable(&page_text(&app, &client, partner, material).await);
    let provider = scripted(vec![thin_answer(&quote)]);
    app.run_knowledge(&app.knowledge_worker(provider.clone()))
        .await;
    assert_eq!(provider.call_count(), 1, "the budget allowed one request");

    let report = coverage_of(&app, &client, partner).await;
    assert_eq!(report["pages_total"], 3, "the denominator is the document");
    assert_eq!(report["pages_processed"], 1);
    assert_eq!(report["pages_deferred"], 1);
    assert_eq!(report["pages_unreadable"], 1);
    assert_eq!(report["state"], "incomplete");
    assert_eq!(report["allows_automatic_publication"], false);
    assert_eq!(
        report["status"], "partial",
        "a run with pages still queued has not finished the material"
    );

    // Three pages, three different answers to "what became of it" — and the one that is
    // merely waiting is told apart from the one nobody could read.
    let pages = report["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 3);
    let by_number = |number: i64| {
        pages
            .iter()
            .find(|page| page["page_number"] == number)
            .unwrap_or_else(|| panic!("page {number} is missing from the account"))
    };
    assert_eq!(by_number(1)["disposition"], "processed");
    assert_eq!(by_number(2)["disposition"], "unreadable_needs_ocr");
    assert_eq!(by_number(3)["disposition"], "deferred_budget");
    for number in [2, 3] {
        assert!(
            by_number(number)["reason"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty()),
            "page {number} left the draft without a reason"
        );
    }

    // Only the budget deferral can resume on its own: a page awaiting recognition needs
    // the reader to run again first, and offering it to the queue would spin forever.
    assert_eq!(
        report["resumable_pages"].as_array().unwrap(),
        &vec![json!(3)]
    );

    // The reasons are also in the run's own notes, with the page numbers named.
    let notes: Vec<String> = report["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|note| note.as_str().unwrap().to_owned())
        .collect();
    assert!(
        notes.iter().any(|note| note.contains("бюджет")),
        "{notes:?}"
    );
    assert!(
        notes.iter().any(|note| note.contains("распознавания")),
        "{notes:?}"
    );

    // And the page nobody could read is an *unknown*, not an absence: a reader of the
    // passport learns that something is on page 2 and that it has not been read.
    let unknowns = get(
        &app,
        &client,
        &format!("/api/partners/{partner}/uncertainties"),
    )
    .await;
    let unreadable: Vec<&Value> = unknowns["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["kind"] == "unreadable_page")
        .collect();
    assert_eq!(unreadable.len(), 1, "{unknowns}");
    assert_eq!(unreadable[0]["page_number"], 2);
    assert!(
        unreadable[0]["quote"].is_null(),
        "there is nothing to quote"
    );
}

// --- the live regression -------------------------------------------------------------------

/// The defect a live pass found, reproduced end to end and then made unreportable.
///
/// A real run over a multi-page technical catalogue came back `coverage = complete`,
/// `requirements = met` with **0 terms, 0 applications and 123 unsettled readings** — and
/// a published version was built from it. It passed because the declaration escape hatch
/// was unconditional: the model said "there is none" about all five topics and every
/// requirement cleared. The audited failure had simply moved. Instead of an empty array
/// passing silently, a self-serving sentence passed it.
///
/// Two things have to hold now, and this test asserts both against the same run:
///
/// 1. the requirement check refuses those declarations and names each topic, so an empty
///    glossary and an empty application map are reported rather than cleared;
/// 2. nothing publishes itself from a partner in that state.
#[tokio::test]
async fn declaring_every_topic_empty_over_a_real_catalogue_neither_passes_nor_publishes() {
    // The suite's default upload limit is a few kilobytes; a six-page document is the
    // point of this test, so it gets room for one.
    let app = TestApp::start_with_env(std::collections::BTreeMap::from([(
        "OTDEL_MAX_UPLOAD_BYTES".to_owned(),
        "262144".to_owned(),
    )]))
    .await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "Партнёр").await;

    let response = app
        .send(client.upload_request(
            &format!("/api/partners/{partner}/materials"),
            "technical.pdf",
            Some("application/pdf"),
            &technical_catalogue(),
        ))
        .await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.text());
    app.run_worker(&app.extractor()).await;

    let quote = quotable(
        &page_text(
            &app,
            &client,
            partner,
            Uuid::parse_str(response.json()["id"].as_str().unwrap()).unwrap(),
        )
        .await,
    );
    let word = word_from(&quote);

    // Exactly the live answer's shape: several products, a fact or two, and every topic
    // waved away in words. One reply per page — the run is not budget-limited here, so
    // the *only* thing standing between this draft and "met" is the declaration rule.
    let answer = json!({
        "categories": [],
        "products": (1..=4).map(|n| json!({
            "ref": format!("p{n}"), "category_ref": null, "kind": "product",
            "name": format!("AP{n}0"), "summary": "профиль монтажный", "aliases": [],
        })).collect::<Vec<_>>(),
        "facts": [{
            "product_ref": "p1", "kind": "characteristic",
            "attribute": "обозначение", "value": word,
            "unit": null, "conditions": null, "model_context": null,
            "evidence": [{"source": "S1", "quote": quote}],
        }],
        "glossary": [],
        "qa": [],
        "gaps": [],
        "applications": [],
        "declarations": {
            "glossary": "каталог не вводит терминов",
            "questions": "спрашивать нечего",
            "applications": "задач применения материал не описывает",
            "commercial_unknowns": "коммерческих неизвестных не осталось",
            "technical_unknowns": "технических неизвестных не осталось",
        },
    });

    let replies: Vec<Value> = (0..6).map(|_| answer.clone()).collect();
    let report = app
        .run_knowledge(&app.knowledge_worker(scripted(replies)))
        .await;
    assert_eq!(report.jobs_completed, 1);
    assert_eq!(
        report.runs_below_requirements, 1,
        "the pass itself must report that this draft is not publishable"
    );

    // --- 1. the requirement check --------------------------------------------------
    let coverage = coverage_of(&app, &client, partner).await;
    // The pages really were all read: this is not a coverage failure, and reporting it as
    // one would send the owner looking in the wrong place.
    assert_eq!(coverage["state"], "complete");
    assert_eq!(coverage["pages_processed"], coverage["pages_total"]);
    assert_eq!(
        coverage["requirements"], "unmet",
        "five sentences must not clear a six-page catalogue: {coverage}"
    );
    assert_eq!(coverage["allows_automatic_publication"], false);

    let missing: Vec<String> = coverage["requirements_missing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line.as_str().unwrap().to_owned())
        .collect();
    // Three topics refused because the run itself disagrees with them. `glossary` and
    // `applications` are *not* here, and that is the rule working rather than failing:
    // those two passes read every page of this material, so "we found none" is something
    // they are in a position to say. The next test is the case where they are not.
    for topic in ["questions:", "commercial_unknowns:", "technical_unknowns:"] {
        assert!(
            missing.iter().any(|line| line.starts_with(topic)),
            "{topic} is not named in {missing:?}"
        );
    }
    // The refusals explain themselves rather than reading as "nothing was said": a
    // declaration *was* made and was not good enough, which is a different instruction
    // to the owner.
    assert!(
        missing.iter().any(|line| line.contains("противоречит")),
        "the unsettled readings contradict the declarations: {missing:?}"
    );

    // The uncertainties the contradiction rests on are real and visible.
    let unknowns = get(
        &app,
        &client,
        &format!("/api/partners/{partner}/uncertainties"),
    )
    .await;
    assert!(
        !unknowns["items"].as_array().unwrap().is_empty(),
        "the table with no unit must leave something unsettled"
    );

    // --- 2. the publication gate ----------------------------------------------------
    // The understanding worker queues a check automatically; run it.
    // No model and no embedding adapter: the deterministic check alone is enough to
    // publish, so nothing here weakens the case — if this partner published, it would be
    // on the strength of the rules and not of a missing dependency.
    let llm: Arc<dyn LlmProvider> = Arc::new(UnconfiguredProvider::new(
        &otdel_core::llm_config::LlmSettings::default(),
    ));
    let embeddings: Arc<dyn EmbeddingProvider> =
        otdel_embed::build_provider(&otdel_core::retrieval_config::EmbeddingSettings::default());
    let validation = app
        .run_validation(&app.validation_worker(llm, embeddings))
        .await;
    assert_eq!(validation.jobs_completed, 1);
    assert_eq!(
        validation.versions_published, 0,
        "a partner whose passports are incomplete does not publish itself"
    );
    assert_eq!(validation.versions_blocked, 1);

    let published = get(&app, &client, &format!("/api/partners/{partner}/versions")).await;
    let live: Vec<&Value> = published["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|version| version["status"] == "published")
        .collect();
    assert!(live.is_empty(), "nothing was published: {published}");

    // The blocked snapshot says why, in the owner's terms, naming the material.
    let blocked = published["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|version| version["status"] == "blocked")
        .expect("the snapshot is kept and marked, never discarded");
    let reasons: Vec<String> = blocked["blocked_reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line.as_str().unwrap().to_owned())
        .collect();
    assert!(
        reasons.iter().any(|line| line.contains("паспорт собран")),
        "{reasons:?}"
    );
    assert!(
        reasons.iter().any(|line| line.contains("technical.pdf")),
        "{reasons:?}"
    );

    // Preserved: the partial draft is still there to look at. Blocking publication is not
    // throwing the work away.
    let passports = get(&app, &client, &format!("/api/partners/{partner}/passports")).await;
    assert_eq!(passports["items"].as_array().unwrap().len(), 4);
}

// --- R05.2: the purpose-specific passes ------------------------------------------------

/// A material that really has terms and applications must end up with them.
///
/// The second live run was the gate working and the productologist failing: 51 products,
/// 6 facts, **0 terms, 0 applications** over a technical catalogue that plainly has both.
/// One omnibus request asked for everything at once and spent its output budget on the
/// cheapest section, and nothing downstream could tell that apart from a material with no
/// terms in it.
///
/// The run is five bounded passes now, each with a schema containing only its own
/// sections. This test gives every pass something real to find and asserts the sections
/// arrive — and that a draft which genuinely carries them is allowed to publish.
#[tokio::test]
async fn a_material_with_terms_and_applications_produces_both_sections_and_may_publish() {
    let app = TestApp::start_with_env(std::collections::BTreeMap::from([(
        "OTDEL_MAX_UPLOAD_BYTES".to_owned(),
        "262144".to_owned(),
    )]))
    .await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "Партнёр").await;

    let material = upload_catalogue(&app, &client, partner).await;
    let quote = quotable(&page_text(&app, &client, partner, material).await);
    let word = word_from(&quote);

    let provider = scripted(vec![full_draft_answer(&quote, &word)]);
    let report = app
        .run_knowledge(&app.knowledge_worker(provider.clone()))
        .await;
    assert_eq!(report.jobs_completed, 1);
    // One request per purpose: the material fits a single batch each way.
    assert_eq!(provider.call_count(), support::PASSES_PER_RUN);

    // Each pass was asked for its own thing, and only its own thing.
    let prompts = provider.prompts();
    assert!(
        prompts[0].contains("ТОЛЬКО СОСТАВ ПРЕДЛОЖЕНИЯ"),
        "{}",
        prompts[0]
    );
    assert!(
        prompts[1].contains("ТОЛЬКО ХАРАКТЕРИСТИКИ"),
        "{}",
        prompts[1]
    );
    assert!(prompts[2].contains("ТОЛЬКО ТЕРМИНЫ"), "{}", prompts[2]);
    assert!(
        prompts[3].contains("ТОЛЬКО ЗАДАЧИ ПРИМЕНЕНИЯ"),
        "{}",
        prompts[3]
    );
    assert!(
        prompts[4].contains("ТОЛЬКО ВОПРОСЫ И ПРОБЕЛЫ"),
        "{}",
        prompts[4]
    );
    // …and the later passes were handed the products instead of being asked to re-list
    // them, which is what kept the budget from going on products a second time.
    assert!(
        prompts[1].contains("ИЗДЕЛИЯ, УЖЕ ВЫДЕЛЕННЫЕ"),
        "{}",
        prompts[1]
    );
    assert!(!prompts[2].contains("ТОЛЬКО СОСТАВ"), "{}", prompts[2]);

    // The sections the live run lost.
    let glossary = get(
        &app,
        &client,
        &format!("/api/partners/{partner}/knowledge/glossary"),
    )
    .await;
    assert!(
        !glossary["items"].as_array().unwrap().is_empty(),
        "the glossary pass produced nothing: {glossary}"
    );

    let applications = get(
        &app,
        &client,
        &format!("/api/partners/{partner}/applications"),
    )
    .await;
    assert!(
        !applications["items"].as_array().unwrap().is_empty(),
        "the applications pass produced nothing: {applications}"
    );

    // Every pass covered the material, and the draft carries what a passport needs…
    let coverage = coverage_of(&app, &client, partner).await;
    assert_eq!(coverage["state"], "complete");
    assert_eq!(
        coverage["requirements"], "met",
        "a genuinely filled draft must be allowed through: {coverage}"
    );
    assert_eq!(coverage["allows_automatic_publication"], true);

    // …so this one publishes, which is the other half of the gate being correct.
    let llm: Arc<dyn LlmProvider> = Arc::new(UnconfiguredProvider::new(
        &otdel_core::llm_config::LlmSettings::default(),
    ));
    let embeddings: Arc<dyn EmbeddingProvider> =
        otdel_embed::build_provider(&otdel_core::retrieval_config::EmbeddingSettings::default());
    let validation = app
        .run_validation(&app.validation_worker(llm, embeddings))
        .await;
    assert_eq!(
        validation.versions_published, 1,
        "a complete passport must still be publishable"
    );
}

/// The same material, with a budget that cannot reach the glossary and application passes.
///
/// This is the case the owner named: the absences must be **unresolved coverage under
/// their own topic**, never a declaration clearing a topic nothing examined.
#[tokio::test]
async fn a_topic_whose_pass_never_ran_is_unresolved_coverage_and_not_a_declaration_escape() {
    let app = TestApp::start_with_env(std::collections::BTreeMap::from([
        ("OTDEL_MAX_UPLOAD_BYTES".to_owned(), "262144".to_owned()),
        // One page per request over a six-page material, and six requests for the whole
        // run: the fair share gives every pass one request, so no pass finishes.
        ("OTDEL_LLM_MAX_PAGES_PER_REQUEST".to_owned(), "1".to_owned()),
        ("OTDEL_LLM_MAX_REQUESTS_PER_RUN".to_owned(), "5".to_owned()),
        (
            "OTDEL_LLM_MAX_REQUESTS_PER_PURPOSE".to_owned(),
            "1".to_owned(),
        ),
    ]))
    .await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "Партнёр").await;

    let material = upload_catalogue(&app, &client, partner).await;
    let quote = quotable(&page_text(&app, &client, partner, material).await);
    let word = word_from(&quote);

    // The model declares every topic empty — the escape the second live run took.
    let mut answer = full_draft_answer(&quote, &word);
    answer["glossary"] = json!([]);
    answer["applications"] = json!([]);
    answer["declarations"] = json!({
        "glossary": "терминов в материале нет",
        "questions": "спрашивать нечего",
        "applications": "задач применения материал не описывает",
        "commercial_unknowns": "коммерческих неизвестных не осталось",
        "technical_unknowns": "технических неизвестных не осталось",
    });

    let provider = scripted(vec![answer]);
    let report = app.run_knowledge(&app.knowledge_worker(provider)).await;
    assert_eq!(report.jobs_completed, 1);
    assert_eq!(report.runs_below_requirements, 1);

    let coverage = coverage_of(&app, &client, partner).await;
    assert_eq!(coverage["requirements"], "unmet");
    assert_eq!(coverage["allows_automatic_publication"], false);

    let missing: Vec<String> = coverage["requirements_missing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line.as_str().unwrap().to_owned())
        .collect();

    // Named under their own topics, and named as *coverage* — the run is not claiming the
    // material has no terms, it is saying nothing finished looking.
    for topic in ["glossary:", "applications:"] {
        let line = missing
            .iter()
            .find(|line| line.starts_with(topic))
            .unwrap_or_else(|| panic!("{topic} is not named in {missing:?}"));
        assert!(
            line.contains("охват"),
            "{topic} must read as unresolved coverage, not as a verdict: {line}"
        );
    }

    // And the per-pass account backs it up: each pass says how much it actually read.
    let passes = coverage["passes"].as_array().unwrap();
    assert_eq!(passes.len(), support::PASSES_PER_RUN, "{coverage}");
    for pass in passes {
        assert_eq!(pass["requests_allowed"], 1);
        assert_eq!(
            pass["covered_everything"], false,
            "one request cannot cover six pages: {pass}"
        );
        assert!(pass["pages_deferred"].as_i64().unwrap() > 0, "{pass}");
    }
}

/// Re-queueing a material drops what the previous pass said about *itself*.
///
/// The run row survives a re-queue, so its page account, its unsettled readings and its
/// explicit absences have to go with the counters that were just zeroed. A run reporting
/// `coverage_state = 'unknown'` beside yesterday's page-by-page report — or beside
/// yesterday's «в материале нет терминов» — would be a stale clearance of exactly the kind
/// this package exists to remove.
#[tokio::test]
async fn re_queueing_a_material_leaves_no_claim_from_the_previous_pass() {
    let app = TestApp::start_with_provider(Arc::new(FakeProvider::new(Vec::new()))).await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material = upload_and_read(&app, &client, partner).await;
    let quote = quotable(&page_text(&app, &client, partner, material).await);

    let provider = scripted(vec![declared_answer(&quote)]);
    app.run_knowledge(&app.knowledge_worker(provider)).await;

    let settled = coverage_of(&app, &client, partner).await;
    assert_eq!(settled["declarations"].as_array().unwrap().len(), 3);
    assert_eq!(settled["pages"].as_array().unwrap().len(), 1);

    // Queue it again and look before the worker runs.
    let response = app
        .send(client.json_request(
            axum::http::Method::POST,
            &format!("/api/partners/{partner}/materials/{material}/understand"),
            json!({}),
        ))
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text());

    let queued = coverage_of(&app, &client, partner).await;
    assert_eq!(queued["status"], "queued");
    assert_eq!(queued["state"], "unknown");
    assert_eq!(queued["requirements"], "unknown");
    assert_eq!(queued["pages_total"], 0);
    assert!(
        queued["pages"].as_array().unwrap().is_empty(),
        "the previous pass's page report must not describe this one"
    );
    assert!(
        queued["declarations"].as_array().unwrap().is_empty(),
        "a run that has said nothing yet must not carry yesterday's statements"
    );
    assert_eq!(queued["allows_automatic_publication"], false);

    // The candidates themselves are deliberately left alone: a re-run that never happens
    // must not delete the draft the owner already has.
    let passports = get(&app, &client, &format!("/api/partners/{partner}/passports")).await;
    assert_eq!(passports["items"].as_array().unwrap().len(), 1);
}

/// R03 read the tables; R05 is the first thing to use what it found.
#[tokio::test]
async fn the_model_is_shown_the_servers_reading_of_the_table_and_never_as_a_quotation() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material = upload_bytes_and_read(
        &app,
        &client,
        partner,
        "loads.pdf",
        &otdel_extract::fixtures::table_pdf(),
    )
    .await;

    let quote = quotable(&page_text(&app, &client, partner, material).await);
    let provider = scripted(vec![thin_answer(&quote)]);
    app.run_knowledge(&app.knowledge_worker(provider.clone()))
        .await;

    let prompt = &provider.prompts()[0];
    assert!(
        prompt.contains("РАЗБОР ТАБЛИЦ НА СТРАНИЦЕ 1"),
        "the established rows are shown beside the page: {prompt}"
    );
    assert!(
        prompt.contains("это чтение сервера, а не цитата"),
        "the reading is labelled as the server's, so it is never quotable: {prompt}"
    );
    assert!(
        prompt.contains("изделие: BP21"),
        "the model is told which product the row is about: {prompt}"
    );
    // Still no identifier the model could use to name anything outside this run.
    assert!(!prompt.contains(&material.to_string()));
}

// --- the passport ---------------------------------------------------------------------------

#[tokio::test]
async fn a_passport_carries_the_facts_the_tasks_and_everything_that_is_still_missing() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material = upload_and_read(&app, &client, partner).await;
    let text = page_text(&app, &client, partner, material).await;
    let quote = quotable(&text);
    let word = word_from(&quote);

    let provider = scripted(vec![rich_answer(&quote)]);
    app.run_knowledge(&app.knowledge_worker(provider)).await;

    let passports = get(&app, &client, &format!("/api/partners/{partner}/passports")).await;
    let items = passports["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{passports}");
    let passport = &items[0];

    assert_eq!(passport["product"]["name"], "BP21");
    assert_eq!(passport["category"]["name"], "Монтажные системы");
    assert_eq!(passport["material_filename"], "catalogue.pdf");

    // What is known…
    assert_eq!(passport["facts"].as_array().unwrap().len(), 1);
    assert_eq!(passport["applications"].as_array().unwrap().len(), 1);
    // …and, in the same response and without asking for it, what is not.
    assert_eq!(
        passport["gaps"].as_array().unwrap().len(),
        2,
        "a passport that shows only the facts has told half the truth"
    );
    let natures: Vec<&str> = passport["gaps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|gap| gap["nature"].as_str().unwrap())
        .collect();
    assert!(natures.contains(&"commercial"));
    assert!(natures.contains(&"technical"));

    // An alias the page really shows is recorded, as an observation. `unclear` is a
    // legal answer and is never safe to follow.
    let aliases = passport["aliases"].as_array().unwrap();
    assert_eq!(
        aliases.len(),
        1,
        "the invented one was refused: {aliases:?}"
    );
    assert_eq!(aliases[0]["surface"], word);
    assert_eq!(aliases[0]["relation"], "unclear");
    assert!(aliases[0]["quote"]
        .as_str()
        .unwrap()
        .to_lowercase()
        .contains(&word.to_lowercase()));

    // The single-product view is the same shape as the list entry.
    let product_id = passport["product"]["id"].as_str().unwrap();
    let single = get(
        &app,
        &client,
        &format!("/api/partners/{partner}/passports/{product_id}"),
    )
    .await;
    assert_eq!(single["product"]["id"], product_id);
    assert_eq!(single["facts"], passport["facts"]);

    // A product of another partner is not reachable through this partner's path.
    let other = app.create_partner(&client, "ДРУГОЙ").await;
    let response = app
        .send(client.get(&format!("/api/partners/{other}/passports/{product_id}")))
        .await;
    assert_eq!(
        response.status,
        StatusCode::NOT_FOUND,
        "{}",
        response.text()
    );
}

#[tokio::test]
async fn the_application_map_carries_parameters_with_their_source_and_questions_without_one() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material = upload_and_read(&app, &client, partner).await;
    let quote = quotable(&page_text(&app, &client, partner, material).await);
    let word = word_from(&quote);

    let provider = scripted(vec![rich_answer(&quote)]);
    app.run_knowledge(&app.knowledge_worker(provider)).await;

    let map = get(
        &app,
        &client,
        &format!("/api/partners/{partner}/applications"),
    )
    .await;
    let applications = map["items"].as_array().unwrap();
    assert_eq!(applications.len(), 1, "{map}");
    let application = &applications[0];

    assert_eq!(
        application["task"],
        "закрепить кабельный лоток к бетонному перекрытию"
    );
    assert_eq!(application["product_name"], "BP21");
    // The model's framing stays in its own field and is never presented as a quotation.
    assert_eq!(application["model_context"], "обобщение модели, не цитата");
    assert!(!application["quote"].as_str().unwrap().is_empty());

    let details = application["details"].as_array().unwrap();
    // Two of the three survived: the constraint claimed something and cited nothing.
    assert_eq!(details.len(), 2, "{details:?}");

    let parameter = details
        .iter()
        .find(|detail| detail["kind"] == "parameter")
        .expect("the parameter");
    assert_eq!(parameter["value_text"], word);
    assert!(parameter["quote"]
        .as_str()
        .unwrap()
        .to_lowercase()
        .contains(&word.to_lowercase()));
    assert!(parameter["audience"].is_null());

    let question = details
        .iter()
        .find(|detail| detail["kind"] == "question")
        .expect("the question");
    assert_eq!(question["audience"], "partner");
    assert!(question["value_text"].is_null());
    assert!(
        question["quote"].is_null(),
        "a question asserts nothing, so it needs no fragment"
    );

    // Narrowing to the product returns the same task; narrowing to another returns none.
    let product_id = get(&app, &client, &format!("/api/partners/{partner}/passports")).await
        ["items"][0]["product"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let owned = get(
        &app,
        &client,
        &format!("/api/partners/{partner}/applications?product_id={product_id}"),
    )
    .await;
    assert_eq!(owned["items"].as_array().unwrap().len(), 1);

    let elsewhere = get(
        &app,
        &client,
        &format!(
            "/api/partners/{partner}/applications?product_id={}",
            Uuid::from_u128(999)
        ),
    )
    .await;
    assert!(elsewhere["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_term_used_two_ways_keeps_both_readings_and_its_other_spellings() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material = upload_and_read(&app, &client, partner).await;
    let quote = quotable(&page_text(&app, &client, partner, material).await);

    let provider = scripted(vec![rich_answer(&quote)]);
    app.run_knowledge(&app.knowledge_worker(provider)).await;

    let glossary = get(
        &app,
        &client,
        &format!("/api/partners/{partner}/knowledge/glossary"),
    )
    .await;
    let term = &glossary["items"][0];

    let senses = term["senses"].as_array().unwrap();
    assert_eq!(senses.len(), 1, "{term}");
    assert_eq!(senses[0]["label"], "в контексте кабельных лотков");
    // A sense that is the model's wording says so; its evidence still points at the
    // fragment the reading was taken from.
    assert_eq!(senses[0]["definition_is_model_context"], true);
    assert!(!senses[0]["quote"].as_str().unwrap().is_empty());

    let synonyms = term["synonyms"].as_array().unwrap();
    assert_eq!(synonyms.len(), 1, "{term}");
    assert_eq!(synonyms[0]["relation"], "abbreviation");
    assert!(!synonyms[0]["quote"].as_str().unwrap().is_empty());
}

// --- identity across materials ------------------------------------------------------------------

/// Two catalogues naming the same profile produce two rows and one proposal.
///
/// The rows stay separate — that is `0004`'s rule and R05 does not change it. What R05
/// adds is that the reader is told they may be the same thing, and on what grounds.
#[tokio::test]
async fn the_same_product_in_two_materials_is_proposed_as_one_and_never_merged() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;

    let first = upload_and_read(&app, &client, partner).await;
    let quote = quotable(&page_text(&app, &client, partner, first).await);
    // The designation both catalogues share, taken from the page rather than invented:
    // the fact's quotation has to contain the product's name, which is what makes the
    // designation *quoted* rather than merely written in a field somewhere.
    let named = word_from(&quote);

    let answer = |quote: &str| {
        let mut answer = thin_answer(quote);
        answer["products"][0]["name"] = json!(named);
        answer["facts"][0]["value"] = json!(named);
        answer
    };

    let provider = scripted(vec![answer(&quote)]);
    app.run_knowledge(&app.knowledge_worker(provider)).await;

    // A second catalogue of the same partner, naming the same thing on its own page.
    let second =
        upload_bytes_and_read(&app, &client, partner, "pricelist.pdf", &second_catalogue()).await;
    let second_quote = quotable(&page_text(&app, &client, partner, second).await);

    let provider = scripted(vec![answer(&second_quote)]);
    app.run_knowledge(&app.knowledge_worker(provider)).await;

    let passports = get(&app, &client, &format!("/api/partners/{partner}/passports")).await;
    let items = passports["items"].as_array().unwrap();
    assert_eq!(
        items.len(),
        2,
        "the rows stay separate: a merge would invent an identity nobody proved"
    );

    let links = get(&app, &client, &format!("/api/partners/{partner}/identity")).await;
    let proposals = links["items"].as_array().unwrap();
    assert_eq!(proposals.len(), 1, "{links}");
    assert_eq!(proposals[0]["state"], "linked");
    assert_eq!(proposals[0]["basis"], "identical_designation_quoted");
    // A link is unrepresentable without a page on each side; the database enforces it.
    assert!(proposals[0]["page_number"].as_i64().is_some());
    assert!(proposals[0]["other_page_number"].as_i64().is_some());
    assert_ne!(
        proposals[0]["material_id"],
        proposals[0]["other_material_id"]
    );

    // Both passports show the proposal, each reading from its own side.
    for passport in items {
        let own = passport["product"]["id"].as_str().unwrap();
        let shown = passport["identity_links"].as_array().unwrap();
        assert_eq!(shown.len(), 1, "{passport}");
        assert_eq!(shown[0]["product_id"], own);
        assert_ne!(shown[0]["other_product_id"], own);
    }
}

// --- the roll-up ------------------------------------------------------------------------------

#[tokio::test]
async fn the_partner_overview_counts_the_unknowns_beside_the_knowns() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material = upload_and_read(&app, &client, partner).await;
    let quote = quotable(&page_text(&app, &client, partner, material).await);

    let provider = scripted(vec![rich_answer(&quote)]);
    app.run_knowledge(&app.knowledge_worker(provider)).await;

    let overview = get(&app, &client, &format!("/api/partners/{partner}/knowledge")).await;
    let summary = &overview["summary"];
    assert_eq!(summary["products_total"], 1);
    assert_eq!(summary["applications_total"], 1);
    assert_eq!(summary["gaps_total"], 2);
    assert_eq!(summary["terms_total"], 1);
    // The fixture has no tables and its page was read, so nothing is unsettled. Reported
    // as zero rather than omitted: "no open uncertainties" is an answer.
    assert_eq!(summary["uncertainties_total"], 0);
    // The passport is substantive: it has a summary, facts, tasks and gaps. A product row
    // with only a name would not be counted here, which is the point.
    assert_eq!(summary["passports_substantive"], 1);
    // This draft carries everything a passport needs — terms, a question, a commercial
    // gap and a technical one — so the gate opens, and it says so at the partner level
    // too.
    assert_eq!(summary["materials_ready"], 1);

    let run = &overview["runs"][0];
    assert_eq!(run["applications_created"], 1);
    assert_eq!(run["declarations_made"], 0, "nothing needed declaring");
    // The account travels on the run itself, so no reader of a run can see «фактов 1»
    // without also seeing what it was one fact *out of*.
    assert_eq!(run["coverage"]["pages_total"], 1);
    assert_eq!(run["coverage"]["pages_processed"], 1);
    assert_eq!(run["coverage"]["state"], "complete");
    assert_eq!(run["coverage"]["requirements"], "met");
    assert!(run["coverage"]["requirements_missing"]
        .as_array()
        .unwrap()
        .is_empty());
}
