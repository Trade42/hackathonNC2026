//! The demo of spec section 9 over HTTP, on the seeded fixtures with mock Nessie: the judge
//! (and Dana) order on run_demo, Maya closes the window, pays a receipt lower than funded, picks
//! up, and both orders are handed off, the judge's by "Got it" and Dana's by Maya entering
//! Dana's code. Every response is checked against contract/openapi.yaml, the ledger after each
//! step, and every event each of the four people receives on /v1/stream, in order. The pickup
//! starts Maya's simulated walk (demo step 4), whose first ping is checked too.
mod common;
mod payments_support;
mod realtime_support;
mod runs_support;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use ferry_core::demo::simulator::stop_walk;
use ferry_core::payments::math;
use ferry_core::tracking::{fallback_distance_m, latlng};
use payments_support::{assert_conserved, card, checking, escrow, merchant};
use realtime_support::{Ws, assert_contract, assert_silent, next_json};
use runs_support::{
    RUN, WEB_URL, burrito, call, chips, demo_server, event_schema, expect_events, sign_in,
};
use serde_json::{Value, json};
use sqlx::PgPool;

const QUIET: Duration = Duration::from_millis(300);

const JUDGE_TOTAL: i64 = 1316;
const JUDGE_SHARE: i64 = 966;
const DANA_TOTAL: i64 = 525;
const DANA_SHARE: i64 = 375;
const FUNDED: i64 = JUDGE_SHARE + DANA_SHARE;
const RECEIPT: i64 = 1300;
const ESCROW_START: i64 = 100_000;

struct Person {
    token: String,
    ws: Ws,
}

async fn person(server: &realtime_support::Server, persona: &'static str) -> Person {
    let token = sign_in(server, persona).await;
    let ws = server
        .connect(&format!("?token={token}"))
        .await
        .expect("stream connects");
    Person { token, ws }
}

async fn as_person(
    server: &realtime_support::Server,
    who: &Person,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    call(server, method, uri, Some(&who.token), body).await
}

/// The next `kinds.len() + 1` events on `ws`: `kinds` in this order, and somewhere among them
/// (it races the pickup's own events) the first `runner.location` of Maya's simulated walk, at
/// Cosmic Cantina. Each must validate as `ServerEvent` and as its own event schema. Returns the
/// events of `kinds`.
async fn expect_events_and_the_first_ping(ws: &mut Ws, who: &str, kinds: &[&str]) -> Vec<Value> {
    let mut events = Vec::with_capacity(kinds.len());
    let mut pings = Vec::new();
    for _ in 0..=kinds.len() {
        let event = next_json(ws).await;
        let kind = event["type"].as_str().unwrap().to_owned();
        assert_contract("ServerEvent", &event);
        assert_contract(event_schema(&kind), &event);
        if kind == "runner.location" {
            pings.push(event);
        } else {
            events.push(event);
        }
    }
    assert_eq!(pings.len(), 1, "{who}: {pings:#?}");
    let ping = &pings[0]["data"];
    assert_eq!(ping["run_id"], RUN);
    assert!(
        (ping["location"]["lat"].as_f64().unwrap() - 35.913211).abs() < 1e-9,
        "{ping:#}"
    );
    assert!(
        (ping["location"]["lng"].as_f64().unwrap() + 79.055094).abs() < 1e-9,
        "{ping:#}"
    );
    let got: Vec<&str> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(got, kinds, "{who}");
    events
}

/// The `run` of a run.updated event, checked: `runner_location` and `eta_minutes` only for those
/// who may see the runner.
fn run_of(event: &Value, status: &str, sees_runner: bool) -> Value {
    let run = event["data"]["run"].clone();
    assert_eq!(run["id"], RUN);
    assert_eq!(run["status"], status, "{run:#}");
    assert_eq!(
        !run["runner_location"].is_null(),
        sees_runner,
        "runner_location: {run:#}"
    );
    if !sees_runner {
        assert!(run["eta_minutes"].is_null(), "{run:#}");
    }
    run
}

/// The `order` of an order.updated event, checked: `handoff_code` only in the customer's copy.
fn order_of(event: &Value, id: &str, status: &str, customer_copy: bool) -> Value {
    let order = event["data"]["order"].clone();
    assert_eq!(order["id"], id);
    assert_eq!(order["status"], status, "{order:#}");
    assert_eq!(
        order["handoff_code"].is_string(),
        customer_copy,
        "handoff_code: {order:#}"
    );
    order
}

