//! Shared by the requests_*.rs tests: request bodies, the stored request and notifications, and
//! the money tables a request must never touch. The server is runs_support's seeded demo, whose
//! OSRM refuses connections, so every distance is the straight line × 1.3.
//!
//! The numbers the tests rely on (docs/DECISIONS.md, Requests): from Cosmic Cantina, Fetzer Gym
//! is about 101 m (1.05 minutes at Maya's brisk pace) out of the way to Hinton James, Maya's
//! profile drop-off; the walk from Cosmic Cantina to Fetzer is about 1,124 m (11.7 minutes brisk).
#![allow(dead_code)]

use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::realtime_support::{Server, assert_contract};
use crate::runs_support::{fetzer, minutes_from_now, post};

/// The judge's request: "chicken burrito no beans" from Cosmic Cantina, $2 tip, to Fetzer within
/// 100 m, needed `minutes` from now.
pub async fn burrito_request(pool: &PgPool, minutes: i32) -> Value {
    json!({
        "restaurant_id": "rst_cosmic",
        "lines": [{
            "menu_item_id": "itm_cosmic_chicken_burrito",
            "qty": 1,
            "choice_ids": ["cho_cosmic_chicken_burrito_no_beans"]
        }],
        "tip_cents": 200,
        "dropoff": fetzer(),
        "dropoff_radius_m": 100,
        "needed_by": minutes_from_now(pool, minutes).await
    })
}

/// A classic dog with chili from Sup Dogs, no tip, to Fetzer, needed `minutes` from now.
pub async fn dog_request(pool: &PgPool, minutes: i32) -> Value {
    json!({
        "restaurant_id": "rst_supdogs",
        "lines": [{
            "menu_item_id": "itm_supdogs_classic_dog",
            "qty": 1,
            "choice_ids": ["cho_supdogs_classic_dog_chili"]
        }],
        "tip_cents": 0,
        "dropoff": fetzer(),
        "needed_by": minutes_from_now(pool, minutes).await
    })
}

/// `POST /v1/requests` as `user`: 200 and a `PickupRequest` that validates against the contract.
pub async fn create(server: &Server, user: &str, body: Value) -> Value {
    let (status, request) = post(server, "/v1/requests", user, Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{request}");
    assert_contract("PickupRequest", &request);
    let mut keys: Vec<&str> = request
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["dropoff_radius_m", "id", "run_id", "status"],
        "{request}"
    );
    request
}

/// The stored request: status, run_id and order_id.
pub async fn stored(pool: &PgPool, id: &str) -> (String, Option<String>, Option<String>) {
    sqlx::query_as("SELECT status, run_id, order_id FROM ferry.pickup_requests WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// One stored notification.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Stored {
    #[sqlx(rename = "type")]
    pub kind: String,
    pub title: String,
    pub body: String,
    pub data: Value,
}

/// `user`'s notifications, oldest first.
pub async fn notifications(pool: &PgPool, user: &str) -> Vec<Stored> {
    sqlx::query_as(
        "SELECT type, title, body, data FROM ferry.notifications WHERE user_id = $1 \
         ORDER BY created_at, id",
    )
    .bind(user)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// `user`'s notifications about requests: the match (`request_id` in a `run_open`) and every
/// `system` one.
pub async fn request_notices(pool: &PgPool, user: &str) -> Vec<Stored> {
    notifications(pool, user)
        .await
        .into_iter()
        .filter(|n| n.kind == "system" || n.data.get("request_id").is_some())
        .collect()
}

/// How many orders and ledger rows exist: a request must never add either.
pub async fn money_rows(pool: &PgPool) -> (i64, i64) {
    sqlx::query_as(
        "SELECT (SELECT count(*) FROM ferry.orders), (SELECT count(*) FROM ferry.ledger_entries)",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Sets `user`'s profile drop-off (their usual spot) to `place`, or clears it.
pub async fn set_home(pool: &PgPool, user: &str, place: Option<(f64, f64)>) {
    sqlx::query(
        "UPDATE ferry.profiles SET dropoff_place_id = NULL, \
                dropoff_label = CASE WHEN $2::float8 IS NULL THEN NULL ELSE 'Home' END, \
                dropoff_lat = $2, dropoff_lng = $3, dropoff_note = NULL \
         WHERE user_id = $1",
    )
    .bind(user)
    .bind(place.map(|p| p.0))
    .bind(place.map(|p| p.1))
    .execute(pool)
    .await
    .unwrap();
}

/// Hinton James, Maya's seeded profile drop-off.
pub const HINTON: (f64, f64) = (35.902364, -79.043231);
