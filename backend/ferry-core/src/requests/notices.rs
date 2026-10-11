//! The notifications pickup requests send, written in the matching or expiring transaction and
//! published after it commits. The types are the contract's (`NotificationType`); no new one.
//!
//! - `run_open` to the customer when their request matches: "Maya is heading to Cosmic Cantina",
//!   "Your request fits their run. Order by 6:10 PM.", data `{run_id, circle_id, request_id,
//!   url}` with `url` = `{PUBLIC_WEB_URL}/r/{run_id}`, the same shape as runs' own `run_open`.
//! - `system` to that run's runner: "Someone near your route wants Cosmic Cantina", "About a
//!   minute out of your way. They can order until 6:10 PM.", data `{run_id, url}`. No name or id
//!   of the customer: the runner sees who ordered once they order, as with any order. Once per
//!   customer and run: when the same customer already has a `matched` request on the run (a
//!   double tap), only the customer is told.
//! - `system` to the customer when the request expires: "No run to Sup Dogs in time", data
//!   `{request_id, restaurant_id}`.
//! - `system` to the runner of a nearby run that fits (no run to the request's own restaurant
//!   did): "Someone near your route wants Sup Dogs, a minute from Cosmic Cantina", "Pick it up?
//!   Open a Ferry to Sup Dogs too and they can order on it. About a minute out of your way.", data
//!   `{run_id, request_id, new_run_restaurant_id, url}` with `url` =
//!   `{PUBLIC_WEB_URL}/run/new?restaurant={restaurant_id}`, the new-run screen with the request's
//!   restaurant picked. No name or id of the customer. Once per customer, restaurant and run, so
//!   neither the sweep nor a double tap repeats it. The customer is not told.
//!
//! `url` is left out when `PUBLIC_WEB_URL` is unset, as runs' notifications do.

use serde_json::{Map, Value};
use sqlx::PgConnection;

use super::matcher::{Candidate, OpenRequest, Score};
use crate::catalog::walk::walk_minutes;
use crate::config::Config;
use crate::error::ApiResult;
use crate::notifications::{Notification, NotificationType, create_notification};
use crate::runs::notices::{deep_link, name_or};

fn data(pairs: &[(&str, &str)], url: Option<String>) -> Value {
    let mut data: Map<String, Value> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), Value::from(*v)))
        .collect();
    if let Some(url) = url {
        data.insert("url".into(), Value::from(url));
    }
    Value::Object(data)
}

/// "About a minute" under 1.5 minutes, else "About N minutes", N rounded to the nearest minute.
pub fn detour_phrase(extra_minutes: f64) -> String {
    let minutes = extra_minutes.round().max(1.0) as i64;
    if minutes == 1 {
        "About a minute".to_owned()
    } else {
        format!("About {minutes} minutes")
    }
}

/// Stores the customer's `run_open` for a match.
pub async fn matched(
    conn: &mut PgConnection,
    config: &Config,
    request: &OpenRequest,
    run: &Candidate,
) -> ApiResult<Notification> {
    create_notification(
        &mut *conn,
        &request.customer_id,
        NotificationType::RunOpen,
        &format!(
            "{} is heading to {}",
            name_or(&run.runner_name, "Someone"),
            run.restaurant_name
        ),
        &format!(
            "Your request fits their run. Order by {}.",
            run.closes_local
        ),
        data(
            &[
                ("run_id", &run.run_id),
                ("circle_id", &run.circle_id),
                ("request_id", &request.id),
            ],
            deep_link(config, "r", &run.run_id),
        ),
    )
    .await
}

/// Advisory lock class for one customer's suggestions on one run ("FREQ").
const SUGGESTION_LOCK_CLASS: i32 = 0x4652_4551;

/// Stores the runner's `system` suggestion for a match of `request`, in the claim's transaction
/// (`request` is already `matched` there). `None`, and nothing stored, when the same customer
/// already has another request matched to this run: the runner was told then.
pub async fn suggested(
    conn: &mut PgConnection,
    config: &Config,
    request: &OpenRequest,
    run: &Candidate,
    score: &Score,
) -> ApiResult<Option<Notification>> {
    // Two of this customer's requests matching this run at once serialize here, so the second's
    // check (a statement of its own, under READ COMMITTED) sees the first's committed match.
    sqlx::query("SELECT pg_advisory_xact_lock($1, hashtext($2 || ' ' || $3))")
        .bind(SUGGESTION_LOCK_CLASS)
        .bind(&request.customer_id)
        .bind(&run.run_id)
        .execute(&mut *conn)
        .await?;
    let already: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM ferry.pickup_requests \
                        WHERE customer_id = $1 AND run_id = $2 AND status = 'matched' \
                          AND id <> $3)",
    )
    .bind(&request.customer_id)
    .bind(&run.run_id)
    .bind(&request.id)
    .fetch_one(&mut *conn)
    .await?;
    if already {
        return Ok(None);
    }
    create_notification(
        &mut *conn,
        &run.runner_id,
        NotificationType::System,
        &format!("Someone near your route wants {}", run.restaurant_name),
        &format!(
            "{} out of your way. They can order until {}.",
            detour_phrase(score.extra_minutes),
            run.closes_local
        ),
        data(
            &[("run_id", &run.run_id)],
            deep_link(config, "r", &run.run_id),
        ),
    )
    .await
    .map(Some)
}

