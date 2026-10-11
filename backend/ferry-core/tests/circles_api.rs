//! Circles: GET /v1/circles and /v1/circles/{id} for members and outsiders, the derived
//! last_met_at, last_active_at and stats, and the agent's meet_again_last_circle.
mod common;
mod realtime_support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::assert_envelope;
use ferry_core::circles::{meet_again_last_circle, most_recent_circle};
use ferry_core::error::ErrorCode;
use ferry_core::invites::MeetAgainRequest;
use ferry_core::notifications::HubSettings;
use realtime_support::{Server, assert_contract, serve, state};
use serde_json::{Value, json};
use sqlx::PgPool;

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

/// A circle with `members` in that join order, last active at `active`.
async fn add_circle(pool: &PgPool, id: &str, name: &str, active: &str, members: &[&str]) {
    sqlx::query(
        "INSERT INTO ferry.circles (id, name, emoji, last_active_at) \
         VALUES ($1, $2, '🌊', $3::timestamptz)",
    )
    .bind(id)
    .bind(name)
    .bind(active)
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

/// The demo's people and circles, plus usr_out, who shares no circle with them, and usr_solo,
/// alone in cir_solo.
async fn seed(pool: &PgPool) {
    for (id, name, phone) in [
        ("usr_judge", "Jordan", "+19195550101"),
        ("usr_maya", "Maya", "+19195550102"),
        ("usr_dana", "Dana", "+19195550103"),
        ("usr_leo", "Leo", "+19195550104"),
        ("usr_out", "Outsider", "+19195550105"),
        ("usr_solo", "Solo", "+19195550106"),
    ] {
        add_user(pool, id, name, phone).await;
    }
    add_circle(
        pool,
        "cir_hinton4",
        "Hinton James 4th floor",
        "2026-10-01T12:00:00Z",
        &["usr_judge", "usr_maya", "usr_dana", "usr_leo"],
    )
    .await;
    add_circle(
        pool,
        "cir_climb",
        "Climbing crew",
        "2026-09-20T12:00:00Z",
        &["usr_maya", "usr_leo"],
    )
    .await;
    add_circle(
        pool,
        "cir_solo",
        "Just me",
        "2026-10-05T12:00:00Z",
        &["usr_solo"],
    )
    .await;
    sqlx::query(
        "INSERT INTO ferry.areas (id, name, center_lat, center_lng) \
         VALUES ('area_franklin', 'Franklin St', 35.9132, -79.0558)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ferry.restaurants (id, name, area_id, lat, lng, address, hours_today, price_level) \
         VALUES ('rst_cosmic', 'Cosmic Cantina', 'area_franklin', 35.913211, -79.055094, \
                 '118 E Franklin St', '11:00-04:00', 1)",
    )
    .execute(pool)
    .await
    .unwrap();
}

async fn add_run(pool: &PgPool, id: &str, circle: &str, status: &str, leaving: &str, demo: bool) {
    sqlx::query(
        "INSERT INTO ferry.runs (id, restaurant_id, runner_id, circle_id, status, created_via, \
                                 leaving_at, window_closes_at, is_demo_history, created_at) \
         VALUES ($1, 'rst_cosmic', 'usr_maya', $2, $3, 'app', $4::timestamptz, \
                 $4::timestamptz + interval '10 minutes', $5, $4::timestamptz - interval '5 minutes')",
    )
    .bind(id)
    .bind(circle)
    .bind(status)
    .bind(leaving)
    .bind(demo)
    .execute(pool)
    .await
    .unwrap();
}

/// An order on `run`; `delivered` is its delivery time for status delivered or settled.
async fn add_order(
    pool: &PgPool,
    id: &str,
    run: &str,
    status: &str,
    co2: f64,
    delivered: Option<&str>,
    demo: bool,
) {
    sqlx::query(
        "INSERT INTO ferry.orders (id, run_id, customer_id, status, subtotal_cents, tax_cents, \
             ferry_fee_cents, tip_cents, total_cents, dropoff_label, dropoff_lat, dropoff_lng, \
             handoff_code, co2_saved_g, delivered_at, is_demo_history) \
         VALUES ($1, $2, 'usr_judge', $3, 1000, 75, 150, 200, 1425, 'Fetzer Gym', 35.909152, \
                 -79.046905, '1234', $4, $5::timestamptz, $6)",
    )
    .bind(id)
    .bind(run)
    .bind(status)
    .bind(co2)
    .bind(delivered)
    .bind(demo)
    .execute(pool)
    .await
    .unwrap();
}

async fn server(pool: PgPool) -> Server {
    seed(&pool).await;
    serve(state(pool, HubSettings::default())).await
}

/// `GET uri` as `user_id`: asserts 200 and returns the body.
async fn get_ok(server: &Server, uri: &str, user_id: &str) -> Value {
    let (status, body) = server.get_as(uri, user_id).await;
    assert_eq!(status, StatusCode::OK, "{uri} as {user_id}: {body}");
    body
}

/// `GET /v1/circles` as `user_id`, each item checked against the contract's `Circle`.
async fn list(server: &Server, user_id: &str) -> Vec<Value> {
    let body = get_ok(server, "/v1/circles", user_id).await;
    let items = body.as_array().expect("an array").clone();
    for item in &items {
        assert_contract("Circle", item);
    }
    items
}

async fn circle(server: &Server, id: &str, user_id: &str) -> Value {
    let body = get_ok(server, &format!("/v1/circles/{id}"), user_id).await;
    assert_contract("Circle", &body);
    body
}

fn ids(items: &[Value]) -> Vec<&str> {
    items.iter().map(|c| c["id"].as_str().unwrap()).collect()
}

#[sqlx::test(migrations = "../migrations")]
async fn members_see_their_circles_and_outsiders_see_nothing(pool: PgPool) {
    let server = server(pool).await;

    // The public community first (everyone is in it, and it lists no members), then most recently
    // active first.
    let mayas = list(&server, "usr_maya").await;
    assert_eq!(ids(&mayas), ["cir_campus", "cir_hinton4", "cir_climb"]);
    assert_eq!(mayas[0]["name"], "UNC Chapel Hill");
    assert_eq!(mayas[0]["is_public"], true);
    assert_eq!(mayas[0]["members"], json!([]));
    assert_contract("Circle", &mayas[0]);
    let hinton = &mayas[1];
    assert_eq!(hinton["is_public"], false);
    assert_eq!(hinton["name"], "Hinton James 4th floor");
    assert_eq!(hinton["emoji"], "🌊");
    assert_eq!(
        hinton["members"],
        json!([
            {"id": "usr_judge", "display_name": "Jordan", "avatar_seed": "usr_judge", "bubble_color": "#336699"},
            {"id": "usr_maya", "display_name": "Maya", "avatar_seed": "usr_maya", "bubble_color": "#336699"},
            {"id": "usr_dana", "display_name": "Dana", "avatar_seed": "usr_dana", "bubble_color": "#336699"},
            {"id": "usr_leo", "display_name": "Leo", "avatar_seed": "usr_leo", "bubble_color": "#336699"}
        ])
    );
    for member in hinton["members"].as_array().unwrap() {
        assert_contract("PublicUser", member);
        assert_eq!(
            member.as_object().unwrap().len(),
            4,
            "PublicUser only, no phone: {member}"
        );
    }
    assert_eq!(hinton["last_met_at"], Value::Null);
    assert_eq!(hinton["last_active_at"], "2026-10-01T12:00:00.000Z");
    assert_eq!(hinton["stats"], json!({"runs": 0, "co2_saved_g": 0.0}));

    assert_eq!(
        ids(&list(&server, "usr_judge").await),
        ["cir_campus", "cir_hinton4"]
    );
    assert_eq!(circle(&server, "cir_hinton4", "usr_judge").await, *hinton);
    assert_eq!(circle(&server, "cir_climb", "usr_leo").await, mayas[2]);
    assert_eq!(
        circle(&server, "cir_campus", "usr_judge").await["members"],
        json!([])
    );

    // Outsiders: only the public community, and 404 for a circle they are not in, as for one that does not exist.
    assert_eq!(ids(&list(&server, "usr_out").await), ["cir_campus"]);
    for (id, user) in [
        ("cir_hinton4", "usr_out"),
        ("cir_climb", "usr_judge"),
        ("cir_nope", "usr_maya"),
    ] {
        let (status, body) = server.get_as(&format!("/v1/circles/{id}"), user).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{id} as {user}: {body}");
        assert_envelope(&body, "not_found");
        assert_contract("Error", &body);
    }

    // No token.
    for uri in ["/v1/circles", "/v1/circles/cir_hinton4"] {
        let (status, _, body) = server
            .send(Request::get(uri).body(Body::empty()).unwrap())
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_envelope(&body, "unauthorized");
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn stats_and_last_met_at_follow_delivered_runs_and_accepted_meetups(pool: PgPool) {
    let server = server(pool.clone()).await;

    // Two delivered runs (one seeded demo history) with three delivered orders, and runs that do
    // not count: an open one, a cancelled one, and a delivered one in another circle.
    add_run(
        &pool,
        "run_a",
        "cir_hinton4",
        "delivered",
        "2026-10-02T22:00:00Z",
        false,
    )
    .await;
    add_order(
        &pool,
        "ord_a1",
        "run_a",
        "settled",
        480.0,
        Some("2026-10-02T22:30:00Z"),
        false,
    )
    .await;
    add_order(
        &pool,
        "ord_a2",
        "run_a",
        "delivered",
        455.5,
        Some("2026-10-02T22:41:00Z"),
        false,
    )
    .await;
    add_order(&pool, "ord_a3", "run_a", "refunded", 999.0, None, false).await;
    add_run(
        &pool,
        "run_hist",
        "cir_hinton4",
        "delivered",
        "2026-09-12T22:00:00Z",
        true,
    )
    .await;
    add_order(
        &pool,
        "ord_h1",
        "run_hist",
        "settled",
        480.0,
        Some("2026-09-12T22:25:00Z"),
        true,
    )
    .await;
    add_run(
        &pool,
        "run_open",
        "cir_hinton4",
        "open",
        "2026-10-04T22:00:00Z",
        false,
    )
    .await;
    add_run(
        &pool,
        "run_cxl",
        "cir_hinton4",
        "cancelled",
        "2026-10-03T22:00:00Z",
        false,
    )
    .await;
    add_run(
        &pool,
        "run_climb",
        "cir_climb",
        "delivered",
        "2026-09-25T22:00:00Z",
        false,
    )
    .await;
    add_order(
        &pool,
        "ord_c1",
        "run_climb",
        "settled",
        480.0,
        Some("2026-09-25T22:20:00Z"),
        false,
    )
    .await;

    let hinton = circle(&server, "cir_hinton4", "usr_judge").await;
    assert_eq!(hinton["stats"], json!({"runs": 2, "co2_saved_g": 1415.5}));
    // The latest delivery of a delivered run.
    assert_eq!(hinton["last_met_at"], "2026-10-02T22:41:00.000Z");
    // The open run started latest (created 5 minutes before it leaves).
    assert_eq!(hinton["last_active_at"], "2026-10-04T21:55:00.000Z");

    let climb = circle(&server, "cir_climb", "usr_maya").await;
    assert_eq!(climb["stats"], json!({"runs": 1, "co2_saved_g": 480.0}));
    assert_eq!(climb["last_met_at"], "2026-09-25T22:20:00.000Z");
    // Its last meeting is later than its stored activity and its run's start.
    assert_eq!(climb["last_active_at"], "2026-09-25T22:20:00.000Z");

    // A delivered run without delivery times counts at its leaving time.
    add_run(
        &pool,
        "run_b",
        "cir_climb",
        "delivered",
        "2026-09-28T22:00:00Z",
        false,
    )
    .await;
    let climb = circle(&server, "cir_climb", "usr_maya").await;
    assert_eq!(climb["last_met_at"], "2026-09-28T22:00:00.000Z");
    assert_eq!(climb["stats"]["runs"], 2);

    // The stored last_met_at (seeded history) wins when it is later.
    sqlx::query(
        "UPDATE ferry.circles SET last_met_at = '2026-10-03T23:30:00Z' WHERE id = 'cir_hinton4'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let hinton = circle(&server, "cir_hinton4", "usr_judge").await;
    assert_eq!(hinton["last_met_at"], "2026-10-03T23:30:00.000Z");

    // An accepted meetup counts at its proposed time once that has passed, and not before.
    sqlx::query(
        "INSERT INTO ferry.invites (id, circle_id, from_user_id, message, proposed_time, created_at) \
         VALUES ('inv_past', 'cir_hinton4', 'usr_dana', 'Dinner', '2026-10-05T23:00:00Z', '2026-10-05T12:00:00Z'), \
                ('inv_future', 'cir_hinton4', 'usr_dana', 'Brunch', now() + interval '3 days', '2026-10-05T13:00:00Z'), \
                ('inv_unanswered', 'cir_hinton4', 'usr_dana', 'Lunch', '2026-10-06T16:00:00Z', '2026-10-05T14:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ferry.invite_responses (invite_id, user_id, status, responded_at) \
         VALUES ('inv_past', 'usr_judge', 'accepted', '2026-10-05T12:30:00Z'), \
                ('inv_past', 'usr_maya', 'declined', '2026-10-05T12:40:00Z'), \
                ('inv_future', 'usr_judge', 'accepted', '2026-10-05T13:10:00Z'), \
                ('inv_unanswered', 'usr_judge', 'pending', NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let hinton = circle(&server, "cir_hinton4", "usr_judge").await;
    assert_eq!(hinton["last_met_at"], "2026-10-05T23:00:00.000Z");
    // Activity includes the meetup and the latest invite created or answered.
    assert_eq!(hinton["last_active_at"], "2026-10-05T23:00:00.000Z");

    // Without a proposed time, an accepted meetup counts from its first acceptance.
    sqlx::query(
        "INSERT INTO ferry.invites (id, circle_id, from_user_id, message, created_at) \
         VALUES ('inv_now', 'cir_hinton4', 'usr_dana', 'Now?', '2026-10-06T20:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ferry.invite_responses (invite_id, user_id, status, responded_at) \
         VALUES ('inv_now', 'usr_leo', 'accepted', '2026-10-06T20:20:00Z'), \
                ('inv_now', 'usr_judge', 'accepted', '2026-10-06T20:05:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let hinton = circle(&server, "cir_hinton4", "usr_judge").await;
    assert_eq!(hinton["last_met_at"], "2026-10-06T20:05:00.000Z");
    assert_eq!(hinton["last_active_at"], "2026-10-06T20:20:00.000Z");
    assert_eq!(hinton["stats"], json!({"runs": 2, "co2_saved_g": 1415.5}));
}

#[sqlx::test(migrations = "../migrations")]
async fn the_agent_meets_again_with_the_most_recent_circle(pool: PgPool) {
    let server = server(pool.clone()).await;
    let request = || MeetAgainRequest {
        message: Some("Same crew as last time?".into()),
        proposed_time: None,
        place: None,
    };

    // Neither circle has met: the most recently active one.
    assert_eq!(
        most_recent_circle(&pool, "usr_maya")
            .await
            .unwrap()
            .as_deref(),
        Some("cir_hinton4")
    );
    // A circle that has met beats one that never did, however active.
    sqlx::query(
        "UPDATE ferry.circles SET last_met_at = '2026-09-10T20:00:00Z' WHERE id = 'cir_climb'",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        most_recent_circle(&pool, "usr_maya")
            .await
            .unwrap()
            .as_deref(),
        Some("cir_climb")
    );
    // Then the latest meeting.
    sqlx::query(
        "UPDATE ferry.circles SET last_met_at = '2026-09-11T20:00:00Z' WHERE id = 'cir_hinton4'",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        most_recent_circle(&pool, "usr_maya")
            .await
            .unwrap()
            .as_deref(),
        Some("cir_hinton4")
    );

    // Dana's most recent circle is cir_hinton4: the invite goes to its other three members.
    let record = meet_again_last_circle(&server.state, "usr_dana", request())
        .await
        .unwrap();
    let invite = record.as_seen_by("usr_dana");
    let value = serde_json::to_value(&invite).unwrap();
    assert_contract("Invite", &value);
    assert!(invite.id.starts_with("inv_"), "{}", invite.id);
    assert_eq!(value["circle_id"], "cir_hinton4");
    assert_eq!(value["from"]["id"], "usr_dana");
    assert_eq!(value["message"], "Same crew as last time?");
    let invited: Vec<&str> = value["responses"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["user"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(invited, ["usr_judge", "usr_maya", "usr_leo"]);
    // The DMs the bot sends for it, one per invitee, with the answer buttons.
    let dms = record.dms();
    let to: Vec<&str> = dms.iter().map(|d| d.user_id.as_str()).collect();
    assert_eq!(to, invited);
    assert_eq!(
        dms[0].buttons[0].callback_data,
        format!("invite:{}:accept", invite.id)
    );
    // Exactly as the endpoint: the judge has the invite and its notification.
    let listed = get_ok(&server, "/v1/invites", "usr_judge").await;
    assert_eq!(listed[0]["id"], value["id"]);
    let notifications = get_ok(&server, "/v1/notifications", "usr_judge").await;
    assert_eq!(notifications[0]["type"], "invite");
    assert_eq!(notifications[0]["data"]["invite_id"], value["id"]);

    // usr_solo shares no circle with anyone, usr_out is in none.
    for user in ["usr_solo", "usr_out"] {
        assert_eq!(most_recent_circle(&pool, user).await.unwrap(), None);
        let err = meet_again_last_circle(&server.state, user, request())
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound, "{user}");
    }
}
