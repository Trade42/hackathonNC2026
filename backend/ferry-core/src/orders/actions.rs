//! What each order endpoint does, as functions of a user, so the agent (M2) can call them from
//! Telegram under exactly the endpoints' rules. The HTTP handlers in `orders/mod.rs` only unwrap
//! the request.
//!
//! Who may do what:
//! - Ordering and quoting: members of the run's circle other than its runner (403 for the runner
//!   and for a past customer who left the circle; 404 for anyone who may not see the run).
//! - Reading an order: its customer and its run's runner; 404 for anyone else.
//! - Handoff: the customer with no code, or the runner with the customer's 4-digit code; after
//!   [`MAX_WRONG_CODES`] wrong codes on an order, the runner's codes on it are refused.
//! - Cancelling: the customer only (403 for the runner), before the purchase.
//!
//! Each change locks the run first, then the order, as `runs::actions` does.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rand::RngExt;
use serde::Deserialize;
use serde_json::json;

use super::machine::OrderStatus;
use super::pricing::{self, CreateOrderRequest, PricedOrder};
use super::steps;
use super::view::{self, Order, OrderRecord};
use crate::error::{ApiError, ApiResult};
use crate::payments::{OrderQuote, Payments, ops};
use crate::profile::validate::FieldError;
use crate::runs::access::{self, RunAccess};
use crate::runs::live::LivePlan;
use crate::runs::publish::{Outbox, publish};
use crate::runs::{RunStatus, notices, view as run_view};
use crate::state::AppState;
use crate::{ids, impact};

/// `HandoffRequest` in contract/openapi.yaml.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct HandoffRequest {
    #[serde(default)]
    pub code: Option<String>,
}

/// An order with what the agent says about it: "Maya is 4 min away (0.3 mi), seen 20 s ago".
#[derive(Debug, Clone, PartialEq)]
pub struct OrderStatusReport {
    /// The order as the user sees it, `eta_minutes` included.
    pub order: Order,
    /// Walking meters from the runner's latest ping to this order's drop-off, through the stops
    /// before it. Only with a recent ping (`runs::live::RECENT_PING`) while the order is active.
    pub distance_m: Option<f64>,
    /// How old the runner's latest location is, when the user may see it.
    pub location_age: Option<Duration>,
    /// The same age as the agent says it: "just now", "20 s ago", "3 min ago".
    pub seen_ago: Option<String>,
}

fn field_error(field: &str, message: impl Into<String>) -> ApiError {
    pricing::invalid(&[FieldError {
        field: field.to_owned(),
        message: message.into(),
    }])
}

fn not_found() -> ApiError {
    ApiError::not_found("No such order.")
}

/// 403 unless `access`'s viewer may order on the run: a member of its circle, not its runner.
fn ensure_can_order(access: &RunAccess) -> ApiResult<()> {
    if access.is_runner {
        return Err(ApiError::forbidden(
            "You are this run's runner: buy your own food at the counter.",
        ));
    }
    if !access.is_member {
        return Err(ApiError::forbidden(
            "Only members of the run's circle can order on it.",
        ));
    }
    Ok(())
}

/// A random 4-digit handoff code, leading zeros kept.
pub fn new_handoff_code() -> String {
    format!("{:04}", rand::rng().random_range(0..10_000u32))
}

/// Wrong handoff codes the runner may enter on one order. After that, their code attempts on it
/// are 429 `rate_limited`, the right code too, until the customer confirms (runs-orders review
/// MINOR 2: 10,000 codes could otherwise be tried until one settles the order).
pub const MAX_WRONG_CODES: u32 = 5;

/// Wrong codes entered per order id, in this process: a restart forgets them (docs/DECISIONS.md).
/// Only a picked_up order gets an entry ([`confirm_handoff`] checks the status first), and it goes
/// when the order is handed off.
static WRONG_CODES: LazyLock<Mutex<HashMap<String, u32>>> = LazyLock::new(Default::default);

fn wrong_codes() -> MutexGuard<'static, HashMap<String, u32>> {
    WRONG_CODES.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Checks the code the runner entered for `record`. 429 `rate_limited` once
