//! Phase 1B end to end: upload → worker → page records → API.
//!
//! Every test here drives the *real* pieces: the HTTP API, a real PostgreSQL under
//! row-level security, the real object store and the real extraction worker. The only
//! thing ever substituted is the OCR engine, and only in the two tests that need a
//! working one — everywhere else recognition is genuinely unavailable, which is the
//! state of a machine with no Tesseract and the state these checks are about.
//!
//! The documents are built in memory (`otdel_extract::fixtures`); no partner file is
//! committed to this repository.

mod support;

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use otdel_extract::{
    fixtures, OcrEngine, PageRasteriser, RecognisedText, ToolAvailability, ToolResult,
};
use serde_json::Value;
use support::{TestApp, TestClient};
use uuid::Uuid;

// --- helpers --------------------------------------------------------------------------

async fn upload(
    app: &TestApp,
    client: &TestClient,
    partner: Uuid,
    name: &str,
    bytes: &[u8],
) -> Uuid {
    let response = app
        .send(client.upload_request(
            &format!("/api/partners/{partner}/materials"),
            name,
            Some("application/pdf"),
            bytes,
        ))
        .await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.text());
    Uuid::parse_str(response.json()["id"].as_str().unwrap()).unwrap()
}

async fn material(app: &TestApp, client: &TestClient, partner: Uuid, material: Uuid) -> Value {
    let response = app
        .send(client.get(&format!("/api/partners/{partner}/materials/{material}")))
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text());
    response.json()
}

async fn page_list(
    app: &TestApp,
    client: &TestClient,
    partner: Uuid,
    material: Uuid,
) -> Vec<Value> {
    let response = app
        .send(client.get(&format!(
            "/api/partners/{partner}/materials/{material}/pages"
        )))
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text());
    response.json()["items"].as_array().unwrap().clone()
}

async fn page_detail(
    app: &TestApp,
    client: &TestClient,
    partner: Uuid,
    material: Uuid,
    page: i32,
) -> Value {
    let response = app
        .send(client.get(&format!(
            "/api/partners/{partner}/materials/{material}/pages/{page}"
        )))
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text());
    response.json()
}

/// An engine that always recognises the same text. Used to prove the *positive* path
/// without making the suite depend on an installed binary.
struct StubEngine(&'static str);

#[async_trait]
impl OcrEngine for StubEngine {
    fn name(&self) -> &str {
        "stub-ocr"
    }
    fn language(&self) -> &str {
        "rus+eng"
    }
    async fn availability(&self) -> ToolAvailability {
        ToolAvailability::Available {
            version: "stub-ocr 1.0".to_owned(),
        }
    }
    async fn recognise(&self, _image: &Path) -> ToolResult<RecognisedText> {
        Ok(RecognisedText {
            text: self.0.to_owned(),
            engine: "stub-ocr".to_owned(),
            engine_version: "stub-ocr 1.0".to_owned(),
            language: "rus+eng".to_owned(),
        })
    }
}

struct StubRasteriser;

#[async_trait]
impl PageRasteriser for StubRasteriser {
    fn name(&self) -> &str {
        "stub-rasteriser"
    }
    async fn availability(&self) -> ToolAvailability {
        ToolAvailability::Available {
            version: "stub-rasteriser 1.0".to_owned(),
        }
    }
    async fn render(
        &self,
        _pdf: &Path,
        page: u32,
        out_dir: &Path,
    ) -> ToolResult<std::path::PathBuf> {
        Ok(out_dir.join(format!("page-{page}.png")))
    }
}

// --- the text-layer document ----------------------------------------------------------

#[tokio::test]
async fn a_pdf_with_a_text_layer_becomes_pages_with_text_and_source_regions() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload(
        &app,
        &client,
        partner,
        "catalogue.pdf",
        &fixtures::text_pdf(),
    )
    .await;

    // Before the worker runs, nothing pretends the file has been read.
    let before = material(&app, &client, partner, material_id).await;
    assert_eq!(before["status"], "queued");
    assert!(before["extraction"].is_null());
    assert!(before["page_count"].is_null());

    let report = app.run_worker(&app.extractor()).await;
    assert_eq!(report.jobs_claimed, 1);
    assert_eq!(report.jobs_completed, 1);
    assert_eq!(report.pages_read, 1);

    let after = material(&app, &client, partner, material_id).await;
    assert_eq!(after["status"], "completed");
    assert_eq!(after["page_count"], 1);
    let summary = &after["extraction"];
    assert_eq!(summary["pages_total"], 1);
    assert_eq!(summary["pages_extracted"], 1);
    assert_eq!(summary["pages_needs_ocr"], 0);
    assert_eq!(summary["pages_failed"], 0);
    // The parser that produced it is recorded, so a later re-read with another version
    // is distinguishable.
    assert_eq!(summary["parser_name"], "pdf-extract");
    assert!(summary["parser_version"].is_string());
    assert!(summary["ocr_engine"].is_null(), "no engine was involved");

    let pages = page_list(&app, &client, partner, material_id).await;
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0]["page_number"], 1);
    assert_eq!(pages[0]["status"], "extracted");
    assert_eq!(pages[0]["text_source"], "text_layer");
    assert!(pages[0]["char_count"].as_i64().unwrap() > 100);
    assert!(pages[0]["region_count"].as_i64().unwrap() >= 2);
    assert!(pages[0]["ocr_engine"].is_null());

    // The page itself carries the text and the regions that point back at the original.
    let detail = page_detail(&app, &client, partner, material_id, 1).await;
    assert!(detail["text"].as_str().unwrap().contains("BASIS"));
    let regions = detail["regions"].as_array().unwrap();
    assert!(!regions.is_empty());
    for region in regions {
        assert_eq!(region["page_number"], 1);
        assert_eq!(region["source"], "text_layer");
        // A text-layer region knows where it is on the page.
        assert!(region["bbox"].is_object(), "{region}");
        assert!(region["bbox"]["x0"].as_f64().unwrap() >= 0.0);
    }
    assert!(regions.iter().any(|region| region["kind"] == "heading"));

    app.cleanup().await;
}

