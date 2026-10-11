//! The bridge to the Spectrum sidecar: /internal/messages/inbound, and calls to the sidecar.

use axum::Router;
use axum::routing::post;

use crate::state::AppState;

pub mod inbound;
pub mod mask;
pub mod sidecar;

/// This module's routes, with full paths. Merged into the app by `app::router`.
pub fn router() -> Router<AppState> {
    Router::new().route("/internal/messages/inbound", post(inbound::inbound))
}
