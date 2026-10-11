//! Web Push delivery against a stub push service: every notification.created reaches each of its
//! user's web subscriptions as one encrypted, VAPID-signed POST. The test decrypts it with the
//! subscription's private key and finds `{"notification": …}`. A 404 or 410 deletes the
//! subscription; a 5xx or 429 is retried once; a hanging push service never slows the API.
mod common;
mod push_support;
mod realtime_support;

use std::time::{Duration, Instant};

use axum::http::StatusCode;
use ferry_core::notifications::{Event, NotificationType, create_notification, notify};
use ferry_core::state::AppState;
use push_support::{
    Browser, Reply, Stub, WEB_URL, assert_push_headers, post_json, push_server, rows, seed_hinton4,
    stored, subscribe_ok, wait_for_rows,
};
use realtime_support::{Ws, assert_contract, next_json, serve};
use serde_json::{Value, json};
use sqlx::PgPool;

/// How long a test waits to be sure nothing more arrives.
const QUIET: Duration = Duration::from_millis(400);

async fn next_event(ws: &mut Ws, kind: &str) -> Value {
    loop {
        let event = next_json(ws).await;
        if event["type"] == kind {
            return event;
        }
    }
}

async fn subscribe_browser(
    server: &realtime_support::Server,
    user_id: &str,
    stub: &Stub,
    name: &str,
) -> Browser {
    let browser = Browser::new(stub.endpoint(name));
    subscribe_ok(server, user_id, &browser.subscribe_body()).await;
    browser
}

