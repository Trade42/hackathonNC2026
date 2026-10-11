//! Shared by the payments_*.rs tests: fixture rows, an order placer that does what the orders
//! task will do, ledger readers, and contract validation against contract/openapi.yaml.
#![allow(dead_code)]

use std::sync::Arc;

use ferry_core::error::ApiError;
use ferry_core::payments::ledger::{self, LedgerEntry, accounts};
use ferry_core::payments::nessie::{MockNessie, Nessie};
use ferry_core::payments::{Payments, math, ops};
use serde_json::{Value, json};
use sqlx::PgPool;

pub const RUNNER: &str = "usr_maya";
pub const RESTAURANT: &str = "rst_cosmic";
pub const CIRCLE: &str = "cir_hinton4";
pub const CUSTOMERS: [&str; 4] = ["usr_judge", "usr_dana", "usr_sam", "usr_eve"];
pub const SEED_CENTS: i64 = 100_000;

/// Payments over `pool` with its own mock Nessie, and that mock.
pub fn payments(pool: &PgPool) -> (Payments, Arc<MockNessie>) {
    let mock = Arc::new(MockNessie::new(0x123));
    let config = crate::common::config(&[]);
    (
        Payments::new(pool.clone(), Arc::new(config), Nessie::Mock(mock.clone())),
        mock,
    )
}

/// An area, Cosmic Cantina, the runner and four customers in one circle.
pub async fn seed_people(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO ferry.areas (id, name, center_lat, center_lng) \
         VALUES ('area_franklin', 'Franklin St', 35.9132, -79.0558)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ferry.restaurants (id, name, area_id, cuisine, lat, lng, address, hours_today, price_level) \
         VALUES ($1, 'Cosmic Cantina', 'area_franklin', '{mexican}', 35.913211, -79.055094, \
                 '118 E Franklin St, Chapel Hill, NC 27514', '11:00–04:00', 1)",
    )
    .bind(RESTAURANT)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ferry.circles (id, name, emoji) VALUES ($1, 'Hinton James 4th floor', '🏠')",
    )
    .bind(CIRCLE)
    .execute(pool)
    .await
    .unwrap();
    for (i, user) in std::iter::once(RUNNER).chain(CUSTOMERS).enumerate() {
        sqlx::query(
            "INSERT INTO ferry.users (id, phone_e164, display_name, avatar_seed, bubble_color) \
             VALUES ($1, $2, $3, $1, '#3366FF')",
        )
        .bind(user)
        .bind(format!("+1919555010{i}"))
        .bind(user.trim_start_matches("usr_"))
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO ferry.circle_members (circle_id, user_id) VALUES ($1, $2)")
            .bind(CIRCLE)
            .bind(user)
            .execute(pool)
            .await
            .unwrap();
    }
}

/// An open run to Cosmic Cantina by the runner.
pub async fn create_run(pool: &PgPool, run_id: &str) {
    sqlx::query(
        "INSERT INTO ferry.runs (id, restaurant_id, runner_id, circle_id, created_via, leaving_at, window_closes_at) \
         VALUES ($1, $2, $3, $4, 'app', now() + interval '20 minutes', now() + interval '10 minutes')",
    )
    .bind(run_id)
    .bind(RESTAURANT)
    .bind(RUNNER)
    .bind(CIRCLE)
    .execute(pool)
    .await
    .unwrap();
}

/// What the orders task does on POST /v1/runs/{id}/orders: in one transaction, lock the run
/// FOR UPDATE, insert the order (priced by `math::quote`), hold it, commit; then post. A failed
/// hold rolls everything back. Returns the order id and its hold rows.
pub async fn place_order(
    payments: &Payments,
    run_id: &str,
    order_id: &str,
    customer: &str,
    subtotal_cents: i64,
    tip_cents: i64,
) -> Result<Vec<LedgerEntry>, ApiError> {
    let q = math::quote_with(
        payments.config(),
        &[math::PricedLine {
            unit_price_cents: subtotal_cents,
            qty: 1,
        }],
        tip_cents,
    )
    .unwrap();
    let mut tx = payments.pool().begin().await.unwrap();
    sqlx::query("SELECT id FROM ferry.runs WHERE id = $1 FOR UPDATE")
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO ferry.orders (id, run_id, customer_id, subtotal_cents, tax_cents, ferry_fee_cents, \
             tip_cents, total_cents, dropoff_label, dropoff_lat, dropoff_lng, handoff_code) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'Hinton James', 35.902364, -79.043231, '4829')",
    )
    .bind(order_id)
    .bind(run_id)
    .bind(customer)
    .bind(q.subtotal_cents)
    .bind(q.tax_cents)
    .bind(q.ferry_fee_cents)
    .bind(q.tip_cents)
    .bind(q.total_cents)
    .execute(&mut *tx)
    .await
    .unwrap();
    let rows = ops::hold_order_in(&mut tx, order_id).await?;
    tx.commit().await.unwrap();
    Ok(payments.post(&rows).await)
}

