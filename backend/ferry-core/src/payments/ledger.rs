//! Ferry's double-entry ledger, `ferry.ledger_entries`: the source of truth for money.
//!
//! Each row moves `amount_cents` out of `credit_account` into `debit_account` (docs/DECISIONS.md,
//! Schema), so an account's balance is the sum where it is the debit account minus the sum where
//! it is the credit account. Rows are written with a unique idempotency key before any Nessie
//! call; a retry finds the row by its key.

use serde::Serialize;
use sqlx::PgConnection;

use crate::error::ApiResult;
use crate::ids;

/// Account names, as the ledger's CHECK spells them.
pub mod accounts {
    /// Money entering or leaving Ferry (a demo deposit).
    pub const EXTERNAL: &str = "external";
    /// The platform's Escrow account under the customer "Ferry (demo)".
    pub const ESCROW: &str = "platform:ferry:escrow";

    pub fn checking(user_id: &str) -> String {
        format!("user:{user_id}:checking")
    }

    /// A runner's Ferry Card (an account under the platform customer, keyed by the runner).
    pub fn ferry_card(user_id: &str) -> String {
        format!("user:{user_id}:ferry_card")
    }

    pub fn merchant(restaurant_id: &str) -> String {
        format!("restaurant:{restaurant_id}:merchant")
    }

    /// An account name, parsed.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Account {
        External,
        Escrow,
        Checking(String),
        FerryCard(String),
        Merchant(String),
        /// P2 partner restaurants' settlement account; nothing posts to it yet.
        RestaurantChecking(String),
    }

    pub fn parse(account: &str) -> Option<Account> {
        if account == EXTERNAL {
            return Some(Account::External);
        }
        if account == ESCROW {
            return Some(Account::Escrow);
        }
        let mut parts = account.splitn(3, ':');
        let (kind, owner, purpose) = (parts.next()?, parts.next()?, parts.next()?);
        match (kind, purpose) {
            ("user", "checking") if owner.starts_with("usr_") => {
                Some(Account::Checking(owner.to_owned()))
            }
            ("user", "ferry_card") if owner.starts_with("usr_") => {
                Some(Account::FerryCard(owner.to_owned()))
            }
            ("restaurant", "merchant") if owner.starts_with("rst_") => {
                Some(Account::Merchant(owner.to_owned()))
            }
            ("restaurant", "checking") if owner.starts_with("rst_") => {
                Some(Account::RestaurantChecking(owner.to_owned()))
            }
            _ => None,
        }
    }
}

/// Idempotency keys, one per money movement (spec section 4). Card returns share the `refund`
/// kind and overage covers the `card_funding` kind, so the key prefix tells them apart.
pub mod keys {
    pub fn order_hold(order_id: &str) -> String {
        format!("order_hold:{order_id}")
    }
    pub fn card_funding(run_id: &str) -> String {
        format!("card_funding:{run_id}")
    }
    pub fn overage(run_id: &str) -> String {
        format!("overage:{run_id}")
    }
    pub fn card_purchase(run_id: &str) -> String {
        format!("card_purchase:{run_id}")
    }
    /// The card's unspent remainder after the purchase, back to Escrow.
    pub fn card_return(run_id: &str) -> String {
        format!("card_return:{run_id}")
    }
    /// A cancelled order's share of the card funding, back to Escrow (cancel after close).
    pub fn card_return_cancel(run_id: &str, order_id: &str) -> String {
        format!("card_return:{run_id}:{order_id}")
    }
    pub fn refund_receipt(order_id: &str) -> String {
        format!("refund:{order_id}:receipt")
    }
    pub fn refund_cancel(order_id: &str) -> String {
        format!("refund:{order_id}:cancel")
    }
    pub fn payout(order_id: &str) -> String {
        format!("payout:{order_id}")
    }
    /// The seeder's starting balance for a user.
    pub fn seed_deposit(user_id: &str) -> String {
        format!("deposit:{user_id}:seed")
    }
    /// A demo top-up: a new key per deposit. A repeat of the same amount within a few seconds
    /// gets the earlier row back instead (`ops::demo_topup_in`).
    pub fn topup(user_id: &str) -> String {
        format!("{}{}", topup_prefix(user_id), crate::ids::new_id("tup"))
    }
    /// What every top-up key of `user_id` starts with.
    pub fn topup_prefix(user_id: &str) -> String {
        format!("deposit:{user_id}:topup:")
    }
    /// The platform funds in Escrow that cover receipt overages.
    pub const PLATFORM_SEED: &str = "deposit:platform:escrow_seed";
}

