//! The demo's starting money through the real commands, `ferry-core seed` and `ferry-core reset`
//! (`demo::run_command`): Escrow and every persona start with their balances, a second seed adds
//! nothing, and after a reset the judge can order on run_demo again.
mod common;
mod payments_support;

use ferry_core::config::Config;
use ferry_core::demo::{Command, run_command};
use ferry_core::error::ErrorCode;
use ferry_core::payments::demo_balances::PERSONA_BALANCES;
use ferry_core::payments::ledger::keys;
use ferry_core::payments::{PLATFORM_SEED_CENTS, math};
use payments_support::*;
use sqlx::PgPool;

fn demo_config() -> Config {
    common::config(&[("DEMO_MODE", "true")])
}

async fn assert_starting_balances(pool: &PgPool) {
    assert_eq!(escrow(pool).await, PLATFORM_SEED_CENTS);
    for (user, cents) in PERSONA_BALANCES {
        assert_eq!(checking(pool, user).await, *cents, "{user}");
    }
}

/// The judge's order in the demo script: a chicken burrito, no beans, with their default tip.
/// Returns (subtotal, tip).
async fn judge_order(pool: &PgPool) -> (i64, i64) {
    let price: i64 = sqlx::query_scalar(
        "SELECT price_cents FROM ferry.menu_items WHERE id = 'itm_cosmic_chicken_burrito'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let delta: i64 = sqlx::query_scalar(
        "SELECT price_delta_cents FROM ferry.item_choices \
         WHERE id = 'cho_cosmic_chicken_burrito_no_beans'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let tip: i64 = sqlx::query_scalar(
        "SELECT default_tip_cents FROM ferry.profiles WHERE user_id = 'usr_judge'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    (price + delta, tip)
}

#[sqlx::test(migrations = "../migrations")]
async fn seed_gives_escrow_and_every_persona_their_starting_money_once(pool: PgPool) {
    run_command(Command::Seed, &pool, &demo_config())
        .await
        .unwrap();
    assert_starting_balances(&pool).await;
    assert_eq!(rows_with_key(&pool, keys::PLATFORM_SEED).await, 1);
    for (user, _) in PERSONA_BALANCES {
        assert_eq!(rows_with_key(&pool, &keys::seed_deposit(user)).await, 1);
        // Every persona with money is a seeded user.
        let seeded: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM ferry.users WHERE id = $1)")
                .bind(user)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(seeded, "{user}");
    }
    let rows = row_count(&pool).await;
    assert_eq!(rows, 1 + PERSONA_BALANCES.len() as i64);

    // A second seed adds no money.
    run_command(Command::Seed, &pool, &demo_config())
        .await
        .unwrap();
    assert_eq!(row_count(&pool).await, rows);
    assert_starting_balances(&pool).await;

    // The command only writes the rows; the server's retry task posts each one to Nessie once.
    let (payments, mock) = payments(&pool);
    assert_eq!(payments.retry_due().await.unwrap() as i64, rows);
    assert_eq!(mock.post_count() as i64, rows);
    assert_eq!(payments.retry_due().await.unwrap(), 0);
    assert_conserved(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn after_a_reset_the_judge_can_order_on_run_demo(pool: PgPool) {
    let config = demo_config();
    run_command(Command::Seed, &pool, &config).await.unwrap();
    let (payments, _mock) = payments(&pool);
    let (subtotal, tip) = judge_order(&pool).await;
    let total = math::quote_with(
        payments.config(),
        &[math::PricedLine {
            unit_price_cents: subtotal,
            qty: 1,
        }],
        tip,
    )
    .unwrap()
    .total_cents;

    // A demo before: the judge orders on run_demo until the money runs out, then the run closes,
    // the card is funded and the receipt comes in over the estimate, so Escrow covers some.
    let mut placed = 0;
    loop {
        let order = format!("ord_before_{placed}");
        match place_order(&payments, "run_demo", &order, "usr_judge", subtotal, tip).await {
            Ok(_) => placed += 1,
            Err(err) => {
                assert_eq!(err.code, ErrorCode::InsufficientFunds);
                break;
            }
        }
    }
    assert!(
        placed >= 1,
        "the starting balance pays for at least one order"
    );
    assert!(checking(&pool, "usr_judge").await < total);
    set_run_status(&pool, "run_demo", "closed").await;
    let funded = payments.fund_card("run_demo").await.unwrap()[0].amount_cents;
    payments
        .card_purchase("run_demo", math::max_receipt_cents(funded))
        .await
        .unwrap();

    // The demo reset puts every starting balance back, Escrow's platform funds included.
    run_command(Command::Reset, &pool, &config).await.unwrap();
    assert_starting_balances(&pool).await;
    assert_eq!(
        row_count(&pool).await,
        1 + PERSONA_BALANCES.len() as i64,
        "only the starting deposits"
    );

    // The judge orders on run_demo again: held, not 402.
    let rows = place_order(
        &payments,
        "run_demo",
        "ord_after",
        "usr_judge",
        subtotal,
        tip,
    )
    .await
    .unwrap();
    assert_eq!(rows[0].amount_cents, total);
    let judge_start = PERSONA_BALANCES
        .iter()
        .find(|(user, _)| *user == "usr_judge")
        .unwrap()
        .1;
    assert_eq!(checking(&pool, "usr_judge").await, judge_start - total);
    assert_eq!(escrow(&pool).await, PLATFORM_SEED_CENTS + total);
    assert_conserved(&pool).await;
}
