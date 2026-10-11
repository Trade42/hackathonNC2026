//! The agent: Ferry's Gemini tool loop in Telegram groups and DMs (spec section 3, Sal's
//! REQUEST-telegram.md). Trip detection, ETA and status answers, menus, the wallet, impact and
//! meet-again, with groups mapped to circles.
//!
//! - `messaging::inbound` hands every Telegram event that is not sign-in to [`dispatch`], which
//!   decides at once whether the bot speaks and answers the sidecar `{replies: []}`. The turn runs
//!   on its own task and sends through the sidecar's `POST /send`, so a slow turn never blocks
//!   the sidecar or makes it retry.
//! - [`privacy`] is the only way anything reaches [`gemini`]: names, usernames, phones, emails and
//!   addresses never do. People are placeholders that code fills in after Gemini answers.
//! - Facts, totals, ETAs, permissions and money come from the tools' code; Gemini picks tools and
//!   phrases replies. Without Gemini (no key, an outage, a refused payload) the same tools answer
//!   through the deterministic fallback.

use axum::Router;

use crate::state::AppState;

pub mod announce;
pub mod breaker;
pub mod dispatch;
pub mod fallback;
pub mod gemini;
pub mod log;
pub mod meet;
pub mod order_text;
pub mod ordering;
pub mod patterns;
pub mod privacy;
pub mod spaces;
pub mod tools;
pub mod trips;
pub mod turn;

/// This module's routes, with full `/v1/...` paths. Merged into the app by `app::router`.
pub fn router() -> Router<AppState> {
    Router::new()
}