/// [`MAX_WRONG_CODES`] wrong codes are in, whatever this one is; otherwise 400
/// `validation_failed` when it is missing, not 4 digits, or wrong. Only a wrong 4-digit code counts,
/// the only kind that can be a guess. Check and count happen under one lock, so concurrent
/// attempts cannot get past the limit.
fn check_runner_code(record: &OrderRecord, code: Option<&str>) -> ApiResult<()> {
    let mut wrong = wrong_codes();
    let entered = wrong.get(&record.id).copied().unwrap_or(0);
    if entered >= MAX_WRONG_CODES {
        return Err(ApiError::rate_limited(
            "Too many wrong codes for this order. Ask the customer to tap Got it in the app.",
        )
        .with_details(json!({"max_wrong_codes": MAX_WRONG_CODES})));
    }
    let Some(code) = code.map(str::trim).filter(|c| !c.is_empty()) else {
        return Err(field_error(
            "code",
            "is required: enter the 4-digit code on the customer's screen",
        ));
    };
    if code.len() != 4 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(field_error("code", "must be 4 digits"));
    }
    if code != record.handoff_code {
        wrong.insert(record.id.clone(), entered + 1);
        let left = MAX_WRONG_CODES - entered - 1;
        return Err(field_error(
            "code",
            format!(
                "does not match the customer's code ({left} {} left)",
                if left == 1 { "try" } else { "tries" }
            ),
        ));
    }
    Ok(())
}

/// Prices `request` on `run_id` as placing it would, with no side effects: the lines, the totals
/// and the drop-off. 404, 403, 409 `window_closed` and 400 as for [`place_order`]; never 402.
/// The agent's draft order uses it; `POST /v1/runs/{id}/quote` answers its `quote`.
pub async fn price_order(
    state: &AppState,
    user_id: &str,
    run_id: &str,
    request: &CreateOrderRequest,
) -> ApiResult<PricedOrder> {
    let run = access::read(&state.pool, run_id, user_id).await?;
    ensure_can_order(&run)?;
    run.ensure_taking_orders()?;
    pricing::price(&state.pool, &state.config, &run.restaurant_id, request).await
}

/// `POST /v1/runs/{id}/quote`.
pub async fn quote_order(
    state: &AppState,
    user_id: &str,
    run_id: &str,
    request: &CreateOrderRequest,
) -> ApiResult<OrderQuote> {
    Ok(price_order(state, user_id, run_id, request).await?.quote)
}

/// The order as `viewer` sees it now, with an ETA from a recent ping while it is active, and the
/// plan's distance and the location's age for [`order_status`]. 404 unless the viewer is its
/// customer or runner.
async fn order_report(
    state: &AppState,
    order_id: &str,
    viewer: &str,
) -> ApiResult<OrderStatusReport> {
    let mut record = view::load_one(&state.pool, order_id)
        .await?
        .filter(|o| o.is_party(viewer))
        .ok_or_else(not_found)?;
    view::sign_receipts(state, std::slice::from_mut(&mut record)).await;
    let run = run_view::load_one(&state.pool, &state.config, &record.run_id).await?;
    let mut report = OrderStatusReport {
        order: record.as_seen_by(viewer, None),
        distance_m: None,
        location_age: None,
        seen_ago: None,
    };
    let Some(run) = run.filter(|r| r.may_see_location(viewer)) else {
        return Ok(report);
    };
    if let Some(ping) = &run.ping {
        report.location_age = Some(ping.age);
        report.seen_ago = Some(ping.seen_ago());
    }
    if record.status.is_active()
        && let Some(live) = LivePlan::for_run(state, &run).await?
        && let Some(stop) = live.stop(&record.id)
    {
        report.order = record.as_seen_by(viewer, Some(stop.eta_minutes));
        report.distance_m = Some(stop.distance_m.round());
    }
    Ok(report)
}

/// `GET /v1/orders/{id}`: only its customer and its run's runner; `handoff_code` only for the
/// customer.
pub async fn get_order(state: &AppState, user_id: &str, order_id: &str) -> ApiResult<Order> {
    Ok(order_report(state, order_id, user_id).await?.order)
}

