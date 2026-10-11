//! meet-place, contract v1.5.0, over REST and the stream: `MeetAgainRequest.place` (a restaurant's
//! or known place's id, or a label with a location), `Invite.place` in every response and in
//! `invite.updated`, the 400s for a bad place, the invitees' notification with the place and the
//! time, and each answer relayed to the inviter once as a notification. Every response and event
//! validates against contract/openapi.yaml. The catalog is the demo seed's (contract/fixtures).
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

const QUIET: Duration = Duration::from_millis(300);
/// rst_cosmic's location in contract/fixtures/restaurants.json.
const COSMIC: (f64, f64) = (35.913211, -79.055094);

async fn server(pool: PgPool) -> Server {
    ferry_core::demo::seed::seed_pool(&pool).await.unwrap();
    serve(
        AppState::new(
            pool,
            common::config(&[("PUBLIC_WEB_URL", "https://ferry.example")]),
        )
        .unwrap(),
    )
    .await
}

async fn post(server: &Server, uri: &str, user_id: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::post(uri)
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", server.token(user_id)),
        )
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, _, body) = server.send(request).await;
    (status, body)
}

/// Dana's meet-again to cir_hinton4 with `body`: 200 and a contract `Invite`.
async fn meet_again(server: &Server, body: Value) -> Value {
    let (status, invite) = post(
        server,
        "/v1/circles/cir_hinton4/meet-again",
        "usr_dana",
        body,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{invite}");
    assert_contract("Invite", &invite);
    invite
}

/// The next event on `ws`, of type `kind`, validated against the contract; returns its data.
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
    event["data"].clone()
}

/// An ISO 8601 UTC time two days from now (a day after tomorrow, so its wording is fixed).
async fn in_two_days(pool: &PgPool) -> String {
    sqlx::query_scalar(
        "SELECT to_char((now() + interval '2 days') AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

/// `time` on Chapel Hill's clock, as the ping words a day after tomorrow: `7:00 PM on Tue, Oct 13`.
async fn clock(pool: &PgPool, time: &str) -> String {
    sqlx::query_scalar(
        "SELECT to_char($1::timestamptz AT TIME ZONE 'America/New_York', 'FMHH12:MI AM \"on\" Dy, Mon FMDD')",
    )
    .bind(time)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrations = "../migrations")]
async fn a_place_by_id_takes_the_catalogs_name_and_location(pool: PgPool) {
    let server = server(pool.clone()).await;
    let mut judge = server.connect_with_token("usr_judge").await;
    let mut dana = server.connect_with_token("usr_dana").await;
    let when = in_two_days(&pool).await;

    // A restaurant: whatever label and location the client sent, the catalog's.
    let invite = meet_again(
        &server,
        json!({
            "proposed_time": when,
            "place": {"id": "rst_cosmic", "label": "cosmic??", "location": {"lat": 0.0, "lng": 0.0}}
        }),
    )
    .await;
    assert_eq!(
        invite["place"],
        json!({"id": "rst_cosmic", "label": "Cosmic Cantina", "location": {"lat": COSMIC.0, "lng": COSMIC.1}})
    );
    let id = invite["id"].as_str().unwrap().to_owned();

    // The judge's notification says the place and the time, and the event carries the place.
    let data = expect_event(&mut judge, "notification.created").await;
    let notification = &data["notification"];
    assert_eq!(
        notification["title"],
        format!(
            "Dana wants to go to Cosmic Cantina at {}",
            clock(&pool, &when).await
        )
    );
    assert_eq!(notification["body"], "You in?");
    assert_eq!(notification["data"]["invite_id"], id.as_str());
    let data = expect_event(&mut judge, "invite.updated").await;
    assert_eq!(data["invite"]["place"], invite["place"]);
    let data = expect_event(&mut dana, "invite.updated").await;
    assert_eq!(data["invite"], invite);

    // A known place.
    let fetzer = meet_again(
        &server,
        json!({"place": {"id": "place_fetzer", "label": "x", "location": {"lat": 1.0, "lng": 1.0}}}),
    )
    .await;
    assert_eq!(fetzer["place"]["id"], "place_fetzer");
    assert_eq!(fetzer["place"]["label"], "Fetzer Gym (HackNC)");
    assert_eq!(fetzer["place"]["location"]["lat"], 35.909152);
    let data = expect_event(&mut judge, "notification.created").await;
    assert_eq!(
        data["notification"]["title"],
        "Dana wants to go to Fetzer Gym (HackNC)"
    );

    // A map pin: no id, the label and location as given (trimmed).
    let pin = meet_again(
        &server,
        json!({"place": {"label": "  Polk Place  ", "location": {"lat": 35.9105, "lng": -79.0505}}}),
    )
    .await;
    assert_eq!(
        pin["place"],
        json!({"id": null, "label": "Polk Place", "location": {"lat": 35.9105, "lng": -79.0505}})
    );

    // No place, or null: Invite.place is null, and still sent.
    for body in [json!({}), json!({"place": null})] {
        let none = meet_again(&server, body).await;
        assert_eq!(none["place"], Value::Null);
        assert!(none.as_object().unwrap().contains_key("place"));
    }

    // Every list copy validates, newest first.
    let (status, list) = server.get_as("/v1/invites", "usr_judge").await;
    assert_eq!(status, StatusCode::OK);
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 5);
    for invite in list {
        assert_contract("Invite", invite);
    }
    assert_eq!(list[4]["place"]["label"], "Cosmic Cantina");
}

