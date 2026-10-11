//! The money operations of spec section 4. Each one writes ledger rows only: it never calls
//! Nessie, so it can run inside the caller's transaction, beside the state change it pays for.
//!
//! Every `*_in` function takes the caller's connection inside an open transaction and:
//! 1. locks the run (`SELECT … FOR UPDATE`), then the orders that decide the amount;
//! 2. returns the existing rows when its idempotency key is already written (before any balance
//!    check, so a retry never fails on a balance it already spent);
//! 3. locks the accounts money leaves (`ledger::lock_accounts`) and checks their balances;
//! 4. inserts the rows with `ON CONFLICT (idempotency_key) DO NOTHING`.
//!
//! The caller commits, then hands the returned rows to `Payments::post` (Nessie, at most
//! `poster::USER_WAIT`). Rows nobody posts are picked up by the retry task.
//!
//! Lock order, everywhere: the run, then its orders by id, then accounts by name. A caller that
//! locks the run itself must use `FOR UPDATE` (not `FOR SHARE`) and take it before inserting or
//! updating the run's orders, or two order placements can deadlock on the upgrade.

use serde_json::json;
use sqlx::PgConnection;

use super::ledger::{self, Kind, LedgerEntry, NewEntry, accounts, keys};
use super::math;
use crate::error::{ApiError, ApiResult};

/// The run row a money op locks.
#[derive(Debug, Clone, sqlx::FromRow)]
struct RunRow {
    id: String,
    runner_id: String,
    restaurant_id: String,
    status: String,
}

/// The order row a money op locks.
#[derive(Debug, Clone, sqlx::FromRow)]
struct OrderRow {
    id: String,
    customer_id: String,
    status: String,
    subtotal_cents: i64,
    tax_cents: i64,
    ferry_fee_cents: i64,
    tip_cents: i64,
    total_cents: i64,
}

impl OrderRow {
    /// What the order puts on the runner's card: its food and tax.
    fn card_share(&self) -> i64 {
        self.subtotal_cents + self.tax_cents
    }
}

macro_rules! order_columns {
    () => {
        "o.id, o.customer_id, o.status, o.subtotal_cents, o.tax_cents, \
         o.ferry_fee_cents, o.tip_cents, o.total_cents"
    };
}

async fn lock_run(conn: &mut PgConnection, run_id: &str) -> ApiResult<RunRow> {
    sqlx::query_as(
        "SELECT id, runner_id, restaurant_id, status FROM ferry.runs WHERE id = $1 FOR UPDATE",
    )
    .bind(run_id)
    .fetch_optional(conn)
    .await?
    .ok_or_else(|| ApiError::not_found("Run not found."))
}

/// Locks the order's run, then the order.
async fn lock_order(conn: &mut PgConnection, order_id: &str) -> ApiResult<(RunRow, OrderRow)> {
    let run_id: Option<(String,)> = sqlx::query_as("SELECT run_id FROM ferry.orders WHERE id = $1")
        .bind(order_id)
        .fetch_optional(&mut *conn)
        .await?;
    let (run_id,) = run_id.ok_or_else(|| ApiError::not_found("Order not found."))?;
    let run = lock_run(conn, &run_id).await?;
    let order: OrderRow = sqlx::query_as(concat!(
        "SELECT ",
        order_columns!(),
        " FROM ferry.orders o WHERE o.id = $1 FOR UPDATE"
    ))
    .bind(order_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok((run, order))
}

/// The run's live orders, locked, by id: held and not refunded on cancel. The ledger decides,
/// not the status column, so a caller that updates the status later cannot skew the money.
async fn lock_live_orders(conn: &mut PgConnection, run_id: &str) -> ApiResult<Vec<OrderRow>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        order_columns!(),
        " FROM ferry.orders o \
         WHERE o.run_id = $1 \
           AND EXISTS (SELECT 1 FROM ferry.ledger_entries h WHERE h.idempotency_key = 'order_hold:' || o.id) \
           AND NOT EXISTS (SELECT 1 FROM ferry.ledger_entries r WHERE r.idempotency_key = 'refund:' || o.id || ':cancel') \
         ORDER BY o.id FOR UPDATE OF o"
    ))
    .bind(run_id)
    .fetch_all(conn)
    .await?)
}

/// Fails with 500 if `account` cannot pay `amount`: a platform invariant, not a user error.
async fn ensure_covers(conn: &mut PgConnection, account: &str, amount: i64) -> ApiResult<()> {
    let balance = ledger::balance(conn, account).await?;
    if balance < amount {
        return Err(ApiError::internal(format!(
            "ledger invariant: {account} holds {balance} cents, needs {amount}"
        )));
    }
    Ok(())
}

