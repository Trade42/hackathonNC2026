//! Shared by the push_*.rs tests: a stub push service on 127.0.0.1, a fake browser subscription
//! that decrypts what it receives, generated VAPID keys, and the app wired to the stub through
//! PUSH_TEST_STUB_ORIGIN (honored because tests use the local database).
//!
//! The decryption and the VAPID check here are written from RFC 8291 and RFC 8292 on their own,
//! not with ferry-core's code, so a mistake there cannot cancel itself out.
#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aws_lc_rs::aead::{AES_128_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use aws_lc_rs::agreement::{self, ECDH_P256, PrivateKey, UnparsedPublicKey};
use aws_lc_rs::hkdf::{HKDF_SHA256, KeyType, Salt};
use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use aws_lc_rs::signature::{self, ECDSA_P256_SHA256_FIXED};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Request, StatusCode, Uri, header};
use axum::routing::post;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ferry_core::notifications::push::{self, PushSettings};
use ferry_core::state::AppState;
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::common;
use crate::realtime_support::{Server, assert_contract, serve};

pub const WEB_URL: &str = "https://ferry.example";

pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn unb64(text: &str) -> Vec<u8> {
    URL_SAFE_NO_PAD.decode(text).unwrap()
}

/// A VAPID key pair as `npx web-push generate-vapid-keys` prints it: (public, private), base64url.
pub fn vapid_pair() -> (String, String) {
    loop {
        let mut scalar = [0u8; 32];
        SystemRandom::new().fill(&mut scalar).unwrap();
        if let Ok(private) = PrivateKey::from_private_key(&ECDH_P256, &scalar) {
            let public = private.compute_public_key().unwrap();
            return (b64(public.as_ref()), b64(&scalar));
        }
    }
}

/// What the stub answers one request with.
#[derive(Debug, Clone, Copy)]
pub struct Reply {
    pub status: u16,
    pub delay: Duration,
}

impl Reply {
    pub const fn status(status: u16) -> Self {
        Self {
            status,
            delay: Duration::ZERO,
        }
    }

    /// Answers 201 only after `delay`: a push service that hangs.
    pub const fn hang(delay: Duration) -> Self {
        Self { status: 201, delay }
    }
}

/// One request the stub received.
#[derive(Debug, Clone)]
pub struct Received {
    pub path: String,
    pub headers: HeaderMap,
    pub body: Bytes,
}

#[derive(Default)]
struct StubState {
    received: Mutex<Vec<Received>>,
    /// Per endpoint name, the replies to use in order; when none are left, 201.
    script: Mutex<HashMap<String, VecDeque<Reply>>>,
}

/// A push service on 127.0.0.1 that records every POST and answers from a script.
pub struct Stub {
    pub origin: String,
    state: Arc<StubState>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Stub {
    pub async fn start() -> Self {
        let state = Arc::new(StubState::default());
        let app = Router::new()
            .route("/push/{id}", post(receive))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            origin,
            state,
            task,
        }
    }

    pub fn endpoint(&self, name: &str) -> String {
        format!("{}/push/{name}", self.origin)
    }

    /// The next requests to [`Stub::endpoint`]`(name)` get these replies, in order.
    pub fn script(&self, name: &str, replies: &[Reply]) {
        self.state
            .script
            .lock()
            .unwrap()
            .entry(name.to_owned())
            .or_default()
            .extend(replies);
    }

    /// How many requests reached [`Stub::endpoint`]`(name)`.
    pub fn count_at(&self, name: &str) -> usize {
        let path = format!("/push/{name}");
        self.received().iter().filter(|r| r.path == path).count()
    }

    pub fn received(&self) -> Vec<Received> {
        self.state.received.lock().unwrap().clone()
    }

    pub fn count(&self) -> usize {
        self.state.received.lock().unwrap().len()
    }

    /// Waits until at least `n` requests arrived (5 s at most), then returns them all.
    pub async fn wait_for(&self, n: usize) -> Vec<Received> {
        crate::realtime_support::eventually(&format!("{n} push request(s)"), || self.count() >= n)
            .await;
        self.received()
    }

    /// Asserts nothing more than `n` requests arrive within `wait`.
    pub async fn assert_stays_at(&self, n: usize, wait: Duration) {
        tokio::time::sleep(wait).await;
        assert_eq!(self.count(), n, "push requests: {:?}", self.paths());
    }

    pub fn paths(&self) -> Vec<String> {
        self.received().into_iter().map(|r| r.path).collect()
    }
}

async fn receive(
    State(state): State<Arc<StubState>>,
    Path(name): Path<String>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    state.received.lock().unwrap().push(Received {
        path: uri.path().to_owned(),
        headers,
        body,
    });
    let reply = state
        .script
        .lock()
        .unwrap()
        .get_mut(&name)
        .and_then(VecDeque::pop_front)
        .unwrap_or(Reply::status(201));
    tokio::time::sleep(reply.delay).await;
    StatusCode::from_u16(reply.status).unwrap()
}

