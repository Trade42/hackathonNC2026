//! Detour-aware matching over HTTP on the seeded demo, with OSRM refused so every distance is the
//! straight line × 1.3: what fits (on the way home, on the way past a drop-off already on the
//! run, inside the run's zone or the request's own radius of the runner's path, the least extra
//! time among two), and every kind of no fit (too far, outside the zone, another restaurant, a
//! closed window, a closed run, a circle the customer is not in, the customer's own run, a runner
//! with nowhere to head, food too late). A request that does not fit stays searching, with no
//! notification; none adds an order or a ledger row.
mod common;
mod realtime_support;
mod requests_support;
mod runs_support;

use axum::http::StatusCode;
use realtime_support::Server;
use requests_support::{
    HINTON, burrito_request, create, dog_request, money_rows, request_notices, set_home, stored,
};
use runs_support::{RUN, chips, demo_server, minutes_from_now, post};
use serde_json::{Value, json};
use sqlx::PgPool;

/// The judge's request (Cosmic Cantina to Fetzer, needed in an hour), with `edit` applied.
async fn judge_request(server: &Server, edit: impl FnOnce(&mut Value)) -> Value {
    let mut body = burrito_request(&server.state.pool, 60).await;
    edit(&mut body);
    create(server, "usr_judge", body).await
}

/// Asserts the request stayed searching, untouched, with no notification about it.
async fn assert_searching(server: &Server, request: &Value) {
    assert_eq!(request["status"], "searching", "{request}");
    assert_eq!(request["run_id"], Value::Null, "{request}");
    let id = request["id"].as_str().unwrap();
    assert_eq!(
        stored(&server.state.pool, id).await,
        ("searching".to_owned(), None, None)
    );
    let notices = request_notices(&server.state.pool, "usr_judge").await;
    assert!(
        notices.iter().all(|n| n.data["request_id"] != id),
        "{notices:#?}"
    );
}

async fn exec(pool: &PgPool, sql: &'static str) {
    sqlx::query(sql).execute(pool).await.unwrap();
}

/// Gives run_demo a return place at Hinton James and a drop-off zone of `radius_m`.
async fn zone(pool: &PgPool, radius_m: i32) {
    sqlx::query(
        "UPDATE ferry.runs SET return_place_id = 'place_hinton', return_label = 'Hinton James', \
                return_lat = $1, return_lng = $2, dropoff_radius_m = $3 WHERE id = 'run_demo'",
    )
    .bind(HINTON.0)
    .bind(HINTON.1)
    .bind(radius_m)
    .execute(pool)
    .await
    .unwrap();
}