/// Op 1: Customer checking → Escrow for the order's total (subtotal + tax + ferry fee + tip), key
/// `order_hold:<ord_id>`. Call it in the transaction that inserts the order (status `funded`),
/// after locking the run. Not enough balance: 402 `insufficient_funds` with details
/// `{balance_cents, total_cents}`; the caller rolls back, so nothing is written. A run that is not
/// `open` (closed or later), or whose card is funded, takes no new hold: 409 `window_closed`. An
/// order already held returns its row in any run status.
pub async fn hold_order_in(conn: &mut PgConnection, order_id: &str) -> ApiResult<Vec<LedgerEntry>> {
    let (run, order) = lock_order(conn, order_id).await?;
    let key = keys::order_hold(&order.id);
    if let Some(existing) = ledger::by_key(conn, &key).await? {
        return Ok(vec![existing]);
    }
    if run.status != "open" {
        return Err(ApiError::window_closed(format!(
            "The order window is closed: the run is {}.",
            run.status
        )));
    }
    if ledger::by_key(conn, &keys::card_funding(&run.id))
        .await?
        .is_some()
    {
        return Err(ApiError::window_closed(
            "The order window is closed: the runner's card is already funded.",
        ));
    }
    if order.status != "funded" {
        return Err(ApiError::conflict(format!(
            "An order in status {} cannot be held.",
            order.status
        )));
    }
    if order.total_cents <= 0 {
        return Ok(Vec::new());
    }
    let checking = accounts::checking(&order.customer_id);
    ledger::lock_accounts(conn, &[&checking]).await?;
    let balance = ledger::balance(conn, &checking).await?;
    if balance < order.total_cents {
        return Err(ApiError::insufficient_funds(format!(
            "This order costs {} cents and your balance is {} cents.",
            order.total_cents, balance
        ))
        .with_details(json!({"balance_cents": balance, "total_cents": order.total_cents})));
    }
    let (entry, _) = ledger::insert(
        conn,
        &NewEntry {
            kind: Kind::OrderHold,
            debit_account: accounts::ESCROW,
            credit_account: &checking,
            amount_cents: order.total_cents,
            run_id: Some(&run.id),
            order_id: Some(&order.id),
            idempotency_key: &key,
        },
    )
    .await?;
    Ok(vec![entry])
}

/// Op 2: Escrow → the runner's Ferry Card for exactly the sum of subtotal + tax over the run's live
/// orders, key `card_funding:<run_id>`. Call it in the transaction that closes the window. A run
/// with no live orders funds nothing (no row). The run must be `open` or `closed`.
pub async fn fund_card_in(conn: &mut PgConnection, run_id: &str) -> ApiResult<Vec<LedgerEntry>> {
    let run = lock_run(conn, run_id).await?;
    let key = keys::card_funding(&run.id);
    if let Some(existing) = ledger::by_key(conn, &key).await? {
        return Ok(vec![existing]);
    }
    if run.status != "open" && run.status != "closed" {
        return Err(ApiError::conflict(format!(
            "A run in status {} cannot fund its card.",
            run.status
        )));
    }
    let orders = lock_live_orders(conn, &run.id).await?;
    let amount: i64 = orders.iter().map(OrderRow::card_share).sum();
    if amount <= 0 {
        return Ok(Vec::new());
    }
    ledger::lock_accounts(conn, &[accounts::ESCROW]).await?;
    ensure_covers(conn, accounts::ESCROW, amount).await?;
    let card = accounts::ferry_card(&run.runner_id);
    let (entry, _) = ledger::insert(
        conn,
        &NewEntry {
            kind: Kind::CardFunding,
            debit_account: &card,
            credit_account: accounts::ESCROW,
            amount_cents: amount,
            run_id: Some(&run.id),
            order_id: None,
            idempotency_key: &key,
        },
    )
    .await?;
    Ok(vec![entry])
}

/// The rows a run's purchase wrote, oldest first.
async fn purchase_rows(conn: &mut PgConnection, run_id: &str) -> ApiResult<Vec<LedgerEntry>> {
    let fixed = vec![
        keys::overage(run_id),
        keys::card_purchase(run_id),
        keys::card_return(run_id),
    ];
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        ledger::ledger_columns!(),
        " FROM ferry.ledger_entries \
         WHERE run_id = $1 AND (idempotency_key = ANY($2) OR idempotency_key LIKE 'refund:%:receipt') \
         ORDER BY ferry.ledger_entries.created_at, ferry.ledger_entries.id"
    ))
    .bind(run_id)
    .bind(&fixed)
        .fetch_all(conn)
        .await?)
}

