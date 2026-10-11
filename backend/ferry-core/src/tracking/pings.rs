//! `POST /v1/runs/{id}/location`: the runner's app, about every 5 s while the run is active. Each
//! accepted ping is stored in `ferry.location_pings` and sent as `runner.location` to the run's
//! runner and its customers with an active order, each copy with that viewer's own ETA.
//! Also the latest-ping helpers the agent uses for "seen 20 s ago".

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use sqlx::PgPool;

use super::geo::{LatLng, is_valid, latlng};
use super::plan::Plan;
use super::track::{RunnerLocation, active_orders, load_context};
use crate::error::{ApiError, ApiResult};
use crate::extract::{Json, Path};
use crate::jwt::AuthUser;
use crate::notifications::Event;
use crate::state::AppState;

/// How far in the future a ping's `at` may be (phone clocks drift).
pub const MAX_CLOCK_AHEAD: Duration = Duration::from_secs(120);

/// How old a ping's `at` may be: a phone that queued fixes while offline may send them late, but
/// a fix older than this says nothing about where the runner is.
pub const MAX_PING_AGE: Duration = Duration::from_secs(10 * 60);

/// The largest `accuracy_m` accepted, in meters. A phone's worst fixes (cell towers) are a few
/// kilometers; anything above this is not a measurement.
pub const MAX_ACCURACY_M: f64 = 10_000.0;

/// `LocationPing` in contract/openapi.yaml.
#[derive(Debug, Clone, Deserialize)]
pub struct LocationPing {
    pub location: LatLng,
    pub accuracy_m: f64,
    pub at: String,
}

/// A run's latest ping and how old it is.
#[derive(Debug, Clone, PartialEq)]
pub struct LatestPing {
    pub location: LatLng,
    /// When the runner's app measured it, ISO 8601 UTC.
    pub at: String,
    /// Now minus `at`; zero when `at` is ahead of the server's clock.
    pub age: Duration,
}

impl LatestPing {
    pub(crate) fn new(location: LatLng, at: String, age_seconds: f64) -> Self {
        let age = if age_seconds.is_finite() && age_seconds > 0.0 {
            Duration::from_secs_f64(age_seconds)
        } else {
            Duration::ZERO
        };
        Self { location, at, age }
    }

    /// The contract's `{location, at}`.
    pub fn runner_location(&self) -> RunnerLocation {
        RunnerLocation {
            location: self.location,
            at: self.at.clone(),
        }
    }

    /// "seen 20 s ago" wording for the agent.
    pub fn seen_ago(&self) -> String {
        seen_ago(self.age)
    }
}

/// `age` as the agent says it: "just now", "20 s ago", "3 min ago", "2 h ago".
pub fn seen_ago(age: Duration) -> String {
    let secs = age.as_secs();
    match secs {
        0..5 => "just now".to_owned(),
        5..60 => format!("{secs} s ago"),
        60..3600 => format!("{} min ago", secs / 60),
        _ => format!("{} h ago", secs / 3600),
    }
}

/// The run's latest ping, whoever asks. The agent must apply the visibility rule itself (or use
/// `tracking::visible_runner_location`) before telling anyone where the runner is.
///
/// "Latest" is the last one the server received (the highest id), not the highest `at`: a ping
/// whose `at` is up to [`MAX_CLOCK_AHEAD`] in the future must not hide the pings after it.
pub async fn latest_ping(pool: &PgPool, run_id: &str) -> Result<Option<LatestPing>, sqlx::Error> {
    let row: Option<(f64, f64, String, f64)> = sqlx::query_as(
        r#"SELECT p.lat, p.lng,
                  to_char(p.ts AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
                  extract(epoch FROM now() - p.ts)::float8
           FROM ferry.location_pings p
           WHERE p.run_id = $1
           ORDER BY p.id DESC LIMIT 1"#,
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(lat, lng, at, age)| LatestPing::new(latlng(lat, lng), at, age)))
}

/// How old the run's latest ping is; `None` before the first ping (or after retention deleted
/// them).
pub async fn data_age(pool: &PgPool, run_id: &str) -> Result<Option<Duration>, sqlx::Error> {
    Ok(latest_ping(pool, run_id).await?.map(|p| p.age))
}

