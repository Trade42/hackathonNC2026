//! Location retention (spec section 5): a run's pings are deleted 24 hours after it ends, and the
//! walk's distance stays on its orders.
//!
//! - A run ends when it is delivered (at its last order's `delivered_at`) or cancelled (at its
//!   last ping). `ferry.runs` has no delivery time of its own.
//! - Before the delete, each delivered order whose `walk_distance_m` is still null gets the
//!   distance the runner walked from its pickup (the order's `picked_up` event, else the first
//!   ping) to its `delivered_at`, measured over the pings. `delivered_at` (the timing) is already
//!   on the order and its timeline stays in `ferry.order_events`.
//! - [`start`] runs [`sweep_once`] at boot and every 10 minutes.

use std::time::Duration;

use sqlx::PgPool;

use super::geo::{LatLng, haversine_m, latlng};
use crate::state::AppState;

/// How long a run's pings outlive the run.
pub const PING_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

/// How often the sweeper looks.
pub const SWEEP_EVERY: Duration = Duration::from_secs(10 * 60);

/// GPS jitter: the walk only counts a move once the runner is this far from the last counted
/// point, so a phone resting at the restaurant adds nothing.
pub const MIN_STEP_M: f64 = 15.0;

/// Fixes less precise than this are left out of the walk's distance.
pub const MAX_ACCURACY_M: f64 = 50.0;

/// Runs handled per sweep; the rest wait for the next one.
const RUNS_PER_SWEEP: i64 = 100;

/// How long one run's sweep waits for a lock (`lock_timeout`, in Postgres's syntax). A run whose
/// rows another transaction holds longer is skipped until the next sweep, so the sweeper never
/// holds a pool connection waiting.
pub const SWEEP_LOCK_TIMEOUT: &str = "5s";

/// lock_not_available: `lock_timeout` ran out.
const LOCK_NOT_AVAILABLE: &str = "55P03";

/// What one sweep did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sweep {
    pub runs: u64,
    pub pings_deleted: u64,
    pub orders_measured: u64,
    /// Runs left for the next sweep because a lock did not come within the lock timeout.
    pub runs_skipped: u64,
    /// True when even the list of ended runs waited too long for a lock (a migration holding
    /// the pings table, say): the whole sweep is left for the next one.
    pub postponed: bool,
}

/// Starts the retention sweeper on the current Tokio runtime: once now, then every
/// [`SWEEP_EVERY`]. main.rs calls it once after building the state.
pub fn start(state: &AppState) -> tokio::task::JoinHandle<()> {
    let pool = state.pool.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(SWEEP_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            match sweep_once(&pool).await {
                Ok(sweep) if sweep.runs > 0 || sweep.runs_skipped > 0 || sweep.postponed => {
                    tracing::info!(
                        runs = sweep.runs,
                        pings_deleted = sweep.pings_deleted,
                        orders_measured = sweep.orders_measured,
                        runs_skipped = sweep.runs_skipped,
                        postponed = sweep.postponed,
                        "location retention: deleted pings of ended runs"
                    )
                }
                Ok(_) => {}
                Err(err) => tracing::warn!(error = %err, "location retention sweep failed"),
            }
        }
    })
}

/// Deletes the pings of every run that ended more than [`PING_RETENTION`] ago, after recording
/// each delivered order's walk distance. One transaction per run. No statement waits more than
/// [`SWEEP_LOCK_TIMEOUT`] for a lock: a run that times out is skipped, and a sweep that cannot
/// even list the runs is postponed, until the next sweep.
pub async fn sweep_once(pool: &PgPool) -> Result<Sweep, sqlx::Error> {
    sweep_with_lock_timeout(pool, SWEEP_LOCK_TIMEOUT).await
}

/// [`sweep_once`] with another lock timeout, in Postgres's syntax (`"200ms"`), for tests.
pub async fn sweep_with_lock_timeout(
    pool: &PgPool,
    lock_timeout: &str,
) -> Result<Sweep, sqlx::Error> {
    let mut sweep = Sweep::default();
    let runs = match ended_runs(pool, lock_timeout).await {
        Ok(runs) => runs,
        Err(err) if is_lock_timeout(&err) => {
            tracing::debug!("location retention: the pings table is locked; sweep postponed");
            sweep.postponed = true;
            return Ok(sweep);
        }
        Err(err) => return Err(err),
    };
    for run_id in runs {
        match sweep_run(pool, &run_id, lock_timeout).await {
            Ok((orders, pings)) => {
                sweep.runs += 1;
                sweep.orders_measured += orders;
                sweep.pings_deleted += pings;
            }
            Err(err) if is_lock_timeout(&err) => {
                tracing::debug!(run_id, "location retention: run locked; skipped");
                sweep.runs_skipped += 1;
            }
            Err(err) => return Err(err),
        }
    }
    Ok(sweep)
}

fn is_lock_timeout(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db) if db.code().as_deref() == Some(LOCK_NOT_AVAILABLE))
}

/// Begins a transaction whose statements wait at most `lock_timeout` for a lock (SET LOCAL: the
/// connection goes back to the pool unchanged).
async fn begin_with_lock_timeout(
    pool: &PgPool,
    lock_timeout: &str,
) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('lock_timeout', $1, true)")
        .bind(lock_timeout)
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

