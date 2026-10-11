//! The routing client: OSRM on the FOSSGIS server (`routed-foot` for walking, `routed-car` for
//! car distance), with a 10-minute cache, a 3 s timeout, one retry and a straight-line fallback.
//! It always answers: when OSRM fails, the route is the straight line × 1.3 with no geometry.
//! docs/DECISIONS.md (Routing) records the URL format, verified against the live server.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use reqwest::header::USER_AGENT as USER_AGENT_HEADER;
use serde::{Deserialize, Serialize};

use super::geo::{LatLng, Pace, eta_minutes, fallback_distance_m, grid_key, is_valid, latlng};
use crate::config::Config;

/// The FOSSGIS OSRM server, the default for `OSRM_BASE_URL`.
pub const OSRM_DEFAULT_BASE_URL: &str = "https://routing.openstreetmap.de";

/// The User-Agent every OSRM request carries, naming the project (FOSSGIS asks for one).
pub const USER_AGENT: &str = "Ferry-HackNC2026/0.1 (+https://github.com/Trade42/HackNC2026)";

/// Which OSRM instance to ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Profile {
    Foot,
    Car,
}

impl Profile {
    /// The FOSSGIS path prefix that selects the instance.
    fn path(self) -> &'static str {
        match self {
            Profile::Foot => "routed-foot",
            Profile::Car => "routed-car",
        }
    }
}

/// Where a route's distance came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteSource {
    /// OSRM's route along streets and paths.
    Osrm,
    /// Straight-line distance × 1.3, because OSRM failed or is resting after a failure.
    Fallback,
}

/// One route between two points.
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    /// Meters along the route (OSRM), or the fallback distance.
    pub distance_m: f64,
    /// GeoJSON positions, `[lng, lat]`, at least 2. `None` for the fallback.
    pub geometry: Option<Vec<[f64; 2]>>,
    pub source: RouteSource,
}

impl Route {
    fn fallback(from: LatLng, to: LatLng) -> Self {
        Self {
            distance_m: fallback_distance_m(from, to),
            geometry: None,
            source: RouteSource::Fallback,
        }
    }
}

/// `GeoJSONLineString` in contract/openapi.yaml: `{type: "LineString", coordinates}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LineString {
    #[serde(rename = "type")]
    kind: &'static str,
    pub coordinates: Vec<[f64; 2]>,
}

impl LineString {
    /// A LineString from at least 2 positions; `None` with fewer.
    pub fn new(coordinates: Vec<[f64; 2]>) -> Option<Self> {
        (coordinates.len() >= 2).then_some(Self {
            kind: "LineString",
            coordinates,
        })
    }
}

/// A walk between two points at a pace: what other modules get from `tracking::walk`.
#[derive(Debug, Clone, PartialEq)]
pub struct Walk {
    pub distance_m: f64,
    /// Whole minutes at the pace, rounded up (`geo::eta_minutes`).
    pub eta_minutes: f64,
    /// The walking route; `None` when the distance is the fallback.
    pub route: Option<LineString>,
    pub source: RouteSource,
}

/// Timings and limits. [`RoutingSettings::new`] has the spec's values; tests shorten them.
#[derive(Debug, Clone)]
pub struct RoutingSettings {
    /// Without a trailing slash.
    pub base_url: String,
    /// Per attempt, connect to last body byte. There is one retry.
    pub timeout: Duration,
    /// How long an OSRM answer is reused for the same (profile, origin, destination).
    pub cache_ttl: Duration,
    /// After OSRM times out, refuses connections or answers 5xx or 429 (after the retry), every
    /// route uses the fallback for this long without calling OSRM, so a dead server costs one
    /// slow request per window instead of 6 s on every call.
    pub rest_after_failure: Duration,
    /// Cached routes kept at most; the oldest goes first.
    pub max_cache_entries: usize,
    /// Least time between two OSRM requests from this client. FOSSGIS's usage policy allows one
    /// request per second.
    pub min_interval: Duration,
    /// The longest a route waits for its request slot. A route that would wait longer gets the
    /// fallback (not cached), so a burst never queues for long.
    pub max_slot_wait: Duration,
}

