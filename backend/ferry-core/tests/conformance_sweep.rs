//! The contract-wide sweep (conformance task): no request to the API produces a 5xx, and every
//! operation answers in the contract's shapes.
//!
//! It reads contract/openapi.yaml and, for every operation in `paths`:
//! - calls it once with valid inputs from the seeded demo (Nessie in mock mode, Storage on a local
//!   stub, OSRM refused at once) and validates the answer against the operation's own response
//!   schema;
//! - calls it with hostile inputs built from the contract: NUL and other control characters in
//!   each path and query parameter and in each string field of the body (at any depth), a 1 MB
//!   string in each body string (60 KB in a path or query, about the most `http::Uri` holds; a
//!   real server refuses longer URIs before ferry-core sees them), wrong JSON types, each required
//!   field missing, malformed and oversized bodies, numbers out of range, and date-times with
//!   absurd offsets and years. An operation that changes state gets a fresh run, order or invite
//!   for each probe, and its probes are sent as the actor allowed to make the call, so they reach
//!   the handler's own validation rather than stopping at 401, 403 or 404;
//! - asserts that no answer is 5xx or a panic, that every 4xx is the contract's Error envelope,
//!   that every 2xx validates against the operation's schema, and that a control character in a
//!   path or query parameter is exactly 400 `validation_failed`.
//!
//! Skipped, because they need the outside world:
//! - `startTelegramAuth`: a live Telegram bot. Without one it answers 503 `upstream_unavailable`
//!   by contract. Its poll, `getTelegramAuthStatus`, is swept: an unknown challenge is 404.
//!
//! Nothing else needs the outside world here: Nessie live and Gemini are never called (Nessie is
//! in mock mode and no operation in the contract calls Gemini), Telegram messages go through the
//! sidecar, which is not configured, and Web Push has a generated VAPID pair and may reach only a
//! local push stub, which `subscribePush`'s valid call subscribes, so every notification the sweep
//! triggers is pushed there and never to a live push service. An operation the server does not
//! route yet (it answers 404 `not_found`, in the envelope) goes in [`NOT_ROUTED`] and is
//! reported, not failed; today every operation is routed.
//!
//! Every probe runs in its own task, so a handler that panics (integer overflow in a debug build,
//! for example) is recorded as a finding instead of ending the sweep; the test fails at the end
//! with the whole list. Run it under the shared lock like every database test.

