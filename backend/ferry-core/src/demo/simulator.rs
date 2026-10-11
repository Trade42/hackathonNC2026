//! The demo's runner simulator (DEMO_MODE only): walks the runner from the restaurant to each
//! active order's drop-off, posting a ping every 5 s through `tracking::accept_ping`, so the
//! track and `runner.location` behave exactly as with a real phone.
//!
//! For other modules, where `state` is a `&AppState` and `run_id` a `&str`, after the change's
//! transaction commits; this form passes `clippy -D warnings` in edition 2024:
//!
//! ```ignore
//! if state.config.demo_mode
//!     && let Err(err) = crate::demo::simulator::start_walk(state, run_id).await
//! {
//!     tracing::warn!(run_id, error = %err, "demo walk not started");
//! }
//! ```
//!
//! `runs::actions::mark_picked_up` starts it this way for a demo persona's run, spawned with owned
//! copies (`start_walk(&s, &id)`) so the pickup's 200 waits on nothing.
//!
//! - **Route.** `tracking::Plan` from the restaurant: the active orders' drop-offs in walking
//!   order, each leg OSRM's walking geometry or, on the fallback, the straight line. The path
//!   starts exactly at the restaurant and passes exactly through each drop-off.
//! - **Pace.** The runner's profile pace × `DEMO_SIM_SPEED`. The first ping is at the restaurant
//!   at once, then one every 5 s of wall time with `at` = now and `accuracy_m` from 5 to 10, and
//!   the last exactly at the last drop-off.
//! - **Control.** One walk per run: a second start restarts it from the restaurant, and a stop
//!   ends it. A walk also ends at the last drop-off, or when a ping is refused (the run left the
//!   active states). A ping the database could not take is skipped, and the walk goes on.
//! - **Restart.** A walk lives in the server process, so a restart ends it. main.rs calls
//!   [`resume_at_startup`] once the server listens: in DEMO_MODE, each `delivering` run whose
//!   runner is a demo persona and that has an active order walks on from its latest ping (from
//!   the restaurant without one), along tracking's own plan from there (the route the track
//!   shows) through the active drop-offs, at the same pace. Its first ping repeats the latest
//!   one's location at once. A resume never replaces a walk already running on the run, and logs
//!   one line per run, with the run id only.
//! - **The phone waits.** In DEMO_MODE, while the simulator walks a run, a ping its runner's phone
//!   posts to `POST /v1/runs/{id}/location` is answered 204 and not stored
//!   ([`ignores_phone_ping`]), so the bubble never jumps between the walk and the phone.
//! - **Only locations.** No run, order or money changes. Logs name the run id only.
//! - **Internal routes**: `POST /internal/demo/runs/{id}/walk` (202 with [`WalkStarted`]) and
//!   `POST /internal/demo/runs/{id}/walk/stop` (200 with [`WalkStopped`]). Outside DEMO_MODE
//!   every method on them is the app's 404 for an unknown route. In DEMO_MODE, POST is 401
//!   without the right `X-Ferry-Secret` (checked as the inbound route checks it), and any other
//!   method is the app's 405.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rand::RngExt;
use serde::Serialize;
use tokio::task::{AbortHandle, JoinHandle};
use tokio::time::MissedTickBehavior;

use crate::auth::demo::DemoPersona;
use crate::config::Config;
use crate::error::{ApiError, ApiResult, ErrorCode};
use crate::extract::{Json, Path};
use crate::messaging::inbound::secret_matches;
use crate::messaging::sidecar::SECRET_HEADER;
use crate::notifications::rfc3339;
use crate::runs::machine::RunStatus;
use crate::runs::view;
use crate::state::AppState;
use crate::tracking::{self, ActiveOrder, LatLng, LocationPing, Plan, haversine_m, latlng};

/// Wall time between two pings: the phone app's cadence.
pub const PING_EVERY: Duration = Duration::from_secs(5);

/// The `accuracy_m` range of a simulated fix, in meters: a phone outdoors.
const ACCURACY_M: std::ops::RangeInclusive<f64> = 5.0..=10.0;

