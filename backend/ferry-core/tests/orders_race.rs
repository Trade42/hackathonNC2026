//! Placing an order and closing the window race on the run's row lock: an order either lands
//! before the close, and the close funds the card with it, or it gets 409 `window_closed` and
//! leaves nothing behind.
mod common;
mod payments_support;
mod realtime_support;
mod runs_support;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use payments_support::{assert_conserved, checking};
use runs_support::{
    burrito, card_funding, chips, demo_server, open_run, order_rows, post, run_status, topup,
};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;

/// The card share (subtotal + tax) of each 201 order, after checking every other answer is
/// 409 window_closed.
fn landed_shares(answers: &[(StatusCode, Value)]) -> Vec<i64> {
    answers
        .iter()
        .filter_map(|(status, body)| match *status {
            StatusCode::CREATED => {
                assert_eq!(body["status"], "funded", "{body}");
                Some(body["subtotal_cents"].as_i64().unwrap() + body["tax_cents"].as_i64().unwrap())
            }
            StatusCode::CONFLICT => {
                common::assert_envelope(body, "window_closed");
                None
            }
            other => panic!("unexpected {other}: {body}"),
        })
        .collect()
}

#[sqlx::test(migrations = "../migrations")]
async fn concurrent_orders_and_close_either_fund_or_get_409(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    for user in ["usr_judge", "usr_dana", "usr_leo"] {
        topup(&server, user, 100_000).await;
    }
    let mut landed_total = 0;
    let mut refused_total = 0;
    for round in 0..12 {
        let run_id = open_run(&server, "usr_maya", "cir_hinton4").await;
        let orders = format!("/v1/runs/{run_id}/orders");
        let close = format!("/v1/runs/{run_id}/close");
        // The close starts with the orders or a little after them. It takes the run's lock after
        // two queries and an order after about six, so with no delay the close usually wins, and
        // with a few milliseconds the orders do; in between they truly race.
        let delay = Duration::from_millis([0, 2, 5, 10, 20, 40][round % 6]);
        let (a, b, c, closed) = tokio::join!(
            post(&server, &orders, "usr_judge", Some(chips())),
            post(&server, &orders, "usr_dana", Some(chips())),
            post(&server, &orders, "usr_leo", Some(burrito())),
            async {
                tokio::time::sleep(delay).await;
                post(&server, &close, "usr_maya", None).await
            },
        );
        assert_eq!(closed.0, StatusCode::OK, "{}", closed.1);
        assert_eq!(closed.1["status"], "closed");
        let answers = [a, b, c];
        let shares = landed_shares(&answers);
        // Every order that landed is on the card, and nothing else is.
        assert_eq!(
            card_funding(&pool, &run_id).await,
            shares.iter().sum::<i64>(),
            "round {round}: {answers:?}"
        );
        assert_eq!(order_rows(&pool, &run_id).await, shares.len() as i64);
        assert_eq!(closed.1["order_count"], shares.len() as i64);
        landed_total += shares.len();
        refused_total += answers.len() - shares.len();
    }
    // Each order either landed or got 409; across the rounds both happened.
    assert!(landed_total > 0, "no order ever landed");
    assert!(refused_total > 0, "no order was ever refused");
    assert_conserved(&pool).await;
}

/// An order that reaches the run while the close holds its lock waits, sees the run closed, and
/// gets 409 `window_closed` with nothing stored, although the run was open when it arrived.
#[sqlx::test(migrations = "../migrations")]
async fn an_order_waiting_on_the_close_gets_409(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let judge_before = checking(&pool, "usr_judge").await;

    // Stand in for a close in progress: lock the run and keep the lock.
    let mut close = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM ferry.runs WHERE id = 'run_demo' FOR UPDATE")
        .execute(&mut *close)
        .await
        .unwrap();

    let app = ferry_core::app::router(server.state.clone());
    let token = server.token("usr_judge");
    let request = Request::post("/v1/runs/run_demo/orders")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(chips().to_string()))
        .unwrap();
    let order = tokio::spawn(async move { app.oneshot(request).await.unwrap().status() });

    // Wait until the order is blocked on the run's lock.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity \
             WHERE datname = current_database() AND wait_event_type = 'Lock'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        if waiting > 0 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the order never waited on the lock"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    sqlx::query("UPDATE ferry.runs SET status = 'closed' WHERE id = 'run_demo'")
        .execute(&mut *close)
        .await
        .unwrap();
    close.commit().await.unwrap();

    assert_eq!(order.await.unwrap(), StatusCode::CONFLICT);
    assert_eq!(order_rows(&pool, "run_demo").await, 0);
    assert_eq!(checking(&pool, "usr_judge").await, judge_before);
    assert_eq!(run_status(&pool, "run_demo").await, "closed");
    let notifications: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ferry.notifications WHERE user_id = 'usr_judge'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(notifications, 0);
}
