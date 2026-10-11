//! The Gemini client: one `generateContent` call over REST (Google's Generative Language API,
//! v1beta) with function calling, a 12 s timeout per attempt and one retry, so a call with its
//! retry fits the agent's 25 s turn budget.
//!
//! - It sends only a [`Sealed`] body, so nothing reaches Gemini without passing
//!   `privacy::Redactor::seal`. The pure builders ([`build_request`], [`user_text`], ...) make the
//!   body; the turn loop seals it.
//! - The key travels in the `x-goog-api-key` header, never in the URL. Logs carry the model, the
//!   attempts, the latency and the error kind, never a request or response body (they hold user
//!   text) and never the key.
//! - [`Generated::content`] is the model's turn exactly as Gemini sent it. Thinking models (2.5 and
//!   later) refuse a conversation whose function calls come back without their
//!   `thoughtSignature`, so the turn loop appends it unchanged.
//! - Without `GEMINI_API_KEY` and `GEMINI_MODEL`, [`Gemini::from_config`] is `None` and the agent
//!   answers through its deterministic fallback, as it does when [`Gemini::generate`] fails.
//! - A Gemini 3 model gets `thinkingConfig.thinkingLevel: "low"` ([`thinking_level`]): its
//!   default (`medium` on 3.8 Flash) thinks longer than a one-line reply needs, and `minimal` is
//!   refused there. Replies come from the tools' lines anyway.
//! - A `functionCall` part's `id`, when Gemini sends one, comes back on its `functionResponse`.
//! - [`Gemini::for_app`] attaches the app's circuit breaker ([`super::breaker`]): after two failed
//!   calls in a row, calls return [`GeminiError::Open`] at once until a probe answers.

use std::fmt;
use std::time::{Duration, Instant};

use reqwest::header::HeaderValue;
use serde_json::{Value, json};

use super::breaker::Breaker;
use super::privacy::Sealed;
use crate::config::Config;
use crate::state::AppState;

/// Google's endpoint, the default for `GEMINI_BASE_URL`.
pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";

/// Per attempt, connect to last body byte. There is one retry: 2 × 12 s + the retry delay fits
/// the turn's 25 s budget.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(12);

/// Wait before the one retry.
pub const RETRY_DELAY: Duration = Duration::from_millis(300);

/// The header that carries the API key.
const API_KEY_HEADER: &str = "x-goog-api-key";

/// The client. Cheap to clone. `Debug` prints the model only.
#[derive(Clone)]
pub struct Gemini {
    http: reqwest::Client,
    /// Without a trailing slash.
    base_url: String,
    /// `GEMINI_API_KEY`, marked sensitive so reqwest never prints it.
    api_key: HeaderValue,
    /// Without a `models/` prefix.
    model: String,
    /// Per attempt: [`CALL_TIMEOUT`], shorter in tests.
    timeout: Duration,
    /// The app's breaker ([`Gemini::for_app`]); None for a bare client.
    breaker: Option<Breaker>,
}

impl fmt::Debug for Gemini {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Gemini")
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

/// One reply from [`Gemini::generate`].
#[derive(Debug, Clone)]
pub struct Generated {
    /// The first candidate's `content` exactly as Gemini sent it (role "model", every part,
    /// including `thoughtSignature` and `thought` parts), to append to the conversation unchanged
    /// on the next round. Thinking models require function calls to come back with their
    /// `thoughtSignature`, so never rebuild it.
    pub content: Value,
    /// Every `functionCall` part, in order.
    pub calls: Vec<FunctionCall>,
    /// The text parts joined (parts with `"thought": true` skipped), trimmed.
    pub text: String,
    /// The candidate's `finishReason`, if any.
    pub finish_reason: Option<String>,
    /// Wall time of the whole call, the retry included, in ms.
    pub latency_ms: u64,
    /// 1 or 2.
    pub attempts: u32,
}

/// One tool Gemini asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    /// Always an object: `{}` when Gemini sent none.
    pub args: Value,
    /// The call's id, when Gemini sent one; its `functionResponse` carries it back.
    pub id: Option<String>,
}

/// Why [`Gemini::generate`] gave no reply. The messages hold no payload, so they can be logged.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GeminiError {
    #[error("Gemini timed out")]
    Timeout,
    #[error("Gemini could not be reached")]
    Network,
    #[error("Gemini answered {0}")]
    Status(u16),
    /// 200 but no usable candidate: no candidates (e.g. `promptFeedback.blockReason`), a candidate
    /// with neither text nor function calls (e.g. `finishReason` SAFETY), or unparsable JSON. The
    /// string is a short reason with no payload content.
    #[error("Gemini gave no usable answer: {0}")]
    Empty(String),
    /// The breaker is open: nothing was sent ([`super::breaker`]).
    #[error("Gemini's breaker is open")]
    Open,
}

