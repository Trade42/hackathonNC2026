//! What each run endpoint does, as functions of a user, so the agent (M2) can call them from
//! Telegram under exactly the endpoints' rules. The HTTP handlers in `runs/mod.rs` only unwrap the
//! request.
//!
//! Every change runs in one transaction that locks the run first (`access::lock`), moves the
//! state through `RunStatus::transition` and `OrderStatus::transition` (409 `conflict` when
//! illegal), calls the payments op in the same transaction (a failed op rolls everything back),
//! stores its notifications, and commits. Then it publishes the events (`publish`) and posts the
//! ledger rows to Nessie (`Payments::post`, at most 2.5 s).

use rand::RngExt;
use serde::Deserialize;
use sqlx::PgConnection;

use super::access::{self, RunAccess};
use super::live::LivePlan;
use super::machine::RunStatus;
use super::notices;
use super::publish::{Outbox, publish};
use super::view::{self, Run, RunDetail, RunPreview};
use crate::auth::demo::DemoPersona;
use crate::catalog::types::Place;
use crate::circles;
use crate::error::{ApiError, ApiResult};
use crate::orders::pricing;
use crate::orders::steps::{self, Moved};
use crate::orders::{OrderStatus, view as order_view};
use crate::payments::ledger::{LedgerEntry, keys};
use crate::payments::{Payments, ops};
use crate::profile::validate::{self, FieldError};
use crate::state::AppState;

/// `window_minutes` is 1 to this.
pub const MAX_WINDOW_MINUTES: i64 = 120;
/// `leaving_at` is at most this many hours ahead.
pub const MAX_LEAVING_AHEAD_HOURS: i32 = 24;
/// `returning_at` (contract v1.4) is at most this many hours after `leaving_at`.
pub const MAX_RETURN_AFTER_HOURS: i32 = 24;
/// `receipt_path` is at most this long.
pub const MAX_RECEIPT_PATH_CHARS: usize = 300;
/// Random characters after `run_`: 36^24 is about 2^124, so a run id cannot be guessed and
/// `GET /v1/public/runs/{id}` reveals nothing to someone without the link.
pub const RUN_ID_RANDOM_LEN: usize = 24;

/// `CreateRunRequest` in contract/openapi.yaml.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CreateRunRequest {
    pub restaurant_id: String,
    pub circle_id: String,
    pub leaving_at: String,
    pub window_minutes: i64,
}

/// The going-somewhere plan of `POST /v1/runs` (contract v1.4): `CreateRunRequest`'s optional
/// `returning_at`, `return_place` and `dropoff_radius_m`, kept apart from [`CreateRunRequest`] so
/// callers that build that struct are unchanged. Left out or null, each is not given; a run with
/// no `dropoff_radius_m` takes the runner's `Profile.max_dropoff_distance_m`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct RunPlan {
    #[serde(default)]
    pub returning_at: Option<String>,
    #[serde(default)]
    pub return_place: Option<Place>,
    #[serde(default)]
    pub dropoff_radius_m: Option<i64>,
}

/// The body of `POST /v1/runs`: [`CreateRunRequest`] and the [`RunPlan`] in one JSON object.
/// It is read as one plain struct, [`CreateRunFields`], and then split: `#[serde(flatten)]` would
/// drop the field's name from a type error, which must say `window_minutes: invalid type …`
/// (contract-v1-4 review MINOR 1).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(from = "CreateRunFields")]
pub struct CreateRunBody {
    pub run: CreateRunRequest,
    pub plan: RunPlan,
}

/// Every field of `CreateRunRequest` in contract/openapi.yaml, as the JSON body has them.
#[derive(Deserialize)]
struct CreateRunFields {
    restaurant_id: String,
    circle_id: String,
    leaving_at: String,
    window_minutes: i64,
    #[serde(default)]
    returning_at: Option<String>,
    #[serde(default)]
    return_place: Option<Place>,
    #[serde(default)]
    dropoff_radius_m: Option<i64>,
}

impl From<CreateRunFields> for CreateRunBody {
    fn from(fields: CreateRunFields) -> Self {
        Self {
            run: CreateRunRequest {
                restaurant_id: fields.restaurant_id,
                circle_id: fields.circle_id,
                leaving_at: fields.leaving_at,
                window_minutes: fields.window_minutes,
            },
            plan: RunPlan {
                returning_at: fields.returning_at,
                return_place: fields.return_place,
                dropoff_radius_m: fields.dropoff_radius_m,
            },
        }
    }
}

