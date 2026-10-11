//! Shared by the agent_* tests: a stub sidecar that records sends and invite-link requests, a stub
//! Gemini that records every request and answers scripted replies, the app wired to both, the
//! demo seed with Telegram ids, the Telegram events the sidecar forwards, and the privacy scan of
//! what reached Gemini. Each test file declares both `mod common;` and `mod agent_support;`.
//! Nothing here calls the real Gemini or the real sidecar.
#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing;
use ferry_core::app;
use ferry_core::notifications::rfc3339;
use ferry_core::state::AppState;
use ferry_core::tracking::Routing;
use serde_json::{Value, json};
use sqlx::PgPool;

use super::common;

pub const STUB_SECRET: &str = "stub-sidecar-secret";
pub const BOT_USERNAME: &str = "Ferrygo_bot";
pub const GEMINI_KEY: &str = "test-gemini-key";
pub const GEMINI_MODEL: &str = "gemini-test";

/// The demo personas' Telegram user ids, as `seed_demo` links them. Leo stays unlinked.
pub const JUDGE_TG: &str = "111111111";
pub const MAYA_TG: &str = "222222222";
pub const DANA_TG: &str = "333333333";
/// A Telegram account with no Ferry user.
pub const STRANGER_TG: &str = "999999999";
/// Their Telegram usernames (without `@`).
pub const JUDGE_USERNAME: &str = "judge_tg";
pub const MAYA_USERNAME: &str = "maya_tg";
pub const DANA_USERNAME: &str = "dana_tg";

/// The demo group, mapped to cir_hinton4 by `map_demo_group`.
pub const GROUP_ID: &str = "-1001234567890";
pub const GROUP_TITLE: &str = "Hinton James 4th floor";

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .map(|v| v.to_str().unwrap_or_default().to_owned())
}

/// A request body as JSON; a body that is not JSON is kept as a string, so it is still recorded.
fn parse_body(bytes: &Bytes) -> Value {
    serde_json::from_slice(bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(bytes).into_owned()))
}

/// A local server for `router` on an OS-assigned port; returns its base URL.
async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    base
}

fn json_response(status: u16, body: Value) -> Response {
    let status = StatusCode::from_u16(status).expect("a valid HTTP status");
    (status, axum::Json(body)).into_response()
}

// ---------- The stub sidecar ----------

/// One `POST /send` the stub received.
#[derive(Debug, Clone)]
pub struct Sent {
    pub secret: Option<String>,
    pub body: Value,
}

impl Sent {
    /// The chat this send goes to: `space_id`, or the one `to_user_ids` entry (a DM's space id is
    /// the user's id).
    pub fn target(&self) -> Option<String> {
        send_target(&self.body)
    }
}

fn send_target(body: &Value) -> Option<String> {
    if let Some(space_id) = body["space_id"].as_str() {
        return Some(space_id.to_owned());
    }
    match &body["to_user_ids"][0] {
        Value::String(id) => Some(id.clone()),
        Value::Number(id) => Some(id.to_string()),
        _ => None,
    }
}

#[derive(Default)]
struct SidecarInner {
    sent: Vec<Sent>,
    invite_links: Vec<Value>,
    /// target -> (status, code) for `/send`.
    send_failures: HashMap<String, (u16, String)>,
    invite_failure: Option<(u16, String)>,
}

type SidecarShared = Arc<Mutex<SidecarInner>>;

/// The contract's error body the real sidecar answers with.
fn sidecar_error(status: u16, code: &str) -> Response {
    json_response(
        status,
        json!({"error": {"code": code, "message": format!("stub sidecar: {code}")}}),
    )
}

async fn sidecar_send(
    State(inner): State<SidecarShared>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let body = parse_body(&body);
    let target = send_target(&body);
    let failure = {
        let mut inner = inner.lock().unwrap();
        inner.sent.push(Sent {
            secret: header_value(&headers, "x-ferry-secret"),
            body,
        });
        target
            .as_deref()
            .and_then(|t| inner.send_failures.get(t).cloned())
    };
    match failure {
        Some((status, code)) => sidecar_error(status, &code),
        None => json_response(200, json!({"space_id": target})),
    }
}

