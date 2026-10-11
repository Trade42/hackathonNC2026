//! `Order.receipt_url` against a local Storage stub: after Maya uploads the receipt and taps
//! Paid, the judge's order (REST and the order.updated event) carries a signed link valid 1 hour,
//! as does every copy the runner sees; nobody else can read the order; a run without a receipt,
//! or an object Storage does not hold, gives null; links are cached; and Storage down, slow or
//! unset gives null quickly, never a failed read.
mod common;
mod realtime_support;
mod receipts_support;
mod runs_support;

use std::time::{Duration, Instant};

use axum::http::StatusCode;
use realtime_support::{Server, Ws, assert_contract, next_json};
use receipts_support::{Mode, SECRET_KEY, Stub, seed, server};
use runs_support::{OUTSIDER, RUN, burrito, chips, get, post};
use serde_json::{Value, json};
use sqlx::PgPool;

/// The judge and Dana order on run_demo, Maya closes it and asks for an upload token, and the
/// stub then holds the object (the app's `uploadToSignedUrl`). Returns the two order ids and
/// the receipt's path.
async fn closed_with_receipt(server: &Server, stub: &Stub) -> (String, String, String) {
    let (status, judge) = post(
        server,
        "/v1/runs/run_demo/orders",
        "usr_judge",
        Some(burrito()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{judge}");
    let (status, dana) = post(
        server,
        "/v1/runs/run_demo/orders",
        "usr_dana",
        Some(chips()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{dana}");
    let (status, run) = post(server, "/v1/runs/run_demo/close", "usr_maya", None).await;
    assert_eq!(status, StatusCode::OK, "{run}");
    let (status, upload) = post(server, "/v1/runs/run_demo/receipt-upload", "usr_maya", None).await;
    assert_eq!(status, StatusCode::OK, "{upload}");
    let path = upload["path"].as_str().unwrap().to_owned();
    stub.put_object("receipts", &path);
    (
        judge["id"].as_str().unwrap().to_owned(),
        dana["id"].as_str().unwrap().to_owned(),
        path,
    )
}

async fn purchase(server: &Server, receipt_path: Option<&str>) {
    let mut body = json!({"receipt_total_cents": 1300});
    if let Some(path) = receipt_path {
        body["receipt_path"] = json!(path);
    }
    let (status, run) = post(
        server,
        "/v1/runs/run_demo/purchased",
        "usr_maya",
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{run}");
}

/// The next order.updated on `ws` for `order_id` in `status`, skipping other events; each event
/// must validate against the contract.
async fn order_event(ws: &mut Ws, order_id: &str, status: &str) -> Value {
    loop {
        let event = next_json(ws).await;
        assert_contract("ServerEvent", &event);
        if event["type"] == "order.updated"
            && event["data"]["order"]["id"] == order_id
            && event["data"]["order"]["status"] == status
        {
            assert_contract("OrderUpdatedEvent", &event);
            return event["data"]["order"].clone();
        }
    }
}

/// `GET /v1/orders/{id}` as `user`: the order, checked against the contract.
async fn read_order(server: &Server, id: &str, user: &str) -> Value {
    let (status, order) = get(server, &format!("/v1/orders/{id}"), user).await;
    assert_eq!(status, StatusCode::OK, "{user}: {order}");
    assert_contract("Order", &order);
    order
}

#[sqlx::test(migrations = "../migrations")]
async fn the_customer_and_the_runner_get_a_signed_link(pool: PgPool) {
    let stub = Stub::start().await;
    seed(&pool).await;
    let server = server(pool.clone(), Some(&stub.base)).await;
    let (judge_order, dana_order, path) = closed_with_receipt(&server, &stub).await;
    assert!(read_order(&server, &judge_order, "usr_judge").await["receipt_url"].is_null());

    let mut judge = server.connect_with_token("usr_judge").await;
    let mut maya = server.connect_with_token("usr_maya").await;
    purchase(&server, Some(&path)).await;

    // The demo's moment: the receipt appears on the judge's order, in the event.
    let tokens = stub.tokens();
    assert_eq!(tokens.len(), 2, "one upload token, one download link");
    let link = stub.link(&path, &tokens[1]);
    let event = order_event(&mut judge, &judge_order, "purchased").await;
    assert_eq!(event["receipt_url"], link, "{event:#}");
    assert!(event["handoff_code"].is_string());
    for id in [&judge_order, &dana_order] {
        let event = order_event(&mut maya, id, "purchased").await;
        assert_eq!(event["receipt_url"], link, "maya's copy of {id}");
        assert!(event["handoff_code"].is_null());
    }

    // One signing call: the receipts bucket, a 1-hour link, the secret key in apikey only.
    let signs: Vec<_> = stub
        .seen()
        .into_iter()
        .filter(|s| s.route == "sign")
        .collect();
    assert_eq!(signs.len(), 1, "{signs:?}");
    assert_eq!(signs[0].path, "/storage/v1/object/sign/receipts");
    assert_eq!(signs[0].body, json!({"expiresIn": 3600, "paths": [path]}));
    assert_eq!(signs[0].apikey.as_deref(), Some(SECRET_KEY));
    assert_eq!(signs[0].authorization, None);

    // REST, every copy a party may read.
    assert_eq!(
        read_order(&server, &judge_order, "usr_judge").await["receipt_url"],
        link
    );
    assert_eq!(
        read_order(&server, &judge_order, "usr_maya").await["receipt_url"],
        link
    );
    assert_eq!(
        read_order(&server, &dana_order, "usr_dana").await["receipt_url"],
        link
    );
    assert_eq!(
        read_order(&server, &dana_order, "usr_maya").await["receipt_url"],
        link
    );
    let (status, list) = get(&server, "/v1/orders", "usr_judge").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_contract("Order", &list[0]);
    assert_eq!(list[0]["receipt_url"], link);
    let (status, detail) = get(&server, "/v1/runs/run_demo", "usr_maya").await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_contract("RunDetail", &detail);
    let orders = detail["orders"].as_array().unwrap();
    assert_eq!(orders.len(), 2);
    assert!(
        orders.iter().all(|o| o["receipt_url"] == link),
        "{detail:#}"
    );
    let (_, detail) = get(&server, "/v1/runs/run_demo", "usr_judge").await;
    assert_contract("RunDetail", &detail);
    assert_eq!(detail["orders"].as_array().unwrap().len(), 1);
    assert_eq!(detail["orders"][0]["receipt_url"], link);

    // Nobody else reads an order: a member without one sees none, others get 404.
    let (_, detail) = get(&server, "/v1/runs/run_demo", "usr_leo").await;
    assert_contract("RunDetail", &detail);
    assert_eq!(detail["orders"], json!([]));
    let (status, _) = get(&server, "/v1/runs/run_demo", OUTSIDER).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    for (user, id) in [
        ("usr_dana", &judge_order),
        ("usr_leo", &judge_order),
        (OUTSIDER, &dana_order),
    ] {
        let (status, body) = get(&server, &format!("/v1/orders/{id}"), user).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{user}: {body}");
        assert!(!body.to_string().contains("token="), "{body}");
    }
    let (_, list) = get(&server, "/v1/orders", "usr_leo").await;
    assert_eq!(list, json!([]));

    // Every read came from the cache.
    assert_eq!(stub.count("sign"), 1);
}

#[sqlx::test(migrations = "../migrations")]
async fn no_receipt_or_no_object_is_null(pool: PgPool) {
    let stub = Stub::start().await;
    seed(&pool).await;
    let server = server(pool.clone(), Some(&stub.base)).await;
    let (judge_order, _, path) = closed_with_receipt(&server, &stub).await;

    // Paid without a photo: null, and Storage is not asked.
    purchase(&server, None).await;
    assert!(read_order(&server, &judge_order, "usr_judge").await["receipt_url"].is_null());
    assert!(read_order(&server, &judge_order, "usr_maya").await["receipt_url"].is_null());
    assert_eq!(stub.count("sign"), 0);

    let set_path = |p: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query("UPDATE ferry.runs SET receipt_path = $2 WHERE id = $1")
                .bind(RUN)
                .bind(p)
                .execute(&pool)
                .await
                .unwrap();
        }
    };

    // A path Storage holds no object for: null, and asked again only after a minute.
    set_path("run_demo/never-uploaded.jpg").await;
    assert!(read_order(&server, &judge_order, "usr_judge").await["receipt_url"].is_null());
    assert_eq!(stub.count("sign"), 1);
    assert!(read_order(&server, &judge_order, "usr_maya").await["receipt_url"].is_null());
    assert_eq!(stub.count("sign"), 1);

    // A path with a space still gives a valid URI.
    set_path("run_demo/my receipt.jpg").await;
    stub.put_object("receipts", "run_demo/my receipt.jpg");
    let order = read_order(&server, &judge_order, "usr_judge").await;
    let link = order["receipt_url"].as_str().unwrap();
    let token = stub.tokens().last().unwrap().clone();
    assert_eq!(
        link,
        format!(
            "{}/storage/v1/object/sign/receipts/run_demo/my%20receipt.jpg?token={token}",
            stub.base
        )
    );

    // And the upload's own path, once set, signs too.
    sqlx::query("UPDATE ferry.runs SET receipt_path = $2 WHERE id = $1")
        .bind(RUN)
        .bind(&path)
        .execute(&pool)
        .await
        .unwrap();
    let order = read_order(&server, &judge_order, "usr_maya").await;
    assert!(
        order["receipt_url"].as_str().unwrap().contains(&path),
        "{order:#}"
    );
}

/// `GET /v1/orders/{id}` as the judge: 200 with a null receipt_url, within `limit`.
async fn assert_null_within(server: &Server, order_id: &str, limit: Duration, what: &str) {
    let started = Instant::now();
    let order = read_order(server, order_id, "usr_judge").await;
    let took = started.elapsed();
    assert!(order["receipt_url"].is_null(), "{what}: {order:#}");
    assert!(took < limit, "{what}: took {took:?}");
}

#[sqlx::test(migrations = "../migrations")]
async fn storage_down_slow_failing_or_unset_reads_are_null(pool: PgPool) {
    let stub = Stub::start().await;
    seed(&pool).await;
    let up = server(pool.clone(), Some(&stub.base)).await;
    let (judge_order, _, path) = closed_with_receipt(&up, &stub).await;
    // Nobody is connected, so the purchase signs nothing yet.
    purchase(&up, Some(&path)).await;
    assert_eq!(stub.count("sign"), 0);

    // Storage hangs: the 2 s timeout, then 30 s of not asking.
    stub.set_mode(Mode::Hang);
    assert_null_within(&up, &judge_order, Duration::from_secs(4), "Storage hangs").await;
    assert_eq!(stub.count("sign"), 1);
    assert_null_within(&up, &judge_order, Duration::from_millis(800), "backing off").await;
    let started = Instant::now();
    let (status, detail) = get(&up, "/v1/runs/run_demo", "usr_maya").await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_contract("RunDetail", &detail);
    assert!(
        detail["orders"]
            .as_array()
            .unwrap()
            .iter()
            .all(|o| o["receipt_url"].is_null())
    );
    let (status, list) = get(&up, "/v1/orders", "usr_judge").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert!(list[0]["receipt_url"].is_null());
    assert!(started.elapsed() < Duration::from_millis(800));
    assert_eq!(stub.count("sign"), 1);

    // Storage answers 500.
    let failing_stub = Stub::start().await;
    failing_stub.set_mode(Mode::Fail(500));
    let failing = server(pool.clone(), Some(&failing_stub.base)).await;
    assert_null_within(
        &failing,
        &judge_order,
        Duration::from_secs(2),
        "Storage 500",
    )
    .await;
    assert_eq!(failing_stub.count("sign"), 1);

    // Nothing listens.
    let refused = server(pool.clone(), Some("http://127.0.0.1:1")).await;
    assert_null_within(
        &refused,
        &judge_order,
        Duration::from_secs(2),
        "connection refused",
    )
    .await;

    // SUPABASE_URL or the secret key unset.
    let unset = server(pool.clone(), None).await;
    assert_null_within(
        &unset,
        &judge_order,
        Duration::from_secs(1),
        "not configured",
    )
    .await;
}
