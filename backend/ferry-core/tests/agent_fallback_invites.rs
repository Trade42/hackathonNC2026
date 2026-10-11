//! Round 2's MINOR 3 on PR #22: with Gemini down, the fallback reads "sorry, I can't accept" as no
//! answer, never as Accept, while "count me in" still accepts. Through
//! `/internal/messages/inbound` with a stub sidecar; Gemini is off.
mod agent_support;
mod common;

use agent_support::{
    DANA_TG, DANA_USERNAME, JUDGE_TG, JUDGE_USERNAME, app, dm, inbound, map_demo_group, seed_demo,
};
use serde_json::Value;
use sqlx::PgPool;

fn text(sent: &Value) -> &str {
    sent["text"].as_str().unwrap()
}

/// The judge's answer to Dana's invite.
async fn judges_answer(pool: &PgPool) -> String {
    sqlx::query_scalar("SELECT status FROM ferry.invite_responses WHERE user_id = 'usr_judge'")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../migrations")]
async fn a_negated_accept_leaves_the_invite_pending(pool: PgPool) {
    seed_demo(&pool).await;
    map_demo_group(&pool).await;
    let app = app(pool.clone(), false, &[]).await;
    inbound(
        &app,
        &dm(
            DANA_TG,
            Some(DANA_USERNAME),
            "Dana",
            "let's meet with the people I hung out with last time",
        ),
    )
    .await;
    let sent = app.sidecar.wait_for(3).await;
    // The invitees' DMs go out as the invite is made, so Dana's reply is found by chat.
    let reply = app.sidecar.sent_to(DANA_TG);
    assert!(text(&reply[0]).starts_with("Pinged Jordan"), "{}", reply[0]);
    assert_eq!(judges_answer(&pool).await, "pending");

    let mut count = sent.len();
    for words in [
        "sorry, I can't accept, busy tonight",
        "sorry, I can’t accept",
        "I won't accept",
        "don't count me in",
    ] {
        inbound(&app, &dm(JUDGE_TG, Some(JUDGE_USERNAME), "Jordan", words)).await;
        let sent = app.sidecar.wait_for(count + 1).await;
        count = sent.len();
        let reply = text(&sent[count - 1].body);
        assert_eq!(sent[count - 1].body["space_id"], JUDGE_TG);
        assert!(!reply.contains("You're in"), "{words}: {reply}");
        assert_eq!(judges_answer(&pool).await, "pending", "{words}");
    }

    // A plain accept still accepts.
    inbound(
        &app,
        &dm(JUDGE_TG, Some(JUDGE_USERNAME), "Jordan", "count me in"),
    )
    .await;
    // The judge's reply, and the answer relayed to Dana (meet-place), in either order.
    app.sidecar.wait_for(count + 2).await;
    let judge = app.sidecar.sent_to(JUDGE_TG);
    let reply = text(judge.last().unwrap());
    assert!(reply.starts_with("You're in!"), "{reply}");
    assert_eq!(
        text(app.sidecar.sent_to(DANA_TG).last().unwrap()),
        "Jordan's in."
    );
    assert_eq!(judges_answer(&pool).await, "accepted");
    assert_eq!(app.gemini.request_count(), 0);
}
