//! Notifications: notify stores and publishes notification.created; GET /v1/notifications lists
//! the viewer's own, newest first; POST /v1/notifications/{id}/read marks the owner's read.
mod common;
mod realtime_support;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::assert_envelope;
use ferry_core::notifications::{
    Event, HubSettings, Notification, NotificationType, create_notification, notify,
};
use realtime_support::{Server, assert_contract, assert_silent, next_json, serve, state};
use serde_json::{Value, json};
use sqlx::PgPool;

async fn add_user(pool: &PgPool, id: &str, phone: &str) {
    sqlx::query(
        "INSERT INTO ferry.users (id, phone_e164, display_name, avatar_seed, bubble_color) \
         VALUES ($1, $2, $1, $1, '#336699')",
    )
    .bind(id)
    .bind(phone)
    .execute(pool)
    .await
    .unwrap();
}

async fn server(pool: PgPool) -> Server {
    add_user(&pool, "usr_a", "+19195550101").await;
    add_user(&pool, "usr_b", "+19195550102").await;
    serve(state(pool, HubSettings::default())).await
}

/// `GET /v1/notifications` as `user_id`: asserts 200 and that each item is a contract
/// `Notification`.
async fn list(server: &Server, user_id: &str) -> Vec<Value> {
    let (status, body) = server.get_as("/v1/notifications", user_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let items = body.as_array().expect("an array").clone();
    for item in &items {
        assert_contract("Notification", item);
    }
    items
}

#[sqlx::test(migrations = "../migrations")]
async fn notify_publishes_notification_created_and_lists_it(pool: PgPool) {
    let server = server(pool).await;
    let mut a = server.connect_with_token("usr_a").await;
    let mut b = server.connect_with_token("usr_b").await;

    let notification = notify(
        &server.state,
        "usr_a",
        NotificationType::RunOpen,
        "Maya is heading to Cosmic Cantina",
        "Order by 6:10.",
        json!({"run_id": "run_demo"}),
    )
    .await
    .unwrap();
    assert!(notification.id.starts_with("ntf_"));
    assert!(!notification.read);

    let event = next_json(&mut a).await;
    assert_eq!(event["type"], "notification.created");
    assert_eq!(
        event["data"]["notification"],
        serde_json::to_value(&notification).unwrap()
    );
    assert_contract("NotificationCreatedEvent", &event);
    assert_contract("ServerEvent", &event);
    assert_silent(&mut b, Duration::from_millis(300)).await;

    let listed = list(&server, "usr_a").await;
    assert_eq!(listed, [serde_json::to_value(&notification).unwrap()]);
    assert_eq!(listed[0]["type"], "run_open");
    assert_eq!(listed[0]["data"], json!({"run_id": "run_demo"}));
    assert_eq!(listed[0]["read"], false);
    assert!(list(&server, "usr_b").await.is_empty());
}

#[sqlx::test(migrations = "../migrations")]
async fn the_list_is_newest_first(pool: PgPool) {
    let server = server(pool).await;
    let mut ids = Vec::new();
    for kind in NotificationType::ALL {
        let n = notify(&server.state, "usr_a", kind, kind.as_str(), "", json!({}))
            .await
            .unwrap();
        ids.push(n.id);
        // Distinct created_at values (millisecond precision).
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    notify(
        &server.state,
        "usr_b",
        NotificationType::System,
        "b's",
        "",
        json!({}),
    )
    .await
    .unwrap();

    let listed = list(&server, "usr_a").await;
    let listed_ids: Vec<&str> = listed.iter().map(|n| n["id"].as_str().unwrap()).collect();
    ids.reverse();
    assert_eq!(listed_ids, ids);
    let created: Vec<&str> = listed
        .iter()
        .map(|n| n["created_at"].as_str().unwrap())
        .collect();
    assert!(created.windows(2).all(|w| w[0] > w[1]), "{created:?}");
}

#[sqlx::test(migrations = "../migrations")]
async fn only_the_owner_marks_a_notification_read(pool: PgPool) {
    let server = server(pool).await;
    let n = notify(
        &server.state,
        "usr_a",
        NotificationType::OrderUpdate,
        "Your order is on its way",
        "Maya picked it up.",
        json!({"order_id": "ord_8kq2m4"}),
    )
    .await
    .unwrap();
    let uri = format!("/v1/notifications/{}/read", n.id);

    // Someone else's: not_found, and it stays unread.
    let (status, body) = server.post_as(&uri, "usr_b").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_envelope(&body, "not_found");
    assert_contract("Error", &body);
    assert_eq!(list(&server, "usr_a").await[0]["read"], false);

    // The owner's: 204 with no body, then read is true. Again is still 204.
    for _ in 0..2 {
        let (status, body) = server.post_as(&uri, "usr_a").await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(body, Value::Null);
        let listed = list(&server, "usr_a").await;
        assert_eq!(listed[0]["id"], n.id.as_str());
        assert_eq!(listed[0]["read"], true);
    }

    // Unknown ids.
    for id in ["ntf_doesnotexist", "not-an-id"] {
        let (status, body) = server
            .post_as(&format!("/v1/notifications/{id}/read"), "usr_a")
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{id}");
        assert_envelope(&body, "not_found");
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn the_endpoints_need_a_bearer_token(pool: PgPool) {
    let server = server(pool).await;
    for request in [
        Request::get("/v1/notifications").body(Body::empty()),
        Request::post("/v1/notifications/ntf_x/read").body(Body::empty()),
    ] {
        let (status, _, body) = server.send(request.unwrap()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_envelope(&body, "unauthorized");
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn create_notification_works_inside_a_transaction(pool: PgPool) {
    let server = server(pool.clone()).await;
    let mut a = server.connect_with_token("usr_a").await;

    // Rolled back: nothing stored, nothing sent.
    let mut tx = pool.begin().await.unwrap();
    create_notification(
        &mut *tx,
        "usr_a",
        NotificationType::Payout,
        "Rolled back",
        "",
        Value::Null,
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert!(list(&server, "usr_a").await.is_empty());

    // Committed, then published by the caller.
    let mut tx = pool.begin().await.unwrap();
    let n: Notification = create_notification(
        &mut *tx,
        "usr_a",
        NotificationType::Payout,
        "You earned $3.50",
        "For the Cosmic Cantina run.",
        Value::Null,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(n.data, json!({}), "null data is stored as {{}}");
    assert_eq!(
        server
            .hub()
            .send_to("usr_a", &Event::notification_created(&n)),
        1
    );
    assert_contract("NotificationCreatedEvent", &next_json(&mut a).await);
    assert_eq!(list(&server, "usr_a").await.len(), 1);

    // data must be an object.
    let err = create_notification(
        &pool,
        "usr_a",
        NotificationType::System,
        "x",
        "",
        json!([1]),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
}
