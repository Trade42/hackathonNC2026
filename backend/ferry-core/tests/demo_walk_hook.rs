//! The demo follow-ups around the runner simulator: picked-up starts the demo runner's walk (only
//! in DEMO_MODE, only for a demo persona, never failing the pickup), DEMO_SIM_SPEED never stops
//! the server, the internal walk routes look like no route outside DEMO_MODE, and
//! `GET /v1/runs?mine=true` never lists demo history.
//!
//! Walks are process-wide and keyed by run id, so each test that picks up walks its own new run,
//! never `run_demo`.
mod common;
mod realtime_support;
mod runs_support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use ferry_core::app;
use ferry_core::config::{Config, DEFAULT_DEMO_SIM_SPEED};
use ferry_core::demo::simulator::{is_walking, stop_walk};
use ferry_core::state::AppState;
use realtime_support::{Server, assert_contract, eventually, next_json, serve};
use runs_support::{WEB_URL, burrito, demo_server, get, open_run, post, seed};
use serde_json::{Value, json};
use sqlx::PgPool;

/// A real (non-persona) member of the demo circle, `cir_hinton4`.
const REAL: &str = "usr_rae";

/// How long a test waits to be sure a walk did not start: the first ping comes at once.
const NO_WALK_WAIT: Duration = Duration::from_millis(1500);

/// The seeded demo served with these extra `pairs` (OSRM refused at once, deep links on).
async fn server_with(
    pool: PgPool,
    pairs: &[(&str, &str)],
    tweak: impl FnOnce(&mut Config),
) -> Server {
    seed(&pool).await;
    let mut vars = vec![
        ("PUBLIC_WEB_URL", WEB_URL),
        ("OSRM_BASE_URL", "http://127.0.0.1:1"),
    ];
    vars.extend_from_slice(pairs);
    let mut config = common::config(&vars);
    tweak(&mut config);
    serve(AppState::new(pool, config).unwrap()).await
}

/// Adds [`REAL`], a signed-up user who is no demo persona, to `cir_hinton4`.
async fn add_real_member(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO ferry.users (id, phone_e164, display_name, avatar_seed, bubble_color) \
         VALUES ($1, '+19195550177', 'Rae', $1, '#225588')",
    )
    .bind(REAL)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO ferry.circle_members (circle_id, user_id) VALUES ('cir_hinton4', $1)")
        .bind(REAL)
        .execute(pool)
        .await
        .unwrap();
}

