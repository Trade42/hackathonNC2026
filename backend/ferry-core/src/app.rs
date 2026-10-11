//! The HTTP app: routes, the error fallbacks, CORS, tracing and compression.

use std::borrow::Cow;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Method, Request, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use rand::RngExt;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;

use crate::config::Config;
use crate::error::{ApiError, ErrorCode, ErrorLogged};
use crate::state::AppState;
use crate::{
    agent, auth, catalog, circles, demo, health, impact, invites, messaging, notifications, orders,
    payments, profile, requests, runs, tracking,
};

/// Origins that may call the API from a browser or a native wrapper (spec section 1), plus the
/// origin of `PUBLIC_WEB_URL`. `https://localhost` is the Android app's (Capacitor's default
/// scheme there; `capacitor://localhost` is iOS's). The web app is `https://ferrygo.tech` (Sal,
/// 2026-10-11), and `https://ferry-hacknc.vercel.app` is its current Vercel deploy, listed so it
/// keeps working when `PUBLIC_WEB_URL` moves to the new domain. Lowercase, as browsers send them.
pub const ALLOWED_ORIGINS: [&str; 10] = [
    "http://localhost:5173",
    "http://127.0.0.1:5173",
    "capacitor://localhost",
    "http://localhost",
    "https://localhost",
    "tauri://localhost",
    "https://tauri.localhost",
    "https://ferrygo.tech",
    "https://www.ferrygo.tech",
    "https://ferry-hacknc.vercel.app",
];

/// The prefix of the Telegram sign-in routes; what follows it is `start` or a challenge id.
const TELEGRAM_AUTH_PREFIX: &str = "/v1/auth/telegram/";

/// The response header that carries the request's id.
pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// The longest path the logs show; a longer one is cut there and ends with `…`.
pub const MAX_LOGGED_PATH: usize = 200;

/// The whole app. Each area module owns its routes, written with their full `/v1/...` paths as
/// in contract/openapi.yaml.
pub fn router(state: AppState) -> Router {
    router_with(state, Router::new())
}

/// [`router`] with `extra` routes merged in, under the same fallbacks and layers: tests mount a
/// route the app does not have this way.
pub fn router_with(state: AppState, extra: Router<AppState>) -> Router {
    let cors = cors_layer(&state.config);
    Router::new()
        .route("/healthz", get(health::healthz))
        .merge(auth::router())
        .merge(profile::router())
        .merge(catalog::router())
        .merge(runs::router())
        .merge(orders::router())
        .merge(requests::router())
        .merge(payments::router())
        .merge(tracking::router())
        .merge(circles::router())
        .merge(invites::router())
        .merge(notifications::router_with_push(&state))
        .merge(impact::router())
        .merge(agent::router())
        .merge(messaging::router())
        .merge(demo::router())
        .merge(extra)
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        // Layers apply to the routes and fallbacks above; the last one added runs first.
        .layer(CompressionLayer::new())
        .layer(cors)
        // Inside the trace layer, so its line carries the request id.
        .layer(middleware::from_fn(log_server_errors))
        .layer(
            TraceLayer::new_for_http()
                // The request id, method and path only: the query string can carry a token
                // (/v1/stream?token=), the path is redacted where it carries a challenge id, and
                // no body is ever logged.
                .make_span_with(|req: &Request<_>| {
                    let id = req.extensions().get::<RequestId>().map_or("-", |id| &id.0);
                    let path = clip_for_logs(path_for_logs(req.uri().path()));
                    tracing::info_span!("http", request_id = %id, method = %req.method(), path = %path)
                })
                .on_response(DefaultOnResponse::new().level(Level::INFO))
                // A 5xx's one ERROR line is `ApiError::internal`'s, which has the cause, or else
                // `log_server_errors`'s; the response line above still records the status.
                .on_failure(()),
        )
        // Outermost, so the trace span above sees the id.
        .layer(middleware::from_fn(request_id))
        .with_state(state)
}

/// A request's id: 16 random hex characters, new for every request (a client's own
/// `x-request-id` is ignored, so it cannot forge log lines). The trace span puts it on every log
/// line of the request, and the response carries it in `x-request-id`, so a 500 someone reports
/// leads to its one log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(pub String);

