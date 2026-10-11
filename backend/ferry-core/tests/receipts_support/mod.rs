//! Shared by the receipts_*.rs tests: a local stub of Supabase Storage's two signing routes, and
//! the seeded demo served with Storage pointed at it.
//!
//! The stub answers like Storage's server (github.com/supabase/storage, `src/http/routes/object/`
//! `getSignedUploadURL.ts` and `getSignedURLs.ts`): `POST /storage/v1/object/upload/sign/{bucket}/{path}`
//! returns `{url: "/object/upload/sign/{bucket}/{path}?token=…", token}`, and
//! `POST /storage/v1/object/sign/{bucket}` with `{expiresIn, paths}` returns one
//! `{error, path, signedURL}` per path, `signedURL` null for an object it does not hold. Tokens
//! are HS256 JWTs with an `exp`, so the server's expiry decoding runs. No test reaches Supabase.
#![allow(dead_code)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use ferry_core::state::AppState;
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::common;
use crate::realtime_support::{Server, serve};
use crate::runs_support::WEB_URL;

/// The secret key the servers send; the stub checks it arrives in `apikey` only.
pub const SECRET_KEY: &str = "sb_secret_receipts_test";
/// Refuses connections at once, so routes use tracking's straight-line fallback.
pub const OSRM_OFF: &str = "http://127.0.0.1:1";
/// The upload tokens' lifetime: hosted Storage's documented 2 hours.
pub const UPLOAD_TOKEN_SECS: u64 = 7200;

/// How the stub answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Ok,
    /// Every request gets this status and an error body.
    Fail(u16),
    /// Every request waits a minute before answering.
    Hang,
}

/// One request the stub received.
#[derive(Debug, Clone)]
pub struct Seen {
    /// `upload-sign` or `sign`.
    pub route: &'static str,
    /// The request path, as sent (percent-encoded).
    pub path: String,
    pub apikey: Option<String>,
    pub authorization: Option<String>,
    pub body: Value,
}

#[derive(Default)]
struct Inner {
    mode: Option<Mode>,
    /// Objects the stub holds, as `{bucket}/{path}`.
    objects: HashSet<String>,
    seen: Vec<Seen>,
    /// Every token it issued, upload and download.
    tokens: Vec<String>,
}

#[derive(Clone)]
pub struct Stub {
    pub base: String,
    inner: Arc<Mutex<Inner>>,
}

