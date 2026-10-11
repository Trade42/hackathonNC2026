//! Money and the agent, after PR #22's review: no order is placed or cancelled without the user's
//! own tap (BLOCKER 1, its four repros a-d), placing is claimed so it never doubles or starves the
//! pool (MAJOR 2: five people at once, eight presses on one draft, a failure after the commit),
//! "cancel my order" offers the candidates with buttons (MINOR 2), and an edit that loses the race
//! to Place order changes nothing (MINOR 4). Round 2: a refused press is never reported as placed
//! with another draft's order (its MAJOR), and a second claim of the same user on the same run
//! never takes the first one's order (its MINOR 5). Through `/internal/messages/inbound`, a stub sidecar
//! and a stub Gemini; every Gemini body is scanned for names when each test's app is dropped.
mod agent_support;
mod common;

use std::time::{Duration, Instant};

use agent_support::{
    DANA_TG, DANA_USERNAME, GROUP_ID, JUDGE_TG, JUDGE_USERNAME, TestApp, app, call_reply,
    calls_reply, dm, group_message, inbound, map_demo_group, press, seed_demo, text_reply,
};
use ferry_core::agent::ordering::{self, Draft, EDIT_TOO_LATE};
use ferry_core::error::ApiError;
use ferry_core::orders::{self, CreateOrderRequest};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// The judge's draft line for "chicken burrito no beans" on run_demo, in a DM.
const JUDGE_DRAFT: &str = "Chicken Burrito (No beans) from Cosmic Cantina: $8.99 + tax $0.67 + Ferry fee $1.50 + tip $2.00 = $13.16, to Fetzer Gym (HackNC). Place the order?";
const PLACED: &str =
    "Order placed: $13.16 is held from your Ferry balance. Maya brings it to Fetzer Gym (HackNC).";

fn text(sent: &Value) -> &str {
    sent["text"].as_str().unwrap()
}

fn labels(sent: &Value) -> Vec<String> {
    sent["buttons"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|b| b["label"].as_str().unwrap().to_owned())
        .collect()
}

fn button_data(sent: &Value, label: &str) -> String {
    sent["buttons"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|b| b["label"] == label)
        .and_then(|b| b["callback_data"].as_str())
        .unwrap_or_else(|| panic!("no {label} button: {sent}"))
        .to_owned()
}

/// The demo with money: the personas' starting balances (the judge has $50, Dana $40).
async fn seed_with_money(pool: &PgPool) {
    seed_demo(pool).await;
    map_demo_group(pool).await;
    ferry_core::payments::seed_demo_balances(pool)
        .await
        .unwrap();
}

/// Orders placed since the seed, and the escrow holds written for them.
async fn placed(pool: &PgPool) -> (i64, i64) {
    let orders: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ferry.orders WHERE NOT is_demo_history")
            .fetch_one(pool)
            .await
            .unwrap();
    let holds: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ferry.ledger_entries WHERE kind = 'order_hold' AND order_id IS NOT NULL",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    (orders, holds)
}