/// A browser's push subscription: its key pair and auth secret, and the endpoint it was given.
pub struct Browser {
    pub endpoint: String,
    private: PrivateKey,
    pub p256dh: Vec<u8>,
    pub auth: [u8; 16],
}

impl Browser {
    pub fn new(endpoint: impl Into<String>) -> Self {
        let private = PrivateKey::generate(&ECDH_P256).unwrap();
        let p256dh = private.compute_public_key().unwrap().as_ref().to_vec();
        let mut auth = [0u8; 16];
        SystemRandom::new().fill(&mut auth).unwrap();
        Self {
            endpoint: endpoint.into(),
            private,
            p256dh,
            auth,
        }
    }

    /// `PushSubscription.toJSON()`.
    pub fn to_json(&self) -> Value {
        json!({
            "endpoint": self.endpoint,
            "expirationTime": null,
            "keys": {"p256dh": b64(&self.p256dh), "auth": b64(&self.auth)}
        })
    }

    /// The `POST /v1/push/subscribe` body the web app sends.
    pub fn subscribe_body(&self) -> Value {
        json!({"platform": "web", "web_push": self.to_json()})
    }

    /// Opens an `aes128gcm` body (RFC 8291) as this browser would, and returns the plaintext.
    pub fn decrypt(&self, body: &[u8]) -> Vec<u8> {
        assert!(body.len() > 86 + 16, "body too short: {}", body.len());
        let salt = &body[..16];
        let record_size = u32::from_be_bytes(body[16..20].try_into().unwrap());
        assert!(record_size >= body.len() as u32 - 86, "one record");
        assert_eq!(body[20], 65, "key id is the sender's P-256 public key");
        let as_public = &body[21..86];
        let ciphertext = &body[86..];

        let (cek, nonce) = agreement::agree(
            &self.private,
            UnparsedPublicKey::new(&ECDH_P256, as_public),
            (),
            |ecdh_secret| {
                let mut key_info = b"WebPush: info\0".to_vec();
                key_info.extend_from_slice(&self.p256dh);
                key_info.extend_from_slice(as_public);
                let mut ikm = [0u8; 32];
                Salt::new(HKDF_SHA256, &self.auth)
                    .extract(ecdh_secret)
                    .expand(&[&key_info], Out(32))
                    .unwrap()
                    .fill(&mut ikm)
                    .unwrap();
                let prk = Salt::new(HKDF_SHA256, salt).extract(&ikm);
                let mut cek = [0u8; 16];
                prk.expand(&[b"Content-Encoding: aes128gcm\0"], Out(16))
                    .unwrap()
                    .fill(&mut cek)
                    .unwrap();
                let mut nonce = [0u8; 12];
                prk.expand(&[b"Content-Encoding: nonce\0"], Out(12))
                    .unwrap()
                    .fill(&mut nonce)
                    .unwrap();
                Ok((cek, nonce))
            },
        )
        .unwrap();
        let mut record = ciphertext.to_vec();
        let plaintext = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, &cek).unwrap())
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::empty(),
                &mut record,
            )
            .expect("the push decrypts with this subscription's keys");
        // The last record: data, then 0x02, then zero padding.
        let end = plaintext
            .iter()
            .rposition(|b| *b != 0)
            .expect("a padding delimiter");
        assert_eq!(plaintext[end], 0x02, "last-record delimiter");
        plaintext[..end].to_vec()
    }

    /// The decrypted body of `received` as JSON.
    pub fn open(&self, received: &Received) -> Value {
        serde_json::from_slice(&self.decrypt(&received.body)).expect("the push is JSON")
    }
}

struct Out(usize);

impl KeyType for Out {
    fn len(&self) -> usize {
        self.0
    }
}

/// Checks a push request's Web Push headers and its VAPID token (RFC 8292) for `vapid_public`,
/// and returns the token's claims.
pub fn assert_push_headers(received: &Received, vapid_public: &str) -> Value {
    let h = &received.headers;
    assert_eq!(h[header::CONTENT_ENCODING], "aes128gcm");
    assert_eq!(h[header::CONTENT_TYPE], "application/octet-stream");
    let ttl: u64 = h["ttl"].to_str().unwrap().parse().unwrap();
    assert!(ttl > 0);
    assert_eq!(h["urgency"], "high");

    let authorization = h[header::AUTHORIZATION].to_str().unwrap();
    let rest = authorization.strip_prefix("vapid ").expect("vapid scheme");
    let mut token = None;
    let mut key = None;
    for part in rest.split(',').map(str::trim) {
        if let Some(t) = part.strip_prefix("t=") {
            token = Some(t);
        } else if let Some(k) = part.strip_prefix("k=") {
            key = Some(k);
        }
    }
    assert_eq!(key, Some(vapid_public));
    let token = token.expect("t=");
    let (signing_input, signature) = token.rsplit_once('.').unwrap();
    let (head, claims) = signing_input.split_once('.').unwrap();
    let head: Value = serde_json::from_slice(&unb64(head)).unwrap();
    assert_eq!(head["alg"], "ES256");
    assert_eq!(head["typ"], "JWT");
    signature::UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, unb64(vapid_public))
        .verify(signing_input.as_bytes(), &unb64(signature))
        .expect("the VAPID token is signed by the VAPID key");
    let claims: Value = serde_json::from_slice(&unb64(claims)).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let exp = claims["exp"].as_u64().unwrap();
    assert!(exp > now && exp <= now + 24 * 60 * 60, "exp within 24 h");
    claims
}