impl GeminiError {
    /// The kind, for logs: "timeout", "network", "status", "empty" or "breaker_open".
    pub fn label(&self) -> &'static str {
        match self {
            GeminiError::Timeout => "timeout",
            GeminiError::Network => "network",
            GeminiError::Status(_) => "status",
            GeminiError::Empty(_) => "empty",
            GeminiError::Open => "breaker_open",
        }
    }

    /// A timeout, a connection error, 429 or a 5xx: worth the one retry. Never another 4xx, and
    /// never `Empty`, which is a 200.
    fn is_retryable(&self) -> bool {
        match self {
            GeminiError::Timeout | GeminiError::Network => true,
            GeminiError::Status(code) => *code == 429 || (500..=599).contains(code),
            GeminiError::Empty(_) | GeminiError::Open => false,
        }
    }

    fn status(&self) -> Option<u16> {
        match self {
            GeminiError::Status(code) => Some(*code),
            _ => None,
        }
    }
}

impl Gemini {
    /// The client for `GEMINI_MODEL` at `GEMINI_BASE_URL` (Google's when unset), sending with
    /// `http` (the app's shared client; each attempt sets its own timeout). `None` when
    /// `GEMINI_API_KEY` or `GEMINI_MODEL` is unset: the agent then answers with its fallback.
    pub fn from_config(http: &reqwest::Client, config: &Config) -> Option<Self> {
        let key = config.gemini_api_key.as_ref()?;
        // Accept "models/gemini-..." too: the URL adds the prefix itself.
        let model = config.gemini_model.as_deref()?.trim();
        let model = model.strip_prefix("models/").unwrap_or(model);
        if model.is_empty() {
            return None;
        }
        let Ok(mut api_key) = HeaderValue::from_str(key.expose()) else {
            tracing::warn!("GEMINI_API_KEY is not a valid header value; Gemini is off");
            return None;
        };
        api_key.set_sensitive(true);
        let base_url = config
            .gemini_base_url
            .as_deref()
            .unwrap_or(DEFAULT_BASE_URL)
            .trim_end_matches('/')
            .to_owned();
        Some(Self {
            http: http.clone(),
            base_url,
            api_key,
            model: model.to_owned(),
            timeout: CALL_TIMEOUT,
            breaker: None,
        })
    }

    /// The client for the app's config ([`Gemini::from_config`]) with the app's breaker. Every
    /// agent call goes through this one.
    pub fn for_app(state: &AppState) -> Option<Self> {
        let mut gemini = Self::from_config(&state.http, &state.config)?;
        gemini.breaker = Some(state.gemini_breaker.clone());
        Some(gemini)
    }

    /// The same client with a shorter timeout per attempt, so the timeout tests run fast.
    #[cfg(test)]
    pub(crate) fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// One `generateContent` call. Each attempt times out after [`CALL_TIMEOUT`]. One retry, after
    /// [`RETRY_DELAY`], on a timeout, a connection error, HTTP 429 or a 5xx; never on another 4xx
    /// or an unusable 200. Returns the parsed reply, or the last error.
    pub async fn generate(&self, request: &Sealed) -> Result<Generated, GeminiError> {
        self.generate_counted(request).await.0
    }

    /// [`Gemini::generate`], with how many attempts it made (1 or 2) on either outcome, for the
    /// agent's log. With the breaker open: [`GeminiError::Open`] at once, after 0 attempts.
    pub async fn generate_counted(
        &self,
        request: &Sealed,
    ) -> (Result<Generated, GeminiError>, u32) {
        if self.breaker.as_ref().is_some_and(Breaker::is_open) {
            return (Err(GeminiError::Open), 0);
        }
        let (result, attempts) = self.generate_unguarded(request).await;
        if let Some(breaker) = &self.breaker {
            breaker.record(&result, self);
        }
        (result, attempts)
    }

    /// The same client without a breaker, for the breaker's own probe: the probe task must not
    /// hold the breaker it reports to.
    pub(crate) fn without_breaker(&self) -> Self {
        Self {
            breaker: None,
            ..self.clone()
        }
    }

    /// The breaker's probe: ONE attempt, whose outcome the probe itself judges.
    pub(crate) async fn probe(&self, request: &Sealed) -> Result<(), GeminiError> {
        self.attempt(request.as_value()).await.map(|_| ())
    }

