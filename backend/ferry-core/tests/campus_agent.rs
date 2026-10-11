//! The public community every user is in (`cir_campus`, "Campus" below) and the Telegram agent's
//! circle choice, through `POST /internal/messages/inbound` with a
//! stub sidecar and Gemini off (the patterns and the fallback answer):
//!
//! - In a DM, meet-again goes to the user's friends' circle, never Campus, even when Campus met
//!   last.
//! - In a DM, a trip lands in the user's friends' circle when they have one.
//! - A user whose only circle is Campus: the DM trip lands in Campus, both the question and the
//!   opened line say it is for everyone there (by the circle's name from `ferry.circles`), and Yes
//!   opens its run there.
//! - A user in no circle at all, Campus included: the trip has no circle, as before Campus.
mod agent_support;
mod common;

use agent_support::{DANA_TG, DANA_USERNAME, TestApp, app, dm, inbound, press, seed_demo};
use ferry_core::agent::trips;
use ferry_core::circles::CAMPUS_CIRCLE_ID;
use serde_json::Value;
use sqlx::PgPool;

/// A Ferry user with Telegram linked and no circle but Campus (the trigger adds them to it).
const RAE: &str = "usr_rae";
const RAE_TG: &str = "777777777";
const RAE_USERNAME: &str = "rae_tg";

const TRIP: &str = "heading to Cosmic Cantina at 6, anyone want anything?";

fn text(sent: &Value) -> &str {
    sent["text"].as_str().unwrap()
}

/// Campus met just now, so it would be anyone's most recent circle if it were ever picked.
async fn campus_met_last(pool: &PgPool) {
    let done = sqlx::query("UPDATE ferry.circles SET last_met_at = now() WHERE id = $1")
        .bind(CAMPUS_CIRCLE_ID)
        .execute(pool)
        .await
        .unwrap();
    assert_eq!(done.rows_affected(), 1, "the migration seeds Campus");
}

async fn circles_of(pool: &PgPool, user_id: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT circle_id FROM ferry.circle_members WHERE user_id = $1 ORDER BY circle_id",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// Rae: a new user, linked to Telegram, in Campus only.
async fn seed_rae(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO ferry.users (id, phone_e164, display_name, avatar_seed, bubble_color, telegram_user_id)
         VALUES ($1, '+12025550177', 'Rae', $1, '#225588', $2)",
    )
    .bind(RAE)
    .bind(RAE_TG)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("UPDATE ferry.users SET telegram_username = $2 WHERE id = $1")
        .bind(RAE)
        .bind(RAE_USERNAME)
        .execute(pool)
        .await
        .unwrap();
    assert_eq!(circles_of(pool, RAE).await, [CAMPUS_CIRCLE_ID]);
}

