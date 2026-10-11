//! Shared by the auth_* and profile_* tests: a stub sidecar, the app, request helpers, seeders
//! and the contract validator. Each test file declares both `mod common;` and `mod auth_support;`.
#![allow(dead_code)]

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::routing;
use ferry_core::app;
use ferry_core::state::AppState;
use serde_json::{Value, json};
use sqlx::PgPool;

use super::common;

pub const STUB_SECRET: &str = "stub-sidecar-secret";

// ---------- The stub sidecar ----------

/// One `POST /send` the stub received.
#[derive(Debug, Clone)]
pub struct Sent {
    pub secret: Option<String>,
    pub body: Value,
}

/// A stand-in for the sidecar's internal API on an OS-assigned local port. It records every
/// `POST /send` and answers 200 `{space_id}`.
#[derive(Clone)]
pub struct Stub {
    pub base: String,
    sent: Arc<Mutex<Vec<Sent>>>,
}

async fn stub_send(
    State(sent): State<Arc<Mutex<Vec<Sent>>>>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> axum::Json<Value> {
    let space_id = body["space_id"].clone();
    sent.lock().unwrap().push(Sent {
        secret: headers
            .get("x-ferry-secret")
            .map(|v| v.to_str().unwrap().to_owned()),
        body,
    });
    axum::Json(json!({"space_id": space_id}))
}

impl Stub {
    pub async fn start() -> Stub {
        let sent = Arc::<Mutex<Vec<Sent>>>::default();
        let router = Router::new()
            .route("/send", routing::post(stub_send))
            .with_state(sent.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Stub { base, sent }
    }

    pub fn sent(&self) -> Vec<Sent> {
        self.sent.lock().unwrap().clone()
    }

    /// Waits up to 5 s until the stub has received `count` sends in all (core sends after it
    /// answers the inbound event), and returns them.
    pub async fn wait_for(&self, count: usize) -> Vec<Sent> {
        for _ in 0..500 {
            let sent = self.sent();
            if sent.len() >= count {
                return sent;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the stub got {} sends, not {count}", self.sent().len());
    }

    /// Asserts nothing more than `count` sends arrives within a short while.
    pub async fn assert_no_more_than(&self, count: usize) {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let sent = self.sent();
        assert_eq!(sent.len(), count, "{sent:#?}");
    }
}

// ---------- The app ----------

/// The app with `pairs` in its config (plus the test JWT secret and the local database).
pub fn app_with(pool: PgPool, pairs: &[(&str, &str)]) -> (Router, AppState) {
    let state = AppState::new(pool, common::config(pairs)).unwrap();
    (app::router(state.clone()), state)
}

// ---------- Requests ----------

pub fn json_request(
    method: &str,
    uri: &str,
    body: &Value,
    headers: &[(&str, &str)],
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

pub fn post(uri: &str, body: Value) -> Request<Body> {
    json_request("POST", uri, &body, &[])
}

pub fn get_with_token(uri: &str, token: &str) -> Request<Body> {
    Request::get(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

pub fn put_with_token(uri: &str, token: &str, body: &Value) -> Request<Body> {
    json_request(
        "PUT",
        uri,
        body,
        &[("authorization", &format!("Bearer {token}"))],
    )
}

pub async fn call(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let (status, _, body) = common::send(app.clone(), request).await;
    (status, body)
}

/// Asserts a contract error: the status, the envelope with `code`, and the `Error` schema.
pub fn assert_error(status: StatusCode, body: &Value, expected: StatusCode, code: &str) {
    assert_eq!(status, expected, "{body}");
    common::assert_envelope(body, code);
    assert_contract("Error", body);
}

// ---------- Seeds ----------

pub async fn seed_user(pool: &PgPool, id: &str, phone: &str, name: &str) {
    sqlx::query(
        "INSERT INTO ferry.users (id, phone_e164, display_name, avatar_seed, bubble_color, onboarding_complete)
         VALUES ($1, $2, $3, $1, '#2563EB', false)",
    )
    .bind(id)
    .bind(phone)
    .bind(name)
    .execute(pool)
    .await
    .unwrap();
}

/// The four demo personas, as the demo seeder stores them (fixture ids, masked-fake phones).
/// Jordan also gets a full profiles row; the others have none.
pub async fn seed_personas(pool: &PgPool) {
    for (id, phone, name) in [
        ("usr_judge", "+19195550101", "Jordan"),
        ("usr_maya", "+19195550102", "Maya"),
        ("usr_dana", "+19195550103", "Dana"),
        ("usr_leo", "+19195550104", "Leo"),
    ] {
        seed_user(pool, id, phone, name).await;
    }
    seed_catalog(pool).await;
    sqlx::query(
        "INSERT INTO ferry.profiles (user_id, dropoff_place_id, dropoff_label, dropoff_lat, dropoff_lng,
             dropoff_note, favorite_cuisines, favorite_restaurant_ids, usual_times, default_tip_cents)
         VALUES ('usr_judge', 'place_fetzer', 'Fetzer Gym (HackNC)', 35.909152, -79.046905,
             'HackNC venue.', '{mexican,american}', '{rst_cosmic}',
             '[{\"label\": \"Hackathon dinner\", \"days\": [\"sat\"], \"start\": \"18:00\", \"end\": \"20:00\"}]',
             200)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("UPDATE ferry.users SET onboarding_complete = true WHERE id = 'usr_judge'")
        .execute(pool)
        .await
        .unwrap();
}

/// One area, two restaurants and two places from the fixtures.
pub async fn seed_catalog(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO ferry.areas (id, name, center_lat, center_lng)
         VALUES ('area_franklin', 'Franklin St', 35.9132, -79.0558)
         ON CONFLICT DO NOTHING",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ferry.restaurants (id, name, area_id, lat, lng, address, hours_today, price_level)
         VALUES ('rst_cosmic', 'Cosmic Cantina', 'area_franklin', 35.913211, -79.055094,
                 '118 E Franklin St', '11:00-04:00', 1),
                ('rst_supdogs', 'Sup Dogs', 'area_franklin', 35.913597, -79.05552,
                 '107 E Franklin St', '11:00-03:00', 1)
         ON CONFLICT DO NOTHING",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ferry.places (id, label, lat, lng, note)
         VALUES ('place_fetzer', 'Fetzer Gym (HackNC)', 35.909152, -79.046905, NULL),
                ('place_hinton', 'Hinton James lobby', 35.902364, -79.043231, NULL)
         ON CONFLICT DO NOTHING",
    )
    .execute(pool)
    .await
    .unwrap();
}

// ---------- Logs ----------

/// Every log line written while a [`LogCapture`] is installed on this thread, at every level and
/// without colors. `#[sqlx::test]` runs on a current-thread runtime, so this covers the tasks the
/// test spawns too.
#[derive(Clone, Default)]
pub struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl LogBuffer {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

/// Captures logs into the buffer until dropped.
pub struct LogCapture {
    pub logs: LogBuffer,
    _guard: tracing::subscriber::DefaultGuard,
}

pub fn capture_logs() -> LogCapture {
    let logs = LogBuffer::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    LogCapture {
        logs,
        _guard: tracing::subscriber::set_default(subscriber),
    }
}

// ---------- The contract ----------

/// contract/openapi.yaml as JSON.
pub fn openapi() -> &'static Value {
    static OPENAPI: OnceLock<Value> = OnceLock::new();
    OPENAPI.get_or_init(|| {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../contract/openapi.yaml");
        serde_norway::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    })
}

/// Asserts `instance` validates against `#/components/schemas/<schema>` of contract/openapi.yaml
/// (JSON Schema 2020-12, as OpenAPI 3.1 uses, with formats such as date-time checked).
pub fn assert_contract(schema: &str, instance: &Value) {
    assert!(
        openapi()["components"]["schemas"].get(schema).is_some(),
        "contract/openapi.yaml has no schema {schema}"
    );
    let mut root = openapi().clone();
    root["$ref"] = json!(format!("#/components/schemas/{schema}"));
    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .build(&root)
        .unwrap_or_else(|e| panic!("schema {schema} does not compile: {e}"));
    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|e| format!("{e} at {}", e.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "{schema} does not validate: {errors:#?}\n{instance:#}"
    );
}
