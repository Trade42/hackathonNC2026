//! `GET /v1/wallet` and `POST /v1/wallet/demo-topup`: the contract's Wallet, read from the
//! ledger.
//!
//! The balance is the viewer's checking. The transactions are the ledger rows that touch the
//! viewer's checking or, for a runner, their Ferry Card, signed from the viewer's side, newest
//! first.

use axum::extract::State;
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};

use super::ledger::{self, accounts};
use super::{Payments, math};
use crate::error::{ApiError, ApiResult};
use crate::extract::Json;
use crate::jwt::AuthUser;
use crate::state::AppState;

/// At most this many transactions in a Wallet.
pub const MAX_TRANSACTIONS: i64 = 200;
/// A demo top-up is 1 to 100000 cents ($1,000).
pub const MAX_TOPUP_CENTS: i64 = 100_000;

/// The contract's `Wallet`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Wallet {
    pub balance_cents: i64,
    pub account_label: String,
    pub mode: &'static str,
    pub transactions: Vec<WalletTxn>,
}

/// The contract's `WalletTxn`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WalletTxn {
    pub id: String,
    pub kind: String,
    pub amount_cents: i64,
    pub description: String,
    pub at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
}

/// The contract's `DemoTopupRequest`.
#[derive(Debug, Clone, Deserialize)]
pub struct DemoTopupRequest {
    pub amount_cents: i64,
}

#[derive(sqlx::FromRow)]
struct TxnRow {
    id: String,
    kind: String,
    debit_account: String,
    credit_account: String,
    amount_cents: i64,
    run_id: Option<String>,
    order_id: Option<String>,
    idempotency_key: String,
    at: String,
    restaurant_name: Option<String>,
}

/// What a row says to the person reading their wallet.
pub fn describe(kind: &str, idempotency_key: &str, restaurant: Option<&str>) -> String {
    let at = restaurant.unwrap_or("the restaurant");
    let prefix = idempotency_key.split(':').next().unwrap_or_default();
    match (kind, prefix) {
        ("deposit", _) if idempotency_key.contains(":topup:") => "Demo top-up".into(),
        ("deposit", _) if idempotency_key.ends_with(":seed") => "Demo starting balance".into(),
        ("deposit", _) => "Deposit".into(),
        ("order_hold", _) => format!("Order from {at}, held until delivery"),
        ("refund", "card_return") if idempotency_key.matches(':').count() == 2 => {
            format!("Ferry Card: a cancelled order's share returned ({at})")
        }
        ("refund", "card_return") => format!("Ferry Card: unspent funds returned ({at})"),
        ("refund", _) if idempotency_key.ends_with(":cancel") => {
            format!("Refund: order from {at} cancelled")
        }
        ("refund", _) if idempotency_key.ends_with(":receipt") => {
            format!("Refund: {at}'s receipt came in lower")
        }
        ("refund", _) => "Refund".into(),
        ("card_funding", "overage") => format!("Ferry Card: Ferry covered the overage at {at}"),
        ("card_funding", _) => format!("Ferry Card funded for the run to {at}"),
        ("card_purchase", _) => format!("Ferry Card purchase at {at}"),
        ("payout", _) => format!("Payout: fee and tip for a delivery from {at}"),
        _ => kind.replace('_', " "),
    }
}

/// The label the wallet shows for the viewer's checking.
pub fn account_label(mode: &str) -> String {
    if mode == "nessie_mock" {
        "Ferry checking (Nessie mock)".into()
    } else {
        "Ferry checking (Nessie sandbox)".into()
    }
}

