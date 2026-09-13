//! Database-level tenant isolation, the runtime-role refusal, job/partner consistency
//! and the concurrent-upload regression.

mod support;

use axum::http::StatusCode;
use otdel_db::Database;
use sqlx::{Executor, PgPool, Row};
use support::TestApp;
use uuid::Uuid;

/// Row-level security, verified against the *runtime role* itself rather than through
/// the handlers: even a hand-written query with a foreign `bureau_id` sees nothing.
#[tokio::test]
async fn row_level_security_isolates_bureaus_for_the_runtime_role() {
    let runtime_url = support::require_env("OTDEL_TEST_DATABASE_URL");
    let admin_url = support::require_env("OTDEL_TEST_ADMIN_DATABASE_URL");

    let admin_pool = PgPool::connect(&admin_url).await.expect("admin connection");
    let runtime_pool = PgPool::connect(&runtime_url)
        .await
        .expect("runtime connection");

    let (bureau_a, _) = support::new_bureau_as_admin(&admin_pool).await;
    let (bureau_b, _) = support::new_bureau_as_admin(&admin_pool).await;
    let partner_a = support::insert_partner_as_admin(&admin_pool, bureau_a, "A").await;
    let partner_b = support::insert_partner_as_admin(&admin_pool, bureau_b, "B").await;

    // Scoped to A: sees A, not B.
    let mut tx = support::set_bureau_context(&runtime_pool, bureau_a).await;
    let visible: Vec<Uuid> = sqlx::query("SELECT id FROM otdel.partners")
        .fetch_all(&mut *tx)
        .await
        .expect("select partners")
        .iter()
        .map(|row| row.get::<Uuid, _>("id"))
        .collect();
    assert!(visible.contains(&partner_a));
    assert!(
        !visible.contains(&partner_b),
        "bureau B's partner is visible to a session scoped to bureau A"
    );

    // Explicitly asking for B's row still returns nothing.
    let direct = sqlx::query("SELECT id FROM otdel.partners WHERE id = $1")
        .bind(partner_b)
        .fetch_optional(&mut *tx)
        .await
        .expect("select by id");
    assert!(direct.is_none(), "RLS must hide another bureau's row by id");

    // Writing a row into another bureau is rejected by the policy's WITH CHECK.
    let forbidden_insert =
        sqlx::query("INSERT INTO otdel.partners (bureau_id, name) VALUES ($1, $2)")
            .bind(bureau_b)
            .bind("smuggled")
            .execute(&mut *tx)
            .await;
    assert!(
        forbidden_insert.is_err(),
        "inserting into another bureau must be refused"
    );
    tx.rollback().await.ok();

    // With no context at all, nothing is visible (fail closed).
    let mut plain = runtime_pool.begin().await.unwrap();
    let rows = sqlx::query("SELECT count(*) AS total FROM otdel.partners")
        .fetch_one(&mut *plain)
        .await
        .expect("count");
    assert_eq!(
        rows.get::<i64, _>("total"),
        0,
        "without a bureau context the runtime role must see no rows"
    );
    plain.rollback().await.ok();

    // The runtime role cannot touch sessions directly at all.
    let sessions = runtime_pool
        .execute(sqlx::query("SELECT count(*) FROM otdel.sessions"))
        .await;
    assert!(
        sessions.is_err(),
        "the runtime role must not be able to read otdel.sessions"
    );

    support::delete_bureau_as_admin(&admin_pool, bureau_a).await;
    support::delete_bureau_as_admin(&admin_pool, bureau_b).await;
    admin_pool.close().await;
    runtime_pool.close().await;
}

