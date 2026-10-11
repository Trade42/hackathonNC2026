//! Who may do what with a run, read in one query, and the run's row lock.
//!
//! - The run's runner, a member of its circle, or a customer with an order on it may see it.
//!   Anyone else gets 404 `not_found`, the same as for a run that does not exist.
//! - Only the runner closes, marks purchased or picked up, and cancels: anyone else who may see
//!   the run gets 403 `forbidden`.
//!
//! Every change takes the run's row lock first ([`lock`], `SELECT … FOR UPDATE`), then its orders,
//! then the payments ops (which lock the same run again, then orders by id, then accounts), so two
//! changes on one run never deadlock and an order can never land after its window closed.

use sqlx::PgExecutor;

use super::machine::RunStatus;
use crate::error::{ApiError, ApiResult};

macro_rules! access_select {
    () => {
        "SELECT r.id, r.runner_id, r.circle_id, r.restaurant_id, r.status, r.is_demo_history, \
                r.window_closes_at > now() AS window_open, \
                r.runner_id = $2 AS is_runner, \
                EXISTS (SELECT 1 FROM ferry.circle_members m \
                        WHERE m.circle_id = r.circle_id AND m.user_id = $2) AS is_member, \
                EXISTS (SELECT 1 FROM ferry.orders o \
                        WHERE o.run_id = r.id AND o.customer_id = $2) AS has_order \
         FROM ferry.runs r WHERE r.id = $1"
    };
}

const READ: &str = access_select!();
const LOCK: &str = concat!(access_select!(), " FOR UPDATE OF r");

/// A run and one viewer's relation to it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RunAccess {
    pub id: String,
    pub runner_id: String,
    pub circle_id: String,
    pub restaurant_id: String,
    status: String,
    pub is_demo_history: bool,
    /// `window_closes_at` is still ahead.
    pub window_open: bool,
    pub is_runner: bool,
    pub is_member: bool,
    pub has_order: bool,
}

impl RunAccess {
    pub fn status(&self) -> ApiResult<RunStatus> {
        RunStatus::from_db(&self.status)
    }

    /// The runner, a member of the circle, or a customer on the run.
    pub fn can_see(&self) -> bool {
        self.is_runner || self.is_member || self.has_order
    }

    /// 409 `window_closed` unless the run takes orders: open, with its window still ahead.
    pub fn ensure_taking_orders(&self) -> ApiResult<()> {
        if self.status()? != RunStatus::Open {
            return Err(ApiError::window_closed(format!(
                "This run is no longer taking orders: it is {}.",
                self.status
            )));
        }
        if !self.window_open {
            return Err(ApiError::window_closed(
                "This run's order window has closed.",
            ));
        }
        Ok(())
    }

    /// 403 `forbidden` unless the viewer is the runner.
    pub fn ensure_runner(&self) -> ApiResult<()> {
        if self.is_runner {
            Ok(())
        } else {
            Err(ApiError::forbidden("Only the runner can do that."))
        }
    }

    /// 409 `conflict` for the demo's seeded past runs, which are read-only.
    pub fn ensure_real(&self) -> ApiResult<()> {
        if self.is_demo_history {
            Err(ApiError::conflict("Demo history is read-only."))
        } else {
            Ok(())
        }
    }
}

fn not_found() -> ApiError {
    ApiError::not_found("No such run.")
}

/// The run as `viewer` may reach it, without a lock: 404 when it does not exist or the viewer
/// may not see it.
pub async fn read<'e>(
    executor: impl PgExecutor<'e>,
    run_id: &str,
    viewer: &str,
) -> ApiResult<RunAccess> {
    let access: Option<RunAccess> = sqlx::query_as(READ)
        .bind(run_id)
        .bind(viewer)
        .fetch_optional(executor)
        .await?;
    access.filter(RunAccess::can_see).ok_or_else(not_found)
}

/// [`read`] under the run's row lock, held until the transaction ends.
pub async fn lock<'e>(
    executor: impl PgExecutor<'e>,
    run_id: &str,
    viewer: &str,
) -> ApiResult<RunAccess> {
    let access: Option<RunAccess> = sqlx::query_as(LOCK)
        .bind(run_id)
        .bind(viewer)
        .fetch_optional(executor)
        .await?;
    access.filter(RunAccess::can_see).ok_or_else(not_found)
}