/// A [`RunPlan`] that passed [`check_plan`]: the place cleaned, the radius in range.
#[derive(Debug, Clone, Default, PartialEq)]
struct CheckedPlan {
    returning_at: Option<String>,
    return_place: Option<Place>,
    dropoff_radius_m: Option<i32>,
}

/// `RunCreatedVia`: how the run was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreatedVia {
    /// By the agent, from a message ("heading to Cosmic Cantina at 6").
    Message,
    /// In the app, `POST /v1/runs`.
    App,
}

impl CreatedVia {
    pub fn as_str(self) -> &'static str {
        match self {
            CreatedVia::Message => "message",
            CreatedVia::App => "app",
        }
    }
}

/// `MarkPurchasedRequest` in contract/openapi.yaml.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MarkPurchasedRequest {
    pub receipt_total_cents: i64,
    #[serde(default)]
    pub receipt_path: Option<String>,
}

/// A new run id: `run_` plus [`RUN_ID_RANDOM_LEN`] characters from `[0-9a-z]`.
pub fn new_run_id() -> String {
    const ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut rng = rand::rng();
    let mut id = String::with_capacity(4 + RUN_ID_RANDOM_LEN);
    id.push_str("run_");
    for _ in 0..RUN_ID_RANDOM_LEN {
        id.push(ALPHABET[rng.random_range(0..ALPHABET.len())] as char);
    }
    id
}

fn field_error(field: &str, message: impl Into<String>) -> ApiError {
    crate::orders::pricing::invalid(&[FieldError {
        field: field.to_owned(),
        message: message.into(),
    }])
}

/// True when `s` has the shape of an RFC 3339 date-time with an offset, such as
/// `2026-10-10T22:00:00Z` or `2026-10-10T18:00:00.000-04:00`. Postgres then rejects impossible
/// dates.
pub fn is_rfc3339(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 20 {
        return false;
    }
    let digits = |range: std::ops::Range<usize>| b[range].iter().all(u8::is_ascii_digit);
    let shape = digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10)
        && matches!(b[10], b'T' | b't')
        && digits(11..13)
        && b[13] == b':'
        && digits(14..16)
        && b[16] == b':'
        && digits(17..19);
    if !shape {
        return false;
    }
    let mut rest = &b[19..];
    if let Some(fraction) = rest.strip_prefix(b".") {
        let n = fraction.iter().take_while(|c| c.is_ascii_digit()).count();
        if n == 0 {
            return false;
        }
        rest = &fraction[n..];
    }
    match rest {
        [b'Z' | b'z'] => true,
        [sign, h1, h2, b':', m1, m2] => {
            matches!(sign, b'+' | b'-') && [h1, h2, m1, m2].iter().all(|c| c.is_ascii_digit())
        }
        _ => false,
    }
}

/// The run as `viewer` sees it now, with an ETA when there is a recent ping. The caller has
/// checked that the viewer may see it.
pub async fn run_as_seen_by(state: &AppState, run_id: &str, viewer: &str) -> ApiResult<Run> {
    let record = view::load_one(&state.pool, &state.config, run_id)
        .await?
        .ok_or_else(|| ApiError::not_found("No such run."))?;
    let live = if record.may_see_location(viewer) {
        LivePlan::for_run(state, &record).await?
    } else {
        None
    };
    let walker = view::walkers(&state.pool, &[viewer.to_owned()])
        .await?
        .remove(viewer);
    Ok(record.as_seen_by(viewer, walker, live.and_then(|l| l.run_eta(viewer))))
}

