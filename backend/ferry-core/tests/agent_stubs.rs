//! The agent tests' own stubs and seeds (tests/agent_support): the stub Gemini scripts, records and
//! fails as asked, the privacy scan catches names, and the stub sidecar records sends and fails per
//! chat. Nothing here reaches the real Gemini or sidecar.
mod agent_support;
mod common;

use agent_support::{
    DANA_TG, GEMINI_KEY, GROUP_ID, JUDGE_TG, MAYA_TG, NO_MORE_SCRIPTED, STUB_INVITE_LINK,
    STUB_SECRET, StubGemini, StubSidecar, app, assert_bodies_private, call_reply, calls_reply, dm,
    find_leak, group_message, json_reply, leak_in, map_demo_group, membership, press,
    seed_active_order, seed_demo, seed_ping, text_reply,
};
use serde_json::{Value, json};
use sqlx::PgPool;

/// POSTs `body` to `url` with `headers`; returns the status and the JSON body.
async fn post(url: &str, headers: &[(&str, &str)], body: &Value) -> (u16, Value) {
    let mut request = reqwest::Client::new().post(url).json(body);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap())
}

fn generate_url(gemini: &StubGemini) -> String {
    format!("{}/v1beta/models/gemini-test:generateContent", gemini.base)
}

#[tokio::test]
async fn gemini_stub_records_and_answers_the_script_in_order() {
    let gemini = StubGemini::start().await;
    gemini.script(vec![
        call_reply("get_run", json!({"run_id": "run_demo"})),
        text_reply("first"),
    ]);
    let url = generate_url(&gemini);
    let key = [("x-goog-api-key", GEMINI_KEY)];

    let (status, first) = post(&url, &key, &json!({"contents": [], "n": 1})).await;
    assert_eq!(status, 200);
    let part = &first["candidates"][0]["content"]["parts"][0];
    assert_eq!(part["functionCall"]["name"], "get_run");
    assert_eq!(part["functionCall"]["args"]["run_id"], "run_demo");
    assert_eq!(part["thoughtSignature"], "stub-signature");
    assert_eq!(first["candidates"][0]["finishReason"], "STOP");

    let (_, second) = post(&url, &key, &json!({"n": 2})).await;
    assert_eq!(second, text_reply("first"));
    let (status, third) = post(&url, &[], &json!({"n": 3})).await;
    assert_eq!(status, 200);
    assert_eq!(third, text_reply(NO_MORE_SCRIPTED));

    let requests = gemini.requests();
    assert_eq!(gemini.request_count(), 3);
    assert_eq!(
        requests[0].path,
        "/v1beta/models/gemini-test:generateContent"
    );
    assert_eq!(requests[0].api_key.as_deref(), Some(GEMINI_KEY));
    assert_eq!(requests[2].api_key, None);
    assert_eq!(requests[1].body, json!({"n": 2}));
}

#[tokio::test]
async fn gemini_stub_helpers_shape_replies_and_read_function_responses() {
    let calls = calls_reply(&[("a", json!({})), ("b", json!({"x": 1}))]);
    let parts = calls["candidates"][0]["content"]["parts"]
        .as_array()
        .unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0]["thoughtSignature"], "stub-signature");
    assert!(parts[1].get("thoughtSignature").is_none());
    assert_eq!(parts[1]["functionCall"]["args"]["x"], 1);
    let classified = json_reply(&json!({"intent": "trip"}));
    let text = classified["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(text).unwrap(),
        json!({"intent": "trip"})
    );

    let gemini = StubGemini::start().await;
    let body = json!({"contents": [
        {"role": "user", "parts": [{"text": "hi"}]},
        {"role": "model", "parts": [{"functionCall": {"name": "get_run", "args": {}}}]},
        {"role": "user", "parts": [
            {"functionResponse": {"name": "get_run", "response": {"status": "open"}}},
            {"functionResponse": {"name": "get_menu", "response": {"items": 3}}}
        ]}
    ]});
    post(&generate_url(&gemini), &[], &body).await;
    assert_eq!(
        gemini.function_responses(0),
        vec![
            ("get_run".to_owned(), json!({"status": "open"})),
            ("get_menu".to_owned(), json!({"items": 3})),
        ]
    );
}