mod common;
mod push_support;
mod realtime_support;
mod receipts_support;
mod runs_support;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use ferry_core::app;
use ferry_core::notifications::push::{self, PushSettings};
use ferry_core::state::AppState;
use http_body_util::BodyExt;
use serde_json::{Map, Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

use realtime_support::{Server, openapi};
use receipts_support::Stub;
use runs_support::{burrito, fetzer, minutes_from_now};

/// Operations that need the outside world, with the reason.
const SKIPPED: [(&str, &str); 1] = [(
    "startTelegramAuth",
    "needs a live Telegram bot (503 upstream_unavailable by contract without one)",
)];

/// Operations in the contract that ferry-core does not route yet: they answer 404 `not_found`
/// until their task lands. When one starts answering, its valid call must validate like any other.
/// Empty since requests (#32) routed `createPickupRequest`.
const NOT_ROUTED: [&str; 0] = [];

/// The longest string a probe puts in a path or query parameter: `http::Uri` refuses URIs longer
/// than about 64 KB, and a real server refuses them before routing.
const URI_STRING_LEN: usize = 60_000;

/// A body string "1 MB" long (axum's default body limit is 2 MB).
const BODY_STRING_LEN: usize = 1_000_000;

const JUDGE: &str = "usr_judge";
const MAYA: &str = "usr_maya";
const DANA: &str = "usr_dana";
const HINTON: &str = "cir_hinton4";

// ---------- The contract ----------

#[derive(Debug, Clone)]
struct Operation {
    id: String,
    method: Method,
    /// The template, such as `/v1/runs/{id}/orders`.
    path: String,
    /// `paths/<escaped path>/<method>`, for JSON pointers into the contract.
    pointer: String,
    path_params: Vec<String>,
    /// Name and schema.
    query_params: Vec<(String, Value)>,
    /// The request body's schema and whether the body is required.
    body: Option<(Value, bool)>,
    /// The first 2xx status and a pointer to its JSON schema, if it has a body.
    success: (u16, Option<String>),
}

/// `~` and `/` escaped for a JSON pointer.
fn escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

/// Follows `$ref`s to `#/components/...` in the contract.
fn resolve(schema: &Value) -> &Value {
    let mut schema = schema;
    for _ in 0..16 {
        match schema.get("$ref").and_then(Value::as_str) {
            Some(reference) => {
                let pointer = reference.trim_start_matches('#');
                schema = openapi()
                    .pointer(pointer)
                    .unwrap_or_else(|| panic!("unresolved $ref {reference}"));
            }
            None => return schema,
        }
    }
    panic!("$ref chain too long")
}

/// Every operation in `paths`, in the contract's order.
fn operations() -> Vec<Operation> {
    let paths = openapi()["paths"].as_object().expect("paths");
    let mut out = Vec::new();
    for (path, item) in paths {
        let shared: Vec<Value> = item
            .get("parameters")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for (method_name, method) in [
            ("get", Method::GET),
            ("post", Method::POST),
            ("put", Method::PUT),
            ("patch", Method::PATCH),
            ("delete", Method::DELETE),
        ] {
            let Some(op) = item.get(method_name) else {
                continue;
            };
            let id = op["operationId"].as_str().expect("operationId").to_owned();
            let mut params = shared.clone();
            params.extend(
                op.get("parameters")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            );
            let mut path_params = Vec::new();
            let mut query_params = Vec::new();
            for param in &params {
                let param = resolve(param);
                let name = param["name"].as_str().unwrap().to_owned();
                match param["in"].as_str() {
                    Some("path") => path_params.push(name),
                    Some("query") => query_params.push((name, param["schema"].clone())),
                    other => panic!("{id}: parameter {name} in {other:?} is not swept"),
                }
            }
            let body = op.get("requestBody").map(|body| {
                let body = resolve(body);
                (
                    body["content"]["application/json"]["schema"].clone(),
                    body["required"].as_bool().unwrap_or(false),
                )
            });
            let pointer = format!("/paths/{}/{method_name}", escape(path));
            let responses = op["responses"].as_object().expect("responses");
            let (code, response) = responses
                .iter()
                .find(|(code, _)| code.starts_with('2'))
                .unwrap_or_else(|| panic!("{id} has no 2xx response"));
            let schema = resolve(response)
                .get("content")
                .map(|_| format!("{pointer}/responses/{code}/content/application~1json/schema"));
            out.push(Operation {
                id,
                method,
                path: path.clone(),
                pointer,
                path_params,
                query_params,
                body,
                success: (code.parse().unwrap(), schema),
            });
        }
    }
    out
}

/// Compiled validators for JSON pointers into the contract, so each compiles once.
#[derive(Default)]
struct Validators(Mutex<HashMap<String, Arc<jsonschema::Validator>>>);

impl Validators {
    /// Why `instance` does not validate against the schema at `pointer`; empty when it does.
    fn errors(&self, pointer: &str, instance: &Value) -> Vec<String> {
        let validator = {
            let mut cache = self.0.lock().unwrap();
            cache
                .entry(pointer.to_owned())
                .or_insert_with(|| {
                    let mut root = openapi().clone();
                    // A URI fragment: the braces of `{id}` must be percent-encoded.
                    let fragment = pointer.replace('{', "%7B").replace('}', "%7D");
                    root["$ref"] = json!(format!("#{fragment}"));
                    Arc::new(
                        jsonschema::draft202012::options()
                            .should_validate_formats(true)
                            .build(&root)
                            .unwrap_or_else(|e| panic!("{pointer} does not compile: {e}")),
                    )
                })
                .clone()
        };
        validator
            .iter_errors(instance)
            .map(|e| format!("{e} at {}", e.instance_path()))
            .collect()
    }
}

/// Problems with an error body: the contract's `Error`, and nothing but `error` at the top.
fn envelope_problems(validators: &Validators, body: &Value) -> Vec<String> {
    let mut problems = validators.errors("/components/schemas/Error", body);
    match body.as_object() {
        Some(object) if object.len() == 1 => {}
        _ => problems.push("the body has more than `error` at the top level".into()),
    }
    if let Some(error) = body["error"].as_object()
        && !error
            .keys()
            .all(|k| ["code", "message", "details"].contains(&k.as_str()))
    {
        problems.push("`error` has keys besides code, message and details".into());
    }
    problems
}

// ---------- Requests ----------

/// One call: who makes it and what it sends.
#[derive(Debug, Clone, Default)]
struct Target {
    /// The user whose token goes in `Authorization`; none for public calls.
    actor: Option<String>,
    /// Path parameter values, already percent-encoded.
    path: BTreeMap<String, String>,
    /// Query parameters, already percent-encoded.
    query: Vec<(String, String)>,
    body: Option<Value>,
    /// The status the valid call answers when it is not the operation's 2xx (an unknown
    /// challenge's 404).
    expect: Option<u16>,
}

impl Target {
    fn by(actor: &str) -> Self {
        Self {
            actor: Some(actor.to_owned()),
            ..Self::default()
        }
    }

    fn public() -> Self {
        Self::default()
    }

    fn path(mut self, name: &str, value: &str) -> Self {
        self.path.insert(name.to_owned(), encode(value));
        self
    }

    fn query(mut self, name: &str, value: &str) -> Self {
        self.query.push((name.to_owned(), encode(value)));
        self
    }

    fn body(mut self, body: Value) -> Self {
        self.body = Some(body);
        self
    }
}

/// Percent-encodes everything but RFC 3986's unreserved characters.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// What a probe sends as its body.
#[derive(Debug, Clone)]
enum Payload {
    None,
    Json(Value),
    /// Raw bytes with a content type, for malformed bodies.
    Raw(&'static str, Vec<u8>),
}

#[derive(Debug, Clone)]
struct Probe {
    what: String,
    path: BTreeMap<String, String>,
    query: Vec<(String, String)>,
    payload: Payload,
    /// The exact answer the probe must get (status, code), when there is one.
    exact: Option<(u16, &'static str)>,
}

/// The answer to one probe: status and body, or the panic that replaced it.
enum Answer {
    Response(StatusCode, Value),
    Panicked(String),
}

fn uri(op: &Operation, path: &BTreeMap<String, String>, query: &[(String, String)]) -> String {
    let mut uri = op.path.clone();
    for name in &op.path_params {
        let value = path
            .get(name)
            .unwrap_or_else(|| panic!("{}: no value for {name}", op.id));
        uri = uri.replace(&format!("{{{name}}}"), value);
    }
    if !query.is_empty() {
        let pairs: Vec<String> = query.iter().map(|(k, v)| format!("{k}={v}")).collect();
        uri.push('?');
        uri.push_str(&pairs.join("&"));
    }
    uri
}

/// Sends a probe to a fresh copy of the app in its own task.
async fn send(server: &Server, op: &Operation, actor: Option<&str>, probe: &Probe) -> Answer {
    let mut request =
        Request::builder()
            .method(op.method.clone())
            .uri(uri(op, &probe.path, &probe.query));
    if let Some(actor) = actor {
        request = request.header(
            header::AUTHORIZATION,
            format!("Bearer {}", server.token(actor)),
        );
    }
    let request = match &probe.payload {
        Payload::None => request.body(Body::empty()),
        Payload::Json(value) => request
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(value.to_string())),
        Payload::Raw(content_type, bytes) => request
            .header(header::CONTENT_TYPE, *content_type)
            .body(Body::from(bytes.clone())),
    }
    .unwrap();
    let router = app::router(server.state.clone());
    let task = tokio::spawn(async move {
        let response = router.oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(
                |_| json!({"<not JSON>": String::from_utf8_lossy(&bytes[..bytes.len().min(300)])}),
            )
        };
        (status, body)
    });
    match task.await {
        Ok((status, body)) => Answer::Response(status, body),
        Err(err) if err.is_panic() => {
            let panic = err.into_panic();
            let message = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "a panic".into());
            Answer::Panicked(message)
        }
        Err(err) => Answer::Panicked(err.to_string()),
    }
}

// ---------- Hostile inputs ----------

