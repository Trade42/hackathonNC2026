//! Tracking over HTTP: POST /v1/runs/{id}/location, GET /v1/runs/{id}/track and the
//! runner.location events, against the local Postgres and a local OSRM stub (never the live
//! server). Every response is checked against contract/openapi.yaml.

mod common;
mod realtime_support;

use std::time::{Duration, SystemTime};

use axum::Router;
use axum::body::Body;
use axum::extract::Path;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use ferry_core::app;
use ferry_core::notifications::rfc3339;
use ferry_core::state::AppState;
use ferry_core::tracking::{
    LatLng, Pace, Routing, eta_minutes, fallback_distance_m, haversine_m, latlng,
};

/// `[lng, lat]`, a GeoJSON position.
fn position(point: LatLng) -> [f64; 2] {
    [point.lng, point.lat]
}
use realtime_support::{assert_contract, assert_silent, next_json, serve};
use serde_json::{Value, json};
use sqlx::PgPool;

const COSMIC: LatLng = latlng(35.913211, -79.055094);
const HINTON: LatLng = latlng(35.902364, -79.043231);
const DAVIS: LatLng = latlng(35.910957, -79.047906);
/// Where Maya is when she pings: on Franklin St, about 60 m from Cosmic Cantina.
const MAYA_AT: LatLng = latlng(35.9135, -79.0556);

const RUN: &str = "run_track1";
const RUNNER: &str = "usr_maya";
const CUSTOMER: &str = "usr_judge";
const MEMBER: &str = "usr_dana";
const STRANGER: &str = "usr_leo";
const CUSTOMER2: &str = "usr_cara";

// ---------- The OSRM stub ----------

/// The stub's route distance: the straight line × 1.1, so tests can predict every number.
fn stub_distance(from: LatLng, to: LatLng) -> f64 {
    haversine_m(from, to) * 1.1
}

/// `"lng,lat;lng,lat"` from an OSRM route path.
fn parse_coords(coords: &str) -> (LatLng, LatLng) {
    let points: Vec<LatLng> = coords
        .split(';')
        .map(|p| {
            let (lng, lat) = p.split_once(',').unwrap();
            latlng(lat.parse().unwrap(), lng.parse().unwrap())
        })
        .collect();
    (points[0], points[1])
}

async fn osrm_route(Path((_profile, coords)): Path<(String, String)>) -> axum::Json<Value> {
    let (from, to) = parse_coords(&coords);
    let mid = [(from.lng + to.lng) / 2.0, (from.lat + to.lat) / 2.0];
    axum::Json(json!({
        "code": "Ok",
        "routes": [{
            "distance": stub_distance(from, to),
            "duration": 1.0,
            "geometry": {"type": "LineString", "coordinates": [position(from), mid, position(to)]}
        }],
        "waypoints": []
    }))
}

