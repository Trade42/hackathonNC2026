//! Manual only, against Google's real Gemini API, with fixture text only (no user's data, no
//! name). Ignored by default, so `cargo test` never calls Gemini. Both read `GEMINI_API_KEY` and
//! `GEMINI_MODEL` from the root .env as ferry-core does and print no key and no body. Run by hand:
//!
//! ```sh
//! cargo test --test agent_live -- --ignored --nocapture
//! ```
//!
//! - [`the_sealed_request_reaches_google_without_a_key`] sends the agent's real request (every
//!   declaration, the system instruction, the thinking level) with a bogus key through ferry-core's
//!   own HTTP client: Google reads the whole body and refuses the key, so it shows how long the
//!   transport alone takes, without running the model.
//! - [`gemini_accepts_the_declarations_and_picks_get_eta`] is one real call: one HTTP request with
//!   a 60 s timeout (no retry), timed to the headers and to the last body byte, so its latency can
//!   be compared with the client's per-attempt timeout (`gemini::CALL_TIMEOUT`). It checks that
//!   Gemini accepts all the declarations and picks get_eta for the demo's question.
//! - [`gemini_answers_a_trivial_call`] is one call with no tools: does the model answer at all.
//! - [`gemini_streams_the_full_request`] is the full request through `streamGenerateContent`,
//!   timed to the headers and to each chunk, with what each chunk holds (part kinds only).
//! - [`the_production_client_answers_the_full_request`] is the full request through
//!   `gemini::Gemini::generate_counted`: the production client, its timeout and its one retry.
//! - [`the_production_client_classifies_a_trip`] is the trip classifier's JSON request (the
//!   catalog's fixture restaurants) through the production client.
//!
//! `LIVE_GEMINI_MODEL=<model>` tries another model without touching .env.

mod common;

use std::time::{Duration, Instant};

use ferry_core::agent::gemini::{self, CALL_TIMEOUT, ToolMode, thinking_level};
use ferry_core::agent::privacy::Redactor;
use ferry_core::agent::tools;
use ferry_core::agent::turn::system_instruction;
use ferry_core::config::Config;
use ferry_core::state::http_client;
use reqwest::header::HeaderValue;
use serde_json::{Value, json};

/// `LIVE_GEMINI_MODEL` when set (to try another model without touching .env), else `GEMINI_MODEL`.
fn live_model(config: &Config) -> String {
    std::env::var("LIVE_GEMINI_MODEL")
        .ok()
        .filter(|m| !m.trim().is_empty())
        .or_else(|| config.gemini_model.clone())
        .expect("GEMINI_MODEL in the root .env")
        .trim()
        .trim_start_matches("models/")
        .to_owned()
}

/// The model, its generateContent URL and the agent's sealed request for the demo's question,
/// with the thinking level the client adds.
fn live_request(config: &Config) -> (String, String, Value) {
    let model = live_model(config);
    let base = config
        .gemini_base_url
        .as_deref()
        .unwrap_or(gemini::DEFAULT_BASE_URL)
        .trim_end_matches('/');
    let url = format!("{base}/v1beta/models/{model}:generateContent");
    let system = system_instruction(
        false,
        "Sat, Oct 10, 7:42 PM",
        "The user's saved defaults (use them for suggestions): default drop-off: {dropoff}; \
         favorite restaurants: rst_cosmic; default tip: $2.00; walking pace: brisk.",
    );
    let contents = [gemini::user_text("how long until my food gets here?")];
    let declarations = tools::declarations();
    assert_eq!(declarations.len(), tools::Tool::ALL.len());
    let body = gemini::build_request(&system, &contents, &declarations, ToolMode::Auto, 2048);
    let mut redactor = Redactor::new();
    for name in ["Jordan", "Maya", "Dana", "Leo"] {
        redactor.forbid(name);
    }
    let mut body = redactor
        .seal(body)
        .expect("the payload holds no name")
        .as_value()
        .clone();
    if let Some(level) = thinking_level(&model) {
        body["generationConfig"]["thinkingConfig"] = json!({"thinkingLevel": level});
    }
    (model, url, body)
}

