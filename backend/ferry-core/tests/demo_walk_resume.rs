//! The demo walk survives a ferry-core restart: `demo::simulator::resume_walks_with` (what main.rs
//! runs once the server listens) walks on each delivering demo-persona run from its latest ping,
//! at the same pace, to the drop-off. A restart is simulated as the process would see it: the
//! walk's task is gone and the registry empty (`stop_walk`), then a fresh `AppState` resumes.
//!
//! Also: while the simulator walks a run in DEMO_MODE, its runner's phone pings are answered 204
//! and not stored (integration pass S2).
//!
//! OSRM is refused at once (port 1), so every leg is the straight line and positions along the
//! walk are exact. Walks are process-wide and keyed by run id, so each test uses its own run ids.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use ferry_core::app;
use ferry_core::demo::simulator::{
    WalkSettings, is_walking, resume_at_startup, resume_walks_with, start_walk_with, stop_walk,
};
use ferry_core::notifications::now_rfc3339;
use ferry_core::state::AppState;
use ferry_core::tracking::{self, LatLng, Pace, Routing, haversine_m, latlng};
use serde_json::{Value, json};
use sqlx::PgPool;

const COSMIC: LatLng = latlng(35.913211, -79.055094);
const FETZER: LatLng = latlng(35.909152, -79.046905);
const DAVIS: LatLng = latlng(35.910957, -79.047906);

const MAYA: &str = "usr_maya";
const JUDGE: &str = "usr_judge";
/// A signed-up runner who is no demo persona.
const RAE: &str = "usr_rae";

/// Ping every 100 ms in these tests.
const INTERVAL: Duration = Duration::from_millis(100);

// ---------- State ----------

/// A fresh process's state: OSRM refused at once, without FOSSGIS's pacing.
fn state_with(pool: PgPool, pairs: &[(&str, &str)]) -> AppState {
    let mut vars = vec![("OSRM_BASE_URL", "http://127.0.0.1:1")];
    vars.extend_from_slice(pairs);
    let mut state = AppState::new(pool, common::config(&vars)).unwrap();
    let mut settings = state.routing.settings().clone();
    settings.min_interval = Duration::ZERO;
    state.routing = Routing::new(state.http.clone(), settings);
    state
}

fn demo_state(pool: PgPool) -> AppState {
    state_with(pool, &[("DEMO_MODE", "true")])
}

/// Settings that walk `length_m` in `steps` pings after the first (rounded up), at Maya's brisk
/// pace and a ping every [`INTERVAL`].
fn settings_for(length_m: f64, steps: f64) -> WalkSettings {
    WalkSettings {
        interval: INTERVAL,
        speed: length_m / steps / (Pace::Brisk.meters_per_second() * INTERVAL.as_secs_f64()),
    }
}

/// Meters between two pings at `settings`, for brisk Maya.
fn step_m(settings: WalkSettings) -> f64 {
    Pace::Brisk.meters_per_second() * settings.speed * settings.interval.as_secs_f64()
}

// ---------- Fixtures ----------