/// How a walk moves. [`WalkSettings::from_config`] has the demo's values; tests shorten the
/// interval.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WalkSettings {
    /// Wall time between pings.
    pub interval: Duration,
    /// The multiplier over the runner's pace (`DEMO_SIM_SPEED`).
    pub speed: f64,
}

impl WalkSettings {
    /// A ping every 5 s at `DEMO_SIM_SPEED`.
    pub fn from_config(config: &Config) -> Self {
        Self {
            interval: PING_EVERY,
            speed: config.demo_sim_speed,
        }
    }

    fn check(self) -> Result<Self, WalkError> {
        if self.interval.is_zero() || !(self.speed.is_finite() && self.speed > 0.0) {
            return Err(WalkError::BadSettings);
        }
        Ok(self)
    }
}

/// Why a walk did not start.
#[derive(Debug, thiserror::Error)]
pub enum WalkError {
    #[error("the runner simulator runs only when DEMO_MODE is true")]
    NotDemoMode,
    #[error("no such run")]
    NoSuchRun,
    #[error("the run is {0}; a walk runs only while it is open, closed, purchased or delivering")]
    Inactive(String),
    #[error("the run has no active order to deliver")]
    NoStops,
    #[error("the walk's interval and speed must be above 0")]
    BadSettings,
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
}

impl From<WalkError> for ApiError {
    fn from(err: WalkError) -> Self {
        match err {
            WalkError::NotDemoMode => ApiError::not_found("The runner simulator is off."),
            WalkError::NoSuchRun => ApiError::not_found("No such run."),
            WalkError::Inactive(_) | WalkError::NoStops => ApiError::conflict(err.to_string()),
            WalkError::BadSettings => ApiError::internal(err),
            WalkError::Db(db) => db.into(),
        }
    }
}

/// What `start_walk` started; the body of the internal route's 202.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WalkStarted {
    pub run_id: String,
    /// Meters per second: the runner's pace × the speed multiplier.
    pub speed_mps: f64,
    /// Seconds between pings.
    pub interval_s: f64,
    /// True when this start stopped a walk already running on the run.
    pub restarted: bool,
}

/// The body of the stop route's 200.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WalkStopped {
    pub run_id: String,
    /// False when no walk was running.
    pub stopped: bool,
}

// ---------- The path ----------

/// A walking path: its points in order and the meters walked to each.
#[derive(Debug, Clone, PartialEq)]
pub struct WalkPath {
    points: Vec<LatLng>,
    /// `walked[i]` is the meters from the first point to `points[i]`.
    walked: Vec<f64>,
}

impl WalkPath {
    /// The path through `points`, with repeated consecutive points dropped. `None` without a
    /// point.
    pub fn new(points: impl IntoIterator<Item = LatLng>) -> Option<Self> {
        let mut path = Self {
            points: Vec::new(),
            walked: Vec::new(),
        };
        for point in points {
            match path.points.last() {
                None => path.walked.push(0.0),
                Some(&last) if last == point => continue,
                Some(&last) => path.walked.push(path.length_m() + haversine_m(last, point)),
            }
            path.points.push(point);
        }
        (!path.points.is_empty()).then_some(path)
    }

    pub fn points(&self) -> &[LatLng] {
        &self.points
    }

    /// Meters along the path, by straight lines between its points.
    pub fn length_m(&self) -> f64 {
        self.walked.last().copied().unwrap_or(0.0)
    }

    /// The point `meters` along the path, clamped to its first and last points.
    pub fn point_at(&self, meters: f64) -> LatLng {
        let last = self.points.len() - 1;
        if meters.is_nan() || meters <= 0.0 {
            return self.points[0];
        }
        if meters >= self.length_m() {
            return self.points[last];
        }
        // The segment from points[i - 1] to points[i] holds `meters`.
        let i = self.walked.partition_point(|&w| w < meters).clamp(1, last);
        let (from, to) = (self.points[i - 1], self.points[i]);
        let segment = self.walked[i] - self.walked[i - 1];
        if segment <= 0.0 {
            return to;
        }
        let t = (meters - self.walked[i - 1]) / segment;
        latlng(
            from.lat + (to.lat - from.lat) * t,
            from.lng + (to.lng - from.lng) * t,
        )
    }