#[tokio::test]
#[ignore = "manual: one request to Google's endpoint with a bogus key (no model runs)"]
async fn the_sealed_request_reaches_google_without_a_key() {
    let (config, _) = Config::load().unwrap();
    let (model, url, body) = live_request(&config);
    let started = Instant::now();
    let response = http_client()
        .unwrap()
        .post(&url)
        .timeout(CALL_TIMEOUT)
        .header("x-goog-api-key", "not-a-key")
        .json(&body)
        .send()
        .await
        .expect("Google answered");
    let status = response.status();
    let headers_ms = started.elapsed().as_millis();
    let _ = response.bytes().await;
    println!(
        "model {model}: {} declarations, body {} bytes, keyless status {status}, headers {headers_ms} ms, body {} ms",
        tools::declarations().len(),
        body.to_string().len(),
        started.elapsed().as_millis()
    );
    assert!(status.is_client_error(), "a bogus key is refused");
}

#[tokio::test]
#[ignore = "manual: one call to the real Gemini API"]
async fn gemini_accepts_the_declarations_and_picks_get_eta() {
    let (config, _) = Config::load().unwrap();
    let (model, url, body) = live_request(&config);
    let mut key = HeaderValue::from_str(
        config
            .gemini_api_key
            .as_ref()
            .expect("GEMINI_API_KEY in the root .env")
            .expose(),
    )
    .unwrap();
    key.set_sensitive(true);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap();
    let started = Instant::now();
    let response = client
        .post(&url)
        .header("x-goog-api-key", key)
        .json(&body)
        .send()
        .await;
    let headers_ms = started.elapsed().as_millis();
    let response = match response {
        Ok(response) => response,
        Err(err) => panic!(
            "no answer after {headers_ms} ms (timeout {}, connect {})",
            err.is_timeout(),
            err.is_connect()
        ),
    };
    let status = response.status();
    let bytes = response.bytes().await.expect("the body");
    let total_ms = started.elapsed().as_millis();
    let reply: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let parts = reply
        .pointer("/candidates/0/content/parts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let picked: Vec<&str> = parts
        .iter()
        .filter_map(|p| p.pointer("/functionCall/name").and_then(Value::as_str))
        .collect();
    let finish = reply.pointer("/candidates/0/finishReason").cloned();
    let tokens = reply.pointer("/usageMetadata").cloned();
    println!(
        "model {model}: {} declarations, thinkingLevel {:?}, body {} bytes, status {status}, \
         headers {headers_ms} ms, total {total_ms} ms (per-attempt timeout {} ms), \
         tools picked {picked:?}, finish {finish:?}, usage {tokens:?}",
        tools::declarations().len(),
        thinking_level(&model),
        body.to_string().len(),
        CALL_TIMEOUT.as_millis()
    );
    assert!(status.is_success(), "Gemini refused the request: {status}");
    assert_eq!(picked, ["get_eta"]);
}

/// The key from the root .env, marked sensitive.
fn live_key(config: &Config) -> HeaderValue {
    let mut key = HeaderValue::from_str(
        config
            .gemini_api_key
            .as_ref()
            .expect("GEMINI_API_KEY in the root .env")
            .expose(),
    )
    .unwrap();
    key.set_sensitive(true);
    key
}

#[tokio::test]
#[ignore = "manual: one trivial call (no tools) to the real Gemini API"]
async fn gemini_answers_a_trivial_call() {
    let (config, _) = Config::load().unwrap();
    let model = live_model(&config);
    let base = config
        .gemini_base_url
        .as_deref()
        .unwrap_or(gemini::DEFAULT_BASE_URL)
        .trim_end_matches('/');
    let url = format!("{base}/v1beta/models/{model}:generateContent");
    let body = gemini::build_request(
        "Answer with one word.",
        &[gemini::user_text("Say ok.")],
        &[],
        ToolMode::Auto,
        256,
    );
    let mut body = Redactor::new().seal(body).unwrap().as_value().clone();
    if let Some(level) = thinking_level(&model) {
        body["generationConfig"]["thinkingConfig"] = json!({"thinkingLevel": level});
    }
    let started = Instant::now();
    let response = reqwest::Client::new()
        .post(&url)
        .timeout(Duration::from_secs(25))
        .header("x-goog-api-key", live_key(&config))
        .json(&body)
        .send()
        .await;
    let headers_ms = started.elapsed().as_millis();
    let response = match response {
        Ok(response) => response,
        Err(err) => panic!(
            "model {model}: no answer after {headers_ms} ms (timeout {})",
            err.is_timeout()
        ),
    };
    let status = response.status();
    let bytes = response.bytes().await.unwrap_or_default();
    let reply: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    println!(
        "model {model}: trivial call, status {status}, headers {headers_ms} ms, total {} ms, \
         finish {:?}, text chars {}",
        started.elapsed().as_millis(),
        reply.pointer("/candidates/0/finishReason"),
        reply
            .pointer("/candidates/0/content/parts/0/text")
            .and_then(Value::as_str)
            .map_or(0, str::len),
    );
    assert!(status.is_success(), "{status}");
}

#[tokio::test]
#[ignore = "manual: one streamed call to the real Gemini API with every declaration"]
async fn gemini_streams_the_full_request() {
    let (config, _) = Config::load().unwrap();
    let (model, url, body) = live_request(&config);
    let url = url.replace(":generateContent", ":streamGenerateContent?alt=sse");
    let started = Instant::now();
    let response = reqwest::Client::new()
        .post(&url)
        .timeout(Duration::from_secs(40))
        .header("x-goog-api-key", live_key(&config))
        .json(&body)
        .send()
        .await;
    let headers_ms = started.elapsed().as_millis();
    let mut response = match response {
        Ok(response) => response,
        Err(err) => panic!(
            "model {model}: no headers after {headers_ms} ms (timeout {})",
            err.is_timeout()
        ),
    };
    println!(
        "model {model}: stream status {}, headers {headers_ms} ms",
        response.status()
    );
    let mut buffer = String::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                // Google's events end with CRLF CRLF.
                buffer.push_str(&String::from_utf8_lossy(&chunk).replace("\r\n", "\n"));
                while let Some(end) = buffer.find("\n\n") {
                    let event: String = buffer.drain(..end + 2).collect();
                    let data = event.trim().trim_start_matches("data:").trim();
                    let value: Value = serde_json::from_str(data).unwrap_or(Value::Null);
                    let parts = value
                        .pointer("/candidates/0/content/parts")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let kinds: Vec<String> = parts
                        .iter()
                        .map(|p| {
                            if let Some(name) = p.pointer("/functionCall/name") {
                                format!("call {name}")
                            } else if p.get("thought") == Some(&json!(true)) {
                                "thought".to_owned()
                            } else if p.get("text").is_some() {
                                "text".to_owned()
                            } else {
                                "other".to_owned()
                            }
                        })
                        .collect();
                    println!(
                        "  chunk at {} ms: parts {kinds:?}, finish {:?}, usage {:?}",
                        started.elapsed().as_millis(),
                        value.pointer("/candidates/0/finishReason"),
                        value.pointer("/usageMetadata"),
                    );
                }
            }
            Ok(None) => break,
            Err(err) => {
                println!(
                    "  stream ended with an error at {} ms (timeout {})",
                    started.elapsed().as_millis(),
                    err.is_timeout()
                );
                break;
            }
        }
    }
    println!(
        "  done at {} ms, {} bytes left unparsed",
        started.elapsed().as_millis(),
        buffer.trim().len()
    );
}