#[tokio::test]
async fn gemini_stub_down_closure_and_delay() {
    let gemini = StubGemini::start().await;
    let url = generate_url(&gemini);
    gemini.script(vec![text_reply("queued")]);
    gemini.respond_with(|body| (200, text_reply(&format!("echo {}", body["n"]))));
    let (_, echoed) = post(&url, &[], &json!({"n": 7})).await;
    assert_eq!(echoed, text_reply("echo 7"));

    gemini.set_down(503);
    let (status, body) = post(&url, &[], &json!({})).await;
    assert_eq!(status, 503);
    assert_eq!(body["error"]["code"], 503);
    assert_eq!(body["error"]["status"], "UNAVAILABLE");

    gemini.set_up();
    gemini.set_delay(std::time::Duration::from_millis(200));
    let started = std::time::Instant::now();
    post(&url, &[], &json!({"n": 8})).await;
    assert!(started.elapsed() >= std::time::Duration::from_millis(200));
    assert_eq!(gemini.request_count(), 3);
}

#[tokio::test]
async fn privacy_scan_passes_clean_requests() {
    let gemini = StubGemini::start().await;
    let clean = json!({"contents": [{"role": "user", "parts": [
        {"text": "{runner} is heading to Cosmic Cantina, ping @someone. Leopard print, oleo."}
    ]}]});
    post(&generate_url(&gemini), &[], &clean).await;
    gemini.assert_private();
}

#[test]
fn privacy_scan_finds_names_usernames_phones_and_mentions() {
    assert_eq!(leak_in("hi MAYA"), Some("Maya"));
    assert_eq!(leak_in("runner usr_maya"), Some("Maya"));
    assert_eq!(leak_in("ask Jordan"), Some("Jordan"));
    assert_eq!(leak_in("ask dana_tg"), Some("Dana"));
    assert_eq!(leak_in("judge_tg says"), Some("judge_tg"));
    assert_eq!(leak_in("Leo."), Some("Leo"));
    assert_eq!(leak_in("usr_leo"), Some("Leo"));
    assert_eq!(leak_in("leopard and oleo"), None);
    assert_eq!(leak_in("call +19195550102"), Some("+1919555"));
    assert_eq!(leak_in("ping @kim"), Some("@mention"));
    assert_eq!(leak_in("ping @someone_else"), Some("@mention"));
    assert_eq!(leak_in("ping @someone, at 6 @ 7"), None);
    // PR #22's review inputs.
    assert_eq!(leak_in("is maya lopez coming"), Some("Maya"));
    assert_eq!(leak_in("ask LOPEZ"), Some("Lopez"));
    assert_eq!(leak_in("ping my roommate (@sam_k99)"), Some("sam_k99"));
    assert_eq!(leak_in("ping (@kim_99)"), Some("@mention"));
    assert_eq!(
        leak_in("bring it to 412 Pittsboro Street."),
        Some("Pittsboro")
    );
    assert_eq!(leak_in("bring it to 9 Elm St."), Some("address"));
    assert_eq!(leak_in("to Zelda's place"), Some("Zelda"));
    assert_eq!(leak_in("from Sam Smith's apt"), Some("Sam Smith"));
    assert_eq!(leak_in("user usr_x1"), Some("usr_"));
    assert_eq!(leak_in("2 burritos at 6:30, $13.16 to {dropoff}"), None);
    // A name behind an escaped line break is found in the decoded string.
    let escaped = [json!({"text": "Back from İstanbul!\nDana says hi"})];
    assert_eq!(find_leak(&escaped), Some((0, "Dana")));
    let bodies = [json!({"a": "clean"}), json!({"b": "with Dana"})];
    assert_eq!(find_leak(&bodies), Some((1, "Dana")));
}

#[test]
#[should_panic(expected = "Gemini request 0 holds a forbidden term: \"Maya\"")]
fn privacy_scan_panics_on_a_name() {
    assert_bodies_private(&[json!({"contents": [{"parts": [{"text": "Maya is heading out"}]}]})]);
}

