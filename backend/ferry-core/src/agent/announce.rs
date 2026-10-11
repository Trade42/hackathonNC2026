//! Run announcements: when a run the agent opened changes state, the chat it was announced in
//! hears it once (spec section 3: "when a run it manages changes state", one short message per
//! event).
//!
//! The runner moves a run along in the app (`POST /v1/runs/{id}/close`, `/purchased`,
//! `/picked-up`, `/cancel`, and the last handoff makes it delivered), so a sweep every
//! [`EVERY`] compares each agent-opened run's status with the one last announced
//! (`trip_intents.announced_status`) and claims the change with a conditional update before it
//! speaks, so two servers on one database never announce it twice. A run that moves twice
//! between sweeps is announced at its latest status. Only runs of the last day are watched.
//!
//! The sweep starts with the first Telegram event a process handles (`ensure_started`), once per
//! database, and stops when the pool closes.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use super::dispatch::deliver;
use crate::error::ApiResult;
use crate::messaging::sidecar::SendRequest;
use crate::state::AppState;

/// How often runs are checked.
pub const EVERY: Duration = Duration::from_secs(2);

/// The line for a run that reached `status`; None for a status that is not announced (`open`:
/// the trip's reply already said it).
pub fn line(status: &str, restaurant: &str, runner: &str, orders: i64) -> Option<String> {
    let runner = match runner.trim() {
        "" => "The runner",
        name => name,
    };
    let orders = match orders {
        1 => "1 order".to_owned(),
        n => format!("{n} orders"),
    };
    Some(match status {
        "closed" => format!("Orders for the Ferry to {restaurant} are closed: {orders}."),
        "purchased" => format!("{runner} paid at {restaurant}."),
        "delivering" => format!("{runner} picked up the food at {restaurant} and is walking back."),
        "delivered" => format!("Every order from the Ferry to {restaurant} is delivered."),
        "cancelled" => format!("The Ferry to {restaurant} was cancelled."),
        _ => return None,
    })
}

/// Starts the sweep for this state's database, unless it runs already.
pub fn ensure_started(state: &AppState) {
    static STARTED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let options = state.pool.connect_options();
    let key = format!(
        "{}:{}/{}",
        options.get_host(),
        options.get_port(),
        options.get_database().unwrap_or_default()
    );
    let mut started = STARTED
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !started.insert(key) {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(EVERY);
        loop {
            tick.tick().await;
            if state.pool.is_closed() {
                return;
            }
            if let Err(err) = sweep(&state).await {
                tracing::debug!(code = ?err.code, "run announcement sweep failed");
            }
        }
    });
}

#[derive(sqlx::FromRow)]
struct Change {
    intent_id: i64,
    space_id: String,
    status: String,
    restaurant: String,
    runner: String,
    orders: i64,
}

/// One pass: announces each agent-opened run whose status changed since it was last announced.
/// Returns how many lines it sent.
pub async fn sweep(state: &AppState) -> ApiResult<usize> {
    let changes: Vec<Change> = sqlx::query_as(
        "SELECT DISTINCT ON (t.run_id)
                t.id AS intent_id, t.space_id, r.status, rest.name AS restaurant,
                u.display_name AS runner,
                (SELECT count(*) FROM ferry.orders o
                  WHERE o.run_id = r.id AND o.status NOT IN ('cancelled', 'refunded')) AS orders
           FROM ferry.trip_intents t
           JOIN ferry.runs r ON r.id = t.run_id
           JOIN ferry.restaurants rest ON rest.id = r.restaurant_id
           JOIN ferry.users u ON u.id = r.runner_id
          WHERE t.status = 'opened' AND r.status <> 'open'
            AND t.announced_status IS DISTINCT FROM r.status
            AND t.created_at > now() - interval '1 day'
          ORDER BY t.run_id, t.created_at DESC, t.id DESC",
    )
    .fetch_all(&state.pool)
    .await?;
    let mut sent = 0;
    for change in changes {
        let claimed = sqlx::query(
            "UPDATE ferry.trip_intents SET announced_status = $2
              WHERE id = $1 AND announced_status IS DISTINCT FROM $2",
        )
        .bind(change.intent_id)
        .bind(&change.status)
        .execute(&state.pool)
        .await?
        .rows_affected();
        if claimed == 0 {
            continue;
        }
        let Some(text) = line(
            &change.status,
            &change.restaurant,
            &change.runner,
            change.orders,
        ) else {
            continue;
        };
        tracing::info!(
            intent = change.intent_id,
            status = change.status,
            "run change announced"
        );
        deliver(
            state,
            &[SendRequest {
                space_id: change.space_id,
                text,
                buttons: Vec::new(),
            }],
        )
        .await;
        sent += 1;
    }
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines() {
        assert_eq!(
            line("closed", "Cosmic Cantina", "Maya", 2).as_deref(),
            Some("Orders for the Ferry to Cosmic Cantina are closed: 2 orders.")
        );
        assert_eq!(
            line("closed", "Sup Dogs", "Maya", 1).as_deref(),
            Some("Orders for the Ferry to Sup Dogs are closed: 1 order.")
        );
        assert_eq!(
            line("purchased", "Cosmic Cantina", "", 1).as_deref(),
            Some("The runner paid at Cosmic Cantina.")
        );
        assert_eq!(line("open", "Cosmic Cantina", "Maya", 0), None);
        assert!(
            line("delivering", "X", "Maya", 1)
                .unwrap()
                .contains("walking back")
        );
        assert!(
            line("cancelled", "X", "Maya", 1)
                .unwrap()
                .contains("cancelled")
        );
        assert!(
            line("delivered", "X", "Maya", 1)
                .unwrap()
                .contains("delivered")
        );
    }
}
