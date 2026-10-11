//! Nearby suggestions on the seeded demo (OSRM refused, so straight lines × 1.3). run_demo is
//! Maya's open Cosmic Cantina run, heading back to Hinton James (her profile drop-off). Sup Dogs
//! is 58 m from Cosmic Cantina, Lime & Basil 324 m and Mediterranean Deli 571 m. A request to
//! Fetzer is about a minute out of Maya's way from Cosmic Cantina, so a Sup Dogs request gets
//! Maya one `system` notice to open a second Ferry to Sup Dogs, once per customer, restaurant and
//! run; the request stays searching, the customer is told nothing, and no order or ledger row is
//! added. Further restaurants, a run that does not fit and a run to the request's own restaurant
//! get no such notice. When Maya opens that Ferry, the next sweep matches the request to it.
mod common;
mod realtime_support;
mod requests_support;
mod runs_support;

use axum::http::StatusCode;
use ferry_core::requests::{self, Sweep};
use realtime_support::Server;
use requests_support::{
    Stored, burrito_request, create, dog_request, money_rows, notifications, request_notices,
    set_home, stored,
};
use runs_support::{RUN, WEB_URL, demo_server, expect_events, fetzer, minutes_from_now, post};
use serde_json::{Value, json};
use sqlx::PgPool;

/// Davis Library: a runner heading there is 5.3 minutes out of their way via Fetzer.
const DAVIS: (f64, f64) = (35.910957, -79.047906);

/// `user`'s nearby suggestions: `system` notices with `new_run_restaurant_id`.
async fn nearby(pool: &PgPool, user: &str) -> Vec<Stored> {
    notifications(pool, user)
        .await
        .into_iter()
        .filter(|n| n.kind == "system" && n.data.get("new_run_restaurant_id").is_some())
        .collect()
}

/// A one-item request from `restaurant_id` to Fetzer, needed in 90 minutes.
async fn request_from(pool: &PgPool, restaurant_id: &str, item: &str, choices: &[&str]) -> Value {
    json!({
        "restaurant_id": restaurant_id,
        "lines": [{"menu_item_id": item, "qty": 1, "choice_ids": choices}],
        "tip_cents": 0,
        "dropoff": fetzer(),
        "needed_by": minutes_from_now(pool, 90).await
    })
}

async fn sweep(server: &Server) -> Sweep {
    requests::sweep_once(&server.state).await.unwrap()
}

#[sqlx::test(migrations = "../migrations")]
async fn a_runner_at_a_nearby_restaurant_is_asked_once(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let money = money_rows(&pool).await;
    let mut maya_ws = server.connect_with_token("usr_maya").await;
    let request = create(&server, "usr_judge", dog_request(&pool, 90).await).await;
    assert_eq!(request["status"], "searching", "{request}");
    assert_eq!(request["run_id"], Value::Null);
    let id = request["id"].as_str().unwrap();

    let maya = nearby(&pool, "usr_maya").await;
    assert_eq!(maya.len(), 1, "{maya:#?}");
    assert_eq!(
        maya[0].title,
        "Someone near your route wants Sup Dogs, a minute from Cosmic Cantina"
    );
    assert_eq!(
        maya[0].body,
        "Pick it up? Open a Ferry to Sup Dogs too and they can order on it. About a minute out \
         of your way."
    );
    assert_eq!(
        maya[0].data,
        json!({
            "run_id": RUN,
            "request_id": id,
            "new_run_restaurant_id": "rst_supdogs",
            "url": format!("{WEB_URL}/run/new?restaurant=rst_supdogs")
        })
    );
    let event = &expect_events(&mut maya_ws, "maya", &["notification.created"]).await[0];
    assert_eq!(
        event["data"]["notification"]["data"]["new_run_restaurant_id"],
        "rst_supdogs"
    );

    // The customer is told nothing, and the request stays searching.
    assert!(request_notices(&pool, "usr_judge").await.is_empty());
    assert_eq!(
        stored(&pool, id).await,
        ("searching".to_owned(), None, None)
    );

    // Once: two sweeps add nothing.
    let maya_count = notifications(&pool, "usr_maya").await.len();
    assert_eq!(sweep(&server).await, Sweep::default());
    assert_eq!(sweep(&server).await, Sweep::default());
    assert_eq!(notifications(&pool, "usr_maya").await.len(), maya_count);
    assert!(request_notices(&pool, "usr_judge").await.is_empty());
    assert_eq!(stored(&pool, id).await.0, "searching");
    assert_eq!(money_rows(&pool).await, money);
}

#[sqlx::test(migrations = "../migrations")]
async fn a_double_tap_asks_once(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let first = create(&server, "usr_judge", dog_request(&pool, 90).await).await;
    let second = create(&server, "usr_judge", dog_request(&pool, 60).await).await;
    assert_eq!(
        (first["status"].as_str(), second["status"].as_str()),
        (Some("searching"), Some("searching"))
    );
    assert_eq!(sweep(&server).await, Sweep::default());
    let maya = nearby(&pool, "usr_maya").await;
    assert_eq!(maya.len(), 1, "{maya:#?}");
    assert_eq!(maya[0].data["request_id"], first["id"]);
}

