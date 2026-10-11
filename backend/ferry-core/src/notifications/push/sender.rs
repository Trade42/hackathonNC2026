//! Delivery. The hub offers each `notification.created` to a bounded queue without waiting; one
//! worker per hub looks up the user's web subscriptions and POSTs an encrypted copy to each, at
//! most a few at a time, each with a short timeout and at most one retry. Nothing here can slow
//! or fail the API request that created the notification.
//!
//! Logs name the push service's host, the status and the subscription's row id: never an endpoint
//! (it is a capability), a key or a payload.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::http::{StatusCode, header};
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::Semaphore;
use tokio::sync::mpsc::{self, error::TrySendError};
use url::Origin;

use super::crypto::{self, Vapid, VapidError};
use super::endpoint;
use crate::config::Config;
use crate::state::AppState;
use crate::test_support::is_local_database_url;

/// The largest push payload, in bytes of JSON before encryption (the brief's 3 KB; RFC 8291 allows
/// 3993 in one 4096-byte record).
pub const MAX_PAYLOAD: usize = 3072;
/// `sub` in the VAPID token when PUBLIC_WEB_URL is not an https URL: the web app's domain.
pub const DEFAULT_VAPID_SUBJECT: &str = "https://ferrygo.tech";
const ELLIPSIS: &str = "…";

/// Delivery limits and timings. [`PushSettings::from_config`] is what the app uses; tests shorten
/// the times.
#[derive(Debug, Clone)]
pub struct PushSettings {
    /// One push request's whole time, connecting included. Default 5 s.
    pub timeout: Duration,
    /// The wait before the one retry after a 5xx or a 429. Default 1 s.
    pub retry_delay: Duration,
    /// Notifications waiting for the worker. When it is full, more are dropped with one WARN.
    /// Default 256.
    pub queue: usize,
    /// Push requests in flight at once. Default 16.
    pub concurrency: usize,
    /// How long a push service keeps a message for an offline device (the `TTL` header). Default
    /// 4 hours: a run's order window or an invite is stale after that.
    pub ttl: Duration,
    /// The test stub's origin, from `PUSH_TEST_STUB_ORIGIN` through [`PushSettings::from_config`]
    /// only. Private, so nothing else can set it.
    stub: Option<Origin>,
}

impl Default for PushSettings {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            retry_delay: Duration::from_secs(1),
            queue: 256,
            concurrency: 16,
            ttl: Duration::from_secs(4 * 60 * 60),
            stub: None,
        }
    }
}

impl PushSettings {
    /// The defaults, plus the test stub when `PUSH_TEST_STUB_ORIGIN` is set and allowed.
    ///
    /// The stub is honored only when DATABASE_URL is a local database (127.0.0.1, localhost or
    /// ::1, as `test_support::is_local_database_url` decides) and the value is an http(s) origin
    /// on this machine. Production's DATABASE_URL is the Supabase pooler, so there the variable is
    /// ignored with a WARN, whatever its value.
    pub fn from_config(config: &Config) -> Self {
        Self {
            stub: test_stub(config),
            ..Self::default()
        }
    }

    pub fn stub(&self) -> Option<&Origin> {
        self.stub.as_ref()
    }
}

fn test_stub(config: &Config) -> Option<Origin> {
    let value = config.push_test_stub_origin.as_deref()?;
    if !is_local_database_url(config.database_url.expose()) {
        tracing::warn!("PUSH_TEST_STUB_ORIGIN is ignored: DATABASE_URL is not a local database");
        return None;
    }
    let origin = endpoint::loopback_origin(value);
    if origin.is_none() {
        tracing::warn!(
            "PUSH_TEST_STUB_ORIGIN is ignored: it must be an http origin on 127.0.0.1, [::1] or localhost"
        );
    }
    origin
}

/// The hub's end of the queue.
pub(crate) struct PushQueue {
    tx: mpsc::Sender<Job>,
    /// Set while the queue is full, so a burst of drops logs one WARN.
    full: AtomicBool,
    stub: Option<Origin>,
}

impl PushQueue {
    /// Hands a `notification.created` payload (`{"notification": …}`) for `user_id` to the worker.
    /// Never waits: when the queue is full the push is dropped.
    pub(crate) fn offer(&self, user_id: &str, data: &Value) {
        let job = Job {
            user_id: user_id.to_owned(),
            data: data.clone(),
        };
        match self.tx.try_send(job) {
            Ok(()) => self.full.store(false, Ordering::Relaxed),
            Err(TrySendError::Full(_)) => {
                if !self.full.swap(true, Ordering::Relaxed) {
                    tracing::warn!("web push: the queue is full; dropping pushes until it drains");
                }
            }
            // The worker ended with its runtime.
            Err(TrySendError::Closed(_)) => {}
        }
    }

