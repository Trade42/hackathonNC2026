//! The event hub: who is connected to `/v1/stream`, and best-effort delivery of the contract's
//! server events to them.
//!
//! Publishers decide the audience and build each recipient's payload, so per-viewer redaction
//! happens at the source. Delivery never blocks: each connection has a bounded queue, and a
//! connection whose queue is full is dropped (the client reconnects and refetches over REST).
//!
//! Every `notification.created` sent with [`Hub::send_to`] is also offered to Web Push when it is
//! attached (`notifications::push`), whether or not the user is connected.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, SystemTime};

use axum::extract::ws::Utf8Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{Notify, mpsc};

use super::clock;
use super::notify::Notification;
use super::push::PushQueue;
use super::tickets::Tickets;

/// Limits and timings. `Hub::default()` uses [`HubSettings::default`]; tests shorten them.
#[derive(Debug, Clone)]
pub struct HubSettings {
    /// Events queued per connection. A connection that falls this far behind is dropped.
    pub buffer: usize,
    /// How often the server sends a WebSocket ping frame, so proxies (the Cloudflare tunnel)
    /// don't close the socket as idle.
    pub ping_interval: Duration,
    /// A connection that sends nothing (no message, ping or pong frame) for this long is closed.
    pub idle_timeout: Duration,
    /// How long one write to the socket may take before the connection is dropped.
    pub write_timeout: Duration,
    /// How long a stream ticket stays valid.
    pub ticket_ttl: Duration,
    /// Connections one user may hold at once. Opening another closes their oldest.
    pub max_connections_per_user: usize,
}

impl Default for HubSettings {
    fn default() -> Self {
        Self {
            buffer: 64,
            ping_interval: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(75),
            write_timeout: Duration::from_secs(10),
            ticket_ttl: Duration::from_secs(60),
            max_connections_per_user: 16,
        }
    }
}

/// `StreamEventType` in contract/openapi.yaml.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventType {
    #[serde(rename = "run.updated")]
    RunUpdated,
    #[serde(rename = "order.updated")]
    OrderUpdated,
    #[serde(rename = "runner.location")]
    RunnerLocation,
    #[serde(rename = "notification.created")]
    NotificationCreated,
    #[serde(rename = "invite.updated")]
    InviteUpdated,
    #[serde(rename = "wallet.updated")]
    WalletUpdated,
}

impl EventType {
    pub const ALL: [EventType; 6] = [
        EventType::RunUpdated,
        EventType::OrderUpdated,
        EventType::RunnerLocation,
        EventType::NotificationCreated,
        EventType::InviteUpdated,
        EventType::WalletUpdated,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EventType::RunUpdated => "run.updated",
            EventType::OrderUpdated => "order.updated",
            EventType::RunnerLocation => "runner.location",
            EventType::NotificationCreated => "notification.created",
            EventType::InviteUpdated => "invite.updated",
            EventType::WalletUpdated => "wallet.updated",
        }
    }
}

/// One server message, `{type, data, at}` (`StreamEnvelope`). Build it with the constructor for
/// its type; `at` is the time it was built.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Event {
    #[serde(rename = "type")]
    kind: EventType,
    data: Value,
    at: String,
}

impl Event {
    /// `run.updated`, data `{run}`: the run as the REST API shows it to this recipient (each
    /// copy's `runner_location` null unless that viewer may see it; contract/events.md).
    pub fn run_updated<T: Serialize + ?Sized>(run: &T) -> Self {
        Self::wrap(EventType::RunUpdated, "run", run)
    }

    /// `order.updated`, data `{order}`: the order as the REST API shows it to this recipient
    /// (`handoff_code` only in the customer's copy).
    pub fn order_updated<T: Serialize + ?Sized>(order: &T) -> Self {
        Self::wrap(EventType::OrderUpdated, "order", order)
    }

