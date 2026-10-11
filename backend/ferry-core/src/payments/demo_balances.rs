//! The demo's starting money: the platform funds in Escrow and each persona's checking balance.
//!
//! `ferry-core seed` and `ferry-core reset` call [`seed_demo_balances`] last. The deposits use
//! fixed keys (`keys::PLATFORM_SEED`, `keys::seed_deposit`), so a second seed writes nothing, and
//! a reset, which deletes every ledger row outside demo history (these deposits too), gets them
//! back. The rows are only written here: the command exits right away, so the server's retry task
//! posts them to Nessie on its next sweep.

use sqlx::{PgConnection, PgPool};

use super::ledger::{LedgerEntry, keys};
use super::{PLATFORM_SEED_CENTS, ops};
use crate::error::ApiResult;

/// Each persona's starting balance, in cents: one per user in contract/fixtures/users.json, the
/// same amounts the frontend's mock wallets start from.
pub const PERSONA_BALANCES: &[(&str, i64)] = &[
    ("usr_judge", 5_000),
    ("usr_maya", 2_500),
    ("usr_dana", 4_000),
    ("usr_leo", 3_000),
];

/// Writes the platform funds and every persona's starting deposit on the caller's transaction,
/// each unless its key is already written. Returns the rows, written now or before.
pub async fn seed_demo_balances_in(conn: &mut PgConnection) -> ApiResult<Vec<LedgerEntry>> {
    let mut rows = ops::seed_platform_in(conn, PLATFORM_SEED_CENTS).await?;
    for (user_id, cents) in PERSONA_BALANCES {
        rows.extend(
            ops::demo_deposit_in(conn, user_id, *cents, &keys::seed_deposit(user_id)).await?,
        );
    }
    Ok(rows)
}

/// [`seed_demo_balances_in`] in a transaction of its own. For `ferry-core seed` and `reset`.
pub async fn seed_demo_balances(pool: &PgPool) -> ApiResult<Vec<LedgerEntry>> {
    let mut tx = pool.begin().await?;
    let rows = seed_demo_balances_in(&mut tx).await?;
    tx.commit().await?;
    let unposted = rows.iter().filter(|r| !r.is_posted()).count();
    tracing::info!(
        personas = PERSONA_BALANCES.len(),
        escrow_cents = PLATFORM_SEED_CENTS,
        unposted,
        "demo balances in the ledger; the server's retry task posts what is unposted to Nessie"
    );
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::fixtures::Fixtures;

    #[test]
    fn every_fixture_persona_has_a_starting_balance() {
        let mut fixture_users: Vec<String> = Fixtures::load()
            .unwrap()
            .users
            .into_iter()
            .map(|u| u.id)
            .collect();
        fixture_users.sort();
        let mut personas: Vec<String> = PERSONA_BALANCES
            .iter()
            .map(|(id, _)| (*id).to_owned())
            .collect();
        personas.sort();
        assert_eq!(personas, fixture_users);
        assert!(PERSONA_BALANCES.iter().all(|(_, cents)| *cents > 0));
    }
}
