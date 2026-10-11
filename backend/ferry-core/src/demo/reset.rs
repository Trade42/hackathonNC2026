//! `ferry-core reset` (DEMO_MODE only): back to the demo's starting state.
//!
//! Deletes the activity created after the seed: runs, orders and their lines, events and pings,
//! pickup requests, ledger rows, notifications, invites and the agent's trip intents (so the demo
//! trip is announced again after a rehearsal). Seeded past weeks stay: a run with
//! `is_demo_history` and its orders that carry the flag too. Then seeds again, in the same
//! transaction, so run_demo is open and timed from now. Only schema ferry is touched.

use sqlx::PgPool;

use super::seed::{SeedError, SeedReport, seed};
use crate::config::Config;

#[derive(Debug, thiserror::Error)]
pub enum ResetError {
    #[error("reset runs only when DEMO_MODE is true; it deletes every run and order")]
    NotDemoMode,
    #[error(transparent)]
    Seed(#[from] SeedError),
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
}

/// Rows each delete removed, then what the seed wrote.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResetReport {
    pub deleted: Vec<(&'static str, u64)>,
    pub seeded: SeedReport,
}

impl ResetReport {
    pub fn deleted(&self, table: &str) -> Option<u64> {
        self.deleted
            .iter()
            .find(|(name, _)| *name == table)
            .map(|(_, n)| *n)
    }
}

impl std::fmt::Display for ResetReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let parts: Vec<String> = self
            .deleted
            .iter()
            .map(|(table, n)| format!("{table} {n}"))
            .collect();
        write!(f, "deleted {}; seeded {}", parts.join(", "), self.seeded)
    }
}

// What stays: a run with is_demo_history, and an order with the flag on such a run.
const KEPT_RUN: &str = "SELECT 1 FROM ferry.runs r WHERE r.id = {run} AND r.is_demo_history";
const KEPT_ORDER: &str = "SELECT 1 FROM ferry.orders o JOIN ferry.runs r ON r.id = o.run_id \
                          WHERE o.id = {order} AND o.is_demo_history AND r.is_demo_history";

/// The deletes, children before the rows they reference (orders and runs are ON DELETE
/// RESTRICT from the ledger and pickup requests).
fn deletes() -> [(&'static str, String); 10] {
    let kept_run = |run: &str| KEPT_RUN.replace("{run}", run);
    let kept_order = |order: &str| KEPT_ORDER.replace("{order}", order);
    [
        (
            "ledger_entries",
            format!(
                "DELETE FROM ferry.ledger_entries l WHERE NOT EXISTS ({}) \
                 OR (l.order_id IS NOT NULL AND NOT EXISTS ({}))",
                kept_run("l.run_id"),
                kept_order("l.order_id")
            ),
        ),
        (
            "pickup_requests",
            format!(
                "DELETE FROM ferry.pickup_requests p WHERE NOT EXISTS ({}) \
                 OR (p.order_id IS NOT NULL AND NOT EXISTS ({}))",
                kept_run("p.run_id"),
                kept_order("p.order_id")
            ),
        ),
        (
            "order_events",
            format!(
                "DELETE FROM ferry.order_events e WHERE NOT EXISTS ({})",
                kept_order("e.order_id")
            ),
        ),
        (
            "order_lines",
            format!(
                "DELETE FROM ferry.order_lines l WHERE NOT EXISTS ({})",
                kept_order("l.order_id")
            ),
        ),
        (
            "location_pings",
            format!(
                "DELETE FROM ferry.location_pings p WHERE NOT EXISTS ({})",
                kept_run("p.run_id")
            ),
        ),
        (
            "orders",
            format!(
                "DELETE FROM ferry.orders x WHERE NOT EXISTS ({})",
                kept_order("x.id")
            ),
        ),
        // The agent's trips point at runs (ON DELETE SET NULL); none is seeded, so every one is
        // newer, and they go before their runs.
        ("trip_intents", "DELETE FROM ferry.trip_intents".to_owned()),
        (
            "runs",
            "DELETE FROM ferry.runs WHERE NOT is_demo_history".to_owned(),
        ),
        // The seed makes no notifications or invites, so every one is newer.
        (
            "notifications",
            "DELETE FROM ferry.notifications".to_owned(),
        ),
        // Invite responses go with their invites (ON DELETE CASCADE).
        ("invites", "DELETE FROM ferry.invites".to_owned()),
    ]
}

/// Resets the demo, or refuses unless `config.demo_mode`.
pub async fn reset(pool: &PgPool, config: &Config) -> Result<ResetReport, ResetError> {
    if !config.demo_mode {
        return Err(ResetError::NotDemoMode);
    }
    let mut tx = pool.begin().await?;
    let mut report = ResetReport::default();
    for (table, sql) in deletes() {
        // Built from the constants above only.
        let deleted = sqlx::query(sqlx::AssertSqlSafe(sql))
            .execute(&mut *tx)
            .await?
            .rows_affected();
        report.deleted.push((table, deleted));
    }
    report.seeded = seed(&mut tx).await?;
    tx.commit().await?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_delete_is_schema_qualified_and_filled_in() {
        for (table, sql) in deletes() {
            assert!(
                sql.starts_with(&format!("DELETE FROM ferry.{table}")),
                "{sql}"
            );
            assert!(!sql.contains('{'), "{sql}");
            // Every table named is in schema ferry.
            for word in sql.split_whitespace().skip(2) {
                if word.ends_with("_entries") || word.ends_with("orders") || word.ends_with("runs")
                {
                    assert!(word.starts_with("ferry."), "{word} in {sql}");
                }
            }
        }
    }
}
