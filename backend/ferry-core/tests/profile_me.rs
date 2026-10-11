//! GET /v1/me, PUT /v1/me/profile and GET /v1/me/stats.
mod auth_support;
mod common;

use auth_support::{
    app_with, assert_contract, assert_error, call, get_with_token, put_with_token, seed_catalog,
    seed_user,
};
use axum::Router;
use axum::http::StatusCode;
use ferry_core::state::AppState;
use serde_json::{Value, json};
use sqlx::PgPool;

async fn signed_in(pool: &PgPool) -> (Router, AppState, String) {
    seed_catalog(pool).await;
    seed_user(pool, "usr_maya", "+19195550102", "Maya").await;
    let (app, state) = app_with(pool.clone(), &[]);
    let token = state.jwt.issue_token("usr_maya").unwrap();
    (app, state, token)
}

fn good_request() -> Value {
    json!({
        "display_name": "  Maya R.  ",
        "profile": {
            "dropoff_default": {
                "id": "place_hinton",
                "label": "Hinton James lobby",
                "location": {"lat": 35.902364, "lng": -79.043231},
                "note": "Main lobby."
            },
            "diet": ["vegetarian"],
            "avoid": ["peanuts"],
            "favorite_cuisines": ["mexican", "vietnamese"],
            "favorite_restaurant_ids": ["rst_cosmic", "rst_supdogs"],
            "usual_times": [
                {"label": "Dinner run", "days": ["mon", "tue", "wed"], "start": "18:00", "end": "19:30"},
                {"label": "Late-night snack", "days": ["fri", "sat"], "start": "23:00", "end": "00:30"}
            ],
            "walking_pace": "brisk",
            "runner_opt_in": true,
            "default_tip_cents": 250,
            "notify_push": true,
            "notify_messages": false,
            "share_location_when_running": true,
            "max_dropoff_distance_m": 400
        }
    })
}

