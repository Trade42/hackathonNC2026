//! `GET /v1/public/push-key` and `POST /v1/push/subscribe`.

use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;
use serde_json::{Map, Value, json};
use sqlx::PgPool;
use url::Origin;

use super::crypto::{self, Vapid};
use super::endpoint;
use crate::error::{ApiError, ApiResult};
use crate::extract::Json;
use crate::jwt::AuthUser;
use crate::state::AppState;

/// Subscriptions kept per user and platform. A new one beyond this replaces the oldest.
pub const MAX_PER_PLATFORM: i64 = 10;
/// The longest native device token accepted, in bytes. APNs tokens are 64 hex characters and FCM
/// tokens about 160. Postgres's btree index behind `UNIQUE (platform, token)` takes at most 2704
/// bytes per row, so a longer cap let a long token through to a 500 (web-push review MINOR 2).
pub const MAX_TOKEN_LEN: usize = 512;
/// `expirationTime` above this (9999-12-31, in ms) is refused rather than stored.
const MAX_EXPIRATION_MS: f64 = 253_402_300_799_999.0;

/// `PushKey` in contract/openapi.yaml.
#[derive(Debug, Serialize)]
pub struct PushKey {
    pub vapid_public_key: String,
}

/// `GET /v1/public/push-key`: the VAPID public key, base64url. Without a usable key pair it is 503
/// `upstream_unavailable`, never an empty key.
pub(crate) async fn push_key(State(state): State<AppState>) -> ApiResult<Json<PushKey>> {
    match Vapid::from_config(&state.config) {
        Ok(vapid) => Ok(Json(PushKey {
            vapid_public_key: vapid.public_key().to_owned(),
        })),
        Err(_) => Err(ApiError::upstream_unavailable(
            "Web Push is not set up on this server.",
        )),
    }
}