/// The plan's problems that need no clock: `returning_at`'s shape, `return_place` as an order's
/// drop-off is checked (its id among `GET /v1/places`), and the radius's range. 400 listing every
/// problem.
async fn check_plan(state: &AppState, plan: &RunPlan) -> ApiResult<CheckedPlan> {
    let mut errors = Vec::new();
    if let Some(returning_at) = &plan.returning_at
        && !is_rfc3339(returning_at)
    {
        errors.push(FieldError {
            field: "returning_at".into(),
            message: "must be an ISO 8601 date-time with a time zone, such as 2026-10-10T22:40:00Z"
                .into(),
        });
    }
    let return_place = match &plan.return_place {
        Some(place) => {
            let place = pricing::check_place(&mut errors, "return_place", place);
            pricing::check_place_id(&state.pool, &mut errors, "return_place", &place).await?;
            Some(place)
        }
        None => None,
    };
    validate::check_radius(&mut errors, "dropoff_radius_m", plan.dropoff_radius_m);
    if !errors.is_empty() {
        return Err(pricing::invalid(&errors));
    }
    Ok(CheckedPlan {
        returning_at: plan.returning_at.clone(),
        return_place,
        // In range (50 to 5000), so it fits.
        dropoff_radius_m: plan.dropoff_radius_m.map(|m| m as i32),
    })
}

/// Maps Postgres's "not a valid date-time" errors to a 400 on `field`.
fn bad_time(field: &'static str) -> impl FnOnce(sqlx::Error) -> ApiError {
    move |err| match &err {
        // invalid_datetime_format, datetime_field_overflow, invalid_time_zone_displacement_value
        // (an offset such as +16:00)
        sqlx::Error::Database(db)
            if matches!(db.code().as_deref(), Some("22007" | "22008" | "22009")) =>
        {
            field_error(field, "is not a valid date-time")
        }
        _ => err.into(),
    }
}

/// Opens a run with `user_id` as the runner and no plan: [`open_run_with_plan`] with
/// [`RunPlan::default()`], as the agent opens runs ([`CreatedVia::Message`]). The run still takes
/// the runner's `Profile.max_dropoff_distance_m` as its `dropoff_radius_m`.
pub async fn open_run(
    state: &AppState,
    user_id: &str,
    request: &CreateRunRequest,
    via: CreatedVia,
) -> ApiResult<Run> {
    open_run_with_plan(state, user_id, request, &RunPlan::default(), via).await
}

