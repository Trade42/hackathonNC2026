//! Detour-aware matching of a pickup request to an open run (spec M3: "extra time =
//! route(runner → restaurant → drop-off) minus route(runner → runner's destination); suggest
//! runners under 4 extra minutes").
//!
//! **Candidates** are the runs that are `open` with their window still ahead and closing before
//! `needed_by`, at the request's restaurant, in one of the customer's circles, not demo history,
//! and not the customer's own.
//!
//! **The runner's walk** after the restaurant is tracking's (`tracking::Plan`): the run's active
//! orders' drop-offs in nearest-neighbor order from the restaurant, then the runner's
//! destination. The destination is the run's `return_place`, else the runner's profile
//! `dropoff_default` (where they usually are). A run with no destination and no drop-offs gives
//! the matcher nothing to compare against: it is never scored, so it never fits.
//!
//! **Extra time** is the walk with the request's drop-off added minus the walk without it, at the
//! runner's pace, in minutes (not rounded). Candidates are open, so the runner has not picked up
//! yet: the walk to the restaurant is the same with and without the drop-off and cancels out.
//! Legs come from tracking's routing (OSRM, cached 10 minutes, or the straight line × 1.3). When
//! any leg of either walk is the fallback, every leg of both is measured the fallback's way, so
//! the difference never mixes OSRM's streets with the straight line.
//!
//! **A run fits** when all three hold:
//! - the extra time is at most [`MAX_EXTRA_MINUTES`];
//! - the food gets to the drop-off by `needed_by`: the pickup is at the window's close (or later,
//!   when the runner's recent ping says they cannot reach the restaurant by then), plus the walk
//!   from the restaurant through the earlier drop-offs;
//! - when the run has both a `return_place` and a `dropoff_radius_m`, the drop-off is within that
//!   radius of the return place, or within the request's own `dropoff_radius_m` of the runner's
//!   path (straight segments from the restaurant through the drop-offs to the return place).
//!   Distances here are straight lines, as the frontend's zone warning measures them.
//!
//! The fitting run with the least extra time wins (then the earliest food, then the run id). The
//! claim is one guarded update (`status = 'searching'`) under the run's share lock, so a request
//! is matched at most once and only to a run that is still open.
//!
//! **Nearby runs.** When no run to the request's own restaurant takes it, the open runs (same
//! candidate rules) to other restaurants within [`NEARBY_RESTAURANT_M`] of it, straight line
//! between the two restaurants, are scored the same way from their own restaurant. The runner of
//! each that fits gets one `system` suggestion to open a second Ferry to the request's restaurant
//! ([`notices::nearby`]), once per customer, restaurant and run. The request stays `searching`, the
//! customer is not told, and nothing else changes: when the runner opens that Ferry, the request
//! matches it by the rules above.

use std::cmp::Ordering;
use std::time::Duration;

use sqlx::PgPool;

use super::notices;
use crate::catalog::walk::EARTH_RADIUS_M;
use crate::error::ApiResult;
use crate::notifications::{Event, Notification};
use crate::runs::live::RECENT_PING;
use crate::state::AppState;
use crate::tracking::{
    self, ActiveOrder, LatLng, Pace, Place, Plan, Profile, Route, RouteSource, Routing,
    fallback_distance_m, haversine_m,
};

/// A run fits when the drop-off adds at most this many minutes to the runner's walk.
pub const MAX_EXTRA_MINUTES: f64 = 4.0;
/// A run to another restaurant at most this far (straight line, about a 4-minute walk) from the
/// request's restaurant gets a nearby suggestion when it fits.
pub const NEARBY_RESTAURANT_M: f64 = 300.0;
/// Runs scored per request, oldest first.
pub const CANDIDATES_PER_REQUEST: i64 = 20;
/// Every matcher and sweep transaction starts with this: it waits at most 2 s for a row lock.
pub const SET_LOCK_TIMEOUT: &str = "SET LOCAL lock_timeout = '2s'";
/// Scoring gives up after this long (OSRM can be slow); the request stays searching and the next
/// sweep tries again.
pub const SCORING_BUDGET: Duration = Duration::from_secs(8);

/// A searching request as the matcher reads it, with the database clock.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OpenRequest {
    pub id: String,
    pub customer_id: String,
    pub restaurant_id: String,
    pub restaurant_name: String,
    pub restaurant_lat: f64,
    pub restaurant_lng: f64,
    pub dropoff_lat: f64,
    pub dropoff_lng: f64,
    pub dropoff_radius_m: Option<i32>,
    /// `needed_by` in seconds since the epoch.
    pub needed_by_s: f64,
    /// The database's `now()` in seconds since the epoch.
    pub now_s: f64,
}

