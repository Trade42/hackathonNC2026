//! The run and order lifecycles over HTTP beyond the happy path: illegal moves are 409 and change
//! nothing, the receipt limits, cancels before and after the close, the window's time, body
//! validation, 402 with nothing stored, and a run nobody ordered on.
mod common;
mod payments_support;
mod realtime_support;
mod runs_support;

use axum::http::StatusCode;
use payments_support::{assert_conserved, card, checking, escrow, merchant};
use realtime_support::assert_contract;
use runs_support::{
    burrito, card_funding, chips, demo_server, fetzer, get, minutes_from_now, open_run, order_rows,
    post, run_status,
};
use serde_json::{Value, json};
use sqlx::PgPool;

const ESCROW_START: i64 = 100_000;

fn assert_error(answer: &(StatusCode, Value), status: StatusCode, code: &str, what: &str) {
    assert_eq!(answer.0, status, "{what}: {}", answer.1);
    common::assert_envelope(&answer.1, code);
}

async fn order(server: &realtime_support::Server, user: &str, body: Value) -> String {
    let (status, order) = post(server, "/v1/runs/run_demo/orders", user, Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{order}");
    order["id"].as_str().unwrap().to_owned()
}

async fn order_status(pool: &PgPool, order_id: &str) -> String {
    sqlx::query_scalar("SELECT status FROM ferry.orders WHERE id = $1")
        .bind(order_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn events_of(pool: &PgPool, order_id: &str) -> Vec<String> {
    sqlx::query_scalar("SELECT status FROM ferry.order_events WHERE order_id = $1 ORDER BY ts, id")
        .bind(order_id)
        .fetch_all(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../migrations")]
async fn illegal_moves_are_409_and_change_nothing(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let judge = order(&server, "usr_judge", burrito()).await;
    let handoff = format!("/v1/orders/{judge}/handoff");

    // Open: no purchase, no pickup, no handoff yet.
    let purchased = json!({"receipt_total_cents": 900});
    let answer = post(
        &server,
        "/v1/runs/run_demo/purchased",
        "usr_maya",
        Some(purchased.clone()),
    )
    .await;
    assert_error(
        &answer,
        StatusCode::CONFLICT,
        "conflict",
        "purchased while open",
    );
    assert_eq!(answer.1["error"]["details"]["status"], "open");
    let answer = post(&server, "/v1/runs/run_demo/picked-up", "usr_maya", None).await;
    assert_error(
        &answer,
        StatusCode::CONFLICT,
        "conflict",
        "picked up while open",
    );
    let answer = post(&server, &handoff, "usr_judge", None).await;
    assert_error(
        &answer,
        StatusCode::CONFLICT,
        "conflict",
        "handoff while funded",
    );
    assert_eq!(run_status(&pool, "run_demo").await, "open");
    assert_eq!(order_status(&pool, &judge).await, "funded");

    // Closed: not twice, and no pickup before the purchase.
    let (status, _) = post(&server, "/v1/runs/run_demo/close", "usr_maya", None).await;
    assert_eq!(status, StatusCode::OK);
    let answer = post(&server, "/v1/runs/run_demo/close", "usr_maya", None).await;
    assert_error(&answer, StatusCode::CONFLICT, "conflict", "close twice");
    let answer = post(&server, "/v1/runs/run_demo/picked-up", "usr_maya", None).await;
    assert_error(
        &answer,
        StatusCode::CONFLICT,
        "conflict",
        "picked up while closed",
    );

    // Purchased: no cancel of the run or the order any more, no second purchase.
    let (status, _) = post(
        &server,
        "/v1/runs/run_demo/purchased",
        "usr_maya",
        Some(purchased.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for (uri, user) in [
        ("/v1/runs/run_demo/cancel".to_owned(), "usr_maya"),
        (format!("/v1/orders/{judge}/cancel"), "usr_judge"),
    ] {
        let answer = post(&server, &uri, user, None).await;
        assert_error(&answer, StatusCode::CONFLICT, "conflict", &uri);
    }
    let answer = post(
        &server,
        "/v1/runs/run_demo/purchased",
        "usr_maya",
        Some(purchased),
    )
    .await;
    assert_error(&answer, StatusCode::CONFLICT, "conflict", "purchased twice");
    let answer = post(&server, &handoff, "usr_judge", None).await;
    assert_error(
        &answer,
        StatusCode::CONFLICT,
        "conflict",
        "handoff while purchased",
    );
    assert_eq!(order_status(&pool, &judge).await, "purchased");

    // Delivering, then delivered: nothing moves back.
    let (status, _) = post(&server, "/v1/runs/run_demo/picked-up", "usr_maya", None).await;
    assert_eq!(status, StatusCode::OK);
    let answer = post(&server, "/v1/runs/run_demo/picked-up", "usr_maya", None).await;
    assert_error(&answer, StatusCode::CONFLICT, "conflict", "picked up twice");
    let (status, _) = post(&server, &handoff, "usr_judge", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(run_status(&pool, "run_demo").await, "delivered");
    for uri in ["close", "picked-up", "cancel"] {
        let answer = post(
            &server,
            &format!("/v1/runs/run_demo/{uri}"),
            "usr_maya",
            None,
        )
        .await;
        assert_error(&answer, StatusCode::CONFLICT, "conflict", uri);
    }
    let answer = post(&server, &handoff, "usr_judge", None).await;
    assert_error(&answer, StatusCode::CONFLICT, "conflict", "handoff twice");
    let answer = post(
        &server,
        &format!("/v1/orders/{judge}/cancel"),
        "usr_judge",
        None,
    )
    .await;
    assert_error(
        &answer,
        StatusCode::CONFLICT,
        "conflict",
        "cancel after delivery",
    );
    assert_eq!(
        events_of(&pool, &judge).await,
        vec!["funded", "purchased", "picked_up", "delivered", "settled"]
    );
    assert_conserved(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn receipts_over_five_percent_are_refused_and_up_to_five_are_covered(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let judge = order(&server, "usr_judge", burrito()).await;
    let dana = order(&server, "usr_dana", chips()).await;
    post(&server, "/v1/runs/run_demo/close", "usr_maya", None).await;
    let funded = 966 + 375;
    let max = funded + funded * 5 / 100;
    let (judge_before, escrow_before) = (checking(&pool, "usr_judge").await, escrow(&pool).await);

    let answer = post(
        &server,
        "/v1/runs/run_demo/purchased",
        "usr_maya",
        Some(json!({"receipt_total_cents": max + 1})),
    )
    .await;
    assert_error(&answer, StatusCode::CONFLICT, "conflict", "over 5%");
    assert_eq!(answer.1["error"]["details"]["max_receipt_cents"], max);
    // Nothing moved, nothing changed.
    assert_eq!(run_status(&pool, "run_demo").await, "closed");
    assert_eq!(order_status(&pool, &judge).await, "funded");
    assert_eq!(order_status(&pool, &dana).await, "funded");
    assert_eq!(merchant(&pool).await, 0);
    assert_eq!(card(&pool).await, funded);
    assert_eq!(escrow(&pool).await, escrow_before);

    // Exactly 5% over: Escrow covers the difference, nobody is refunded.
    let (status, run) = post(
        &server,
        "/v1/runs/run_demo/purchased",
        "usr_maya",
        Some(json!({"receipt_total_cents": max})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(merchant(&pool).await, max);
    assert_eq!(card(&pool).await, 0);
    assert_eq!(escrow(&pool).await, escrow_before - (max - funded));
    assert_eq!(checking(&pool, "usr_judge").await, judge_before);
    assert_eq!(order_status(&pool, &judge).await, "purchased");
    assert_conserved(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn the_runner_cancels_before_and_after_the_close(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    // Before the close: every hold comes back, the orders are refunded.
    let judge = order(&server, "usr_judge", burrito()).await;
    let (status, run) = post(&server, "/v1/runs/run_demo/cancel", "usr_maya", None).await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_contract("Run", &run);
    assert_eq!(run["status"], "cancelled");
    assert_eq!(run["order_count"], 0);
    assert_eq!(order_status(&pool, &judge).await, "refunded");
    assert_eq!(events_of(&pool, &judge).await, vec!["funded", "refunded"]);
    assert_eq!(checking(&pool, "usr_judge").await, 5000);
    assert_eq!(escrow(&pool).await, ESCROW_START);
    let (_, seen) = get(&server, &format!("/v1/orders/{judge}"), "usr_judge").await;
    assert_contract("Order", &seen);
    assert_eq!(seen["status"], "refunded");
    let (_, notifications) = get(&server, "/v1/notifications", "usr_judge").await;
    assert_eq!(
        notifications[0]["title"],
        "Maya cancelled the run to Cosmic Cantina"
    );
    assert_eq!(notifications[0]["type"], "order_update");
    // A cancelled run takes no orders.
    let answer = post(
        &server,
        "/v1/runs/run_demo/orders",
        "usr_dana",
        Some(chips()),
    )
    .await;
    assert_error(
        &answer,
        StatusCode::CONFLICT,
        "window_closed",
        "order on a cancelled run",
    );

    // After the close: the card's funding goes back to Escrow first, then to the customers.
    let run_id = open_run(&server, "usr_maya", "cir_hinton4").await;
    let (status, placed) = post(
        &server,
        &format!("/v1/runs/{run_id}/orders"),
        "usr_dana",
        Some(chips()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{placed}");
    post(
        &server,
        &format!("/v1/runs/{run_id}/close"),
        "usr_maya",
        None,
    )
    .await;
    assert_eq!(card(&pool).await, 375);
    let (status, run) = post(
        &server,
        &format!("/v1/runs/{run_id}/cancel"),
        "usr_maya",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(card(&pool).await, 0);
    assert_eq!(checking(&pool, "usr_dana").await, 4000);
    assert_eq!(escrow(&pool).await, ESCROW_START);
    assert_conserved(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn customers_cancel_before_the_purchase(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let judge = order(&server, "usr_judge", burrito()).await;
    let dana = order(&server, "usr_dana", chips()).await;

    // Before the close.
    let (status, cancelled) = post(
        &server,
        &format!("/v1/orders/{judge}/cancel"),
        "usr_judge",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cancelled}");
    assert_contract("Order", &cancelled);
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(cancelled["handoff_code"].as_str().unwrap().len(), 4);
    assert_eq!(checking(&pool, "usr_judge").await, 5000);
    let (_, run) = get(&server, "/v1/runs/run_demo", "usr_maya").await;
    assert_eq!(run["order_count"], 1);
    // Every order still shows to the runner, cancelled ones too.
    assert_eq!(run["orders"].as_array().unwrap().len(), 2);
    let answer = post(
        &server,
        &format!("/v1/orders/{judge}/cancel"),
        "usr_judge",
        None,
    )
    .await;
    assert_error(&answer, StatusCode::CONFLICT, "conflict", "cancel twice");

    // After the close: Dana's share comes off the card first.
    post(&server, "/v1/runs/run_demo/close", "usr_maya", None).await;
    assert_eq!(card(&pool).await, 375);
    let (status, cancelled) = post(
        &server,
        &format!("/v1/orders/{dana}/cancel"),
        "usr_dana",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cancelled}");
    assert_eq!(card(&pool).await, 0);
    assert_eq!(checking(&pool, "usr_dana").await, 4000);
    assert_eq!(escrow(&pool).await, ESCROW_START);
    assert_eq!(events_of(&pool, &dana).await, vec!["funded", "cancelled"]);
    // With nothing left on the card, the purchase is refused; the run can only be cancelled.
    let answer = post(
        &server,
        "/v1/runs/run_demo/purchased",
        "usr_maya",
        Some(json!({"receipt_total_cents": 100})),
    )
    .await;
    assert_error(
        &answer,
        StatusCode::CONFLICT,
        "conflict",
        "purchase with nothing funded",
    );
    let (status, _) = post(&server, "/v1/runs/run_demo/cancel", "usr_maya", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_conserved(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn a_run_nobody_ordered_on(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let (status, run) = post(&server, "/v1/runs/run_demo/close", "usr_maya", None).await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(card_funding(&pool, "run_demo").await, 0);
    let answer = post(
        &server,
        "/v1/runs/run_demo/purchased",
        "usr_maya",
        Some(json!({"receipt_total_cents": 500})),
    )
    .await;
    assert_error(
        &answer,
        StatusCode::CONFLICT,
        "conflict",
        "purchase with no orders",
    );
    assert_eq!(run_status(&pool, "run_demo").await, "closed");
    let (status, run) = post(&server, "/v1/runs/run_demo/cancel", "usr_maya", None).await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(run["status"], "cancelled");
}

#[sqlx::test(migrations = "../migrations")]
async fn the_window_closes_on_time_even_while_the_run_is_open(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    sqlx::query("UPDATE ferry.runs SET window_closes_at = now() - interval '1 second' WHERE id = 'run_demo'")
        .execute(&pool)
        .await
        .unwrap();
    for uri in ["/v1/runs/run_demo/orders", "/v1/runs/run_demo/quote"] {
        let answer = post(&server, uri, "usr_judge", Some(burrito())).await;
        assert_error(&answer, StatusCode::CONFLICT, "window_closed", uri);
    }
    assert_eq!(order_rows(&pool, "run_demo").await, 0);
    // The runner may still close it.
    let (status, _) = post(&server, "/v1/runs/run_demo/close", "usr_maya", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[sqlx::test(migrations = "../migrations")]
async fn not_enough_balance_is_402_and_stores_nothing(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    // Leo has $30: ten burritos with guac are $112.37 with tax, fee and tip.
    let body = json!({
        "lines": [{
            "menu_item_id": "itm_cosmic_chicken_burrito", "qty": 10,
            "choice_ids": ["cho_cosmic_chicken_burrito_black_beans", "cho_cosmic_chicken_burrito_guac"]
        }],
        "tip_cents": 0,
        "dropoff": fetzer()
    });
    let (status, quote) = post(
        &server,
        "/v1/runs/run_demo/quote",
        "usr_leo",
        Some(body.clone()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a quote never checks the balance: {quote}"
    );
    assert_eq!(quote["subtotal_cents"], 10490);
    let answer = post(&server, "/v1/runs/run_demo/orders", "usr_leo", Some(body)).await;
    assert_error(
        &answer,
        StatusCode::PAYMENT_REQUIRED,
        "insufficient_funds",
        "too much",
    );
    assert_eq!(answer.1["error"]["details"]["balance_cents"], 3000);
    assert_eq!(order_rows(&pool, "run_demo").await, 0);
    assert_eq!(checking(&pool, "usr_leo").await, 3000);
    let stored: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM ferry.order_lines), (SELECT count(*) FROM ferry.order_events), \
                (SELECT count(*) FROM ferry.notifications WHERE user_id = 'usr_leo')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored, (0, 0, 0));
}

#[sqlx::test(migrations = "../migrations")]
async fn bad_bodies_are_400_with_every_problem(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let bad = json!({
        "lines": [
            {"menu_item_id": "itm_cosmic_chicken_burrito", "qty": 0, "choice_ids": []},
            {"menu_item_id": "itm_supdogs_hot_dog", "qty": 1, "choice_ids": []},
            {"menu_item_id": "itm_cosmic_chicken_burrito", "qty": 1,
             "choice_ids": ["cho_cosmic_chicken_burrito_black_beans", "cho_cosmic_chicken_burrito_black_beans"]}
        ],
        "tip_cents": -5,
        "dropoff": {"id": "place_nowhere", "label": " ", "location": {"lat": 95.0, "lng": 0.0}}
    });
    for uri in ["/v1/runs/run_demo/orders", "/v1/runs/run_demo/quote"] {
        let answer = post(&server, uri, "usr_judge", Some(bad.clone())).await;
        assert_error(&answer, StatusCode::BAD_REQUEST, "validation_failed", uri);
        let fields: Vec<&str> = answer.1["error"]["details"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["field"].as_str().unwrap())
            .collect();
        assert_eq!(
            fields,
            vec![
                "lines[0].qty",
                "lines[0].choice_ids",
                "lines[1].menu_item_id",
                "lines[2].choice_ids[1]",
                "tip_cents",
                "dropoff.label",
                "dropoff.location",
                "dropoff.id",
            ],
            "{}",
            answer.1
        );
    }
    // A body of the wrong shape, or none, is 400 too.
    for body in [json!({"lines": "burrito"}), json!({})] {
        let answer = post(&server, "/v1/runs/run_demo/orders", "usr_judge", Some(body)).await;
        assert_error(
            &answer,
            StatusCode::BAD_REQUEST,
            "validation_failed",
            "shape",
        );
    }
    assert_eq!(order_rows(&pool, "run_demo").await, 0);

    // Opening a run.
    let leaving = minutes_from_now(&pool, 5).await;
    let past = minutes_from_now(&pool, -30).await;
    let far = minutes_from_now(&pool, 25 * 60).await;
    for (body, field) in [
        (
            json!({"restaurant_id": "rst_cosmic", "circle_id": "cir_hinton4", "leaving_at": leaving, "window_minutes": 0}),
            "window_minutes",
        ),
        (
            json!({"restaurant_id": "rst_cosmic", "circle_id": "cir_hinton4", "leaving_at": leaving, "window_minutes": 121}),
            "window_minutes",
        ),
        (
            json!({"restaurant_id": "rst_cosmic", "circle_id": "cir_hinton4", "leaving_at": "6pm", "window_minutes": 10}),
            "leaving_at",
        ),
        (
            json!({"restaurant_id": "rst_cosmic", "circle_id": "cir_hinton4", "leaving_at": "2026-02-30T18:00:00Z", "window_minutes": 10}),
            "leaving_at",
        ),
        // An offset Postgres refuses (22009) is the field's own error, as 22007 and 22008 are.
        (
            json!({"restaurant_id": "rst_cosmic", "circle_id": "cir_hinton4", "leaving_at": "2026-10-11T18:00:00+16:00", "window_minutes": 10}),
            "leaving_at",
        ),
        (
            json!({"restaurant_id": "rst_cosmic", "circle_id": "cir_hinton4", "leaving_at": past, "window_minutes": 10}),
            "leaving_at",
        ),
        (
            json!({"restaurant_id": "rst_cosmic", "circle_id": "cir_hinton4", "leaving_at": far, "window_minutes": 10}),
            "leaving_at",
        ),
        (
            json!({"restaurant_id": "rst_nowhere", "circle_id": "cir_hinton4", "leaving_at": leaving, "window_minutes": 10}),
            "restaurant_id",
        ),
    ] {
        let answer = post(&server, "/v1/runs", "usr_maya", Some(body.clone())).await;
        assert_error(
            &answer,
            StatusCode::BAD_REQUEST,
            "validation_failed",
            &body.to_string(),
        );
        assert_eq!(
            answer.1["error"]["details"][0]["field"], field,
            "{}",
            answer.1
        );
    }
    // Receipts: at least a cent, and a path under the run's own folder.
    post(
        &server,
        "/v1/runs/run_demo/orders",
        "usr_judge",
        Some(burrito()),
    )
    .await;
    post(&server, "/v1/runs/run_demo/close", "usr_maya", None).await;
    for body in [
        json!({"receipt_total_cents": 0}),
        json!({"receipt_total_cents": 900, "receipt_path": "run_demo/"}),
        json!({"receipt_total_cents": 900, "receipt_path": "run_demo/../run_x/r.jpg"}),
        json!({"receipt_total_cents": 900, "receipt_path": "run_demox/r.jpg"}),
        // `..` inside a name: the field's own 400, not the CHECK's generic one.
        json!({"receipt_total_cents": 900, "receipt_path": "run_demo/r..jpg"}),
        json!({"receipt_total_cents": 900, "receipt_path": "run_demo/..r.jpg"}),
    ] {
        let answer = post(
            &server,
            "/v1/runs/run_demo/purchased",
            "usr_maya",
            Some(body.clone()),
        )
        .await;
        assert_error(
            &answer,
            StatusCode::BAD_REQUEST,
            "validation_failed",
            &body.to_string(),
        );
        if body.get("receipt_path").is_some() {
            assert_eq!(
                answer.1["error"]["details"][0]["field"], "receipt_path",
                "{}",
                answer.1
            );
        }
    }
    assert_eq!(run_status(&pool, "run_demo").await, "closed");
}