    /// The test stub's origin, which subscribe accepts as an endpoint too.
    pub(crate) fn stub(&self) -> Option<&Origin> {
        self.stub.as_ref()
    }
}

struct Job {
    user_id: String,
    data: Value,
}

/// Starts the worker and returns the queue to it, or None when push is off: no VAPID key pair, no
/// Tokio runtime, or no HTTP client. Says which in one log line.
pub(super) fn start(state: &AppState, settings: PushSettings) -> Option<PushQueue> {
    let vapid = match Vapid::from_config(&state.config) {
        Ok(vapid) => vapid,
        Err(VapidError::Unset) => {
            tracing::info!("web push: off (VAPID_PUBLIC_KEY and VAPID_PRIVATE_KEY are unset)");
            return None;
        }
        Err(VapidError::Invalid(reason)) => {
            tracing::error!("web push: off ({reason})");
            return None;
        }
    };
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        tracing::warn!("web push: off (no Tokio runtime to run the sender on)");
        return None;
    };
    let http = match client(&settings) {
        Ok(http) => http,
        Err(err) => {
            tracing::error!(error = %err, "web push: off (cannot build its HTTP client)");
            return None;
        }
    };
    let (tx, rx) = mpsc::channel(settings.queue.max(1));
    let stub = settings.stub.clone();
    if let Some(stub) = &stub {
        tracing::warn!(
            origin = %stub.ascii_serialization(),
            "web push: PUSH_TEST_STUB_ORIGIN is honored (local database), so pushes may also go to this origin"
        );
    }
    let worker = Arc::new(Worker {
        pool: state.pool.clone(),
        http,
        subject: vapid_subject(&state.config),
        vapid,
        slots: Arc::new(Semaphore::new(settings.concurrency.max(1))),
        settings,
    });
    runtime.spawn(run(rx, worker));
    tracing::info!("web push: on");
    Some(PushQueue {
        tx,
        full: AtomicBool::new(false),
        stub,
    })
}