// --- the scanned document -------------------------------------------------------------

#[tokio::test]
async fn a_scanned_pdf_is_never_reported_as_read_or_empty_without_an_engine() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload(
        &app,
        &client,
        partner,
        "presentation.pdf",
        &fixtures::scanned_pdf(4),
    )
    .await;

    let report = app.run_worker(&app.extractor()).await;
    assert_eq!(report.pages_read, 4);
    assert_eq!(report.pages_recognised, 0);
    assert_eq!(report.pages_needing_recognition, 4);

    let after = material(&app, &client, partner, material_id).await;
    // The decisive negative: a scan nobody could read is not "completed".
    assert_ne!(after["status"], "completed");
    assert_eq!(after["status"], "failed");
    // ...but all its pages are accounted for.
    assert_eq!(after["page_count"], 4);
    assert_eq!(after["extraction"]["pages_total"], 4);
    assert_eq!(after["extraction"]["pages_needs_ocr"], 4);
    assert_eq!(after["extraction"]["pages_empty"], 0);
    assert!(after["extraction"]["ocr_engine"].is_null());

    let pages = page_list(&app, &client, partner, material_id).await;
    assert_eq!(pages.len(), 4);
    for page in &pages {
        assert_eq!(page["status"], "needs_ocr");
        assert_ne!(page["status"], "empty");
        assert_eq!(page["text_source"], "none");
        assert_eq!(page["char_count"], 0);
        assert!(page["image_count"].as_i64().unwrap() >= 1);
        // The reason is written down, in words, and names the missing tool.
        let diagnostic = page["diagnostic"].as_str().unwrap();
        assert!(diagnostic.contains("не найден"), "{diagnostic}");
    }

    // And no text was invented for any of them.
    let detail = page_detail(&app, &client, partner, material_id, 2).await;
    assert!(detail["text"].is_null());
    assert!(detail["regions"].as_array().unwrap().is_empty());

    app.cleanup().await;
}

