//! CO2 impact: /v1/impact and /v1/public/impact, the carbon estimate per delivered order, and the
//! demo's past weeks of history.
//!
//! For other modules:
//!
//! ```ignore
//! // At delivery (runs-orders): driving meters from the restaurant to the drop-off (OSRM's car
//! // route, else straight line × 1.3) and the CO2 that car trip would have emitted.
//! let (car_distance_m, co2_saved_g) = impact::carbon_for_delivery(&state, restaurant, dropoff).await;
//! // The agent's explain_impact tool: the Impact a viewer may see, with the endpoint's rules
//! // (circle needs circle_id: 400; unknown circle: 404; not a member: 403).
//! let impact = impact::impact_for(&state.pool, &viewer_id, ImpactScope::Circle, Some("cir_hinton4")).await?;
//! // The same without a permission check.
//! let campus = impact::compute(&state.pool, &Target::Campus).await?;
//! ```

mod carbon;
pub mod history;
mod report;
mod routes;
mod weeks;

use axum::Router;
use axum::routing::get;

use crate::state::AppState;

pub use carbon::{
    CO2_G_PER_CAR_MILE, METERS_PER_MILE, METHOD, carbon_for_delivery, carbon_for_distance,
    fallback_carbon,
};
pub use history::{HistoryReport, seed_history};
pub use report::{Impact, ImpactScope, Target, compute, impact_for};
pub use routes::{PUBLIC_TTL, public_impact};
pub use weeks::{ImpactWeek, WEEKS};

/// This module's routes, with full `/v1/...` paths. Merged into the app by `app::router`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/impact", get(routes::get_impact))
        .route("/v1/public/impact", get(routes::get_public_impact))
}
