//! Order status changes, each with its `ferry.order_events` row (the order's `timeline`), inside
//! the caller's transaction after it has locked the run.

use serde_json::Value;
use sqlx::PgConnection;

use super::machine::OrderStatus;
use crate::error::ApiResult;

/// An order a step moved.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Moved {
    pub id: String,
    pub customer_id: String,
    pub dropoff_label: String,
    pub total_cents: i64,
}

/// One `order_events` row per order in `order_ids`, at `clock_timestamp()` so two steps in one
/// transaction keep their order.
pub async fn write_events(
    conn: &mut PgConnection,
    run_id: &str,
    order_ids: &[String],
    status: OrderStatus,
    payload: &Value,
) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO ferry.order_events (order_id, run_id, ts, status, payload) \
         SELECT o.id, $2, clock_timestamp(), $3, $4 FROM unnest($1::text[]) AS o(id)",
    )
    .bind(order_ids)
    .bind(run_id)
    .bind(status.as_str())
    .bind(payload)
    .execute(conn)
    .await?;
    Ok(())
}

/// Moves every order of the run in status `from` to `to` (a move the machine must allow), with an
/// event each. Returns them by id.
pub async fn move_orders(
    conn: &mut PgConnection,
    run_id: &str,
    from: OrderStatus,
    to: OrderStatus,
) -> ApiResult<Vec<Moved>> {
    from.transition(to)?;
    let mut moved: Vec<Moved> = sqlx::query_as(
        "UPDATE ferry.orders SET status = $3 WHERE run_id = $1 AND status = $2 \
         RETURNING id, customer_id, dropoff_label, total_cents",
    )
    .bind(run_id)
    .bind(from.as_str())
    .bind(to.as_str())
    .fetch_all(&mut *conn)
    .await?;
    moved.sort_by(|a, b| a.id.cmp(&b.id));
    let ids: Vec<String> = moved.iter().map(|m| m.id.clone()).collect();
    write_events(conn, run_id, &ids, to, &Value::Object(Default::default())).await?;
    Ok(moved)
}

/// Moves one order from `from` to `to` (a move the machine must allow; the caller read `from`
/// under the lock), with its event.
pub async fn move_order(
    conn: &mut PgConnection,
    run_id: &str,
    order_id: &str,
    from: OrderStatus,
    to: OrderStatus,
    payload: &Value,
) -> ApiResult<()> {
    from.transition(to)?;
    sqlx::query("UPDATE ferry.orders SET status = $3 WHERE id = $1 AND status = $2")
        .bind(order_id)
        .bind(from.as_str())
        .bind(to.as_str())
        .execute(&mut *conn)
        .await?;
    write_events(conn, run_id, &[order_id.to_owned()], to, payload).await
}
