//! The money flow of spec section 4 against the local Postgres with mock Nessie: holds, card
//! funding at close, the three receipt cases, payouts and cancels, with every balance checked
//! after each step; idempotency, concurrency, the 402 envelope, retries and the 3 s cap.
mod common;
mod payments_support;

use std::time::{Duration, Instant};

use axum::response::IntoResponse;
use ferry_core::payments::ledger::{accounts, keys};
use ferry_core::payments::nessie::NessieApi;
use ferry_core::payments::{math, ops, poster};
use http_body_util::BodyExt;
use payments_support::*;
use serde_json::Value;
use sqlx::PgPool;

// Three orders at 750 bps tax and a 150 cent fee:
//   judge: 1249 + tax 94 (93.675) + 150 + tip 200 = 1693, card share 1343
//   dana:   899 + tax 67 (67.425) + 150 + tip   0 = 1116, card share  966
//   sam:   1500 + tax 113 (112.5, half up) + 150 + tip 100 = 1863, card share 1613
// Funded at close: 1343 + 966 + 1613 = 3922.
const JUDGE: (&str, i64, i64) = ("usr_judge", 1249, 200);
const DANA: (&str, i64, i64) = ("usr_dana", 899, 0);
const SAM: (&str, i64, i64) = ("usr_sam", 1500, 100);
const START: i64 = 5000;

async fn setup(
    pool: &PgPool,
) -> (
    ferry_core::payments::Payments,
    std::sync::Arc<ferry_core::payments::nessie::MockNessie>,
) {
    seed_people(pool).await;
    let (payments, mock) = payments(pool);
    payments.seed_platform(SEED_CENTS).await.unwrap();
    for user in CUSTOMERS {
        payments
            .demo_deposit(user, START, &keys::seed_deposit(user))
            .await
            .unwrap();
    }
    (payments, mock)
}

/// Places the three orders on `run_id` as ord_<run suffix>_<customer>.
async fn three_orders(payments: &ferry_core::payments::Payments, run_id: &str) -> [String; 3] {
    let mut ids = Vec::new();
    for (user, subtotal, tip) in [JUDGE, DANA, SAM] {
        let id = format!(
            "ord_{}_{}",
            run_id.trim_start_matches("run_"),
            user.trim_start_matches("usr_")
        );
        place_order(payments, run_id, &id, user, subtotal, tip)
            .await
            .unwrap();
        ids.push(id);
    }
    ids.try_into().unwrap()
}