/// A strict RFC 3339 date-time shape: `YYYY-MM-DDTHH:MM:SS`, optional fraction, then `Z` or a
/// `±HH:MM` offset. Postgres checks the ranges when it parses the value.
pub fn is_rfc3339(value: &str) -> bool {
    let b = value.as_bytes();
    let digits = |range: std::ops::Range<usize>| {
        b.get(range.clone())
            .is_some_and(|s| s.len() == range.len() && s.iter().all(u8::is_ascii_digit))
    };
    let fixed = digits(0..4)
        && b.get(4) == Some(&b'-')
        && digits(5..7)
        && b.get(7) == Some(&b'-')
        && digits(8..10)
        && matches!(b.get(10), Some(b'T' | b't'))
        && digits(11..13)
        && b.get(13) == Some(&b':')
        && digits(14..16)
        && b.get(16) == Some(&b':')
        && digits(17..19);
    if !fixed {
        return false;
    }
    let mut i = 19;
    if b.get(i) == Some(&b'.') {
        let start = i + 1;
        i = start;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    match &b[i..] {
        [b'Z' | b'z'] => true,
        [b'+' | b'-', h1, h2, b':', m1, m2] => [h1, h2, m1, m2].iter().all(|c| c.is_ascii_digit()),
        _ => false,
    }
}

/// A Postgres date-time parse or range error as 400: 22007 (format), 22008 (field out of range,
/// such as February 30) or 22009 (an offset such as `+16:00`).
fn bad_time(err: sqlx::Error) -> ApiError {
    match &err {
        sqlx::Error::Database(db)
            if matches!(db.code().as_deref(), Some("22007" | "22008" | "22009")) =>
        {
            ApiError::validation("at must be an ISO 8601 date-time.")
        }
        _ => err.into(),
    }
}

pub(crate) async fn post_location(
    State(state): State<AppState>,
    user: AuthUser,
    Path(run_id): Path<String>,
    Json(ping): Json<LocationPing>,
) -> ApiResult<StatusCode> {
    if crate::demo::simulator::ignores_phone_ping(&state, &run_id, &user.user_id) {
        return Ok(StatusCode::NO_CONTENT);
    }
    accept_ping(&state, &run_id, &user.user_id, &ping).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The whole of `POST /v1/runs/{id}/location` after authentication, for `runner_id`: the checks
/// (in docs/DECISIONS.md's order), the insert, then `runner.location` to each connected viewer.
/// The demo's runner simulator posts through here, so its pings behave like a phone's.
pub async fn accept_ping(
    state: &AppState,
    run_id: &str,
    runner_id: &str,
    ping: &LocationPing,
) -> ApiResult<()> {
    if !is_valid(ping.location) {
        return Err(ApiError::validation(
            "location needs lat from -90 to 90 and lng from -180 to 180.",
        ));
    }
    if !(0.0..=MAX_ACCURACY_M).contains(&ping.accuracy_m) {
        return Err(ApiError::validation(
            "accuracy_m must be a number of meters from 0 to 10000.",
        ));
    }
    if !is_rfc3339(&ping.at) {
        return Err(ApiError::validation(
            "at must be an ISO 8601 date-time such as 2026-10-10T22:04:12Z.",
        ));
    }

    let run: Option<(String, String, bool, bool)> = sqlx::query_as(
        "SELECT r.runner_id, r.status, \
                $2::timestamptz > now() + make_interval(secs => $3), \
                $2::timestamptz < now() - make_interval(secs => $4) \
         FROM ferry.runs r WHERE r.id = $1",
    )
    .bind(run_id)
    .bind(&ping.at)
    .bind(MAX_CLOCK_AHEAD.as_secs_f64())
    .bind(MAX_PING_AGE.as_secs_f64())
    .fetch_optional(&state.pool)
    .await
    .map_err(bad_time)?;
    let Some((owner_id, status, in_future, too_old)) = run else {
        return Err(ApiError::not_found("No such run."));
    };
    if owner_id != runner_id {
        return Err(ApiError::forbidden(
            "Only the run's runner reports its location.",
        ));
    }
    if in_future {
        return Err(ApiError::validation(
            "at is more than 2 minutes in the future.",
        ));
    }
    if too_old {
        return Err(ApiError::validation(
            "at is more than 10 minutes in the past.",
        ));
    }
    if !super::track::ACTIVE_RUN_STATUSES.contains(&status.as_str()) {
        return Err(ApiError::conflict(format!(
            "The run is {status}; location is recorded only while it is active."
        )));
    }

    // The status check again, in the insert itself, in case the run was delivered meanwhile.
    let measured_at: Option<String> = sqlx::query_scalar(
        r#"INSERT INTO ferry.location_pings (run_id, user_id, ts, lat, lng, accuracy_m)
           SELECT r.id, r.runner_id, $3::timestamptz, $4, $5, $6
           FROM ferry.runs r
           WHERE r.id = $1 AND r.runner_id = $2
             AND r.status IN ('open', 'closed', 'purchased', 'delivering')
           RETURNING to_char(ts AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"')"#,
    )
    .bind(run_id)
    .bind(runner_id)
    .bind(&ping.at)
    .bind(ping.location.lat)
    .bind(ping.location.lng)
    .bind(ping.accuracy_m)
    .fetch_optional(&state.pool)
    .await
    .map_err(bad_time)?;
    let Some(measured_at) = measured_at else {
        return Err(ApiError::conflict(
            "The run is no longer active; location is not recorded.",
        ));
    };

    // Routing can take seconds when OSRM is slow, so the runner's app gets its 204 first.
    tokio::spawn(publish_location(
        state.clone(),
        run_id.to_owned(),
        ping.location,
        measured_at,
    ));
    Ok(())
}

/// Sends `runner.location` for one ping to the run's runner and its customers with an active
/// order who are connected, each with their own ETA. Nothing after delivery.
pub(crate) async fn publish_location(
    state: AppState,
    run_id: String,
    location: LatLng,
    measured_at: String,
) {
    if let Err(err) = try_publish(&state, &run_id, location, &measured_at).await {
        tracing::warn!(run_id, error = %err, "runner.location not sent");
    }
}

async fn try_publish(
    state: &AppState,
    run_id: &str,
    location: LatLng,
    measured_at: &str,
) -> Result<usize, sqlx::Error> {
    let Some(context) = load_context(&state.pool, run_id, "").await? else {
        return Ok(0);
    };
    if !context.is_active() {
        return Ok(0);
    }
    let orders = active_orders(&state.pool, run_id).await?;
    let mut recipients = vec![context.runner_id.clone()];
    recipients.extend(orders.iter().map(|o| o.customer_id.clone()));
    recipients.sort();
    recipients.dedup();
    if recipients
        .iter()
        .all(|user| state.hub.connection_count(user) == 0)
    {
        return Ok(0);
    }
    let plan = Plan::build(
        &state.routing,
        location,
        context.restaurant_first(),
        &orders,
        context.pace(),
    )
    .await;
    // Every recipient is the runner or has a stop, so each gets their own ETA.
    let runner_id = &context.runner_id;
    Ok(state.hub.send_to_many(&recipients, |user| {
        let eta = plan.eta_for(user, runner_id).unwrap_or(0.0);
        Event::runner_location(run_id, &location, measured_at, eta)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_shapes() {
        for ok in [
            "2026-10-10T22:04:12Z",
            "2026-10-10T22:04:12.123Z",
            "2026-10-10t22:04:12z",
            "2026-10-10T18:04:12-04:00",
            "2026-10-10T22:04:12.5+00:00",
        ] {
            assert!(is_rfc3339(ok), "{ok}");
        }
        for bad in [
            "",
            "now",
            "today",
            "2026-10-10",
            "2026-10-10 22:04:12Z",
            "2026-10-10T22:04:12",
            "2026-10-10T22:04Z",
            "2026-10-10T22:04:12.Z",
            "2026-10-10T22:04:12+0400",
            "2026-10-10T22:04:12Z extra",
            "20261-10-10T22:04:12Z",
            "2026-1a-10T22:04:12Z",
        ] {
            assert!(!is_rfc3339(bad), "{bad}");
        }
    }

    #[test]
    fn seen_ago_wording() {
        assert_eq!(seen_ago(Duration::from_secs(2)), "just now");
        assert_eq!(seen_ago(Duration::from_secs(20)), "20 s ago");
        assert_eq!(seen_ago(Duration::from_secs(185)), "3 min ago");
        assert_eq!(seen_ago(Duration::from_secs(7300)), "2 h ago");
    }

    #[test]
    fn age_is_never_negative() {
        let ahead = LatestPing::new(latlng(0.0, 0.0), "x".into(), -30.0);
        assert_eq!(ahead.age, Duration::ZERO);
        let old = LatestPing::new(latlng(0.0, 0.0), "x".into(), 20.4);
        assert_eq!(old.age.as_secs(), 20);
        assert_eq!(old.seen_ago(), "20 s ago");
    }
}