impl Stub {
    /// Starts the stub on a free local port, answering [`Mode::Ok`].
    pub async fn start() -> Stub {
        let inner = Arc::new(Mutex::new(Inner::default()));
        let app = Router::new()
            .route(
                "/storage/v1/object/upload/sign/{bucket}/{*path}",
                post(upload_sign),
            )
            .route("/storage/v1/object/sign/{bucket}", post(sign))
            .with_state(inner.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Stub { base, inner }
    }

    pub fn set_mode(&self, mode: Mode) {
        self.inner.lock().unwrap().mode = Some(mode);
    }

    /// What the client's `uploadToSignedUrl` would leave: the object exists.
    pub fn put_object(&self, bucket: &str, path: &str) {
        self.inner
            .lock()
            .unwrap()
            .objects
            .insert(format!("{bucket}/{path}"));
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.inner.lock().unwrap().seen.clone()
    }

    /// How many requests reached `route`.
    pub fn count(&self, route: &str) -> usize {
        self.seen().iter().filter(|s| s.route == route).count()
    }

    pub fn tokens(&self) -> Vec<String> {
        self.inner.lock().unwrap().tokens.clone()
    }

    /// The absolute signed link the server should build for `path` with `token`.
    pub fn link(&self, path: &str, token: &str) -> String {
        format!(
            "{}/storage/v1/object/sign/receipts/{path}?token={token}",
            self.base
        )
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn mint(claims: Value) -> String {
    jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(b"storage-stub"),
    )
    .unwrap()
}

/// Records the request and returns the mode's early answer, if any.
async fn arrive(
    inner: &Arc<Mutex<Inner>>,
    route: &'static str,
    path: String,
    headers: &HeaderMap,
    body: Value,
) -> Option<Response> {
    let header = |name: &str| headers.get(name).map(|v| v.to_str().unwrap().to_owned());
    let mode = {
        let mut inner = inner.lock().unwrap();
        inner.seen.push(Seen {
            route,
            path,
            apikey: header("apikey"),
            authorization: header("authorization"),
            body,
        });
        inner.mode.unwrap_or(Mode::Ok)
    };
    match mode {
        Mode::Ok => None,
        Mode::Fail(status) => Some(
            (
                StatusCode::from_u16(status).unwrap(),
                axum::Json(json!({"statusCode": status.to_string(), "error": "stub", "message": "stub failure"})),
            )
                .into_response(),
        ),
        Mode::Hang => {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Some(StatusCode::GATEWAY_TIMEOUT.into_response())
        }
    }
}

async fn upload_sign(
    State(inner): State<Arc<Mutex<Inner>>>,
    Path((bucket, path)): Path<(String, String)>,
    uri: axum::http::Uri,
    headers: HeaderMap,
) -> Response {
    if let Some(early) = arrive(
        &inner,
        "upload-sign",
        uri.path().to_owned(),
        &headers,
        Value::Null,
    )
    .await
    {
        return early;
    }
    let url = format!("{bucket}/{path}");
    let token = mint(json!({
        "owner": null, "url": url, "upsert": false, "scope": "upload",
        "iat": now_secs(), "exp": now_secs() + UPLOAD_TOKEN_SECS
    }));
    inner.lock().unwrap().tokens.push(token.clone());
    axum::Json(json!({"url": format!("/object/upload/sign/{url}?token={token}"), "token": token}))
        .into_response()
}

async fn sign(
    State(inner): State<Arc<Mutex<Inner>>>,
    Path(bucket): Path<String>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    if let Some(early) = arrive(
        &inner,
        "sign",
        uri.path().to_owned(),
        &headers,
        body.clone(),
    )
    .await
    {
        return early;
    }
    let expires_in = body["expiresIn"].as_u64().expect("expiresIn is an integer");
    let mut inner = inner.lock().unwrap();
    let mut answer = Vec::new();
    for path in body["paths"].as_array().expect("paths is an array") {
        let path = path.as_str().unwrap();
        let url = format!("{bucket}/{path}");
        if inner.objects.contains(&url) {
            let token = mint(json!({
                "url": url, "scope": "download",
                "iat": now_secs(), "exp": now_secs() + expires_in
            }));
            inner.tokens.push(token.clone());
            answer.push(json!({"error": null, "path": path, "signedURL": format!("/object/sign/{url}?token={token}")}));
        } else {
            answer.push(json!({
                "error": "Either the object does not exist or you do not have access to it",
                "path": path, "signedURL": null
            }));
        }
    }
    axum::Json(Value::Array(answer)).into_response()
}

/// The seeded demo (runs_support::seed) on `pool`. Call once per test, before [`server`].
pub async fn seed(pool: &PgPool) {
    crate::runs_support::seed(pool).await;
}

/// The demo served in demo mode with Storage at `storage_base` (or not configured), without
/// seeding.
pub async fn server(pool: PgPool, storage_base: Option<&str>) -> Server {
    let mut pairs = vec![
        ("DEMO_MODE", "true"),
        ("PUBLIC_WEB_URL", WEB_URL),
        ("OSRM_BASE_URL", OSRM_OFF),
    ];
    if let Some(base) = storage_base {
        pairs.push(("SUPABASE_URL", base));
        pairs.push(("SUPABASE_SECRET_KEY", SECRET_KEY));
    }
    serve(AppState::new(pool, common::config(&pairs)).unwrap()).await
}