/// Migrations must be re-runnable.
///
/// Regression for a real failure: SQLx records applied versions in an *unqualified*
/// `_sqlx_migrations` table, so the migration role's default `search_path`
/// (`otdel, public`) put the history in `public` on the first run and then looked for it
/// in `otdel` on the second — finding an empty history and replaying the DDL
/// (“function current_bureau_id already exists”). The connection now pins
/// `search_path`, and this test runs the migrator repeatedly to prove it.
#[tokio::test]
async fn migrations_can_be_applied_repeatedly() {
    let admin_url = support::require_env("OTDEL_TEST_ADMIN_DATABASE_URL");

    for attempt in 1..=3 {
        otdel_db::run_migrations(&admin_url)
            .await
            .unwrap_or_else(|error| panic!("migration attempt {attempt} failed: {error}"));
    }

    // Exactly one history table, in `public`, with both migrations recorded.
    let admin_pool = PgPool::connect(&admin_url).await.expect("admin connection");
    let history_tables: i64 = sqlx::query(
        "SELECT count(*) AS total FROM pg_tables \
          WHERE tablename = '_sqlx_migrations' AND schemaname IN ('public', 'otdel')",
    )
    .fetch_one(&admin_pool)
    .await
    .unwrap()
    .get("total");
    assert_eq!(
        history_tables, 1,
        "there must be exactly one _sqlx_migrations table (the canonical one in public)"
    );

    let applied: i64 = sqlx::query("SELECT count(*) AS total FROM public._sqlx_migrations")
        .fetch_one(&admin_pool)
        .await
        .unwrap()
        .get("total");
    assert!(
        applied >= 2,
        "both migrations must be recorded, found {applied}"
    );
    admin_pool.close().await;
}

/// The API refuses to start with a privileged role (review finding 1).
#[tokio::test]
async fn privileged_database_roles_are_refused_at_startup() {
    // The restricted role passes.
    let runtime_url = support::require_env("OTDEL_TEST_DATABASE_URL");
    let runtime = Database::connect(&runtime_url, 2).await.expect("connect");
    runtime
        .verify_runtime_role()
        .await
        .expect("the restricted runtime role must be accepted");
    runtime.close().await;

    // The migration role owns the tables: refused.
    let admin_url = support::require_env("OTDEL_TEST_ADMIN_DATABASE_URL");
    let admin = Database::connect(&admin_url, 2).await.expect("connect");
    let error = admin
        .verify_runtime_role()
        .await
        .expect_err("the schema owner must be refused as a runtime role");
    let message = error.to_string();
    assert!(
        message.contains("owns tables") || message.contains("row-level security"),
        "unexpected refusal reason: {message}"
    );
    admin.close().await;

    // The superuser, when the environment provides it.
    if let Some(superuser_url) = support::optional_env("OTDEL_TEST_SUPERUSER_URL") {
        let superuser = Database::connect(&superuser_url, 2).await.expect("connect");
        let error = superuser
            .verify_runtime_role()
            .await
            .expect_err("a superuser must be refused as a runtime role");
        assert!(
            error.to_string().contains("SUPERUSER")
                || error.to_string().contains("BYPASSRLS")
                || error.to_string().contains("row-level security"),
            "unexpected refusal reason: {error}"
        );
        superuser.close().await;
    }
}