async fn sidecar_invite_links(State(inner): State<SidecarShared>, body: Bytes) -> Response {
    let failure = {
        let mut inner = inner.lock().unwrap();
        inner.invite_links.push(parse_body(&body));
        inner.invite_failure.clone()
    };
    match failure {
        Some((status, code)) => sidecar_error(status, &code),
        None => json_response(200, json!({"url": STUB_INVITE_LINK})),
    }
}

/// What `POST /invite-links` answers unless `set_invite_link_status` says otherwise.
pub const STUB_INVITE_LINK: &str = "https://t.me/+stubInviteLink";

/// A stand-in for the sidecar's internal API on an OS-assigned local port. `POST /send` records
/// `{secret, body}` and answers 200 `{space_id}`; `POST /invite-links` records the body and
/// answers 200 `{url}`. Failures are configurable per target. Every call is recorded, failed or not.
#[derive(Clone)]
pub struct StubSidecar {
    pub base: String,
    inner: SidecarShared,
}

impl StubSidecar {
    pub async fn start() -> StubSidecar {
        let inner = SidecarShared::default();
        let router = Router::new()
            .route("/send", routing::post(sidecar_send))
            .route("/invite-links", routing::post(sidecar_invite_links))
            .with_state(inner.clone());
        StubSidecar {
            base: serve(router).await,
            inner,
        }
    }

    /// `/send` to `space_id` (or `to_user_ids: [space_id]`) answers `status` with the contract
    /// error body carrying `code`, e.g. 403 `recipient_not_reachable`.
    pub fn set_send_status(&self, space_id: &str, status: u16, code: &str) {
        self.inner
            .lock()
            .unwrap()
            .send_failures
            .insert(space_id.to_owned(), (status, code.to_owned()));
    }

    /// `/send` to `space_id` succeeds again.
    pub fn clear_send_status(&self, space_id: &str) {
        self.inner.lock().unwrap().send_failures.remove(space_id);
    }

    /// Every `/invite-links` call answers `status` with the contract error body carrying `code`,
    /// e.g. 403 `not_admin`.
    pub fn set_invite_link_status(&self, status: u16, code: &str) {
        self.inner.lock().unwrap().invite_failure = Some((status, code.to_owned()));
    }

    /// Every send received so far, in order.
    pub fn sent(&self) -> Vec<Sent> {
        self.inner.lock().unwrap().sent.clone()
    }

    /// The bodies of the sends to `space_id` (by `space_id` or `to_user_ids`), in order.
    pub fn sent_to(&self, space_id: &str) -> Vec<Value> {
        self.sent()
            .into_iter()
            .filter(|s| s.target().as_deref() == Some(space_id))
            .map(|s| s.body)
            .collect()
    }

    /// The bodies of the `/invite-links` calls, in order.
    pub fn invite_link_calls(&self) -> Vec<Value> {
        self.inner.lock().unwrap().invite_links.clone()
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
        let sent = self.sent();
        panic!(
            "the stub sidecar got {} sends, not {count}: {sent:#?}",
            sent.len()
        );
    }

    /// Asserts exactly `count` sends after a short while: nothing more arrives.
    pub async fn assert_no_more_than(&self, count: usize) {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let sent = self.sent();
        assert_eq!(sent.len(), count, "{sent:#?}");
    }
}

// ---------- The stub Gemini ----------

/// One `generateContent` request the stub received.
#[derive(Debug, Clone)]
pub struct Recorded {
    /// The full path, `/v1beta/models/<model>:generateContent`.
    pub path: String,
    /// The `x-goog-api-key` header.
    pub api_key: Option<String>,
    pub body: Value,
}

type Responder = Arc<dyn Fn(&Value) -> (u16, Value) + Send + Sync>;