/// Sets an order's status, as the orders task does beside a money op.
pub async fn set_order_status(pool: &PgPool, order_id: &str, status: &str) {
    sqlx::query(
        "UPDATE ferry.orders SET status = $2, \
             delivered_at = CASE WHEN $2 IN ('delivered', 'settled') THEN now() ELSE delivered_at END \
         WHERE id = $1",
    )
    .bind(order_id)
    .bind(status)
    .execute(pool)
    .await
    .unwrap();
}

pub async fn set_run_status(pool: &PgPool, run_id: &str, status: &str) {
    sqlx::query("UPDATE ferry.runs SET status = $2 WHERE id = $1")
        .bind(run_id)
        .bind(status)
        .execute(pool)
        .await
        .unwrap();
}

pub async fn balance(pool: &PgPool, account: &str) -> i64 {
    let mut conn = pool.acquire().await.unwrap();
    ledger::balance(&mut conn, account).await.unwrap()
}

pub async fn escrow(pool: &PgPool) -> i64 {
    balance(pool, accounts::ESCROW).await
}

pub async fn checking(pool: &PgPool, user: &str) -> i64 {
    balance(pool, &accounts::checking(user)).await
}

pub async fn card(pool: &PgPool) -> i64 {
    balance(pool, &accounts::ferry_card(RUNNER)).await
}

pub async fn merchant(pool: &PgPool) -> i64 {
    balance(pool, &accounts::merchant(RESTAURANT)).await
}

pub async fn row_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM ferry.ledger_entries")
        .fetch_one(pool)
        .await
        .unwrap()
}

pub async fn rows_with_key(pool: &PgPool, key: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM ferry.ledger_entries WHERE idempotency_key = $1")
        .bind(key)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Every account but `external` sums to what came in from outside: money is only moved.
pub async fn assert_conserved(pool: &PgPool) {
    let (inside, deposited): (i64, i64) = sqlx::query_as(
        "WITH moves AS ( \
             SELECT debit_account AS account, amount_cents AS delta FROM ferry.ledger_entries \
             UNION ALL SELECT credit_account, -amount_cents FROM ferry.ledger_entries) \
         SELECT (SELECT COALESCE(SUM(delta), 0) FROM moves WHERE account <> 'external')::bigint, \
                (SELECT COALESCE(SUM(amount_cents), 0) FROM ferry.ledger_entries \
                 WHERE credit_account = 'external')::bigint",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(inside, deposited, "money was created or destroyed");
    let negative: Vec<(String, i64)> = sqlx::query_as(
        "WITH moves AS ( \
             SELECT debit_account AS account, amount_cents AS delta FROM ferry.ledger_entries \
             UNION ALL SELECT credit_account, -amount_cents FROM ferry.ledger_entries) \
         SELECT account, SUM(delta)::bigint FROM moves WHERE account <> 'external' \
         GROUP BY account HAVING SUM(delta) < 0",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert!(negative.is_empty(), "negative balances: {negative:?}");
}

/// A validator for `#/components/schemas/<name>` in contract/openapi.yaml, with formats checked.
pub fn contract_validator(name: &str) -> jsonschema::Validator {
    let yaml = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contract/openapi.yaml"
    ))
    .unwrap();
    let doc: Value = serde_norway::from_str(&yaml).unwrap();
    let schema = json!({
        "$ref": format!("#/components/schemas/{name}"),
        "components": doc["components"],
    });
    jsonschema::options()
        .should_validate_formats(true)
        .build(&schema)
        .unwrap()
}

/// Panics with every error unless `value` is a valid `name`.
pub fn assert_contract(name: &str, value: &Value) {
    let validator = contract_validator(name);
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "{name} does not match the contract: {errors:?}\n{value:#}"
    );
}