impl OpenRequest {
    pub fn dropoff(&self) -> LatLng {
        LatLng {
            lat: self.dropoff_lat,
            lng: self.dropoff_lng,
        }
    }

    /// Where the request's restaurant is.
    pub fn restaurant(&self) -> LatLng {
        LatLng {
            lat: self.restaurant_lat,
            lng: self.restaurant_lng,
        }
    }
}

#[derive(sqlx::FromRow)]
struct CandidateRow {
    run_id: String,
    runner_id: String,
    circle_id: String,
    runner_name: String,
    restaurant_name: String,
    closes_local: String,
    closes_at_s: f64,
    restaurant_lat: f64,
    restaurant_lng: f64,
    return_lat: Option<f64>,
    return_lng: Option<f64>,
    dropoff_radius_m: Option<i32>,
    home_lat: Option<f64>,
    home_lng: Option<f64>,
    pace: String,
    ping_lat: Option<f64>,
    ping_lng: Option<f64>,
}

/// An open run that might take the request.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub run_id: String,
    pub runner_id: String,
    pub circle_id: String,
    pub runner_name: String,
    pub restaurant_name: String,
    /// The window's close in Chapel Hill, "6:10 PM".
    pub closes_local: String,
    pub closes_at_s: f64,
    pub restaurant: LatLng,
    pub return_place: Option<LatLng>,
    pub dropoff_radius_m: Option<i32>,
    /// The runner's profile `dropoff_default`.
    pub runner_home: Option<LatLng>,
    pub pace: Pace,
    /// The runner's latest ping, when it is at most `RECENT_PING` old.
    pub ping: Option<LatLng>,
}

impl Candidate {
    /// Where the runner heads after the drop-offs: `return_place`, else their profile drop-off.
    pub fn destination(&self) -> Option<LatLng> {
        self.return_place.or(self.runner_home)
    }
}

fn both(lat: Option<f64>, lng: Option<f64>) -> Option<LatLng> {
    Some(LatLng {
        lat: lat?,
        lng: lng?,
    })
}

impl From<CandidateRow> for Candidate {
    fn from(row: CandidateRow) -> Self {
        Self {
            run_id: row.run_id,
            runner_id: row.runner_id,
            circle_id: row.circle_id,
            runner_name: row.runner_name,
            restaurant_name: row.restaurant_name,
            closes_local: row.closes_local,
            closes_at_s: row.closes_at_s,
            restaurant: LatLng {
                lat: row.restaurant_lat,
                lng: row.restaurant_lng,
            },
            return_place: both(row.return_lat, row.return_lng),
            dropoff_radius_m: row.dropoff_radius_m,
            runner_home: both(row.home_lat, row.home_lng),
            pace: Pace::parse(&row.pace).unwrap_or(Pace::Normal),
            ping: both(row.ping_lat, row.ping_lng),
        }
    }
}

/// The request if it is still searching and before `needed_by`.
pub async fn load_open(pool: &PgPool, request_id: &str) -> ApiResult<Option<OpenRequest>> {
    Ok(sqlx::query_as::<_, OpenRequest>(
        "SELECT pr.id, pr.customer_id, pr.restaurant_id, rs.name AS restaurant_name, \
                rs.lat AS restaurant_lat, rs.lng AS restaurant_lng, \
                pr.dropoff_lat, pr.dropoff_lng, pr.dropoff_radius_m, \
                extract(epoch FROM pr.needed_by)::float8 AS needed_by_s, \
                extract(epoch FROM now())::float8 AS now_s \
         FROM ferry.pickup_requests pr \
         JOIN ferry.restaurants rs ON rs.id = pr.restaurant_id \
         WHERE pr.id = $1 AND pr.status = 'searching' AND pr.needed_by > now()",
    )
    .bind(request_id)
    .fetch_optional(pool)
    .await?)
}

/// Which runs [`candidates_in`] reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Runs to the request's own restaurant.
    Same,
    /// Runs to another restaurant within [`NEARBY_RESTAURANT_M`] of it whose runner was not
    /// suggested this customer's request for that restaurant yet.
    Nearby,
}

/// The open runs to the request's own restaurant that might take `request`, oldest first, at most
/// [`CANDIDATES_PER_REQUEST`].
pub async fn candidates(pool: &PgPool, request: &OpenRequest) -> ApiResult<Vec<Candidate>> {
    candidates_in(pool, request, Scope::Same).await
}

