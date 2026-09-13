//! Partner CRUD, material intake, download, deduplication, retry and the queue.

mod support;

use axum::body::Body;
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE, COOKIE};
use axum::http::{Method, Request, StatusCode};
use support::{MultipartPart, TestApp};

#[tokio::test]
async fn partner_validation_follows_the_contract() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;

    // Empty and over-long names are refused.
    for bad_name in ["", "   ", &"x".repeat(201)] {
        let response = app
            .send(client.json_request(
                Method::POST,
                "/api/partners",
                serde_json::json!({ "name": bad_name }),
            ))
            .await;
        assert_eq!(
            response.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{bad_name:?}"
        );
        assert_eq!(response.error_code(), "validation_failed");
    }

    // Over-long note.
    let long_note = app
        .send(client.json_request(
            Method::POST,
            "/api/partners",
            serde_json::json!({ "name": "BASIS", "note": "n".repeat(10_001) }),
        ))
        .await;
    assert_eq!(long_note.status, StatusCode::UNPROCESSABLE_ENTITY);

    // Trimming, and the exact response shape.
    let created = app
        .send(client.json_request(
            Method::POST,
            "/api/partners",
            serde_json::json!({ "name": "  BASIS  ", "note": "  металлокаркас  " }),
        ))
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text());
    let partner = created.json();
    assert_eq!(partner["name"], "BASIS");
    assert_eq!(partner["note"], "металлокаркас");
    for field in ["id", "name", "note", "created_at", "updated_at"] {
        assert!(partner.get(field).is_some(), "missing {field}");
    }
    assert_eq!(partner.as_object().unwrap().len(), 5);
    let partner_id = partner["id"].as_str().unwrap().to_owned();

    // PATCH: absent note keeps it, null clears it.
    let renamed = app
        .send(client.json_request(
            Method::PATCH,
            &format!("/api/partners/{partner_id}"),
            serde_json::json!({ "name": "BASIS Group" }),
        ))
        .await;
    assert_eq!(renamed.status, StatusCode::OK);
    assert_eq!(renamed.json()["name"], "BASIS Group");
    assert_eq!(renamed.json()["note"], "металлокаркас");

    let cleared = app
        .send(client.json_request(
            Method::PATCH,
            &format!("/api/partners/{partner_id}"),
            serde_json::json!({ "note": null }),
        ))
        .await;
    assert_eq!(cleared.status, StatusCode::OK);
    assert!(cleared.json()["note"].is_null());

    // An empty patch is a validation error, not a silent no-op.
    let empty = app
        .send(client.json_request(
            Method::PATCH,
            &format!("/api/partners/{partner_id}"),
            serde_json::json!({}),
        ))
        .await;
    assert_eq!(empty.status, StatusCode::UNPROCESSABLE_ENTITY);

    // Unknown fields are refused rather than ignored.
    let unknown = app
        .send(client.json_request(
            Method::POST,
            "/api/partners",
            serde_json::json!({ "name": "X", "inn": "7700000000" }),
        ))
        .await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);

    // A non-UUID path parameter is a clean validation error.
    let bad_path = app.send(client.get("/api/partners/not-a-uuid")).await;
    assert_eq!(bad_path.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(bad_path.error_code(), "validation_failed");

    app.cleanup().await;
}

