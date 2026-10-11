//! `ferry-core seed` and `reset`: every fixture row lands once, a second seed changes nothing but
//! the demo run's times, and a reset clears the activity since while keeping demo history.
mod catalog_support;
mod common;

use catalog_support::{by_id, fixture, seed};
use ferry_core::demo::reset::{ResetError, reset};
use serde_json::Value;
use sqlx::PgPool;

/// The row counts the fixtures imply, table by table.
fn fixture_counts() -> Vec<(&'static str, i64)> {
    let len = |v: &Value| v.as_array().unwrap().len() as i64;
    let users = fixture("users");
    let circles = fixture("circles");
    let menus = fixture("menus");
    let mut categories = 0;
    let mut items = 0;
    let mut options = 0;
    let mut choices = 0;
    for menu in menus.as_array().unwrap() {
        for category in menu["categories"].as_array().unwrap() {
            categories += 1;
            for item in category["items"].as_array().unwrap() {
                items += 1;
                for option in item["options"].as_array().unwrap() {
                    options += 1;
                    choices += len(&option["choices"]);
                }
            }
        }
    }
    vec![
        ("users", len(&users)),
        ("places", len(&fixture("places"))),
        ("profiles", len(&users)),
        ("circles", len(&circles)),
        (
            "circle_members",
            circles
                .as_array()
                .unwrap()
                .iter()
                .map(|c| len(&c["members"]))
                .sum(),
        ),
        ("areas", len(&fixture("areas"))),
        ("restaurants", len(&fixture("restaurants"))),
        ("menu_categories", categories),
        ("menu_items", items),
        ("item_options", options),
        ("item_choices", choices),
        ("runs", len(&fixture("runs"))),
    ]
}

/// The rows of `table`, leaving out Campus (migration `20261011000008`), which is no fixture:
/// every user is in it, and [`assert_everyone_in_campus`] checks that instead.
async fn count(pool: &PgPool, table: &str) -> i64 {
    let campus = match table {
        "circles" => " WHERE id <> 'cir_campus'",
        "circle_members" => " WHERE circle_id <> 'cir_campus'",
        _ => "",
    };
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM ferry.{table}{campus}"
    )))
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn counts(pool: &PgPool) -> Vec<(&'static str, i64)> {
    let mut out = Vec::new();
    for (table, _) in fixture_counts() {
        out.push((table, count(pool, table).await));
    }
    out
}

/// Campus survives seeding and reset, and every user is in it.
async fn assert_everyone_in_campus(pool: &PgPool) {
    let (circles, outside): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM ferry.circles WHERE id = 'cir_campus'), \
                (SELECT count(*) FROM ferry.users u WHERE NOT EXISTS ( \
                     SELECT 1 FROM ferry.circle_members m \
                     WHERE m.circle_id = 'cir_campus' AND m.user_id = u.id))",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(
        (circles, outside),
        (1, 0),
        "Campus and the users outside it"
    );
}