/// Opens a run with `user_id` as the runner (`POST /v1/runs`, or [`open_run`]).
/// `window_closes_at` is `leaving_at` plus `window_minutes`. The plan (contract v1.4) is stored as
/// given; with no `dropoff_radius_m`, the run takes the runner's `Profile.max_dropoff_distance_m`.
///
/// - 400 `validation_failed`: `window_minutes` outside 1 to [`MAX_WINDOW_MINUTES`]; `leaving_at`
///   not an RFC 3339 date-time, more than [`MAX_LEAVING_AHEAD_HOURS`] hours ahead, or so early
///   that the window has already closed; an unknown `restaurant_id`; `returning_at` not an RFC
///   3339 date-time, not after `leaving_at`, or more than [`MAX_RETURN_AFTER_HOURS`] hours after
///   it; a `return_place` that is not a valid drop-off `Place`; `dropoff_radius_m` outside 50 to
///   5000.
/// - 404 `not_found`: the user is not a member of `circle_id` (or it does not exist), as for
///   `GET /v1/circles/{id}`.
///
/// Each other member of the circle gets a `run_open` notification, and every member `run.updated`.
/// In a public community (UNC Chapel Hill has every user), nobody gets a `run_open` (so no push to
/// everyone): the trip shows in every member's `GET /v1/runs` and in `run.updated` to those
/// connected.
pub async fn open_run_with_plan(
    state: &AppState,
    user_id: &str,
    request: &CreateRunRequest,
    plan: &RunPlan,
    via: CreatedVia,
) -> ApiResult<Run> {
    if !(1..=MAX_WINDOW_MINUTES).contains(&request.window_minutes) {
        return Err(field_error(
            "window_minutes",
            format!("must be 1 to {MAX_WINDOW_MINUTES}"),
        ));
    }
    let window_minutes = request.window_minutes as i32;
    if !is_rfc3339(&request.leaving_at) {
        return Err(field_error(
            "leaving_at",
            "must be an ISO 8601 date-time with a time zone, such as 2026-10-10T22:00:00Z",
        ));
    }
    let plan = check_plan(state, plan).await?;
    let is_member: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM ferry.circle_members WHERE circle_id = $1 AND user_id = $2)",
    )
    .bind(&request.circle_id)
    .bind(user_id)
    .fetch_one(&state.pool)
    .await?;
    if !is_member {
        return Err(ApiError::not_found("No such circle."));
    }
    let names: Option<(String, String)> = sqlx::query_as(
        "SELECT u.display_name, rs.name FROM ferry.users u CROSS JOIN ferry.restaurants rs \
         WHERE u.id = $1 AND rs.id = $2",
    )
    .bind(user_id)
    .bind(&request.restaurant_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some((runner_name, restaurant_name)) = names else {
        return Err(field_error(
            "restaurant_id",
            format!("no restaurant has id {:?}", request.restaurant_id),
        ));
    };
    let (still_open, soon_enough): (bool, bool) = sqlx::query_as(
        "SELECT $1::timestamptz + make_interval(mins => $2) > now(), \
                $1::timestamptz <= now() + make_interval(hours => $3)",
    )
    .bind(&request.leaving_at)
    .bind(window_minutes)
    .bind(MAX_LEAVING_AHEAD_HOURS)
    .fetch_one(&state.pool)
    .await
    .map_err(bad_time("leaving_at"))?;
    if !still_open {
        return Err(field_error(
            "leaving_at",
            "is so early that the order window would already be closed",
        ));
    }
    if !soon_enough {
        return Err(field_error(
            "leaving_at",
            format!("must be at most {MAX_LEAVING_AHEAD_HOURS} hours ahead"),
        ));
    }
    if let Some(returning_at) = &plan.returning_at {
        let (after, soon_after): (bool, bool) = sqlx::query_as(
            "SELECT $2::timestamptz > $1::timestamptz, \
                    $2::timestamptz <= $1::timestamptz + make_interval(hours => $3)",
        )
        .bind(&request.leaving_at)
        .bind(returning_at)
        .bind(MAX_RETURN_AFTER_HOURS)
        .fetch_one(&state.pool)
        .await
        .map_err(bad_time("returning_at"))?;
        if !after {
            return Err(field_error("returning_at", "must be after leaving_at"));
        }
        if !soon_after {
            return Err(field_error(
                "returning_at",
                format!("must be at most {MAX_RETURN_AFTER_HOURS} hours after leaving_at"),
            ));
        }
    }

    let run_id = new_run_id();
    let mut tx = state.pool.begin().await?;
    let place = plan.return_place.as_ref();
    // With no radius in the plan, the runner's profile walk distance (null without one).
    let closes_local: String = sqlx::query_scalar(
        "INSERT INTO ferry.runs (id, restaurant_id, runner_id, circle_id, status, created_via, \
                                 leaving_at, window_closes_at, returning_at, return_place_id, \
                                 return_label, return_lat, return_lng, return_note, \
                                 dropoff_radius_m) \
         VALUES ($1, $2, $3, $4, 'open', $5, $6::timestamptz, \
                 $6::timestamptz + make_interval(mins => $7), $8::timestamptz, $9, $10, $11, $12, \
                 $13, COALESCE($14::integer, \
                     (SELECT p.max_dropoff_distance_m FROM ferry.profiles p WHERE p.user_id = $3))) \
         RETURNING to_char(window_closes_at AT TIME ZONE 'America/New_York', 'FMHH12:MI AM')",
    )
    .bind(&run_id)
    .bind(&request.restaurant_id)
    .bind(user_id)
    .bind(&request.circle_id)
    .bind(via.as_str())
    .bind(&request.leaving_at)
    .bind(window_minutes)
    .bind(plan.returning_at.as_deref())
    .bind(place.and_then(|p| p.id.as_deref()))
    .bind(place.map(|p| p.label.as_str()))
    .bind(place.map(|p| p.location.lat))
    .bind(place.map(|p| p.location.lng))
    .bind(place.and_then(|p| p.note.as_deref()))
    .bind(plan.dropoff_radius_m)
    .fetch_one(&mut *tx)
    .await?;
    let members: Vec<String> = if circles::is_public(&mut *tx, &request.circle_id).await? {
        Vec::new()
    } else {
        sqlx::query_scalar(
            "SELECT user_id FROM ferry.circle_members WHERE circle_id = $1 AND user_id <> $2 \
             ORDER BY joined_at, user_id",
        )
        .bind(&request.circle_id)
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?
    };
    let mut outbox = Outbox::new(&run_id);
    outbox.run = true;
    for member in &members {
        outbox.notifications.push(
            notices::run_open(
                &mut tx,
                &state.config,
                member,
                &run_id,
                &request.circle_id,
                &runner_name,
                &restaurant_name,
                &closes_local,
            )
            .await?,
        );
    }
    tx.commit().await?;
    tracing::info!(run_id, circle_id = %request.circle_id, via = via.as_str(), "run opened");

    publish(state, &outbox).await;
    run_as_seen_by(state, &run_id, user_id).await
}

