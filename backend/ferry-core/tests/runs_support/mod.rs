//! Shared by the runs_*.rs and orders_*.rs tests: the seeded demo (every fixture, the personas'
//! starting balances) behind a real socket, demo sign-in, JSON calls, the demo's order bodies,
//! and stream events checked against contract/openapi.yaml.
//!
//! OSRM is `http://127.0.0.1:1`, which refuses connections at once, so every route is tracking's
//! straight-line fallback (× 1.3) and no test reaches the internet.
#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use ferry_core::state::AppState;
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::common;
use crate::realtime_support::{Server, Ws, assert_contract, next_json, serve};

pub const WEB_URL: &str = "https://ferry.example";
pub const RUN: &str = "run_demo";
pub const OUTSIDER: &str = "usr_out";

/// The fixtures and the personas' balances (judge $50, maya $25, dana $40, leo $30, Escrow
/// $1,000), plus `usr_out`, who is in no circle.
pub async fn seed(pool: &PgPool) {
    ferry_core::demo::seed::seed_pool(pool).await.unwrap();
    ferry_core::payments::seed_demo_balances(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO ferry.users (id, phone_e164, display_name, avatar_seed, bubble_color) \
         VALUES ($1, '+19195550199', 'Outsider', $1, '#123456')",
    )
    .bind(OUTSIDER)
    .execute(pool)
    .await
    .unwrap();
}

/// The seeded demo served in demo mode, with deep links on [`WEB_URL`].
pub async fn demo_server(pool: PgPool) -> Server {
    seed(&pool).await;
    serve(
        AppState::new(
            pool,
            common::config(&[
                ("DEMO_MODE", "true"),
                ("PUBLIC_WEB_URL", WEB_URL),
                ("OSRM_BASE_URL", "http://127.0.0.1:1"),
            ]),
        )
        .unwrap(),
    )
    .await
}

/// `method uri` with an optional bearer token and an optional JSON body.
pub async fn call(
    server: &Server,
    method: Method,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let request = match body {
        Some(body) => request
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string())),
        None => request.body(Body::empty()),
    }
    .unwrap();
    let (status, _, body) = server.send(request).await;
    (status, body)
}

/// `GET uri` as `user_id`.
pub async fn get(server: &Server, uri: &str, user_id: &str) -> (StatusCode, Value) {
    call(server, Method::GET, uri, Some(&server.token(user_id)), None).await
}

/// `POST uri` as `user_id`, with a JSON body or none.
pub async fn post(
    server: &Server,
    uri: &str,
    user_id: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    call(
        server,
        Method::POST,
        uri,
        Some(&server.token(user_id)),
        body,
    )
    .await
}

/// `POST /v1/auth/demo` for `persona`: the token.
pub async fn sign_in(server: &Server, persona: &str) -> String {
    let (status, session) = call(
        server,
        Method::POST,
        "/v1/auth/demo",
        None,
        Some(json!({"persona": persona})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{session}");
    session["token"].as_str().unwrap().to_owned()
}

/// Fetzer Gym, the demo's drop-off.
pub fn fetzer() -> Value {
    json!({
        "id": "place_fetzer",
        "label": "Fetzer Gym (HackNC)",
        "location": {"lat": 35.909152, "lng": -79.046905}
    })
}

/// The judge's demo order: "chicken burrito no beans", $2 tip, to Fetzer.
/// 899 + tax 67 (67.425) + fee 150 + tip 200 = 1316; card share 966.
pub fn burrito() -> Value {
    json!({
        "lines": [{
            "menu_item_id": "itm_cosmic_chicken_burrito",
            "qty": 1,
            "choice_ids": ["cho_cosmic_chicken_burrito_no_beans"]
        }],
        "tip_cents": 200,
        "dropoff": fetzer()
    })
}

/// Chips and salsa, no tip, to Hinton James (a map pin with a note).
/// 349 + tax 26 (26.175) + fee 150 + tip 0 = 525; card share 375.
pub fn chips() -> Value {
    json!({
        "lines": [{"menu_item_id": "itm_cosmic_chips_salsa", "qty": 1, "choice_ids": [], "note": "extra salsa"}],
        "tip_cents": 0,
        "dropoff": {
            "label": "Hinton James lobby",
            "location": {"lat": 35.902364, "lng": -79.043231},
            "note": "by the mailboxes"
        }
    })
}

/// The schema each stream event type's envelope must match.
pub fn event_schema(kind: &str) -> &'static str {
    match kind {
        "run.updated" => "RunUpdatedEvent",
        "order.updated" => "OrderUpdatedEvent",
        "runner.location" => "RunnerLocationEvent",
        "notification.created" => "NotificationCreatedEvent",
        "invite.updated" => "InviteUpdatedEvent",
        "wallet.updated" => "WalletUpdatedEvent",
        other => panic!("unknown event type {other}"),
    }
}

/// The next `kinds.len()` messages on `ws`, which must have exactly these types in this order;
/// each must validate as `ServerEvent` and as its own event schema.
pub async fn expect_events(ws: &mut Ws, who: &str, kinds: &[&str]) -> Vec<Value> {
    let mut events = Vec::with_capacity(kinds.len());
    for (i, kind) in kinds.iter().enumerate() {
        let event = next_json(ws).await;
        assert_eq!(
            event["type"], *kind,
            "{who}: event {i} should be {kind}, got {event:#}"
        );
        assert_contract("ServerEvent", &event);
        assert_contract(event_schema(kind), &event);
        events.push(event);
    }
    events
}

/// An ISO 8601 UTC time `minutes` from now (negative for the past), from the database clock.
pub async fn minutes_from_now(pool: &PgPool, minutes: i32) -> String {
    sqlx::query_scalar(
        "SELECT to_char((now() + make_interval(mins => $1)) AT TIME ZONE 'UTC', \
                'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"')",
    )
    .bind(minutes)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// `POST /v1/runs` as `runner` to Cosmic Cantina in `circle`, leaving in 5 minutes with a
/// 30-minute window: the new run's id.
pub async fn open_run(server: &Server, runner: &str, circle: &str) -> String {
    let leaving = minutes_from_now(&server.state.pool, 5).await;
    let (status, run) = post(
        server,
        "/v1/runs",
        runner,
        Some(json!({
            "restaurant_id": "rst_cosmic", "circle_id": circle,
            "leaving_at": leaving, "window_minutes": 30
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    run["id"].as_str().unwrap().to_owned()
}

/// `POST /v1/wallet/demo-topup` for `user`.
pub async fn topup(server: &Server, user: &str, cents: i64) {
    let (status, wallet) = post(
        server,
        "/v1/wallet/demo-topup",
        user,
        Some(json!({"amount_cents": cents})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{wallet}");
}

/// The run's card funding in cents (0 when the close funded nothing).
pub async fn card_funding(pool: &PgPool, run_id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount_cents), 0)::bigint FROM ferry.ledger_entries \
         WHERE idempotency_key = 'card_funding:' || $1",
    )
    .bind(run_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// How many orders the run has, in any status.
pub async fn order_rows(pool: &PgPool, run_id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM ferry.orders WHERE run_id = $1")
        .bind(run_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The run's status in the database.
pub async fn run_status(pool: &PgPool, run_id: &str) -> String {
    sqlx::query_scalar("SELECT status FROM ferry.runs WHERE id = $1")
        .bind(run_id)
        .fetch_one(pool)
        .await
        .unwrap()
}