    /// The position at each ping: the first point, then every `step_m` meters, then the last
    /// point exactly. A zero-length path is its one point. A step that is not above 0 goes
    /// straight to the end.
    pub fn positions(&self, step_m: f64) -> impl Iterator<Item = LatLng> + '_ {
        let length = self.length_m();
        let step = if step_m.is_finite() && step_m > 0.0 {
            step_m
        } else {
            length
        };
        std::iter::successors(Some(0.0_f64), move |&walked| {
            (walked < length).then(|| (walked + step).min(length))
        })
        .map(|walked| self.point_at(walked))
    }
}

/// The walk `plan` describes, from its origin: each leg's OSRM geometry (`[lng, lat]`), or the
/// straight line on the fallback, through each stop's exact drop-off.
pub fn path_of(plan: &Plan) -> WalkPath {
    let mut points = vec![plan.origin];
    for (leg, stop) in plan.legs.iter().zip(&plan.stops) {
        if let Some(geometry) = &leg.geometry {
            points.extend(geometry.iter().map(|&[lng, lat]| latlng(lat, lng)));
        }
        points.push(stop.order.dropoff.location);
    }
    WalkPath::new(points).expect("the path has its origin")
}

// ---------- The walks ----------

struct Running {
    generation: u64,
    task: AbortHandle,
    runner_id: String,
}

/// The walk running on each run, in this process (the one that holds the realtime hub).
static WALKS: LazyLock<Mutex<HashMap<String, Running>>> = LazyLock::new(Default::default);
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

fn walks() -> MutexGuard<'static, HashMap<String, Running>> {
    WALKS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Starts (or restarts) the walk on `run_id` at the demo's pace: a ping every 5 s at
/// `DEMO_SIM_SPEED`. The run must be active and have an active order; the walk itself runs in a
/// spawned task.
pub async fn start_walk(state: &AppState, run_id: &str) -> Result<WalkStarted, WalkError> {
    start_walk_with(state, run_id, WalkSettings::from_config(&state.config)).await
}

/// [`start_walk`] with explicit settings.
pub async fn start_walk_with(
    state: &AppState,
    run_id: &str,
    settings: WalkSettings,
) -> Result<WalkStarted, WalkError> {
    if !state.config.demo_mode {
        return Err(WalkError::NotDemoMode);
    }
    let settings = settings.check()?;
    let context = tracking::load_context(&state.pool, run_id, "")
        .await?
        .ok_or(WalkError::NoSuchRun)?;
    if !context.is_active() {
        return Err(WalkError::Inactive(context.status));
    }
    let orders = tracking::active_orders(&state.pool, run_id).await?;
    if orders.is_empty() {
        return Err(WalkError::NoStops);
    }

    let pace = context.pace();
    let speed_mps = pace.meters_per_second() * settings.speed;
    let walk = Walk::new(
        state,
        run_id,
        &context.runner_id,
        pace,
        context.restaurant(),
        orders,
        settings,
    );
    // Spawned under the lock, so the walk cannot unregister itself before it is registered.
    let restarted = {
        let mut walks = walks();
        let (generation, runner_id) = (walk.generation, walk.runner_id.clone());
        let task = tokio::spawn(walk.run()).abort_handle();
        let running = Running {
            generation,
            task,
            runner_id,
        };
        let previous = walks.insert(run_id.to_owned(), running);
        previous.map(|p| p.task.abort()).is_some()
    };
    if restarted {
        tracing::info!(run_id, "demo walk restarted");
    } else {
        tracing::info!(run_id, "demo walk started");
    }
    Ok(WalkStarted {
        run_id: run_id.to_owned(),
        speed_mps,
        interval_s: settings.interval.as_secs_f64(),
        restarted,
    })
}

/// Stops the walk on `run_id`. False when none was running.
pub fn stop_walk(run_id: &str) -> bool {
    let stopped = walks().remove(run_id).map(|r| r.task.abort()).is_some();
    if stopped {
        tracing::info!(run_id, "demo walk stopped");
    }
    stopped
}

/// True while a walk is running on `run_id`.
pub fn is_walking(run_id: &str) -> bool {
    walks().contains_key(run_id)
}