    /// `runner.location`, data `{run_id, location, at, eta_minutes}`. `location` serializes as
    /// `{lat, lng}`; `measured_at` is when the runner's app measured it (ISO 8601 UTC), not
    /// when the event is sent. `eta_minutes` must be finite.
    pub fn runner_location<L: Serialize + ?Sized>(
        run_id: &str,
        location: &L,
        measured_at: &str,
        eta_minutes: f64,
    ) -> Self {
        #[derive(Serialize)]
        struct Data<'a, L: ?Sized> {
            run_id: &'a str,
            location: &'a L,
            at: &'a str,
            eta_minutes: f64,
        }
        Self::with_data(
            EventType::RunnerLocation,
            &Data {
                run_id,
                location,
                at: measured_at,
                eta_minutes,
            },
        )
    }

    /// `notification.created`, data `{notification}`. `notifications::notify` sends it for you.
    pub fn notification_created(notification: &Notification) -> Self {
        Self::wrap(EventType::NotificationCreated, "notification", notification)
    }

    /// `invite.updated`, data `{invite}`.
    pub fn invite_updated<T: Serialize + ?Sized>(invite: &T) -> Self {
        Self::wrap(EventType::InviteUpdated, "invite", invite)
    }

    /// `wallet.updated`, data `{wallet}`: the recipient's own wallet.
    pub fn wallet_updated<T: Serialize + ?Sized>(wallet: &T) -> Self {
        Self::wrap(EventType::WalletUpdated, "wallet", wallet)
    }

    pub fn kind(&self) -> EventType {
        self.kind
    }

    pub fn data(&self) -> &Value {
        &self.data
    }

    pub fn at(&self) -> &str {
        &self.at
    }

    /// `{key: value}` as the data.
    fn wrap<T: Serialize + ?Sized>(kind: EventType, key: &str, value: &T) -> Self {
        Self::with_data(kind, &BTreeMap::from([(key, value)]))
    }

    fn with_data<T: Serialize + ?Sized>(kind: EventType, data: &T) -> Self {
        let data = serde_json::to_value(data).unwrap_or_else(|err| payload_error(kind, &err));
        Self {
            kind,
            data,
            at: clock::now_rfc3339(),
        }
    }

    /// The text frame to send, or `None` when the payload did not serialize (already logged).
    fn frame(&self) -> Option<Utf8Bytes> {
        if !self.data.is_object() {
            return None;
        }
        serde_json::to_string(self).ok().map(Utf8Bytes::from)
    }
}

/// A payload that does not serialize is a bug in its `Serialize` impl. The event is not sent.
fn payload_error(kind: EventType, err: &serde_json::Error) -> Value {
    tracing::error!(event = kind.as_str(), error = %err, "stream event payload does not serialize; not sent");
    Value::Null
}

/// The hub. Cheap to clone; `AppState.hub` holds the one the app uses.
#[derive(Clone, Default)]
pub struct Hub(Arc<Inner>);

#[derive(Default)]
struct Inner {
    settings: HubSettings,
    next_id: AtomicU64,
    connections: Mutex<HashMap<String, Vec<Connection>>>,
    tickets: Tickets,
    /// Web Push, decided once by [`Hub::attach_push`]: None inside when push is off.
    push: OnceLock<Option<PushQueue>>,
}

#[derive(Clone)]
struct Connection {
    id: u64,
    tx: mpsc::Sender<Utf8Bytes>,
    kick: Arc<Notify>,
}

impl Hub {
    pub fn with_settings(settings: HubSettings) -> Self {
        Self(Arc::new(Inner {
            settings,
            ..Inner::default()
        }))
    }

    pub fn settings(&self) -> &HubSettings {
        &self.0.settings
    }

    /// Queues `event` on every connection `user_id` has open and returns how many that is (0 when
    /// the user is not connected). Never blocks. Publish after the transaction behind the change
    /// commits, so a client that refetches on the event sees it.
    ///
    /// A `notification.created` is also offered to Web Push when it is attached, connected or not.
    /// That never waits either: the push goes out from its own task, or is dropped when its queue
    /// is full.
    pub fn send_to(&self, user_id: &str, event: &Event) -> usize {
        let Some(frame) = event.frame() else {
            return 0;
        };
        if event.kind == EventType::NotificationCreated
            && let Some(push) = self.push()
        {
            push.offer(user_id, &event.data);
        }
        let targets = self.connections_of(user_id);
        let mut dropped = Vec::new();
        let sent = deliver(user_id, &targets, &frame, &mut dropped);
        self.remove(&dropped);
        sent
    }