#[derive(Default)]
struct GeminiInner {
    requests: Vec<Recorded>,
    queue: VecDeque<Value>,
    responder: Option<Responder>,
    down: Option<u16>,
    delay: Duration,
}

type GeminiShared = Arc<Mutex<GeminiInner>>;

/// What the stub answers once its scripted replies run out.
pub const NO_MORE_SCRIPTED: &str = "stub: no more scripted replies";

enum Answer {
    Fixed(u16, Value),
    Respond(Responder),
}

async fn gemini_generate(
    State(inner): State<GeminiShared>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let body = parse_body(&body);
    let (delay, answer) = {
        let mut inner = inner.lock().unwrap();
        inner.requests.push(Recorded {
            path: uri.path().to_owned(),
            api_key: header_value(&headers, "x-goog-api-key"),
            body: body.clone(),
        });
        let answer = if let Some(status) = inner.down {
            Answer::Fixed(
                status,
                json!({"error": {"code": status, "message": "stub down", "status": "UNAVAILABLE"}}),
            )
        } else if let Some(respond) = inner.responder.clone() {
            Answer::Respond(respond)
        } else {
            let next = inner.queue.pop_front();
            Answer::Fixed(200, next.unwrap_or_else(|| text_reply(NO_MORE_SCRIPTED)))
        };
        (inner.delay, answer)
    };
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let (status, value) = match answer {
        Answer::Fixed(status, value) => (status, value),
        Answer::Respond(respond) => respond(&body),
    };
    json_response(status, value)
}

/// A stand-in for Gemini's REST API (`POST /v1beta/models/{model}:generateContent`) on an
/// OS-assigned local port. It records every request and answers, in this order of precedence:
/// `set_down`'s error, `respond_with`'s closure, the `script` queue, then a text reply
/// [`NO_MORE_SCRIPTED`]. `set_delay` sleeps before any answer (after recording).
#[derive(Clone)]
pub struct StubGemini {
    pub base: String,
    inner: GeminiShared,
}

impl StubGemini {
    pub async fn start() -> StubGemini {
        let inner = GeminiShared::default();
        let router = Router::new()
            .route("/v1beta/models/{*rest}", routing::post(gemini_generate))
            .with_state(inner.clone());
        StubGemini {
            base: serve(router).await,
            inner,
        }
    }

    /// Queues `responses` (each a full generateContent response body), answered 200 in order.
    pub fn script(&self, responses: Vec<Value>) {
        self.inner.lock().unwrap().queue.extend(responses);
    }

    /// `f` decides (status, body) from each request body; it wins over the queue.
    pub fn respond_with(&self, f: impl Fn(&Value) -> (u16, Value) + Send + Sync + 'static) {
        self.inner.lock().unwrap().responder = Some(Arc::new(f));
    }

    /// Every call answers `status` with Gemini's error body, ahead of everything else.
    pub fn set_down(&self, status: u16) {
        self.inner.lock().unwrap().down = Some(status);
    }

    /// Gemini answers normally again.
    pub fn set_up(&self) {
        self.inner.lock().unwrap().down = None;
    }

    /// Sleeps `delay` before each answer, as a slow Gemini.
    pub fn set_delay(&self, delay: Duration) {
        self.inner.lock().unwrap().delay = delay;
    }

    /// Every request received so far, in order.
    pub fn requests(&self) -> Vec<Recorded> {
        self.inner.lock().unwrap().requests.clone()
    }

    pub fn request_count(&self) -> usize {
        self.inner.lock().unwrap().requests.len()
    }

    /// Every `functionResponse {name, response}` part in the `i`-th request's `contents`, in order.
    pub fn function_responses(&self, i: usize) -> Vec<(String, Value)> {
        let requests = self.requests();
        let request = requests
            .get(i)
            .unwrap_or_else(|| panic!("no Gemini request {i}; there are {}", requests.len()));
        let mut found = Vec::new();
        for content in request.body["contents"].as_array().into_iter().flatten() {
            for part in content["parts"].as_array().into_iter().flatten() {
                let response = &part["functionResponse"];
                if response.is_object() {
                    found.push((
                        response["name"].as_str().unwrap_or_default().to_owned(),
                        response["response"].clone(),
                    ));
                }
            }
        }
        found
    }