/// Op 3: The runner pays `receipt_total_cents` with the Ferry Card at the restaurant's merchant, key
/// `card_purchase:<run_id>`. Call it in the transaction that marks the run purchased. Against the
/// funded amount F (subtotal + tax over the live orders):
/// - receipt lower: the card's unspent remainder goes back to Escrow (`card_return:<run_id>`,
///   kind refund) and the difference F - receipt is refunded to the customers pro rata to their
///   subtotal + tax (`refund:<ord_id>:receipt`), by largest remainder so the cents add up;
/// - receipt higher by at most 5% of F (rounded down): Escrow covers the difference first
///   (`overage:<run_id>`, kind card_funding);
/// - higher by more: 409 `conflict` with details `{max_receipt_cents}`, and nothing moves.
///
/// Calling it again with the same receipt returns the same rows; with another receipt, 409.
pub async fn card_purchase_in(
    conn: &mut PgConnection,
    run_id: &str,
    receipt_total_cents: i64,
) -> ApiResult<Vec<LedgerEntry>> {
    if receipt_total_cents < 1 {
        return Err(ApiError::validation(
            "receipt_total_cents must be at least 1.",
        ));
    }
    let run = lock_run(conn, run_id).await?;
    if let Some(existing) = ledger::by_key(conn, &keys::card_purchase(&run.id)).await? {
        if existing.amount_cents != receipt_total_cents {
            return Err(ApiError::conflict(format!(
                "This run's purchase is already recorded at {} cents.",
                existing.amount_cents
            )));
        }
        return purchase_rows(conn, &run.id).await;
    }
    let Some(funding) = ledger::by_key(conn, &keys::card_funding(&run.id)).await? else {
        return Err(ApiError::conflict(
            "The runner's card is not funded: close the order window first.",
        ));
    };
    let orders = lock_live_orders(conn, &run.id).await?;
    let funded: i64 = orders.iter().map(OrderRow::card_share).sum();
    let max = math::max_receipt_cents(funded);
    if receipt_total_cents > max {
        return Err(ApiError::conflict(format!(
            "The receipt is more than {}% over the {funded} cents funded; the most Ferry covers is {max} cents.",
            math::OVERAGE_LIMIT_PERCENT
        ))
        .with_details(json!({"max_receipt_cents": max})));
    }

    let card = accounts::ferry_card(&run.runner_id);
    ledger::lock_accounts(conn, &[accounts::ESCROW, &card]).await?;
    // What this run still has on the card: the funding minus cancelled orders' returns.
    let returned: (i64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(amount_cents), 0)::bigint FROM ferry.ledger_entries \
         WHERE run_id = $1 AND starts_with(idempotency_key, $2)",
    )
    .bind(&run.id)
    .bind(format!("card_return:{}:", run.id))
    .fetch_one(&mut *conn)
    .await?;
    let on_card = funding.amount_cents - returned.0;
    let overage = (receipt_total_cents - funded).max(0);
    let mut written = Vec::new();

    if overage > 0 {
        let balance = ledger::balance(conn, accounts::ESCROW).await?;
        if balance < overage {
            return Err(ApiError::conflict(
                "Escrow cannot cover this receipt's overage right now.",
            ));
        }
        written.push(
            ledger::insert(
                conn,
                &NewEntry {
                    kind: Kind::CardFunding,
                    debit_account: &card,
                    credit_account: accounts::ESCROW,
                    amount_cents: overage,
                    run_id: Some(&run.id),
                    order_id: None,
                    idempotency_key: &keys::overage(&run.id),
                },
            )
            .await?
            .0,
        );
    }
    if on_card + overage < receipt_total_cents {
        return Err(ApiError::internal(format!(
            "ledger invariant: run {} has {on_card} cents on the card for a {receipt_total_cents} cent receipt",
            run.id
        )));
    }
    let merchant = accounts::merchant(&run.restaurant_id);
    written.push(
        ledger::insert(
            conn,
            &NewEntry {
                kind: Kind::CardPurchase,
                debit_account: &merchant,
                credit_account: &card,
                amount_cents: receipt_total_cents,
                run_id: Some(&run.id),
                order_id: None,
                idempotency_key: &keys::card_purchase(&run.id),
            },
        )
        .await?
        .0,
    );
    let remainder = on_card + overage - receipt_total_cents;
    if remainder > 0 {
        written.push(
            ledger::insert(
                conn,
                &NewEntry {
                    kind: Kind::Refund,
                    debit_account: accounts::ESCROW,
                    credit_account: &card,
                    amount_cents: remainder,
                    run_id: Some(&run.id),
                    order_id: None,
                    idempotency_key: &keys::card_return(&run.id),
                },
            )
            .await?
            .0,
        );
    }
    let refund_total = funded - receipt_total_cents;
    if refund_total > 0 {
        ensure_covers(conn, accounts::ESCROW, refund_total).await?;
        let weights: Vec<(&str, i64)> = orders
            .iter()
            .map(|o| (o.id.as_str(), o.card_share()))
            .collect();
        let shares = math::split_pro_rata(refund_total, &weights).map_err(ApiError::internal)?;
        for (order, (_, share)) in orders.iter().zip(shares) {
            if share <= 0 {
                continue;
            }
            let checking = accounts::checking(&order.customer_id);
            written.push(
                ledger::insert(
                    conn,
                    &NewEntry {
                        kind: Kind::Refund,
                        debit_account: &checking,
                        credit_account: accounts::ESCROW,
                        amount_cents: share,
                        run_id: Some(&run.id),
                        order_id: Some(&order.id),
                        idempotency_key: &keys::refund_receipt(&order.id),
                    },
                )
                .await?
                .0,
            );
        }
    }
    Ok(written)
}

