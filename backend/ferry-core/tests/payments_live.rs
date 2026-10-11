//! The money flow against the live Nessie sandbox. Ignored by default: it needs the network
//! and `NESSIE_API_KEY` in the root .env, and it creates sandbox objects under that key.
//!
//! Run it from `backend/` with:
//!
//! ```sh
//! cargo test --test payments_live -- --ignored --nocapture
//! ```
//!
//! The database is still the local test Postgres (the guard in tests/common forces it).
mod common;
mod payments_support;

use std::sync::Arc;

use ferry_core::config::Config;
use ferry_core::payments::Payments;
use ferry_core::payments::ledger::{accounts, keys};
use ferry_core::payments::nessie::{
    DEFAULT_BASE_URL, LiveNessie, MOCK_ID_PREFIX, Nessie, NessieApi,
};
use ferry_core::payments::provision::{self, Target};
use payments_support::*;
use sqlx::PgPool;

/// Live Payments over `pool`, or None (with a note) when no key is configured.
fn live_payments(pool: &PgPool) -> Option<Payments> {
    let (loaded, _) = Config::load().expect("config loads");
    let Some(key) = loaded.nessie_api_key.clone() else {
        eprintln!("NESSIE_API_KEY is unset: skipping the live Nessie test");
        return None;
    };
    let base = loaded
        .nessie_base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
    let host = url::Url::parse(&base)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned));
    eprintln!("live Nessie at {}", host.unwrap_or_default());
    let http = ferry_core::state::http_client().unwrap();
    let nessie = Nessie::Live(LiveNessie::new(http, &base, key));
    // Only the Nessie settings come from the file; everything else is the test config.
    let config = common::config(&[("NESSIE_MODE", "live")]);
    Some(Payments::new(pool.clone(), Arc::new(config), nessie))
}

#[sqlx::test(migrations = "../migrations")]
#[ignore = "calls the live Nessie sandbox; run with --ignored"]
async fn live_nessie_money_flow(pool: PgPool) {
    let Some(payments) = live_payments(&pool) else {
        return;
    };
    assert_eq!(payments.wallet_mode(), "nessie_live");
    seed_people(&pool).await;
    payments.seed_platform(SEED_CENTS).await.unwrap();
    for user in ["usr_judge", "usr_dana"] {
        payments
            .demo_deposit(user, 5000, &keys::seed_deposit(user))
            .await
            .unwrap();
    }
    create_run(&pool, "run_live").await;
    place_order(
        &payments,
        "run_live",
        "ord_live_judge",
        "usr_judge",
        1249,
        200,
    )
    .await
    .unwrap();
    place_order(&payments, "run_live", "ord_live_dana", "usr_dana", 899, 0)
        .await
        .unwrap();
    payments.fund_card("run_live").await.unwrap();
    payments.card_purchase("run_live", 2209).await.unwrap();
    payments.payout("ord_live_judge").await.unwrap();
    payments.payout("ord_live_dana").await.unwrap();

    // Whatever the 2.5 s request budget left behind, the retry pass finishes.
    for _ in 0..5 {
        if payments.retry_due().await.unwrap() == 0 {
            break;
        }
    }
    let rows: Vec<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, idempotency_key, nessie_object_id, nessie_status FROM ferry.ledger_entries \
         ORDER BY created_at",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    for (id, key, object, status) in &rows {
        eprintln!("{key:32} {id} -> {object:?} {status:?}");
        let object = object
            .as_deref()
            .unwrap_or_else(|| panic!("{key} not posted: {status:?}"));
        assert_eq!(object.len(), 36, "a Nessie UUID: {object}");
        assert!(!object.starts_with(MOCK_ID_PREFIX));
        assert_eq!(status.as_deref(), Some("completed"));
    }
    assert_eq!(
        rows.len(),
        12,
        "seed, 2 deposits, 2 holds, funding, purchase, return, 2 refunds, 2 payouts"
    );

    // A retry after a lost response finds the object by its description.
    let Target::Account(judge) =
        provision::resolve(&pool, payments.nessie(), &accounts::checking("usr_judge"))
            .await
            .unwrap()
    else {
        panic!("judge's checking is an account");
    };
    let deposits = payments.nessie().deposits(&judge).await.unwrap();
    let seed_row = rows
        .iter()
        .find(|r| r.1 == "deposit:usr_judge:seed")
        .unwrap();
    assert!(
        deposits.iter().any(|d| d
            .description
            .as_deref()
            .unwrap_or_default()
            .starts_with(&format!("Ferry {} ", seed_row.0))),
        "the deposit is listed on judge's account"
    );
    assert_eq!(checking(&pool, "usr_judge").await, 5000 - 1693 + 58);
    assert_eq!(checking(&pool, RUNNER).await, 500);
    assert_conserved(&pool).await;
}
