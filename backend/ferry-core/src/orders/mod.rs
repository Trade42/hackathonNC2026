//! Orders: /v1/runs/{id}/orders, /v1/runs/{id}/quote, /v1/orders, handoff and cancel. Pickup
//! requests (/v1/requests) are a later task.
//!
//! - `POST /v1/runs/{id}/orders` → 201: price from the menu, hold the total in Escrow.
//! - `POST /v1/runs/{id}/quote`: the same pricing with no side effects.
//! - `GET /v1/orders` (`mine` is accepted; the list is always the viewer's own orders).
//! - `GET /v1/orders/{id}`, `POST /v1/orders/{id}/handoff`, `POST /v1/orders/{id}/cancel`.
//!
//! For the agent (M2), the same rules as functions of a user, in [`actions`]: [`price_order`]
//! (a priced draft to confirm), [`place_order`], [`cancel_order`], [`order_status`] (with ETA,
//! distance and the location's age), [`get_order`], [`list_orders`] and [`confirm_handoff`].

pub mod actions;
pub mod machine;
pub mod pricing;
pub mod steps;
pub mod view;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, OptionalFromRequest, Request, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use serde::Deserialize;

use crate::error::{ApiError, ApiResult};
use crate::extract::{Json, Path, Query};
use crate::jwt::AuthUser;
use crate::payments::OrderQuote;
use crate::state::AppState;

pub use actions::{
    HandoffRequest, OrderStatusReport, cancel_order, confirm_handoff, get_order, list_orders,
    order_status, place_order, price_order, quote_order,
};
pub use machine::OrderStatus;
pub use pricing::{CreateOrderRequest, OrderLineInput, PricedOrder};
pub use view::{Order, OrderLine, OrderTimelineEntry};

/// This module's routes, with full `/v1/...` paths. Merged into the app by `app::router`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/runs/{id}/orders", post(place))
        .route("/v1/runs/{id}/quote", post(quote))
        .route("/v1/orders", get(list))
        .route("/v1/orders/{id}", get(read))
        .route("/v1/orders/{id}/handoff", post(handoff))
        .route("/v1/orders/{id}/cancel", post(cancel))
}

/// The query of `GET /v1/orders`. `mine` is optional; with or without it the list is the viewer's
/// own orders as customer, the only orders a list may show.
#[derive(Debug, Deserialize)]
pub struct OrdersQuery {
    #[allow(dead_code)]
    pub mine: Option<bool>,
}

async fn place(
    State(state): State<AppState>,
    user: AuthUser,
    Path(run_id): Path<String>,
    Json(request): Json<CreateOrderRequest>,
) -> ApiResult<(StatusCode, Json<Order>)> {
    let order = place_order(&state, &user.user_id, &run_id, &request).await?;
    Ok((StatusCode::CREATED, Json(order)))
}

async fn quote(
    State(state): State<AppState>,
    user: AuthUser,
    Path(run_id): Path<String>,
    Json(request): Json<CreateOrderRequest>,
) -> ApiResult<Json<OrderQuote>> {
    Ok(Json(
        quote_order(&state, &user.user_id, &run_id, &request).await?,
    ))
}

async fn list(
    State(state): State<AppState>,
    user: AuthUser,
    Query(_query): Query<OrdersQuery>,
) -> ApiResult<Json<Vec<Order>>> {
    Ok(Json(list_orders(&state, &user.user_id).await?))
}

async fn read(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Order>> {
    Ok(Json(get_order(&state, &user.user_id, &id).await?))
}

async fn handoff(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    body: Request,
) -> ApiResult<Json<Order>> {
    let request = handoff_body(body).await?;
    Ok(Json(
        confirm_handoff(&state, &user.user_id, &id, request.code.as_deref()).await?,
    ))
}

/// The handoff's body, which the contract makes optional: no body, or an empty one even with
/// `Content-Type: application/json` (runs-orders review MINOR 1), is `{}`. A non-empty body is
/// read as `Option<Json<_>>` reads it: without a content type it is ignored, and otherwise it must
/// be a JSON `HandoffRequest` (400 when not).
async fn handoff_body(request: Request) -> ApiResult<HandoffRequest> {
    let headers = request.headers().clone();
    let bytes = Bytes::from_request(request, &())
        .await
        .map_err(|err| ApiError::from(JsonRejection::from(err)))?;
    if bytes.is_empty() {
        return Ok(HandoffRequest::default());
    }
    let mut request = Request::new(Body::from(bytes));
    *request.headers_mut() = headers;
    let body =
        <Json<HandoffRequest> as OptionalFromRequest<()>>::from_request(request, &()).await?;
    Ok(body.map(|Json(b)| b).unwrap_or_default())
}

async fn cancel(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Order>> {
    Ok(Json(cancel_order(&state, &user.user_id, &id).await?))
}
