//! `GET /v1/stream` (the WebSocket) and `POST /v1/stream/ticket`. See contract/events.md.

use axum::body::Bytes;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code};
use axum::extract::{RawQuery, State};
use axum::response::Response;
use serde::{Deserialize, Serialize};
use tokio::time::{Duration, Instant, MissedTickBehavior};

use super::clock;
use super::hub::{Hub, Registration};
use crate::error::{ApiError, ApiResult};
use crate::extract::Json;
use crate::jwt::AuthUser;
use crate::state::AppState;

/// The largest message a client may send. Clients only send `{"type": "ping"}`.
const MAX_CLIENT_MESSAGE: usize = 4 * 1024;

/// How long the server waits to send its close frame before dropping the socket.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

/// `StreamTicket` in contract/openapi.yaml (v1.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamTicket {
    pub ticket: String,
    pub expires_at: String,
}

/// `POST /v1/stream/ticket`: a single-use ticket for `/v1/stream?ticket=`, valid about 60 s.
pub(crate) async fn create_ticket(State(hub): State<Hub>, user: AuthUser) -> Json<StreamTicket> {
    let (ticket, expires_at) = hub.issue_ticket(&user.user_id);
    Json(StreamTicket {
        ticket,
        expires_at: clock::rfc3339(expires_at),
    })
}

/// `GET /v1/stream?ticket=<ticket>` or `?token=<jwt>`. Bad or missing credentials are a 401 in the
/// contract's envelope before any upgrade. The connection is registered before the 101 goes out,
/// so nothing published after the client sees the socket open is missed.
pub(crate) async fn stream(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> ApiResult<Response> {
    let user_id = authenticate(&state, query.as_deref().unwrap_or_default())?;
    let upgrade = upgrade.map_err(|rejection| {
        ApiError::validation(rejection.body_text()).with_status(rejection.status())
    })?;
    let registration = state.hub.register(&user_id);
    let hub = state.hub.clone();
    Ok(upgrade
        .max_message_size(MAX_CLIENT_MESSAGE)
        .max_frame_size(MAX_CLIENT_MESSAGE)
        .on_upgrade(move |socket| serve(socket, hub, registration)))
}

/// The user behind `?ticket=` (preferred when both are given) or `?token=`. Never logs either.
fn authenticate(state: &AppState, query: &str) -> ApiResult<String> {
    let (mut ticket, mut token) = (None, None);
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "ticket" if ticket.is_none() => ticket = Some(value.into_owned()),
            "token" if token.is_none() => token = Some(value.into_owned()),
            _ => {}
        }
    }
    if let Some(ticket) = ticket.filter(|t| !t.is_empty()) {
        return state.hub.redeem_ticket(&ticket).ok_or_else(|| {
            ApiError::unauthorized(
                "Invalid, expired or already used ticket. Get a new one from POST /v1/stream/ticket.",
            )
        });
    }
    if let Some(token) = token.filter(|t| !t.is_empty()) {
        return Ok(state.jwt.verify(&token)?.sub);
    }
    Err(ApiError::unauthorized(
        "Sign in first: open /v1/stream?ticket=<ticket> or ?token=<jwt>.",
    ))
}

/// One connection: forwards the hub's frames, pings, and closes on idle, overflow or error.
async fn serve(mut socket: WebSocket, hub: Hub, mut registration: Registration) {
    let settings = hub.settings().clone();
    tracing::debug!(user_id = registration.user_id(), "stream: connected");
    let mut ping = tokio::time::interval_at(
        Instant::now() + settings.ping_interval,
        settings.ping_interval,
    );
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let idle = tokio::time::sleep(settings.idle_timeout);
    tokio::pin!(idle);

    let close = loop {
        tokio::select! {
            biased;
            () = registration.kick.notified() => {
                break Some(close_frame(close_code::AGAIN, "Fell behind. Reconnect and refetch."));
            }
            received = socket.recv() => match received {
                // Any frame counts as activity: the client's {"type":"ping"} (no reply), a pong
                // to our ping, or anything else, which is ignored. A close is answered on the
                // next read, which then ends the stream.
                Some(Ok(_)) => idle.as_mut().reset(Instant::now() + settings.idle_timeout),
                Some(Err(_)) | None => break None,
            },
            frame = registration.rx.recv() => match frame {
                Some(text) => {
                    if !write(&mut socket, Message::Text(text), settings.write_timeout).await {
                        break None;
                    }
                }
                None => break None,
            },
            _ = ping.tick() => {
                if !write(&mut socket, Message::Ping(Bytes::new()), settings.write_timeout).await {
                    break None;
                }
            }
            () = &mut idle => break Some(close_frame(close_code::AWAY, "Idle for too long.")),
        }
    };
    if let Some(frame) = close {
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, socket.send(Message::Close(Some(frame)))).await;
    }
    tracing::debug!(user_id = registration.user_id(), "stream: closed");
}

/// Sends one message, giving up after `limit` (a stalled client). False means drop the socket.
async fn write(socket: &mut WebSocket, message: Message, limit: Duration) -> bool {
    matches!(
        tokio::time::timeout(limit, socket.send(message)).await,
        Ok(Ok(()))
    )
}

fn close_frame(code: u16, reason: &'static str) -> CloseFrame {
    CloseFrame {
        code,
        reason: reason.into(),
    }
}