    /// Asserts no request reached Gemini with a name, username, phone or @mention (see
    /// [`find_leak`]).
    pub fn assert_private(&self) {
        let bodies: Vec<Value> = self.requests().into_iter().map(|r| r.body).collect();
        assert_bodies_private(&bodies);
    }
}

/// A generateContent response with one candidate holding `parts`.
fn candidate(parts: Value) -> Value {
    json!({
        "candidates": [{
            "content": {"role": "model", "parts": parts},
            "finishReason": "STOP"
        }]
    })
}

/// Gemini answering with text.
pub fn text_reply(text: &str) -> Value {
    candidate(json!([{"text": text}]))
}

/// Gemini calling one function. The part carries a `thoughtSignature`, as Gemini 3 sends it.
pub fn call_reply(name: &str, args: Value) -> Value {
    calls_reply(&[(name, args)])
}

/// Gemini calling several functions in one turn. As in Gemini 3's parallel calls, only the first
/// part carries the `thoughtSignature`.
pub fn calls_reply(calls: &[(&str, Value)]) -> Value {
    let parts: Vec<Value> = calls
        .iter()
        .enumerate()
        .map(|(i, (name, args))| {
            let mut part = json!({"functionCall": {"name": name, "args": args}});
            if i == 0 {
                part["thoughtSignature"] = json!("stub-signature");
            }
            part
        })
        .collect();
    candidate(Value::Array(parts))
}

/// Gemini answering a JSON-mode call (a classification): `value` serialized as the text.
pub fn json_reply(value: &Value) -> Value {
    text_reply(&value.to_string())
}

// ---------- Privacy ----------

/// Names, usernames and private labels that must never reach Gemini: the fixture display names,
/// the names and usernames this module's events and seeds use, and the inputs of PR #22's review
/// (a surname, a changed Telegram first name, a drop-off label and an address, a circle named
/// after a person, a handle inside parentheses, a name that is also a food). Matched as
/// case-insensitive substrings (so a user id such as `usr_maya` counts too), except "Leo", which
/// must stand alone between non-letters, so "leopard" and "oleo" do not count.
pub const FORBIDDEN: &[&str] = &[
    "Jordan",
    "Maya",
    "Dana",
    "Leo",
    JUDGE_USERNAME,
    MAYA_USERNAME,
    DANA_USERNAME,
    "Lopez",
    "Jordy",
    "Zelda",
    "Main Street",
    "Pittsboro",
    "Sam Smith",
    "sam_k99",
    "Chicken Little",
];

/// Street words that end an address ("412 Pittsboro Street").
const STREET_WORDS: &[&str] = &[
    "st",
    "street",
    "ave",
    "avenue",
    "rd",
    "road",
    "dr",
    "drive",
    "ln",
    "lane",
    "blvd",
    "boulevard",
    "ct",
    "court",
    "way",
    "pl",
    "place",
    "hwy",
];

/// Whether `text` holds a house number (1 to 5 digits) followed within 4 words by a street word.
fn has_address(text: &str) -> bool {
    let bare = |w: &str| {
        w.trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase()
    };
    let words: Vec<String> = text.split_whitespace().map(bare).collect();
    words.iter().enumerate().any(|(i, w)| {
        (1..=5).contains(&w.len())
            && w.bytes().all(|b| b.is_ascii_digit())
            && words[i + 1..words.len().min(i + 6)]
                .iter()
                .any(|next| STREET_WORDS.contains(&next.as_str()))
    })
}

/// The demo personas' phone prefix (+1 919 555 01xx).
pub const PHONE_PREFIX: &str = "+1919555";

/// The one `@word` allowed: what the agent's scrubber writes in place of an @mention.
const SCRUBBED_MENTION: &str = "someone";

