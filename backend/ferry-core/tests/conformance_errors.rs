//! Errors and input validation (conformance task), against the local Postgres:
//! - every SQLSTATE in `error::BAD_INPUT_SQLSTATES` that real SQL can raise maps to 400
//!   `validation_failed` with a generic message, and other database errors stay 500;
//! - a CHECK violation is 400 only on a CHECK request input can reach (`error::INPUT_CHECKS`); the
//!   ledger's and orders' CHECKs stay 500, directly and through an endpoint;
//! - the NUL cases the reviews found (ids in paths, query parameters and bodies), out-of-range
//!   date-times and the `receipt_path` CHECK are 400 in the envelope.
//!
//! One log line per 500 is `conformance_logs.rs`, alone in its binary.

mod common;
mod realtime_support;
mod runs_support;

use axum::http::{Method, StatusCode};
use ferry_core::error::{
    ApiError, BAD_INPUT_SQLSTATES, CHECK_VIOLATION, ErrorCode, INPUT_CHECKS, bad_input,
    bad_input_sqlstate,
};
use serde_json::{Value, json};
use sqlx::PgPool;

use realtime_support::assert_contract;
use runs_support::{burrito, call, demo_server, minutes_from_now, post};

/// Runs `sql` on `conn` and returns its error, which must carry `sqlstate`.
async fn db_error(
    conn: &mut sqlx::PgConnection,
    sql: &'static str,
    bind: Option<&str>,
    sqlstate: &str,
) -> sqlx::Error {
    let query = sqlx::query(sql);
    let query = match bind {
        Some(value) => query.bind(value.to_owned()),
        None => query,
    };
    let err = query
        .execute(&mut *conn)
        .await
        .expect_err("the statement should fail");
    match &err {
        sqlx::Error::Database(db) => {
            assert_eq!(db.code().as_deref(), Some(sqlstate), "{sql}: {db}")
        }
        other => panic!("{sql}: {other}"),
    }
    err
}

