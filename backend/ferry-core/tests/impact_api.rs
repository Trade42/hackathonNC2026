//! GET /v1/impact, GET /v1/public/impact, and the demo history `ferry-core seed` adds.
mod auth_support;
mod common;

use auth_support::{app_with, assert_contract, assert_error, call, get_with_token};
use axum::Router;
use axum::http::StatusCode;
use ferry_core::demo::{self, Command};
use ferry_core::impact::{self, ImpactScope, METHOD, Target};
use ferry_core::state::AppState;
use serde_json::Value;
use sqlx::PgPool;

async fn app(pool: &PgPool) -> (Router, AppState) {
    app_with(pool.clone(), &[])
}

fn token(state: &AppState, user: &str) -> String {
    state.jwt.issue_token(user).unwrap()
}

/// GET `uri` as `user`; asserts 200 and the Impact schema.
async fn impact_as(app: &Router, state: &AppState, user: &str, uri: &str) -> Value {
    let (status, body) = call(app, get_with_token(uri, &token(state, user))).await;
    assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    assert_contract("Impact", &body);
    body
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

/// A real (not history) delivered run.
async fn real_run(pool: &PgPool, id: &str, runner: &str, circle: &str) {
    sqlx::query(
        "INSERT INTO ferry.runs (id, restaurant_id, runner_id, circle_id, status, created_via,
                                 leaving_at, window_closes_at)
         VALUES ($1, 'rst_cosmic', $2, $3, 'delivered', 'app',
                 now() - interval '120 days', now() - interval '120 days')",
    )
    .bind(id)
    .bind(runner)
    .bind(circle)
    .execute(pool)
    .await
    .unwrap();
}

/// A real order on `run`, delivered `days_ago` days ago (None: not delivered yet).
async fn real_order(
    pool: &PgPool,
    id: &str,
    run: &str,
    customer: &str,
    status: &str,
    co2_saved_g: Option<f64>,
    days_ago: Option<i32>,
) {
    sqlx::query(
        "INSERT INTO ferry.orders (id, run_id, customer_id, status, subtotal_cents, tax_cents,
                                   ferry_fee_cents, tip_cents, total_cents, dropoff_label,
                                   dropoff_lat, dropoff_lng, handoff_code, car_distance_m,
                                   co2_saved_g, delivered_at, created_at)
         VALUES ($1, $2, $3, $4, 899, 67, 150, 200, 1316, 'Hinton James lobby',
                 35.902364, -79.043231, '1234', $5 / 400.0 * 1609.344, $5,
                 now() - make_interval(days => $6), now() - make_interval(days => coalesce($6, 0)) - interval '45 minutes')",
    )
    .bind(id)
    .bind(run)
    .bind(customer)
    .bind(status)
    .bind(co2_saved_g)
    .bind(days_ago)
    .execute(pool)
    .await
    .unwrap();
}

/// Fixtures plus real deliveries:
/// - run_r1 (Maya, cir_hinton4): the judge's (500 g, 2 days ago), Dana's (settled, 400 g,
///   9 days ago), Leo's (still funded).
/// - run_r2 (Maya, cir_climb): Leo's (300 g, 20 days ago).
/// - run_r3 (Dana, cir_hinton4): the judge's (100 g, 100 days ago: in the totals, not a week).
async fn seed_real(pool: &PgPool) {
    demo::seed::seed_pool(pool).await.unwrap();
    real_run(pool, "run_r1", "usr_maya", "cir_hinton4").await;
    real_run(pool, "run_r2", "usr_maya", "cir_climb").await;
    real_run(pool, "run_r3", "usr_dana", "cir_hinton4").await;
    for (id, run, customer, status, co2, days) in [
        (
            "ord_r1a",
            "run_r1",
            "usr_judge",
            "delivered",
            Some(500.0),
            Some(2),
        ),
        (
            "ord_r1b",
            "run_r1",
            "usr_dana",
            "settled",
            Some(400.0),
            Some(9),
        ),
        ("ord_r1c", "run_r1", "usr_leo", "funded", None, None),
        (
            "ord_r2a",
            "run_r2",
            "usr_leo",
            "delivered",
            Some(300.0),
            Some(20),
        ),
        (
            "ord_r3a",
            "run_r3",
            "usr_judge",
            "delivered",
            Some(100.0),
            Some(100),
        ),
    ] {
        real_order(pool, id, run, customer, status, co2, days).await;
    }
}