/// Whether `needle` occurs in `haystack` with no letter right before or after it.
fn contains_alone(haystack: &str, needle: &str) -> bool {
    haystack.match_indices(needle).any(|(i, m)| {
        let before = haystack[..i].chars().next_back();
        let after = haystack[i + m.len()..].chars().next();
        !before.is_some_and(char::is_alphabetic) && !after.is_some_and(char::is_alphabetic)
    })
}

/// Whether `text` holds an `@` followed by a letter, other than `@someone`.
fn has_mention(text: &str) -> bool {
    text.match_indices('@').any(|(i, _)| {
        let rest = &text[i + 1..];
        if !rest.chars().next().is_some_and(char::is_alphabetic) {
            return false;
        }
        let scrubbed = rest.strip_prefix(SCRUBBED_MENTION).is_some_and(|after| {
            !after
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        });
        !scrubbed
    })
}

/// The first forbidden thing in `text`: a [`FORBIDDEN`] term, [`PHONE_PREFIX`], `"@mention"`, a
/// user id (`"usr_"`) or `"address"`.
pub fn leak_in(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    for term in FORBIDDEN {
        let needle = term.to_lowercase();
        let hit = if needle == "leo" {
            contains_alone(&lower, &needle)
        } else {
            lower.contains(&needle)
        };
        if hit {
            return Some(term);
        }
    }
    if text.contains(PHONE_PREFIX) {
        return Some(PHONE_PREFIX);
    }
    if has_mention(text) {
        return Some("@mention");
    }
    if lower.contains("usr_") {
        return Some("usr_");
    }
    if has_address(text) {
        return Some("address");
    }
    None
}

/// Every string in `value`, decoded (so an escaped line break hides nothing), keys included.
fn strings_in<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(s) => out.push(s),
        Value::Array(items) => items.iter().for_each(|v| strings_in(v, out)),
        Value::Object(map) => {
            for (key, v) in map {
                out.push(key);
                strings_in(v, out);
            }
        }
        _ => {}
    }
}

/// The index of the first body that leaks, and what it leaks: checked on the serialized body and
/// on each decoded string.
pub fn find_leak(bodies: &[Value]) -> Option<(usize, &'static str)> {
    bodies.iter().enumerate().find_map(|(i, body)| {
        let mut strings = Vec::new();
        strings_in(body, &mut strings);
        leak_in(&body.to_string())
            .or_else(|| strings.iter().find_map(|s| leak_in(s)))
            .map(|term| (i, term))
    })
}

/// Panics with the request's index and the term (never the body) if any body leaks.
pub fn assert_bodies_private(bodies: &[Value]) {
    if let Some((i, term)) = find_leak(bodies) {
        panic!("Gemini request {i} holds a forbidden term: {term:?}");
    }
}

// ---------- The app ----------

/// The app under test and the stubs it talks to. When a test's app goes away, every request its
/// stub Gemini recorded is scanned ([`StubGemini::assert_private`]), so every test that runs the
/// agent checks privacy, whatever else it asserts.
pub struct TestApp {
    pub router: Router,
    pub state: AppState,
    pub sidecar: StubSidecar,
    pub gemini: StubGemini,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.gemini.assert_private();
        }
    }
}