/// `kind`: WalletTxnKind's six strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Deposit,
    OrderHold,
    Refund,
    Payout,
    CardFunding,
    CardPurchase,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Deposit => "deposit",
            Kind::OrderHold => "order_hold",
            Kind::Refund => "refund",
            Kind::Payout => "payout",
            Kind::CardFunding => "card_funding",
            Kind::CardPurchase => "card_purchase",
        }
    }
}

/// One `ferry.ledger_entries` row. `created_at` is ISO 8601 UTC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct LedgerEntry {
    pub id: String,
    pub kind: String,
    pub debit_account: String,
    pub credit_account: String,
    pub amount_cents: i64,
    pub run_id: Option<String>,
    pub order_id: Option<String>,
    pub idempotency_key: String,
    pub nessie_object_id: Option<String>,
    pub nessie_status: Option<String>,
    pub created_at: String,
}

impl LedgerEntry {
    /// True once Nessie has the object and its id is stored.
    pub fn is_posted(&self) -> bool {
        self.nessie_object_id.is_some()
    }
}

/// The select list for [`LedgerEntry`], with `created_at` as an ISO 8601 UTC string. A macro, so
/// statements built with `concat!` stay string literals (sqlx 0.9 refuses runtime-built SQL).
macro_rules! ledger_columns {
    () => {
        "id, kind, debit_account, credit_account, amount_cents, run_id, order_id, \
         idempotency_key, nessie_object_id, nessie_status, \
         to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS created_at"
    };
}
pub(crate) use ledger_columns;

/// A row to write.
#[derive(Debug, Clone)]
pub struct NewEntry<'a> {
    pub kind: Kind,
    pub debit_account: &'a str,
    pub credit_account: &'a str,
    pub amount_cents: i64,
    pub run_id: Option<&'a str>,
    pub order_id: Option<&'a str>,
    pub idempotency_key: &'a str,
}

/// Inserts `entry` unless its idempotency key exists. Returns the row and whether this call
/// wrote it. `created_at` is `clock_timestamp()`, so rows written in one transaction keep their
/// order.
pub async fn insert(
    conn: &mut PgConnection,
    entry: &NewEntry<'_>,
) -> ApiResult<(LedgerEntry, bool)> {
    let inserted: Option<LedgerEntry> = sqlx::query_as(concat!(
        "INSERT INTO ferry.ledger_entries \
             (id, kind, debit_account, credit_account, amount_cents, run_id, order_id, idempotency_key, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, clock_timestamp()) \
         ON CONFLICT (idempotency_key) DO NOTHING \
         RETURNING ",
        ledger_columns!()
    ))
        .bind(ids::new_id(ids::TXN))
        .bind(entry.kind.as_str())
        .bind(entry.debit_account)
        .bind(entry.credit_account)
        .bind(entry.amount_cents)
        .bind(entry.run_id)
        .bind(entry.order_id)
        .bind(entry.idempotency_key)
        .fetch_optional(&mut *conn)
        .await?;
    match inserted {
        Some(row) => Ok((row, true)),
        None => {
            let existing = by_key(conn, entry.idempotency_key).await?.ok_or_else(|| {
                crate::error::ApiError::internal("ledger insert conflicted but the row is missing")
            })?;
            Ok((existing, false))
        }
    }
}

