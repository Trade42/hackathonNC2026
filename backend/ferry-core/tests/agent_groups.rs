//! Telegram chats and the people in them, through `POST /internal/messages/inbound` with a stub
//! sidecar and a stub Gemini (never the real one): unknown senders told once, a duplicate delivery
//! answered once, groups mapped to circles (by membership, lazily, from a startgroup link), admin
//! promotion storing the invite link, removal and supergroup migration, blocked DMs and 403s
//! marking a chat unreachable, and invite buttons pressed by the wrong person.
mod agent_support;
mod common;

use std::time::Duration;

use agent_support::{
    BOT_USERNAME, DANA_TG, DANA_USERNAME, GROUP_ID, GROUP_TITLE, JUDGE_TG, JUDGE_USERNAME, MAYA_TG,
    MAYA_USERNAME, STRANGER_TG, STUB_INVITE_LINK, TestApp, app, dm, group_message, inbound,
    map_demo_group, membership, press, seed_demo,
};
use ferry_core::agent::dispatch::UNKNOWN_SENDER;
use ferry_core::agent::spaces::GROUP_HELLO;
use serde_json::{Value, json};
use sqlx::PgPool;

/// Another Telegram account with no Ferry user.
const OTHER_STRANGER_TG: &str = "888888888";
/// What Dana's meet-again says before naming who gets it in the app only.
const DANA_INVITE_SENT: &str = "Pinged Jordan, Leo and Maya to meet up. I'll tell you who's in.";

fn text(sent: &Value) -> &str {
    sent["text"].as_str().unwrap()
}

fn buttons(sent: &Value) -> Vec<Value> {
    sent["buttons"].as_array().cloned().unwrap_or_default()
}

/// A message in the group `space_id` titled `title`: [`group_message`] moved to another chat.
fn message_in(
    space_id: &str,
    title: &str,
    sender_id: &str,
    username: &str,
    name: &str,
    text: &str,
) -> Value {
    let mut event = group_message(sender_id, Some(username), name, text);
    event["space_id"] = json!(space_id);
    event["chat_title"] = json!(title);
    event
}

/// `ferry.message_spaces` for `space_id`: (circle_id, bot_in_chat, bot_is_admin, invite_url).
type SpaceRow = (Option<String>, bool, bool, Option<String>);

async fn space(pool: &PgPool, space_id: &str) -> Option<SpaceRow> {
    sqlx::query_as(
        "SELECT circle_id, bot_in_chat, bot_is_admin, invite_url FROM ferry.message_spaces
          WHERE space_id = $1",
    )
    .bind(space_id)
    .fetch_optional(pool)
    .await
    .unwrap()
}

