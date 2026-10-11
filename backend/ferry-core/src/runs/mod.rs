//! Runs and their state machine: /v1/runs and /v1/public/runs/{id}.
//!
//! - `GET /v1/runs?status=&mine=`: the runs in the viewer's circles, or with `mine=true` their own
//!   active runs as runner in any circle.
//! - `POST /v1/runs`: open a run (the viewer is the runner, `created_via` is `app`), with the
//!   optional going-somewhere plan of contract v1.4.
//! - `GET /v1/runs/{id}`: the run with the orders the viewer may see.
//! - `POST /v1/runs/{id}/close`, `/purchased`, `/picked-up`, `/cancel`: the runner's steps.
//! - `POST /v1/runs/{id}/receipt-upload`: the runner's one-time receipt photo upload token
//!   ([`receipt`]).
//! - `GET /v1/public/runs/{id}`: the four-field preview, without sign-in.
//!
//! For the agent (M2), the same rules as functions of a user, in [`actions`]: [`open_run`] (with
//! [`CreatedVia::Message`]; [`open_run_with_plan`] adds the plan), [`list_runs`], [`run_detail`], [`close_run`], [`mark_purchased`],
//! [`mark_picked_up`] and [`cancel_run`]. Orders are in `crate::orders`.

pub mod access;
pub mod actions;
pub mod live;
pub mod machine;
pub mod notices;
pub mod publish;
pub mod receipt;
pub mod view;

use axum::Router;
use axum::extract::State;
use axum::routing::{get, post};
use serde::Deserialize;

use crate::error::ApiResult;
use crate::extract::{Json, Path, Query};
use crate::jwt::AuthUser;
use crate::state::AppState;

pub use actions::{
    CreateRunBody, CreateRunRequest, CreatedVia, MarkPurchasedRequest, RunPlan, cancel_run,
    close_run, list_runs, mark_picked_up, mark_purchased, open_run, open_run_with_plan, preview,
    run_as_seen_by, run_detail,
};
pub use machine::RunStatus;
pub use receipt::{ReceiptUpload, create_receipt_upload};
pub use view::{Run, RunDetail, RunPreview};

/// This module's routes, with full `/v1/...` paths. Merged into the app by `app::router`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/runs", get(list).post(create))
        .route("/v1/runs/{id}", get(read))
        .route("/v1/runs/{id}/close", post(close))
        .route("/v1/runs/{id}/receipt-upload", post(receipt_upload))
        .route("/v1/runs/{id}/purchased", post(purchased))
        .route("/v1/runs/{id}/picked-up", post(picked_up))
        .route("/v1/runs/{id}/cancel", post(cancel))
        .route("/v1/public/runs/{id}", get(public_preview))
}

/// The query of `GET /v1/runs`; both parameters are optional.
#[derive(Debug, Deserialize)]
pub struct RunsQuery {
    pub status: Option<RunStatus>,
    pub mine: Option<bool>,
}

async fn list(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<RunsQuery>,
) -> ApiResult<Json<Vec<Run>>> {
    Ok(Json(
        list_runs(
            &state,
            &user.user_id,
            query.status,
            query.mine.unwrap_or(false),
        )
        .await?,
    ))
}

async fn create(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<CreateRunBody>,
) -> ApiResult<Json<Run>> {
    Ok(Json(
        open_run_with_plan(
            &state,
            &user.user_id,
            &body.run,
            &body.plan,
            CreatedVia::App,
        )
        .await?,
    ))
}

async fn read(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<RunDetail>> {
    Ok(Json(run_detail(&state, &user.user_id, &id).await?))
}

async fn close(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Run>> {
    Ok(Json(close_run(&state, &user.user_id, &id).await?))
}

async fn receipt_upload(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<ReceiptUpload>> {
    Ok(Json(
        create_receipt_upload(&state, &user.user_id, &id).await?,
    ))
}

async fn purchased(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(request): Json<MarkPurchasedRequest>,
) -> ApiResult<Json<Run>> {
    Ok(Json(
        mark_purchased(&state, &user.user_id, &id, &request).await?,
    ))
}

async fn picked_up(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Run>> {
    Ok(Json(mark_picked_up(&state, &user.user_id, &id).await?))
}

async fn cancel(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Run>> {
    Ok(Json(cancel_run(&state, &user.user_id, &id).await?))
}

async fn public_preview(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<RunPreview>> {
    Ok(Json(preview(&state, &id).await?))
}
