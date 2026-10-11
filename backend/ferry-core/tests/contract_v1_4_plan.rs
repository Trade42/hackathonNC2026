//! Contract v1.4 on runs and orders, over HTTP on the seeded demo: the going-somewhere plan
//! (`returning_at`, `return_place`, `dropoff_radius_m`) on `POST /v1/runs`, every problem with it,
//! the runner's profile walk distance as the default radius (the agent's `open_run` too), the plan
//! in `Run`, `RunDetail`, lists and `run.updated`, the unchanged four-field public preview, and
//! `dropoff_radius_m` stored and echoed on orders. Every response is checked against
//! contract/openapi.yaml; the migration's CHECKs are tried directly.
mod common;
mod realtime_support;
mod runs_support;

use axum::http::{Method, StatusCode};
use ferry_core::runs::{self, CreateRunRequest, CreatedVia};
use realtime_support::{Server, assert_contract};
use runs_support::{
    burrito, call, demo_server, expect_events, fetzer, get, minutes_from_now, post,
};
use serde_json::{Value, json};
use sqlx::PgPool;

const PLAN_FIELDS: [&str; 3] = ["returning_at", "return_place", "dropoff_radius_m"];

/// Hinton James, a fixture place, with its note padded to show it is stored trimmed.
fn hinton() -> Value {
    json!({
        "id": "place_hinton",
        "label": "Hinton James lobby",
        "location": {"lat": 35.902364, "lng": -79.043231},
        "note": "  Main lobby.  "
    })
}

/// A `POST /v1/runs` body to Cosmic Cantina in cir_hinton4, leaving in 5 minutes with a
/// 30-minute window, with `plan`'s fields added.
async fn body(server: &Server, plan: Value) -> Value {
    let mut body = json!({
        "restaurant_id": "rst_cosmic",
        "circle_id": "cir_hinton4",
        "leaving_at": minutes_from_now(&server.state.pool, 5).await,
        "window_minutes": 30
    });
    for (key, value) in plan.as_object().unwrap() {
        body[key] = value.clone();
    }
    body
}

/// Opens a run as Maya with `plan`: the run, checked against the contract.
async fn open(server: &Server, plan: Value) -> Value {
    let request = body(server, plan).await;
    let (status, run) = post(server, "/v1/runs", "usr_maya", Some(request)).await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_contract("Run", &run);
    run
}