/// `GET /v1/runs`: the runs in the user's circles (real activity, newest first), or with `mine`
/// their own active runs in any circle; `status` narrows either. No ETAs in lists.
pub async fn list_runs(
    state: &AppState,
    user_id: &str,
    status: Option<RunStatus>,
    mine: bool,
) -> ApiResult<Vec<Run>> {
    let records = view::load_list(&state.pool, &state.config, user_id, status, mine).await?;
    let walker = view::walkers(&state.pool, &[user_id.to_owned()])
        .await?
        .remove(user_id);
    Ok(records
        .iter()
        .map(|r| r.as_seen_by(user_id, walker, None))
        .collect())
}

/// `GET /v1/runs/{id}`: the run and its orders. The runner sees every order (cancelled and
/// refunded ones too); anyone else in the circle only their own. 404 for anyone else.
pub async fn run_detail(state: &AppState, user_id: &str, run_id: &str) -> ApiResult<RunDetail> {
    let access = access::read(&state.pool, run_id, user_id).await?;
    let record = view::load_one(&state.pool, &state.config, run_id)
        .await?
        .ok_or_else(|| ApiError::not_found("No such run."))?;
    let live = if record.may_see_location(user_id) {
        LivePlan::for_run(state, &record).await?
    } else {
        None
    };
    let customer = (!access.is_runner).then_some(user_id);
    let mut orders = order_view::load_for_run(&state.pool, run_id, customer).await?;
    order_view::sign_receipts(state, &mut orders).await;
    let walker = view::walkers(&state.pool, &[user_id.to_owned()])
        .await?
        .remove(user_id);
    let live = live.as_ref();
    Ok(RunDetail {
        run: record.as_seen_by(user_id, walker, live.and_then(|l| l.run_eta(user_id))),
        orders: orders
            .iter()
            .map(|o| o.as_seen_by(user_id, live.and_then(|l| l.order_eta(&o.id))))
            .collect(),
    })
}

/// `GET /v1/public/runs/{id}`, no sign-in: only the four preview fields. 404 for an unknown id.
pub async fn preview(state: &AppState, run_id: &str) -> ApiResult<RunPreview> {
    Ok(view::load_one(&state.pool, &state.config, run_id)
        .await?
        .ok_or_else(|| ApiError::not_found("No such run."))?
        .preview())
}

/// Locks the run for a runner-only change: 404 unless the user may see it, 403 unless they are
/// its runner, 409 for demo history and for a move the machine does not allow.
async fn lock_for_runner(
    conn: &mut PgConnection,
    run_id: &str,
    user_id: &str,
    next: RunStatus,
) -> ApiResult<RunAccess> {
    let run = access::lock(&mut *conn, run_id, user_id).await?;
    run.ensure_runner()?;
    run.ensure_real()?;
    run.status()?.transition(next)?;
    Ok(run)
}

async fn set_run_status(conn: &mut PgConnection, run_id: &str, status: RunStatus) -> ApiResult<()> {
    sqlx::query("UPDATE ferry.runs SET status = $2 WHERE id = $1")
        .bind(run_id)
        .bind(status.as_str())
        .execute(conn)
        .await?;
    Ok(())
}

/// The runner's and restaurant's names for notification texts.
async fn names(conn: &mut PgConnection, run_id: &str) -> ApiResult<(String, String)> {
    Ok(sqlx::query_as(
        "SELECT u.display_name, rs.name FROM ferry.runs r \
         JOIN ferry.users u ON u.id = r.runner_id \
         JOIN ferry.restaurants rs ON rs.id = r.restaurant_id \
         WHERE r.id = $1",
    )
    .bind(run_id)
    .fetch_one(conn)
    .await?)
}