/// A local OSRM on this test's runtime; returns its base URL.
async fn osrm_stub() -> String {
    let app = Router::new().route("/{profile}/route/v1/driving/{coords}", get(osrm_route));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// The app state with OSRM at `osrm` (through OSRM_BASE_URL). Each test has its own stub, so the
/// client's one-request-per-second pacing for FOSSGIS is turned off.
fn state_with(pool: PgPool, osrm: &str) -> AppState {
    let mut state = AppState::new(pool, common::config(&[("OSRM_BASE_URL", osrm)])).unwrap();
    assert_eq!(state.routing.settings().base_url, osrm);
    assert_eq!(
        state.routing.settings().min_interval,
        Duration::from_secs(1)
    );
    let mut settings = state.routing.settings().clone();
    settings.min_interval = Duration::ZERO;
    state.routing = Routing::new(state.http.clone(), settings);
    state
}

async fn state(pool: PgPool) -> AppState {
    let osrm = osrm_stub().await;
    state_with(pool, &osrm)
}

// ---------- Fixtures ----------

async fn exec(pool: &PgPool, sql: &'static str) {
    sqlx::raw_sql(sql).execute(pool).await.unwrap();
}

/// Maya (brisk) runs to Cosmic Cantina for the circle (Maya, Jordan, Dana, Cara); Jordan has a
/// funded order to Hinton James. Leo is in no circle. The run is open.
async fn seed(pool: &PgPool) {
    exec(
        pool,
        r##"
        INSERT INTO ferry.users (id, phone_e164, display_name, avatar_seed, bubble_color) VALUES
            ('usr_maya',  '+19195550102', 'Maya',   'maya',  '#336699'),
            ('usr_judge', '+19195550101', 'Jordan', 'judge', '#336699'),
            ('usr_dana',  '+19195550103', 'Dana',   'dana',  '#336699'),
            ('usr_leo',   '+19195550104', 'Leo',    'leo',   '#336699'),
            ('usr_cara',  '+19195550105', 'Cara',   'cara',  '#336699');
        INSERT INTO ferry.profiles (user_id, walking_pace) VALUES ('usr_maya', 'brisk');
        INSERT INTO ferry.circles (id, name, emoji) VALUES ('cir_t', 'Hinton James 4th floor', '🌊');
        INSERT INTO ferry.circle_members (circle_id, user_id) VALUES
            ('cir_t', 'usr_maya'), ('cir_t', 'usr_judge'), ('cir_t', 'usr_dana'), ('cir_t', 'usr_cara');
        INSERT INTO ferry.areas (id, name, center_lat, center_lng)
            VALUES ('area_franklin', 'Franklin St', 35.9132, -79.0558);
        INSERT INTO ferry.restaurants (id, name, area_id, lat, lng, address, hours_today, price_level)
            VALUES ('rst_cosmic', 'Cosmic Cantina', 'area_franklin', 35.913211, -79.055094,
                    '118 E Franklin St', '11:00-04:00', 1);
        INSERT INTO ferry.places (id, label, lat, lng) VALUES
            ('place_hinton', 'Hinton James lobby', 35.902364, -79.043231),
            ('place_davis', 'Davis Library steps', 35.910957, -79.047906);
        INSERT INTO ferry.runs (id, restaurant_id, runner_id, circle_id, status, created_via, leaving_at, window_closes_at)
            VALUES ('run_track1', 'rst_cosmic', 'usr_maya', 'cir_t', 'open', 'app', now(), now() + interval '10 minutes');
        INSERT INTO ferry.orders (id, run_id, customer_id, status, subtotal_cents, tax_cents, ferry_fee_cents,
                                  tip_cents, total_cents, dropoff_place_id, dropoff_label, dropoff_lat, dropoff_lng,
                                  handoff_code, created_at)
            VALUES ('ord_judge', 'run_track1', 'usr_judge', 'funded', 1000, 75, 150, 100, 1325,
                    'place_hinton', 'Hinton James lobby', 35.902364, -79.043231, '4829', now() - interval '2 minutes');
        "##,
    )
    .await;
}

/// Cara orders too, to Davis Library, which is on the way.
async fn add_second_customer(pool: &PgPool) {
    exec(
        pool,
        "INSERT INTO ferry.orders (id, run_id, customer_id, status, subtotal_cents, tax_cents, ferry_fee_cents, \
                                   tip_cents, total_cents, dropoff_place_id, dropoff_label, dropoff_lat, dropoff_lng, \
                                   handoff_code, created_at) \
         VALUES ('ord_cara', 'run_track1', 'usr_cara', 'funded', 500, 38, 150, 0, 688, \
                 'place_davis', 'Davis Library steps', 35.910957, -79.047906, '1111', now() - interval '1 minute')",
    )
    .await;
}

/// The run is delivered: every order delivered a minute ago.
async fn deliver(pool: &PgPool) {
    exec(
        pool,
        "UPDATE ferry.orders SET status = 'delivered', delivered_at = now() - interval '1 minute' \
         WHERE run_id = 'run_track1'; \
         UPDATE ferry.runs SET status = 'delivered' WHERE id = 'run_track1';",
    )
    .await;
}

// ---------- Requests ----------

fn token(state: &AppState, user: &str) -> String {
    format!("Bearer {}", state.jwt.issue_token(user).unwrap())
}

async fn get_as(state: &AppState, uri: &str, user: &str) -> (StatusCode, Value) {
    let request = Request::get(uri)
        .header(header::AUTHORIZATION, token(state, user))
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = common::send(app::router(state.clone()), request).await;
    (status, body)
}

async fn post_as(state: &AppState, uri: &str, user: &str, body: &Value) -> (StatusCode, Value) {
    let request = Request::post(uri)
        .header(header::AUTHORIZATION, token(state, user))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, _, body) = common::send(app::router(state.clone()), request).await;
    (status, body)
}

