//! docs/DEMO.md's "Channel: Telegram" script through `POST /internal/messages/inbound`, with a stub
//! sidecar and a stub Gemini (never the real one): Maya's trip in the demo group, the judge's ETA
//! question in a DM with Maya's name filled in by code, Dana's meet-again with "I'm in" and "Can't
//! make it" buttons and the judge's "I'm in" relayed to Dana, silence when the group is not
//! addressed, and the fallback when Gemini is down. Every test ends with the privacy scan of what
//! reached Gemini.
mod agent_support;
mod common;

use agent_support::{
    DANA_TG, DANA_USERNAME, GROUP_ID, JUDGE_TG, JUDGE_USERNAME, MAYA_TG, MAYA_USERNAME,
    STUB_INVITE_LINK, TestApp, app, call_reply, dm, group_message, inbound, json_reply,
    map_demo_group, press, seed_active_order, seed_demo, seed_ping, text_reply,
};
use serde_json::{Value, json};
use sqlx::PgPool;

/// Gemini as the demo needs it: a trip classification says Cosmic Cantina; a turn calls `tool`
/// first, then repeats the tool's `say` line, as the system prompt tells it to.
fn scripted_gemini(app: &TestApp, tool: &'static str, args: Value) {
    app.gemini.respond_with(move |body| {
        if body["generationConfig"]["responseMimeType"] == "application/json" {
            return (
                200,
                json_reply(&json!({"is_trip": true, "restaurant_id": "rst_cosmic"})),
            );
        }
        let contents = body["contents"].as_array().unwrap();
        let last = &contents[contents.len() - 1];
        match last["parts"][0].get("functionResponse") {
            Some(response) => (
                200,
                text_reply(response["response"]["say"].as_str().unwrap_or("done")),
            ),
            None => (200, call_reply(tool, args.clone())),
        }
    });
}

fn text(sent: &Value) -> &str {
    sent["text"].as_str().unwrap()
}

fn buttons(sent: &Value) -> Vec<Value> {
    sent["buttons"].as_array().cloned().unwrap_or_default()
}

/// A logged turn: reply_text, tool_calls, model and error.
type TurnRow = (Option<String>, Value, Option<String>, Option<String>);