/// The open runs in `scope` that might take `request`, oldest first, at most
/// [`CANDIDATES_PER_REQUEST`]. The distance filter is in the query, so runs elsewhere never
/// crowd the nearby ones out of the limit.
pub async fn candidates_in(
    pool: &PgPool,
    request: &OpenRequest,
    scope: Scope,
) -> ApiResult<Vec<Candidate>> {
    let rows: Vec<CandidateRow> = sqlx::query_as(
        "SELECT r.id AS run_id, r.runner_id, r.circle_id, \
                u.display_name AS runner_name, rs.name AS restaurant_name, \
                to_char(r.window_closes_at AT TIME ZONE 'America/New_York', 'FMHH12:MI AM') \
                    AS closes_local, \
                extract(epoch FROM r.window_closes_at)::float8 AS closes_at_s, \
                rs.lat AS restaurant_lat, rs.lng AS restaurant_lng, \
                r.return_lat, r.return_lng, r.dropoff_radius_m, \
                p.dropoff_lat AS home_lat, p.dropoff_lng AS home_lng, \
                coalesce(p.walking_pace, 'normal') AS pace, \
                lp.lat AS ping_lat, lp.lng AS ping_lng \
         FROM ferry.runs r \
         JOIN ferry.restaurants rs ON rs.id = r.restaurant_id \
         JOIN ferry.users u ON u.id = r.runner_id \
         LEFT JOIN ferry.profiles p ON p.user_id = r.runner_id \
         LEFT JOIN LATERAL ( \
             SELECT lp.lat, lp.lng FROM ferry.location_pings lp \
             WHERE lp.run_id = r.id AND lp.ts > now() - make_interval(secs => $4) \
             ORDER BY lp.ts DESC LIMIT 1 \
         ) lp ON true \
         WHERE r.status = 'open' \
           AND r.window_closes_at > now() \
           AND r.window_closes_at < to_timestamp($3) \
           AND CASE WHEN NOT $6 THEN r.restaurant_id = $2 \
               ELSE r.restaurant_id <> $2 \
                 AND 2 * $9 * asin(least(1, sqrt( \
                       power(sin(radians(rs.lat - $7) / 2), 2) \
                       + cos(radians($7)) * cos(radians(rs.lat)) \
                         * power(sin(radians(rs.lng - $8) / 2), 2)))) <= $10 \
                 AND NOT EXISTS ( \
                     SELECT 1 FROM ferry.notifications n \
                     JOIN ferry.pickup_requests q ON q.id = n.data->>'request_id' \
                     WHERE n.user_id = r.runner_id AND n.type = 'system' \
                       AND n.data->>'run_id' = r.id \
                       AND n.data->>'new_run_restaurant_id' = $2 \
                       AND q.customer_id = $1) \
               END \
           AND NOT r.is_demo_history \
           AND r.runner_id <> $1 \
           AND r.circle_id IN (SELECT m.circle_id FROM ferry.circle_members m \
                               WHERE m.user_id = $1) \
         ORDER BY r.created_at, r.id \
         LIMIT $5",
    )
    .bind(&request.customer_id)
    .bind(&request.restaurant_id)
    .bind(request.needed_by_s)
    .bind(RECENT_PING.as_secs_f64())
    .bind(CANDIDATES_PER_REQUEST)
    .bind(scope == Scope::Nearby)
    .bind(request.restaurant_lat)
    .bind(request.restaurant_lng)
    .bind(EARTH_RADIUS_M)
    .bind(NEARBY_RESTAURANT_M)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(Candidate::from).collect())
}

/// A walk from the restaurant: its waypoints (the restaurant first) and the leg into each next one.
#[derive(Debug, Clone, PartialEq)]
pub struct Walk {
    pub waypoints: Vec<LatLng>,
    pub legs: Vec<Route>,
}

impl Walk {
    /// Meters along the walk up to waypoint `index` (0 is the restaurant).
    pub fn distance_to(&self, index: usize) -> f64 {
        self.legs.iter().take(index).map(|l| l.distance_m).sum()
    }

    pub fn total_m(&self) -> f64 {
        self.distance_to(self.legs.len())
    }

    fn has_fallback(&self) -> bool {
        self.legs.iter().any(|l| l.source == RouteSource::Fallback)
    }

    /// Every leg measured the fallback's way (straight line × 1.3).
    fn as_fallback(&self) -> Self {
        let legs = self
            .waypoints
            .windows(2)
            .map(|pair| Route {
                distance_m: fallback_distance_m(pair[0], pair[1]),
                geometry: None,
                source: RouteSource::Fallback,
            })
            .collect();
        Self {
            waypoints: self.waypoints.clone(),
            legs,
        }
    }
}

/// `base` and `with` measured the same way: when either has a fallback leg, both become fallback.
pub fn consistent(base: Walk, with: Walk) -> (Walk, Walk) {
    if base.has_fallback() || with.has_fallback() {
        (base.as_fallback(), with.as_fallback())
    } else {
        (base, with)
    }
}