/// The app wired to a fresh stub sidecar and stub Gemini, in demo mode with Nessie mocked and OSRM
/// on a dead port (routes use the straight-line fallback). With `gemini_on`, Gemini is configured
/// to the stub; without it, Gemini is off (the stub still runs, so tests can assert it saw
/// nothing). `extra` overrides or adds config pairs.
pub async fn app(pool: PgPool, gemini_on: bool, extra: &[(&str, &str)]) -> TestApp {
    let sidecar = StubSidecar::start().await;
    let gemini = StubGemini::start().await;
    let mut pairs: Vec<(String, String)> = vec![
        ("SIDECAR_URL".into(), sidecar.base.clone()),
        ("SIDECAR_SHARED_SECRET".into(), STUB_SECRET.into()),
        ("TELEGRAM_BOT_USERNAME".into(), BOT_USERNAME.into()),
        ("DEMO_MODE".into(), "true".into()),
        ("PUBLIC_WEB_URL".into(), "https://ferry.example".into()),
        ("OSRM_BASE_URL".into(), "http://127.0.0.1:1".into()),
        ("NESSIE_MODE".into(), "mock".into()),
    ];
    if gemini_on {
        pairs.push(("GEMINI_API_KEY".into(), GEMINI_KEY.into()));
        pairs.push(("GEMINI_MODEL".into(), GEMINI_MODEL.into()));
        pairs.push(("GEMINI_BASE_URL".into(), gemini.base.clone()));
    }
    for (key, value) in extra {
        match pairs.iter_mut().find(|(k, _)| k == key) {
            Some(pair) => pair.1 = (*value).to_owned(),
            None => pairs.push(((*key).to_owned(), (*value).to_owned())),
        }
    }
    let borrowed: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let mut state = AppState::new(pool, common::config(&borrowed)).unwrap();
    // No pacing between OSRM requests: the dead port answers at once and every test is alone.
    let mut routing = state.routing.settings().clone();
    routing.min_interval = Duration::ZERO;
    state.routing = Routing::new(state.http.clone(), routing);
    let router = app::router(state.clone());
    TestApp {
        router,
        state,
        sidecar,
        gemini,
    }
}

// ---------- Seeds ----------

/// Every contract fixture (Jordan, Maya, Dana and Leo in cir_hinton4, the catalog, places and the
/// open run_demo with Maya running), with Jordan, Maya and Dana linked to Telegram. Leo is not.
pub async fn seed_demo(pool: &PgPool) {
    ferry_core::demo::seed::seed_pool(pool).await.unwrap();
    for (user, tg, username) in [
        ("usr_judge", JUDGE_TG, JUDGE_USERNAME),
        ("usr_maya", MAYA_TG, MAYA_USERNAME),
        ("usr_dana", DANA_TG, DANA_USERNAME),
    ] {
        sqlx::query(
            "UPDATE ferry.users SET telegram_user_id = $2, telegram_username = $3 WHERE id = $1",
        )
        .bind(user)
        .bind(tg)
        .bind(username)
        .execute(pool)
        .await
        .unwrap();
    }
}

/// A funded order on `run_id` for `customer`, dropped off at place_fetzer: $10.00 subtotal, $0.75
/// tax, $1.50 fee, $1.00 tip, $13.25 total, handoff code 4829.
pub async fn seed_active_order(pool: &PgPool, order_id: &str, customer: &str, run_id: &str) {
    let done = sqlx::query(
        "INSERT INTO ferry.orders (id, run_id, customer_id, status, subtotal_cents, tax_cents,
                                   ferry_fee_cents, tip_cents, total_cents, dropoff_place_id,
                                   dropoff_label, dropoff_lat, dropoff_lng, dropoff_note,
                                   handoff_code)
         SELECT $1, $2, $3, 'funded', 1000, 75, 150, 100, 1325, id, label, lat, lng, note, '4829'
           FROM ferry.places WHERE id = 'place_fetzer'",
    )
    .bind(order_id)
    .bind(run_id)
    .bind(customer)
    .execute(pool)
    .await
    .unwrap();
    assert_eq!(done.rows_affected(), 1, "place_fetzer is not seeded");
}

/// A location ping by `user_id` on `run_id`, measured `seconds_ago`.
pub async fn seed_ping(
    pool: &PgPool,
    run_id: &str,
    user_id: &str,
    lat: f64,
    lng: f64,
    seconds_ago: i64,
) {
    sqlx::query(
        "INSERT INTO ferry.location_pings (run_id, user_id, ts, lat, lng, accuracy_m)
         VALUES ($1, $2, now() - make_interval(secs => $3::float8), $4, $5, 8.0)",
    )
    .bind(run_id)
    .bind(user_id)
    .bind(seconds_ago as f64)
    .bind(lat)
    .bind(lng)
    .execute(pool)
    .await
    .unwrap();
}