pub async fn add_user(pool: &PgPool, id: &str, name: &str, phone: &str) {
    sqlx::query(
        "INSERT INTO ferry.users (id, phone_e164, display_name, avatar_seed, bubble_color) \
         VALUES ($1, $2, $3, $1, '#336699')",
    )
    .bind(id)
    .bind(phone)
    .bind(name)
    .execute(pool)
    .await
    .unwrap();
}

/// The demo's four people in cir_hinton4: judge, maya, dana and leo.
pub async fn seed_hinton4(pool: &PgPool) {
    for (id, name, phone) in [
        ("usr_judge", "Jordan", "+19195550101"),
        ("usr_maya", "Maya", "+19195550102"),
        ("usr_dana", "Dana", "+19195550103"),
        ("usr_leo", "Leo", "+19195550104"),
    ] {
        add_user(pool, id, name, phone).await;
    }
    sqlx::query("INSERT INTO ferry.circles (id, name, emoji) VALUES ('cir_hinton4', 'Hinton James 4th floor', '🌊')")
        .execute(pool)
        .await
        .unwrap();
    for (n, member) in ["usr_judge", "usr_maya", "usr_dana", "usr_leo"]
        .iter()
        .enumerate()
    {
        sqlx::query(
            "INSERT INTO ferry.circle_members (circle_id, user_id, joined_at) \
             VALUES ('cir_hinton4', $1, '2026-09-01T00:00:00Z'::timestamptz + make_interval(mins => $2))",
        )
        .bind(member)
        .bind(n as i32)
        .execute(pool)
        .await
        .unwrap();
    }
}

/// The app with a generated VAPID pair, PUBLIC_WEB_URL [`WEB_URL`], `extra` config, and Web Push
/// allowed to reach `stub` (when given). `tweak` adjusts the push settings before they are
/// attached. Returns the server and the VAPID public key.
pub async fn push_server(
    pool: PgPool,
    stub: Option<&Stub>,
    extra: &[(&str, &str)],
    tweak: impl FnOnce(&mut PushSettings),
) -> (Server, String) {
    seed_hinton4(&pool).await;
    let (public, private) = vapid_pair();
    let mut pairs: Vec<(&str, &str)> = vec![
        ("PUBLIC_WEB_URL", WEB_URL),
        ("VAPID_PUBLIC_KEY", &public),
        ("VAPID_PRIVATE_KEY", &private),
    ];
    if let Some(stub) = stub {
        pairs.push(("PUSH_TEST_STUB_ORIGIN", &stub.origin));
    }
    pairs.extend_from_slice(extra);
    let state = AppState::new(pool, common::config(&pairs)).unwrap();
    let mut settings = PushSettings::from_config(&state.config);
    tweak(&mut settings);
    assert!(push::attach_with(&state, settings), "web push is on");
    (serve(state).await, public)
}

/// `POST uri` as `user_id` with a JSON body.
pub async fn post_json(
    server: &Server,
    uri: &str,
    user_id: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let request = Request::post(uri)
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", server.token(user_id)),
        )
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, _, body) = server.send(request).await;
    (status, body)
}

/// `POST /v1/push/subscribe` as `user_id`; asserts the body is a contract `PushSubscribeRequest`.
pub async fn subscribe(server: &Server, user_id: &str, body: &Value) -> (StatusCode, Value) {
    assert_contract("PushSubscribeRequest", body);
    post_json(server, "/v1/push/subscribe", user_id, body).await
}

/// Subscribes and asserts 204 with no body.
pub async fn subscribe_ok(server: &Server, user_id: &str, body: &Value) {
    let (status, response) = subscribe(server, user_id, body).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{response}");
    assert_eq!(response, Value::Null);
}

/// The stored subscriptions as (user_id, platform, endpoint or token), in id order.
pub async fn stored(pool: &PgPool) -> Vec<(String, String, String)> {
    sqlx::query_as(
        "SELECT user_id, platform, COALESCE(endpoint, token) FROM ferry.push_subscriptions ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

/// `stored`'s rows from string slices, for comparisons.
pub fn rows(list: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    list.iter()
        .map(|(u, p, e)| (u.to_string(), p.to_string(), e.to_string()))
        .collect()
}

/// Waits until `ferry.push_subscriptions` holds `n` rows (5 s at most).
pub async fn wait_for_rows(pool: &PgPool, n: i64) {
    let deadline = tokio::time::Instant::now() + crate::realtime_support::RECV_TIMEOUT;
    loop {
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM ferry.push_subscriptions")
            .fetch_one(pool)
            .await
            .unwrap();
        if count == n {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{count} subscriptions, not {n}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
