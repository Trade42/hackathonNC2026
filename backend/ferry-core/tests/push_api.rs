//! GET /v1/public/push-key and POST /v1/push/subscribe: the key with and without a configured
//! pair, web subscriptions (stored by endpoint, moved between users), native tokens and their
//! length cap, a platform alone, per-field 400s, and the SSRF guard on endpoints. Responses
//! validate against contract/openapi.yaml.
mod common;
mod push_support;
mod realtime_support;

use axum::http::{Request, StatusCode};
use common::assert_envelope;
use ferry_core::state::AppState;
use push_support::{
    Browser, Stub, WEB_URL, b64, push_server, rows, seed_hinton4, stored, subscribe, subscribe_ok,
    vapid_pair,
};
use realtime_support::{Server, assert_contract, serve};
use serde_json::{Value, json};
use sqlx::PgPool;

const FCM: &str = "https://fcm.googleapis.com/fcm/send/dpH5lCsTSSM:APA91bH";
/// `notifications::push::subscribe::MAX_TOKEN_LEN`, in bytes.
const MAX_TOKEN_LEN: usize = 512;

async fn push_key(server: &Server) -> (StatusCode, Value) {
    // Public: no Authorization header.
    let request = Request::get("/v1/public/push-key")
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, _, body) = server.send(request).await;
    (status, body)
}

async fn plain_server(pool: PgPool, pairs: &[(&str, &str)]) -> Server {
    seed_hinton4(&pool).await;
    serve(AppState::new(pool, common::config(pairs)).unwrap()).await
}

fn assert_400(what: &str, (status, body): (StatusCode, Value), fields: &[&str]) {
    assert_eq!(status, StatusCode::BAD_REQUEST, "{what}: {body}");
    assert_envelope(&body, "validation_failed");
    assert_contract("Error", &body);
    let listed: Vec<&str> = body["error"]["details"]
        .as_array()
        .unwrap_or_else(|| panic!("{what}: details is a list: {body}"))
        .iter()
        .map(|d| d["field"].as_str().unwrap())
        .collect();
    assert_eq!(listed, fields, "{what}: {body}");
}

#[sqlx::test(migrations = "../migrations")]
async fn push_key_with_a_configured_pair(pool: PgPool) {
    let (server, public) = push_server(pool, None, &[], |_| {}).await;
    let (status, body) = push_key(&server).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_contract("PushKey", &body);
    assert_eq!(body, json!({"vapid_public_key": public}));
    // What the web app passes as applicationServerKey: a 65-byte uncompressed P-256 point.
    let key = push_support::unb64(body["vapid_public_key"].as_str().unwrap());
    assert_eq!((key.len(), key[0]), (65, 0x04));
}

#[sqlx::test(migrations = "../migrations")]
async fn push_key_without_a_usable_pair_is_503_never_an_empty_key(pool: PgPool) {
    let (public, private) = vapid_pair();
    let (other_public, _) = vapid_pair();
    let server = plain_server(pool, &[]).await;
    let cases: [(&str, Vec<(&str, &str)>); 4] = [
        ("unset", vec![]),
        ("public only", vec![("VAPID_PUBLIC_KEY", &public)]),
        ("private only", vec![("VAPID_PRIVATE_KEY", &private)]),
        (
            "not one pair",
            vec![
                ("VAPID_PUBLIC_KEY", &other_public),
                ("VAPID_PRIVATE_KEY", &private),
            ],
        ),
    ];
    for (what, pairs) in cases {
        let mut state = server.state.clone();
        state.config = std::sync::Arc::new(common::config(&pairs));
        let request = Request::get("/v1/public/push-key")
            .body(axum::body::Body::empty())
            .unwrap();
        let (status, _, body) = common::send(ferry_core::app::router(state), request).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{what}: {body}");
        assert_envelope(&body, "upstream_unavailable");
        assert_contract("Error", &body);
        assert!(!body.to_string().contains(&private), "{what}");
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn subscribe_needs_a_signed_in_user(pool: PgPool) {
    let server = plain_server(pool.clone(), &[]).await;
    let request = Request::post("/v1/push/subscribe")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            Browser::new(FCM).subscribe_body().to_string(),
        ))
        .unwrap();
    let (status, _, body) = server.send(request).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_envelope(&body, "unauthorized");
    assert!(stored(&pool).await.is_empty());
}