#[sqlx::test(migrations = "../migrations")]
async fn bad_input_sqlstates_are_validation_failed(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    for setup in [
        "CREATE TEMP TABLE t_short (v varchar(2))",
        "CREATE TEMP TABLE t_check (v int CHECK (v > 0))",
        "CREATE TEMP TABLE t_indexed (v text)",
        "CREATE INDEX ON t_indexed (v)",
    ] {
        sqlx::query(setup).execute(&mut *conn).await.unwrap();
    }
    let cases: [(&str, &'static str, Option<&str>); 13] = [
        ("22001", "INSERT INTO t_short VALUES ('abc')", None),
        ("22003", "SELECT 2147483647::int4 + 1", None),
        ("22007", "SELECT 'nope'::timestamptz", None),
        ("22008", "SELECT '2026-02-30T00:00:00Z'::timestamptz", None),
        (
            "22009",
            "SELECT '2026-10-10T22:04:12+16:00'::timestamptz",
            None,
        ),
        ("22015", "SELECT interval '2147483648 days'", None),
        ("22021", "SELECT $1::text", Some("run_\0x")),
        ("22025", "SELECT 'a' LIKE 'a' ESCAPE 'ab'", None),
        ("22026", "SELECT B'101' & B'1'", None),
        ("22P02", "SELECT 'abc'::int", None),
        ("22P05", r#"SELECT '"\u0000"'::jsonb"#, None),
        ("22P05", "SELECT $1::jsonb", Some(r#"{"x": "\u0000"}"#)),
        (
            "54000",
            "INSERT INTO t_indexed SELECT string_agg(md5(i::text), '') \
             FROM generate_series(1, 2000) i",
            None,
        ),
    ];
    let mut seen = Vec::new();
    for (sqlstate, sql, bind) in cases {
        let err = db_error(&mut conn, sql, bind, sqlstate).await;
        let api: ApiError = err.into();
        assert_eq!(api.status, StatusCode::BAD_REQUEST, "{sqlstate}");
        assert_eq!(api.code, ErrorCode::ValidationFailed, "{sqlstate}");
        // Postgres's message can quote the value; the client never gets it.
        assert_eq!(
            api.message, "A value in the request is invalid.",
            "{sqlstate}"
        );
        seen.push(sqlstate);
    }
    // Every listed code is covered here, except 22P03 (a malformed binary parameter, which sqlx
    // never sends).
    for (code, _) in BAD_INPUT_SQLSTATES {
        assert!(
            seen.contains(&code) || code == "22P03",
            "{code} has no case"
        );
        assert!(bad_input_sqlstate(code).is_some());
    }

    // Anything else from the database is still a bug: 500, with the cause hidden. That includes a
    // CHECK no request value can reach (`t_check_v_check` is not in INPUT_CHECKS).
    for (sqlstate, sql) in [
        ("42P01", "SELECT * FROM ferry.no_such_table"),
        ("42601", "SELECT FROM WHERE"),
        ("2201B", "SELECT 'a' ~ '('"),
        ("23514", "INSERT INTO t_check VALUES (-1)"),
    ] {
        let api: ApiError = db_error(&mut conn, sql, None, sqlstate).await.into();
        assert_eq!(api.status, StatusCode::INTERNAL_SERVER_ERROR, "{sqlstate}");
        assert_eq!(api.code, ErrorCode::UpstreamUnavailable);
        assert!(!api.message.contains("no_such_table"));
    }
}

fn assert_validation_failed(what: &str, status: StatusCode, body: &Value) {
    assert_eq!(status, StatusCode::BAD_REQUEST, "{what}: {body}");
    common::assert_envelope(body, "validation_failed");
    assert_contract("Error", body);
}

/// The 500s the reviews reproduced (runs-orders MINOR 5, impact MINOR 4, realtime MINOR 1,
/// tracking MINOR 2, circles-invites MINOR 1), each now 400 in the envelope.
#[sqlx::test(migrations = "../migrations")]
async fn the_reviews_500s_are_validation_failed(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    for (method, uri, user) in [
        (Method::GET, "/v1/runs/run_%00x", "usr_judge"),
        (Method::GET, "/v1/runs/run_%00x/track", "usr_judge"),
        (Method::POST, "/v1/runs/run_%00x/close", "usr_maya"),
        (Method::POST, "/v1/runs/run_%00x/cancel", "usr_maya"),
        (Method::GET, "/v1/orders/ord_%00x", "usr_judge"),
        (Method::POST, "/v1/orders/ord_%00x/cancel", "usr_judge"),
        (Method::GET, "/v1/public/runs/run_%00x", ""),
        (Method::GET, "/v1/circles/%00x", "usr_judge"),
        (Method::GET, "/v1/circles/cir_%00x", "usr_judge"),
        (Method::GET, "/v1/restaurants/rst_%00x/menu", "usr_judge"),
        (Method::POST, "/v1/notifications/%00/read", "usr_judge"),
        (Method::POST, "/v1/invites/inv_%00/respond", "usr_judge"),
        (Method::GET, "/v1/auth/telegram/chl_%00", ""),
        (
            Method::GET,
            "/v1/impact?scope=circle&circle_id=%00x",
            "usr_judge",
        ),
        (Method::GET, "/v1/restaurants?q=%00", "usr_judge"),
        (Method::GET, "/v1/restaurants?area_id=area_%00", "usr_judge"),
    ] {
        let token = (!user.is_empty()).then(|| server.token(user));
        let body =
            (method == Method::POST && uri.ends_with("/respond")).then(|| json!({"accept": true}));
        let (status, answer) = call(&server, method.clone(), uri, token.as_deref(), body).await;
        assert_validation_failed(&format!("{method} {uri}"), status, &answer);
        assert!(
            answer["error"]["details"]["parameter"].is_string(),
            "{answer}"
        );
    }

    // Ids in bodies.
    let mut nul_item = burrito();
    nul_item["lines"][0]["menu_item_id"] = json!("itm_\u{0}");
    let mut nul_place = burrito();
    nul_place["dropoff"]["id"] = json!("place_\u{0}");
    let leaving = minutes_from_now(&pool, 5).await;
    let at = minutes_from_now(&pool, 0).await;
    let soon = minutes_from_now(&pool, 60).await;
    let offset = |time: &str, to: &str| time.replace('Z', to);
    let (_, me) = server.get_as("/v1/me", "usr_judge").await;
    let mut profile = json!({"display_name": me["display_name"], "profile": me["profile"]});
    profile["profile"]["dropoff_default"] = json!({
        "id": "place_\u{0}", "label": "Fetzer", "location": {"lat": 35.9, "lng": -79.0}
    });
    let cases = [
        ("/v1/runs/run_demo/orders", "usr_judge", nul_item.clone()),
        ("/v1/runs/run_demo/quote", "usr_judge", nul_item),
        ("/v1/runs/run_demo/orders", "usr_judge", nul_place),
        (
            "/v1/runs",
            "usr_maya",
            json!({"restaurant_id": "rst_cosmic", "circle_id": "cir_\u{0}",
                   "leaving_at": leaving, "window_minutes": 30}),
        ),
        (
            "/v1/runs",
            "usr_maya",
            json!({"restaurant_id": "rst_\u{0}", "circle_id": "cir_hinton4",
                   "leaving_at": leaving, "window_minutes": 30}),
        ),
        // Out-of-range offsets (SQLSTATE 22009).
        (
            "/v1/runs",
            "usr_maya",
            json!({"restaurant_id": "rst_cosmic", "circle_id": "cir_hinton4",
                   "leaving_at": offset(&leaving, "+16:00"), "window_minutes": 30}),
        ),
        (
            "/v1/runs/run_demo/location",
            "usr_maya",
            json!({"location": {"lat": 35.91, "lng": -79.05}, "accuracy_m": 5,
                   "at": offset(&at, "+99:00")}),
        ),
        (
            "/v1/circles/cir_hinton4/meet-again",
            "usr_dana",
            json!({"proposed_time": offset(&soon, "+16:00")}),
        ),
        (
            "/v1/circles/cir_hinton4/meet-again",
            "usr_dana",
            json!({"message": "dinner\u{0}"}),
        ),
        ("/v1/me/profile", "usr_judge", profile),
    ];
    for (uri, user, body) in cases {
        let method = if uri == "/v1/me/profile" {
            Method::PUT
        } else {
            Method::POST
        };
        let (status, answer) = call(
            &server,
            method,
            uri,
            Some(&server.token(user)),
            Some(body.clone()),
        )
        .await;
        assert_validation_failed(&format!("{uri} {body}"), status, &answer);
    }

    // Nothing was stored by any of them.
    let orders: i64 = sqlx::query_scalar("SELECT count(*) FROM ferry.orders")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(orders, 0);
}

/// The `receipt_path` CHECK (no `..` anywhere) is stricter than the handler's own check (no `..`
/// segment), so `run_x/receipt..jpg` reached Postgres and was 500 (23514). It is 400 now, and the
/// purchase's transaction, ledger rows included, rolls back.
#[sqlx::test(migrations = "../migrations")]
async fn a_receipt_path_the_check_refuses_is_validation_failed(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let run = runs_support::open_run(&server, "usr_maya", "cir_hinton4").await;
    let (status, order) = post(
        &server,
        &format!("/v1/runs/{run}/orders"),
        "usr_judge",
        Some(burrito()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{order}");
    let (status, closed) = post(&server, &format!("/v1/runs/{run}/close"), "usr_maya", None).await;
    assert_eq!(status, StatusCode::OK, "{closed}");
    let ledger_before: i64 = sqlx::query_scalar("SELECT count(*) FROM ferry.ledger_entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    let (status, answer) = post(
        &server,
        &format!("/v1/runs/{run}/purchased"),
        "usr_maya",
        Some(json!({"receipt_total_cents": 966, "receipt_path": format!("{run}/receipt..jpg")})),
    )
    .await;
    assert_validation_failed("receipt..jpg", status, &answer);
    let ledger_after: i64 = sqlx::query_scalar("SELECT count(*) FROM ferry.ledger_entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(ledger_after, ledger_before);
    assert_eq!(runs_support::run_status(&pool, &run).await, "closed");
}

/// Runs `sql` with `id` bound as `$1`; it must break the CHECK `constraint` on `table`. Returns
/// the error as the API maps it.
async fn check_violation(
    pool: &PgPool,
    sql: &'static str,
    id: &str,
    table: &str,
    constraint: &str,
) -> ApiError {
    let err = sqlx::query(sql)
        .bind(id)
        .execute(pool)
        .await
        .expect_err("the statement should break a CHECK");
    match &err {
        sqlx::Error::Database(db) => {
            assert_eq!(db.code().as_deref(), Some(CHECK_VIOLATION), "{sql}: {db}");
            assert_eq!(db.table(), Some(table), "{sql}");
            assert_eq!(db.constraint(), Some(constraint), "{sql}");
        }
        other => panic!("{sql}: {other}"),
    }
    err.into()
}

/// A CHECK violation (23514) is 400 only on a CHECK request input can reach: `runs_check`
/// (`receipt_path`). The ledger's and orders' CHECKs guard invariants ferry-core keeps itself (the
/// amounts, the accounts, an order's and a line's totals, statuses), so breaking one is a bug in
/// ferry-core: 500 with the cause hidden, never a 400 that blames the client (conformance review,
/// MAJOR 2).
#[sqlx::test(migrations = "../migrations")]
async fn only_checks_request_input_can_reach_are_validation_failed(pool: PgPool) {
    assert_eq!(INPUT_CHECKS, [("runs", "runs_check")]);
    assert_eq!(
        bad_input(CHECK_VIOLATION, Some("runs"), Some("runs_check")),
        Some("check_violation")
    );
    for (table, constraint) in [
        (None, None),
        (Some("orders"), Some("runs_check")),
        (Some("runs"), Some("runs_status_check")),
        (
            Some("ledger_entries"),
            Some("ledger_entries_amount_cents_check"),
        ),
    ] {
        assert_eq!(
            bad_input(CHECK_VIOLATION, table, constraint),
            None,
            "{constraint:?}"
        );
    }

    let server = demo_server(pool.clone()).await;
    let run = runs_support::open_run(&server, "usr_maya", "cir_hinton4").await;
    let (status, order) = post(
        &server,
        &format!("/v1/runs/{run}/orders"),
        "usr_judge",
        Some(burrito()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{order}");
    let order = order["id"].as_str().unwrap().to_owned();

    // Input reaches it: 400.
    let api = check_violation(
        &pool,
        "UPDATE ferry.runs SET receipt_path = $1 || '/receipt..jpg' WHERE id = $1",
        &run,
        "runs",
        "runs_check",
    )
    .await;
    assert_eq!(api.status, StatusCode::BAD_REQUEST);
    assert_eq!(api.code, ErrorCode::ValidationFailed);
    assert_eq!(api.message, "A value in the request is invalid.");

    // Only a bug reaches these: 500.
    for (sql, table, constraint) in [
        (
            "UPDATE ferry.ledger_entries SET amount_cents = 0 WHERE order_id = $1",
            "ledger_entries",
            "ledger_entries_amount_cents_check",
        ),
        (
            "UPDATE ferry.ledger_entries SET credit_account = debit_account WHERE order_id = $1",
            "ledger_entries",
            "ledger_entries_check",
        ),
        (
            "UPDATE ferry.ledger_entries SET kind = 'gift' WHERE order_id = $1",
            "ledger_entries",
            "ledger_entries_kind_check",
        ),
        (
            "UPDATE ferry.orders SET tip_cents = tip_cents + 1 WHERE id = $1",
            "orders",
            "orders_check",
        ),
        (
            "UPDATE ferry.orders SET status = 'lost' WHERE id = $1",
            "orders",
            "orders_status_check",
        ),
        (
            "UPDATE ferry.order_lines SET line_total_cents = line_total_cents + 1 WHERE order_id = $1",
            "order_lines",
            "order_lines_check",
        ),
    ] {
        let api = check_violation(&pool, sql, &order, table, constraint).await;
        assert_eq!(
            api.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{constraint}"
        );
        assert_eq!(api.code, ErrorCode::UpstreamUnavailable, "{constraint}");
        assert!(!api.message.contains(constraint), "{}", api.message);
    }

    // Through an endpoint: the ledger's amount CHECK, tightened under its own name so that the
    // order's hold breaks it, as a bug in the money code would. Placing an order is 500 in the
    // envelope, and its transaction rolls back: no order, no ledger row.
    sqlx::query(
        "ALTER TABLE ferry.ledger_entries \
         DROP CONSTRAINT ledger_entries_amount_cents_check, \
         ADD CONSTRAINT ledger_entries_amount_cents_check \
             CHECK (amount_cents > 0 AND kind <> 'order_hold') NOT VALID",
    )
    .execute(&pool)
    .await
    .unwrap();
    let counts = || async {
        sqlx::query_as::<_, (i64, i64)>(
            "SELECT (SELECT count(*) FROM ferry.orders), (SELECT count(*) FROM ferry.ledger_entries)",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let before = counts().await;
    let run = runs_support::open_run(&server, "usr_maya", "cir_hinton4").await;
    let (status, answer) = post(
        &server,
        &format!("/v1/runs/{run}/orders"),
        "usr_judge",
        Some(burrito()),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{answer}");
    common::assert_envelope(&answer, "upstream_unavailable");
    assert_contract("Error", &answer);
    assert!(!answer.to_string().contains("ledger"), "{answer}");
    assert_eq!(counts().await, before, "(orders, ledger rows)");
}
