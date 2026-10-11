//! The event hub over a real WebSocket (tokio-tungstenite): /v1/stream with a token or a ticket,
//! the 401s before the upgrade, per-user delivery, a stalled client, pings and the idle timeout.
mod common;
mod realtime_support;

use std::time::{Duration, Instant, SystemTime};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::assert_envelope;
use ferry_core::jwt::{Claims, JwtKeys};
use ferry_core::notifications::{Event, HubSettings, rfc3339};
use futures_util::{SinkExt, StreamExt};
use realtime_support::{
    Server, Ws, assert_contract, assert_rejected_401, assert_silent, contract_errors, eventually,
    fixture_run, next_json, serve, state,
};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

async fn server_with(settings: HubSettings) -> Server {
    serve(state(common::unreachable_pool(), settings)).await
}

async fn server() -> Server {
    server_with(HubSettings::default()).await
}

fn location_event() -> Event {
    Event::runner_location(
        "run_demo",
        &json!({"lat": 35.913211, "lng": -79.055094}),
        "2026-10-10T22:04:12Z",
        6.0,
    )
}

/// Reads until the server's close frame, skipping ping and pong frames.
async fn next_close(ws: &mut Ws) -> CloseFrame {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("no close within 5 s")
            .expect("the stream ended without a close frame")
            .expect("WebSocket error");
        match message {
            Message::Close(Some(frame)) => return frame,
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("expected a close frame, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn token_connects_and_send_to_arrives_as_the_contract_envelope() {
    let server = server().await;
    let mut ws = server.connect_with_token("usr_a").await;
    // Registered before the 101 went out, so nothing published from here on is missed.
    assert_eq!(server.hub().connection_count("usr_a"), 1);

    let event = location_event();
    assert_eq!(server.hub().send_to("usr_a", &event), 1);
    let received = next_json(&mut ws).await;
    assert_eq!(received, serde_json::to_value(&event).unwrap());
    let mut keys: Vec<&str> = received
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["at", "data", "type"]);
    assert_eq!(received["type"], "runner.location");
    assert_contract("StreamEnvelope", &received);
    assert_contract("RunnerLocationEvent", &received);
    assert_contract("ServerEvent", &received);

    // An event that carries a whole contract object: the fixture run.
    let run = fixture_run();
    server.hub().send_to("usr_a", &Event::run_updated(&run));
    let received = next_json(&mut ws).await;
    assert_eq!(received["type"], "run.updated");
    assert_eq!(received["data"]["run"], run);
    assert_contract("RunUpdatedEvent", &received);
    assert_contract("ServerEvent", &received);
}

#[test]
fn the_contract_check_rejects_malformed_events() {
    let good = serde_json::to_value(location_event()).unwrap();
    assert!(contract_errors("RunnerLocationEvent", &good).is_empty());
    let mut bad_cases = Vec::new();
    for (pointer, value) in [
        ("/at", json!("yesterday")),
        ("/type", json!("run.updated")),
        ("/data/location", json!({"lat": 35.9})),
        ("/data/eta_minutes", json!(null)),
    ] {
        let mut bad = good.clone();
        *bad.pointer_mut(pointer).unwrap() = value;
        bad_cases.push(bad);
    }
    let mut missing_at = good.clone();
    missing_at.as_object_mut().unwrap().remove("at");
    bad_cases.push(missing_at);
    for bad in &bad_cases {
        assert!(
            !contract_errors("RunnerLocationEvent", bad).is_empty(),
            "accepted {bad}"
        );
        assert!(
            !contract_errors("ServerEvent", bad).is_empty(),
            "accepted {bad}"
        );
    }
    let mut no_run = serde_json::to_value(Event::run_updated(&fixture_run())).unwrap();
    no_run["data"]["run"]
        .as_object_mut()
        .unwrap()
        .remove("status");
    assert!(!contract_errors("RunUpdatedEvent", &no_run).is_empty());
}

#[tokio::test]
async fn a_ticket_connects_once() {
    let server = server().await;
    let low = rfc3339(SystemTime::now() + Duration::from_secs(59));
    let ticket = server.ticket("usr_a").await;
    let high = rfc3339(SystemTime::now() + Duration::from_secs(61));

    let (_, body) = server.post_as("/v1/stream/ticket", "usr_a").await;
    let expires_at = body["expires_at"].as_str().unwrap();
    assert!(
        low.as_str() < expires_at && expires_at < high.as_str(),
        "expires_at {expires_at} is not about 60 s ahead"
    );

    let mut ws = server
        .connect(&format!("?ticket={ticket}"))
        .await
        .expect("connect with a ticket");
    assert_eq!(server.hub().send_to("usr_a", &location_event()), 1);
    assert_contract("RunnerLocationEvent", &next_json(&mut ws).await);

    // The same ticket again: 401, and the first connection is unaffected.
    assert_rejected_401(server.connect(&format!("?ticket={ticket}")).await);
    assert_eq!(server.hub().connection_count("usr_a"), 1);
    server.hub().send_to("usr_a", &location_event());
    assert_eq!(next_json(&mut ws).await["type"], "runner.location");
}

#[tokio::test]
async fn an_expired_ticket_is_401() {
    let server = server_with(HubSettings {
        ticket_ttl: Duration::from_millis(300),
        ..HubSettings::default()
    })
    .await;
    let ticket = server.ticket("usr_a").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_rejected_401(server.connect(&format!("?ticket={ticket}")).await);
    assert_eq!(server.hub().connection_count("usr_a"), 0);
}

#[tokio::test]
async fn the_ticket_endpoint_needs_a_bearer_token() {
    let server = server().await;
    let request = Request::post("/v1/stream/ticket")
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = server.send(request).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_envelope(&body, "unauthorized");

    let request = Request::post("/v1/stream/ticket")
        .header(header::AUTHORIZATION, "Bearer not.a.jwt")
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = server.send(request).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_envelope(&body, "unauthorized");
}

#[tokio::test]
async fn missing_or_bad_credentials_are_401_before_the_upgrade() {
    let server = server().await;
    let other_secret = JwtKeys::new(b"another-secret-another-secret-xx")
        .issue_token("usr_a")
        .unwrap();
    let expired = server
        .state
        .jwt
        .sign(&Claims {
            sub: "usr_a".into(),
            iat: 1_000,
            exp: 2_000,
        })
        .unwrap();
    let queries = [
        String::new(),
        "?".into(),
        "?token=".into(),
        "?ticket=".into(),
        "?other=1".into(),
        "?token=not.a.jwt".into(),
        format!("?token={other_secret}"),
        format!("?token={expired}"),
        "?ticket=0123456789abcdef".into(),
        // A bad ticket wins over a good token.
        format!("?ticket=nope&token={}", server.token("usr_a")),
    ];
    for query in &queries {
        assert_rejected_401(server.connect(query).await);
    }
    assert_eq!(server.hub().connection_count("usr_a"), 0);

    // Plain HTTP without credentials gets the same 401.
    let (status, _, body) = server.send(common::get("/v1/stream")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_envelope(&body, "unauthorized");

    // Signed in, but not a WebSocket upgrade: a 4xx in the envelope, and nothing registered.
    let uri = format!("/v1/stream?token={}", server.token("usr_a"));
    let (status, _, body) = server.send(common::get(&uri)).await;
    assert!(status.is_client_error(), "{status}");
    assert_envelope(&body, "validation_failed");
    assert_eq!(server.hub().connection_count("usr_a"), 0);
}

#[tokio::test]
async fn a_user_never_receives_another_users_event() {
    let server = server().await;
    let mut a = server.connect_with_token("usr_a").await;
    let mut b = server.connect_with_token("usr_b").await;

    assert_eq!(server.hub().send_to("usr_a", &location_event()), 1);
    assert_eq!(next_json(&mut a).await["type"], "runner.location");
    assert_silent(&mut b, Duration::from_millis(300)).await;

    // send_to_many: each recipient gets the copy built for them, and only that one.
    let sent = server.hub().send_to_many(["usr_a", "usr_b"], |viewer| {
        Event::order_updated(&json!({"viewer": viewer}))
    });
    assert_eq!(sent, 2);
    assert_eq!(next_json(&mut a).await["data"]["order"]["viewer"], "usr_a");
    assert_eq!(next_json(&mut b).await["data"]["order"]["viewer"], "usr_b");

    // An audience without b: b hears nothing.
    server
        .hub()
        .send_to_many(["usr_a", "usr_c"], |_| location_event());
    assert_eq!(next_json(&mut a).await["type"], "runner.location");
    assert_silent(&mut a, Duration::from_millis(200)).await;
    assert_silent(&mut b, Duration::from_millis(200)).await;
}

#[tokio::test]
async fn every_connection_of_a_user_receives() {
    let server = server().await;
    let mut phone = server.connect_with_token("usr_a").await;
    let ticket = server.ticket("usr_a").await;
    let mut laptop = server.connect(&format!("?ticket={ticket}")).await.unwrap();
    assert_eq!(server.hub().connection_count("usr_a"), 2);

    let event = location_event();
    assert_eq!(server.hub().send_to("usr_a", &event), 2);
    let expected = serde_json::to_value(&event).unwrap();
    assert_eq!(next_json(&mut phone).await, expected);
    assert_eq!(next_json(&mut laptop).await, expected);

    // A closed connection leaves the hub; the other keeps receiving.
    phone.close(None).await.unwrap();
    eventually("the closed connection is removed", || {
        server.hub().connection_count("usr_a") == 1
    })
    .await;
    assert_eq!(server.hub().send_to("usr_a", &event), 1);
    assert_eq!(next_json(&mut laptop).await, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_client_does_not_block_send_to_many() {
    let server = server_with(HubSettings {
        buffer: 4,
        write_timeout: Duration::from_secs(1),
        ..HubSettings::default()
    })
    .await;
    // Connected, but never reads: once the socket buffers fill, its queue backs up.
    let _stalled = server.connect_with_token("usr_stalled").await;
    let mut healthy = server.connect_with_token("usr_healthy").await;

    // 200 events of 64 KiB (12.5 MiB) is more than loopback buffers hold.
    let pad = "x".repeat(64 * 1024);
    let mut slowest = Duration::ZERO;
    for n in 0..200 {
        let started = Instant::now();
        server
            .hub()
            .send_to_many(["usr_stalled", "usr_healthy"], |viewer| {
                Event::run_updated(&json!({"n": n, "viewer": viewer, "pad": pad}))
            });
        slowest = slowest.max(started.elapsed());
        let received: Value = next_json(&mut healthy).await;
        assert_eq!(received["data"]["run"]["n"], n);
        assert_eq!(received["data"]["run"]["viewer"], "usr_healthy");
    }
    assert!(
        slowest < Duration::from_millis(500),
        "send_to_many took up to {slowest:?}"
    );
    eventually("the stalled connection is dropped", || {
        server.hub().connection_count("usr_stalled") == 0
    })
    .await;
    assert_eq!(server.hub().connection_count("usr_healthy"), 1);
}

#[tokio::test]
async fn client_pings_keep_a_connection_open_and_silence_closes_it() {
    let server = server_with(HubSettings {
        idle_timeout: Duration::from_secs(1),
        ping_interval: Duration::from_secs(3600),
        ..HubSettings::default()
    })
    .await;
    let mut ws = server.connect_with_token("usr_a").await;
    let mut last_ping = Instant::now();
    for _ in 0..8 {
        ws.send(Message::Text(r#"{"type":"ping"}"#.into()))
            .await
            .unwrap();
        last_ping = Instant::now();
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    // 1.6 s of pings, longer than the idle timeout: still open. The pings got no reply, so the first
    // text message is the event.
    server.hub().send_to("usr_a", &location_event());
    assert_eq!(next_json(&mut ws).await["type"], "runner.location");

    // Then silence: closed with 1001 once the idle timeout has passed since the last ping.
    let close = next_close(&mut ws).await;
    assert_eq!(close.code, CloseCode::Away);
    let quiet = last_ping.elapsed();
    assert!(
        quiet >= Duration::from_secs(1) && quiet < Duration::from_secs(3),
        "closed {quiet:?} after the last ping"
    );
    eventually("the idle connection is removed", || {
        server.hub().connection_count("usr_a") == 0
    })
    .await;
}

#[tokio::test]
async fn server_pings_and_the_clients_pongs_keep_a_quiet_connection_open() {
    let server = server_with(HubSettings {
        ping_interval: Duration::from_millis(200),
        idle_timeout: Duration::from_secs(1),
        ..HubSettings::default()
    })
    .await;
    let mut ws = server.connect_with_token("usr_a").await;
    // Only read for 2.5 s, longer than the idle timeout: tungstenite answers each ping with a
    // pong, and the pongs count as activity.
    let deadline = tokio::time::Instant::now() + Duration::from_millis(2500);
    let mut pings = 0;
    while let Ok(message) = tokio::time::timeout_at(deadline, ws.next()).await {
        match message {
            Some(Ok(Message::Ping(_))) => pings += 1,
            other => panic!("expected only pings, got {other:?}"),
        }
    }
    assert!(pings >= 5, "{pings} pings");
    assert_eq!(server.hub().connection_count("usr_a"), 1);
    server.hub().send_to("usr_a", &location_event());
    assert_eq!(next_json(&mut ws).await["type"], "runner.location");
}