impl RequestId {
    pub fn new() -> Self {
        Self(format!("{:016x}", rand::rng().random::<u64>()))
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

async fn request_id(mut req: Request<Body>, next: Next) -> Response {
    let id = RequestId::new();
    let value = HeaderValue::from_str(&id.0).expect("hex is a valid header value");
    req.extensions_mut().insert(id);
    let mut response = next.run(req).await;
    response.headers_mut().insert(REQUEST_ID_HEADER, value);
    response
}

/// One ERROR line for a 5xx whose cause nobody logged at ERROR: axum's own 500 (a `Json` response
/// that fails to serialize, say), a 503 `upstream_unavailable`, a 5xx from a layer. A response
/// from `ApiError::internal` carries [`ErrorLogged`]: its line, with the cause, is already written.
/// Together they give every 5xx exactly one ERROR line.
async fn log_server_errors(req: Request<Body>, next: Next) -> Response {
    let response = next.run(req).await;
    let status = response.status();
    if status.is_server_error() && response.extensions().get::<ErrorLogged>().is_none() {
        tracing::error!(status = status.as_u16(), "server error");
    }
    response
}

/// `path` cut to [`MAX_LOGGED_PATH`] characters, so a megabyte-long id makes a short log line.
pub fn clip_for_logs(path: Cow<'_, str>) -> Cow<'_, str> {
    match path.char_indices().nth(MAX_LOGGED_PATH) {
        Some((end, _)) => Cow::Owned(format!("{}…", &path[..end])),
        None => path,
    }
}

/// A request path as the logs show it. A Telegram sign-in challenge id is the only key to the
/// poll's token, so `/v1/auth/telegram/{challenge_id}` (and anything else after that prefix but
/// `start`) logs as `/v1/auth/telegram/:id`. Every other path logs as it is.
pub fn path_for_logs(path: &str) -> Cow<'_, str> {
    match path.strip_prefix(TELEGRAM_AUTH_PREFIX) {
        None | Some("start") => Cow::Borrowed(path),
        Some(_) => Cow::Owned(format!("{TELEGRAM_AUTH_PREFIX}:id")),
    }
}

/// The fixed allowlist plus the origin of `PUBLIC_WEB_URL`, without duplicates.
pub fn allowed_origins(config: &Config) -> Vec<HeaderValue> {
    let mut origins: Vec<HeaderValue> = ALLOWED_ORIGINS
        .iter()
        .map(|o| HeaderValue::from_static(o))
        .collect();
    if let Some(web) = &config.public_web_url {
        match url::Url::parse(web).map(|u| u.origin()) {
            Ok(origin) if origin.is_tuple() => {
                let value = HeaderValue::from_str(&origin.ascii_serialization())
                    .expect("a serialized origin is a valid header value");
                if !origins.contains(&value) {
                    origins.push(value);
                }
            }
            _ => tracing::warn!("PUBLIC_WEB_URL has no http(s) origin; it is not added to CORS"),
        }
    }
    origins
}

fn cors_layer(config: &Config) -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(allowed_origins(config)))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
        // So the web app can read a response's request id, to quote it in a report.
        .expose_headers([REQUEST_ID_HEADER])
        .max_age(Duration::from_secs(600))
}

async fn not_found(method: Method, uri: axum::http::Uri) -> ApiError {
    ApiError::not_found(format!("No route for {method} {}.", uri.path()))
}

/// The path exists but not with this method: 405, with the closest contract code.
async fn method_not_allowed(method: Method, uri: axum::http::Uri) -> ApiError {
    ApiError::new(
        ErrorCode::NotFound,
        format!("{method} is not allowed on {}.", uri.path()),
    )
    .with_status(axum::http::StatusCode::METHOD_NOT_ALLOWED)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_ids_never_reach_the_logged_path() {
        for path in [
            "/v1/auth/telegram/chl_0123456789abcdefghijklmnopqrstuv",
            "/v1/auth/telegram/nope",
            "/v1/auth/telegram/chl_x/extra",
            "/v1/auth/telegram//chl_x",
            "/v1/auth/telegram/start/chl_x",
            "/v1/auth/telegram/",
        ] {
            assert_eq!(path_for_logs(path), "/v1/auth/telegram/:id", "{path}");
        }
        for path in [
            "/v1/auth/telegram/start",
            "/v1/auth/demo",
            "/v1/runs/run_demo",
            "/healthz",
            "/v1/auth/telegram",
        ] {
            assert_eq!(path_for_logs(path), path);
        }
    }

    #[test]
    fn long_paths_are_clipped_in_logs() {
        let long = format!("/v1/runs/{}", "x".repeat(1_000_000));
        let clipped = clip_for_logs(Cow::Borrowed(&long));
        assert_eq!(clipped.chars().count(), MAX_LOGGED_PATH + 1);
        assert!(clipped.ends_with('…'));
        assert_eq!(
            clip_for_logs(Cow::Borrowed("/v1/runs/run_demo")),
            "/v1/runs/run_demo"
        );
        let accents = "é".repeat(MAX_LOGGED_PATH + 5);
        assert_eq!(
            clip_for_logs(Cow::Borrowed(&accents)).chars().count(),
            MAX_LOGGED_PATH + 1
        );
    }

    #[test]
    fn request_ids_are_16_hex_characters_and_new_each_time() {
        let a = RequestId::new();
        assert_eq!(a.0.len(), 16);
        assert!(a.0.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(a, RequestId::new());
    }

    #[test]
    fn origins_are_lowercase_and_unique() {
        for origin in ALLOWED_ORIGINS {
            assert_eq!(origin, origin.to_ascii_lowercase());
            assert_eq!(ALLOWED_ORIGINS.iter().filter(|o| **o == origin).count(), 1);
        }
    }
}
