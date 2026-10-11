//! Shared by the catalog_*.rs and seed_*.rs tests: the app with a chosen config, signed-in
//! requests, the fixture files, and validation against contract/openapi.yaml.
#![allow(dead_code)]

use std::sync::OnceLock;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use ferry_core::app;
use ferry_core::state::AppState;
use serde_json::Value;
use sqlx::PgPool;

use crate::common;

/// An app state on `pool`, with its config built from `pairs` alone.
pub fn state(pool: PgPool, pairs: &[(&str, &str)]) -> AppState {
    AppState::new(pool, common::config(pairs)).unwrap()
}

/// `GET uri` as `user_id` (a signed token), or without a token when `user_id` is `None`.
pub async fn get(state: &AppState, uri: &str, user_id: Option<&str>) -> (StatusCode, Value) {
    let mut request = Request::get(uri);
    if let Some(user_id) = user_id {
        let token = state.jwt.issue_token(user_id).unwrap();
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let (status, _, body) = common::send(
        app::router(state.clone()),
        request.body(Body::empty()).unwrap(),
    )
    .await;
    (status, body)
}

/// `contract/fixtures/<name>.json`, read from disk.
pub fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/../../contract/fixtures/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// `items` keyed by their `id`, for comparing lists whose order the fixtures do not fix.
pub fn by_id(items: &Value) -> std::collections::BTreeMap<String, Value> {
    items
        .as_array()
        .expect("an array")
        .iter()
        .map(|item| (item["id"].as_str().unwrap().to_owned(), item.clone()))
        .collect()
}

/// contract/openapi.yaml as JSON.
pub fn openapi() -> &'static Value {
    static OPENAPI: OnceLock<Value> = OnceLock::new();
    OPENAPI.get_or_init(|| {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../contract/openapi.yaml");
        serde_norway::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    })
}

/// Asserts `body` validates against the 200 response schema of `GET path` in
/// contract/openapi.yaml (JSON Schema 2020-12, as OpenAPI 3.1 uses, with formats such as uri
/// checked).
pub fn assert_get_200(path: &str, body: &Value) {
    let schema = &openapi()["paths"][path]["get"]["responses"]["200"]["content"]["application/json"]
        ["schema"];
    let schema = schema
        .as_object()
        .unwrap_or_else(|| panic!("contract/openapi.yaml has no 200 schema for GET {path}"));
    // The whole document is the root, so the operation's $refs to components resolve.
    let mut root = openapi().clone();
    for (key, value) in schema {
        root[key] = value.clone();
    }
    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .build(&root)
        .unwrap_or_else(|e| panic!("the GET {path} schema does not compile: {e}"));
    let errors: Vec<String> = validator
        .iter_errors(body)
        .map(|e| format!("{e} at {}", e.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "GET {path} response does not validate: {errors:#?}\n{body:#}"
    );
}

/// Asserts `body` is the contract's error envelope with `code`.
pub fn assert_error(body: &Value, code: &str) {
    common::assert_envelope(body, code);
}

/// Seeds the fixtures through the library, as `ferry-core seed` does.
pub async fn seed(pool: &PgPool) -> ferry_core::demo::seed::SeedReport {
    ferry_core::demo::seed::seed_pool(pool).await.unwrap()
}
