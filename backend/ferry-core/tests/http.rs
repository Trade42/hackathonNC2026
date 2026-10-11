//! The HTTP skeleton: healthz, CORS, the error envelope and AuthUser.
mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::{get, post};
use common::{assert_envelope, config, get as get_req, send, unreachable_pool};
use ferry_core::app;
use ferry_core::error::ApiResult;
use ferry_core::extract::{Json, Path, Query};
use ferry_core::jwt::{AuthUser, Claims};
use ferry_core::state::AppState;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;

fn state(pool: PgPool) -> AppState {
    AppState::new(
        pool,
        config(&[("PUBLIC_WEB_URL", "https://ferry.example/app")]),
    )
    .unwrap()
}

fn app_unreachable() -> Router {
    app::router(state(unreachable_pool()))
}

#[sqlx::test]
async fn healthz_ok(pool: PgPool) {
    let (status, _, body) = send(app::router(state(pool)), get_req("/healthz")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"status": "ok", "db": "ok"}));
}

#[tokio::test]
async fn healthz_503_when_the_database_is_unreachable() {
    let (status, _, body) = send(app_unreachable(), get_req("/healthz")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["db"], "unreachable");
}

#[tokio::test]
async fn cors_allows_only_listed_origins() {
    for origin in [
        "http://localhost:5173",
        "http://127.0.0.1:5173",
        "capacitor://localhost",
        "http://localhost",
        "https://localhost",
        "tauri://localhost",
        "https://tauri.localhost",
        "https://ferrygo.tech",
        "https://www.ferrygo.tech",
        "https://ferry-hacknc.vercel.app",
        "https://ferry.example",
    ] {
        let request = Request::get("/healthz")
            .header(header::ORIGIN, origin)
            .body(Body::empty())
            .unwrap();
        let (_, headers, _) = send(app_unreachable(), request).await;
        assert_eq!(
            headers
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .map(|v| v.to_str().unwrap()),
            Some(origin),
            "{origin}"
        );
    }
    for origin in [
        "https://evil.example",
        "http://localhost:3000",
        "null",
        "https://localhost:8443",
        "http://ferrygo.tech",
        "https://evil.ferrygo.tech",
        "https://ferrygo.tech.evil.example",
    ] {
        let request = Request::get("/healthz")
            .header(header::ORIGIN, origin)
            .body(Body::empty())
            .unwrap();
        let (_, headers, _) = send(app_unreachable(), request).await;
        assert!(
            headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none(),
            "{origin}"
        );
    }
}

#[tokio::test]
async fn cors_preflight() {
    let request = Request::options("/v1/runs")
        .header(header::ORIGIN, "http://localhost:5173")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
        .header(
            header::ACCESS_CONTROL_REQUEST_HEADERS,
            "authorization,content-type",
        )
        .body(Body::empty())
        .unwrap();
    let (status, headers, _) = send(app_unreachable(), request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "http://localhost:5173"
    );
    assert!(
        headers[header::ACCESS_CONTROL_ALLOW_METHODS]
            .to_str()
            .unwrap()
            .contains("POST")
    );
    let allowed = headers[header::ACCESS_CONTROL_ALLOW_HEADERS]
        .to_str()
        .unwrap();
    assert!(allowed.contains("authorization") && allowed.contains("content-type"));
}

/// The Android app (`https://localhost`), the web app on Sal's domain and the current Vercel
/// deploy: preflights for a JSON POST without auth (Telegram sign-in) and a GET with a bearer
/// token both pass.
#[tokio::test]
async fn cors_preflight_from_the_android_app_and_the_web_origins() {
    for origin in [
        "https://localhost",
        "https://ferrygo.tech",
        "https://www.ferrygo.tech",
        "https://ferry-hacknc.vercel.app",
    ] {
        for (method, path, headers) in [
            ("POST", "/v1/auth/telegram/start", "content-type"),
            ("GET", "/v1/me", "authorization"),
        ] {
            let request = Request::options(path)
                .header(header::ORIGIN, origin)
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, method)
                .header(header::ACCESS_CONTROL_REQUEST_HEADERS, headers)
                .body(Body::empty())
                .unwrap();
            let (status, response, _) = send(app_unreachable(), request).await;
            assert_eq!(status, StatusCode::OK, "{origin} {method} {path}");
            assert_eq!(
                response[header::ACCESS_CONTROL_ALLOW_ORIGIN],
                origin,
                "{origin}"
            );
            assert!(
                response[header::ACCESS_CONTROL_ALLOW_METHODS]
                    .to_str()
                    .unwrap()
                    .contains(method),
                "{origin} {method}"
            );
            assert!(
                response[header::ACCESS_CONTROL_ALLOW_HEADERS]
                    .to_str()
                    .unwrap()
                    .contains(headers),
                "{origin} {headers}"
            );
        }
    }
}