/// `runner` opens a new run in `cir_hinton4`, the judge orders the burrito on it, and the runner
/// closes it and pays the receipt: the run is purchased, one picked-up away. Its id.
async fn purchased_run(server: &Server, runner: &str) -> String {
    let run = open_run(server, runner, "cir_hinton4").await;
    let (status, order) = post(
        server,
        &format!("/v1/runs/{run}/orders"),
        "usr_judge",
        Some(burrito()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{order}");
    let (status, body) = post(server, &format!("/v1/runs/{run}/close"), runner, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = post(
        server,
        &format!("/v1/runs/{run}/purchased"),
        runner,
        Some(json!({"receipt_total_cents": 966})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    run
}

/// `POST /v1/runs/{run}/picked-up` as `runner`: asserts 200, delivering, the contract's Run.
async fn pick_up(server: &Server, run: &str, runner: &str) {
    let (status, body) = post(server, &format!("/v1/runs/{run}/picked-up"), runner, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_contract("Run", &body);
    assert_eq!(body["status"], "delivering");
}

async fn pings(pool: &PgPool, run: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM ferry.location_pings WHERE run_id = $1")
        .bind(run)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Waits [`NO_WALK_WAIT`], then asserts no walk runs on `run` and it has no ping.
async fn assert_no_walk(pool: &PgPool, run: &str) {
    tokio::time::sleep(NO_WALK_WAIT).await;
    assert!(!is_walking(run), "a walk started on {run}");
    assert_eq!(pings(pool, run).await, 0, "pings on {run}");
}

// ---------- The walk at picked-up ----------

#[sqlx::test(migrations = "../migrations")]
async fn picked_up_starts_the_demo_runners_walk(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let run = purchased_run(&server, "usr_maya").await;
    assert_eq!(pings(&pool, &run).await, 0);
    let mut judge = server.connect_with_token("usr_judge").await;

    pick_up(&server, &run, "usr_maya").await;

    // Nothing calls /internal/demo/...: the pickup itself starts the walk. Its first ping is at
    // Cosmic Cantina at once, stored as Maya's and sent to the judge, whose order is active.
    let started = tokio::time::Instant::now();
    loop {
        let event = next_json(&mut judge).await;
        if event["type"] == "runner.location" {
            assert_contract("RunnerLocationEvent", &event);
            assert_eq!(event["data"]["run_id"], run.as_str());
            let at = &event["data"]["location"];
            assert!(
                (at["lat"].as_f64().unwrap() - 35.913211).abs() < 1e-9,
                "{event}"
            );
            assert!(
                (at["lng"].as_f64().unwrap() + 79.055094).abs() < 1e-9,
                "{event}"
            );
            break;
        }
    }
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(is_walking(&run));
    let (count, runner): (i64, String) =
        sqlx::query_as("SELECT count(*), min(user_id) FROM ferry.location_pings WHERE run_id = $1")
            .bind(&run)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(count >= 1);
    assert_eq!(runner, "usr_maya");

    // The judge's track shows Maya on the map.
    let (status, track) = get(&server, &format!("/v1/runs/{run}/track"), "usr_judge").await;
    assert_eq!(status, StatusCode::OK, "{track}");
    assert_contract("Track", &track);
    assert!(track["runner_location"].is_object(), "{track:#}");

    assert!(stop_walk(&run));
}

#[sqlx::test(migrations = "../migrations")]
async fn picked_up_starts_no_walk_outside_demo_mode(pool: PgPool) {
    let server = server_with(pool.clone(), &[], |_| {}).await;
    assert!(!server.state.config.demo_mode);
    let run = purchased_run(&server, "usr_maya").await;
    pick_up(&server, &run, "usr_maya").await;
    assert_no_walk(&pool, &run).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn picked_up_starts_no_walk_for_a_real_users_run(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    add_real_member(&pool).await;
    let run = purchased_run(&server, REAL).await;
    pick_up(&server, &run, REAL).await;
    assert_no_walk(&pool, &run).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn picked_up_answers_200_when_the_walk_cannot_start(pool: PgPool) {
    // A speed the config can no longer produce makes start_walk refuse (BadSettings).
    let server = server_with(pool.clone(), &[("DEMO_MODE", "true")], |c| {
        c.demo_sim_speed = f64::NAN;
    })
    .await;
    let logs = Logs::default();
    let _guard = tracing::subscriber::set_default(logs.subscriber());
    let run = purchased_run(&server, "usr_maya").await;

    pick_up(&server, &run, "usr_maya").await;

    eventually("the walk's warning", || {
        logs.text().contains("demo walk not started")
    })
    .await;
    let text = logs.text();
    let line = text
        .lines()
        .find(|l| l.contains("demo walk not started"))
        .unwrap();
    assert!(line.contains("WARN"), "{line}");
    assert!(line.contains(&run), "{line}");
    assert!(
        line.contains("interval and speed must be above 0"),
        "{line}"
    );
    assert_no_walk(&pool, &run).await;
    let (_, detail) = get(&server, &format!("/v1/runs/{run}"), "usr_maya").await;
    assert_eq!(detail["status"], "delivering");
}

// ---------- DEMO_SIM_SPEED ----------

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

    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

/// The config `pairs` build, and what it logged.
fn config_logging(pairs: &[(&str, &str)]) -> (Config, String) {
    let logs = Logs::default();
    let config = tracing::subscriber::with_default(logs.subscriber(), || common::config(pairs));
    (config, logs.text())
}

#[test]
fn bad_demo_sim_speeds_boot_with_the_default_and_one_warning() {
    for demo_mode in ["true", "false"] {
        for bad in [
            "fast", "0", "1e-9", "10000", "NaN", "inf", "-3", "0.49", "100.5",
        ] {
            let (config, logs) =
                config_logging(&[("DEMO_MODE", demo_mode), ("DEMO_SIM_SPEED", bad)]);
            let what = format!("DEMO_MODE={demo_mode} DEMO_SIM_SPEED={bad}");
            assert_eq!(config.demo_sim_speed, DEFAULT_DEMO_SIM_SPEED, "{what}");
            let lines: Vec<&str> = logs.lines().collect();
            assert_eq!(lines.len(), 1, "{what}: {logs}");
            assert!(lines[0].contains("WARN"), "{what}: {logs}");
            assert!(lines[0].contains("DEMO_SIM_SPEED"), "{what}: {logs}");
            if demo_mode == "true" {
                assert!(lines[0].contains("from 0.5 to 100"), "{what}: {logs}");
            } else {
                assert!(lines[0].contains("DEMO_MODE is off"), "{what}: {logs}");
            }
            for word in ["fast", "NaN", "10000", "1e-9", "inf"] {
                assert!(!lines[0].contains(word), "{what}: the value leaked: {logs}");
            }
        }
    }
}

#[test]
fn good_demo_sim_speeds_are_used_in_demo_mode_only() {
    for (value, speed) in [("0.5", 0.5), ("2.5", 2.5), ("8", 8.0), (" 100 ", 100.0)] {
        let (config, logs) = config_logging(&[("DEMO_MODE", "true"), ("DEMO_SIM_SPEED", value)]);
        assert_eq!(config.demo_sim_speed, speed, "{value}");
        assert!(logs.is_empty(), "{value}: {logs}");

        // Outside DEMO_MODE a set value is ignored, with one warning.
        let (config, logs) = config_logging(&[("DEMO_SIM_SPEED", value)]);
        assert_eq!(config.demo_sim_speed, DEFAULT_DEMO_SIM_SPEED, "{value}");
        assert_eq!(logs.lines().count(), 1, "{value}: {logs}");
        assert!(logs.contains("DEMO_MODE is off"), "{logs}");
    }
    // Unset or empty: the default, silently, in either mode.
    for pairs in [
        &[][..],
        &[("DEMO_MODE", "true")][..],
        &[("DEMO_MODE", "true"), ("DEMO_SIM_SPEED", "")][..],
        &[("DEMO_SIM_SPEED", "  ")][..],
    ] {
        let (config, logs) = config_logging(pairs);
        assert_eq!(config.demo_sim_speed, DEFAULT_DEMO_SIM_SPEED, "{pairs:?}");
        assert!(logs.is_empty(), "{pairs:?}: {logs}");
    }
}

// ---------- The internal walk routes outside DEMO_MODE ----------

async fn send_raw(
    state: &AppState,
    method: &Method,
    uri: &str,
) -> (StatusCode, Option<String>, Value) {
    let request = Request::builder()
        .method(method.clone())
        .uri(uri)
        .header("x-ferry-secret", "sim-test-secret")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let (status, headers, body) = common::send(app::router(state.clone()), request).await;
    let allow = headers
        .get(header::ALLOW)
        .map(|v| v.to_str().unwrap().to_owned());
    (status, allow, body)
}

#[tokio::test]
async fn walk_routes_answer_every_method_as_an_unknown_route_outside_demo_mode() {
    let pairs = [("SIDECAR_SHARED_SECRET", "sim-test-secret")];
    let off = AppState::new(common::unreachable_pool(), common::config(&pairs)).unwrap();
    let methods = [
        Method::GET,
        Method::HEAD,
        Method::POST,
        Method::PUT,
        Method::PATCH,
        Method::DELETE,
        Method::OPTIONS,
    ];
    for path in [
        "/internal/demo/runs/run_demo/walk",
        "/internal/demo/runs/run_demo/walk/stop",
    ] {
        for method in &methods {
            let (status, allow, body) = send_raw(&off, method, path).await;
            let unknown = format!("{path}x");
            let (unknown_status, unknown_allow, unknown_body) =
                send_raw(&off, method, &unknown).await;
            assert_eq!(status, unknown_status, "{method} {path}: {body}");
            assert_eq!(allow, unknown_allow, "{method} {path}");
            assert_eq!(allow, None, "{method} {path}");
            if method == Method::OPTIONS {
                // The CORS layer answers every OPTIONS before routing, on either path.
                assert_eq!(status, StatusCode::OK);
                assert_eq!((&body, &unknown_body), (&Value::Null, &Value::Null));
                continue;
            }
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}: {body}");
            if method == Method::HEAD {
                // No body for HEAD, on either.
                assert_eq!((&body, &unknown_body), (&Value::Null, &Value::Null));
                continue;
            }
            common::assert_envelope(&body, "not_found");
            assert_eq!(
                body,
                json!({"error": {"code": "not_found", "message": format!("No route for {method} {path}.")}}),
            );
            // Exactly the unknown route's answer, but for the path.
            let message = unknown_body["error"]["message"].as_str().unwrap();
            assert_eq!(message.replace(&unknown, path), body["error"]["message"]);
        }
    }

    // In DEMO_MODE nothing else changes: POST checks the secret first, other methods are 405.
    let on = AppState::new(
        common::unreachable_pool(),
        common::config(&[("DEMO_MODE", "true"), ("SIDECAR_SHARED_SECRET", "another")]),
    )
    .unwrap();
    let path = "/internal/demo/runs/run_demo/walk";
    let (status, _, body) = send_raw(&on, &Method::POST, path).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    common::assert_envelope(&body, "unauthorized");
    let (status, _, body) = send_raw(&on, &Method::POST, &format!("{path}/stop")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    let (status, allow, body) = send_raw(&on, &Method::GET, path).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{body}");
    assert_eq!(allow.as_deref(), Some("POST"));
    assert_eq!(
        body["error"]["message"],
        format!("GET is not allowed on {path}.")
    );
}

// ---------- mine=true and demo history ----------

#[sqlx::test(migrations = "../migrations")]
async fn mine_never_lists_demo_history(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    ferry_core::impact::seed_history(&pool).await.unwrap();
    // History runs are all delivered, which hid them from mine=true; make some of Maya's look
    // active, so only the history filter keeps them out.
    let flipped: Vec<String> = sqlx::query_scalar(
        "UPDATE ferry.runs SET status = s.status FROM ( \
             SELECT id, (ARRAY['open', 'closed', 'purchased', 'delivering'])[1 + (row_number() OVER (ORDER BY id))::int % 4] AS status \
             FROM ferry.runs WHERE is_demo_history AND runner_id = 'usr_maya' ORDER BY id LIMIT 4 \
         ) s WHERE ferry.runs.id = s.id RETURNING ferry.runs.id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(flipped.len(), 4, "the history has runs by Maya");

    for uri in [
        "/v1/runs?mine=true",
        "/v1/runs?mine=true&status=open",
        "/v1/runs?mine=true&status=closed",
        "/v1/runs?mine=true&status=purchased",
        "/v1/runs?mine=true&status=delivering",
    ] {
        let (status, runs) = get(&server, uri, "usr_maya").await;
        assert_eq!(status, StatusCode::OK, "{runs}");
        let ids: Vec<&str> = runs
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                assert_contract("Run", r);
                r["id"].as_str().unwrap()
            })
            .collect();
        for id in &flipped {
            assert!(!ids.contains(&id.as_str()), "{uri} lists history run {id}");
        }
        if uri == "/v1/runs?mine=true" || uri.ends_with("status=open") {
            assert_eq!(ids, vec!["run_demo"], "{uri}");
        } else {
            assert!(ids.is_empty(), "{uri}: {ids:?}");
        }
    }
}
