//! What a committed change tells the phones (contract/events.md), sent after the commit and
//! before the HTTP response, so a client that refetches on an event sees the change and events of
//! one request never overtake the previous request's.
//!
//! - `notification.created` to each stored notification's user.
//! - `run.updated` to the members of the run's circle (and its runner), each copy as
//!   `GET /v1/runs/{id}` shows it to that recipient: `runner_location` and `eta_minutes` only for
//!   the runner and customers with an active order. A handoff also sends it to that order's
//!   customer alone, whose copy then has no runner location.
//! - `order.updated` to each changed order's customer (with `handoff_code`) and runner (without).
//! - `wallet.updated` to every user whose checking or Ferry Card a ledger row moved.
//!
//! Each copy is built only for users who are connected. A failure here is logged and never undoes
//! or fails the change, which is already committed.

use std::collections::BTreeSet;

use super::live::LivePlan;
use super::view::{self, RunRecord};
use crate::error::ApiResult;
use crate::notifications::{Event, Notification};
use crate::orders::view as order_view;
use crate::payments::Payments;
use crate::payments::ledger::{LedgerEntry, accounts};
use crate::state::AppState;

/// The events one committed change owes.
#[derive(Debug, Default)]
pub struct Outbox {
    pub run_id: String,
    /// Send `run.updated` (the status or the order count changed).
    pub run: bool,
    /// Also send `run.updated` to these users, even when `run` is false: a customer whose order was
    /// just handed off, so their cached Run stops showing the runner (runs-orders review MINOR 3).
    pub run_to: Vec<String>,
    /// Send `order.updated` for these orders.
    pub orders: Vec<String>,
    /// The ledger rows the change wrote: their users get `wallet.updated`.
    pub ledger: Vec<LedgerEntry>,
    /// Stored notifications: `(user_id, notification)`.
    pub notifications: Vec<(String, Notification)>,
}

impl Outbox {
    pub fn new(run_id: &str) -> Self {
        Self {
            run_id: run_id.to_owned(),
            ..Self::default()
        }
    }
}

/// Sends everything in `outbox`. Never fails: a problem is logged.
pub async fn publish(state: &AppState, outbox: &Outbox) {
    if let Err(err) = try_publish(state, outbox).await {
        tracing::warn!(run_id = %outbox.run_id, error = %err, "run and order events not all sent");
    }
}

async fn try_publish(state: &AppState, outbox: &Outbox) -> ApiResult<()> {
    for (user_id, notification) in &outbox.notifications {
        state
            .hub
            .send_to(user_id, &Event::notification_created(notification));
    }

    if outbox.run || !outbox.run_to.is_empty() || !outbox.orders.is_empty() {
        let Some(run) = view::load_one(&state.pool, &state.config, &outbox.run_id).await? else {
            return Ok(());
        };
        // One plan serves every copy, and only when someone who may see an ETA is connected.
        let mut watchers = run.active_customers.clone();
        watchers.push(run.runner_id.clone());
        let live = if watchers.iter().any(|u| state.hub.connection_count(u) > 0) {
            LivePlan::for_run(state, &run).await?
        } else {
            None
        };
        if outbox.run || !outbox.run_to.is_empty() {
            publish_run(state, &run, outbox.run, &outbox.run_to, live.as_ref()).await?;
        }
        if !outbox.orders.is_empty() {
            publish_orders(state, &outbox.orders, live.as_ref()).await?;
        }
    }

    publish_wallets(state, &outbox.ledger).await
}

