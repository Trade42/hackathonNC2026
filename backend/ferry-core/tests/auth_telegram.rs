//! Telegram sign-in end to end: start, the bot's /start and its Confirm and Not me buttons
//! through POST /internal/messages/inbound, the poll and /v1/me, against a stub sidecar.
mod auth_support;
mod common;

use auth_support::{
    STUB_SECRET, Stub, app_with, assert_contract, assert_error, call, get_with_token, json_request,
    post, seed_personas,
};
use axum::Router;
use axum::http::StatusCode;
use ferry_core::auth::telegram::{
    ALREADY_SIGNED_IN, CANCELLED, EXPIRED, LINK_TAKEN, LINK_USED, NOT_YOURS, SIGNED_IN, WELCOME,
    prompt_text,
};
use serde_json::{Value, json};
use sqlx::PgPool;

const BOT: &str = "Ferrygo_bot";
const MAYA_TG: &str = "987654321";
const OTHER_TG: &str = "123456789";

/// The app wired to `stub`, with the bot configured, plus `extra`.
fn app(pool: PgPool, stub: &Stub, extra: &[(&str, &str)]) -> Router {
    let mut pairs = vec![
        ("SIDECAR_URL", stub.base.as_str()),
        ("SIDECAR_SHARED_SECRET", STUB_SECRET),
        ("TELEGRAM_BOT_USERNAME", BOT),
    ];
    pairs.extend_from_slice(extra);
    app_with(pool, &pairs).0
}

/// A started challenge: its id, the start code in its bot_url, and the confirm code the client
/// shows.
struct Started {
    id: String,
    code: String,
    confirm_code: String,
}