#[tokio::test]
async fn a_partly_readable_document_is_partial_not_completed() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload(&app, &client, partner, "mixed.pdf", &fixtures::mixed_pdf()).await;

    app.run_worker(&app.extractor()).await;

    let after = material(&app, &client, partner, material_id).await;
    assert_eq!(after["status"], "partial");
    assert_eq!(after["extraction"]["pages_total"], 3);
    assert_eq!(after["extraction"]["pages_extracted"], 2);
    assert_eq!(after["extraction"]["pages_needs_ocr"], 1);

    let pages = page_list(&app, &client, partner, material_id).await;
    assert_eq!(pages[0]["status"], "extracted");
    assert_eq!(pages[1]["status"], "needs_ocr");
    assert_eq!(pages[2]["status"], "extracted");

    app.cleanup().await;
}

// --- tables ----------------------------------------------------------------------------

#[tokio::test]
async fn a_specification_table_reaches_the_database_with_units_and_blanks() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload(&app, &client, partner, "table.pdf", &fixtures::table_pdf()).await;

    app.run_worker(&app.extractor()).await;

    let pages = page_list(&app, &client, partner, material_id).await;
    assert_eq!(pages[0]["table_count"], 1);

    let detail = page_detail(&app, &client, partner, material_id, 1).await;
    let table = detail["regions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|region| region["kind"] == "table")
        .expect("the table region must be stored");

    assert_eq!(table["row_count"], 5);
    assert_eq!(table["column_count"], 3);

    let cells = table["cells"].as_array().unwrap();
    assert_eq!(cells.len(), 15);

    let cell = |row: i64, column: i64| {
        cells
            .iter()
            .find(|cell| cell["row_index"] == row && cell["column_index"] == column)
            .unwrap_or_else(|| panic!("cell {row}/{column} is missing"))
    };

    // Header kept verbatim.
    assert_eq!(cell(0, 1)["raw_text"], "Length, mm");
    assert_eq!(cell(0, 1)["is_header"], true);

    // A value keeps its text, its unit and the header it came from — nothing is parsed
    // into a number on the way in.
    let length = cell(1, 1);
    assert_eq!(length["raw_text"], "1200");
    assert_eq!(length["value_kind"], "number");
    assert_eq!(length["unit"], "mm");
    assert_eq!(length["column_header"], "Length, mm");

    // A designation column has no unit invented for it.
    assert_eq!(cell(1, 0)["raw_text"], "BP21");
    assert!(cell(1, 0)["unit"].is_null());

    // The load that the source does not print stays blank, not zero.
    let missing = cell(4, 2);
    assert_eq!(missing["raw_text"], "");
    assert_eq!(missing["value_kind"], "empty");
    assert_ne!(missing["value_kind"], "number");

    app.cleanup().await;
}

// --- recognition, when it is genuinely available ----------------------------------------

#[tokio::test]
async fn an_available_engine_produces_recognised_pages_attributed_to_it() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload(
        &app,
        &client,
        partner,
        "presentation.pdf",
        &fixtures::scanned_pdf(2),
    )
    .await;

    let extractor = app.extractor_with(
        Arc::new(StubEngine("Надёжная основа крепления\n\nинженерных систем")),
        Arc::new(StubRasteriser),
    );
    let report = app.run_worker(&extractor).await;
    assert_eq!(report.pages_recognised, 2);
    assert_eq!(report.pages_needing_recognition, 0);

    let after = material(&app, &client, partner, material_id).await;
    assert_eq!(after["status"], "completed");
    assert_eq!(after["extraction"]["pages_extracted"], 2);
    assert_eq!(after["extraction"]["ocr_engine"], "stub-ocr");

    let pages = page_list(&app, &client, partner, material_id).await;
    for page in &pages {
        assert_eq!(page["status"], "extracted");
        // Recognised text is recorded as recognised, never as a text layer.
        assert_eq!(page["text_source"], "ocr");
        assert_eq!(page["ocr_engine"], "stub-ocr");
        assert_eq!(page["ocr_language"], "rus+eng");
    }

    let detail = page_detail(&app, &client, partner, material_id, 1).await;
    assert!(detail["text"].as_str().unwrap().contains("Надёжная"));
    for region in detail["regions"].as_array().unwrap() {
        assert_eq!(region["source"], "ocr");
        // The engine returns text, not coordinates, so none are invented.
        assert!(region["bbox"].is_null(), "{region}");
    }

    app.cleanup().await;
}

// --- failures ---------------------------------------------------------------------------