/// The run's plan as the database stores it.
async fn stored_plan(pool: &PgPool, run_id: &str) -> Value {
    sqlx::query_scalar(
        "SELECT json_build_object('returning_at', returning_at, 'place_id', return_place_id, \
                'label', return_label, 'lat', return_lat, 'lng', return_lng, 'note', return_note, \
                'radius', dropoff_radius_m) FROM ferry.runs WHERE id = $1",
    )
    .bind(run_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn run_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM ferry.runs")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Sets `user`'s profile walk distance directly.
async fn set_walk_distance(pool: &PgPool, user: &str, meters: Option<i32>) {
    sqlx::query(
        "INSERT INTO ferry.profiles (user_id, max_dropoff_distance_m) VALUES ($1, $2) \
         ON CONFLICT (user_id) DO UPDATE SET max_dropoff_distance_m = EXCLUDED.max_dropoff_distance_m",
    )
    .bind(user)
    .bind(meters)
    .execute(pool)
    .await
    .unwrap();
}

fn assert_plan_is_null(run: &Value, what: &str) {
    for field in PLAN_FIELDS {
        assert_eq!(
            run.get(field),
            Some(&Value::Null),
            "{what}: {field} in {run:#}"
        );
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn a_run_without_the_plan_serves_three_nulls(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let run = open(&server, json!({})).await;
    assert_plan_is_null(&run, "POST /v1/runs");
    let id = run["id"].as_str().unwrap();

    let (status, detail) = get(&server, &format!("/v1/runs/{id}"), "usr_judge").await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_contract("RunDetail", &detail);
    assert_plan_is_null(&detail, "GET /v1/runs/{id}");

    let (status, list) = get(&server, "/v1/runs?status=open", "usr_judge").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let listed = list
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .expect("the new run is listed");
    assert_contract("Run", listed);
    assert_plan_is_null(listed, "GET /v1/runs");

    // The seeded run_demo has no plan either.
    let (status, demo) = get(&server, "/v1/runs/run_demo", "usr_maya").await;
    assert_eq!(status, StatusCode::OK, "{demo}");
    assert_contract("RunDetail", &demo);
    assert_plan_is_null(&demo, "run_demo");
}

#[sqlx::test(migrations = "../migrations")]
async fn the_plan_is_stored_and_served_in_every_run_response_and_event(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let mut maya = server.connect_with_token("usr_maya").await;
    let mut judge = server.connect_with_token("usr_judge").await;
    let mut leo = server.connect_with_token("usr_leo").await;

    let returning_at = minutes_from_now(&pool, 35).await;
    let run = open(
        &server,
        json!({"returning_at": returning_at, "return_place": hinton(), "dropoff_radius_m": 400}),
    )
    .await;
    let id = run["id"].as_str().unwrap().to_owned();
    let expected_place = json!({
        "id": "place_hinton",
        "label": "Hinton James lobby",
        "location": {"lat": 35.902364, "lng": -79.043231},
        "note": "Main lobby."
    });
    let check = |run: &Value, what: &str| {
        assert_eq!(run["id"], id.as_str(), "{what}");
        assert_eq!(run["returning_at"], returning_at.as_str(), "{what}");
        assert_eq!(run["return_place"], expected_place, "{what}");
        assert_eq!(run["dropoff_radius_m"], 400, "{what}");
    };
    check(&run, "POST /v1/runs");
    assert_eq!(
        stored_plan(&pool, &id).await,
        json!({
            "returning_at": stored_time(&pool, &returning_at).await,
            "place_id": "place_hinton", "label": "Hinton James lobby",
            "lat": 35.902364, "lng": -79.043231, "note": "Main lobby.", "radius": 400
        })
    );

    // run.updated carries the plan to the runner and to each member.
    let events = expect_events(&mut maya, "maya", &["run.updated"]).await;
    check(&events[0]["data"]["run"], "maya's run.updated");
    for (who, ws) in [("judge", &mut judge), ("leo", &mut leo)] {
        let events = expect_events(ws, who, &["notification.created", "run.updated"]).await;
        check(&events[1]["data"]["run"], who);
    }

    // RunDetail, the runner's and a member's, and both lists.
    for viewer in ["usr_maya", "usr_judge"] {
        let (status, detail) = get(&server, &format!("/v1/runs/{id}"), viewer).await;
        assert_eq!(status, StatusCode::OK, "{detail}");
        assert_contract("RunDetail", &detail);
        check(&detail, viewer);
    }
    for (uri, viewer) in [("/v1/runs", "usr_leo"), ("/v1/runs?mine=true", "usr_maya")] {
        let (status, list) = get(&server, uri, viewer).await;
        assert_eq!(status, StatusCode::OK, "{list}");
        let listed = list
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id.as_str())
            .unwrap_or_else(|| panic!("{uri}: the run is listed"));
        assert_contract("Run", listed);
        check(listed, uri);
    }

    // A step that publishes run.updated again keeps the plan: the runner closes the window.
    let (status, closed) = post(&server, &format!("/v1/runs/{id}/close"), "usr_maya", None).await;
    assert_eq!(status, StatusCode::OK, "{closed}");
    assert_contract("Run", &closed);
    check(&closed, "close");
    let events = expect_events(&mut leo, "leo", &["run.updated"]).await;
    assert_eq!(events[0]["data"]["run"]["status"], "closed");
    check(&events[0]["data"]["run"], "leo after the close");
}

/// `time` as Postgres stores and `json_build_object` prints a timestamptz.
async fn stored_time(pool: &PgPool, time: &str) -> Value {
    sqlx::query_scalar("SELECT to_json($1::timestamptz)")
        .bind(time)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../migrations")]
async fn the_profile_walk_distance_is_the_default_radius(pool: PgPool) {
    let server = demo_server(pool.clone()).await;

    // Without a profile walk distance, no radius.
    let run = open(&server, json!({"return_place": hinton()})).await;
    assert_eq!(run["dropoff_radius_m"], Value::Null);

    // With one, a plan without a radius (left out or null) takes it; an explicit radius wins.
    set_walk_distance(&pool, "usr_maya", Some(600)).await;
    let run = open(&server, json!({"return_place": hinton()})).await;
    assert_eq!(run["dropoff_radius_m"], 600);
    let run = open(
        &server,
        json!({"return_place": hinton(), "dropoff_radius_m": null}),
    )
    .await;
    assert_eq!(run["dropoff_radius_m"], 600);
    let run = open(&server, json!({"dropoff_radius_m": 250})).await;
    assert_eq!(run["dropoff_radius_m"], 250);
    // No plan at all still takes the runner's distance.
    let run = open(&server, json!({})).await;
    assert_eq!(run["dropoff_radius_m"], 600);
    assert_eq!(run["return_place"], Value::Null);
    assert_eq!(run["returning_at"], Value::Null);

    // The agent's open_run keeps its signature and opens with no plan, the same default.
    let request = CreateRunRequest {
        restaurant_id: "rst_cosmic".into(),
        circle_id: "cir_hinton4".into(),
        leaving_at: minutes_from_now(&pool, 5).await,
        window_minutes: 20,
    };
    let run = runs::open_run(&server.state, "usr_maya", &request, CreatedVia::Message)
        .await
        .unwrap();
    let run = serde_json::to_value(run).unwrap();
    assert_contract("Run", &run);
    assert_eq!(run["created_via"], "message");
    assert_eq!(run["dropoff_radius_m"], 600);
    assert_eq!(run["return_place"], Value::Null);
    assert_eq!(run["returning_at"], Value::Null);

    // The default is read when the run opens; a later change leaves the run alone.
    let id = run["id"].as_str().unwrap();
    set_walk_distance(&pool, "usr_maya", Some(900)).await;
    let (_, detail) = get(&server, &format!("/v1/runs/{id}"), "usr_maya").await;
    assert_eq!(detail["dropoff_radius_m"], 600);
}

#[sqlx::test(migrations = "../migrations")]
async fn every_plan_problem_is_validation_failed_and_stores_nothing(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let leaving = minutes_from_now(&pool, 5).await;
    let at_leaving = leaving.clone();
    let before_leaving = minutes_from_now(&pool, 4).await;
    let too_late = minutes_from_now(&pool, 5 + 24 * 60 + 1).await;
    let just_in_time = minutes_from_now(&pool, 5 + 24 * 60 - 1).await;
    let with = |change: &dyn Fn(&mut Value)| {
        let mut place = hinton();
        change(&mut place);
        json!({ "return_place": place })
    };
    let cases: Vec<(&str, Value, Option<&str>)> = vec![
        (
            "returning_at not a date-time",
            json!({"returning_at": "7pm"}),
            Some("returning_at"),
        ),
        (
            "returning_at without a zone",
            json!({"returning_at": "2026-10-10T22:40:00"}),
            Some("returning_at"),
        ),
        (
            "returning_at an impossible date",
            json!({"returning_at": "2026-02-30T22:40:00Z"}),
            Some("returning_at"),
        ),
        (
            "returning_at at leaving_at",
            json!({"returning_at": at_leaving}),
            Some("returning_at"),
        ),
        (
            "returning_at before leaving_at",
            json!({"returning_at": before_leaving}),
            Some("returning_at"),
        ),
        (
            "returning_at over 24 h after leaving_at",
            json!({"returning_at": too_late}),
            Some("returning_at"),
        ),
        (
            "returning_at a number",
            json!({"returning_at": 1_700_000_000}),
            None,
        ),
        (
            "return_place with an empty label",
            with(&|p| p["label"] = json!("   ")),
            Some("return_place.label"),
        ),
        (
            "return_place with a long label",
            with(&|p| p["label"] = json!("x".repeat(101))),
            Some("return_place.label"),
        ),
        (
            "return_place with a control character",
            with(&|p| p["label"] = json!("Hinton\u{7}")),
            Some("return_place.label"),
        ),
        (
            "return_place with lat 91",
            with(&|p| p["location"]["lat"] = json!(91.0)),
            Some("return_place.location"),
        ),
        (
            "return_place with lng -181",
            with(&|p| p["location"]["lng"] = json!(-181.0)),
            Some("return_place.location"),
        ),
        (
            "return_place with a long note",
            with(&|p| p["note"] = json!("n".repeat(281))),
            Some("return_place.note"),
        ),
        (
            "return_place with an unknown id",
            with(&|p| p["id"] = json!("place_nowhere")),
            Some("return_place.id"),
        ),
        (
            "return_place without a location",
            with(&|p| {
                p.as_object_mut().unwrap().remove("location");
            }),
            None,
        ),
        (
            "return_place a string",
            json!({"return_place": "Hinton"}),
            None,
        ),
        (
            "dropoff_radius_m 49",
            json!({"dropoff_radius_m": 49}),
            Some("dropoff_radius_m"),
        ),
        (
            "dropoff_radius_m 5001",
            json!({"dropoff_radius_m": 5001}),
            Some("dropoff_radius_m"),
        ),
        (
            "dropoff_radius_m 0",
            json!({"dropoff_radius_m": 0}),
            Some("dropoff_radius_m"),
        ),
        (
            "dropoff_radius_m negative",
            json!({"dropoff_radius_m": -400}),
            Some("dropoff_radius_m"),
        ),
        (
            "dropoff_radius_m huge",
            json!({"dropoff_radius_m": i64::MAX}),
            Some("dropoff_radius_m"),
        ),
        (
            "dropoff_radius_m a fraction",
            json!({"dropoff_radius_m": 400.5}),
            None,
        ),
        (
            "dropoff_radius_m a string",
            json!({"dropoff_radius_m": "400"}),
            None,
        ),
    ];
    let before = run_count(&pool).await;
    for (what, plan, field) in cases {
        let mut request = body(&server, plan).await;
        request["leaving_at"] = json!(leaving);
        let (status, answer) = post(&server, "/v1/runs", "usr_maya", Some(request)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{what}: {answer}");
        common::assert_envelope(&answer, "validation_failed");
        assert_contract("Error", &answer);
        if let Some(field) = field {
            let fields: Vec<&str> = answer["error"]["details"]
                .as_array()
                .unwrap_or_else(|| panic!("{what}: details list the fields: {answer}"))
                .iter()
                .map(|e| e["field"].as_str().unwrap())
                .collect();
            assert!(fields.contains(&field), "{what}: {field} not in {fields:?}");
        }
    }
    assert_eq!(run_count(&pool).await, before, "no rejected run is stored");

    // A wrong type names its field in the message, the four fields of v1.3 and the plan's alike
    // (contract-v1-4 review MINOR 1: two flattened structs had lost the name).
    let place_without_location = {
        let mut place = hinton();
        place.as_object_mut().unwrap().remove("location");
        place
    };
    for (field, value) in [
        ("restaurant_id", json!(7)),
        ("circle_id", json!(["cir_hinton4"])),
        ("leaving_at", json!(1_700_000_000)),
        ("window_minutes", json!("x")),
        ("window_minutes", json!(30.5)),
        ("returning_at", json!(1_700_000_000)),
        ("return_place", json!("Hinton")),
        ("return_place", place_without_location),
        ("dropoff_radius_m", json!("400")),
        ("dropoff_radius_m", json!(400.5)),
    ] {
        let mut request = body(&server, json!({})).await;
        request["leaving_at"] = json!(leaving);
        request[field] = value.clone();
        let (status, answer) = post(&server, "/v1/runs", "usr_maya", Some(request)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{field} {value}: {answer}");
        common::assert_envelope(&answer, "validation_failed");
        assert_contract("Error", &answer);
        let message = answer["error"]["message"].as_str().unwrap();
        assert!(
            message.contains(&format!(": {field}: ")),
            "{field} {value}: the message names the field: {message}"
        );
    }
    assert_eq!(run_count(&pool).await, before, "no rejected run is stored");

    // Several problems at once are all listed.
    let mut request = body(
        &server,
        json!({"returning_at": "soon", "dropoff_radius_m": 10, "return_place": {"label": "", "location": {"lat": 99.0, "lng": 0.0}}}),
    )
    .await;
    request["leaving_at"] = json!(leaving);
    let (status, answer) = post(&server, "/v1/runs", "usr_maya", Some(request)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    let fields: Vec<&str> = answer["error"]["details"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["field"].as_str().unwrap())
        .collect();
    assert_eq!(
        fields,
        [
            "returning_at",
            "return_place.label",
            "return_place.location",
            "dropoff_radius_m"
        ]
    );

    // The edges pass: 50 m, 5000 m, back just under 24 hours after leaving, a map pin.
    for plan in [
        json!({"dropoff_radius_m": 50}),
        json!({"dropoff_radius_m": 5000}),
        json!({"returning_at": just_in_time}),
        json!({"return_place": {"label": "The Pit", "location": {"lat": 35.910, "lng": -79.048}}}),
    ] {
        let mut request = body(&server, plan.clone()).await;
        request["leaving_at"] = json!(leaving);
        let (status, run) = post(&server, "/v1/runs", "usr_maya", Some(request)).await;
        assert_eq!(status, StatusCode::OK, "{plan}: {run}");
        assert_contract("Run", &run);
    }
    let (_, pin) = get(&server, "/v1/runs?mine=true", "usr_maya").await;
    assert_eq!(
        pin[0]["return_place"],
        json!({"label": "The Pit", "location": {"lat": 35.910, "lng": -79.048}})
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn the_public_preview_keeps_its_four_fields(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let returning_at = minutes_from_now(&pool, 40).await;
    let run = open(
        &server,
        json!({"returning_at": returning_at, "return_place": hinton(), "dropoff_radius_m": 300}),
    )
    .await;
    let id = run["id"].as_str().unwrap();
    // No sign-in.
    let (status, preview) = call(
        &server,
        Method::GET,
        &format!("/v1/public/runs/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_contract("RunPreview", &preview);
    let mut keys: Vec<&str> = preview
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "restaurant_name",
            "runner_display_name",
            "status",
            "window_closes_at"
        ]
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn orders_store_and_echo_their_dropoff_radius(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let mut judge = server.connect_with_token("usr_judge").await;
    let mut maya = server.connect_with_token("usr_maya").await;

    // A radius is stored and echoed in the order, its event, its reads and the runner's detail.
    let mut request = burrito();
    request["dropoff_radius_m"] = json!(150);
    let (status, order) = post(
        &server,
        "/v1/runs/run_demo/orders",
        "usr_judge",
        Some(request),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{order}");
    assert_contract("Order", &order);
    assert_eq!(order["dropoff_radius_m"], 150);
    let id = order["id"].as_str().unwrap().to_owned();
    let stored: Option<i32> =
        sqlx::query_scalar("SELECT dropoff_radius_m FROM ferry.orders WHERE id = $1")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, Some(150));
    let events = expect_events(
        &mut judge,
        "judge",
        &[
            "notification.created",
            "run.updated",
            "order.updated",
            "wallet.updated",
        ],
    )
    .await;
    assert_eq!(events[2]["data"]["order"]["dropoff_radius_m"], 150);
    let events = expect_events(&mut maya, "maya", &["run.updated", "order.updated"]).await;
    assert_eq!(events[1]["data"]["order"]["dropoff_radius_m"], 150);

    let (status, read) = get(&server, &format!("/v1/orders/{id}"), "usr_judge").await;
    assert_eq!(status, StatusCode::OK, "{read}");
    assert_contract("Order", &read);
    assert_eq!(read["dropoff_radius_m"], 150);
    let (status, list) = get(&server, "/v1/orders", "usr_judge").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_contract("Order", &list[0]);
    assert_eq!(list[0]["dropoff_radius_m"], 150);
    let (status, detail) = get(&server, "/v1/runs/run_demo", "usr_maya").await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_contract("RunDetail", &detail);
    assert_eq!(detail["orders"][0]["dropoff_radius_m"], 150);

    // Without one, or with null, the order's radius is null.
    for extra in [None, Some(Value::Null)] {
        let mut request = burrito();
        if let Some(extra) = extra {
            request["dropoff_radius_m"] = extra;
        }
        let (status, order) = post(
            &server,
            "/v1/runs/run_demo/orders",
            "usr_dana",
            Some(request),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{order}");
        assert_contract("Order", &order);
        assert_eq!(order.get("dropoff_radius_m"), Some(&Value::Null));
    }

    // Out of range is 400 on placing and quoting, and stores nothing.
    let orders_before: i64 = sqlx::query_scalar("SELECT count(*) FROM ferry.orders")
        .fetch_one(&pool)
        .await
        .unwrap();
    for radius in [json!(49), json!(5001), json!(-1)] {
        let mut request = burrito();
        request["dropoff_radius_m"] = radius.clone();
        for uri in ["/v1/runs/run_demo/orders", "/v1/runs/run_demo/quote"] {
            let (status, answer) = post(&server, uri, "usr_leo", Some(request.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} {radius}: {answer}");
            common::assert_envelope(&answer, "validation_failed");
            assert_eq!(answer["error"]["details"][0]["field"], "dropoff_radius_m");
        }
    }
    let mut request = burrito();
    request["dropoff_radius_m"] = json!(120.5);
    let (status, answer) = post(
        &server,
        "/v1/runs/run_demo/orders",
        "usr_leo",
        Some(request),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    common::assert_envelope(&answer, "validation_failed");
    let orders_after: i64 = sqlx::query_scalar("SELECT count(*) FROM ferry.orders")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(orders_after, orders_before);

    // No zone rule: a drop-off well outside the runner's zone is accepted (the client warns).
    let returning_at = minutes_from_now(&pool, 35).await;
    let run = open(
        &server,
        json!({"returning_at": returning_at, "return_place": hinton(), "dropoff_radius_m": 100}),
    )
    .await;
    let mut far = burrito();
    far["dropoff"] = fetzer(); // about 820 m from Hinton James
    far["dropoff_radius_m"] = json!(50);
    let uri = format!("/v1/runs/{}/orders", run["id"].as_str().unwrap());
    let (status, order) = post(&server, &uri, "usr_judge", Some(far)).await;
    assert_eq!(status, StatusCode::CREATED, "{order}");
    assert_contract("Order", &order);
    assert_eq!(order["dropoff_radius_m"], 50);
}

/// The migration's CHECKs refuse what the API refuses, should anything else write the tables.
#[sqlx::test(migrations = "../migrations")]
async fn the_migration_checks_refuse_bad_rows(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let order = {
        let (status, order) = post(
            &server,
            "/v1/runs/run_demo/orders",
            "usr_judge",
            Some(burrito()),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{order}");
        order["id"].as_str().unwrap().to_owned()
    };
    let bad = [
        "UPDATE ferry.runs SET dropoff_radius_m = 49 WHERE id = 'run_demo'",
        "UPDATE ferry.runs SET dropoff_radius_m = 5001 WHERE id = 'run_demo'",
        "UPDATE ferry.runs SET returning_at = leaving_at WHERE id = 'run_demo'",
        "UPDATE ferry.runs SET returning_at = leaving_at - interval '1 minute' WHERE id = 'run_demo'",
        "UPDATE ferry.runs SET return_label = 'Hinton' WHERE id = 'run_demo'",
        "UPDATE ferry.runs SET return_label = 'X', return_lat = 91, return_lng = 0 WHERE id = 'run_demo'",
        "UPDATE ferry.runs SET return_label = 'X', return_lat = 0, return_lng = 181 WHERE id = 'run_demo'",
        "UPDATE ferry.runs SET return_note = 'lobby' WHERE id = 'run_demo'",
        "INSERT INTO ferry.profiles (user_id, max_dropoff_distance_m) VALUES ('usr_maya', 10) \
         ON CONFLICT (user_id) DO UPDATE SET max_dropoff_distance_m = EXCLUDED.max_dropoff_distance_m",
        "INSERT INTO ferry.profiles (user_id, max_dropoff_distance_m) VALUES ('usr_maya', 5001) \
         ON CONFLICT (user_id) DO UPDATE SET max_dropoff_distance_m = EXCLUDED.max_dropoff_distance_m",
        "UPDATE ferry.orders SET dropoff_radius_m = 0 WHERE id = $1",
        "INSERT INTO ferry.pickup_requests (id, customer_id, restaurant_id, lines, tip_cents, \
             dropoff_label, dropoff_lat, dropoff_lng, needed_by, dropoff_radius_m) \
         VALUES ('req_bad', 'usr_judge', 'rst_cosmic', '[]', 0, 'Fetzer', 35.9, -79.0, now(), 6000)",
    ];
    // Only the statements that name $1 take the order's id.
    let run = |sql: &'static str| {
        let query = sqlx::query(sql);
        let query = if sql.contains("$1") {
            query.bind(order.clone())
        } else {
            query
        };
        query.execute(&pool)
    };
    for sql in bad {
        let err = run(sql).await.expect_err(sql);
        let code = err
            .as_database_error()
            .and_then(|e| e.code().map(|c| c.into_owned()));
        assert_eq!(code.as_deref(), Some("23514"), "{sql}: {err}");
    }
    let good = [
        "UPDATE ferry.runs SET dropoff_radius_m = 50, returning_at = leaving_at + interval '1 hour', \
             return_place_id = 'place_hinton', return_label = 'Hinton', return_lat = 35.9, \
             return_lng = -79.04, return_note = 'lobby' WHERE id = 'run_demo'",
        "UPDATE ferry.orders SET dropoff_radius_m = 5000 WHERE id = $1",
        "INSERT INTO ferry.pickup_requests (id, customer_id, restaurant_id, lines, tip_cents, \
             dropoff_label, dropoff_lat, dropoff_lng, needed_by, dropoff_radius_m) \
         VALUES ('req_ok', 'usr_judge', 'rst_cosmic', '[]', 0, 'Fetzer', 35.9, -79.0, now(), 100)",
    ];
    for sql in good {
        run(sql).await.expect(sql);
    }
    // Deleting a place a run returns to keeps the run and clears the place id.
    sqlx::query("DELETE FROM ferry.places WHERE id = 'place_hinton'")
        .execute(&pool)
        .await
        .unwrap();
    let (_, demo) = get(&server, "/v1/runs/run_demo", "usr_maya").await;
    assert_contract("RunDetail", &demo);
    assert_eq!(
        demo["return_place"],
        json!({"label": "Hinton", "location": {"lat": 35.9, "lng": -79.04}, "note": "lobby"})
    );
}
