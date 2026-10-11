//! Stored notifications: `notify`, `GET /v1/notifications` and `POST /v1/notifications/{id}/read`.

use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgExecutor, PgPool};

use super::hub::Event;
use crate::error::{ApiError, ApiResult};
use crate::extract::{Json, Path};
use crate::ids;
use crate::jwt::AuthUser;
use crate::state::AppState;

/// `GET /v1/notifications` returns at most this many, newest first.
pub const LIST_LIMIT: i64 = 100;

/// `NotificationType` in contract/openapi.yaml.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationType {
    Invite,
    RunOpen,
    OrderUpdate,
    Payout,
    System,
}

impl NotificationType {
    pub const ALL: [NotificationType; 5] = [
        NotificationType::Invite,
        NotificationType::RunOpen,
        NotificationType::OrderUpdate,
        NotificationType::Payout,
        NotificationType::System,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            NotificationType::Invite => "invite",
            NotificationType::RunOpen => "run_open",
            NotificationType::OrderUpdate => "order_update",
            NotificationType::Payout => "payout",
            NotificationType::System => "system",
        }
    }
}

impl TryFrom<String> for NotificationType {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::ALL
            .into_iter()
            .find(|t| t.as_str() == value)
            .ok_or_else(|| format!("unknown notification type {value:?}"))
    }
}

/// `Notification` in contract/openapi.yaml, as stored in `ferry.notifications`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Notification {
    pub id: String,
    #[serde(rename = "type")]
    #[sqlx(rename = "type", try_from = "String")]
    pub kind: NotificationType,
    pub title: String,
    pub body: String,
    /// Free-form object for the client, for example `{"run_id": "run_demo"}`.
    pub data: Value,
    /// ISO 8601 UTC with milliseconds.
    pub created_at: String,
    pub read: bool,
}

/// The `Notification` columns of `ferry.notifications`, with `created_at` as ISO 8601 UTC.
macro_rules! notification_columns {
    () => {
        "id, type, title, body, data, \
         to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS created_at, \
         read_at IS NOT NULL AS read"
    };
}

/// Stores a notification for `user_id` and returns it, without publishing it. Use this inside a
/// transaction (`&mut *tx`), then after the commit publish
/// `Event::notification_created(&notification)` with `state.hub.send_to(user_id, ..)`. Outside a
/// transaction, [`notify`] does both. `data` must be a JSON object; null means `{}`.
pub async fn create_notification<'e>(
    executor: impl PgExecutor<'e>,
    user_id: &str,
    kind: NotificationType,
    title: &str,
    body: &str,
    data: Value,
) -> ApiResult<Notification> {
    let data = match data {
        Value::Null => Value::Object(Default::default()),
        Value::Object(_) => data,
        _ => {
            return Err(ApiError::internal(
                "notification data must be a JSON object",
            ));
        }
    };
    let notification = sqlx::query_as::<_, Notification>(concat!(
        "INSERT INTO ferry.notifications (id, user_id, type, title, body, data) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING ",
        notification_columns!()
    ))
    .bind(ids::new_id(ids::NOTIFICATION))
    .bind(user_id)
    .bind(kind.as_str())
    .bind(title)
    .bind(body)
    .bind(&data)
    .fetch_one(executor)
    .await?;
    Ok(notification)
}

/// Stores a notification for `user_id` and publishes `notification.created` to them.
pub async fn notify(
    state: &AppState,
    user_id: &str,
    kind: NotificationType,
    title: &str,
    body: &str,
    data: Value,
) -> ApiResult<Notification> {
    let notification = create_notification(&state.pool, user_id, kind, title, body, data).await?;
    state
        .hub
        .send_to(user_id, &Event::notification_created(&notification));
    Ok(notification)
}

/// `GET /v1/notifications`: the viewer's own, newest first, at most [`LIST_LIMIT`].
pub(crate) async fn list(
    State(pool): State<PgPool>,
    user: AuthUser,
) -> ApiResult<Json<Vec<Notification>>> {
    let notifications = sqlx::query_as::<_, Notification>(concat!(
        "SELECT ",
        notification_columns!(),
        " FROM ferry.notifications n WHERE n.user_id = $1 \
         ORDER BY n.created_at DESC, n.id DESC LIMIT $2"
    ))
    .bind(&user.user_id)
    .bind(LIST_LIMIT)
    .fetch_all(&pool)
    .await?;
    Ok(Json(notifications))
}

/// `POST /v1/notifications/{id}/read`: 204, also when it was already read. Someone else's or an
/// unknown id is 404 `not_found`.
pub(crate) async fn mark_read(
    State(pool): State<PgPool>,
    user: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let updated: Option<String> = sqlx::query_scalar(
        "UPDATE ferry.notifications SET read_at = COALESCE(read_at, now()) \
         WHERE id = $1 AND user_id = $2 RETURNING id",
    )
    .bind(&id)
    .bind(&user.user_id)
    .fetch_optional(&pool)
    .await?;
    match updated {
        Some(_) => Ok(StatusCode::NO_CONTENT),
        None => Err(ApiError::not_found("No such notification.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_match_the_contract() {
        let yaml = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contract/openapi.yaml"
        ))
        .unwrap();
        let line = yaml
            .lines()
            .skip_while(|l| l.trim() != "NotificationType:")
            .find(|l| l.trim_start().starts_with("enum:"))
            .unwrap();
        let listed: Vec<&str> = line
            .trim()
            .trim_start_matches("enum:")
            .trim()
            .trim_matches(['[', ']'])
            .split(',')
            .map(str::trim)
            .collect();
        let ours: Vec<&str> = NotificationType::ALL.iter().map(|t| t.as_str()).collect();
        assert_eq!(listed, ours);
        for t in NotificationType::ALL {
            assert_eq!(serde_json::to_value(t).unwrap(), t.as_str());
            assert_eq!(NotificationType::try_from(t.as_str().to_owned()), Ok(t));
        }
        assert!(NotificationType::try_from("nope".to_owned()).is_err());
    }
}