/// Polls `space_id`'s row for up to 2 s until `ok` holds; returns the row (spawned tasks write
/// some of it after the inbound call returns).
async fn space_until(pool: &PgPool, space_id: &str, ok: impl Fn(&SpaceRow) -> bool) -> SpaceRow {
    for _ in 0..200 {
        if let Some(row) = space(pool, space_id).await
            && ok(&row)
        {
            return row;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "message_spaces {space_id} never got there: {:?}",
        space(pool, space_id).await
    );
}

async fn count(pool: &PgPool, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

async fn is_member(pool: &PgPool, circle_id: &str, user_id: &str) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM ferry.circle_members WHERE circle_id = $1 AND user_id = $2)",
    )
    .bind(circle_id)
    .bind(user_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The Chapel Hill hour now, from the database (the catalog computes `is_open` from it).
async fn local_hour(pool: &PgPool) -> i32 {
    sqlx::query_scalar("SELECT extract(hour FROM now() AT TIME ZONE 'America/New_York')::int")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// list_restaurants' answer to "what's open?". The fixtures' hours leave nothing open from 04:00
/// to 07:00 in Chapel Hill; then the line says so instead.
async fn assert_open_line(pool: &PgPool, line: &str) {
    if (4..7).contains(&local_hour(pool).await) {
        assert!(
            line.starts_with("Open now:") || line == "Nothing that matches is open right now.",
            "{line}"
        );
    } else {
        assert!(line.starts_with("Open now:"), "{line}");
    }
}

/// Dana's "let's meet with the people I hung out with last time" in her DM (Gemini off: the
/// fallback routes it to start_meet_again).
async fn danas_meet_again(app: &TestApp) {
    let event = dm(
        DANA_TG,
        Some(DANA_USERNAME),
        "Dana",
        "let's meet with the people I hung out with last time",
    );
    assert!(inbound(app, &event).await.is_empty());
}

async fn the_invite(pool: &PgPool) -> String {
    sqlx::query_scalar("SELECT id FROM ferry.invites")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../migrations")]
async fn an_unknown_sender_is_told_once(pool: PgPool) {
    seed_demo(&pool).await;
    map_demo_group(&pool).await;
    let app = app(pool.clone(), true, &[]).await;

    let hi = dm(STRANGER_TG, Some("stranger"), "Sam", "hi");
    assert!(inbound(&app, &hi).await.is_empty());
    let sent = app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    assert_eq!(sent[0].target().as_deref(), Some(STRANGER_TG));
    assert_eq!(text(&sent[0].body), UNKNOWN_SENDER);
    let link = buttons(&sent[0].body);
    assert_eq!(link.len(), 1, "{}", sent[0].body);
    assert_eq!(link[0]["label"], "Open Ferry");
    assert_eq!(link[0]["url"], "https://ferry.example");

    // Then silence: in the DM, and in a group where the same account addresses the bot.
    inbound(&app, &dm(STRANGER_TG, Some("stranger"), "Sam", "hello?")).await;
    app.sidecar.assert_no_more_than(1).await;
    let told: Vec<String> =
        sqlx::query_scalar("SELECT telegram_user_id FROM ferry.unknown_senders")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(told, [STRANGER_TG]);
    let in_group = group_message(STRANGER_TG, Some("stranger"), "Sam", "ferry what's open?");
    inbound(&app, &in_group).await;
    app.sidecar.assert_no_more_than(1).await;

    // Someone else unknown addressing the bot in the group gets the line there, once.
    let other = group_message(
        OTHER_STRANGER_TG,
        Some("other"),
        "Alex",
        "ferry what's open?",
    );
    inbound(&app, &other).await;
    let sent = app.sidecar.wait_for(2).await;
    app.sidecar.assert_no_more_than(2).await;
    assert_eq!(sent[1].body["space_id"], GROUP_ID);
    assert_eq!(text(&sent[1].body), UNKNOWN_SENDER);
    // Nobody unknown is logged as a turn, and nothing reached Gemini.
    assert_eq!(
        count(&pool, "SELECT count(*) FROM ferry.agent_turns").await,
        0
    );
    assert_eq!(app.gemini.request_count(), 0);
    app.gemini.assert_private();
}

#[sqlx::test(migrations = "../migrations")]
async fn a_duplicate_delivery_is_answered_once(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), false, &[]).await;
    let event = dm(
        JUDGE_TG,
        Some(JUDGE_USERNAME),
        "Jordan",
        "what are my defaults?",
    );
    assert!(inbound(&app, &event).await.is_empty());
    assert!(inbound(&app, &event.clone()).await.is_empty());
    let sent = app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    assert_eq!(sent[0].body["space_id"], JUDGE_TG);
    assert!(
        text(&sent[0].body).starts_with("Your Ferry defaults: "),
        "{}",
        sent[0].body
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM ferry.agent_turns").await,
        1
    );
    assert_eq!(app.gemini.request_count(), 0);
}

#[sqlx::test(migrations = "../migrations")]
async fn a_membership_event_maps_the_demo_group_and_says_hello_once(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    assert_eq!(space(&pool, GROUP_ID).await, None);

    let added = membership(
        GROUP_ID,
        true,
        MAYA_TG,
        Some(GROUP_TITLE),
        true,
        Some(false),
        None,
    );
    assert!(inbound(&app, &added).await.is_empty());
    let (circle, in_chat, admin, invite_url) = space(&pool, GROUP_ID).await.unwrap();
    assert_eq!(circle.as_deref(), Some("cir_hinton4"));
    assert!(in_chat);
    assert!(!admin);
    assert_eq!(invite_url, None);
    let sent = app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    assert_eq!(sent[0].body["space_id"], GROUP_ID);
    assert_eq!(text(&sent[0].body), GROUP_HELLO);
    assert!(buttons(&sent[0].body).is_empty());

    // Another membership event for the same chat (a change of rights) says nothing new.
    let again = membership(
        GROUP_ID,
        true,
        MAYA_TG,
        Some(GROUP_TITLE),
        true,
        Some(false),
        None,
    );
    inbound(&app, &again).await;
    app.sidecar.assert_no_more_than(1).await;
    assert_eq!(
        count(&pool, "SELECT count(*) FROM ferry.message_spaces").await,
        1
    );
    assert_eq!(app.gemini.request_count(), 0);
    app.gemini.assert_private();
}

#[sqlx::test(migrations = "../migrations")]
async fn a_new_group_gets_a_circle_named_after_its_title(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), false, &[]).await;
    let chat = "-100777";

    let added = membership(chat, true, MAYA_TG, Some("Climbing crew"), true, None, None);
    inbound(&app, &added).await;
    let (circle, in_chat, admin, _) = space(&pool, chat).await.unwrap();
    let circle = circle.expect("the group has a circle");
    assert_ne!(circle, "cir_hinton4");
    assert!(in_chat && !admin);
    let (name, emoji): (String, String) =
        sqlx::query_as("SELECT name, emoji FROM ferry.circles WHERE id = $1")
            .bind(&circle)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((name.as_str(), emoji.as_str()), ("Climbing crew", "👥"));
    assert!(
        is_member(&pool, &circle, "usr_maya").await,
        "whoever adds the bot joins"
    );
    assert!(!is_member(&pool, &circle, "usr_judge").await);
    let sent = app.sidecar.wait_for(1).await;
    assert_eq!(text(&sent[0].body), GROUP_HELLO);

    // A linked sender joins the circle when they first speak there.
    let asked = message_in(
        chat,
        "Climbing crew",
        JUDGE_TG,
        JUDGE_USERNAME,
        "Jordan",
        "ferry what's open?",
    );
    assert!(inbound(&app, &asked).await.is_empty());
    assert!(is_member(&pool, &circle, "usr_judge").await);
    let sent = app.sidecar.wait_for(2).await;
    app.sidecar.assert_no_more_than(2).await;
    assert_eq!(sent[1].body["space_id"], chat);
    assert_open_line(&pool, text(&sent[1].body)).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn the_demo_group_maps_lazily_on_its_first_message(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), false, &[]).await;
    assert_eq!(space(&pool, GROUP_ID).await, None);

    let asked = group_message(MAYA_TG, Some(MAYA_USERNAME), "Maya", "ferry what's open?");
    assert!(inbound(&app, &asked).await.is_empty());
    let (circle, in_chat, _, _) = space(&pool, GROUP_ID).await.unwrap();
    assert_eq!(circle.as_deref(), Some("cir_hinton4"));
    assert!(in_chat);
    let sent = app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    assert_eq!(sent[0].body["space_id"], GROUP_ID);
    assert_open_line(&pool, text(&sent[0].body)).await;
    // No hello: the bot was there before this code shipped.
    assert_ne!(text(&sent[0].body), GROUP_HELLO);
}