#[tokio::test]
async fn upload_stores_the_original_and_queues_extraction() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;

    let pdf = support::tiny_pdf("catalogue");
    let response = app
        .send(client.upload_request(
            &format!("/api/partners/{partner}/materials"),
            "каталог 2026.pdf",
            Some("application/pdf"),
            &pdf,
        ))
        .await;

    assert_eq!(response.status, StatusCode::CREATED, "{}", response.text());
    let material = response.json();
    for field in [
        "id",
        "partner_id",
        "filename",
        "media_type",
        "size_bytes",
        "sha256",
        "status",
        "page_count",
        "created_at",
        "error",
        // Phase 1B: the page roll-up, `null` until the worker has read anything.
        "extraction",
        // Phase 1F: how many times this original has been *read*. `0` for a fresh
        // upload — a changed file is a new material, so this only counts re-readings.
        "content_revision",
    ] {
        assert!(material.get(field).is_some(), "missing {field}");
    }
    assert_eq!(material.as_object().unwrap().len(), 12);
    assert_eq!(material["partner_id"], partner.to_string());
    assert_eq!(material["media_type"], "application/pdf");
    assert_eq!(material["filename"], "каталог 2026.pdf");
    assert_eq!(material["size_bytes"], pdf.len() as i64);
    assert_eq!(material["sha256"].as_str().unwrap().len(), 64);
    // A freshly uploaded file is queued, never “completed”: nothing has read it yet, so
    // there is no page count and no extraction summary either.
    assert_eq!(material["status"], "queued");
    assert!(material["page_count"].is_null());
    assert!(material["error"].is_null());
    assert!(material["extraction"].is_null());
    assert_eq!(material["content_revision"], 0);

    // The queue really holds a job for it.
    let jobs = app
        .send(client.get(&format!("/api/partners/{partner}/jobs")))
        .await;
    assert_eq!(jobs.status, StatusCode::OK);
    let items = jobs.json()["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["kind"], "extract_document");
    assert_eq!(items[0]["status"], "queued");
    assert_eq!(items[0]["material_id"], material["id"]);
    assert_eq!(items[0]["attempts"], 0);
    assert!(items[0]["stage"].is_null());
    // A whole-document job names no page; only a single-page retry does.
    assert!(items[0]["page_number"].is_null());

    // And the listing shows it.
    let listed = app
        .send(client.get(&format!("/api/partners/{partner}/materials")))
        .await;
    assert_eq!(listed.json()["items"].as_array().unwrap().len(), 1);

    app.cleanup().await;
}

#[tokio::test]
async fn identical_upload_is_deduplicated_per_partner() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let first_partner = app.create_partner(&client, "BASIS").await;
    let second_partner = app.create_partner(&client, "Another partner").await;

    let pdf = support::tiny_pdf("dedup");

    let created = app
        .send(client.upload_request(
            &format!("/api/partners/{first_partner}/materials"),
            "catalogue.pdf",
            Some("application/pdf"),
            &pdf,
        ))
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    let material_id = created.json()["id"].as_str().unwrap().to_owned();

    // Same bytes again: 200, the same material, no second row and no second job.
    let duplicate = app
        .send(client.upload_request(
            &format!("/api/partners/{first_partner}/materials"),
            "catalogue-copy.pdf",
            Some("application/pdf"),
            &pdf,
        ))
        .await;
    assert_eq!(duplicate.status, StatusCode::OK, "{}", duplicate.text());
    assert_eq!(duplicate.json()["id"], material_id);

    let materials = app
        .send(client.get(&format!("/api/partners/{first_partner}/materials")))
        .await;
    assert_eq!(materials.json()["items"].as_array().unwrap().len(), 1);
    let jobs = app
        .send(client.get(&format!("/api/partners/{first_partner}/jobs")))
        .await;
    assert_eq!(
        jobs.json()["items"].as_array().unwrap().len(),
        1,
        "a repeated upload must not enqueue a second job"
    );

    // The same file for a *different* partner is a separate original.
    let other = app
        .send(client.upload_request(
            &format!("/api/partners/{second_partner}/materials"),
            "catalogue.pdf",
            Some("application/pdf"),
            &pdf,
        ))
        .await;
    assert_eq!(other.status, StatusCode::CREATED);
    assert_ne!(other.json()["id"], material_id);
    assert_eq!(other.json()["sha256"], created.json()["sha256"]);

    // A changed file is a new material, not an overwrite.
    let changed = app
        .send(client.upload_request(
            &format!("/api/partners/{first_partner}/materials"),
            "catalogue.pdf",
            Some("application/pdf"),
            &support::tiny_pdf("dedup-revision-2"),
        ))
        .await;
    assert_eq!(changed.status, StatusCode::CREATED);
    assert_ne!(changed.json()["id"], material_id);
    let materials = app
        .send(client.get(&format!("/api/partners/{first_partner}/materials")))
        .await;
    assert_eq!(materials.json()["items"].as_array().unwrap().len(), 2);

    app.cleanup().await;
}

