//! `GET /healthz`: unauthenticated liveness plus a database check.

use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;

/// How long the database check may take.
pub const DB_CHECK_TIMEOUT: Duration = Duration::from_secs(2);

/// 200 `{"status":"ok","db":"ok"}` when `SELECT 1` answers within 2 s, otherwise
/// 503 `{"status":"degraded","db":"unreachable"}`.
pub async fn healthz(State(pool): State<PgPool>) -> (StatusCode, Json<Value>) {
    match tokio::time::timeout(DB_CHECK_TIMEOUT, sqlx::query("SELECT 1").execute(&pool)).await {
        Ok(Ok(_)) => (StatusCode::OK, Json(json!({"status": "ok", "db": "ok"}))),
        result => {
            match result {
                Ok(Err(err)) => tracing::warn!("healthz: database check failed: {err}"),
                _ => tracing::warn!("healthz: database check timed out"),
            }
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"status": "degraded", "db": "unreachable"})),
            )
        }
    }
}
