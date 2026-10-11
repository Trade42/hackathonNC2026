//! The contract's error envelope: HTTP status plus `{"error": {"code", "message", "details"?}}`.

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::Value;

/// `ErrorCode` in contract/openapi.yaml.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Unauthorized,
    Forbidden,
    NotFound,
    ValidationFailed,
    Conflict,
    WindowClosed,
    InsufficientFunds,
    UpstreamUnavailable,
    RateLimited,
}

impl ErrorCode {
    pub const ALL: [ErrorCode; 9] = [
        ErrorCode::Unauthorized,
        ErrorCode::Forbidden,
        ErrorCode::NotFound,
        ErrorCode::ValidationFailed,
        ErrorCode::Conflict,
        ErrorCode::WindowClosed,
        ErrorCode::InsufficientFunds,
        ErrorCode::UpstreamUnavailable,
        ErrorCode::RateLimited,
    ];

    /// The status each code uses (docs/DECISIONS.md, Contract). `validation_failed` is 400
    /// everywhere (Sal's contract v1.2 decision).
    pub fn status(self) -> StatusCode {
        match self {
            ErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
            ErrorCode::Forbidden => StatusCode::FORBIDDEN,
            ErrorCode::NotFound => StatusCode::NOT_FOUND,
            ErrorCode::ValidationFailed => StatusCode::BAD_REQUEST,
            ErrorCode::Conflict => StatusCode::CONFLICT,
            ErrorCode::WindowClosed => StatusCode::CONFLICT,
            ErrorCode::InsufficientFunds => StatusCode::PAYMENT_REQUIRED,
            ErrorCode::UpstreamUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        }
    }
}

/// An error response in the contract's envelope. Handlers return `Result<T, ApiError>`.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: ErrorCode,
    pub message: String,
    pub details: Option<Value>,
    /// Its cause is already logged at ERROR ([`ApiError::internal`]).
    logged: bool,
}

/// A response extension on an error response whose cause is already logged at ERROR (one built
/// by [`ApiError::internal`]). `app::router` logs one ERROR line for every other 5xx, so each 5xx
/// gets exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorLogged;

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Serialize)]
struct Envelope<'a> {
    error: Body<'a>,
}

#[derive(Serialize)]
struct Body<'a> {
    code: ErrorCode,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<&'a Value>,
}

impl ApiError {
    /// An error with the code's usual status.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            status: code.status(),
            code,
            message: message.into(),
            details: None,
            logged: false,
        }
    }

    pub fn with_status(mut self, status: StatusCode) -> Self {
        self.status = status;
        self
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unauthorized, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Forbidden, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    /// 400 `validation_failed`.
    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ValidationFailed, message)
    }

    /// 409 `conflict`, for illegal state transitions.
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Conflict, message)
    }

    pub fn window_closed(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::WindowClosed, message)
    }

    pub fn insufficient_funds(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InsufficientFunds, message)
    }

    pub fn upstream_unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::UpstreamUnavailable, message)
    }

    pub fn rate_limited(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::RateLimited, message)
    }

    /// A bug or an unexpected failure: logs `err` and answers 500. The contract has no internal
    /// error code, so the envelope says `upstream_unavailable` with a generic message.
    ///
    /// This log line is the only one a 500 gets: the response carries [`ErrorLogged`], so
    /// `app::router` adds none. It carries the request's `request_id` from the request span. Never
    /// pass a request body or a secret in `err`.
    pub fn internal(err: impl std::fmt::Display) -> Self {
        tracing::error!(error = %err, "internal error");
        let mut error = Self::new(
            ErrorCode::UpstreamUnavailable,
            "Something went wrong on our side.",
        )
        .with_status(StatusCode::INTERNAL_SERVER_ERROR);
        error.logged = true;
        error
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {:?}: {}",
            self.status.as_u16(),
            self.code,
            self.message
        )
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Envelope {
            error: Body {
                code: self.code,
                message: &self.message,
                details: self.details.as_ref(),
            },
        };
        let mut response = (self.status, Json(body)).into_response();
        if self.logged {
            response.extensions_mut().insert(ErrorLogged);
        }
        response
    }
}

