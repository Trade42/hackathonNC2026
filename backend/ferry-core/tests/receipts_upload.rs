//! `POST /v1/runs/{id}/receipt-upload` against a local Storage stub: the runner of a closed run
//! gets a path under the run's id and Storage's upload token, as supabase-js's
//! `uploadToSignedUrl(path, token, file)` needs them; everyone else and every other state is
//! refused without calling Storage; and Storage down, slow or unset is 503 within seconds.
mod common;
mod realtime_support;
mod receipts_support;
mod runs_support;

use std::time::{Duration, Instant, UNIX_EPOCH};

use axum::http::{Method, StatusCode};
use ferry_core::notifications::rfc3339;
use realtime_support::{Server, assert_contract};
use receipts_support::{Mode, SECRET_KEY, Stub, UPLOAD_TOKEN_SECS, seed, server};
use runs_support::{OUTSIDER, RUN, burrito, call, get, open_run, post};
use serde_json::{Value, json};
use sqlx::PgPool;

const UPLOAD: &str = "/v1/runs/run_demo/receipt-upload";

fn assert_error(answer: &(StatusCode, Value), status: StatusCode, code: &str, what: &str) {
    assert_eq!(answer.0, status, "{what}: {}", answer.1);
    common::assert_envelope(&answer.1, code);
}

/// The judge orders on run_demo and Maya closes it.
async fn closed_demo_run(server: &Server) {
    let (status, order) = post(
        server,
        "/v1/runs/run_demo/orders",
        "usr_judge",
        Some(burrito()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{order}");
    let (status, run) = post(server, "/v1/runs/run_demo/close", "usr_maya", None).await;
    assert_eq!(status, StatusCode::OK, "{run}");
}

/// The `exp` claim of a stub token, unverified.
fn exp_of(token: &str) -> u64 {
    #[derive(serde::Deserialize)]
    struct Exp {
        exp: u64,
    }
    jsonwebtoken::dangerous::insecure_decode_claims::<Exp>(token)
        .unwrap()
        .exp
}

#[sqlx::test(migrations = "../migrations")]
async fn the_runner_of_a_closed_run_gets_an_upload_token(pool: PgPool) {
    let stub = Stub::start().await;
    seed(&pool).await;
    let server = server(pool.clone(), Some(&stub.base)).await;
    closed_demo_run(&server).await;

    let (status, upload) = post(&server, UPLOAD, "usr_maya", None).await;
    assert_eq!(status, StatusCode::OK, "{upload}");
    assert_contract("ReceiptUpload", &upload);
    assert_eq!(upload["bucket"], "receipts");
    let path = upload["path"].as_str().unwrap();
    let name = path
        .strip_prefix("run_demo/")
        .expect("path starts with the run's id");
    let stem = name.strip_suffix(".jpg").expect("a .jpg file");
    assert_eq!(stem.len(), 24, "{path}");
    assert!(
        stem.bytes()
            .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase())
    );

    // The token is Storage's, and expires_at is its exp claim.
    let token = upload["token"].as_str().unwrap();
    assert_eq!(stub.tokens(), vec![token.to_owned()]);
    let exp = exp_of(token);
    assert_eq!(
        upload["expires_at"],
        rfc3339(UNIX_EPOCH + Duration::from_secs(exp))
    );
    let now = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(exp.abs_diff(now + UPLOAD_TOKEN_SECS) < 60, "exp {exp}");

    // One request to Storage: the receipts bucket, that path, the secret key in apikey only.
    let seen = stub.seen();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].route, "upload-sign");
    assert_eq!(
        seen[0].path,
        format!("/storage/v1/object/upload/sign/receipts/{path}")
    );
    assert_eq!(seen[0].apikey.as_deref(), Some(SECRET_KEY));
    assert_eq!(seen[0].authorization, None);

    // Each call is a fresh path, so a retake never overwrites.
    let (status, again) = post(&server, UPLOAD, "usr_maya", None).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_ne!(again["path"], upload["path"]);
    assert_ne!(again["token"], upload["token"]);

    // The path is what purchased accepts.
    let (status, run) = post(
        &server,
        "/v1/runs/run_demo/purchased",
        "usr_maya",
        Some(json!({"receipt_total_cents": 900, "receipt_path": path})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    let stored: Option<String> =
        sqlx::query_scalar("SELECT receipt_path FROM ferry.runs WHERE id = $1")
            .bind(RUN)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored.as_deref(), Some(path));
}