#[sqlx::test(migrations = "../migrations")]
async fn a_web_subscription_is_stored_and_moves_to_whoever_subscribes_it(pool: PgPool) {
    let server = plain_server(pool.clone(), &[]).await;
    let browser = Browser::new(FCM);
    subscribe_ok(&server, "usr_judge", &browser.subscribe_body()).await;
    assert_eq!(stored(&pool).await, rows(&[("usr_judge", "web", FCM)]));

    // The same subscription again: still one row.
    subscribe_ok(&server, "usr_judge", &browser.subscribe_body()).await;
    assert_eq!(stored(&pool).await.len(), 1);

    // Another user signs in on that browser: the endpoint moves, with the new keys.
    let same_endpoint_new_keys = Browser::new(FCM);
    subscribe_ok(
        &server,
        "usr_maya",
        &same_endpoint_new_keys.subscribe_body(),
    )
    .await;
    assert_eq!(stored(&pool).await, rows(&[("usr_maya", "web", FCM)]));
    let (p256dh, auth): (String, String) =
        sqlx::query_as("SELECT p256dh, auth FROM ferry.push_subscriptions")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(p256dh, b64(&same_endpoint_new_keys.p256dh));
    assert_eq!(auth, b64(&same_endpoint_new_keys.auth));

    // Every push service's endpoint form is accepted, and expirationTime is stored.
    for endpoint in [
        "https://updates.push.services.mozilla.com/wpush/v2/gAAAAABk",
        "https://wns2-bl2p.notify.windows.com/w/?token=BQYAAAB",
        "https://web.push.apple.com/QGuQyavXutnMH8",
    ] {
        subscribe_ok(
            &server,
            "usr_judge",
            &Browser::new(endpoint).subscribe_body(),
        )
        .await;
    }
    let mut body = Browser::new("https://fcm.googleapis.com/fcm/send/expiring").subscribe_body();
    body["web_push"]["expirationTime"] = json!(1_790_000_000_000_i64);
    subscribe_ok(&server, "usr_judge", &body).await;
    let expires: Option<String> = sqlx::query_scalar(
        "SELECT to_char(expiration_time AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS') \
         FROM ferry.push_subscriptions WHERE endpoint LIKE '%expiring'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(expires.as_deref(), Some("2026-09-21T14:13:20.000"));
    assert_eq!(stored(&pool).await.len(), 5);
}

#[sqlx::test(migrations = "../migrations")]
async fn native_tokens_are_stored_and_a_platform_alone_stores_nothing(pool: PgPool) {
    let server = plain_server(pool.clone(), &[]).await;
    for platform in ["web", "ios", "android"] {
        subscribe_ok(&server, "usr_judge", &json!({"platform": platform})).await;
    }
    assert!(stored(&pool).await.is_empty());

    subscribe_ok(
        &server,
        "usr_judge",
        &json!({"platform": "ios", "token": "apns-token-1"}),
    )
    .await;
    subscribe_ok(
        &server,
        "usr_judge",
        &json!({"platform": "android", "token": "fcm-token-1"}),
    )
    .await;
    // The same token from another user moves; the same string on another platform is its own.
    subscribe_ok(
        &server,
        "usr_maya",
        &json!({"platform": "ios", "token": "apns-token-1"}),
    )
    .await;
    subscribe_ok(
        &server,
        "usr_maya",
        &json!({"platform": "android", "token": "apns-token-1"}),
    )
    .await;
    assert_eq!(
        stored(&pool).await,
        rows(&[
            ("usr_maya", "ios", "apns-token-1"),
            ("usr_judge", "android", "fcm-token-1"),
            ("usr_maya", "android", "apns-token-1"),
        ])
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn a_native_token_over_the_cap_is_400_never_500(pool: PgPool) {
    let server = plain_server(pool.clone(), &[]).await;
    // Hex that does not compress, as a real token's characters, so Postgres indexes it at full
    // length: 3,840 characters of it got a 500 under the old 4096-byte cap (web-push review
    // MINOR 2).
    let hex = |len: usize| -> String {
        let mut hex: String = (1..=len.div_ceil(16) as u64)
            .map(|i| format!("{:016x}", i.wrapping_mul(0x9e37_79b9_7f4a_7c15)))
            .collect();
        hex.truncate(len);
        hex
    };

    let longest = hex(MAX_TOKEN_LEN);
    subscribe_ok(
        &server,
        "usr_judge",
        &json!({"platform": "ios", "token": longest}),
    )
    .await;
    for len in [MAX_TOKEN_LEN + 1, 3840] {
        let body = json!({"platform": "android", "token": hex(len)});
        assert_400(
            &format!("a {len}-byte token"),
            push_support::post_json(&server, "/v1/push/subscribe", "usr_judge", &body).await,
            &["token"],
        );
    }
    assert_eq!(stored(&pool).await, rows(&[("usr_judge", "ios", &longest)]));
}

#[sqlx::test(migrations = "../migrations")]
async fn malformed_requests_are_400_with_each_field(pool: PgPool) {
    let server = plain_server(pool.clone(), &[]).await;
    let good = Browser::new(FCM).to_json();
    let with = |path: &[&str], value: Value| {
        let mut body = json!({"platform": "web", "web_push": good.clone()});
        let mut target = &mut body["web_push"];
        for key in &path[..path.len() - 1] {
            target = &mut target[*key];
        }
        let last = path[path.len() - 1];
        if value.is_null() {
            target.as_object_mut().unwrap().remove(last);
        } else {
            target[last] = value;
        }
        body
    };

    let cases = [
        (
            "no endpoint",
            with(&["endpoint"], Value::Null),
            vec!["web_push.endpoint"],
        ),
        (
            "no p256dh",
            with(&["keys", "p256dh"], Value::Null),
            vec!["web_push.keys.p256dh"],
        ),
        (
            "no auth",
            with(&["keys", "auth"], Value::Null),
            vec!["web_push.keys.auth"],
        ),
        (
            "no keys",
            with(&["keys"], Value::Null),
            vec!["web_push.keys.p256dh", "web_push.keys.auth"],
        ),
        (
            "short p256dh",
            with(&["keys", "p256dh"], json!(b64(&[4u8; 33]))),
            vec!["web_push.keys.p256dh"],
        ),
        (
            "off-curve p256dh",
            with(&["keys", "p256dh"], json!(b64(&[4u8; 65]))),
            vec!["web_push.keys.p256dh"],
        ),
        (
            "long auth",
            with(&["keys", "auth"], json!(b64(&[1u8; 32]))),
            vec!["web_push.keys.auth"],
        ),
        (
            "not base64url",
            with(&["keys", "auth"], json!("%%%")),
            vec!["web_push.keys.auth"],
        ),
        (
            "bad expirationTime",
            with(&["expirationTime"], json!(-5)),
            vec!["web_push.expirationTime"],
        ),
    ];
    for (what, body, fields) in cases {
        assert_400(
            what,
            push_support::post_json(&server, "/v1/push/subscribe", "usr_judge", &body).await,
            &fields,
        );
    }
    for (what, body, fields) in [
        ("no platform", json!({}), vec!["platform"]),
        (
            "unknown platform",
            json!({"platform": "fax"}),
            vec!["platform"],
        ),
        (
            "empty token",
            json!({"platform": "ios", "token": ""}),
            vec!["token"],
        ),
        (
            "web_push not an object",
            json!({"platform": "web", "web_push": "x"}),
            vec!["web_push"],
        ),
    ] {
        assert_400(
            what,
            push_support::post_json(&server, "/v1/push/subscribe", "usr_judge", &body).await,
            &fields,
        );
    }
    // Not JSON at all: 400 in the envelope too.
    let request = Request::post("/v1/push/subscribe")
        .header(
            "authorization",
            format!("Bearer {}", server.token("usr_judge")),
        )
        .header("content-type", "application/json")
        .body(axum::body::Body::from("{"))
        .unwrap();
    let (status, _, body) = server.send(request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_envelope(&body, "validation_failed");
    assert!(stored(&pool).await.is_empty());
}

#[sqlx::test(migrations = "../migrations")]
async fn endpoints_off_the_push_services_are_refused(pool: PgPool) {
    // Even with a stub configured, only its exact origin is an exception.
    let stub = Stub::start().await;
    let (server, _) = push_server(pool.clone(), Some(&stub), &[], |_| {}).await;
    let stub_port: u16 = stub.origin.rsplit(':').next().unwrap().parse().unwrap();
    let other_port = format!("http://127.0.0.1:{}/push/x", stub_port.wrapping_add(1));
    for endpoint in [
        "http://fcm.googleapis.com/fcm/send/x",
        "https://127.0.0.1/push",
        "https://[::1]/push",
        "https://localhost/push",
        "https://169.254.169.254/latest/meta-data",
        "https://10.0.0.5/push",
        "https://example.com/push",
        "https://fcm.googleapis.com.evil.example/x",
        "https://fcm.googleapis.com:8443/x",
        "https://user:pw@fcm.googleapis.com/x",
        "http://localhost/push",
        &other_port,
        "not a url",
    ] {
        // post_json, not subscribe: "not a url" is not even a contract `uri`.
        let (status, body) = push_support::post_json(
            &server,
            "/v1/push/subscribe",
            "usr_judge",
            &Browser::new(endpoint).subscribe_body(),
        )
        .await;
        assert_400(endpoint, (status, body), &["web_push.endpoint"]);
    }
    assert!(stored(&pool).await.is_empty());
    subscribe_ok(
        &server,
        "usr_judge",
        &Browser::new(stub.endpoint("ok")).subscribe_body(),
    )
    .await;
    assert_eq!(stored(&pool).await.len(), 1);
}

#[sqlx::test(migrations = "../migrations")]
async fn without_the_stub_setting_a_local_endpoint_is_refused(pool: PgPool) {
    let stub = Stub::start().await;
    let server = plain_server(pool.clone(), &[("PUBLIC_WEB_URL", WEB_URL)]).await;
    let (status, body) = subscribe(
        &server,
        "usr_judge",
        &Browser::new(stub.endpoint("x")).subscribe_body(),
    )
    .await;
    assert_400(
        "stub without the setting",
        (status, body),
        &["web_push.endpoint"],
    );
    assert!(stored(&pool).await.is_empty());
}

#[sqlx::test(migrations = "../migrations")]
async fn each_user_keeps_their_newest_subscriptions(pool: PgPool) {
    let server = plain_server(pool.clone(), &[]).await;
    let endpoint = |n: usize| format!("https://fcm.googleapis.com/fcm/send/{n}");
    let endpoints = |rows: Vec<(String, String, String)>| -> Vec<String> {
        rows.into_iter().map(|(_, _, e)| e).collect()
    };
    for n in 0..12 {
        subscribe_ok(
            &server,
            "usr_judge",
            &Browser::new(endpoint(n)).subscribe_body(),
        )
        .await;
    }
    let kept = endpoints(stored(&pool).await);
    assert_eq!(
        kept.len(),
        ferry_core::notifications::push::MAX_PER_PLATFORM as usize
    );
    assert_eq!(kept, (2..12).map(endpoint).collect::<Vec<_>>());

    // An endpoint with an old row that moves to a user who already has 10 is the one kept: their
    // oldest other subscription goes instead.
    sqlx::query("DELETE FROM ferry.push_subscriptions")
        .execute(&pool)
        .await
        .unwrap();
    subscribe_ok(&server, "usr_maya", &Browser::new(FCM).subscribe_body()).await;
    for n in 0..10 {
        subscribe_ok(
            &server,
            "usr_judge",
            &Browser::new(endpoint(n)).subscribe_body(),
        )
        .await;
    }
    subscribe_ok(&server, "usr_judge", &Browser::new(FCM).subscribe_body()).await;
    let rows = stored(&pool).await;
    assert_eq!(rows.len(), 10);
    assert!(rows.iter().all(|(user, _, _)| user == "usr_judge"));
    assert_eq!(rows[0].2, FCM);
    assert!(!endpoints(rows).contains(&endpoint(0)));

    // Other users and other platforms are counted apart.
    subscribe_ok(
        &server,
        "usr_maya",
        &Browser::new(endpoint(99)).subscribe_body(),
    )
    .await;
    subscribe_ok(
        &server,
        "usr_judge",
        &json!({"platform": "ios", "token": "t1"}),
    )
    .await;
    assert_eq!(stored(&pool).await.len(), 12);
}