/// The runs whose pings are due for deletion, at most [`RUNS_PER_SWEEP`].
async fn ended_runs(pool: &PgPool, lock_timeout: &str) -> Result<Vec<String>, sqlx::Error> {
    let mut tx = begin_with_lock_timeout(pool, lock_timeout).await?;
    let runs: Vec<String> = sqlx::query_scalar(
        "SELECT r.id FROM ferry.runs r \
         WHERE r.status IN ('delivered', 'cancelled') \
           AND EXISTS (SELECT 1 FROM ferry.location_pings p WHERE p.run_id = r.id) \
           AND coalesce( \
                 (SELECT max(o.delivered_at) FROM ferry.orders o WHERE o.run_id = r.id), \
                 (SELECT max(p.ts) FROM ferry.location_pings p WHERE p.run_id = r.id) \
               ) < now() - make_interval(secs => $1) \
         ORDER BY r.id LIMIT $2",
    )
    .bind(PING_RETENTION.as_secs_f64())
    .bind(RUNS_PER_SWEEP)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(runs)
}

/// One stored ping: seconds since the epoch, position, accuracy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fix {
    pub t: f64,
    pub at: LatLng,
    pub accuracy_m: f64,
}

async fn sweep_run(
    pool: &PgPool,
    run_id: &str,
    lock_timeout: &str,
) -> Result<(u64, u64), sqlx::Error> {
    let mut tx = begin_with_lock_timeout(pool, lock_timeout).await?;
    let fixes: Vec<Fix> = sqlx::query_as::<_, (f64, f64, f64, f64)>(
        "SELECT extract(epoch FROM p.ts)::float8, p.lat, p.lng, p.accuracy_m \
         FROM ferry.location_pings p WHERE p.run_id = $1 ORDER BY p.ts, p.id",
    )
    .bind(run_id)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|(t, lat, lng, accuracy_m)| Fix {
        t,
        at: latlng(lat, lng),
        accuracy_m,
    })
    .collect();

    let orders: Vec<(String, Option<f64>, f64)> = sqlx::query_as(
        "SELECT o.id, \
                (SELECT extract(epoch FROM min(e.ts))::float8 FROM ferry.order_events e \
                 WHERE e.order_id = o.id AND e.status = 'picked_up'), \
                extract(epoch FROM o.delivered_at)::float8 \
         FROM ferry.orders o \
         WHERE o.run_id = $1 AND o.walk_distance_m IS NULL AND o.delivered_at IS NOT NULL",
    )
    .bind(run_id)
    .fetch_all(&mut *tx)
    .await?;
    let mut measured = 0;
    for (order_id, picked_up, delivered) in orders {
        let meters = walked_m(&fixes, picked_up.unwrap_or(f64::NEG_INFINITY), delivered);
        measured += sqlx::query(
            "UPDATE ferry.orders SET walk_distance_m = $2 \
             WHERE id = $1 AND walk_distance_m IS NULL",
        )
        .bind(&order_id)
        .bind(meters.round())
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }

    let deleted = sqlx::query("DELETE FROM ferry.location_pings WHERE run_id = $1")
        .bind(run_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok((measured, deleted))
}

/// Meters walked between `from` and `to` (epoch seconds, inclusive) over `fixes` in time order.
/// Fixes worse than [`MAX_ACCURACY_M`] are skipped, and a move counts only once it reaches
/// [`MIN_STEP_M`] from the last counted point.
pub fn walked_m(fixes: &[Fix], from: f64, to: f64) -> f64 {
    let mut window = fixes
        .iter()
        .filter(|f| f.t >= from && f.t <= to && f.accuracy_m <= MAX_ACCURACY_M);
    let Some(first) = window.next() else {
        return 0.0;
    };
    let mut anchor = first.at;
    let mut total = 0.0;
    for fix in window {
        let step = haversine_m(anchor, fix.at);
        if step >= MIN_STEP_M {
            total += step;
            anchor = fix.at;
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fix(t: f64, lat: f64, lng: f64, accuracy_m: f64) -> Fix {
        Fix {
            t,
            at: latlng(lat, lng),
            accuracy_m,
        }
    }

    #[test]
    fn walked_distance_ignores_jitter_bad_fixes_and_the_window_edges() {
        // 0.0001 degrees of latitude is about 11.1 m.
        let fixes = [
            fix(0.0, 35.9000, -79.05, 5.0),
            // Jitter at the start: within 15 m of the first fix.
            fix(5.0, 35.90005, -79.05, 5.0),
            fix(10.0, 35.89996, -79.05, 5.0),
            // A real walk north: 100 m, then 100 m more.
            fix(15.0, 35.9009, -79.05, 5.0),
            fix(20.0, 35.9018, -79.05, 5.0),
            // A wild fix (300 m off, 120 m accuracy) is skipped.
            fix(25.0, 35.9045, -79.05, 120.0),
            fix(30.0, 35.9018, -79.05, 5.0),
            // After delivery.
            fix(100.0, 35.9100, -79.05, 5.0),
        ];
        let meters = walked_m(&fixes, f64::NEG_INFINITY, 30.0);
        let expected = haversine_m(latlng(35.9, -79.05), latlng(35.9018, -79.05));
        assert!((meters - expected).abs() < 1.0, "{meters} vs {expected}");
        // From the pickup at t = 15: only the second 100 m.
        let from_pickup = walked_m(&fixes, 15.0, 30.0);
        assert!((from_pickup - expected / 2.0).abs() < 1.0, "{from_pickup}");
        assert_eq!(walked_m(&fixes, 200.0, 300.0), 0.0);
        assert_eq!(walked_m(&[], f64::NEG_INFINITY, 1.0), 0.0);
    }
}