/// `POST /v1/push/subscribe`, 204.
///
/// - `web` with `web_push`: stored by endpoint. An endpoint already stored for someone else moves
///   to the caller (one browser, one signed-in user).
/// - `ios` or `android` with `token`: the token is stored (P2: nothing is sent to it).
/// - Only `platform` (a browser without Push, or the app in mock mode): nothing is stored.
///
/// A missing or malformed field is 400 `validation_failed`, with every problem in `details` as
/// `{field, message}`. An endpoint that is not a known push service over https is one of them.
pub(crate) async fn subscribe(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<Value>,
) -> ApiResult<StatusCode> {
    let stub = state.hub.push().and_then(|push| push.stub());
    match parse(&body, stub).map_err(|errors| invalid(&errors))? {
        Request::Nothing => {}
        Request::Web(web) => store_web(&state.pool, &user.user_id, &web).await?,
        Request::Native { platform, token } => {
            store_native(&state.pool, &user.user_id, platform, &token).await?
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

/// One problem with the request.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

fn push(errors: &mut Vec<FieldError>, field: impl Into<String>, message: impl Into<String>) {
    errors.push(FieldError {
        field: field.into(),
        message: message.into(),
    });
}

/// The 400 for `errors`: the first problem as the message, all of them as `details`.
fn invalid(errors: &[FieldError]) -> ApiError {
    let message = errors
        .first()
        .map_or("The push subscription is not valid.".to_owned(), |e| {
            format!("{}: {}.", e.field, e.message)
        });
    ApiError::validation(message).with_details(json!(errors))
}

#[derive(Debug, PartialEq)]
enum Request {
    Nothing,
    Web(WebSubscription),
    Native {
        platform: &'static str,
        token: String,
    },
}

#[derive(Debug, PartialEq)]
struct WebSubscription {
    endpoint: String,
    /// base64url without padding, as decoded and re-encoded.
    p256dh: String,
    auth: String,
    /// `expirationTime`, ms since 1970.
    expiration_ms: Option<f64>,
}

fn parse(body: &Value, stub: Option<&Origin>) -> Result<Request, Vec<FieldError>> {
    let mut errors = Vec::new();
    let Some(body) = body.as_object() else {
        push(&mut errors, "body", "must be a JSON object");
        return Err(errors);
    };
    let platform = match body.get("platform") {
        Some(Value::String(p)) if matches!(p.as_str(), "web" | "ios" | "android") => p.as_str(),
        None | Some(Value::Null) => {
            push(&mut errors, "platform", "is required");
            return Err(errors);
        }
        Some(_) => {
            push(&mut errors, "platform", "must be web, ios or android");
            return Err(errors);
        }
    };
    match platform {
        "web" => match body.get("web_push") {
            None | Some(Value::Null) => Ok(Request::Nothing),
            Some(web_push) => parse_web(web_push, stub).map(Request::Web),
        },
        native => match body.get("token") {
            None | Some(Value::Null) => Ok(Request::Nothing),
            Some(token) => parse_token(token).map(|token| Request::Native {
                platform: if native == "ios" { "ios" } else { "android" },
                token,
            }),
        },
    }
}

fn parse_web(web_push: &Value, stub: Option<&Origin>) -> Result<WebSubscription, Vec<FieldError>> {
    let mut errors = Vec::new();
    let Some(web_push) = web_push.as_object() else {
        push(&mut errors, "web_push", "must be an object");
        return Err(errors);
    };
    let endpoint = match web_push.get("endpoint") {
        Some(Value::String(endpoint)) => match endpoint::check(endpoint, stub) {
            Ok(_) => Some(endpoint.clone()),
            Err(reason) => {
                push(&mut errors, "web_push.endpoint", reason);
                None
            }
        },
        None | Some(Value::Null) => {
            push(&mut errors, "web_push.endpoint", "is required");
            None
        }
        Some(_) => {
            push(&mut errors, "web_push.endpoint", "must be a string");
            None
        }
    };
    let keys = match web_push.get("keys") {
        Some(Value::Object(keys)) => Some(keys),
        None | Some(Value::Null) => None,
        Some(_) => {
            push(&mut errors, "web_push.keys", "must be an object");
            return Err(errors);
        }
    };
    let p256dh = key(&mut errors, keys, "p256dh", |bytes| {
        crypto::parse_ua_public(bytes).map(|_| ())
    });
    let auth = key(&mut errors, keys, "auth", |bytes| {
        crypto::parse_auth_secret(bytes).map(|_| ())
    });
    let expiration_ms = match web_push.get("expirationTime") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_f64() {
            Some(ms) if (0.0..=MAX_EXPIRATION_MS).contains(&ms) => Some(ms),
            _ => {
                push(
                    &mut errors,
                    "web_push.expirationTime",
                    "must be null or a time in milliseconds since 1970",
                );
                None
            }
        },
    };
    match (endpoint, p256dh, auth) {
        (Some(endpoint), Some(p256dh), Some(auth)) if errors.is_empty() => Ok(WebSubscription {
            endpoint,
            p256dh,
            auth,
            expiration_ms,
        }),
        _ => Err(errors),
    }
}

/// `keys.<name>` decoded and checked, re-encoded as base64url without padding.
fn key(
    errors: &mut Vec<FieldError>,
    keys: Option<&Map<String, Value>>,
    name: &str,
    check: impl Fn(&[u8]) -> Result<(), &'static str>,
) -> Option<String> {
    let field = format!("web_push.keys.{name}");
    match keys.and_then(|keys| keys.get(name)) {
        Some(Value::String(text)) => match crypto::b64url_decode(text) {
            Some(bytes) => match check(&bytes) {
                Ok(()) => Some(crypto::b64url(&bytes)),
                Err(reason) => {
                    push(errors, field, reason);
                    None
                }
            },
            None => {
                push(errors, field, "is not base64url");
                None
            }
        },
        None | Some(Value::Null) => {
            push(errors, field, "is required");
            None
        }
        Some(_) => {
            push(errors, field, "must be a string");
            None
        }
    }
}

fn parse_token(token: &Value) -> Result<String, Vec<FieldError>> {
    let mut errors = Vec::new();
    let message = match token {
        Value::String(token) => {
            let token = token.trim();
            if token.is_empty() {
                "must not be empty"
            } else if token.len() > MAX_TOKEN_LEN {
                "is too long"
            } else if token.chars().any(|c| c.is_whitespace() || c.is_control()) {
                "must not contain spaces or control characters"
            } else {
                return Ok(token.to_owned());
            }
        }
        _ => "must be a string",
    };
    push(&mut errors, "token", message);
    Err(errors)
}

/// Upserts on the endpoint, which moves it to `user_id` when someone else had it, then keeps the
/// user's [`MAX_PER_PLATFORM`] newest web subscriptions (this one always among them).
async fn store_web(pool: &PgPool, user_id: &str, web: &WebSubscription) -> ApiResult<()> {
    let mut tx = pool.begin().await?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO ferry.push_subscriptions (user_id, platform, endpoint, p256dh, auth, expiration_time) \
         VALUES ($1, 'web', $2, $3, $4, to_timestamp($5::float8 / 1000)) \
         ON CONFLICT (endpoint) DO UPDATE SET user_id = EXCLUDED.user_id, p256dh = EXCLUDED.p256dh, \
         auth = EXCLUDED.auth, expiration_time = EXCLUDED.expiration_time \
         RETURNING id",
    )
    .bind(user_id)
    .bind(&web.endpoint)
    .bind(&web.p256dh)
    .bind(&web.auth)
    .bind(web.expiration_ms)
    .fetch_one(&mut *tx)
    .await?;
    prune(&mut tx, user_id, "web", id).await?;
    tx.commit().await?;
    Ok(())
}

/// Upserts on (platform, token), which moves the token to `user_id`, then prunes like web.
async fn store_native(
    pool: &PgPool,
    user_id: &str,
    platform: &'static str,
    token: &str,
) -> ApiResult<()> {
    let mut tx = pool.begin().await?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO ferry.push_subscriptions (user_id, platform, token) VALUES ($1, $2, $3) \
         ON CONFLICT (platform, token) DO UPDATE SET user_id = EXCLUDED.user_id \
         RETURNING id",
    )
    .bind(user_id)
    .bind(platform)
    .bind(token)
    .fetch_one(&mut *tx)
    .await?;
    prune(&mut tx, user_id, platform, id).await?;
    tx.commit().await?;
    Ok(())
}