/// Publishes, posts the rows, and answers with the run as the runner sees it.
async fn finish(
    state: &AppState,
    payments: &Payments,
    outbox: Outbox,
    user_id: &str,
) -> ApiResult<Run> {
    publish(state, &outbox).await;
    payments.post(&outbox.ledger).await;
    run_as_seen_by(state, &outbox.run_id, user_id).await
}

fn order_ids(moved: &[Moved]) -> Vec<String> {
    moved.iter().map(|m| m.id.clone()).collect()
}

/// `POST /v1/runs/{id}/close` (runner only): open → closed, and Escrow funds the runner's Ferry
/// Card with the live orders' subtotal + tax (`payments::ops::fund_card_in`). Orders keep their
/// status. A run with no orders funds nothing; it can then only be cancelled.
pub async fn close_run(state: &AppState, user_id: &str, run_id: &str) -> ApiResult<Run> {
    let payments = Payments::from_state(state);
    let mut tx = state.pool.begin().await?;
    lock_for_runner(&mut tx, run_id, user_id, RunStatus::Closed).await?;
    set_run_status(&mut tx, run_id, RunStatus::Closed).await?;
    let rows = ops::fund_card_in(&mut tx, run_id).await?;
    tx.commit().await?;
    tracing::info!(
        run_id,
        funded = rows.first().map_or(0, |r| r.amount_cents),
        "run closed"
    );

    let mut outbox = Outbox::new(run_id);
    outbox.run = true;
    outbox.ledger = rows;
    finish(state, &payments, outbox, user_id).await
}

/// `POST /v1/runs/{id}/purchased` (runner only): closed → purchased through the Ferry Card's
/// purchase at the restaurant's merchant (`payments::ops::card_purchase_in`), and every funded
/// order → purchased. A lower receipt refunds the customers pro rata; up to 5% higher is covered
/// from Escrow; more is 409 `conflict` with nothing changed. `receipt_path`, when given, must
/// start with `<run_id>/` (400 otherwise).
pub async fn mark_purchased(
    state: &AppState,
    user_id: &str,
    run_id: &str,
    request: &MarkPurchasedRequest,
) -> ApiResult<Run> {
    let payments = Payments::from_state(state);
    let mut tx = state.pool.begin().await?;
    lock_for_runner(&mut tx, run_id, user_id, RunStatus::Purchased).await?;
    if request.receipt_total_cents < 1 {
        return Err(field_error("receipt_total_cents", "must be at least 1"));
    }
    let receipt_path = match request.receipt_path.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(path) => {
            let prefix = format!("{run_id}/");
            if !path.starts_with(&prefix)
                || path.len() == prefix.len()
                || path.chars().count() > MAX_RECEIPT_PATH_CHARS
                // `..` anywhere, as the runs_check CHECK refuses it, not only as a segment.
                || path.contains("..")
                || path.split('/').any(|s| s == ".")
                || path.contains("//")
                || path.contains('\\')
                || path.chars().any(char::is_control)
            {
                return Err(field_error(
                    "receipt_path",
                    format!("must be a file under {prefix}, the path from receipt-upload"),
                ));
            }
            Some(path.to_owned())
        }
    };

    let rows = ops::card_purchase_in(&mut tx, run_id, request.receipt_total_cents).await?;
    sqlx::query(
        "UPDATE ferry.runs SET status = 'purchased', receipt_total_cents = $2, receipt_path = $3 \
         WHERE id = $1",
    )
    .bind(run_id)
    .bind(request.receipt_total_cents)
    .bind(receipt_path.as_deref())
    .execute(&mut *tx)
    .await?;
    let moved =
        steps::move_orders(&mut tx, run_id, OrderStatus::Funded, OrderStatus::Purchased).await?;
    let (runner, restaurant) = names(&mut tx, run_id).await?;
    let runner = notices::name_or(&runner, "Your runner").to_owned();
    let mut outbox = Outbox::new(run_id);
    for order in &moved {
        let refund = rows
            .iter()
            .find(|r| r.idempotency_key == keys::refund_receipt(&order.id))
            .map(|r| r.amount_cents);
        let body = match refund {
            Some(cents) => format!(
                "The receipt from {restaurant} came in lower, so {} went back to your Ferry wallet.",
                notices::money(cents)
            ),
            None => format!("{runner} paid {restaurant} with the Ferry Card."),
        };
        outbox.notifications.push(
            notices::order_update(
                &mut tx,
                &state.config,
                &order.customer_id,
                &order.id,
                run_id,
                OrderStatus::Purchased,
                &format!("{runner} bought your food"),
                &body,
            )
            .await?,
        );
    }
    tx.commit().await?;
    tracing::info!(
        run_id,
        receipt_total_cents = request.receipt_total_cents,
        orders = moved.len(),
        "run purchased"
    );

    outbox.run = true;
    outbox.orders = order_ids(&moved);
    outbox.ledger = rows;
    finish(state, &payments, outbox, user_id).await
}

