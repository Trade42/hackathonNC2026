//! Calls from core to the Spectrum sidecar's internal API (messaging/README.md, Internal API):
//! `POST {SIDECAR_URL}/send` into a Telegram chat, with inline buttons, and
//! `POST {SIDECAR_URL}/invite-links` for a group's invite link.
//!
//! Inbound replies (`{replies: [{space_id, text}]}`) carry text only, so a message with buttons
//! goes out through `/send`. [`spawn_send`] sends after the inbound answer, so the sidecar's 3 s
//! wait for core never includes Telegram's time.

use std::time::Duration;

use serde::Serialize;

use super::mask::mask_space_id;
use crate::config::Config;
use crate::state::AppState;

/// How long core waits for `POST /send`: the sidecar gives Telegram 10 s, then answers 502.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(12);

/// The header every internal route checks against `SIDECAR_SHARED_SECRET`, both ways.
pub const SECRET_HEADER: &str = "X-Ferry-Secret";

/// One inline button: a link (`url`) or a button whose press comes back to core as a
/// `kind: "callback"` event carrying its `callback_data` (1 to 64 bytes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Button {
    Url {
        label: String,
        url: String,
    },
    Callback {
        label: String,
        callback_data: String,
    },
}

impl Button {
    pub fn url(label: &str, url: &str) -> Self {
        Button::Url {
            label: label.to_owned(),
            url: url.to_owned(),
        }
    }

    pub fn callback(label: &str, callback_data: String) -> Self {
        debug_assert!((1..=64).contains(&callback_data.len()));
        Button::Callback {
            label: label.to_owned(),
            callback_data,
        }
    }
}

/// The body of `POST /send` to a chat by its id (a Telegram DM's is the user's id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SendRequest {
    pub space_id: String,
    pub text: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub buttons: Vec<Button>,
}

#[derive(Debug, thiserror::Error)]
pub enum SidecarError {
    /// `SIDECAR_URL` or `SIDECAR_SHARED_SECRET` is unset, so nothing was sent.
    #[error("the sidecar is not configured (SIDECAR_URL and SIDECAR_SHARED_SECRET)")]
    NotConfigured,
    /// No answer: connection refused, a timeout, or a broken response.
    #[error("the sidecar did not answer: {0}")]
    Unreachable(#[source] reqwest::Error),
    /// The sidecar answered with a status other than 2xx (its codes are in messaging/README.md).
    #[error("the sidecar answered {0}")]
    Status(reqwest::StatusCode),
}

impl SidecarError {
    /// The sidecar's 403: Telegram refused the chat (`recipient_not_reachable` on `/send`: the
    /// user never pressed Start or blocked the bot, or the bot left the group; `not_admin` on
    /// `/invite-links`).
    pub fn is_forbidden(&self) -> bool {
        matches!(self, SidecarError::Status(status) if *status == reqwest::StatusCode::FORBIDDEN)
    }
}

/// Sends `request` through the sidecar. Waits at most [`SEND_TIMEOUT`] and never retries, since a
/// send that timed out may still be delivered. Logs only the masked space id, never the text.
pub async fn send(
    http: &reqwest::Client,
    config: &Config,
    request: &SendRequest,
) -> Result<(), SidecarError> {
    let space = mask_space_id(&request.space_id);
    let (Some(base), Some(secret)) = (&config.sidecar_url, &config.sidecar_shared_secret) else {
        tracing::warn!(
            %space,
            "sidecar send skipped: SIDECAR_URL or SIDECAR_SHARED_SECRET is unset"
        );
        return Err(SidecarError::NotConfigured);
    };
    let result = http
        .post(format!("{base}/send"))
        .header(SECRET_HEADER, secret.expose())
        .timeout(SEND_TIMEOUT)
        .json(request)
        .send()
        .await;
    match result {
        Ok(response) if response.status().is_success() => Ok(()),
        Ok(response) => {
            let status = response.status();
            tracing::warn!(%space, %status, "sidecar send refused");
            Err(SidecarError::Status(status))
        }
        Err(err) => {
            // The error names the URL (SIDECAR_URL, not a secret), never the headers.
            tracing::warn!(
                %space,
                timeout = err.is_timeout(),
                connect = err.is_connect(),
                "sidecar send failed: {err}"
            );
            Err(SidecarError::Unreachable(err))
        }
    }
}

/// How long core waits for `POST /invite-links` (the sidecar gives Telegram 10 s).
pub const INVITE_LINK_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(serde::Deserialize)]
struct InviteLink {
    url: String,
}

/// A new invite link to the group `space_id` (`https://t.me/+...`), which the sidecar can make
/// only where the bot is an admin with the right to invite users (403 otherwise). No retry.
pub async fn invite_link(
    http: &reqwest::Client,
    config: &Config,
    space_id: &str,
) -> Result<String, SidecarError> {
    let (Some(base), Some(secret)) = (&config.sidecar_url, &config.sidecar_shared_secret) else {
        return Err(SidecarError::NotConfigured);
    };
    let response = http
        .post(format!("{base}/invite-links"))
        .header(SECRET_HEADER, secret.expose())
        .timeout(INVITE_LINK_TIMEOUT)
        .json(&serde_json::json!({"space_id": space_id, "name": "Ferry"}))
        .send()
        .await
        .map_err(SidecarError::Unreachable)?;
    let status = response.status();
    if !status.is_success() {
        tracing::info!(space = %mask_space_id(space_id), %status, "sidecar gave no invite link");
        return Err(SidecarError::Status(status));
    }
    let link: InviteLink = response.json().await.map_err(SidecarError::Unreachable)?;
    Ok(link.url)
}

/// [`send`] on its own task, so the caller answers at once. A failure is logged by [`send`].
pub fn spawn_send(state: &AppState, request: SendRequest) {
    let http = state.http.clone();
    let config = state.config.clone();
    tokio::spawn(async move {
        let _ = send(&http, &config, &request).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_body_is_the_sidecars() {
        let request = SendRequest {
            space_id: "987654321".into(),
            text: "Hi".into(),
            buttons: vec![
                Button::url("Open Ferry", "https://ferry.example"),
                Button::callback("Confirm", "signin:abc:confirm".into()),
            ],
        };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({"space_id": "987654321", "text": "Hi", "buttons": [
                {"label": "Open Ferry", "url": "https://ferry.example"},
                {"label": "Confirm", "callback_data": "signin:abc:confirm"}
            ]})
        );
        let plain = SendRequest {
            space_id: "1".into(),
            text: "x".into(),
            buttons: vec![],
        };
        assert_eq!(
            serde_json::to_value(&plain).unwrap(),
            json!({"space_id": "1", "text": "x"})
        );
    }
}
