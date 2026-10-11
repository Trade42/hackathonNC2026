//! Public communities (migration 20261011000008, `circles.is_public`), starting with UNC Chapel
//! Hill (cir_campus), the circle every user is in. New users join it through the
//! `users_join_campus` trigger; public circles are listed first with no members; anyone can see and
//! order on a trip posted there while friends' circles stay private; they refuse meet-again; opening
//! a run in one stores no `run_open` for anyone; a circle made public by data alone gets the same
//! guardrails; the migration runs twice without change; and with the cir_campus row gone, users
//! still sign up.
mod common;
mod payments_support;
mod realtime_support;
mod runs_support;

use axum::http::StatusCode;
use common::assert_envelope;
use ferry_core::circles::most_recent_circle;
use realtime_support::{Server, assert_contract};
use runs_support::{OUTSIDER, chips, demo_server, get, open_run, post, topup};
use serde_json::{Value, json};
use sqlx::PgPool;

const CAMPUS: &str = "cir_campus";
const MIGRATION: &str = include_str!("../../migrations/20261011000008_campus_circle.sql");

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

/// How many circle_members rows `user_id` has in `circle_id`.
async fn memberships(pool: &PgPool, circle_id: &str, user_id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM ferry.circle_members WHERE circle_id = $1 AND user_id = $2",
    )
    .bind(circle_id)
    .bind(user_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// How many `run_open` notifications were stored for `run_id`, to anyone.
async fn run_open_rows(pool: &PgPool, run_id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM ferry.notifications \
         WHERE type = 'run_open' AND data->>'run_id' = $1",
    )
    .bind(run_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// `GET uri` as `user_id`: asserts 200 and returns the body.
async fn get_ok(server: &Server, uri: &str, user_id: &str) -> Value {
    let (status, body) = get(server, uri, user_id).await;
    assert_eq!(status, StatusCode::OK, "{uri} as {user_id}: {body}");
    body
}

/// `GET /v1/circles` as `user_id`, each item checked against the contract's `Circle`.
async fn circles(server: &Server, user_id: &str) -> Vec<Value> {
    let body = get_ok(server, "/v1/circles", user_id).await;
    let items = body.as_array().expect("an array").clone();
    for item in &items {
        assert_contract("Circle", item);
    }
    items
}

fn ids(items: &[Value]) -> Vec<&str> {
    items.iter().map(|c| c["id"].as_str().unwrap()).collect()
}

/// The run ids in `GET /v1/runs` as `user_id`, each checked against the contract's `Run`.
async fn run_ids(server: &Server, user_id: &str) -> Vec<String> {
    let body = get_ok(server, "/v1/runs", user_id).await;
    body.as_array()
        .expect("an array")
        .iter()
        .map(|r| {
            assert_contract("Run", r);
            r["id"].as_str().unwrap().to_owned()
        })
        .collect()
}

fn assert_not_found(what: &str, (status, body): (StatusCode, Value)) {
    assert_eq!(status, StatusCode::NOT_FOUND, "{what}: {body}");
    assert_envelope(&body, "not_found");
    assert_contract("Error", &body);
}

#[sqlx::test(migrations = "../migrations")]
async fn a_new_user_joins_campus_and_sees_only_it(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    add_user(&pool, "usr_fresh", "Fresh", "+19195550188").await;
    assert_eq!(memberships(&pool, CAMPUS, "usr_fresh").await, 1);
    let total: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ferry.circle_members WHERE user_id = 'usr_fresh'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(total, 1, "Campus and nothing else");

    let listed = circles(&server, "usr_fresh").await;
    assert_eq!(ids(&listed), [CAMPUS]);
    assert_eq!(listed[0]["name"], "UNC Chapel Hill");
    assert_eq!(listed[0]["emoji"], "🐏");
    assert_eq!(listed[0]["is_public"], true);
    assert_eq!(listed[0]["members"], json!([]));

    // Campus is never the agent's "most recent circle".
    assert_eq!(most_recent_circle(&pool, "usr_fresh").await.unwrap(), None);
}

#[sqlx::test(migrations = "../migrations")]
async fn strangers_see_and_order_on_a_campus_trip(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    // The judge and the outsider share no friends circle.
    let shared: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ferry.circle_members a JOIN ferry.circle_members b \
         ON a.circle_id = b.circle_id \
         WHERE a.user_id = 'usr_judge' AND b.user_id = $1 AND a.circle_id <> $2",
    )
    .bind(OUTSIDER)
    .bind(CAMPUS)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(shared, 0);

    let run_id = open_run(&server, "usr_judge", CAMPUS).await;
    assert!(run_ids(&server, OUTSIDER).await.contains(&run_id));
    let run = get_ok(&server, &format!("/v1/runs/{run_id}"), OUTSIDER).await;
    assert_eq!(run["id"], run_id.as_str());
    assert_eq!(run["runner"]["id"], "usr_judge");

    topup(&server, OUTSIDER, 100_000).await;
    let (status, order) = post(
        &server,
        &format!("/v1/runs/{run_id}/orders"),
        OUTSIDER,
        Some(chips()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{order}");
    assert_contract("Order", &order);
    assert_eq!(order["status"], "funded");
    assert_eq!(order["run_id"], run_id.as_str());
}

#[sqlx::test(migrations = "../migrations")]
async fn a_friends_circle_stays_private(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let run_id = open_run(&server, "usr_maya", "cir_hinton4").await;

    assert_not_found(
        "the friends' run",
        get(&server, &format!("/v1/runs/{run_id}"), OUTSIDER).await,
    );
    let listed = run_ids(&server, OUTSIDER).await;
    assert!(!listed.contains(&run_id), "{listed:?}");
    assert!(!listed.iter().any(|r| r == "run_demo"), "{listed:?}");
    assert_not_found(
        "the friends' circle",
        get(&server, "/v1/circles/cir_hinton4", OUTSIDER).await,
    );
    assert_eq!(ids(&circles(&server, OUTSIDER).await), [CAMPUS]);

    // A member still sees it.
    assert!(run_ids(&server, "usr_judge").await.contains(&run_id));
}

#[sqlx::test(migrations = "../migrations")]
async fn campus_is_listed_first_without_members(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    // Make the friends circle the most recently active, so cir_campus is first by rule, not by date.
    open_run(&server, "usr_maya", "cir_hinton4").await;

    let listed = circles(&server, "usr_judge").await;
    assert_eq!(ids(&listed)[0], CAMPUS, "{listed:#?}");
    assert!(ids(&listed).contains(&"cir_hinton4"), "{listed:#?}");
    assert_eq!(listed[0]["name"], "UNC Chapel Hill");
    assert_eq!(listed[0]["emoji"], "🐏");
    assert_eq!(listed[0]["is_public"], true);
    assert_eq!(listed[0]["members"], json!([]));
    let hinton = listed.iter().find(|c| c["id"] == "cir_hinton4").unwrap();
    assert_eq!(hinton["is_public"], false);
    // The demo seed gives its members one joined_at, so they come back in id order.
    let members: Vec<&str> = hinton["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(members, ["usr_dana", "usr_judge", "usr_leo", "usr_maya"]);

    let campus = get_ok(&server, "/v1/circles/cir_campus", "usr_judge").await;
    assert_contract("Circle", &campus);
    assert_eq!(campus["id"], CAMPUS);
    assert_eq!(campus["is_public"], true);
    assert_eq!(campus["members"], json!([]));
    assert_eq!(campus, listed[0]);
}

#[sqlx::test(migrations = "../migrations")]
async fn campus_refuses_meet_again(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let invites_before: i64 = sqlx::query_scalar("SELECT count(*) FROM ferry.invites")
        .fetch_one(&pool)
        .await
        .unwrap();
    for body in [None, Some(json!({"message": "Anyone around?"}))] {
        let (status, error) = post(
            &server,
            "/v1/circles/cir_campus/meet-again",
            "usr_judge",
            body,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_envelope(&error, "conflict");
        assert_contract("Error", &error);
    }
    let campus_invites: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ferry.invites WHERE circle_id = 'cir_campus'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(campus_invites, 0);
    let invites_after: i64 = sqlx::query_scalar("SELECT count(*) FROM ferry.invites")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(invites_after, invites_before);
}

#[sqlx::test(migrations = "../migrations")]
async fn a_campus_run_notifies_nobody_and_a_friends_run_notifies_each_member(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let campus_run = open_run(&server, "usr_maya", CAMPUS).await;
    assert_eq!(run_open_rows(&pool, &campus_run).await, 0);

    let friends_run = open_run(&server, "usr_maya", "cir_hinton4").await;
    assert_eq!(run_open_rows(&pool, &friends_run).await, 3);
    let notified: Vec<String> = sqlx::query_scalar(
        "SELECT user_id FROM ferry.notifications \
         WHERE type = 'run_open' AND data->>'run_id' = $1 ORDER BY user_id",
    )
    .bind(&friends_run)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(notified, ["usr_dana", "usr_judge", "usr_leo"]);
}

#[sqlx::test(migrations = "../migrations")]
async fn a_circle_made_public_by_data_gets_the_guardrails(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    sqlx::query("UPDATE ferry.circles SET is_public = true WHERE id = 'cir_climb'")
        .execute(&pool)
        .await
        .unwrap();

    // Both public circles come before the private one.
    let listed = circles(&server, "usr_maya").await;
    let mut public = ids(&listed)[..2].to_vec();
    public.sort_unstable();
    assert_eq!(public, ["cir_campus", "cir_climb"], "{listed:#?}");
    assert_eq!(ids(&listed)[2..], ["cir_hinton4"], "{listed:#?}");
    let climb = listed.iter().find(|c| c["id"] == "cir_climb").unwrap();
    assert_eq!(climb["is_public"], true);
    assert_eq!(climb["members"], json!([]));
    let one = get_ok(&server, "/v1/circles/cir_climb", "usr_maya").await;
    assert_contract("Circle", &one);
    assert_eq!(one["members"], json!([]));
    let hinton = listed.iter().find(|c| c["id"] == "cir_hinton4").unwrap();
    assert_eq!(hinton["is_public"], false);
    assert_eq!(hinton["members"].as_array().unwrap().len(), 4);

    // No meet-again.
    let (status, error) = post(
        &server,
        "/v1/circles/cir_climb/meet-again",
        "usr_maya",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_envelope(&error, "conflict");
    assert_contract("Error", &error);
    let invites: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ferry.invites WHERE circle_id = 'cir_climb'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(invites, 0);

    // A run there pings nobody, Leo included.
    let run_id = open_run(&server, "usr_maya", "cir_climb").await;
    assert_eq!(run_open_rows(&pool, &run_id).await, 0);
    assert!(run_ids(&server, "usr_leo").await.contains(&run_id));

    // Never the agent's most recent circle, though it is now the most recently active.
    assert_eq!(
        most_recent_circle(&pool, "usr_maya")
            .await
            .unwrap()
            .as_deref(),
        Some("cir_hinton4")
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn the_migration_runs_twice_without_change(pool: PgPool) {
    add_user(&pool, "usr_a", "A", "+19195550181").await;
    add_user(&pool, "usr_b", "B", "+19195550182").await;
    sqlx::raw_sql(MIGRATION).execute(&pool).await.unwrap();

    let campus_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ferry.circles WHERE id = 'cir_campus'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(campus_rows, 1);
    let (users, members, distinct): (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM ferry.users), \
                (SELECT count(*) FROM ferry.circle_members WHERE circle_id = 'cir_campus'), \
                (SELECT count(DISTINCT user_id) FROM ferry.circle_members WHERE circle_id = 'cir_campus')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(users, 2);
    assert_eq!(members, users);
    assert_eq!(distinct, users);

    // The trigger still fires once.
    add_user(&pool, "usr_c", "C", "+19195550183").await;
    assert_eq!(memberships(&pool, CAMPUS, "usr_c").await, 1);
}

#[sqlx::test(migrations = "../migrations")]
async fn without_the_campus_row_users_still_sign_up(pool: PgPool) {
    add_user(&pool, "usr_a", "A", "+19195550181").await;
    sqlx::query("DELETE FROM ferry.circle_members WHERE circle_id = 'cir_campus'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ferry.circles WHERE id = 'cir_campus'")
        .execute(&pool)
        .await
        .unwrap();

    add_user(&pool, "usr_b", "B", "+19195550182").await;
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ferry.circle_members WHERE user_id = 'usr_b'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(rows, 0);
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM ferry.users WHERE id = 'usr_b')")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(exists);
}
