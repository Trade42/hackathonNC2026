//! Location pings, OSRM routes and ETAs: /v1/runs/{id}/location, /v1/runs/{id}/track.
//!
//! For other modules (every call always answers, from OSRM or the straight-line fallback):
//!
//! ```ignore
//! // A walk: distance, whole-minute ETA at the pace, and the GeoJSON route (None on the fallback).
//! let walk = tracking::walk(&state, from, to, Pace::Brisk).await;
//! // Driving meters for the CO2 estimate (OSRM's car route, else straight line × 1.3).
//! let meters = tracking::car_distance_m(&state, restaurant, dropoff).await;
//! // Minutes for a distance you already have.
//! let minutes = tracking::eta_minutes(walk.distance_m, Pace::Normal);
//! // The track as a viewer may see it: Run.runner_location, Run.eta_minutes, Order.eta_minutes.
//! let track = tracking::track_for(&state, &run_id, &viewer_id).await?;
//! // The runner's location for one viewer, or None (one query, no routing): Run.runner_location.
//! let seen = tracking::visible_runner_location(&state.pool, &run_id, &viewer_id).await?;
//! // How old the latest ping is, for the agent's "seen 20 s ago".
//! let ping = tracking::latest_ping(&state.pool, &run_id).await?; // ping.seen_ago()
//! // A ping as if the runner's app posted it (the demo's runner simulator): same checks and events.
//! tracking::accept_ping(&state, &run_id, &runner_id, &ping).await?;
//! ```

mod geo;
mod pings;
mod plan;
mod retention;
mod routing;
mod track;

use axum::Router;
use axum::routing::{get, post};

use crate::state::AppState;

pub use geo::{
    DETOUR_FACTOR, LatLng, Pace, Place, eta_minutes, fallback_distance_m, haversine_m, is_valid,
    latlng,
};
pub use pings::{
    LatestPing, LocationPing, MAX_CLOCK_AHEAD, accept_ping, data_age, latest_ping, seen_ago,
};
pub use plan::{ActiveOrder, Plan, PlannedStop};
pub use retention::{
    PING_RETENTION, SWEEP_EVERY, SWEEP_LOCK_TIMEOUT, Sweep, start, sweep_once,
    sweep_with_lock_timeout,
};
pub use routing::{
    LineString, OSRM_DEFAULT_BASE_URL, Profile, Route, RouteSource, Routing, RoutingSettings,
    USER_AGENT, Walk,
};
pub use track::{RunnerLocation, Track, TrackStop, track_for, visible_runner_location};
pub(crate) use track::{active_orders, load_context};

/// This module's routes, with full `/v1/...` paths. Merged into the app by `app::router`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/runs/{id}/location", post(pings::post_location))
        .route("/v1/runs/{id}/track", get(track::get_track))
}

/// A walk from `from` to `to` at `pace`: OSRM's foot route (cached 10 minutes), or the
/// straight line × 1.3 with no route when OSRM fails.
pub async fn walk(state: &AppState, from: LatLng, to: LatLng, pace: Pace) -> Walk {
    state.routing.walk(from, to, pace).await
}

/// Driving meters from `from` to `to`: OSRM's car route (cached 10 minutes), or the straight
/// line × 1.3. For the CO2 estimate (spec section 5).
pub async fn car_distance_m(state: &AppState, from: LatLng, to: LatLng) -> f64 {
    state.routing.car_distance_m(from, to).await
}
