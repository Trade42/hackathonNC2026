//! Location retention (pings deleted 24 hours after a run ends, walk distance kept on the order)
//! and the data-age helpers, against the local Postgres.

mod common;

use std::time::{Duration, Instant};

use ferry_core::tracking::{
    data_age, haversine_m, latest_ping, latlng, sweep_once, sweep_with_lock_timeout,
    visible_runner_location,
};
use sqlx::PgPool;

async fn exec(pool: &PgPool, sql: &'static str) {
    sqlx::raw_sql(sql).execute(pool).await.unwrap();
}

/// Four runs by Maya for the circle: `run_old` delivered 25 hours ago, `run_recent` delivered an
/// hour ago, `run_live` still delivering (with two-day-old pings), `run_cancelled` cancelled with
/// its last ping 25 hours ago. Jordan ordered on each.
async fn seed(pool: &PgPool) {
    exec(
        pool,
        r##"
        INSERT INTO ferry.users (id, phone_e164, display_name, avatar_seed, bubble_color) VALUES
            ('usr_maya',  '+19195550102', 'Maya',   'maya',  '#336699'),
            ('usr_judge', '+19195550101', 'Jordan', 'judge', '#336699'),
            ('usr_dana',  '+19195550103', 'Dana',   'dana',  '#336699');
        INSERT INTO ferry.circles (id, name, emoji) VALUES ('cir_t', 'Hinton James 4th floor', '🌊');
        INSERT INTO ferry.circle_members (circle_id, user_id) VALUES
            ('cir_t', 'usr_maya'), ('cir_t', 'usr_judge'), ('cir_t', 'usr_dana');
        INSERT INTO ferry.areas (id, name, center_lat, center_lng)
            VALUES ('area_franklin', 'Franklin St', 35.9132, -79.0558);
        INSERT INTO ferry.restaurants (id, name, area_id, lat, lng, address, hours_today, price_level)
            VALUES ('rst_cosmic', 'Cosmic Cantina', 'area_franklin', 35.913211, -79.055094,
                    '118 E Franklin St', '11:00-04:00', 1);
        INSERT INTO ferry.runs (id, restaurant_id, runner_id, circle_id, status, created_via, leaving_at, window_closes_at)
        SELECT id, 'rst_cosmic', 'usr_maya', 'cir_t', status, 'app', now() - interval '3 days', now() - interval '3 days'
        FROM (VALUES ('run_old', 'delivered'), ('run_recent', 'delivered'),
                     ('run_live', 'delivering'), ('run_cancelled', 'cancelled')) AS v (id, status);
        INSERT INTO ferry.orders (id, run_id, customer_id, status, subtotal_cents, tax_cents, ferry_fee_cents,
                                  tip_cents, total_cents, dropoff_label, dropoff_lat, dropoff_lng, handoff_code,
                                  delivered_at, walk_distance_m, created_at)
        VALUES
            ('ord_old', 'run_old', 'usr_judge', 'delivered', 1000, 75, 150, 100, 1325,
             'Hinton James lobby', 35.902364, -79.043231, '4829', now() - interval '25 hours', NULL, now() - interval '27 hours'),
            -- Already measured: retention leaves it alone.
            ('ord_old_measured', 'run_old', 'usr_dana', 'settled', 1000, 75, 150, 100, 1325,
             'Hinton James lobby', 35.902364, -79.043231, '1111', now() - interval '25 hours', 777, now() - interval '27 hours'),
            ('ord_recent', 'run_recent', 'usr_judge', 'delivered', 1000, 75, 150, 100, 1325,
             'Hinton James lobby', 35.902364, -79.043231, '4829', now() - interval '1 hour', NULL, now() - interval '2 hours'),
            ('ord_live', 'run_live', 'usr_judge', 'picked_up', 1000, 75, 150, 100, 1325,
             'Hinton James lobby', 35.902364, -79.043231, '4829', NULL, NULL, now() - interval '3 days'),
            ('ord_cancelled', 'run_cancelled', 'usr_judge', 'refunded', 1000, 75, 150, 100, 1325,
             'Hinton James lobby', 35.902364, -79.043231, '4829', NULL, NULL, now() - interval '3 days');
        -- ord_old was picked up 26 hours ago, after the walk to the restaurant.
        INSERT INTO ferry.order_events (order_id, run_id, ts, status) VALUES
            ('ord_old', 'run_old', now() - interval '26 hours', 'picked_up');
        -- run_old: 100 m north before pickup (not counted), resting jitter, then 200 m north.
        INSERT INTO ferry.location_pings (run_id, user_id, ts, lat, lng, accuracy_m) VALUES
            ('run_old', 'usr_maya', now() - interval '26 hours 10 minutes', 35.9000, -79.05, 5),
            ('run_old', 'usr_maya', now() - interval '26 hours 5 minutes',  35.9009, -79.05, 5),
            ('run_old', 'usr_maya', now() - interval '26 hours',            35.9009, -79.05, 5),
            ('run_old', 'usr_maya', now() - interval '25 hours 50 minutes', 35.90093, -79.05, 6),
            ('run_old', 'usr_maya', now() - interval '25 hours 40 minutes', 35.9018, -79.05, 5),
            ('run_old', 'usr_maya', now() - interval '25 hours 30 minutes', 35.9027, -79.05, 5),
            ('run_recent', 'usr_maya', now() - interval '70 minutes', 35.9, -79.05, 5),
            ('run_live', 'usr_maya', now() - interval '2 days', 35.9, -79.05, 5),
            ('run_cancelled', 'usr_maya', now() - interval '26 hours', 35.9, -79.05, 5),
            ('run_cancelled', 'usr_maya', now() - interval '25 hours', 35.9001, -79.05, 5);
        "##,
    )
    .await;
}