/// `POST /v1/runs` to Cosmic Cantina as `runner` in cir_hinton4, leaving in 5 minutes with a
/// 30-minute window, plus `plan`: the run's id.
async fn open_cosmic_run(server: &Server, runner: &str, plan: Value) -> String {
    let mut body = json!({
        "restaurant_id": "rst_cosmic",
        "circle_id": "cir_hinton4",
        "leaving_at": minutes_from_now(&server.state.pool, 5).await,
        "window_minutes": 30
    });
    for (key, value) in plan.as_object().unwrap() {
        body[key] = value.clone();
    }
    let (status, run) = post(server, "/v1/runs", runner, Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{run}");
    run["id"].as_str().unwrap().to_owned()
}

#[sqlx::test(migrations = "../migrations")]
async fn too_far_from_the_runners_way(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let money = money_rows(&pool).await;
    // A pin about 3.5 km north-east of Franklin St: far more than 4 minutes out of the way.
    let request = judge_request(&server, |b| {
        b["dropoff"] = json!({"label": "Far away", "location": {"lat": 35.94, "lng": -79.02}});
    })
    .await;
    assert_searching(&server, &request).await;
    assert!(request_notices(&pool, "usr_maya").await.is_empty());
    assert_eq!(money_rows(&pool).await, money);
}

#[sqlx::test(migrations = "../migrations")]
async fn the_runs_drop_off_zone(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    // Maya heads back to Hinton James and walks 400 m from there. Fetzer is about 825 m from
    // Hinton James and about 250 m off the straight line from Cosmic Cantina.
    zone(&pool, 400).await;
    let outside = judge_request(&server, |_| {}).await;
    assert_searching(&server, &outside).await;
    let no_radius = judge_request(&server, |b| b["dropoff_radius_m"] = Value::Null).await;
    assert_searching(&server, &no_radius).await;

    // The judge will walk 300 m: within that of Maya's path, so it fits.
    let inside = judge_request(&server, |b| b["dropoff_radius_m"] = json!(300)).await;
    assert_eq!(inside["status"], "matched");
    assert_eq!(inside["run_id"], RUN);
    assert_eq!(inside["dropoff_radius_m"], 300);
}

#[sqlx::test(migrations = "../migrations")]
async fn a_zone_wide_enough_takes_the_drop_off(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    zone(&pool, 900).await;
    let request = judge_request(&server, |b| b["dropoff_radius_m"] = Value::Null).await;
    assert_eq!(request["status"], "matched");
    assert_eq!(request["run_id"], RUN);
}

#[sqlx::test(migrations = "../migrations")]
async fn another_restaurant(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    // Only run_demo (Cosmic Cantina) is open; the judge wants Sup Dogs.
    let request = create(&server, "usr_judge", dog_request(&pool, 90).await).await;
    assert_searching(&server, &request).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn a_closed_window_or_a_closed_run(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    exec(
        &pool,
        "UPDATE ferry.runs SET leaving_at = now() - interval '20 minutes', \
                window_closes_at = now() - interval '1 minute' WHERE id = 'run_demo'",
    )
    .await;
    assert_searching(&server, &judge_request(&server, |_| {}).await).await;

    // Window ahead, but the runner closed it.
    exec(
        &pool,
        "UPDATE ferry.runs SET leaving_at = now() + interval '5 minutes', \
                window_closes_at = now() + interval '15 minutes', status = 'closed' \
         WHERE id = 'run_demo'",
    )
    .await;
    assert_searching(&server, &judge_request(&server, |_| {}).await).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn only_runs_in_the_customers_circles(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    // run_demo moves to cir_climb (Maya and Leo): the judge is not in it.
    exec(
        &pool,
        "UPDATE ferry.runs SET circle_id = 'cir_climb' WHERE id = 'run_demo'",
    )
    .await;
    assert_searching(&server, &judge_request(&server, |_| {}).await).await;

    // Someone in no circle at all.
    let mut body = burrito_request(&pool, 60).await;
    body["dropoff_radius_m"] = json!(150);
    let outsider = create(&server, runs_support::OUTSIDER, body.clone()).await;
    assert_eq!(outsider["status"], "searching");

    // Leo is in cir_climb: the same request fits.
    let leo = create(&server, "usr_leo", body).await;
    assert_eq!(leo["status"], "matched");
    assert_eq!(leo["run_id"], RUN);
}

#[sqlx::test(migrations = "../migrations")]
async fn never_the_customers_own_run(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let body = burrito_request(&pool, 60).await;
    let request = create(&server, "usr_maya", body).await;
    assert_eq!(request["status"], "searching");
    assert!(request_notices(&pool, "usr_maya").await.is_empty());
}

#[sqlx::test(migrations = "../migrations")]
async fn a_runner_with_nowhere_to_head_is_never_scored(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    // No return place, no drop-offs on run_demo, and now no profile drop-off: nothing to compare
    // the detour with. Extra time is never taken as 0.
    set_home(&pool, "usr_maya", None).await;
    assert_searching(&server, &judge_request(&server, |_| {}).await).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn a_drop_off_already_on_the_run_is_its_plan(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    // Maya has no profile drop-off, but Dana's order goes to Hinton James: the walk from Cosmic
    // Cantina to Hinton passes Fetzer.
    set_home(&pool, "usr_maya", None).await;
    let (status, order) = post(
        &server,
        &format!("/v1/runs/{RUN}/orders"),
        "usr_dana",
        Some(chips()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{order}");
    let money = money_rows(&pool).await;
    let request = judge_request(&server, |_| {}).await;
    assert_eq!(request["status"], "matched");
    assert_eq!(request["run_id"], RUN);
    assert_eq!(money_rows(&pool).await, money);
}

#[sqlx::test(migrations = "../migrations")]
async fn the_food_must_arrive_by_needed_by(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    // run_demo closes in 25 minutes, then about 11.7 minutes' walk to Fetzer: about 37 minutes.
    let in_30 = minutes_from_now(&pool, 30).await;
    let too_soon = judge_request(&server, |b| b["needed_by"] = json!(in_30)).await;
    assert_searching(&server, &too_soon).await;
    let in_40 = minutes_from_now(&pool, 40).await;
    let in_time = judge_request(&server, |b| b["needed_by"] = json!(in_40)).await;
    assert_eq!(in_time["status"], "matched");
    assert_eq!(in_time["run_id"], RUN);
}

#[sqlx::test(migrations = "../migrations")]
async fn the_least_extra_time_wins(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    // Dana opens a second Cosmic Cantina run heading back to Fetzer itself: no detour at all,
    // against run_demo's minute.
    let dana_run = open_cosmic_run(
        &server,
        "usr_dana",
        json!({"return_place": {
            "id": "place_fetzer",
            "label": "Fetzer Gym (HackNC)",
            "location": {"lat": 35.909152, "lng": -79.046905}
        }}),
    )
    .await;
    let request = judge_request(&server, |_| {}).await;
    assert_eq!(request["status"], "matched");
    assert_eq!(request["run_id"], dana_run.as_str());
    // Dana got the suggestion, not Maya.
    assert_eq!(request_notices(&pool, "usr_dana").await.len(), 1);
    assert!(request_notices(&pool, "usr_maya").await.is_empty());
}