impl RoutingSettings {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            timeout: Duration::from_secs(3),
            cache_ttl: Duration::from_secs(10 * 60),
            rest_after_failure: Duration::from_secs(60),
            max_cache_entries: 10_000,
            min_interval: Duration::from_secs(1),
            max_slot_wait: Duration::from_secs(2),
        }
    }

    /// `OSRM_BASE_URL`, or the FOSSGIS server when it is unset.
    pub fn from_config(config: &Config) -> Self {
        Self::new(
            config
                .osrm_base_url
                .as_deref()
                .unwrap_or(OSRM_DEFAULT_BASE_URL),
        )
    }
}

/// The client. Cheap to clone; `AppState.routing` holds the one the app uses.
#[derive(Clone)]
pub struct Routing(Arc<Inner>);

struct Inner {
    http: reqwest::Client,
    settings: RoutingSettings,
    cache: Mutex<HashMap<CacheKey, CacheEntry>>,
    resting_until: Mutex<Option<Instant>>,
    /// The earliest time the next OSRM request may start.
    next_slot: Mutex<Instant>,
}

/// (profile, origin, destination), the points on a grid of about 10 m (`LatLng::grid_key`).
type CacheKey = (Profile, (i64, i64), (i64, i64));

struct CacheEntry {
    route: Route,
    stored: Instant,
}

/// Why one OSRM attempt gave no route.
#[derive(Debug)]
enum Failure {
    /// Timeout, connection error, 5xx or an unreadable body: retried once, then OSRM rests.
    Transient(String),
    /// 429: not retried; OSRM rests.
    Overloaded,
    /// OSRM answered, but with no usable route (4xx such as NoRoute, or `code` not "Ok"). The
    /// fallback is cached like a route, so the same pair does not ask again for 10 minutes.
    NoRoute(String),
    /// No request slot within `max_slot_wait`: the fallback this once, not cached.
    Busy,
}

#[derive(Deserialize)]
struct OsrmResponse {
    code: String,
    #[serde(default)]
    routes: Vec<OsrmRoute>,
}

#[derive(Deserialize)]
struct OsrmRoute {
    distance: f64,
    geometry: OsrmGeometry,
}

#[derive(Deserialize)]
struct OsrmGeometry {
    coordinates: Vec<[f64; 2]>,
}

impl Routing {
    /// A client that sends requests with `http` (the app's shared client; each OSRM request sets
    /// its own timeout and User-Agent).
    pub fn new(http: reqwest::Client, settings: RoutingSettings) -> Self {
        Self(Arc::new(Inner {
            http,
            settings,
            cache: Mutex::default(),
            resting_until: Mutex::default(),
            next_slot: Mutex::new(Instant::now()),
        }))
    }

    pub fn settings(&self) -> &RoutingSettings {
        &self.0.settings
    }