/// Postgres errors (SQLSTATE) that mean a request carried a value the database cannot take: a NUL
/// byte in text, a number or date out of range, a string too long for its column or index. The
/// handler should have refused the value itself; when it did not, the answer is still 400
/// `validation_failed`, never 500. A CHECK violation (23514) is not on the list: it is 400 only
/// for the constraints in [`INPUT_CHECKS`].
pub const BAD_INPUT_SQLSTATES: [(&str, &str); 13] = [
    ("22001", "string_data_right_truncation"),
    ("22003", "numeric_value_out_of_range"),
    ("22007", "invalid_datetime_format"),
    ("22008", "datetime_field_overflow"),
    ("22009", "invalid_time_zone_displacement_value"),
    ("22015", "interval_field_overflow"),
    ("22021", "character_not_in_repertoire"),
    ("22025", "invalid_escape_sequence"),
    ("22026", "string_data_length_mismatch"),
    ("22P02", "invalid_text_representation"),
    ("22P03", "invalid_binary_representation"),
    ("22P05", "untranslatable_character"),
    ("54000", "program_limit_exceeded"),
];

/// SQLSTATE `check_violation`.
pub const CHECK_VIOLATION: &str = "23514";

/// The CHECK constraints, as (table, constraint), that a request's own value can violate: a 23514
/// on one of them is 400 `validation_failed`, like [`BAD_INPUT_SQLSTATES`]. Every other CHECK
/// guards an invariant ferry-core keeps itself (the ledger's amounts and accounts, an order's
/// total, a line's total, statuses and kinds), so its violation is a bug in ferry-core: 500, with
/// its ERROR line, never a 400 that blames the client. Add a constraint here only when request
/// input reaches it, with a comment saying how; better still, refuse the value in the handler.
pub const INPUT_CHECKS: [(&str, &str); 1] = [
    // `runs.receipt_path` must start with the run's id and hold no `..` anywhere. The runner sends
    // it to `POST /v1/runs/{id}/purchased`, and runs' own check refuses only a `..` segment, so
    // `run_x/receipt..jpg` reaches the CHECK.
    ("runs", "runs_check"),
];

/// The SQLSTATE's name when the error is bad input: one of [`BAD_INPUT_SQLSTATES`], or a CHECK
/// violation on one of [`INPUT_CHECKS`].
pub fn bad_input(
    code: &str,
    table: Option<&str>,
    constraint: Option<&str>,
) -> Option<&'static str> {
    if code == CHECK_VIOLATION {
        let check = (table.unwrap_or_default(), constraint.unwrap_or_default());
        return INPUT_CHECKS.contains(&check).then_some("check_violation");
    }
    bad_input_sqlstate(code)
}

/// The SQLSTATE's name when it is one of [`BAD_INPUT_SQLSTATES`].
pub fn bad_input_sqlstate(code: &str) -> Option<&'static str> {
    BAD_INPUT_SQLSTATES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, name)| *name)
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        match err {
            sqlx::Error::RowNotFound => Self::not_found("Not found."),
            sqlx::Error::PoolTimedOut
            | sqlx::Error::PoolClosed
            | sqlx::Error::Io(_)
            | sqlx::Error::Tls(_) => {
                tracing::warn!(error = %err, "database unavailable");
                Self::upstream_unavailable("The database is unavailable. Try again shortly.")
            }
            sqlx::Error::Database(db) => {
                let code = db.code().unwrap_or_default();
                match bad_input(&code, db.table(), db.constraint()) {
                    // Postgres's message can quote the value ("invalid input syntax for type
                    // integer: ..."), so neither the log nor the client gets it.
                    Some(name) => {
                        tracing::warn!(
                            sqlstate = %code,
                            sqlstate_name = name,
                            constraint = db.constraint().unwrap_or_default(),
                            "the database refused a request value"
                        );
                        Self::validation("A value in the request is invalid.")
                    }
                    None => Self::internal(sqlx::Error::Database(db)),
                }
            }
            other => Self::internal(other),
        }
    }
}