/// Op 4: At handoff, the order's ferry fee + tip from Escrow to the runner's checking, key
/// `payout:<ord_id>`. Call it in the transaction that settles the order. Needs the run's purchase
/// and a live (held, not cancelled) order, else 409. A zero fee and tip pays nothing (no row).
pub async fn payout_in(conn: &mut PgConnection, order_id: &str) -> ApiResult<Vec<LedgerEntry>> {
    let (run, order) = lock_order(conn, order_id).await?;
    let key = keys::payout(&order.id);
    if let Some(existing) = ledger::by_key(conn, &key).await? {
        return Ok(vec![existing]);
    }
    if ledger::by_key(conn, &keys::order_hold(&order.id))
        .await?
        .is_none()
    {
        return Err(ApiError::conflict("This order was never paid for."));
    }
    if ledger::by_key(conn, &keys::refund_cancel(&order.id))
        .await?
        .is_some()
    {
        return Err(ApiError::conflict("This order was cancelled and refunded."));
    }
    if ledger::by_key(conn, &keys::card_purchase(&run.id))
        .await?
        .is_none()
    {
        return Err(ApiError::conflict(
            "The runner has not paid the restaurant yet.",
        ));
    }
    let amount = order.ferry_fee_cents + order.tip_cents;
    if amount <= 0 {
        return Ok(Vec::new());
    }
    ledger::lock_accounts(conn, &[accounts::ESCROW]).await?;
    ensure_covers(conn, accounts::ESCROW, amount).await?;
    let checking = accounts::checking(&run.runner_id);
    let (entry, _) = ledger::insert(
        conn,
        &NewEntry {
            kind: Kind::Payout,
            debit_account: &checking,
            credit_account: accounts::ESCROW,
            amount_cents: amount,
            run_id: Some(&run.id),
            order_id: Some(&order.id),
            idempotency_key: &key,
        },
    )
    .await?;
    Ok(vec![entry])
}

/// Op 5: Cancel before purchase: the full hold from Escrow back to the customer, key
/// `refund:<ord_id>:cancel`, equal to the hold. If the card was already funded (cancel after
/// close), the order's subtotal + tax first comes back from the card to Escrow
/// (`card_return:<run_id>:<ord_id>`). After the purchase: 409, no automatic refund. Call it in the
/// transaction that cancels the order. An order with no hold refunds nothing (no row).
pub async fn refund_cancelled_in(
    conn: &mut PgConnection,
    order_id: &str,
) -> ApiResult<Vec<LedgerEntry>> {
    let (run, order) = lock_order(conn, order_id).await?;
    let refund_key = keys::refund_cancel(&order.id);
    let return_key = keys::card_return_cancel(&run.id, &order.id);
    if ledger::by_key(conn, &refund_key).await?.is_some() {
        return ledger::by_keys(conn, &[return_key, refund_key]).await;
    }
    if ledger::by_key(conn, &keys::card_purchase(&run.id))
        .await?
        .is_some()
    {
        return Err(ApiError::conflict(
            "The runner already paid for this order, so it is not refunded automatically.",
        ));
    }
    let Some(hold) = ledger::by_key(conn, &keys::order_hold(&order.id)).await? else {
        return Ok(Vec::new());
    };
    let funded = ledger::by_key(conn, &keys::card_funding(&run.id))
        .await?
        .is_some();
    let card = accounts::ferry_card(&run.runner_id);
    ledger::lock_accounts(conn, &[accounts::ESCROW, &card]).await?;
    let mut written = Vec::new();
    if funded && order.card_share() > 0 {
        ensure_covers(conn, &card, order.card_share()).await?;
        written.push(
            ledger::insert(
                conn,
                &NewEntry {
                    kind: Kind::Refund,
                    debit_account: accounts::ESCROW,
                    credit_account: &card,
                    amount_cents: order.card_share(),
                    run_id: Some(&run.id),
                    order_id: Some(&order.id),
                    idempotency_key: &return_key,
                },
            )
            .await?
            .0,
        );
    }
    ensure_covers(conn, accounts::ESCROW, hold.amount_cents).await?;
    let checking = accounts::checking(&order.customer_id);
    written.push(
        ledger::insert(
            conn,
            &NewEntry {
                kind: Kind::Refund,
                debit_account: &checking,
                credit_account: accounts::ESCROW,
                amount_cents: hold.amount_cents,
                run_id: Some(&run.id),
                order_id: Some(&order.id),
                idempotency_key: &refund_key,
            },
        )
        .await?
        .0,
    );
    Ok(written)
}

