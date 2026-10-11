//! `POST /v1/requests`: validate the body, store it as `searching`, then match it at once.
//!
//! Every problem is listed in one 400 `validation_failed` with `details: [{field, message}]`:
//! - `restaurant_id`: a known restaurant, open at some time between now and `needed_by` by its
//!   `hours_today` at Chapel Hill's time (DEMO_MODE: always open; unreadable hours fall back to
//!   the stored flag, as the catalog does);
//! - `needed_by`: an RFC 3339 date-time with an offset, in the future and at most
//!   [`MAX_NEEDED_AHEAD_HOURS`] hours ahead;
//! - `lines`, `tip_cents`, `dropoff` and `dropoff_radius_m`: exactly as an order's body is checked
//!   (`orders::pricing`), against the restaurant's menu.
//!
//! A customer may have at most [`MAX_SEARCHING_PER_CUSTOMER`] requests searching at once; one
//! more is 429 `rate_limited`. Nothing here touches orders or money.

use serde_json::{Value, json};
use sqlx::PgPool;

use super::{CreatePickupRequest, PickupRequest, PickupRequestStatus, matcher};
use crate::catalog::hours::is_open_at;
use crate::error::{ApiError, ApiResult};
use crate::ids;
use crate::orders::pricing::{self, MAX_LINES, Menu};
use crate::profile::validate::{self, FieldError};
use crate::runs::actions::is_rfc3339;
use crate::state::AppState;

/// `needed_by` is at most this many hours ahead.
pub const MAX_NEEDED_AHEAD_HOURS: i32 = 24;
/// Requests one customer may have searching at once.
pub const MAX_SEARCHING_PER_CUSTOMER: i64 = 5;

fn push(errors: &mut Vec<FieldError>, field: &str, message: impl Into<String>) {
    errors.push(FieldError {
        field: field.to_owned(),
        message: message.into(),
    });
}

/// `needed_by` as the database reads it, next to its clock.
#[derive(sqlx::FromRow)]
struct Timing {
    in_future: bool,
    soon_enough: bool,
    /// Minutes since midnight in Chapel Hill now.
    local_minutes: i32,
    /// Whole minutes from now to `needed_by`, rounded up, at most one past the
    /// [`MAX_NEEDED_AHEAD_HOURS`] bound, so a far-future date never overflows `int` and gets the
    /// field's own "at most 24 hours ahead".
    minutes_ahead: i32,
}

/// Reads `needed_by` against the database clock, or the reason it is not a date-time.
async fn timing(pool: &PgPool, needed_by: &str) -> ApiResult<Result<Timing, &'static str>> {
    let result = sqlx::query_as::<_, Timing>(
        "SELECT $1::timestamptz > now() AS in_future, \
                $1::timestamptz <= now() + make_interval(hours => $2) AS soon_enough, \
                (extract(hour FROM now() AT TIME ZONE 'America/New_York') * 60 \
                 + extract(minute FROM now() AT TIME ZONE 'America/New_York'))::int \
                    AS local_minutes, \
                least(greatest(ceil(extract(epoch FROM $1::timestamptz - now()) / 60), 0), \
                      $2 * 60 + 1)::int \
                    AS minutes_ahead",
    )
    .bind(needed_by)
    .bind(MAX_NEEDED_AHEAD_HOURS)
    .fetch_one(pool)
    .await;
    match result {
        Ok(timing) => Ok(Ok(timing)),
        // invalid_datetime_format, datetime_field_overflow (an impossible date such as 02-30)
        Err(sqlx::Error::Database(db))
            if matches!(db.code().as_deref(), Some("22007" | "22008")) =>
        {
            Ok(Err("is not a valid date-time"))
        }
        Err(err) => Err(err.into()),
    }
}

/// Whether `hours_today` is open at some minute from `local_minutes` to `minutes_ahead` later.
/// Unreadable hours fall back to `stored_open`, as the catalog's `is_open` does.
pub fn open_before(
    hours_today: &str,
    stored_open: bool,
    local_minutes: u32,
    minutes_ahead: u32,
) -> bool {
    (0..=minutes_ahead).any(|m| is_open_at(hours_today, local_minutes + m).unwrap_or(stored_open))
}

/// The body's lines as stored: `OrderLineInput[]` as sent.
fn lines_json(body: &CreatePickupRequest) -> Value {
    Value::Array(
        body.lines
            .iter()
            .map(|l| {
                let mut line = json!({
                    "menu_item_id": l.menu_item_id,
                    "qty": l.qty,
                    "choice_ids": l.choice_ids,
                });
                if let Some(note) = &l.note {
                    line["note"] = Value::from(note.as_str());
                }
                line
            })
            .collect(),
    )
}

