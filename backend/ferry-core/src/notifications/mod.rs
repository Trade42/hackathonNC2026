//! Realtime and notifications: the event hub every module publishes to, the WebSocket
//! `/v1/stream` with its ticket (`POST /v1/stream/ticket`), `/v1/notifications`, and Web Push
//! (`/v1/public/push-key`, `/v1/push/subscribe`, and a push for every `notification.created`; see
//! [`push`]).
//!
//! Publishing, from any module (see contract/events.md for who gets which event):
//!
//! ```ignore
//! // One user, one payload:
//! state.hub.send_to(&wallet.user_id, &Event::wallet_updated(&wallet));
//! // Several users, each with their own copy:
//! state.hub.send_to_many(&recipients, |viewer| Event::order_updated(&order_as_seen_by(viewer)));
//! // A stored notification plus its notification.created event:
//! notify(&state, &user_id, NotificationType::RunOpen, "Maya is heading to Cosmic Cantina", "Order by 6:10", json!({"run_id": run.id})).await?;
//! ```

mod clock;
mod hub;
mod notify;
pub mod push;
mod stream;
mod tickets;

use axum::Router;
use axum::routing::{get, post};

use crate::state::AppState;

pub use clock::{now_rfc3339, rfc3339};
pub use hub::{Event, EventType, Hub, HubSettings};
pub use notify::{LIST_LIMIT, Notification, NotificationType, create_notification, notify};
pub use stream::StreamTicket;
pub use tickets::MAX_TICKETS_PER_USER;

/// This module's routes, with full `/v1/...` paths.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/stream", get(stream::stream))
        .route("/v1/stream/ticket", post(stream::create_ticket))
        .route("/v1/notifications", get(notify::list))
        .route("/v1/notifications/{id}/read", post(notify::mark_read))
        .route("/v1/public/push-key", get(push::push_key))
        .route("/v1/push/subscribe", post(push::subscribe))
}

/// [`router`], after attaching Web Push to `state.hub` ([`push::attach`]). `app::router` merges
/// this. Attaching happens once per hub, so building the app again on the same state starts
/// nothing new.
pub fn router_with_push(state: &AppState) -> Router<AppState> {
    push::attach(state);
    router()
}