    async fn generate_unguarded(&self, request: &Sealed) -> (Result<Generated, GeminiError>, u32) {
        let started = Instant::now();
        let mut attempts = 1;
        let mut result = self.attempt(request.as_value()).await;
        if let Err(err) = &result
            && err.is_retryable()
        {
            tracing::debug!(
                model = %self.model,
                error = err.label(),
                status = ?err.status(),
                "gemini attempt failed; retrying once"
            );
            tokio::time::sleep(RETRY_DELAY).await;
            attempts = 2;
            result = self.attempt(request.as_value()).await;
        }
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        match result {
            Ok(reply) => {
                tracing::info!(
                    model = %self.model,
                    attempts,
                    latency_ms,
                    calls = reply.calls.len(),
                    finish_reason = reply.finish_reason.as_deref().unwrap_or("none"),
                    "gemini call"
                );
                let generated = Generated {
                    content: reply.content,
                    calls: reply.calls,
                    text: reply.text,
                    finish_reason: reply.finish_reason,
                    latency_ms,
                    attempts,
                };
                (Ok(generated), attempts)
            }
            Err(err) => {
                tracing::warn!(
                    model = %self.model,
                    attempts,
                    latency_ms,
                    error = err.label(),
                    status = ?err.status(),
                    "gemini call failed"
                );
                (Err(err), attempts)
            }
        }
    }

    /// One request: the parsed reply, or why there is none.
    async fn attempt(&self, body: &Value) -> Result<Reply, GeminiError> {
        let url = format!(
            "{}/v1beta/models/{}:generateContent",
            self.base_url, self.model
        );
        // The thinking level is fixed text added after sealing; it holds nothing of the user's.
        let mut body = body.clone();
        if let Some(level) = thinking_level(&self.model)
            && body["generationConfig"].is_object()
        {
            body["generationConfig"]["thinkingConfig"] = json!({"thinkingLevel": level});
        }
        // `json` sets `content-type: application/json`.
        let response = self
            .http
            .post(url)
            .timeout(self.timeout)
            .header(API_KEY_HEADER, self.api_key.clone())
            .json(&body)
            .send()
            .await
            .map_err(classify)?;
        let status = response.status();
        if !status.is_success() {
            // The error body is not read: it may quote the request.
            return Err(GeminiError::Status(status.as_u16()));
        }
        let bytes = response.bytes().await.map_err(classify)?;
        let parsed: Value = serde_json::from_slice(&bytes)
            .map_err(|_| GeminiError::Empty("unparsable JSON".to_owned()))?;
        parse_reply(&parsed)
    }
}

/// A reqwest error as its kind only; the error itself never reaches a log.
fn classify(err: reqwest::Error) -> GeminiError {
    if err.is_timeout() {
        GeminiError::Timeout
    } else {
        GeminiError::Network
    }
}

/// The usable part of a 200 answer, before timing is added.
#[derive(Debug)]
struct Reply {
    content: Value,
    calls: Vec<FunctionCall>,
    text: String,
    finish_reason: Option<String>,
}

/// The first candidate of a `generateContent` answer, or `Empty` with a reason.
fn parse_reply(body: &Value) -> Result<Reply, GeminiError> {
    let Some(candidate) = body
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
    else {
        let reason = match body
            .pointer("/promptFeedback/blockReason")
            .and_then(Value::as_str)
            .and_then(enum_name)
        {
            Some(block) => format!("no candidates (blockReason {block})"),
            None => "no candidates".to_owned(),
        };
        return Err(GeminiError::Empty(reason));
    };
    let finish_reason = candidate
        .get("finishReason")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut content = candidate.get("content").cloned().unwrap_or(Value::Null);
    // The role is what lets the turn go back as the model's; Gemini always sends it.
    if let Some(turn) = content.as_object_mut()
        && !turn.contains_key("role")
    {
        turn.insert("role".to_owned(), json!("model"));
    }
    let parts: &[Value] = content
        .get("parts")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice);

    let mut calls = Vec::new();
    let mut text = String::new();
    for part in parts {
        if let Some(call) = part.get("functionCall") {
            if let Some(name) = call.get("name").and_then(Value::as_str) {
                let args = call
                    .get("args")
                    .filter(|a| a.is_object())
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                calls.push(FunctionCall {
                    name: name.to_owned(),
                    args,
                    id: call.get("id").and_then(Value::as_str).map(str::to_owned),
                });
            }
            continue;
        }
        let thought = part.get("thought").and_then(Value::as_bool) == Some(true);
        if let Some(t) = part.get("text").and_then(Value::as_str)
            && !thought
        {
            // Gemini splits one message across consecutive text parts: no separator.
            text.push_str(t);
        }
    }
    let text = text.trim().to_owned();

    if calls.is_empty() && text.is_empty() {
        let reason = match finish_reason.as_deref().and_then(enum_name) {
            Some(finish) => format!("no text or function calls (finishReason {finish})"),
            None => "no text or function calls".to_owned(),
        };
        return Err(GeminiError::Empty(reason));
    }
    // Text or a call came from `content.parts`, so `content` is an object here.
    Ok(Reply {
        content,
        calls,
        text,
        finish_reason,
    })
}