/// run_demo's status, seconds from now until it leaves, and seconds from leaving to the window
/// closing.
async fn demo_run(pool: &PgPool) -> (String, f64, f64) {
    sqlx::query_as(
        "SELECT status, extract(epoch FROM leaving_at - now())::float8, \
                extract(epoch FROM window_closes_at - leaving_at)::float8 \
         FROM ferry.runs WHERE id = 'run_demo'",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

fn assert_demo_run_fresh(run: &(String, f64, f64)) {
    let (status, leaves_in, window_after) = run;
    assert_eq!(status, "open");
    // Leaving in 15 minutes (less the moments since the seed).
    assert!(
        (14.0 * 60.0..=15.0 * 60.0).contains(leaves_in),
        "leaves in {leaves_in} s"
    );
    // The fixture's window closes 10 minutes after leaving.
    assert_eq!(*window_after, 600.0);
}

#[sqlx::test(migrations = "../migrations")]
async fn seed_twice_gives_the_fixture_row_counts(pool: PgPool) {
    let first = seed(&pool).await;
    let expected = fixture_counts();
    assert_eq!(counts(&pool).await, expected);
    for (table, n) in &expected {
        assert_eq!(first.written(table), Some(*n as u64), "{table}");
    }

    assert_everyone_in_campus(&pool).await;

    let second = seed(&pool).await;
    assert_eq!(counts(&pool).await, expected);
    assert_everyone_in_campus(&pool).await;
    // Nothing matching its fixture is rewritten; only the demo run is re-timed.
    for (table, _) in &expected {
        let want = if *table == "runs" {
            expected.iter().find(|(t, _)| *t == "runs").unwrap().1 as u64
        } else {
            0
        };
        assert_eq!(second.written(table), Some(want), "{table}");
    }
    assert_demo_run_fresh(&demo_run(&pool).await);
}

#[sqlx::test(migrations = "../migrations")]
async fn seed_writes_the_fixture_values(pool: PgPool) {
    seed(&pool).await;

    let phones: Vec<(String, String)> =
        sqlx::query_as("SELECT id, phone_e164 FROM ferry.users ORDER BY phone_e164")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        phones,
        [
            ("usr_judge".to_owned(), "+19195550101".to_owned()),
            ("usr_maya".to_owned(), "+19195550102".to_owned()),
            ("usr_dana".to_owned(), "+19195550103".to_owned()),
            ("usr_leo".to_owned(), "+19195550104".to_owned()),
        ]
    );

    // Each profile, read back as a contract Profile, is its fixture's.
    for user in fixture("users").as_array().unwrap() {
        let id = user["id"].as_str().unwrap();
        let profile: Value = sqlx::query_scalar(
            "SELECT jsonb_build_object( \
                 'dropoff_default', CASE WHEN dropoff_label IS NOT NULL THEN jsonb_strip_nulls( \
                     jsonb_build_object('id', dropoff_place_id, 'label', dropoff_label, \
                         'location', jsonb_build_object('lat', dropoff_lat, 'lng', dropoff_lng), \
                         'note', dropoff_note)) END, \
                 'diet', to_jsonb(diet), 'avoid', to_jsonb(avoid), \
                 'favorite_cuisines', to_jsonb(favorite_cuisines), \
                 'favorite_restaurant_ids', to_jsonb(favorite_restaurant_ids), \
                 'usual_times', usual_times, 'walking_pace', walking_pace, \
                 'runner_opt_in', runner_opt_in, 'default_tip_cents', default_tip_cents, \
                 'notify_push', notify_push, 'notify_messages', notify_messages, \
                 'share_location_when_running', share_location_when_running) \
             FROM ferry.profiles WHERE user_id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(profile, user["profile"], "{id}");
    }
    let onboarded: Vec<(String, String, String, bool)> = sqlx::query_as(
        "SELECT id, display_name, bubble_color, onboarding_complete FROM ferry.users ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    for (id, name, color, done) in onboarded {
        let user = &by_id(&fixture("users"))[&id];
        assert_eq!(name, user["display_name"].as_str().unwrap());
        assert_eq!(color, user["bubble_color"].as_str().unwrap());
        assert_eq!(done, user["onboarding_complete"].as_bool().unwrap());
    }

    // The demo run: Maya to Cosmic Cantina in the Hinton James circle.
    let run: (String, String, String, String, bool, Option<String>) = sqlx::query_as(
        "SELECT restaurant_id, runner_id, circle_id, created_via, is_demo_history, receipt_path \
         FROM ferry.runs WHERE id = 'run_demo'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        run,
        (
            "rst_cosmic".into(),
            "usr_maya".into(),
            "cir_hinton4".into(),
            "message".into(),
            false,
            None
        )
    );
    assert_demo_run_fresh(&demo_run(&pool).await);
}

#[sqlx::test(migrations = "../migrations")]
async fn seed_restores_edited_rows(pool: PgPool) {
    seed(&pool).await;
    for sql in [
        "UPDATE ferry.restaurants SET name = 'Cosmic Cantina (closed)', cuisine = '{}' WHERE id = 'rst_cosmic'",
        "UPDATE ferry.profiles SET walking_pace = 'relaxed', avoid = '{}' WHERE user_id = 'usr_maya'",
        "DELETE FROM ferry.item_choices WHERE id = 'cho_cosmic_chicken_burrito_guac'",
        "UPDATE ferry.runs SET status = 'cancelled', leaving_at = now() - interval '1 day' WHERE id = 'run_demo'",
    ] {
        sqlx::query(sql).execute(&pool).await.unwrap();
    }
    let report = seed(&pool).await;
    assert_eq!(report.written("restaurants"), Some(1));
    assert_eq!(report.written("profiles"), Some(1));
    assert_eq!(report.written("item_choices"), Some(1));
    assert_eq!(report.written("users"), Some(0));
    assert_eq!(counts(&pool).await, fixture_counts());

    let (name, cuisine): (String, Vec<String>) =
        sqlx::query_as("SELECT name, cuisine FROM ferry.restaurants WHERE id = 'rst_cosmic'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        (name.as_str(), cuisine),
        ("Cosmic Cantina", vec!["mexican".to_owned()])
    );
    let (pace, avoid): (String, Vec<String>) =
        sqlx::query_as("SELECT walking_pace, avoid FROM ferry.profiles WHERE user_id = 'usr_maya'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        (pace.as_str(), avoid),
        ("brisk", vec!["peanuts".to_owned()])
    );
    assert_demo_run_fresh(&demo_run(&pool).await);
}

/// Seeded past weeks that a reset keeps: a delivered history run with one order, its line, event,
/// ping and ledger row. created_at is set explicitly, before delivered_at.
const HISTORY: &[&str] = &[
    "INSERT INTO ferry.runs (id, restaurant_id, runner_id, circle_id, status, created_via, leaving_at, window_closes_at, is_demo_history, created_at) \
     VALUES ('run_hist_1', 'rst_supdogs', 'usr_maya', 'cir_hinton4', 'delivered', 'app', now() - interval '7 days', now() - interval '7 days' + interval '10 minutes', true, now() - interval '7 days')",
    "INSERT INTO ferry.orders (id, run_id, customer_id, status, subtotal_cents, tax_cents, ferry_fee_cents, tip_cents, total_cents, dropoff_place_id, dropoff_label, dropoff_lat, dropoff_lng, handoff_code, co2_saved_g, delivered_at, is_demo_history, created_at) \
     VALUES ('ord_hist_1', 'run_hist_1', 'usr_judge', 'delivered', 899, 67, 150, 200, 1316, 'place_fetzer', 'Fetzer Gym (HackNC)', 35.909152, -79.046905, '1234', 480, now() - interval '7 days' + interval '30 minutes', true, now() - interval '7 days')",
    "INSERT INTO ferry.order_lines (order_id, line_no, menu_item_id, name, qty, unit_price_cents, line_total_cents) \
     SELECT 'ord_hist_1', 0, id, name, 1, 899, 899 FROM ferry.menu_items WHERE restaurant_id = 'rst_supdogs' ORDER BY sort_order LIMIT 1",
    "INSERT INTO ferry.order_events (order_id, run_id, ts, status) VALUES ('ord_hist_1', 'run_hist_1', now() - interval '7 days', 'delivered')",
    "INSERT INTO ferry.location_pings (run_id, user_id, ts, lat, lng, accuracy_m) VALUES ('run_hist_1', 'usr_maya', now() - interval '7 days', 35.91, -79.05, 5)",
    "INSERT INTO ferry.ledger_entries (id, kind, debit_account, credit_account, amount_cents, run_id, order_id, idempotency_key) \
     VALUES ('txn_hist_1', 'order_hold', 'platform:ferry:escrow', 'user:usr_judge:checking', 1316, 'run_hist_1', 'ord_hist_1', 'hist-hold-1')",
];

/// Activity after the seed that a reset deletes, on run_demo, on a new run and even on the
/// history run, by a persona and by a real user (who stays).
const ACTIVITY: &[&str] = &[
    "INSERT INTO ferry.users (id, phone_e164, display_name, avatar_seed, bubble_color) VALUES ('usr_real', '+19195551234', 'Real', 'real', '#123456')",
    "UPDATE ferry.runs SET status = 'closed', leaving_at = now() - interval '1 hour', window_closes_at = now() - interval '50 minutes' WHERE id = 'run_demo'",
    "INSERT INTO ferry.runs (id, restaurant_id, runner_id, circle_id, status, created_via, leaving_at, window_closes_at) \
     VALUES ('run_extra', 'rst_alpine', 'usr_dana', 'cir_hinton4', 'open', 'app', now() + interval '1 hour', now() + interval '50 minutes')",
    "INSERT INTO ferry.orders (id, run_id, customer_id, subtotal_cents, tax_cents, ferry_fee_cents, tip_cents, total_cents, dropoff_label, dropoff_lat, dropoff_lng, handoff_code) \
     VALUES ('ord_demo_1', 'run_demo', 'usr_judge', 899, 67, 150, 200, 1316, 'Fetzer Gym (HackNC)', 35.909152, -79.046905, '4321'), \
            ('ord_extra_1', 'run_extra', 'usr_real', 500, 38, 150, 0, 688, 'Pin', 35.90, -79.04, '1111'), \
            ('ord_odd_1', 'run_hist_1', 'usr_leo', 500, 38, 150, 0, 688, 'Pin', 35.90, -79.04, '2222')",
    "INSERT INTO ferry.order_lines (order_id, line_no, menu_item_id, name, qty, choice_ids, unit_price_cents, line_total_cents) \
     VALUES ('ord_demo_1', 0, 'itm_cosmic_chicken_burrito', 'Chicken Burrito', 1, '{cho_cosmic_chicken_burrito_no_beans}', 899, 899)",
    "INSERT INTO ferry.order_events (order_id, run_id, status) VALUES ('ord_demo_1', 'run_demo', 'funded'), ('ord_extra_1', 'run_extra', 'funded')",
    "INSERT INTO ferry.location_pings (run_id, user_id, ts, lat, lng, accuracy_m) VALUES ('run_demo', 'usr_maya', now(), 35.912, -79.054, 8)",
    "INSERT INTO ferry.ledger_entries (id, kind, debit_account, credit_account, amount_cents, run_id, order_id, idempotency_key) \
     VALUES ('txn_dep_1', 'deposit', 'user:usr_judge:checking', 'external', 5000, NULL, NULL, 'dep-1'), \
            ('txn_hold_1', 'order_hold', 'platform:ferry:escrow', 'user:usr_judge:checking', 1316, 'run_demo', 'ord_demo_1', 'hold-1'), \
            ('txn_odd_1', 'order_hold', 'platform:ferry:escrow', 'user:usr_leo:checking', 688, 'run_hist_1', 'ord_odd_1', 'hold-odd-1')",
    "INSERT INTO ferry.pickup_requests (id, customer_id, restaurant_id, lines, tip_cents, dropoff_label, dropoff_lat, dropoff_lng, needed_by, status, run_id, order_id) \
     VALUES ('req_1', 'usr_real', 'rst_alpine', '[]', 0, 'Pin', 35.90, -79.04, now() + interval '1 hour', 'matched', 'run_extra', 'ord_extra_1'), \
            ('req_2', 'usr_leo', 'rst_cosmic', '[]', 0, 'Pin', 35.90, -79.04, now() + interval '1 hour', 'searching', NULL, NULL)",
    "INSERT INTO ferry.notifications (id, user_id, type, title, body) VALUES ('ntf_1', 'usr_judge', 'run_open', 'Ferry to Cosmic', 'Open until 6:10')",
    "INSERT INTO ferry.invites (id, circle_id, from_user_id, message) VALUES ('inv_1', 'cir_hinton4', 'usr_dana', 'Dinner again?')",
    "INSERT INTO ferry.invite_responses (invite_id, user_id) VALUES ('inv_1', 'usr_judge')",
    // The agent's trip that opened run_demo, and one that opened run_extra.
    "INSERT INTO ferry.trip_intents (space_id, user_id, circle_id, restaurant_id, leaving_at, window_closes_at, status, detected_by, run_id, decided_at) \
     VALUES ('-1001', 'usr_maya', 'cir_hinton4', 'rst_cosmic', now(), now() + interval '10 minutes', 'opened', 'gemini', 'run_demo', now()), \
            ('-1001', 'usr_dana', 'cir_hinton4', 'rst_alpine', now(), now() + interval '10 minutes', 'opened', 'gemini', 'run_extra', now())",
];

async fn exec_all(pool: &PgPool, statements: &[&str]) {
    for sql in statements {
        sqlx::query(sqlx::AssertSqlSafe(*sql))
            .execute(pool)
            .await
            .unwrap_or_else(|e| panic!("{e}\n{sql}"));
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn reset_returns_to_the_seeded_state_and_keeps_history(pool: PgPool) {
    seed(&pool).await;
    exec_all(&pool, HISTORY).await;
    let history_counts = [
        ("orders", 1),
        ("order_lines", 1),
        ("order_events", 1),
        ("location_pings", 1),
        ("ledger_entries", 1),
    ];
    exec_all(&pool, ACTIVITY).await;
    assert_eq!(count(&pool, "orders").await, 4);

    let config = common::config(&[("DEMO_MODE", "true")]);
    let report = reset(&pool, &config).await.unwrap();
    for (table, deleted) in [
        ("ledger_entries", 3),
        ("pickup_requests", 2),
        ("order_events", 2),
        ("order_lines", 1),
        ("location_pings", 1),
        ("orders", 3),
        ("runs", 2),
        ("notifications", 1),
        ("invites", 1),
        ("trip_intents", 2),
    ] {
        assert_eq!(report.deleted(table), Some(deleted), "{table}: {report}");
    }

    // The fixtures are back, plus the history run.
    let mut expected = fixture_counts();
    for (table, n) in expected.iter_mut() {
        if *table == "runs" {
            *n += 1;
        }
    }
    let users = count(&pool, "users").await;
    assert_eq!(
        counts(&pool).await,
        expected
            .iter()
            .map(|(t, n)| (*t, if *t == "users" { users } else { *n }))
            .collect::<Vec<_>>()
    );
    // The real user stays; only activity goes.
    assert_eq!(users, fixture_counts()[0].1 + 1);
    assert_everyone_in_campus(&pool).await;
    for (table, n) in history_counts {
        assert_eq!(count(&pool, table).await, n, "{table}");
    }
    for table in [
        "pickup_requests",
        "notifications",
        "invites",
        "invite_responses",
        "trip_intents",
    ] {
        assert_eq!(count(&pool, table).await, 0, "{table}");
    }
    let kept: Vec<String> = sqlx::query_scalar("SELECT id FROM ferry.runs ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(kept, ["run_demo", "run_hist_1"]);
    let kept_order: String = sqlx::query_scalar("SELECT id FROM ferry.orders")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kept_order, "ord_hist_1");

    // run_demo is open again and timed from now.
    assert_demo_run_fresh(&demo_run(&pool).await);

    // A second reset finds nothing new to delete: only run_demo goes, to be seeded again.
    let again = reset(&pool, &config).await.unwrap();
    for (table, deleted) in &again.deleted {
        let want = if *table == "runs" { 1 } else { 0 };
        assert_eq!(*deleted, want, "{table}: {again}");
    }
    assert_demo_run_fresh(&demo_run(&pool).await);
}

#[sqlx::test(migrations = "../migrations")]
async fn reset_refuses_without_demo_mode(pool: PgPool) {
    seed(&pool).await;
    exec_all(&pool, &ACTIVITY[1..3]).await;
    let err = reset(&pool, &common::config(&[])).await.unwrap_err();
    assert!(matches!(err, ResetError::NotDemoMode), "{err}");
    assert!(err.to_string().contains("DEMO_MODE"));
    // Nothing was touched.
    assert_eq!(count(&pool, "runs").await, 2);
    let (status, _, _) = demo_run(&pool).await;
    assert_eq!(status, "closed");
}
