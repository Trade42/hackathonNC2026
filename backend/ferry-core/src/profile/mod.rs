//! The signed-in user: /v1/me, /v1/me/profile, /v1/me/stats.

use axum::Router;
use axum::extract::State;
use axum::routing::{get, put};
use serde_json::json;

use crate::error::{ApiError, ApiResult};
use crate::extract::Json;
use crate::jwt::AuthUser;
use crate::state::AppState;

pub mod store;
pub mod types;
pub mod validate;

pub use store::load_user;
pub use types::{MeStats, Profile, UpdateProfileRequest, User};

/// This module's routes, with full `/v1/...` paths. Merged into the app by `app::router`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/me", get(get_me))
        .route("/v1/me/profile", put(update_profile))
        .route("/v1/me/stats", get(get_stats))
}

/// A valid token whose user no longer exists: the client should sign in again.
fn gone() -> ApiError {
    ApiError::unauthorized("This account no longer exists. Sign in again.")
}

/// `GET /v1/me`.
async fn get_me(State(state): State<AppState>, user: AuthUser) -> ApiResult<Json<User>> {
    let me = load_user(&state.pool, &user.user_id)
        .await?
        .ok_or_else(gone)?;
    Ok(Json(me))
}

/// `PUT /v1/me/profile`: validates every field, saves the answers (and the display name when
/// given), and sets onboarding_complete.
async fn update_profile(
    State(state): State<AppState>,
    user: AuthUser,
    Json(request): Json<UpdateProfileRequest>,
) -> ApiResult<Json<User>> {
    let mut errors = validate::check_request(&request);
    let profile = &request.profile;

    let ids = &profile.favorite_restaurant_ids;
    if !ids.is_empty() && ids.len() <= validate::MAX_LIST_ITEMS {
        let known: Vec<String> =
            sqlx::query_scalar("SELECT id FROM ferry.restaurants WHERE id = ANY($1)")
                .bind(ids)
                .fetch_all(&state.pool)
                .await?;
        for (i, id) in ids.iter().enumerate() {
            if !known.contains(id) {
                errors.push(validate::FieldError {
                    field: format!("profile.favorite_restaurant_ids[{i}]"),
                    message: format!("no restaurant has id {id:?}"),
                });
            }
        }
    }
    if let Some(place_id) = profile
        .dropoff_default
        .as_ref()
        .and_then(|p| p.id.as_deref())
    {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM ferry.places WHERE id = $1)")
                .bind(place_id)
                .fetch_one(&state.pool)
                .await?;
        if !exists {
            errors.push(validate::FieldError {
                field: "profile.dropoff_default.id".into(),
                message: format!("no place has id {place_id:?}"),
            });
        }
    }
    if let Some(first) = errors.first() {
        return Err(
            ApiError::validation(format!("{}: {}.", first.field, first.message))
                .with_details(json!(errors)),
        );
    }

    let display_name = request.display_name.as_deref().map(str::trim);
    let mut tx = state.pool.begin().await?;
    // Locks the user row, so concurrent saves apply one after the other.
    let updated = sqlx::query(
        "UPDATE ferry.users
            SET onboarding_complete = true, display_name = COALESCE($2, display_name)
          WHERE id = $1",
    )
    .bind(&user.user_id)
    .bind(display_name)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(gone());
    }
    store::save_profile(&mut *tx, &user.user_id, profile).await?;
    tx.commit().await?;

    let me = load_user(&state.pool, &user.user_id)
        .await?
        .ok_or_else(gone)?;
    Ok(Json(me))
}

/// `GET /v1/me/stats`: orders placed (not cancelled or refunded), runs made (not cancelled),
/// payouts received, and the CO2 saved by delivered orders the user ordered or ran.
async fn get_stats(State(state): State<AppState>, user: AuthUser) -> ApiResult<Json<MeStats>> {
    let (orders, runs, earned_cents, co2_saved_g): (i64, i64, i64, f64) = sqlx::query_as(
        "SELECT
             (SELECT count(*) FROM ferry.orders
               WHERE customer_id = $1 AND status NOT IN ('cancelled', 'refunded')),
             (SELECT count(*) FROM ferry.runs
               WHERE runner_id = $1 AND status <> 'cancelled'),
             (SELECT COALESCE(sum(amount_cents), 0)::bigint FROM ferry.ledger_entries
               WHERE kind = 'payout' AND debit_account = 'user:' || $1 || ':checking'),
             (SELECT COALESCE(sum(o.co2_saved_g), 0)::double precision
                FROM ferry.orders o JOIN ferry.runs r ON r.id = o.run_id
               WHERE (o.customer_id = $1 OR r.runner_id = $1)
                 AND o.status IN ('delivered', 'settled'))",
    )
    .bind(&user.user_id)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(MeStats {
        orders,
        runs,
        earned_cents,
        co2_saved_g,
    }))
}
