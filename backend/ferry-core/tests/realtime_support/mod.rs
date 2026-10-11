//! Shared by the realtime_*.rs tests: the app on a real socket, a WebSocket client
//! (tokio-tungstenite), and validation against contract/openapi.yaml.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::OnceLock;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use ferry_core::app;
use ferry_core::notifications::{Hub, HubSettings};
use ferry_core::state::AppState;
use futures_util::StreamExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::common;

pub type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// How long a test waits for a message it expects.
pub const RECV_TIMEOUT: Duration = Duration::from_secs(5);

/// The app on 127.0.0.1 with an ephemeral port, stopped when dropped.
pub struct Server {
    pub addr: SocketAddr,
    pub state: AppState,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// An app state on `pool` with the hub's settings replaced.
pub fn state(pool: PgPool, settings: HubSettings) -> AppState {
    let mut state = AppState::new(pool, common::config(&[])).unwrap();
    state.hub = Hub::with_settings(settings);
    state
}

pub async fn serve(state: AppState) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app::router(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Server { addr, state, task }
}

impl Server {
    pub fn hub(&self) -> &Hub {
        &self.state.hub
    }

    pub fn token(&self, user_id: &str) -> String {
        self.state.jwt.issue_token(user_id).unwrap()
    }

    /// `ws://.../v1/stream` plus `query` (for example `?token=...`).
    pub fn stream_url(&self, query: &str) -> String {
        format!("ws://{}/v1/stream{query}", self.addr)
    }

    pub async fn connect(&self, query: &str) -> Result<Ws, tungstenite::Error> {
        tokio_tungstenite::connect_async(self.stream_url(query))
            .await
            .map(|(ws, _)| ws)
    }

    pub async fn connect_with_token(&self, user_id: &str) -> Ws {
        self.connect(&format!("?token={}", self.token(user_id)))
            .await
            .expect("connect with a token")
    }

    /// Sends `request` to the app (in process, same state and hub).
    pub async fn send(&self, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
        common::send(app::router(self.state.clone()), request).await
    }

    pub async fn get_as(&self, uri: &str, user_id: &str) -> (StatusCode, Value) {
        let request = Request::get(uri)
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", self.token(user_id)),
            )
            .body(Body::empty())
            .unwrap();
        let (status, _, body) = self.send(request).await;
        (status, body)
    }

    pub async fn post_as(&self, uri: &str, user_id: &str) -> (StatusCode, Value) {
        let request = Request::post(uri)
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", self.token(user_id)),
            )
            .body(Body::empty())
            .unwrap();
        let (status, _, body) = self.send(request).await;
        (status, body)
    }

    /// `POST /v1/stream/ticket` as `user_id`; asserts 200 and the contract's shape.
    pub async fn ticket(&self, user_id: &str) -> String {
        let (status, body) = self.post_as("/v1/stream/ticket", user_id).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_stream_ticket(&body);
        body["ticket"].as_str().unwrap().to_owned()
    }
}

/// The next text message as JSON, skipping ping and pong frames.
pub async fn next_json(ws: &mut Ws) -> Value {
    loop {
        let message = tokio::time::timeout(RECV_TIMEOUT, ws.next())
            .await
            .expect("no message within the timeout")
            .expect("the stream ended")
            .expect("WebSocket error");
        match message {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("unexpected message: {other:?}"),
        }
    }
}

/// Asserts no text message arrives within `wait` (ping and pong frames are ignored).
pub async fn assert_silent(ws: &mut Ws, wait: Duration) {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        match tokio::time::timeout_at(deadline, ws.next()).await {
            Err(_) => return,
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
            Ok(other) => panic!("expected nothing, got {other:?}"),
        }
    }
}

/// Asserts a connect attempt failed before the upgrade with 401 in the contract's envelope.
pub fn assert_rejected_401(result: Result<Ws, tungstenite::Error>) {
    match result {
        Err(tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            let body: Value = serde_json::from_slice(response.body().as_deref().unwrap_or(b""))
                .expect("a JSON body");
            common::assert_envelope(&body, "unauthorized");
            assert_contract("Error", &body);
        }
        Err(other) => panic!("expected an HTTP 401, got {other}"),
        Ok(_) => panic!("expected an HTTP 401, but the upgrade succeeded"),
    }
}

/// contract/openapi.yaml as JSON.
pub fn openapi() -> &'static Value {
    static OPENAPI: OnceLock<Value> = OnceLock::new();
    OPENAPI.get_or_init(|| {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../contract/openapi.yaml");
        let yaml = std::fs::read_to_string(path).unwrap();
        serde_norway::from_str(&yaml).unwrap()
    })
}

/// Asserts `instance` validates against `#/components/schemas/<schema>` of contract/openapi.yaml
/// (JSON Schema 2020-12, as OpenAPI 3.1 uses, with formats such as date-time checked).
pub fn assert_contract(schema: &str, instance: &Value) {
    let errors = contract_errors(schema, instance);
    assert!(
        errors.is_empty(),
        "{schema} does not validate: {errors:#?}\n{instance:#}"
    );
}

/// Why `instance` does not validate against `#/components/schemas/<schema>`; empty if it does.
pub fn contract_errors(schema: &str, instance: &Value) -> Vec<String> {
    assert!(
        openapi()["components"]["schemas"].get(schema).is_some(),
        "contract/openapi.yaml has no schema {schema}"
    );
    let mut root = openapi().clone();
    root["$ref"] = json!(format!("#/components/schemas/{schema}"));
    schema_errors(schema, &root, instance)
}

/// Asserts `instance` validates against `schema` (JSON Schema 2020-12, formats checked).
pub fn assert_schema(name: &str, schema: &Value, instance: &Value) {
    let errors = schema_errors(name, schema, instance);
    assert!(
        errors.is_empty(),
        "{name} does not validate: {errors:#?}\n{instance:#}"
    );
}

fn schema_errors(name: &str, schema: &Value, instance: &Value) -> Vec<String> {
    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .build(schema)
        .unwrap_or_else(|e| panic!("schema {name} does not compile: {e}"));
    validator
        .iter_errors(instance)
        .map(|e| format!("{e} at {}", e.instance_path()))
        .collect()
}

/// `StreamTicket` from contract v1.2 (PR #5), checked here until main's openapi.yaml has it; when
/// it does, the instance is checked against the contract's copy as well.
pub fn assert_stream_ticket(instance: &Value) {
    let v1_2 = json!({
        "type": "object",
        "required": ["ticket", "expires_at"],
        "properties": {
            "ticket": {"type": "string"},
            "expires_at": {"type": "string", "format": "date-time"}
        }
    });
    assert_schema("StreamTicket (v1.2)", &v1_2, instance);
    if openapi()["components"]["schemas"]
        .get("StreamTicket")
        .is_some()
    {
        assert_contract("StreamTicket", instance);
    }
}

/// The first run in contract/fixtures/runs.json: a full contract `Run`.
pub fn fixture_run() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contract/fixtures/runs.json"
    );
    let runs: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    runs[0].clone()
}

/// Polls `check` every 20 ms until it holds or `RECV_TIMEOUT` passes.
pub async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + RECV_TIMEOUT;
    while !check() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