#[sqlx::test(migrations = "../migrations")]
async fn three_orders_receipt_lower_cancel_before_close_and_payouts(pool: PgPool) {
    let (payments, mock) = setup(&pool).await;
    assert_eq!(escrow(&pool).await, SEED_CENTS);
    create_run(&pool, "run_low").await;

    // Holds: each customer to Escrow for the order total.
    let [judge, dana, sam] = three_orders(&payments, "run_low").await;
    assert_eq!(checking(&pool, "usr_judge").await, START - 1693);
    assert_eq!(checking(&pool, "usr_dana").await, START - 1116);
    assert_eq!(checking(&pool, "usr_sam").await, START - 1863);
    assert_eq!(escrow(&pool).await, SEED_CENTS + 1693 + 1116 + 1863);

    // A fourth order, cancelled before close: the full hold comes back, nothing else moves.
    let eve = "ord_low_eve";
    place_order(&payments, "run_low", eve, "usr_eve", 700, 50)
        .await
        .unwrap();
    let eve_total = 700 + 53 + 150 + 50; // tax 52.5 → 53
    assert_eq!(checking(&pool, "usr_eve").await, START - eve_total);
    set_order_status(&pool, eve, "cancelled").await;
    let refund = payments.refund_cancelled(eve).await.unwrap();
    assert_eq!(refund.len(), 1, "no card funding yet, so no card return");
    assert_eq!(refund[0].amount_cents, eve_total, "refunds equal holds");
    assert_eq!(checking(&pool, "usr_eve").await, START);
    assert_eq!(escrow(&pool).await, SEED_CENTS + 1693 + 1116 + 1863);

    // Close: Escrow funds the card with exactly subtotal + tax of the live orders.
    set_run_status(&pool, "run_low", "closed").await;
    let funding = payments.fund_card("run_low").await.unwrap();
    assert_eq!(funding[0].amount_cents, 3922);
    assert_eq!(card(&pool).await, 3922);
    assert_eq!(escrow(&pool).await, SEED_CENTS + 1693 + 1116 + 1863 - 3922);
    // No hold after the card is funded.
    let late = place_order(&payments, "run_low", "ord_low_late", "usr_eve", 100, 0).await;
    assert_eq!(
        late.unwrap_err().code,
        ferry_core::error::ErrorCode::WindowClosed
    );

    // Receipt lower: 3800. The card returns 122 and the customers get 122 pro rata.
    let rows = payments.card_purchase("run_low", 3800).await.unwrap();
    let keys_written: Vec<&str> = rows.iter().map(|r| r.idempotency_key.as_str()).collect();
    assert_eq!(
        keys_written,
        [
            "card_purchase:run_low",
            "card_return:run_low",
            // Refunds go in order id order, the order the orders are locked in.
            &format!("refund:{dana}:receipt"),
            &format!("refund:{judge}:receipt"),
            &format!("refund:{sam}:receipt"),
        ]
    );
    // 122 over 1343 : 966 : 1613 → 41.78, 30.05, 50.17: floors 41, 30, 50 and the leftover
    // cent to the largest remainder (judge).
    let shares = math::split_pro_rata(122, &[("j", 1343), ("d", 966), ("s", 1613)]).unwrap();
    assert_eq!(shares, vec![("j", 42), ("d", 30), ("s", 50)]);
    assert_eq!(merchant(&pool).await, 3800);
    assert_eq!(card(&pool).await, 0);
    assert_eq!(checking(&pool, "usr_judge").await, START - 1693 + 42);
    assert_eq!(checking(&pool, "usr_dana").await, START - 1116 + 30);
    assert_eq!(checking(&pool, "usr_sam").await, START - 1863 + 50);
    assert_eq!(escrow(&pool).await, SEED_CENTS + 350 + 150 + 250);
    // After the purchase there is no automatic refund.
    let late_cancel = payments.refund_cancelled(&sam).await.unwrap_err();
    assert_eq!(late_cancel.code, ferry_core::error::ErrorCode::Conflict);

    // Handoffs: fee + tip to the runner's checking. Escrow ends exactly at the platform seed.
    for (order, fee_tip) in [(&judge, 350), (&dana, 150), (&sam, 250)] {
        set_order_status(&pool, order, "settled").await;
        let payout = payments.payout(order).await.unwrap();
        assert_eq!(payout[0].amount_cents, fee_tip);
    }
    assert_eq!(checking(&pool, RUNNER).await, 750);
    assert_eq!(escrow(&pool).await, SEED_CENTS);
    assert_conserved(&pool).await;

    // Every row reached (mock) Nessie exactly once.
    let unposted: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ferry.ledger_entries WHERE nessie_object_id IS NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(unposted, 0);
    assert_eq!(mock.post_count() as i64, row_count(&pool).await);
    let purchase = mock.posts_matching("card_purchase:run_low");
    assert_eq!(purchase.len(), 1);
    assert!(
        purchase[0].merchant_id.is_some(),
        "a purchase at the merchant"
    );
    let links: i64 = sqlx::query_scalar("SELECT count(*) FROM ferry.nessie_links")
        .fetch_one(&pool)
        .await
        .unwrap();
    // platform customer + escrow, 4 customers + 4 checkings, runner customer + checking, card, merchant.
    assert_eq!(links, 2 + 8 + 2 + 1 + 1);
}