/// The plan's walk from the restaurant, then on to `destination` when there is one.
async fn finish(
    routing: &Routing,
    restaurant: LatLng,
    plan: &Plan,
    destination: Option<LatLng>,
) -> Walk {
    let mut waypoints = vec![restaurant];
    waypoints.extend(plan.stops.iter().map(|s| s.order.dropoff.location));
    let mut legs = plan.legs.clone();
    if let Some(destination) = destination {
        let last = *waypoints.last().expect("the restaurant is always there");
        legs.push(routing.route(Profile::Foot, last, destination).await);
        waypoints.push(destination);
    }
    Walk { waypoints, legs }
}

/// How a candidate run would take the request.
#[derive(Debug, Clone, PartialEq)]
pub struct Score {
    /// Meters the drop-off adds to the runner's walk (at least 0).
    pub extra_m: f64,
    /// `extra_m` at the runner's pace, in minutes, not rounded.
    pub extra_minutes: f64,
    /// When the food would reach the drop-off, in seconds since the epoch.
    pub food_at_s: f64,
    /// The drop-off is inside the run's drop-off zone, or the run has none.
    pub in_zone: bool,
    /// OSRM measured every leg of both walks, or the fallback measured them all.
    pub measured_by: RouteSource,
}

impl Score {
    /// The three rules of the module docs.
    pub fn fits(&self, needed_by_s: f64) -> bool {
        self.in_zone && self.extra_minutes <= MAX_EXTRA_MINUTES && self.food_at_s <= needed_by_s
    }
}