#[sqlx::test(migrations = "../migrations")]
async fn an_admin_promotion_stores_the_groups_invite_link(pool: PgPool) {
    seed_demo(&pool).await;
    map_demo_group(&pool).await;
    let app = app(pool.clone(), true, &[]).await;

    let promoted = membership(
        GROUP_ID,
        true,
        MAYA_TG,
        Some(GROUP_TITLE),
        true,
        Some(true),
        None,
    );
    assert!(inbound(&app, &promoted).await.is_empty());
    let (circle, in_chat, admin, invite_url) =
        space_until(&pool, GROUP_ID, |row| row.3.is_some()).await;
    assert_eq!(invite_url.as_deref(), Some(STUB_INVITE_LINK));
    assert!(admin && in_chat);
    assert_eq!(circle.as_deref(), Some("cir_hinton4"));
    let calls = app.sidecar.invite_link_calls();
    assert_eq!(calls.len(), 1, "{calls:#?}");
    assert_eq!(calls[0]["space_id"], GROUP_ID);
    // The bot was in the group already: no hello.
    app.sidecar.assert_no_more_than(0).await;

    // A sidecar that says the bot may not invite: nothing stored, nothing broken.
    app.sidecar.set_invite_link_status(403, "not_admin");
    let other = "-100888";
    let promoted = membership(
        other,
        true,
        MAYA_TG,
        Some("Study group"),
        true,
        Some(true),
        None,
    );
    assert!(inbound(&app, &promoted).await.is_empty());
    for _ in 0..200 {
        if app.sidecar.invite_link_calls().len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let calls = app.sidecar.invite_link_calls();
    assert_eq!(calls.len(), 2, "{calls:#?}");
    assert_eq!(calls[1]["space_id"], other);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (circle, in_chat, _, invite_url) = space(&pool, other).await.unwrap();
    assert!(circle.is_some() && in_chat);
    assert_eq!(invite_url, None);
    // The first group's link is untouched.
    let (_, _, _, kept) = space(&pool, GROUP_ID).await.unwrap();
    assert_eq!(kept.as_deref(), Some(STUB_INVITE_LINK));
    assert_eq!(app.gemini.request_count(), 0);
    app.gemini.assert_private();
}

#[sqlx::test(migrations = "../migrations")]
async fn removal_keeps_the_circle_and_migration_moves_the_group(pool: PgPool) {
    seed_demo(&pool).await;
    map_demo_group(&pool).await;
    let app = app(pool.clone(), false, &[]).await;

    // A trip in the group before it moves (Gemini off: the patterns read it).
    let trip = group_message(
        MAYA_TG,
        Some(MAYA_USERNAME),
        "Maya",
        "heading to sup dogs at 7pm",
    );
    inbound(&app, &trip).await;
    let sent = app.sidecar.wait_for(1).await;
    assert!(
        text(&sent[0].body).starts_with("Open a Ferry to Sup Dogs?"),
        "{}",
        sent[0].body
    );
    let trip_space: String = sqlx::query_scalar("SELECT space_id FROM ferry.trip_intents")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(trip_space, GROUP_ID);

    // The bot removed: the row and its circle stay, marked out of the chat; nothing is said.
    let removed = membership(
        GROUP_ID,
        true,
        MAYA_TG,
        Some(GROUP_TITLE),
        false,
        Some(false),
        None,
    );
    assert!(inbound(&app, &removed).await.is_empty());
    let (circle, in_chat, admin, _) = space(&pool, GROUP_ID).await.unwrap();
    assert_eq!(circle.as_deref(), Some("cir_hinton4"));
    assert!(!in_chat && !admin);
    app.sidecar.assert_no_more_than(1).await;

    // The group became a supergroup: everything moves to the new id, and the move is no join.
    let new_id = "-1009999";
    let moved = membership(
        new_id,
        true,
        MAYA_TG,
        Some(GROUP_TITLE),
        true,
        None,
        Some(GROUP_ID),
    );
    assert!(inbound(&app, &moved).await.is_empty());
    assert_eq!(space(&pool, GROUP_ID).await, None, "the old row is gone");
    let (circle, in_chat, _, _) = space(&pool, new_id).await.unwrap();
    assert_eq!(circle.as_deref(), Some("cir_hinton4"));
    assert!(in_chat);
    let trip_space: String = sqlx::query_scalar("SELECT space_id FROM ferry.trip_intents")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(trip_space, new_id);
    app.sidecar.assert_no_more_than(1).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn a_blocked_dm_gets_the_invite_in_the_app_only(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), false, &[]).await;

    let blocked = membership(MAYA_TG, false, MAYA_TG, None, false, None, None);
    assert!(inbound(&app, &blocked).await.is_empty());
    let (circle, in_chat, _, _) = space(&pool, MAYA_TG).await.unwrap();
    assert_eq!(circle, None);
    assert!(!in_chat);
    let dm_user: Option<String> =
        sqlx::query_scalar("SELECT dm_user_id FROM ferry.message_spaces WHERE space_id = $1")
            .bind(MAYA_TG)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(dm_user.as_deref(), Some("usr_maya"));

    danas_meet_again(&app).await;
    app.sidecar.wait_for(2).await;
    app.sidecar.assert_no_more_than(2).await;
    // The invitees' DMs go out as the invite is made, so they may come before Dana's reply.
    let reply = app.sidecar.sent_to(DANA_TG);
    assert_eq!(
        text(&reply[0]),
        format!("{DANA_INVITE_SENT} Leo and Maya get it in the app only.")
    );
    let dm = app.sidecar.sent_to(JUDGE_TG);
    assert_eq!(dm.len(), 1, "only the judge gets a DM");
    assert_eq!(text(&dm[0]), "Dana wants to meet up. You in?");
    // Maya still gets the app's notification.
    let maya_notified: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ferry.notifications WHERE user_id = 'usr_maya' AND type = 'invite'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(maya_notified, 1);

    // Unblocked: reachable again.
    let unblocked = membership(MAYA_TG, false, MAYA_TG, None, true, None, None);
    assert!(inbound(&app, &unblocked).await.is_empty());
    let (_, in_chat, _, _) = space(&pool, MAYA_TG).await.unwrap();
    assert!(in_chat);
    app.sidecar.assert_no_more_than(2).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn a_403_from_the_sidecar_marks_the_dm_unreachable(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), false, &[]).await;
    app.sidecar
        .set_send_status(JUDGE_TG, 403, "recipient_not_reachable");
    assert_eq!(space(&pool, JUDGE_TG).await, None);

    danas_meet_again(&app).await;
    let sent = app.sidecar.wait_for(3).await;
    app.sidecar.assert_no_more_than(3).await;
    assert_eq!(
        text(&app.sidecar.sent_to(DANA_TG)[0]),
        DANA_INVITE_SENT.to_owned() + " Leo gets it in the app only."
    );
    let mut targets: Vec<String> = sent
        .iter()
        .filter_map(|s| s.target())
        .filter(|t| t != DANA_TG)
        .collect();
    targets.sort();
    assert_eq!(targets, [JUDGE_TG, MAYA_TG]);
    let (circle, in_chat, admin, _) = space_until(&pool, JUDGE_TG, |row| !row.1).await;
    assert_eq!(circle, None);
    assert!(!in_chat && !admin);
    let (is_group, dm_user): (bool, Option<String>) =
        sqlx::query_as("SELECT is_group, dm_user_id FROM ferry.message_spaces WHERE space_id = $1")
            .bind(JUDGE_TG)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!is_group);
    assert_eq!(dm_user.as_deref(), Some("usr_judge"));
    // Maya's DM went through and is not marked.
    assert_eq!(space(&pool, MAYA_TG).await, None);
}