#[sqlx::test(migrations = "../migrations")]
async fn receipt_higher_within_five_percent_and_over_five_percent_refused(pool: PgPool) {
    let (payments, _mock) = setup(&pool).await;
    create_run(&pool, "run_high").await;
    let [judge, dana, sam] = three_orders(&payments, "run_high").await;
    payments.fund_card("run_high").await.unwrap();
    let escrow_after_close = SEED_CENTS + 1693 + 1116 + 1863 - 3922;
    assert_eq!(escrow(&pool).await, escrow_after_close);

    // The limit is 5% of 3922 rounded down: 196, so the most is 4118.
    assert_eq!(math::max_receipt_cents(3922), 4118);
    let rows_before = row_count(&pool).await;
    let err = payments.card_purchase("run_high", 4119).await.unwrap_err();
    assert_eq!(err.code, ferry_core::error::ErrorCode::Conflict);
    assert_eq!(err.status.as_u16(), 409);
    assert_eq!(
        err.details,
        Some(serde_json::json!({"max_receipt_cents": 4118}))
    );
    assert_eq!(row_count(&pool).await, rows_before, "nothing moves");
    assert_eq!(escrow(&pool).await, escrow_after_close);
    assert_eq!(card(&pool).await, 3922);

    // 4100: Escrow covers the 178 overage first, then the card pays 4100. No refunds.
    let rows = payments.card_purchase("run_high", 4100).await.unwrap();
    let written: Vec<(&str, &str, i64)> = rows
        .iter()
        .map(|r| (r.idempotency_key.as_str(), r.kind.as_str(), r.amount_cents))
        .collect();
    assert_eq!(
        written,
        [
            ("overage:run_high", "card_funding", 178),
            ("card_purchase:run_high", "card_purchase", 4100),
        ]
    );
    assert_eq!(card(&pool).await, 0);
    assert_eq!(merchant(&pool).await, 4100);
    assert_eq!(escrow(&pool).await, escrow_after_close - 178);
    for user in ["usr_judge", "usr_dana", "usr_sam"] {
        let total = match user {
            "usr_judge" => 1693,
            "usr_dana" => 1116,
            _ => 1863,
        };
        assert_eq!(
            checking(&pool, user).await,
            START - total,
            "no refund when higher"
        );
    }
    for order in [&judge, &dana, &sam] {
        payments.payout(order).await.unwrap();
    }
    assert_eq!(checking(&pool, RUNNER).await, 750);
    // The platform paid the overage and nothing else.
    assert_eq!(escrow(&pool).await, SEED_CENTS - 178);
    assert_conserved(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn cancel_after_close_returns_the_card_share(pool: PgPool) {
    let (payments, _mock) = setup(&pool).await;
    create_run(&pool, "run_cxl").await;
    let [judge, dana, sam] = three_orders(&payments, "run_cxl").await;
    payments.fund_card("run_cxl").await.unwrap();
    assert_eq!(card(&pool).await, 3922);

    // Dana cancels after close, before the purchase: her 966 comes off the card first, then
    // her whole 1116 hold goes back to her.
    let rows = payments.refund_cancelled(&dana).await.unwrap();
    let written: Vec<(&str, i64)> = rows
        .iter()
        .map(|r| (r.idempotency_key.as_str(), r.amount_cents))
        .collect();
    assert_eq!(
        written,
        [
            (format!("card_return:run_cxl:{dana}").as_str(), 966),
            (format!("refund:{dana}:cancel").as_str(), 1116),
        ]
    );
    assert_eq!(card(&pool).await, 3922 - 966);
    assert_eq!(checking(&pool, "usr_dana").await, START);

    // The purchase is measured against the live orders only: 1343 + 1613 = 2956.
    assert_eq!(
        payments
            .card_purchase("run_cxl", 3105)
            .await
            .unwrap_err()
            .details,
        Some(serde_json::json!({"max_receipt_cents": 2956 + 147}))
    );
    payments.card_purchase("run_cxl", 2900).await.unwrap();
    assert_eq!(card(&pool).await, 0);
    // 56 back, split 1343 : 1613 → 25.44, 30.56 → 25 and 31.
    assert_eq!(checking(&pool, "usr_judge").await, START - 1693 + 25);
    assert_eq!(checking(&pool, "usr_sam").await, START - 1863 + 31);
    // A cancelled order gets no payout.
    assert!(payments.payout(&dana).await.is_err());
    payments.payout(&judge).await.unwrap();
    payments.payout(&sam).await.unwrap();
    assert_eq!(checking(&pool, RUNNER).await, 350 + 250);
    assert_eq!(escrow(&pool).await, SEED_CENTS);
    assert_conserved(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn every_op_called_twice_posts_once(pool: PgPool) {
    let (payments, mock) = setup(&pool).await;
    create_run(&pool, "run_twice").await;
    let [judge, dana, sam] = three_orders(&payments, "run_twice").await;
    place_order(&payments, "run_twice", "ord_twice_eve", "usr_eve", 300, 0)
        .await
        .unwrap();

    // Each op a second time, through both entry points: the same rows come back.
    let again = ops_twice(&payments, |p| async move {
        let mut tx = p.pool().begin().await.unwrap();
        let rows = ops::hold_order_in(&mut tx, "ord_twice_eve").await.unwrap();
        tx.commit().await.unwrap();
        rows
    })
    .await;
    assert_eq!(again.len(), 1);
    assert_eq!(
        payments.hold_order(&judge).await.unwrap()[0].idempotency_key,
        format!("order_hold:{judge}")
    );

    let cancel_1 = payments.refund_cancelled("ord_twice_eve").await.unwrap();
    let cancel_2 = payments.refund_cancelled("ord_twice_eve").await.unwrap();
    assert_eq!(ids(&cancel_1), ids(&cancel_2));

    let fund_1 = payments.fund_card("run_twice").await.unwrap();
    let fund_2 = payments.fund_card("run_twice").await.unwrap();
    assert_eq!(ids(&fund_1), ids(&fund_2));

    // 3921 is one cent under: the cent goes to one customer and the other shares are zero,
    // which write no row.
    let buy_1 = payments.card_purchase("run_twice", 3921).await.unwrap();
    let buy_2 = payments.card_purchase("run_twice", 3921).await.unwrap();
    assert_eq!(ids(&buy_1), ids(&buy_2));
    assert_eq!(
        buy_1.len(),
        3,
        "purchase, card return, one refund: {buy_1:#?}"
    );
    // Another receipt for the same run is a conflict, not a second purchase.
    assert_eq!(
        payments
            .card_purchase("run_twice", 3000)
            .await
            .unwrap_err()
            .code,
        ferry_core::error::ErrorCode::Conflict
    );

    for order in [&judge, &dana, &sam] {
        let p1 = payments.payout(order).await.unwrap();
        let p2 = payments.payout(order).await.unwrap();
        assert_eq!(ids(&p1), ids(&p2));
    }
    let d1 = payments
        .demo_deposit("usr_judge", 500, "deposit:usr_judge:once")
        .await
        .unwrap();
    let d2 = payments
        .demo_deposit("usr_judge", 500, "deposit:usr_judge:once")
        .await
        .unwrap();
    assert_eq!(ids(&d1), ids(&d2));
    let s2 = payments.seed_platform(SEED_CENTS).await.unwrap();
    assert_eq!(s2[0].idempotency_key, keys::PLATFORM_SEED);

    // One row per key, one Nessie post per row.
    let dupes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM (SELECT idempotency_key FROM ferry.ledger_entries \
         GROUP BY idempotency_key HAVING count(*) > 1) d",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(dupes, 0);
    let all: Vec<(String,)> = sqlx::query_as("SELECT id FROM ferry.ledger_entries")
        .fetch_all(&pool)
        .await
        .unwrap();
    for (id,) in &all {
        assert_eq!(
            mock.posts_matching(&format!("Ferry {id} ")).len(),
            1,
            "{id}"
        );
    }
    assert_eq!(mock.post_count(), all.len());
    assert_eq!(
        payments.retry_due().await.unwrap(),
        0,
        "nothing left to post"
    );
    assert_conserved(&pool).await;
}

fn ids(rows: &[ferry_core::payments::LedgerEntry]) -> Vec<String> {
    rows.iter().map(|r| r.id.clone()).collect()
}

/// Runs `op` twice and checks it returned the same rows both times.
async fn ops_twice<F, Fut>(
    payments: &ferry_core::payments::Payments,
    op: F,
) -> Vec<ferry_core::payments::LedgerEntry>
where
    F: Fn(ferry_core::payments::Payments) -> Fut,
    Fut: std::future::Future<Output = Vec<ferry_core::payments::LedgerEntry>>,
{
    let first = op(payments.clone()).await;
    let second = op(payments.clone()).await;
    assert_eq!(ids(&first), ids(&second));
    second
}

#[sqlx::test(migrations = "../migrations")]
async fn concurrent_holds_never_take_a_balance_below_zero(pool: PgPool) {
    seed_people(&pool).await;
    let (payments, _mock) = payments(&pool);
    payments
        .demo_deposit("usr_judge", 1000, "deposit:usr_judge:seed")
        .await
        .unwrap();
    // Eight runs, one order each, all at once: 100 + tax 8 + fee 150 = 258, so three fit in
    // 1000 (774) and the fourth would need 1032.
    for i in 0..8 {
        create_run(&pool, &format!("run_c{i}")).await;
    }
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = Vec::new();
    for i in 0..8 {
        let (payments, barrier) = (payments.clone(), barrier.clone());
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            place_order(
                &payments,
                &format!("run_c{i}"),
                &format!("ord_c{i}"),
                "usr_judge",
                100,
                0,
            )
            .await
        }));
    }
    let mut ok = 0;
    let mut refused = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => ok += 1,
            Err(err) => {
                assert_eq!(err.code, ferry_core::error::ErrorCode::InsufficientFunds);
                refused += 1;
            }
        }
    }
    assert_eq!((ok, refused), (3, 5));
    assert_eq!(checking(&pool, "usr_judge").await, 1000 - 3 * 258);
    // A refused hold rolled its order back too.
    let orders: i64 = sqlx::query_scalar("SELECT count(*) FROM ferry.orders")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(orders, 3);
    assert_conserved(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn insufficient_funds_is_402_in_the_contract_envelope(pool: PgPool) {
    seed_people(&pool).await;
    let (payments, _mock) = payments(&pool);
    payments
        .demo_deposit("usr_judge", 500, "deposit:usr_judge:seed")
        .await
        .unwrap();
    create_run(&pool, "run_poor").await;
    let rows_before = row_count(&pool).await;
    let err = place_order(&payments, "run_poor", "ord_poor", "usr_judge", 1249, 200)
        .await
        .unwrap_err();
    assert_eq!(row_count(&pool).await, rows_before, "nothing written");
    let response = err.into_response();
    assert_eq!(response.status().as_u16(), 402);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    common::assert_envelope(&body, "insufficient_funds");
    assert_eq!(
        body["error"]["details"],
        serde_json::json!({"balance_cents": 500, "total_cents": 1693})
    );
    assert_contract("Error", &body);
}

/// The row's `nessie_status` once no poster holds it. The request path stops waiting after
/// `poster::USER_WAIT` and the post finishes in the background; on a loaded machine that can
/// take longer (provisioning is one at a time per process, shared by every test in this binary).
async fn settled_status(pool: &PgPool, id: &str) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let (object_id, status) = nessie_state(pool, id).await;
        let held = status.as_deref().is_some_and(|s| s.starts_with("posting:"));
        if object_id.is_some() || !held {
            return status;
        }
        assert!(Instant::now() < deadline, "{id} stayed {status:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn a_nessie_failure_stays_pending_and_one_retry_posts_once(pool: PgPool) {
    seed_people(&pool).await;
    let (payments, mock) = payments(&pool);
    mock.set_failing(true);
    let rows = payments
        .demo_deposit("usr_judge", 700, "deposit:usr_judge:seed")
        .await
        .unwrap();
    let row = &rows[0];
    assert!(row.nessie_object_id.is_none());
    let status = settled_status(&pool, &row.id).await.unwrap();
    assert!(
        status.starts_with("pending:1:") && status.ends_with(":mock_failure"),
        "{status}"
    );
    // The ledger moved anyway: Ferry's ledger is the truth, Nessie catches up.
    assert_eq!(checking(&pool, "usr_judge").await, 700);
    // Not due yet (backoff), and still failing.
    assert_eq!(payments.retry_due().await.unwrap(), 0);

    mock.set_failing(false);
    sqlx::query(
        "UPDATE ferry.ledger_entries SET nessie_status = 'pending:1:0:mock_failure' WHERE id = $1",
    )
    .bind(&row.id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(payments.retry_due().await.unwrap(), 1);
    assert_eq!(payments.retry_due().await.unwrap(), 0);
    let posted = mock.posts_matching(&format!("Ferry {} ", row.id));
    assert_eq!(posted.len(), 1);
    let (object_id, status): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT nessie_object_id, nessie_status FROM ferry.ledger_entries WHERE id = $1",
    )
    .bind(&row.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(object_id.as_deref(), Some(posted[0].id.as_str()));
    assert_eq!(status.as_deref(), Some("completed"));
}

#[sqlx::test(migrations = "../migrations")]
async fn a_lost_response_is_found_not_posted_again(pool: PgPool) {
    seed_people(&pool).await;
    let (payments, mock) = payments(&pool);
    // Write the row without posting, then post it straight to the mock as if the response had
    // been lost after Nessie accepted it.
    let mut tx = pool.begin().await.unwrap();
    let rows = ops::demo_deposit_in(&mut tx, "usr_dana", 900, "deposit:usr_dana:seed")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let row = &rows[0];
    let target = ferry_core::payments::provision::resolve(
        &pool,
        payments.nessie(),
        &accounts::checking("usr_dana"),
    )
    .await
    .unwrap();
    let ferry_core::payments::provision::Target::Account(account) = target else {
        panic!()
    };
    let lost = mock
        .deposit(
            &account,
            900,
            "2026-10-11",
            &format!("Ferry {} {}", row.id, row.idempotency_key),
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE ferry.ledger_entries SET nessie_status = 'pending:1:0:timeout' WHERE id = $1",
    )
    .bind(&row.id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(payments.retry_due().await.unwrap(), 1);
    assert_eq!(
        mock.posts_matching(&row.id).len(),
        1,
        "found, not posted again"
    );
    let object_id: Option<String> =
        sqlx::query_scalar("SELECT nessie_object_id FROM ferry.ledger_entries WHERE id = $1")
            .bind(&row.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(object_id, Some(lost.id));
}

#[sqlx::test(migrations = "../migrations")]
async fn a_slow_nessie_never_holds_the_response_past_three_seconds(pool: PgPool) {
    seed_people(&pool).await;
    let (payments, mock) = payments(&pool);
    // Provision first, so only the deposit itself is slow.
    ferry_core::payments::provision::resolve(
        &pool,
        payments.nessie(),
        &accounts::checking("usr_sam"),
    )
    .await
    .unwrap();
    mock.set_delay(Duration::from_secs(4));
    let started = Instant::now();
    let rows = payments
        .demo_deposit("usr_sam", 1200, "deposit:usr_sam:seed")
        .await
        .unwrap();
    let waited = started.elapsed();
    assert!(waited < Duration::from_secs(3), "waited {waited:?}");
    assert!(waited >= poster::USER_WAIT, "waited {waited:?}");
    assert!(rows[0].nessie_object_id.is_none());
    assert!(
        rows[0]
            .nessie_status
            .as_deref()
            .unwrap()
            .starts_with("posting:1:")
    );
    assert_eq!(
        checking(&pool, "usr_sam").await,
        1200,
        "the money moved at once"
    );

    // The post finishes in the background, once.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let posted: Option<String> =
            sqlx::query_scalar("SELECT nessie_object_id FROM ferry.ledger_entries WHERE id = $1")
                .bind(&rows[0].id)
                .fetch_one(&pool)
                .await
                .unwrap();
        if posted.is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the background post never finished"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(mock.posts_matching(&rows[0].id).len(), 1);
}

#[sqlx::test(migrations = "../migrations")]
async fn two_posters_never_post_the_same_row(pool: PgPool) {
    seed_people(&pool).await;
    let (payments, mock) = payments(&pool);
    mock.set_failing(true);
    let rows = payments
        .demo_deposit("usr_eve", 300, "deposit:usr_eve:seed")
        .await
        .unwrap();
    // The failed attempt is recorded before the mock recovers.
    let failed = settled_status(&pool, &rows[0].id).await.unwrap();
    assert!(failed.starts_with("pending:1:"), "{failed}");
    mock.set_failing(false);
    mock.set_delay(Duration::from_millis(300));
    let id = rows[0].id.clone();
    let (a, b) = tokio::join!(
        poster::post_one(&pool, payments.nessie(), &id, true),
        poster::post_one(&pool, payments.nessie(), &id, true),
    );
    let claimed = [a.unwrap(), b.unwrap()];
    assert_eq!(claimed.iter().filter(|c| **c).count(), 1, "{claimed:?}");
    assert_eq!(mock.posts_matching(&id).len(), 1);
}

/// Places an order that must be refused with 409 `window_closed`, in the contract envelope, and
/// writes nothing.
async fn assert_window_closed(
    payments: &ferry_core::payments::Payments,
    run_id: &str,
    order_id: &str,
) {
    let err = place_order(payments, run_id, order_id, "usr_judge", 100, 0)
        .await
        .unwrap_err();
    let response = err.into_response();
    assert_eq!(response.status().as_u16(), 409, "{run_id}");
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    common::assert_envelope(&body, "window_closed");
    assert_contract("Error", &body);
}

#[sqlx::test(migrations = "../migrations")]
async fn no_hold_on_a_run_that_is_not_open_even_with_no_orders(pool: PgPool) {
    let (payments, _mock) = setup(&pool).await;
    // Closed with no orders at all: the close funds nothing, so no card_funding row exists.
    create_run(&pool, "run_empty").await;
    set_run_status(&pool, "run_empty", "closed").await;
    assert!(payments.fund_card("run_empty").await.unwrap().is_empty());
    // Closed after its only order was cancelled: again no card_funding row.
    create_run(&pool, "run_gone").await;
    place_order(&payments, "run_gone", "ord_gone_eve", "usr_eve", 700, 0)
        .await
        .unwrap();
    set_order_status(&pool, "ord_gone_eve", "cancelled").await;
    payments.refund_cancelled("ord_gone_eve").await.unwrap();
    set_run_status(&pool, "run_gone", "closed").await;
    assert!(payments.fund_card("run_gone").await.unwrap().is_empty());
    // Every later status, never funded either.
    let later = ["purchased", "delivering", "delivered", "cancelled"];
    for status in later {
        let run = format!("run_{status}");
        create_run(&pool, &run).await;
        set_run_status(&pool, &run, status).await;
    }

    let rows_before = row_count(&pool).await;
    let runs = ["run_empty", "run_gone"]
        .into_iter()
        .map(str::to_owned)
        .chain(later.iter().map(|s| format!("run_{s}")));
    for run in runs {
        assert_window_closed(&payments, &run, &format!("ord_late_{run}")).await;
    }
    assert_eq!(row_count(&pool).await, rows_before, "nothing written");
    let late_orders: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ferry.orders WHERE id LIKE 'ord_late_%'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(late_orders, 0, "the refused orders rolled back");
    assert_eq!(checking(&pool, "usr_judge").await, START);

    // An order held while the run was open still returns its row after the close: a retry of a
    // hold that succeeded is not refused.
    create_run(&pool, "run_retry").await;
    let held = place_order(&payments, "run_retry", "ord_retry_dana", "usr_dana", 899, 0)
        .await
        .unwrap();
    set_run_status(&pool, "run_retry", "closed").await;
    let again = payments.hold_order("ord_retry_dana").await.unwrap();
    assert_eq!(ids(&again), ids(&held));
    assert_conserved(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn holds_racing_a_close_with_no_orders_never_land_after_it(pool: PgPool) {
    let (payments, _mock) = setup(&pool).await;
    for round in 0..6 {
        let run = format!("run_race{round}");
        create_run(&pool, &run).await;
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(9));
        // Set once the closer has committed.
        let (closed_tx, closed_rx) = tokio::sync::watch::channel(false);
        let mut holds = Vec::new();
        for i in 0..8 {
            let (payments, barrier, run) = (payments.clone(), barrier.clone(), run.clone());
            let mut closed = closed_rx.clone();
            let customer = CUSTOMERS[i % CUSTOMERS.len()];
            holds.push(tokio::spawn(async move {
                barrier.wait().await;
                if round % 2 == 0 {
                    // Even rounds: every hold starts after the close committed with no orders.
                    closed.wait_for(|done| *done).await.unwrap();
                } else {
                    // Odd rounds: the holds are staggered around the close.
                    let delay = (i as u64 * round) % 7;
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                place_order(
                    &payments,
                    &run,
                    &format!("ord_race{round}_{i}"),
                    customer,
                    100,
                    0,
                )
                .await
            }));
        }
        // The closer: lock the run, close it, fund the card, in one transaction.
        let closer = {
            let (payments, barrier, run) = (payments.clone(), barrier.clone(), run.clone());
            tokio::spawn(async move {
                barrier.wait().await;
                if round % 2 == 1 {
                    tokio::time::sleep(Duration::from_millis(round * 2)).await;
                }
                let mut tx = payments.pool().begin().await.unwrap();
                sqlx::query("SELECT id FROM ferry.runs WHERE id = $1 FOR UPDATE")
                    .bind(&run)
                    .execute(&mut *tx)
                    .await
                    .unwrap();
                sqlx::query("UPDATE ferry.runs SET status = 'closed' WHERE id = $1")
                    .bind(&run)
                    .execute(&mut *tx)
                    .await
                    .unwrap();
                let rows = ops::fund_card_in(&mut tx, &run).await.unwrap();
                tx.commit().await.unwrap();
                closed_tx.send_replace(true);
                rows
            })
        };
        let mut held = 0;
        for task in holds {
            match task.await.unwrap() {
                Ok(_) => held += 1,
                Err(err) => assert_eq!(err.code, ferry_core::error::ErrorCode::WindowClosed),
            }
        }
        let funding = closer.await.unwrap();
        // 100 + tax 8: every hold that landed is on the card, and none landed after the close.
        let funded: i64 = funding.iter().map(|r| r.amount_cents).sum();
        assert_eq!(funded, held * 108, "round {round}: {held} held");
        let holds_on_run: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM ferry.ledger_entries WHERE run_id = $1 AND kind = 'order_hold'",
        )
        .bind(&run)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(holds_on_run, held);
        if round % 2 == 0 {
            assert_eq!(held, 0, "round {round}: a hold landed after the close");
        }
    }
    assert_conserved(&pool).await;
}

/// The row's `nessie_object_id` and `nessie_status`.
async fn nessie_state(pool: &PgPool, id: &str) -> (Option<String>, Option<String>) {
    sqlx::query_as("SELECT nessie_object_id, nessie_status FROM ferry.ledger_entries WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../migrations")]
async fn a_timed_out_post_is_looked_up_before_it_is_posted_again(pool: PgPool) {
    seed_people(&pool).await;
    let (payments, mock) = payments(&pool);
    // Provision first, so only the deposit itself times out.
    ferry_core::payments::provision::resolve(
        &pool,
        payments.nessie(),
        &accounts::checking("usr_dana"),
    )
    .await
    .unwrap();
    // The deposit lands in Nessie, but the answer is lost, and Nessie will not list it for a while.
    mock.set_losing_responses(true);
    mock.set_list_lag(Duration::from_secs(3600));
    let key = keys::seed_deposit("usr_dana");
    let rows = payments.demo_deposit("usr_dana", 900, &key).await.unwrap();
    let row = &rows[0];
    assert_eq!(mock.posts_matching(&row.id).len(), 1, "it landed");
    assert!(row.nessie_object_id.is_none());
    let status = settled_status(&pool, &row.id).await.unwrap();
    let parts: Vec<&str> = status.split(':').collect();
    assert_eq!((parts[0], parts[1], parts[3]), ("unsure", "1", "timeout"));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let retry_at: i64 = parts[2].parse().unwrap();
    assert!(
        retry_at >= now + poster::RECONCILE_AFTER_SECS - 2,
        "{status} at {now}"
    );
    mock.set_losing_responses(false);

    // A client retrying the op (whose request path forces a post), a forced post and the sweep
    // all leave the row alone until the lookup can see the object.
    let again = payments.demo_deposit("usr_dana", 900, &key).await.unwrap();
    assert_eq!(again[0].id, row.id);
    assert_eq!(again[0].nessie_status.as_deref(), Some(status.as_str()));
    assert!(
        !poster::post_one(&pool, payments.nessie(), &row.id, true)
            .await
            .unwrap()
    );
    assert_eq!(payments.retry_due().await.unwrap(), 0);
    assert_eq!(
        mock.posts_matching(&row.id).len(),
        1,
        "not posted a second time"
    );

    // Once the wait is over and Nessie lists the deposit, the lookup records it.
    mock.set_list_lag(Duration::ZERO);
    sqlx::query(
        "UPDATE ferry.ledger_entries SET nessie_status = 'unsure:1:0:timeout' WHERE id = $1",
    )
    .bind(&row.id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(payments.retry_due().await.unwrap(), 1);
    let landed = mock.posts_matching(&row.id);
    assert_eq!(landed.len(), 1, "found, not posted again");
    let (object_id, status) = nessie_state(&pool, &row.id).await;
    assert_eq!(object_id, Some(landed[0].id.clone()));
    assert_eq!(status.as_deref(), Some("completed"));
    assert_eq!(checking(&pool, "usr_dana").await, 900);
}

#[sqlx::test(migrations = "../migrations")]
async fn one_bad_row_never_stops_the_sweep(pool: PgPool) {
    seed_people(&pool).await;
    let (payments, mock) = payments(&pool);
    // An older row whose status no claim can parse, and a younger row never posted.
    let mut tx = pool.begin().await.unwrap();
    let bad = ops::demo_deposit_in(&mut tx, "usr_judge", 100, "deposit:usr_judge:bad")
        .await
        .unwrap()
        .remove(0);
    let good = ops::demo_deposit_in(&mut tx, "usr_dana", 200, "deposit:usr_dana:good")
        .await
        .unwrap()
        .remove(0);
    tx.commit().await.unwrap();
    assert!(bad.created_at <= good.created_at);
    sqlx::query(
        "UPDATE ferry.ledger_entries SET nessie_status = 'posting:1:notanumber' WHERE id = $1",
    )
    .bind(&bad.id)
    .execute(&pool)
    .await
    .unwrap();

    // The sweep skips the bad row (and logs it) and posts the good one.
    assert_eq!(payments.retry_due().await.unwrap(), 1);
    let (object_id, status) = nessie_state(&pool, &good.id).await;
    assert!(object_id.is_some());
    assert_eq!(status.as_deref(), Some("completed"));
    assert_eq!(mock.posts_matching(&good.id).len(), 1);
    // The bad row is left as it was, for a person to look at.
    assert_eq!(
        nessie_state(&pool, &bad.id).await,
        (None, Some("posting:1:notanumber".to_owned()))
    );
    assert!(mock.posts_matching(&bad.id).is_empty());
    // The next sweep does the same and still does not fail.
    assert_eq!(payments.retry_due().await.unwrap(), 0);
}
