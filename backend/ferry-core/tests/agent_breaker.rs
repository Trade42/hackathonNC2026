//! Gemini's circuit breaker (`agent::breaker`), through `POST /internal/messages/inbound` with a
//! stub sidecar and a stub Gemini (never the real one): two failed calls in a row open it, an
//! open breaker answers by the fallback at once without calling Gemini, the trip classifier obeys
//! it too, and only a probe with the real request shape closes it. Probes are checked by their
//! shape and their spacing (one loop), never by how many ran: that depends on the machine's load.
mod agent_support;
mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agent_support::{
    GROUP_ID, JUDGE_TG, JUDGE_USERNAME, MAYA_TG, MAYA_USERNAME, TestApp, app, dm, group_message,
    inbound, map_demo_group, seed_demo, text_reply,
};
use ferry_core::agent::breaker::Breaker;
use ferry_core::agent::fallback::HELP;
use ferry_core::agent::tools::Tool;
use serde_json::{Value, json};
use sqlx::PgPool;

/// The judge's DM saying `words`.
fn judge_says(words: &str) -> Value {
    dm(JUDGE_TG, Some(JUDGE_USERNAME), "Jordan", words)
}

/// The app with a breaker whose probe runs every `probe_every`.
async fn app_probing(pool: PgPool, probe_every: Duration) -> TestApp {
    let mut app = app(pool, true, &[]).await;
    app.state.gemini_breaker = Breaker::new(probe_every);
    app.router = ferry_core::app::router(app.state.clone());
    app
}