#[sqlx::test(migrations = "../migrations")]
async fn a_bad_place_is_400_and_creates_nothing(pool: PgPool) {
    let server = server(pool.clone()).await;
    let pin =
        |label: &str, lat: f64| json!({"label": label, "location": {"lat": lat, "lng": -79.05}});
    for (what, place, field) in [
        (
            "not a restaurant or a place",
            json!({"id": "usr_maya", "label": "x", "location": {"lat": 1.0, "lng": 1.0}}),
            "place.id",
        ),
        (
            "no such restaurant",
            json!({"id": "rst_nope", "label": "x", "location": {"lat": 1.0, "lng": 1.0}}),
            "place.id",
        ),
        ("an empty label", pin("   ", 35.9), "place.label"),
        (
            "a label too long",
            pin(&"x".repeat(101), 35.9),
            "place.label",
        ),
        ("off the map", pin("Polk Place", 91.0), "place.location"),
    ] {
        let (status, body) = post(
            &server,
            "/v1/circles/cir_hinton4/meet-again",
            "usr_dana",
            json!({"place": place}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{what}: {body}");
        assert_envelope(&body, "validation_failed");
        assert_contract("Error", &body);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .starts_with(field),
            "{what}: {body}"
        );
    }
    // A label without a location is not a Place.
    let (status, body) = post(
        &server,
        "/v1/circles/cir_hinton4/meet-again",
        "usr_dana",
        json!({"place": {"label": "Polk Place"}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_envelope(&body, "validation_failed");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM ferry.invites")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrations = "../migrations")]
async fn each_answer_reaches_the_inviter_once(pool: PgPool) {
    let server = server(pool.clone()).await;
    let when = in_two_days(&pool).await;
    let invite = meet_again(
        &server,
        json!({"proposed_time": when, "place": {"id": "rst_supdogs", "label": "Sup Dogs", "location": {"lat": 1.0, "lng": 1.0}}}),
    )
    .await;
    let id = invite["id"].as_str().unwrap().to_owned();
    let uri = format!("/v1/invites/{id}/respond");
    let mut dana = server.connect_with_token("usr_dana").await;
    let plan = format!("Sup Dogs at {}.", clock(&pool, &when).await);

    for (who, accept, line) in [
        ("usr_judge", true, "Jordan's in."),
        ("usr_maya", false, "Maya can't make it."),
        ("usr_judge", false, "Jordan can't make it."),
    ] {
        let (status, answered) = post(&server, &uri, who, json!({"accept": accept})).await;
        assert_eq!(status, StatusCode::OK, "{answered}");
        assert_contract("Invite", &answered);
        assert_eq!(answered["place"]["id"], "rst_supdogs");
        let data = expect_event(&mut dana, "notification.created").await;
        let notification = &data["notification"];
        assert_eq!(notification["type"], "invite");
        assert_eq!(notification["title"], line);
        assert_eq!(notification["body"], plan.as_str());
        assert_eq!(
            notification["data"],
            json!({"invite_id": id, "circle_id": "cir_hinton4", "url": format!("https://ferry.example/i/{id}")})
        );
        let data = expect_event(&mut dana, "invite.updated").await;
        assert_eq!(data["invite"]["place"]["label"], "Sup Dogs");
    }

    // The same answer again: the invite is published, and nothing is relayed.
    let (status, _) = post(&server, &uri, "usr_judge", json!({"accept": false})).await;
    assert_eq!(status, StatusCode::OK);
    expect_event(&mut dana, "invite.updated").await;
    assert_silent(&mut dana, QUIET).await;
    let relays: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ferry.notifications WHERE user_id = 'usr_dana'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(relays, 3);
}
