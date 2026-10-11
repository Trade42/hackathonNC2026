//! meet-place (Sal, 2026-10-11): meet-again is a ping with a place and a time, not a way back into
//! an old group. Through `POST /internal/messages/inbound` with a stub sidecar and a stub Gemini
//! (never the real ones):
//!
//! - Dana's "anyone want to go to Cosmic at 7pm?" in her DM: each invitee who pressed Start gets
//!   "Dana wants to go to Cosmic Cantina at 7:00 PM. You in?" with "I'm in" and "Can't make it";
//!   every answer is relayed to Dana once ("Jordan's in.", "Maya can't make it.").
//! - The DM with a place and a time, only one of them, and neither.
//! - In a group, the group's circle is invited; in a DM, Dana's most recent circle.
//! - A place typed with a name in it reaches Gemini scrubbed and the invitees as written; a
//!   timestamp from Gemini is never trusted.
//! - With Gemini down, the patterns read the phrase and the bot asks once before sending.
//!
//! "at 7pm" is 7:00 PM today, or tomorrow once 7 PM has passed, so those assertions only check
//! the start of the line; "8pm tomorrow" is always "8:00 PM tomorrow". Every app's Gemini
//! requests are scanned for names when it is dropped.
mod agent_support;
mod common;
mod realtime_support;

use agent_support::{
    DANA_TG, DANA_USERNAME, GROUP_ID, JUDGE_TG, JUDGE_USERNAME, MAYA_TG, MAYA_USERNAME, TestApp,
    app, call_reply, dm, group_message, inbound, map_demo_group, press, seed_demo, text_reply,
};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use ferry_core::agent::meet;
use realtime_support::assert_contract;
use serde_json::{Value, json};
use sqlx::PgPool;

fn text(sent: &Value) -> &str {
    sent["text"].as_str().unwrap()
}

fn buttons(sent: &Value) -> Vec<Value> {
    sent["buttons"].as_array().cloned().unwrap_or_default()
}

/// Gemini as the tool loop: start_meet_again with `args`, then the tool's own line.
fn gemini_calls(app: &TestApp, args: Value) {
    app.gemini.respond_with(move |body| {
        let contents = body["contents"].as_array().unwrap();
        let last = &contents[contents.len() - 1];
        match last["parts"][0].get("functionResponse") {
            Some(response) => (
                200,
                text_reply(response["response"]["say"].as_str().unwrap_or("done")),
            ),
            None => (200, call_reply("start_meet_again", args.clone())),
        }
    });
}

fn dana(said: &str) -> Value {
    dm(DANA_TG, Some(DANA_USERNAME), "Dana", said)
}

/// Dana's DM `said`, answered by Gemini calling start_meet_again with `args`; waits for `sends`
/// sends in all.
async fn dana_asks(app: &TestApp, said: &str, args: Value, sends: usize) {
    gemini_calls(app, args);
    assert!(inbound(app, &dana(said)).await.is_empty());
    app.sidecar.wait_for(sends).await;
}

fn press_as(tg: &str, username: &str, name: &str, data: &str) -> Value {
    press(tg, false, tg, Some(username), name, data)
}

async fn invite_ids(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar("SELECT id FROM ferry.invites ORDER BY created_at, id")
        .fetch_all(pool)
        .await
        .unwrap()
}