/// `{PUBLIC_WEB_URL}/run/new?restaurant={restaurant_id}`, or `None` when it is unset.
pub fn new_run_link(config: &Config, restaurant_id: &str) -> Option<String> {
    let web = config.public_web_url.as_deref()?.trim_end_matches('/');
    Some(format!("{web}/run/new?restaurant={restaurant_id}"))
}

/// "a minute", else "N minutes".
fn minutes_phrase(minutes: u32) -> String {
    if minutes <= 1 {
        "a minute".to_owned()
    } else {
        format!("{minutes} minutes")
    }
}

/// Advisory lock class for one customer's nearby suggestions on one run ("FNEA").
const NEARBY_LOCK_CLASS: i32 = 0x464E_4541;

/// Stores the runner's `system` suggestion to open a Ferry to `request`'s restaurant too, for a
/// nearby `run` that fits it, in the caller's transaction. `None`, and nothing stored, when the
/// runner was already suggested this customer's request for that restaurant on this run.
pub async fn nearby(
    conn: &mut PgConnection,
    config: &Config,
    request: &OpenRequest,
    run: &Candidate,
    score: &Score,
) -> ApiResult<Option<Notification>> {
    // The at-once match and the sweep (or a double tap) serialize here, so the second's check, a
    // statement of its own under READ COMMITTED, sees the first's committed notice.
    sqlx::query("SELECT pg_advisory_xact_lock($1, hashtext($2 || ' ' || $3 || ' ' || $4))")
        .bind(NEARBY_LOCK_CLASS)
        .bind(&request.customer_id)
        .bind(&request.restaurant_id)
        .bind(&run.run_id)
        .execute(&mut *conn)
        .await?;
    let already: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM ferry.notifications n \
                        JOIN ferry.pickup_requests q ON q.id = n.data->>'request_id' \
                        WHERE n.user_id = $1 AND n.type = 'system' \
                          AND n.data->>'run_id' = $2 \
                          AND n.data->>'new_run_restaurant_id' = $3 \
                          AND q.customer_id = $4)",
    )
    .bind(&run.runner_id)
    .bind(&run.run_id)
    .bind(&request.restaurant_id)
    .bind(&request.customer_id)
    .fetch_one(&mut *conn)
    .await?;
    if already {
        return Ok(None);
    }
    let walk = walk_minutes(run.restaurant, request.restaurant(), run.pace);
    create_notification(
        &mut *conn,
        &run.runner_id,
        NotificationType::System,
        &format!(
            "Someone near your route wants {}, {} from {}",
            request.restaurant_name,
            minutes_phrase(walk),
            run.restaurant_name
        ),
        &format!(
            "Pick it up? Open a Ferry to {} too and they can order on it. {} out of your way.",
            request.restaurant_name,
            detour_phrase(score.extra_minutes)
        ),
        data(
            &[
                ("run_id", &run.run_id),
                ("request_id", &request.id),
                ("new_run_restaurant_id", &request.restaurant_id),
            ],
            new_run_link(config, &request.restaurant_id),
        ),
    )
    .await
    .map(Some)
}

/// Stores the customer's `system` notice that their request expired.
pub async fn expired(
    conn: &mut PgConnection,
    customer_id: &str,
    request_id: &str,
    restaurant_id: &str,
    restaurant_name: &str,
) -> ApiResult<Notification> {
    create_notification(
        &mut *conn,
        customer_id,
        NotificationType::System,
        &format!("No run to {restaurant_name} in time"),
        "Nobody in your circles headed there before your deadline. Nothing was charged.",
        data(
            &[("request_id", request_id), ("restaurant_id", restaurant_id)],
            None,
        ),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minutes_phrases() {
        assert_eq!(minutes_phrase(0), "a minute");
        assert_eq!(minutes_phrase(1), "a minute");
        assert_eq!(minutes_phrase(4), "4 minutes");
    }

    #[test]
    fn detour_phrases() {
        assert_eq!(detour_phrase(0.0), "About a minute");
        assert_eq!(detour_phrase(0.4), "About a minute");
        assert_eq!(detour_phrase(1.05), "About a minute");
        assert_eq!(detour_phrase(1.5), "About 2 minutes");
        assert_eq!(detour_phrase(3.9), "About 4 minutes");
        assert_eq!(detour_phrase(4.0), "About 4 minutes");
    }
}