#[sqlx::test(migrations = "../migrations")]
async fn concurrent_suggestions_send_one(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    // Made while the run is closed, so creating it suggests nothing; then the run reopens.
    sqlx::query("UPDATE ferry.runs SET status = 'closed' WHERE id = $1")
        .bind(RUN)
        .execute(&pool)
        .await
        .unwrap();
    let request = create(&server, "usr_judge", dog_request(&pool, 90).await).await;
    let id = request["id"].as_str().unwrap();
    assert!(nearby(&pool, "usr_maya").await.is_empty());
    sqlx::query("UPDATE ferry.runs SET status = 'open' WHERE id = $1")
        .bind(RUN)
        .execute(&pool)
        .await
        .unwrap();

    let state = &server.state;
    let (a, b, c) = tokio::join!(
        requests::matcher::match_request(state, id),
        requests::matcher::match_request(state, id),
        requests::sweep_once(state),
    );
    assert_eq!((a.unwrap(), b.unwrap()), (None, None));
    assert_eq!(c.unwrap(), Sweep::default());
    assert_eq!(nearby(&pool, "usr_maya").await.len(), 1);
}

#[sqlx::test(migrations = "../migrations")]
async fn restaurants_further_than_300_m_are_not_nearby(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    // Lime & Basil is 324 m from Cosmic Cantina; Mediterranean Deli 571 m.
    for (restaurant, item) in [
        ("rst_limebasil", "itm_limebasil_egg_rolls"),
        ("rst_meddeli", "itm_meddeli_baklava"),
    ] {
        let request = create(
            &server,
            "usr_judge",
            request_from(&pool, restaurant, item, &[]).await,
        )
        .await;
        assert_eq!(request["status"], "searching", "{request}");
    }
    assert_eq!(sweep(&server).await, Sweep::default());
    assert!(nearby(&pool, "usr_maya").await.is_empty());
    assert!(request_notices(&pool, "usr_maya").await.is_empty());

    // Insomnia Cookies, 104 m away, is nearby.
    let request = create(
        &server,
        "usr_judge",
        request_from(
            &pool,
            "rst_insomnia",
            "itm_insomnia_milk",
            &["cho_insomnia_milk_whole"],
        )
        .await,
    )
    .await;
    assert_eq!(request["status"], "searching", "{request}");
    let maya = nearby(&pool, "usr_maya").await;
    assert_eq!(maya.len(), 1, "{maya:#?}");
    assert_eq!(maya[0].data["new_run_restaurant_id"], "rst_insomnia");
}

#[sqlx::test(migrations = "../migrations")]
async fn a_nearby_run_that_does_not_fit_is_not_asked(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    // Heading to Davis Library, Fetzer is over 5 minutes out of Maya's way.
    set_home(&pool, "usr_maya", Some(DAVIS)).await;
    let request = create(&server, "usr_judge", dog_request(&pool, 90).await).await;
    assert_eq!(request["status"], "searching", "{request}");
    assert_eq!(sweep(&server).await, Sweep::default());
    assert!(nearby(&pool, "usr_maya").await.is_empty());
}

#[sqlx::test(migrations = "../migrations")]
async fn a_run_to_the_restaurant_itself_matches_without_a_nearby_notice(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let request = create(&server, "usr_judge", burrito_request(&pool, 60).await).await;
    assert_eq!(request["status"], "matched", "{request}");
    assert_eq!(request["run_id"], RUN);
    assert!(nearby(&pool, "usr_maya").await.is_empty());
    let maya = request_notices(&pool, "usr_maya").await;
    assert_eq!(maya.len(), 1, "{maya:#?}");
    assert_eq!(
        maya[0].title,
        "Someone near your route wants Cosmic Cantina"
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn opening_the_suggested_ferry_matches_the_request(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let money = money_rows(&pool).await;
    let request = create(&server, "usr_judge", dog_request(&pool, 90).await).await;
    let id = request["id"].as_str().unwrap();
    assert_eq!(nearby(&pool, "usr_maya").await.len(), 1);

    // Maya taps it and opens a Sup Dogs run.
    let body = json!({
        "restaurant_id": "rst_supdogs",
        "circle_id": "cir_hinton4",
        "leaving_at": minutes_from_now(&pool, 5).await,
        "window_minutes": 30
    });
    let (status, run) = post(&server, "/v1/runs", "usr_maya", Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{run}");
    let run_id = run["id"].as_str().unwrap();

    assert_eq!(
        sweep(&server).await,
        Sweep {
            expired: 0,
            matched: 1
        }
    );
    assert_eq!(
        stored(&pool, id).await,
        ("matched".to_owned(), Some(run_id.to_owned()), None)
    );
    let judge = request_notices(&pool, "usr_judge").await;
    assert_eq!(judge.len(), 1, "{judge:#?}");
    assert_eq!(judge[0].title, "Maya is heading to Sup Dogs");
    assert_eq!(nearby(&pool, "usr_maya").await.len(), 1);
    assert_eq!(money_rows(&pool).await, money);
}