/// The `n`-th finished turn's (reply_text, error) in `space_id`, oldest first, waiting for it.
async fn finished_turn(
    pool: &PgPool,
    space_id: &str,
    n: usize,
) -> (Option<String>, Option<String>) {
    for _ in 0..500 {
        let rows: Vec<(Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT reply_text, error FROM ferry.agent_turns
              WHERE space_id = $1 AND latency_ms IS NOT NULL ORDER BY id",
        )
        .bind(space_id)
        .fetch_all(pool)
        .await
        .unwrap();
        if let Some(row) = rows.get(n) {
            return row.clone();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no finished turn {n} in {space_id}");
}

/// Whether `body` is the breaker's probe: the fixture question and every declaration.
fn is_probe(body: &Value) -> bool {
    let question = body["contents"][0]["parts"][0]["text"].as_str();
    let declared = body["tools"][0]["functionDeclarations"]
        .as_array()
        .map_or(0, Vec::len);
    body["contents"].as_array().map_or(0, Vec::len) == 1
        && question == Some("how long until my food gets here?")
        && declared == Tool::ALL.len()
}

/// How long a turn may take while the breaker is open: it never waits on Gemini, so this holds
/// under load and stays far below the client's 12 s attempt timeout.
const OPEN_ANSWER: Duration = Duration::from_secs(3);

/// The first `turn_calls` requests the stub saw are the turns' own calls (none is a probe), and
/// every later one is a probe: while the breaker is open, nothing else reaches Gemini. Probes are
/// checked by their shape, not counted, since how many run depends on the machine's load.
fn only_probes_after(app: &TestApp, turn_calls: usize) {
    let requests = app.gemini.requests();
    assert!(
        requests.len() >= turn_calls,
        "{} requests, not {turn_calls}",
        requests.len()
    );
    assert!(
        requests[..turn_calls].iter().all(|r| !is_probe(&r.body)),
        "the turns' own calls"
    );
    assert!(
        requests[turn_calls..].iter().all(|r| is_probe(&r.body)),
        "every request while open is a probe"
    );
}

/// Two DMs whose turns each fail on Gemini (a 503 and its retry): the breaker opens.
async fn open_with_503s(app: &TestApp, pool: &PgPool) {
    app.gemini.set_down(503);
    inbound(app, &judge_says("hi")).await;
    app.sidecar.wait_for(1).await;
    finished_turn(pool, JUDGE_TG, 0).await;
    assert!(
        !app.state.gemini_breaker.is_open(),
        "one failed call does not open it"
    );
    assert_eq!(app.gemini.request_count(), 2, "the call and its retry");
    inbound(app, &judge_says("hello")).await;
    app.sidecar.wait_for(2).await;
    finished_turn(pool, JUDGE_TG, 1).await;
    assert!(
        app.state.gemini_breaker.is_open(),
        "two failed calls open it"
    );
    // The two turns' calls and retries; any later request is a probe (they run once it is open).
    only_probes_after(app, 4);
}

/// The brief's case: Gemini hangs (no answer within the 12 s attempt timeout). Two turns that
/// wait it out open the breaker; the next message is answered by the fallback at once (well under
/// the 12 s attempt timeout), with no request to Gemini but probes.
#[sqlx::test(migrations = "../migrations")]
async fn a_hanging_gemini_opens_the_breaker_and_then_messages_are_answered_at_once(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    app.gemini.set_delay(Duration::from_secs(40));

    // Two turns at once, each waiting out two 12 s attempts.
    let started = Instant::now();
    inbound(&app, &judge_says("hi")).await;
    inbound(&app, &judge_says("hello")).await;
    while app.sidecar.sent().len() < 2 {
        assert!(started.elapsed() < Duration::from_secs(40), "no fallback");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let waited = started.elapsed();
    assert!(app.state.gemini_breaker.is_open(), "after {waited:?}");
    // Two calls, each retried once.
    only_probes_after(&app, 4);
    for n in 0..2 {
        let (reply, error) = finished_turn(&pool, JUDGE_TG, n).await;
        // The client's two attempts (24.3 s) or the turn's 25 s budget, whichever ends first.
        assert!(
            matches!(error.as_deref(), Some("timeout" | "turn_budget")),
            "{error:?}"
        );
        assert_eq!(reply.as_deref(), Some(HELP));
    }

    // Open: the fallback answers at once, and Gemini is not called.
    let started = Instant::now();
    inbound(&app, &judge_says("hey")).await;
    let sent = app.sidecar.wait_for(3).await;
    let answered_in = started.elapsed();
    assert!(answered_in < OPEN_ANSWER, "{answered_in:?}");
    assert_eq!(sent[2].body["text"], HELP);
    only_probes_after(&app, 4);
    let (reply, error) = finished_turn(&pool, JUDGE_TG, 2).await;
    assert_eq!(reply.as_deref(), Some(HELP));
    assert_eq!(error.as_deref(), Some("breaker_open"));
}

/// A probe that fails keeps the breaker open; one that answers closes it, and Gemini answers the
/// next message. Every probe is one attempt with the real request shape.
#[sqlx::test(migrations = "../migrations")]
async fn a_failed_probe_keeps_it_open_and_an_answered_probe_closes_it(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app_probing(pool.clone(), Duration::from_millis(150)).await;
    open_with_503s(&app, &pool).await;

    // From here a closure answers, noting when each probe arrives: 503 while Gemini is down; once
    // it is back, "ok" to a probe and a greeting to a turn. Installed before `set_up`, so no
    // request falls between the two.
    let probes: Arc<Mutex<Vec<Instant>>> = Arc::default();
    let back = Arc::new(AtomicBool::new(false));
    {
        let (probes, back) = (probes.clone(), back.clone());
        app.gemini.respond_with(move |body| {
            let probe = is_probe(body);
            if probe {
                probes.lock().unwrap().push(Instant::now());
            }
            match (back.load(Ordering::SeqCst), probe) {
                (false, _) => (
                    503,
                    json!({"error": {"code": 503, "message": "stub down", "status": "UNAVAILABLE"}}),
                ),
                (true, true) => (200, text_reply("ok")),
                (true, false) => (200, text_reply("Hi from Ferry.")),
            }
        });
    }
    app.gemini.set_up();

    // Probes fail: it stays open, and messages still get the fallback at once.
    let started = Instant::now();
    while probes.lock().unwrap().len() < 3 {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{} failed probes in 10 s",
            probes.lock().unwrap().len()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(app.state.gemini_breaker.is_open());
    let started = Instant::now();
    inbound(&app, &judge_says("hey")).await;
    let sent = app.sidecar.wait_for(3).await;
    let answered_in = started.elapsed();
    assert!(answered_in < OPEN_ANSWER, "{answered_in:?}");
    assert_eq!(sent[2].body["text"], HELP);
    assert_eq!(
        finished_turn(&pool, JUDGE_TG, 2).await.1.as_deref(),
        Some("breaker_open")
    );
    only_probes_after(&app, 4);

    // Gemini is back: the next probe answers and closes the breaker.
    back.store(true, Ordering::SeqCst);
    let started = Instant::now();
    while app.state.gemini_breaker.is_open() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "no probe closed it"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let closed_at = app.gemini.request_count();
    assert!(is_probe(&app.gemini.requests()[closed_at - 1].body));
    // No request after the closing probe.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(app.gemini.request_count(), closed_at);
    // One probe loop: it waits 150 ms before each probe, so two probes closer than 100 ms would be
    // two loops.
    let times = probes.lock().unwrap().clone();
    assert!(times.len() >= 4, "{} probes", times.len());
    for (i, pair) in times.windows(2).enumerate() {
        let gap = pair[1] - pair[0];
        assert!(
            gap >= Duration::from_millis(100),
            "probes {i} and {} only {gap:?} apart",
            i + 1
        );
    }

    // Closed: Gemini answers again.
    inbound(&app, &judge_says("hi again")).await;
    let sent = app.sidecar.wait_for(4).await;
    assert_eq!(sent[3].body["text"], "Hi from Ferry.");
    assert_eq!(app.gemini.request_count(), closed_at + 1);
    assert_eq!(finished_turn(&pool, JUDGE_TG, 3).await.1, None);
}

/// The trip classifier obeys the breaker: with it open, a group trip is read by the patterns
/// alone and Gemini sees nothing.
#[sqlx::test(migrations = "../migrations")]
async fn an_open_breaker_leaves_a_group_trip_to_the_patterns(pool: PgPool) {
    seed_demo(&pool).await;
    map_demo_group(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    open_with_503s(&app, &pool).await;

    let started = Instant::now();
    let trip = group_message(
        MAYA_TG,
        Some(MAYA_USERNAME),
        "Maya",
        "heading to sup dogs at 7pm",
    );
    inbound(&app, &trip).await;
    let sent = app.sidecar.wait_for(3).await;
    let answered_in = started.elapsed();
    assert!(answered_in < OPEN_ANSWER, "{answered_in:?}");
    assert_eq!(sent[2].body["space_id"], GROUP_ID);
    let said = sent[2].body["text"].as_str().unwrap();
    assert!(said.starts_with("Open a Ferry to Sup Dogs?"), "{said}");
    // The classifier was not called.
    only_probes_after(&app, 4);
    let (_, error) = finished_turn(&pool, GROUP_ID, 0).await;
    assert_eq!(error.as_deref(), Some("breaker_open"));
}

/// Each app has its own breaker: one opened in another app does not touch a new one.
#[sqlx::test(migrations = "../migrations")]
async fn a_new_app_starts_closed(pool: PgPool) {
    seed_demo(&pool).await;
    let app = app(pool.clone(), true, &[]).await;
    assert!(!app.state.gemini_breaker.is_open());
    app.gemini.script(vec![text_reply("Hi from Ferry.")]);
    inbound(&app, &judge_says("hi")).await;
    let sent = app.sidecar.wait_for(1).await;
    assert_eq!(sent[0].body["text"], "Hi from Ferry.");
    assert_eq!(app.gemini.request_count(), 1);
}