/// The row with this idempotency key, if any.
pub async fn by_key(conn: &mut PgConnection, key: &str) -> ApiResult<Option<LedgerEntry>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        ledger_columns!(),
        " FROM ferry.ledger_entries WHERE idempotency_key = $1"
    ))
    .bind(key)
    .fetch_optional(conn)
    .await?)
}

/// The rows whose idempotency key is one of `keys`, oldest first.
pub async fn by_keys(conn: &mut PgConnection, keys: &[String]) -> ApiResult<Vec<LedgerEntry>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        ledger_columns!(),
        " FROM ferry.ledger_entries WHERE idempotency_key = ANY($1) ORDER BY ferry.ledger_entries.created_at, ferry.ledger_entries.id"
    ))
    .bind(keys)
    .fetch_all(conn)
    .await?)
}

/// The rows with these ids, oldest first.
pub async fn by_ids(conn: &mut PgConnection, ids: &[String]) -> ApiResult<Vec<LedgerEntry>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        ledger_columns!(),
        " FROM ferry.ledger_entries WHERE id = ANY($1) ORDER BY ferry.ledger_entries.created_at, ferry.ledger_entries.id"
    ))
    .bind(ids)
    .fetch_all(conn)
    .await?)
}

/// An account's balance from the ledger: received (debit) minus paid (credit).
pub async fn balance(conn: &mut PgConnection, account: &str) -> ApiResult<i64> {
    let (balance,): (i64,) = sqlx::query_as(
        "SELECT (COALESCE(SUM(amount_cents) FILTER (WHERE debit_account = $1), 0) \
               - COALESCE(SUM(amount_cents) FILTER (WHERE credit_account = $1), 0))::bigint \
         FROM ferry.ledger_entries WHERE debit_account = $1 OR credit_account = $1",
    )
    .bind(account)
    .fetch_one(conn)
    .await?;
    Ok(balance)
}

/// Namespace for the per-account advisory locks ("FERY").
const ACCOUNT_LOCK_CLASS: i32 = 0x4645_5259;

/// Locks `accounts` until the transaction ends, in sorted order so two callers never deadlock.
///
/// A row lock on existing ledger rows cannot stop a concurrent insert, so the balance check and
/// the insert that spends from an account serialize on this lock instead. Take it before reading
/// the balance, in a statement of its own: under READ COMMITTED the next statement then sees every
/// row the previous holder committed.
pub async fn lock_accounts(conn: &mut PgConnection, accounts: &[&str]) -> ApiResult<()> {
    let mut sorted: Vec<&str> = accounts.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    for account in sorted {
        sqlx::query("SELECT pg_advisory_xact_lock($1, hashtext($2))")
            .bind(ACCOUNT_LOCK_CLASS)
            .bind(account)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::accounts::{self, Account};

    #[test]
    fn account_names_round_trip() {
        assert_eq!(accounts::checking("usr_judge"), "user:usr_judge:checking");
        assert_eq!(accounts::ferry_card("usr_maya"), "user:usr_maya:ferry_card");
        assert_eq!(
            accounts::merchant("rst_cosmic"),
            "restaurant:rst_cosmic:merchant"
        );
        assert_eq!(accounts::parse("external"), Some(Account::External));
        assert_eq!(
            accounts::parse("platform:ferry:escrow"),
            Some(Account::Escrow)
        );
        assert_eq!(
            accounts::parse("user:usr_judge:checking"),
            Some(Account::Checking("usr_judge".into()))
        );
        assert_eq!(
            accounts::parse("user:usr_maya:ferry_card"),
            Some(Account::FerryCard("usr_maya".into()))
        );
        assert_eq!(
            accounts::parse("restaurant:rst_cosmic:merchant"),
            Some(Account::Merchant("rst_cosmic".into()))
        );
        assert_eq!(accounts::parse("user:judge:checking"), None);
        assert_eq!(accounts::parse("platform:ferry:card"), None);
        assert_eq!(accounts::parse("user:usr_x"), None);
    }
}