/// In DEMO_MODE, true when `user_id` is the runner of a run the simulator is walking: a ping from
/// their phone would fight the walk, so `POST /v1/runs/{id}/location` answers 204 and stores
/// nothing. Logs one line with the run id. Anyone else's ping takes the usual checks.
pub fn ignores_phone_ping(state: &AppState, run_id: &str, user_id: &str) -> bool {
    let ignored = state.config.demo_mode
        && walks()
            .get(run_id)
            .is_some_and(|walk| walk.runner_id == user_id);
    if ignored {
        tracing::info!(run_id, "phone ping ignored: the demo walk is running");
    }
    ignored
}

// ---------- After a restart ----------

/// Resumes the walks a restart ended ([`resume_walks`]) in the background, in DEMO_MODE only;
/// outside it, nothing. main.rs calls it once, after the server starts listening.
pub fn resume_at_startup(state: &AppState) -> Option<JoinHandle<()>> {
    if !state.config.demo_mode {
        return None;
    }
    let state = state.clone();
    Some(tokio::spawn(async move {
        resume_walks(&state).await;
    }))
}

/// [`resume_walks_with`] at the demo's pace: a ping every 5 s at `DEMO_SIM_SPEED`.
pub async fn resume_walks(state: &AppState) -> Vec<String> {
    resume_walks_with(state, WalkSettings::from_config(&state.config)).await
}

/// After a restart, walks on each run that is `delivering`, whose runner is a demo persona and
/// that has an active order: from the run's latest ping (the restaurant without one), along
/// tracking's plan from there through the active drop-offs, at the runner's pace × `settings`.
/// It ends as any walk does. A run that already has a walk keeps it. Each run resumed logs one
/// line with its id. Returns the ids of the runs resumed; outside DEMO_MODE, none.
pub async fn resume_walks_with(state: &AppState, settings: WalkSettings) -> Vec<String> {
    if !state.config.demo_mode {
        return Vec::new();
    }
    let settings = match settings.check() {
        Ok(settings) => settings,
        Err(err) => {
            tracing::warn!(error = %err, "demo walks not resumed");
            return Vec::new();
        }
    };
    let mut resumed = Vec::new();
    for persona in DemoPersona::ALL {
        let runs = view::load_list(
            &state.pool,
            &state.config,
            persona.user_id(),
            Some(RunStatus::Delivering),
            true,
        )
        .await;
        // One persona's list error skips only that persona's runs.
        let runs = match runs {
            Ok(runs) => runs,
            Err(err) => {
                tracing::warn!(persona = persona.name(), error = %err, "demo walks not resumed");
                continue;
            }
        };
        for run in runs {
            let run_id = run.id.as_str();
            match resume_walk(state, run_id, settings).await {
                Ok(true) => {
                    tracing::info!(run_id, "demo walk resumed");
                    resumed.push(run.id);
                }
                Ok(false) => {}
                Err(err) => tracing::warn!(run_id, error = %err, "demo walk not resumed"),
            }
        }
    }
    resumed
}

/// Resumes the walk on `run_id` if it is eligible ([`resume_walks_with`]) and has no walk. True
/// when it did.
async fn resume_walk(
    state: &AppState,
    run_id: &str,
    settings: WalkSettings,
) -> Result<bool, sqlx::Error> {
    let Some(context) = tracking::load_context(&state.pool, run_id, "").await? else {
        return Ok(false);
    };
    let persona = DemoPersona::ALL
        .iter()
        .any(|p| p.user_id() == context.runner_id);
    if context.status != RunStatus::Delivering.as_str() || !persona {
        return Ok(false);
    }
    let orders = tracking::active_orders(&state.pool, run_id).await?;
    if orders.is_empty() {
        return Ok(false);
    }
    let origin = context
        .ping
        .latest()
        .map_or_else(|| context.restaurant(), |ping| ping.location);
    let walk = Walk::new(
        state,
        run_id,
        &context.runner_id,
        context.pace(),
        origin,
        orders,
        settings,
    );
    // Spawned under the lock, as in `start_walk_with`; unlike a start, never over a running walk.
    let mut walks = walks();
    let Entry::Vacant(slot) = walks.entry(run_id.to_owned()) else {
        return Ok(false);
    };
    let (generation, runner_id) = (walk.generation, walk.runner_id.clone());
    let task = tokio::spawn(walk.run()).abort_handle();
    slot.insert(Running {
        generation,
        task,
        runner_id,
    });
    Ok(true)
}