/// A job may not name one partner while pointing at another partner's material, even
/// inside the same bureau (review finding 2 — enforced by the composite foreign key).
#[tokio::test]
async fn a_job_cannot_mix_partner_and_material() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner_a = app.create_partner(&client, "Partner A").await;
    let partner_b = app.create_partner(&client, "Partner B").await;

    let created = app
        .send(client.upload_request(
            &format!("/api/partners/{partner_a}/materials"),
            "catalogue.pdf",
            Some("application/pdf"),
            &support::tiny_pdf("fk"),
        ))
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    let material_of_a = Uuid::parse_str(created.json()["id"].as_str().unwrap()).unwrap();

    // Run inside the *correct* bureau context, so row-level security cannot be what
    // rejects this: the failure must come from the composite foreign key itself.
    let mut tx = app.admin_tx().await;
    let mismatched = sqlx::query(
        "INSERT INTO otdel.jobs (bureau_id, partner_id, material_id, kind, status, idempotency_key) \
         VALUES ($1, $2, $3, 'extract_document', 'queued', $4)",
    )
    .bind(app.bureau_id)
    .bind(partner_b)
    .bind(material_of_a)
    .bind(format!("mismatch:{material_of_a}"))
    .execute(&mut *tx)
    .await;

    let error = mismatched
        .expect_err("a job naming partner B with a material of partner A must be rejected");
    assert_eq!(
        support::sqlstate(&error).as_deref(),
        Some("23503"),
        "the mismatch must be refused by the foreign key (23503), not by something else: {error}"
    );
    tx.rollback().await.ok();

    // A job that names the right partner for that material is accepted, which shows the
    // constraint above rejects the mismatch specifically and not every insert.
    let mut tx = app.admin_tx().await;
    sqlx::query(
        "INSERT INTO otdel.jobs (bureau_id, partner_id, material_id, kind, status, idempotency_key) \
         VALUES ($1, $2, $3, 'extract_document', 'queued', $4)",
    )
    .bind(app.bureau_id)
    .bind(partner_a)
    .bind(material_of_a)
    .bind(format!("matching:{material_of_a}"))
    .execute(&mut *tx)
    .await
    .expect("a job with the material's own partner must be accepted");
    tx.rollback().await.ok();

    // A material cannot be moved to another bureau: with the correct context set, the
    // write is refused outright rather than matching zero rows.
    let (other_bureau, _) = support::new_bureau_as_admin(&app.admin_pool).await;
    let mut tx = app.admin_tx().await;
    let moved = sqlx::query("UPDATE otdel.materials SET bureau_id = $1 WHERE id = $2")
        .bind(other_bureau)
        .bind(material_of_a)
        .execute(&mut *tx)
        .await;
    match moved {
        Err(error) => {
            let code = support::sqlstate(&error);
            assert!(
                matches!(code.as_deref(), Some("42501") | Some("23503")),
                "a material moved to another bureau must be refused by RLS (42501) or the \
                 foreign key (23503), got {code:?}: {error}"
            );
        }
        Ok(result) => panic!(
            "moving a material to another bureau was not refused ({} row(s) changed)",
            result.rows_affected()
        ),
    }
    tx.rollback().await.ok();

    // The material is still where it was.
    let mut tx = app.admin_tx().await;
    let owner: Uuid = sqlx::query("SELECT bureau_id FROM otdel.materials WHERE id = $1")
        .bind(material_of_a)
        .fetch_one(&mut *tx)
        .await
        .expect("the material must still be visible in its own bureau")
        .get("bureau_id");
    assert_eq!(owner, app.bureau_id);
    tx.rollback().await.ok();

    support::delete_bureau_as_admin(&app.admin_pool, other_bureau).await;
    app.cleanup().await;
}

