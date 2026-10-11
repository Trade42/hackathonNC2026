//! A 5xx is logged once, at ERROR, with the request id the response carries in `x-request-id`,
//! and never with the request body (conformance task): a 500 from `ApiError::internal` (its line
//! has the cause), axum's own 500 and a 503 `upstream_unavailable` (one line each from the app's
//! 5xx log). The web app can read `x-request-id` across origins.
//!
//! This is the only test in its binary on purpose, like auth_logs.rs: the capture is a
//! thread-local subscriber, and tracing caches each callsite's interest when it is first hit, so a
//! callsite first hit on another test's thread can stay uninterested and its lines (here, the
//! request span with its id) would go missing.

mod common;
mod realtime_support;
mod runs_support;

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use axum::{Json, Router};
use ferry_core::app::{self, REQUEST_ID_HEADER};
use ferry_core::state::AppState;
use serde_json::json;
use sqlx::PgPool;

use runs_support::seed;

#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Logs {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }

    /// The ERROR lines of the request `request_id`.
    fn errors_of(&self, request_id: &str) -> Vec<String> {
        self.text()
            .lines()
            .filter(|l| l.contains(" ERROR ") && l.contains(&format!("request_id={request_id}")))
            .map(str::to_owned)
            .collect()
    }
}

/// A body serde_json refuses to write, so axum's `Json` answers its own plain-text 500, which no
/// `ApiError` logged.
struct Unserializable;

impl serde::Serialize for Unserializable {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("no JSON for this"))
    }
}

async fn axum_500() -> Json<Unserializable> {
    Json(Unserializable)
}

#[sqlx::test(migrations = "../migrations")]
async fn a_500_is_logged_once_with_its_request_id_and_never_the_body(pool: PgPool) {
    seed(&pool).await;
    let state = AppState::new(pool.clone(), common::config(&[("DEMO_MODE", "true")])).unwrap();
    // A real bug's shape: a table the handler needs is gone.
    sqlx::query("ALTER TABLE ferry.invites RENAME TO invites_gone")
        .execute(&pool)
        .await
        .unwrap();

    let logs = Logs::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let marker = "BODY-MARKER-7f3a";
    let request = Request::post("/v1/circles/cir_hinton4/meet-again")
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", state.jwt.issue_token("usr_dana").unwrap()),
        )
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"message": marker}).to_string()))
        .unwrap();
    let (status, headers, body) =
        common::send(ferry_core::app::router(state.clone()), request).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    common::assert_envelope(&body, "upstream_unavailable");
    assert!(
        !body.to_string().contains("invites"),
        "the cause stays hidden: {body}"
    );
    let request_id = headers[REQUEST_ID_HEADER].to_str().unwrap().to_owned();
    assert_eq!(request_id.len(), 16);

    let text = logs.text();
    let errors: Vec<&str> = text.lines().filter(|l| l.contains(" ERROR ")).collect();
    assert_eq!(errors.len(), 1, "exactly one ERROR line:\n{text}");
    assert!(
        errors[0].contains(&format!("request_id={request_id}")),
        "{}",
        errors[0]
    );
    assert!(
        errors[0].contains("invites"),
        "the log has the cause: {}",
        errors[0]
    );
    assert!(
        !text.contains(marker),
        "the body never reaches the log:\n{text}"
    );
    // The response line still records the status, with the same id.
    assert!(
        text.lines()
            .any(|l| l.contains(&format!("request_id={request_id}")) && l.contains("status=500")),
        "{text}"
    );

    // Every response carries its own id; a client's own is replaced, not echoed.
    let request = Request::get("/healthz")
        .header(REQUEST_ID_HEADER, "forged\u{1}id")
        .body(Body::empty());
    assert!(
        request.is_err(),
        "a control character is not even a header value"
    );
    let request = Request::get("/healthz")
        .header(REQUEST_ID_HEADER, "client-chosen")
        .body(Body::empty())
        .unwrap();
    let (_, headers, _) = common::send(ferry_core::app::router(state.clone()), request).await;
    let id = headers[REQUEST_ID_HEADER].to_str().unwrap();
    assert_ne!(id, "client-chosen");
    assert_ne!(id, request_id);

    // A 5xx no `ApiError::internal` built still gets exactly one ERROR line, with its request id
    // (conformance review, MINOR 3): axum's own 500, and a 503 `upstream_unavailable` (Web Push
    // without a VAPID pair).
    let request = Request::get("/boom").body(Body::empty()).unwrap();
    let router = app::router_with(state.clone(), Router::new().route("/boom", get(axum_500)));
    let response = tower::ServiceExt::oneshot(router, request).await.unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let id = response.headers()[REQUEST_ID_HEADER].to_str().unwrap();
    let errors = logs.errors_of(id);
    assert_eq!(errors.len(), 1, "one ERROR line:\n{}", logs.text());
    assert!(errors[0].contains("status=500"), "{}", errors[0]);

    let request = Request::get("/v1/public/push-key")
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = common::send(app::router(state.clone()), request).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    let id = headers[REQUEST_ID_HEADER].to_str().unwrap();
    let errors = logs.errors_of(id);
    assert_eq!(errors.len(), 1, "one ERROR line:\n{}", logs.text());
    assert!(errors[0].contains("status=503"), "{}", errors[0]);

    // The web app's origin may read the request id (MINOR 4); the allowed origins are unchanged.
    let request = Request::get("/healthz")
        .header(header::ORIGIN, "https://ferrygo.tech")
        .body(Body::empty())
        .unwrap();
    let (_, headers, _) = common::send(app::router(state.clone()), request).await;
    assert_eq!(
        headers[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "https://ferrygo.tech"
    );
    let exposed = headers[header::ACCESS_CONTROL_EXPOSE_HEADERS]
        .to_str()
        .unwrap()
        .to_ascii_lowercase();
    assert!(
        exposed.split(',').any(|h| h.trim() == "x-request-id"),
        "{exposed}"
    );
    let request = Request::get("/healthz")
        .header(header::ORIGIN, "https://evil.example")
        .body(Body::empty())
        .unwrap();
    let (_, headers, _) = common::send(app::router(state), request).await;
    assert!(headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
}