/// The 8 weeks Postgres puts the scope's deliveries in (`date_trunc('week', ...)` is ISO, Monday
/// 00:00), oldest first: (week_start, deliveries, grams). `filter` is SQL over `o` and `r`.
async fn expected_weeks(pool: &PgPool, filter: &str) -> Vec<(String, i64, f64)> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "WITH w AS (
             SELECT date_trunc('week', now() AT TIME ZONE 'UTC') - make_interval(weeks => g) AS start
               FROM generate_series(7, 0, -1) AS g
         )
         SELECT to_char(w.start, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'),
                count(o.id)::bigint,
                coalesce(sum(o.co2_saved_g), 0)::double precision
           FROM w
           LEFT JOIN (ferry.orders o JOIN ferry.runs r ON r.id = o.run_id)
             ON date_trunc('week', o.delivered_at AT TIME ZONE 'UTC') = w.start
            AND o.status IN ('delivered', 'settled') AND ({filter})
          GROUP BY w.start ORDER BY w.start"
    )))
    .fetch_all(pool)
    .await
    .unwrap()
}

fn weeks_of(body: &Value) -> Vec<(String, i64, f64)> {
    body["by_week"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| {
            (
                w["week_start"].as_str().unwrap().to_owned(),
                w["deliveries"].as_i64().unwrap(),
                w["co2_saved_g"].as_f64().unwrap(),
            )
        })
        .collect()
}

fn assert_weeks(body: &Value, expected: &[(String, i64, f64)]) {
    let got = weeks_of(body);
    assert_eq!(got.len(), 8, "{body}");
    for (g, e) in got.iter().zip(expected) {
        assert_eq!((&g.0, g.1), (&e.0, e.1), "{body}");
        assert!(close(g.2, e.2), "{g:?} vs {e:?}");
    }
}

fn assert_totals(body: &Value, scope: &str, deliveries: i64, co2: f64, history: bool) {
    assert_eq!(body["scope"], scope, "{body}");
    assert_eq!(body["deliveries"], deliveries, "{body}");
    let grams = body["co2_saved_g"].as_f64().unwrap();
    assert!(close(grams, co2), "{grams} vs {co2}: {body}");
    // The test rows store car meters as grams / 400 miles.
    let miles = body["car_miles_avoided"].as_f64().unwrap();
    assert!(close(miles, co2 / 400.0), "{miles}: {body}");
    assert_eq!(body["method"], METHOD);
    assert_eq!(body["includes_demo_history"], history, "{body}");
}

#[sqlx::test(migrations = "../migrations")]
async fn each_scope_sums_its_delivered_orders(pool: PgPool) {
    seed_real(&pool).await;
    let (app, state) = app(&pool).await;

    // me: the judge ordered r1a and r3a.
    let me = impact_as(&app, &state, "usr_judge", "/v1/impact?scope=me").await;
    assert_totals(&me, "me", 2, 600.0, false);
    assert_weeks(
        &me,
        &expected_weeks(&pool, "o.customer_id = 'usr_judge'").await,
    );
    // No scope is me.
    let default = impact_as(&app, &state, "usr_judge", "/v1/impact").await;
    assert_eq!(default, me);

    // me counts runs too: Maya ran r1 and r2 (Leo's funded order is not delivered).
    let maya = impact_as(&app, &state, "usr_maya", "/v1/impact?scope=me").await;
    assert_totals(&maya, "me", 3, 1200.0, false);
    assert_weeks(
        &maya,
        &expected_weeks(
            &pool,
            "o.customer_id = 'usr_maya' OR r.runner_id = 'usr_maya'",
        )
        .await,
    );

    // circle: r1 and r3 are cir_hinton4's, r2 is cir_climb's.
    let hinton = impact_as(
        &app,
        &state,
        "usr_judge",
        "/v1/impact?scope=circle&circle_id=cir_hinton4",
    )
    .await;
    assert_totals(&hinton, "circle", 3, 1000.0, false);
    assert_weeks(
        &hinton,
        &expected_weeks(&pool, "r.circle_id = 'cir_hinton4'").await,
    );
    let climb = impact_as(
        &app,
        &state,
        "usr_leo",
        "/v1/impact?scope=circle&circle_id=cir_climb",
    )
    .await;
    assert_totals(&climb, "circle", 1, 300.0, false);

    // campus: everything delivered, and circle_id is ignored outside the circle scope.
    let campus = impact_as(
        &app,
        &state,
        "usr_leo",
        "/v1/impact?scope=campus&circle_id=cir_nowhere",
    )
    .await;
    assert_totals(&campus, "campus", 4, 1300.0, false);
    assert_weeks(&campus, &expected_weeks(&pool, "true").await);
    // The 100-day-old delivery is in the totals but not in the 8 weeks.
    let in_weeks: i64 = weeks_of(&campus).iter().map(|w| w.1).sum();
    assert_eq!(in_weeks, 3);
}