async fn draft_statuses(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar("SELECT status FROM ferry.order_drafts ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

/// Waits until every claimed turn has finished (its row has a latency).
async fn turns_finished(pool: &PgPool) {
    for _ in 0..500 {
        let pending: i64 =
            sqlx::query_scalar("SELECT count(*) FROM ferry.agent_turns WHERE latency_ms IS NULL")
                .fetch_one(pool)
                .await
                .unwrap();
        if pending == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("agent turns still running");
}

/// Waits up to `seconds` for the stub to hold `count` sends.
async fn wait_long(app: &TestApp, count: usize, seconds: u64) -> Vec<Value> {
    for _ in 0..seconds * 50 {
        let sent = app.sidecar.sent();
        if sent.len() >= count {
            return sent.into_iter().map(|s| s.body).collect();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "the stub got {} sends, not {count}: {:#?}",
        app.sidecar.sent().len(),
        app.sidecar.sent()
    );
}

fn judge_dm(words: &str) -> Value {
    dm(JUDGE_TG, Some(JUDGE_USERNAME), "Jordan", words)
}

fn judge_press(space_id: &str, data: &str) -> Value {
    press(
        space_id,
        space_id == GROUP_ID,
        JUDGE_TG,
        Some(JUDGE_USERNAME),
        "Jordan",
        data,
    )
}

/// A Gemini that calls `first` on the user's message, then each tool in `then` after the previous
/// tool's result, then answers `last` in words.
fn gemini_calls(app: &TestApp, steps: Vec<(&'static str, Value)>, last: &'static str) {
    app.gemini.respond_with(move |body| {
        let contents = body["contents"].as_array().unwrap();
        let done = contents
            .iter()
            .filter(|c| c["parts"][0].get("functionResponse").is_some())
            .count();
        match steps.get(done) {
            Some((tool, args)) => (200, call_reply(tool, args.clone())),
            None => (200, text_reply(last)),
        }
    });
}

/// The judge's draft in the DM, through the fallback (Gemini off) or Gemini's draft tool.
async fn judge_drafts_in_dm(app: &TestApp, already_sent: usize) -> String {
    inbound(app, &judge_dm("chicken burrito no beans")).await;
    let sent = app.sidecar.wait_for(already_sent + 1).await;
    let draft = &sent[already_sent].body;
    assert_eq!(text(draft), JUDGE_DRAFT);
    button_data(draft, "Place order")
}

// ---------- BLOCKER 1 ----------

/// BLOCKER 1a: Gemini calls draft_order_from_text and then confirm_order in one turn. The user sees
/// the total with Place order and Cancel; nothing is placed and nothing is held.
#[sqlx::test(migrations = "../migrations")]
async fn draft_and_confirm_in_one_turn_place_nothing(pool: PgPool) {
    seed_with_money(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    gemini_calls(
        &app,
        vec![
            (
                "draft_order_from_text",
                json!({"text": "chicken burrito no beans"}),
            ),
            ("confirm_order", json!({})),
        ],
        "Done.",
    );
    inbound(&app, &judge_dm("chicken burrito no beans")).await;
    let sent = app.sidecar.wait_for(1).await;
    app.sidecar.assert_no_more_than(1).await;
    assert_eq!(text(&sent[0].body), JUDGE_DRAFT);
    assert_eq!(labels(&sent[0].body), ["Place order", "Cancel"]);
    turns_finished(&pool).await;
    assert_eq!(placed(&pool).await, (0, 0), "nothing placed, nothing held");
    assert_eq!(draft_statuses(&pool).await, ["pending"]);
    // Gemini was told the draft still needs the tap.
    let results = app.gemini.function_responses(2);
    assert_eq!(results[1].0, "confirm_order");
    assert_eq!(results[1].1["placed"], false);
    assert_eq!(results[1].1["needs_tap"], true);

    // The same in one parallel round.
    app.gemini.respond_with(|body| {
        let contents = body["contents"].as_array().unwrap();
        if contents
            .last()
            .is_some_and(|c| c["parts"][0].get("functionResponse").is_some())
        {
            (200, text_reply("Your order is placed!"))
        } else {
            (
                200,
                calls_reply(&[
                    (
                        "draft_order_from_text",
                        json!({"text": "chicken burrito no beans"}),
                    ),
                    ("confirm_order", json!({})),
                ]),
            )
        }
    });
    inbound(&app, &judge_dm("one more chicken burrito no beans")).await;
    let sent = app.sidecar.wait_for(2).await;
    assert_eq!(text(&sent[1].body), JUDGE_DRAFT);
    assert_eq!(labels(&sent[1].body), ["Place order", "Cancel"]);
    turns_finished(&pool).await;
    assert_eq!(placed(&pool).await, (0, 0));
}

/// BLOCKER 1b: after a draft, an unrelated message on which Gemini calls confirm_order only shows
/// the draft again; Gemini's "Here is the menu." does not reach the user (MAJOR 1).
#[sqlx::test(migrations = "../migrations")]
async fn confirm_on_a_later_message_shows_the_draft_and_places_nothing(pool: PgPool) {
    seed_with_money(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    gemini_calls(
        &app,
        vec![(
            "draft_order_from_text",
            json!({"text": "chicken burrito no beans"}),
        )],
        "Here you go.",
    );
    let place = judge_drafts_in_dm(&app, 0).await;
    gemini_calls(
        &app,
        vec![("confirm_order", json!({}))],
        "Here is the menu.",
    );
    inbound(
        &app,
        &judge_dm("hmm how much is that again? not sure I want it"),
    )
    .await;
    let sent = app.sidecar.wait_for(2).await;
    assert_eq!(text(&sent[1].body), JUDGE_DRAFT);
    assert_eq!(button_data(&sent[1].body, "Place order"), place);
    turns_finished(&pool).await;
    assert_eq!(placed(&pool).await, (0, 0));
    assert_eq!(draft_statuses(&pool).await, ["pending"]);
}

/// BLOCKER 1c: with Gemini down, "wait, don't place it yet" shows the draft and places nothing.
#[sqlx::test(migrations = "../migrations")]
async fn the_fallbacks_dont_place_it_yet_places_nothing(pool: PgPool) {
    seed_with_money(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    app.gemini.set_down(503);
    judge_drafts_in_dm(&app, 0).await;
    for (i, words) in [
        "wait, don't place it yet",
        "yes place it",
        "confirm",
        "order it",
    ]
    .into_iter()
    .enumerate()
    {
        inbound(&app, &judge_dm(words)).await;
        let sent = app.sidecar.wait_for(i + 2).await;
        assert_eq!(text(&sent[i + 1].body), JUDGE_DRAFT, "{words}");
        assert_eq!(labels(&sent[i + 1].body), ["Place order", "Cancel"]);
    }
    turns_finished(&pool).await;
    assert_eq!(placed(&pool).await, (0, 0));
    assert_eq!(draft_statuses(&pool).await, ["pending"]);
}

/// BLOCKER 1d: with Gemini down, "please don't cancel my order" cancels nothing; "cancel my order"
/// only offers the button; the customer's press cancels it.
#[sqlx::test(migrations = "../migrations")]
async fn the_fallbacks_dont_cancel_my_order_cancels_nothing(pool: PgPool) {
    seed_with_money(&pool).await;
    let app = app(pool.clone(), false, &[]).await;
    let place = judge_drafts_in_dm(&app, 0).await;
    inbound(&app, &judge_press(JUDGE_TG, &place)).await;
    let sent = app.sidecar.wait_for(2).await;
    assert_eq!(text(&sent[1].body), PLACED);
    let status = || async {
        sqlx::query_scalar::<_, String>("SELECT status FROM ferry.orders WHERE NOT is_demo_history")
            .fetch_one(&pool)
            .await
            .unwrap()
    };

    inbound(&app, &judge_dm("please don't cancel my order")).await;
    let sent = app.sidecar.wait_for(3).await;
    assert_eq!(
        text(&sent[2].body),
        "Your Cosmic Cantina order is paid and waiting for pickup."
    );
    assert_eq!(status().await, "funded");

    inbound(&app, &judge_dm("cancel my order")).await;
    let sent = app.sidecar.wait_for(4).await;
    assert_eq!(labels(&sent[3].body), ["Cancel Cosmic Cantina order"]);
    turns_finished(&pool).await;
    assert_eq!(status().await, "funded");
    let refunds: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ferry.ledger_entries WHERE kind = 'refund'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(refunds, 0);

    let cancel = button_data(&sent[3].body, "Cancel Cosmic Cantina order");
    inbound(&app, &judge_press(JUDGE_TG, &cancel)).await;
    let sent = app.sidecar.wait_for(5).await;
    assert_eq!(
        text(&sent[4].body),
        "Cancelled your Cosmic Cantina order: $13.16 is back in your Ferry balance."
    );
    assert_eq!(status().await, "cancelled");
}

/// BLOCKER 1, through Gemini: cancel_order never cancels; it offers the button.
#[sqlx::test(migrations = "../migrations")]
async fn geminis_cancel_order_only_offers_the_button(pool: PgPool) {
    seed_with_money(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    gemini_calls(
        &app,
        vec![(
            "draft_order_from_text",
            json!({"text": "chicken burrito no beans"}),
        )],
        "Here you go.",
    );
    let place = judge_drafts_in_dm(&app, 0).await;
    inbound(&app, &judge_press(JUDGE_TG, &place)).await;
    app.sidecar.wait_for(2).await;
    gemini_calls(
        &app,
        vec![("cancel_order", json!({}))],
        "Cancelled it for you.",
    );
    inbound(&app, &judge_dm("actually cancel that")).await;
    let sent = app.sidecar.wait_for(3).await;
    assert_eq!(
        text(&sent[2].body),
        "Tap the button to cancel your Cosmic Cantina order ($13.16). Nothing is cancelled until you do, and a cancelled order comes back to your Ferry balance in full."
    );
    assert_eq!(labels(&sent[2].body), ["Cancel Cosmic Cantina order"]);
    turns_finished(&pool).await;
    let status: String =
        sqlx::query_scalar("SELECT status FROM ferry.orders WHERE NOT is_demo_history")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(status, "funded");
}

// ---------- MAJOR 2 ----------

/// A pool like production's: 5 connections, a 5 s acquire timeout.
async fn production_pool(options: PgPoolOptions, connect: PgConnectOptions) -> PgPool {
    options
        .max_connections(ferry_core::db::MAX_CONNECTIONS)
        .acquire_timeout(ferry_core::db::ACQUIRE_TIMEOUT)
        .connect_with(connect)
        .await
        .unwrap()
}

/// Two more members of cir_hinton4 with Telegram ids and $40 each, and Leo linked: with the judge
/// and Dana, five customers. Returns (user id, Telegram id) for all five.
async fn five_customers(pool: &PgPool) -> Vec<(String, String)> {
    let mut all = vec![
        ("usr_judge".to_owned(), JUDGE_TG.to_owned()),
        ("usr_dana".to_owned(), DANA_TG.to_owned()),
        ("usr_leo".to_owned(), "444444444".to_owned()),
    ];
    sqlx::query("UPDATE ferry.users SET telegram_user_id = '444444444' WHERE id = 'usr_leo'")
        .execute(pool)
        .await
        .unwrap();
    for (i, (id, tg)) in [("usr_amy", "555555555"), ("usr_ben", "666666666")]
        .into_iter()
        .enumerate()
    {
        sqlx::query(
            "INSERT INTO ferry.users (id, phone_e164, display_name, avatar_seed, bubble_color, telegram_user_id)
             VALUES ($1, $2, '', $1, '#123456', $3)",
        )
        .bind(id)
        .bind(format!("+1202555010{i}"))
        .bind(tg)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO ferry.circle_members (circle_id, user_id) VALUES ('cir_hinton4', $1)",
        )
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        ferry_core::payments::ops::demo_deposit_in(&mut conn, id, 4_000, &format!("test-{id}"))
            .await
            .unwrap();
        all.push((id.to_owned(), tg.to_owned()));
    }
    all
}

/// MAJOR 2: five people press Place order on their own drafts at once, on a 5-connection pool.
/// Each order is placed exactly once, nobody is told "couldn't price", and nothing stalls.
#[sqlx::test(migrations = "../migrations")]
async fn five_people_pressing_at_once_each_place_once(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = production_pool(options, connect).await;
    seed_with_money(&pool).await;
    let customers = five_customers(&pool).await;
    let app = app(pool.clone(), false, &[]).await;
    // The judge's draft in the group; the others get the same request.
    inbound(
        &app,
        &group_message(
            JUDGE_TG,
            Some(JUDGE_USERNAME),
            "Jordan",
            "chicken burrito no beans",
        ),
    )
    .await;
    app.sidecar.wait_for(1).await;
    sqlx::query(
        "INSERT INTO ferry.order_drafts (user_id, run_id, space_id, request, total_cents)
         SELECT u, d.run_id, d.space_id, d.request, d.total_cents
           FROM ferry.order_drafts d, unnest($1::text[]) u",
    )
    .bind(
        customers[1..]
            .iter()
            .map(|(u, _)| u.clone())
            .collect::<Vec<_>>(),
    )
    .execute(&pool)
    .await
    .unwrap();
    let drafts: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, user_id FROM ferry.order_drafts ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(drafts.len(), 5);
    let presses: Vec<Value> = drafts
        .iter()
        .map(|(id, user)| {
            let tg = &customers.iter().find(|(u, _)| u == user).unwrap().1;
            press(GROUP_ID, true, tg, None, "", &format!("order:{id}:place"))
        })
        .collect();

    let started = Instant::now();
    tokio::join!(
        inbound(&app, &presses[0]),
        inbound(&app, &presses[1]),
        inbound(&app, &presses[2]),
        inbound(&app, &presses[3]),
        inbound(&app, &presses[4]),
    );
    let sent = wait_long(&app, 6, 10).await;
    let took = started.elapsed();
    let lines: Vec<&str> = sent[1..].iter().map(text).collect();
    assert!(
        lines.iter().all(|l| l.starts_with("Order placed: $13.16")),
        "{lines:#?}"
    );
    assert!(took < Duration::from_secs(4), "five presses took {took:?}");
    turns_finished(&pool).await;
    app.sidecar.assert_no_more_than(6).await;
    assert_eq!(placed(&pool).await, (5, 5));
    assert_eq!(draft_statuses(&pool).await, ["placed"; 5]);
    let linked: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT order_id) FROM ferry.order_drafts WHERE order_id IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(linked, 5);
}

/// MAJOR 2: eight presses on one draft at once place it once; the others hear it is being placed or
/// placed already, never "try again", and nothing stalls.
#[sqlx::test(migrations = "../migrations")]
async fn eight_presses_on_one_draft_place_it_once(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = production_pool(options, connect).await;
    seed_with_money(&pool).await;
    let app = app(pool.clone(), false, &[]).await;
    let place = judge_drafts_in_dm(&app, 0).await;
    let p = || judge_press(JUDGE_TG, &place);
    let (a, b, c, d, e, f, g, h) = (p(), p(), p(), p(), p(), p(), p(), p());
    let started = Instant::now();
    tokio::join!(
        inbound(&app, &a),
        inbound(&app, &b),
        inbound(&app, &c),
        inbound(&app, &d),
        inbound(&app, &e),
        inbound(&app, &f),
        inbound(&app, &g),
        inbound(&app, &h),
    );
    let sent = wait_long(&app, 9, 10).await;
    let took = started.elapsed();
    let lines: Vec<&str> = sent[1..].iter().map(text).collect();
    assert_eq!(
        lines.iter().filter(|l| **l == PLACED).count(),
        1,
        "{lines:#?}"
    );
    for line in &lines {
        assert!(
            [
                PLACED,
                ordering::PLACING_NOW,
                "That order is placed already."
            ]
            .contains(line),
            "{line}"
        );
        assert!(!line.contains("ry again"), "{line}");
    }
    assert!(took < Duration::from_secs(4), "eight presses took {took:?}");
    turns_finished(&pool).await;
    app.sidecar.assert_no_more_than(9).await;
    assert_eq!(placed(&pool).await, (1, 1));
    assert_eq!(draft_statuses(&pool).await, ["placed"]);
    let balance: i64 = sqlx::query_scalar(
        "SELECT COALESCE(sum(CASE WHEN debit_account = 'user:usr_judge:checking' THEN amount_cents ELSE 0 END)
                       - sum(CASE WHEN credit_account = 'user:usr_judge:checking' THEN amount_cents ELSE 0 END), 0)::bigint
           FROM ferry.ledger_entries",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(balance, 5_000 - 1_316, "held exactly once");
}

/// MAJOR 2: placing failed after the order committed (simulated: the claim is taken, the order is
/// placed, then placing reports an error). The order stands, the draft points at it, the user is
/// told it is placed and never to try again, and later presses place nothing more. A press that
/// arrives while the claim is still held, after the commit, says it is placed and links it.
#[sqlx::test(migrations = "../migrations")]
async fn a_failure_after_the_commit_places_once(pool: PgPool) {
    seed_with_money(&pool).await;
    let app = app(pool.clone(), false, &[]).await;
    judge_drafts_in_dm(&app, 0).await;
    judge_drafts_in_dm(&app, 1).await;
    let drafts: Vec<Draft> = sqlx::query_as(
        "SELECT id, run_id, request, total_cents FROM ferry.order_drafts ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let claim_and_commit = |draft: Draft| {
        let state = app.state.clone();
        let pool = pool.clone();
        async move {
            sqlx::query(
                "UPDATE ferry.order_drafts SET status = 'placing', decided_at = now() WHERE id = $1",
            )
            .bind(draft.id)
            .execute(&pool)
            .await
            .unwrap();
            let request: CreateOrderRequest = serde_json::from_value(draft.request).unwrap();
            orders::place_order(&state, "usr_judge", &draft.run_id, &request)
                .await
                .unwrap()
                .id
        }
    };

    // The money step's own error after the commit.
    let first = drafts[0].id;
    let order_id = claim_and_commit(drafts[0].clone()).await;
    let lost = Err(ApiError::internal(std::io::Error::other(
        "pool timed out after the commit",
    )));
    let said = ordering::settle(&app.state, first, lost, false).await;
    assert_eq!(said.say(), PLACED);
    assert_eq!(said.order_id.as_deref(), Some(order_id.as_str()));
    let (status, linked): (String, Option<String>) =
        sqlx::query_as("SELECT status, order_id FROM ferry.order_drafts WHERE id = $1")
            .bind(first)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        (status.as_str(), linked.as_deref()),
        ("placed", Some(order_id.as_str()))
    );
    inbound(
        &app,
        &judge_press(JUDGE_TG, &format!("order:{first}:place")),
    )
    .await;
    let sent = app.sidecar.wait_for(3).await;
    assert_eq!(text(&sent[2].body), "That order is placed already.");

    // A press while the claim is held and the order has committed but is not linked yet.
    let second = drafts[1].id;
    let second_order = claim_and_commit(drafts[1].clone()).await;
    inbound(
        &app,
        &judge_press(JUDGE_TG, &format!("order:{second}:place")),
    )
    .await;
    let sent = app.sidecar.wait_for(4).await;
    assert_eq!(text(&sent[3].body), "That order is placed already.");
    let linked: Option<String> =
        sqlx::query_scalar("SELECT order_id FROM ferry.order_drafts WHERE id = $1")
            .bind(second)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(linked.as_deref(), Some(second_order.as_str()));

    turns_finished(&pool).await;
    assert_eq!(placed(&pool).await, (2, 2), "one order per draft");
    // A refusal before the commit (nothing committed) releases the claim instead.
    let third = {
        judge_drafts_in_dm(&app, 4).await;
        sqlx::query_scalar::<_, i64>(
            "UPDATE ferry.order_drafts SET status = 'placing', decided_at = now()
              WHERE status = 'pending' RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let refused = Err(ApiError::internal(std::io::Error::other(
        "before the commit",
    )));
    let said = ordering::settle(&app.state, third, refused, false).await;
    assert_eq!(said.order_id, None);
    assert_eq!(draft_statuses(&pool).await, ["placed", "placed", "pending"]);
    assert_eq!(placed(&pool).await, (2, 2));
}

// ---------- Round 2's MAJOR and MINOR 5 ----------

/// The 402 line: a press refused for the balance.
const SHORT: &str =
    "Your Ferry balance is short for this order. Top up in the app, then tap Place order again.";

/// Round 2's MAJOR: the judge ($50) has two drafts on run_demo, 3 chicken burritos ($32.49) in the
/// group and 3 steak burritos ($37.33) in the DM, and presses both at once. One order is placed
/// and linked to one draft; the other press hears the 402 line and its draft is pending again,
/// never "placed" with the first draft's order.
#[sqlx::test(migrations = "../migrations")]
async fn two_drafts_pressed_at_once_with_money_for_one_place_one(pool: PgPool) {
    seed_with_money(&pool).await;
    let app = app(pool.clone(), false, &[]).await;
    inbound(
        &app,
        &group_message(
            JUDGE_TG,
            Some(JUDGE_USERNAME),
            "Jordan",
            "3 chicken burritos no beans",
        ),
    )
    .await;
    let sent = app.sidecar.wait_for(1).await;
    assert!(text(&sent[0].body).contains("= $32.49"), "{}", sent[0].body);
    let group_place = button_data(&sent[0].body, "Place order");
    inbound(&app, &judge_dm("3 steak burritos pinto beans")).await;
    let sent = app.sidecar.wait_for(2).await;
    assert!(text(&sent[1].body).contains("= $37.33"), "{}", sent[1].body);
    let dm_place = button_data(&sent[1].body, "Place order");

    let (in_group, in_dm) = (
        judge_press(GROUP_ID, &group_place),
        judge_press(JUDGE_TG, &dm_place),
    );
    tokio::join!(inbound(&app, &in_group), inbound(&app, &in_dm));
    let sent = wait_long(&app, 4, 10).await;
    turns_finished(&pool).await;
    app.sidecar.assert_no_more_than(4).await;
    let lines: Vec<&str> = sent[2..].iter().map(text).collect();
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.starts_with("Order placed: "))
            .count(),
        1,
        "{lines:#?}"
    );
    assert_eq!(
        lines.iter().filter(|l| **l == SHORT).count(),
        1,
        "{lines:#?}"
    );
    assert_eq!(placed(&pool).await, (1, 1));
    let order: String = sqlx::query_scalar("SELECT id FROM ferry.orders WHERE NOT is_demo_history")
        .fetch_one(&pool)
        .await
        .unwrap();
    let drafts: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT status, order_id FROM ferry.order_drafts ORDER BY status")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        drafts,
        [
            ("pending".to_owned(), None),
            ("placed".to_owned(), Some(order))
        ]
    );
}

/// Round 2's MINOR 5: two claims of one user on one run at once (the judge's group and DM drafts),
/// and the first one's order has committed but is not linked yet. Neither a press on the second
/// draft nor an unclear failure of its placing takes the first one's order: the press hears it is
/// being placed, and the failure says Ferry can't confirm and keeps the claim. Once the first is
/// linked, the second's failure finds nothing of its own and releases it.
#[sqlx::test(migrations = "../migrations")]
async fn a_second_claim_never_takes_the_first_ones_order(pool: PgPool) {
    seed_with_money(&pool).await;
    let app = app(pool.clone(), false, &[]).await;
    inbound(
        &app,
        &group_message(
            JUDGE_TG,
            Some(JUDGE_USERNAME),
            "Jordan",
            "chicken burrito no beans",
        ),
    )
    .await;
    app.sidecar.wait_for(1).await;
    judge_drafts_in_dm(&app, 1).await;
    let drafts: Vec<Draft> = sqlx::query_as(
        "UPDATE ferry.order_drafts SET status = 'placing', decided_at = now()
         RETURNING id, run_id, request, total_cents",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let (first, second) = if drafts[0].id < drafts[1].id {
        (&drafts[0], &drafts[1])
    } else {
        (&drafts[1], &drafts[0])
    };
    let request: CreateOrderRequest = serde_json::from_value(first.request.clone()).unwrap();
    let first_order = orders::place_order(&app.state, "usr_judge", &first.run_id, &request)
        .await
        .unwrap()
        .id;

    // A press on the second draft while both are claimed.
    inbound(
        &app,
        &judge_press(JUDGE_TG, &format!("order:{}:place", second.id)),
    )
    .await;
    let sent = app.sidecar.wait_for(3).await;
    assert_eq!(text(&sent[2].body), ordering::PLACING_NOW);
    // The second's placing fails without saying whether it committed.
    let lost = || Err(ApiError::internal(std::io::Error::other("pool timed out")));
    let said = ordering::settle(&app.state, second.id, lost(), false).await;
    assert_eq!(said.say(), ordering::PLACING_UNSURE);
    assert_eq!(said.order_id, None);
    assert_eq!(draft_statuses(&pool).await, ["placing", "placing"]);

    // The first finishes and links its order; then the second's failure releases its claim.
    let said = ordering::settle(&app.state, first.id, Ok(first_order.clone()), true).await;
    assert_eq!(said.order_id.as_deref(), Some(first_order.as_str()));
    let said = ordering::settle(&app.state, second.id, lost(), false).await;
    assert_eq!(said.order_id, None);
    assert!(!said.say().starts_with("Order placed"), "{}", said.say());
    let links: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT status, order_id FROM ferry.order_drafts ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        links,
        [
            ("placed".to_owned(), Some(first_order)),
            ("pending".to_owned(), None)
        ]
    );
    turns_finished(&pool).await;
    assert_eq!(placed(&pool).await, (1, 1));
}

// ---------- MINOR 2 and MINOR 4 ----------

/// MINOR 2: with a placed order and a newer pending draft, "cancel my order" names both, each with
/// its own button, and cancels nothing; dropping the draft leaves the order alone.
#[sqlx::test(migrations = "../migrations")]
async fn cancel_in_words_offers_every_candidate(pool: PgPool) {
    seed_with_money(&pool).await;
    let app = app(pool.clone(), false, &[]).await;
    let place = judge_drafts_in_dm(&app, 0).await;
    inbound(&app, &judge_press(JUDGE_TG, &place)).await;
    app.sidecar.wait_for(2).await;
    judge_drafts_in_dm(&app, 2).await;

    inbound(&app, &judge_dm("cancel my order")).await;
    let sent = app.sidecar.wait_for(4).await;
    let choices = &sent[3].body;
    assert_eq!(
        text(choices),
        "Which one? Tap the button for your Cosmic Cantina draft ($13.16, not placed yet) or your Cosmic Cantina order ($13.16). Nothing is cancelled until you do."
    );
    assert_eq!(
        labels(choices),
        [
            "Drop the Cosmic Cantina draft",
            "Cancel Cosmic Cantina order"
        ]
    );
    turns_finished(&pool).await;
    assert_eq!(draft_statuses(&pool).await, ["placed", "pending"]);

    let drop = button_data(choices, "Drop the Cosmic Cantina draft");
    inbound(&app, &judge_press(JUDGE_TG, &drop)).await;
    let sent = app.sidecar.wait_for(5).await;
    assert_eq!(
        text(&sent[4].body),
        "OK, I dropped that order. Nothing was charged."
    );
    let status: String =
        sqlx::query_scalar("SELECT status FROM ferry.orders WHERE NOT is_demo_history")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(status, "funded");

    // Dana has nothing to cancel.
    inbound(
        &app,
        &dm(DANA_TG, Some(DANA_USERNAME), "Dana", "cancel my order"),
    )
    .await;
    let sent = app.sidecar.wait_for(6).await;
    assert_eq!(text(&sent[5].body), "You have no order to cancel.");
    assert!(labels(&sent[5].body).is_empty());
}

/// MINOR 4: an "add guac" edit read the draft while it was pending, then a Place order press
/// claimed it. The edit stores nothing: there is never a second live draft.
#[sqlx::test(migrations = "../migrations")]
async fn an_edit_that_loses_the_race_to_place_changes_nothing(pool: PgPool) {
    seed_with_money(&pool).await;
    let app = app(pool.clone(), false, &[]).await;
    judge_drafts_in_dm(&app, 0).await;
    let draft = ordering::pending_draft(&pool, "usr_judge", JUDGE_TG)
        .await
        .unwrap()
        .unwrap();
    sqlx::query(
        "UPDATE ferry.order_drafts SET status = 'placing', decided_at = now() WHERE id = $1",
    )
    .bind(draft.id)
    .execute(&pool)
    .await
    .unwrap();
    let reply = ordering::edit_draft(&app.state, "usr_judge", JUDGE_TG, false, &draft, "add guac")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reply.text(), EDIT_TOO_LATE);
    assert_eq!(reply.draft_id, None);
    assert_eq!(draft_statuses(&pool).await, ["placing"]);

    // While it is pending, the same edit replaces it.
    sqlx::query(
        "UPDATE ferry.order_drafts SET status = 'pending', decided_at = NULL WHERE id = $1",
    )
    .bind(draft.id)
    .execute(&pool)
    .await
    .unwrap();
    let reply = ordering::edit_draft(&app.state, "usr_judge", JUDGE_TG, false, &draft, "add guac")
        .await
        .unwrap()
        .unwrap();
    assert!(reply.draft_id.is_some());
    assert!(reply.text().contains("Guacamole"), "{}", reply.text());
    assert_eq!(draft_statuses(&pool).await, ["cancelled", "pending"]);
}
