//! Bucket creation against a local stub of the Supabase Storage API.
mod common;

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use common::config;
use ferry_core::state::http_client;
use ferry_core::storage::{EnsureOutcome, StorageError, ensure_buckets};
use serde_json::{Value, json};

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    apikey: Option<String>,
    authorization: Option<String>,
    body: Value,
}

#[derive(Clone)]
struct Stub {
    existing: Value,
    list_status: StatusCode,
    seen: Arc<Mutex<Vec<Seen>>>,
}

fn record(stub: &Stub, method: &str, headers: &HeaderMap, body: Value) {
    let header = |name: &str| headers.get(name).map(|v| v.to_str().unwrap().to_owned());
    stub.seen.lock().unwrap().push(Seen {
        method: method.to_owned(),
        apikey: header("apikey"),
        authorization: header("authorization"),
        body,
    });
}

async fn list(State(stub): State<Stub>, headers: HeaderMap) -> (StatusCode, axum::Json<Value>) {
    record(&stub, "GET", &headers, Value::Null);
    (stub.list_status, axum::Json(stub.existing.clone()))
}

async fn create(
    State(stub): State<Stub>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> axum::Json<Value> {
    record(&stub, "POST", &headers, body.clone());
    axum::Json(json!({"name": body["name"]}))
}

/// Starts the stub on a free local port; returns its base URL and what it saw.
async fn start(existing: Value, list_status: StatusCode) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let stub = Stub {
        existing,
        list_status,
        seen: seen.clone(),
    };
    let app = Router::new()
        .route("/storage/v1/bucket", get(list).post(create))
        .with_state(stub);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, seen)
}

#[tokio::test]
async fn secret_key_creates_both_buckets_with_apikey_only() {
    let (base, seen) = start(json!([]), StatusCode::OK).await;
    let config = config(&[
        ("SUPABASE_URL", &base),
        ("SUPABASE_SECRET_KEY", "sb_secret_test"),
    ]);
    let outcome = ensure_buckets(&http_client().unwrap(), &config)
        .await
        .unwrap();
    assert_eq!(
        outcome,
        EnsureOutcome::Done {
            created: vec!["receipts", "media"],
            existing: vec![]
        }
    );
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 3);
    for request in &seen {
        assert_eq!(request.apikey.as_deref(), Some("sb_secret_test"));
        assert_eq!(request.authorization, None);
    }
    assert_eq!(seen[0].method, "GET");
    assert_eq!(
        seen[1].body,
        json!({"id": "receipts", "name": "receipts", "public": false,
               "file_size_limit": 5_242_880, "allowed_mime_types": ["image/*"]})
    );
    assert_eq!(
        seen[2].body,
        json!({"id": "media", "name": "media", "public": true})
    );
}

#[tokio::test]
async fn legacy_key_sends_both_headers_and_skips_existing_buckets() {
    let existing = json!([{"id": "receipts", "name": "receipts", "public": false,
                           "file_size_limit": 5_242_880, "allowed_mime_types": ["image/*"]}]);
    let (base, seen) = start(existing, StatusCode::OK).await;
    let key = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJyb2xlIjoic2VydmljZV9yb2xlIn0.c2ln";
    let config = config(&[("SUPABASE_URL", &base), ("SUPABASE_SECRET_KEY", key)]);
    let outcome = ensure_buckets(&http_client().unwrap(), &config)
        .await
        .unwrap();
    assert_eq!(
        outcome,
        EnsureOutcome::Done {
            created: vec!["media"],
            existing: vec!["receipts"]
        }
    );
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    for request in &seen {
        assert_eq!(request.apikey.as_deref(), Some(key));
        assert_eq!(
            request.authorization.as_deref(),
            Some(format!("Bearer {key}").as_str())
        );
    }
}

#[tokio::test]
async fn unset_key_skips_without_a_request() {
    let (base, seen) = start(json!([]), StatusCode::OK).await;
    let config = config(&[("SUPABASE_URL", &base)]);
    let outcome = ensure_buckets(&http_client().unwrap(), &config)
        .await
        .unwrap();
    assert_eq!(
        outcome,
        EnsureOutcome::Skipped("SUPABASE_SECRET_KEY is unset")
    );
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn storage_errors_are_reported() {
    let (base, _) = start(
        json!({"message": "Invalid Compact JWS"}),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    let config = config(&[
        ("SUPABASE_URL", &base),
        ("SUPABASE_SECRET_KEY", "sb_secret_test"),
    ]);
    let err = ensure_buckets(&http_client().unwrap(), &config)
        .await
        .unwrap_err();
    assert!(
        matches!(err, StorageError::Status { status: 401, .. }),
        "{err}"
    );
}

/// Starts a stub whose bucket list answers `status` with the raw `body`; returns its base URL.
async fn start_raw(status: StatusCode, body: Vec<u8>) -> String {
    let app = Router::new().route(
        "/storage/v1/bucket",
        get(move || async move { (status, body) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

/// A multibyte character straddling byte 300 of an error body used to panic in `truncate`.
#[tokio::test]
async fn error_body_is_cut_on_a_char_boundary() {
    let ascii = "a".repeat(299);
    let cases: [(&str, Vec<u8>, String); 3] = [
        // `é` is 2 bytes, at 299..301.
        (
            "é",
            [ascii.as_bytes(), "é".as_bytes()].concat(),
            ascii.clone(),
        ),
        // Not UTF-8: reqwest's `text()` makes 0xFF the 3-byte U+FFFD, at 299..302.
        ("0xFF", [ascii.as_bytes(), &[0xFF]].concat(), ascii.clone()),
        // ASCII is still cut at 300.
        ("ascii", "b".repeat(400).into_bytes(), "b".repeat(300)),
    ];
    for (name, raw, expected) in cases {
        let base = start_raw(StatusCode::BAD_GATEWAY, raw).await;
        let config = config(&[
            ("SUPABASE_URL", &base),
            ("SUPABASE_SECRET_KEY", "sb_secret_test"),
        ]);
        let err = ensure_buckets(&http_client().unwrap(), &config)
            .await
            .unwrap_err();
        match err {
            StorageError::Status { status, body, .. } => {
                assert_eq!(status, 502, "{name}");
                assert_eq!(body, expected, "{name}");
            }
            other => panic!("{name}: expected StorageError::Status, got {other}"),
        }
    }
}
