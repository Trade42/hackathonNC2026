//! The state every handler can reach: `State<AppState>`, or one field such as `State<PgPool>`.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::FromRef;
use sqlx::PgPool;

use crate::config::Config;
use crate::jwt::JwtKeys;

#[derive(Clone, FromRef)]
pub struct AppState {
    pub pool: PgPool,
    pub config: Arc<Config>,
    /// One client for every outbound call (Supabase Storage, Nessie, Gemini, OSRM, the sidecar).
    pub http: reqwest::Client,
    pub jwt: JwtKeys,
    /// The realtime event hub (`/v1/stream`); see `notifications::Hub`.
    pub hub: crate::notifications::Hub,
    /// OSRM routes with the 10-minute cache and the straight-line fallback; see `tracking::Routing`.
    pub routing: crate::tracking::Routing,
    /// Gemini's circuit breaker, one per app; see `agent::breaker`.
    pub gemini_breaker: crate::agent::breaker::Breaker,
}

impl AppState {
    pub fn new(pool: PgPool, config: Config) -> Result<Self, reqwest::Error> {
        let jwt = JwtKeys::from_config(&config);
        let http = http_client()?;
        let routing = crate::tracking::Routing::new(
            http.clone(),
            crate::tracking::RoutingSettings::from_config(&config),
        );
        Ok(Self {
            pool,
            config: Arc::new(config),
            http,
            jwt,
            hub: crate::notifications::Hub::default(),
            routing,
            gemini_breaker: crate::agent::breaker::Breaker::default(),
        })
    }
}

/// Connect timeout 5 s, whole-request timeout 10 s. A caller that needs less sets a shorter
/// timeout on its request (`RequestBuilder::timeout`).
pub fn http_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .user_agent(concat!("ferry-core/", env!("CARGO_PKG_VERSION")))
        .build()
}