#[tokio::test]
async fn sidecar_stub_records_sends_and_fails_as_configured() {
    let sidecar = StubSidecar::start().await;
    let send = format!("{}/send", sidecar.base);
    let secret = [("x-ferry-secret", STUB_SECRET)];

    let (status, body) = post(&send, &secret, &json!({"space_id": GROUP_ID, "text": "hi"})).await;
    assert_eq!(status, 200);
    assert_eq!(body, json!({"space_id": GROUP_ID}));

    sidecar.set_send_status(MAYA_TG, 403, "recipient_not_reachable");
    let (status, body) = post(
        &send,
        &secret,
        &json!({"to_user_ids": [MAYA_TG], "text": "x"}),
    )
    .await;
    assert_eq!(status, 403);
    common::assert_envelope(&body, "recipient_not_reachable");
    let (status, _) = post(&send, &[], &json!({"space_id": MAYA_TG, "text": "y"})).await;
    assert_eq!(status, 403);
    sidecar.clear_send_status(MAYA_TG);
    let (status, body) = post(
        &send,
        &secret,
        &json!({"to_user_ids": [MAYA_TG], "text": "z"}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body, json!({"space_id": MAYA_TG}));

    let sent = sidecar.wait_for(4).await;
    assert_eq!(sent[0].secret.as_deref(), Some(STUB_SECRET));
    assert_eq!(sent[2].secret, None);
    let to_maya: Vec<Value> = sidecar
        .sent_to(MAYA_TG)
        .iter()
        .map(|b| b["text"].clone())
        .collect();
    assert_eq!(to_maya, vec![json!("x"), json!("y"), json!("z")]);
    sidecar.assert_no_more_than(4).await;

    let links = format!("{}/invite-links", sidecar.base);
    let (status, body) = post(&links, &secret, &json!({"space_id": GROUP_ID})).await;
    assert_eq!((status, body), (200, json!({"url": STUB_INVITE_LINK})));
    sidecar.set_invite_link_status(403, "not_admin");
    let (status, body) = post(&links, &secret, &json!({"space_id": GROUP_ID})).await;
    assert_eq!(status, 403);
    common::assert_envelope(&body, "not_admin");
    assert_eq!(sidecar.invite_link_calls().len(), 2);
}

#[test]
fn events_have_the_sidecar_shape_and_distinct_times() {
    let a = group_message(MAYA_TG, Some("maya_tg"), "Maya", "heading to Cosmic at 6");
    let b = group_message(MAYA_TG, Some("maya_tg"), "Maya", "heading to Cosmic at 6");
    assert_ne!(a["received_at"], b["received_at"]);
    assert_eq!(a["space_id"], GROUP_ID);
    assert_eq!(a["is_group"], true);
    assert_eq!(a["sender_phone"], Value::Null);
    let d = dm(JUDGE_TG, None, "Jordan", "hi");
    assert_eq!(d["space_id"], JUDGE_TG);
    assert_eq!(d["chat_title"], Value::Null);
    let p = press(
        GROUP_ID,
        true,
        DANA_TG,
        Some("dana_tg"),
        "Dana",
        "trip:1:yes",
    );
    assert_eq!((&p["kind"], &p["text"]), (&json!("callback"), &json!("")));
    assert_eq!(p["callback_data"], "trip:1:yes");
    let m = membership(
        "-100999",
        true,
        MAYA_TG,
        Some("New"),
        true,
        None,
        Some("-999"),
    );
    assert_eq!(m["kind"], "membership");
    assert_eq!(m["bot_is_admin"], Value::Null);
    assert_eq!(m["migrated_from_space_id"], "-999");
}

#[sqlx::test(migrations = "../migrations")]
async fn seeds_link_telegram_and_insert_orders_pings_and_the_group(pool: PgPool) {
    seed_demo(&pool).await;
    let linked: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, telegram_user_id, telegram_username FROM ferry.users ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let maya = linked.iter().find(|(id, _, _)| id == "usr_maya").unwrap();
    assert_eq!(maya.1.as_deref(), Some(MAYA_TG));
    assert_eq!(maya.2.as_deref(), Some("maya_tg"));
    let leo = linked.iter().find(|(id, _, _)| id == "usr_leo").unwrap();
    assert_eq!(leo.1, None);
    let run_status: String =
        sqlx::query_scalar("SELECT status FROM ferry.runs WHERE id = 'run_demo'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(run_status, "open");

    seed_active_order(&pool, "ord_stub", "usr_judge", "run_demo").await;
    let (status, label, total): (String, String, i64) = sqlx::query_as(
        "SELECT status, dropoff_label, total_cents FROM ferry.orders WHERE id = 'ord_stub'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (status.as_str(), label.as_str(), total),
        ("funded", "Fetzer Gym (HackNC)", 1325)
    );

    seed_ping(&pool, "run_demo", "usr_maya", 35.9135, -79.0556, 90).await;
    let age: f64 = sqlx::query_scalar(
        "SELECT extract(epoch FROM now() - ts)::float8 FROM ferry.location_pings WHERE run_id = 'run_demo'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!((age - 90.0).abs() < 1.0, "{age}");

    map_demo_group(&pool).await;
    let circle: Option<String> =
        sqlx::query_scalar("SELECT circle_id FROM ferry.message_spaces WHERE space_id = $1")
            .bind(GROUP_ID)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(circle.as_deref(), Some("cir_hinton4"));

    let test_app = app(pool, true, &[("PUBLIC_WEB_URL", "https://other.example")]).await;
    let config = &test_app.state.config;
    assert_eq!(
        config.gemini_base_url.as_deref(),
        Some(test_app.gemini.base.as_str())
    );
    assert_eq!(
        config.public_web_url.as_deref(),
        Some("https://other.example")
    );
    assert!(config.demo_mode);
}