#[sqlx::test(migrations = "../migrations")]
async fn every_me_route_needs_a_token(pool: PgPool) {
    let (app, _, _) = signed_in(&pool).await;
    for request in [
        common::get("/v1/me"),
        common::get("/v1/me/stats"),
        auth_support::json_request("PUT", "/v1/me/profile", &good_request(), &[]),
    ] {
        let (status, body) = call(&app, request).await;
        assert_error(status, &body, StatusCode::UNAUTHORIZED, "unauthorized");
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn a_token_for_a_missing_user_is_401(pool: PgPool) {
    let (app, state, _) = signed_in(&pool).await;
    let token = state.jwt.issue_token("usr_gone").unwrap();
    let (status, body) = call(&app, get_with_token("/v1/me", &token)).await;
    assert_error(status, &body, StatusCode::UNAUTHORIZED, "unauthorized");
    let (status, body) = call(
        &app,
        put_with_token("/v1/me/profile", &token, &good_request()),
    )
    .await;
    assert_error(status, &body, StatusCode::UNAUTHORIZED, "unauthorized");
}

#[sqlx::test(migrations = "../migrations")]
async fn a_user_without_answers_reads_the_defaults(pool: PgPool) {
    let (app, _, token) = signed_in(&pool).await;
    let (status, me) = call(&app, get_with_token("/v1/me", &token)).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_contract("User", &me);
    assert_eq!(
        me,
        json!({
            "id": "usr_maya", "display_name": "Maya", "avatar_seed": "usr_maya",
            "bubble_color": "#2563EB", "phone_masked": "+1 ••• ••• 0102",
            "telegram_username": null, "onboarding_complete": false,
            "profile": {
                "dropoff_default": null, "diet": [], "avoid": [], "favorite_cuisines": [],
                "favorite_restaurant_ids": [], "usual_times": [], "walking_pace": "normal",
                "runner_opt_in": false, "default_tip_cents": 0, "notify_push": true,
                "notify_messages": true, "share_location_when_running": false,
                "max_dropoff_distance_m": null
            }
        })
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn the_profile_round_trips(pool: PgPool) {
    let (app, _, token) = signed_in(&pool).await;
    let request = good_request();
    let (status, user) = call(&app, put_with_token("/v1/me/profile", &token, &request)).await;
    assert_eq!(status, StatusCode::OK, "{user}");
    assert_contract("User", &user);
    assert_eq!(user["display_name"], "Maya R.");
    assert_eq!(user["onboarding_complete"], true);
    assert_eq!(user["profile"], request["profile"]);

    let (status, me) = call(&app, get_with_token("/v1/me", &token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me, user);

    // Without display_name the name stays; a map pin with no id or note is a valid drop-off.
    let mut second = good_request();
    second.as_object_mut().unwrap().remove("display_name");
    second["profile"]["dropoff_default"] =
        json!({"label": "The Pit", "location": {"lat": 35.910, "lng": -79.048}});
    second["profile"]["usual_times"] = json!([]);
    second["profile"]["favorite_restaurant_ids"] = json!([]);
    let (status, user) = call(&app, put_with_token("/v1/me/profile", &token, &second)).await;
    assert_eq!(status, StatusCode::OK, "{user}");
    assert_contract("User", &user);
    assert_eq!(user["display_name"], "Maya R.");
    assert_eq!(user["profile"], second["profile"]);

    // And null clears the drop-off.
    second["profile"]["dropoff_default"] = Value::Null;
    let (status, user) = call(&app, put_with_token("/v1/me/profile", &token, &second)).await;
    assert_eq!(status, StatusCode::OK, "{user}");
    assert_eq!(user["profile"]["dropoff_default"], Value::Null);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM ferry.profiles")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 1);
}

/// `good_request()` changed by `change`.
fn with(change: impl Fn(&mut Value)) -> Value {
    let mut request = good_request();
    change(&mut request);
    request
}

#[sqlx::test(migrations = "../migrations")]
async fn each_invalid_field_is_validation_failed(pool: PgPool) {
    let (app, _, token) = signed_in(&pool).await;
    let remove = |key: &'static str| {
        move |r: &mut Value| {
            r["profile"].as_object_mut().unwrap().remove(key);
        }
    };
    let cases: Vec<(&str, Value)> = vec![
        (
            "unknown pace",
            with(|r| r["profile"]["walking_pace"] = json!("sprint")),
        ),
        (
            "unknown day",
            with(|r| r["profile"]["usual_times"][0]["days"] = json!(["monday"])),
        ),
        (
            "hour 25",
            with(|r| r["profile"]["usual_times"][0]["start"] = json!("25:00")),
        ),
        (
            "one-digit hour",
            with(|r| r["profile"]["usual_times"][0]["start"] = json!("9:00")),
        ),
        (
            "24:00",
            with(|r| r["profile"]["usual_times"][1]["end"] = json!("24:00")),
        ),
        (
            "negative tip",
            with(|r| r["profile"]["default_tip_cents"] = json!(-1)),
        ),
        (
            "fractional tip",
            with(|r| r["profile"]["default_tip_cents"] = json!(2.5)),
        ),
        (
            "tip as a string",
            with(|r| r["profile"]["default_tip_cents"] = json!("200")),
        ),
        (
            "huge tip",
            with(|r| r["profile"]["default_tip_cents"] = json!(10_001)),
        ),
        (
            "unknown restaurant",
            with(|r| r["profile"]["favorite_restaurant_ids"] = json!(["rst_cosmic", "rst_nope"])),
        ),
        (
            "unknown place",
            with(|r| r["profile"]["dropoff_default"]["id"] = json!("place_nope")),
        ),
        (
            "latitude 91",
            with(|r| r["profile"]["dropoff_default"]["location"]["lat"] = json!(91)),
        ),
        (
            "longitude 200",
            with(|r| r["profile"]["dropoff_default"]["location"]["lng"] = json!(200)),
        ),
        (
            "empty label",
            with(|r| r["profile"]["dropoff_default"]["label"] = json!(" ")),
        ),
        (
            "missing location",
            with(|r| {
                r["profile"]["dropoff_default"]
                    .as_object_mut()
                    .unwrap()
                    .remove("location");
            }),
        ),
        ("missing notify_push", with(remove("notify_push"))),
        ("missing dropoff_default", with(remove("dropoff_default"))),
        ("missing usual_times", with(remove("usual_times"))),
        ("no profile", json!({"display_name": "Maya"})),
        (
            "empty display_name",
            with(|r| r["display_name"] = json!("")),
        ),
        (
            "long display_name",
            with(|r| r["display_name"] = json!("x".repeat(51))),
        ),
        (
            "empty diet entry",
            with(|r| r["profile"]["diet"] = json!([""])),
        ),
        (
            "diet not a list",
            with(|r| r["profile"]["diet"] = json!("vegetarian")),
        ),
        (
            "empty usual time label",
            with(|r| r["profile"]["usual_times"][0]["label"] = json!("")),
        ),
        (
            "boolean as a string",
            with(|r| r["profile"]["runner_opt_in"] = json!("yes")),
        ),
        (
            "too many usual times",
            with(|r| {
                let one = r["profile"]["usual_times"][0].clone();
                r["profile"]["usual_times"] = Value::Array(vec![one; 21]);
            }),
        ),
    ];
    for (label, request) in cases {
        let (status, body) = call(&app, put_with_token("/v1/me/profile", &token, &request)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{label}: {body}");
        assert_error(status, &body, StatusCode::BAD_REQUEST, "validation_failed");
    }
    // Nothing was saved.
    let (_, me) = call(&app, get_with_token("/v1/me", &token)).await;
    assert_eq!(me["onboarding_complete"], false);
    assert_eq!(me["display_name"], "Maya");
}

#[sqlx::test(migrations = "../migrations")]
async fn validation_details_name_every_bad_field(pool: PgPool) {
    let (app, _, token) = signed_in(&pool).await;
    let request = with(|r| {
        r["profile"]["usual_times"][0]["start"] = json!("25:00");
        r["profile"]["default_tip_cents"] = json!(-5);
        r["profile"]["favorite_restaurant_ids"] = json!(["rst_nope"]);
    });
    let (status, body) = call(&app, put_with_token("/v1/me/profile", &token, &request)).await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "validation_failed");
    let fields: Vec<&str> = body["error"]["details"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["field"].as_str().unwrap())
        .collect();
    assert_eq!(
        fields,
        [
            "profile.usual_times[0].start",
            "profile.default_tip_cents",
            "profile.favorite_restaurant_ids[0]"
        ]
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn stats_start_at_zero(pool: PgPool) {
    let (app, _, token) = signed_in(&pool).await;
    let (status, stats) = call(&app, get_with_token("/v1/me/stats", &token)).await;
    assert_eq!(status, StatusCode::OK, "{stats}");
    assert_contract("MeStats", &stats);
    assert_eq!(
        stats,
        json!({"orders": 0, "runs": 0, "earned_cents": 0, "co2_saved_g": 0.0})
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn stats_count_orders_runs_payouts_and_co2(pool: PgPool) {
    let (app, state, token) = signed_in(&pool).await;
    seed_user(&pool, "usr_dana", "+19195550103", "Dana").await;
    sqlx::query(
        "INSERT INTO ferry.circles (id, name, emoji) VALUES ('cir_hinton4', 'Hinton James 4th floor', '🌊')",
    )
    .execute(&pool)
    .await
    .unwrap();
    // Maya ran run_a (delivered) and run_b (cancelled); Dana ran run_c.
    sqlx::query(
        "INSERT INTO ferry.runs (id, restaurant_id, runner_id, circle_id, status, created_via, leaving_at, window_closes_at)
         VALUES ('run_a', 'rst_cosmic', 'usr_maya', 'cir_hinton4', 'delivered', 'app', now(), now()),
                ('run_b', 'rst_cosmic', 'usr_maya', 'cir_hinton4', 'cancelled', 'app', now(), now()),
                ('run_c', 'rst_supdogs', 'usr_dana', 'cir_hinton4', 'delivered', 'message', now(), now())",
    )
    .execute(&pool)
    .await
    .unwrap();
    // Maya ordered on run_c three times (delivered, open, cancelled); Dana ordered on run_a.
    sqlx::query(
        "INSERT INTO ferry.orders (id, run_id, customer_id, status, subtotal_cents, tax_cents, ferry_fee_cents,
             tip_cents, total_cents, dropoff_label, dropoff_lat, dropoff_lng, handoff_code, co2_saved_g, delivered_at)
         VALUES ('ord_1', 'run_c', 'usr_maya', 'delivered', 1000, 75, 150, 200, 1425, 'Hinton', 35.9, -79.04, '1234', 480, now()),
                ('ord_2', 'run_a', 'usr_dana', 'settled', 1000, 75, 150, 200, 1425, 'Ehaus', 35.9, -79.04, '2345', 300, now()),
                ('ord_3', 'run_c', 'usr_maya', 'cancelled', 1000, 75, 150, 0, 1225, 'Hinton', 35.9, -79.04, '3456', NULL, NULL),
                ('ord_4', 'run_c', 'usr_maya', 'funded', 500, 38, 150, 0, 688, 'Hinton', 35.9, -79.04, '4567', NULL, NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    // Two payouts into Maya's checking; a deposit and Dana's payout do not count.
    sqlx::query(
        "INSERT INTO ferry.ledger_entries (id, kind, debit_account, credit_account, amount_cents, run_id, order_id, idempotency_key)
         VALUES ('txn_1', 'payout', 'user:usr_maya:checking', 'platform:ferry:escrow', 150, 'run_a', 'ord_2', 'k1'),
                ('txn_2', 'payout', 'user:usr_maya:checking', 'platform:ferry:escrow', 200, 'run_a', 'ord_2', 'k2'),
                ('txn_3', 'deposit', 'user:usr_maya:checking', 'external', 5000, NULL, NULL, 'k3'),
                ('txn_4', 'payout', 'user:usr_dana:checking', 'platform:ferry:escrow', 350, 'run_c', 'ord_1', 'k4')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let (status, stats) = call(&app, get_with_token("/v1/me/stats", &token)).await;
    assert_eq!(status, StatusCode::OK, "{stats}");
    assert_contract("MeStats", &stats);
    // Orders 1 and 4; run_a; 150 + 200; 480 as customer plus 300 as run_a's runner.
    assert_eq!(
        stats,
        json!({"orders": 2, "runs": 1, "earned_cents": 350, "co2_saved_g": 780.0})
    );

    let dana = state.jwt.issue_token("usr_dana").unwrap();
    let (_, stats) = call(&app, get_with_token("/v1/me/stats", &dana)).await;
    assert_eq!(
        stats,
        json!({"orders": 1, "runs": 1, "earned_cents": 350, "co2_saved_g": 780.0})
    );
}