// Axum's rejections, rendered in the envelope as 400 `validation_failed`: JSON that does not
// parse, JSON of the wrong shape, no JSON content type, a body over the size limit, and bad path
// or query values.
impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        Self::validation(rejection.body_text())
    }
}

impl From<QueryRejection> for ApiError {
    fn from(rejection: QueryRejection) -> Self {
        Self::validation(rejection.body_text())
    }
}

impl From<PathRejection> for ApiError {
    fn from(rejection: PathRejection) -> Self {
        match rejection {
            PathRejection::FailedToDeserializePathParams(e) => Self::validation(e.body_text()),
            // A route declared without the parameters its handler asks for: a bug.
            other => Self::internal(other.body_text()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    async fn render(err: ApiError) -> (StatusCode, Value) {
        let response = err.into_response();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn status_for_each_code() {
        let expected = [
            (ErrorCode::Unauthorized, 401, "unauthorized"),
            (ErrorCode::Forbidden, 403, "forbidden"),
            (ErrorCode::NotFound, 404, "not_found"),
            (ErrorCode::ValidationFailed, 400, "validation_failed"),
            (ErrorCode::Conflict, 409, "conflict"),
            (ErrorCode::WindowClosed, 409, "window_closed"),
            (ErrorCode::InsufficientFunds, 402, "insufficient_funds"),
            (ErrorCode::UpstreamUnavailable, 503, "upstream_unavailable"),
            (ErrorCode::RateLimited, 429, "rate_limited"),
        ];
        assert_eq!(expected.len(), ErrorCode::ALL.len());
        for (code, status, name) in expected {
            let (s, body) = render(ApiError::new(code, "msg")).await;
            assert_eq!(s.as_u16(), status);
            assert_eq!(
                body,
                serde_json::json!({"error": {"code": name, "message": "msg"}})
            );
        }
    }

    #[tokio::test]
    async fn details_are_optional_and_any_json() {
        let (_, body) =
            render(ApiError::validation("bad").with_details(serde_json::json!(["qty"]))).await;
        assert_eq!(body["error"]["details"], serde_json::json!(["qty"]));
    }

    #[tokio::test]
    async fn internal_hides_the_cause() {
        let (status, body) = render(ApiError::internal(
            "password authentication failed for user x",
        ))
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["code"], "upstream_unavailable");
        assert!(!body.to_string().contains("password"));
    }

    #[test]
    fn only_internal_errors_are_marked_as_logged() {
        let response = ApiError::internal("boom").into_response();
        assert_eq!(
            response.extensions().get::<ErrorLogged>(),
            Some(&ErrorLogged)
        );
        for err in [
            ApiError::upstream_unavailable("Nessie is down."),
            ApiError::validation("bad"),
            ApiError::upstream_unavailable("x").with_status(StatusCode::INTERNAL_SERVER_ERROR),
        ] {
            let response = err.into_response();
            assert!(response.extensions().get::<ErrorLogged>().is_none());
        }
    }

    #[test]
    fn codes_match_the_contract() {
        let yaml = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contract/openapi.yaml"
        ))
        .unwrap();
        let block: Vec<String> = yaml
            .lines()
            .skip_while(|l| l.trim() != "ErrorCode:")
            .skip_while(|l| l.trim() != "enum:")
            .skip(1)
            .take_while(|l| l.trim_start().starts_with("- "))
            .map(|l| l.trim_start().trim_start_matches("- ").trim().to_owned())
            .collect();
        let ours: Vec<String> = ErrorCode::ALL
            .iter()
            .map(|c| {
                serde_json::to_value(c)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(block, ours);
    }
}