/// The turn logged for `space_id`, once it is finished (the log is written after the sends).
async fn finished_turn(pool: &PgPool, space_id: &str) -> TurnRow {
    for _ in 0..200 {
        let row: Option<TurnRow> = sqlx::query_as(
            "SELECT reply_text, tool_calls, model, error FROM ferry.agent_turns
              WHERE space_id = $1 AND latency_ms IS NOT NULL",
        )
        .bind(space_id)
        .fetch_optional(pool)
        .await
        .unwrap();
        if let Some(row) = row {
            return row;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("no finished turn for {space_id}");
}

/// Maya's ping 20 s ago, on Franklin St just east of Cosmic Cantina.
async fn maya_walking(pool: &PgPool) {
    seed_active_order(pool, "ord_agent_judge", "usr_judge", "run_demo").await;
    seed_ping(pool, "run_demo", "usr_maya", 35.9125, -79.0520, 20).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn mayas_trip_in_the_demo_group_gets_one_reply_with_the_run_link(pool: PgPool) {
    seed_demo(&pool).await;
    map_demo_group(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    scripted_gemini(&app, "get_profile", json!({}));

    let event = group_message(
        MAYA_TG,
        Some(MAYA_USERNAME),
        "Maya",
        "heading to Cosmic Cantina at 6, anyone want anything?",
    );
    assert!(
        inbound(&app, &event).await.is_empty(),
        "the bot answers through /send"
    );
    let sent = app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    let reply = &sent[0].body;
    assert_eq!(reply["space_id"], GROUP_ID);
    let line = text(reply);
    assert!(
        line.starts_with("Ferry to Cosmic Cantina is open until "),
        "{line}"
    );
    assert!(
        line.ends_with(". Reply with your order or tap the button."),
        "{line}"
    );
    // Maya's seeded run_demo is the run: its link is the button (docs/DEMO.md, step 1).
    let link = buttons(reply);
    assert_eq!(link.len(), 1, "{reply}");
    assert_eq!(link[0]["url"], "https://ferry.example/r/run_demo");

    let (user, circle, restaurant, status, detected_by, run): (
        String,
        String,
        String,
        String,
        String,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT user_id, circle_id, restaurant_id, status, detected_by, run_id
           FROM ferry.trip_intents",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (user.as_str(), circle.as_str(), restaurant.as_str()),
        ("usr_maya", "cir_hinton4", "rst_cosmic")
    );
    assert_eq!(
        (status.as_str(), detected_by.as_str()),
        ("opened", "gemini")
    );
    assert_eq!(run.as_deref(), Some("run_demo"));
    // Only the classification reached Gemini, with the message's words and no name.
    assert_eq!(app.gemini.request_count(), 1);
    let classify = &app.gemini.requests()[0].body;
    assert!(
        classify
            .to_string()
            .contains("heading to Cosmic Cantina at 6"),
        "{classify}"
    );

    // Saying it again in the same chat says nothing new.
    let again = group_message(
        MAYA_TG,
        Some(MAYA_USERNAME),
        "Maya",
        "heading to cosmic at 6",
    );
    inbound(&app, &again).await;
    app.sidecar.assert_no_more_than(1).await;
    app.gemini.assert_private();
}

#[sqlx::test(migrations = "../migrations")]
async fn the_judges_eta_question_names_maya_from_code(pool: PgPool) {
    seed_demo(&pool).await;
    maya_walking(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    scripted_gemini(&app, "get_eta", json!({}));

    let event = dm(
        JUDGE_TG,
        Some(JUDGE_USERNAME),
        "Jordan",
        "how long until my food gets here?",
    );
    assert!(inbound(&app, &event).await.is_empty());
    let sent = app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    let reply = &sent[0].body;
    assert_eq!(reply["space_id"], JUDGE_TG);
    let line = text(reply);
    assert!(line.starts_with("Maya is "), "{line}");
    assert!(
        line.contains(" min away (") && line.contains(" mi), seen "),
        "{line}"
    );
    assert!(line.ends_with(" s ago."), "the data's age: {line}");

    // Gemini saw the tool's result with a placeholder, never the name, and got its own
    // function-call part back unchanged, thought signature included.
    let requests = app.gemini.requests();
    assert_eq!(requests.len(), 2);
    let results = app.gemini.function_responses(1);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "get_eta");
    assert_eq!(results[0].1["runner"], "{runner}");
    assert!(
        results[0].1["say"]
            .as_str()
            .unwrap()
            .starts_with("{runner} is "),
        "{}",
        results[0].1
    );
    let replayed = &requests[1].body["contents"];
    let model_turn = &replayed[replayed.as_array().unwrap().len() - 2];
    assert_eq!(model_turn["role"], "model");
    assert_eq!(model_turn["parts"][0]["thoughtSignature"], "stub-signature");
    assert_eq!(model_turn["parts"][0]["functionCall"]["name"], "get_eta");
    app.gemini.assert_private();

    // The turn is logged with every call and its latency.
    let (reply_text, calls, model, error) = finished_turn(&pool, JUDGE_TG).await;
    assert_eq!(reply_text.as_deref(), Some(line));
    assert_eq!(model.as_deref(), Some("gemini-test"));
    assert_eq!(error, None);
    let names: Vec<&str> = calls
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["gemini", "get_eta", "gemini"]);
    assert!(
        calls
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["latency_ms"].is_u64() && c["ok"] == true)
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn danas_meet_again_dms_with_buttons_and_the_judge_accepts(pool: PgPool) {
    seed_demo(&pool).await;
    map_demo_group(&pool).await;
    sqlx::query(
        "UPDATE ferry.message_spaces SET bot_is_admin = true, invite_url = $2 WHERE space_id = $1",
    )
    .bind(GROUP_ID)
    .bind(STUB_INVITE_LINK)
    .execute(&pool)
    .await
    .unwrap();
    let app = app(pool.clone(), true, &[]).await;
    scripted_gemini(&app, "start_meet_again", json!({}));

    let event = dm(
        DANA_TG,
        Some(DANA_USERNAME),
        "Dana",
        "let's meet with the people I hung out with last time",
    );
    inbound(&app, &event).await;
    let sent = app.sidecar.wait_for(3).await;
    app.sidecar.assert_no_more_than(3).await;
    // The invitees' DMs go out as the invite is made, so they may come before Dana's reply.
    let reply = app.sidecar.sent_to(DANA_TG);
    assert_eq!(reply.len(), 1);
    assert_eq!(
        text(&reply[0]),
        "Pinged Jordan, Leo and Maya to meet up. I'll tell you who's in. Leo gets it in the app only."
    );
    let invite_id: String = sqlx::query_scalar("SELECT id FROM ferry.invites")
        .fetch_one(&pool)
        .await
        .unwrap();
    let mut dm_targets = Vec::new();
    for s in sent.iter().filter(|s| s.body["space_id"] != DANA_TG) {
        dm_targets.push(s.body["space_id"].as_str().unwrap().to_owned());
        assert_eq!(
            text(&s.body),
            "Dana wants to meet up. You in?",
            "{}",
            s.body
        );
        let b = buttons(&s.body);
        assert_eq!(b[0]["label"], "I'm in");
        assert_eq!(b[0]["callback_data"], format!("invite:{invite_id}:accept"));
        assert_eq!(b[1]["label"], "Can't make it");
        assert_eq!(b[1]["callback_data"], format!("invite:{invite_id}:decline"));
        assert_eq!(b[2]["url"], format!("https://ferry.example/i/{invite_id}"));
    }
    dm_targets.sort();
    assert_eq!(dm_targets, [JUDGE_TG, MAYA_TG]);
    // Leo never pressed Start: the app's notification is his only copy.
    let leo_notified: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ferry.notifications WHERE user_id = 'usr_leo' AND type = 'invite'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(leo_notified, 1);

    // The judge taps Accept in their DM: invites' respond records it, and the group's link comes
    // back as a button.
    let accept = format!("invite:{invite_id}:accept");
    inbound(
        &app,
        &press(
            JUDGE_TG,
            false,
            JUDGE_TG,
            Some(JUDGE_USERNAME),
            "Jordan",
            &accept,
        ),
    )
    .await;
    // The judge's answer, with the group's link as an extra, and its relay to Dana, in either
    // order.
    app.sidecar.wait_for(5).await;
    app.sidecar.assert_no_more_than(5).await;
    let judge = app.sidecar.sent_to(JUDGE_TG);
    let answer = judge.last().unwrap();
    assert_eq!(text(answer), "You're in! I let Dana know.");
    assert_eq!(buttons(answer)[0]["url"], STUB_INVITE_LINK);
    assert_eq!(
        text(app.sidecar.sent_to(DANA_TG).last().unwrap()),
        "Jordan's in."
    );
    let status: String = sqlx::query_scalar(
        "SELECT status FROM ferry.invite_responses WHERE invite_id = $1 AND user_id = 'usr_judge'",
    )
    .bind(&invite_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(status, "accepted");
    // Pressing it again is a new event with the same answer, not a second reply to one message.
    app.gemini.assert_private();
}

#[sqlx::test(migrations = "../migrations")]
async fn the_bot_stays_silent_in_a_group_when_not_addressed(pool: PgPool) {
    seed_demo(&pool).await;
    map_demo_group(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    scripted_gemini(&app, "get_profile", json!({}));
    for chat in [
        "lol same",
        "who's coming to the talk at 6?",
        "going to be late, save me a seat",
        "I'm going to sleep",
    ] {
        let event = group_message(JUDGE_TG, Some(JUDGE_USERNAME), "Jordan", chat);
        assert!(inbound(&app, &event).await.is_empty());
    }
    app.sidecar.assert_no_more_than(0).await;
    // Silence costs nothing: no Gemini call and no log.
    assert_eq!(app.gemini.request_count(), 0);
    let logged: i64 = sqlx::query_scalar("SELECT count(*) FROM ferry.agent_turns")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(logged, 0);

    // Addressed, it answers once.
    let asked = group_message(
        JUDGE_TG,
        Some(JUDGE_USERNAME),
        "Jordan",
        "ferry what are my defaults?",
    );
    inbound(&app, &asked).await;
    let sent = app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    assert_eq!(sent[0].body["space_id"], GROUP_ID);
    app.gemini.assert_private();
}

#[sqlx::test(migrations = "../migrations")]
async fn gemini_down_gives_the_fallback_reply(pool: PgPool) {
    seed_demo(&pool).await;
    maya_walking(&pool).await;
    map_demo_group(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    app.gemini.set_down(503);

    let event = dm(
        JUDGE_TG,
        Some(JUDGE_USERNAME),
        "Jordan",
        "how long until my food gets here?",
    );
    inbound(&app, &event).await;
    let sent = app.sidecar.wait_for(1).await;
    let line = text(&sent[0].body);
    assert!(
        line.starts_with("Maya is ") && line.ends_with(" s ago."),
        "{line}"
    );
    assert_eq!(app.gemini.request_count(), 2, "one call, one retry");
    let (_, calls, _, error) = finished_turn(&pool, JUDGE_TG).await;
    assert_eq!(
        error.as_deref(),
        Some("status"),
        "the turn records why it fell back"
    );
    assert_eq!(calls[0]["name"], "gemini");
    assert_eq!(calls[0]["ok"], false);
    assert_eq!(calls[1]["name"], "get_eta");

    // A trip still gets its question, from the patterns alone.
    let trip = group_message(
        MAYA_TG,
        Some(MAYA_USERNAME),
        "Maya",
        "heading to sup dogs at 7:30pm",
    );
    inbound(&app, &trip).await;
    let sent = app.sidecar.wait_for(2).await;
    // Without Gemini a trip is never sure, so the bot asks. Orders close 10 minutes after 7:30 PM
    // (later when that time is already past).
    assert!(
        text(&sent[1].body).starts_with("Open a Ferry to Sup Dogs? Orders close "),
        "{}",
        sent[1].body
    );
    let detected_by: String = sqlx::query_scalar("SELECT detected_by FROM ferry.trip_intents")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(detected_by, "patterns");
    app.gemini.assert_private();
}

#[sqlx::test(migrations = "../migrations")]
async fn names_in_the_users_own_words_never_reach_gemini(pool: PgPool) {
    seed_demo(&pool).await;
    maya_walking(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    scripted_gemini(&app, "get_eta", json!({}));
    let event = dm(
        JUDGE_TG,
        Some(JUDGE_USERNAME),
        "Jordan",
        "is Maya close? ask @maya_tg or Dana, or text me at +1 919 555 0101",
    );
    inbound(&app, &event).await;
    let sent = app.sidecar.wait_for(1).await;
    assert!(
        text(&sent[0].body).starts_with("Maya is "),
        "{}",
        sent[0].body
    );
    let first = app.gemini.requests()[0].body.to_string();
    assert!(
        first.contains("@someone") && first.contains("[phone]"),
        "{first}"
    );
    app.gemini.assert_private();
}