/// `GET /v1/invites` as `user_id`: each one a contract v1.5 `Invite`.
async fn invites_of(app: &TestApp, user_id: &str) -> Vec<Value> {
    let token = app.state.jwt.issue_token(user_id).unwrap();
    let request = Request::get("/v1/invites")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = common::send(app.router.clone(), request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let list = body.as_array().unwrap().clone();
    for invite in &list {
        assert_contract("Invite", invite);
    }
    list
}

/// The stored invite's time is ahead, within a day and a hour (the next 7 PM).
async fn assert_next_day(pool: &PgPool, invite_id: &str) {
    let ahead: bool = sqlx::query_scalar(
        "SELECT proposed_time > now() AND proposed_time <= now() + interval '25 hours'
           FROM ferry.invites WHERE id = $1",
    )
    .bind(invite_id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(ahead, "{invite_id}'s proposed_time");
}

#[sqlx::test(migrations = "../migrations")]
async fn danas_ping_with_a_place_and_a_time_and_every_answer_relayed_once(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    dana_asks(
        &app,
        "anyone want to go to Cosmic at 7pm?",
        json!({"place": "Cosmic", "when": "7pm"}),
        3,
    )
    .await;
    app.sidecar.assert_no_more_than(3).await;
    let id = invite_ids(&pool).await.remove(0);

    // Dana's reply names who was pinged and what, from code.
    let reply = app.sidecar.sent_to(DANA_TG);
    let line = text(&reply[0]);
    assert!(
        line.starts_with("Pinged Jordan, Leo and Maya: Cosmic Cantina at 7:00 PM"),
        "{line}"
    );
    assert!(
        line.ends_with(". I'll tell you who's in. Leo gets it in the app only."),
        "{line}"
    );
    assert_eq!(
        buttons(&reply[0])[0]["url"],
        format!("https://ferry.example/i/{id}")
    );

    // The judge's and Maya's DMs: the place and the time, I'm in and Can't make it.
    for tg in [JUDGE_TG, MAYA_TG] {
        let ping = app.sidecar.sent_to(tg);
        assert_eq!(ping.len(), 1, "{tg}");
        let line = text(&ping[0]);
        assert!(
            line.starts_with("Dana wants to go to Cosmic Cantina at 7:00 PM"),
            "{line}"
        );
        assert!(
            line.ends_with(". You in?") && !line.contains('\n'),
            "{line}"
        );
        assert_eq!(
            buttons(&ping[0]),
            [
                json!({"label": "I'm in", "callback_data": format!("invite:{id}:accept")}),
                json!({"label": "Can't make it", "callback_data": format!("invite:{id}:decline")}),
                json!({"label": "Open in Ferry", "url": format!("https://ferry.example/i/{id}")}),
            ]
        );
    }
    // Leo never pressed Start: the app's notification, with the same line.
    let (title, body): (String, String) = sqlx::query_as(
        "SELECT title, body FROM ferry.notifications WHERE user_id = 'usr_leo' AND type = 'invite'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        title.starts_with("Dana wants to go to Cosmic Cantina at 7:00 PM"),
        "{title}"
    );
    assert_eq!(body, "You in?");

    // The invite stores the catalog's place and a time code chose.
    assert_next_day(&pool, &id).await;
    let place = &invites_of(&app, "usr_dana").await[0]["place"];
    assert_eq!(place["id"], "rst_cosmic");
    assert_eq!(place["label"], "Cosmic Cantina");
    assert!(place["location"]["lat"].is_f64(), "{place}");

    // What Gemini saw: the plan, and people as placeholders.
    let results = app.gemini.function_responses(1);
    assert_eq!(results[0].0, "start_meet_again");
    let seen = &results[0].1;
    assert!(
        seen["plan"]
            .as_str()
            .unwrap()
            .starts_with("Cosmic Cantina at 7:00 PM"),
        "{seen}"
    );
    assert_eq!(seen["telegram_messages_sent"], 2);
    assert!(
        seen["invited"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p.as_str().unwrap().starts_with('{')),
        "{seen}"
    );

    // The judge taps I'm in: their answer, and one DM to Dana.
    let accept = format!("invite:{id}:accept");
    inbound(&app, &press_as(JUDGE_TG, JUDGE_USERNAME, "Jordan", &accept)).await;
    app.sidecar.wait_for(5).await;
    let answer = text(app.sidecar.sent_to(JUDGE_TG).last().unwrap()).to_owned();
    assert!(
        answer.starts_with("You're in: Cosmic Cantina at 7:00 PM")
            && answer.ends_with(". I let Dana know."),
        "{answer}"
    );
    assert_eq!(text(&app.sidecar.sent_to(DANA_TG)[1]), "Jordan's in.");

    // The same answer again is no news: the judge hears it, Dana does not.
    inbound(&app, &press_as(JUDGE_TG, JUDGE_USERNAME, "Jordan", &accept)).await;
    app.sidecar.wait_for(6).await;
    app.sidecar.assert_no_more_than(6).await;
    assert_eq!(app.sidecar.sent_to(DANA_TG).len(), 2);

    // Maya can't make it.
    let decline = format!("invite:{id}:decline");
    inbound(&app, &press_as(MAYA_TG, MAYA_USERNAME, "Maya", &decline)).await;
    app.sidecar.wait_for(8).await;
    assert_eq!(
        text(app.sidecar.sent_to(MAYA_TG).last().unwrap()),
        "Got it, you can't make it. I let Dana know."
    );
    assert_eq!(
        text(&app.sidecar.sent_to(DANA_TG)[2]),
        "Maya can't make it."
    );

    // The judge changes their mind in the app: relayed by DM too.
    let token = app.state.jwt.issue_token("usr_judge").unwrap();
    let request = Request::post(format!("/v1/invites/{id}/respond"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"accept": false}).to_string()))
        .unwrap();
    let (status, _, invite) = common::send(app.router.clone(), request).await;
    assert_eq!(status, StatusCode::OK, "{invite}");
    assert_contract("Invite", &invite);
    app.sidecar.wait_for(9).await;
    app.sidecar.assert_no_more_than(9).await;
    assert_eq!(
        text(&app.sidecar.sent_to(DANA_TG)[3]),
        "Jordan can't make it."
    );

    // And in Dana's app: one notification per answer, with the plan.
    let relays: Vec<(String, String)> = sqlx::query_as(
        "SELECT title, body FROM ferry.notifications WHERE user_id = 'usr_dana'
          ORDER BY created_at, id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let titles: Vec<&str> = relays.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        titles,
        [
            "Jordan's in.",
            "Maya can't make it.",
            "Jordan can't make it."
        ]
    );
    for (_, body) in &relays {
        assert!(body.starts_with("Cosmic Cantina at 7:00 PM"), "{body}");
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn the_ping_with_a_place_and_a_time_one_of_them_or_neither(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    let cases = [
        (
            "anyone want to get Sup Dogs at 8 tomorrow?",
            json!({"place": "Sup Dogs", "when": "8 tomorrow"}),
            "Dana wants to go to Sup Dogs at 8:00 PM tomorrow. You in?",
            "Pinged Jordan, Leo and Maya: Sup Dogs at 8:00 PM tomorrow. I'll tell you who's in. Leo gets it in the app only.",
        ),
        (
            "anyone want to get Sup Dogs?",
            json!({"place": "sup dogs"}),
            "Dana wants to go to Sup Dogs. You in?",
            "Pinged Jordan, Leo and Maya: Sup Dogs. I'll tell you who's in. Leo gets it in the app only.",
        ),
        (
            "anyone want to meet up at 8pm tomorrow?",
            json!({"when": "8pm tomorrow"}),
            "Dana wants to meet up at 8:00 PM tomorrow. You in?",
            "Pinged Jordan, Leo and Maya: meeting up at 8:00 PM tomorrow. I'll tell you who's in. Leo gets it in the app only.",
        ),
        (
            "let's meet with the people I hung out with last time",
            json!({}),
            "Dana wants to meet up. You in?",
            "Pinged Jordan, Leo and Maya to meet up. I'll tell you who's in. Leo gets it in the app only.",
        ),
    ];
    for (i, (said, args, ping, reply)) in cases.iter().enumerate() {
        dana_asks(&app, said, args.clone(), 3 * (i + 1)).await;
        assert_eq!(text(&app.sidecar.sent_to(DANA_TG)[i]), *reply, "{said}");
        for tg in [JUDGE_TG, MAYA_TG] {
            assert_eq!(text(&app.sidecar.sent_to(tg)[i]), *ping, "{said}");
        }
    }
    app.sidecar.assert_no_more_than(12).await;
    let places: Vec<Value> = invites_of(&app, "usr_judge")
        .await
        .into_iter()
        .map(|i| i["place"].clone())
        .collect();
    // Newest first.
    assert_eq!(places[0], Value::Null);
    assert_eq!(places[1], Value::Null);
    assert_eq!(places[2]["id"], "rst_supdogs");
    assert_eq!(places[3]["label"], "Sup Dogs");
}

#[sqlx::test(migrations = "../migrations")]
async fn in_a_group_the_groups_circle_and_in_a_dm_the_last_circle(pool: PgPool) {
    seed_demo(&pool).await;
    map_demo_group(&pool).await;
    // Dana climbs too, and the climbers met last (Oct 8, after the 4th floor's Oct 3): her most
    // recent circle is cir_climb, not the group's cir_hinton4.
    sqlx::query(
        "INSERT INTO ferry.circle_members (circle_id, user_id) VALUES ('cir_climb', 'usr_dana')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let app = app(pool.clone(), true, &[]).await;

    // In her DM: the climbers, Maya and Leo (Leo in the app only).
    dana_asks(
        &app,
        "anyone want to go to Cosmic at 7pm?",
        json!({"place": "Cosmic", "when": "7pm"}),
        2,
    )
    .await;
    app.sidecar.assert_no_more_than(2).await;
    let first = invite_ids(&pool).await.remove(0);
    let (circle, invited): (String, Vec<String>) = sqlx::query_as(
        "SELECT i.circle_id, array_agg(r.user_id ORDER BY r.user_id) FROM ferry.invites i
           JOIN ferry.invite_responses r ON r.invite_id = i.id WHERE i.id = $1 GROUP BY i.circle_id",
    )
    .bind(&first)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(circle, "cir_climb");
    assert_eq!(invited, ["usr_leo", "usr_maya"]);
    assert!(app.sidecar.sent_to(JUDGE_TG).is_empty());
    assert!(
        text(&app.sidecar.sent_to(DANA_TG)[0]).starts_with("Pinged Leo and Maya: Cosmic Cantina")
    );

    // In the group: the group's circle, whatever her last circle is.
    gemini_calls(&app, json!({"place": "Cosmic", "when": "7pm"}));
    let said = group_message(
        DANA_TG,
        Some(DANA_USERNAME),
        "Dana",
        "ferry, anyone want to go to Cosmic at 7pm?",
    );
    assert!(inbound(&app, &said).await.is_empty());
    app.sidecar.wait_for(5).await;
    app.sidecar.assert_no_more_than(5).await;
    let second = invite_ids(&pool).await.remove(1);
    let (circle, invited): (String, Vec<String>) = sqlx::query_as(
        "SELECT i.circle_id, array_agg(r.user_id ORDER BY r.user_id) FROM ferry.invites i
           JOIN ferry.invite_responses r ON r.invite_id = i.id WHERE i.id = $1 GROUP BY i.circle_id",
    )
    .bind(&second)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(circle, "cir_hinton4");
    assert_eq!(invited, ["usr_judge", "usr_leo", "usr_maya"]);
    let in_group = app.sidecar.sent_to(GROUP_ID);
    assert!(
        text(&in_group[0]).starts_with("Pinged Jordan, Leo and Maya: Cosmic Cantina at 7:00 PM"),
        "{}",
        in_group[0]
    );
    assert!(
        text(&app.sidecar.sent_to(JUDGE_TG)[0]).starts_with("Dana wants to go to Cosmic Cantina")
    );
    assert_eq!(app.sidecar.sent_to(MAYA_TG).len(), 2);
}

#[sqlx::test(migrations = "../migrations")]
async fn a_typed_place_reaches_gemini_scrubbed_and_the_invitees_as_written(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    // Gemini copies the place from the scrubbed message, placeholder and all.
    app.gemini.respond_with(|body| {
        let contents = body["contents"].as_array().unwrap();
        let last = &contents[contents.len() - 1];
        if let Some(response) = last["parts"][0].get("functionResponse") {
            return (
                200,
                text_reply(response["response"]["say"].as_str().unwrap_or("done")),
            );
        }
        let said = last["parts"][0]["text"].as_str().unwrap_or_default();
        let place = said
            .split("go to ")
            .nth(1)
            .and_then(|rest| rest.split(" at ").next())
            .unwrap_or_default()
            .to_owned();
        (
            200,
            call_reply("start_meet_again", json!({"place": place, "when": "9pm"})),
        )
    });
    inbound(&app, &dana("anyone want to go to Maya's room at 9pm?")).await;
    app.sidecar.wait_for(3).await;

    let results = app.gemini.function_responses(1);
    let plan = results[0].1["plan"].as_str().unwrap();
    assert!(
        plan.starts_with("{person_") && plan.contains("}'s room at 9:00 PM"),
        "{plan}"
    );
    let judge = text(&app.sidecar.sent_to(JUDGE_TG)[0]).to_owned();
    assert!(
        judge.starts_with("Dana wants to go to Maya's room at 9:00 PM"),
        "{judge}"
    );
    assert!(
        text(&app.sidecar.sent_to(DANA_TG)[0])
            .starts_with("Pinged Jordan, Leo and Maya: Maya's room at 9:00 PM")
    );
    let invite = &invites_of(&app, "usr_judge").await[0];
    assert_eq!(
        invite["place"],
        json!({"id": null, "label": "Maya's room", "location": null})
    );
    // The drop scans every request for "Maya".
    app.gemini.assert_private();
}

#[sqlx::test(migrations = "../migrations")]
async fn a_timestamp_from_gemini_is_never_the_time(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    dana_asks(
        &app,
        "anyone want to go to Cosmic at 7pm?",
        json!({"place": "Cosmic", "when": "2026-10-11T23:00:00Z"}),
        1,
    )
    .await;
    app.sidecar.assert_no_more_than(1).await;
    assert_eq!(
        text(&app.sidecar.sent_to(DANA_TG)[0]),
        "That didn't work: Tell me the time the way you'd say it, like \"at 7pm\"."
    );
    assert!(invite_ids(&pool).await.is_empty());
}

/// The fallback's question for draft `id`'s buttons.
fn question_buttons(id: i64) -> Vec<Value> {
    vec![
        json!({"label": "Send", "callback_data": format!("meet:{id}:send")}),
        json!({"label": "Cancel", "callback_data": format!("meet:{id}:cancel")}),
    ]
}

/// The newest meet draft's id and status.
async fn newest_draft(pool: &PgPool) -> (i64, String) {
    sqlx::query_as("SELECT id, status FROM ferry.meet_drafts ORDER BY id DESC LIMIT 1")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../migrations")]
async fn with_gemini_down_the_patterns_read_it_and_the_bot_asks_once(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    app.gemini.set_down(503);

    // The question, and nothing sent to anyone else.
    inbound(&app, &dana("anyone want to go to Cosmic at 7pm?")).await;
    app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    let (id, status) = newest_draft(&pool).await;
    assert_eq!(status, "asked");
    let asked = &app.sidecar.sent_to(DANA_TG)[0];
    let line = text(asked);
    assert!(
        line.starts_with("Ask Jordan, Leo and Maya to go to Cosmic Cantina at 7:00 PM")
            && line.ends_with('?'),
        "{line}"
    );
    assert_eq!(buttons(asked), question_buttons(id));
    assert!(invite_ids(&pool).await.is_empty());

    // Only Dana's press counts.
    let send = meet::button_data(id, true);
    inbound(&app, &press_as(JUDGE_TG, JUDGE_USERNAME, "Jordan", &send)).await;
    app.sidecar.wait_for(2).await;
    assert_eq!(text(&app.sidecar.sent_to(JUDGE_TG)[0]), meet::NOT_YOURS);
    assert!(invite_ids(&pool).await.is_empty());

    // Dana's Send: the invite, the DMs and her line.
    inbound(&app, &press_as(DANA_TG, DANA_USERNAME, "Dana", &send)).await;
    app.sidecar.wait_for(5).await;
    app.sidecar.assert_no_more_than(5).await;
    let ids = invite_ids(&pool).await;
    assert_eq!(ids.len(), 1);
    assert_next_day(&pool, &ids[0]).await;
    let sent = text(&app.sidecar.sent_to(DANA_TG)[1]).to_owned();
    assert!(
        sent.starts_with("Pinged Jordan, Leo and Maya: Cosmic Cantina at 7:00 PM")
            && sent.ends_with("Leo gets it in the app only."),
        "{sent}"
    );
    assert!(
        text(&app.sidecar.sent_to(MAYA_TG)[0])
            .starts_with("Dana wants to go to Cosmic Cantina at 7:00 PM")
    );
    let (draft_status, invite_id): (String, Option<String>) =
        sqlx::query_as("SELECT status, invite_id FROM ferry.meet_drafts WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(draft_status, "sent");
    assert_eq!(invite_id.as_deref(), Some(ids[0].as_str()));

    // A second press sends nothing more.
    inbound(&app, &press_as(DANA_TG, DANA_USERNAME, "Dana", &send)).await;
    app.sidecar.wait_for(6).await;
    assert_eq!(text(&app.sidecar.sent_to(DANA_TG)[2]), meet::ALREADY_SENT);
    assert_eq!(invite_ids(&pool).await.len(), 1);

    // Another question, cancelled; Send after it does nothing.
    inbound(&app, &dana("anyone want to get Sup Dogs at 8 tomorrow?")).await;
    app.sidecar.wait_for(7).await;
    assert_eq!(
        text(&app.sidecar.sent_to(DANA_TG)[3]),
        "Ask Jordan, Leo and Maya to go to Sup Dogs at 8:00 PM tomorrow?"
    );
    let (sup_dogs, _) = newest_draft(&pool).await;
    inbound(
        &app,
        &press_as(
            DANA_TG,
            DANA_USERNAME,
            "Dana",
            &meet::button_data(sup_dogs, false),
        ),
    )
    .await;
    app.sidecar.wait_for(8).await;
    assert_eq!(text(&app.sidecar.sent_to(DANA_TG)[4]), meet::CANCELLED);
    inbound(
        &app,
        &press_as(
            DANA_TG,
            DANA_USERNAME,
            "Dana",
            &meet::button_data(sup_dogs, true),
        ),
    )
    .await;
    app.sidecar.wait_for(9).await;
    assert_eq!(
        text(&app.sidecar.sent_to(DANA_TG)[5]),
        meet::ALREADY_CANCELLED
    );

    // A question left half an hour expires.
    inbound(&app, &dana("anyone want to get Sup Dogs at 8 tomorrow?")).await;
    app.sidecar.wait_for(10).await;
    let (old, _) = newest_draft(&pool).await;
    sqlx::query(
        "UPDATE ferry.meet_drafts SET created_at = now() - interval '31 minutes' WHERE id = $1",
    )
    .bind(old)
    .execute(&pool)
    .await
    .unwrap();
    inbound(
        &app,
        &press_as(
            DANA_TG,
            DANA_USERNAME,
            "Dana",
            &meet::button_data(old, true),
        ),
    )
    .await;
    app.sidecar.wait_for(11).await;
    assert_eq!(text(&app.sidecar.sent_to(DANA_TG)[7]), meet::EXPIRED);

    // A time that passed while the question waited: refused, and the draft is asked again.
    inbound(&app, &dana("anyone want to get Sup Dogs at 8 tomorrow?")).await;
    app.sidecar.wait_for(12).await;
    let (late, _) = newest_draft(&pool).await;
    sqlx::query(
        "UPDATE ferry.meet_drafts SET proposed_time = now() - interval '1 minute' WHERE id = $1",
    )
    .bind(late)
    .execute(&pool)
    .await
    .unwrap();
    inbound(
        &app,
        &press_as(
            DANA_TG,
            DANA_USERNAME,
            "Dana",
            &meet::button_data(late, true),
        ),
    )
    .await;
    app.sidecar.wait_for(13).await;
    app.sidecar.assert_no_more_than(13).await;
    assert_eq!(text(&app.sidecar.sent_to(DANA_TG)[9]), meet::PASSED);
    let (_, status) = newest_draft(&pool).await;
    assert_eq!(status, "asked");
    assert_eq!(invite_ids(&pool).await.len(), 1);
}