/// Hostile values for a path or query parameter, percent-encoded, with the answer each must get.
/// A hostile value: what it is, the value, and the exact (status, code) it must get, if any.
type Hostile = (String, String, Option<(u16, &'static str)>);

fn hostile_uri_values(valid: &str) -> Vec<Hostile> {
    let control = Some((400, "validation_failed"));
    vec![
        ("a NUL after the id".into(), format!("{valid}%00x"), control),
        ("only a NUL".into(), "%00".into(), control),
        ("a NUL inside".into(), format!("run_%00{valid}"), control),
        ("a C0 control".into(), format!("{valid}%01"), control),
        ("a newline".into(), format!("{valid}%0A"), control),
        ("CR LF".into(), format!("{valid}%0D%0Ax"), control),
        ("a tab".into(), format!("{valid}%09"), control),
        ("DEL".into(), format!("{valid}%7F"), control),
        ("a C1 control".into(), format!("{valid}%C2%85"), control),
        ("not UTF-8".into(), format!("{valid}%FF"), None),
        ("an encoded slash".into(), format!("{valid}%2F..%2F"), None),
        ("dot dot".into(), "..".into(), None),
        ("emoji".into(), format!("{valid}%F0%9F%8C%AE"), None),
        ("a quote".into(), format!("{valid}%27%22"), None),
        ("SQL".into(), encode("x' OR '1'='1"), None),
        ("a percent sign".into(), format!("{valid}%25"), None),
        ("LIKE wildcards".into(), encode("%_\\"), None),
        ("a regex".into(), encode("(["), None),
        ("60 KB".into(), "a".repeat(URI_STRING_LEN), None),
        ("a huge number".into(), "9".repeat(400), None),
    ]
}

/// The kind of a value in a request schema.
#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Str(Option<String>),
    Int,
    Num,
    Bool,
    Enum,
    Arr,
    Obj,
    Any,
}

/// One field of a request body: where it is (keys, or `None` for an array's first item), its kind,
/// and whether its object requires it.
#[derive(Debug, Clone)]
struct Field {
    at: Vec<Option<String>>,
    kind: Kind,
    required: bool,
}

/// The schema without its `null` alternative, `$ref`s followed.
fn concrete(schema: &Value) -> Value {
    let schema = resolve(schema);
    if let Some(options) = schema.get("oneOf").and_then(Value::as_array)
        && let Some(first) = options.iter().map(resolve).find(|s| s["type"] != "null")
    {
        return concrete(first);
    }
    schema.clone()
}

fn kind_of(schema: &Value) -> Kind {
    if schema.get("enum").is_some() {
        return Kind::Enum;
    }
    let ty = match &schema["type"] {
        Value::String(t) => t.clone(),
        Value::Array(types) => types
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null")
            .unwrap_or("null")
            .to_owned(),
        _ => String::new(),
    };
    match ty.as_str() {
        "string" => Kind::Str(schema["format"].as_str().map(str::to_owned)),
        "integer" => Kind::Int,
        "number" => Kind::Num,
        "boolean" => Kind::Bool,
        "array" => Kind::Arr,
        "object" => Kind::Obj,
        _ => Kind::Any,
    }
}

/// Every field under `schema`, depth first.
fn fields(schema: &Value, at: &[Option<String>], out: &mut Vec<Field>, depth: usize) {
    if depth > 6 {
        return;
    }
    let schema = concrete(schema);
    match kind_of(&schema) {
        Kind::Obj => {
            let required: Vec<&str> = schema["required"]
                .as_array()
                .map(|r| r.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            if let Some(properties) = schema["properties"].as_object() {
                for (name, sub) in properties {
                    let mut here = at.to_vec();
                    here.push(Some(name.clone()));
                    let sub = concrete(sub);
                    out.push(Field {
                        at: here.clone(),
                        kind: kind_of(&sub),
                        required: required.contains(&name.as_str()),
                    });
                    fields(&sub, &here, out, depth + 1);
                }
            }
        }
        Kind::Arr => {
            let item = concrete(&schema["items"]);
            let mut here = at.to_vec();
            here.push(None);
            out.push(Field {
                at: here.clone(),
                kind: kind_of(&item),
                required: false,
            });
            fields(&item, &here, out, depth + 1);
        }
        _ => {}
    }
}

/// `body` with the value at `at` replaced (or removed when `value` is `None`), creating missing
/// objects and arrays on the way.
fn with_value(body: &Value, at: &[Option<String>], value: Option<Value>) -> Value {
    fn set(node: &mut Value, at: &[Option<String>], value: Option<Value>) {
        let Some((first, rest)) = at.split_first() else {
            return;
        };
        match first {
            Some(key) => {
                if !node.is_object() {
                    *node = Value::Object(Map::new());
                }
                let object = node.as_object_mut().unwrap();
                if rest.is_empty() {
                    match value {
                        Some(value) => {
                            object.insert(key.clone(), value);
                        }
                        None => {
                            object.remove(key);
                        }
                    }
                } else {
                    let child = object.entry(key.clone()).or_insert(Value::Null);
                    set(child, rest, value);
                }
            }
            None => {
                if !node.is_array() {
                    *node = Value::Array(Vec::new());
                }
                let array = node.as_array_mut().unwrap();
                if rest.is_empty() {
                    match value {
                        Some(value) if array.is_empty() => array.push(value),
                        Some(value) => array[0] = value,
                        None => {
                            if !array.is_empty() {
                                array.remove(0);
                            }
                        }
                    }
                } else {
                    if array.is_empty() {
                        array.push(Value::Null);
                    }
                    set(&mut array[0], rest, value);
                }
            }
        }
    }
    let mut body = body.clone();
    set(&mut body, at, value);
    body
}

/// The value at `at` in `body`, if there is one.
fn value_at<'a>(body: &'a Value, at: &[Option<String>]) -> Option<&'a Value> {
    let mut node = body;
    for segment in at {
        node = match segment {
            Some(key) => node.get(key)?,
            None => node.get(0)?,
        };
    }
    Some(node)
}