    /// The route from `from` to `to`: OSRM's, from the cache when it is under 10 minutes old,
    /// otherwise the fallback. Never fails. Invalid coordinates get the fallback without a call.
    pub async fn route(&self, profile: Profile, from: LatLng, to: LatLng) -> Route {
        if !is_valid(from) || !is_valid(to) {
            return Route::fallback(from, to);
        }
        let key = (profile, grid_key(from), grid_key(to));
        if let Some(route) = self.cached(&key) {
            return route;
        }
        if self.resting() {
            return Route::fallback(from, to);
        }
        let mut result = self.paced_fetch(profile, from, to).await;
        if matches!(result, Err(Failure::Transient(_))) {
            result = self.paced_fetch(profile, from, to).await;
        }
        match result {
            Ok(route) => {
                self.store(key, route.clone());
                route
            }
            Err(Failure::NoRoute(reason)) => {
                tracing::info!(profile = profile.path(), %reason, "osrm: no route; using the straight-line fallback");
                let route = Route::fallback(from, to);
                self.store(key, route.clone());
                route
            }
            Err(Failure::Busy) => {
                tracing::debug!(
                    profile = profile.path(),
                    "osrm: no request slot; using the straight-line fallback"
                );
                Route::fallback(from, to)
            }
            Err(failure) => {
                let rest = self.0.settings.rest_after_failure;
                let reason = match failure {
                    Failure::Transient(reason) => reason,
                    Failure::Overloaded => "HTTP 429".to_owned(),
                    Failure::NoRoute(reason) => reason,
                    Failure::Busy => "busy".to_owned(),
                };
                tracing::warn!(
                    profile = profile.path(),
                    %reason,
                    "osrm unavailable; using the straight-line fallback for {}s",
                    rest.as_secs()
                );
                *lock(&self.0.resting_until) = Some(Instant::now() + rest);
                Route::fallback(from, to)
            }
        }
    }

    /// A walk at `pace`: distance, ETA and route (`None` on the fallback).
    pub async fn walk(&self, from: LatLng, to: LatLng, pace: Pace) -> Walk {
        let route = self.route(Profile::Foot, from, to).await;
        Walk {
            distance_m: route.distance_m,
            eta_minutes: eta_minutes(route.distance_m, pace),
            route: route.geometry.and_then(LineString::new),
            source: route.source,
        }
    }

    /// Driving distance in meters (OSRM's car route, else straight line × 1.3), for the CO2
    /// estimate.
    pub async fn car_distance_m(&self, from: LatLng, to: LatLng) -> f64 {
        self.route(Profile::Car, from, to).await.distance_m
    }

    fn cached(&self, key: &CacheKey) -> Option<Route> {
        let ttl = self.0.settings.cache_ttl;
        let cache = lock(&self.0.cache);
        let entry = cache.get(key)?;
        (entry.stored.elapsed() < ttl).then(|| entry.route.clone())
    }

    fn store(&self, key: CacheKey, route: Route) {
        let settings = &self.0.settings;
        let mut cache = lock(&self.0.cache);
        cache.retain(|_, entry| entry.stored.elapsed() < settings.cache_ttl);
        while cache.len() >= settings.max_cache_entries.max(1) {
            let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, entry)| entry.stored)
                .map(|(key, _)| *key)
            else {
                break;
            };
            cache.remove(&oldest);
        }
        cache.insert(
            key,
            CacheEntry {
                route,
                stored: Instant::now(),
            },
        );
    }

    fn resting(&self) -> bool {
        let mut until = lock(&self.0.resting_until);
        match *until {
            Some(t) if Instant::now() < t => true,
            Some(_) => {
                *until = None;
                false
            }
            None => false,
        }
    }

    /// One OSRM request in this client's next free slot, or `Busy`.
    async fn paced_fetch(
        &self,
        profile: Profile,
        from: LatLng,
        to: LatLng,
    ) -> Result<Route, Failure> {
        let settings = &self.0.settings;
        let wait = {
            let mut next = lock(&self.0.next_slot);
            let now = Instant::now();
            let slot = (*next).max(now);
            if slot - now > settings.max_slot_wait {
                return Err(Failure::Busy);
            }
            *next = slot + settings.min_interval;
            slot - now
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        self.fetch(profile, from, to).await
    }

    /// One OSRM request.
    async fn fetch(&self, profile: Profile, from: LatLng, to: LatLng) -> Result<Route, Failure> {
        let settings = &self.0.settings;
        let url = format!(
            "{}/{}/route/v1/driving/{:.6},{:.6};{:.6},{:.6}?overview=full&geometries=geojson&steps=false",
            settings.base_url,
            profile.path(),
            from.lng,
            from.lat,
            to.lng,
            to.lat
        );
        let response = self
            .0
            .http
            .get(&url)
            .timeout(settings.timeout)
            .header(USER_AGENT_HEADER, USER_AGENT)
            .send()
            .await
            .map_err(|e| Failure::Transient(describe(&e)))?;
        let status = response.status();
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(Failure::Overloaded);
        }
        if status.is_server_error() {
            return Err(Failure::Transient(format!("HTTP {}", status.as_u16())));
        }
        let body = response
            .bytes()
            .await
            .map_err(|e| Failure::Transient(describe(&e)))?;
        let parsed: Option<OsrmResponse> = serde_json::from_slice(&body).ok();
        if status.is_client_error() {
            let code = parsed.map_or_else(|| "no code".to_owned(), |p| p.code);
            return Err(Failure::NoRoute(format!("HTTP {} {code}", status.as_u16())));
        }
        let parsed =
            parsed.ok_or_else(|| Failure::Transient("unreadable OSRM response".to_owned()))?;
        if parsed.code != "Ok" {
            return Err(Failure::NoRoute(parsed.code));
        }
        let route = parsed
            .routes
            .into_iter()
            .next()
            .ok_or_else(|| Failure::NoRoute("no routes".to_owned()))?;
        if !route.distance.is_finite() || route.distance < 0.0 {
            return Err(Failure::NoRoute("bad distance".to_owned()));
        }
        let mut coordinates: Vec<[f64; 2]> = route
            .geometry
            .coordinates
            .into_iter()
            .filter(|[lng, lat]| is_valid(latlng(*lat, *lng)))
            .collect();
        if coordinates.len() == 1 {
            coordinates.push(coordinates[0]);
        }
        Ok(Route {
            distance_m: route.distance,
            geometry: (coordinates.len() >= 2).then_some(coordinates),
            source: RouteSource::Osrm,
        })
    }
}