/// One walk's task.
struct Walk {
    state: AppState,
    run_id: String,
    runner_id: String,
    /// Where the walk starts: the restaurant, or after a restart the latest ping.
    origin: LatLng,
    orders: Vec<ActiveOrder>,
    pace: tracking::Pace,
    step_m: f64,
    interval: Duration,
    generation: u64,
}

impl Walk {
    /// `runner_id`'s walk on `run_id` from `origin` through `orders`, at `pace` × `settings`.
    fn new(
        state: &AppState,
        run_id: &str,
        runner_id: &str,
        pace: tracking::Pace,
        origin: LatLng,
        orders: Vec<ActiveOrder>,
        settings: WalkSettings,
    ) -> Self {
        Self {
            state: state.clone(),
            run_id: run_id.to_owned(),
            runner_id: runner_id.to_owned(),
            origin,
            orders,
            pace,
            step_m: pace.meters_per_second() * settings.speed * settings.interval.as_secs_f64(),
            interval: settings.interval,
            generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
        }
    }

    async fn run(self) {
        let ended = self.walk().await;
        let run_id = self.run_id.as_str();
        {
            let mut walks = walks();
            if walks
                .get(run_id)
                .is_some_and(|r| r.generation == self.generation)
            {
                walks.remove(run_id);
            }
        }
        tracing::info!(run_id, "demo walk ended: {ended}");
    }

    /// Pings along the path until the last drop-off, or until a ping is refused. Returns why it
    /// ended.
    async fn walk(&self) -> &'static str {
        let plan = Plan::build(
            &self.state.routing,
            self.origin,
            None,
            &self.orders,
            self.pace,
        )
        .await;
        let path = path_of(&plan);
        let mut ticker = tokio::time::interval(self.interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        for location in path.positions(self.step_m) {
            // The first tick is immediate.
            ticker.tick().await;
            let ping = LocationPing {
                location,
                accuracy_m: rand::rng().random_range(ACCURACY_M),
                at: rfc3339(SystemTime::now()),
            };
            match tracking::accept_ping(&self.state, &self.run_id, &self.runner_id, &ping).await {
                Ok(()) => {}
                // The database is unavailable: skip this ping, as a phone would retry the next.
                Err(err) if err.code == ErrorCode::UpstreamUnavailable => {}
                Err(err) if err.code == ErrorCode::Conflict => {
                    return "the run is no longer active";
                }
                Err(_) => return "a ping was refused",
            }
        }
        "arrived at the last drop-off"
    }
}

// ---------- The internal routes ----------

/// `/internal/demo/runs/{id}/walk`, every method (see [`refusal`]). POST starts or restarts the walk:
/// 202 with [`WalkStarted`].
pub(crate) async fn walk_route(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    run_id: Result<Path<String>, ApiError>,
) -> Response {
    if let Some(refused) = refusal(&state, &method, &uri, &headers) {
        return refused;
    }
    post_walk(&state, run_id).await.into_response()
}

async fn post_walk(
    state: &AppState,
    run_id: Result<Path<String>, ApiError>,
) -> ApiResult<(StatusCode, Json<WalkStarted>)> {
    let Path(run_id) = run_id?;
    let started = start_walk(state, &run_id).await?;
    Ok((StatusCode::ACCEPTED, Json(started)))
}

/// `/internal/demo/runs/{id}/walk/stop`, every method (see [`refusal`]). POST: 200 with
/// [`WalkStopped`], whether or not a walk was running.
pub(crate) async fn walk_stop_route(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    run_id: Result<Path<String>, ApiError>,
) -> Response {
    if let Some(refused) = refusal(&state, &method, &uri, &headers) {
        return refused;
    }
    post_walk_stop(run_id).into_response()
}

