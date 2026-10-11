//! `GET /v1/orders`: the viewer's own orders as customer, newest first, without the demo's seeded
//! history; and demo-history orders and runs are read-only (handoff and cancel are 409).
mod common;
mod payments_support;
mod realtime_support;
mod runs_support;

use axum::http::StatusCode;
use realtime_support::assert_contract;
use runs_support::{burrito, chips, demo_server, get, open_run, post};
use sqlx::PgPool;

/// A delivered run from last week with one settled order for the judge, both flagged as demo
/// history, as the impact task seeds them.
async fn seed_history(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO ferry.runs (id, restaurant_id, runner_id, circle_id, status, created_via, \
                                 leaving_at, window_closes_at, is_demo_history, created_at) \
         VALUES ('run_history', 'rst_cosmic', 'usr_maya', 'cir_hinton4', 'delivered', 'message', \
                 now() - interval '7 days', now() - interval '7 days' + interval '10 minutes', true, \
                 now() - interval '7 days')",
    )
    .execute(pool)
    .await
    .unwrap();
    for (id, status) in [
        ("ord_history", "settled"),
        ("ord_history_open", "picked_up"),
    ] {
        sqlx::query(
            "INSERT INTO ferry.orders (id, run_id, customer_id, status, subtotal_cents, tax_cents, \
                 ferry_fee_cents, tip_cents, total_cents, dropoff_label, dropoff_lat, dropoff_lng, \
                 handoff_code, delivered_at, car_distance_m, co2_saved_g, is_demo_history, created_at) \
             VALUES ($1, 'run_history', 'usr_judge', $2, 899, 67, 150, 200, 1316, 'Fetzer Gym (HackNC)', \
                     35.909152, -79.046905, '1234', \
                     CASE WHEN $2 = 'settled' THEN now() - interval '7 days' END, \
                     1930, 479.7, true, now() - interval '7 days')",
        )
        .bind(id)
        .bind(status)
        .execute(pool)
        .await
        .unwrap();
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn the_orders_list_leaves_out_demo_history(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    seed_history(&pool).await;
    let (status, first) = post(
        &server,
        "/v1/runs/run_demo/orders",
        "usr_judge",
        Some(burrito()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let run_id = open_run(&server, "usr_dana", "cir_hinton4").await;
    let (status, second) = post(
        &server,
        &format!("/v1/runs/{run_id}/orders"),
        "usr_judge",
        Some(chips()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{second}");

    for uri in [
        "/v1/orders",
        "/v1/orders?mine=true",
        "/v1/orders?mine=false",
    ] {
        let (status, orders) = get(&server, uri, "usr_judge").await;
        assert_eq!(status, StatusCode::OK, "{orders}");
        let ids: Vec<&str> = orders
            .as_array()
            .unwrap()
            .iter()
            .map(|o| {
                assert_contract("Order", o);
                assert!(o["handoff_code"].is_string(), "the customer's own copies");
                assert!(o["eta_minutes"].is_null(), "no ETAs in lists");
                o["id"].as_str().unwrap()
            })
            .collect();
        // Newest first, and no demo history.
        assert_eq!(
            ids,
            vec![
                second["id"].as_str().unwrap(),
                first["id"].as_str().unwrap()
            ],
            "{uri}"
        );
    }
    // The history run stays out of the circle's run list too.
    let (_, runs) = get(&server, "/v1/runs", "usr_judge").await;
    assert!(
        runs.as_array()
            .unwrap()
            .iter()
            .all(|r| r["id"] != "run_history"),
        "{runs}"
    );
    // It can still be read by id.
    let (status, order) = get(&server, "/v1/orders/ord_history", "usr_judge").await;
    assert_eq!(status, StatusCode::OK);
    assert_contract("Order", &order);
    assert_eq!(order["status"], "settled");
}

#[sqlx::test(migrations = "../migrations")]
async fn demo_history_is_read_only(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    seed_history(&pool).await;
    for (uri, user) in [
        ("/v1/orders/ord_history_open/handoff", "usr_judge"),
        ("/v1/orders/ord_history_open/cancel", "usr_judge"),
        ("/v1/orders/ord_history/cancel", "usr_judge"),
        ("/v1/runs/run_history/cancel", "usr_maya"),
    ] {
        let (status, body) = post(&server, uri, user, None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{uri}: {body}");
        common::assert_envelope(&body, "conflict");
    }
    let status: String =
        sqlx::query_scalar("SELECT status FROM ferry.orders WHERE id = 'ord_history_open'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(status, "picked_up");
}