/// A reqwest error without its URL (the URL carries the runner's coordinates).
fn describe(err: &reqwest::Error) -> String {
    let kind = if err.is_timeout() {
        "timeout"
    } else if err.is_connect() {
        "connect"
    } else if err.is_body() || err.is_decode() {
        "body"
    } else {
        "request"
    };
    kind.to_owned()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Router;
    use axum::extract::{Path, State};
    use axum::http::HeaderMap;
    use axum::response::IntoResponse;
    use axum::routing::get;

    pub const COSMIC: LatLng = latlng(35.913211, -79.055094);
    pub const HINTON: LatLng = latlng(35.902364, -79.043231);

    /// How the stub answers.
    #[derive(Clone, Copy)]
    pub enum Mode {
        /// 200 with a route of `distance` meters and 3 positions.
        Ok { distance: f64 },
        /// Sleeps this long, then answers like `Ok`.
        Slow(Duration),
        /// This HTTP status with an OSRM error body.
        Status(u16),
    }

    #[derive(Clone)]
    struct Stub {
        mode: Mode,
        hits: Arc<AtomicUsize>,
        user_agents: Arc<Mutex<Vec<String>>>,
        paths: Arc<Mutex<Vec<String>>>,
    }

    pub struct StubServer {
        pub base_url: String,
        hits: Arc<AtomicUsize>,
        user_agents: Arc<Mutex<Vec<String>>>,
        paths: Arc<Mutex<Vec<String>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl StubServer {
        pub fn hits(&self) -> usize {
            self.hits.load(Ordering::SeqCst)
        }
    }

    impl Drop for StubServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn handle(
        State(stub): State<Stub>,
        Path((profile, coords)): Path<(String, String)>,
        headers: HeaderMap,
    ) -> axum::response::Response {
        stub.hits.fetch_add(1, Ordering::SeqCst);
        stub.paths
            .lock()
            .unwrap()
            .push(format!("{profile} {coords}"));
        if let Some(ua) = headers.get("user-agent").and_then(|v| v.to_str().ok()) {
            stub.user_agents.lock().unwrap().push(ua.to_owned());
        }
        let distance = match stub.mode {
            Mode::Ok { distance } => distance,
            Mode::Slow(wait) => {
                tokio::time::sleep(wait).await;
                1000.0
            }
            Mode::Status(code) => {
                let status = axum::http::StatusCode::from_u16(code).unwrap();
                return (status, axum::Json(serde_json::json!({"code": "NoRoute"})))
                    .into_response();
            }
        };
        axum::Json(serde_json::json!({
            "code": "Ok",
            "routes": [{
                "distance": distance,
                "duration": distance / 1.25,
                "geometry": {"type": "LineString", "coordinates": [[-79.05, 35.91], [-79.048, 35.906], [-79.043, 35.902]]}
            }],
            "waypoints": []
        }))
        .into_response()
    }

    pub async fn stub(mode: Mode) -> StubServer {
        let hits = Arc::new(AtomicUsize::new(0));
        let user_agents = Arc::new(Mutex::new(Vec::new()));
        let paths = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/{profile}/route/v1/driving/{coords}", get(handle))
            .with_state(Stub {
                mode,
                hits: hits.clone(),
                user_agents: user_agents.clone(),
                paths: paths.clone(),
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        StubServer {
            base_url: format!("http://{addr}"),
            hits,
            user_agents,
            paths,
            task,
        }
    }

    /// Settings for tests: a 300 ms timeout and no pacing.
    pub fn settings(base_url: &str) -> RoutingSettings {
        let mut settings = RoutingSettings::new(base_url);
        settings.timeout = Duration::from_millis(300);
        settings.min_interval = Duration::ZERO;
        settings
    }

    pub fn client(base_url: &str) -> Routing {
        Routing::new(crate::state::http_client().unwrap(), settings(base_url))
    }

    #[tokio::test]
    async fn osrm_route_with_geometry_and_user_agent() {
        let server = stub(Mode::Ok { distance: 1861.7 }).await;
        let routing = client(&server.base_url);
        let route = routing.route(Profile::Foot, COSMIC, HINTON).await;
        assert_eq!(route.source, RouteSource::Osrm);
        assert_eq!(route.distance_m, 1861.7);
        assert_eq!(route.geometry.as_ref().unwrap().len(), 3);
        assert_eq!(server.user_agents.lock().unwrap().as_slice(), [USER_AGENT]);
        assert_eq!(
            server.paths.lock().unwrap().as_slice(),
            ["routed-foot -79.055094,35.913211;-79.043231,35.902364"]
        );

        let walk = routing.walk(COSMIC, HINTON, Pace::Brisk).await;
        // 1861.7 m at 1.6 m/s is 19.4 minutes: 20.
        assert_eq!(walk.eta_minutes, 20.0);
        let line = serde_json::to_value(walk.route.unwrap()).unwrap();
        assert_eq!(line["type"], "LineString");
        assert_eq!(line["coordinates"][0], serde_json::json!([-79.05, 35.91]));
        // The walk came from the cache.
        assert_eq!(server.hits(), 1);

        let car = routing.car_distance_m(COSMIC, HINTON).await;
        assert_eq!(car, 1861.7);
        assert_eq!(
            server.paths.lock().unwrap()[1].split(' ').next(),
            Some("routed-car")
        );
    }

    #[tokio::test]
    async fn cache_hit_within_ttl_and_miss_after() {
        let server = stub(Mode::Ok { distance: 500.0 }).await;
        let mut settings = settings(&server.base_url);
        settings.cache_ttl = Duration::from_millis(400);
        let routing = Routing::new(crate::state::http_client().unwrap(), settings);

        routing.route(Profile::Foot, COSMIC, HINTON).await;
        routing.route(Profile::Foot, COSMIC, HINTON).await;
        // A point 3 m away shares the cache key.
        let near = latlng(COSMIC.lat + 0.000_02, COSMIC.lng + 0.000_02);
        let hit = routing.route(Profile::Foot, near, HINTON).await;
        assert_eq!(hit.source, RouteSource::Osrm);
        assert_eq!(
            server.hits(),
            1,
            "second and third calls come from the cache"
        );

        // The other direction and the other profile are different keys.
        routing.route(Profile::Foot, HINTON, COSMIC).await;
        routing.route(Profile::Car, COSMIC, HINTON).await;
        assert_eq!(server.hits(), 3);

        tokio::time::sleep(Duration::from_millis(500)).await;
        routing.route(Profile::Foot, COSMIC, HINTON).await;
        assert_eq!(server.hits(), 4, "expired after the TTL");
    }

    #[test]
    fn default_settings_are_the_spec_values() {
        let settings = RoutingSettings::new("https://routing.openstreetmap.de/");
        assert_eq!(settings.base_url, OSRM_DEFAULT_BASE_URL);
        assert_eq!(settings.timeout, Duration::from_secs(3));
        assert_eq!(settings.cache_ttl, Duration::from_secs(600));
    }

    #[tokio::test]
    async fn timeout_retries_once_then_falls_back_and_rests() {
        let server = stub(Mode::Slow(Duration::from_secs(2))).await;
        let routing = client(&server.base_url);
        let started = Instant::now();
        let route = routing.route(Profile::Foot, COSMIC, HINTON).await;
        assert_eq!(route.source, RouteSource::Fallback);
        assert_eq!(route.geometry, None);
        assert!((route.distance_m - fallback_distance_m(COSMIC, HINTON)).abs() < 1e-9);
        assert_eq!(server.hits(), 2, "one retry");
        assert!(started.elapsed() < Duration::from_millis(1500));

        // Resting: the next call falls back at once without asking OSRM, and is not cached.
        let started = Instant::now();
        let again = routing.route(Profile::Foot, HINTON, COSMIC).await;
        assert_eq!(again.source, RouteSource::Fallback);
        assert!(started.elapsed() < Duration::from_millis(100));
        assert_eq!(server.hits(), 2);
        assert!(
            routing
                .cached(&(Profile::Foot, grid_key(COSMIC), grid_key(HINTON)))
                .is_none()
        );
    }

    #[tokio::test]
    async fn rest_ends_and_osrm_is_asked_again() {
        let server = stub(Mode::Status(503)).await;
        let mut settings = settings(&server.base_url);
        settings.rest_after_failure = Duration::from_millis(300);
        let routing = Routing::new(crate::state::http_client().unwrap(), settings);
        assert_eq!(
            routing.route(Profile::Foot, COSMIC, HINTON).await.source,
            RouteSource::Fallback
        );
        assert_eq!(server.hits(), 2, "a 5xx is retried once");
        routing.route(Profile::Foot, COSMIC, HINTON).await;
        assert_eq!(server.hits(), 2, "resting");
        tokio::time::sleep(Duration::from_millis(700)).await;
        routing.route(Profile::Foot, COSMIC, HINTON).await;
        assert_eq!(
            server.hits(),
            4,
            "asked again (with its retry) after the rest"
        );
    }

    #[tokio::test]
    async fn no_route_is_not_retried_and_its_fallback_is_cached() {
        let server = stub(Mode::Status(400)).await;
        let routing = client(&server.base_url);
        let route = routing.route(Profile::Foot, COSMIC, HINTON).await;
        assert_eq!(route.source, RouteSource::Fallback);
        assert_eq!(server.hits(), 1);
        routing.route(Profile::Foot, COSMIC, HINTON).await;
        assert_eq!(server.hits(), 1, "cached");
        // Not resting: another pair still asks OSRM.
        routing.route(Profile::Foot, HINTON, COSMIC).await;
        assert_eq!(server.hits(), 2);
    }

    #[tokio::test]
    async fn too_many_requests_is_not_retried() {
        let server = stub(Mode::Status(429)).await;
        let routing = client(&server.base_url);
        assert_eq!(
            routing.route(Profile::Foot, COSMIC, HINTON).await.source,
            RouteSource::Fallback
        );
        assert_eq!(server.hits(), 1);
        routing.route(Profile::Foot, HINTON, COSMIC).await;
        assert_eq!(server.hits(), 1, "resting after a 429");
    }

    #[tokio::test]
    async fn unreachable_server_falls_back() {
        // Port 1 on localhost refuses connections.
        let routing = client("http://127.0.0.1:1");
        let walk = routing.walk(COSMIC, HINTON, Pace::Normal).await;
        assert_eq!(walk.source, RouteSource::Fallback);
        assert_eq!(walk.route, None);
        assert_eq!(walk.eta_minutes, 25.0);
        assert!(
            (routing.car_distance_m(COSMIC, HINTON).await - fallback_distance_m(COSMIC, HINTON))
                .abs()
                < 1e-9
        );
    }

    #[tokio::test]
    async fn invalid_coordinates_never_reach_osrm() {
        let server = stub(Mode::Ok { distance: 1.0 }).await;
        let routing = client(&server.base_url);
        let route = routing
            .route(Profile::Foot, latlng(95.0, 0.0), HINTON)
            .await;
        assert_eq!(route.source, RouteSource::Fallback);
        assert_eq!(server.hits(), 0);
    }

    #[tokio::test]
    async fn cache_is_bounded() {
        let server = stub(Mode::Ok { distance: 10.0 }).await;
        let mut settings = settings(&server.base_url);
        settings.max_cache_entries = 2;
        let routing = Routing::new(crate::state::http_client().unwrap(), settings);
        for i in 0..4 {
            let from = latlng(35.9 + f64::from(i) * 0.001, -79.05);
            routing.route(Profile::Foot, from, HINTON).await;
        }
        assert_eq!(lock(&routing.0.cache).len(), 2);
    }

    #[tokio::test]
    async fn requests_are_paced_and_a_burst_sheds_to_the_fallback() {
        // Wide margins, so a loaded machine cannot flip the outcome: a task would have to start
        // 700 ms late to get a slot it should not.
        let server = stub(Mode::Ok { distance: 10.0 }).await;
        let mut settings = settings(&server.base_url);
        settings.min_interval = Duration::from_secs(1);
        settings.max_slot_wait = Duration::from_millis(300);
        let routing = Routing::new(crate::state::http_client().unwrap(), settings);
        let mut tasks = Vec::new();
        for i in 0..4 {
            let routing = routing.clone();
            let from = latlng(35.9 + f64::from(i) * 0.001, -79.05);
            tasks.push(tokio::spawn(async move {
                routing.route(Profile::Foot, from, HINTON).await.source
            }));
        }
        let mut sources = Vec::new();
        for task in tasks {
            sources.push(task.await.unwrap());
        }
        // One slot now; the next is a second away, over the 300 ms limit, so the rest shed.
        assert_eq!(server.hits(), 1, "{sources:?}");
        assert_eq!(
            sources
                .iter()
                .filter(|s| **s == RouteSource::Fallback)
                .count(),
            3
        );

        // A shed route is not cached and does not rest OSRM: after the interval it gets a slot.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let from = latlng(35.902, -79.05);
        assert_eq!(
            routing.route(Profile::Foot, from, HINTON).await.source,
            RouteSource::Osrm
        );
        // A route whose slot is under 300 ms away waits for it instead of shedding.
        tokio::time::sleep(Duration::from_millis(800)).await;
        let from = latlng(35.903, -79.05);
        assert_eq!(
            routing.route(Profile::Foot, from, HINTON).await.source,
            RouteSource::Osrm
        );
        assert_eq!(server.hits(), 3);
    }

    #[test]
    fn default_pacing_is_one_request_per_second() {
        let settings = RoutingSettings::new(OSRM_DEFAULT_BASE_URL);
        assert_eq!(settings.min_interval, Duration::from_secs(1));
        assert_eq!(settings.max_slot_wait, Duration::from_secs(2));
    }
}
