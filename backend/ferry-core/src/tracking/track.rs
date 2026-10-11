//! `GET /v1/runs/{id}/track`, and the visibility rule every runner-location reader shares.
//!
//! Who sees what (contract v1.2, CLAUDE.md's checklist):
//! - The run's runner and customers with an active order on it (funded, purchased or picked_up)
//!   get `runner_location` and `route` while the run is active (open, closed, purchased or
//!   delivering). After delivery (or cancellation) everyone gets nulls and no stops.
//! - The runner sees every stop. A customer sees only their own stops, with ETAs that count the
//!   runner's earlier stops; their route goes through the restaurant (before pickup) straight to
//!   their drop-off, so it never shows another customer's drop-off.
//! - Other members of the run's circle get 200 with `runner_location`, `route`, `distance_m` and
//!   `eta_minutes` null and no stops. Anyone else is 403 `forbidden`; an unknown run is 404.

use axum::extract::State;
use serde::Serialize;
use sqlx::PgPool;

use super::geo::{LatLng, Pace, Place, latlng};
use super::pings::LatestPing;
use super::plan::{ActiveOrder, Plan};
use super::routing::LineString;
use crate::error::{ApiError, ApiResult};
use crate::extract::{Json, Path};
use crate::jwt::AuthUser;
use crate::state::AppState;

/// `RunnerLocation` in contract/openapi.yaml.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunnerLocation {
    pub location: LatLng,
    /// When the runner's app measured it, ISO 8601 UTC.
    pub at: String,
}

/// `TrackStop` in contract/openapi.yaml.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrackStop {
    pub order_id: String,
    pub dropoff: Place,
    pub eta_minutes: f64,
}

/// `Track` in contract/openapi.yaml.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Track {
    pub runner_location: Option<RunnerLocation>,
    pub route: Option<LineString>,
    pub distance_m: Option<f64>,
    pub eta_minutes: Option<f64>,
    pub stops: Vec<TrackStop>,
}

impl Track {
    /// Nulls and no stops: what a viewer who may not see the runner gets.
    pub fn hidden() -> Self {
        Self {
            runner_location: None,
            route: None,
            distance_m: None,
            eta_minutes: None,
            stops: Vec::new(),
        }
    }
}

/// Run statuses in which the runner is still walking.
pub const ACTIVE_RUN_STATUSES: [&str; 4] = ["open", "closed", "purchased", "delivering"];

