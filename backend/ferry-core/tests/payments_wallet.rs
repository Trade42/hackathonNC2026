//! GET /v1/wallet and POST /v1/wallet/demo-topup over HTTP, validated against the contract's
//! Wallet schema (contract/openapi.yaml).
mod common;
mod payments_support;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{assert_envelope, config, send};
use ferry_core::app;
use ferry_core::payments::Payments;
use ferry_core::payments::ledger::keys;
use ferry_core::state::AppState;
use payments_support::*;
use serde_json::{Value, json};
use sqlx::PgPool;

fn state(pool: &PgPool, demo_mode: bool) -> AppState {
    AppState::new(
        pool.clone(),
        config(&[("DEMO_MODE", if demo_mode { "true" } else { "false" })]),
    )
    .unwrap()
}

fn get_wallet(state: &AppState, user: &str) -> Request<Body> {
    let token = state.jwt.issue_token(user).unwrap();
    Request::get("/v1/wallet")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

fn topup(state: &AppState, user: &str, body: &str) -> Request<Body> {
    let token = state.jwt.issue_token(user).unwrap();
    Request::post("/v1/wallet/demo-topup")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

/// Kinds and amounts, newest first.
fn txns(wallet: &Value) -> Vec<(String, i64)> {
    wallet["transactions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["kind"].as_str().unwrap().to_owned(),
                t["amount_cents"].as_i64().unwrap(),
            )
        })
        .collect()
}

#[sqlx::test(migrations = "../migrations")]
async fn wallets_after_a_run_match_the_contract_and_sign_from_the_viewer(pool: PgPool) {
    seed_people(&pool).await;
    let state = state(&pool, true);
    let payments = Payments::from_state(&state);
    assert_eq!(payments.wallet_mode(), "nessie_mock");
    payments.seed_platform(SEED_CENTS).await.unwrap();
    for user in ["usr_judge", "usr_dana"] {
        payments
            .demo_deposit(user, 5000, &keys::seed_deposit(user))
            .await
            .unwrap();
    }
    create_run(&pool, "run_w").await;
    // judge: 1249 + 94 + 150 + 200 = 1693 (card share 1343); dana: 899 + 67 + 150 = 1116 (966).
    place_order(&payments, "run_w", "ord_w_judge", "usr_judge", 1249, 200)
        .await
        .unwrap();
    place_order(&payments, "run_w", "ord_w_dana", "usr_dana", 899, 0)
        .await
        .unwrap();
    payments.fund_card("run_w").await.unwrap(); // 2309
    payments.card_purchase("run_w", 2209).await.unwrap(); // 100 back: 58 and 42
    payments.payout("ord_w_judge").await.unwrap(); // 350
    payments.payout("ord_w_dana").await.unwrap(); // 150

    let app = app::router(state.clone());
    let (status, _, judge) = send(app.clone(), get_wallet(&state, "usr_judge")).await;
    assert_eq!(status, StatusCode::OK);
    assert_contract("Wallet", &judge);
    assert_eq!(judge["mode"], "nessie_mock");
    assert_eq!(judge["balance_cents"], 5000 - 1693 + 58);
    assert_eq!(
        txns(&judge),
        [
            ("refund".into(), 58),
            ("order_hold".into(), -1693),
            ("deposit".into(), 5000)
        ]
    );
    let sum: i64 = txns(&judge).iter().map(|(_, a)| a).sum();
    assert_eq!(sum, judge["balance_cents"].as_i64().unwrap());
    let [refund, hold, deposit] = judge["transactions"].as_array().unwrap().as_slice() else {
        panic!()
    };
    assert_eq!(hold["order_id"], "ord_w_judge");
    assert_eq!(hold["run_id"], "run_w");
    assert_eq!(
        hold["description"],
        "Order from Cosmic Cantina, held until delivery"
    );
    assert_eq!(
        refund["description"],
        "Refund: Cosmic Cantina's receipt came in lower"
    );
    assert!(deposit.get("run_id").is_none() && deposit.get("order_id").is_none());
    assert!(hold["id"].as_str().unwrap().starts_with("txn_"));

    // The runner sees their checking and their Ferry Card.
    let (_, _, maya) = send(app.clone(), get_wallet(&state, RUNNER)).await;
    assert_contract("Wallet", &maya);
    assert_eq!(maya["balance_cents"], 350 + 150);
    let mut kinds = txns(&maya);
    kinds.sort();
    assert_eq!(
        kinds,
        [
            ("card_funding".into(), 2309),
            ("card_purchase".into(), -2209),
            ("payout".into(), 150),
            ("payout".into(), 350),
            ("refund".into(), -100),
        ]
    );

    // Someone with no money yet has an empty wallet.
    let (_, _, eve) = send(app, get_wallet(&state, "usr_eve")).await;
    assert_contract("Wallet", &eve);
    assert_eq!(eve["balance_cents"], 0);
    assert_eq!(eve["transactions"], json!([]));
}