/// Straight-line meters from `point` to the path through `waypoints` (segments between
/// consecutive points, on a local flat projection, plenty within a town). Infinite for no points.
pub fn path_distance_m(point: LatLng, waypoints: &[LatLng]) -> f64 {
    const M_PER_DEG: f64 = 6_371_008.8 * std::f64::consts::PI / 180.0;
    let cos = point.lat.to_radians().cos();
    let xy = |p: LatLng| {
        (
            (p.lng - point.lng) * cos * M_PER_DEG,
            (p.lat - point.lat) * M_PER_DEG,
        )
    };
    match waypoints {
        [] => f64::INFINITY,
        [only] => haversine_m(point, *only),
        _ => waypoints
            .windows(2)
            .map(|pair| {
                let (ax, ay) = xy(pair[0]);
                let (bx, by) = xy(pair[1]);
                let (dx, dy) = (bx - ax, by - ay);
                let len2 = dx * dx + dy * dy;
                // The point is the origin; t is where its foot falls on the segment.
                let t = if len2 > 0.0 {
                    (-(ax * dx + ay * dy) / len2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                (ax + t * dx).hypot(ay + t * dy)
            })
            .fold(f64::INFINITY, f64::min),
    }
}

/// The drop-off zone rule: true when the run has no `return_place` or no `dropoff_radius_m`;
/// else the drop-off is within that radius of the return place, or within the request's own
/// radius of `path`.
pub fn in_zone(
    dropoff: LatLng,
    request_radius_m: Option<i32>,
    return_place: Option<LatLng>,
    run_radius_m: Option<i32>,
    path: &[LatLng],
) -> bool {
    let (Some(return_place), Some(run_radius)) = (return_place, run_radius_m) else {
        return true;
    };
    haversine_m(dropoff, return_place) <= f64::from(run_radius)
        || request_radius_m.is_some_and(|q| path_distance_m(dropoff, path) <= f64::from(q))
}

/// Scores `candidate` for `request`, given the run's active orders. `None` when the run has no
/// destination and no drop-offs, so there is no walk to compare with.
pub async fn score(
    routing: &Routing,
    candidate: &Candidate,
    orders: &[ActiveOrder],
    request: &OpenRequest,
) -> Option<Score> {
    let destination = candidate.destination();
    if destination.is_none() && orders.is_empty() {
        return None;
    }
    let pace = candidate.pace;
    let stop = ActiveOrder {
        id: request.id.clone(),
        customer_id: request.customer_id.clone(),
        dropoff: Place {
            id: None,
            label: "request".into(),
            location: request.dropoff(),
            note: None,
        },
    };
    let mut with_orders = orders.to_vec();
    with_orders.push(stop);
    let base_plan = Plan::build(routing, candidate.restaurant, None, orders, pace).await;
    let with_plan = Plan::build(routing, candidate.restaurant, None, &with_orders, pace).await;
    // Waypoint 0 is the restaurant, so stop i is waypoint i + 1.
    let stop_index = with_plan
        .stops
        .iter()
        .position(|s| s.order.id == request.id)
        .expect("the request's stop is planned")
        + 1;
    let base = finish(routing, candidate.restaurant, &base_plan, destination).await;
    let with = finish(routing, candidate.restaurant, &with_plan, destination).await;
    let (base, with) = consistent(base, with);
    let measured_by = if base.has_fallback() {
        RouteSource::Fallback
    } else {
        RouteSource::Osrm
    };

    let mps = pace.meters_per_second();
    let extra_m = (with.total_m() - base.total_m()).max(0.0);
    let pickup_s = match candidate.ping {
        Some(ping) => {
            let to_restaurant = routing
                .route(Profile::Foot, ping, candidate.restaurant)
                .await;
            candidate
                .closes_at_s
                .max(request.now_s + to_restaurant.distance_m / mps)
        }
        None => candidate.closes_at_s,
    };
    Some(Score {
        extra_m,
        extra_minutes: extra_m / mps / 60.0,
        food_at_s: pickup_s + with.distance_to(stop_index) / mps,
        in_zone: in_zone(
            request.dropoff(),
            request.dropoff_radius_m,
            candidate.return_place,
            candidate.dropoff_radius_m,
            &base.waypoints,
        ),
        measured_by,
    })
}

/// The best first: least extra time, then the earliest food, then the run id.
fn by_preference(a: &(Candidate, Score), b: &(Candidate, Score)) -> Ordering {
    a.1.extra_minutes
        .total_cmp(&b.1.extra_minutes)
        .then(a.1.food_at_s.total_cmp(&b.1.food_at_s))
        .then_with(|| a.0.run_id.cmp(&b.0.run_id))
}

/// The runs to the request's own restaurant that fit `request`, best first.
pub async fn fitting_runs(
    state: &AppState,
    request: &OpenRequest,
) -> ApiResult<Vec<(Candidate, Score)>> {
    fitting_runs_in(state, request, Scope::Same).await
}

/// The runs in `scope` that fit `request`, best first. A nearby run is scored from its own
/// restaurant, by the same rules.
pub async fn fitting_runs_in(
    state: &AppState,
    request: &OpenRequest,
    scope: Scope,
) -> ApiResult<Vec<(Candidate, Score)>> {
    let mut fits = Vec::new();
    for candidate in candidates_in(&state.pool, request, scope).await? {
        let orders = tracking::active_orders(&state.pool, &candidate.run_id).await?;
        if let Some(score) = score(&state.routing, &candidate, &orders, request).await
            && score.fits(request.needed_by_s)
        {
            fits.push((candidate, score));
        }
    }
    fits.sort_by(by_preference);
    Ok(fits)
}

/// A request that was just matched.
#[derive(Debug, Clone, PartialEq)]
pub struct Matched {
    pub request_id: String,
    pub run_id: String,
    pub extra_minutes: f64,
}

enum Claim {
    Matched(Vec<(String, Notification)>),
    /// The run closed, was cancelled or its window passed since it was scored.
    RunGone,
    /// The request is no longer searching (matched elsewhere, or expired).
    Taken,
}

/// Matches the request to its best fitting run, if it is still searching and one fits: the
/// request becomes `matched` with that `run_id`, and the customer and the runner are notified.
/// `Ok(None)` when nothing fits, scoring ran out of time, or the request is not searching. When no
/// run to its own restaurant took it, the fitting runs to nearby restaurants get their suggestion
/// ([`suggest_nearby`]); the request stays searching.
pub async fn match_request(state: &AppState, request_id: &str) -> ApiResult<Option<Matched>> {
    let Some(request) = load_open(&state.pool, request_id).await? else {
        return Ok(None);
    };
    let fits = match tokio::time::timeout(SCORING_BUDGET, fitting_runs(state, &request)).await {
        Ok(fits) => fits?,
        Err(_) => {
            tracing::warn!(
                request_id,
                "pickup request: scoring took too long; left searching"
            );
            return Ok(None);
        }
    };
    for (candidate, score) in fits {
        match claim(state, &request, &candidate, &score).await? {
            Claim::Matched(notifications) => {
                tracing::info!(
                    request_id,
                    run_id = %candidate.run_id,
                    extra_minutes = format!("{:.2}", score.extra_minutes),
                    measured_by = ?score.measured_by,
                    "pickup request matched"
                );
                for (user_id, notification) in &notifications {
                    state
                        .hub
                        .send_to(user_id, &Event::notification_created(notification));
                }
                return Ok(Some(Matched {
                    request_id: request.id.clone(),
                    run_id: candidate.run_id,
                    extra_minutes: score.extra_minutes,
                }));
            }
            Claim::RunGone => continue,
            Claim::Taken => return Ok(None),
        }
    }
    suggest_nearby(state, &request).await?;
    Ok(None)
}

/// Suggests `request` to the runner of each fitting run to a nearby restaurant, best first, once
/// per customer, restaurant and run: a `system` notice to open a second Ferry to the request's
/// restaurant. Returns how many were sent. Scoring past [`SCORING_BUDGET`] sends none; the next
/// sweep tries again.
pub async fn suggest_nearby(state: &AppState, request: &OpenRequest) -> ApiResult<u64> {
    let request_id = request.id.as_str();
    let scoring = fitting_runs_in(state, request, Scope::Nearby);
    let fits = match tokio::time::timeout(SCORING_BUDGET, scoring).await {
        Ok(fits) => fits?,
        Err(_) => {
            tracing::warn!(
                request_id,
                "pickup request: nearby scoring took too long; none suggested"
            );
            return Ok(0);
        }
    };
    let mut sent = 0;
    for (candidate, score) in fits {
        match suggest_one(state, request, &candidate, &score).await? {
            Nearby::Sent(notification) => {
                tracing::info!(
                    request_id,
                    run_id = %candidate.run_id,
                    extra_minutes = format!("{:.2}", score.extra_minutes),
                    measured_by = ?score.measured_by,
                    "pickup request suggested to a nearby run"
                );
                state.hub.send_to(
                    &candidate.runner_id,
                    &Event::notification_created(&notification),
                );
                sent += 1;
            }
            Nearby::Skipped => continue,
            Nearby::Taken => break,
        }
    }
    Ok(sent)
}

enum Nearby {
    Sent(Notification),
    /// The run closed since it was scored, or its runner was already told.
    Skipped,
    /// The request is no longer searching.
    Taken,
}

/// One transaction: the run still open with its window ahead and the request still searching
/// (share locks on both, so neither changes until this commits), then the runner's notice, once
/// per customer, restaurant and run ([`notices::nearby`]).
async fn suggest_one(
    state: &AppState,
    request: &OpenRequest,
    candidate: &Candidate,
    score: &Score,
) -> ApiResult<Nearby> {
    let mut tx = state.pool.begin().await?;
    sqlx::query(SET_LOCK_TIMEOUT).execute(&mut *tx).await?;
    let open: Option<String> = sqlx::query_scalar(
        "SELECT id FROM ferry.runs WHERE id = $1 AND status = 'open' AND window_closes_at > now() \
         FOR SHARE",
    )
    .bind(&candidate.run_id)
    .fetch_optional(&mut *tx)
    .await?;
    if open.is_none() {
        return Ok(Nearby::Skipped);
    }
    let searching: Option<String> = sqlx::query_scalar(
        "SELECT id FROM ferry.pickup_requests \
         WHERE id = $1 AND status = 'searching' AND needed_by > now() FOR SHARE",
    )
    .bind(&request.id)
    .fetch_optional(&mut *tx)
    .await?;
    if searching.is_none() {
        return Ok(Nearby::Taken);
    }
    let notice = notices::nearby(&mut tx, &state.config, request, candidate, score).await?;
    tx.commit().await?;
    Ok(notice.map_or(Nearby::Skipped, Nearby::Sent))
}

/// One transaction: the run must still be open with its window ahead (share lock, so it cannot
/// close until the match commits), then the guarded update, then the customer's notification and
/// the runner's, once per customer and run ([`notices::suggested`]).
async fn claim(
    state: &AppState,
    request: &OpenRequest,
    candidate: &Candidate,
    score: &Score,
) -> ApiResult<Claim> {
    let mut tx = state.pool.begin().await?;
    sqlx::query(SET_LOCK_TIMEOUT).execute(&mut *tx).await?;
    let open: Option<String> = sqlx::query_scalar(
        "SELECT id FROM ferry.runs WHERE id = $1 AND status = 'open' AND window_closes_at > now() \
         FOR SHARE",
    )
    .bind(&candidate.run_id)
    .fetch_optional(&mut *tx)
    .await?;
    if open.is_none() {
        return Ok(Claim::RunGone);
    }
    let claimed: Option<String> = sqlx::query_scalar(
        "UPDATE ferry.pickup_requests SET status = 'matched', run_id = $2 \
         WHERE id = $1 AND status = 'searching' AND needed_by > now() \
         RETURNING id",
    )
    .bind(&request.id)
    .bind(&candidate.run_id)
    .fetch_optional(&mut *tx)
    .await?;
    if claimed.is_none() {
        return Ok(Claim::Taken);
    }
    let customer = notices::matched(&mut tx, &state.config, request, candidate).await?;
    let runner = notices::suggested(&mut tx, &state.config, request, candidate, score).await?;
    tx.commit().await?;
    let mut sent = vec![(request.customer_id.clone(), customer)];
    sent.extend(runner.map(|runner| (candidate.runner_id.clone(), runner)));
    Ok(Claim::Matched(sent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracking::latlng;

    const COSMIC: LatLng = latlng(35.913211, -79.055094);
    const FETZER: LatLng = latlng(35.909152, -79.046905);
    const HINTON: LatLng = latlng(35.902364, -79.043231);
    const DAVIS: LatLng = latlng(35.910957, -79.047906);

    fn osrm(distance_m: f64) -> Route {
        Route {
            distance_m,
            geometry: Some(vec![[0.0, 0.0], [1.0, 1.0]]),
            source: RouteSource::Osrm,
        }
    }

    fn walk(waypoints: &[LatLng], legs: &[f64]) -> Walk {
        Walk {
            waypoints: waypoints.to_vec(),
            legs: legs.iter().map(|d| osrm(*d)).collect(),
        }
    }

    #[test]
    fn distances_along_a_walk() {
        let w = walk(&[COSMIC, FETZER, HINTON], &[1000.0, 900.0]);
        assert_eq!(w.distance_to(0), 0.0);
        assert_eq!(w.distance_to(1), 1000.0);
        assert_eq!(w.total_m(), 1900.0);
    }

    #[test]
    fn one_fallback_leg_makes_both_walks_fallback() {
        let base = walk(&[COSMIC, HINTON], &[1850.0]);
        let mut with = walk(&[COSMIC, FETZER, HINTON], &[1000.0, 900.0]);
        // All OSRM: kept as they are.
        let (b, w) = consistent(base.clone(), with.clone());
        assert_eq!((b.total_m(), w.total_m()), (1850.0, 1900.0));
        // One fallback leg in either: every leg of both is the straight line × 1.3.
        with.legs[1].source = RouteSource::Fallback;
        let (b, w) = consistent(base, with);
        assert_eq!(b.total_m(), fallback_distance_m(COSMIC, HINTON));
        let expected = fallback_distance_m(COSMIC, FETZER) + fallback_distance_m(FETZER, HINTON);
        assert!((w.total_m() - expected).abs() < 1e-9);
        assert!(w.legs.iter().all(|l| l.source == RouteSource::Fallback));
    }

    #[test]
    fn fetzer_is_about_a_minute_off_the_way_to_hinton() {
        // The numbers in docs/DECISIONS.md (Requests), on the fallback as the tests run it.
        let base = fallback_distance_m(COSMIC, HINTON);
        let with = fallback_distance_m(COSMIC, FETZER) + fallback_distance_m(FETZER, HINTON);
        let extra_m = with - base;
        assert!((95.0..115.0).contains(&extra_m), "{extra_m}");
        let brisk = extra_m / Pace::Brisk.meters_per_second() / 60.0;
        assert!((1.0..1.2).contains(&brisk), "{brisk}");
        // Without a destination the whole leg to Fetzer would count: far over 4 minutes.
        let leg = fallback_distance_m(COSMIC, FETZER) / Pace::Brisk.meters_per_second() / 60.0;
        assert!(leg > 11.0, "{leg}");
    }

    #[test]
    fn path_distance() {
        // Fetzer is about 250 m off the straight line from Cosmic Cantina to Hinton James.
        let off = path_distance_m(FETZER, &[COSMIC, HINTON]);
        assert!((230.0..275.0).contains(&off), "{off}");
        // A point on the path is 0 away; past an end, the distance is to that end.
        assert!(path_distance_m(COSMIC, &[COSMIC, HINTON]) < 1e-6);
        let beyond = latlng(35.898, -79.039);
        assert!(
            (path_distance_m(beyond, &[COSMIC, HINTON]) - haversine_m(beyond, HINTON)).abs() < 5.0
        );
        // Several segments: the nearest one counts.
        assert!(path_distance_m(DAVIS, &[COSMIC, DAVIS, HINTON]) < 1e-6);
        assert_eq!(path_distance_m(FETZER, &[]), f64::INFINITY);
        assert!((path_distance_m(FETZER, &[HINTON]) - haversine_m(FETZER, HINTON)).abs() < 1e-9);
    }

    #[test]
    fn zone_rule() {
        let path = [COSMIC, HINTON];
        // No zone (no return place, or no radius): always inside.
        assert!(in_zone(FETZER, None, None, Some(100), &path));
        assert!(in_zone(FETZER, None, Some(HINTON), None, &path));
        // Fetzer is about 825 m from Hinton James.
        let from_hinton = haversine_m(FETZER, HINTON);
        assert!((800.0..850.0).contains(&from_hinton), "{from_hinton}");
        assert!(in_zone(FETZER, None, Some(HINTON), Some(900), &path));
        assert!(!in_zone(FETZER, None, Some(HINTON), Some(400), &path));
        // Within the request's own radius of the runner's path (about 250 m off it).
        assert!(!in_zone(FETZER, Some(100), Some(HINTON), Some(400), &path));
        assert!(in_zone(FETZER, Some(300), Some(HINTON), Some(400), &path));
    }

    #[test]
    fn fits_needs_all_three() {
        let ok = Score {
            extra_m: 100.0,
            extra_minutes: 1.0,
            food_at_s: 1000.0,
            in_zone: true,
            measured_by: RouteSource::Osrm,
        };
        assert!(ok.fits(1000.0));
        assert!(!ok.fits(999.0), "late");
        assert!(
            Score {
                extra_minutes: MAX_EXTRA_MINUTES,
                ..ok.clone()
            }
            .fits(1000.0),
            "exactly 4 minutes fits"
        );
        assert!(
            !Score {
                extra_minutes: 4.01,
                ..ok.clone()
            }
            .fits(1000.0)
        );
        assert!(
            !Score {
                in_zone: false,
                ..ok
            }
            .fits(1000.0)
        );
    }

    fn candidate(run_id: &str) -> Candidate {
        Candidate {
            run_id: run_id.into(),
            runner_id: "usr_maya".into(),
            circle_id: "cir_hinton4".into(),
            runner_name: "Maya".into(),
            restaurant_name: "Cosmic Cantina".into(),
            closes_local: "6:10 PM".into(),
            closes_at_s: 0.0,
            restaurant: COSMIC,
            return_place: None,
            dropoff_radius_m: None,
            runner_home: None,
            pace: Pace::Brisk,
            ping: None,
        }
    }

    fn request() -> OpenRequest {
        OpenRequest {
            id: "req_test".into(),
            customer_id: "usr_judge".into(),
            restaurant_id: "rst_cosmic".into(),
            restaurant_name: "Cosmic Cantina".into(),
            restaurant_lat: COSMIC.lat,
            restaurant_lng: COSMIC.lng,
            dropoff_lat: FETZER.lat,
            dropoff_lng: FETZER.lng,
            dropoff_radius_m: None,
            needed_by_s: 3600.0,
            now_s: 0.0,
        }
    }

    #[tokio::test]
    async fn scoring_on_the_fallback() {
        let routing = crate::tracking::Routing::new(
            crate::state::http_client().unwrap(),
            crate::tracking::RoutingSettings::new("http://127.0.0.1:1"),
        );
        // No destination and no drop-offs: not scored.
        assert_eq!(
            score(&routing, &candidate("run_a"), &[], &request()).await,
            None
        );

        // Heading back to Hinton James: Fetzer is about a minute out of the way.
        let home = Candidate {
            runner_home: Some(HINTON),
            closes_at_s: 600.0,
            ..candidate("run_b")
        };
        let s = score(&routing, &home, &[], &request()).await.unwrap();
        let expected = fallback_distance_m(COSMIC, FETZER) + fallback_distance_m(FETZER, HINTON)
            - fallback_distance_m(COSMIC, HINTON);
        assert!((s.extra_m - expected).abs() < 1e-6);
        assert!(s.extra_minutes < 1.2);
        // Food: the window's close, then the walk from the restaurant to Fetzer.
        let walk_s = fallback_distance_m(COSMIC, FETZER) / Pace::Brisk.meters_per_second();
        assert!((s.food_at_s - (600.0 + walk_s)).abs() < 1e-6);
        assert!(s.in_zone && s.fits(3600.0));
        assert_eq!(s.measured_by, RouteSource::Fallback);

        // The return place wins over the profile drop-off.
        let davis_bound = Candidate {
            return_place: Some(DAVIS),
            ..home.clone()
        };
        // Cosmic, Fetzer, Davis instead of Cosmic, Davis: about 508 m, 5.3 minutes at brisk.
        let s = score(&routing, &davis_bound, &[], &request())
            .await
            .unwrap();
        assert!((5.0..5.6).contains(&s.extra_minutes), "{}", s.extra_minutes);
        assert!(!s.fits(3600.0));

        // A recent ping far from the restaurant pushes the pickup past the window's close.
        let far = Candidate {
            ping: Some(HINTON),
            ..home
        };
        let s = score(&routing, &far, &[], &request()).await.unwrap();
        let ping_s = fallback_distance_m(HINTON, COSMIC) / Pace::Brisk.meters_per_second();
        assert!((s.food_at_s - (ping_s + walk_s)).abs() < 1e-6);
    }
}