/// The demo group ([`GROUP_ID`]) mapped to cir_hinton4, with the bot in it (not an admin).
pub async fn map_demo_group(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO ferry.message_spaces (space_id, is_group, circle_id) VALUES ($1, true, 'cir_hinton4')",
    )
    .bind(GROUP_ID)
    .execute(pool)
    .await
    .unwrap();
}

// ---------- Events, as the sidecar forwards them ----------

static EVENT_CLOCK: AtomicU64 = AtomicU64::new(0);

/// Now plus a counter's milliseconds, so no two events get the same `received_at`.
fn received_at() -> String {
    let tick = EVENT_CLOCK.fetch_add(1, Ordering::Relaxed);
    rfc3339(SystemTime::now() + Duration::from_millis(tick))
}

/// The fields every event shares.
#[allow(clippy::too_many_arguments)]
fn event(
    kind: &str,
    space_id: &str,
    is_group: bool,
    sender_id: &str,
    username: Option<&str>,
    first_name: &str,
    chat_title: Option<&str>,
    text: &str,
) -> Value {
    json!({
        "kind": kind, "channel": "telegram", "space_id": space_id, "is_group": is_group,
        "participants": [sender_id], "sender_phone": null, "sender_id": sender_id,
        "sender_name": first_name, "sender_username": username, "chat_title": chat_title,
        "text": text, "attachments": [], "location": null, "received_at": received_at()
    })
}

/// A message in the demo group.
pub fn group_message(
    sender_id: &str,
    username: Option<&str>,
    first_name: &str,
    text: &str,
) -> Value {
    event(
        "message",
        GROUP_ID,
        true,
        sender_id,
        username,
        first_name,
        Some(GROUP_TITLE),
        text,
    )
}

/// A DM to the bot from `sender_id` (a DM's space id is the sender's id).
pub fn dm(sender_id: &str, username: Option<&str>, first_name: &str, text: &str) -> Value {
    event(
        "message", sender_id, false, sender_id, username, first_name, None, text,
    )
}

/// A button press by `sender_id` in `space_id`.
pub fn press(
    space_id: &str,
    is_group: bool,
    sender_id: &str,
    username: Option<&str>,
    first_name: &str,
    callback_data: &str,
) -> Value {
    let title = is_group.then_some(GROUP_TITLE);
    let mut event = event(
        "callback", space_id, is_group, sender_id, username, first_name, title, "",
    );
    event["callback_data"] = json!(callback_data);
    event
}

/// The bot added to, promoted in or removed from `space_id` by `sender_id` (in a DM: unblocked or
/// blocked).
pub fn membership(
    space_id: &str,
    is_group: bool,
    sender_id: &str,
    chat_title: Option<&str>,
    bot_in_chat: bool,
    bot_is_admin: Option<bool>,
    migrated_from: Option<&str>,
) -> Value {
    let mut event = json!({
        "kind": "membership", "channel": "telegram", "space_id": space_id, "is_group": is_group,
        "participants": [sender_id], "sender_phone": null, "sender_id": sender_id,
        "sender_name": null, "sender_username": null, "chat_title": chat_title, "text": "",
        "attachments": [], "location": null, "received_at": received_at(),
        "bot_in_chat": bot_in_chat, "bot_is_admin": bot_is_admin
    });
    if let Some(old) = migrated_from {
        event["migrated_from_space_id"] = json!(old);
    }
    event
}

/// Posts `event` to `/internal/messages/inbound` with the sidecar's secret, asserts 200, and
/// returns the immediate `replies`.
pub async fn inbound(app: &TestApp, event: &Value) -> Vec<Value> {
    let request = Request::post("/internal/messages/inbound")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-ferry-secret", STUB_SECRET)
        .body(Body::from(event.to_string()))
        .unwrap();
    let (status, _, body) = common::send(app.router.clone(), request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["replies"]
        .as_array()
        .unwrap_or_else(|| panic!("no replies array: {body}"))
        .clone()
}
