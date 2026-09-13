//! The durable queue under the extraction worker.
//!
//! These checks are about the properties that make an interrupted run safe: a job is
//! held by exactly one worker, a worker that lost its lease cannot write results, a
//! permanent failure stops instead of spinning, and an expired lease comes back to the
//! queue without a human.

mod support;

use std::time::Duration;

use axum::http::StatusCode;
use otdel_core::model::{JobKind, JobStatus};
use otdel_db::{jobs, pages};
use otdel_extract::fixtures;
use support::{TestApp, TestClient};
use uuid::Uuid;

const LEASE: Duration = Duration::from_secs(120);

async fn queued_material(app: &TestApp, client: &TestClient) -> (Uuid, Uuid) {
    let partner = app.create_partner(client, "BASIS").await;
    let response = app
        .send(client.upload_request(
            &format!("/api/partners/{partner}/materials"),
            "catalogue.pdf",
            Some("application/pdf"),
            &fixtures::text_pdf(),
        ))
        .await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.text());
    let material_id = Uuid::parse_str(response.json()["id"].as_str().unwrap()).unwrap();
    (partner, material_id)
}

#[tokio::test]
async fn a_claimed_job_is_not_offered_to_a_second_worker() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let (_partner, material_id) = queued_material(&app, &client).await;

    let mut tx = app.state.db.begin_scoped(app.bureau_id).await.unwrap();
    let first = jobs::claim_next(&mut tx, "worker-a", LEASE, &JobKind::extraction_kinds())
        .await
        .unwrap()
        .expect("the queued job must be claimable");
    tx.commit().await.unwrap();

    assert_eq!(first.material_id, material_id);
    assert_eq!(first.status, JobStatus::Running);
    assert_eq!(first.kind, JobKind::ExtractDocument);
    // The attempt is counted at claim time, so a worker that dies still burns one.
    assert_eq!(first.attempts, 1);

    let mut tx = app.state.db.begin_scoped(app.bureau_id).await.unwrap();
    let second = jobs::claim_next(&mut tx, "worker-b", LEASE, &JobKind::extraction_kinds())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(
        second.is_none(),
        "a leased job must not be handed out twice"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn a_worker_that_lost_its_lease_cannot_settle_the_job() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let (_partner, _material_id) = queued_material(&app, &client).await;

    let mut tx = app.state.db.begin_scoped(app.bureau_id).await.unwrap();
    let job = jobs::claim_next(&mut tx, "worker-a", LEASE, &JobKind::extraction_kinds())
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = app.state.db.begin_scoped(app.bureau_id).await.unwrap();
    assert!(
        jobs::heartbeat(&mut tx, job.id, "worker-a", LEASE, Some("page 1/1"))
            .await
            .unwrap(),
        "the holder renews its own lease"
    );
    // Another worker cannot renew, complete or fail a job it does not hold.
    assert!(!jobs::heartbeat(&mut tx, job.id, "worker-b", LEASE, None)
        .await
        .unwrap());
    assert!(!jobs::complete(&mut tx, job.id, "worker-b").await.unwrap());
    assert!(!jobs::fail(
        &mut tx,
        job.id,
        "worker-b",
        "boom",
        false,
        Duration::from_secs(1)
    )
    .await
    .unwrap());
    tx.commit().await.unwrap();

    app.cleanup().await;
}

#[tokio::test]
async fn a_permanent_failure_stops_and_a_transient_one_is_scheduled_again() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let (partner, _material_id) = queued_material(&app, &client).await;

    // Transient: back to the queue, but not before the backoff has passed.
    let mut tx = app.state.db.begin_scoped(app.bureau_id).await.unwrap();
    let job = jobs::claim_next(&mut tx, "worker-a", LEASE, &JobKind::extraction_kinds())
        .await
        .unwrap()
        .unwrap();
    assert!(jobs::fail(
        &mut tx,
        job.id,
        "worker-a",
        "хранилище недоступно",
        false,
        Duration::from_secs(3600)
    )
    .await
    .unwrap());
    let listed = jobs::list_for_partner(&mut tx, partner).await.unwrap();
    assert_eq!(listed[0].status, JobStatus::Queued);
    assert_eq!(listed[0].error.as_deref(), Some("хранилище недоступно"));

    // Not runnable yet: the backoff is real, not cosmetic.
    assert!(
        jobs::claim_next(&mut tx, "worker-b", LEASE, &JobKind::extraction_kinds())
            .await
            .unwrap()
            .is_none()
    );
    tx.commit().await.unwrap();

    // Permanent: failed, and never handed out again.
    let mut tx = app.state.db.begin_scoped(app.bureau_id).await.unwrap();
    sqlx::query("UPDATE otdel.jobs SET run_after = now() WHERE id = $1")
        .bind(job.id)
        .execute(tx.conn())
        .await
        .unwrap();
    let job = jobs::claim_next(&mut tx, "worker-a", LEASE, &JobKind::extraction_kinds())
        .await
        .unwrap()
        .expect("runnable again once the backoff passed");
    assert_eq!(job.attempts, 2);
    assert!(jobs::fail(
        &mut tx,
        job.id,
        "worker-a",
        "файл не открывается как PDF",
        true,
        Duration::from_secs(0)
    )
    .await
    .unwrap());
    let listed = jobs::list_for_partner(&mut tx, partner).await.unwrap();
    assert_eq!(listed[0].status, JobStatus::Failed);
    assert!(
        jobs::claim_next(&mut tx, "worker-b", LEASE, &JobKind::extraction_kinds())
            .await
            .unwrap()
            .is_none()
    );
    tx.commit().await.unwrap();

    app.cleanup().await;
}