/// Regression for the concurrent-upload cleanup race (review finding 4).
///
/// Two requests upload the identical file for the same partner. One of them fails after
/// the object was already published (a second `file` part). The accepted original must
/// remain readable: an error path must never delete a finalized, content-addressed
/// object, because the other request may legitimately have adopted the same bytes.
#[tokio::test]
async fn a_failed_upload_never_removes_an_accepted_original() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;
    let uri = format!("/api/partners/{partner}/materials");
    let pdf = support::tiny_pdf("shared-content");

    // Request A: the same bytes, but a second file part makes the request fail *after*
    // the object has been written.
    let failing = client.raw_multipart_request(
        &uri,
        &[
            support::MultipartPart {
                name: "file",
                filename: Some("catalogue.pdf"),
                content_type: Some("application/pdf"),
                bytes: pdf.clone(),
            },
            support::MultipartPart {
                name: "file",
                filename: Some("second.pdf"),
                content_type: Some("application/pdf"),
                bytes: support::tiny_pdf("second"),
            },
        ],
    );
    // Request B: the accepted upload of the same content.
    let accepted = client.upload_request(&uri, "catalogue.pdf", Some("application/pdf"), &pdf);

    let (failed_response, accepted_response) = tokio::join!(app.send(failing), app.send(accepted));

    assert_eq!(
        failed_response.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        failed_response.text()
    );
    assert_eq!(
        accepted_response.status,
        StatusCode::CREATED,
        "{}",
        accepted_response.text()
    );
    let material_id = accepted_response.json()["id"].as_str().unwrap().to_owned();

    // The accepted original is still fully readable afterwards.
    let download = app
        .send(client.get(&format!(
            "/api/partners/{partner}/materials/{material_id}/original"
        )))
        .await;
    assert_eq!(
        download.status,
        StatusCode::OK,
        "the accepted original must survive the other request's failure: {}",
        download.text()
    );
    assert_eq!(download.body, pdf, "the stored bytes must be intact");

    // Exactly one material exists for that content.
    let materials = app.send(client.get(&uri)).await;
    let items = materials.json()["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], material_id);

    app.cleanup().await;
}

/// The upload endpoint publishes metadata and the queue entry together; an interrupted
/// request must not leave a material without its job or the other way round.
#[tokio::test]
async fn material_and_job_are_published_together() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;

    for index in 0..3 {
        let response = app
            .send(client.upload_request(
                &format!("/api/partners/{partner}/materials"),
                &format!("catalogue-{index}.pdf"),
                Some("application/pdf"),
                &support::tiny_pdf(&format!("atomic-{index}")),
            ))
            .await;
        assert_eq!(response.status, StatusCode::CREATED);
    }

    let mut tx = app.admin_tx().await;
    let materials_total: i64 = sqlx::query("SELECT count(*) AS total FROM otdel.materials")
        .fetch_one(&mut *tx)
        .await
        .unwrap()
        .get("total");
    assert_eq!(
        materials_total, 3,
        "the fixture must be visible in its context"
    );

    let orphan_materials: i64 = sqlx::query(
        "SELECT count(*) AS total FROM otdel.materials m \
          WHERE NOT EXISTS (SELECT 1 FROM otdel.jobs j WHERE j.material_id = m.id)",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap()
    .get("total");
    assert_eq!(
        orphan_materials, 0,
        "every material must have its queue entry"
    );

    let orphan_jobs: i64 = sqlx::query(
        "SELECT count(*) AS total FROM otdel.jobs j \
          WHERE NOT EXISTS (SELECT 1 FROM otdel.materials m WHERE m.id = j.material_id)",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap()
    .get("total");
    assert_eq!(orphan_jobs, 0);
    tx.rollback().await.ok();

    app.cleanup().await;
}

/// Originals live under a per-bureau/per-partner namespace and are never reachable as a
/// path built from the uploaded file name.
#[tokio::test]
async fn stored_objects_are_namespaced_and_not_named_after_the_upload() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;
    let partner = app.create_partner(&client, "BASIS").await;

    let response = app
        .send(client.upload_request(
            &format!("/api/partners/{partner}/materials"),
            "../../../etc/passwd",
            Some("application/pdf"),
            &support::tiny_pdf("traversal"),
        ))
        .await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.text());
    // The name is kept for display only, with the path stripped.
    assert_eq!(response.json()["filename"], "passwd");

    let listing = app.state.store.list_objects(10).await.unwrap();
    assert_eq!(listing.keys.len(), 1);
    let key = &listing.keys[0];
    let namespace = key.namespace().unwrap();
    assert_eq!(namespace.bureau_id, app.bureau_id);
    assert_eq!(namespace.partner_id, partner);
    assert_eq!(key.digest(), response.json()["sha256"].as_str().unwrap());
    assert!(!key.as_str().contains("passwd"));
    assert!(!key.as_str().contains(".."));

    app.cleanup().await;
}
