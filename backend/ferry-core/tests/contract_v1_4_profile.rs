//! Contract v1.4 on the profile: `Profile.max_dropoff_distance_m`, read by `GET /v1/me` (always
//! sent, null when unset), written by `PUT /v1/me/profile` (replaced like every other answer), and
//! checked to 50 to 5000 meters. Every response is checked against contract/openapi.yaml.
mod auth_support;
mod common;

use auth_support::{
    app_with, assert_contract, assert_error, call, get_with_token, put_with_token, seed_catalog,
    seed_user,
};
use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;

async fn signed_in(pool: &PgPool) -> (Router, String) {
    seed_catalog(pool).await;
    seed_user(pool, "usr_maya", "+19195550102", "Maya").await;
    let (app, state) = app_with(pool.clone(), &[]);
    let token = state.jwt.issue_token("usr_maya").unwrap();
    (app, token)
}

/// A whole profile with `max_dropoff_distance_m` set to `distance`, or left out for `None`.
fn request(distance: Option<Value>) -> Value {
    let mut body = json!({
        "profile": {
            "dropoff_default": null,
            "diet": [], "avoid": [], "favorite_cuisines": [], "favorite_restaurant_ids": [],
            "usual_times": [], "walking_pace": "normal", "runner_opt_in": true,
            "default_tip_cents": 200, "notify_push": true, "notify_messages": true,
            "share_location_when_running": false
        }
    });
    if let Some(distance) = distance {
        body["profile"]["max_dropoff_distance_m"] = distance;
    }
    body
}

async fn stored(pool: &PgPool) -> Option<i32> {
    sqlx::query_scalar(
        "SELECT max_dropoff_distance_m FROM ferry.profiles WHERE user_id = 'usr_maya'",
    )
    .fetch_optional(pool)
    .await
    .unwrap()
    .flatten()
}

async fn me(app: &Router, token: &str) -> Value {
    let (status, me) = call(app, get_with_token("/v1/me", token)).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_contract("User", &me);
    me
}

#[sqlx::test(migrations = "../migrations")]
async fn the_walk_distance_reads_as_null_until_set(pool: PgPool) {
    let (app, token) = signed_in(&pool).await;
    let me = me(&app, &token).await;
    assert_eq!(
        me["profile"].get("max_dropoff_distance_m"),
        Some(&Value::Null),
        "{me:#}"
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn the_walk_distance_is_written_read_and_replaced(pool: PgPool) {
    let (app, token) = signed_in(&pool).await;

    let (status, user) = call(
        &app,
        put_with_token("/v1/me/profile", &token, &request(Some(json!(500)))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{user}");
    assert_contract("User", &user);
    assert_eq!(user["profile"]["max_dropoff_distance_m"], 500);
    assert_eq!(
        me(&app, &token).await["profile"]["max_dropoff_distance_m"],
        500
    );
    assert_eq!(stored(&pool).await, Some(500));

    // The edges.
    for meters in [50, 5000] {
        let (status, user) = call(
            &app,
            put_with_token("/v1/me/profile", &token, &request(Some(json!(meters)))),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{user}");
        assert_eq!(user["profile"]["max_dropoff_distance_m"], meters);
    }

    // Replaced like every other answer: null clears it, and so does leaving it out.
    for (what, distance) in [("null", Some(Value::Null)), ("left out", None)] {
        let (status, _) = call(
            &app,
            put_with_token("/v1/me/profile", &token, &request(Some(json!(800)))),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, user) = call(
            &app,
            put_with_token("/v1/me/profile", &token, &request(distance)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{what}: {user}");
        assert_contract("User", &user);
        assert_eq!(
            user["profile"].get("max_dropoff_distance_m"),
            Some(&Value::Null),
            "{what}"
        );
        assert_eq!(stored(&pool).await, None, "{what}");
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn a_bad_walk_distance_is_validation_failed_and_changes_nothing(pool: PgPool) {
    let (app, token) = signed_in(&pool).await;
    let (status, _) = call(
        &app,
        put_with_token("/v1/me/profile", &token, &request(Some(json!(300)))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    for meters in [
        json!(49),
        json!(5001),
        json!(0),
        json!(-300),
        json!(i64::MAX),
    ] {
        let (status, body) = call(
            &app,
            put_with_token("/v1/me/profile", &token, &request(Some(meters.clone()))),
        )
        .await;
        assert_error(status, &body, StatusCode::BAD_REQUEST, "validation_failed");
        assert_eq!(
            body["error"]["details"],
            json!([{"field": "profile.max_dropoff_distance_m", "message": "must be 50 to 5000 meters"}]),
            "{meters}"
        );
    }
    for meters in [json!(300.5), json!("300"), json!(true)] {
        let (status, body) = call(
            &app,
            put_with_token("/v1/me/profile", &token, &request(Some(meters.clone()))),
        )
        .await;
        assert_error(status, &body, StatusCode::BAD_REQUEST, "validation_failed");
    }
    assert_eq!(stored(&pool).await, Some(300));
    assert_eq!(
        me(&app, &token).await["profile"]["max_dropoff_distance_m"],
        300
    );
}