/// `POST /v1/runs/{id}/picked-up` (runner only): purchased → delivering, and every purchased
/// order → picked_up. No money moves. In DEMO_MODE, a demo persona's run then starts the runner
/// simulator's walk (`demo::simulator::start_walk`) in the background.
pub async fn mark_picked_up(state: &AppState, user_id: &str, run_id: &str) -> ApiResult<Run> {
    let payments = Payments::from_state(state);
    let mut tx = state.pool.begin().await?;
    lock_for_runner(&mut tx, run_id, user_id, RunStatus::Delivering).await?;
    set_run_status(&mut tx, run_id, RunStatus::Delivering).await?;
    let moved = steps::move_orders(
        &mut tx,
        run_id,
        OrderStatus::Purchased,
        OrderStatus::PickedUp,
    )
    .await?;
    let (runner, restaurant) = names(&mut tx, run_id).await?;
    let runner = notices::name_or(&runner, "Your runner").to_owned();
    let mut outbox = Outbox::new(run_id);
    for order in &moved {
        outbox.notifications.push(
            notices::order_update(
                &mut tx,
                &state.config,
                &order.customer_id,
                &order.id,
                run_id,
                OrderStatus::PickedUp,
                &format!("{runner} picked up your food"),
                &format!(
                    "On the way from {restaurant} to {}. Have your handoff code ready.",
                    order.dropoff_label
                ),
            )
            .await?,
        );
    }
    tx.commit().await?;
    tracing::info!(run_id, orders = moved.len(), "run picked up");
    // Demo step 4 (docs/DEMO.md): a demo persona's walk starts by itself. Spawned, so the 200
    // waits on nothing, and best-effort: a walk that cannot start never fails the pickup.
    if state.config.demo_mode && DemoPersona::ALL.iter().any(|p| p.user_id() == user_id) {
        let (s, id) = (state.clone(), run_id.to_owned());
        tokio::spawn(async move {
            if let Err(err) = crate::demo::simulator::start_walk(&s, &id).await {
                tracing::warn!(run_id = %id, error = %err, "demo walk not started");
            }
        });
    }

    outbox.run = true;
    outbox.orders = order_ids(&moved);
    finish(state, &payments, outbox, user_id).await
}

