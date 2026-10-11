//! The demo's runner simulator: walks over a local OSRM stub (never the live server), pings in the
//! local Postgres, the track and runner.location events as a phone's pings would give them, and
//! the internal routes' gates. Each test walks its own run id: the walks are process-wide.

mod common;
mod realtime_support;

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::Path;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use ferry_core::app;
use ferry_core::demo::simulator::{
    WalkError, WalkSettings, is_walking, start_walk, start_walk_with, stop_walk,
};
use ferry_core::state::AppState;
use ferry_core::tracking::{LatLng, Pace, Routing, haversine_m, latlng};
use realtime_support::{assert_contract, assert_silent, eventually, next_json, serve};
use serde_json::{Value, json};
use sqlx::PgPool;

const COSMIC: LatLng = latlng(35.913211, -79.055094);
const FETZER: LatLng = latlng(35.909152, -79.046905);
const DAVIS: LatLng = latlng(35.910957, -79.047906);

const RUNNER: &str = "usr_maya";
const CUSTOMER: &str = "usr_judge";
const MEMBER: &str = "usr_dana";
const SECRET: &str = "sim-test-secret";

// ---------- The OSRM stub ----------

/// `[lng, lat]`, a GeoJSON position.
fn position(point: LatLng) -> [f64; 2] {
    [point.lng, point.lat]
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

/// Routes along the straight line, through its midpoint, at the straight line × 1.1.
async fn osrm_route(Path((_profile, coords)): Path<(String, String)>) -> axum::Json<Value> {
    let (from, to) = parse_coords(&coords);
    let mid = [(from.lng + to.lng) / 2.0, (from.lat + to.lat) / 2.0];
    axum::Json(json!({
        "code": "Ok",
        "routes": [{
            "distance": haversine_m(from, to) * 1.1,
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

/// The app state with OSRM at `osrm` and the extra `pairs`, without FOSSGIS's pacing.
fn state_with(pool: PgPool, osrm: &str, pairs: &[(&str, &str)]) -> AppState {
    let mut vars = vec![("OSRM_BASE_URL", osrm)];
    vars.extend_from_slice(pairs);
    let mut state = AppState::new(pool, common::config(&vars)).unwrap();
    let mut settings = state.routing.settings().clone();
    settings.min_interval = Duration::ZERO;
    state.routing = Routing::new(state.http.clone(), settings);
    state
}

/// DEMO_MODE with the test secret and a fresh OSRM stub.
async fn demo_state(pool: PgPool) -> AppState {
    let osrm = osrm_stub().await;
    state_with(
        pool,
        &osrm,
        &[("DEMO_MODE", "true"), ("SIDECAR_SHARED_SECRET", SECRET)],
    )
}

// ---------- Fixtures ----------

/// Maya (brisk) ran to Cosmic Cantina for the circle (Maya, Jordan, Dana) and has picked up:
/// `run` is delivering, and Jordan's order to Fetzer Gym is picked_up.
async fn seed(pool: &PgPool, run: &str) {
    sqlx::raw_sql(
        r##"
        INSERT INTO ferry.users (id, phone_e164, display_name, avatar_seed, bubble_color) VALUES
            ('usr_maya',  '+19195550102', 'Maya',   'maya',  '#336699'),
            ('usr_judge', '+19195550101', 'Jordan', 'judge', '#336699'),
            ('usr_dana',  '+19195550103', 'Dana',   'dana',  '#336699');
        INSERT INTO ferry.profiles (user_id, walking_pace) VALUES ('usr_maya', 'brisk');
        INSERT INTO ferry.circles (id, name, emoji) VALUES ('cir_t', 'Hinton James 4th floor', '🌊');
        INSERT INTO ferry.circle_members (circle_id, user_id) VALUES
            ('cir_t', 'usr_maya'), ('cir_t', 'usr_judge'), ('cir_t', 'usr_dana');
        INSERT INTO ferry.areas (id, name, center_lat, center_lng)
            VALUES ('area_franklin', 'Franklin St', 35.9132, -79.0558);
        INSERT INTO ferry.restaurants (id, name, area_id, lat, lng, address, hours_today, price_level)
            VALUES ('rst_cosmic', 'Cosmic Cantina', 'area_franklin', 35.913211, -79.055094,
                    '118 E Franklin St', '11:00-04:00', 1);
        INSERT INTO ferry.places (id, label, lat, lng) VALUES
            ('place_fetzer', 'Fetzer Gym (HackNC)', 35.909152, -79.046905),
            ('place_davis', 'Davis Library steps', 35.910957, -79.047906);
        "##,
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ferry.runs (id, restaurant_id, runner_id, circle_id, status, created_via, \
                                 leaving_at, window_closes_at) \
         VALUES ($1, 'rst_cosmic', 'usr_maya', 'cir_t', 'delivering', 'app', now(), now())",
    )
    .bind(run)
    .execute(pool)
    .await
    .unwrap();
    add_order(pool, run, "ord_judge", CUSTOMER, "place_fetzer", FETZER).await;
}

async fn add_order(pool: &PgPool, run: &str, order: &str, customer: &str, place: &str, at: LatLng) {
    sqlx::query(
        "INSERT INTO ferry.orders (id, run_id, customer_id, status, subtotal_cents, tax_cents, \
                                   ferry_fee_cents, tip_cents, total_cents, dropoff_place_id, \
                                   dropoff_label, dropoff_lat, dropoff_lng, handoff_code) \
         VALUES ($1, $2, $3, 'picked_up', 1000, 75, 150, 100, 1325, $4, $4, $5, $6, '4829')",
    )
    .bind(order)
    .bind(run)
    .bind(customer)
    .bind(place)
    .bind(at.lat)
    .bind(at.lng)
    .execute(pool)
    .await
    .unwrap();
}

async fn set_run_status(pool: &PgPool, run: &str, status: &str) {
    sqlx::query("UPDATE ferry.runs SET status = $1 WHERE id = $2")
        .bind(status)
        .bind(run)
        .execute(pool)
        .await
        .unwrap();
}

/// A stored ping: where, how accurate, and when (epoch seconds).
#[derive(Debug, Clone, Copy)]
struct Ping {
    at: LatLng,
    accuracy_m: f64,
    ts: f64,
}

async fn pings(pool: &PgPool, run: &str) -> Vec<Ping> {
    let rows: Vec<(f64, f64, f64, f64, String)> = sqlx::query_as(
        "SELECT lat, lng, accuracy_m, extract(epoch FROM ts)::float8, user_id \
         FROM ferry.location_pings WHERE run_id = $1 ORDER BY ts, id",
    )
    .bind(run)
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(lat, lng, accuracy_m, ts, user_id)| {
            assert_eq!(user_id, RUNNER, "every ping is the runner's");
            Ping {
                at: latlng(lat, lng),
                accuracy_m,
                ts,
            }
        })
        .collect()
}

/// Waits until `run` has at least `n` pings; returns them.
async fn wait_for_pings(pool: &PgPool, run: &str, n: usize) -> Vec<Ping> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let found = pings(pool, run).await;
        if found.len() >= n {
            return found;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{run}: {} pings, expected {n}",
            found.len()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn near(a: LatLng, b: LatLng) -> bool {
    haversine_m(a, b) < 0.01
}

fn location_of(value: &Value) -> LatLng {
    latlng(
        value["lat"].as_f64().unwrap(),
        value["lng"].as_f64().unwrap(),
    )
}

/// The multiplier over Maya's brisk pace that walks `length_m` in `steps` pings after the first
/// (rounded up), at a ping every `interval`.
fn speed_for(length_m: f64, steps: f64, interval: Duration) -> f64 {
    length_m / steps / (Pace::Brisk.meters_per_second() * interval.as_secs_f64())
}

// ---------- Requests ----------

async fn track(state: &AppState, run: &str, user: &str) -> Value {
    let request = Request::get(format!("/v1/runs/{run}/track"))
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", state.jwt.issue_token(user).unwrap()),
        )
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = common::send(app::router(state.clone()), request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_contract("Track", &body);
    body
}

/// `POST /internal/demo/runs/{run}/{action}` with `secret` as X-Ferry-Secret (none when `None`).
async fn internal(
    state: &AppState,
    run: &str,
    action: &str,
    secret: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::post(format!("/internal/demo/runs/{run}/{action}"));
    if let Some(secret) = secret {
        request = request.header("X-Ferry-Secret", secret);
    }
    let (status, _, body) = common::send(
        app::router(state.clone()),
        request.body(Body::empty()).unwrap(),
    )
    .await;
    (status, body)
}

fn assert_error(status: StatusCode, body: &Value, expected: StatusCode, code: &str) {
    assert_eq!(status, expected, "{body}");
    common::assert_envelope(body, code);
    assert_contract("Error", body);
}

/// Every runner.location event `ws` gets until it is quiet for `quiet`, oldest `at` first.
async fn drain_locations(ws: &mut realtime_support::Ws, quiet: Duration) -> Vec<Value> {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    let mut events = Vec::new();
    while let Ok(Some(message)) = tokio::time::timeout(quiet, ws.next()).await {
        if let Message::Text(text) = message.unwrap() {
            events.push(serde_json::from_str::<Value>(&text).unwrap());
        }
    }
    events
}

// ---------- Tests ----------

#[sqlx::test(migrations = "../migrations")]
async fn a_walk_pings_every_interval_moves_the_track_and_reaches_the_customer(pool: PgPool) {
    const RUN: &str = "run_sim_walk";
    seed(&pool, RUN).await;
    let server = serve(demo_state(pool).await).await;
    let state = server.state.clone();
    let mut judge = server.connect_with_token(CUSTOMER).await;
    let mut dana = server.connect_with_token(MEMBER).await;
    eventually("two connections", || {
        [CUSTOMER, MEMBER]
            .iter()
            .all(|u| server.hub().connection_count(u) == 1)
    })
    .await;

    // The stub's route is straight through its midpoint, so the path is the straight line.
    // 7.5 steps of the walk: 8 intervals, 9 pings.
    let interval = Duration::from_millis(200);
    let length = haversine_m(COSMIC, FETZER);
    let speed = speed_for(length, 7.5, interval);
    let started = start_walk_with(&state, RUN, WalkSettings { interval, speed })
        .await
        .unwrap();
    assert_eq!(started.run_id, RUN);
    assert!(!started.restarted);
    assert!((started.speed_mps - 1.6 * speed).abs() < 1e-9);
    assert_eq!(started.interval_s, 0.2);
    assert!(is_walking(RUN));

    // The judge sees Maya move: one event per ping, and the track read in between.
    let mut events = Vec::new();
    let mut tracks = Vec::new();
    while events.len() < 9 {
        let event = next_json(&mut judge).await;
        assert_contract("RunnerLocationEvent", &event);
        assert_contract("ServerEvent", &event);
        assert_eq!(event["type"], "runner.location");
        assert_eq!(event["data"]["run_id"], RUN);
        events.push(event);
        if events.len() % 3 == 1 {
            tracks.push(track(&state, RUN, CUSTOMER).await);
        }
    }
    assert!(
        drain_locations(&mut judge, Duration::from_millis(500))
            .await
            .is_empty(),
        "no tenth event"
    );
    // Each ping's event is sent by its own task, so order them by `at`.
    events.sort_by(|a, b| {
        a["data"]["at"]
            .as_str()
            .unwrap()
            .cmp(b["data"]["at"].as_str().unwrap())
    });
    assert!(near(location_of(&events[0]["data"]["location"]), COSMIC));
    assert!(near(location_of(&events[8]["data"]["location"]), FETZER));
    // ETAs count down to 0 at the drop-off.
    let etas: Vec<f64> = events
        .iter()
        .map(|e| e["data"]["eta_minutes"].as_f64().unwrap())
        .collect();
    assert!(etas.windows(2).all(|w| w[1] <= w[0]), "{etas:?}");
    assert!(etas[0] > 0.0);
    assert_eq!(etas[8], 0.0);
    // Dana has no order: nothing.
    assert_silent(&mut dana, Duration::from_millis(200)).await;

    // The track moved between reads: later, and nearer the drop-off.
    assert!(tracks.len() >= 3, "{tracks:#?}");
    for pair in tracks.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        assert!(
            b["runner_location"]["at"].as_str() > a["runner_location"]["at"].as_str(),
            "{a} {b}"
        );
        assert!(
            b["distance_m"].as_f64() < a["distance_m"].as_f64(),
            "{a} {b}"
        );
    }

    // The stored pings: 9, from the restaurant to the drop-off, 200 ms apart, 5 to 10 m.
    eventually("the walk ends", || !is_walking(RUN)).await;
    let stored = pings(&state.pool, RUN).await;
    assert_eq!(stored.len(), 9);
    assert!(near(stored[0].at, COSMIC));
    assert!(near(stored[8].at, FETZER));
    let mut gaps = Vec::new();
    for pair in stored.windows(2) {
        // Never sooner than the interval; later only when the database was slow.
        let gap = pair[1].ts - pair[0].ts;
        assert!(gap >= 0.18, "gap {gap} s");
        gaps.push(gap);
        // Equal steps along the line but the last.
        assert!(haversine_m(pair[0].at, pair[1].at) <= length / 7.5 + 0.05);
    }
    gaps.sort_by(f64::total_cmp);
    assert!(gaps[4] < 0.4, "median gap {} s: {gaps:?}", gaps[4]);
    let duration = stored[8].ts - stored[0].ts;
    assert!(duration >= 1.5, "walked in {duration} s");
    assert!(stored.iter().all(|p| (5.0..=10.0).contains(&p.accuracy_m)));

    // Arrived: no more pings.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(pings(&state.pool, RUN).await.len(), 9);
}

#[sqlx::test(migrations = "../migrations")]
async fn stop_ends_a_walk_and_a_restart_begins_again_at_the_restaurant(pool: PgPool) {
    const RUN: &str = "run_sim_stop";
    seed(&pool, RUN).await;
    let state = demo_state(pool).await;
    let interval = Duration::from_millis(100);
    // A long walk: 50 steps.
    let settings = WalkSettings {
        interval,
        speed: speed_for(haversine_m(COSMIC, FETZER), 50.0, interval),
    };
    start_walk_with(&state, RUN, settings).await.unwrap();
    let walked = wait_for_pings(&state.pool, RUN, 3).await;
    assert!(near(walked[0].at, COSMIC));
    assert!(!near(walked[2].at, COSMIC));

    assert!(stop_walk(RUN));
    assert!(!is_walking(RUN));
    assert!(!stop_walk(RUN), "nothing left to stop");
    // Only a ping already in flight may still land; a running walk would add several.
    let at_stop = pings(&state.pool, RUN).await.len();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let stopped = pings(&state.pool, RUN).await.len();
    assert!(
        stopped <= at_stop + 1,
        "{at_stop} pings at the stop, {stopped} a second later"
    );

    // A new start walks again from the restaurant.
    let again = start_walk_with(&state, RUN, settings).await.unwrap();
    assert!(!again.restarted);
    let after = wait_for_pings(&state.pool, RUN, stopped + 3).await;
    assert!(near(after[stopped].at, COSMIC));
    assert!(!near(after[stopped + 2].at, COSMIC));

    // A start while walking restarts: back at the restaurant, and one walk only.
    let restarted = start_walk_with(&state, RUN, settings).await.unwrap();
    assert!(restarted.restarted);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let (restart_at, all) = loop {
        let all = pings(&state.pool, RUN).await;
        // The restart's first ping is the last one at the restaurant.
        if let Some(i) = all.iter().rposition(|p| near(p.at, COSMIC))
            && i > stopped
            && all.len() >= i + 5
        {
            break (i, all);
        }
        assert!(tokio::time::Instant::now() < deadline, "{all:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    // Had the earlier walk gone on, its pings far along the line would interleave with the new
    // walk's: from the restart on, each ping is farther from the restaurant than the last.
    let from_restaurant: Vec<f64> = all[restart_at..]
        .iter()
        .map(|p| haversine_m(COSMIC, p.at))
        .collect();
    assert!(
        from_restaurant.windows(2).all(|w| w[1] > w[0]),
        "{from_restaurant:?}"
    );

    assert!(stop_walk(RUN));
}

#[sqlx::test(migrations = "../migrations")]
async fn a_walk_ends_when_the_run_leaves_the_active_states(pool: PgPool) {
    const RUN: &str = "run_sim_ends";
    seed(&pool, RUN).await;
    let state = demo_state(pool).await;
    let interval = Duration::from_millis(100);
    let settings = WalkSettings {
        interval,
        speed: speed_for(haversine_m(COSMIC, FETZER), 50.0, interval),
    };
    start_walk_with(&state, RUN, settings).await.unwrap();
    wait_for_pings(&state.pool, RUN, 2).await;

    sqlx::query(
        "UPDATE ferry.orders SET status = 'delivered', delivered_at = now() WHERE run_id = $1",
    )
    .bind(RUN)
    .execute(&state.pool)
    .await
    .unwrap();
    set_run_status(&state.pool, RUN, "delivered").await;
    eventually("the walk ends", || !is_walking(RUN)).await;
    let ended = pings(&state.pool, RUN).await.len();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(pings(&state.pool, RUN).await.len(), ended);

    // Nor does a delivered run start one.
    let err = start_walk_with(&state, RUN, settings).await.unwrap_err();
    assert!(
        matches!(err, WalkError::Inactive(ref s) if s == "delivered"),
        "{err}"
    );
    assert!(!is_walking(RUN));
}

#[sqlx::test(migrations = "../migrations")]
async fn without_osrm_the_walk_is_straight_through_each_dropoff(pool: PgPool) {
    const RUN: &str = "run_sim_fallback";
    seed(&pool, RUN).await;
    // Dana's order to Davis Library is on the way, so the walk goes there first.
    add_order(&pool, RUN, "ord_dana", MEMBER, "place_davis", DAVIS).await;
    // Port 1 refuses connections: every leg is the straight-line fallback.
    let state = state_with(
        pool,
        "http://127.0.0.1:1",
        &[("DEMO_MODE", "true"), ("SIDECAR_SHARED_SECRET", SECRET)],
    );
    let interval = Duration::from_millis(50);
    let length = haversine_m(COSMIC, DAVIS) + haversine_m(DAVIS, FETZER);
    let settings = WalkSettings {
        interval,
        speed: speed_for(length, 9.5, interval),
    };
    start_walk_with(&state, RUN, settings).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while is_walking(RUN) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the walk never ended"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let stored = pings(&state.pool, RUN).await;
    assert_eq!(stored.len(), 11);
    assert!(near(stored[0].at, COSMIC));
    assert!(near(stored[10].at, FETZER));
    // Every ping is on the straight line Cosmic to Davis, or Davis to Fetzer.
    let on = |p: LatLng, a: LatLng, b: LatLng| {
        (haversine_m(a, p) + haversine_m(p, b) - haversine_m(a, b)).abs() < 0.5
    };
    for ping in &stored {
        assert!(
            on(ping.at, COSMIC, DAVIS) || on(ping.at, DAVIS, FETZER),
            "{ping:?} is off the path"
        );
    }
    assert!(
        stored
            .iter()
            .any(|p| on(p.at, DAVIS, FETZER) && !near(p.at, DAVIS))
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn the_internal_routes_start_restart_and_stop_a_walk(pool: PgPool) {
    const RUN: &str = "run_sim_routes";
    seed(&pool, RUN).await;
    let state = demo_state(pool).await;

    // DEMO_SIM_SPEED is unset: 8 × brisk, a ping every 5 s, the first at once.
    let (status, body) = internal(&state, RUN, "walk", Some(SECRET)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(
        body,
        json!({"run_id": RUN, "speed_mps": 12.8, "interval_s": 5.0, "restarted": false})
    );
    assert!(is_walking(RUN));
    let first = wait_for_pings(&state.pool, RUN, 1).await;
    assert!(near(first[0].at, COSMIC));

    let (status, body) = internal(&state, RUN, "walk", Some(SECRET)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["restarted"], true);

    let (status, body) = internal(&state, RUN, "walk/stop", Some(SECRET)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"run_id": RUN, "stopped": true}));
    assert!(!is_walking(RUN));
    let (status, body) = internal(&state, RUN, "walk/stop", Some(SECRET)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"run_id": RUN, "stopped": false}));

    // An unknown run, a run with nothing to deliver, a delivered run.
    let (status, body) = internal(&state, "run_nope", "walk", Some(SECRET)).await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "not_found");
    sqlx::query("UPDATE ferry.orders SET status = 'cancelled' WHERE run_id = $1")
        .bind(RUN)
        .execute(&state.pool)
        .await
        .unwrap();
    let (status, body) = internal(&state, RUN, "walk", Some(SECRET)).await;
    assert_error(status, &body, StatusCode::CONFLICT, "conflict");
    set_run_status(&state.pool, RUN, "cancelled").await;
    let (status, body) = internal(&state, RUN, "walk", Some(SECRET)).await;
    assert_error(status, &body, StatusCode::CONFLICT, "conflict");
    assert!(!is_walking(RUN));
}

#[sqlx::test(migrations = "../migrations")]
async fn refused_outside_demo_mode_or_without_the_secret(pool: PgPool) {
    const RUN: &str = "run_sim_refused";
    seed(&pool, RUN).await;
    let osrm = osrm_stub().await;

    // Outside DEMO_MODE the routes do not exist, even with the secret, and start_walk refuses.
    for demo in [None, Some("false")] {
        let mut pairs = vec![("SIDECAR_SHARED_SECRET", SECRET)];
        pairs.extend(demo.map(|d| ("DEMO_MODE", d)));
        let state = state_with(pool.clone(), &osrm, &pairs);
        for action in ["walk", "walk/stop"] {
            let (status, body) = internal(&state, RUN, action, Some(SECRET)).await;
            assert_error(status, &body, StatusCode::NOT_FOUND, "not_found");
            assert_eq!(
                body["error"]["message"],
                format!("No route for POST /internal/demo/runs/{RUN}/{action}.")
            );
        }
        assert!(matches!(
            start_walk(&state, RUN).await,
            Err(WalkError::NotDemoMode)
        ));
    }

    // In DEMO_MODE: no secret, a wrong one, one of another length, and an empty one.
    let state = state_with(
        pool.clone(),
        &osrm,
        &[("DEMO_MODE", "true"), ("SIDECAR_SHARED_SECRET", SECRET)],
    );
    for secret in [
        None,
        Some("sim-test-secreT"),
        Some("sim-test-secret-2"),
        Some(""),
    ] {
        for action in ["walk", "walk/stop"] {
            let (status, body) = internal(&state, RUN, action, secret).await;
            assert_error(status, &body, StatusCode::UNAUTHORIZED, "unauthorized");
        }
    }

    // With SIDECAR_SHARED_SECRET unset, nothing passes.
    let open = state_with(pool.clone(), &osrm, &[("DEMO_MODE", "true")]);
    for secret in [None, Some(""), Some(SECRET)] {
        let (status, body) = internal(&open, RUN, "walk", secret).await;
        assert_error(status, &body, StatusCode::UNAUTHORIZED, "unauthorized");
    }

    assert!(!is_walking(RUN));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(pings(&pool, RUN).await.is_empty());
}