/// `POST /v1/requests` for `customer_id`: 400 with every problem, 429 past
/// [`MAX_SEARCHING_PER_CUSTOMER`], else the stored request after matching it at once (`matched`
/// with its `run_id`, or `searching`). A matching failure is logged and leaves the request
/// searching for the sweep; it never fails the request.
pub async fn create_request(
    state: &AppState,
    customer_id: &str,
    body: &CreatePickupRequest,
) -> ApiResult<PickupRequest> {
    let pool = &state.pool;
    let mut errors = Vec::new();

    let restaurant: Option<(String, bool)> =
        sqlx::query_as("SELECT hours_today, is_open FROM ferry.restaurants WHERE id = $1")
            .bind(&body.restaurant_id)
            .fetch_optional(pool)
            .await?;
    if restaurant.is_none() {
        push(
            &mut errors,
            "restaurant_id",
            format!("no restaurant has id {:?}", body.restaurant_id),
        );
    }

    let timing = if is_rfc3339(&body.needed_by) {
        match timing(pool, &body.needed_by).await? {
            Ok(timing) => Some(timing),
            Err(message) => {
                push(&mut errors, "needed_by", message);
                None
            }
        }
    } else {
        push(
            &mut errors,
            "needed_by",
            "must be an ISO 8601 date-time with a time zone, such as 2026-10-10T23:00:00Z",
        );
        None
    };
    if let Some(t) = &timing {
        if !t.in_future {
            push(&mut errors, "needed_by", "must be in the future");
        } else if !t.soon_enough {
            push(
                &mut errors,
                "needed_by",
                format!("must be at most {MAX_NEEDED_AHEAD_HOURS} hours ahead"),
            );
        } else if let Some((hours, stored_open)) = &restaurant
            && !state.config.demo_mode
            && !open_before(
                hours,
                *stored_open,
                u32::try_from(t.local_minutes).unwrap_or(0),
                u32::try_from(t.minutes_ahead).unwrap_or(0),
            )
        {
            push(
                &mut errors,
                "restaurant_id",
                format!("is not open before needed_by (hours today: {hours})"),
            );
        }
    }

    // The order's own checks, against this restaurant's menu.
    let menu = if restaurant.is_some() && (1..=MAX_LINES).contains(&body.lines.len()) {
        let item_ids: Vec<String> = body.lines.iter().map(|l| l.menu_item_id.clone()).collect();
        pricing::load_menu(pool, &body.restaurant_id, &item_ids).await?
    } else {
        Menu::new()
    };
    if let Err(line_errors) = pricing::price_lines(&menu, &body.lines) {
        errors.extend(line_errors);
    }
    pricing::check_tip(&mut errors, body.tip_cents);
    let dropoff = pricing::check_dropoff(&mut errors, &body.dropoff);
    pricing::check_place_id(pool, &mut errors, "dropoff", &dropoff).await?;
    validate::check_radius(&mut errors, "dropoff_radius_m", body.dropoff_radius_m);
    if !errors.is_empty() {
        return Err(pricing::invalid(&errors));
    }

    let searching: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ferry.pickup_requests \
         WHERE customer_id = $1 AND status = 'searching' AND needed_by > now()",
    )
    .bind(customer_id)
    .fetch_one(pool)
    .await?;
    if searching >= MAX_SEARCHING_PER_CUSTOMER {
        return Err(ApiError::rate_limited(format!(
            "You already have {searching} requests waiting for a run; try again when one is matched or expires."
        ))
        .with_details(json!({"max_searching": MAX_SEARCHING_PER_CUSTOMER})));
    }

    let id = ids::new_id(ids::REQUEST);
    sqlx::query(
        "INSERT INTO ferry.pickup_requests (id, customer_id, restaurant_id, lines, tip_cents, \
                                            dropoff_place_id, dropoff_label, dropoff_lat, \
                                            dropoff_lng, dropoff_note, needed_by, status, \
                                            dropoff_radius_m) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11::timestamptz, 'searching', $12)",
    )
    .bind(&id)
    .bind(customer_id)
    .bind(&body.restaurant_id)
    .bind(lines_json(body))
    .bind(body.tip_cents)
    .bind(dropoff.id.as_deref())
    .bind(&dropoff.label)
    .bind(dropoff.location.lat)
    .bind(dropoff.location.lng)
    .bind(dropoff.note.as_deref())
    .bind(&body.needed_by)
    // Checked above: 50 to 5000, so it fits.
    .bind(body.dropoff_radius_m.map(|m| m as i32))
    .execute(pool)
    .await?;
    tracing::info!(request_id = %id, restaurant_id = %body.restaurant_id, "pickup request stored");

    if let Err(err) = matcher::match_request(state, &id).await {
        tracing::warn!(request_id = %id, error = %err, "pickup request: matching at once failed; the sweep retries");
    }
    load(pool, &id)
        .await?
        .ok_or_else(|| ApiError::internal("the stored pickup request is gone"))
}

/// The request as `PickupRequest`, or `None` for an unknown id.
pub async fn load(pool: &PgPool, id: &str) -> ApiResult<Option<PickupRequest>> {
    let row: Option<(String, String, Option<String>, Option<i32>)> = sqlx::query_as(
        "SELECT id, status, run_id, dropoff_radius_m FROM ferry.pickup_requests WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.map(|(id, status, run_id, dropoff_radius_m)| {
        Ok(PickupRequest {
            id,
            status: PickupRequestStatus::from_db(&status)?,
            run_id,
            dropoff_radius_m,
        })
    })
    .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::hours::parse_hhmm;

    fn at(hhmm: &str) -> u32 {
        parse_hhmm(hhmm).unwrap()
    }

    #[test]
    fn open_some_time_before_the_deadline() {
        // Sup Dogs, 11:00–03:00: closed at 08:00, open within 3 hours, not within 2.
        let hours = "11:00–03:00";
        assert!(!open_before(hours, true, at("08:00"), 120));
        assert!(open_before(hours, true, at("08:00"), 180));
        // Open now.
        assert!(open_before(hours, false, at("23:00"), 0));
        // Past midnight it is still open until 03:00, then closed until 11:00.
        assert!(open_before(hours, false, at("02:30"), 10));
        assert!(!open_before(hours, true, at("03:30"), 60));
        // Unreadable hours use the stored flag.
        assert!(open_before("Closed", true, at("12:00"), 30));
        assert!(!open_before("Closed", false, at("12:00"), 30));
    }
}