/// The only trip intent: (id, circle_id, status).
async fn the_intent(pool: &PgPool) -> (i64, Option<String>, String) {
    sqlx::query_as("SELECT id, circle_id, status FROM ferry.trip_intents")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// `tg`'s DM trip to Cosmic Cantina (Gemini off: the patterns read it and the bot asks once);
/// returns the question.
async fn dm_trip(app: &TestApp, tg: &str, username: &str, name: &str) -> String {
    assert!(
        inbound(app, &dm(tg, Some(username), name, TRIP))
            .await
            .is_empty()
    );
    let sent = app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    assert_eq!(sent[0].target().as_deref(), Some(tg));
    text(&sent[0].body).to_owned()
}

#[sqlx::test(migrations = "../migrations")]
async fn a_dm_meet_again_goes_to_the_friends_circle_never_campus(pool: PgPool) {
    seed_demo(&pool).await;
    campus_met_last(&pool).await;
    assert_eq!(
        circles_of(&pool, "usr_dana").await,
        [CAMPUS_CIRCLE_ID, "cir_hinton4"]
    );
    let app = app(pool.clone(), false, &[]).await;

    // Gemini off: the fallback routes it to start_meet_again.
    let event = dm(
        DANA_TG,
        Some(DANA_USERNAME),
        "Dana",
        "let's meet with the people I hung out with last time",
    );
    assert!(inbound(&app, &event).await.is_empty());
    let sent = app.sidecar.wait_for(1).await;
    assert!(
        text(&app.sidecar.sent_to(DANA_TG)[0]).starts_with("Pinged Jordan, Leo and Maya"),
        "{sent:#?}"
    );

    let (circle, invited): (String, Vec<String>) = sqlx::query_as(
        "SELECT i.circle_id, array_agg(r.user_id ORDER BY r.user_id) FROM ferry.invites i
           JOIN ferry.invite_responses r ON r.invite_id = i.id GROUP BY i.circle_id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(circle, "cir_hinton4");
    assert_eq!(invited, ["usr_judge", "usr_leo", "usr_maya"]);
}

#[sqlx::test(migrations = "../migrations")]
async fn a_dm_trip_lands_in_the_friends_circle_when_there_is_one(pool: PgPool) {
    seed_demo(&pool).await;
    campus_met_last(&pool).await;
    let app = app(pool.clone(), false, &[]).await;

    let asked = dm_trip(&app, DANA_TG, DANA_USERNAME, "Dana").await;
    assert!(
        asked.starts_with("Open a Ferry to Cosmic Cantina? Orders close "),
        "{asked}"
    );
    let (_, circle, status) = the_intent(&pool).await;
    assert_eq!(circle.as_deref(), Some("cir_hinton4"));
    assert_eq!(status, "asked");
}

#[sqlx::test(migrations = "../migrations")]
async fn a_campus_only_users_dm_trip_lands_in_campus(pool: PgPool) {
    seed_demo(&pool).await;
    seed_rae(&pool).await;
    let name: String = sqlx::query_scalar("SELECT name FROM ferry.circles WHERE id = $1")
        .bind(CAMPUS_CIRCLE_ID)
        .fetch_one(&pool)
        .await
        .unwrap();
    let app = app(pool.clone(), false, &[]).await;

    let asked = dm_trip(&app, RAE_TG, RAE_USERNAME, "Rae").await;
    assert!(
        asked.starts_with(&format!(
            "Open a Ferry to Cosmic Cantina for everyone in {name}? Orders close "
        )),
        "{asked}"
    );
    let (id, circle, status) = the_intent(&pool).await;
    assert_eq!(circle.as_deref(), Some(CAMPUS_CIRCLE_ID));
    assert_eq!(status, "asked");

    // Yes opens the run in Campus.
    let yes = trips::button_data(id, true);
    inbound(
        &app,
        &press(RAE_TG, false, RAE_TG, Some(RAE_USERNAME), "Rae", &yes),
    )
    .await;
    let sent = app.sidecar.wait_for(2).await;
    assert!(
        text(&sent[1].body).starts_with(&format!(
            "Ferry to Cosmic Cantina for everyone in {name} is open until "
        )),
        "{}",
        sent[1].body
    );
    let (status, run_circle): (String, String) = sqlx::query_as(
        "SELECT t.status, r.circle_id FROM ferry.trip_intents t
           JOIN ferry.runs r ON r.id = t.run_id WHERE t.id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(status, "opened");
    assert_eq!(run_circle, CAMPUS_CIRCLE_ID);
}

#[sqlx::test(migrations = "../migrations")]
async fn a_user_in_no_circle_at_all_gets_no_circle(pool: PgPool) {
    seed_demo(&pool).await;
    seed_rae(&pool).await;
    sqlx::query("DELETE FROM ferry.circle_members WHERE user_id = $1")
        .bind(RAE)
        .execute(&pool)
        .await
        .unwrap();
    let app = app(pool.clone(), false, &[]).await;

    let asked = dm_trip(&app, RAE_TG, RAE_USERNAME, "Rae").await;
    assert!(
        asked.starts_with("Open a Ferry to Cosmic Cantina? Orders close "),
        "{asked}"
    );
    let (id, circle, _) = the_intent(&pool).await;
    assert_eq!(circle, None);

    // Yes has no circle to open it in.
    let yes = trips::button_data(id, true);
    inbound(
        &app,
        &press(RAE_TG, false, RAE_TG, Some(RAE_USERNAME), "Rae", &yes),
    )
    .await;
    let sent = app.sidecar.wait_for(2).await;
    assert_eq!(text(&sent[1].body), trips::NO_CIRCLE);
}