/// Maya (brisk, a demo persona), the judge and Rae (no persona) in one circle; Cosmic Cantina;
/// the Fetzer Gym and Davis Library drop-offs.
async fn seed(pool: &PgPool) {
    sqlx::raw_sql(
        r##"
        INSERT INTO ferry.users (id, phone_e164, display_name, avatar_seed, bubble_color) VALUES
            ('usr_maya',  '+19195550102', 'Maya',   'maya',  '#336699'),
            ('usr_judge', '+19195550101', 'Jordan', 'judge', '#336699'),
            ('usr_rae',   '+19195550177', 'Rae',    'rae',   '#225588');
        INSERT INTO ferry.profiles (user_id, walking_pace) VALUES
            ('usr_maya', 'brisk'), ('usr_rae', 'brisk');
        INSERT INTO ferry.circles (id, name, emoji) VALUES ('cir_t', 'Hinton James 4th floor', '🌊');
        INSERT INTO ferry.circle_members (circle_id, user_id) VALUES
            ('cir_t', 'usr_maya'), ('cir_t', 'usr_judge'), ('cir_t', 'usr_rae');
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
}

/// `runner`'s run to Cosmic Cantina in `status`, with the judge's order to Fetzer Gym in
/// `order_status`.
async fn add_run(pool: &PgPool, run: &str, runner: &str, status: &str, order_status: &str) {
    sqlx::query(
        "INSERT INTO ferry.runs (id, restaurant_id, runner_id, circle_id, status, created_via, \
                                 leaving_at, window_closes_at) \
         VALUES ($1, 'rst_cosmic', $2, 'cir_t', $3, 'app', now(), now())",
    )
    .bind(run)
    .bind(runner)
    .bind(status)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ferry.orders (id, run_id, customer_id, status, subtotal_cents, tax_cents, \
                                   ferry_fee_cents, tip_cents, total_cents, dropoff_place_id, \
                                   dropoff_label, dropoff_lat, dropoff_lng, handoff_code, \
                                   delivered_at) \
         VALUES ($1, $2, $3, $4, 1000, 75, 150, 100, 1325, 'place_fetzer', 'Fetzer Gym (HackNC)', \
                 $5, $6, '4829', CASE WHEN $4 = 'delivered' THEN now() END)",
    )
    .bind(format!("ord_{run}"))
    .bind(run)
    .bind(JUDGE)
    .bind(order_status)
    .bind(FETZER.lat)
    .bind(FETZER.lng)
    .execute(pool)
    .await
    .unwrap();
}

/// A ping on `run` at `at`, measured `ago` before now, as `runner`'s phone would post it.
async fn add_ping(pool: &PgPool, run: &str, runner: &str, at: LatLng, ago: Duration) {
    sqlx::query(
        "INSERT INTO ferry.location_pings (run_id, user_id, ts, lat, lng, accuracy_m) \
         VALUES ($1, $2, now() - make_interval(secs => $3), $4, $5, 8)",
    )
    .bind(run)
    .bind(runner)
    .bind(ago.as_secs_f64())
    .bind(at.lat)
    .bind(at.lng)
    .execute(pool)
    .await
    .unwrap();
}

/// The run's stored pings, oldest first.
async fn pings(pool: &PgPool, run: &str) -> Vec<LatLng> {
    let rows: Vec<(f64, f64)> = sqlx::query_as(
        "SELECT lat, lng FROM ferry.location_pings WHERE run_id = $1 ORDER BY ts, id",
    )
    .bind(run)
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(lat, lng)| latlng(lat, lng))
        .collect()
}