async fn ping_count(pool: &PgPool, run_id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM ferry.location_pings WHERE run_id = $1")
        .bind(run_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn walk_distance(pool: &PgPool, order_id: &str) -> Option<f64> {
    sqlx::query_scalar("SELECT walk_distance_m FROM ferry.orders WHERE id = $1")
        .bind(order_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../migrations")]
async fn pings_go_24_hours_after_the_run_ends_and_the_distance_stays(pool: PgPool) {
    seed(&pool).await;
    let sweep = sweep_once(&pool).await.unwrap();
    assert_eq!(sweep.runs, 2, "{sweep:?}");
    assert_eq!(sweep.pings_deleted, 8);
    assert_eq!(sweep.orders_measured, 1);

    assert_eq!(ping_count(&pool, "run_old").await, 0);
    assert_eq!(ping_count(&pool, "run_cancelled").await, 0);
    assert_eq!(
        ping_count(&pool, "run_recent").await,
        1,
        "delivered an hour ago"
    );
    assert_eq!(ping_count(&pool, "run_live").await, 1, "still active");

    // From the pickup on: 200 m north. The 100 m to the restaurant and the jitter don't count.
    let expected = haversine_m(latlng(35.9009, -79.05), latlng(35.9027, -79.05));
    let measured = walk_distance(&pool, "ord_old").await.unwrap();
    assert!(
        (measured - expected.round()).abs() < 0.5,
        "{measured} vs {expected}"
    );
    assert_eq!(walk_distance(&pool, "ord_old_measured").await, Some(777.0));
    assert_eq!(walk_distance(&pool, "ord_recent").await, None);
    assert_eq!(
        walk_distance(&pool, "ord_cancelled").await,
        None,
        "never delivered"
    );

    // The orders themselves, their timing and their timeline stay.
    let delivered: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ferry.orders WHERE run_id = 'run_old' AND delivered_at IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(delivered, 2);
    let events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ferry.order_events WHERE run_id = 'run_old'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(events, 1);

    // A second sweep finds nothing.
    let again = sweep_once(&pool).await.unwrap();
    assert_eq!(again.runs, 0);
}

#[sqlx::test(migrations = "../migrations")]
async fn data_age_and_visibility_helpers(pool: PgPool) {
    seed(&pool).await;
    exec(
        &pool,
        "INSERT INTO ferry.location_pings (run_id, user_id, ts, lat, lng, accuracy_m) VALUES \
           ('run_live', 'usr_maya', now() - interval '25 seconds', 35.9050, -79.0473, 5), \
           ('run_live', 'usr_maya', now() - interval '20 seconds', 35.9051, -79.0472, 5)",
    )
    .await;

    let latest = latest_ping(&pool, "run_live").await.unwrap().unwrap();
    assert_eq!(latest.location, latlng(35.9051, -79.0472));
    assert!(
        (Duration::from_secs(19)..Duration::from_secs(30)).contains(&latest.age),
        "{:?}",
        latest.age
    );
    assert!(
        latest.seen_ago().ends_with(" s ago"),
        "{}",
        latest.seen_ago()
    );
    assert!(latest.at.ends_with('Z'));
    let age = data_age(&pool, "run_live").await.unwrap().unwrap();
    assert!(age >= Duration::from_secs(19));
    assert_eq!(data_age(&pool, "run_nope").await.unwrap(), None);

    // Jordan has an active order on run_live and Maya runs it; Dana sees nothing.
    let seen = visible_runner_location(&pool, "run_live", "usr_judge")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(seen.location, latest.location);
    assert!(
        visible_runner_location(&pool, "run_live", "usr_maya")
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        visible_runner_location(&pool, "run_live", "usr_dana")
            .await
            .unwrap(),
        None
    );
    // run_recent is delivered: nobody sees it, not even its runner.
    assert_eq!(
        visible_runner_location(&pool, "run_recent", "usr_maya")
            .await
            .unwrap(),
        None
    );
}

/// The latest ping is the last one received, so a ping whose `at` is ahead of the server's clock
/// (up to 2 minutes is accepted) does not hide the pings after it.
#[sqlx::test(migrations = "../migrations")]
async fn a_future_dated_ping_does_not_hide_later_pings(pool: PgPool) {
    seed(&pool).await;
    exec(
        &pool,
        "INSERT INTO ferry.location_pings (run_id, user_id, ts, lat, lng, accuracy_m) VALUES \
           ('run_live', 'usr_maya', now() + interval '110 seconds', 35.9000, -79.0500, 5)",
    )
    .await;
    exec(
        &pool,
        "INSERT INTO ferry.location_pings (run_id, user_id, ts, lat, lng, accuracy_m) VALUES \
           ('run_live', 'usr_maya', now() - interval '1 second', 35.9100, -79.0400, 5)",
    )
    .await;
    let latest = latest_ping(&pool, "run_live").await.unwrap().unwrap();
    assert_eq!(latest.location, latlng(35.9100, -79.0400));
    let seen = visible_runner_location(&pool, "run_live", "usr_judge")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(seen.location, latlng(35.9100, -79.0400));
}

/// A lock held elsewhere never holds the sweep: with SHARE on the pings table (reads pass, the
/// delete waits) each due run is skipped after the lock timeout and nothing changes; with ACCESS
/// EXCLUSIVE (a migration, say) even the list waits, and the sweep is postponed. Once the locks
/// are gone, the next sweep does the work.
#[sqlx::test(migrations = "../migrations")]
async fn a_held_lock_never_holds_the_sweep(pool: PgPool) {
    seed(&pool).await;

    let mut other = pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE ferry.location_pings IN SHARE MODE")
        .execute(&mut *other)
        .await
        .unwrap();
    let started = Instant::now();
    let sweep = sweep_with_lock_timeout(&pool, "200ms").await.unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(sweep.runs, 0, "{sweep:?}");
    assert_eq!(sweep.runs_skipped, 2, "{sweep:?}");
    assert!(!sweep.postponed);
    other.rollback().await.unwrap();
    assert_eq!(ping_count(&pool, "run_old").await, 6);
    assert_eq!(walk_distance(&pool, "ord_old").await, None, "rolled back");

    let mut other = pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE ferry.location_pings IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *other)
        .await
        .unwrap();
    let started = Instant::now();
    let sweep = sweep_with_lock_timeout(&pool, "200ms").await.unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert!(sweep.postponed, "{sweep:?}");
    assert_eq!((sweep.runs, sweep.runs_skipped), (0, 0));
    other.rollback().await.unwrap();

    let sweep = sweep_once(&pool).await.unwrap();
    assert_eq!(sweep.runs, 2, "{sweep:?}");
    assert_eq!(sweep.pings_deleted, 8);
    // The setting was for the sweep's own transactions only.
    let timeout: String = sqlx::query_scalar("SHOW lock_timeout")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(timeout, "0");
}
