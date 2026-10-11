//! The requests sweep: every [`SWEEP_EVERY`], one worker per process ([`start`], from main.rs).
//!
//! 1. **Expire.** Requests still `searching` at `needed_by` become `expired`, each with one
//!    `system` notification to its customer, in one transaction (`FOR UPDATE SKIP LOCKED`, at
//!    most [`REQUESTS_PER_SWEEP`]). If that transaction fails, the sweep logs it and still
//!    matches; the next sweep expires them.
//! 2. **Match.** Every request still searching (the `pickup_requests_searching_needed_by_idx`
//!    partial index, soonest deadline first, at most [`REQUESTS_PER_SWEEP`]) goes through
//!    [`matcher::match_request`] against the runs open now, so a run that opened since the request
//!    was made can take it. The claim is guarded by `status = 'searching'`, so a request is never
//!    matched twice, even with the request's own at-once match or another instance racing.
//!
//! Each transaction sets `lock_timeout` ([`matcher::SET_LOCK_TIMEOUT`], 2 s), so the sweep never queues
//! behind a long lock; a request it could not lock waits for the next sweep.

use std::time::Duration;

use super::{matcher, notices};
use crate::error::ApiResult;
use crate::notifications::Event;
use crate::state::AppState;

/// How often the sweep runs.
pub const SWEEP_EVERY: Duration = Duration::from_secs(30);
/// Requests expired, and requests matched, per sweep at most; the rest wait for the next one.
pub const REQUESTS_PER_SWEEP: i64 = 100;

/// What one sweep did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sweep {
    pub expired: u64,
    pub matched: u64,
}

/// Starts the sweep on the current Tokio runtime: once now, then every [`SWEEP_EVERY`]. main.rs
/// calls it once after building the state. Sweeps never overlap: the next waits for this one.
pub fn start(state: &AppState) -> tokio::task::JoinHandle<()> {
    let state = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(SWEEP_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            match sweep_once(&state).await {
                Ok(sweep) if sweep != Sweep::default() => tracing::info!(
                    expired = sweep.expired,
                    matched = sweep.matched,
                    "pickup requests sweep"
                ),
                Ok(_) => {}
                Err(err) => tracing::warn!(error = %err, "pickup requests sweep failed"),
            }
        }
    })
}

/// One sweep: expire, then match. An expiry that fails is logged once and counts nothing; the
/// matching still runs. A request whose matching fails is logged and skipped.
pub async fn sweep_once(state: &AppState) -> ApiResult<Sweep> {
    let expired = match expire(state).await {
        Ok(expired) => expired,
        Err(err) => {
            tracing::warn!(error = %err, "pickup requests sweep: expiry failed; matching anyway");
            0
        }
    };
    let searching: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM ferry.pickup_requests \
         WHERE status = 'searching' AND needed_by > now() \
         ORDER BY needed_by, id LIMIT $1",
    )
    .bind(REQUESTS_PER_SWEEP)
    .fetch_all(&state.pool)
    .await?;
    let mut matched = 0;
    for request_id in searching {
        match matcher::match_request(state, &request_id).await {
            Ok(Some(_)) => matched += 1,
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(request_id, error = %err, "pickup requests sweep: matching failed");
            }
        }
    }
    Ok(Sweep { expired, matched })
}

/// Expires the requests past `needed_by` and notifies each customer once.
async fn expire(state: &AppState) -> ApiResult<u64> {
    let mut tx = state.pool.begin().await?;
    sqlx::query(matcher::SET_LOCK_TIMEOUT)
        .execute(&mut *tx)
        .await?;
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "WITH due AS ( \
             SELECT id FROM ferry.pickup_requests \
             WHERE status = 'searching' AND needed_by <= now() \
             ORDER BY needed_by, id LIMIT $1 \
             FOR UPDATE SKIP LOCKED \
         ) \
         UPDATE ferry.pickup_requests pr SET status = 'expired' \
         FROM due, ferry.restaurants rs \
         WHERE pr.id = due.id AND rs.id = pr.restaurant_id \
         RETURNING pr.id, pr.customer_id, pr.restaurant_id, rs.name",
    )
    .bind(REQUESTS_PER_SWEEP)
    .fetch_all(&mut *tx)
    .await?;
    let mut notifications = Vec::with_capacity(rows.len());
    for (request_id, customer_id, restaurant_id, restaurant_name) in &rows {
        let notification = notices::expired(
            &mut tx,
            customer_id,
            request_id,
            restaurant_id,
            restaurant_name,
        )
        .await?;
        notifications.push((customer_id.clone(), notification));
    }
    tx.commit().await?;
    for (request_id, ..) in &rows {
        tracing::info!(request_id, "pickup request expired");
    }
    for (user_id, notification) in &notifications {
        state
            .hub
            .send_to(user_id, &Event::notification_created(notification));
    }
    Ok(rows.len() as u64)
}