/// Waits until `run` has at least `n` pings; returns them.
async fn wait_for_pings(pool: &PgPool, run: &str, n: usize) -> Vec<LatLng> {
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

/// Waits until no walk runs on `run`.
async fn wait_for_end(run: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while is_walking(run) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{run}: the walk never ended"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The process dies: its walk's task is gone and the registry empty. Returns the pings stored by
/// then, after any ping in flight has landed.
async fn kill(pool: &PgPool, run: &str) -> Vec<LatLng> {
    assert!(stop_walk(run), "{run} was not walking");
    tokio::time::sleep(Duration::from_millis(300)).await;
    pings(pool, run).await
}

fn near(a: LatLng, b: LatLng) -> bool {
    haversine_m(a, b) < 0.01
}

/// Asserts `walked` is one walk at `step` meters a ping along the straight line to Fetzer: each
/// ping farther from Cosmic than the last, every step `step` but the last (what remains), and
/// the last exactly at Fetzer.
fn assert_walks_on(walked: &[LatLng], step: f64) {
    assert!(walked.len() >= 2, "{walked:?}");
    let from_cosmic: Vec<f64> = walked.iter().map(|&p| haversine_m(COSMIC, p)).collect();
    assert!(
        from_cosmic.windows(2).all(|w| w[1] > w[0]),
        "{from_cosmic:?}"
    );
    for pair in walked.windows(2).take(walked.len() - 2) {
        let moved = haversine_m(pair[0], pair[1]);
        assert!(
            (moved - step).abs() < 0.05,
            "a step of {moved} m, not {step}"
        );
    }
    let last = haversine_m(walked[walked.len() - 2], walked[walked.len() - 1]);
    assert!(last > 0.0 && last <= step + 0.05, "a last step of {last} m");
    assert!(near(*walked.last().unwrap(), FETZER), "{walked:?}");
}

// ---------- Logs ----------

/// Every event at any level, as text.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Logs {
    fn subscriber(&self) -> impl tracing::Subscriber + Send + Sync + use<> {
        let writer = self.clone();
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .without_time()
            .with_writer(move || writer.clone())
            .finish()
    }

    fn lines_with(&self, text: &str) -> Vec<String> {
        String::from_utf8(self.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .filter(|l| l.contains(text))
            .map(str::to_owned)
            .collect()
    }
}

// ---------- Tests ----------

#[sqlx::test(migrations = "../migrations")]
async fn an_interrupted_walk_continues_from_its_last_ping_to_the_dropoff(pool: PgPool) {
    const RUN: &str = "run_resume_mid";
    seed(&pool).await;
    add_run(&pool, RUN, MAYA, "delivering", "picked_up").await;
    let settings = settings_for(haversine_m(COSMIC, FETZER), 19.5);
    let step = step_m(settings);

    // Picked-up started the walk; the server dies a few pings in.
    let first = demo_state(pool.clone());
    let started = start_walk_with(&first, RUN, settings).await.unwrap();
    assert!(!started.restarted);
    wait_for_pings(&pool, RUN, 5).await;
    let before = kill(&pool, RUN).await;
    drop(first);
    let stopped_at = *before.last().unwrap();
    assert!(near(before[0], COSMIC));
    assert!(!near(stopped_at, COSMIC) && !near(stopped_at, FETZER));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        pings(&pool, RUN).await.len(),
        before.len(),
        "a dead walk pinged"
    );

    // The new process resumes it, and dies again a few pings later (a crash loop).
    let second = demo_state(pool.clone());
    assert_eq!(resume_walks_with(&second, settings).await, [RUN]);
    assert!(is_walking(RUN));
    wait_for_pings(&pool, RUN, before.len() + 3).await;
    let middle = kill(&pool, RUN).await;
    drop(second);
    let resumed = &middle[before.len()..];
    // On from where it stopped: the first resumed ping is where the last one was, never back at
    // the restaurant, then on at the same pace.
    assert!(near(resumed[0], stopped_at), "{resumed:?}");
    assert!(resumed.iter().all(|&p| !near(p, COSMIC)), "{resumed:?}");
    let on: Vec<f64> = resumed
        .windows(2)
        .map(|w| haversine_m(w[0], w[1]))
        .collect();
    assert!(
        on.iter().all(|m| (m - step).abs() < 0.05),
        "{on:?} m, not {step}"
    );

    // The third process resumes it again and it reaches Fetzer: the walk ends as any walk does.
    let third = demo_state(pool.clone());
    assert_eq!(resume_walks_with(&third, settings).await, [RUN]);
    wait_for_end(RUN).await;
    let all = pings(&pool, RUN).await;
    let last_stop = *middle.last().unwrap();
    let resumed = &all[middle.len()..];
    assert!(near(resumed[0], last_stop), "{resumed:?}");
    assert_walks_on(&resumed[1..], step);
    // Exactly the steps that were left after the second stop, plus its repeat: one walk.
    let left = haversine_m(last_stop, FETZER);
    assert_eq!(
        resumed.len(),
        (left / step).ceil() as usize + 1,
        "{resumed:?}"
    );

    // Over the three processes, one walk: the pings without the two repeats are one straight walk
    // from Cosmic to Fetzer, at one pace.
    let mut walked = before.clone();
    walked.extend_from_slice(&middle[before.len() + 1..]);
    walked.extend_from_slice(&resumed[1..]);
    assert!(near(walked[0], COSMIC));
    assert_walks_on(&walked, step);

    // The judge's track shows Maya at Fetzer.
    let track = tracking::track_for(&third, RUN, JUDGE).await.unwrap();
    assert!(near(track.runner_location.unwrap().location, FETZER));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        pings(&pool, RUN).await.len(),
        all.len(),
        "pings after arrival"
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn without_a_ping_the_resumed_walk_starts_at_the_restaurant(pool: PgPool) {
    const RUN: &str = "run_resume_noping";
    seed(&pool).await;
    add_run(&pool, RUN, MAYA, "delivering", "picked_up").await;
    let length = haversine_m(COSMIC, FETZER);
    let settings = settings_for(length, 7.5);

    let state = demo_state(pool.clone());
    assert_eq!(resume_walks_with(&state, settings).await, [RUN]);
    wait_for_end(RUN).await;
    let walked = pings(&pool, RUN).await;
    assert_eq!(walked.len(), 9, "{walked:?}");
    assert!(near(walked[0], COSMIC));
    assert_walks_on(&walked, step_m(settings));
}

#[sqlx::test(migrations = "../migrations")]
async fn an_off_route_ping_walks_on_from_there(pool: PgPool) {
    // The latest ping, at Davis Library, is about 145 m off the restaurant's straight line to
    // Fetzer (a phone's ping, say): the walk goes on from it, straight to the drop-off, along the
    // route the track shows from there.
    const RUN: &str = "run_resume_offroute";
    seed(&pool).await;
    add_run(&pool, RUN, MAYA, "delivering", "picked_up").await;
    add_ping(&pool, RUN, MAYA, COSMIC, Duration::from_secs(20)).await;
    add_ping(&pool, RUN, MAYA, DAVIS, Duration::from_secs(10)).await;
    let length = haversine_m(DAVIS, FETZER);
    let settings = settings_for(length, 3.5);

    let state = demo_state(pool.clone());
    assert_eq!(resume_walks_with(&state, settings).await, [RUN]);
    wait_for_end(RUN).await;
    let walked = pings(&pool, RUN).await;
    assert_eq!(walked.len(), 2 + 5, "{walked:?}");
    let resumed = &walked[2..];
    assert!(near(resumed[0], DAVIS));
    let on_line = |p: LatLng| {
        (haversine_m(DAVIS, p) + haversine_m(p, FETZER) - haversine_m(DAVIS, FETZER)).abs() < 0.5
    };
    assert!(resumed.iter().all(|&p| on_line(p)), "{resumed:?}");
    let from_davis: Vec<f64> = resumed.iter().map(|&p| haversine_m(DAVIS, p)).collect();
    assert!(from_davis.windows(2).all(|w| w[1] > w[0]), "{from_davis:?}");
    assert!(near(*resumed.last().unwrap(), FETZER));
}

#[sqlx::test(migrations = "../migrations")]
async fn a_walk_that_had_arrived_stays_at_the_dropoff(pool: PgPool) {
    // The walk reached Fetzer, and the judge has not tapped Got it yet: the run is still
    // delivering. A restart repeats the ping at Fetzer and ends.
    const RUN: &str = "run_resume_arrived";
    seed(&pool).await;
    add_run(&pool, RUN, MAYA, "delivering", "picked_up").await;
    add_ping(&pool, RUN, MAYA, FETZER, Duration::from_secs(30)).await;

    let state = demo_state(pool.clone());
    let settings = settings_for(haversine_m(COSMIC, FETZER), 7.5);
    assert_eq!(resume_walks_with(&state, settings).await, [RUN]);
    wait_for_end(RUN).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let walked = pings(&pool, RUN).await;
    assert_eq!(walked.len(), 2, "{walked:?}");
    assert!(walked.iter().all(|&p| near(p, FETZER)), "{walked:?}");
}

#[sqlx::test(migrations = "../migrations")]
async fn only_delivering_demo_persona_runs_resume_and_only_in_demo_mode(pool: PgPool) {
    const OK: &str = "run_resume_ok";
    const REAL: &str = "run_resume_real";
    const PURCHASED: &str = "run_resume_purchased";
    const DELIVERED: &str = "run_resume_delivered";
    const NOTHING_LEFT: &str = "run_resume_nothing";
    const HISTORY: &str = "run_resume_history";
    const OTHERS: [&str; 5] = [REAL, PURCHASED, DELIVERED, NOTHING_LEFT, HISTORY];
    seed(&pool).await;
    add_run(&pool, OK, MAYA, "delivering", "picked_up").await;
    // A real user's run, a run before pickup, a delivered run, a delivering run whose orders are
    // all delivered, and demo history.
    add_run(&pool, REAL, RAE, "delivering", "picked_up").await;
    add_run(&pool, PURCHASED, MAYA, "purchased", "purchased").await;
    add_run(&pool, DELIVERED, MAYA, "delivered", "delivered").await;
    add_run(&pool, NOTHING_LEFT, MAYA, "delivering", "delivered").await;
    add_run(&pool, HISTORY, MAYA, "delivering", "picked_up").await;
    sqlx::query("UPDATE ferry.runs SET is_demo_history = true WHERE id = $1")
        .bind(HISTORY)
        .execute(&pool)
        .await
        .unwrap();
    let midway = latlng(
        (COSMIC.lat + FETZER.lat) / 2.0,
        (COSMIC.lng + FETZER.lng) / 2.0,
    );
    for run in [OK].iter().chain(&OTHERS) {
        let runner = if *run == REAL { RAE } else { MAYA };
        add_ping(&pool, run, runner, midway, Duration::from_secs(10)).await;
    }
    let settings = settings_for(haversine_m(COSMIC, FETZER), 50.0);

    // Outside DEMO_MODE: nothing, from either entry point.
    for demo in [None, Some("false")] {
        let state = state_with(
            pool.clone(),
            &Vec::from_iter(demo.map(|d| ("DEMO_MODE", d))),
        );
        assert!(!state.config.demo_mode);
        assert!(resume_walks_with(&state, settings).await.is_empty());
        assert!(resume_at_startup(&state).is_none());
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    for run in [OK].iter().chain(&OTHERS) {
        assert!(!is_walking(run), "{run}");
        assert_eq!(pings(&pool, run).await.len(), 1, "{run}");
    }

    // In DEMO_MODE, only the demo persona's delivering run with an active order.
    let state = demo_state(pool.clone());
    assert_eq!(resume_walks_with(&state, settings).await, [OK]);
    let walked = wait_for_pings(&pool, OK, 3).await;
    assert!(near(walked[1], midway));
    assert!(stop_walk(OK));
    // The startup entry point, at the demo's pace, does the same in the background.
    let started = resume_at_startup(&state).expect("DEMO_MODE resumes");
    started.await.unwrap();
    assert!(is_walking(OK));
    assert!(stop_walk(OK));
    tokio::time::sleep(Duration::from_millis(300)).await;
    for run in OTHERS {
        assert!(!is_walking(run), "{run}");
        assert_eq!(pings(&pool, run).await.len(), 1, "{run}");
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn a_resume_never_doubles_a_running_walk(pool: PgPool) {
    const RUN: &str = "run_resume_running";
    seed(&pool).await;
    add_run(&pool, RUN, MAYA, "delivering", "picked_up").await;
    let settings = settings_for(haversine_m(COSMIC, FETZER), 14.5);

    // A fresh start is walking; a resume keeps it and adds no walk.
    let state = demo_state(pool.clone());
    start_walk_with(&state, RUN, settings).await.unwrap();
    wait_for_pings(&pool, RUN, 2).await;
    assert!(resume_walks_with(&state, settings).await.is_empty());
    let other = demo_state(pool.clone());
    assert!(resume_walks_with(&other, settings).await.is_empty());
    assert!(is_walking(RUN));
    wait_for_end(RUN).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    // One walk: 16 pings from Cosmic to Fetzer, none repeated.
    let walked = pings(&pool, RUN).await;
    assert_eq!(walked.len(), 16, "{walked:?}");
    assert!(near(walked[0], COSMIC));
    assert_walks_on(&walked, step_m(settings));
}

#[sqlx::test(migrations = "../migrations")]
async fn a_start_after_a_resume_keeps_one_walk_and_a_resume_logs_one_line(pool: PgPool) {
    const RUN: &str = "run_resume_then_start";
    seed(&pool).await;
    add_run(&pool, RUN, MAYA, "delivering", "picked_up").await;
    let midway = latlng(
        (COSMIC.lat + FETZER.lat) / 2.0,
        (COSMIC.lng + FETZER.lng) / 2.0,
    );
    add_ping(&pool, RUN, MAYA, midway, Duration::from_secs(10)).await;
    let settings = settings_for(haversine_m(COSMIC, FETZER), 14.5);
    let logs = Logs::default();
    let _guard = tracing::subscriber::set_default(logs.subscriber());

    // Resumed once; a second resume in the same process finds it walking and does nothing.
    let state = demo_state(pool.clone());
    assert_eq!(resume_walks_with(&state, settings).await, [RUN]);
    assert!(resume_walks_with(&state, settings).await.is_empty());
    let lines = logs.lines_with("demo walk resumed");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("INFO"), "{}", lines[0]);
    assert!(
        lines[0].ends_with(&format!("demo walk resumed run_id=\"{RUN}\"")),
        "{}",
        lines[0]
    );
    wait_for_pings(&pool, RUN, 3).await;

    // A start (the internal route) restarts it from the restaurant: still one walk.
    let restart = start_walk_with(&state, RUN, settings).await.unwrap();
    assert!(restart.restarted);
    wait_for_end(RUN).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let all = pings(&pool, RUN).await;
    let from = all.iter().rposition(|&p| near(p, COSMIC)).unwrap();
    assert_eq!(all.len() - from, 16, "{all:?}");
    assert_walks_on(&all[from..], step_m(settings));
    assert_eq!(logs.lines_with("demo walk resumed").len(), 1);
}

// ---------- The runner's phone during a walk ----------

/// `POST /v1/runs/{run}/location` as `user`'s phone, at `at` now.
async fn post_location(state: &AppState, run: &str, user: &str, at: LatLng) -> (StatusCode, Value) {
    let body = json!({
        "location": {"lat": at.lat, "lng": at.lng},
        "accuracy_m": 6.0,
        "at": now_rfc3339(),
    });
    let request = Request::post(format!("/v1/runs/{run}/location"))
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", state.jwt.issue_token(user).unwrap()),
        )
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, _, body) = common::send(app::router(state.clone()), request).await;
    (status, body)
}

#[sqlx::test(migrations = "../migrations")]
async fn the_runners_phone_does_not_fight_a_demo_walk(pool: PgPool) {
    const RUN: &str = "run_resume_phone";
    seed(&pool).await;
    add_run(&pool, RUN, MAYA, "delivering", "picked_up").await;
    // A long walk: 50 steps.
    let settings = settings_for(haversine_m(COSMIC, FETZER), 50.0);
    let at_davis = |pings: &[LatLng]| pings.iter().filter(|&&p| near(p, DAVIS)).count();
    let logs = Logs::default();
    let _guard = tracing::subscriber::set_default(logs.subscriber());

    // While the simulator walks the run, Maya's phone (at Davis Library) gets its 204, and nothing
    // is stored; one line names the run.
    let state = demo_state(pool.clone());
    start_walk_with(&state, RUN, settings).await.unwrap();
    wait_for_pings(&pool, RUN, 2).await;
    let (status, body) = post_location(&state, RUN, MAYA, DAVIS).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(body, Value::Null);
    let lines = logs.lines_with("phone ping ignored");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("INFO"), "{}", lines[0]);
    assert!(
        lines[0].ends_with(&format!(
            "phone ping ignored: the demo walk is running run_id=\"{RUN}\""
        )),
        "{}",
        lines[0]
    );
    // Anyone else's ping takes the usual checks: the judge is not the runner.
    let (status, body) = post_location(&state, RUN, JUDGE, DAVIS).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    common::assert_envelope(&body, "forbidden");
    wait_for_pings(&pool, RUN, 4).await;
    assert_eq!(at_davis(&pings(&pool, RUN).await), 0);

    // Outside DEMO_MODE nothing is ignored, even with a walk in this process.
    let live = state_with(pool.clone(), &[]);
    assert!(is_walking(RUN));
    let (status, body) = post_location(&live, RUN, MAYA, DAVIS).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(at_davis(&pings(&pool, RUN).await), 1);

    // Once the walk stops, the phone's ping is stored as always.
    assert!(stop_walk(RUN));
    let (status, body) = post_location(&state, RUN, MAYA, DAVIS).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(at_davis(&pings(&pool, RUN).await), 2);
    assert_eq!(logs.lines_with("phone ping ignored").len(), 1);
}
