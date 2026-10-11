//! Extractors whose rejections render the contract's error envelope. Use these instead of
//! `axum::Json`, `axum::extract::Path` and `axum::extract::Query` in handlers.
//!
//! `Path` and `Query` also refuse a value holding a control character (a percent-encoded NUL
//! byte, for example) with 400 `validation_failed` before the handler runs, so no such id or
//! search text reaches Postgres. A JSON body's strings may hold newlines and tabs; a NUL there is
//! refused by Postgres, which `error.rs` maps to the same 400.

use axum::extract::{FromRequest, FromRequestParts, OptionalFromRequest, RawPathParams, Request};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::ApiError;

/// A JSON body (or response). As `Option<Json<T>>` it accepts a request without a body.
#[derive(Debug, Clone, Copy, Default, FromRequest)]
#[from_request(via(axum::Json), rejection(ApiError))]
pub struct Json<T>(pub T);

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

impl<T, S> OptionalFromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Option<Self>, Self::Rejection> {
        let body = <axum::Json<T> as OptionalFromRequest<S>>::from_request(req, state).await?;
        Ok(body.map(|axum::Json(value)| Json(value)))
    }
}

/// Path parameters. A parameter that holds a control character once percent-decoded is 400.
#[derive(Debug, Clone, Copy, Default)]
pub struct Path<T>(pub T);

impl<T, S> FromRequestParts<S> for Path<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        // A parameter that is not UTF-8 once decoded fails here; axum's Path below then gives
        // its own 400 for it.
        if let Ok(params) = RawPathParams::from_request_parts(parts, state).await
            && let Some((name, _)) = params.iter().find(|(_, value)| has_control(value))
        {
            return Err(control_character(name));
        }
        let axum::extract::Path(value) =
            axum::extract::Path::<T>::from_request_parts(parts, state).await?;
        Ok(Path(value))
    }
}

/// Query parameters. A name or value that holds a control character once decoded is 400.
#[derive(Debug, Clone, Copy, Default)]
pub struct Query<T>(pub T);

impl<T, S> FromRequestParts<S> for Query<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let query = parts.uri.query().unwrap_or_default();
        if let Some((name, _)) = url::form_urlencoded::parse(query.as_bytes())
            .find(|(name, value)| has_control(name) || has_control(value))
        {
            return Err(control_character(&name));
        }
        let axum::extract::Query(value) = axum::extract::Query::<T>::try_from_uri(&parts.uri)?;
        Ok(Query(value))
    }
}

/// True when `value` holds a control character (Unicode Cc: U+0000 to U+001F and U+007F to
/// U+009F), such as a NUL byte, which Postgres refuses in text.
pub fn has_control(value: &str) -> bool {
    value.chars().any(char::is_control)
}

fn control_character(name: &str) -> ApiError {
    // Only the parameter's name, which comes from the route or is clean of control characters
    // itself when it is a query name that holds one; never the value.
    let name: String = name.chars().filter(|c| !c.is_control()).take(64).collect();
    ApiError::validation(format!("{name} holds a control character."))
        .with_details(serde_json::json!({ "parameter": name }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::routing::get;
    use http_body_util::BodyExt;
    use serde::Deserialize;
    use serde_json::Value;
    use tower::ServiceExt;

    #[derive(Deserialize)]
    struct Search {
        q: Option<String>,
    }

    fn app() -> Router {
        Router::new()
            .route(
                "/things/{id}",
                get(|Path(id): Path<String>| async move { id }),
            )
            .route(
                "/search",
                get(|Query(search): Query<Search>| async move { search.q.unwrap_or_default() }),
            )
    }

    async fn call(uri: &str) -> (u16, String) {
        let response = app()
            .oneshot(axum::http::Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    fn code(body: &str) -> String {
        let value: Value = serde_json::from_str(body).unwrap();
        value["error"]["code"].as_str().unwrap().to_owned()
    }

    #[tokio::test]
    async fn control_characters_in_paths_are_validation_failed() {
        for uri in [
            "/things/run_%00x",
            "/things/%00",
            "/things/a%0Ab",
            "/things/a%09b",
            "/things/a%7Fb",
            "/things/a%C2%85b",
        ] {
            let (status, body) = call(uri).await;
            assert_eq!(status, 400, "{uri}: {body}");
            assert_eq!(code(&body), "validation_failed", "{uri}");
            assert!(body.contains("\"parameter\":\"id\""), "{uri}: {body}");
        }
        // Not UTF-8 once decoded: axum's own rejection, in the envelope.
        let (status, body) = call("/things/%FF").await;
        assert_eq!((status, code(&body).as_str()), (400, "validation_failed"));
        // Ordinary ids, including encoded spaces and non-ASCII letters, pass through.
        assert_eq!(call("/things/run_demo").await, (200, "run_demo".into()));
        assert_eq!(call("/things/a%20b").await, (200, "a b".into()));
        assert_eq!(call("/things/caf%C3%A9").await, (200, "café".into()));
    }

    #[tokio::test]
    async fn control_characters_in_queries_are_validation_failed() {
        for uri in [
            "/search?q=%00",
            "/search?q=burrito%0D%0A",
            "/search?other=%01",
            "/search?%00=x",
        ] {
            let (status, body) = call(uri).await;
            assert_eq!(status, 400, "{uri}: {body}");
            assert_eq!(code(&body), "validation_failed", "{uri}");
            assert!(!body.contains('\0'), "{uri}");
        }
        assert_eq!(call("/search?q=taco+bell").await, (200, "taco bell".into()));
        assert_eq!(call("/search?q=%F0%9F%8C%AE").await, (200, "🌮".into()));
        assert_eq!(call("/search").await, (200, String::new()));
    }

    #[test]
    fn control_means_unicode_cc() {
        for bad in [
            "\0", "a\u{1}", "\t", "\n", "\r", "\u{7f}", "\u{85}", "\u{9f}",
        ] {
            assert!(has_control(bad), "{bad:?}");
        }
        for ok in ["", "run_demo", "café", "🌮", "a b", "\u{a0}", "\u{200b}"] {
            assert!(!has_control(ok), "{ok:?}");
        }
    }
}