#[sqlx::test(migrations = "../migrations")]
async fn a_startgroup_link_maps_the_group_to_the_invites_circle(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), false, &[]).await;

    danas_meet_again(&app).await;
    app.sidecar.wait_for(3).await;
    let invite_id = the_invite(&pool).await;
    let accept = format!("invite:{invite_id}:accept");
    let pressed = press(
        JUDGE_TG,
        false,
        JUDGE_TG,
        Some(JUDGE_USERNAME),
        "Jordan",
        &accept,
    );
    assert!(inbound(&app, &pressed).await.is_empty());
    // The judge's answer, and its relay to Dana, in either order.
    app.sidecar.wait_for(5).await;
    let answer = app.sidecar.sent_to(JUDGE_TG);
    assert_eq!(answer.len(), 2, "the invite's DM, then the answer");
    assert_eq!(text(&answer[1]), "You're in! I let Dana know.");
    assert_eq!(
        text(app.sidecar.sent_to(DANA_TG).last().unwrap()),
        "Jordan's in."
    );
    let start = format!("/start@{BOT_USERNAME} {invite_id}");

    // Maya never answered: her startgroup maps nothing to the invite's circle.
    let maya_group = "-1006666";
    let maya_start = message_in(
        maya_group,
        "Other group",
        MAYA_TG,
        MAYA_USERNAME,
        "Maya",
        &start,
    );
    assert!(inbound(&app, &maya_start).await.is_empty());
    let (circle, _, _, _) = space(&pool, maya_group).await.unwrap();
    assert_ne!(circle.as_deref(), Some("cir_hinton4"));
    app.sidecar.assert_no_more_than(5).await;

    // The judge accepted: adding the bot makes a throwaway circle, then the payload maps it.
    let chat = "-1005555";
    let added = membership(chat, true, JUDGE_TG, Some("New group"), true, None, None);
    inbound(&app, &added).await;
    let sent = app.sidecar.wait_for(6).await;
    assert_eq!(text(&sent[5].body), GROUP_HELLO);
    let (throwaway, _, _, _) = space(&pool, chat).await.unwrap();
    let throwaway = throwaway.unwrap();
    let name: String = sqlx::query_scalar("SELECT name FROM ferry.circles WHERE id = $1")
        .bind(&throwaway)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "New group");

    let judge_start = message_in(
        chat,
        "New group",
        JUDGE_TG,
        JUDGE_USERNAME,
        "Jordan",
        &start,
    );
    assert!(inbound(&app, &judge_start).await.is_empty());
    let (circle, in_chat, _, _) = space(&pool, chat).await.unwrap();
    assert_eq!(circle.as_deref(), Some("cir_hinton4"));
    assert!(in_chat);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM ferry.circles WHERE id = $1")
        .bind(&throwaway)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0, "the throwaway circle is deleted");
    let sent = app.sidecar.wait_for(7).await;
    app.sidecar.assert_no_more_than(7).await;
    assert_eq!(sent[6].body["space_id"], chat);
    assert_eq!(
        text(&sent[6].body),
        "This group is now 🌊 Hinton James 4th floor on Ferry."
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn invite_buttons_answer_only_the_invited(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), false, &[]).await;
    danas_meet_again(&app).await;
    app.sidecar.wait_for(3).await;
    let invite_id = the_invite(&pool).await;

    // The inviter presses Accept on her own invite.
    let own = format!("invite:{invite_id}:accept");
    inbound(
        &app,
        &press(DANA_TG, false, DANA_TG, Some(DANA_USERNAME), "Dana", &own),
    )
    .await;
    let sent = app.sidecar.wait_for(4).await;
    assert_eq!(sent[3].body["space_id"], DANA_TG);
    assert_eq!(
        text(&sent[3].body),
        "You sent this invite; only the people you invited answer it."
    );
    assert!(buttons(&sent[3].body).is_empty());

    // An invite that does not exist.
    let missing = "invite:inv_doesnotexist0:accept";
    inbound(
        &app,
        &press(
            DANA_TG,
            false,
            DANA_TG,
            Some(DANA_USERNAME),
            "Dana",
            missing,
        ),
    )
    .await;
    let sent = app.sidecar.wait_for(5).await;
    app.sidecar.assert_no_more_than(5).await;
    assert_eq!(text(&sent[4].body), "This invite isn't one you can answer.");
    // Nothing was recorded for either press.
    let answered: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ferry.invite_responses WHERE status <> 'pending'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(answered, 0);
}