#[tokio::test]
async fn unknown_route_is_404_in_the_envelope() {
    let request = Request::get("/v1/nope")
        .header(header::ORIGIN, "http://localhost:5173")
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = send(app_unreachable(), request).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_envelope(&body, "not_found");
    // Errors carry CORS headers too, so the browser can read them.
    assert!(headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_some());
}

#[tokio::test]
async fn wrong_method_is_405_in_the_envelope() {
    let request = Request::post("/healthz").body(Body::empty()).unwrap();
    let (status, _, body) = send(app_unreachable(), request).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_envelope(&body, "not_found");
}

// A route behind AuthUser, mounted only for these tests.
async fn me(user: AuthUser) -> Json<Value> {
    Json(json!({"user_id": user.user_id}))
}

fn auth_app() -> (Router, AppState) {
    let state = state(unreachable_pool());
    let app = Router::new()
        .route("/me", get(me))
        .with_state(state.clone());
    (app, state)
}

fn with_auth(value: &str) -> Request<Body> {
    Request::get("/me")
        .header(header::AUTHORIZATION, value)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn auth_user_requires_a_valid_bearer_token() {
    let (app, state) = auth_app();

    let (status, _, body) = send(app.clone(), get_req("/me")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_envelope(&body, "unauthorized");

    let expired = state
        .jwt
        .sign(&Claims {
            sub: "usr_judge".into(),
            iat: 1_000,
            exp: 2_000,
        })
        .unwrap();
    for bad in [
        "Bearer".to_owned(),
        "Basic dXNlcjpwYXNz".to_owned(),
        "Bearer not-a-jwt".to_owned(),
        format!("Bearer {expired}"),
    ] {
        let (status, _, body) = send(app.clone(), with_auth(&bad)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{bad}");
        assert_envelope(&body, "unauthorized");
    }

    let token = state.jwt.issue_token("usr_judge").unwrap();
    let (status, _, body) = send(app.clone(), with_auth(&format!("Bearer {token}"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"user_id": "usr_judge"}));
    let (status, _, _) = send(app, with_auth(&format!("bearer {token}"))).await;
    assert_eq!(status, StatusCode::OK);
}

// Rejections from the envelope extractors.
#[derive(Deserialize)]
struct Line {
    qty: i64,
}

#[derive(Deserialize)]
struct Filter {
    #[allow(dead_code)]
    open_now: Option<bool>,
}

async fn echo(Json(line): Json<Line>) -> Json<Value> {
    Json(json!({"qty": line.qty}))
}

async fn optional(body: Option<Json<Line>>) -> Json<Value> {
    Json(json!({"qty": body.map(|Json(l)| l.qty)}))
}

async fn filter(Query(_): Query<Filter>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({})))
}

async fn by_number(Path(n): Path<u32>) -> Json<Value> {
    Json(json!({"n": n}))
}

fn reject_app() -> Router {
    Router::new()
        .route("/echo", post(echo))
        .route("/optional", post(optional))
        .route("/filter", get(filter))
        .route("/n/{n}", get(by_number))
}

fn post_json(uri: &str, content_type: Option<&str>, body: &str) -> Request<Body> {
    let mut builder = Request::post(uri);
    if let Some(ct) = content_type {
        builder = builder.header(header::CONTENT_TYPE, ct);
    }
    builder.body(Body::from(body.to_owned())).unwrap()
}

#[tokio::test]
async fn rejections_render_the_envelope() {
    let json_ct = Some("application/json");
    let cases = [
        // JSON that does not parse, of the wrong type, missing a field.
        post_json("/echo", json_ct, "{\"qty\": "),
        post_json("/echo", json_ct, "{\"qty\": \"two\"}"),
        post_json("/echo", json_ct, "{}"),
        // No JSON content type, for a required and an optional body.
        post_json("/echo", None, "{\"qty\": 2}"),
        post_json("/optional", Some("text/plain"), "2"),
        // Bad query and path values.
        get_req("/filter?open_now=maybe"),
        get_req("/n/abc"),
    ];
    for request in cases {
        let label = format!("{} {}", request.method(), request.uri());
        let (status, _, body) = send(reject_app(), request).await;
        // validation_failed is 400 everywhere.
        assert_eq!(status, StatusCode::BAD_REQUEST, "{label}: {body}");
        assert_envelope(&body, "validation_failed");
    }
}

#[tokio::test]
async fn valid_requests_pass_the_extractors() {
    let (status, _, body) = send(
        reject_app(),
        post_json("/echo", Some("application/json"), "{\"qty\": 2}"),
    )
    .await;
    assert_eq!((status, body), (StatusCode::OK, json!({"qty": 2})));
    let (status, _, body) = send(reject_app(), post_json("/optional", None, "")).await;
    assert_eq!((status, body), (StatusCode::OK, json!({"qty": null})));
    let (status, _, body) = send(reject_app(), get_req("/n/7")).await;
    assert_eq!((status, body), (StatusCode::OK, json!({"n": 7})));
}