#[tokio::test]
async fn a_file_that_cannot_be_parsed_fails_with_a_reason_and_stops_retrying() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload(&app, &client, partner, "broken.pdf", &fixtures::not_a_pdf()).await;

    let report = app.run_worker(&app.extractor()).await;
    assert_eq!(report.jobs_failed, 1);
    assert_eq!(report.pages_read, 0);

    let after = material(&app, &client, partner, material_id).await;
    assert_eq!(after["status"], "failed");
    assert!(after["error"].as_str().unwrap().contains("PDF"));

    // The job is settled as permanently failed rather than queued for another attempt.
    let jobs = app
        .send(client.get(&format!("/api/partners/{partner}/jobs")))
        .await;
    let items = jobs.json()["items"].as_array().unwrap().clone();
    assert_eq!(items[0]["status"], "failed");
    assert!(items[0]["error"].as_str().unwrap().contains("PDF"));

    // A second pass finds nothing to do — the failure does not spin.
    let again = app.run_worker(&app.extractor()).await;
    assert_eq!(again.jobs_claimed, 0);

    app.cleanup().await;
}

// --- retry -------------------------------------------------------------------------------

#[tokio::test]
async fn retrying_one_page_is_idempotent_and_creates_no_duplicates() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload(&app, &client, partner, "mixed.pdf", &fixtures::mixed_pdf()).await;

    app.run_worker(&app.extractor()).await;
    let before = page_detail(&app, &client, partner, material_id, 2).await;
    assert_eq!(before["page"]["status"], "needs_ocr");

    let retry_path = format!("/api/partners/{partner}/materials/{material_id}/pages/2/retry");

    // Pressing retry twice must not create two jobs or two page rows.
    for _ in 0..2 {
        let response = app
            .send(client.json_request(Method::POST, &retry_path, serde_json::json!({})))
            .await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.text());
        assert_eq!(response.json()["status"], "pending");
        assert_eq!(response.json()["page_number"], 2);
    }

    let jobs = app
        .send(client.get(&format!("/api/partners/{partner}/jobs")))
        .await;
    let page_jobs: Vec<Value> = jobs.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|job| job["kind"] == "extract_page")
        .cloned()
        .collect();
    assert_eq!(page_jobs.len(), 1, "one row per (material, page)");
    assert_eq!(page_jobs[0]["page_number"], 2);
    assert_eq!(page_jobs[0]["status"], "queued");

    // Running the worker again re-reads only that page and lands on the same outcome.
    let report = app.run_worker(&app.extractor()).await;
    assert_eq!(report.jobs_claimed, 1);
    assert_eq!(report.pages_read, 1);

    let pages = page_list(&app, &client, partner, material_id).await;
    assert_eq!(pages.len(), 3, "no duplicate page rows");
    assert_eq!(pages[1]["status"], "needs_ocr");
    // The attempt counter records that the page really was read twice.
    assert_eq!(pages[1]["attempts"], 2);
    // The other pages were not touched.
    assert_eq!(pages[0]["attempts"], 1);

    app.cleanup().await;
}

#[tokio::test]
async fn a_page_that_was_read_cleanly_cannot_be_retried() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload(
        &app,
        &client,
        partner,
        "catalogue.pdf",
        &fixtures::text_pdf(),
    )
    .await;
    app.run_worker(&app.extractor()).await;

    let response = app
        .send(client.json_request(
            Method::POST,
            &format!("/api/partners/{partner}/materials/{material_id}/pages/1/retry"),
            serde_json::json!({}),
        ))
        .await;
    assert_eq!(response.status, StatusCode::CONFLICT, "{}", response.text());
    assert_eq!(response.error_code(), "conflict");

    app.cleanup().await;
}

