//! Shared by every integration test binary. Each file in tests/ must start with `mod common;`
//! (tests/guard.rs fails otherwise): the constructor below is what keeps `#[sqlx::test]` and the
//! app's config away from Supabase, whatever directory cargo runs from.
#![allow(dead_code)]

use std::collections::HashMap;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use ferry_core::config::Config;
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

#[ctor::ctor(unsafe)]
fn force_local_test_env() {
    // SAFETY: constructors run before main, while the process has one thread.
    unsafe { ferry_core::test_support::force_local_test_env() }
}

/// Where tests send OSRM requests unless they pass their own `OSRM_BASE_URL`: a port that refuses
/// at once, so routing takes its straight-line fallback and no test reaches the live server.
pub const OSRM_OFFLINE: &str = "http://127.0.0.1:1";

/// A config built from `pairs` alone, never from the environment or the root .env. Without an
/// `OSRM_BASE_URL` pair, OSRM is [`OSRM_OFFLINE`].
pub fn config(pairs: &[(&str, &str)]) -> Config {
    let mut vars: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    vars.entry("DATABASE_URL".into())
        .or_insert_with(|| std::env::var("DATABASE_URL").unwrap());
    vars.entry("OSRM_BASE_URL".into())
        .or_insert_with(|| OSRM_OFFLINE.into());
    vars.entry("JWT_SECRET".into())
        .or_insert_with(|| "test-secret-test-secret-test-secret!".into());
    Config::from_vars(&vars).unwrap()
}

/// A pool whose every connection attempt is refused at once.
pub fn unreachable_pool() -> PgPool {
    PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(200))
        .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/unreachable")
        .unwrap()
}

/// Sends `request` to `app` and returns the status, headers and JSON body (Null when empty).
pub async fn send(app: axum::Router, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| panic!("body is not JSON: {}", String::from_utf8_lossy(&bytes)))
    };
    (status, headers, body)
}

pub fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

/// Asserts `body` is exactly the contract's error envelope with `code`.
pub fn assert_envelope(body: &Value, code: &str) {
    let object = body.as_object().expect("error body is an object");
    assert_eq!(object.len(), 1, "only `error` at the top level: {body}");
    let error = body["error"].as_object().expect("`error` is an object");
    assert_eq!(error["code"], code, "{body}");
    assert!(error["message"].is_string(), "{body}");
    assert!(
        error
            .keys()
            .all(|k| ["code", "message", "details"].contains(&k.as_str())),
        "{body}"
    );
}