/// The order with its ETA, distance and the runner location's age, for the agent's "how long
/// until my food gets here?". The same 404 as [`get_order`].
pub async fn order_status(
    state: &AppState,
    user_id: &str,
    order_id: &str,
) -> ApiResult<OrderStatusReport> {
    order_report(state, order_id, user_id).await
}

/// `GET /v1/orders`: the user's orders as customer, newest first, without demo history. No ETAs
/// in lists.
pub async fn list_orders(state: &AppState, user_id: &str) -> ApiResult<Vec<Order>> {
    let mut orders = view::load_for_customer(&state.pool, user_id).await?;
    view::sign_receipts(state, &mut orders).await;
    Ok(orders.iter().map(|o| o.as_seen_by(user_id, None)).collect())
}

/// `POST /v1/runs/{id}/orders`: prices the body from the menu, then in one transaction locks the
/// run, checks its window again, stores the order (funded), its lines and its first timeline
/// entry, and holds the total from the customer's checking in Escrow
/// (`payments::ops::hold_order_in`). Not enough balance is 402 `insufficient_funds` and stores
/// nothing. A run that is not open, or whose window has passed, is 409 `window_closed`.
pub async fn place_order(
    state: &AppState,
    user_id: &str,
    run_id: &str,
    request: &CreateOrderRequest,
) -> ApiResult<Order> {
    let priced = price_order(state, user_id, run_id, request).await?;
    let payments = Payments::from_state(state);
    let order_id = ids::new_id(ids::ORDER);
    let quote = priced.quote;
    let dropoff = &priced.dropoff;

    let mut tx = state.pool.begin().await?;
    // The close takes this lock too, so an order lands before the close (and is funded) or after
    // it (and is refused here).
    let run = access::lock(&mut *tx, run_id, user_id).await?;
    ensure_can_order(&run)?;
    run.ensure_taking_orders()?;
    sqlx::query(
        "INSERT INTO ferry.orders (id, run_id, customer_id, status, subtotal_cents, tax_cents, \
             ferry_fee_cents, tip_cents, total_cents, dropoff_place_id, dropoff_label, dropoff_lat, \
             dropoff_lng, dropoff_note, handoff_code, dropoff_radius_m) \
         VALUES ($1, $2, $3, 'funded', $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
    )
    .bind(&order_id)
    .bind(run_id)
    .bind(user_id)
    .bind(quote.subtotal_cents)
    .bind(quote.tax_cents)
    .bind(quote.ferry_fee_cents)
    .bind(quote.tip_cents)
    .bind(quote.total_cents)
    .bind(dropoff.id.as_deref())
    .bind(&dropoff.label)
    .bind(dropoff.location.lat)
    .bind(dropoff.location.lng)
    .bind(dropoff.note.as_deref())
    .bind(new_handoff_code())
    .bind(priced.dropoff_radius_m)
    .execute(&mut *tx)
    .await?;
    for (line_no, line) in priced.lines.iter().enumerate() {
        sqlx::query(
            "INSERT INTO ferry.order_lines (order_id, line_no, menu_item_id, name, qty, choice_ids, \
                 unit_price_cents, line_total_cents, note) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(&order_id)
        .bind(line_no as i32)
        .bind(&line.menu_item_id)
        .bind(&line.name)
        .bind(line.qty as i32)
        .bind(&line.choice_ids)
        .bind(line.unit_price_cents)
        .bind(line.line_total_cents)
        .bind(line.note.as_deref())
        .execute(&mut *tx)
        .await?;
    }
    steps::write_events(
        &mut tx,
        run_id,
        std::slice::from_ref(&order_id),
        OrderStatus::Funded,
        &json!({}),
    )
    .await?;
    // A 402 here returns before the commit, so nothing above is stored.
    let rows = ops::hold_order_in(&mut tx, &order_id).await?;
    let (runner, restaurant): (String, String) = sqlx::query_as(
        "SELECT u.display_name, rs.name FROM ferry.users u CROSS JOIN ferry.restaurants rs \
         WHERE u.id = $1 AND rs.id = $2",
    )
    .bind(&run.runner_id)
    .bind(&run.restaurant_id)
    .fetch_one(&mut *tx)
    .await?;
    let mut outbox = Outbox::new(run_id);
    outbox.notifications.push(
        notices::order_update(
            &mut tx,
            &state.config,
            user_id,
            &order_id,
            run_id,
            OrderStatus::Funded,
            "Order placed",
            &format!(
                "Your {restaurant} order is in. {} is held until {} hands it over.",
                notices::money(quote.total_cents),
                notices::name_or(&runner, "your runner")
            ),
        )
        .await?,
    );
    tx.commit().await?;
    tracing::info!(
        run_id,
        order_id,
        total_cents = quote.total_cents,
        "order placed"
    );

    outbox.run = true;
    outbox.orders = vec![order_id.clone()];
    outbox.ledger = rows;
    publish(state, &outbox).await;
    payments.post(&outbox.ledger).await;
    get_order(state, user_id, &order_id).await
}

/// The order for a change by `user_id`: 404 unless they are its customer or runner, 409 for demo
/// history.
async fn order_for_change(
    state: &AppState,
    user_id: &str,
    order_id: &str,
) -> ApiResult<OrderRecord> {
    let record = view::load_one(&state.pool, order_id)
        .await?
        .filter(|o| o.is_party(user_id))
        .ok_or_else(not_found)?;
    if record.is_demo_history {
        return Err(ApiError::conflict("Demo history is read-only."));
    }
    Ok(record)
}

/// Locks the order's run, then the order, and returns the run and the order's status.
async fn lock_order(
    conn: &mut sqlx::PgConnection,
    record: &OrderRecord,
    user_id: &str,
) -> ApiResult<(RunAccess, OrderStatus)> {
    let run = access::lock(&mut *conn, &record.run_id, user_id).await?;
    let status: String =
        sqlx::query_scalar("SELECT status FROM ferry.orders WHERE id = $1 FOR UPDATE")
            .bind(&record.id)
            .fetch_one(&mut *conn)
            .await?;
    Ok((run, OrderStatus::from_db(&status)?))
}

/// `POST /v1/orders/{id}/handoff`: the customer confirms ("Got it", no code), or the runner
/// submits the customer's 4-digit code (400 when missing or wrong; 429 after [`MAX_WRONG_CODES`]
/// wrong ones, until the customer confirms). picked_up → delivered, with
/// `car_distance_m` and `co2_saved_g`, then the fee and tip pay out to the runner
/// (`payments::ops::payout_in`) and the order → settled, all in one transaction. When no active
/// order is left on the run, the run → delivered. Any other status is 409 `conflict`, checked
/// before the code, so a code on such an order is neither checked nor counted. The customer gets
/// `run.updated` (without the runner's location) even when the run goes on.
pub async fn confirm_handoff(
    state: &AppState,
    user_id: &str,
    order_id: &str,
    code: Option<&str>,
) -> ApiResult<Order> {
    let record = order_for_change(state, user_id, order_id).await?;
    // Refuse early, before the routing call below can take seconds, and before the runner's code
    // is checked: only a picked_up order counts wrong codes, and its count ends with the handoff.
    record.status.transition(OrderStatus::Delivered)?;
    if user_id != record.customer_id {
        check_runner_code(&record, code)?;
    }
    let (car_distance_m, co2_saved_g) =
        impact::carbon_for_delivery(state, record.restaurant_location, record.dropoff.location)
            .await;

    let payments = Payments::from_state(state);
    let mut tx = state.pool.begin().await?;
    let (run, status) = lock_order(&mut tx, &record, user_id).await?;
    status.transition(OrderStatus::Delivered)?;
    sqlx::query(
        "UPDATE ferry.orders SET status = 'delivered', delivered_at = clock_timestamp(), \
             car_distance_m = $2, co2_saved_g = $3 \
         WHERE id = $1",
    )
    .bind(order_id)
    .bind(car_distance_m)
    .bind(co2_saved_g)
    .execute(&mut *tx)
    .await?;
    steps::write_events(
        &mut tx,
        &record.run_id,
        &[order_id.to_owned()],
        OrderStatus::Delivered,
        &json!({"car_distance_m": car_distance_m, "co2_saved_g": co2_saved_g, "method": impact::METHOD}),
    )
    .await?;
    let rows = ops::payout_in(&mut tx, order_id).await?;
    let paid: i64 = rows.iter().map(|r| r.amount_cents).sum();
    steps::move_order(
        &mut tx,
        &record.run_id,
        order_id,
        OrderStatus::Delivered,
        OrderStatus::Settled,
        &json!({"payout_cents": paid}),
    )
    .await?;

    let mut outbox = Outbox::new(&record.run_id);
    let left: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ferry.orders \
         WHERE run_id = $1 AND status IN ('funded', 'purchased', 'picked_up')",
    )
    .bind(&record.run_id)
    .fetch_one(&mut *tx)
    .await?;
    let run_status = run.status()?;
    if left == 0 && run_status.can_become(RunStatus::Delivered) {
        sqlx::query("UPDATE ferry.runs SET status = 'delivered' WHERE id = $1")
            .bind(&record.run_id)
            .execute(&mut *tx)
            .await?;
        outbox.run = true;
    }
    outbox.notifications.push(
        notices::order_update(
            &mut tx,
            &state.config,
            &record.customer_id,
            order_id,
            &record.run_id,
            OrderStatus::Settled,
            "Delivered",
            &notices::co2_line(co2_saved_g),
        )
        .await?,
    );
    if paid > 0 {
        outbox.notifications.push(
            notices::payout(
                &mut tx,
                &state.config,
                &record.runner_id,
                order_id,
                &record.run_id,
                paid,
                &record.customer.display_name,
            )
            .await?,
        );
    }
    tx.commit().await?;
    wrong_codes().remove(order_id);
    tracing::info!(
        run_id = %record.run_id,
        order_id,
        payout_cents = paid,
        co2_saved_g,
        run_delivered = outbox.run,
        "order handed off"
    );

    // The customer's order is no longer active, so their copy has no runner location.
    outbox.run_to = vec![record.customer_id.clone()];
    outbox.orders = vec![order_id.to_owned()];
    outbox.ledger = rows;
    publish(state, &outbox).await;
    payments.post(&outbox.ledger).await;
    get_order(state, user_id, order_id).await
}