#[sqlx::test(migrations = "../migrations")]
async fn me_matches_me_stats(pool: PgPool) {
    seed_real(&pool).await;
    let (app, state) = app(&pool).await;
    for user in ["usr_judge", "usr_maya", "usr_dana", "usr_leo"] {
        let (status, stats) =
            call(&app, get_with_token("/v1/me/stats", &token(&state, user))).await;
        assert_eq!(status, StatusCode::OK, "{stats}");
        let me = impact_as(&app, &state, user, "/v1/impact?scope=me").await;
        assert!(
            close(
                me["co2_saved_g"].as_f64().unwrap(),
                stats["co2_saved_g"].as_f64().unwrap()
            ),
            "{user}: {me} vs {stats}"
        );
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn nothing_delivered_is_eight_zero_weeks(pool: PgPool) {
    demo::seed::seed_pool(&pool).await.unwrap();
    let (app, state) = app(&pool).await;
    for uri in [
        "/v1/impact?scope=me",
        "/v1/impact?scope=campus",
        "/v1/impact?scope=circle&circle_id=cir_hinton4",
    ] {
        let body = impact_as(&app, &state, "usr_judge", uri).await;
        assert_eq!(body["deliveries"], 0);
        assert_eq!(body["co2_saved_g"].as_f64(), Some(0.0));
        assert_eq!(body["includes_demo_history"], false);
        assert_weeks(&body, &expected_weeks(&pool, "false").await);
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn circle_scope_rules(pool: PgPool) {
    seed_real(&pool).await;
    let (app, state) = app(&pool).await;
    let judge = token(&state, "usr_judge");
    for (uri, status, code) in [
        // circle_id missing or empty: 400.
        (
            "/v1/impact?scope=circle",
            StatusCode::BAD_REQUEST,
            "validation_failed",
        ),
        (
            "/v1/impact?scope=circle&circle_id=",
            StatusCode::BAD_REQUEST,
            "validation_failed",
        ),
        // An unknown circle: 404.
        (
            "/v1/impact?scope=circle&circle_id=cir_nowhere",
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        // The judge is not in the climbing crew: 403.
        (
            "/v1/impact?scope=circle&circle_id=cir_climb",
            StatusCode::FORBIDDEN,
            "forbidden",
        ),
        // A scope outside the enum: 400.
        (
            "/v1/impact?scope=planet",
            StatusCode::BAD_REQUEST,
            "validation_failed",
        ),
        (
            "/v1/impact?scope=",
            StatusCode::BAD_REQUEST,
            "validation_failed",
        ),
    ] {
        let (got, body) = call(&app, get_with_token(uri, &judge)).await;
        assert_error(got, &body, status, code);
    }

    // The internal function applies the same rules (the agent's explain_impact).
    let err = impact::impact_for(&pool, "usr_judge", ImpactScope::Circle, Some("cir_climb"))
        .await
        .unwrap_err();
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    let err = impact::impact_for(&pool, "usr_judge", ImpactScope::Circle, None)
        .await
        .unwrap_err();
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    let ok = impact::impact_for(&pool, "usr_maya", ImpactScope::Circle, Some("cir_climb"))
        .await
        .unwrap();
    assert_eq!((ok.scope, ok.deliveries), (ImpactScope::Circle, 1));
}

#[sqlx::test(migrations = "../migrations")]
async fn impact_needs_a_token_and_public_impact_does_not(pool: PgPool) {
    seed_real(&pool).await;
    let (app, state) = app(&pool).await;
    for uri in ["/v1/impact", "/v1/impact?scope=campus"] {
        let (status, body) = call(&app, common::get(uri)).await;
        assert_error(status, &body, StatusCode::UNAUTHORIZED, "unauthorized");
    }
    // A bad scope with no token is still 401: the token is checked first.
    let (status, body) = call(&app, common::get("/v1/impact?scope=planet")).await;
    assert_error(status, &body, StatusCode::UNAUTHORIZED, "unauthorized");

    let (status, public) = call(&app, common::get("/v1/public/impact")).await;
    assert_eq!(status, StatusCode::OK, "{public}");
    assert_contract("Impact", &public);
    assert_totals(&public, "campus", 4, 1300.0, false);
    let campus = impact_as(&app, &state, "usr_dana", "/v1/impact?scope=campus").await;
    assert_eq!(public, campus);
}

#[sqlx::test(migrations = "../migrations")]
async fn public_impact_is_cached(pool: PgPool) {
    seed_real(&pool).await;
    let (app, state) = app(&pool).await;
    let (_, first) = call(&app, common::get("/v1/public/impact")).await;
    assert_eq!(first["deliveries"], 4);

    real_order(
        &pool,
        "ord_r1d",
        "run_r1",
        "usr_dana",
        "delivered",
        Some(250.0),
        Some(1),
    )
    .await;
    // Within 30 s the landing page keeps its number; the signed-in campus scope is live.
    let (status, second) = call(&app, common::get("/v1/public/impact")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second, first);
    let campus = impact_as(&app, &state, "usr_dana", "/v1/impact?scope=campus").await;
    assert_totals(&campus, "campus", 5, 1550.0, false);
    assert_eq!(impact::PUBLIC_TTL.as_secs(), 30);
}

/// Totals of a scope, read through the internal function: (deliveries, grams, history).
async fn totals(pool: &PgPool, target: Target<'_>) -> (i64, f64, bool) {
    let i = impact::compute(pool, &target).await.unwrap();
    (i.deliveries, i.co2_saved_g, i.includes_demo_history)
}

#[sqlx::test(migrations = "../migrations")]
async fn seed_adds_demo_history_idempotently_and_reset_keeps_it(pool: PgPool) {
    let config = common::config(&[("DEMO_MODE", "true")]);
    demo::run_command(Command::Seed, &pool, &config)
        .await
        .unwrap();
    let (app, state) = app(&pool).await;

    let campus = impact_as(&app, &state, "usr_judge", "/v1/impact?scope=campus").await;
    assert_eq!(campus["deliveries"], 28);
    assert!(
        campus["co2_saved_g"].as_f64().unwrap() > 5_000.0,
        "{campus}"
    );
    assert!(
        campus["car_miles_avoided"].as_f64().unwrap() > 10.0,
        "{campus}"
    );
    assert_eq!(campus["includes_demo_history"], true);
    // All of it is inside the 8-week chart, spread over the weeks.
    assert_weeks(&campus, &expected_weeks(&pool, "true").await);
    let weeks = weeks_of(&campus);
    assert_eq!(weeks.iter().map(|w| w.1).sum::<i64>(), 28);
    assert!(weeks.iter().filter(|w| w.1 > 0).count() >= 6, "{campus}");

    for uri in [
        "/v1/impact?scope=me",
        "/v1/impact?scope=circle&circle_id=cir_hinton4",
    ] {
        let body = impact_as(&app, &state, "usr_judge", uri).await;
        assert!(body["deliveries"].as_i64().unwrap() > 0, "{uri}: {body}");
        assert!(body["co2_saved_g"].as_f64().unwrap() > 0.0, "{uri}: {body}");
        assert_eq!(body["includes_demo_history"], true, "{uri}");
    }
    let (status, public) = call(&app, common::get("/v1/public/impact")).await;
    assert_eq!(status, StatusCode::OK);
    assert_contract("Impact", &public);
    assert_eq!(public["includes_demo_history"], true);
    assert_eq!(public["deliveries"], 28);

    let before = [
        totals(&pool, Target::Campus).await,
        totals(
            &pool,
            Target::Me {
                user_id: "usr_judge",
            },
        )
        .await,
        totals(
            &pool,
            Target::Circle {
                circle_id: "cir_hinton4",
            },
        )
        .await,
        totals(
            &pool,
            Target::Circle {
                circle_id: "cir_climb",
            },
        )
        .await,
    ];

    // A second seed writes no history row (same UTC day) and gives the same totals.
    let again = impact::seed_history(&pool).await.unwrap();
    for (table, n) in &again.written {
        assert_eq!(*n, 0, "{table}: {again}");
    }
    demo::run_command(Command::Seed, &pool, &config)
        .await
        .unwrap();
    let after_seed = [
        totals(&pool, Target::Campus).await,
        totals(
            &pool,
            Target::Me {
                user_id: "usr_judge",
            },
        )
        .await,
        totals(
            &pool,
            Target::Circle {
                circle_id: "cir_hinton4",
            },
        )
        .await,
        totals(
            &pool,
            Target::Circle {
                circle_id: "cir_climb",
            },
        )
        .await,
    ];
    assert_eq!(after_seed, before);

    // Reset (DEMO_MODE) deletes activity but keeps the history.
    demo::run_command(Command::Reset, &pool, &config)
        .await
        .unwrap();
    let after_reset = [
        totals(&pool, Target::Campus).await,
        totals(
            &pool,
            Target::Me {
                user_id: "usr_judge",
            },
        )
        .await,
        totals(
            &pool,
            Target::Circle {
                circle_id: "cir_hinton4",
            },
        )
        .await,
        totals(
            &pool,
            Target::Circle {
                circle_id: "cir_climb",
            },
        )
        .await,
    ];
    assert_eq!(after_reset, before);
    let counts: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM ferry.runs WHERE is_demo_history),
                (SELECT count(*) FROM ferry.orders WHERE is_demo_history),
                (SELECT count(*) FROM ferry.order_lines l JOIN ferry.orders o ON o.id = l.order_id
                  WHERE o.is_demo_history),
                (SELECT count(*) FROM ferry.ledger_entries l JOIN ferry.runs r ON r.id = l.run_id
                  WHERE r.is_demo_history)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(counts, (15, 28, 29, 0));
}

#[sqlx::test(migrations = "../migrations")]
async fn history_rows_are_well_formed(pool: PgPool) {
    demo::seed::seed_pool(&pool).await.unwrap();
    let report = impact::seed_history(&pool).await.unwrap();
    assert_eq!(report.written("runs"), Some(15));
    assert_eq!(report.written("orders"), Some(28));
    // One order has two lines (a dog and tots).
    assert_eq!(report.written("order_lines"), Some(29));
    // funded, purchased, picked_up, delivered and settled per order.
    assert_eq!(report.written("order_events"), Some(28 * 5));

    let bad: Vec<String> = sqlx::query_scalar(
        "SELECT o.id FROM ferry.orders o JOIN ferry.runs r ON r.id = o.run_id
          WHERE o.is_demo_history AND (
                NOT r.is_demo_history
             OR r.status <> 'delivered'
             OR o.status <> 'settled'
             OR o.delivered_at <= o.created_at
             OR o.delivered_at >= date_trunc('day', now(), 'UTC')
             OR o.created_at < r.created_at
             OR o.created_at > r.window_closes_at
             OR o.car_distance_m IS NULL OR o.co2_saved_g IS NULL
             OR abs(o.co2_saved_g - o.car_distance_m / 1609.344 * 400) > 1e-6
             OR o.customer_id = r.runner_id
             OR NOT EXISTS (SELECT 1 FROM ferry.circle_members m
                             WHERE m.circle_id = r.circle_id AND m.user_id = o.customer_id)
             OR o.subtotal_cents <> (SELECT sum(l.line_total_cents) FROM ferry.order_lines l
                                      WHERE l.order_id = o.id)
             OR (SELECT max(e.ts) FROM ferry.order_events e WHERE e.order_id = o.id)
                  < o.delivered_at)",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(bad.is_empty(), "{bad:?}");

    // delivery_minutes_daily has history to answer "how long does Sup Dogs usually take?".
    let p50: Option<f64> = sqlx::query_scalar(
        "SELECT max(p50) FROM ferry.delivery_minutes_daily WHERE restaurant_id = 'rst_supdogs'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(p50.is_some_and(|m| (20.0..90.0).contains(&m)), "{p50:?}");
}

#[sqlx::test(migrations = "../migrations")]
async fn history_never_touches_other_rows(pool: PgPool) {
    seed_real(&pool).await;
    // run_demo mid-flight, and a real row that took a history id.
    sqlx::query("UPDATE ferry.runs SET status = 'closed' WHERE id = 'run_demo'")
        .execute(&pool)
        .await
        .unwrap();
    real_run(&pool, "run_hist_05", "usr_dana", "cir_hinton4").await;
    let snapshot = |pool: PgPool| async move {
        let runs: Vec<(String, String, bool, String)> = sqlx::query_as(
            "SELECT id, status, is_demo_history, leaving_at::text FROM ferry.runs
              WHERE NOT is_demo_history ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let orders: Vec<(String, String, String, Option<f64>)> = sqlx::query_as(
            "SELECT id, run_id, status, co2_saved_g FROM ferry.orders
              WHERE NOT is_demo_history ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        (runs, orders)
    };
    let before = snapshot(pool.clone()).await;
    let report = impact::seed_history(&pool).await.unwrap();
    assert_eq!(snapshot(pool.clone()).await, before);
    // The taken run and the order that would go on it are skipped.
    assert_eq!(report.written("runs"), Some(14));
    assert_eq!(report.written("orders"), Some(27));
    let on_real: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ferry.orders WHERE run_id = 'run_hist_05'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(on_real, 0);
    let status: String = sqlx::query_scalar("SELECT status FROM ferry.runs WHERE id = 'run_demo'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "closed");
}

#[sqlx::test(migrations = "../migrations")]
async fn a_seed_on_a_later_day_moves_the_times(pool: PgPool) {
    demo::seed::seed_pool(&pool).await.unwrap();
    impact::seed_history(&pool).await.unwrap();
    let before = totals(&pool, Target::Campus).await;
    // As if the history had been seeded yesterday.
    for sql in [
        "UPDATE ferry.runs SET leaving_at = leaving_at - interval '1 day',
                window_closes_at = window_closes_at - interval '1 day',
                created_at = created_at - interval '1 day'
          WHERE is_demo_history",
        "UPDATE ferry.orders SET delivered_at = delivered_at - interval '1 day',
                created_at = created_at - interval '1 day'
          WHERE is_demo_history",
        "UPDATE ferry.order_events e SET ts = e.ts - interval '1 day'
           FROM ferry.orders o WHERE o.id = e.order_id AND o.is_demo_history",
    ] {
        sqlx::query(sql).execute(&pool).await.unwrap();
    }

    let report = impact::seed_history(&pool).await.unwrap();
    assert_eq!(report.written("runs"), Some(15));
    assert_eq!(report.written("orders"), Some(28));
    assert_eq!(report.written("order_lines"), Some(0));
    // Every event is stale: removed, then written at the new time.
    assert_eq!(report.written("order_events"), Some(2 * (28 * 5)));
    let (events, latest_ok): (i64, bool) = sqlx::query_as(
        "SELECT count(*),
                bool_and(e.ts <= o.delivered_at + interval '2 minutes'
                         AND e.ts >= o.created_at)
           FROM ferry.order_events e JOIN ferry.orders o ON o.id = e.order_id
          WHERE o.is_demo_history",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((events, latest_ok), (28 * 5, true));
    assert_eq!(totals(&pool, Target::Campus).await, before);

    let third = impact::seed_history(&pool).await.unwrap();
    assert!(third.written.iter().all(|(_, n)| *n == 0), "{third}");
}
