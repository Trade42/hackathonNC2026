//! Demo seed, reset and the runner simulator; /v1/wallet/demo-topup.
//!
//! `ferry-core seed` loads contract/fixtures into schema ferry; `ferry-core reset` (DEMO_MODE
//! only) deletes the activity since and seeds again. Both run after the migrations, then exit.
//! The runner simulator (`simulator`, DEMO_MODE only) walks a run's runner along the route.

use axum::Router;
use axum::routing::any;
use sqlx::PgPool;

use crate::config::Config;
use crate::state::AppState;

pub mod fixtures;
pub mod reset;
pub mod seed;
pub mod simulator;

/// This module's routes, with full `/v1/...` paths. Merged into the app by `app::router`. The
/// `/internal/demo/...` routes answer only in DEMO_MODE and with `X-Ferry-Secret`. They take every
/// method (`any`), so outside DEMO_MODE each one answers exactly as an unknown route does: the
/// app's 405 fallback and axum's `Allow` header never apply (runner-sim review MINOR 3).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/internal/demo/runs/{id}/walk", any(simulator::walk_route))
        .route(
            "/internal/demo/runs/{id}/walk/stop",
            any(simulator::walk_stop_route),
        )
}

/// A `ferry-core <command>` that runs instead of the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Seed,
    Reset,
}

impl Command {
    /// The command in the process arguments (after the program name); `None` serves the API.
    pub fn from_args(mut args: impl Iterator<Item = String>) -> anyhow::Result<Option<Self>> {
        let command = match args.next().as_deref() {
            None => return Ok(None),
            Some("seed") => Command::Seed,
            Some("reset") => Command::Reset,
            Some(other) => anyhow::bail!(
                "unknown command {other:?}: run ferry-core with no arguments to serve, or with seed or reset"
            ),
        };
        if let Some(extra) = args.next() {
            anyhow::bail!("unexpected argument {extra:?} after {command:?}");
        }
        Ok(Some(command))
    }
}

/// Runs `command` against the migrated database.
pub async fn run_command(command: Command, pool: &PgPool, config: &Config) -> anyhow::Result<()> {
    match command {
        Command::Seed => {
            let report = seed::seed_pool(pool).await?;
            tracing::info!("seed done, rows written: {report}");
            let history = crate::impact::seed_history(pool).await?;
            tracing::info!("demo history done, rows written: {history}");
            crate::payments::seed_demo_balances(pool).await?;
        }
        Command::Reset => {
            let report = reset::reset(pool, config).await?;
            tracing::info!("reset done: {report}");
            crate::payments::seed_demo_balances(pool).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> anyhow::Result<Option<Command>> {
        Command::from_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn commands() {
        assert_eq!(parse(&[]).unwrap(), None);
        assert_eq!(parse(&["seed"]).unwrap(), Some(Command::Seed));
        assert_eq!(parse(&["reset"]).unwrap(), Some(Command::Reset));
        assert!(parse(&["serve"]).is_err());
        assert!(parse(&["seed", "now"]).is_err());
        assert!(parse(&["--seed"]).is_err());
    }
}
