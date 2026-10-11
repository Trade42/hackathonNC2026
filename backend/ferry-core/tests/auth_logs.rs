//! No Telegram sign-in challenge id reaches a log line (PR #17 review MINOR 1). Every log line of a
//! whole sign-in is captured and searched for the ids and secrets it used.
//!
//! This is the only test in its binary on purpose. The capture is a thread-local subscriber, and
//! tracing caches each callsite's interest when the callsite is first hit: a callsite first hit on
//! another test's thread while this one installs its subscriber can stay cached as uninterested,
//! and its lines would silently go missing.
mod auth_support;
mod common;

use auth_support::{STUB_SECRET, Stub, app_with, call, capture_logs, json_request, post};
use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;

const BOT: &str = "Ferrygo_bot";
const TG: &str = "987654321";

/// An event from the Telegram account `TG` in its DM, as the sidecar sends it.
fn event(kind: &str, text: &str, callback_data: Option<&str>) -> Value {
    json!({
        "kind": kind, "channel": "telegram", "space_id": TG, "is_group": false,
        "participants": [TG], "sender_phone": null, "sender_id": TG, "sender_name": "Maya",
        "sender_username": "maya_l", "chat_title": null, "text": text, "attachments": [],
        "location": null, "callback_data": callback_data,
        "received_at": "2026-10-11T02:00:00.000Z"
    })
}

async fn inbound(app: &Router, event: Value) {
    let (status, body) = call(
        app,
        json_request(
            "POST",
            "/internal/messages/inbound",
            &event,
            &[("x-ferry-secret", STUB_SECRET)],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn poll(app: &Router, challenge_id: &str) -> Value {
    let (status, body) = call(
        app,
        common::get(&format!("/v1/auth/telegram/{challenge_id}")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

#[sqlx::test(migrations = "../migrations")]
async fn no_challenge_id_reaches_a_log_line(pool: PgPool) {
    let capture = capture_logs();
    let stub = Stub::start().await;
    let (app, _) = app_with(
        pool,
        &[
            ("SIDECAR_URL", stub.base.as_str()),
            ("SIDECAR_SHARED_SECRET", STUB_SECRET),
            ("TELEGRAM_BOT_USERNAME", BOT),
        ],
    );

    // Start, poll, /start, Confirm, poll (the token), poll again.
    let (status, started) = call(&app, post("/v1/auth/telegram/start", json!(null))).await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let challenge_id = started["challenge_id"].as_str().unwrap().to_owned();
    let code = started["bot_url"]
        .as_str()
        .unwrap()
        .rsplit_once("?start=")
        .unwrap()
        .1
        .to_owned();
    assert_eq!(poll(&app, &challenge_id).await["status"], "pending");
    inbound(&app, event("message", &format!("/start {code}"), None)).await;
    let sent = stub.wait_for(1).await;
    let confirm = sent[0].body["buttons"][0]["callback_data"]
        .as_str()
        .unwrap()
        .to_owned();
    inbound(&app, event("callback", "", Some(&confirm))).await;
    let verified = poll(&app, &challenge_id).await;
    let token = verified["token"].as_str().unwrap().to_owned();
    assert_eq!(
        poll(&app, &challenge_id).await,
        json!({"status": "verified"})
    );

    // An unknown id, the wrong method on a challenge's path, and a path under it.
    let unknown = "chl_zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz";
    let (status, _) = call(&app, common::get(&format!("/v1/auth/telegram/{unknown}"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &app,
        post(&format!("/v1/auth/telegram/{challenge_id}"), json!(null)),
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let (status, _) = call(
        &app,
        common::get(&format!("/v1/auth/telegram/{challenge_id}/x")),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // The "signed in" line goes back as a reply; the prompt was the only send.
    stub.assert_no_more_than(1).await;

    let logs = capture.logs.text();
    // The capture works: every request is there, the polls with the redacted path.
    assert!(
        logs.contains("path=/v1/auth/telegram/:id"),
        "no redacted poll in:\n{logs}"
    );
    assert!(logs.contains("path=/v1/auth/telegram/start"), "{logs}");
    assert!(logs.contains("path=/internal/messages/inbound"), "{logs}");
    assert!(logs.contains("signed in with Telegram"), "{logs}");
    for secret in [
        challenge_id.as_str(),
        &challenge_id[4..],
        unknown,
        code.as_str(),
        confirm.as_str(),
        token.as_str(),
        TG,
        "maya_l",
    ] {
        assert!(!logs.contains(secret), "{secret} is in the logs:\n{logs}");
    }
}
