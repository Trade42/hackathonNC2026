//! `GET /v1/impact` (bearer) and `GET /v1/public/impact` (no auth, cached about 30 s).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::extract::State;
use serde::Deserialize;
use sqlx::PgPool;

use super::report::{Impact, ImpactScope, Target, compute, impact_for};
use crate::error::ApiResult;
use crate::extract::{Json, Query};
use crate::jwt::AuthUser;
use crate::state::AppState;

/// How long the landing page's campus Impact is reused.
pub const PUBLIC_TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize)]
pub struct ImpactQuery {
    /// `me` when absent.
    pub scope: Option<ImpactScope>,
    pub circle_id: Option<String>,
}

/// `GET /v1/impact?scope=me|circle|campus&circle_id=`.
pub async fn get_impact(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<ImpactQuery>,
) -> ApiResult<Json<Impact>> {
    let impact = impact_for(
        &state.pool,
        &user.user_id,
        query.scope.unwrap_or(ImpactScope::Me),
        query.circle_id.as_deref(),
    )
    .await?;
    Ok(Json(impact))
}

/// `GET /v1/public/impact`: the campus scope, for the landing page.
pub async fn get_public_impact(State(pool): State<PgPool>) -> ApiResult<Json<Impact>> {
    Ok(Json(public_impact(&pool).await?))
}

/// One cached campus Impact per database, so test databases in one process never share one.
static PUBLIC_CACHE: LazyLock<Mutex<HashMap<String, (Instant, Impact)>>> =
    LazyLock::new(Mutex::default);

/// The campus Impact, computed at most once per [`PUBLIC_TTL`] per database.
pub async fn public_impact(pool: &PgPool) -> Result<Impact, sqlx::Error> {
    let key = cache_key(pool);
    if let Some(impact) = cached(&key) {
        return Ok(impact);
    }
    let impact = compute(pool, &Target::Campus).await?;
    let mut cache = PUBLIC_CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    cache.retain(|_, (stored, _)| stored.elapsed() < PUBLIC_TTL);
    cache.insert(key, (Instant::now(), impact.clone()));
    Ok(impact)
}

fn cached(key: &str) -> Option<Impact> {
    let cache = PUBLIC_CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    let (stored, impact) = cache.get(key)?;
    (stored.elapsed() < PUBLIC_TTL).then(|| impact.clone())
}

/// `host:port/database` of the pool: no user or password.
fn cache_key(pool: &PgPool) -> String {
    let options = pool.connect_options();
    format!(
        "{}:{}/{}",
        options.get_host(),
        options.get_port(),
        options.get_database().unwrap_or_default()
    )
}