#[tokio::test]
async fn retrying_the_whole_material_replaces_its_pages_in_place() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload(&app, &client, partner, "mixed.pdf", &fixtures::mixed_pdf()).await;

    app.run_worker(&app.extractor()).await;
    let first = page_list(&app, &client, partner, material_id).await;
    assert_eq!(first.len(), 3);
    let first_page_id = first[0]["id"].clone();

    let response = app
        .send(client.json_request(
            Method::POST,
            &format!("/api/partners/{partner}/materials/{material_id}/retry"),
            serde_json::json!({}),
        ))
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text());
    assert_eq!(response.json()["status"], "queued");

    app.run_worker(&app.extractor()).await;

    let second = page_list(&app, &client, partner, material_id).await;
    assert_eq!(second.len(), 3, "a retry must not duplicate pages");
    // The same rows were updated, so identifiers recorded elsewhere stay valid.
    assert_eq!(second[0]["id"], first_page_id);
    assert_eq!(second[0]["attempts"], 2);
    assert_eq!(
        material(&app, &client, partner, material_id).await["status"],
        "partial"
    );

    app.cleanup().await;
}

// --- isolation ---------------------------------------------------------------------------

#[tokio::test]
async fn one_partners_pages_are_not_reachable_through_another_partner() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let basis = app.create_partner(&client, "BASIS").await;
    let other = app.create_partner(&client, "Другой партнёр").await;

    let material_id = upload(&app, &client, basis, "catalogue.pdf", &fixtures::text_pdf()).await;
    app.run_worker(&app.extractor()).await;

    // The very same material id, asked for under the wrong partner.
    for path in [
        format!("/api/partners/{other}/materials/{material_id}/pages"),
        format!("/api/partners/{other}/materials/{material_id}/pages/1"),
        format!("/api/partners/{other}/materials/{material_id}"),
    ] {
        let response = app.send(client.get(&path)).await;
        assert_eq!(
            response.status,
            StatusCode::NOT_FOUND,
            "{path} must not be readable: {}",
            response.text()
        );
        assert_eq!(response.error_code(), "not_found");
        // And the response says nothing about what is really there.
        assert!(!response.text().contains("BASIS"));
    }

    // Retrying a page of somebody else's material is refused too.
    let response = app
        .send(client.json_request(
            Method::POST,
            &format!("/api/partners/{other}/materials/{material_id}/pages/1/retry"),
            serde_json::json!({}),
        ))
        .await;
    assert_eq!(response.status, StatusCode::NOT_FOUND);

    app.cleanup().await;
}

#[tokio::test]
async fn another_bureau_cannot_see_pages_even_with_the_right_identifiers() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload(
        &app,
        &client,
        partner,
        "catalogue.pdf",
        &fixtures::text_pdf(),
    )
    .await;
    app.run_worker(&app.extractor()).await;

    // A second bureau with its own partner; the ids of the first are known to the test
    // but must be useless inside that bureau.
    let (other_bureau, _slug) = support::new_bureau_as_admin(&app.admin_pool).await;
    let mut tx = support::set_bureau_context(&app.admin_pool, other_bureau).await;
    let visible: i64 =
        sqlx::query_scalar("SELECT count(*) FROM otdel.material_pages WHERE material_id = $1")
            .bind(material_id)
            .fetch_one(&mut *tx)
            .await
            .expect("the query itself must succeed");
    tx.commit().await.unwrap();
    assert_eq!(visible, 0, "row-level security must hide the pages");

    let mut tx = support::set_bureau_context(&app.admin_pool, other_bureau).await;
    let regions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM otdel.page_regions WHERE material_id = $1")
            .bind(material_id)
            .fetch_one(&mut *tx)
            .await
            .expect("the query itself must succeed");
    tx.commit().await.unwrap();
    assert_eq!(regions, 0, "row-level security must hide the regions");

    support::delete_bureau_as_admin(&app.admin_pool, other_bureau).await;
    app.cleanup().await;
}

// --- page numbering ------------------------------------------------------------------------

#[tokio::test]
async fn a_page_that_does_not_exist_is_reported_not_invented() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let material_id = upload(
        &app,
        &client,
        partner,
        "catalogue.pdf",
        &fixtures::text_pdf(),
    )
    .await;
    app.run_worker(&app.extractor()).await;

    let response = app
        .send(client.get(&format!(
            "/api/partners/{partner}/materials/{material_id}/pages/99"
        )))
        .await;
    assert_eq!(response.status, StatusCode::NOT_FOUND);

    let response = app
        .send(client.get(&format!(
            "/api/partners/{partner}/materials/{material_id}/pages/0"
        )))
        .await;
    assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}
