//! The handoff follow-ups from the runs-orders review: an empty JSON body is the customer's
//! confirm, wrong codes are capped per order (money safety) and counted only on a picked_up order,
//! and a handed-off customer gets `run.updated` without the runner's location.
//!
//! The runner is a signed-up user who is no demo persona, so picked-up starts no simulated walk
//! and the streams hear only the handoffs.
mod common;
mod payments_support;
mod realtime_support;
mod runs_support;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use ferry_core::app;
use ferry_core::error::ErrorCode;
use ferry_core::orders::actions::MAX_WRONG_CODES;
use payments_support::checking;
use realtime_support::{Server, assert_silent};
use runs_support::{burrito, chips, demo_server, expect_events, get, open_run, post};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::time::Duration;

const RUNNER: &str = "usr_rae";
const QUIET: Duration = Duration::from_millis(300);

/// A picked-up run by [`RUNNER`] in `cir_hinton4`, with the judge's burrito and Dana's chips.
struct Delivery {
    run: String,
    judge_order: String,
    judge_code: String,
    dana_order: String,
    dana_code: String,
}

async fn order(server: &Server, run: &str, user: &str, body: Value) -> (String, String) {
    let (status, order) = post(server, &format!("/v1/runs/{run}/orders"), user, Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{order}");
    (
        order["id"].as_str().unwrap().to_owned(),
        order["handoff_code"].as_str().unwrap().to_owned(),
    )
}

async fn delivering(server: &Server) -> Delivery {
    run_until(server, "picked-up").await
}

/// [`delivering`]'s run and orders, with the runner's steps taken up to and including `last`.
async fn run_until(server: &Server, last: &str) -> Delivery {
    let pool = &server.state.pool;
    sqlx::query(
        "INSERT INTO ferry.users (id, phone_e164, display_name, avatar_seed, bubble_color) \
         VALUES ($1, '+19195550177', 'Rae', $1, '#225588')",
    )
    .bind(RUNNER)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO ferry.circle_members (circle_id, user_id) VALUES ('cir_hinton4', $1)")
        .bind(RUNNER)
        .execute(pool)
        .await
        .unwrap();
    let run = open_run(server, RUNNER, "cir_hinton4").await;
    let (judge_order, judge_code) = order(server, &run, "usr_judge", burrito()).await;
    let (dana_order, dana_code) = order(server, &run, "usr_dana", chips()).await;
    for (step, body) in [
        ("close", None),
        ("purchased", Some(json!({"receipt_total_cents": 966 + 375}))),
        ("picked-up", None),
    ] {
        let (status, body) = post(server, &format!("/v1/runs/{run}/{step}"), RUNNER, body).await;
        assert_eq!(status, StatusCode::OK, "{step}: {body}");
        if step == last {
            break;
        }
    }
    Delivery {
        run,
        judge_order,
        judge_code,
        dana_order,
        dana_code,
    }
}

/// `POST /v1/orders/{order}/handoff` as `user` with this raw body and content type.
async fn handoff_raw(
    server: &Server,
    order: &str,
    user: &str,
    content_type: Option<&str>,
    body: &'static str,
) -> (StatusCode, Value) {
    let mut request = Request::post(format!("/v1/orders/{order}/handoff")).header(
        header::AUTHORIZATION,
        format!("Bearer {}", server.token(user)),
    );
    if let Some(content_type) = content_type {
        request = request.header(header::CONTENT_TYPE, content_type);
    }
    let (status, _, body) = server.send(request.body(Body::from(body)).unwrap()).await;
    (status, body)
}

/// The runner enters `code` on `order`.
async fn runner_code(server: &Server, order: &str, code: &str) -> (StatusCode, Value) {
    post(
        server,
        &format!("/v1/orders/{order}/handoff"),
        RUNNER,
        Some(json!({"code": code})),
    )
    .await
}

async fn order_status(pool: &PgPool, order: &str) -> String {
    sqlx::query_scalar("SELECT status FROM ferry.orders WHERE id = $1")
        .bind(order)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn payouts(pool: &PgPool, order: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM ferry.ledger_entries WHERE order_id = $1 AND kind = 'payout'",
    )
    .bind(order)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// A 4-digit code that is not `code`, different for each `i`.
fn wrong(code: &str, i: u32) -> String {
    let code: u32 = code.parse().unwrap();
    format!("{:04}", (code + 1 + i) % 10_000)
}

fn assert_error(answer: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(answer.0, status, "{}", answer.1);
    common::assert_envelope(&answer.1, code);
}

// ---------- An empty body ----------

#[sqlx::test(migrations = "../migrations")]
async fn an_empty_json_body_is_the_customers_confirm(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let d = delivering(&server).await;
    let json = Some("application/json");

    // The runner still needs a code: an empty body is 400, and not a wrong code.
    let answer = handoff_raw(&server, &d.judge_order, RUNNER, json, "").await;
    assert_error(&answer, StatusCode::BAD_REQUEST, "validation_failed");
    assert_eq!(answer.1["error"]["details"][0]["field"], "code");

    // The judge: Content-Type application/json and no body at all is "Got it".
    let (status, order) = handoff_raw(&server, &d.judge_order, "usr_judge", json, "").await;
    assert_eq!(status, StatusCode::OK, "{order}");
    realtime_support::assert_contract("Order", &order);
    assert_eq!(order["status"], "settled");

    // A non-empty body must still be JSON of the right shape.
    for (content_type, body) in [
        (json, "{"),
        (json, " "),
        (json, "null"),
        (json, r#"{"code": 1234}"#),
        (Some("text/plain"), "{}"),
    ] {
        let answer = handoff_raw(&server, &d.dana_order, "usr_dana", content_type, body).await;
        assert_error(&answer, StatusCode::BAD_REQUEST, "validation_failed");
    }
    assert_eq!(order_status(&pool, &d.dana_order).await, "picked_up");
    // Without a content type the body is ignored, as before.
    let (status, order) = handoff_raw(&server, &d.dana_order, "usr_dana", None, "").await;
    assert_eq!(status, StatusCode::OK, "{order}");
    assert_eq!(order["status"], "settled");
}

// ---------- The wrong-code cap ----------

#[sqlx::test(migrations = "../migrations")]
async fn wrong_codes_are_capped_until_the_customer_confirms(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let d = delivering(&server).await;
    let runner_before = checking(&pool, RUNNER).await;
    assert_eq!(MAX_WRONG_CODES, 5);

    // Five wrong codes, each a 400 that says how many tries are left. Missing and malformed
    // codes in between are 400 too, but are no guesses and do not count.
    let tries_left = [
        "4 tries left",
        "3 tries left",
        "2 tries left",
        "1 try left",
        "0 tries left",
    ];
    for (i, left) in tries_left.iter().enumerate() {
        if i == 2 {
            for body in [json!({}), json!({"code": "12a4"}), json!({"code": "123"})] {
                let answer = post(
                    &server,
                    &format!("/v1/orders/{}/handoff", d.judge_order),
                    RUNNER,
                    Some(body),
                )
                .await;
                assert_error(&answer, StatusCode::BAD_REQUEST, "validation_failed");
            }
        }
        let answer = runner_code(&server, &d.judge_order, &wrong(&d.judge_code, i as u32)).await;
        assert_error(&answer, StatusCode::BAD_REQUEST, "validation_failed");
        let message = answer.1["error"]["details"][0]["message"].as_str().unwrap();
        assert!(message.contains(left), "{message}");
    }

    // The sixth is refused, and so is the right code, a malformed one and none: 429.
    for code in [wrong(&d.judge_code, 5), d.judge_code.clone(), "12a4".into()] {
        let answer = runner_code(&server, &d.judge_order, &code).await;
        assert_error(&answer, StatusCode::TOO_MANY_REQUESTS, "rate_limited");
        assert_eq!(answer.1["error"]["details"]["max_wrong_codes"], 5);
    }
    let answer = post(
        &server,
        &format!("/v1/orders/{}/handoff", d.judge_order),
        RUNNER,
        None,
    )
    .await;
    assert_error(&answer, StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    // The agent calls the same function, under the same cap.
    let err = ferry_core::orders::confirm_handoff(
        &server.state,
        RUNNER,
        &d.judge_order,
        Some(&d.judge_code),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::RateLimited);

    // Nothing moved: the order is still out, and the runner was not paid.
    assert_eq!(order_status(&pool, &d.judge_order).await, "picked_up");
    assert_eq!(payouts(&pool, &d.judge_order).await, 0);
    assert_eq!(checking(&pool, RUNNER).await, runner_before);

    // The cap is per order: Dana's code works at once.
    let (status, order) = runner_code(&server, &d.dana_order, &d.dana_code).await;
    assert_eq!(status, StatusCode::OK, "{order}");
    assert_eq!(order["status"], "settled");

    // The judge's "Got it" still settles, and pays the runner the fee and tip.
    let (status, order) = post(
        &server,
        &format!("/v1/orders/{}/handoff", d.judge_order),
        "usr_judge",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{order}");
    assert_eq!(order["status"], "settled");
    assert_eq!(payouts(&pool, &d.judge_order).await, 1);
    assert_eq!(
        checking(&pool, RUNNER).await,
        runner_before + 150 + 150 + 200
    );
    let (status, run) = get(&server, &format!("/v1/runs/{}", d.run), RUNNER).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(run["status"], "delivered");

    // Settled now, so the right code is the machine's 409, not the cap's 429.
    let answer = runner_code(&server, &d.judge_order, &d.judge_code).await;
    assert_error(&answer, StatusCode::CONFLICT, "conflict");
}

#[sqlx::test(migrations = "../migrations")]
async fn codes_count_only_while_the_order_is_picked_up(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let d = run_until(&server, "purchased").await;
    let assert_conflict = |answer: &(StatusCode, Value), status: &str| {
        assert_error(answer, StatusCode::CONFLICT, "conflict");
        assert_eq!(
            answer.1["error"]["details"]["status"], status,
            "{}",
            answer.1
        );
    };

    // Not picked up yet: the status is refused first, so no code is checked or counted.
    for i in 0..=MAX_WRONG_CODES {
        let answer = runner_code(&server, &d.judge_order, &wrong(&d.judge_code, i)).await;
        assert_conflict(&answer, "purchased");
    }
    assert_conflict(
        &runner_code(&server, &d.judge_order, &d.judge_code).await,
        "purchased",
    );

    // Picked up: the runner still has every try.
    let (status, body) = post(
        &server,
        &format!("/v1/runs/{}/picked-up", d.run),
        RUNNER,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let answer = runner_code(&server, &d.judge_order, &wrong(&d.judge_code, 0)).await;
    assert_error(&answer, StatusCode::BAD_REQUEST, "validation_failed");
    let message = answer.1["error"]["details"][0]["message"].as_str().unwrap();
    assert!(message.contains("4 tries left"), "{message}");

    // Settled by Got it: every code is the machine's 409 again, never a counted 400 or a 429.
    let (status, order) = post(
        &server,
        &format!("/v1/orders/{}/handoff", d.judge_order),
        "usr_judge",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{order}");
    assert_eq!(order["status"], "settled");
    for i in 0..=MAX_WRONG_CODES + 1 {
        let answer = runner_code(&server, &d.judge_order, &wrong(&d.judge_code, i)).await;
        assert_conflict(&answer, "settled");
    }
    assert_conflict(
        &runner_code(&server, &d.judge_order, &d.judge_code).await,
        "settled",
    );
    assert_eq!(payouts(&pool, &d.judge_order).await, 1);
}

#[sqlx::test(migrations = "../migrations")]
async fn concurrent_wrong_codes_cannot_pass_the_cap(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let d = delivering(&server).await;
    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..12 {
        let state = server.state.clone();
        let token = server.token(RUNNER);
        let uri = format!("/v1/orders/{}/handoff", d.judge_order);
        let body = json!({"code": wrong(&d.judge_code, i)}).to_string();
        tasks.spawn(async move {
            let request = Request::post(uri)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap();
            common::send(app::router(state), request).await.0
        });
    }
    let mut statuses = tasks.join_all().await;
    statuses.sort();
    let expected: Vec<StatusCode> = [StatusCode::BAD_REQUEST; 5]
        .into_iter()
        .chain([StatusCode::TOO_MANY_REQUESTS; 7])
        .collect();
    assert_eq!(statuses, expected);
    let answer = runner_code(&server, &d.judge_order, &d.judge_code).await;
    assert_error(&answer, StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    assert_eq!(order_status(&pool, &d.judge_order).await, "picked_up");
}

// ---------- run.updated for the handed-off customer ----------

#[sqlx::test(migrations = "../migrations")]
async fn the_handed_off_customer_gets_run_updated_without_the_runner(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let d = delivering(&server).await;
    // The runner's phone pinged a few seconds ago, before anyone connected.
    sqlx::query(
        "INSERT INTO ferry.location_pings (run_id, user_id, ts, lat, lng, accuracy_m) \
         VALUES ($1, $2, now() - interval '5 seconds', 35.9125, -79.0540, 8)",
    )
    .bind(&d.run)
    .bind(RUNNER)
    .execute(&pool)
    .await
    .unwrap();
    let (_, run) = get(&server, &format!("/v1/runs/{}", d.run), "usr_judge").await;
    assert!(run["runner_location"].is_object(), "{run:#}");
    let mut judge = server.connect_with_token("usr_judge").await;
    let mut dana = server.connect_with_token("usr_dana").await;
    let mut leo = server.connect_with_token("usr_leo").await;
    let mut runner = server.connect_with_token(RUNNER).await;

    // The judge taps Got it while Dana's order is still out.
    let (status, order) = post(
        &server,
        &format!("/v1/orders/{}/handoff", d.judge_order),
        "usr_judge",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{order}");
    let events = expect_events(
        &mut judge,
        "judge",
        &["notification.created", "run.updated", "order.updated"],
    )
    .await;
    let run = &events[1]["data"]["run"];
    assert_eq!(run["id"], d.run.as_str());
    assert_eq!(run["status"], "delivering");
    assert!(run["runner_location"].is_null(), "{run:#}");
    assert!(run["eta_minutes"].is_null(), "{run:#}");
    assert_eq!(events[2]["data"]["order"]["status"], "settled");
    // Only the judge: the runner hears the order and the payout, the others nothing.
    expect_events(
        &mut runner,
        "runner",
        &["notification.created", "order.updated", "wallet.updated"],
    )
    .await;
    assert_silent(&mut dana, QUIET).await;
    assert_silent(&mut leo, QUIET).await;
    assert_silent(&mut runner, QUIET).await;
    // REST agrees.
    let (_, run) = get(&server, &format!("/v1/runs/{}", d.run), "usr_judge").await;
    assert!(run["runner_location"].is_null(), "{run:#}");
    let (_, run) = get(&server, &format!("/v1/runs/{}", d.run), "usr_dana").await;
    assert!(run["runner_location"].is_object(), "{run:#}");

    // Dana's handoff is the last: the whole circle hears the run delivered, as before.
    let (status, _) = runner_code(&server, &d.dana_order, &d.dana_code).await;
    assert_eq!(status, StatusCode::OK);
    let events = expect_events(
        &mut dana,
        "dana",
        &["notification.created", "run.updated", "order.updated"],
    )
    .await;
    assert_eq!(events[1]["data"]["run"]["status"], "delivered");
    assert!(events[1]["data"]["run"]["runner_location"].is_null());
    expect_events(
        &mut runner,
        "runner",
        &[
            "notification.created",
            "run.updated",
            "order.updated",
            "wallet.updated",
        ],
    )
    .await;
    for (who, ws) in [("judge", &mut judge), ("leo", &mut leo)] {
        let events = expect_events(ws, who, &["run.updated"]).await;
        assert_eq!(events[0]["data"]["run"]["status"], "delivered");
    }
    for ws in [&mut judge, &mut dana, &mut leo, &mut runner] {
        assert_silent(ws, QUIET).await;
    }
}
