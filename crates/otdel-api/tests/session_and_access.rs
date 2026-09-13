//! Authentication, CSRF and cross-bureau access.
//!
//! Needs a real database: see `tests/support/mod.rs` for the environment variables and
//! `docs/backend-1a.md` for how to provide them.

mod support;

use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use axum::http::{Method, Request, StatusCode};
use support::{TestApp, OWNER_PASSWORD};

#[tokio::test]
async fn health_and_ready_report_dependencies() {
    let app = TestApp::start().await;

    let health = app
        .send(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(health.status, StatusCode::OK);
    assert_eq!(health.json()["status"], "ok");

    let ready = app
        .send(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(ready.status, StatusCode::OK, "{}", ready.text());
    let body = ready.json();
    assert_eq!(body["status"], "ready");
    assert_eq!(body["checks"]["database"], "ok");
    assert_eq!(body["checks"]["object_store"], "ok");
    assert_eq!(body["checks"]["bureau"], "provisioned");
    // Honest reporting: whatever the state of pgvector is, it is named, not implied.
    assert!(
        ["installed", "available_not_installed", "absent", "unknown"]
            .contains(&body["checks"]["pgvector"].as_str().unwrap()),
        "unexpected pgvector status: {body}"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn sign_in_sets_a_hardened_cookie_and_returns_a_csrf_token() {
    let app = TestApp::start().await;

    let response = app
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

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.json()["authenticated"], true);
    let csrf_token = response.json()["csrf_token"].as_str().unwrap().to_owned();
    assert!(csrf_token.len() >= 20);

    let cookie = response
        .headers
        .get(SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Strict"), "{cookie}");
    assert!(cookie.contains("Max-Age="), "{cookie}");
    // The opaque token must not be the CSRF token, and neither may appear in the body
    // in the other's place.
    assert!(!cookie.contains(&csrf_token), "{cookie}");

    // GET restores the CSRF token after a reload.
    let restored = app
        .send(
            Request::builder()
                .uri("/api/session")
                .header(COOKIE, cookie.split(';').next().unwrap())
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(restored.status, StatusCode::OK);
    assert_eq!(restored.json()["csrf_token"], csrf_token);

    app.cleanup().await;
}

#[tokio::test]
async fn wrong_password_is_rejected_without_hints() {
    let app = TestApp::start().await;

    let response = app
        .send(
            Request::builder()
                .method(Method::POST)
                .uri("/api/session")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({ "password": "not-the-password" }).to_string(),
                ))
                .unwrap(),
        )
        .await;

    assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    assert_eq!(response.error_code(), "unauthorized");
    let message = response.json()["error"]["message"]
        .as_str()
        .unwrap()
        .to_owned();
    // No hint about hashes, users, configuration or the password itself.
    for leak in ["argon2", "hash", "$", "owner_password", "not-the-password"] {
        assert!(
            !message.to_lowercase().contains(leak),
            "message leaks `{leak}`: {message}"
        );
    }
    assert!(response.headers.get(SET_COOKIE).is_none());

    app.cleanup().await;
}

#[tokio::test]
async fn protected_endpoints_require_a_session() {
    let app = TestApp::start().await;

    for (method, uri) in [
        (Method::GET, "/api/partners"),
        (Method::POST, "/api/partners"),
        (Method::GET, "/api/session"),
    ] {
        let response = app
            .send(
                Request::builder()
                    .method(method.clone())
                    .uri(uri)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from("{\"name\":\"x\"}"))
                    .unwrap(),
            )
            .await;
        assert_eq!(
            response.status,
            StatusCode::UNAUTHORIZED,
            "{method} {uri} must require a session"
        );
        assert_eq!(response.error_code(), "unauthorized");
    }

    // An invalid cookie is the same 401, not a different error.
    let response = app
        .send(
            Request::builder()
                .uri("/api/partners")
                .header(COOKIE, "otdel_session=not-a-real-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(response.status, StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}

#[tokio::test]
async fn state_changing_requests_need_a_matching_csrf_token() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;

    // Missing header.
    let missing = app
        .send(
            Request::builder()
                .method(Method::POST)
                .uri("/api/partners")
                .header(COOKIE, &client.cookie)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from("{\"name\":\"BASIS\"}"))
                .unwrap(),
        )
        .await;
    assert_eq!(missing.status, StatusCode::FORBIDDEN);
    assert_eq!(missing.error_code(), "invalid_csrf_token");

    // Wrong value.
    let wrong = app
        .send(
            Request::builder()
                .method(Method::POST)
                .uri("/api/partners")
                .header(COOKIE, &client.cookie)
                .header(CONTENT_TYPE, "application/json")
                .header("x-csrf-token", "wrong-token")
                .body(Body::from("{\"name\":\"BASIS\"}"))
                .unwrap(),
        )
        .await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    assert_eq!(wrong.error_code(), "invalid_csrf_token");

    // A foreign origin is refused even with the right token.
    let foreign_origin = app
        .send(
            Request::builder()
                .method(Method::POST)
                .uri("/api/partners")
                .header(COOKIE, &client.cookie)
                .header(CONTENT_TYPE, "application/json")
                .header("x-csrf-token", &client.csrf_token)
                .header("origin", "http://evil.test")
                .body(Body::from("{\"name\":\"BASIS\"}"))
                .unwrap(),
        )
        .await;
    assert_eq!(foreign_origin.status, StatusCode::FORBIDDEN);
    assert_eq!(foreign_origin.error_code(), "forbidden");

    // With the token (and no hostile origin) it works.
    let accepted = app
        .send(client.json_request(
            Method::POST,
            "/api/partners",
            serde_json::json!({ "name": "BASIS" }),
        ))
        .await;
    assert_eq!(accepted.status, StatusCode::CREATED, "{}", accepted.text());

    // Read-only requests do not need the token.
    let list = app.send(client.get("/api/partners")).await;
    assert_eq!(list.status, StatusCode::OK);
    assert_eq!(list.json()["items"].as_array().unwrap().len(), 1);

    app.cleanup().await;
}

#[tokio::test]
async fn sign_out_invalidates_the_session() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;

    let logout = app
        .send(
            Request::builder()
                .method(Method::DELETE)
                .uri("/api/session")
                .header(COOKIE, &client.cookie)
                .header("x-csrf-token", &client.csrf_token)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(logout.status, StatusCode::OK);
    assert_eq!(logout.json()["authenticated"], false);
    assert!(logout
        .headers
        .get(SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));

    // The cookie is now worthless.
    let after = app.send(client.get("/api/partners")).await;
    assert_eq!(after.status, StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}

#[tokio::test]
async fn sign_out_itself_requires_the_csrf_token() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;

    let response = app
        .send(
            Request::builder()
                .method(Method::DELETE)
                .uri("/api/session")
                .header(COOKIE, &client.cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(response.status, StatusCode::FORBIDDEN);
    assert_eq!(response.error_code(), "invalid_csrf_token");

    // The session still works afterwards.
    assert_eq!(
        app.send(client.get("/api/partners")).await.status,
        StatusCode::OK
    );

    app.cleanup().await;
}

#[tokio::test]
async fn a_session_cannot_reach_another_bureau_data() {
    let app = TestApp::start().await;
    let client = app.sign_in().await;

    // A partner that belongs to a different bureau entirely.
    let (other_bureau, _slug) = support::new_bureau_as_admin(&app.admin_pool).await;
    let foreign_partner =
        support::insert_partner_as_admin(&app.admin_pool, other_bureau, "Foreign partner").await;

    let listed = app.send(client.get("/api/partners")).await;
    assert_eq!(listed.status, StatusCode::OK);
    assert!(
        listed.json()["items"].as_array().unwrap().is_empty(),
        "another bureau's partner must not be listed: {}",
        listed.text()
    );

    for uri in [
        format!("/api/partners/{foreign_partner}"),
        format!("/api/partners/{foreign_partner}/materials"),
        format!("/api/partners/{foreign_partner}/jobs"),
    ] {
        let response = app.send(client.get(&uri)).await;
        assert_eq!(response.status, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(response.error_code(), "not_found");
    }

    // Patching it is equally invisible (404, not 403: no existence oracle).
    let patch = app
        .send(client.json_request(
            Method::PATCH,
            &format!("/api/partners/{foreign_partner}"),
            serde_json::json!({ "name": "taken over" }),
        ))
        .await;
    assert_eq!(patch.status, StatusCode::NOT_FOUND);

    support::delete_bureau_as_admin(&app.admin_pool, other_bureau).await;
    app.cleanup().await;
}

#[tokio::test]
async fn unknown_routes_and_methods_use_the_error_envelope() {
    let app = TestApp::start().await;

    let missing = app
        .send(
            Request::builder()
                .uri("/api/nothing")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.error_code(), "not_found");

    let wrong_method = app
        .send(
            Request::builder()
                .method(Method::PUT)
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(wrong_method.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(wrong_method.error_code(), "method_not_allowed");

    app.cleanup().await;
}