/// A client of its own: no redirects (a push service's redirect could point anywhere) and the
/// short timeout.
fn client(settings: &PushSettings) -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(settings.timeout)
        .timeout(settings.timeout)
        .user_agent(concat!("ferry-core/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// The VAPID `sub`: PUBLIC_WEB_URL when it is https, else [`DEFAULT_VAPID_SUBJECT`]. Apple
/// refuses a `sub` that is not https or mailto.
fn vapid_subject(config: &Config) -> String {
    config
        .public_web_url
        .as_deref()
        .filter(|url| url.starts_with("https://"))
        .unwrap_or(DEFAULT_VAPID_SUBJECT)
        .to_owned()
}

struct Worker {
    pool: PgPool,
    http: reqwest::Client,
    vapid: Vapid,
    subject: String,
    settings: PushSettings,
    /// One permit per push request in flight.
    slots: Arc<Semaphore>,
}

#[derive(sqlx::FromRow)]
struct Subscription {
    id: i64,
    endpoint: String,
    p256dh: Option<String>,
    auth: Option<String>,
}

async fn run(mut rx: mpsc::Receiver<Job>, worker: Arc<Worker>) {
    while let Some(job) = rx.recv().await {
        let Some(payload) = payload(&job.data) else {
            tracing::warn!("web push: a notification does not fit in a push; not sent");
            continue;
        };
        let subscriptions = match web_subscriptions(&worker.pool, &job.user_id).await {
            Ok(subscriptions) => subscriptions,
            Err(err) => {
                tracing::warn!(error = %err, "web push: cannot read the subscriptions; not sent");
                continue;
            }
        };
        let payload: Arc<[u8]> = payload.into();
        for subscription in subscriptions {
            let Ok(permit) = worker.slots.clone().acquire_owned().await else {
                return;
            };
            let worker = worker.clone();
            let payload = payload.clone();
            tokio::spawn(async move {
                worker.deliver(&subscription, &payload).await;
                drop(permit);
            });
        }
    }
}

/// The user's web subscriptions that have not expired. One query, on the user_id index.
async fn web_subscriptions(pool: &PgPool, user_id: &str) -> sqlx::Result<Vec<Subscription>> {
    sqlx::query_as::<_, Subscription>(
        "SELECT id, endpoint, p256dh, auth FROM ferry.push_subscriptions \
         WHERE user_id = $1 AND platform = 'web' \
         AND (expiration_time IS NULL OR expiration_time > now()) \
         ORDER BY id",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// `data` as JSON of at most [`MAX_PAYLOAD`] bytes. Too long, the notification's body and then
/// its title are shortened, ending in an ellipsis. None if it still does not fit.
pub(crate) fn payload(data: &Value) -> Option<Vec<u8>> {
    let mut data = data.clone();
    for _ in 0..8 {
        let bytes = serde_json::to_vec(&data).ok()?;
        if bytes.len() <= MAX_PAYLOAD {
            return Some(bytes);
        }
        let excess = bytes.len() - MAX_PAYLOAD;
        let notification = data.get_mut("notification")?.as_object_mut()?;
        let field = ["body", "title"].into_iter().find(|field| {
            notification
                .get(*field)
                .and_then(Value::as_str)
                .is_some_and(|text| !text.is_empty())
        })?;
        let text = notification[field].as_str().unwrap_or_default();
        let shorter = shorten(text, excess);
        notification.insert(field.to_owned(), Value::from(shorter));
    }
    None
}

/// `text` at least `excess` bytes shorter, cut at a character boundary and ending in an
/// ellipsis; empty when that leaves nothing.
fn shorten(text: &str, excess: usize) -> String {
    let Some(mut end) = text.len().checked_sub(excess + ELLIPSIS.len()) else {
        return String::new();
    };
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if end == 0 {
        return String::new();
    }
    format!("{}{ELLIPSIS}", &text[..end])
}

impl Worker {
    async fn deliver(&self, subscription: &Subscription, payload: &[u8]) {
        let id = subscription.id;
        let url = match endpoint::check(&subscription.endpoint, self.settings.stub.as_ref()) {
            Ok(url) => url,
            Err(_) => {
                tracing::warn!(
                    subscription = id,
                    "web push: a stored endpoint is not an allowed push service; not sent"
                );
                return;
            }
        };
        let host = url.host_str().unwrap_or_default().to_owned();
        let Some(body) = encrypt(subscription, payload) else {
            tracing::warn!(%host, subscription = id, "web push: the subscription's keys are unusable; not sent");
            return;
        };
        let audience = url.origin().ascii_serialization();
        for attempt in 1..=2 {
            let Ok(authorization) = self
                .vapid
                .authorization(&audience, &self.subject, unix_now())
            else {
                tracing::error!(%host, "web push: cannot sign the VAPID token; not sent");
                return;
            };
            let sent = self
                .http
                .post(url.clone())
                .header(header::AUTHORIZATION, authorization)
                .header(header::CONTENT_ENCODING, "aes128gcm")
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .header("TTL", self.settings.ttl.as_secs().to_string())
                .header("Urgency", "high")
                .body(body.clone())
                .send()
                .await;
            // A reqwest error's text includes the URL, so only its kind is logged.
            let status = match sent {
                Ok(response) => response.status(),
                Err(err) => {
                    tracing::warn!(%host, subscription = id, error = failure(&err), "web push: not delivered");
                    return;
                }
            };
            let code = status.as_u16();
            if status.is_success() {
                tracing::info!(%host, status = code, subscription = id, "web push: delivered");
                return;
            }
            if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
                self.forget(subscription).await;
                tracing::info!(%host, status = code, subscription = id, "web push: the subscription is gone; deleted it");
                return;
            }
            let retryable = status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS;
            if retryable && attempt == 1 {
                tracing::info!(%host, status = code, subscription = id, "web push: retrying once");
                tokio::time::sleep(self.settings.retry_delay).await;
                continue;
            }
            tracing::warn!(%host, status = code, subscription = id, "web push: refused by the push service");
            return;
        }
    }

    /// Deletes a subscription the push service says is gone (404 or 410). The endpoint is in the
    /// condition too, so a row that changed since it was read is left alone.
    async fn forget(&self, subscription: &Subscription) {
        let deleted =
            sqlx::query("DELETE FROM ferry.push_subscriptions WHERE id = $1 AND endpoint = $2")
                .bind(subscription.id)
                .bind(&subscription.endpoint)
                .execute(&self.pool)
                .await;
        if let Err(err) = deleted {
            tracing::warn!(error = %err, subscription = subscription.id, "web push: cannot delete a gone subscription");
        }
    }
}

/// The request body for `subscription`, or None when its stored keys don't parse.
fn encrypt(subscription: &Subscription, payload: &[u8]) -> Option<Vec<u8>> {
    let p256dh = crypto::b64url_decode(subscription.p256dh.as_deref()?)?;
    let auth = crypto::b64url_decode(subscription.auth.as_deref()?)?;
    let ua_public = crypto::parse_ua_public(&p256dh).ok()?;
    let auth = crypto::parse_auth_secret(&auth).ok()?;
    crypto::encrypt(&ua_public, &auth, payload).ok()
}

fn failure(err: &reqwest::Error) -> &'static str {
    if err.is_timeout() {
        "timeout"
    } else if err.is_connect() {
        "connect"
    } else {
        "request"
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;

    fn config(pairs: &[(&str, &str)]) -> Config {
        let mut vars: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        vars.entry("DATABASE_URL".into())
            .or_insert_with(|| "postgres://postgres:postgres@127.0.0.1:55432/db".into());
        Config::from_vars(&vars).unwrap()
    }

    #[test]
    fn the_stub_needs_a_local_database_and_a_loopback_origin() {
        let local = PushSettings::from_config(&config(&[(
            "PUSH_TEST_STUB_ORIGIN",
            "http://127.0.0.1:8291",
        )]));
        assert_eq!(
            local.stub().map(Origin::ascii_serialization).as_deref(),
            Some("http://127.0.0.1:8291")
        );

        // The Supabase session pooler, as in production: ignored.
        let production = PushSettings::from_config(&config(&[
            (
                "DATABASE_URL",
                "postgres://postgres.ref:pw@aws-0-us-east-1.pooler.supabase.com:5432/postgres",
            ),
            ("PUSH_TEST_STUB_ORIGIN", "http://127.0.0.1:8291"),
        ]));
        assert!(production.stub().is_none());

        let remote_stub = PushSettings::from_config(&config(&[(
            "PUSH_TEST_STUB_ORIGIN",
            "http://10.0.0.5:8291",
        )]));
        assert!(remote_stub.stub().is_none());

        assert!(PushSettings::from_config(&config(&[])).stub().is_none());
        assert!(PushSettings::default().stub().is_none());
    }

    #[test]
    fn the_subject_is_an_https_web_url_or_the_default() {
        assert_eq!(
            vapid_subject(&config(&[(
                "PUBLIC_WEB_URL",
                "https://ferry-hacknc.vercel.app"
            )])),
            "https://ferry-hacknc.vercel.app"
        );
        assert_eq!(
            vapid_subject(&config(&[("PUBLIC_WEB_URL", "http://localhost:5173")])),
            DEFAULT_VAPID_SUBJECT
        );
        assert_eq!(vapid_subject(&config(&[])), DEFAULT_VAPID_SUBJECT);
    }

    fn notification(title: &str, body: &str) -> Value {
        json!({"notification": {
            "id": "ntf_1", "type": "invite", "title": title, "body": body,
            "data": {"invite_id": "inv_1"}, "created_at": "2026-10-11T01:02:03.000Z", "read": false
        }})
    }

    #[test]
    fn a_small_payload_is_the_data_as_is() {
        let data = notification("Dana wants to meet again", "Want to meet up again?");
        assert_eq!(payload(&data).unwrap(), serde_json::to_vec(&data).unwrap());
    }

    #[test]
    fn a_long_body_is_shortened_to_fit() {
        for body in [
            "x".repeat(5000),
            "é".repeat(3000),
            "\"".repeat(2000),
            "🌊".repeat(1500),
        ] {
            let bytes = payload(&notification("Title", &body)).unwrap();
            assert!(bytes.len() <= MAX_PAYLOAD, "{}", bytes.len());
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            let n = &value["notification"];
            assert_eq!(n["title"], "Title");
            assert_eq!(n["id"], "ntf_1");
            let shortened = n["body"].as_str().unwrap();
            assert!(shortened.ends_with(ELLIPSIS) && shortened.len() > 500);
            assert!(body.starts_with(shortened.trim_end_matches(ELLIPSIS)));
        }
        // Then the title, when the body is already empty.
        let bytes = payload(&notification(&"t".repeat(4000), "")).unwrap();
        assert!(bytes.len() <= MAX_PAYLOAD);
        // Data that cannot fit is not sent.
        let mut huge = notification("t", "b");
        huge["notification"]["data"]["x"] = Value::from("d".repeat(4000));
        assert!(payload(&huge).is_none());
        assert!(payload(&json!({"other": "x".repeat(4000)})).is_none());
    }

    #[test]
    fn shorten_cuts_at_a_character_boundary() {
        assert_eq!(shorten("abcdef", 1), "ab…");
        assert_eq!(shorten("ab", 1), "");
        assert_eq!(shorten("éé", 0), "");
        assert_eq!(shorten("ééé", 0), "é…");
    }
}
