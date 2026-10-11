//! Posts ledger rows to Nessie after their transaction commits, and retries the ones that fail.
//!
//! A row is unposted while `nessie_object_id` is NULL (the `ledger_entries_unposted_idx` queue).
//! While unposted, `nessie_status` is Ferry's own bookkeeping:
//! - NULL: never tried;
//! - `posting:<attempt>:<lease_until>`: a poster claimed it (epoch seconds);
//! - `pending:<attempt>:<retry_at>:<label>`: the last attempt failed before anything reached
//!   Nessie; retry after `retry_at`, or at once on the request path (`force`);
//! - `unsure:<attempt>:<retry_at>:<label>`: the last attempt may have posted (a timeout, a dropped
//!   connection, a 5xx, an unreadable reply); nothing touches it before `retry_at`, at least
//!   [`RECONCILE_AFTER_SECS`] away, not even `force`.
//!
//! Claiming is one conditional UPDATE, so two servers on one database never post the same row
//! at once. Before a second attempt the poster lists the Nessie account and looks for the row's
//! id in the descriptions, so an attempt whose response was lost is recorded, not posted again.
//! Nessie is eventually consistent, so that lookup waits long enough for the object to be listed.
//! Once posted, `nessie_status` is Nessie's own status (`completed`).

use std::time::Duration;

use sqlx::PgPool;

use super::ledger::{self, LedgerEntry};
use super::nessie::{Nessie, NessieApi, NessieTxn};
use super::provision::{self, ProvisionError, Target};

/// The longest a user response waits on Nessie. Spec: never more than 3 s.
pub const USER_WAIT: Duration = Duration::from_millis(2500);
/// How long a claim lasts: longer than a post with its provisioning calls can take.
pub const LEASE_SECS: i64 = 120;
/// The retry backoff doubles per attempt up to this.
pub const MAX_BACKOFF_SECS: i64 = 300;
/// After an attempt that may have posted, the next one (which first looks the object up) waits at
/// least this long, so that Nessie lists an object it accepted late.
pub const RECONCILE_AFTER_SECS: i64 = 60;
/// How often the background task looks for due rows.
pub const SWEEP_EVERY: Duration = Duration::from_secs(5);

macro_rules! claim_sql {
    () => {
        "\
UPDATE ferry.ledger_entries SET nessie_status = 'posting:' \
    || (CASE WHEN nessie_status IS NULL THEN 1 ELSE split_part(nessie_status, ':', 2)::int + 1 END) \
    || ':' || (extract(epoch FROM now())::bigint + $2) \
WHERE id = $1 AND nessie_object_id IS NULL AND CASE \
    WHEN nessie_status IS NULL THEN true \
    WHEN starts_with(nessie_status, 'posting:') \
        THEN split_part(nessie_status, ':', 3)::bigint < extract(epoch FROM now()) \
    WHEN starts_with(nessie_status, 'pending:') \
        THEN $3 OR split_part(nessie_status, ':', 3)::bigint <= extract(epoch FROM now()) \
    WHEN starts_with(nessie_status, 'unsure:') \
        THEN split_part(nessie_status, ':', 3)::bigint <= extract(epoch FROM now()) \
    ELSE false END"
    };
}