#[tokio::test]
async fn an_expired_lease_returns_the_job_to_the_queue() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let (_partner, _material_id) = queued_material(&app, &client).await;

    let mut tx = app.state.db.begin_scoped(app.bureau_id).await.unwrap();
    let job = jobs::claim_next(&mut tx, "worker-a", LEASE, &JobKind::extraction_kinds())
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();

    // Simulate the worker dying: its lease runs out.
    app.admin_update(
        "UPDATE otdel.jobs SET lease_expires_at = now() - interval '1 minute' WHERE id = $1",
        job.id,
        1,
    )
    .await;

    let mut tx = app.state.db.begin_scoped(app.bureau_id).await.unwrap();
    let reclaimed = jobs::reclaim_expired_leases(&mut tx).await.unwrap();
    assert_eq!(reclaimed, 1);
    let taken = jobs::claim_next(&mut tx, "worker-b", LEASE, &JobKind::extraction_kinds())
        .await
        .unwrap()
        .expect("the interrupted job must become runnable again");
    assert_eq!(taken.id, job.id);
    assert_eq!(taken.attempts, 2);
    tx.commit().await.unwrap();

    app.cleanup().await;
}

#[tokio::test]
async fn a_page_job_carries_its_page_and_reuses_one_row() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let (partner, material_id) = queued_material(&app, &client).await;

    // Give the material a page to point at.
    app.run_worker(&app.extractor()).await;

    let mut tx = app.state.db.begin_scoped(app.bureau_id).await.unwrap();
    let first = jobs::enqueue_page_extraction(&mut tx, partner, material_id, 1)
        .await
        .unwrap();
    let second = jobs::enqueue_page_extraction(&mut tx, partner, material_id, 1)
        .await
        .unwrap();
    let other_page = jobs::enqueue_page_extraction(&mut tx, partner, material_id, 2)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(first.id, second.id, "the same page reuses its job row");
    assert_ne!(
        first.id, other_page.id,
        "a different page is a different job"
    );
    assert_eq!(first.kind, JobKind::ExtractPage);
    assert_eq!(first.page_number, Some(1));
    assert_eq!(other_page.page_number, Some(2));
    assert_eq!(second.status, JobStatus::Queued);

    app.cleanup().await;
}

#[tokio::test]
async fn resetting_a_page_clears_its_text_but_keeps_the_row() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let (_partner, material_id) = queued_material(&app, &client).await;
    app.run_worker(&app.extractor()).await;

    let mut tx = app.state.db.begin_scoped(app.bureau_id).await.unwrap();
    let before = pages::find_page(&mut tx, material_id, 1)
        .await
        .unwrap()
        .expect("the page exists after a run");
    assert!(before.char_count > 0);

    let after = pages::reset_page(&mut tx, material_id, 1)
        .await
        .unwrap()
        .expect("resetting keeps the row");
    tx.commit().await.unwrap();

    assert_eq!(after.id, before.id, "the page identity survives a retry");
    assert_eq!(after.char_count, 0);
    assert_eq!(
        after.status,
        otdel_core::extraction::PageStatus::Pending,
        "a reset page waits to be read, it is not empty"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn a_character_the_database_cannot_hold_costs_one_character_not_the_document() {
    // Found on a real catalogue: a font names its glyphs in a form the parser's table
    // does not know, the affected code points decode to U+0000, and PostgreSQL refuses
    // `U+0000` in a `text` column. Before the guard this failed the whole 32-page job
    // with a database error.
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let (partner, material_id) = queued_material(&app, &client).await;
    app.run_worker(&app.extractor()).await;

    let outcome = pages::PageOutcomeRow {
        page_number: 1,
        status: otdel_core::extraction::PageStatus::Extracted,
        text_source: otdel_core::extraction::TextSource::TextLayer,
        text: Some("Про\u{0}филь ВР\u{0}21".to_owned()),
        char_count: 12,
        word_count: 2,
        image_count: 0,
        width_pt: Some(595.0),
        height_pt: Some(842.0),
        rotation: 0,
        parser_name: Some("pdf-extract".to_owned()),
        parser_version: Some("0.12".to_owned()),
        ocr_engine: None,
        ocr_version: None,
        ocr_language: None,
        duration_ms: Some(3),
        diagnostic: Some("частично\u{0}декодировано".to_owned()),
    };
    let region = pages::NewRegion {
        kind: otdel_core::extraction::RegionKind::Table,
        text: "таблица\u{0}".to_owned(),
        source: otdel_core::extraction::TextSource::TextLayer,
        bbox: None,
        row_count: Some(1),
        column_count: Some(1),
        cells: vec![pages::NewCell {
            row_index: 0,
            column_index: 0,
            is_header: false,
            raw_text: "12\u{0}00".to_owned(),
            value_kind: otdel_core::extraction::CellValueKind::Text,
            unit: Some("м\u{0}м".to_owned()),
            column_header: Some("Длина\u{0}".to_owned()),
            bbox: None,
        }],
    };

    let mut tx = app.state.db.begin_scoped(app.bureau_id).await.unwrap();
    pages::record_outcome(&mut tx, partner, material_id, &outcome, &[region])
        .await
        .expect("an undecodable character must not fail the write");
    let detail = pages::page_detail(&mut tx, material_id, 1)
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();

    // Stored, minus exactly the characters that are not text.
    assert_eq!(detail.text.as_deref(), Some("Профиль ВР21"));
    assert!(!detail.page.diagnostic.unwrap().contains('\u{0}'));
    assert_eq!(detail.regions[0].cells[0].raw_text, "1200");
    assert_eq!(detail.regions[0].cells[0].unit.as_deref(), Some("мм"));

    app.cleanup().await;
}