#[tokio::test]
#[ignore = "manual: one call through the production client (up to 2 attempts) to the real Gemini API"]
async fn the_production_client_answers_the_full_request() {
    let (mut config, _) = Config::load().unwrap();
    config.gemini_model = Some(live_model(&config));
    let (model, _, _) = live_request(&config);
    let client = gemini::Gemini::from_config(&http_client().unwrap(), &config).unwrap();
    let system = system_instruction(
        false,
        "Sat, Oct 10, 7:42 PM",
        "The user's saved defaults (use them for suggestions): default drop-off: {dropoff}; \
         favorite restaurants: rst_cosmic; default tip: $2.00; walking pace: brisk.",
    );
    let contents = [gemini::user_text("how long until my food gets here?")];
    let declarations = tools::declarations();
    let body = gemini::build_request(&system, &contents, &declarations, ToolMode::Auto, 2048);
    let mut redactor = Redactor::new();
    for name in ["Jordan", "Maya", "Dana", "Leo"] {
        redactor.forbid(name);
    }
    let sealed = redactor.seal(body).expect("the payload holds no name");
    let started = Instant::now();
    let (result, attempts) = client.generate_counted(&sealed).await;
    let elapsed = started.elapsed().as_millis();
    match &result {
        Ok(reply) => println!(
            "model {model}: {} declarations, thinkingLevel {:?}, attempts {attempts}, {elapsed} ms, \
             tools picked {:?}, finish {:?}, text chars {}",
            declarations.len(),
            thinking_level(&model),
            reply
                .calls
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            reply.finish_reason,
            reply.text.len()
        ),
        Err(err) => println!(
            "model {model}: {} declarations, attempts {attempts}, {elapsed} ms, failed: {err}",
            declarations.len()
        ),
    }
    let reply = result.expect("Gemini answered");
    assert_eq!(
        reply
            .calls
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["get_eta"]
    );
}