    /// Sends to each user in `user_ids` the event `build(user_id)` returns, so each recipient can
    /// get their own copy (for example an order with `handoff_code` only for its customer).
    /// `build` runs only for users who are connected, once per user even if `user_ids` repeats
    /// one. Returns how many connections the events were queued on. Never blocks. Send
    /// `notification.created` with [`Hub::send_to`] instead, which also pushes to users who are
    /// not connected.
    pub fn send_to_many<I, S, F>(&self, user_ids: I, mut build: F) -> usize
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
        F: FnMut(&str) -> Event,
    {
        let mut seen = HashSet::new();
        let targets: Vec<(String, Vec<Connection>)> = {
            let connections = self.lock();
            user_ids
                .into_iter()
                .filter_map(|user_id| {
                    let user_id = user_id.as_ref();
                    let open = connections.get(user_id)?;
                    seen.insert(user_id.to_owned())
                        .then(|| (user_id.to_owned(), open.clone()))
                })
                .collect()
        };
        // `build` runs without the lock held.
        let mut sent = 0;
        let mut dropped = Vec::new();
        for (user_id, open) in &targets {
            if let Some(frame) = build(user_id).frame() {
                sent += deliver(user_id, open, &frame, &mut dropped);
            }
        }
        self.remove(&dropped);
        sent
    }

    /// Decides once whether this hub (and every clone of it) pushes: the first call runs `start`
    /// and keeps what it returns, and later calls do nothing. Returns whether push is on.
    pub(crate) fn attach_push(&self, start: impl FnOnce() -> Option<PushQueue>) -> bool {
        self.0.push.get_or_init(start).is_some()
    }

    /// The attached Web Push queue, if push is on.
    pub(crate) fn push(&self) -> Option<&PushQueue> {
        self.0.push.get().and_then(Option::as_ref)
    }

    /// How many stream connections `user_id` has open.
    pub fn connection_count(&self, user_id: &str) -> usize {
        self.lock().get(user_id).map_or(0, Vec::len)
    }

    /// A single-use ticket for `user_id` and when it expires.
    pub fn issue_ticket(&self, user_id: &str) -> (String, SystemTime) {
        let ttl = self.settings().ticket_ttl;
        (self.0.tickets.issue(user_id, ttl), SystemTime::now() + ttl)
    }

    /// The user a ticket was issued to, if it is valid. Uses the ticket up either way.
    pub fn redeem_ticket(&self, ticket: &str) -> Option<String> {
        self.0.tickets.redeem(ticket)
    }