/// Deletes `user_id`'s `platform` subscriptions beyond the newest [`MAX_PER_PLATFORM`], keeping
/// `keep` (the one just stored).
async fn prune(
    tx: &mut sqlx::PgConnection,
    user_id: &str,
    platform: &str,
    keep: i64,
) -> sqlx::Result<()> {
    sqlx::query(
        "DELETE FROM ferry.push_subscriptions \
         WHERE user_id = $1 AND platform = $2 AND id <> $3 AND id NOT IN ( \
           SELECT id FROM ferry.push_subscriptions \
           WHERE user_id = $1 AND platform = $2 AND id <> $3 \
           ORDER BY id DESC LIMIT $4)",
    )
    .bind(user_id)
    .bind(platform)
    .bind(keep)
    .bind(MAX_PER_PLATFORM - 1)
    .execute(tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const P256DH: &str =
        "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
    const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";
    const FCM: &str = "https://fcm.googleapis.com/fcm/send/abc";

    fn fields(errors: &[FieldError]) -> Vec<&str> {
        errors.iter().map(|e| e.field.as_str()).collect()
    }

    #[test]
    fn web_with_a_subscription() {
        let body = json!({"platform": "web", "web_push": {
            "endpoint": FCM, "expirationTime": null, "keys": {"p256dh": P256DH, "auth": format!("{AUTH}==")}
        }});
        assert_eq!(
            parse(&body, None).unwrap(),
            Request::Web(WebSubscription {
                endpoint: FCM.into(),
                p256dh: P256DH.into(),
                auth: AUTH.into(),
                expiration_ms: None,
            })
        );
        let with_expiry = json!({"platform": "web", "web_push": {
            "endpoint": FCM, "expirationTime": 1_790_000_000_000_i64, "keys": {"p256dh": P256DH, "auth": AUTH}
        }});
        assert!(matches!(
            parse(&with_expiry, None).unwrap(),
            Request::Web(WebSubscription { expiration_ms: Some(ms), .. }) if ms == 1_790_000_000_000.0
        ));
    }

    #[test]
    fn platform_alone_stores_nothing() {
        for platform in ["web", "ios", "android"] {
            assert_eq!(
                parse(&json!({"platform": platform}), None).unwrap(),
                Request::Nothing
            );
        }
    }

    #[test]
    fn native_tokens() {
        assert_eq!(
            parse(&json!({"platform": "ios", "token": " abc123 "}), None).unwrap(),
            Request::Native {
                platform: "ios",
                token: "abc123".into()
            }
        );
        let longest = "x".repeat(MAX_TOKEN_LEN);
        assert_eq!(
            parse(&json!({"platform": "ios", "token": longest}), None).unwrap(),
            Request::Native {
                platform: "ios",
                token: longest.clone()
            }
        );
        for token in [
            json!(""),
            json!("a b"),
            json!(7),
            json!("x".repeat(MAX_TOKEN_LEN + 1)),
            json!("x".repeat(5000)),
        ] {
            let errors = parse(&json!({"platform": "android", "token": token}), None).unwrap_err();
            assert_eq!(fields(&errors), ["token"]);
        }
    }

    #[test]
    fn every_problem_is_listed_by_field() {
        let errors = parse(&json!({"platform": "web", "web_push": {}}), None).unwrap_err();
        assert_eq!(
            fields(&errors),
            [
                "web_push.endpoint",
                "web_push.keys.p256dh",
                "web_push.keys.auth"
            ]
        );
        let errors = parse(
            &json!({"platform": "web", "web_push": {
                "endpoint": "http://127.0.0.1/x", "expirationTime": -1,
                "keys": {"p256dh": "AAAA", "auth": "not base64!"}
            }}),
            None,
        )
        .unwrap_err();
        assert_eq!(
            fields(&errors),
            [
                "web_push.endpoint",
                "web_push.keys.p256dh",
                "web_push.keys.auth",
                "web_push.expirationTime"
            ]
        );
        assert_eq!(errors[1].message, "must be 65 bytes");
        assert_eq!(errors[2].message, "is not base64url");
        assert_eq!(
            fields(
                &parse(
                    &json!({"platform": "web", "web_push": {"endpoint": FCM, "keys": "x"}}),
                    None
                )
                .unwrap_err()
            ),
            ["web_push.keys"]
        );
        assert_eq!(fields(&parse(&json!({}), None).unwrap_err()), ["platform"]);
        assert_eq!(
            fields(&parse(&json!({"platform": "fax"}), None).unwrap_err()),
            ["platform"]
        );
        assert_eq!(fields(&parse(&json!([1]), None).unwrap_err()), ["body"]);

        let error = invalid(&parse(&json!({}), None).unwrap_err());
        assert_eq!(error.message, "platform: is required.");
        assert_eq!(
            error.details,
            Some(json!([{"field": "platform", "message": "is required"}]))
        );
    }
}