#[tokio::test]
#[ignore = "manual: one trip-classifier call (JSON answer) through the production client"]
async fn the_production_client_classifies_a_trip() {
    let (mut config, _) = Config::load().unwrap();
    let model = live_model(&config);
    config.gemini_model = Some(model.clone());
    let client = gemini::Gemini::from_config(&http_client().unwrap(), &config).unwrap();
    // The classifier's request as `trips::classify` builds it, over the fixture catalog.
    let restaurants = [
        ("rst_cosmic", "Cosmic Cantina", "mexican"),
        ("rst_supdogs", "Sup Dogs", "american"),
        ("rst_lime", "Lime Fresh", "mexican"),
    ];
    let list: Vec<String> = restaurants
        .iter()
        .map(|(id, name, cuisine)| format!("- {id}: {name} ({cuisine})"))
        .collect();
    let system = ferry_core::agent::trips::classifier_instruction(&list.join("\n"));
    let mut ids: Vec<Value> = restaurants.iter().map(|(id, _, _)| json!(id)).collect();
    ids.push(json!("none"));
    let schema = json!({
        "type": "object",
        "properties": {
            "is_trip": {"type": "boolean"},
            "restaurant_id": {"type": "string", "enum": ids},
        },
        "required": ["is_trip", "restaurant_id"],
    });
    let contents = [gemini::user_text(
        "heading to cosmic at 6, anyone want anything?",
    )];
    let body = gemini::build_json_request(&system, &contents, &schema, 1024);
    let sealed = Redactor::new().seal(body).expect("fixture text only");
    let started = Instant::now();
    let (result, attempts) = client.generate_counted(&sealed).await;
    let elapsed = started.elapsed().as_millis();
    let answer = result.as_ref().ok().and_then(gemini::parse_json_answer);
    println!(
        "model {model}: classifier, attempts {attempts}, {elapsed} ms, answer {answer:?}, error {:?}",
        result.as_ref().err()
    );
    let answer = answer.expect("a JSON answer");
    assert_eq!(answer["is_trip"], true);
    assert_eq!(answer["restaurant_id"], "rst_cosmic");
}