#[tokio::test]
async fn uploads_are_validated_by_signature_and_size() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let uri = format!("/api/partners/{partner}/materials");

    // Not an accepted format (a ZIP, e.g. a renamed .docx).
    let zip = app
        .send(client.upload_request(
            &uri,
            "catalogue.pdf",
            Some("application/pdf"),
            b"PK\x03\x04rest-of-zip",
        ))
        .await;
    assert_eq!(zip.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(zip.error_code(), "unsupported_media_type");

    // HTML pretending to be a PDF by name.
    let html = app
        .send(client.upload_request(&uri, "catalogue.pdf", None, b"<html><body>not a pdf</body>"))
        .await;
    assert_eq!(html.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);

    // A declared type that contradicts the content.
    let mismatch = app
        .send(client.upload_request(
            &uri,
            "image.png",
            Some("image/png"),
            &support::tiny_pdf("x"),
        ))
        .await;
    assert_eq!(mismatch.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert!(mismatch.json()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("does not match"));

    // Empty file.
    let empty = app
        .send(client.upload_request(&uri, "empty.pdf", Some("application/pdf"), b""))
        .await;
    assert_eq!(empty.status, StatusCode::UNPROCESSABLE_ENTITY);

    // Over the configured limit (4096 bytes in the test configuration).
    let big = support::tiny_pdf(&"padding ".repeat(1000));
    assert!(big.len() > 4096);
    let too_large = app
        .send(client.upload_request(&uri, "big.pdf", Some("application/pdf"), &big))
        .await;
    assert_eq!(too_large.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(too_large.error_code(), "payload_too_large");

    // PNG and JPEG are accepted in 1A.
    let png = app
        .send(client.upload_request(&uri, "schema.png", Some("image/png"), &support::tiny_png()))
        .await;
    assert_eq!(png.status, StatusCode::CREATED, "{}", png.text());
    assert_eq!(png.json()["media_type"], "image/png");

    let jpeg = app
        .send(client.upload_request(&uri, "photo.jpg", None, &support::tiny_jpeg()))
        .await;
    assert_eq!(jpeg.status, StatusCode::CREATED, "{}", jpeg.text());
    assert_eq!(jpeg.json()["media_type"], "image/jpeg");

    // Nothing was stored for the rejected uploads: only the two accepted images exist.
    let materials = app.send(client.get(&uri)).await;
    assert_eq!(materials.json()["items"].as_array().unwrap().len(), 2);

    app.cleanup().await;
}

#[tokio::test]
async fn upload_requires_exactly_one_file_part_and_valid_multipart() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let uri = format!("/api/partners/{partner}/materials");

    // No `file` part at all.
    let no_file = app
        .send(client.raw_multipart_request(
            &uri,
            &[MultipartPart {
                name: "note",
                filename: None,
                content_type: None,
                bytes: b"hello".to_vec(),
            }],
        ))
        .await;
    assert_eq!(no_file.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(no_file.error_code(), "validation_failed");

    // Two file parts in one request.
    let two_files = app
        .send(client.raw_multipart_request(
            &uri,
            &[
                MultipartPart {
                    name: "file",
                    filename: Some("a.pdf"),
                    content_type: Some("application/pdf"),
                    bytes: support::tiny_pdf("a"),
                },
                MultipartPart {
                    name: "file",
                    filename: Some("b.pdf"),
                    content_type: Some("application/pdf"),
                    bytes: support::tiny_pdf("b"),
                },
            ],
        ))
        .await;
    assert_eq!(two_files.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(two_files.error_code(), "validation_failed");
    // Neither file became a material.
    let materials = app.send(client.get(&uri)).await;
    assert!(materials.json()["items"].as_array().unwrap().is_empty());

    // A body that is not multipart at all still answers in the JSON envelope
    // (regression: the bare extractor used to answer text/plain).
    let not_multipart = app
        .send(
            Request::builder()
                .method(Method::POST)
                .uri(&uri)
                .header(COOKIE, &client.cookie)
                .header("x-csrf-token", &client.csrf_token)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from("{\"file\":\"nope\"}"))
                .unwrap(),
        )
        .await;
    assert!(
        not_multipart.status.is_client_error(),
        "status was {}",
        not_multipart.status
    );
    assert_eq!(not_multipart.error_code(), "bad_request");

    // Missing boundary in the content type: same envelope.
    let no_boundary = app
        .send(
            Request::builder()
                .method(Method::POST)
                .uri(&uri)
                .header(COOKIE, &client.cookie)
                .header("x-csrf-token", &client.csrf_token)
                .header(CONTENT_TYPE, "multipart/form-data")
                .body(Body::from(support::multipart_body(&[MultipartPart {
                    name: "file",
                    filename: Some("a.pdf"),
                    content_type: Some("application/pdf"),
                    bytes: support::tiny_pdf("a"),
                }])))
                .unwrap(),
        )
        .await;
    assert!(no_boundary.status.is_client_error());
    assert_eq!(no_boundary.error_code(), "bad_request");

    app.cleanup().await;
}

#[tokio::test]
async fn original_is_served_only_through_its_own_partner() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let other_partner = app.create_partner(&client, "Other partner").await;

    let pdf = support::tiny_pdf("download");
    let created = app
        .send(client.upload_request(
            &format!("/api/partners/{partner}/materials"),
            "каталог.pdf",
            Some("application/pdf"),
            &pdf,
        ))
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    let material_id = created.json()["id"].as_str().unwrap().to_owned();

    let download = app
        .send(client.get(&format!(
            "/api/partners/{partner}/materials/{material_id}/original"
        )))
        .await;
    assert_eq!(download.status, StatusCode::OK, "{}", download.text());
    assert_eq!(download.body, pdf);
    assert_eq!(
        download.headers.get(CONTENT_TYPE).unwrap(),
        "application/pdf"
    );
    assert_eq!(
        download.headers.get(CONTENT_LENGTH).unwrap(),
        &pdf.len().to_string()
    );
    assert_eq!(
        download.headers.get("x-content-type-options").unwrap(),
        "nosniff"
    );
    let disposition = download
        .headers
        .get(CONTENT_DISPOSITION)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(disposition.starts_with("attachment;"), "{disposition}");
    assert!(disposition.contains("filename*=UTF-8''"), "{disposition}");

    // The same material id under a different partner of the same bureau: 404.
    let wrong_partner = app
        .send(client.get(&format!(
            "/api/partners/{other_partner}/materials/{material_id}/original"
        )))
        .await;
    assert_eq!(wrong_partner.status, StatusCode::NOT_FOUND);
    assert_eq!(wrong_partner.error_code(), "not_found");

    // Unknown material id.
    let unknown = app
        .send(client.get(&format!(
            "/api/partners/{partner}/materials/{}/original",
            uuid::Uuid::new_v4()
        )))
        .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);

    // Downloads need a session, like everything else.
    let anonymous = app
        .send(
            Request::builder()
                .uri(format!(
                    "/api/partners/{partner}/materials/{material_id}/original"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}

#[tokio::test]
async fn retry_is_refused_for_a_queued_material_and_stays_idempotent() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let other_partner = app.create_partner(&client, "Other partner").await;

    let created = app
        .send(client.upload_request(
            &format!("/api/partners/{partner}/materials"),
            "catalogue.pdf",
            Some("application/pdf"),
            &support::tiny_pdf("retry"),
        ))
        .await;
    let material_id = created.json()["id"].as_str().unwrap().to_owned();

    // Freshly queued: retry would be a lie, so it is a conflict.
    let queued_retry = app
        .send(client.json_request(
            Method::POST,
            &format!("/api/partners/{partner}/materials/{material_id}/retry"),
            serde_json::json!({}),
        ))
        .await;
    assert_eq!(queued_retry.status, StatusCode::CONFLICT);
    assert_eq!(queued_retry.error_code(), "conflict");

    // Simulate a failed extraction (what the 1B worker would record). The fixture runs
    // inside the bureau's row-level-security context and asserts that it really changed
    // a row — without the context these updates would silently match nothing.
    let material_uuid = uuid::Uuid::parse_str(&material_id).unwrap();
    app.admin_update(
        "UPDATE otdel.materials SET status = 'failed', error = 'boom' WHERE id = $1",
        material_uuid,
        1,
    )
    .await;
    app.admin_update(
        "UPDATE otdel.jobs SET status = 'failed', attempts = 2, error = 'boom' WHERE material_id = $1",
        material_uuid,
        1,
    )
    .await;

    // The API reflects the failed state before the retry.
    let before = app
        .send(client.get(&format!("/api/partners/{partner}/materials")))
        .await;
    assert_eq!(before.json()["items"][0]["status"], "failed");
    assert_eq!(before.json()["items"][0]["error"], "boom");

    let retried = app
        .send(client.json_request(
            Method::POST,
            &format!("/api/partners/{partner}/materials/{material_id}/retry"),
            serde_json::json!({}),
        ))
        .await;
    assert_eq!(retried.status, StatusCode::OK, "{}", retried.text());
    assert_eq!(retried.json()["status"], "queued");
    assert!(retried.json()["error"].is_null());

    // The job was reused, not duplicated, and its attempt history is preserved.
    let jobs = app
        .send(client.get(&format!("/api/partners/{partner}/jobs")))
        .await;
    let items = jobs.json()["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 1, "retry must not create a second job");
    assert_eq!(items[0]["status"], "queued");
    assert_eq!(items[0]["attempts"], 2);
    assert!(items[0]["error"].is_null());

    // Retrying again now conflicts (it is queued), which is the idempotent answer.
    let again = app
        .send(client.json_request(
            Method::POST,
            &format!("/api/partners/{partner}/materials/{material_id}/retry"),
            serde_json::json!({}),
        ))
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    let jobs = app
        .send(client.get(&format!("/api/partners/{partner}/jobs")))
        .await;
    assert_eq!(jobs.json()["items"].as_array().unwrap().len(), 1);

    // Retry through the wrong partner is a 404, never someone else's job.
    let wrong_partner = app
        .send(client.json_request(
            Method::POST,
            &format!("/api/partners/{other_partner}/materials/{material_id}/retry"),
            serde_json::json!({}),
        ))
        .await;
    assert_eq!(wrong_partner.status, StatusCode::NOT_FOUND);

    app.cleanup().await;
}

#[tokio::test]
async fn material_and_job_routes_reject_unknown_partners() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let unknown = uuid::Uuid::new_v4();

    for uri in [
        format!("/api/partners/{unknown}/materials"),
        format!("/api/partners/{unknown}/jobs"),
    ] {
        let response = app.send(client.get(&uri)).await;
        assert_eq!(response.status, StatusCode::NOT_FOUND, "{uri}");
    }

    let upload = app
        .send(client.upload_request(
            &format!("/api/partners/{unknown}/materials"),
            "catalogue.pdf",
            Some("application/pdf"),
            &support::tiny_pdf("unknown-partner"),
        ))
        .await;
    assert_eq!(upload.status, StatusCode::NOT_FOUND);
    assert_eq!(upload.error_code(), "not_found");

    // Nothing was written to storage for a partner that does not exist.
    let listing = app.state.store.list_objects(10).await.unwrap();
    assert!(listing.keys.is_empty(), "{:?}", listing.keys);

    app.cleanup().await;
}