/// The viewer's Wallet, from the ledger.
pub async fn load(conn: &mut PgConnection, user_id: &str, mode: &'static str) -> ApiResult<Wallet> {
    let checking = accounts::checking(user_id);
    let card = accounts::ferry_card(user_id);
    let balance_cents = ledger::balance(&mut *conn, &checking).await?;
    let rows: Vec<TxnRow> = sqlx::query_as(
        "SELECT e.id, e.kind, e.debit_account, e.credit_account, e.amount_cents, e.run_id, \
                e.order_id, e.idempotency_key, \
                to_char(e.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS at, \
                r.name AS restaurant_name \
         FROM ferry.ledger_entries e \
         LEFT JOIN ferry.runs ru ON ru.id = e.run_id \
         LEFT JOIN ferry.restaurants r ON r.id = ru.restaurant_id \
         WHERE e.debit_account = ANY($1) OR e.credit_account = ANY($1) \
         ORDER BY e.created_at DESC, e.id DESC \
         LIMIT $2",
    )
    .bind(vec![checking.clone(), card.clone()])
    .bind(MAX_TRANSACTIONS)
    .fetch_all(conn)
    .await?;
    let mine = [checking.as_str(), card.as_str()];
    let transactions = rows
        .into_iter()
        .map(|row| WalletTxn {
            amount_cents: math::signed_amount(
                &row.debit_account,
                &row.credit_account,
                row.amount_cents,
                &mine,
            ),
            description: describe(
                &row.kind,
                &row.idempotency_key,
                row.restaurant_name.as_deref(),
            ),
            id: row.id,
            kind: row.kind,
            at: row.at,
            run_id: row.run_id,
            order_id: row.order_id,
        })
        .collect();
    Ok(Wallet {
        balance_cents,
        account_label: account_label(mode),
        mode,
        transactions,
    })
}

/// A valid token whose user no longer exists gets 401 `unauthorized` from both wallet endpoints,
/// as from `/v1/me` (docs/DECISIONS.md), so the client signs in again.
async fn ensure_user_exists(pool: &PgPool, user_id: &str) -> ApiResult<()> {
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM ferry.users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(pool)
        .await?;
    match exists {
        Some(_) => Ok(()),
        None => Err(ApiError::unauthorized(
            "This account no longer exists. Sign in again.",
        )),
    }
}

/// `GET /v1/wallet`.
pub async fn get_wallet(State(state): State<AppState>, user: AuthUser) -> ApiResult<Json<Wallet>> {
    ensure_user_exists(&state.pool, &user.user_id).await?;
    let payments = Payments::from_state(&state);
    let mut conn = state.pool.acquire().await?;
    Ok(Json(
        load(&mut conn, &user.user_id, payments.wallet_mode()).await?,
    ))
}

/// `POST /v1/wallet/demo-topup {amount_cents}`: demo mode only (else 403 `forbidden`), 1 to
/// 100000 cents (else 400 `validation_failed`), for a user who exists (else 401). The same amount
/// again within `ops::TOPUP_REPEAT_SECS` answers the same wallet without a second deposit.
pub async fn demo_topup(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<DemoTopupRequest>,
) -> ApiResult<Json<Wallet>> {
    if !state.config.demo_mode {
        return Err(ApiError::forbidden("Demo top-ups work only in demo mode."));
    }
    if !(1..=MAX_TOPUP_CENTS).contains(&body.amount_cents) {
        return Err(ApiError::validation(format!(
            "amount_cents must be between 1 and {MAX_TOPUP_CENTS}."
        ))
        .with_details(serde_json::json!({"field": "amount_cents"})));
    }
    ensure_user_exists(&state.pool, &user.user_id).await?;
    let payments = Payments::from_state(&state);
    payments
        .demo_topup(&user.user_id, body.amount_cents)
        .await?;
    let mut conn = state.pool.acquire().await?;
    Ok(Json(
        load(&mut conn, &user.user_id, payments.wallet_mode()).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptions_tell_shared_kinds_apart() {
        let r = Some("Cosmic Cantina");
        assert_eq!(
            describe("deposit", "deposit:usr_a:seed", r),
            "Demo starting balance"
        );
        assert_eq!(
            describe("deposit", "deposit:usr_a:topup:tup_x", r),
            "Demo top-up"
        );
        assert_eq!(
            describe("refund", "card_return:run_1", r),
            "Ferry Card: unspent funds returned (Cosmic Cantina)"
        );
        assert_eq!(
            describe("refund", "card_return:run_1:ord_1", r),
            "Ferry Card: a cancelled order's share returned (Cosmic Cantina)"
        );
        assert_eq!(
            describe("refund", "refund:ord_1:cancel", r),
            "Refund: order from Cosmic Cantina cancelled"
        );
        assert_eq!(
            describe("refund", "refund:ord_1:receipt", r),
            "Refund: Cosmic Cantina's receipt came in lower"
        );
        assert_eq!(
            describe("card_funding", "overage:run_1", r),
            "Ferry Card: Ferry covered the overage at Cosmic Cantina"
        );
        assert_eq!(
            describe("card_funding", "card_funding:run_1", None),
            "Ferry Card funded for the run to the restaurant"
        );
    }
}
