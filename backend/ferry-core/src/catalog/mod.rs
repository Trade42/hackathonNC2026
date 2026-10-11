//! Areas, restaurants, menus and drop-off places, which the demo seeder loads from
//! contract/fixtures, and the public client config: /v1/areas, /v1/restaurants,
//! /v1/restaurants/{id}/menu, /v1/places and /v1/public/config.

use axum::Router;
use axum::routing::get;

use crate::state::AppState;

pub mod handlers;
pub mod hours;
pub mod media;
pub mod public_config;
pub mod types;
pub mod walk;

/// This module's routes, with full `/v1/...` paths. Merged into the app by `app::router`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/areas", get(handlers::list_areas))
        .route("/v1/restaurants", get(handlers::list_restaurants))
        .route("/v1/restaurants/{id}/menu", get(handlers::get_menu))
        .route("/v1/places", get(handlers::list_places))
        .route("/v1/public/config", get(public_config::get_public_config))
}