/// Starts a challenge.
async fn start_challenge(app: &Router) -> Started {
    let (status, body) = call(app, post("/v1/auth/telegram/start", json!(null))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_contract("TelegramAuthChallenge", &body);
    let url = body["bot_url"].as_str().unwrap();
    let code = url
        .strip_prefix(&format!("https://t.me/{BOT}?start="))
        .unwrap_or_else(|| panic!("unexpected bot_url {url}"));
    Started {
        id: body["challenge_id"].as_str().unwrap().to_owned(),
        code: code.to_owned(),
        confirm_code: body["confirm_code"].as_str().unwrap().to_owned(),
    }
}

/// Starts a challenge and returns (challenge_id, start code).
async fn start(app: &Router) -> (String, String) {
    let started = start_challenge(app).await;
    (started.id, started.code)
}

async fn poll(app: &Router, challenge_id: &str) -> Value {
    let (status, body) = call(
        app,
        common::get(&format!("/v1/auth/telegram/{challenge_id}")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_contract("TelegramAuthStatus", &body);
    body
}

/// A Telegram DM from `sender_id` (whose DM's space id is the same id), as the sidecar sends it.
fn dm(sender_id: &str, username: Option<&str>, text: &str) -> Value {
    json!({
        "kind": "message", "channel": "telegram", "space_id": sender_id, "is_group": false,
        "participants": [sender_id], "sender_phone": null, "sender_id": sender_id,
        "sender_name": "Maya", "sender_username": username, "chat_title": null, "text": text,
        "attachments": [], "location": null, "received_at": "2026-10-11T02:00:00.000Z"
    })
}

/// A button press by `sender_id` in their DM.
fn press(sender_id: &str, username: Option<&str>, callback_data: &str) -> Value {
    json!({
        "kind": "callback", "channel": "telegram", "space_id": sender_id, "is_group": false,
        "participants": [sender_id], "sender_phone": null, "sender_id": sender_id,
        "sender_name": "Maya", "sender_username": username, "chat_title": null, "text": "",
        "attachments": [], "location": null, "callback_data": callback_data,
        "received_at": "2026-10-11T02:00:00.000Z"
    })
}

/// Posts `event` to the inbound route with the right secret; returns the replies.
async fn inbound(app: &Router, event: Value) -> Vec<Value> {
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
    body["replies"].as_array().unwrap().clone()
}

/// The one reply `replies` holds, to `space_id`: its text.
fn only_reply(replies: &[Value], space_id: &str) -> String {
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_eq!(replies[0]["space_id"], space_id);
    replies[0]["text"].as_str().unwrap().to_owned()
}

/// The Confirm and Not me callback data of a prompt the stub received, after checking that it
/// shows `confirm_code`.
fn prompt_buttons(sent: &Value, space_id: &str, confirm_code: &str) -> (String, String) {
    assert_eq!(sent["space_id"], space_id, "{sent}");
    assert_eq!(
        sent["text"],
        format!("Sign in to Ferry on another device? Check that the screen shows {confirm_code}."),
        "{sent}"
    );
    assert_eq!(sent["text"], prompt_text(confirm_code), "{sent}");
    let buttons = sent["buttons"].as_array().unwrap();
    assert_eq!(buttons.len(), 2, "{sent}");
    assert_eq!(buttons[0]["label"], "Confirm");
    assert_eq!(buttons[1]["label"], "Not me");
    let confirm = buttons[0]["callback_data"].as_str().unwrap().to_owned();
    let cancel = buttons[1]["callback_data"].as_str().unwrap().to_owned();
    assert!(confirm.len() <= 64 && cancel.len() <= 64, "{sent}");
    (confirm, cancel)
}

/// start, then `/start <code>` from `sender_id`: returns (challenge_id, confirm, not_me). The
/// prompt must show the confirm code the start returned.
async fn start_and_bind(
    app: &Router,
    stub: &Stub,
    sender_id: &str,
    username: Option<&str>,
) -> (String, String, String) {
    let started = start_challenge(app).await;
    let before = stub.sent().len();
    let text = format!("/start {}", started.code);
    let replies = inbound(app, dm(sender_id, username, &text)).await;
    assert!(replies.is_empty(), "{replies:?}");
    let sent = stub.wait_for(before + 1).await;
    assert_eq!(sent[before].secret.as_deref(), Some(STUB_SECRET));
    let (confirm, not_me) = prompt_buttons(&sent[before].body, sender_id, &started.confirm_code);
    (started.id, confirm, not_me)
}

async fn user_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM ferry.users")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../migrations")]
async fn start_returns_a_bot_link_and_a_pending_challenge(pool: PgPool) {
    let stub = Stub::start().await;
    let app = app(pool.clone(), &stub, &[]);
    let Started {
        id: challenge_id,
        code,
        confirm_code,
    } = start_challenge(&app).await;
    assert!(
        challenge_id.starts_with("chl_") && challenge_id.len() == 36,
        "{challenge_id}"
    );
    assert_eq!(code.len(), 24);
    assert!(code.bytes().all(|b| b.is_ascii_alphanumeric()), "{code}");
    assert_ne!(code, challenge_id);
    // Four digits, kept with the challenge for the bot's prompt.
    assert_eq!(confirm_code.len(), 4, "{confirm_code}");
    assert!(
        confirm_code.bytes().all(|b| b.is_ascii_digit()),
        "{confirm_code}"
    );
    let stored: String =
        sqlx::query_scalar("SELECT confirm_code FROM ferry.telegram_challenges WHERE id = $1")
            .bind(&challenge_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, confirm_code);
    assert_eq!(
        poll(&app, &challenge_id).await,
        json!({"status": "pending"})
    );
    // Starting sends nothing.
    stub.assert_no_more_than(0).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn start_expires_in_ten_minutes(pool: PgPool) {
    let stub = Stub::start().await;
    let app = app(pool.clone(), &stub, &[]);
    let (challenge_id, _) = start(&app).await;
    let minutes: f64 = sqlx::query_scalar(
        "SELECT extract(epoch FROM expires_at - created_at)::float8 / 60
           FROM ferry.telegram_challenges WHERE id = $1",
    )
    .bind(&challenge_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!((minutes - 10.0).abs() < 0.01, "{minutes}");
}

#[sqlx::test(migrations = "../migrations")]
async fn start_without_a_bot_is_503(pool: PgPool) {
    let (app, _) = app_with(pool, &[]);
    let (status, body) = call(&app, post("/v1/auth/telegram/start", json!(null))).await;
    assert_error(
        status,
        &body,
        StatusCode::SERVICE_UNAVAILABLE,
        "upstream_unavailable",
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn start_binds_without_verifying_and_confirm_signs_in(pool: PgPool) {
    let stub = Stub::start().await;
    let app = app(pool.clone(), &stub, &[]);
    let (challenge_id, confirm, _) = start_and_bind(&app, &stub, MAYA_TG, Some("maya_l")).await;
    // /start alone verifies nothing.
    assert_eq!(
        poll(&app, &challenge_id).await,
        json!({"status": "pending"})
    );
    assert_eq!(user_count(&pool).await, 0);

    let replies = inbound(&app, press(MAYA_TG, Some("maya_l"), &confirm)).await;
    // PUBLIC_WEB_URL is unset, so the app link is in the text, not a button.
    assert_eq!(
        only_reply(&replies, MAYA_TG),
        format!("{SIGNED_IN} http://localhost:5173")
    );
    let body = poll(&app, &challenge_id).await;
    assert_eq!(body["status"], "verified");
    assert_eq!(body["is_new"], true);
    let user = &body["user"];
    assert!(user["id"].as_str().unwrap().starts_with("usr_"), "{user}");
    assert_eq!(user["telegram_username"], "maya_l");
    assert_eq!(user["phone_masked"], Value::Null);
    assert_eq!(user["display_name"], "");
    assert_eq!(user["onboarding_complete"], false);

    let token = body["token"].as_str().unwrap();
    let (status, me) = call(&app, get_with_token("/v1/me", token)).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_contract("User", &me);
    assert_eq!(&me, user);
    assert_eq!(me["telegram_username"], "maya_l");

    let telegram_user_id: String =
        sqlx::query_scalar("SELECT telegram_user_id FROM ferry.users WHERE id = $1")
            .bind(user["id"].as_str().unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(telegram_user_id, MAYA_TG);
    // The prompt was the only send.
    stub.assert_no_more_than(1).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn a_user_without_a_username_has_a_null_one(pool: PgPool) {
    let stub = Stub::start().await;
    let app = app(pool, &stub, &[]);
    let (challenge_id, confirm, _) = start_and_bind(&app, &stub, MAYA_TG, None).await;
    inbound(&app, press(MAYA_TG, None, &confirm)).await;
    let body = poll(&app, &challenge_id).await;
    assert_eq!(body["user"]["telegram_username"], Value::Null);
}

#[sqlx::test(migrations = "../migrations")]
async fn a_confirm_from_another_account_does_nothing(pool: PgPool) {
    let stub = Stub::start().await;
    let app = app(pool.clone(), &stub, &[]);
    let (challenge_id, confirm, cancel) =
        start_and_bind(&app, &stub, MAYA_TG, Some("maya_l")).await;

    for data in [&confirm, &cancel] {
        let replies = inbound(&app, press(OTHER_TG, Some("mallory"), data)).await;
        assert_eq!(only_reply(&replies, OTHER_TG), NOT_YOURS);
        assert_eq!(
            poll(&app, &challenge_id).await,
            json!({"status": "pending"})
        );
    }
    assert_eq!(user_count(&pool).await, 0);

    // The bound account still can.
    inbound(&app, press(MAYA_TG, Some("maya_l"), &confirm)).await;
    assert_eq!(poll(&app, &challenge_id).await["status"], "verified");
}

#[sqlx::test(migrations = "../migrations")]
async fn not_me_expires_the_challenge(pool: PgPool) {
    let stub = Stub::start().await;
    let app = app(pool.clone(), &stub, &[]);
    let (challenge_id, confirm, not_me) =
        start_and_bind(&app, &stub, MAYA_TG, Some("maya_l")).await;
    assert!(not_me.ends_with(":cancel"), "{not_me}");
    let replies = inbound(&app, press(MAYA_TG, Some("maya_l"), &not_me)).await;
    assert_eq!(only_reply(&replies, MAYA_TG), CANCELLED);
    assert_eq!(
        poll(&app, &challenge_id).await,
        json!({"status": "expired"})
    );

    // Confirm after Not me does nothing.
    let replies = inbound(&app, press(MAYA_TG, Some("maya_l"), &confirm)).await;
    assert_eq!(only_reply(&replies, MAYA_TG), EXPIRED);
    assert_eq!(
        poll(&app, &challenge_id).await,
        json!({"status": "expired"})
    );
    assert_eq!(user_count(&pool).await, 0);
}

#[sqlx::test(migrations = "../migrations")]
async fn replays_mint_nothing_new(pool: PgPool) {
    let stub = Stub::start().await;
    let app = app(pool.clone(), &stub, &[]);
    let Started {
        id: challenge_id,
        code,
        confirm_code,
    } = start_challenge(&app).await;
    let start_text = format!("/start {code}");

    // /start twice before confirming: the same prompt, the same code, the same token.
    inbound(&app, dm(MAYA_TG, Some("maya_l"), &start_text)).await;
    inbound(&app, dm(MAYA_TG, Some("maya_l"), &start_text)).await;
    let sent = stub.wait_for(2).await;
    let first = prompt_buttons(&sent[0].body, MAYA_TG, &confirm_code);
    assert_eq!(prompt_buttons(&sent[1].body, MAYA_TG, &confirm_code), first);

    // Another account's /start with the same code: refused, binding unchanged.
    let replies = inbound(&app, dm(OTHER_TG, Some("mallory"), &start_text)).await;
    assert_eq!(only_reply(&replies, OTHER_TG), LINK_TAKEN);

    inbound(&app, press(MAYA_TG, Some("maya_l"), &first.0)).await;
    let verified = poll(&app, &challenge_id).await;
    let user_id = verified["user"]["id"].clone();
    assert_eq!(user_count(&pool).await, 1);

    // A replayed Confirm, a Cancel and a /start after verification change nothing.
    let replies = inbound(&app, press(MAYA_TG, Some("maya_l"), &first.0)).await;
    assert_eq!(
        only_reply(&replies, MAYA_TG),
        format!("{ALREADY_SIGNED_IN} http://localhost:5173")
    );
    let replies = inbound(&app, press(MAYA_TG, Some("maya_l"), &first.1)).await;
    assert_eq!(
        only_reply(&replies, MAYA_TG),
        format!("{ALREADY_SIGNED_IN} http://localhost:5173")
    );
    let replies = inbound(&app, dm(MAYA_TG, Some("maya_l"), &start_text)).await;
    assert_eq!(
        only_reply(&replies, MAYA_TG),
        format!("{LINK_USED} http://localhost:5173")
    );
    assert_eq!(user_count(&pool).await, 1);
    // The token went out on the first poll; this one has only the status.
    assert_eq!(verified["user"]["id"], user_id);
    assert_eq!(verified["is_new"], true);
    assert_eq!(
        poll(&app, &challenge_id).await,
        json!({"status": "verified"})
    );
    let verified_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ferry.telegram_challenges WHERE status = 'verified'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(verified_count, 1);
    stub.assert_no_more_than(2).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn a_second_sign_in_finds_the_same_user(pool: PgPool) {
    let stub = Stub::start().await;
    let app = app(pool.clone(), &stub, &[]);
    let (first, confirm, _) = start_and_bind(&app, &stub, MAYA_TG, Some("maya_l")).await;
    inbound(&app, press(MAYA_TG, Some("maya_l"), &confirm)).await;
    let user_id = poll(&app, &first).await["user"]["id"].clone();

    // The account changed its username since.
    let (second, confirm, _) = start_and_bind(&app, &stub, MAYA_TG, Some("maya_new")).await;
    inbound(&app, press(MAYA_TG, Some("maya_new"), &confirm)).await;
    let body = poll(&app, &second).await;
    assert_eq!(body["user"]["id"], user_id);
    assert_eq!(body["is_new"], false);
    assert_eq!(body["user"]["telegram_username"], "maya_new");
    assert_eq!(user_count(&pool).await, 1);
}

#[sqlx::test(migrations = "../migrations")]
async fn demo_personas_land_on_their_seeded_users(pool: PgPool) {
    seed_personas(&pool).await;
    let stub = Stub::start().await;
    let personas = [
        ("DEMO_MODE", "true"),
        ("DEMO_TELEGRAM_PERSONAS", "maya=@maya_demo"),
    ];
    let demo = app(pool.clone(), &stub, &personas);
    let users_before = user_count(&pool).await;

    // The username matches without case.
    let (challenge_id, confirm, _) = start_and_bind(&demo, &stub, MAYA_TG, Some("Maya_Demo")).await;
    inbound(&demo, press(MAYA_TG, Some("Maya_Demo"), &confirm)).await;
    let body = poll(&demo, &challenge_id).await;
    assert_eq!(body["user"]["id"], "usr_maya");
    assert_eq!(body["is_new"], false);
    assert_eq!(body["user"]["display_name"], "Maya");
    assert_eq!(body["user"]["telegram_username"], "Maya_Demo");
    assert_eq!(body["user"]["phone_masked"], "+1 ••• ••• 0102");
    assert_eq!(user_count(&pool).await, users_before);
    let (_, me) = call(
        &demo,
        get_with_token("/v1/me", body["token"].as_str().unwrap()),
    )
    .await;
    assert_eq!(me["id"], "usr_maya");
    assert_contract("User", &me);

    // Without DEMO_MODE the variable is ignored: another account with a listed username gets a
    // new user.
    let live = app(pool.clone(), &stub, &[personas[1]]);
    let (challenge_id, confirm, _) =
        start_and_bind(&live, &stub, OTHER_TG, Some("maya_demo")).await;
    inbound(&live, press(OTHER_TG, Some("maya_demo"), &confirm)).await;
    let body = poll(&live, &challenge_id).await;
    assert_eq!(body["is_new"], true);
    assert_ne!(body["user"]["id"], "usr_maya");
    assert_eq!(user_count(&pool).await, users_before + 1);
}

#[sqlx::test(migrations = "../migrations")]
async fn an_unlisted_username_in_demo_mode_is_a_new_user(pool: PgPool) {
    seed_personas(&pool).await;
    let stub = Stub::start().await;
    let app = app(
        pool,
        &stub,
        &[
            ("DEMO_MODE", "true"),
            ("DEMO_TELEGRAM_PERSONAS", "maya=@maya_demo"),
        ],
    );
    let (challenge_id, confirm, _) = start_and_bind(&app, &stub, OTHER_TG, Some("dana_x")).await;
    inbound(&app, press(OTHER_TG, Some("dana_x"), &confirm)).await;
    let body = poll(&app, &challenge_id).await;
    assert_eq!(body["is_new"], true);
    assert!(!["usr_maya", "usr_dana"].contains(&body["user"]["id"].as_str().unwrap()));
}

#[sqlx::test(migrations = "../migrations")]
async fn expired_challenges(pool: PgPool) {
    let stub = Stub::start().await;
    let app = app(pool.clone(), &stub, &[]);
    let expire = |id: String| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "UPDATE ferry.telegram_challenges
                    SET expires_at = now() - interval '1 second' WHERE id = $1",
            )
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        }
    };

    // Unbound and past its time: expired on the poll, and /start says so.
    let (challenge_id, code) = start(&app).await;
    expire(challenge_id.clone()).await;
    assert_eq!(
        poll(&app, &challenge_id).await,
        json!({"status": "expired"})
    );
    let replies = inbound(&app, dm(MAYA_TG, None, &format!("/start {code}"))).await;
    assert_eq!(
        only_reply(&replies, MAYA_TG),
        format!("{LINK_USED} http://localhost:5173")
    );

    // Bound, then past its time: Confirm does nothing.
    let (challenge_id, confirm, _) = start_and_bind(&app, &stub, MAYA_TG, None).await;
    expire(challenge_id.clone()).await;
    let replies = inbound(&app, press(MAYA_TG, None, &confirm)).await;
    assert_eq!(only_reply(&replies, MAYA_TG), EXPIRED);
    assert_eq!(
        poll(&app, &challenge_id).await,
        json!({"status": "expired"})
    );
    assert_eq!(user_count(&pool).await, 0);
}

async fn token_issued_at(pool: &PgPool, challenge_id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT token_issued_at::text FROM ferry.telegram_challenges WHERE id = $1")
        .bind(challenge_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../migrations")]
async fn the_token_is_handed_out_once(pool: PgPool) {
    let stub = Stub::start().await;
    let app = app(pool.clone(), &stub, &[]);
    let (challenge_id, confirm, _) = start_and_bind(&app, &stub, MAYA_TG, None).await;
    // Pending polls hand out nothing and mark nothing.
    assert_eq!(
        poll(&app, &challenge_id).await,
        json!({"status": "pending"})
    );
    inbound(&app, press(MAYA_TG, None, &confirm)).await;
    assert_eq!(token_issued_at(&pool, &challenge_id).await, None);

    let first = poll(&app, &challenge_id).await;
    let token = first["token"].as_str().unwrap();
    assert_eq!(first["is_new"], true);
    let (status, me) = call(&app, get_with_token("/v1/me", token)).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert!(token_issued_at(&pool, &challenge_id).await.is_some());

    // Every later poll, inside the minute, has only the status.
    for _ in 0..3 {
        assert_eq!(
            poll(&app, &challenge_id).await,
            json!({"status": "verified"})
        );
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn polls_at_the_same_moment_get_one_token(pool: PgPool) {
    let stub = Stub::start().await;
    let app = app(pool.clone(), &stub, &[]);
    let (challenge_id, confirm, _) = start_and_bind(&app, &stub, MAYA_TG, None).await;
    inbound(&app, press(MAYA_TG, None, &confirm)).await;
    let (a, b, c, d) = tokio::join!(
        poll(&app, &challenge_id),
        poll(&app, &challenge_id),
        poll(&app, &challenge_id),
        poll(&app, &challenge_id),
    );
    let with_token = [a, b, c, d]
        .iter()
        .filter(|body| {
            assert_eq!(body["status"], "verified", "{body}");
            body.get("token").is_some()
        })
        .count();
    assert_eq!(with_token, 1);
}

#[sqlx::test(migrations = "../migrations")]
async fn a_token_not_collected_within_a_minute_is_never_handed_out(pool: PgPool) {
    let stub = Stub::start().await;
    let app = app(pool.clone(), &stub, &[]);
    let (challenge_id, confirm, _) = start_and_bind(&app, &stub, MAYA_TG, None).await;
    inbound(&app, press(MAYA_TG, None, &confirm)).await;
    sqlx::query(
        "UPDATE ferry.telegram_challenges SET verified_at = now() - interval '61 seconds'
          WHERE id = $1",
    )
    .bind(&challenge_id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        poll(&app, &challenge_id).await,
        json!({"status": "verified"})
    );
    assert_eq!(token_issued_at(&pool, &challenge_id).await, None);
}

#[sqlx::test(migrations = "../migrations")]
async fn an_unknown_challenge_is_404(pool: PgPool) {
    let stub = Stub::start().await;
    let app = app(pool, &stub, &[]);
    for id in ["chl_00000000000000000000000000000000", "nope"] {
        let (status, body) = call(&app, common::get(&format!("/v1/auth/telegram/{id}"))).await;
        assert_error(status, &body, StatusCode::NOT_FOUND, "not_found");
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn start_is_rate_limited_per_address_and_overall(pool: PgPool) {
    let stub = Stub::start().await;
    let app = app(pool.clone(), &stub, &[]);
    let from = |ip: &'static str| {
        json_request(
            "POST",
            "/v1/auth/telegram/start",
            &json!(null),
            &[("cf-connecting-ip", ip)],
        )
    };
    for i in 0..20 {
        let (status, body) = call(&app, from("203.0.113.7")).await;
        assert_eq!(status, StatusCode::OK, "start {i}: {body}");
    }
    let (status, body) = call(&app, from("203.0.113.7")).await;
    assert_error(status, &body, StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    // Another address still starts.
    let (status, body) = call(&app, from("198.51.100.9")).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 200 starts in the window from anywhere: everyone waits.
    sqlx::query(
        "INSERT INTO ferry.telegram_challenges (id, start_code, client_ip, confirm_code, expires_at)
         SELECT 'chl_' || lpad(i::text, 32, '0'), 'code' || lpad(i::text, 20, '0'),
                '10.0.' || (i / 250) || '.' || (i % 250), '0000', now() + interval '10 minutes'
           FROM generate_series(1, 179) AS i",
    )
    .execute(&pool)
    .await
    .unwrap();
    let (status, body) = call(&app, from("192.0.2.1")).await;
    assert_error(status, &body, StatusCode::TOO_MANY_REQUESTS, "rate_limited");

    // Starts older than the window no longer count.
    sqlx::query("UPDATE ferry.telegram_challenges SET created_at = now() - interval '11 minutes'")
        .execute(&pool)
        .await
        .unwrap();
    let (status, body) = call(&app, from("203.0.113.7")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[sqlx::test(migrations = "../migrations")]
async fn start_without_a_code_gets_the_welcome(pool: PgPool) {
    let stub = Stub::start().await;
    // An https web URL: the app link is a button, sent through /send.
    let app = app(pool, &stub, &[("PUBLIC_WEB_URL", "https://ferry.example")]);
    for text in [
        "/start",
        "/start not-a-code!",
        "/start abcdefghijklmnopqrstuvwx",
    ] {
        let before = stub.sent().len();
        let replies = inbound(&app, dm(MAYA_TG, Some("maya_l"), text)).await;
        assert!(replies.is_empty(), "{text}: {replies:?}");
        let sent = stub.wait_for(before + 1).await;
        assert_eq!(
            sent[before].body,
            json!({"space_id": MAYA_TG, "text": WELCOME,
                   "buttons": [{"label": "Open Ferry", "url": "https://ferry.example"}]}),
            "{text}"
        );
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn signed_in_line_has_a_link_button_with_an_https_web_url(pool: PgPool) {
    let stub = Stub::start().await;
    let app = app(pool, &stub, &[("PUBLIC_WEB_URL", "https://ferry.example")]);
    let (challenge_id, confirm, _) = start_and_bind(&app, &stub, MAYA_TG, None).await;
    let replies = inbound(&app, press(MAYA_TG, None, &confirm)).await;
    assert!(replies.is_empty(), "{replies:?}");
    let sent = stub.wait_for(2).await;
    assert_eq!(
        sent[1].body,
        json!({"space_id": MAYA_TG, "text": SIGNED_IN,
               "buttons": [{"label": "Open Ferry", "url": "https://ferry.example"}]})
    );
    assert_eq!(poll(&app, &challenge_id).await["status"], "verified");
}

#[sqlx::test(migrations = "../migrations")]
async fn group_messages_and_other_events_get_no_replies(pool: PgPool) {
    let stub = Stub::start().await;
    let app = app(pool.clone(), &stub, &[]);
    let (challenge_id, code) = start(&app).await;
    let mut group = dm(MAYA_TG, None, &format!("/start@{BOT} {code}"));
    group["is_group"] = json!(true);
    group["space_id"] = json!("-1001234567890");
    let mut membership = dm(MAYA_TG, None, "");
    membership["kind"] = json!("membership");
    membership["bot_in_chat"] = json!(true);
    let v1 = json!({"space_id": "any;-;+19195550102", "is_group": false,
                    "participants": ["+19195550102"], "sender_phone": "+19195550102",
                    "text": format!("/start {code}"), "attachments": [], "location": null,
                    "received_at": "2026-10-11T02:00:00.000Z"});
    for event in [
        group,
        dm(MAYA_TG, None, "heading to Cosmic Cantina at 6"),
        membership,
        press(MAYA_TG, None, "invite:inv_x:accept"),
        v1,
        json!({}),
    ] {
        assert!(inbound(&app, event.clone()).await.is_empty(), "{event}");
    }
    assert_eq!(
        poll(&app, &challenge_id).await,
        json!({"status": "pending"})
    );
    let bound: Option<String> =
        sqlx::query_scalar("SELECT telegram_user_id FROM ferry.telegram_challenges WHERE id = $1")
            .bind(&challenge_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(bound, None);
    // Nothing here signs in. The agent (src/agent/dispatch.rs) tells an unknown Telegram sender
    // once, through /send, to sign in in the app (the app's URL follows, as PUBLIC_WEB_URL is not
    // https here): that is the one send, for the DM trip message.
    let sent = stub.wait_for(1).await;
    assert_eq!(sent[0].body["space_id"], MAYA_TG);
    let text = sent[0].body["text"].as_str().unwrap();
    assert!(
        text.starts_with(ferry_core::agent::dispatch::UNKNOWN_SENDER),
        "{text}"
    );
    stub.assert_no_more_than(1).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn a_bad_button_token_gets_one_line(pool: PgPool) {
    let stub = Stub::start().await;
    let app = app(pool, &stub, &[]);
    for data in [
        "signin:AAAAAAAAAAAAAAAAAAAAAA:confirm",
        "signin:short:confirm",
        "signin::cancel",
    ] {
        let replies = inbound(&app, press(MAYA_TG, None, data)).await;
        assert_eq!(
            only_reply(&replies, MAYA_TG),
            ferry_core::auth::telegram::BUTTON_GONE,
            "{data}"
        );
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn the_inbound_route_needs_the_secret(pool: PgPool) {
    let stub = Stub::start().await;
    let (app_secret, _) = app_with(
        pool.clone(),
        &[
            ("SIDECAR_URL", stub.base.as_str()),
            ("SIDECAR_SHARED_SECRET", STUB_SECRET),
            ("TELEGRAM_BOT_USERNAME", BOT),
        ],
    );
    let event = dm(MAYA_TG, None, "/start");
    for headers in [
        &[][..],
        &[("x-ferry-secret", "wrong")][..],
        &[("x-ferry-secret", "")][..],
        &[("x-ferry-secret", "stub-sidecar-secret ")][..],
    ] {
        let (status, body) = call(
            &app_secret,
            json_request("POST", "/internal/messages/inbound", &event, headers),
        )
        .await;
        assert_error(status, &body, StatusCode::UNAUTHORIZED, "unauthorized");
    }
    // Checked before the body.
    let (status, body) = call(
        &app_secret,
        json_request(
            "POST",
            "/internal/messages/inbound",
            &json!("not an object"),
            &[("x-ferry-secret", "wrong")],
        ),
    )
    .await;
    assert_error(status, &body, StatusCode::UNAUTHORIZED, "unauthorized");

    // Without SIDECAR_SHARED_SECRET nothing gets in.
    let (unset, _) = app_with(pool, &[("TELEGRAM_BOT_USERNAME", BOT)]);
    for headers in [&[][..], &[("x-ferry-secret", "")][..]] {
        let (status, body) = call(
            &unset,
            json_request("POST", "/internal/messages/inbound", &event, headers),
        )
        .await;
        assert_error(status, &body, StatusCode::UNAUTHORIZED, "unauthorized");
    }
    stub.assert_no_more_than(0).await;
}

/// An inbound request whose body is `body`, with `secret` in `X-Ferry-Secret`.
fn inbound_streaming(
    secret: &str,
    body: axum::body::Body,
) -> axum::http::Request<axum::body::Body> {
    axum::http::Request::post("/internal/messages/inbound")
        .header("content-type", "application/json")
        .header("x-ferry-secret", secret)
        .body(body)
        .unwrap()
}

/// A body of one chunk that sets `read` when anything reads it.
fn watched_body(
    read: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    json: Value,
) -> axum::body::Body {
    let read = read.clone();
    axum::body::Body::from_stream(futures_util::stream::once(async move {
        read.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok::<_, std::io::Error>(axum::body::Bytes::from(json.to_string()))
    }))
}

#[sqlx::test(migrations = "../migrations")]
async fn a_wrong_secret_is_refused_before_the_body_is_read(pool: PgPool) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    let stub = Stub::start().await;
    let app = app(pool, &stub, &[]);

    // A body that never ends: reading it first would hang until the timeout.
    let endless = axum::body::Body::from_stream(futures_util::stream::pending::<
        Result<axum::body::Bytes, std::io::Error>,
    >());
    let (status, body) = tokio::time::timeout(
        Duration::from_secs(5),
        call(&app, inbound_streaming("wrong", endless)),
    )
    .await
    .expect("the refusal must not wait for the body");
    assert_error(status, &body, StatusCode::UNAUTHORIZED, "unauthorized");

    // A body that records being read: refused unread.
    let read = Arc::new(AtomicBool::new(false));
    let event = dm(MAYA_TG, None, "hi");
    let (status, body) = call(
        &app,
        inbound_streaming("wrong", watched_body(&read, event.clone())),
    )
    .await;
    assert_error(status, &body, StatusCode::UNAUTHORIZED, "unauthorized");
    assert!(!read.load(Ordering::SeqCst), "the body was read");

    // The same body with the right secret is read and parsed.
    let (status, body) = call(
        &app,
        inbound_streaming(STUB_SECRET, watched_body(&read, event)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"replies": []}));
    assert!(read.load(Ordering::SeqCst));

    // And a bad body with the right secret is still 400.
    let (status, body) = call(
        &app,
        json_request(
            "POST",
            "/internal/messages/inbound",
            &json!("not an object"),
            &[("x-ferry-secret", STUB_SECRET)],
        ),
    )
    .await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "validation_failed");
    // The DM read with the right secret came from a Telegram account Ferry does not know, so the
    // agent (src/agent/dispatch.rs) tells it once, through /send, to sign in in the app: that is
    // the one send.
    let sent = stub.wait_for(1).await;
    assert_eq!(sent[0].body["space_id"], MAYA_TG);
    let text = sent[0].body["text"].as_str().unwrap();
    assert!(
        text.starts_with(ferry_core::agent::dispatch::UNKNOWN_SENDER),
        "{text}"
    );
    stub.assert_no_more_than(1).await;
}
