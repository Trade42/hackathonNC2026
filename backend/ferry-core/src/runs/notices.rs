//! The stored notifications runs and orders send (`ferry.notifications`), written in the change's
//! own transaction and published after it commits.
//!
//! - `run_open` to each other member of the circle when a run opens; data `{run_id, circle_id,
//!   url}` with `url` = `{PUBLIC_WEB_URL}/r/{run_id}`.
//! - `order_update` to the customer at each step of their order (placed, purchased, picked up,
//!   delivered, cancelled, refunded); data `{order_id, run_id, status, url}` with `url` =
//!   `{PUBLIC_WEB_URL}/o/{order_id}`.
//! - `payout` to the runner when an order's fee and tip pay out; data `{order_id, run_id,
//!   amount_cents, url}` with `url` = `{PUBLIC_WEB_URL}/r/{run_id}`.
//!
//! `url` is left out when `PUBLIC_WEB_URL` is unset, as the invites' notifications do.

use serde_json::{Map, Value};
use sqlx::PgConnection;

use crate::config::Config;
use crate::error::ApiResult;
use crate::notifications::{Notification, NotificationType, create_notification};
use crate::orders::OrderStatus;

/// `$16.93`.
pub fn money(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let cents = cents.unsigned_abs();
    format!("{sign}${}.{:02}", cents / 100, cents % 100)
}

/// `name`, or `fallback` when the user has not set one.
pub fn name_or<'a>(name: &'a str, fallback: &'a str) -> &'a str {
    match name.trim() {
        "" => fallback,
        name => name,
    }
}

/// `{PUBLIC_WEB_URL}/{kind}/{id}`, or `None` when it is unset.
pub fn deep_link(config: &Config, kind: &str, id: &str) -> Option<String> {
    let web = config.public_web_url.as_deref()?.trim_end_matches('/');
    Some(format!("{web}/{kind}/{id}"))
}

fn data(pairs: &[(&str, Value)], url: Option<String>) -> Value {
    let mut data: Map<String, Value> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect();
    if let Some(url) = url {
        data.insert("url".into(), Value::from(url));
    }
    Value::Object(data)
}

/// Stores a `run_open` notification for `user_id`.
#[allow(clippy::too_many_arguments)]
pub async fn run_open(
    conn: &mut PgConnection,
    config: &Config,
    user_id: &str,
    run_id: &str,
    circle_id: &str,
    runner: &str,
    restaurant: &str,
    closes_local: &str,
) -> ApiResult<(String, Notification)> {
    let notification = create_notification(
        &mut *conn,
        user_id,
        NotificationType::RunOpen,
        &format!("{} is heading to {restaurant}", name_or(runner, "Someone")),
        &format!("Order by {closes_local}."),
        data(
            &[
                ("run_id", Value::from(run_id)),
                ("circle_id", Value::from(circle_id)),
            ],
            deep_link(config, "r", run_id),
        ),
    )
    .await?;
    Ok((user_id.to_owned(), notification))
}

/// Stores an `order_update` notification for the order's customer.
#[allow(clippy::too_many_arguments)]
pub async fn order_update(
    conn: &mut PgConnection,
    config: &Config,
    customer_id: &str,
    order_id: &str,
    run_id: &str,
    status: OrderStatus,
    title: &str,
    body: &str,
) -> ApiResult<(String, Notification)> {
    let notification = create_notification(
        &mut *conn,
        customer_id,
        NotificationType::OrderUpdate,
        title,
        body,
        data(
            &[
                ("order_id", Value::from(order_id)),
                ("run_id", Value::from(run_id)),
                ("status", Value::from(status.as_str())),
            ],
            deep_link(config, "o", order_id),
        ),
    )
    .await?;
    Ok((customer_id.to_owned(), notification))
}

/// Stores a `payout` notification for the runner.
pub async fn payout(
    conn: &mut PgConnection,
    config: &Config,
    runner_id: &str,
    order_id: &str,
    run_id: &str,
    amount_cents: i64,
    customer: &str,
) -> ApiResult<(String, Notification)> {
    let notification = create_notification(
        &mut *conn,
        runner_id,
        NotificationType::Payout,
        &format!("+{} fee and tip", money(amount_cents)),
        &format!(
            "{} confirmed the handoff, so your fee and tip are in your Ferry wallet.",
            name_or(customer, "Your customer")
        ),
        data(
            &[
                ("order_id", Value::from(order_id)),
                ("run_id", Value::from(run_id)),
                ("amount_cents", Value::from(amount_cents)),
            ],
            deep_link(config, "r", run_id),
        ),
    )
    .await?;
    Ok((runner_id.to_owned(), notification))
}

/// "+480 g CO2 saved vs a car delivery (EPA estimate)", rounded to the gram.
pub fn co2_line(co2_saved_g: f64) -> String {
    format!(
        "+{} g CO2 saved vs a car delivery (EPA estimate)",
        co2_saved_g.max(0.0).round() as i64
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_formats_cents() {
        assert_eq!(money(0), "$0.00");
        assert_eq!(money(5), "$0.05");
        assert_eq!(money(1693), "$16.93");
        assert_eq!(money(100_000), "$1000.00");
        assert_eq!(money(-42), "-$0.42");
    }

    #[test]
    fn names_and_links() {
        assert_eq!(name_or("Maya", "Someone"), "Maya");
        assert_eq!(name_or("  ", "Someone"), "Someone");
        let mut config = crate::config::Config::from_vars(
            &[(
                "DATABASE_URL".to_owned(),
                "postgres://u:p@127.0.0.1/db".to_owned(),
            )]
            .into_iter()
            .collect(),
        )
        .unwrap();
        assert_eq!(deep_link(&config, "r", "run_demo"), None);
        config.public_web_url = Some("https://ferry.example/".into());
        assert_eq!(
            deep_link(&config, "o", "ord_x").as_deref(),
            Some("https://ferry.example/o/ord_x")
        );
        assert_eq!(
            co2_line(479.6),
            "+480 g CO2 saved vs a car delivery (EPA estimate)"
        );
    }
}
