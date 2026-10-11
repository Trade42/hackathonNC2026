//! Authorization on every runs and orders endpoint, for an outsider (in no circle with the run),
//! another customer (Dana, with her own order on the run) and the runner (Maya), around the
//! judge's order on run_demo. Without a token every endpoint but the public preview is 401.
mod common;
mod payments_support;
mod realtime_support;
mod runs_support;

use axum::http::{Method, StatusCode};
use realtime_support::{Server, assert_contract};
use runs_support::{OUTSIDER, RUN, burrito, call, chips, demo_server, get, minutes_from_now, post};
use serde_json::{Value, json};
use sqlx::PgPool;

/// The judge's and Dana's orders on run_demo.
async fn two_orders(server: &Server) -> (String, String) {
    let (status, judge) = post(
        server,
        "/v1/runs/run_demo/orders",
        "usr_judge",
        Some(burrito()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{judge}");
    let (status, dana) = post(
        server,
        "/v1/runs/run_demo/orders",
        "usr_dana",
        Some(chips()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{dana}");
    (
        judge["id"].as_str().unwrap().to_owned(),
        dana["id"].as_str().unwrap().to_owned(),
    )
}

fn assert_error(answer: &(StatusCode, Value), status: StatusCode, code: &str, what: &str) {
    assert_eq!(answer.0, status, "{what}: {}", answer.1);
    common::assert_envelope(&answer.1, code);
    assert_contract("Error", &answer.1);
}

#[sqlx::test(migrations = "../migrations")]
async fn outsiders_get_404_everywhere(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let (judge_order, _) = two_orders(&server).await;
    let order = format!("/v1/orders/{judge_order}");
    for (method, uri, body) in [
        (Method::GET, "/v1/runs/run_demo".to_owned(), None),
        (
            Method::POST,
            "/v1/runs/run_demo/orders".to_owned(),
            Some(chips()),
        ),
        (
            Method::POST,
            "/v1/runs/run_demo/quote".to_owned(),
            Some(chips()),
        ),
        (Method::POST, "/v1/runs/run_demo/close".to_owned(), None),
        (
            Method::POST,
            "/v1/runs/run_demo/purchased".to_owned(),
            Some(json!({"receipt_total_cents": 100})),
        ),
        (Method::POST, "/v1/runs/run_demo/picked-up".to_owned(), None),
        (Method::POST, "/v1/runs/run_demo/cancel".to_owned(), None),
        (Method::GET, order.clone(), None),
        (
            Method::POST,
            format!("{order}/handoff"),
            Some(json!({"code": "1234"})),
        ),
        (Method::POST, format!("{order}/cancel"), None),
    ] {
        let token = server.token(OUTSIDER);
        let answer = call(&server, method.clone(), &uri, Some(&token), body).await;
        assert_error(
            &answer,
            StatusCode::NOT_FOUND,
            "not_found",
            &format!("{method} {uri}"),
        );
    }
    // Unknown ids are the same 404.
    for uri in ["/v1/runs/run_nope", "/v1/orders/ord_nope"] {
        let answer = get(&server, uri, "usr_judge").await;
        assert_error(&answer, StatusCode::NOT_FOUND, "not_found", uri);
    }
    // Lists show the outsider nothing of the circle's.
    let (status, runs) = get(&server, "/v1/runs", OUTSIDER).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(runs, json!([]));
    let (_, orders) = get(&server, "/v1/orders", OUTSIDER).await;
    assert_eq!(orders, json!([]));
    // Nor may they open a run in a circle they are not in.
    let leaving = minutes_from_now(&pool, 5).await;
    let answer = post(
        &server,
        "/v1/runs",
        OUTSIDER,
        Some(json!({"restaurant_id": "rst_cosmic", "circle_id": "cir_hinton4", "leaving_at": leaving, "window_minutes": 10})),
    )
    .await;
    assert_error(
        &answer,
        StatusCode::NOT_FOUND,
        "not_found",
        "open in another circle",
    );
    // The judge is not in Climbing crew either.
    let answer = post(
        &server,
        "/v1/runs",
        "usr_judge",
        Some(json!({"restaurant_id": "rst_cosmic", "circle_id": "cir_climb", "leaving_at": leaving, "window_minutes": 10})),
    )
    .await;
    assert_error(
        &answer,
        StatusCode::NOT_FOUND,
        "not_found",
        "open in cir_climb",
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn another_customer_sees_only_their_own(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let (judge_order, dana_order) = two_orders(&server).await;
    let order = format!("/v1/orders/{judge_order}");
    // Dana may not read, hand off or cancel the judge's order: it does not exist for her.
    for (method, uri, body) in [
        (Method::GET, order.clone(), None),
        (Method::POST, format!("{order}/handoff"), None),
        (Method::POST, format!("{order}/cancel"), None),
    ] {
        let token = server.token("usr_dana");
        let answer = call(&server, method.clone(), &uri, Some(&token), body).await;
        assert_error(
            &answer,
            StatusCode::NOT_FOUND,
            "not_found",
            &format!("{method} {uri}"),
        );
    }
    // She sees the run with only her own order, with her code, and no runner location.
    let (status, detail) = get(&server, "/v1/runs/run_demo", "usr_dana").await;
    assert_eq!(status, StatusCode::OK);
    assert_contract("RunDetail", &detail);
    let orders = detail["orders"].as_array().unwrap();
    assert_eq!(orders.len(), 1);
    assert_eq!(orders[0]["id"], dana_order);
    assert!(orders[0]["handoff_code"].is_string());
    assert_eq!(detail["order_count"], 2);
    let (_, list) = get(&server, "/v1/orders", "usr_dana").await;
    let ids: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![dana_order.as_str()]);
    // The runner's steps are the runner's.
    for uri in ["close", "picked-up", "cancel"] {
        let answer = post(
            &server,
            &format!("/v1/runs/run_demo/{uri}"),
            "usr_dana",
            None,
        )
        .await;
        assert_error(&answer, StatusCode::FORBIDDEN, "forbidden", uri);
    }
    let answer = post(
        &server,
        "/v1/runs/run_demo/purchased",
        "usr_dana",
        Some(json!({"receipt_total_cents": 1000})),
    )
    .await;
    assert_error(&answer, StatusCode::FORBIDDEN, "forbidden", "purchased");
    // Leo, in the circle with no order, sees the run and no orders.
    let (status, detail) = get(&server, "/v1/runs/run_demo", "usr_leo").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["orders"], json!([]));
}

#[sqlx::test(migrations = "../migrations")]
async fn the_runner_sees_everything_but_codes_and_cannot_order(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let (judge_order, _) = two_orders(&server).await;
    let order = format!("/v1/orders/{judge_order}");
    let (status, seen) = get(&server, &order, "usr_maya").await;
    assert_eq!(status, StatusCode::OK);
    assert_contract("Order", &seen);
    assert!(
        seen["handoff_code"].is_null(),
        "the runner never gets the code"
    );
    let (_, seen) = get(&server, &order, "usr_judge").await;
    assert!(seen["handoff_code"].is_string());
    let (_, detail) = get(&server, "/v1/runs/run_demo", "usr_maya").await;
    assert_eq!(detail["orders"].as_array().unwrap().len(), 2);
    assert!(
        detail["orders"]
            .as_array()
            .unwrap()
            .iter()
            .all(|o| o["handoff_code"].is_null())
    );
    // The runner cannot order or quote on their own run, nor cancel a customer's order.
    for uri in ["/v1/runs/run_demo/orders", "/v1/runs/run_demo/quote"] {
        let answer = post(&server, uri, "usr_maya", Some(chips())).await;
        assert_error(&answer, StatusCode::FORBIDDEN, "forbidden", uri);
    }
    let answer = post(&server, &format!("{order}/cancel"), "usr_maya", None).await;
    assert_error(
        &answer,
        StatusCode::FORBIDDEN,
        "forbidden",
        "runner cancels",
    );
    // The runner cannot hand off an order that is not picked up: the status is refused before any
    // code is checked or counted (demo-walk-hook review MINOR 3). handoff_rules covers the code on
    // a picked_up order.
    let answer = post(&server, &format!("{order}/handoff"), "usr_maya", None).await;
    assert_error(&answer, StatusCode::CONFLICT, "conflict", "no code, funded");
    assert_eq!(answer.1["error"]["details"]["status"], "funded");
    // The runner's own runs, and not the customers'.
    let (_, mine) = get(&server, "/v1/runs?mine=true", "usr_maya").await;
    assert_eq!(mine.as_array().unwrap().len(), 1);
    assert_eq!(mine[0]["id"], RUN);
    let (_, mine) = get(&server, "/v1/runs?mine=true", "usr_judge").await;
    assert_eq!(mine, json!([]));
    // The runner's orders list is their own orders as a customer: none.
    let (_, list) = get(&server, "/v1/orders?mine=true", "usr_maya").await;
    assert_eq!(list, json!([]));
}

#[sqlx::test(migrations = "../migrations")]
async fn a_customer_who_left_the_circle_keeps_their_order_but_cannot_order_again(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let (judge_order, _) = two_orders(&server).await;
    sqlx::query("DELETE FROM ferry.circle_members WHERE circle_id = 'cir_hinton4' AND user_id = 'usr_judge'")
        .execute(&pool)
        .await
        .unwrap();
    let (status, _) = get(&server, &format!("/v1/orders/{judge_order}"), "usr_judge").await;
    assert_eq!(status, StatusCode::OK);
    let (status, detail) = get(&server, "/v1/runs/run_demo", "usr_judge").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["orders"].as_array().unwrap().len(), 1);
    let answer = post(
        &server,
        "/v1/runs/run_demo/orders",
        "usr_judge",
        Some(chips()),
    )
    .await;
    assert_error(
        &answer,
        StatusCode::FORBIDDEN,
        "forbidden",
        "order after leaving",
    );
    // The circle's list no longer shows it to them.
    let (_, runs) = get(&server, "/v1/runs", "usr_judge").await;
    assert_eq!(runs, json!([]));
}

#[sqlx::test(migrations = "../migrations")]
async fn every_endpoint_but_the_preview_needs_a_token(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    for (method, uri) in [
        (Method::GET, "/v1/runs"),
        (Method::POST, "/v1/runs"),
        (Method::GET, "/v1/runs/run_demo"),
        (Method::POST, "/v1/runs/run_demo/orders"),
        (Method::POST, "/v1/runs/run_demo/quote"),
        (Method::POST, "/v1/runs/run_demo/close"),
        (Method::POST, "/v1/runs/run_demo/purchased"),
        (Method::POST, "/v1/runs/run_demo/picked-up"),
        (Method::POST, "/v1/runs/run_demo/cancel"),
        (Method::GET, "/v1/orders"),
        (Method::GET, "/v1/orders/ord_x"),
        (Method::POST, "/v1/orders/ord_x/handoff"),
        (Method::POST, "/v1/orders/ord_x/cancel"),
    ] {
        let answer = call(&server, method.clone(), uri, None, None).await;
        assert_error(
            &answer,
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            &format!("{method} {uri}"),
        );
    }
    // The preview needs none, and says only its four fields.
    let (status, preview) =
        call(&server, Method::GET, "/v1/public/runs/run_demo", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_contract("RunPreview", &preview);
    assert_eq!(
        preview.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec![
            "restaurant_name",
            "runner_display_name",
            "status",
            "window_closes_at"
        ]
    );
    assert_eq!(preview["restaurant_name"], "Cosmic Cantina");
    assert_eq!(preview["runner_display_name"], "Maya");
    assert_eq!(preview["status"], "open");
    let answer = call(&server, Method::GET, "/v1/public/runs/run_nope", None, None).await;
    assert_error(
        &answer,
        StatusCode::NOT_FOUND,
        "not_found",
        "unknown preview",
    );
}
