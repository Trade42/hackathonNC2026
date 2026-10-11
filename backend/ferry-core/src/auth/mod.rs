//! Sign-in: /v1/auth/*. Telegram sign-in (contract v1.3, `telegram`) replaced the text sign-in;
//! demo sign-in stays. The bot's half of Telegram sign-in is called from the messaging bridge.

use axum::Router;
use axum::routing::{get, post};

use crate::state::AppState;

pub mod demo;
pub mod limits;
pub mod telegram;

/// This module's routes, with full `/v1/...` paths. Merged into the app by `app::router`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/auth/telegram/start", post(telegram::start))
        .route("/v1/auth/telegram/{challenge_id}", get(telegram::status))
        .route("/v1/auth/demo", post(demo::sign_in))
}