/// Now, plus `ahead` seconds (negative for the past), as ISO 8601 UTC with milliseconds.
fn at(ahead: i64) -> String {
    let now = SystemTime::now();
    let time = if ahead >= 0 {
        now + Duration::from_secs(ahead as u64)
    } else {
        now - Duration::from_secs(ahead.unsigned_abs())
    };
    rfc3339(time)
}

fn ping_body(location: LatLng, at: &str) -> Value {
    json!({"location": {"lat": location.lat, "lng": location.lng}, "accuracy_m": 8.0, "at": at})
}

async fn ping(state: &AppState, location: LatLng) -> String {
    let measured = at(-1);
    let (status, body) = post_as(
        state,
        &format!("/v1/runs/{RUN}/location"),
        RUNNER,
        &ping_body(location, &measured),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(body, Value::Null, "204 has no body");
    measured
}

async fn track(state: &AppState, user: &str) -> Value {
    let (status, body) = get_as(state, &format!("/v1/runs/{RUN}/track"), user).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_contract("Track", &body);
    body
}

fn assert_error(status: StatusCode, body: &Value, expected: StatusCode, code: &str) {
    assert_eq!(status, expected, "{body}");
    common::assert_envelope(body, code);
    assert_contract("Error", body);
}

fn assert_hidden(track: &Value) {
    assert_eq!(
        track,
        &json!({"runner_location": null, "route": null, "distance_m": null, "eta_minutes": null, "stops": []})
    );
}

fn close(actual: &Value, expected: f64) {
    let actual = actual
        .as_f64()
        .unwrap_or_else(|| panic!("not a number: {actual}"));
    assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
}

// ---------- Tests ----------

#[sqlx::test(migrations = "../migrations")]
async fn runner_pings_and_reads_track(pool: PgPool) {
    seed(&pool).await;
    let state = state(pool).await;
    let measured = ping(&state, MAYA_AT).await;

    let body = track(&state, RUNNER).await;
    assert_eq!(body["runner_location"]["at"], measured);
    close(&body["runner_location"]["location"]["lat"], MAYA_AT.lat);
    close(&body["runner_location"]["location"]["lng"], MAYA_AT.lng);

    // Before pickup the next leg is Maya to the restaurant; Maya walks brisk (1.6 m/s).
    let to_restaurant = stub_distance(MAYA_AT, COSMIC);
    close(&body["distance_m"], to_restaurant.round());
    close(
        &body["eta_minutes"],
        eta_minutes(to_restaurant, Pace::Brisk),
    );
    let route = body["route"]["coordinates"].as_array().unwrap();
    assert_eq!(body["route"]["type"], "LineString");
    assert_eq!(route.len(), 3);
    assert_eq!(route[2], json!([-79.055094, 35.913211]));

    // The one stop: restaurant leg plus restaurant to Hinton.
    let stops = body["stops"].as_array().unwrap();
    assert_eq!(stops.len(), 1);
    assert_eq!(stops[0]["order_id"], "ord_judge");
    assert_eq!(
        stops[0]["dropoff"],
        json!({"id": "place_hinton", "label": "Hinton James lobby", "location": {"lat": 35.902364, "lng": -79.043231}})
    );
    let walk = to_restaurant + stub_distance(COSMIC, HINTON);
    close(&stops[0]["eta_minutes"], eta_minutes(walk, Pace::Brisk));
}

#[sqlx::test(migrations = "../migrations")]
async fn customer_with_an_active_order_sees_the_runner_and_only_their_own_stop(pool: PgPool) {
    seed(&pool).await;
    add_second_customer(&pool).await;
    let state = state(pool).await;
    let measured = ping(&state, MAYA_AT).await;

    // From the restaurant, Davis (Cara) is nearer than Hinton (Jordan).
    let to_restaurant = stub_distance(MAYA_AT, COSMIC);
    let to_davis = to_restaurant + stub_distance(COSMIC, DAVIS);
    let to_hinton = to_davis + stub_distance(DAVIS, HINTON);

    let runner = track(&state, RUNNER).await;
    let order_ids: Vec<&str> = runner["stops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["order_id"].as_str().unwrap())
        .collect();
    assert_eq!(order_ids, ["ord_cara", "ord_judge"]);

    let judge = track(&state, CUSTOMER).await;
    assert_eq!(judge["runner_location"]["at"], measured);
    let stops = judge["stops"].as_array().unwrap();
    assert_eq!(stops.len(), 1, "only Jordan's own stop: {judge}");
    assert_eq!(stops[0]["order_id"], "ord_judge");
    // The ETA counts the stop at Davis first; distance_m is the same walk.
    let eta = eta_minutes(to_hinton, Pace::Brisk);
    close(&stops[0]["eta_minutes"], eta);
    close(&judge["eta_minutes"], eta);
    close(&judge["distance_m"], to_hinton.round());
    // Jordan's route: Maya, the restaurant, then straight to Hinton, never through Davis.
    let route = judge["route"]["coordinates"].as_array().unwrap();
    assert_eq!(route.first().unwrap(), &json!(position(MAYA_AT)));
    assert_eq!(route.last().unwrap(), &json!(position(HINTON)));
    assert!(route.contains(&json!(position(COSMIC))));
    assert!(!route.contains(&json!(position(DAVIS))), "{route:?}");
    assert_eq!(route.len(), 5);

    let cara = track(&state, CUSTOMER2).await;
    let stops = cara["stops"].as_array().unwrap();
    assert_eq!(stops.len(), 1);
    assert_eq!(stops[0]["order_id"], "ord_cara");
    close(&cara["eta_minutes"], eta_minutes(to_davis, Pace::Brisk));
}

#[sqlx::test(migrations = "../migrations")]
async fn member_without_an_order_gets_nulls_and_a_stranger_is_forbidden(pool: PgPool) {
    seed(&pool).await;
    let state = state(pool).await;
    ping(&state, MAYA_AT).await;

    assert_hidden(&track(&state, MEMBER).await);

    let (status, body) = get_as(&state, &format!("/v1/runs/{RUN}/track"), STRANGER).await;
    assert_error(status, &body, StatusCode::FORBIDDEN, "forbidden");

    let (status, body) = get_as(&state, "/v1/runs/run_nope/track", RUNNER).await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "not_found");

    let request = Request::get(format!("/v1/runs/{RUN}/track"))
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = common::send(app::router(state.clone()), request).await;
    assert_error(status, &body, StatusCode::UNAUTHORIZED, "unauthorized");

    // A customer whose order was cancelled is just a member again.
    sqlx::query("UPDATE ferry.orders SET status = 'cancelled' WHERE id = 'ord_judge'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_hidden(&track(&state, CUSTOMER).await);
}

#[sqlx::test(migrations = "../migrations")]
async fn after_delivery_the_location_is_null_for_everyone(pool: PgPool) {
    seed(&pool).await;
    let state = state(pool).await;
    ping(&state, MAYA_AT).await;
    assert!(track(&state, CUSTOMER).await["runner_location"].is_object());

    deliver(&state.pool).await;
    for user in [RUNNER, CUSTOMER, MEMBER] {
        assert_hidden(&track(&state, user).await);
    }
    // And no more pings.
    let (status, body) = post_as(
        &state,
        &format!("/v1/runs/{RUN}/location"),
        RUNNER,
        &ping_body(MAYA_AT, &at(0)),
    )
    .await;
    assert_error(status, &body, StatusCode::CONFLICT, "conflict");

    // A cancelled run is over too.
    sqlx::query("UPDATE ferry.runs SET status = 'cancelled' WHERE id = $1")
        .bind(RUN)
        .execute(&state.pool)
        .await
        .unwrap();
    assert_hidden(&track(&state, RUNNER).await);
}

#[sqlx::test(migrations = "../migrations")]
async fn only_the_runner_may_ping(pool: PgPool) {
    seed(&pool).await;
    let state = state(pool).await;
    let uri = format!("/v1/runs/{RUN}/location");
    for user in [CUSTOMER, MEMBER, STRANGER] {
        let (status, body) = post_as(&state, &uri, user, &ping_body(MAYA_AT, &at(0))).await;
        assert_error(status, &body, StatusCode::FORBIDDEN, "forbidden");
    }
    let (status, body) = post_as(
        &state,
        "/v1/runs/run_nope/location",
        RUNNER,
        &ping_body(MAYA_AT, &at(0)),
    )
    .await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "not_found");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM ferry.location_pings")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrations = "../migrations")]
async fn absurd_pings_are_validation_failed(pool: PgPool) {
    seed(&pool).await;
    let state = state(pool).await;
    let uri = format!("/v1/runs/{RUN}/location");
    let now = at(0);
    let bad = [
        json!({"location": {"lat": 91.0, "lng": -79.0}, "accuracy_m": 5.0, "at": now}),
        json!({"location": {"lat": -90.5, "lng": -79.0}, "accuracy_m": 5.0, "at": now}),
        json!({"location": {"lat": 35.9, "lng": -181.0}, "accuracy_m": 5.0, "at": now}),
        json!({"location": {"lat": 35.9, "lng": 180.1}, "accuracy_m": 5.0, "at": now}),
        json!({"location": {"lat": 35.9, "lng": -79.0}, "accuracy_m": -1.0, "at": now}),
        // More than 10 km: not a measurement.
        json!({"location": {"lat": 35.9, "lng": -79.0}, "accuracy_m": 10000.5, "at": now}),
        json!({"location": {"lat": 35.9, "lng": -79.0}, "accuracy_m": 1e300, "at": now}),
        // More than 10 minutes old, and much more.
        json!({"location": {"lat": 35.9, "lng": -79.0}, "accuracy_m": 5.0, "at": at(-11 * 60)}),
        json!({"location": {"lat": 35.9, "lng": -79.0}, "accuracy_m": 5.0, "at": "1500-01-01T00:00:00Z"}),
        // Offsets Postgres refuses (22009), year 0 (22008) and a five-digit year.
        json!({"location": {"lat": 35.9, "lng": -79.0}, "accuracy_m": 5.0, "at": now.replace('Z', "+16:00")}),
        json!({"location": {"lat": 35.9, "lng": -79.0}, "accuracy_m": 5.0, "at": now.replace('Z', "+99:00")}),
        json!({"location": {"lat": 35.9, "lng": -79.0}, "accuracy_m": 5.0, "at": "0000-01-01T00:00:00Z"}),
        json!({"location": {"lat": 35.9, "lng": -79.0}, "accuracy_m": 5.0, "at": "99999-01-01T00:00:00Z"}),
        // A NUL byte.
        json!({"location": {"lat": 35.9, "lng": -79.0}, "accuracy_m": 5.0, "at": format!("{now}\u{0}")}),
        // More than 2 minutes in the future.
        json!({"location": {"lat": 35.9, "lng": -79.0}, "accuracy_m": 5.0, "at": at(300)}),
        json!({"location": {"lat": 35.9, "lng": -79.0}, "accuracy_m": 5.0, "at": "not a time"}),
        json!({"location": {"lat": 35.9, "lng": -79.0}, "accuracy_m": 5.0, "at": "now"}),
        // The right shape, but no such date: Postgres refuses it.
        json!({"location": {"lat": 35.9, "lng": -79.0}, "accuracy_m": 5.0, "at": "2026-13-45T25:00:00Z"}),
        json!({"location": {"lat": 35.9, "lng": -79.0}, "accuracy_m": 5.0}),
        json!({"location": {"lat": "35.9", "lng": -79.0}, "accuracy_m": 5.0, "at": now}),
    ];
    for body in bad {
        let (status, response) = post_as(&state, &uri, RUNNER, &body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} -> {response}");
        assert_error(
            status,
            &response,
            StatusCode::BAD_REQUEST,
            "validation_failed",
        );
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM ferry.location_pings")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);

    // A phone clock a minute ahead is fine, and an offset is stored as UTC.
    let ahead = at(60);
    let (status, body) = post_as(&state, &uri, RUNNER, &ping_body(MAYA_AT, &ahead)).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let earlier = SystemTime::now() - Duration::from_secs(120);
    let utc = rfc3339(earlier);
    let new_york = rfc3339(earlier - Duration::from_secs(4 * 3600)).replace('Z', "-04:00");
    let (status, body) = post_as(&state, &uri, RUNNER, &ping_body(MAYA_AT, &new_york)).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let stored: Vec<String> = sqlx::query_scalar(
        r#"SELECT to_char(ts AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"')
           FROM ferry.location_pings ORDER BY ts"#,
    )
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert_eq!(stored, [utc, ahead]);
}

/// The bounds are inclusive: 10,000 m of accuracy and an `at` 9 minutes old are accepted. And
/// the track and the run show the last ping received, so one dated ahead of the server's clock
/// does not hide the pings after it.
#[sqlx::test(migrations = "../migrations")]
async fn ping_bounds_and_a_future_ping_does_not_hide_later_ones(pool: PgPool) {
    seed(&pool).await;
    let state = state(pool).await;
    let uri = format!("/v1/runs/{RUN}/location");
    let edge =
        json!({"location": {"lat": 35.9, "lng": -79.0}, "accuracy_m": 10000.0, "at": at(-9 * 60)});
    let (status, body) = post_as(&state, &uri, RUNNER, &edge).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body) = post_as(&state, &uri, RUNNER, &ping_body(COSMIC, &at(110))).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let later = ping(&state, MAYA_AT).await;
    let body = track(&state, RUNNER).await;
    assert_eq!(body["runner_location"]["at"], later);
    close(&body["runner_location"]["location"]["lat"], MAYA_AT.lat);
    close(&body["runner_location"]["location"]["lng"], MAYA_AT.lng);
    // The run's own `runner_location` (runs/view.rs) is the same last ping received.
    let (status, run) = get_as(&state, &format!("/v1/runs/{RUN}"), RUNNER).await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_contract("Run", &run);
    assert_eq!(run["runner_location"]["at"], later, "{run}");
    close(&run["runner_location"]["location"]["lat"], MAYA_AT.lat);
}

#[sqlx::test(migrations = "../migrations")]
async fn pings_are_accepted_while_the_run_is_active(pool: PgPool) {
    seed(&pool).await;
    let state = state(pool).await;
    for status in ["open", "closed", "purchased", "delivering"] {
        sqlx::query("UPDATE ferry.runs SET status = $1 WHERE id = $2")
            .bind(status)
            .bind(RUN)
            .execute(&state.pool)
            .await
            .unwrap();
        ping(&state, MAYA_AT).await;
    }
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ferry.location_pings WHERE run_id = $1 AND user_id = $2",
    )
    .bind(RUN)
    .bind(RUNNER)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(count, 4);
}

#[sqlx::test(migrations = "../migrations")]
async fn delivering_heads_straight_to_the_first_stop(pool: PgPool) {
    seed(&pool).await;
    exec(
        &pool,
        "UPDATE ferry.runs SET status = 'delivering' WHERE id = 'run_track1'; \
         UPDATE ferry.orders SET status = 'picked_up' WHERE run_id = 'run_track1';",
    )
    .await;
    let state = state(pool).await;
    ping(&state, DAVIS).await;
    let body = track(&state, RUNNER).await;
    let walk = stub_distance(DAVIS, HINTON);
    close(&body["distance_m"], walk.round());
    close(&body["eta_minutes"], eta_minutes(walk, Pace::Brisk));
    close(
        &body["stops"][0]["eta_minutes"],
        eta_minutes(walk, Pace::Brisk),
    );
    let customer = track(&state, CUSTOMER).await;
    assert_eq!(
        customer["route"]["coordinates"].as_array().unwrap().len(),
        3
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn before_the_first_ping_stops_count_from_the_restaurant(pool: PgPool) {
    seed(&pool).await;
    let state = state(pool).await;
    let body = track(&state, RUNNER).await;
    assert_eq!(body["runner_location"], Value::Null);
    assert_eq!(body["route"], Value::Null);
    assert_eq!(body["distance_m"], Value::Null);
    assert_eq!(body["eta_minutes"], Value::Null);
    close(
        &body["stops"][0]["eta_minutes"],
        eta_minutes(stub_distance(COSMIC, HINTON), Pace::Brisk),
    );
    // The customer sees their stop too, still with no location.
    let customer = track(&state, CUSTOMER).await;
    assert_eq!(customer["runner_location"], Value::Null);
    assert_eq!(customer["stops"].as_array().unwrap().len(), 1);
}

#[sqlx::test(migrations = "../migrations")]
async fn osrm_down_falls_back_to_the_straight_line(pool: PgPool) {
    seed(&pool).await;
    // Port 1 refuses connections.
    let state = state_with(pool, "http://127.0.0.1:1");
    ping(&state, MAYA_AT).await;
    let body = track(&state, RUNNER).await;
    assert_eq!(body["route"], Value::Null, "no geometry on the fallback");
    let to_restaurant = fallback_distance_m(MAYA_AT, COSMIC);
    close(&body["distance_m"], to_restaurant.round());
    close(
        &body["eta_minutes"],
        eta_minutes(to_restaurant, Pace::Brisk),
    );
    let walk = to_restaurant + fallback_distance_m(COSMIC, HINTON);
    close(
        &body["stops"][0]["eta_minutes"],
        eta_minutes(walk, Pace::Brisk),
    );
    let customer = track(&state, CUSTOMER).await;
    assert_eq!(customer["route"], Value::Null);
    assert!(customer["runner_location"].is_object());
}

#[sqlx::test(migrations = "../migrations")]
async fn runner_location_events_reach_only_the_runner_and_active_customers(pool: PgPool) {
    seed(&pool).await;
    let server = serve(state(pool).await).await;
    let mut maya = server.connect_with_token(RUNNER).await;
    let mut judge = server.connect_with_token(CUSTOMER).await;
    let mut dana = server.connect_with_token(MEMBER).await;
    realtime_support::eventually("three connections", || {
        [RUNNER, CUSTOMER, MEMBER]
            .iter()
            .all(|u| server.hub().connection_count(u) == 1)
    })
    .await;

    let measured = ping(&server.state, MAYA_AT).await;
    let to_restaurant = stub_distance(MAYA_AT, COSMIC);
    let walk = to_restaurant + stub_distance(COSMIC, HINTON);
    for (ws, eta) in [
        (&mut maya, eta_minutes(to_restaurant, Pace::Brisk)),
        (&mut judge, eta_minutes(walk, Pace::Brisk)),
    ] {
        let event = next_json(ws).await;
        assert_contract("RunnerLocationEvent", &event);
        assert_contract("ServerEvent", &event);
        assert_eq!(event["type"], "runner.location");
        assert_eq!(event["data"]["run_id"], RUN);
        assert_eq!(event["data"]["at"], measured);
        close(&event["data"]["location"]["lat"], MAYA_AT.lat);
        close(&event["data"]["eta_minutes"], eta);
    }
    assert_silent(&mut dana, Duration::from_millis(500)).await;

    // After delivery: the ping is refused and nobody hears anything.
    deliver(&server.state.pool).await;
    let (status, body) = post_as(
        &server.state,
        &format!("/v1/runs/{RUN}/location"),
        RUNNER,
        &ping_body(MAYA_AT, &at(0)),
    )
    .await;
    assert_error(status, &body, StatusCode::CONFLICT, "conflict");
    for ws in [&mut maya, &mut judge, &mut dana] {
        assert_silent(ws, Duration::from_millis(300)).await;
    }
}