fn wallet_of(event: &Value) -> i64 {
    event["data"]["wallet"]["balance_cents"].as_i64().unwrap()
}

/// A notification.created event's notification, checked: its type, title and deep link.
fn notification_of(event: &Value, kind: &str, url: &str) -> Value {
    let notification = event["data"]["notification"].clone();
    assert_contract("Notification", &notification);
    assert_eq!(notification["type"], kind, "{notification:#}");
    assert_eq!(notification["data"]["url"], url, "{notification:#}");
    assert_eq!(notification["read"], false);
    notification
}

#[sqlx::test(migrations = "../migrations")]
async fn the_section_9_demo_end_to_end(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    // Maya is walking to Cosmic Cantina: one ping a few seconds old, stored before anyone
    // connects, so it sends no runner.location event.
    sqlx::query(
        "INSERT INTO ferry.location_pings (run_id, user_id, ts, lat, lng, accuracy_m) \
         VALUES ($1, 'usr_maya', now() - interval '5 seconds', 35.9125, -79.0540, 8)",
    )
    .bind(RUN)
    .execute(&pool)
    .await
    .unwrap();
    let mut judge = person(&server, "judge").await;
    let mut maya = person(&server, "maya").await;
    let mut dana = person(&server, "dana").await;
    let mut leo = person(&server, "leo").await;
    let run_url = format!("{WEB_URL}/r/{RUN}");

    assert_eq!(checking(&pool, "usr_judge").await, 5000);
    assert_eq!(checking(&pool, "usr_maya").await, 2500);
    assert_eq!(checking(&pool, "usr_dana").await, 4000);
    assert_eq!(escrow(&pool).await, ESCROW_START);

    // The run as everyone sees it before any order.
    let (status, detail) = as_person(&server, &leo, Method::GET, "/v1/runs/run_demo", None).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_contract("RunDetail", &detail);
    assert_eq!(detail["status"], "open");
    assert_eq!(detail["created_via"], "message");
    assert_eq!(detail["orders"], json!([]));
    assert!(detail["runner_location"].is_null());
    let (status, mine) = as_person(&server, &maya, Method::GET, "/v1/runs/run_demo", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(mine["runner_location"].is_object(), "{mine:#}");
    assert!(mine["eta_minutes"].is_number(), "{mine:#}");

    // ---------- 2. The judge orders "chicken burrito no beans": judge to Escrow. ----------
    let (status, quote) = as_person(
        &server,
        &judge,
        Method::POST,
        "/v1/runs/run_demo/quote",
        Some(burrito()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{quote}");
    assert_contract("OrderQuote", &quote);
    assert_eq!(
        quote,
        json!({"subtotal_cents": 899, "tax_cents": 67, "ferry_fee_cents": 150, "tip_cents": 200, "total_cents": JUDGE_TOTAL})
    );

    let (status, order) = as_person(
        &server,
        &judge,
        Method::POST,
        "/v1/runs/run_demo/orders",
        Some(burrito()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{order}");
    assert_contract("Order", &order);
    let judge_order = order["id"].as_str().unwrap().to_owned();
    let judge_code = order["handoff_code"].as_str().unwrap().to_owned();
    assert_eq!(judge_code.len(), 4);
    assert_eq!(order["status"], "funded");
    assert_eq!(order["total_cents"], JUDGE_TOTAL);
    assert_eq!(order["customer"]["id"], "usr_judge");
    assert_eq!(order["runner"]["id"], "usr_maya");
    assert_eq!(order["restaurant_id"], "rst_cosmic");
    assert_eq!(
        order["lines"],
        json!([{
            "menu_item_id": "itm_cosmic_chicken_burrito", "name": "Chicken Burrito", "qty": 1,
            "choice_ids": ["cho_cosmic_chicken_burrito_no_beans"],
            "unit_price_cents": 899, "line_total_cents": 899
        }])
    );
    assert_eq!(order["dropoff"]["id"], "place_fetzer");
    assert!(order["eta_minutes"].is_number(), "{order:#}");
    assert!(order["receipt_url"].is_null());
    assert_eq!(order["timeline"].as_array().unwrap().len(), 1);
    assert_eq!(order["timeline"][0]["status"], "funded");
    assert_eq!(checking(&pool, "usr_judge").await, 5000 - JUDGE_TOTAL);
    assert_eq!(escrow(&pool).await, ESCROW_START + JUDGE_TOTAL);
    let judge_url = format!("{WEB_URL}/o/{judge_order}");

    let events = expect_events(
        &mut judge.ws,
        "judge",
        &[
            "notification.created",
            "run.updated",
            "order.updated",
            "wallet.updated",
        ],
    )
    .await;
    notification_of(&events[0], "order_update", &judge_url);
    let run = run_of(&events[1], "open", true);
    assert_eq!(run["order_count"], 1);
    assert!(run["eta_minutes"].is_number());
    let copy = order_of(&events[2], &judge_order, "funded", true);
    assert_eq!(copy["handoff_code"], judge_code);
    assert_eq!(wallet_of(&events[3]), 5000 - JUDGE_TOTAL);
    let events = expect_events(&mut maya.ws, "maya", &["run.updated", "order.updated"]).await;
    run_of(&events[0], "open", true);
    order_of(&events[1], &judge_order, "funded", false);
    for (who, p) in [("dana", &mut dana), ("leo", &mut leo)] {
        let events = expect_events(&mut p.ws, who, &["run.updated"]).await;
        assert_eq!(run_of(&events[0], "open", false)["order_count"], 1);
    }

    // Dana orders chips and salsa to a map pin.
    let (status, order) = as_person(
        &server,
        &dana,
        Method::POST,
        "/v1/runs/run_demo/orders",
        Some(chips()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{order}");
    assert_contract("Order", &order);
    let dana_order = order["id"].as_str().unwrap().to_owned();
    let dana_code = order["handoff_code"].as_str().unwrap().to_owned();
    assert_eq!(order["total_cents"], DANA_TOTAL);
    assert_eq!(order["lines"][0]["note"], "extra salsa");
    assert!(order["dropoff"].get("id").is_none());
    assert_eq!(order["dropoff"]["note"], "by the mailboxes");
    assert_eq!(checking(&pool, "usr_dana").await, 4000 - DANA_TOTAL);
    assert_eq!(escrow(&pool).await, ESCROW_START + JUDGE_TOTAL + DANA_TOTAL);
    let dana_url = format!("{WEB_URL}/o/{dana_order}");
    let events = expect_events(
        &mut dana.ws,
        "dana",
        &[
            "notification.created",
            "run.updated",
            "order.updated",
            "wallet.updated",
        ],
    )
    .await;
    notification_of(&events[0], "order_update", &dana_url);
    assert_eq!(run_of(&events[1], "open", true)["order_count"], 2);
    order_of(&events[2], &dana_order, "funded", true);
    assert_eq!(wallet_of(&events[3]), 4000 - DANA_TOTAL);
    let events = expect_events(&mut maya.ws, "maya", &["run.updated", "order.updated"]).await;
    run_of(&events[0], "open", true);
    order_of(&events[1], &dana_order, "funded", false);
    let events = expect_events(&mut judge.ws, "judge", &["run.updated"]).await;
    run_of(&events[0], "open", true);
    let events = expect_events(&mut leo.ws, "leo", &["run.updated"]).await;
    run_of(&events[0], "open", false);

    // The runner sees both orders, each customer only their own, Leo none.
    let (_, detail) = as_person(&server, &maya, Method::GET, "/v1/runs/run_demo", None).await;
    assert_contract("RunDetail", &detail);
    let ids: Vec<&str> = detail["orders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![judge_order.as_str(), dana_order.as_str()]);
    assert!(detail["orders"][0]["handoff_code"].is_null());
    let (_, detail) = as_person(&server, &judge, Method::GET, "/v1/runs/run_demo", None).await;
    assert_eq!(detail["orders"].as_array().unwrap().len(), 1);
    assert_eq!(detail["orders"][0]["handoff_code"], judge_code);
    let (_, detail) = as_person(&server, &leo, Method::GET, "/v1/runs/run_demo", None).await;
    assert_eq!(detail["orders"], json!([]));
    assert_eq!(detail["order_count"], 2);

    // ---------- 3. Maya closes the window: Escrow funds her Ferry Card. ----------
    let (status, run) = as_person(
        &server,
        &maya,
        Method::POST,
        "/v1/runs/run_demo/close",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_contract("Run", &run);
    assert_eq!(run["status"], "closed");
    assert_eq!(card(&pool).await, FUNDED);
    assert_eq!(
        escrow(&pool).await,
        ESCROW_START + JUDGE_TOTAL + DANA_TOTAL - FUNDED
    );
    let events = expect_events(&mut maya.ws, "maya", &["run.updated", "wallet.updated"]).await;
    run_of(&events[0], "closed", true);
    let card_funding = &events[1]["data"]["wallet"]["transactions"][0];
    assert_eq!(card_funding["kind"], "card_funding");
    assert_eq!(card_funding["amount_cents"], FUNDED);
    for (who, p, sees) in [
        ("judge", &mut judge, true),
        ("dana", &mut dana, true),
        ("leo", &mut leo, false),
    ] {
        let events = expect_events(&mut p.ws, who, &["run.updated"]).await;
        run_of(&events[0], "closed", sees);
    }
    // The window is closed: no more orders, and no quote either.
    for uri in ["/v1/runs/run_demo/orders", "/v1/runs/run_demo/quote"] {
        let (status, body) = as_person(&server, &leo, Method::POST, uri, Some(chips())).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        common::assert_envelope(&body, "window_closed");
    }

    // ---------- 3. Maya pays a lower receipt: purchase at the merchant, pro-rata refunds. ----------
    let (status, body) = as_person(
        &server,
        &maya,
        Method::POST,
        "/v1/runs/run_demo/purchased",
        Some(json!({"receipt_total_cents": RECEIPT, "receipt_path": "run_other/receipt.jpg"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    common::assert_envelope(&body, "validation_failed");
    let (status, run) = as_person(
        &server,
        &maya,
        Method::POST,
        "/v1/runs/run_demo/purchased",
        Some(json!({"receipt_total_cents": RECEIPT, "receipt_path": "run_demo/receipt.jpg"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_contract("Run", &run);
    assert_eq!(run["status"], "purchased");
    let refund = FUNDED - RECEIPT;
    let shares = math::split_pro_rata(
        refund,
        &[
            (judge_order.as_str(), JUDGE_SHARE),
            (dana_order.as_str(), DANA_SHARE),
        ],
    )
    .unwrap();
    let (judge_refund, dana_refund) = (shares[0].1, shares[1].1);
    // 41 cents back: 29.53 and 11.47, so 30 and 11.
    assert_eq!((judge_refund, dana_refund), (30, 11));
    assert_eq!(merchant(&pool).await, RECEIPT);
    assert_eq!(card(&pool).await, 0);
    assert_eq!(
        checking(&pool, "usr_judge").await,
        5000 - JUDGE_TOTAL + judge_refund
    );
    assert_eq!(
        checking(&pool, "usr_dana").await,
        4000 - DANA_TOTAL + dana_refund
    );
    let (receipt_path, receipt_total): (Option<String>, Option<i64>) =
        sqlx::query_as("SELECT receipt_path, receipt_total_cents FROM ferry.runs WHERE id = $1")
            .bind(RUN)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(receipt_path.as_deref(), Some("run_demo/receipt.jpg"));
    assert_eq!(receipt_total, Some(RECEIPT));

    let events = expect_events(
        &mut judge.ws,
        "judge",
        &[
            "notification.created",
            "run.updated",
            "order.updated",
            "wallet.updated",
        ],
    )
    .await;
    let n = notification_of(&events[0], "order_update", &judge_url);
    assert!(n["body"].as_str().unwrap().contains("$0.30"), "{n:#}");
    run_of(&events[1], "purchased", true);
    order_of(&events[2], &judge_order, "purchased", true);
    assert_eq!(wallet_of(&events[3]), 5000 - JUDGE_TOTAL + judge_refund);
    let events = expect_events(
        &mut dana.ws,
        "dana",
        &[
            "notification.created",
            "run.updated",
            "order.updated",
            "wallet.updated",
        ],
    )
    .await;
    notification_of(&events[0], "order_update", &dana_url);
    order_of(&events[2], &dana_order, "purchased", true);
    assert_eq!(wallet_of(&events[3]), 4000 - DANA_TOTAL + dana_refund);
    let events = expect_events(
        &mut maya.ws,
        "maya",
        &[
            "run.updated",
            "order.updated",
            "order.updated",
            "wallet.updated",
        ],
    )
    .await;
    run_of(&events[0], "purchased", true);
    order_of(&events[1], &judge_order, "purchased", false);
    order_of(&events[2], &dana_order, "purchased", false);
    let kinds: Vec<&str> = events[3]["data"]["wallet"]["transactions"]
        .as_array()
        .unwrap()
        .iter()
        .take(2)
        .map(|t| t["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["refund", "card_purchase"]);
    let events = expect_events(&mut leo.ws, "leo", &["run.updated"]).await;
    run_of(&events[0], "purchased", false);

    // ---------- 5. Maya picks up. ----------
    let (status, run) = as_person(
        &server,
        &maya,
        Method::POST,
        "/v1/runs/run_demo/picked-up",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_contract("Run", &run);
    assert_eq!(run["status"], "delivering");
    assert!(run["runner_location"].is_object());
    // The pickup also starts Maya's simulated walk: its first ping, at Cosmic Cantina at once,
    // reaches the judge and Dana (active orders) and Maya, never Leo.
    let events = expect_events_and_the_first_ping(
        &mut judge.ws,
        "judge",
        &["notification.created", "run.updated", "order.updated"],
    )
    .await;
    notification_of(&events[0], "order_update", &judge_url);
    run_of(&events[1], "delivering", true);
    let copy = order_of(&events[2], &judge_order, "picked_up", true);
    assert!(copy["eta_minutes"].is_number(), "{copy:#}");
    let events = expect_events_and_the_first_ping(
        &mut dana.ws,
        "dana",
        &["notification.created", "run.updated", "order.updated"],
    )
    .await;
    order_of(&events[2], &dana_order, "picked_up", true);
    let events = expect_events_and_the_first_ping(
        &mut maya.ws,
        "maya",
        &["run.updated", "order.updated", "order.updated"],
    )
    .await;
    run_of(&events[0], "delivering", true);
    // One ping is enough here: stop the walk before its next, 5 s later.
    assert!(stop_walk(RUN));
    let events = expect_events(&mut leo.ws, "leo", &["run.updated"]).await;
    run_of(&events[0], "delivering", false);

    // The judge's order while Maya walks: an ETA from her recent ping.
    let (status, order) = as_person(
        &server,
        &judge,
        Method::GET,
        &format!("/v1/orders/{judge_order}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_contract("Order", &order);
    assert!(order["eta_minutes"].as_f64().unwrap() >= 1.0, "{order:#}");
    let report = ferry_core::orders::order_status(&server.state, "usr_judge", &judge_order)
        .await
        .unwrap();
    assert!(report.distance_m.unwrap() > 0.0);
    assert!(report.seen_ago.is_some());

    // ---------- 6. Handoffs: Maya's codes are checked, the judge taps Got it. ----------
    let handoff = format!("/v1/orders/{judge_order}/handoff");
    let wrong = if judge_code == "0000" { "0001" } else { "0000" };
    for body in [
        None,
        Some(json!({})),
        Some(json!({"code": wrong})),
        Some(json!({"code": "12a4"})),
    ] {
        let (status, error) = as_person(&server, &maya, Method::POST, &handoff, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?}: {error}");
        common::assert_envelope(&error, "validation_failed");
    }
    // Dana is not part of the judge's order.
    let (status, _) = as_person(&server, &dana, Method::POST, &handoff, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, order) = as_person(&server, &judge, Method::POST, &handoff, None).await;
    assert_eq!(status, StatusCode::OK, "{order}");
    assert_contract("Order", &order);
    assert_eq!(order["status"], "settled");
    assert_eq!(order["handoff_code"], judge_code);
    assert!(order["eta_minutes"].is_null());
    let cosmic = latlng(35.913211, -79.055094);
    let expected_co2 =
        fallback_distance_m(cosmic, latlng(35.909152, -79.046905)) / 1609.344 * 400.0;
    let co2 = order["co2_saved_g"].as_f64().unwrap();
    assert!((co2 - expected_co2).abs() < 1e-6, "{co2} vs {expected_co2}");
    let timeline: Vec<&str> = order["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["status"].as_str().unwrap())
        .collect();
    assert_eq!(
        timeline,
        vec!["funded", "purchased", "picked_up", "delivered", "settled"]
    );
    // Maya's fee and tip: 150 + 200.
    assert_eq!(checking(&pool, "usr_maya").await, 2500 + 350);
    // The judge's order is settled, so his own run.updated drops Maya's location.
    let events = expect_events(
        &mut judge.ws,
        "judge",
        &["notification.created", "run.updated", "order.updated"],
    )
    .await;
    let n = notification_of(&events[0], "order_update", &judge_url);
    assert!(
        n["body"]
            .as_str()
            .unwrap()
            .contains("g CO2 saved vs a car delivery (EPA estimate)")
    );
    run_of(&events[1], "delivering", false);
    order_of(&events[2], &judge_order, "settled", true);
    let events = expect_events(
        &mut maya.ws,
        "maya",
        &["notification.created", "order.updated", "wallet.updated"],
    )
    .await;
    let n = notification_of(&events[0], "payout", &run_url);
    assert_eq!(n["title"], "+$3.50 fee and tip");
    assert_eq!(n["data"]["amount_cents"], 350);
    order_of(&events[1], &judge_order, "settled", false);
    assert_eq!(wallet_of(&events[2]), 2850);
    // Dana's order is still out, so the run is still delivering and nobody else hears a thing.
    assert_silent(&mut dana.ws, QUIET).await;
    assert_silent(&mut leo.ws, QUIET).await;
    // A second "Got it" is an illegal move.
    let (status, error) = as_person(&server, &judge, Method::POST, &handoff, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    common::assert_envelope(&error, "conflict");

    // Maya enters Dana's code: the last order, so the run is delivered.
    let (status, order) = as_person(
        &server,
        &maya,
        Method::POST,
        &format!("/v1/orders/{dana_order}/handoff"),
        Some(json!({"code": dana_code})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{order}");
    assert_contract("Order", &order);
    assert_eq!(order["status"], "settled");
    assert!(
        order["handoff_code"].is_null(),
        "the runner's copy has no code"
    );
    assert_eq!(checking(&pool, "usr_maya").await, 2500 + 350 + 150);
    let events = expect_events(
        &mut dana.ws,
        "dana",
        &["notification.created", "run.updated", "order.updated"],
    )
    .await;
    notification_of(&events[0], "order_update", &dana_url);
    run_of(&events[1], "delivered", false);
    order_of(&events[2], &dana_order, "settled", true);
    let events = expect_events(
        &mut maya.ws,
        "maya",
        &[
            "notification.created",
            "run.updated",
            "order.updated",
            "wallet.updated",
        ],
    )
    .await;
    notification_of(&events[0], "payout", &run_url);
    run_of(&events[1], "delivered", false);
    assert_eq!(wallet_of(&events[3]), 3000);
    for (who, p) in [("judge", &mut judge), ("leo", &mut leo)] {
        let events = expect_events(&mut p.ws, who, &["run.updated"]).await;
        run_of(&events[0], "delivered", false);
    }

    // ---------- After: balances, lists and notifications. ----------
    // Escrow is back where it started: the fees and tips went to Maya, the food to the merchant.
    assert_eq!(escrow(&pool).await, ESCROW_START);
    assert_eq!(merchant(&pool).await, RECEIPT);
    assert_eq!(card(&pool).await, 0);
    assert_eq!(
        checking(&pool, "usr_judge").await,
        5000 - JUDGE_TOTAL + judge_refund
    );
    assert_eq!(
        checking(&pool, "usr_dana").await,
        4000 - DANA_TOTAL + dana_refund
    );
    assert_eq!(checking(&pool, "usr_maya").await, 3000);
    assert_conserved(&pool).await;

    let (_, detail) = as_person(&server, &maya, Method::GET, "/v1/runs/run_demo", None).await;
    assert_contract("RunDetail", &detail);
    assert_eq!(detail["status"], "delivered");
    assert!(detail["runner_location"].is_null());
    assert!(detail["eta_minutes"].is_null());
    let (_, orders) = as_person(&server, &judge, Method::GET, "/v1/orders?mine=true", None).await;
    assert_eq!(orders.as_array().unwrap().len(), 1);
    assert_contract("Order", &orders[0]);
    let (_, wallet) = as_person(&server, &maya, Method::GET, "/v1/wallet", None).await;
    assert_contract("Wallet", &wallet);
    assert_eq!(wallet["balance_cents"], 3000);

    let (_, notifications) =
        as_person(&server, &judge, Method::GET, "/v1/notifications", None).await;
    let titles: Vec<&str> = notifications
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .map(|n| n["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        vec![
            "Order placed",
            "Maya bought your food",
            "Maya picked up your food",
            "Delivered"
        ]
    );
    let (_, notifications) =
        as_person(&server, &maya, Method::GET, "/v1/notifications", None).await;
    let kinds: Vec<&str> = notifications
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["type"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["payout", "payout"]);

    for (who, p) in [
        ("judge", &mut judge),
        ("maya", &mut maya),
        ("dana", &mut dana),
        ("leo", &mut leo),
    ] {
        let _ = who;
        assert_silent(&mut p.ws, QUIET).await;
    }
}

/// Step 1 in the app: Maya opens a run for her circle. Each other member gets a run_open
/// notification with the deep link, and everyone in the circle gets run.updated.
#[sqlx::test(migrations = "../migrations")]
async fn maya_opens_a_run(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let mut judge = person(&server, "judge").await;
    let mut maya = person(&server, "maya").await;
    let leaving: String = sqlx::query_scalar(
        "SELECT to_char((now() + interval '10 minutes') AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let (status, run) = as_person(
        &server,
        &maya,
        Method::POST,
        "/v1/runs",
        Some(json!({"restaurant_id": "rst_supdogs", "circle_id": "cir_hinton4", "leaving_at": leaving, "window_minutes": 15})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_contract("Run", &run);
    let run_id = run["id"].as_str().unwrap();
    assert_eq!(run_id.len(), 4 + 24, "{run_id}");
    assert_eq!(run["status"], "open");
    assert_eq!(run["created_via"], "app");
    assert_eq!(run["order_count"], 0);
    assert_eq!(run["runner"]["id"], "usr_maya");
    assert_eq!(run["restaurant"]["id"], "rst_supdogs");
    assert!(
        run["restaurant"]["is_open"].as_bool().unwrap(),
        "demo mode opens every restaurant"
    );
    let (leaves, closes): (bool, bool) = sqlx::query_as(
        "SELECT leaving_at = $2::timestamptz, window_closes_at = $2::timestamptz + interval '15 minutes' \
         FROM ferry.runs WHERE id = $1",
    )
    .bind(run_id)
    .bind(&leaving)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        leaves && closes,
        "window_closes_at is leaving_at + window_minutes"
    );

    let events = expect_events(
        &mut judge.ws,
        "judge",
        &["notification.created", "run.updated"],
    )
    .await;
    let n = notification_of(&events[0], "run_open", &format!("{WEB_URL}/r/{run_id}"));
    assert_eq!(n["title"], "Maya is heading to Sup Dogs");
    assert!(
        n["body"].as_str().unwrap().starts_with("Order by "),
        "{n:#}"
    );
    assert_eq!(n["data"]["run_id"], run_id);
    assert_eq!(events[1]["data"]["run"]["id"], run_id);
    let events = expect_events(&mut maya.ws, "maya", &["run.updated"]).await;
    assert_eq!(events[0]["data"]["run"]["id"], run_id);

    // The judge's open runs in the circle list it; Maya's own active runs list it too.
    let (status, runs) =
        as_person(&server, &judge, Method::GET, "/v1/runs?status=open", None).await;
    assert_eq!(status, StatusCode::OK);
    for run in runs.as_array().unwrap() {
        assert_contract("Run", run);
    }
    let ids: Vec<&str> = runs
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![run_id, RUN]);
    let (_, mine) = as_person(&server, &maya, Method::GET, "/v1/runs?mine=true", None).await;
    assert_eq!(mine.as_array().unwrap().len(), 2);
    let (_, mine) = as_person(&server, &judge, Method::GET, "/v1/runs?mine=true", None).await;
    assert_eq!(mine, json!([]));
    let (_, preview) = call(
        &server,
        Method::GET,
        &format!("/v1/public/runs/{run_id}"),
        None,
        None,
    )
    .await;
    assert_contract("RunPreview", &preview);
    assert_eq!(preview["restaurant_name"], "Sup Dogs");
    assert_eq!(preview["runner_display_name"], "Maya");
    assert_eq!(preview.as_object().unwrap().len(), 4);
}
