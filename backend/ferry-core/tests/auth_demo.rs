//! Demo sign-in: each persona in demo mode, refused outside it. And the text sign-in routes,
//! removed in contract v1.3, are gone.
mod auth_support;
mod common;

use auth_support::{
    app_with, assert_contract, assert_error, call, get_with_token, json_request, post,
    seed_personas,
};
use axum::http::StatusCode;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test(migrations = "../migrations")]
async fn each_persona_signs_in_in_demo_mode(pool: PgPool) {
    seed_personas(&pool).await;
    let (app, _) = app_with(pool, &[("DEMO_MODE", "true")]);
    for (persona, id, name) in [
        ("judge", "usr_judge", "Jordan"),
        ("maya", "usr_maya", "Maya"),
        ("dana", "usr_dana", "Dana"),
        ("leo", "usr_leo", "Leo"),
    ] {
        let (status, session) =
            call(&app, post("/v1/auth/demo", json!({"persona": persona}))).await;
        assert_eq!(status, StatusCode::OK, "{persona}: {session}");
        assert_contract("AuthSession", &session);
        assert_eq!(session["user"]["id"], id);
        assert_eq!(session["user"]["display_name"], name);

        let token = session["token"].as_str().unwrap();
        let (status, me) = call(&app, get_with_token("/v1/me", token)).await;
        assert_eq!(status, StatusCode::OK, "{me}");
        assert_contract("User", &me);
        assert_eq!(me, session["user"]);
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn the_judge_reads_their_seeded_profile(pool: PgPool) {
    seed_personas(&pool).await;
    let (app, _) = app_with(pool, &[("DEMO_MODE", "true")]);
    let (_, session) = call(&app, post("/v1/auth/demo", json!({"persona": "judge"}))).await;
    let user = &session["user"];
    assert_eq!(user["phone_masked"], "+1 ••• ••• 0101");
    assert_eq!(user["onboarding_complete"], true);
    assert_eq!(
        user["profile"]["dropoff_default"],
        json!({"id": "place_fetzer", "label": "Fetzer Gym (HackNC)",
               "location": {"lat": 35.909152, "lng": -79.046905}, "note": "HackNC venue."})
    );
    assert_eq!(
        user["profile"]["usual_times"],
        json!([{"label": "Hackathon dinner", "days": ["sat"], "start": "18:00", "end": "20:00"}])
    );
    assert_eq!(user["profile"]["default_tip_cents"], 200);
}

#[sqlx::test(migrations = "../migrations")]
async fn demo_sign_in_is_forbidden_outside_demo_mode(pool: PgPool) {
    seed_personas(&pool).await;
    for pairs in [&[][..], &[("DEMO_MODE", "false")][..]] {
        let (app, _) = app_with(pool.clone(), pairs);
        let (status, body) = call(&app, post("/v1/auth/demo", json!({"persona": "judge"}))).await;
        assert_error(status, &body, StatusCode::FORBIDDEN, "forbidden");
        // Checked before the body.
        let (status, body) = call(&app, post("/v1/auth/demo", json!({"persona": "mallory"}))).await;
        assert_error(status, &body, StatusCode::FORBIDDEN, "forbidden");
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn bad_personas_are_400(pool: PgPool) {
    seed_personas(&pool).await;
    let (app, _) = app_with(pool, &[("DEMO_MODE", "true")]);
    for body in [
        json!({"persona": "mallory"}),
        json!({"persona": "Judge"}),
        json!({}),
        json!({"persona": null}),
    ] {
        let (status, body) = call(&app, post("/v1/auth/demo", body)).await;
        assert_error(status, &body, StatusCode::BAD_REQUEST, "validation_failed");
    }
    let request = json_request("POST", "/v1/auth/demo", &json!(null), &[]);
    let (status, body) = call(&app, request).await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "validation_failed");
}

#[sqlx::test(migrations = "../migrations")]
async fn an_unseeded_persona_is_404(pool: PgPool) {
    let (app, _) = app_with(pool, &[("DEMO_MODE", "true")]);
    let (status, body) = call(&app, post("/v1/auth/demo", json!({"persona": "maya"}))).await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "not_found");
}

#[sqlx::test(migrations = "../migrations")]
async fn the_text_sign_in_routes_are_gone(pool: PgPool) {
    let (app, _) = app_with(pool, &[("DEMO_MODE", "true")]);
    for request in [
        post("/v1/auth/text/start", json!({"phone": "+19195550102"})),
        post("/v1/auth/text/chl_x/verify", json!({"code": "048213"})),
        common::get("/v1/auth/text/chl_x"),
    ] {
        let (status, body) = call(&app, request).await;
        assert_error(status, &body, StatusCode::NOT_FOUND, "not_found");
    }
}
