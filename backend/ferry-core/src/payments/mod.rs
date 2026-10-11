//! Payments (spec section 4): Ferry's double-entry ledger, the Nessie client (live and mock),
//! money math, the money operations, and the wallet: `/v1/wallet`.
//!
//! Every money movement is one call that writes its ledger row first (unique idempotency key),
//! then moves the money in Nessie, then records the Nessie id. The ledger is the source of
//! truth: balances come from it, never from Nessie.
//!
//! For the runs and orders code: call an op's `ops::*_in` form inside your transaction (after
//! `SELECT … FROM ferry.runs WHERE id = $1 FOR UPDATE`), commit, then `payments.post(&rows)`.
//! Or call the `Payments` method of the same name, which opens its own transaction.

use std::sync::{Arc, OnceLock};

use axum::Router;
use axum::routing::{get, post};
use sqlx::PgPool;

use crate::config::Config;
use crate::error::ApiResult;
use crate::state::AppState;

pub mod demo_balances;
pub mod ledger;
pub mod math;
pub mod nessie;
pub mod ops;
pub mod poster;
pub mod provision;
pub mod wallet;

pub use demo_balances::seed_demo_balances;
pub use ledger::LedgerEntry;
pub use math::{OrderQuote, PricedLine};
pub use nessie::Nessie;

/// Platform funds deposited once into Escrow to cover receipt overages: $1,000. The demo seed and
/// reset write them too (`demo_balances`).
pub const PLATFORM_SEED_CENTS: i64 = 100_000;

/// This module's routes, with full `/v1/...` paths. Merged into the app by `app::router`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/wallet", get(wallet::get_wallet))
        .route("/v1/wallet/demo-topup", post(wallet::demo_topup))
}

/// The money service: the pool and the Nessie client for the configured mode. Cheap to build
/// and to clone.
#[derive(Clone)]
pub struct Payments {
    pool: PgPool,
    nessie: Nessie,
    config: Arc<Config>,
}

impl Payments {
    pub fn new(pool: PgPool, config: Arc<Config>, nessie: Nessie) -> Self {
        Self {
            pool,
            nessie,
            config,
        }
    }

    /// The service for a handler. The first call in the process also starts the retry task
    /// (see [`start`]).
    pub fn from_state(state: &AppState) -> Self {
        let payments = Self::new(
            state.pool.clone(),
            state.config.clone(),
            Nessie::from_config(&state.config, &state.http),
        );
        payments.start_once();
        payments
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn nessie(&self) -> &Nessie {
        &self.nessie
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// WalletMode for this service: `nessie_live` or `nessie_mock`.
    pub fn wallet_mode(&self) -> &'static str {
        self.nessie.wallet_mode()
    }

    /// Spawns, once per process, the task that seeds the platform funds and retries unposted
    /// rows every few seconds.
    fn start_once(&self) {
        static STARTED: OnceLock<()> = OnceLock::new();
        if STARTED.set(()).is_ok() && tokio::runtime::Handle::try_current().is_ok() {
            let payments = self.clone();
            tokio::spawn(async move {
                if let Err(err) = payments.seed_platform(PLATFORM_SEED_CENTS).await {
                    tracing::warn!(error = %err, "could not seed the platform funds in Escrow");
                }
                poster::run_retry_loop(payments.pool.clone(), payments.nessie.clone()).await;
            });
            tracing::info!(mode = self.wallet_mode(), "payments: retry task started");
        }
    }

    /// Posts rows to Nessie, waiting at most `poster::USER_WAIT` (2.5 s); the rest finishes in
    /// the background. Returns the rows as they stand.
    pub async fn post(&self, rows: &[LedgerEntry]) -> Vec<LedgerEntry> {
        poster::post_now(&self.pool, &self.nessie, rows).await
    }

    /// Posts every unposted row that is due now. The retry task calls this; tests call it.
    pub async fn retry_due(&self) -> Result<usize, sqlx::Error> {
        poster::retry_due(&self.pool, &self.nessie).await
    }

    pub async fn hold_order(&self, order_id: &str) -> ApiResult<Vec<LedgerEntry>> {
        let mut tx = self.pool.begin().await?;
        let rows = ops::hold_order_in(&mut tx, order_id).await?;
        tx.commit().await?;
        Ok(self.post(&rows).await)
    }

    pub async fn fund_card(&self, run_id: &str) -> ApiResult<Vec<LedgerEntry>> {
        let mut tx = self.pool.begin().await?;
        let rows = ops::fund_card_in(&mut tx, run_id).await?;
        tx.commit().await?;
        Ok(self.post(&rows).await)
    }

    pub async fn card_purchase(
        &self,
        run_id: &str,
        receipt_total_cents: i64,
    ) -> ApiResult<Vec<LedgerEntry>> {
        let mut tx = self.pool.begin().await?;
        let rows = ops::card_purchase_in(&mut tx, run_id, receipt_total_cents).await?;
        tx.commit().await?;
        Ok(self.post(&rows).await)
    }

    pub async fn payout(&self, order_id: &str) -> ApiResult<Vec<LedgerEntry>> {
        let mut tx = self.pool.begin().await?;
        let rows = ops::payout_in(&mut tx, order_id).await?;
        tx.commit().await?;
        Ok(self.post(&rows).await)
    }

    pub async fn refund_cancelled(&self, order_id: &str) -> ApiResult<Vec<LedgerEntry>> {
        let mut tx = self.pool.begin().await?;
        let rows = ops::refund_cancelled_in(&mut tx, order_id).await?;
        tx.commit().await?;
        Ok(self.post(&rows).await)
    }

    pub async fn demo_deposit(
        &self,
        user_id: &str,
        amount_cents: i64,
        idempotency_key: &str,
    ) -> ApiResult<Vec<LedgerEntry>> {
        let mut tx = self.pool.begin().await?;
        let rows = ops::demo_deposit_in(&mut tx, user_id, amount_cents, idempotency_key).await?;
        tx.commit().await?;
        Ok(self.post(&rows).await)
    }

    /// A demo top-up; the same amount again within `ops::TOPUP_REPEAT_SECS` deposits nothing more.
    pub async fn demo_topup(
        &self,
        user_id: &str,
        amount_cents: i64,
    ) -> ApiResult<Vec<LedgerEntry>> {
        let mut tx = self.pool.begin().await?;
        let rows = ops::demo_topup_in(&mut tx, user_id, amount_cents).await?;
        tx.commit().await?;
        Ok(self.post(&rows).await)
    }

    pub async fn seed_platform(&self, amount_cents: i64) -> ApiResult<Vec<LedgerEntry>> {
        let mut tx = self.pool.begin().await?;
        let rows = ops::seed_platform_in(&mut tx, amount_cents).await?;
        tx.commit().await?;
        Ok(self.post(&rows).await)
    }

    /// An account's balance from the ledger.
    pub async fn balance(&self, account: &str) -> ApiResult<i64> {
        let mut conn = self.pool.acquire().await?;
        ledger::balance(&mut conn, account).await
    }
}

/// Starts the retry task now instead of on the first payments call. For `main.rs`, right after
/// the state is built, so rows left unposted by a restart are retried at once.
pub fn start(state: &AppState) {
    let _ = Payments::from_state(state);
}