#[sqlx::test(migrations = "../migrations")]
async fn demo_topup_in_demo_mode(pool: PgPool) {
    seed_people(&pool).await;
    let state = state(&pool, true);
    let app = app::router(state.clone());

    let (status, _, wallet) = send(
        app.clone(),
        topup(&state, "usr_judge", r#"{"amount_cents": 2500}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{wallet}");
    assert_contract("Wallet", &wallet);
    assert_eq!(wallet["balance_cents"], 2500);
    assert_eq!(wallet["transactions"][0]["kind"], "deposit");
    assert_eq!(wallet["transactions"][0]["amount_cents"], 2500);
    assert_eq!(wallet["transactions"][0]["description"], "Demo top-up");

    // The largest top-up, then a second one: each is its own deposit.
    let (status, _, wallet) = send(
        app.clone(),
        topup(&state, "usr_judge", r#"{"amount_cents": 100000}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(wallet["balance_cents"], 102500);
    assert_eq!(wallet["transactions"].as_array().unwrap().len(), 2);

    for body in [
        r#"{"amount_cents": 0}"#,
        r#"{"amount_cents": -5}"#,
        r#"{"amount_cents": 100001}"#,
        r#"{"amount_cents": 12.5}"#,
        r#"{"amount_cents": "100"}"#,
        r#"{}"#,
        "not json",
    ] {
        let (status, _, error) = send(app.clone(), topup(&state, "usr_judge", body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {error}");
        assert_envelope(&error, "validation_failed");
        assert_contract("Error", &error);
    }
    assert_eq!(
        checking(&pool, "usr_judge").await,
        102500,
        "no partial top-ups"
    );

    // No token: 401. A token for a user who does not exist: 401 too (see the test below).
    let anonymous = Request::post("/v1/wallet/demo-topup")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"amount_cents": 100}"#))
        .unwrap();
    let (status, _, error) = send(app.clone(), anonymous).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_envelope(&error, "unauthorized");
    let (status, _, error) = send(
        app.clone(),
        topup(&state, "usr_ghost", r#"{"amount_cents": 100}"#),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_envelope(&error, "unauthorized");
    let (status, _, error) =
        send(app, Request::get("/v1/wallet").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_envelope(&error, "unauthorized");
}

#[sqlx::test(migrations = "../migrations")]
async fn both_wallet_endpoints_answer_an_unknown_user_alike(pool: PgPool) {
    seed_people(&pool).await;
    let state = state(&pool, true);
    let app = app::router(state.clone());
    // A valid token for a user who does not exist (say, deleted): 401 from both, as from /v1/me,
    // so the client signs in again.
    let (get_status, _, get_error) = send(app.clone(), get_wallet(&state, "usr_ghost")).await;
    let (topup_status, _, topup_error) = send(
        app.clone(),
        topup(&state, "usr_ghost", r#"{"amount_cents": 100}"#),
    )
    .await;
    for (status, error) in [(get_status, &get_error), (topup_status, &topup_error)] {
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{error}");
        assert_envelope(error, "unauthorized");
        assert_contract("Error", error);
    }
    assert_eq!(get_error, topup_error, "the same answer");
    assert_eq!(row_count(&pool).await, 0, "nothing written");
    // The same request for a user who exists works.
    let (status, _, wallet) = send(app, get_wallet(&state, "usr_judge")).await;
    assert_eq!(status, StatusCode::OK);
    assert_contract("Wallet", &wallet);
}

/// The user's top-up deposits of `amount`.
async fn topups_of(pool: &PgPool, user: &str, amount: i64) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM ferry.ledger_entries WHERE idempotency_key LIKE $1 AND amount_cents = $2",
    )
    .bind(format!("deposit:{user}:topup:%"))
    .bind(amount)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrations = "../migrations")]
async fn a_repeated_topup_within_seconds_deposits_once(pool: PgPool) {
    seed_people(&pool).await;
    let state = state(&pool, true);
    let app = app::router(state.clone());

    // A double tap: the same amount twice deposits once, and both answers are the same wallet.
    let body = r#"{"amount_cents": 2500}"#;
    let (s1, _, first) = send(app.clone(), topup(&state, "usr_judge", body)).await;
    let (s2, _, second) = send(app.clone(), topup(&state, "usr_judge", body)).await;
    assert_eq!((s1, s2), (StatusCode::OK, StatusCode::OK));
    assert_contract("Wallet", &second);
    assert_eq!(first, second);
    assert_eq!(second["balance_cents"], 2500);
    assert_eq!(topups_of(&pool, "usr_judge", 2500).await, 1);

    // Ten at once, like retries racing each other: still one deposit, and every answer is 200.
    let mut tasks = Vec::new();
    for _ in 0..10 {
        let request = topup(&state, "usr_judge", r#"{"amount_cents": 7}"#);
        tasks.push(tokio::spawn(send(app.clone(), request)));
    }
    for task in tasks {
        let (status, _, wallet) = task.await.unwrap();
        assert_eq!(status, StatusCode::OK, "{wallet}");
        assert_contract("Wallet", &wallet);
    }
    assert_eq!(topups_of(&pool, "usr_judge", 7).await, 1);
    assert_eq!(checking(&pool, "usr_judge").await, 2507);

    // Another amount in the window is another top-up; so is the same amount by someone else.
    send(
        app.clone(),
        topup(&state, "usr_judge", r#"{"amount_cents": 1000}"#),
    )
    .await;
    send(app.clone(), topup(&state, "usr_dana", body)).await;
    assert_eq!(checking(&pool, "usr_judge").await, 3507);
    assert_eq!(checking(&pool, "usr_dana").await, 2500);

    // Once the window has passed, the same amount again is a new deposit.
    sqlx::query(
        "UPDATE ferry.ledger_entries SET created_at = created_at - make_interval(secs => $1::float8) \
         WHERE idempotency_key LIKE 'deposit:usr_judge:topup:%' AND amount_cents = 2500",
    )
    .bind(ferry_core::payments::ops::TOPUP_REPEAT_SECS + 1)
    .execute(&pool)
    .await
    .unwrap();
    let (status, _, wallet) = send(app, topup(&state, "usr_judge", body)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(wallet["balance_cents"], 6007);
    assert_eq!(topups_of(&pool, "usr_judge", 2500).await, 2);
    assert_conserved(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn demo_topup_is_forbidden_outside_demo_mode(pool: PgPool) {
    seed_people(&pool).await;
    let state = state(&pool, false);
    let (status, _, error) = send(
        app::router(state.clone()),
        topup(&state, "usr_judge", r#"{"amount_cents": 2500}"#),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_envelope(&error, "forbidden");
    assert_eq!(checking(&pool, "usr_judge").await, 0);
}

#[test]
fn the_contract_validator_rejects_bad_wallets() {
    let good = json!({
        "balance_cents": 100, "account_label": "x", "mode": "nessie_live",
        "transactions": [{"id": "txn_1", "kind": "payout", "amount_cents": 5, "description": "d",
                          "at": "2026-10-11T01:02:03.456Z", "run_id": "run_1"}]
    });
    assert!(contract_validator("Wallet").is_valid(&good));
    let mut no_mode = good.clone();
    no_mode.as_object_mut().unwrap().remove("mode");
    let mut bad_kind = good.clone();
    bad_kind["transactions"][0]["kind"] = json!("bonus");
    let mut bad_time = good.clone();
    bad_time["transactions"][0]["at"] = json!("yesterday");
    let mut float_cents = good.clone();
    float_cents["balance_cents"] = json!(1.5);
    for bad in [no_mode, bad_kind, bad_time, float_cents] {
        assert!(!contract_validator("Wallet").is_valid(&bad), "{bad}");
    }
}