/// `run.updated` to those of `also` who are connected, and with `everyone` to the circle's
/// members and the runner who are.
async fn publish_run(
    state: &AppState,
    run: &RunRecord,
    everyone: bool,
    also: &[String],
    live: Option<&LivePlan>,
) -> ApiResult<()> {
    let mut recipients: Vec<String> = also.to_vec();
    if everyone {
        recipients.extend(
            sqlx::query_scalar::<_, String>(
                "SELECT user_id FROM ferry.circle_members WHERE circle_id = $1",
            )
            .bind(&run.circle_id)
            .fetch_all(&state.pool)
            .await?,
        );
        recipients.push(run.runner_id.clone());
    }
    recipients.sort();
    recipients.dedup();
    recipients.retain(|u| state.hub.connection_count(u) > 0);
    if recipients.is_empty() {
        return Ok(());
    }
    let walkers = view::walkers(&state.pool, &recipients).await?;
    state.hub.send_to_many(&recipients, |viewer| {
        let eta = live.and_then(|l| l.run_eta(viewer));
        Event::run_updated(&run.as_seen_by(viewer, walkers.get(viewer).copied(), eta))
    });
    Ok(())
}

/// `order.updated` to each order's customer and runner who are connected.
async fn publish_orders(
    state: &AppState,
    order_ids: &[String],
    live: Option<&LivePlan>,
) -> ApiResult<()> {
    let mut orders = order_view::load_by_ids(&state.pool, order_ids).await?;
    // Copies go only to connected users, so only their orders need a signed receipt link.
    orders.retain(|o| {
        state.hub.connection_count(&o.customer_id) > 0
            || state.hub.connection_count(&o.runner_id) > 0
    });
    order_view::sign_receipts(state, &mut orders).await;
    for order in &orders {
        let eta = live.and_then(|l| l.order_eta(&order.id));
        state
            .hub
            .send_to_many([&order.customer_id, &order.runner_id], |viewer| {
                Event::order_updated(&order.as_seen_by(viewer, eta))
            });
    }
    Ok(())
}

/// The users whose checking or Ferry Card one of `rows` moved, sorted.
pub fn wallet_users(rows: &[LedgerEntry]) -> Vec<String> {
    let users: BTreeSet<String> = rows
        .iter()
        .flat_map(|row| [&row.debit_account, &row.credit_account])
        .filter_map(|account| match accounts::parse(account) {
            Some(accounts::Account::Checking(user) | accounts::Account::FerryCard(user)) => {
                Some(user)
            }
            _ => None,
        })
        .collect();
    users.into_iter().collect()
}

/// `wallet.updated` to each connected user whose balance `rows` moved: their whole Wallet.
async fn publish_wallets(state: &AppState, rows: &[LedgerEntry]) -> ApiResult<()> {
    let users: Vec<String> = wallet_users(rows)
        .into_iter()
        .filter(|u| state.hub.connection_count(u) > 0)
        .collect();
    if users.is_empty() {
        return Ok(());
    }
    let mode = Payments::from_state(state).wallet_mode();
    let mut conn = state.pool.acquire().await?;
    for user in users {
        let wallet = crate::payments::wallet::load(&mut conn, &user, mode).await?;
        state.hub.send_to(&user, &Event::wallet_updated(&wallet));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(debit: &str, credit: &str) -> LedgerEntry {
        LedgerEntry {
            id: "txn_x".into(),
            kind: "refund".into(),
            debit_account: debit.into(),
            credit_account: credit.into(),
            amount_cents: 1,
            run_id: None,
            order_id: None,
            idempotency_key: "k".into(),
            nessie_object_id: None,
            nessie_status: None,
            created_at: "2026-10-10T22:00:00.000Z".into(),
        }
    }

    #[test]
    fn wallet_users_are_checking_and_card_owners() {
        let rows = [
            row("platform:ferry:escrow", "user:usr_judge:checking"),
            row("user:usr_maya:ferry_card", "platform:ferry:escrow"),
            row("restaurant:rst_cosmic:merchant", "user:usr_maya:ferry_card"),
            row("user:usr_dana:checking", "platform:ferry:escrow"),
            row("platform:ferry:escrow", "external"),
        ];
        assert_eq!(
            wallet_users(&rows),
            vec!["usr_dana", "usr_judge", "usr_maya"]
        );
        assert!(wallet_users(&[]).is_empty());
    }
}
