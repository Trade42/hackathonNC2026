//! Meet-again invites end to end: POST /v1/circles/{id}/meet-again, GET /v1/invites and
//! POST /v1/invites/{id}/respond (and Telegram DM buttons), with the notification.created and
//! invite.updated events each user gets over /v1/stream, and the Telegram `open_url` only for the
//! inviter and the accepted invitees. No response carries a phone number.
mod common;
mod realtime_support;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::assert_envelope;
use ferry_core::state::AppState;
use realtime_support::{Server, Ws, assert_contract, assert_silent, next_json, serve};
use serde_json::{Value, json};
use sqlx::PgPool;

const WEB_URL: &str = "https://ferry.example";
const QUIET: Duration = Duration::from_millis(300);

const BOT: &str = "Ferrygo_bot";

/// The bot's startgroup link for an invite.
fn startgroup(invite_id: &str) -> String {
    format!("https://t.me/{BOT}?startgroup={invite_id}")
}

/// Asserts no phone number (all of them are +1 919 555 01xx) appears anywhere in `value`.
fn assert_no_phone(value: &Value) {
    let text = value.to_string();
    assert!(!text.contains("919555"), "a phone number leaked: {text}");
}

async fn add_user(pool: &PgPool, id: &str, name: &str, phone: &str) {
    sqlx::query(
        "INSERT INTO ferry.users (id, phone_e164, display_name, avatar_seed, bubble_color) \
         VALUES ($1, $2, $3, $1, '#336699')",
    )
    .bind(id)
    .bind(phone)
    .bind(name)
    .execute(pool)
    .await
    .unwrap();
}

async fn add_circle(pool: &PgPool, id: &str, name: &str, emoji: &str, members: &[&str]) {
    sqlx::query("INSERT INTO ferry.circles (id, name, emoji) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(name)
        .bind(emoji)
        .execute(pool)
        .await
        .unwrap();
    for (n, member) in members.iter().enumerate() {
        sqlx::query(
            "INSERT INTO ferry.circle_members (circle_id, user_id, joined_at) \
             VALUES ($1, $2, '2026-09-01T00:00:00Z'::timestamptz + make_interval(mins => $3))",
        )
        .bind(id)
        .bind(member)
        .bind(n as i32)
        .execute(pool)
        .await
        .unwrap();
    }
}

/// The demo's four people in cir_hinton4 (in join order judge, maya, dana, leo), Maya and Leo in
/// cir_climb, usr_out in no circle with them and usr_solo alone in cir_solo. The app's
/// PUBLIC_WEB_URL is [`WEB_URL`] and its TELEGRAM_BOT_USERNAME is [`BOT`].
async fn server(pool: PgPool) -> Server {
    server_with(
        pool,
        &[("PUBLIC_WEB_URL", WEB_URL), ("TELEGRAM_BOT_USERNAME", BOT)],
    )
    .await
}

/// The same people and circles, with the app configured by `pairs` alone.
async fn server_with(pool: PgPool, pairs: &[(&str, &str)]) -> Server {
    for (id, name, phone) in [
        ("usr_judge", "Jordan", "+19195550101"),
        ("usr_maya", "Maya", "+19195550102"),
        ("usr_dana", "Dana", "+19195550103"),
        ("usr_leo", "Leo", "+19195550104"),
        ("usr_out", "Outsider", "+19195550105"),
        ("usr_solo", "Solo", "+19195550106"),
    ] {
        add_user(&pool, id, name, phone).await;
    }
    add_circle(
        &pool,
        "cir_hinton4",
        "Hinton James 4th floor",
        "🌊",
        &["usr_judge", "usr_maya", "usr_dana", "usr_leo"],
    )
    .await;
    add_circle(
        &pool,
        "cir_climb",
        "Climbing crew",
        "🧗",
        &["usr_maya", "usr_leo"],
    )
    .await;
    add_circle(&pool, "cir_solo", "Just me", "🫧", &["usr_solo"]).await;
    serve(AppState::new(pool, common::config(pairs)).unwrap()).await
}

/// `POST uri` as `user_id` with a JSON body (`None`: no body at all).
async fn post(
    server: &Server,
    uri: &str,
    user_id: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::post(uri).header(
        header::AUTHORIZATION,
        format!("Bearer {}", server.token(user_id)),
    );
    let request = match body {
        Some(body) => request
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string())),
        None => request.body(Body::empty()),
    }
    .unwrap();
    let (status, _, body) = server.send(request).await;
    (status, body)
}