/// DEMO step 6: Dana's meet-again reaches the judge's browser.
#[sqlx::test(migrations = "../migrations")]
async fn meet_again_reaches_the_judges_browser_encrypted_and_signed(pool: PgPool) {
    let stub = Stub::start().await;
    let (server, vapid) = push_server(pool.clone(), Some(&stub), &[], |_| {}).await;
    let judge = subscribe_browser(&server, "usr_judge", &stub, "judge").await;
    let mut ws = server.connect_with_token("usr_judge").await;

    let (status, invite) = post_json(
        &server,
        "/v1/circles/cir_hinton4/meet-again",
        "usr_dana",
        &json!({"message": "Dinner at Lenoir?"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{invite}");
    let invite_id = invite["id"].as_str().unwrap();

    // One POST: only the judge has a subscription, and the app (built per request here) attached
    // push once.
    let received = stub.wait_for(1).await;
    stub.assert_stays_at(1, QUIET).await;
    let push = &received[0];
    assert_eq!(push.path, "/push/judge");
    let claims = assert_push_headers(push, &vapid);
    assert_eq!(
        claims["aud"], stub.origin,
        "aud is the push service's origin"
    );
    assert_eq!(claims["sub"], WEB_URL);

    let plaintext = judge.decrypt(&push.body);
    assert!(plaintext.len() <= 3072, "{} bytes", plaintext.len());
    let payload: Value = serde_json::from_slice(&plaintext).unwrap();
    assert_eq!(payload.as_object().unwrap().len(), 1, "{payload}");
    let notification = &payload["notification"];
    assert_contract("Notification", notification);
    assert_eq!(notification["type"], "invite");
    assert_eq!(notification["title"], "Dana wants to meet up");
    assert_eq!(notification["body"], "“Dinner at Lenoir?” You in?");
    assert_eq!(notification["read"], false);
    assert_eq!(
        notification["data"],
        json!({"invite_id": invite_id, "circle_id": "cir_hinton4", "url": format!("{WEB_URL}/i/{invite_id}")})
    );

    // The same object as the stored notification and the notification.created event.
    let (status, list) = server.get_as("/v1/notifications", "usr_judge").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list[0], *notification);
    let event = next_event(&mut ws, "notification.created").await;
    assert_eq!(event["data"], payload);
}

#[sqlx::test(migrations = "../migrations")]
async fn every_notification_reaches_every_web_subscription_of_its_user(pool: PgPool) {
    let stub = Stub::start().await;
    let (server, vapid) = push_server(pool.clone(), Some(&stub), &[], |_| {}).await;
    let phone = subscribe_browser(&server, "usr_judge", &stub, "phone").await;
    let laptop = subscribe_browser(&server, "usr_judge", &stub, "laptop").await;
    subscribe_browser(&server, "usr_maya", &stub, "maya").await;
    // A native token is stored but never sent to (P2).
    subscribe_ok(
        &server,
        "usr_judge",
        &json!({"platform": "ios", "token": "apns-1"}),
    )
    .await;

    // notify() for each type, the path most callers take.
    let mut ids = Vec::new();
    for kind in NotificationType::ALL {
        let n = notify(
            &server.state,
            "usr_judge",
            kind,
            kind.as_str(),
            "body",
            json!({"run_id": "run_demo"}),
        )
        .await
        .unwrap();
        ids.push(n.id);
    }
    // create_notification in a transaction, then send_to after the commit: invites and runs.
    let mut tx = server.state.pool.begin().await.unwrap();
    let n = create_notification(
        &mut *tx,
        "usr_judge",
        NotificationType::OrderUpdate,
        "Picked up",
        "On the way",
        json!({"order_id": "ord_1"}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    server
        .hub()
        .send_to("usr_judge", &Event::notification_created(&n));
    ids.push(n.id);

    let expected = ids.len() * 2;
    let received = stub.wait_for(expected).await;
    stub.assert_stays_at(expected, QUIET).await;
    assert_eq!(stub.count_at("maya"), 0);
    for (name, browser) in [("phone", &phone), ("laptop", &laptop)] {
        let path = format!("/push/{name}");
        let mut got: Vec<String> = received
            .iter()
            .filter(|r| r.path == path)
            .map(|r| {
                assert_push_headers(r, &vapid);
                let payload = browser.open(r);
                assert_contract("Notification", &payload["notification"]);
                payload["notification"]["id"].as_str().unwrap().to_owned()
            })
            .collect();
        got.sort();
        let mut want = ids.clone();
        want.sort();
        assert_eq!(got, want, "{name}");
    }

    // The laptop's endpoint moves to Maya: the judge's next notification skips it.
    let moved = Browser::new(stub.endpoint("laptop"));
    subscribe_ok(&server, "usr_maya", &moved.subscribe_body()).await;
    notify(
        &server.state,
        "usr_judge",
        NotificationType::System,
        "Hi",
        "",
        json!({}),
    )
    .await
    .unwrap();
    stub.wait_for(expected + 1).await;
    stub.assert_stays_at(expected + 1, QUIET).await;
    assert_eq!(stub.received().last().unwrap().path, "/push/phone");
    notify(
        &server.state,
        "usr_maya",
        NotificationType::System,
        "Hi Maya",
        "",
        json!({}),
    )
    .await
    .unwrap();
    let received = stub.wait_for(expected + 3).await;
    let maya_paths: Vec<&str> = received[expected + 1..]
        .iter()
        .map(|r| r.path.as_str())
        .collect();
    assert_eq!(maya_paths.len(), 2, "{maya_paths:?}");
    let to_laptop = received[expected + 1..]
        .iter()
        .find(|r| r.path == "/push/laptop")
        .unwrap();
    assert_eq!(moved.open(to_laptop)["notification"]["title"], "Hi Maya");
}

#[sqlx::test(migrations = "../migrations")]
async fn a_404_or_410_deletes_the_subscription(pool: PgPool) {
    let stub = Stub::start().await;
    let (server, _) = push_server(pool.clone(), Some(&stub), &[], |_| {}).await;
    stub.script("gone", &[Reply::status(410)]);
    stub.script("missing", &[Reply::status(404)]);
    for name in ["gone", "missing", "fine"] {
        subscribe_browser(&server, "usr_judge", &stub, name).await;
    }
    notify(
        &server.state,
        "usr_judge",
        NotificationType::RunOpen,
        "Maya is heading to Cosmic Cantina",
        "Order by 6:10",
        json!({"run_id": "run_demo"}),
    )
    .await
    .unwrap();
    stub.wait_for(3).await;
    wait_for_rows(&pool, 1).await;
    assert_eq!(
        stored(&pool).await,
        rows(&[("usr_judge", "web", &stub.endpoint("fine"))])
    );

    // The next notification goes only to the one left.
    notify(
        &server.state,
        "usr_judge",
        NotificationType::System,
        "Again",
        "",
        json!({}),
    )
    .await
    .unwrap();
    stub.wait_for(4).await;
    stub.assert_stays_at(4, QUIET).await;
    assert_eq!(
        (
            stub.count_at("gone"),
            stub.count_at("missing"),
            stub.count_at("fine")
        ),
        (1, 1, 2)
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn a_5xx_or_429_is_retried_once_and_other_errors_not_at_all(pool: PgPool) {
    let stub = Stub::start().await;
    let (server, _) = push_server(pool.clone(), Some(&stub), &[], |settings| {
        settings.retry_delay = Duration::from_millis(50);
    })
    .await;
    stub.script("flaky", &[Reply::status(503)]);
    stub.script(
        "busy",
        &[Reply::status(429), Reply::status(429), Reply::status(429)],
    );
    stub.script(
        "broken",
        &[Reply::status(500), Reply::status(500), Reply::status(500)],
    );
    stub.script("refused", &[Reply::status(400), Reply::status(400)]);
    stub.script("forbidden", &[Reply::status(403), Reply::status(403)]);
    let flaky = subscribe_browser(&server, "usr_judge", &stub, "flaky").await;
    for name in ["busy", "broken", "refused", "forbidden"] {
        subscribe_browser(&server, "usr_judge", &stub, name).await;
    }
    notify(
        &server.state,
        "usr_judge",
        NotificationType::Payout,
        "You got $3.50",
        "Fee and tip from Jordan",
        json!({"order_id": "ord_1"}),
    )
    .await
    .unwrap();
    stub.wait_for(8).await;
    stub.assert_stays_at(8, QUIET).await;
    assert_eq!(
        ["flaky", "busy", "broken", "refused", "forbidden"].map(|name| stub.count_at(name)),
        [2, 2, 2, 1, 1]
    );
    // The retry carries the same message.
    let retried: Vec<_> = stub
        .received()
        .into_iter()
        .filter(|r| r.path == "/push/flaky")
        .collect();
    assert_eq!(flaky.open(&retried[0]), flaky.open(&retried[1]));
    // None of these deletes the subscription.
    assert_eq!(stored(&pool).await.len(), 5);
}

#[sqlx::test(migrations = "../migrations")]
async fn a_hanging_push_service_does_not_slow_the_api(pool: PgPool) {
    let stub = Stub::start().await;
    // Production's settings: a 5 s timeout.
    let (server, _) = push_server(pool.clone(), Some(&stub), &[], |_| {}).await;
    stub.script("slow", &[Reply::hang(Duration::from_secs(30)); 4]);
    subscribe_browser(&server, "usr_judge", &stub, "slow").await;
    subscribe_browser(&server, "usr_judge", &stub, "fast").await;

    for message in ["One", "Two"] {
        let started = Instant::now();
        let (status, body) = post_json(
            &server,
            "/v1/circles/cir_hinton4/meet-again",
            "usr_dana",
            &json!({"message": message}),
        )
        .await;
        let took = started.elapsed();
        assert_eq!(status, StatusCode::OK, "{body}");
        // Well under the 5 s push timeout, with room for a busy shared Postgres.
        assert!(took < Duration::from_secs(3), "meet-again took {took:?}");
    }
    // The fast subscription gets both while the slow one hangs.
    realtime_support::eventually("both pushes at the fast endpoint", || {
        stub.count_at("fast") == 2
    })
    .await;
    realtime_support::eventually("both pushes reached the slow endpoint", || {
        stub.count_at("slow") == 2
    })
    .await;
    assert_eq!(stored(&pool).await.len(), 2);
    let (status, _) = server.get_as("/v1/notifications", "usr_judge").await;
    assert_eq!(status, StatusCode::OK);
}

#[sqlx::test(migrations = "../migrations")]
async fn a_timeout_is_not_retried_and_keeps_the_subscription(pool: PgPool) {
    let stub = Stub::start().await;
    let (server, _) = push_server(pool.clone(), Some(&stub), &[], |settings| {
        settings.timeout = Duration::from_millis(300);
        settings.retry_delay = Duration::from_millis(10);
    })
    .await;
    stub.script("slow", &[Reply::hang(Duration::from_secs(5))]);
    subscribe_browser(&server, "usr_judge", &stub, "slow").await;
    notify(
        &server.state,
        "usr_judge",
        NotificationType::System,
        "One",
        "",
        json!({}),
    )
    .await
    .unwrap();
    stub.wait_for(1).await;
    stub.assert_stays_at(1, Duration::from_millis(800)).await;
    assert_eq!(stored(&pool).await.len(), 1);
    // The worker went on: the next one is delivered.
    notify(
        &server.state,
        "usr_judge",
        NotificationType::System,
        "Two",
        "",
        json!({}),
    )
    .await
    .unwrap();
    stub.wait_for(2).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn a_full_queue_drops_pushes_without_blocking(pool: PgPool) {
    let stub = Stub::start().await;
    let (server, _) = push_server(pool.clone(), Some(&stub), &[], |settings| {
        settings.queue = 1;
        settings.concurrency = 1;
    })
    .await;
    stub.script("slow", &[Reply::hang(Duration::from_secs(30)); 60]);
    subscribe_browser(&server, "usr_judge", &stub, "slow").await;
    // Stored first, so the timing below is the hub's alone, not the database's.
    let mut notifications = Vec::new();
    for n in 0..200 {
        let notification = create_notification(
            &server.state.pool,
            "usr_judge",
            NotificationType::System,
            &format!("n{n}"),
            "",
            json!({}),
        )
        .await
        .unwrap();
        notifications.push(notification);
    }
    let started = Instant::now();
    for notification in &notifications {
        server
            .hub()
            .send_to("usr_judge", &Event::notification_created(notification));
        // Let the worker run between sends, as a busy server would.
        tokio::task::yield_now().await;
    }
    let took = started.elapsed();
    assert!(took < Duration::from_millis(200), "200 sends took {took:?}");
    // One in flight; the worker waits for its slot with one more queued; the rest were dropped.
    stub.wait_for(1).await;
    stub.assert_stays_at(1, QUIET).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn a_stored_endpoint_off_the_allowlist_is_never_contacted(pool: PgPool) {
    let stub = Stub::start().await;
    let elsewhere = Stub::start().await;
    let (server, _) = push_server(pool.clone(), Some(&stub), &[], |_| {}).await;
    subscribe_browser(&server, "usr_judge", &stub, "allowed").await;
    // Rows the API would refuse (written straight to the table, as an old config might have).
    let browser = Browser::new(elsewhere.endpoint("x"));
    let keys = browser.to_json()["keys"].clone();
    for endpoint in [
        elsewhere.endpoint("x"),
        "https://127.0.0.1:1/push".to_owned(),
    ] {
        sqlx::query(
            "INSERT INTO ferry.push_subscriptions (user_id, platform, endpoint, p256dh, auth) \
             VALUES ('usr_judge', 'web', $1, $2, $3)",
        )
        .bind(endpoint)
        .bind(keys["p256dh"].as_str())
        .bind(keys["auth"].as_str())
        .execute(&pool)
        .await
        .unwrap();
    }
    notify(
        &server.state,
        "usr_judge",
        NotificationType::System,
        "Hi",
        "",
        json!({}),
    )
    .await
    .unwrap();
    stub.wait_for(1).await;
    stub.assert_stays_at(1, QUIET).await;
    assert_eq!(elsewhere.count(), 0);
}

#[sqlx::test(migrations = "../migrations")]
async fn without_vapid_keys_nothing_is_pushed(pool: PgPool) {
    let stub = Stub::start().await;
    seed_hinton4(&pool).await;
    let state = AppState::new(
        pool.clone(),
        common::config(&[("PUSH_TEST_STUB_ORIGIN", &stub.origin)]),
    )
    .unwrap();
    assert!(!ferry_core::notifications::push::attach(&state));
    let server = serve(state).await;
    let browser = Browser::new(stub.endpoint("judge"));
    let keys = browser.to_json()["keys"].clone();
    sqlx::query(
        "INSERT INTO ferry.push_subscriptions (user_id, platform, endpoint, p256dh, auth) \
         VALUES ('usr_judge', 'web', $1, $2, $3)",
    )
    .bind(&browser.endpoint)
    .bind(keys["p256dh"].as_str())
    .bind(keys["auth"].as_str())
    .execute(&pool)
    .await
    .unwrap();
    let mut ws = server.connect_with_token("usr_judge").await;
    notify(
        &server.state,
        "usr_judge",
        NotificationType::System,
        "Hi",
        "",
        json!({}),
    )
    .await
    .unwrap();
    // The stream still gets it; the push service doesn't.
    next_event(&mut ws, "notification.created").await;
    stub.assert_stays_at(0, QUIET).await;
}