/// `POST /v1/orders/{id}/cancel`: the customer only (403 for the runner), before the purchase:
/// funded → cancelled, and the full hold comes back from Escrow
/// (`payments::ops::refund_cancelled_in`, which first returns the order's share from the card
/// when the window already closed). Any other status is 409 `conflict`.
pub async fn cancel_order(state: &AppState, user_id: &str, order_id: &str) -> ApiResult<Order> {
    let record = order_for_change(state, user_id, order_id).await?;
    if user_id != record.customer_id {
        return Err(ApiError::forbidden(
            "Only the customer can cancel their order.",
        ));
    }
    record.status.transition(OrderStatus::Cancelled)?;

    let payments = Payments::from_state(state);
    let mut tx = state.pool.begin().await?;
    let (_, status) = lock_order(&mut tx, &record, user_id).await?;
    status.transition(OrderStatus::Cancelled)?;
    let rows = ops::refund_cancelled_in(&mut tx, order_id).await?;
    let refunded: i64 = rows
        .iter()
        .filter(|r| r.idempotency_key.starts_with("refund:"))
        .map(|r| r.amount_cents)
        .sum();
    steps::move_order(
        &mut tx,
        &record.run_id,
        order_id,
        OrderStatus::Funded,
        OrderStatus::Cancelled,
        &json!({}),
    )
    .await?;
    let mut outbox = Outbox::new(&record.run_id);
    outbox.notifications.push(
        notices::order_update(
            &mut tx,
            &state.config,
            user_id,
            order_id,
            &record.run_id,
            OrderStatus::Cancelled,
            "Order cancelled",
            &format!("{} is back in your Ferry wallet.", notices::money(refunded)),
        )
        .await?,
    );
    tx.commit().await?;
    tracing::info!(run_id = %record.run_id, order_id, refunded, "order cancelled");

    outbox.run = true;
    outbox.orders = vec![order_id.to_owned()];
    outbox.ledger = rows;
    publish(state, &outbox).await;
    payments.post(&outbox.ledger).await;
    get_order(state, user_id, order_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_codes_are_four_digits() {
        for _ in 0..1000 {
            let code = new_handoff_code();
            assert_eq!(code.len(), 4);
            assert!(code.bytes().all(|b| b.is_ascii_digit()));
        }
    }
}