/// `POST /v1/runs/{id}/cancel` (runner only, before the purchase): open or closed → cancelled.
/// Every live order gets its full hold back (`payments::ops::refund_cancelled_in`, which first
/// returns the order's share from the card when it was funded) and → refunded.
pub async fn cancel_run(state: &AppState, user_id: &str, run_id: &str) -> ApiResult<Run> {
    let payments = Payments::from_state(state);
    let mut tx = state.pool.begin().await?;
    lock_for_runner(&mut tx, run_id, user_id, RunStatus::Cancelled).await?;
    set_run_status(&mut tx, run_id, RunStatus::Cancelled).await?;
    let live: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM ferry.orders WHERE run_id = $1 AND status = 'funded' ORDER BY id",
    )
    .bind(run_id)
    .fetch_all(&mut *tx)
    .await?;
    let mut rows: Vec<LedgerEntry> = Vec::new();
    for order_id in &live {
        rows.extend(ops::refund_cancelled_in(&mut tx, order_id).await?);
    }
    let moved =
        steps::move_orders(&mut tx, run_id, OrderStatus::Funded, OrderStatus::Refunded).await?;
    let (runner, restaurant) = names(&mut tx, run_id).await?;
    let runner = notices::name_or(&runner, "Your runner").to_owned();
    let mut outbox = Outbox::new(run_id);
    for order in &moved {
        outbox.notifications.push(
            notices::order_update(
                &mut tx,
                &state.config,
                &order.customer_id,
                &order.id,
                run_id,
                OrderStatus::Refunded,
                &format!("{runner} cancelled the run to {restaurant}"),
                &format!(
                    "Your order is refunded: {} is back in your Ferry wallet.",
                    notices::money(order.total_cents)
                ),
            )
            .await?,
        );
    }
    tx.commit().await?;
    tracing::info!(run_id, orders = moved.len(), "run cancelled");

    outbox.run = true;
    outbox.orders = order_ids(&moved);
    outbox.ledger = rows;
    finish(state, &payments, outbox, user_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What axum's `Json` makes of `body` as a [`CreateRunBody`]: the body or the rejection's text.
    fn read_body(body: &str) -> Result<CreateRunBody, String> {
        axum::Json::<CreateRunBody>::from_bytes(body.as_bytes())
            .map(|axum::Json(body)| body)
            .map_err(|rejection| rejection.body_text())
    }

    #[test]
    fn a_create_run_body_splits_into_the_request_and_the_plan() {
        let base = r#""restaurant_id": "rst_cosmic", "circle_id": "cir_hinton4", "leaving_at": "2026-10-10T22:00:00Z", "window_minutes": 30"#;
        let run = CreateRunRequest {
            restaurant_id: "rst_cosmic".into(),
            circle_id: "cir_hinton4".into(),
            leaving_at: "2026-10-10T22:00:00Z".into(),
            window_minutes: 30,
        };
        assert_eq!(
            read_body(&format!("{{{base}}}")).unwrap(),
            CreateRunBody {
                run: run.clone(),
                plan: RunPlan::default(),
            }
        );
        let body = read_body(&format!(
            r#"{{{base}, "returning_at": "2026-10-10T23:00:00Z", "return_place": null, "dropoff_radius_m": 400, "extra": 1}}"#
        ))
        .unwrap();
        assert_eq!(body.run, run);
        assert_eq!(
            body.plan,
            RunPlan {
                returning_at: Some("2026-10-10T23:00:00Z".into()),
                return_place: None,
                dropoff_radius_m: Some(400),
            }
        );
    }

    #[test]
    fn a_wrong_type_in_a_create_run_body_names_its_field() {
        let fields = [
            ("restaurant_id", "7"),
            ("circle_id", "7"),
            ("leaving_at", "7"),
            ("window_minutes", r#""x""#),
            ("returning_at", "7"),
            ("return_place", r#""Hinton""#),
            ("dropoff_radius_m", "400.5"),
        ];
        for (field, wrong) in fields {
            let body: Vec<String> = [
                ("restaurant_id", r#""rst_cosmic""#),
                ("circle_id", r#""cir_hinton4""#),
                ("leaving_at", r#""2026-10-10T22:00:00Z""#),
                ("window_minutes", "30"),
            ]
            .into_iter()
            .filter(|(name, _)| *name != field)
            .chain([(field, wrong)])
            .map(|(name, value)| format!(r#""{name}": {value}"#))
            .collect();
            let message = read_body(&format!("{{{}}}", body.join(", "))).unwrap_err();
            assert!(
                message.contains(&format!(": {field}: invalid type")),
                "{field}: {message}"
            );
        }
    }

    #[test]
    fn run_ids_are_long_and_random() {
        let a = new_run_id();
        let b = new_run_id();
        assert_ne!(a, b);
        assert!(a.starts_with("run_"));
        assert_eq!(a.len(), 4 + RUN_ID_RANDOM_LEN);
        assert!(
            a[4..]
                .bytes()
                .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase())
        );
    }

    #[test]
    fn rfc3339_shapes() {
        for ok in [
            "2026-10-10T22:00:00Z",
            "2026-10-10T22:00:00.123Z",
            "2026-10-10T18:00:00-04:00",
        ] {
            assert!(is_rfc3339(ok), "{ok}");
        }
        for bad in [
            "",
            "6pm",
            "2026-10-10",
            "2026-10-10T22:00:00",
            "2026-10-10 22:00:00Z",
        ] {
            assert!(!is_rfc3339(bad), "{bad}");
        }
    }

    #[test]
    fn created_via_strings() {
        assert_eq!(CreatedVia::App.as_str(), "app");
        assert_eq!(CreatedVia::Message.as_str(), "message");
    }
}
