//! Pickup requests: `POST /v1/requests`, "request a Ferry" when no open run fits.
//!
//! A request never places an order and never holds money (the agent review's BLOCKER 1: no money
//! moves without the user's own tap). It finds an open run that fits, or tells the customer when
//! one opens; the customer then orders on that run themselves, as the frontend's checkout does
//! when the response has a `run_id`.
//!
//! - [`create`]: validate the body (as an order's, plus the restaurant open by `needed_by` and
//!   `needed_by` within 24 hours), store it as `searching`, and match it at once.
//! - [`matcher`]: detour-aware matching. A run fits when the drop-off adds at most
//!   [`matcher::MAX_EXTRA_MINUTES`] to the runner's walk, the food gets there by `needed_by`, and
//!   the drop-off is inside the run's drop-off zone when it has one. The fitting run with the
//!   least extra time wins: the request is `matched`, the customer gets a `run_open` notification
//!   with the `/r/{run_id}` link, and the runner a `system` one ("Someone near your route wants
//!   Cosmic Cantina").
//! - [`sweep`]: every 30 s, matches `searching` requests against the runs open now and expires
//!   those past `needed_by` (one `system` notification each). A request is matched at most once.

pub mod create;
pub mod matcher;
pub mod notices;
pub mod sweep;

use axum::Router;
use axum::extract::State;
use axum::routing::post;
use serde::{Deserialize, Serialize};

use crate::catalog::types::Place;
use crate::error::{ApiError, ApiResult};
use crate::extract::Json;
use crate::jwt::AuthUser;
use crate::orders::OrderLineInput;
use crate::state::AppState;

pub use create::create_request;
pub use sweep::{SWEEP_EVERY, Sweep, start, sweep_once};

/// This module's routes, with full `/v1/...` paths. Merged into the app by `app::router`.
pub fn router() -> Router<AppState> {
    Router::new().route("/v1/requests", post(create_handler))
}

/// `CreatePickupRequest` in contract/openapi.yaml.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CreatePickupRequest {
    pub restaurant_id: String,
    pub lines: Vec<OrderLineInput>,
    pub tip_cents: i64,
    pub dropoff: Place,
    pub needed_by: String,
    /// Contract v1.4, optional: how far from the drop-off the customer will walk to meet.
    #[serde(default)]
    pub dropoff_radius_m: Option<i64>,
}

/// `PickupRequestStatus` in contract/openapi.yaml.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PickupRequestStatus {
    Searching,
    Matched,
    Expired,
}

impl PickupRequestStatus {
    pub const ALL: [PickupRequestStatus; 3] = [
        PickupRequestStatus::Searching,
        PickupRequestStatus::Matched,
        PickupRequestStatus::Expired,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            PickupRequestStatus::Searching => "searching",
            PickupRequestStatus::Matched => "matched",
            PickupRequestStatus::Expired => "expired",
        }
    }

    pub fn from_db(value: &str) -> ApiResult<Self> {
        Self::ALL
            .into_iter()
            .find(|s| s.as_str() == value)
            .ok_or_else(|| ApiError::internal(format!("unknown request status {value:?}")))
    }
}

/// `PickupRequest` in contract/openapi.yaml. `run_id` and `dropoff_radius_m` are always sent,
/// null when not set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PickupRequest {
    pub id: String,
    pub status: PickupRequestStatus,
    pub run_id: Option<String>,
    pub dropoff_radius_m: Option<i32>,
}

async fn create_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<CreatePickupRequest>,
) -> ApiResult<Json<PickupRequest>> {
    Ok(Json(create_request(&state, &user.user_id, &body).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_match_the_contract() {
        let yaml = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contract/openapi.yaml"
        ))
        .unwrap();
        let line = yaml
            .lines()
            .skip_while(|l| l.trim() != "PickupRequestStatus:")
            .find(|l| l.trim_start().starts_with("enum:"))
            .unwrap();
        let listed: Vec<&str> = line
            .trim()
            .trim_start_matches("enum:")
            .trim()
            .trim_matches(['[', ']'])
            .split(',')
            .map(str::trim)
            .collect();
        let ours: Vec<&str> = PickupRequestStatus::ALL
            .iter()
            .map(|s| s.as_str())
            .collect();
        assert_eq!(listed, ours);
        for s in PickupRequestStatus::ALL {
            assert_eq!(serde_json::to_value(s).unwrap(), s.as_str());
            assert_eq!(PickupRequestStatus::from_db(s.as_str()).unwrap(), s);
        }
        assert!(PickupRequestStatus::from_db("nope").is_err());
    }
}
