//! The requests sweep on the seeded demo (OSRM refused, so straight lines × 1.3): a request for
//! Sup Dogs stays searching until Maya opens a Sup Dogs run, then the sweep matches it exactly
//! once (also with the at-once match and a second sweep racing it); requests past `needed_by`
//! expire with one notification each, and matched ones never expire; an expiry that fails does not
//! stop that sweep's matching. No order or ledger row is ever added. Tests call
//! `requests::sweep_once` directly; nothing runs it on a timer here.
mod common;
mod realtime_support;
mod requests_support;
mod runs_support;

use axum::http::StatusCode;
use ferry_core::requests::{self, Sweep};
use realtime_support::Server;
use requests_support::{
    burrito_request, create, dog_request, money_rows, notifications, request_notices, stored,
};
use runs_support::{RUN, WEB_URL, demo_server, expect_events, minutes_from_now, post};
use serde_json::json;
use sqlx::PgPool;

/// Maya opens a Sup Dogs run in cir_hinton4, leaving in 5 minutes with a 30-minute window: its id.
async fn maya_opens_sup_dogs(server: &Server) -> String {
    let body = json!({
        "restaurant_id": "rst_supdogs",
        "circle_id": "cir_hinton4",
        "leaving_at": minutes_from_now(&server.state.pool, 5).await,
        "window_minutes": 30
    });
    let (status, run) = post(server, "/v1/runs", "usr_maya", Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{run}");
    run["id"].as_str().unwrap().to_owned()
}

async fn sweep(server: &Server) -> Sweep {
    requests::sweep_once(&server.state).await.unwrap()
}

#[sqlx::test(migrations = "../migrations")]
async fn a_run_that_opens_later_is_matched_exactly_once(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let money = money_rows(&pool).await;
    let request = create(&server, "usr_judge", dog_request(&pool, 90).await).await;
    assert_eq!(request["status"], "searching");
    let id = request["id"].as_str().unwrap();

    // Nothing to match yet.
    assert_eq!(sweep(&server).await, Sweep::default());
    assert_eq!(stored(&pool, id).await.0, "searching");

    // Maya opens a Sup Dogs run; the judge, a member, gets the run's own run_open first.
    let mut judge_ws = server.connect_with_token("usr_judge").await;
    let mut maya_ws = server.connect_with_token("usr_maya").await;
    let run_id = maya_opens_sup_dogs(&server).await;
    expect_events(
        &mut judge_ws,
        "judge",
        &["notification.created", "run.updated"],
    )
    .await;
    expect_events(&mut maya_ws, "maya", &["run.updated"]).await;

    // The next sweep matches it: Sup Dogs to Fetzer is about a minute out of Maya's way home.
    assert_eq!(
        sweep(&server).await,
        Sweep {
            expired: 0,
            matched: 1
        }
    );
    assert_eq!(
        stored(&pool, id).await,
        ("matched".to_owned(), Some(run_id.clone()), None)
    );
    let judge = request_notices(&pool, "usr_judge").await;
    assert_eq!(judge.len(), 1, "{judge:#?}");
    assert_eq!(judge[0].kind, "run_open");
    assert_eq!(judge[0].title, "Maya is heading to Sup Dogs");
    assert_eq!(judge[0].data["url"], format!("{WEB_URL}/r/{run_id}"));
    // Maya's first notice is the nearby one from run_demo (Cosmic Cantina, 58 m from Sup Dogs),
    // sent when the request was made; the match adds the usual suggestion.
    let maya = request_notices(&pool, "usr_maya").await;
    assert_eq!(maya.len(), 2, "{maya:#?}");
    assert_eq!(maya[0].data["new_run_restaurant_id"], "rst_supdogs");
    assert_eq!(maya[1].title, "Someone near your route wants Sup Dogs");
    let event = &expect_events(&mut judge_ws, "judge", &["notification.created"]).await[0];
    assert_eq!(event["data"]["notification"]["data"]["request_id"], id);
    expect_events(&mut maya_ws, "maya", &["notification.created"]).await;

    // Never twice: further sweeps change nothing.
    let before = (
        notifications(&pool, "usr_judge").await.len(),
        notifications(&pool, "usr_maya").await.len(),
    );
    assert_eq!(sweep(&server).await, Sweep::default());
    assert_eq!(sweep(&server).await, Sweep::default());
    assert_eq!(
        (
            notifications(&pool, "usr_judge").await.len(),
            notifications(&pool, "usr_maya").await.len()
        ),
        before
    );
    assert_eq!(stored(&pool, id).await.1, Some(run_id));
    assert_eq!(money_rows(&pool).await, money);
}

#[sqlx::test(migrations = "../migrations")]
async fn racing_matches_claim_once(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let request = create(&server, "usr_judge", dog_request(&pool, 90).await).await;
    let id = request["id"].as_str().unwrap();
    maya_opens_sup_dogs(&server).await;

    let state = &server.state;
    let (a, b, c) = tokio::join!(
        requests::matcher::match_request(state, id),
        requests::matcher::match_request(state, id),
        requests::sweep_once(state),
    );
    let matched = [a.unwrap().is_some(), b.unwrap().is_some()]
        .iter()
        .filter(|m| **m)
        .count() as u64
        + c.unwrap().matched;
    assert_eq!(matched, 1);
    assert_eq!(stored(&pool, id).await.0, "matched");
    assert_eq!(request_notices(&pool, "usr_judge").await.len(), 1);
    // The nearby notice from run_demo, sent when the request was made, then the match's.
    assert_eq!(request_notices(&pool, "usr_maya").await.len(), 2);
}

#[sqlx::test(migrations = "../migrations")]
async fn requests_expire_at_needed_by_once(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let money = money_rows(&pool).await;
    let searching = create(&server, "usr_judge", dog_request(&pool, 5).await).await;
    assert_eq!(searching["status"], "searching");
    let id = searching["id"].as_str().unwrap();
    // A request matched to run_demo, also past its deadline later: it stays matched.
    let matched = create(&server, "usr_judge", burrito_request(&pool, 60).await).await;
    assert_eq!(matched["status"], "matched");
    let matched_id = matched["id"].as_str().unwrap();

    // Before needed_by nothing expires.
    assert_eq!(sweep(&server).await, Sweep::default());

    // needed_by passes.
    sqlx::query(
        "UPDATE ferry.pickup_requests SET needed_by = now() - interval '1 second' \
         WHERE id = ANY($1)",
    )
    .bind(vec![id.to_owned(), matched_id.to_owned()])
    .execute(&pool)
    .await
    .unwrap();
    let mut judge_ws = server.connect_with_token("usr_judge").await;
    assert_eq!(
        sweep(&server).await,
        Sweep {
            expired: 1,
            matched: 0
        }
    );
    assert_eq!(stored(&pool, id).await, ("expired".to_owned(), None, None));
    assert_eq!(
        stored(&pool, matched_id).await,
        ("matched".to_owned(), Some(RUN.to_owned()), None)
    );
    let expired: Vec<_> = request_notices(&pool, "usr_judge")
        .await
        .into_iter()
        .filter(|n| n.kind == "system")
        .collect();
    assert_eq!(expired.len(), 1, "{expired:#?}");
    assert_eq!(expired[0].title, "No run to Sup Dogs in time");
    assert_eq!(
        expired[0].data,
        json!({"request_id": id, "restaurant_id": "rst_supdogs"})
    );
    expect_events(&mut judge_ws, "judge", &["notification.created"]).await;

    // Once: the next sweep finds nothing, and an expired request is never matched, even when a
    // fitting run opens.
    maya_opens_sup_dogs(&server).await;
    assert_eq!(sweep(&server).await, Sweep::default());
    assert_eq!(stored(&pool, id).await.0, "expired");
    let systems = request_notices(&pool, "usr_judge")
        .await
        .into_iter()
        .filter(|n| n.kind == "system")
        .count();
    assert_eq!(systems, 1);
    assert_eq!(money_rows(&pool).await, money);
}

#[sqlx::test(migrations = "../migrations")]
async fn an_expiry_error_does_not_skip_matching(pool: PgPool) {
    let server = demo_server(pool.clone()).await;
    let due = create(&server, "usr_judge", dog_request(&pool, 5).await).await;
    let due = due["id"].as_str().unwrap();
    let waiting = create(&server, "usr_judge", dog_request(&pool, 90).await).await;
    assert_eq!(waiting["status"], "searching");
    let waiting = waiting["id"].as_str().unwrap();
    sqlx::query(
        "UPDATE ferry.pickup_requests SET needed_by = now() - interval '1 second' WHERE id = $1",
    )
    .bind(due)
    .execute(&pool)
    .await
    .unwrap();

    // The expiry notice cannot be stored, so the expiry transaction fails.
    sqlx::raw_sql(
        "CREATE FUNCTION ferry.refuse_expiry_notice() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN \
             IF NEW.title LIKE 'No run to %' THEN RAISE EXCEPTION 'expiry notice refused'; END IF; \
             RETURN NEW; \
         END $$; \
         CREATE TRIGGER refuse_expiry_notice BEFORE INSERT ON ferry.notifications \
             FOR EACH ROW EXECUTE FUNCTION ferry.refuse_expiry_notice();",
    )
    .execute(&pool)
    .await
    .unwrap();
    maya_opens_sup_dogs(&server).await;

    // The sweep still matches the waiting request; the due one stays searching.
    assert_eq!(
        sweep(&server).await,
        Sweep {
            expired: 0,
            matched: 1
        }
    );
    assert_eq!(stored(&pool, waiting).await.0, "matched");
    assert_eq!(stored(&pool, due).await.0, "searching");

    // Once the notice can be stored, the next sweep expires it.
    sqlx::raw_sql("DROP TRIGGER refuse_expiry_notice ON ferry.notifications")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        sweep(&server).await,
        Sweep {
            expired: 1,
            matched: 0
        }
    );
    assert_eq!(stored(&pool, due).await.0, "expired");
}