#[sqlx::test(migrations = "../migrations")]
async fn refused_for_everyone_else_and_every_other_state(pool: PgPool) {
    let stub = Stub::start().await;
    seed(&pool).await;
    let server = server(pool.clone(), Some(&stub.base)).await;

    // Open: the runner is 409 with the status; others who see the run 403; others 404.
    let (status, order) = post(
        &server,
        "/v1/runs/run_demo/orders",
        "usr_judge",
        Some(burrito()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{order}");
    let answer = post(&server, UPLOAD, "usr_maya", None).await;
    assert_error(&answer, StatusCode::CONFLICT, "conflict", "open run");
    assert_eq!(answer.1["error"]["details"]["status"], "open");
    for (user, status, code) in [
        ("usr_judge", StatusCode::FORBIDDEN, "forbidden"),
        ("usr_leo", StatusCode::FORBIDDEN, "forbidden"),
        (OUTSIDER, StatusCode::NOT_FOUND, "not_found"),
    ] {
        let answer = post(&server, UPLOAD, user, None).await;
        assert_error(&answer, status, code, user);
    }

    // Closed: only the runner.
    let (status, _) = post(&server, "/v1/runs/run_demo/close", "usr_maya", None).await;
    assert_eq!(status, StatusCode::OK);
    for (user, status, code) in [
        ("usr_judge", StatusCode::FORBIDDEN, "forbidden"),
        ("usr_dana", StatusCode::FORBIDDEN, "forbidden"),
        (OUTSIDER, StatusCode::NOT_FOUND, "not_found"),
    ] {
        let answer = post(&server, UPLOAD, user, None).await;
        assert_error(&answer, status, code, user);
    }
    let answer = call(&server, Method::POST, UPLOAD, None, None).await;
    assert_error(
        &answer,
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "no token",
    );
    let answer = post(
        &server,
        "/v1/runs/run_nope/receipt-upload",
        "usr_maya",
        None,
    )
    .await;
    assert_error(&answer, StatusCode::NOT_FOUND, "not_found", "unknown run");
    assert_eq!(stub.seen().len(), 0, "refusals never reach Storage");

    // Purchased, delivering: too late.
    let (status, _) = post(
        &server,
        "/v1/runs/run_demo/purchased",
        "usr_maya",
        Some(json!({"receipt_total_cents": 900})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let answer = post(&server, UPLOAD, "usr_maya", None).await;
    assert_error(&answer, StatusCode::CONFLICT, "conflict", "purchased run");
    assert_eq!(answer.1["error"]["details"]["status"], "purchased");
    let (status, _) = post(&server, "/v1/runs/run_demo/picked-up", "usr_maya", None).await;
    assert_eq!(status, StatusCode::OK);
    let answer = post(&server, UPLOAD, "usr_maya", None).await;
    assert_error(&answer, StatusCode::CONFLICT, "conflict", "delivering run");
    assert_eq!(answer.1["error"]["details"]["status"], "delivering");

    // Cancelled.
    let cancelled = open_run(&server, "usr_dana", "cir_hinton4").await;
    let (status, _) = post(
        &server,
        &format!("/v1/runs/{cancelled}/cancel"),
        "usr_dana",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let answer = post(
        &server,
        &format!("/v1/runs/{cancelled}/receipt-upload"),
        "usr_dana",
        None,
    )
    .await;
    assert_error(&answer, StatusCode::CONFLICT, "conflict", "cancelled run");
    assert_eq!(answer.1["error"]["details"]["status"], "cancelled");

    // Demo history is read-only, even when closed.
    let history = open_run(&server, "usr_dana", "cir_hinton4").await;
    let (status, _) = post(
        &server,
        &format!("/v1/runs/{history}/close"),
        "usr_dana",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    sqlx::query("UPDATE ferry.runs SET is_demo_history = true WHERE id = $1")
        .bind(&history)
        .execute(&pool)
        .await
        .unwrap();
    let answer = post(
        &server,
        &format!("/v1/runs/{history}/receipt-upload"),
        "usr_dana",
        None,
    )
    .await;
    assert_error(&answer, StatusCode::CONFLICT, "conflict", "demo history");

    assert_eq!(stub.seen().len(), 0, "refusals never reach Storage");
    let (status, _) = get(&server, "/v1/runs/run_demo", "usr_maya").await;
    assert_eq!(status, StatusCode::OK);
}

/// Asserts the upload answer is 503 `upstream_unavailable` with nothing secret in it, within
/// `limit`.
async fn assert_unavailable(server: &Server, limit: Duration, what: &str) {
    let started = Instant::now();
    let answer = post(server, UPLOAD, "usr_maya", None).await;
    let took = started.elapsed();
    assert_error(
        &answer,
        StatusCode::SERVICE_UNAVAILABLE,
        "upstream_unavailable",
        what,
    );
    assert!(took < limit, "{what}: took {took:?}");
    let text = answer.1.to_string();
    assert!(
        !text.contains(SECRET_KEY) && !text.contains("storage/v1"),
        "{text}"
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn storage_down_slow_failing_or_unset_is_503_within_seconds(pool: PgPool) {
    seed(&pool).await;
    let stub = Stub::start().await;
    closed_demo_run(&server(pool.clone(), Some(&stub.base)).await).await;

    // Nothing listens.
    let refused = server(pool.clone(), Some("http://127.0.0.1:1")).await;
    assert_unavailable(&refused, Duration::from_secs(2), "connection refused").await;

    // Storage answers 500, then 401 (a bad key).
    let failing = server(pool.clone(), Some(&stub.base)).await;
    stub.set_mode(Mode::Fail(500));
    assert_unavailable(&failing, Duration::from_secs(2), "Storage 500").await;
    stub.set_mode(Mode::Fail(401));
    assert_unavailable(&failing, Duration::from_secs(2), "Storage 401").await;

    // Storage accepts and never answers: the 4 s timeout.
    stub.set_mode(Mode::Hang);
    assert_unavailable(&failing, Duration::from_secs(6), "Storage hangs").await;

    // SUPABASE_URL or the secret key unset.
    let unset = server(pool.clone(), None).await;
    assert_unavailable(&unset, Duration::from_secs(2), "Storage not configured").await;

    // Storage back: the token comes.
    stub.set_mode(Mode::Ok);
    let (status, upload) = post(&failing, UPLOAD, "usr_maya", None).await;
    assert_eq!(status, StatusCode::OK, "{upload}");
    assert_contract("ReceiptUpload", &upload);
}