/// Op 6: A deposit from outside into the user's checking under `idempotency_key`: the seeder's
/// demo balance (`keys::seed_deposit`). A demo top-up goes through [`demo_topup_in`].
pub async fn demo_deposit_in(
    conn: &mut PgConnection,
    user_id: &str,
    amount_cents: i64,
    idempotency_key: &str,
) -> ApiResult<Vec<LedgerEntry>> {
    if amount_cents < 1 {
        return Err(ApiError::validation("amount_cents must be at least 1."));
    }
    if !user_id.starts_with("usr_") {
        return Err(ApiError::validation("Not a user id."));
    }
    let checking = accounts::checking(user_id);
    let (entry, _) = ledger::insert(
        conn,
        &NewEntry {
            kind: Kind::Deposit,
            debit_account: &checking,
            credit_account: accounts::EXTERNAL,
            amount_cents,
            run_id: None,
            order_id: None,
            idempotency_key,
        },
    )
    .await?;
    Ok(vec![entry])
}

/// A demo top-up repeated within this many seconds, by the same user for the same amount, is the
/// same top-up: a double tap or a client's retry deposits once.
pub const TOPUP_REPEAT_SECS: i64 = 10;

/// A demo top-up (`POST /v1/wallet/demo-topup`): a deposit from outside into the user's checking
/// under a new `keys::topup` key, unless the same user topped up the same amount less than
/// [`TOPUP_REPEAT_SECS`] ago, in which case that deposit's row comes back and nothing is written.
/// The account lock serializes a user's top-ups, so of two at once the second sees the first.
pub async fn demo_topup_in(
    conn: &mut PgConnection,
    user_id: &str,
    amount_cents: i64,
) -> ApiResult<Vec<LedgerEntry>> {
    if amount_cents < 1 {
        return Err(ApiError::validation("amount_cents must be at least 1."));
    }
    let checking = accounts::checking(user_id);
    ledger::lock_accounts(conn, &[&checking]).await?;
    let repeat: Option<LedgerEntry> = sqlx::query_as(concat!(
        "SELECT ",
        ledger::ledger_columns!(),
        " FROM ferry.ledger_entries \
         WHERE debit_account = $1 AND credit_account = 'external' \
           AND starts_with(idempotency_key, $2) AND amount_cents = $3 \
           AND created_at > clock_timestamp() - make_interval(secs => $4::float8) \
         ORDER BY ferry.ledger_entries.created_at DESC, ferry.ledger_entries.id DESC LIMIT 1"
    ))
    .bind(&checking)
    .bind(keys::topup_prefix(user_id))
    .bind(amount_cents)
    .bind(TOPUP_REPEAT_SECS)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(row) = repeat {
        return Ok(vec![row]);
    }
    demo_deposit_in(conn, user_id, amount_cents, &keys::topup(user_id)).await
}

/// The platform funds in Escrow that cover receipt overages, deposited once
/// (`keys::PLATFORM_SEED`).
pub async fn seed_platform_in(
    conn: &mut PgConnection,
    amount_cents: i64,
) -> ApiResult<Vec<LedgerEntry>> {
    if amount_cents < 1 {
        return Err(ApiError::validation("amount_cents must be at least 1."));
    }
    let (entry, _) = ledger::insert(
        conn,
        &NewEntry {
            kind: Kind::Deposit,
            debit_account: accounts::ESCROW,
            credit_account: accounts::EXTERNAL,
            amount_cents,
            run_id: None,
            order_id: None,
            idempotency_key: keys::PLATFORM_SEED,
        },
    )
    .await?;
    Ok(vec![entry])
}