/// Asserts 200 and a contract `Invite`, and returns it.
fn invite_ok(what: &str, (status, body): (StatusCode, Value)) -> Value {
    assert_eq!(status, StatusCode::OK, "{what}: {body}");
    assert_contract("Invite", &body);
    assert_no_phone(&body);
    body
}

/// Asserts the error status and code, in the contract's envelope.
fn error(what: &str, (status, body): (StatusCode, Value), expected: StatusCode, code: &str) {
    assert_eq!(status, expected, "{what}: {body}");
    assert_envelope(&body, code);
    assert_contract("Error", &body);
}

async fn meet_again(server: &Server, circle: &str, user_id: &str, body: Option<Value>) -> Value {
    invite_ok(
        "meet-again",
        post(
            server,
            &format!("/v1/circles/{circle}/meet-again"),
            user_id,
            body,
        )
        .await,
    )
}

async fn respond(server: &Server, invite: &str, user_id: &str, accept: bool) -> Value {
    invite_ok(
        &format!("{user_id} responds {accept}"),
        post(
            server,
            &format!("/v1/invites/{invite}/respond"),
            user_id,
            Some(json!({ "accept": accept })),
        )
        .await,
    )
}

/// `GET /v1/invites` as `user_id`, each one a contract `Invite`.
async fn invites(server: &Server, user_id: &str) -> Vec<Value> {
    let (status, body) = server.get_as("/v1/invites", user_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let items = body.as_array().expect("an array").clone();
    for item in &items {
        assert_contract("Invite", item);
        assert_no_phone(item);
    }
    items
}

/// The one invite `user_id` can list with this id.
async fn invite_as(server: &Server, id: &str, user_id: &str) -> Value {
    invites(server, user_id)
        .await
        .into_iter()
        .find(|i| i["id"] == id)
        .unwrap_or_else(|| panic!("{user_id} does not see {id}"))
}

/// The next event on `ws`: asserts its type and that it validates as a `ServerEvent` and as its
/// own `*Event` schema; returns its data.
async fn expect_event(ws: &mut Ws, kind: &str) -> Value {
    let event = next_json(ws).await;
    assert_eq!(event["type"], kind, "{event}");
    assert_contract("ServerEvent", &event);
    let schema = match kind {
        "invite.updated" => "InviteUpdatedEvent",
        "notification.created" => "NotificationCreatedEvent",
        other => panic!("unexpected kind {other}"),
    };
    assert_contract(schema, &event);
    assert_no_phone(&event);
    event["data"].clone()
}

/// The inviter's relay of one answer (contract v1.5): the next event on `ws` is an `invite`
/// notification titled `title` about invite `id`.
async fn expect_relay(ws: &mut Ws, id: &str, title: &str) {
    let data = expect_event(ws, "notification.created").await;
    let notification = &data["notification"];
    assert_eq!(notification["type"], "invite");
    assert_eq!(notification["title"], title);
    assert_eq!(notification["data"]["invite_id"], id);
}

fn statuses(invite: &Value) -> Vec<(String, String)> {
    invite["responses"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["user"]["id"].as_str().unwrap().to_owned(),
                r["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

/// An ISO 8601 UTC time `interval` from now, by the database's clock.
async fn from_now(pool: &PgPool, interval: &str) -> String {
    sqlx::query_scalar(
        "SELECT to_char((now() + $1::interval) AT TIME ZONE 'UTC', \
                'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')",
    )
    .bind(interval)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// `proposed_time` as Chapel Hill reads a clock, as the notification and the DM word it for a
/// day after tomorrow or later (contract v1.5): `7:00 PM on Tue, Oct 13`.
async fn local(pool: &PgPool, time: &str) -> String {
    sqlx::query_scalar(
        "SELECT to_char($1::timestamptz AT TIME ZONE 'America/New_York', \
                'FMHH12:MI AM \"on\" Dy, Mon FMDD')",
    )
    .bind(time)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrations = "../migrations")]
async fn meet_again_notifies_the_invitees_and_publishes_to_participants_only(pool: PgPool) {
    let server = server(pool.clone()).await;
    let mut dana = server.connect_with_token("usr_dana").await;
    let mut judge = server.connect_with_token("usr_judge").await;
    let mut maya = server.connect_with_token("usr_maya").await;
    let mut leo = server.connect_with_token("usr_leo").await;
    let mut outsider = server.connect_with_token("usr_out").await;

    let when = from_now(&pool, "2 days").await;
    let invite = meet_again(
        &server,
        "cir_hinton4",
        "usr_dana",
        Some(json!({"message": "  Dinner at Lenoir again?  ", "proposed_time": when})),
    )
    .await;
    let id = invite["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("inv_"), "{id}");
    assert_eq!(invite["kind"], "meetup");
    assert_eq!(invite["circle_id"], "cir_hinton4");
    assert_eq!(
        invite["from"],
        json!({"id": "usr_dana", "display_name": "Dana", "avatar_seed": "usr_dana", "bubble_color": "#336699"})
    );
    assert_eq!(invite["message"], "Dinner at Lenoir again?");
    assert_eq!(
        invite["proposed_time"],
        when.replace('Z', ".000Z"),
        "normalized to milliseconds"
    );
    assert_eq!(invite["status"], "pending");
    assert_eq!(
        statuses(&invite),
        pairs(&[
            ("usr_judge", "pending"),
            ("usr_maya", "pending"),
            ("usr_leo", "pending")
        ])
    );
    assert_eq!(
        invite["thread"],
        json!({"state": "none", "compose": null, "open_url": null})
    );

    // Each invitee: the stored notification, then the invite.
    let local_when = local(&pool, &when).await;
    for ws in [&mut judge, &mut maya, &mut leo] {
        let data = expect_event(ws, "notification.created").await;
        let notification = &data["notification"];
        assert_eq!(notification["type"], "invite");
        assert_eq!(
            notification["title"],
            format!("Dana wants to meet up at {local_when}")
        );
        assert_eq!(notification["body"], "“Dinner at Lenoir again?” You in?");
        assert_eq!(
            notification["data"],
            json!({"invite_id": id, "circle_id": "cir_hinton4", "url": format!("{WEB_URL}/i/{id}")})
        );
        assert_eq!(notification["read"], false);
        let data = expect_event(ws, "invite.updated").await;
        assert_eq!(data["invite"], invite);
    }
    // The inviter gets the invite, not a notification.
    let data = expect_event(&mut dana, "invite.updated").await;
    assert_eq!(data["invite"], invite);
    // Nobody else hears about it.
    for ws in [&mut dana, &mut judge, &mut maya, &mut leo, &mut outsider] {
        assert_silent(ws, QUIET).await;
    }

    // Stored notifications and the invite list.
    for user in ["usr_judge", "usr_maya", "usr_leo"] {
        let (status, body) = server.get_as("/v1/notifications", user).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.as_array().unwrap().len(), 1, "{user}: {body}");
        assert_contract("Notification", &body[0]);
        assert_eq!(body[0]["data"]["invite_id"], id);
        assert_eq!(
            invites(&server, user).await,
            std::slice::from_ref(&invite),
            "{user}"
        );
    }
    let (_, body) = server.get_as("/v1/notifications", "usr_dana").await;
    assert_eq!(body, json!([]));
    assert_eq!(
        invites(&server, "usr_dana").await,
        std::slice::from_ref(&invite)
    );
    assert!(invites(&server, "usr_out").await.is_empty());
    let (_, body) = server.get_as("/v1/notifications", "usr_out").await;
    assert_eq!(body, json!([]));
}

#[sqlx::test(migrations = "../migrations")]
async fn accepting_gives_the_telegram_link_to_accepted_people_only(pool: PgPool) {
    let server = server(pool.clone()).await;
    let when = from_now(&pool, "1 day").await;
    let invite = meet_again(
        &server,
        "cir_hinton4",
        "usr_dana",
        Some(json!({"proposed_time": when})),
    )
    .await;
    let id = invite["id"].as_str().unwrap().to_owned();
    assert_eq!(invite["message"], "Want to meet up again?");
    let link = startgroup(&id);
    let ready = json!({"state": "ready", "compose": null, "open_url": link});
    let ready_no_link = json!({"state": "ready", "compose": null, "open_url": null});

    let mut dana = server.connect_with_token("usr_dana").await;
    let mut judge = server.connect_with_token("usr_judge").await;
    let mut maya = server.connect_with_token("usr_maya").await;
    let mut leo = server.connect_with_token("usr_leo").await;
    let mut outsider = server.connect_with_token("usr_out").await;

    // The judge accepts: the invite is accepted and the bot's startgroup link is ready.
    let accepted = respond(&server, &id, "usr_judge", true).await;
    assert_eq!(accepted["status"], "accepted");
    assert_eq!(
        statuses(&accepted),
        pairs(&[
            ("usr_judge", "accepted"),
            ("usr_maya", "pending"),
            ("usr_leo", "pending")
        ])
    );
    assert_eq!(accepted["thread"], ready);

    // The inviter hears the answer first (contract v1.5), then everyone in the invite gets
    // invite.updated; only the inviter and the judge get the link.
    expect_relay(&mut dana, &id, "Jordan's in.").await;
    for (ws, user) in [(&mut dana, "usr_dana"), (&mut judge, "usr_judge")] {
        let data = expect_event(ws, "invite.updated").await;
        assert_eq!(data["invite"], accepted, "{user}");
    }
    for (ws, user) in [(&mut maya, "usr_maya"), (&mut leo, "usr_leo")] {
        let data = expect_event(ws, "invite.updated").await;
        let copy = &data["invite"];
        assert_eq!(copy["status"], "accepted", "{user}");
        assert_eq!(copy["thread"], ready_no_link, "{user}");
        assert!(!copy.to_string().contains("t.me"), "{user}: {copy}");
    }
    assert_silent(&mut outsider, QUIET).await;

    // REST copies follow the same rule.
    assert_eq!(invite_as(&server, &id, "usr_dana").await, accepted);
    assert_eq!(invite_as(&server, &id, "usr_judge").await, accepted);
    for user in ["usr_maya", "usr_leo"] {
        let copy = invite_as(&server, &id, user).await;
        assert_eq!(copy["thread"], ready_no_link, "{user}");
    }

    // Maya accepts too and gets the link; Leo, still pending, does not.
    let both = respond(&server, &id, "usr_maya", true).await;
    assert_eq!(both["thread"], ready);
    expect_relay(&mut dana, &id, "Maya's in.").await;
    for ws in [&mut dana, &mut judge, &mut maya] {
        let data = expect_event(ws, "invite.updated").await;
        assert_eq!(data["invite"]["thread"], ready);
    }
    let data = expect_event(&mut leo, "invite.updated").await;
    assert_eq!(data["invite"]["thread"], ready_no_link);
    assert_eq!(
        invite_as(&server, &id, "usr_leo").await["thread"],
        ready_no_link
    );

    // The judge changes their mind: the latest answer counts, and the link leaves their copy.
    let changed = respond(&server, &id, "usr_judge", false).await;
    assert_eq!(changed["status"], "accepted");
    assert_eq!(changed["thread"], ready_no_link);
    assert_eq!(invite_as(&server, &id, "usr_maya").await["thread"], ready);
    assert_eq!(invite_as(&server, &id, "usr_dana").await["thread"], ready);
    expect_relay(&mut dana, &id, "Jordan can't make it.").await;
    expect_event(&mut dana, "invite.updated").await;

    // The same answer again changes nothing, so the inviter hears nothing new: only the invite.
    respond(&server, &id, "usr_judge", false).await;
    expect_event(&mut dana, "invite.updated").await;
    assert_silent(&mut dana, QUIET).await;
    let relays: Vec<String> = sqlx::query_scalar(
        "SELECT title FROM ferry.notifications WHERE user_id = 'usr_dana' ORDER BY created_at, id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        relays,
        ["Jordan's in.", "Maya's in.", "Jordan can't make it."]
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn the_circles_telegram_group_link_wins(pool: PgPool) {
    let server = server(pool.clone()).await;
    // The bot is an admin in the circle's group and stored its invite link; a DM space and another
    // circle's group do not count.
    sqlx::query(
        "INSERT INTO ferry.message_spaces (space_id, is_group, circle_id, dm_user_id, invite_url, created_at) \
         VALUES ('tg:-1001', true, 'cir_hinton4', NULL, 'https://t.me/+OldHinton', now() - interval '1 day'), \
                ('tg:-1002', true, 'cir_hinton4', NULL, 'https://t.me/+Hinton4th', now()), \
                ('tg:-1003', true, 'cir_hinton4', NULL, NULL, now() + interval '1 minute'), \
                ('tg:-1004', true, 'cir_climb', NULL, 'https://t.me/+Climbers', now()), \
                ('tg:42', false, NULL, 'usr_judge', NULL, now())",
    )
    .execute(&pool)
    .await
    .unwrap();
    let invite = meet_again(&server, "cir_hinton4", "usr_dana", None).await;
    let id = invite["id"].as_str().unwrap().to_owned();
    let accepted = respond(&server, &id, "usr_leo", true).await;
    assert_eq!(
        accepted["thread"],
        json!({"state": "ready", "compose": null, "open_url": "https://t.me/+Hinton4th"})
    );
    assert_eq!(
        invite_as(&server, &id, "usr_dana").await["thread"]["open_url"],
        "https://t.me/+Hinton4th"
    );
    assert_eq!(
        invite_as(&server, &id, "usr_judge").await["thread"]["open_url"],
        Value::Null
    );

    // The schema keeps links to Telegram and to groups.
    for (what, sql) in [
        (
            "not a t.me link",
            "UPDATE ferry.message_spaces SET invite_url = 'https://evil.example/+x' WHERE space_id = 'tg:-1002'",
        ),
        (
            "a DM",
            "UPDATE ferry.message_spaces SET invite_url = 'https://t.me/+x' WHERE space_id = 'tg:42'",
        ),
    ] {
        let err = sqlx::query(sql).execute(&pool).await.unwrap_err();
        assert!(
            matches!(&err, sqlx::Error::Database(db) if db.code().as_deref() == Some("23514")),
            "{what}: {err}"
        );
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn without_a_link_the_thread_stays_none(pool: PgPool) {
    // No TELEGRAM_BOT_USERNAME and no stored group link: nothing to open.
    let server = server_with(pool.clone(), &[("PUBLIC_WEB_URL", WEB_URL)]).await;
    let invite = meet_again(&server, "cir_hinton4", "usr_dana", None).await;
    let id = invite["id"].as_str().unwrap().to_owned();
    let accepted = respond(&server, &id, "usr_judge", true).await;
    assert_eq!(accepted["status"], "accepted");
    let none = json!({"state": "none", "compose": null, "open_url": null});
    assert_eq!(accepted["thread"], none);
    assert_eq!(invite_as(&server, &id, "usr_dana").await["thread"], none);
    let stored: String = sqlx::query_scalar("SELECT thread_state FROM ferry.invites WHERE id = $1")
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, "none");
}

#[sqlx::test(migrations = "../migrations")]
async fn dms_and_their_buttons(pool: PgPool) {
    use ferry_core::error::ErrorCode;
    use ferry_core::invites::{MeetAgainRequest, create_meetup_invite, respond_from_button};

    let server = server(pool.clone()).await;
    let when = from_now(&pool, "2 days").await;
    let record = create_meetup_invite(
        &server.state,
        "cir_hinton4",
        "usr_dana",
        MeetAgainRequest {
            message: Some("Dinner at Lenoir again?".into()),
            proposed_time: Some(when.clone()),
            place: None,
        },
    )
    .await
    .unwrap();
    let id = record.id.clone();

    // One DM per invitee, in join order, with the deep link and the two buttons.
    let dms = serde_json::to_value(record.dms()).unwrap();
    assert_no_phone(&dms);
    let text = format!(
        "Dana wants to meet up at {}. You in?\n“Dinner at Lenoir again?”",
        local(&pool, &when).await
    );
    let dm = |user: &str| {
        json!({
            "user_id": user,
            "text": text,
            "url": format!("{WEB_URL}/i/{id}"),
            "buttons": [
                {"label": "I'm in", "callback_data": format!("invite:{id}:accept")},
                {"label": "Can't make it", "callback_data": format!("invite:{id}:decline")}
            ]
        })
    };
    assert_eq!(dms, json!([dm("usr_judge"), dm("usr_maya"), dm("usr_leo")]));

    // A button press answers under the same rules, and publishes the same events.
    let mut dana = server.connect_with_token("usr_dana").await;
    let accepted = respond_from_button(&server.state, "usr_judge", &format!("invite:{id}:accept"))
        .await
        .unwrap();
    assert_eq!(accepted.status.as_str(), "accepted");
    expect_relay(&mut dana, &id, "Jordan's in.").await;
    let data = expect_event(&mut dana, "invite.updated").await;
    assert_eq!(data["invite"]["status"], "accepted");
    assert_eq!(data["invite"]["thread"]["open_url"], startgroup(&id));
    let declined = respond_from_button(&server.state, "usr_judge", &format!("invite:{id}:decline"))
        .await
        .unwrap();
    assert_eq!(declined.status.as_str(), "pending");

    for (user, data, code) in [
        (
            "usr_dana",
            format!("invite:{id}:accept"),
            ErrorCode::Forbidden,
        ),
        (
            "usr_out",
            format!("invite:{id}:accept"),
            ErrorCode::NotFound,
        ),
        (
            "usr_judge",
            "invite:inv_nope:accept".to_owned(),
            ErrorCode::NotFound,
        ),
        (
            "usr_judge",
            format!("invite:{id}:maybe"),
            ErrorCode::ValidationFailed,
        ),
        (
            "usr_judge",
            "order:ord_x:accept".to_owned(),
            ErrorCode::ValidationFailed,
        ),
    ] {
        let err = respond_from_button(&server.state, user, &data)
            .await
            .unwrap_err();
        assert_eq!(err.code, code, "{user} {data}");
    }
    sqlx::query("UPDATE ferry.invites SET created_at = now() - interval '3 days', proposed_time = now() - interval '1 hour' WHERE id = $1")
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
    let err = respond_from_button(&server.state, "usr_maya", &format!("invite:{id}:accept"))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
}

#[sqlx::test(migrations = "../migrations")]
async fn declines_overwrites_and_the_status_rules(pool: PgPool) {
    let server = server(pool).await;
    let invite = meet_again(&server, "cir_hinton4", "usr_dana", Some(json!({}))).await;
    let id = invite["id"].as_str().unwrap().to_owned();

    // Some declines keep it pending; all of them make it declined.
    let one = respond(&server, &id, "usr_judge", false).await;
    assert_eq!(one["status"], "pending");
    let none = json!({"state": "none", "compose": null, "open_url": null});
    assert_eq!(one["thread"], none);
    respond(&server, &id, "usr_maya", false).await;
    let all = respond(&server, &id, "usr_leo", false).await;
    assert_eq!(all["status"], "declined");
    assert_eq!(
        statuses(&all),
        pairs(&[
            ("usr_judge", "declined"),
            ("usr_maya", "declined"),
            ("usr_leo", "declined")
        ])
    );
    assert_eq!(all["thread"], none);
    let danas = invite_as(&server, &id, "usr_dana").await;
    assert_eq!(danas["status"], "declined");
    assert_eq!(danas["thread"], none);

    // A second answer overwrites the first: Leo accepts after all.
    let back = respond(&server, &id, "usr_leo", true).await;
    assert_eq!(back["status"], "accepted");
    assert_eq!(back["thread"]["open_url"], startgroup(&id));
    // And declines again.
    let again = respond(&server, &id, "usr_leo", false).await;
    assert_eq!(again["status"], "declined");
    // The same answer twice is fine.
    let same = respond(&server, &id, "usr_leo", false).await;
    assert_eq!(same["status"], "declined");
}

#[sqlx::test(migrations = "../migrations")]
async fn invites_expire_after_their_time_or_24_hours(pool: PgPool) {
    let server = server(pool.clone()).await;
    let backdate = |id: &str, sql: &'static str| {
        let pool = pool.clone();
        let id = id.to_owned();
        async move {
            sqlx::query(sql).bind(id).execute(&pool).await.unwrap();
        }
    };

    // No proposed time: open for 24 hours.
    let open = meet_again(&server, "cir_hinton4", "usr_dana", None).await;
    let open_id = open["id"].as_str().unwrap().to_owned();
    backdate(
        &open_id,
        "UPDATE ferry.invites SET created_at = now() - interval '23 hours' WHERE id = $1",
    )
    .await;
    assert_eq!(
        invite_as(&server, &open_id, "usr_judge").await["status"],
        "pending"
    );
    backdate(
        &open_id,
        "UPDATE ferry.invites SET created_at = now() - interval '25 hours' WHERE id = $1",
    )
    .await;
    for user in ["usr_dana", "usr_judge"] {
        assert_eq!(
            invite_as(&server, &open_id, user).await["status"],
            "expired"
        );
    }
    error(
        "respond after 24 hours",
        post(
            &server,
            &format!("/v1/invites/{open_id}/respond"),
            "usr_judge",
            Some(json!({"accept": true})),
        )
        .await,
        StatusCode::CONFLICT,
        "conflict",
    );
    assert_eq!(
        invite_as(&server, &open_id, "usr_judge").await["status"],
        "expired"
    );

    // A proposed time: open until then, even past 24 hours.
    let when = from_now(&pool, "3 days").await;
    let timed = meet_again(
        &server,
        "cir_hinton4",
        "usr_dana",
        Some(json!({"proposed_time": when})),
    )
    .await;
    let timed_id = timed["id"].as_str().unwrap().to_owned();
    backdate(
        &timed_id,
        "UPDATE ferry.invites SET created_at = now() - interval '2 days' WHERE id = $1",
    )
    .await;
    assert_eq!(
        invite_as(&server, &timed_id, "usr_maya").await["status"],
        "pending"
    );
    respond(&server, &timed_id, "usr_maya", false).await;
    backdate(
        &timed_id,
        "UPDATE ferry.invites SET proposed_time = now() - interval '1 minute' WHERE id = $1",
    )
    .await;
    assert_eq!(
        invite_as(&server, &timed_id, "usr_maya").await["status"],
        "expired"
    );
    error(
        "respond after the proposed time",
        post(
            &server,
            &format!("/v1/invites/{timed_id}/respond"),
            "usr_leo",
            Some(json!({"accept": true})),
        )
        .await,
        StatusCode::CONFLICT,
        "conflict",
    );

    // An accepted invite stays accepted, with its link, after its time.
    let kept = meet_again(&server, "cir_climb", "usr_maya", None).await;
    let kept_id = kept["id"].as_str().unwrap().to_owned();
    respond(&server, &kept_id, "usr_leo", true).await;
    backdate(
        &kept_id,
        "UPDATE ferry.invites SET created_at = now() - interval '2 days' WHERE id = $1",
    )
    .await;
    let after = invite_as(&server, &kept_id, "usr_leo").await;
    assert_eq!(after["status"], "accepted");
    assert_eq!(after["thread"]["state"], "ready");
    assert_eq!(after["thread"]["open_url"], startgroup(&kept_id));

    // Maya's list, newest first: the one she sent and the two she was invited to.
    sqlx::query("UPDATE ferry.invites SET created_at = now() - interval '30 hours' WHERE id = $1")
        .bind(&kept_id)
        .execute(&pool)
        .await
        .unwrap();
    let listed = invites(&server, "usr_maya").await;
    let ids: Vec<&str> = listed.iter().map(|i| i["id"].as_str().unwrap()).collect();
    // Created 25 hours, 2 days and 30 hours ago.
    assert_eq!(ids, [open_id.as_str(), kept_id.as_str(), timed_id.as_str()]);
}

#[sqlx::test(migrations = "../migrations")]
async fn only_invitees_respond(pool: PgPool) {
    let server = server(pool).await;
    let invite = meet_again(&server, "cir_hinton4", "usr_dana", None).await;
    let id = invite["id"].as_str().unwrap().to_owned();
    let uri = format!("/v1/invites/{id}/respond");
    let yes = || Some(json!({"accept": true}));

    error(
        "the inviter",
        post(&server, &uri, "usr_dana", yes()).await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    for user in ["usr_out", "usr_solo"] {
        error(
            user,
            post(&server, &uri, user, yes()).await,
            StatusCode::NOT_FOUND,
            "not_found",
        );
    }
    error(
        "unknown invite",
        post(&server, "/v1/invites/inv_nope/respond", "usr_judge", yes()).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    for (what, body) in [
        ("no body", None),
        ("no accept", Some(json!({}))),
        ("accept not a boolean", Some(json!({"accept": "yes"}))),
    ] {
        error(
            what,
            post(&server, &uri, "usr_judge", body).await,
            StatusCode::BAD_REQUEST,
            "validation_failed",
        );
    }
    let (status, _, body) = server
        .send(
            Request::post(&uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"accept":true}"#))
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_envelope(&body, "unauthorized");
    let (status, _, _) = server
        .send(Request::get("/v1/invites").body(Body::empty()).unwrap())
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Nothing changed.
    let still = invite_as(&server, &id, "usr_judge").await;
    assert_eq!(still["status"], "pending");
    assert_eq!(still, invite_as(&server, &id, "usr_judge").await);
}

#[sqlx::test(migrations = "../migrations")]
async fn meet_again_checks_the_circle_and_the_body(pool: PgPool) {
    let server = server(pool.clone()).await;
    let uri = "/v1/circles/cir_hinton4/meet-again";

    for (circle, user) in [
        ("cir_hinton4", "usr_out"),
        ("cir_climb", "usr_judge"),
        ("cir_nope", "usr_dana"),
    ] {
        error(
            &format!("{user} in {circle}"),
            post(
                &server,
                &format!("/v1/circles/{circle}/meet-again"),
                user,
                None,
            )
            .await,
            StatusCode::NOT_FOUND,
            "not_found",
        );
    }
    error(
        "a circle of one",
        post(&server, "/v1/circles/cir_solo/meet-again", "usr_solo", None).await,
        StatusCode::CONFLICT,
        "conflict",
    );

    let past = from_now(&pool, "-1 minute").await;
    let too_far = from_now(&pool, "30 days 1 minute").await;
    let in_two_days = from_now(&pool, "2 days").await;
    // Without its `Z`, for other offsets.
    let soon = &in_two_days[..19];
    let long = "x".repeat(281);
    for (what, body) in [
        ("not a date-time", json!({"proposed_time": "tomorrow at 7"})),
        (
            "no time zone",
            json!({"proposed_time": "2030-10-11T19:30:00"}),
        ),
        ("no seconds", json!({"proposed_time": "2030-10-11T19:30Z"})),
        (
            "an impossible date",
            json!({"proposed_time": "2030-02-30T19:30:00Z"}),
        ),
        (
            "an impossible hour",
            json!({"proposed_time": "2030-10-11T25:30:00Z"}),
        ),
        ("in the past", json!({"proposed_time": past})),
        ("more than 30 days ahead", json!({"proposed_time": too_far})),
        (
            "an offset out of range",
            json!({"proposed_time": format!("{soon}+16:00")}),
        ),
        (
            "an absurd offset",
            json!({"proposed_time": format!("{soon}+99:00")}),
        ),
        (
            "a five-digit year",
            json!({"proposed_time": "99999-10-11T19:30:00Z"}),
        ),
        ("year 0", json!({"proposed_time": "0000-10-11T19:30:00Z"})),
        (
            "a NUL byte",
            json!({"proposed_time": format!("{soon}\u{0}Z")}),
        ),
        ("not a string", json!({"proposed_time": 1791669852})),
        ("a long message", json!({"message": long})),
        ("message not a string", json!({"message": 5})),
    ] {
        error(
            what,
            post(&server, uri, "usr_dana", Some(body)).await,
            StatusCode::BAD_REQUEST,
            "validation_failed",
        );
    }
    // Nothing was created by the refusals.
    assert!(invites(&server, "usr_dana").await.is_empty());
    assert!(invites(&server, "usr_out").await.is_empty());

    // An offset is accepted and normalized to UTC; a blank message becomes the default. Up to
    // 30 days ahead is fine.
    let utc = from_now(&pool, "29 days 23 hours").await;
    let with_offset: String = sqlx::query_scalar(
        "SELECT to_char(($1::timestamptz AT TIME ZONE 'UTC') - interval '4 hours', \
                'YYYY-MM-DD\"T\"HH24:MI:SS') || '-04:00'",
    )
    .bind(&utc)
    .fetch_one(&pool)
    .await
    .unwrap();
    let invite = meet_again(
        &server,
        "cir_hinton4",
        "usr_dana",
        Some(json!({"message": "   ", "proposed_time": with_offset})),
    )
    .await;
    assert_eq!(invite["proposed_time"], utc.replace('Z', ".000Z"));
    assert_eq!(invite["message"], "Want to meet up again?");
    let (_, notifications) = server.get_as("/v1/notifications", "usr_judge").await;
    assert_eq!(
        notifications[0]["title"],
        format!("Dana wants to meet up at {}", local(&pool, &utc).await)
    );
    assert_eq!(notifications[0]["body"], "You in?");
    // Any member may start one, and a circle may have several open at once.
    let second = meet_again(&server, "cir_hinton4", "usr_judge", None).await;
    assert_eq!(
        statuses(&second),
        pairs(&[
            ("usr_maya", "pending"),
            ("usr_dana", "pending"),
            ("usr_leo", "pending")
        ])
    );
    assert_eq!(invites(&server, "usr_leo").await.len(), 2);
}

#[sqlx::test(migrations = "../migrations")]
async fn concurrent_answers_leave_a_consistent_stored_status(pool: PgPool) {
    let server = server(pool.clone()).await;
    for round in 0..5 {
        let invite = meet_again(&server, "cir_hinton4", "usr_dana", None).await;
        let id = invite["id"].as_str().unwrap().to_owned();
        // All three decline at once, except the judge accepts on even rounds.
        let accept = round % 2 == 0;
        let (a, b, c) = tokio::join!(
            ferry_core::invites::respond_to_invite(&server.state, &id, "usr_judge", accept),
            ferry_core::invites::respond_to_invite(&server.state, &id, "usr_maya", false),
            ferry_core::invites::respond_to_invite(&server.state, &id, "usr_leo", false),
        );
        for result in [a, b, c] {
            result.unwrap();
        }
        let expected = if accept { "accepted" } else { "declined" };
        let (status, thread): (String, String) =
            sqlx::query_as("SELECT status, thread_state FROM ferry.invites WHERE id = $1")
                .bind(&id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, expected, "round {round}");
        assert_eq!(thread, if accept { "ready" } else { "none" });
        assert_eq!(
            invite_as(&server, &id, "usr_dana").await["status"],
            expected
        );
    }
}