    /// Adds a connection for `user_id`. It is removed when the returned registration drops. If
    /// the user is at `max_connections_per_user`, their oldest connection is closed.
    pub(crate) fn register(&self, user_id: &str) -> Registration {
        let settings = self.settings();
        let (tx, rx) = mpsc::channel(settings.buffer.max(1));
        let kick = Arc::new(Notify::new());
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut connections = self.lock();
            let open = connections.entry(user_id.to_owned()).or_default();
            let max = settings.max_connections_per_user.max(1);
            while open.len() >= max {
                let oldest = open.remove(0);
                tracing::debug!(user_id, "stream: too many connections; closing the oldest");
                oldest.kick.notify_one();
            }
            open.push(Connection {
                id,
                tx,
                kick: kick.clone(),
            });
        }
        Registration {
            hub: self.clone(),
            user_id: user_id.to_owned(),
            id,
            rx,
            kick,
        }
    }

    fn connections_of(&self, user_id: &str) -> Vec<Connection> {
        self.lock().get(user_id).cloned().unwrap_or_default()
    }

    fn remove(&self, dropped: &[(String, u64)]) {
        if dropped.is_empty() {
            return;
        }
        let mut connections = self.lock();
        for (user_id, id) in dropped {
            remove_one(&mut connections, user_id, *id);
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Vec<Connection>>> {
        self.0
            .connections
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// Queues `frame` on each of `open` without waiting. A full queue means the client fell behind:
/// that connection is told to close and listed in `dropped`, as is one that already closed.
fn deliver(
    user_id: &str,
    open: &[Connection],
    frame: &Utf8Bytes,
    dropped: &mut Vec<(String, u64)>,
) -> usize {
    let mut sent = 0;
    for connection in open {
        match connection.tx.try_send(frame.clone()) {
            Ok(()) => sent += 1,
            Err(TrySendError::Full(_)) => {
                tracing::info!(user_id, "stream: a connection fell behind; closing it");
                connection.kick.notify_one();
                dropped.push((user_id.to_owned(), connection.id));
            }
            Err(TrySendError::Closed(_)) => dropped.push((user_id.to_owned(), connection.id)),
        }
    }
    sent
}

fn remove_one(connections: &mut HashMap<String, Vec<Connection>>, user_id: &str, id: u64) {
    if let Some(open) = connections.get_mut(user_id) {
        open.retain(|c| c.id != id);
        if open.is_empty() {
            connections.remove(user_id);
        }
    }
}

/// One open connection's end of the hub: its queue of frames and its "close now" signal.
pub(crate) struct Registration {
    hub: Hub,
    user_id: String,
    id: u64,
    pub(crate) rx: mpsc::Receiver<Utf8Bytes>,
    /// Notified when the hub drops this connection (it fell behind, or the user opened too many).
    pub(crate) kick: Arc<Notify>,
}

impl Registration {
    pub(crate) fn user_id(&self) -> &str {
        &self.user_id
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        remove_one(&mut self.hub.lock(), &self.user_id, self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn small_hub(buffer: usize) -> Hub {
        Hub::with_settings(HubSettings {
            buffer,
            ..HubSettings::default()
        })
    }

    fn parse(frame: Utf8Bytes) -> Value {
        serde_json::from_str(frame.as_str()).unwrap()
    }

    async fn kicked(registration: &Registration) -> bool {
        tokio::time::timeout(Duration::from_millis(100), registration.kick.notified())
            .await
            .is_ok()
    }

    #[test]
    fn event_types_match_the_contract() {
        let yaml = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contract/openapi.yaml"
        ))
        .unwrap();
        let line = yaml
            .lines()
            .skip_while(|l| l.trim() != "StreamEventType:")
            .find(|l| l.trim_start().starts_with("enum:"))
            .unwrap();
        let listed: Vec<&str> = line
            .trim()
            .trim_start_matches("enum:")
            .trim()
            .trim_matches(['[', ']'])
            .split(',')
            .map(str::trim)
            .collect();
        let ours: Vec<&str> = EventType::ALL.iter().map(|t| t.as_str()).collect();
        assert_eq!(listed, ours);
        for t in EventType::ALL {
            assert_eq!(serde_json::to_value(t).unwrap(), json!(t.as_str()));
        }
    }

    #[test]
    fn envelope_shape() {
        let event = Event::run_updated(&json!({"id": "run_demo"}));
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "run.updated");
        assert_eq!(value["data"], json!({"run": {"id": "run_demo"}}));
        assert_eq!(value["at"], event.at());
        assert_eq!(value.as_object().unwrap().len(), 3);

        let location = Event::runner_location(
            "run_demo",
            &json!({"lat": 35.9, "lng": -79.0}),
            "2026-10-10T22:04:12Z",
            4.5,
        );
        assert_eq!(
            location.data(),
            &json!({
                "run_id": "run_demo",
                "location": {"lat": 35.9, "lng": -79.0},
                "at": "2026-10-10T22:04:12Z",
                "eta_minutes": 4.5
            })
        );
        for (event, key) in [
            (Event::order_updated(&json!({})), "order"),
            (Event::invite_updated(&json!({})), "invite"),
            (Event::wallet_updated(&json!({})), "wallet"),
        ] {
            assert_eq!(event.data(), &json!({ key: {} }));
        }
    }

    #[test]
    fn a_payload_that_does_not_serialize_is_not_sent() {
        // Maps with non-string keys do not serialize to JSON.
        let bad: HashMap<(u8, u8), u8> = HashMap::from([((1, 2), 3)]);
        let event = Event::order_updated(&bad);
        assert!(event.frame().is_none());

        let hub = Hub::default();
        let mut registration = hub.register("usr_a");
        assert_eq!(hub.send_to("usr_a", &event), 0);
        assert!(registration.rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn send_to_reaches_every_connection_of_that_user_only() {
        let hub = Hub::default();
        let mut a1 = hub.register("usr_a");
        let mut a2 = hub.register("usr_a");
        let mut b = hub.register("usr_b");
        assert_eq!(hub.connection_count("usr_a"), 2);

        let event = Event::wallet_updated(&json!({"balance_cents": 100}));
        assert_eq!(hub.send_to("usr_a", &event), 2);
        assert_eq!(hub.send_to("usr_nobody", &event), 0);
        for registration in [&mut a1, &mut a2] {
            assert_eq!(
                parse(registration.rx.try_recv().unwrap()),
                serde_json::to_value(&event).unwrap()
            );
        }
        assert!(b.rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn send_to_many_builds_one_copy_per_connected_user() {
        let hub = Hub::default();
        let mut a = hub.register("usr_a");
        let mut b = hub.register("usr_b");
        let mut built = Vec::new();
        let sent = hub.send_to_many(["usr_a", "usr_b", "usr_a", "usr_offline"], |user_id| {
            built.push(user_id.to_owned());
            Event::order_updated(&json!({"viewer": user_id}))
        });
        assert_eq!(sent, 2);
        assert_eq!(built, ["usr_a", "usr_b"]);
        assert_eq!(
            parse(a.rx.try_recv().unwrap())["data"]["order"]["viewer"],
            "usr_a"
        );
        assert_eq!(
            parse(b.rx.try_recv().unwrap())["data"]["order"]["viewer"],
            "usr_b"
        );
        assert!(a.rx.try_recv().is_err(), "usr_a listed twice, sent once");
    }

    #[tokio::test]
    async fn a_connection_that_falls_behind_is_dropped_without_blocking_others() {
        let hub = small_hub(4);
        let stalled = hub.register("usr_slow");
        let mut healthy = hub.register("usr_fast");
        for i in 0..4 {
            let sent = hub.send_to_many(["usr_slow", "usr_fast"], |_| {
                Event::run_updated(&json!({"n": i}))
            });
            assert_eq!(sent, 2);
            healthy.rx.try_recv().unwrap();
        }
        assert!(!kicked(&stalled).await);

        // The fifth event overflows the stalled connection's queue.
        let sent = hub.send_to_many(["usr_slow", "usr_fast"], |_| {
            Event::run_updated(&json!({"n": 4}))
        });
        assert_eq!(sent, 1);
        assert!(kicked(&stalled).await);
        assert_eq!(hub.connection_count("usr_slow"), 0);
        assert_eq!(hub.connection_count("usr_fast"), 1);
        assert_eq!(parse(healthy.rx.try_recv().unwrap())["data"]["run"]["n"], 4);

        // Later events skip the dropped connection.
        assert_eq!(hub.send_to("usr_slow", &Event::run_updated(&json!({}))), 0);
        drop(stalled);
        assert_eq!(hub.connection_count("usr_fast"), 1);
    }

    #[tokio::test]
    async fn dropping_a_registration_unregisters_it() {
        let hub = Hub::default();
        let a1 = hub.register("usr_a");
        let a2 = hub.register("usr_a");
        assert_eq!(a1.user_id(), "usr_a");
        drop(a1);
        assert_eq!(hub.connection_count("usr_a"), 1);
        drop(a2);
        assert_eq!(hub.connection_count("usr_a"), 0);
        assert!(hub.lock().is_empty());
    }

    #[tokio::test]
    async fn a_closed_queue_is_cleaned_up_on_send() {
        let hub = Hub::default();
        let mut registration = hub.register("usr_a");
        // A connection task that is ending: its queue is closed but not yet dropped.
        registration.rx.close();
        assert_eq!(hub.send_to("usr_a", &Event::wallet_updated(&json!({}))), 0);
        assert_eq!(hub.connection_count("usr_a"), 0);
    }

    #[tokio::test]
    async fn too_many_connections_close_the_oldest() {
        let hub = Hub::with_settings(HubSettings {
            max_connections_per_user: 2,
            ..HubSettings::default()
        });
        let first = hub.register("usr_a");
        let second = hub.register("usr_a");
        let _third = hub.register("usr_a");
        assert_eq!(hub.connection_count("usr_a"), 2);
        assert!(kicked(&first).await);
        assert!(!kicked(&second).await);
    }

    #[test]
    fn tickets_go_through_the_hub() {
        let hub = Hub::default();
        let before = SystemTime::now();
        let (ticket, expires_at) = hub.issue_ticket("usr_a");
        let ttl = expires_at.duration_since(before).unwrap();
        assert!(ttl >= Duration::from_secs(60) && ttl < Duration::from_secs(61));
        assert_eq!(hub.redeem_ticket(&ticket).as_deref(), Some("usr_a"));
        assert_eq!(hub.redeem_ticket(&ticket), None);
    }
}
