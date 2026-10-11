//! ETAs for `Run.eta_minutes` and `Order.eta_minutes`, from tracking's walk plan.
//!
//! A run gets a [`LivePlan`] only while it is active and its latest ping is at most
//! [`RECENT_PING`] old; otherwise both ETAs are null. One plan per run serves every viewer and
//! every order on it: the runner's ETA is to the next waypoint (the restaurant before pickup, else
//! the first drop-off), a customer's is to their own first drop-off, and an order's is to that
//! order's drop-off, each counting the stops before it (`tracking::Plan`). Planning may call OSRM
//! (cached 10 minutes per leg), so lists never plan and show null.

use std::time::Duration;

use sqlx::PgPool;

use super::machine::RunStatus;
use super::view::RunRecord;
use crate::catalog::types::{LatLng, Place};
use crate::error::ApiResult;
use crate::state::AppState;
use crate::tracking::{ActiveOrder, Plan, PlannedStop};

/// A ping older than this gives no ETA: the runner's app has stopped reporting.
pub const RECENT_PING: Duration = Duration::from_secs(5 * 60);

/// The runner's walk from their latest ping.
#[derive(Debug, Clone)]
pub struct LivePlan {
    runner_id: String,
    plan: Plan,
}

impl LivePlan {
    /// The plan for `run`, or `None` when it is not active or has no recent ping.
    pub async fn for_run(state: &AppState, run: &RunRecord) -> ApiResult<Option<Self>> {
        let Some(ping) = run.ping.as_ref() else {
            return Ok(None);
        };
        if !run.status.is_active() || ping.age > RECENT_PING {
            return Ok(None);
        }
        let orders = active_orders(&state.pool, &run.id).await?;
        let restaurant_first =
            (run.status != RunStatus::Delivering).then_some(run.restaurant.location);
        let plan = Plan::build(
            &state.routing,
            ping.location,
            restaurant_first,
            &orders,
            run.runner_pace,
        )
        .await;
        Ok(Some(Self {
            runner_id: run.runner_id.clone(),
            plan,
        }))
    }

    /// `Run.eta_minutes` for `viewer`: the runner's next waypoint, or the viewer's own first stop.
    /// `None` for anyone else.
    pub fn run_eta(&self, viewer: &str) -> Option<f64> {
        self.plan.eta_for(viewer, &self.runner_id)
    }

    /// The stop for one active order.
    pub fn stop(&self, order_id: &str) -> Option<&PlannedStop> {
        self.plan.stops.iter().find(|s| s.order.id == order_id)
    }

    /// `Order.eta_minutes`: minutes to that order's drop-off.
    pub fn order_eta(&self, order_id: &str) -> Option<f64> {
        self.stop(order_id).map(|s| s.eta_minutes)
    }
}

#[derive(sqlx::FromRow)]
struct ActiveRow {
    id: String,
    customer_id: String,
    dropoff_place_id: Option<String>,
    dropoff_label: String,
    dropoff_lat: f64,
    dropoff_lng: f64,
    dropoff_note: Option<String>,
}

/// The run's active orders (funded, purchased or picked_up), oldest first, as tracking orders
/// them.
async fn active_orders(pool: &PgPool, run_id: &str) -> ApiResult<Vec<ActiveOrder>> {
    let rows: Vec<ActiveRow> = sqlx::query_as(
        "SELECT o.id, o.customer_id, o.dropoff_place_id, o.dropoff_label, o.dropoff_lat, \
                o.dropoff_lng, o.dropoff_note \
         FROM ferry.orders o \
         WHERE o.run_id = $1 AND o.status IN ('funded', 'purchased', 'picked_up') \
         ORDER BY o.created_at, o.id",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ActiveOrder {
            id: r.id,
            customer_id: r.customer_id,
            dropoff: Place {
                id: r.dropoff_place_id,
                label: r.dropoff_label,
                location: LatLng {
                    lat: r.dropoff_lat,
                    lng: r.dropoff_lng,
                },
                note: r.dropoff_note,
            },
        })
        .collect())
}