/// The run as tracking needs it, in one query.
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct RunContext {
    pub runner_id: String,
    pub status: String,
    pub restaurant_lat: f64,
    pub restaurant_lng: f64,
    pub walking_pace: String,
    pub viewer_is_member: bool,
    #[sqlx(flatten)]
    pub ping: PingRow,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct PingRow {
    pub ping_lat: Option<f64>,
    pub ping_lng: Option<f64>,
    pub ping_at: Option<String>,
    pub ping_age_s: Option<f64>,
}

impl PingRow {
    pub fn latest(&self) -> Option<LatestPing> {
        Some(LatestPing::new(
            latlng(self.ping_lat?, self.ping_lng?),
            self.ping_at.clone()?,
            self.ping_age_s?,
        ))
    }
}

impl RunContext {
    pub fn is_active(&self) -> bool {
        ACTIVE_RUN_STATUSES.contains(&self.status.as_str())
    }

    /// Before pickup the walk goes to the restaurant first.
    pub fn restaurant_first(&self) -> Option<LatLng> {
        (self.status != "delivering").then_some(latlng(self.restaurant_lat, self.restaurant_lng))
    }

    pub fn restaurant(&self) -> LatLng {
        latlng(self.restaurant_lat, self.restaurant_lng)
    }

    pub fn pace(&self) -> Pace {
        Pace::parse(&self.walking_pace).unwrap_or(Pace::Normal)
    }
}

/// Loads the run, its restaurant, its runner's pace, whether `viewer_id` is in its circle, and
/// its latest ping (the last received, as in `pings::latest_ping`). `None` for an unknown run.
pub(crate) async fn load_context(
    pool: &PgPool,
    run_id: &str,
    viewer_id: &str,
) -> Result<Option<RunContext>, sqlx::Error> {
    sqlx::query_as::<_, RunContext>(
        r#"SELECT r.runner_id, r.status,
                  rs.lat AS restaurant_lat, rs.lng AS restaurant_lng,
                  coalesce(pr.walking_pace, 'normal') AS walking_pace,
                  EXISTS (SELECT 1 FROM ferry.circle_members m
                          WHERE m.circle_id = r.circle_id AND m.user_id = $2) AS viewer_is_member,
                  lp.lat AS ping_lat, lp.lng AS ping_lng,
                  to_char(lp.ts AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS ping_at,
                  extract(epoch FROM now() - lp.ts)::float8 AS ping_age_s
           FROM ferry.runs r
           JOIN ferry.restaurants rs ON rs.id = r.restaurant_id
           LEFT JOIN ferry.profiles pr ON pr.user_id = r.runner_id
           LEFT JOIN LATERAL (
               SELECT p.lat, p.lng, p.ts FROM ferry.location_pings p
               WHERE p.run_id = r.id ORDER BY p.id DESC LIMIT 1
           ) lp ON true
           WHERE r.id = $1"#,
    )
    .bind(run_id)
    .bind(viewer_id)
    .fetch_optional(pool)
    .await
}

#[derive(sqlx::FromRow)]
struct OrderRow {
    id: String,
    customer_id: String,
    dropoff_place_id: Option<String>,
    dropoff_label: String,
    dropoff_lat: f64,
    dropoff_lng: f64,
    dropoff_note: Option<String>,
}

/// The run's active orders (funded, purchased or picked_up), oldest first.
pub(crate) async fn active_orders(
    pool: &PgPool,
    run_id: &str,
) -> Result<Vec<ActiveOrder>, sqlx::Error> {
    let rows = sqlx::query_as::<_, OrderRow>(
        "SELECT o.id, o.customer_id, o.dropoff_place_id, o.dropoff_label, o.dropoff_lat, \
                o.dropoff_lng, o.dropoff_note \
         FROM ferry.orders o \
         WHERE o.run_id = $1 AND o.status IN ('funded', 'purchased', 'picked_up') \
         ORDER BY o.created_at, o.id",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ActiveOrder {
            id: r.id,
            customer_id: r.customer_id,
            dropoff: Place {
                id: r.dropoff_place_id,
                label: r.dropoff_label,
                location: latlng(r.dropoff_lat, r.dropoff_lng),
                note: r.dropoff_note,
            },
        })
        .collect())
}

/// The track for `run_id` as `viewer_id` may see it (module docs). Other modules use it for
/// `Run.runner_location`, `Run.eta_minutes` and `Order.eta_minutes`.
pub async fn track_for(state: &AppState, run_id: &str, viewer_id: &str) -> ApiResult<Track> {
    let context = load_context(&state.pool, run_id, viewer_id)
        .await?
        .ok_or_else(|| ApiError::not_found("No such run."))?;
    let is_runner = context.runner_id == viewer_id;
    let orders = active_orders(&state.pool, run_id).await?;
    let is_customer = orders.iter().any(|o| o.customer_id == viewer_id);
    if !is_runner && !is_customer && !context.viewer_is_member {
        return Err(ApiError::forbidden(
            "Only the run's circle can see its track.",
        ));
    }
    if !context.is_active() || !(is_runner || is_customer) {
        return Ok(Track::hidden());
    }

    let ping = context.ping.latest();
    // Before the first ping, ETAs count from the restaurant: the walk after pickup.
    let (origin, restaurant_first) = match &ping {
        Some(p) => (p.location, context.restaurant_first()),
        None => (context.restaurant(), None),
    };
    let plan = Plan::build(
        &state.routing,
        origin,
        restaurant_first,
        &orders,
        context.pace(),
    )
    .await;

    let visible_stops: Vec<_> = plan
        .stops
        .iter()
        .filter(|s| is_runner || s.order.customer_id == viewer_id)
        .collect();
    let stops = visible_stops
        .iter()
        .map(|s| TrackStop {
            order_id: s.order.id.clone(),
            dropoff: s.order.dropoff.clone(),
            eta_minutes: s.eta_minutes,
        })
        .collect();
    let Some(ping) = ping else {
        return Ok(Track {
            stops,
            ..Track::hidden()
        });
    };

    let (route, distance_m, eta_minutes) = if is_runner {
        match plan.next_leg() {
            Some(leg) => (
                leg.geometry.clone().and_then(LineString::new),
                Some(leg.distance_m),
                Some(super::geo::eta_minutes(leg.distance_m, plan.pace)),
            ),
            None => (None, None, None),
        }
    } else {
        match visible_stops.first() {
            Some(stop) => (
                plan.route_to(&state.routing, stop)
                    .await
                    .and_then(LineString::new),
                Some(stop.distance_m),
                Some(stop.eta_minutes),
            ),
            None => (None, None, None),
        }
    };
    Ok(Track {
        runner_location: Some(ping.runner_location()),
        route,
        distance_m: distance_m.map(f64::round),
        eta_minutes,
        stops,
    })
}

/// The runner's latest location if `viewer_id` may see it (the run is active, and the viewer is
/// its runner or has an active order on it), else `None`. One query, no routing: for
/// `Run.runner_location` in lists and in each `run.updated` copy.
pub async fn visible_runner_location(
    pool: &PgPool,
    run_id: &str,
    viewer_id: &str,
) -> Result<Option<LatestPing>, sqlx::Error> {
    let row = sqlx::query_as::<_, PingRow>(
        r#"SELECT lp.lat AS ping_lat, lp.lng AS ping_lng,
                  to_char(lp.ts AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS ping_at,
                  extract(epoch FROM now() - lp.ts)::float8 AS ping_age_s
           FROM ferry.runs r
           JOIN LATERAL (
               SELECT p.lat, p.lng, p.ts FROM ferry.location_pings p
               WHERE p.run_id = r.id ORDER BY p.id DESC LIMIT 1
           ) lp ON true
           WHERE r.id = $1
             AND r.status IN ('open', 'closed', 'purchased', 'delivering')
             AND (r.runner_id = $2 OR EXISTS (
                 SELECT 1 FROM ferry.orders o
                 WHERE o.run_id = r.id AND o.customer_id = $2
                   AND o.status IN ('funded', 'purchased', 'picked_up')))"#,
    )
    .bind(run_id)
    .bind(viewer_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| r.latest()))
}

pub(crate) async fn get_track(
    State(state): State<AppState>,
    user: AuthUser,
    Path(run_id): Path<String>,
) -> ApiResult<Json<Track>> {
    Ok(Json(track_for(&state, &run_id, &user.user_id).await?))
}