/// `value` when it looks like one of Gemini's enum names (`SAFETY`, `MAX_TOKENS`), so a reason
/// string never carries anything else.
fn enum_name(value: &str) -> Option<&str> {
    let ok = !value.is_empty()
        && value.len() <= 40
        && value
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    ok.then_some(value)
}

/// How function calling may be used on a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolMode {
    /// Gemini chooses between a reply and function calls.
    Auto,
    /// The tools stay declared but Gemini must answer in text (the last round of a turn).
    None,
}

impl ToolMode {
    fn as_str(self) -> &'static str {
        match self {
            ToolMode::Auto => "AUTO",
            ToolMode::None => "NONE",
        }
    }
}

/// `{"role": "user", "parts": [{"text": text}]}`.
pub fn user_text(text: &str) -> Value {
    json!({"role": "user", "parts": [{"text": text}]})
}

/// `{"role": "model", "parts": [{"text": text}]}`, for replaying earlier turns of a DM.
pub fn model_text(text: &str) -> Value {
    json!({"role": "model", "parts": [{"text": text}]})
}

/// One user turn holding every result of one round, in order:
/// `{"role": "user", "parts": [{"functionResponse": {"name", "response"}}, ...]}`, with the call's
/// `id` when it had one. A `response` must be a JSON object, so any other value goes in as
/// `{"result": value}`.
pub fn function_responses(results: &[(FunctionCall, Value)]) -> Value {
    let parts: Vec<Value> = results
        .iter()
        .map(|(call, value)| {
            let response = if value.is_object() {
                value.clone()
            } else {
                json!({"result": value})
            };
            let mut part = json!({"name": call.name, "response": response});
            if let Some(id) = &call.id {
                part["id"] = json!(id);
            }
            json!({"functionResponse": part})
        })
        .collect();
    json!({"role": "user", "parts": parts})
}

/// The thinking level `model` gets: `low` for Gemini 3 models, nothing (the model's default) for
/// any other.
pub fn thinking_level(model: &str) -> Option<&'static str> {
    model.starts_with("gemini-3").then_some("low")
}

/// The whole `generateContent` body: `systemInstruction` (left out when `system` is blank, since
/// Gemini refuses an empty text part), `contents`, `tools` with `declarations` and its `toolConfig`
/// (both only when `declarations` is non-empty), and `generationConfig`.
pub fn build_request(
    system: &str,
    contents: &[Value],
    declarations: &[Value],
    mode: ToolMode,
    max_output_tokens: u32,
) -> Value {
    let mut body = base_request(system, contents, max_output_tokens);
    if !declarations.is_empty() {
        body["tools"] = json!([{"functionDeclarations": declarations}]);
        body["toolConfig"] = json!({"functionCallingConfig": {"mode": mode.as_str()}});
    }
    body
}

/// The same body for a JSON answer (trip classification): no tools, and `generationConfig` asks
/// for `application/json` matching `schema` (an OpenAPI-style `responseSchema`).
pub fn build_json_request(
    system: &str,
    contents: &[Value],
    schema: &Value,
    max_output_tokens: u32,
) -> Value {
    let mut body = base_request(system, contents, max_output_tokens);
    body["generationConfig"]["responseMimeType"] = json!("application/json");
    body["generationConfig"]["responseSchema"] = schema.clone();
    body
}

fn base_request(system: &str, contents: &[Value], max_output_tokens: u32) -> Value {
    let mut body = json!({
        "contents": contents,
        // No temperature: the model's default. Google advises against lowering it on Gemini 3
        // (GEMINI_MODEL is a Gemini 3 Flash), and the facts come from the tools anyway.
        "generationConfig": {"maxOutputTokens": max_output_tokens},
    });
    if !system.trim().is_empty() {
        body["systemInstruction"] = json!({"parts": [{"text": system}]});
    }
    body
}

/// `generated.text` parsed as JSON, also when it comes in a ```` ```json ```` fenced block. `None`
/// when it is not JSON.
pub fn parse_json_answer(generated: &Generated) -> Option<Value> {
    let text = generated.text.trim();
    if let Ok(value) = serde_json::from_str(text) {
        return Some(value);
    }
    let fenced = fenced_block(text)?;
    serde_json::from_str(fenced).ok()
}