/// Hostile values for a body field of `kind`, given its valid value.
fn hostile_body_values(kind: &Kind, valid: Option<&Value>) -> Vec<(&'static str, Value)> {
    let big = "a".repeat(BODY_STRING_LEN);
    let mut values: Vec<(&'static str, Value)> = vec![("null", Value::Null)];
    match kind {
        Kind::Str(format) => {
            values.extend([
                ("a NUL inside", json!("a\u{0}b")),
                ("only a NUL", json!("\u{0}")),
                ("control characters", json!("\u{1}\u{1f}\u{7f}\u{85}")),
                ("newlines and tabs", json!("a\r\n\tb")),
                ("1 MB", json!(big)),
                ("empty", json!("")),
                ("whitespace", json!("   ")),
                ("a number", json!(5)),
                ("an object", json!({})),
                ("an array", json!(["x"])),
                ("a lone surrogate escape", json!("\u{fffd}")),
            ]);
            values.push(("a regex", json!("([")));
            if let Some(valid) = valid.and_then(Value::as_str) {
                values.push((
                    "a NUL after the valid value",
                    json!(format!("{valid}\u{0}")),
                ));
                values.push(("two dots in the valid value", json!(format!("{valid}..x"))));
            }
            if format.as_deref() == Some("date-time") {
                values.extend([
                    ("offset +99:00", json!("2026-10-10T22:04:12+99:00")),
                    ("offset +16:00", json!("2026-10-10T22:04:12+16:00")),
                    ("offset -99:99", json!("2026-10-10T22:04:12-99:99")),
                    ("year 99999", json!("99999-10-10T22:04:12Z")),
                    ("year 0", json!("0000-01-01T00:00:00Z")),
                    ("year 1500", json!("1500-01-01T00:00:00Z")),
                    ("year 9999 west of UTC", json!("9999-12-31T23:59:59-12:00")),
                    ("year 9999 east of UTC", json!("9999-12-31T23:59:59+14:00")),
                    ("February 30", json!("2026-02-30T12:00:00Z")),
                    ("hour 24", json!("2026-10-10T24:00:00Z")),
                    ("second 61", json!("2026-10-10T22:04:61Z")),
                    (
                        "a long fraction",
                        json!("2026-10-10T22:04:12.1234567890123456789Z"),
                    ),
                    ("no offset", json!("2026-10-10T22:04:12")),
                    ("epoch seconds", json!("1791669852")),
                    ("words", json!("tomorrow at 7")),
                    ("infinity", json!("infinity")),
                    ("epoch", json!("epoch")),
                ]);
            }
            if format.as_deref() == Some("uri") {
                values.extend([
                    ("not a URL", json!("not a url")),
                    ("a script URL", json!("javascript:alert(1)")),
                ]);
            }
        }
        Kind::Int => values.extend([
            ("i64::MAX", json!(i64::MAX)),
            ("i64::MIN", json!(i64::MIN)),
            ("u64::MAX", json!(u64::MAX)),
            ("i32::MAX + 1", json!(2_147_483_648_i64)),
            ("i32::MIN - 1", json!(-2_147_483_649_i64)),
            ("2^53 + 1", json!(9_007_199_254_740_993_i64)),
            ("-1", json!(-1)),
            ("0", json!(0)),
            ("1e30", json!(1e30)),
            ("1.5", json!(1.5)),
            ("a string", json!("1")),
            ("a boolean", json!(true)),
        ]),
        Kind::Num => values.extend([
            ("1e308", json!(1e308)),
            ("-1e308", json!(-1e308)),
            ("i64::MAX", json!(i64::MAX)),
            ("u64::MAX", json!(u64::MAX)),
            ("-1", json!(-1)),
            ("0", json!(0)),
            ("a subnormal", json!(5e-324)),
            ("a string", json!("1")),
            ("a boolean", json!(true)),
        ]),
        Kind::Bool => values.extend([("a string", json!("true")), ("a number", json!(1))]),
        Kind::Enum => values.extend([
            ("not in the enum", json!("nope")),
            ("empty", json!("")),
            ("a NUL", json!("\u{0}")),
            ("a number", json!(1)),
            (
                "upper case",
                json!(valid.and_then(Value::as_str).unwrap_or("x").to_uppercase()),
            ),
        ]),
        Kind::Arr => {
            values.extend([
                ("empty", json!([])),
                ("a string", json!("x")),
                ("an object", json!({})),
                ("a null item", json!([null])),
            ]);
            if let Some(first) = valid.and_then(|v| v.get(0)) {
                values.push(("1,000 items", Value::Array(vec![first.clone(); 1000])));
            }
        }
        Kind::Obj => values.extend([
            ("empty", json!({})),
            ("a string", json!("x")),
            ("an array", json!([])),
        ]),
        Kind::Any => values.extend([("a string", json!("x")), ("a number", json!(1))]),
    }
    values
}

/// Every hostile probe for `op`, given its valid target.
fn probes(op: &Operation, valid: &Target) -> Vec<Probe> {
    let base = |what: String| Probe {
        what,
        path: valid.path.clone(),
        query: valid.query.clone(),
        payload: valid.body.clone().map_or(Payload::None, Payload::Json),
        exact: None,
    };
    let mut out = Vec::new();

    for name in &op.path_params {
        let current = valid.path[name].clone();
        for (what, value, exact) in hostile_uri_values(&current) {
            let mut probe = base(format!("path {name}: {what}"));
            probe.path.insert(name.clone(), value);
            probe.exact = exact;
            out.push(probe);
        }
    }

    for (name, schema) in &op.query_params {
        let current = valid
            .query
            .iter()
            .find(|(k, _)| k == name)
            .map_or_else(|| "x".to_owned(), |(_, v)| v.clone());
        let mut values = hostile_uri_values(&current);
        values.extend([
            ("empty".into(), String::new(), None),
            ("wrong type".into(), "maybe".into(), None),
            ("not in the enum".into(), "nope".into(), None),
        ]);
        let _ = schema;
        for (what, value, exact) in values {
            let mut probe = base(format!("query {name}: {what}"));
            probe.query.retain(|(k, _)| k != name);
            probe.query.push((name.clone(), value));
            probe.exact = exact;
            out.push(probe);
        }
        let mut twice = base(format!("query {name}: twice"));
        twice.query.push((name.clone(), current.clone()));
        out.push(twice);
    }
    // A parameter the operation does not have. Where the operation reads its query, a control
    // character there is refused too; elsewhere the query is never read, so it is ignored.
    let mut stray = base("query stray: a NUL".into());
    stray.query.push(("stray".into(), "%00".into()));
    if !op.query_params.is_empty() {
        stray.exact = Some((400, "validation_failed"));
    }
    out.push(stray);

    if let Some((schema, required)) = &op.body {
        let valid_body = valid.body.clone().unwrap_or_else(|| json!({}));
        let mut all = Vec::new();
        fields(schema, &[], &mut all, 0);
        for field in &all {
            let name = field
                .at
                .iter()
                .map(|s| s.clone().unwrap_or_else(|| "0".into()))
                .collect::<Vec<_>>()
                .join(".");
            let current = value_at(&valid_body, &field.at);
            for (what, value) in hostile_body_values(&field.kind, current) {
                let mut probe = base(format!("body {name}: {what}"));
                probe.payload = Payload::Json(with_value(&valid_body, &field.at, Some(value)));
                out.push(probe);
            }
            if field.required && current.is_some() {
                let mut probe = base(format!("body {name}: missing"));
                probe.payload = Payload::Json(with_value(&valid_body, &field.at, None));
                out.push(probe);
            }
        }
        let mut extra = valid_body.clone();
        if let Some(object) = extra.as_object_mut() {
            object.insert("unexpected_field".into(), json!("\u{0}"));
        }
        let deep = format!("{}{}", "[".repeat(10_000), "]".repeat(10_000)).into_bytes();
        let huge = format!("\"{}\"", "a".repeat(3_000_000)).into_bytes();
        let whole: Vec<(&str, Payload)> = vec![
            ("no body", Payload::None),
            (
                "malformed JSON",
                Payload::Raw("application/json", b"{".to_vec()),
            ),
            (
                "an empty JSON body",
                Payload::Raw("application/json", Vec::new()),
            ),
            ("a JSON array", Payload::Json(json!([]))),
            ("JSON null", Payload::Json(Value::Null)),
            ("a JSON string", Payload::Json(json!("x"))),
            ("a JSON number", Payload::Json(json!(1))),
            ("an empty object", Payload::Json(json!({}))),
            ("an unexpected field", Payload::Json(extra)),
            (
                "not JSON's content type",
                Payload::Raw("text/plain", valid_body.to_string().into_bytes()),
            ),
            ("10,000 levels deep", Payload::Raw("application/json", deep)),
            ("3 MB", Payload::Raw("application/json", huge)),
            (
                "invalid UTF-8",
                Payload::Raw("application/json", b"{\"x\": \"\xff\xfe\"}".to_vec()),
            ),
            (
                "a NUL byte outside a string",
                Payload::Raw("application/json", b"{\x00}".to_vec()),
            ),
        ];
        for (what, payload) in whole {
            if !required && matches!(payload, Payload::None) {
                continue;
            }
            let mut probe = base(format!("body: {what}"));
            probe.payload = payload;
            out.push(probe);
        }
    } else {
        // An operation without a body still gets one.
        let mut probe = base("body: an unexpected body".into());
        probe.payload = Payload::Json(json!({"x": "\u{0}", "n": i64::MAX}));
        out.push(probe);
        let mut probe = base("body: malformed JSON".into());
        probe.payload = Payload::Raw("application/json", b"{".to_vec());
        out.push(probe);
    }
    out
}

// ---------- The demo the sweep runs on ----------

/// `receipts_support::server`'s app (demo mode, Storage on `storage`, OSRM refused), plus a
/// generated VAPID pair and Web Push allowed to reach `push`, the local stub
/// (`PUSH_TEST_STUB_ORIGIN`). Every subscription the sweep makes is at that stub, so push is on
/// and no push leaves the machine.
async fn server(pool: PgPool, storage: &Stub, push: &push_support::Stub) -> Server {
    let (public, private) = push_support::vapid_pair();
    let config = common::config(&[
        ("DEMO_MODE", "true"),
        ("PUBLIC_WEB_URL", runs_support::WEB_URL),
        ("OSRM_BASE_URL", receipts_support::OSRM_OFF),
        ("SUPABASE_URL", &storage.base),
        ("SUPABASE_SECRET_KEY", receipts_support::SECRET_KEY),
        ("VAPID_PUBLIC_KEY", &public),
        ("VAPID_PRIVATE_KEY", &private),
        ("PUSH_TEST_STUB_ORIGIN", &push.origin),
    ]);
    let state = AppState::new(pool, config).unwrap();
    let settings = PushSettings::from_config(&state.config);
    assert!(push::attach_with(&state, settings), "web push is on");
    realtime_support::serve(state).await
}

struct Demo {
    server: Server,
    pool: PgPool,
    /// The Storage stub, holding every receipt the sweep "uploads".
    storage: Stub,
    /// The push service stub: the only push endpoint the server may reach.
    push: push_support::Stub,
    /// A funded order of the judge's on its own open run.
    order: Option<String>,
    /// A meet-again invite from Dana to cir_hinton4.
    invite: Option<String>,
    /// One of the judge's notifications.
    notification: Option<String>,
    /// Top-ups so far: each uses a new amount, because the same amount twice within a few
    /// seconds is one deposit.
    topups: std::cell::Cell<i64>,
}

impl Demo {
    async fn start(pool: PgPool) -> Self {
        let storage = Stub::start().await;
        let push = push_support::Stub::start().await;
        receipts_support::seed(&pool).await;
        let server = server(pool.clone(), &storage, &push).await;
        Self {
            server,
            pool,
            storage,
            push,
            order: None,
            invite: None,
            notification: None,
            topups: std::cell::Cell::new(0),
        }
    }

    /// Enough money for the judge's next order.
    async fn fund_judge(&self) {
        let n = self.topups.get() + 1;
        self.topups.set(n);
        runs_support::topup(&self.server, JUDGE, 2_000 + n).await;
    }

    async fn call(
        &self,
        method: Method,
        uri: &str,
        actor: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        runs_support::call(
            &self.server,
            method,
            uri,
            Some(&self.server.token(actor)),
            body,
        )
        .await
    }

    async fn ok(&self, method: Method, uri: &str, actor: &str, body: Option<Value>) -> Value {
        let (status, answer) = self.call(method.clone(), uri, actor, body).await;
        assert!(
            status.is_success(),
            "setup {method} {uri} as {actor}: {status} {answer}"
        );
        answer
    }

    async fn time(&self, minutes: i32) -> String {
        minutes_from_now(&self.pool, minutes).await
    }

    /// A new open run of Maya's in cir_hinton4.
    async fn open_run(&self) -> String {
        runs_support::open_run(&self.server, MAYA, HINTON).await
    }

    /// The judge's funded order (the demo burrito) on `run`.
    async fn order_on(&self, run: &str) -> String {
        self.fund_judge().await;
        let order = self
            .ok(
                Method::POST,
                &format!("/v1/runs/{run}/orders"),
                JUDGE,
                Some(burrito()),
            )
            .await;
        order["id"].as_str().unwrap().to_owned()
    }

    /// A new run with the judge's order, moved on to `stage`: open, closed, purchased or
    /// delivering. Returns the run and the order.
    async fn run_at(&self, stage: &str) -> (String, String) {
        let run = self.open_run().await;
        let order = self.order_on(&run).await;
        let steps: &[&str] = match stage {
            "open" => &[],
            "closed" => &["close"],
            "purchased" => &["close", "purchased"],
            "delivering" => &["close", "purchased", "picked-up"],
            other => panic!("no stage {other}"),
        };
        for step in steps {
            let body = (*step == "purchased").then(|| json!({"receipt_total_cents": 900}));
            self.ok(Method::POST, &format!("/v1/runs/{run}/{step}"), MAYA, body)
                .await;
        }
        (run, order)
    }

    async fn ensure_invite(&mut self) -> String {
        if self.invite.is_none() {
            let invite = self
                .ok(
                    Method::POST,
                    &format!("/v1/circles/{HINTON}/meet-again"),
                    DANA,
                    Some(json!({"message": "Dinner again?"})),
                )
                .await;
            self.invite = Some(invite["id"].as_str().unwrap().to_owned());
        }
        self.invite.clone().unwrap()
    }

    /// The valid target for `op`, prepared fresh where the call changes state.
    async fn target(&mut self, op: &Operation) -> Target {
        match op.id.as_str() {
            "getTelegramAuthStatus" => {
                let mut target = Target::public().path("challenge_id", "chl_unknown");
                target.expect = Some(404);
                target
            }
            "demoSignIn" => Target::public().body(json!({"persona": "judge"})),
            "updateProfile" => {
                let me = self.ok(Method::GET, "/v1/me", JUDGE, None).await;
                Target::by(JUDGE).body(json!({
                    "display_name": me["display_name"],
                    "profile": me["profile"],
                }))
            }
            "listRestaurants" => Target::by(JUDGE)
                .query("area_id", "area_franklin")
                .query("q", "cosmic")
                .query("open_now", "false"),
            "getMenu" => Target::by(JUDGE).path("id", "rst_cosmic"),
            "listRuns" => Target::by(JUDGE)
                .query("status", "open")
                .query("mine", "false"),
            "createRun" => {
                let leaving = self.time(5).await;
                Target::by(MAYA).body(json!({
                    "restaurant_id": "rst_cosmic",
                    "circle_id": HINTON,
                    "leaving_at": leaving,
                    "window_minutes": 30,
                }))
            }
            "getRun" | "getRunTrack" => Target::by(JUDGE).path("id", "run_demo"),
            "getRunPreview" => Target::public().path("id", "run_demo"),
            "quoteOrder" => Target::by(JUDGE).path("id", "run_demo").body(burrito()),
            "placeOrder" => {
                let run = self.open_run().await;
                self.fund_judge().await;
                Target::by(JUDGE).path("id", &run).body(burrito())
            }
            "closeRun" => {
                let (run, _) = self.run_at("open").await;
                Target::by(MAYA).path("id", &run)
            }
            "createReceiptUpload" => {
                let (run, _) = self.run_at("closed").await;
                Target::by(MAYA).path("id", &run)
            }
            "markRunPurchased" => {
                let (run, _) = self.run_at("closed").await;
                let receipt = format!("{run}/receipt.jpg");
                self.storage.put_object("receipts", &receipt);
                Target::by(MAYA).path("id", &run).body(json!({
                    "receipt_total_cents": 900,
                    "receipt_path": receipt,
                }))
            }
            "markRunPickedUp" => {
                let (run, _) = self.run_at("purchased").await;
                Target::by(MAYA).path("id", &run)
            }
            "cancelRun" => {
                let run = self.open_run().await;
                Target::by(MAYA).path("id", &run)
            }
            "listOrders" => Target::by(JUDGE).query("mine", "true"),
            "getOrder" => {
                if self.order.is_none() {
                    let run = self.open_run().await;
                    self.order = Some(self.order_on(&run).await);
                }
                Target::by(JUDGE).path("id", self.order.as_deref().unwrap())
            }
            "confirmHandoff" => {
                // The runner submits the customer's code, so `code` probes reach the check.
                let (_, order) = self.run_at("delivering").await;
                let seen = self
                    .ok(Method::GET, &format!("/v1/orders/{order}"), JUDGE, None)
                    .await;
                Target::by(MAYA)
                    .path("id", &order)
                    .body(json!({"code": seen["handoff_code"]}))
            }
            "cancelOrder" => {
                let (_, order) = self.run_at("open").await;
                Target::by(JUDGE).path("id", &order)
            }
            // The judge's request (docs/DECISIONS.md, Requests): the burrito from run_demo's
            // restaurant to Fetzer within 100 m, needed in an hour. It fits run_demo or one of
            // Maya's other open Cosmic Cantina runs, so it answers 200 `matched`; probes that
            // still pass validation store requests too (matched, searching, or 429 past five
            // searching).
            "createPickupRequest" => {
                let needed_by = self.time(60).await;
                Target::by(JUDGE).body(json!({
                    "restaurant_id": "rst_cosmic",
                    "lines": burrito()["lines"],
                    "tip_cents": 200,
                    "dropoff": fetzer(),
                    "dropoff_radius_m": 100,
                    "needed_by": needed_by,
                }))
            }
            "postRunnerLocation" => {
                let at = self.time(0).await;
                Target::by(MAYA).path("id", "run_demo").body(json!({
                    "location": {"lat": 35.912, "lng": -79.054},
                    "accuracy_m": 8.0,
                    "at": at,
                }))
            }
            "getCircle" => Target::by(JUDGE).path("id", HINTON),
            "meetAgain" => {
                let when = self.time(2 * 24 * 60).await;
                Target::by(DANA)
                    .path("id", HINTON)
                    .body(json!({"message": "Dinner again?", "proposed_time": when}))
            }
            "respondInvite" => {
                let invite = self.ensure_invite().await;
                Target::by(JUDGE)
                    .path("id", &invite)
                    .body(json!({"accept": true}))
            }
            "markNotificationRead" => {
                if self.notification.is_none() {
                    self.ensure_invite().await;
                    let list = self.ok(Method::GET, "/v1/notifications", JUDGE, None).await;
                    self.notification = Some(list[0]["id"].as_str().unwrap().to_owned());
                }
                Target::by(JUDGE).path("id", self.notification.as_deref().unwrap())
            }
            // A subscription at the push stub, never at a real push service: the judge's later
            // notifications are pushed to whatever is subscribed.
            "subscribePush" => Target::by(JUDGE)
                .body(push_support::Browser::new(self.push.endpoint("sweep")).subscribe_body()),
            "demoTopup" => Target::by(JUDGE).body(json!({"amount_cents": 500})),
            "getImpact" => Target::by(JUDGE)
                .query("scope", "circle")
                .query("circle_id", HINTON),
            "getPublicImpact" | "getPublicConfig" | "getPushKey" => Target::public(),
            "getMe" | "getMyStats" | "listAreas" | "listPlaces" | "listCircles" | "listInvites"
            | "listNotifications" | "getWallet" | "createStreamTicket" => Target::by(JUDGE),
            _ => self.generic(op).await,
        }
    }

    /// A best-effort target for an operation the sweep has no fixture for (one added to the
    /// contract after this test): the judge, the demo's ids by path, a body sampled from the
    /// schema. Its valid call only has to stay below 500.
    async fn generic(&mut self, op: &Operation) -> Target {
        let mut target = if is_public(op) {
            Target::public()
        } else {
            Target::by(JUDGE)
        };
        for name in &op.path_params {
            let value = if op.path.starts_with("/v1/runs") || op.path.starts_with("/v1/public/runs")
            {
                "run_demo".to_owned()
            } else if op.path.starts_with("/v1/circles") {
                HINTON.to_owned()
            } else if op.path.starts_with("/v1/restaurants") {
                "rst_cosmic".to_owned()
            } else if op.path.starts_with("/v1/invites") {
                self.ensure_invite().await
            } else {
                "x".to_owned()
            };
            target = target.path(name, &value);
        }
        if let Some((schema, _)) = &op.body {
            target.body = Some(sample(schema, 0));
        }
        target.expect = Some(0);
        target
    }
}

fn is_public(op: &Operation) -> bool {
    openapi()
        .pointer(&format!("{}/security", op.pointer))
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
}

/// A plausible value for `schema`: its example, else a value of its type.
fn sample(schema: &Value, depth: usize) -> Value {
    let schema = concrete(schema);
    if let Some(example) = schema.get("example") {
        return example.clone();
    }
    if let Some(first) = schema["enum"].as_array().and_then(|e| e.first()) {
        return first.clone();
    }
    match kind_of(&schema) {
        Kind::Str(Some(f)) if f == "date-time" => json!("2030-01-01T00:00:00Z"),
        Kind::Str(Some(f)) if f == "uri" => json!("https://example.com/x"),
        Kind::Str(_) => json!("x"),
        Kind::Int | Kind::Num => json!(1),
        Kind::Bool => json!(true),
        Kind::Arr if depth < 4 => json!([sample(&schema["items"], depth + 1)]),
        Kind::Obj if depth < 4 => {
            let mut object = Map::new();
            if let Some(properties) = schema["properties"].as_object() {
                for (name, sub) in properties {
                    object.insert(name.clone(), sample(sub, depth + 1));
                }
            }
            Value::Object(object)
        }
        _ => Value::Null,
    }
}

/// Operations whose every call changes the state the next call would need.
fn needs_fresh_target(op: &str) -> bool {
    matches!(
        op,
        "placeOrder"
            | "closeRun"
            | "createReceiptUpload"
            | "markRunPurchased"
            | "markRunPickedUp"
            | "cancelRun"
            | "confirmHandoff"
            | "cancelOrder"
            | "createRun"
            | "postRunnerLocation"
            | "meetAgain"
    )
}

// ---------- The sweep ----------

#[derive(Default)]
struct Report {
    findings: Vec<String>,
    /// Per operation: status → count, over its hostile probes.
    statuses: BTreeMap<String, BTreeMap<u16, usize>>,
    not_routed: Vec<String>,
    probes: usize,
}

impl Report {
    /// Checks one answer; `exact` is the status and code it must be.
    fn check(
        &mut self,
        validators: &Validators,
        op: &Operation,
        what: &str,
        answer: &Answer,
        exact: Option<(u16, &str)>,
    ) {
        let (status, body) = match answer {
            Answer::Panicked(message) => {
                self.findings.push(format!(
                    "{} [{what}]: the handler panicked: {message}",
                    op.id
                ));
                return;
            }
            Answer::Response(status, body) => (*status, body),
        };
        *self
            .statuses
            .entry(op.id.clone())
            .or_default()
            .entry(status.as_u16())
            .or_default() += 1;
        let mut problems = Vec::new();
        if status.is_server_error() {
            problems.push(format!("{status}"));
        }
        if status.is_client_error() || status.is_server_error() {
            problems.extend(envelope_problems(validators, body));
        } else if status.as_u16() == op.success.0 {
            if let Some(pointer) = &op.success.1 {
                problems.extend(validators.errors(pointer, body));
            }
        } else if status.is_success() {
            problems.push(format!(
                "{status}, but the contract's success status is {}",
                op.success.0
            ));
        }
        if let Some((expected, code)) = exact
            && (status.as_u16() != expected || body["error"]["code"] != code)
        {
            problems.push(format!("expected {expected} {code}, got {status}"));
        }
        if !problems.is_empty() {
            let shown = body.to_string();
            let shown: String = shown.chars().take(300).collect();
            self.findings.push(format!(
                "{} [{what}]: {} -> {shown}",
                op.id,
                problems.join("; ")
            ));
        }
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn every_operation_answers_in_the_contract_and_never_5xx(pool: PgPool) {
    let mut demo = Demo::start(pool).await;
    let validators = Validators::default();
    let mut report = Report::default();
    let all = operations();
    assert!(all.len() >= 40, "only {} operations read", all.len());

    let mut swept = Vec::new();
    for op in &all {
        if let Some((_, why)) = SKIPPED.iter().find(|(id, _)| *id == op.id) {
            eprintln!("skipped {}: {why}", op.id);
            continue;
        }

        // The valid call.
        let target = demo.target(op).await;
        let valid = Probe {
            what: "valid".into(),
            path: target.path.clone(),
            query: target.query.clone(),
            payload: target.body.clone().map_or(Payload::None, Payload::Json),
            exact: None,
        };
        let answer = send(&demo.server, op, target.actor.as_deref(), &valid).await;
        let routed = !matches!(&answer, Answer::Response(StatusCode::NOT_FOUND, body)
            if NOT_ROUTED.contains(&op.id.as_str())
                && body["error"]["message"].as_str().is_some_and(|m| m.starts_with("No route")));
        if !routed {
            report.not_routed.push(op.id.clone());
        }
        let expected = match target.expect {
            Some(0) => None,
            Some(code) => Some(code),
            None if routed => Some(op.success.0),
            None => Some(404),
        };
        match (&answer, expected) {
            (Answer::Response(status, body), Some(code)) if status.as_u16() != code => {
                report.findings.push(format!(
                    "{} [valid]: expected {code}, got {status} {body}",
                    op.id
                ));
            }
            _ => {}
        }
        report.check(&validators, op, "valid", &answer, None);
        // The valid call's answer is not part of the hostile statistics.
        if let Some(counts) = report.statuses.get_mut(&op.id) {
            counts.clear();
        }

        // The hostile calls.
        let fresh = needs_fresh_target(&op.id);
        let list = probes(op, &target);
        report.probes += list.len();
        for probe in list {
            let (actor, probe) = if fresh {
                let target = demo.target(op).await;
                let mut probe = probe;
                // The same hostile change, on the fresh run, order or invite.
                for (name, value) in &target.path {
                    let original = &valid.path[name];
                    if let Some(hostile) = probe.path.get_mut(name)
                        && hostile.contains(original.as_str())
                    {
                        *hostile = hostile.replace(original.as_str(), value);
                    }
                }
                if let (Payload::Json(body), Some(fresh_body)) = (&mut probe.payload, &target.body)
                {
                    refresh_body(body, valid_body(&valid), fresh_body);
                    // Ids inside hostile strings too, such as `<run>/receipt.jpg..x`.
                    for (name, value) in &target.path {
                        replace_in_strings(body, &valid.path[name], value);
                    }
                }
                (target.actor, probe)
            } else {
                (target.actor.clone(), probe)
            };
            let answer = send(&demo.server, op, actor.as_deref(), &probe).await;
            report.check(&validators, op, &probe.what, &answer, probe.exact);
        }
        swept.push(op.id.clone());
    }

    eprintln!(
        "swept {} operations with {} hostile probes",
        swept.len(),
        report.probes
    );
    for (op, counts) in &report.statuses {
        eprintln!("  {op}: {counts:?}");
    }
    if !report.not_routed.is_empty() {
        eprintln!("not routed yet (404): {:?}", report.not_routed);
    }

    // Every operation in the contract was swept or skipped with a reason.
    assert_eq!(swept.len() + SKIPPED.len(), all.len());
    // The probes reached the handlers: each routed operation that takes input (a path or query
    // parameter, or a body) refused at least one hostile value with 400 validation_failed.
    for op in &all {
        let takes_input =
            !op.path_params.is_empty() || !op.query_params.is_empty() || op.body.is_some();
        if !takes_input
            || SKIPPED.iter().any(|(id, _)| *id == op.id)
            || report.not_routed.contains(&op.id)
        {
            continue;
        }
        let counts = &report.statuses[&op.id];
        if counts.get(&400).copied().unwrap_or(0) == 0 {
            report.findings.push(format!(
                "{}: no hostile probe reached validation (no 400): {counts:?}",
                op.id
            ));
        }
    }
    assert!(
        report.findings.is_empty(),
        "{} findings:\n{}",
        report.findings.len(),
        report.findings.join("\n")
    );
}

/// Replaces `from` with `to` inside every string in `value`.
fn replace_in_strings(value: &mut Value, from: &str, to: &str) {
    match value {
        Value::String(text) if text.contains(from) => *text = text.replace(from, to),
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| replace_in_strings(item, from, to)),
        Value::Object(fields) => fields
            .values_mut()
            .for_each(|field| replace_in_strings(field, from, to)),
        _ => {}
    }
}

fn valid_body(valid: &Probe) -> Option<&Value> {
    match &valid.payload {
        Payload::Json(body) => Some(body),
        _ => None,
    }
}

/// In a hostile `body` built from `old` (the first valid body), puts back every value that the
/// fresh valid body `new` has differently and the probe did not change: the fresh `at`, the fresh
/// handoff code.
fn refresh_body(body: &mut Value, old: Option<&Value>, new: &Value) {
    let Some(old) = old else {
        return;
    };
    match (body, old, new) {
        (Value::Object(body), Value::Object(old), Value::Object(new)) => {
            for (key, new_value) in new {
                if let (Some(slot), Some(old_value)) = (body.get_mut(key), old.get(key)) {
                    if slot == old_value {
                        *slot = new_value.clone();
                    } else {
                        refresh_body(slot, Some(old_value), new_value);
                    }
                }
            }
        }
        (Value::Array(body), Value::Array(old), Value::Array(new)) => {
            for ((slot, old_value), new_value) in body.iter_mut().zip(old).zip(new) {
                if slot == old_value {
                    *slot = new_value.clone();
                } else {
                    refresh_body(slot, Some(old_value), new_value);
                }
            }
        }
        _ => {}
    }
}
