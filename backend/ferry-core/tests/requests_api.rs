//! `POST /v1/requests` over HTTP on the seeded demo: every validation error, the closed
//! restaurant (outside DEMO_MODE), sign-in, the searching cap, and the judge's request for Cosmic
//! Cantina to Fetzer matching run_demo at once, with both notifications and their events (the
//! runner's only once per customer and run, however often the judge taps). Every
//! response is checked against contract/openapi.yaml, and no request ever adds an order or a
//! ledger row.
mod common;
mod realtime_support;
mod requests_support;
mod runs_support;

use axum::http::{Method, StatusCode};
use ferry_core::state::AppState;
use realtime_support::{Server, assert_contract, serve};
use requests_support::{
    burrito_request, create, dog_request, money_rows, notifications, request_notices, stored,
};
use runs_support::{RUN, WEB_URL, call, demo_server, expect_events, minutes_from_now, post, seed};
use serde_json::{Value, json};
use sqlx::PgPool;

async fn request_rows(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM ferry.pickup_requests")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// `body` as the judge: 400 `validation_failed` naming each of `fields`, as the contract's
/// `Error`.
async fn refused(server: &Server, body: Value, fields: &[&str]) -> Value {
    let (status, error) = post(server, "/v1/requests", "usr_judge", Some(body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    common::assert_envelope(&error, "validation_failed");
    assert_contract("Error", &error);
    let named: Vec<&str> = error["error"]["details"]
        .as_array()
        .unwrap_or_else(|| panic!("details list the problems: {error}"))
        .iter()
        .map(|d| d["field"].as_str().unwrap())
        .collect();
    for field in fields {
        assert!(named.contains(field), "{field} not in {named:?}: {error}");
    }
    error
}

#[sqlx::test(migrations = "../migrations")]
async fn every_validation_error(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let money = money_rows(&pool).await;
    let valid = burrito_request(&pool, 60).await;
    let with = |key: &str, value: Value| {
        let mut body = valid.clone();
        body[key] = value;
        body
    };
    let with_line = |key: &str, value: Value| {
        let mut body = valid.clone();
        body["lines"][0][key] = value;
        body
    };
    let with_dropoff = |path: &[&str], value: Value| {
        let mut body = valid.clone();
        let mut at = &mut body["dropoff"];
        for key in path {
            at = &mut at[*key];
        }
        *at = value;
        body
    };

    // The restaurant: unknown (so the item is not on its menu either).
    refused(
        &server,
        with("restaurant_id", json!("rst_nope")),
        &["restaurant_id", "lines[0].menu_item_id"],
    )
    .await;

    // needed_by: not a date-time, an impossible date, past, more than 24 hours ahead.
    refused(
        &server,
        with("needed_by", json!("tonight at 7")),
        &["needed_by"],
    )
    .await;
    refused(
        &server,
        with("needed_by", json!("2026-02-30T22:00:00Z")),
        &["needed_by"],
    )
    .await;
    let past = minutes_from_now(&pool, -1).await;
    let error = refused(&server, with("needed_by", json!(past)), &["needed_by"]).await;
    assert_eq!(
        error["error"]["details"][0]["message"],
        "must be in the future"
    );
    let late = minutes_from_now(&pool, 24 * 60 + 5).await;
    let error = refused(&server, with("needed_by", json!(late)), &["needed_by"]).await;
    assert_eq!(
        error["error"]["details"][0]["message"],
        "must be at most 24 hours ahead"
    );
    // Far enough ahead that the minutes overflow `int` unclamped: still the field's own message,
    // not the generic "A value in the request is invalid.".
    for far in ["7000-01-01T00:00:00Z", "9999-12-31T23:59:59-12:00"] {
        let error = refused(&server, with("needed_by", json!(far)), &["needed_by"]).await;
        assert_eq!(
            error["error"]["details"][0],
            json!({"field": "needed_by", "message": "must be at most 24 hours ahead"}),
            "{far}: {error}"
        );
    }

    // The lines, exactly as an order's.
    refused(&server, with("lines", json!([])), &["lines"]).await;
    let line = valid["lines"][0].clone();
    refused(&server, with("lines", json!(vec![line; 21])), &["lines"]).await;
    refused(&server, with_line("qty", json!(0)), &["lines[0].qty"]).await;
    refused(&server, with_line("qty", json!(21)), &["lines[0].qty"]).await;
    refused(
        &server,
        with_line("menu_item_id", json!("itm_supdogs_classic_dog")),
        &["lines[0].menu_item_id"],
    )
    .await;
    // The beans option needs exactly one choice.
    refused(
        &server,
        with_line("choice_ids", json!([])),
        &["lines[0].choice_ids"],
    )
    .await;
    refused(
        &server,
        with_line(
            "choice_ids",
            json!([
                "cho_cosmic_chicken_burrito_no_beans",
                "cho_cosmic_chicken_burrito_no_beans"
            ]),
        ),
        &["lines[0].choice_ids[1]"],
    )
    .await;
    refused(
        &server,
        with_line("choice_ids", json!(["cho_supdogs_classic_dog_chili"])),
        &["lines[0].choice_ids[0]"],
    )
    .await;
    refused(
        &server,
        with_line("note", json!("x".repeat(281))),
        &["lines[0].note"],
    )
    .await;

    // The tip.
    refused(&server, with("tip_cents", json!(-1)), &["tip_cents"]).await;
    refused(&server, with("tip_cents", json!(10_001)), &["tip_cents"]).await;

    // The drop-off: a label, valid coordinates, a known place id, a short note.
    refused(
        &server,
        with_dropoff(&["label"], json!("  ")),
        &["dropoff.label"],
    )
    .await;
    refused(
        &server,
        with_dropoff(&["location", "lat"], json!(95.0)),
        &["dropoff.location"],
    )
    .await;
    refused(
        &server,
        with_dropoff(&["id"], json!("place_nope")),
        &["dropoff.id"],
    )
    .await;
    refused(
        &server,
        with_dropoff(&["note"], json!("n".repeat(281))),
        &["dropoff.note"],
    )
    .await;

    // The radius: 50 to 5000 meters.
    refused(
        &server,
        with("dropoff_radius_m", json!(49)),
        &["dropoff_radius_m"],
    )
    .await;
    refused(
        &server,
        with("dropoff_radius_m", json!(5001)),
        &["dropoff_radius_m"],
    )
    .await;

    // Several problems: all listed in one 400.
    let mut several = with("tip_cents", json!(-5));
    several["dropoff_radius_m"] = json!(10);
    several["needed_by"] = json!("soon");
    let error = refused(
        &server,
        several,
        &["needed_by", "tip_cents", "dropoff_radius_m"],
    )
    .await;
    assert_eq!(error["error"]["details"].as_array().unwrap().len(), 3);

    // A body that is not a CreatePickupRequest: 400 before any check.
    let mut missing = valid.clone();
    missing.as_object_mut().unwrap().remove("needed_by");
    let (status, error) = post(&server, "/v1/requests", "usr_judge", Some(missing)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    common::assert_envelope(&error, "validation_failed");

    // Nothing was stored, and no money moved.
    assert_eq!(request_rows(&pool).await, 0);
    assert_eq!(money_rows(&pool).await, money);
    assert!(request_notices(&pool, "usr_judge").await.is_empty());
}

#[sqlx::test(migrations = "../migrations")]
async fn sign_in_is_required(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let body = burrito_request(&pool, 60).await;
    let (status, error) = call(&server, Method::POST, "/v1/requests", None, Some(body)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{error}");
    common::assert_envelope(&error, "unauthorized");
    assert_contract("Error", &error);
    assert_eq!(request_rows(&pool).await, 0);
}

#[sqlx::test(migrations = "../migrations")]
async fn a_restaurant_closed_until_after_needed_by_is_refused(pool: PgPool) {
    // Outside DEMO_MODE, opening hours count. Sup Dogs opens 3 hours from now (Chapel Hill time)
    // for an hour.
    seed(&pool).await;
    sqlx::query(
        "UPDATE ferry.restaurants SET hours_today = \
             to_char(now() AT TIME ZONE 'America/New_York' + interval '3 hours', 'HH24:MI') \
             || '–' || \
             to_char(now() AT TIME ZONE 'America/New_York' + interval '4 hours', 'HH24:MI') \
         WHERE id = 'rst_supdogs'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let server = serve(
        AppState::new(
            pool.clone(),
            common::config(&[
                ("PUBLIC_WEB_URL", WEB_URL),
                ("OSRM_BASE_URL", "http://127.0.0.1:1"),
            ]),
        )
        .unwrap(),
    )
    .await;

    let error = refused(&server, dog_request(&pool, 60).await, &["restaurant_id"]).await;
    let message = error["error"]["details"][0]["message"].as_str().unwrap();
    assert!(
        message.starts_with("is not open before needed_by"),
        "{message}"
    );

    // Needed after it opens: accepted, and searching (no Sup Dogs run is open).
    let request = create(&server, "usr_judge", dog_request(&pool, 200).await).await;
    assert_eq!(request["status"], "searching");
    assert_eq!(request["run_id"], Value::Null);
    assert_eq!(request["dropoff_radius_m"], Value::Null);
}

#[sqlx::test(migrations = "../migrations")]
async fn at_most_five_searching_requests(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    for _ in 0..5 {
        let request = create(&server, "usr_judge", dog_request(&pool, 90).await).await;
        assert_eq!(request["status"], "searching");
    }
    let (status, error) = post(
        &server,
        "/v1/requests",
        "usr_judge",
        Some(dog_request(&pool, 90).await),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{error}");
    common::assert_envelope(&error, "rate_limited");
    assert_contract("Error", &error);
    assert_eq!(error["error"]["details"]["max_searching"], 5);
    assert_eq!(request_rows(&pool).await, 5);

    // Another customer is not affected.
    let request = create(&server, "usr_dana", dog_request(&pool, 90).await).await;
    assert_eq!(request["status"], "searching");
}

#[sqlx::test(migrations = "../migrations")]
async fn the_judges_request_matches_run_demo_at_once(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let mut judge_ws = server.connect_with_token("usr_judge").await;
    let mut maya_ws = server.connect_with_token("usr_maya").await;
    let money = money_rows(&pool).await;

    // run_demo closes in 25 minutes; Maya heads back to Hinton James (her profile drop-off), and
    // Fetzer is about a minute out of her way. The food reaches Fetzer about 37 minutes from now.
    let body = burrito_request(&pool, 60).await;
    let request = create(&server, "usr_judge", body.clone()).await;
    assert_eq!(request["status"], "matched");
    assert_eq!(request["run_id"], RUN);
    assert_eq!(request["dropoff_radius_m"], 100);
    let id = request["id"].as_str().unwrap();
    assert!(id.starts_with("req_"), "{id}");

    // Stored matched to run_demo, with no order, and the lines as sent.
    assert_eq!(
        stored(&pool, id).await,
        ("matched".to_owned(), Some(RUN.to_owned()), None)
    );
    let lines: Value = sqlx::query_scalar("SELECT lines FROM ferry.pickup_requests WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(lines, body["lines"]);

    // The customer: a run_open with the run's deep link.
    let judge = request_notices(&pool, "usr_judge").await;
    assert_eq!(judge.len(), 1, "{judge:#?}");
    assert_eq!(judge[0].kind, "run_open");
    assert_eq!(judge[0].title, "Maya is heading to Cosmic Cantina");
    assert!(
        judge[0]
            .body
            .starts_with("Your request fits their run. Order by "),
        "{}",
        judge[0].body
    );
    assert_eq!(
        judge[0].data,
        json!({
            "run_id": RUN,
            "circle_id": "cir_hinton4",
            "request_id": id,
            "url": format!("{WEB_URL}/r/{RUN}")
        })
    );

    // The runner: one system suggestion, with no name or id of the customer.
    let maya = notifications(&pool, "usr_maya").await;
    assert_eq!(maya.len(), 1, "{maya:#?}");
    assert_eq!(maya[0].kind, "system");
    assert_eq!(
        maya[0].title,
        "Someone near your route wants Cosmic Cantina"
    );
    assert!(
        maya[0]
            .body
            .starts_with("About a minute out of your way. They can order until "),
        "{}",
        maya[0].body
    );
    assert_eq!(
        maya[0].data,
        json!({"run_id": RUN, "url": format!("{WEB_URL}/r/{RUN}")})
    );
    for text in [&maya[0].title, &maya[0].body] {
        assert!(
            !text.contains("Jordan") && !text.contains("judge"),
            "{text}"
        );
    }

    // Each got exactly that notification on the stream, as the contract's event.
    let event = &expect_events(&mut judge_ws, "judge", &["notification.created"]).await[0];
    assert_eq!(event["data"]["notification"]["data"]["request_id"], id);
    let event = &expect_events(&mut maya_ws, "maya", &["notification.created"]).await[0];
    assert_eq!(event["data"]["notification"]["type"], "system");

    // No order and no ledger row: the judge orders on run_demo themselves.
    assert_eq!(money_rows(&pool).await, money);
    let (status, quote) = post(
        &server,
        &format!("/v1/runs/{RUN}/quote"),
        "usr_judge",
        Some(runs_support::burrito()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{quote}");
    assert_eq!(money_rows(&pool).await, money);
}

#[sqlx::test(migrations = "../migrations")]
async fn a_double_tap_suggests_to_the_runner_once(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let body = burrito_request(&pool, 60).await;

    // Twice in a row, then twice at once: each request matches run_demo and the judge is told each
    // time, but Maya hears about the judge's request once.
    let first = create(&server, "usr_judge", body.clone()).await;
    let second = create(&server, "usr_judge", body.clone()).await;
    let (third, fourth) = tokio::join!(
        create(&server, "usr_judge", body.clone()),
        create(&server, "usr_judge", body.clone()),
    );
    for request in [&first, &second, &third, &fourth] {
        assert_eq!(request["status"], "matched", "{request}");
        assert_eq!(request["run_id"], RUN, "{request}");
    }
    let judge = request_notices(&pool, "usr_judge").await;
    assert_eq!(judge.len(), 4, "{judge:#?}");
    assert!(judge.iter().all(|n| n.kind == "run_open"), "{judge:#?}");
    let maya = notifications(&pool, "usr_maya").await;
    assert_eq!(maya.len(), 1, "{maya:#?}");
    assert_eq!(
        maya[0].title,
        "Someone near your route wants Cosmic Cantina"
    );

    // Another customer's match on the same run is a suggestion of its own.
    let dana = create(&server, "usr_dana", body.clone()).await;
    assert_eq!(dana["status"], "matched", "{dana}");
    assert_eq!(dana["run_id"], RUN, "{dana}");
    assert_eq!(notifications(&pool, "usr_maya").await.len(), 2);
}