/// The inside of the first ```` ``` ```` fenced block in `text`, without its language tag.
fn fenced_block(text: &str) -> Option<&str> {
    let start = text.find("```")? + 3;
    let rest = &text[start..];
    // The language tag runs to the end of the opening line.
    let body_start = rest.find('\n').map_or(0, |i| i + 1);
    let tag = rest[..body_start].trim();
    let rest = if tag.chars().all(|c| c.is_ascii_alphanumeric()) {
        &rest[body_start..]
    } else {
        rest
    };
    let end = rest.find("```").unwrap_or(rest.len());
    Some(rest[..end].trim())
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::body::Bytes;
    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, StatusCode, Uri};
    use axum::response::{IntoResponse, Response};
    use axum::routing::post;

    use super::*;

    const KEY: &str = "test-key-not-real";
    const MODEL: &str = "gemini-test";

    fn config(pairs: &[(&str, &str)]) -> Config {
        let mut vars = HashMap::from([(
            "DATABASE_URL".to_owned(),
            "postgres://u:p@127.0.0.1/db".to_owned(),
        )]);
        for (name, value) in pairs {
            vars.insert((*name).to_owned(), (*value).to_owned());
        }
        Config::from_vars(&vars).unwrap()
    }

    fn generated(text: &str) -> Generated {
        Generated {
            content: model_text(text),
            calls: Vec::new(),
            text: text.to_owned(),
            finish_reason: Some("STOP".to_owned()),
            latency_ms: 0,
            attempts: 1,
        }
    }

    fn text_answer(text: &str) -> Value {
        json!({"candidates": [{"content": {"role": "model", "parts": [{"text": text}]}, "finishReason": "STOP"}]})
    }

    // ---------- Builders ----------

    fn call(name: &str, id: Option<&str>) -> FunctionCall {
        FunctionCall {
            name: name.to_owned(),
            args: json!({}),
            id: id.map(str::to_owned),
        }
    }

    #[test]
    fn gemini_3_models_think_low() {
        assert_eq!(thinking_level("gemini-3.8-flash"), Some("low"));
        assert_eq!(thinking_level("gemini-3-flash-preview"), Some("low"));
        assert_eq!(thinking_level("gemini-test"), None);
        assert_eq!(thinking_level("gemini-2.5-flash"), None);
    }

    #[test]
    fn builds_turns_and_function_responses() {
        assert_eq!(
            user_text("hi"),
            json!({"role": "user", "parts": [{"text": "hi"}]})
        );
        assert_eq!(
            model_text("yo"),
            json!({"role": "model", "parts": [{"text": "yo"}]})
        );
        assert_eq!(
            function_responses(&[
                (call("get_eta", None), json!({"eta_minutes": 4})),
                (call("list_open_runs", Some("c-2")), json!([1, 2])),
                (call("get_wallet", None), json!("ok")),
            ]),
            json!({"role": "user", "parts": [
                {"functionResponse": {"name": "get_eta", "response": {"eta_minutes": 4}}},
                {"functionResponse": {"name": "list_open_runs", "id": "c-2", "response": {"result": [1, 2]}}},
                {"functionResponse": {"name": "get_wallet", "response": {"result": "ok"}}},
            ]})
        );
    }

    #[test]
    fn builds_the_request_with_and_without_tools() {
        let contents = [user_text("where is {runner}?")];
        let declarations =
            [json!({"name": "get_eta", "description": "ETA", "parameters": {"type": "object"}})];
        assert_eq!(
            build_request("Be brief.", &contents, &declarations, ToolMode::Auto, 512),
            json!({
                "systemInstruction": {"parts": [{"text": "Be brief."}]},
                "contents": [{"role": "user", "parts": [{"text": "where is {runner}?"}]}],
                "tools": [{"functionDeclarations": [{"name": "get_eta", "description": "ETA", "parameters": {"type": "object"}}]}],
                "toolConfig": {"functionCallingConfig": {"mode": "AUTO"}},
                "generationConfig": {"maxOutputTokens": 512},
            })
        );
        let none = build_request("Be brief.", &contents, &declarations, ToolMode::None, 64);
        assert_eq!(none["toolConfig"]["functionCallingConfig"]["mode"], "NONE");

        // No declarations: no tools and no toolConfig, whatever the mode.
        let plain = build_request("Be brief.", &contents, &[], ToolMode::Auto, 64);
        assert!(plain.get("tools").is_none() && plain.get("toolConfig").is_none());
        // A blank system prompt is left out.
        assert!(
            build_request(" ", &contents, &[], ToolMode::Auto, 64)
                .get("systemInstruction")
                .is_none()
        );
    }

    #[test]
    fn builds_the_json_request() {
        let schema = json!({"type": "OBJECT", "properties": {"trip": {"type": "BOOLEAN"}}});
        let body = build_json_request("Classify.", &[user_text("heading to X")], &schema, 128);
        assert_eq!(
            body,
            json!({
                "systemInstruction": {"parts": [{"text": "Classify."}]},
                "contents": [{"role": "user", "parts": [{"text": "heading to X"}]}],
                "generationConfig": {
                    "maxOutputTokens": 128,
                    "responseMimeType": "application/json",
                    "responseSchema": schema,
                },
            })
        );
    }

    // ---------- Parsing ----------

    #[test]
    fn keeps_the_content_and_extracts_calls_in_order() {
        let content = json!({"role": "model", "parts": [
            {"text": "Planning the lookup.", "thought": true},
            {"functionCall": {"name": "get_eta", "args": {"run": "run_demo"}}, "thoughtSignature": "c2lnbmF0dXJl"},
            {"functionCall": {"name": "list_open_runs"}},
        ]});
        let reply = parse_reply(&json!({
            "candidates": [{"content": content, "finishReason": "STOP", "index": 0}],
            "usageMetadata": {"totalTokenCount": 42},
        }))
        .unwrap();
        assert_eq!(reply.content, content, "the content goes back unchanged");
        assert_eq!(
            reply.calls,
            [
                FunctionCall {
                    name: "get_eta".to_owned(),
                    args: json!({"run": "run_demo"}),
                    id: None,
                },
                FunctionCall {
                    name: "list_open_runs".to_owned(),
                    args: json!({}),
                    id: None,
                },
            ]
        );
        assert_eq!(reply.text, "", "thought text is not reply text");
        assert_eq!(reply.finish_reason.as_deref(), Some("STOP"));
    }

    #[test]
    fn joins_text_parts_and_skips_thoughts() {
        let reply = parse_reply(
            &json!({"candidates": [{"content": {"role": "model", "parts": [
                {"text": "secret plan", "thought": true},
                {"text": " {runner} is 4 min "},
                {"text": "away. ", "thought": false},
            ]}}]}),
        )
        .unwrap();
        assert_eq!(reply.text, "{runner} is 4 min away.");
        assert!(reply.calls.is_empty());
        assert_eq!(reply.finish_reason, None);

        // A reply cut short still has its text; the caller sees the reason.
        let cut = parse_reply(&json!({"candidates": [{"content": {"role": "model", "parts": [{"text": "Half"}]}, "finishReason": "MAX_TOKENS"}]})).unwrap();
        assert_eq!(cut.text, "Half");
        assert_eq!(cut.finish_reason.as_deref(), Some("MAX_TOKENS"));
    }

    #[test]
    fn unusable_answers_are_empty() {
        assert_eq!(
            parse_reply(&json!({"candidates": []})).unwrap_err(),
            GeminiError::Empty("no candidates".to_owned())
        );
        assert_eq!(
            parse_reply(&json!({"promptFeedback": {"blockReason": "SAFETY"}})).unwrap_err(),
            GeminiError::Empty("no candidates (blockReason SAFETY)".to_owned())
        );
        // A reason that is not an enum name is not quoted.
        assert_eq!(
            parse_reply(&json!({"promptFeedback": {"blockReason": "maya said hi"}})).unwrap_err(),
            GeminiError::Empty("no candidates".to_owned())
        );
        assert_eq!(
            parse_reply(&json!({"candidates": [{"finishReason": "SAFETY", "safetyRatings": []}]}))
                .unwrap_err(),
            GeminiError::Empty("no text or function calls (finishReason SAFETY)".to_owned())
        );
        assert_eq!(
            parse_reply(&json!({"candidates": [{"content": {"role": "model"}, "finishReason": "MAX_TOKENS"}]}))
                .unwrap_err(),
            GeminiError::Empty("no text or function calls (finishReason MAX_TOKENS)".to_owned())
        );
        // Only thoughts is no answer either.
        assert!(matches!(
            parse_reply(
                &json!({"candidates": [{"content": {"role": "model", "parts": [{"text": "hmm", "thought": true}]}}]})
            ),
            Err(GeminiError::Empty(_))
        ));
    }

    #[test]
    fn parses_json_answers() {
        assert_eq!(
            parse_json_answer(&generated(
                r#"{"trip": true, "restaurant_id": "rst_cosmic"}"#
            )),
            Some(json!({"trip": true, "restaurant_id": "rst_cosmic"}))
        );
        assert_eq!(
            parse_json_answer(&generated("```json\n{\"trip\": false}\n```")),
            Some(json!({"trip": false}))
        );
        assert_eq!(
            parse_json_answer(&generated("Here it is:\n```\n[1, 2]\n```\nDone.")),
            Some(json!([1, 2]))
        );
        assert_eq!(parse_json_answer(&generated("not json")), None);
        assert_eq!(parse_json_answer(&generated("```json\n{oops\n```")), None);
    }

    #[test]
    fn labels_and_retry_rules() {
        assert_eq!(GeminiError::Timeout.label(), "timeout");
        assert_eq!(GeminiError::Network.label(), "network");
        assert_eq!(GeminiError::Status(503).label(), "status");
        assert_eq!(GeminiError::Empty("x".into()).label(), "empty");
        assert_eq!(GeminiError::Open.label(), "breaker_open");
        for retry in [
            GeminiError::Timeout,
            GeminiError::Network,
            GeminiError::Status(429),
            GeminiError::Status(500),
            GeminiError::Status(503),
        ] {
            assert!(retry.is_retryable(), "{retry:?}");
        }
        for once in [
            GeminiError::Status(400),
            GeminiError::Status(403),
            GeminiError::Status(404),
            GeminiError::Empty("x".into()),
            GeminiError::Open,
        ] {
            assert!(!once.is_retryable(), "{once:?}");
        }
    }

    // ---------- The client ----------

    #[test]
    fn from_config_needs_key_and_model_and_hides_the_key() {
        let http = reqwest::Client::new();
        assert!(Gemini::from_config(&http, &config(&[])).is_none());
        assert!(Gemini::from_config(&http, &config(&[("GEMINI_API_KEY", KEY)])).is_none());
        assert!(Gemini::from_config(&http, &config(&[("GEMINI_MODEL", MODEL)])).is_none());

        let gemini = Gemini::from_config(
            &http,
            &config(&[
                ("GEMINI_API_KEY", KEY),
                ("GEMINI_MODEL", "models/gemini-test"),
            ]),
        )
        .unwrap();
        assert_eq!(gemini.model(), MODEL);
        assert_eq!(gemini.base_url, DEFAULT_BASE_URL);
        assert_eq!(gemini.timeout, CALL_TIMEOUT);
        let debug = format!("{gemini:?}");
        assert!(!debug.contains(KEY), "{debug}");
        assert_eq!(debug, r#"Gemini { model: "gemini-test", .. }"#);
    }

    /// How the stub answers one request.
    #[derive(Clone)]
    enum Mode {
        /// 200 with this body.
        Ok(Value),
        /// This status with a Google-style error body.
        Status(u16),
        /// Sleeps this long, then answers 200 with a text reply.
        Slow(Duration),
    }

    /// One request the stub received.
    #[derive(Debug, Clone)]
    struct Seen {
        rest: String,
        query: Option<String>,
        key: Option<String>,
        content_type: Option<String>,
        body: Value,
    }

    #[derive(Clone, Default)]
    struct Stub {
        script: Arc<Mutex<VecDeque<Mode>>>,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    struct StubServer {
        base_url: String,
        stub: Stub,
        task: tokio::task::JoinHandle<()>,
    }

    impl StubServer {
        fn seen(&self) -> Vec<Seen> {
            self.stub.seen.lock().unwrap().clone()
        }
    }

    impl Drop for StubServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn handle(
        State(stub): State<Stub>,
        Path(rest): Path<String>,
        uri: Uri,
        headers: HeaderMap,
        body: Bytes,
    ) -> Response {
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        stub.seen.lock().unwrap().push(Seen {
            rest,
            query: uri.query().map(str::to_owned),
            key: header("x-goog-api-key"),
            content_type: header("content-type"),
            body: serde_json::from_slice(&body).unwrap_or(Value::Null),
        });
        let mode = stub.script.lock().unwrap().pop_front();
        match mode.expect("the stub got more requests than scripted") {
            Mode::Ok(body) => axum::Json(body).into_response(),
            Mode::Status(code) => {
                let status = StatusCode::from_u16(code).unwrap();
                let error = json!({"error": {"code": code, "message": "stub", "status": "STUB"}});
                (status, axum::Json(error)).into_response()
            }
            Mode::Slow(wait) => {
                tokio::time::sleep(wait).await;
                axum::Json(text_answer("late")).into_response()
            }
        }
    }

    /// A stand-in for Gemini on an OS-assigned local port that answers with `script`, in order.
    async fn stub(script: Vec<Mode>) -> StubServer {
        let stub = Stub {
            script: Arc::new(Mutex::new(script.into())),
            seen: Arc::default(),
        };
        let app = Router::new()
            .route("/v1beta/models/{*rest}", post(handle))
            .with_state(stub.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        StubServer {
            base_url: format!("http://{addr}"),
            stub,
            task,
        }
    }

    fn client(base_url: &str) -> Gemini {
        let config = config(&[
            ("GEMINI_API_KEY", KEY),
            ("GEMINI_MODEL", MODEL),
            ("GEMINI_BASE_URL", base_url),
        ]);
        Gemini::from_config(&crate::state::http_client().unwrap(), &config).unwrap()
    }

    fn sealed() -> Sealed {
        Sealed::unchecked(build_request(
            "Be brief.",
            &[user_text("where is {runner}?")],
            &[],
            ToolMode::Auto,
            256,
        ))
    }

    #[tokio::test]
    async fn posts_to_the_model_with_the_key_in_a_header() {
        let answer = json!({"candidates": [{"content": {"role": "model", "parts": [
            {"functionCall": {"name": "get_eta", "args": {"run": "run_demo"}}, "thoughtSignature": "c2ln"},
        ]}, "finishReason": "STOP"}]});
        let server = stub(vec![Mode::Ok(answer.clone())]).await;
        let request = sealed();
        let generated = client(&server.base_url).generate(&request).await.unwrap();

        let seen = server.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].rest, "gemini-test:generateContent");
        assert_eq!(seen[0].key.as_deref(), Some(KEY));
        assert!(
            seen[0].query.as_deref().is_none_or(|q| !q.contains("key=")),
            "{:?}",
            seen[0].query
        );
        assert_eq!(seen[0].content_type.as_deref(), Some("application/json"));
        assert_eq!(&seen[0].body, request.as_value());

        assert_eq!(generated.attempts, 1);
        assert_eq!(generated.content, answer["candidates"][0]["content"]);
        assert_eq!(generated.calls.len(), 1);
        assert_eq!(generated.calls[0].name, "get_eta");
        assert_eq!(generated.finish_reason.as_deref(), Some("STOP"));
    }

    #[tokio::test]
    async fn a_503_then_a_200_succeeds_on_the_retry() {
        let server = stub(vec![Mode::Status(503), Mode::Ok(text_answer("Hi"))]).await;
        let generated = client(&server.base_url).generate(&sealed()).await.unwrap();
        assert_eq!(generated.attempts, 2);
        assert_eq!(generated.text, "Hi");
        assert!(generated.latency_ms >= RETRY_DELAY.as_millis() as u64);
        assert_eq!(server.seen().len(), 2);
    }

    #[tokio::test]
    async fn a_429_is_retried() {
        let server = stub(vec![Mode::Status(429), Mode::Ok(text_answer("Hi"))]).await;
        let generated = client(&server.base_url).generate(&sealed()).await.unwrap();
        assert_eq!(generated.attempts, 2);
    }

    #[tokio::test]
    async fn two_503s_fail_with_the_status() {
        let server = stub(vec![Mode::Status(503), Mode::Status(503)]).await;
        let err = client(&server.base_url)
            .generate(&sealed())
            .await
            .unwrap_err();
        assert_eq!(err, GeminiError::Status(503));
        assert_eq!(err.to_string(), "Gemini answered 503");
        assert_eq!(server.seen().len(), 2);
    }

    #[tokio::test]
    async fn a_400_is_not_retried() {
        let server = stub(vec![Mode::Status(400)]).await;
        let err = client(&server.base_url)
            .generate(&sealed())
            .await
            .unwrap_err();
        assert_eq!(err, GeminiError::Status(400));
        assert_eq!(server.seen().len(), 1);
    }

    #[tokio::test]
    async fn an_unusable_200_is_not_retried() {
        let server = stub(vec![Mode::Ok(json!({"candidates": []}))]).await;
        let err = client(&server.base_url)
            .generate(&sealed())
            .await
            .unwrap_err();
        assert!(matches!(err, GeminiError::Empty(_)), "{err:?}");
        assert_eq!(server.seen().len(), 1);
    }

    #[tokio::test]
    async fn a_timeout_is_retried_then_reported() {
        let server = stub(vec![
            Mode::Slow(Duration::from_millis(600)),
            Mode::Slow(Duration::from_millis(600)),
        ])
        .await;
        let gemini = client(&server.base_url).with_timeout(Duration::from_millis(150));
        let started = Instant::now();
        let err = gemini.generate(&sealed()).await.unwrap_err();
        assert_eq!(err, GeminiError::Timeout);
        assert_eq!(server.seen().len(), 2);
        assert!(started.elapsed() < Duration::from_millis(1500));
    }

    #[tokio::test]
    async fn a_timeout_then_an_answer_succeeds() {
        let server = stub(vec![
            Mode::Slow(Duration::from_millis(600)),
            Mode::Ok(text_answer("On time")),
        ])
        .await;
        let gemini = client(&server.base_url).with_timeout(Duration::from_millis(150));
        let generated = gemini.generate(&sealed()).await.unwrap();
        assert_eq!(generated.attempts, 2);
        assert_eq!(generated.text, "On time");
    }

    #[tokio::test]
    async fn a_closed_port_is_a_network_error() {
        // A port that was just free: nothing listens there.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let err = client(&base_url).generate(&sealed()).await.unwrap_err();
        assert_eq!(err, GeminiError::Network);
        assert_eq!(err.label(), "network");
    }
}