fn post_walk_stop(run_id: Result<Path<String>, ApiError>) -> ApiResult<Json<WalkStopped>> {
    let Path(run_id) = run_id?;
    let stopped = stop_walk(&run_id);
    Ok(Json(WalkStopped { run_id, stopped }))
}

/// The answer for a request the internal routes refuse, if they do. Outside DEMO_MODE the routes
/// do not exist: every method gets the app's own 404 for an unknown route (`app::not_found`'s
/// answer, with no `Allow` header, which is why the routes use `any`). In DEMO_MODE a method other
/// than POST gets the app's 405 (`app::method_not_allowed`'s answer) with `Allow: POST`, and a
/// POST needs `X-Ferry-Secret` equal to `SIDECAR_SHARED_SECRET`, compared in constant time by the
/// inbound route's own check; with the secret unset, nothing passes (401). The run id is read only
/// after that.
fn refusal(state: &AppState, method: &Method, uri: &Uri, headers: &HeaderMap) -> Option<Response> {
    if !state.config.demo_mode {
        return Some(
            ApiError::not_found(format!("No route for {method} {}.", uri.path())).into_response(),
        );
    }
    if method != Method::POST {
        let refused = ApiError::new(
            ErrorCode::NotFound,
            format!("{method} is not allowed on {}.", uri.path()),
        )
        .with_status(StatusCode::METHOD_NOT_ALLOWED);
        return Some(([(header::ALLOW, "POST")], refused).into_response());
    }
    let given = headers.get(SECRET_HEADER).map(|v| v.as_bytes());
    if !secret_matches(&state.config, given) {
        return Some(ApiError::unauthorized("Wrong or missing X-Ferry-Secret.").into_response());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracking::{Pace, Place, PlannedStop, Route, RouteSource};

    const COSMIC: LatLng = latlng(35.913211, -79.055094);
    const FETZER: LatLng = latlng(35.909152, -79.046905);

    fn close(a: LatLng, b: LatLng) -> bool {
        (a.lat - b.lat).abs() < 1e-9 && (a.lng - b.lng).abs() < 1e-9
    }

    #[test]
    fn interpolates_along_a_polyline() {
        let corner = latlng(COSMIC.lat, FETZER.lng);
        let path = WalkPath::new([COSMIC, corner, FETZER]).unwrap();
        let first = haversine_m(COSMIC, corner);
        let second = haversine_m(corner, FETZER);
        assert!((path.length_m() - (first + second)).abs() < 1e-9);

        // Ends and clamping.
        assert_eq!(path.point_at(0.0), COSMIC);
        assert_eq!(path.point_at(-5.0), COSMIC);
        assert_eq!(path.point_at(f64::NAN), COSMIC);
        assert_eq!(path.point_at(path.length_m()), FETZER);
        assert_eq!(path.point_at(path.length_m() + 100.0), FETZER);
        assert!(close(path.point_at(first), corner));

        // Halfway along the first segment: halfway in lat and lng, and on the segment's line.
        let mid = path.point_at(first / 2.0);
        assert!((mid.lat - COSMIC.lat).abs() < 1e-12);
        assert!((mid.lng - (COSMIC.lng + corner.lng) / 2.0).abs() < 1e-9);
        assert!((haversine_m(COSMIC, mid) - first / 2.0).abs() < 0.01);

        // A quarter into the second segment.
        let quarter = path.point_at(first + second / 4.0);
        assert!((quarter.lng - corner.lng).abs() < 1e-12);
        assert!((haversine_m(corner, quarter) - second / 4.0).abs() < 0.01);
    }

    #[test]
    fn positions_at_a_speed() {
        let path = WalkPath::new([COSMIC, FETZER]).unwrap();
        let length = path.length_m();
        // Brisk (1.6 m/s) × 8, a ping every 5 s: 64 m apart.
        let step = Pace::Brisk.meters_per_second() * 8.0 * 5.0;
        assert!((step - 64.0).abs() < 1e-9);
        let positions: Vec<LatLng> = path.positions(step).collect();
        assert_eq!(positions.len(), (length / step).ceil() as usize + 1);
        assert_eq!(positions[0], COSMIC);
        assert_eq!(*positions.last().unwrap(), FETZER);
        // Every step but the last is 64 m; the last is what remains.
        for pair in positions.windows(2).take(positions.len() - 2) {
            assert!(
                (haversine_m(pair[0], pair[1]) - step).abs() < 0.05,
                "{pair:?}"
            );
        }
        let last = haversine_m(positions[positions.len() - 2], FETZER);
        assert!(last > 0.0 && last <= step + 0.05);
        assert!((last - (length - step * (positions.len() - 2) as f64)).abs() < 0.05);

        // A step exactly dividing the length ends on the last point once.
        let exact: Vec<LatLng> = path.positions(length / 4.0).collect();
        assert_eq!(exact.len(), 5);
        assert_eq!(exact[4], FETZER);
        // A step longer than the path: start and end.
        assert_eq!(
            path.positions(length * 10.0).collect::<Vec<_>>(),
            [COSMIC, FETZER]
        );
        // No usable step: straight to the end.
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                path.positions(bad).collect::<Vec<_>>(),
                [COSMIC, FETZER],
                "{bad}"
            );
        }
    }

    #[test]
    fn zero_length_legs() {
        // Repeated points are dropped, so no segment has zero length.
        let path = WalkPath::new([COSMIC, COSMIC, FETZER, FETZER, FETZER]).unwrap();
        assert_eq!(path.points(), [COSMIC, FETZER]);
        assert!(path.point_at(10.0).lat.is_finite());

        // A walk that ends where it starts is that one point, once.
        let still = WalkPath::new([FETZER, FETZER]).unwrap();
        assert_eq!(still.length_m(), 0.0);
        assert_eq!(still.point_at(3.0), FETZER);
        assert_eq!(still.positions(64.0).collect::<Vec<_>>(), [FETZER]);

        assert!(WalkPath::new([]).is_none());
    }

    fn stop(id: &str, at: LatLng) -> PlannedStop {
        PlannedStop {
            order: ActiveOrder {
                id: id.into(),
                customer_id: "usr_judge".into(),
                dropoff: Place {
                    id: None,
                    label: id.into(),
                    location: at,
                    note: None,
                },
            },
            distance_m: 0.0,
            eta_minutes: 0.0,
        }
    }

    #[test]
    fn path_joins_osrm_geometry_and_fallback_legs() {
        let davis = latlng(35.910957, -79.047906);
        // OSRM snaps to the street: its geometry starts and ends a few meters off the points.
        let osrm = Route {
            distance_m: 900.0,
            geometry: Some(vec![
                [-79.05505, 35.91318],
                [-79.0510, 35.9120],
                [-79.04795, 35.91093],
            ]),
            source: RouteSource::Osrm,
        };
        let fallback = Route {
            distance_m: 300.0,
            geometry: None,
            source: RouteSource::Fallback,
        };
        let plan = Plan {
            origin: COSMIC,
            restaurant: None,
            legs: vec![osrm, fallback],
            stops: vec![stop("ord_a", davis), stop("ord_b", FETZER)],
            pace: Pace::Brisk,
        };
        let path = path_of(&plan);
        assert_eq!(
            path.points(),
            [
                COSMIC,
                latlng(35.91318, -79.05505),
                latlng(35.9120, -79.0510),
                latlng(35.91093, -79.04795),
                davis,
                // The fallback leg: straight from Davis to Fetzer.
                FETZER,
            ]
        );

        // A drop-off at the restaurant itself adds no segment.
        let here = Plan {
            origin: COSMIC,
            restaurant: None,
            legs: vec![Route {
                distance_m: 0.0,
                geometry: None,
                source: RouteSource::Fallback,
            }],
            stops: vec![stop("ord_c", COSMIC)],
            pace: Pace::Brisk,
        };
        assert_eq!(path_of(&here).points(), [COSMIC]);
    }

    #[test]
    fn settings_from_config() {
        let mut vars = std::collections::HashMap::new();
        vars.insert(
            "DATABASE_URL".to_owned(),
            "postgres://u@127.0.0.1/db".to_owned(),
        );
        let config = Config::from_vars(&vars).unwrap();
        assert_eq!(
            WalkSettings::from_config(&config),
            WalkSettings {
                interval: Duration::from_secs(5),
                speed: 8.0
            }
        );
    }
}
