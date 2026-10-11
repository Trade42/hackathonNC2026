//! Web Push (spec section 2, Notifications: P1). `GET /v1/public/push-key` and
//! `POST /v1/push/subscribe`. Every `notification.created` also reaches each of the user's web
//! subscriptions as an encrypted push.
//!
//! The hook is in `Hub::send_to`, which every publisher of `notification.created` goes through
//! (`notify`, invites, runs). Callers add nothing. `app::router` attaches the sender to the hub
//! once, through [`super::router_with_push`]. Then each `notification.created` is offered to a
//! bounded queue, and the worker behind it does the database read and the HTTP (`sender`).
//!
//! What a browser receives is the event's data, `{"notification": <Notification>}`, the shape
//! `frontend/public/sw.js` reads. It is encrypted with RFC 8291 and signed with VAPID (RFC 8292).
//! See `crypto` for both.

mod crypto;
mod endpoint;
mod sender;
mod subscribe;

pub use crypto::{Vapid, VapidError};
pub use endpoint::is_push_service;
pub(crate) use sender::PushQueue;
pub use sender::{DEFAULT_VAPID_SUBJECT, MAX_PAYLOAD, PushSettings};
pub use subscribe::{MAX_PER_PLATFORM, PushKey};
pub(crate) use subscribe::{push_key, subscribe};

use crate::state::AppState;

/// Attaches Web Push to `state.hub` with [`PushSettings::from_config`]. See [`attach_with`].
pub fn attach(state: &AppState) -> bool {
    state
        .hub
        .attach_push(|| sender::start(state, PushSettings::from_config(&state.config)))
}

/// Attaches Web Push to `state.hub` with `settings`, and returns whether push is on for it.
///
/// Only the first call on a hub (and its clones) decides. Later calls change nothing and start no
/// second worker, so building the router more than once on one state is safe. Push stays off
/// without a usable VAPID key pair or outside a Tokio runtime. Subscriptions are stored either way.
pub fn attach_with(state: &AppState, settings: PushSettings) -> bool {
    state.hub.attach_push(|| sender::start(state, settings))
}