/// The attempt number in a `posting:<n>:...`, `pending:<n>:...` or `unsure:<n>:...` status.
pub fn attempt_of(status: Option<&str>) -> u32 {
    status
        .and_then(|s| s.split(':').nth(1))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Backoff before attempt `attempt + 1`: 2, 4, 8, … seconds, at most [`MAX_BACKOFF_SECS`].
pub fn backoff_secs(attempt: u32) -> i64 {
    2_i64.saturating_pow(attempt.min(20)).min(MAX_BACKOFF_SECS)
}

/// The description on the Nessie object: the ledger id first, so a retry finds it.
fn description(entry: &LedgerEntry, payee: Option<&str>) -> String {
    match payee {
        Some(payee) => format!("Ferry {} {} to {payee}", entry.id, entry.idempotency_key),
        None => format!("Ferry {} {}", entry.id, entry.idempotency_key),
    }
}

/// Claims `entry_id`, posts it and records the result. `force` retries a failed row before its
/// backoff ends, but never one another poster holds or one whose last attempt may have posted
/// (`unsure:`). Returns false when the row was not claimable: posted already, held by another
/// poster, or not due.
pub async fn post_one(
    pool: &PgPool,
    nessie: &Nessie,
    entry_id: &str,
    force: bool,
) -> Result<bool, sqlx::Error> {
    let claimed: Option<LedgerEntry> = sqlx::query_as(concat!(
        claim_sql!(),
        " RETURNING ",
        ledger::ledger_columns!()
    ))
    .bind(entry_id)
    .bind(LEASE_SECS)
    .bind(force)
    .fetch_optional(pool)
    .await?;
    let Some(entry) = claimed else {
        return Ok(false);
    };
    let claim = entry.nessie_status.clone().unwrap_or_default();
    let attempt = attempt_of(Some(&claim));
    match post_claimed(pool, nessie, &entry, attempt).await {
        Ok(posted) => {
            sqlx::query(
                "UPDATE ferry.ledger_entries SET nessie_object_id = $2, nessie_status = $3 \
                 WHERE id = $1 AND nessie_object_id IS NULL",
            )
            .bind(&entry.id)
            .bind(&posted.id)
            .bind(posted.status.as_deref().unwrap_or("created"))
            .execute(pool)
            .await?;
        }
        Err(err) => {
            let (state, wait) = failed_state(err.may_have_landed(), attempt);
            tracing::warn!(
                entry = %entry.id, key = %entry.idempotency_key, attempt, error = %err,
                state, retry_in_secs = wait, "nessie post failed; will retry"
            );
            sqlx::query(
                "UPDATE ferry.ledger_entries SET nessie_status = $6 || ':' || $3 || ':' \
                     || (extract(epoch FROM now())::bigint + $4) || ':' || $5 \
                 WHERE id = $1 AND nessie_object_id IS NULL AND nessie_status = $2",
            )
            .bind(&entry.id)
            .bind(&claim)
            .bind(attempt as i32)
            .bind(wait)
            .bind(err.label())
            .bind(state)
            .execute(pool)
            .await?;
        }
    }
    Ok(true)
}

/// The status prefix and wait after failed attempt `attempt`: `unsure` and at least
/// [`RECONCILE_AFTER_SECS`] when the post may have landed, else `pending` and the backoff.
pub fn failed_state(may_have_landed: bool, attempt: u32) -> (&'static str, i64) {
    if may_have_landed {
        ("unsure", backoff_secs(attempt).max(RECONCILE_AFTER_SECS))
    } else {
        ("pending", backoff_secs(attempt))
    }
}

/// One Nessie call for a claimed row (plus provisioning). A deposit when the money comes from
/// `external`, a purchase when it goes to a merchant, otherwise a transfer on the paying account.
async fn post_claimed(
    pool: &PgPool,
    nessie: &Nessie,
    entry: &LedgerEntry,
    attempt: u32,
) -> Result<NessieTxn, ProvisionError> {
    let date = entry.created_at.get(..10).unwrap_or("1970-01-01");
    let receiver = provision::resolve(pool, nessie, &entry.debit_account).await?;
    let payer = provision::resolve(pool, nessie, &entry.credit_account).await?;
    let amount = entry.amount_cents;
    match (payer, receiver) {
        (Target::External, Target::Account(account)) => {
            let description = description(entry, None);
            if attempt > 1
                && let Some(found) = find(nessie.deposits(&account).await?, &entry.id)
            {
                return Ok(found);
            }
            Ok(nessie.deposit(&account, amount, date, &description).await?)
        }
        (Target::Account(card), Target::Merchant(merchant)) => {
            let description = description(entry, None);
            if attempt > 1
                && let Some(found) = find(nessie.purchases(&card).await?, &entry.id)
            {
                return Ok(found);
            }
            Ok(nessie
                .purchase(&card, &merchant, amount, date, &description)
                .await?)
        }
        (Target::Account(from), Target::Account(to)) => {
            let description = description(entry, Some(&to));
            if attempt > 1
                && let Some(found) = find(nessie.transfers(&from).await?, &entry.id)
            {
                return Ok(found);
            }
            Ok(nessie.transfer(&from, amount, date, &description).await?)
        }
        (payer, receiver) => Err(ProvisionError::Unsupported(format!(
            "no Nessie call moves money from {payer:?} to {receiver:?}"
        ))),
    }
}

fn find(posted: Vec<NessieTxn>, entry_id: &str) -> Option<NessieTxn> {
    let needle = format!("Ferry {entry_id} ");
    posted.into_iter().find(|t| {
        t.description
            .as_deref()
            .is_some_and(|d| d.starts_with(&needle))
    })
}

/// Posts `entries` on the request path, waiting at most [`USER_WAIT`]; whatever is not done by
/// then finishes in the background (or in the retry task). Returns the rows as they stand.
pub async fn post_now(pool: &PgPool, nessie: &Nessie, entries: &[LedgerEntry]) -> Vec<LedgerEntry> {
    let ids: Vec<String> = entries
        .iter()
        .filter(|e| !e.is_posted())
        .map(|e| e.id.clone())
        .collect();
    if ids.is_empty() {
        return entries.to_vec();
    }
    let task = {
        let (pool, nessie, ids) = (pool.clone(), nessie.clone(), ids.clone());
        tokio::spawn(async move {
            for id in ids {
                if let Err(err) = post_one(&pool, &nessie, &id, true).await {
                    tracing::warn!(entry = %id, error = %err, "nessie post: database error");
                }
            }
        })
    };
    let _ = tokio::time::timeout(USER_WAIT, task).await;
    let all: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
    match pool.acquire().await {
        Ok(mut conn) => ledger::by_ids(&mut conn, &all)
            .await
            .unwrap_or_else(|_| entries.to_vec()),
        Err(_) => entries.to_vec(),
    }
}

/// Tries every unposted row that is due, oldest first. Returns how many it claimed. A row that
/// fails with a database error (say, a status no claim can parse) is logged and skipped, so one
/// bad row never stops the others; only a failure to list the rows is an error.
pub async fn retry_due(pool: &PgPool, nessie: &Nessie) -> Result<usize, sqlx::Error> {
    let ids: Vec<(String,)> = sqlx::query_as(
        "SELECT id FROM ferry.ledger_entries WHERE nessie_object_id IS NULL \
         ORDER BY created_at LIMIT 100",
    )
    .fetch_all(pool)
    .await?;
    let mut claimed = 0;
    for (id,) in ids {
        match post_one(pool, nessie, &id, false).await {
            Ok(true) => claimed += 1,
            Ok(false) => {}
            Err(err) => tracing::warn!(
                entry = %id, error = %masked(&err),
                "nessie retry sweep: skipped a row it could not claim or record"
            ),
        }
    }
    Ok(claimed)
}

/// A database error for the log, masked: Postgres messages can quote the row's values (a bad
/// status, an account name), so a database error shows only its SQLSTATE code.
fn masked(err: &sqlx::Error) -> String {
    match err {
        sqlx::Error::Database(db) => format!(
            "database error {}",
            db.code().as_deref().unwrap_or("without a code")
        ),
        sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed | sqlx::Error::Io(_) => err.to_string(),
        _ => "unexpected database reply".into(),
    }
}

/// The background retry task: sweeps every [`SWEEP_EVERY`], forever.
pub async fn run_retry_loop(pool: PgPool, nessie: Nessie) {
    let mut tick = tokio::time::interval(SWEEP_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match retry_due(&pool, &nessie).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(claimed = n, "nessie retry sweep"),
            Err(err) => tracing::warn!(error = %err, "nessie retry sweep: database error"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempts_and_backoff() {
        assert_eq!(attempt_of(None), 0);
        assert_eq!(attempt_of(Some("posting:1:1791679457")), 1);
        assert_eq!(attempt_of(Some("pending:3:1791679457:timeout")), 3);
        assert_eq!(backoff_secs(1), 2);
        assert_eq!(backoff_secs(2), 4);
        assert_eq!(backoff_secs(8), 256);
        assert_eq!(backoff_secs(9), MAX_BACKOFF_SECS);
        assert_eq!(backoff_secs(u32::MAX), MAX_BACKOFF_SECS);
        assert_eq!(attempt_of(Some("unsure:2:1791679457:timeout")), 2);
    }

    #[test]
    fn a_post_that_may_have_landed_waits_for_the_lookup() {
        assert_eq!(failed_state(false, 1), ("pending", 2));
        assert_eq!(failed_state(true, 1), ("unsure", RECONCILE_AFTER_SECS));
        assert_eq!(failed_state(true, 6), ("unsure", 64));
        assert_eq!(failed_state(true, 30), ("unsure", MAX_BACKOFF_SECS));
    }
}
