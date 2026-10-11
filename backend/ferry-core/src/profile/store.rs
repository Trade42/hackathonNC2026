//! Reading and writing users and their questionnaire answers in `ferry.users` and `ferry.profiles`.

use sqlx::PgExecutor;
use sqlx::types::Json;

use super::types::{LatLng, Place, Profile, User, UsualTime, WalkingPace};
use crate::error::ApiError;
use crate::phone::mask_phone;

#[derive(sqlx::FromRow)]
struct UserRow {
    id: String,
    phone_e164: Option<String>,
    telegram_username: Option<String>,
    display_name: String,
    avatar_seed: String,
    bubble_color: String,
    onboarding_complete: bool,
    // Every profile column is null when the user has no profiles row (seeded users may not).
    has_profile: bool,
    dropoff_place_id: Option<String>,
    dropoff_label: Option<String>,
    dropoff_lat: Option<f64>,
    dropoff_lng: Option<f64>,
    dropoff_note: Option<String>,
    diet: Option<Vec<String>>,
    avoid: Option<Vec<String>>,
    favorite_cuisines: Option<Vec<String>>,
    favorite_restaurant_ids: Option<Vec<String>>,
    usual_times: Option<Json<Vec<UsualTime>>>,
    walking_pace: Option<String>,
    runner_opt_in: Option<bool>,
    default_tip_cents: Option<i64>,
    notify_push: Option<bool>,
    notify_messages: Option<bool>,
    share_location_when_running: Option<bool>,
    max_dropoff_distance_m: Option<i32>,
}

impl UserRow {
    fn into_user(self) -> Result<User, ApiError> {
        let profile = if self.has_profile {
            let walking_pace = self.walking_pace.as_deref().unwrap_or("normal");
            Profile {
                dropoff_default: match (self.dropoff_label, self.dropoff_lat, self.dropoff_lng) {
                    (Some(label), Some(lat), Some(lng)) => Some(Place {
                        id: self.dropoff_place_id,
                        label,
                        location: LatLng { lat, lng },
                        note: self.dropoff_note,
                    }),
                    _ => None,
                },
                diet: self.diet.unwrap_or_default(),
                avoid: self.avoid.unwrap_or_default(),
                favorite_cuisines: self.favorite_cuisines.unwrap_or_default(),
                favorite_restaurant_ids: self.favorite_restaurant_ids.unwrap_or_default(),
                usual_times: self.usual_times.map(|Json(t)| t).unwrap_or_default(),
                walking_pace: WalkingPace::parse(walking_pace).ok_or_else(|| {
                    ApiError::internal(format!("unknown walking_pace {walking_pace:?}"))
                })?,
                runner_opt_in: self.runner_opt_in.unwrap_or(false),
                default_tip_cents: self.default_tip_cents.unwrap_or(0),
                notify_push: self.notify_push.unwrap_or(true),
                notify_messages: self.notify_messages.unwrap_or(true),
                share_location_when_running: self.share_location_when_running.unwrap_or(false),
                max_dropoff_distance_m: self.max_dropoff_distance_m.map(i64::from),
            }
        } else {
            Profile::default()
        };
        Ok(User {
            id: self.id,
            display_name: self.display_name,
            avatar_seed: self.avatar_seed,
            bubble_color: self.bubble_color,
            phone_masked: self.phone_e164.as_deref().map(mask_phone),
            telegram_username: self.telegram_username,
            profile,
            onboarding_complete: self.onboarding_complete,
        })
    }
}

/// The contract's `User` for `user_id`, or None when there is no such user.
pub async fn load_user<'e>(
    db: impl PgExecutor<'e>,
    user_id: &str,
) -> Result<Option<User>, ApiError> {
    let row: Option<UserRow> = sqlx::query_as(
        "SELECT u.id, u.phone_e164, u.telegram_username, u.display_name, u.avatar_seed, u.bubble_color,
                u.onboarding_complete, p.user_id IS NOT NULL AS has_profile,
                p.dropoff_place_id, p.dropoff_label, p.dropoff_lat, p.dropoff_lng, p.dropoff_note,
                p.diet, p.avoid, p.favorite_cuisines, p.favorite_restaurant_ids, p.usual_times,
                p.walking_pace, p.runner_opt_in, p.default_tip_cents, p.notify_push,
                p.notify_messages, p.share_location_when_running, p.max_dropoff_distance_m
           FROM ferry.users u
           LEFT JOIN ferry.profiles p ON p.user_id = u.id
          WHERE u.id = $1",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await?;
    row.map(UserRow::into_user).transpose()
}

/// Inserts or replaces the user's answers. The caller has validated them and holds the user row.
pub async fn save_profile<'e>(
    db: impl PgExecutor<'e>,
    user_id: &str,
    profile: &Profile,
) -> Result<(), ApiError> {
    let dropoff = profile.dropoff_default.as_ref();
    sqlx::query(
        "INSERT INTO ferry.profiles (
             user_id, dropoff_place_id, dropoff_label, dropoff_lat, dropoff_lng, dropoff_note,
             diet, avoid, favorite_cuisines, favorite_restaurant_ids, usual_times, walking_pace,
             runner_opt_in, default_tip_cents, notify_push, notify_messages,
             share_location_when_running, max_dropoff_distance_m)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)
         ON CONFLICT (user_id) DO UPDATE SET
             dropoff_place_id = EXCLUDED.dropoff_place_id,
             dropoff_label = EXCLUDED.dropoff_label,
             dropoff_lat = EXCLUDED.dropoff_lat,
             dropoff_lng = EXCLUDED.dropoff_lng,
             dropoff_note = EXCLUDED.dropoff_note,
             diet = EXCLUDED.diet,
             avoid = EXCLUDED.avoid,
             favorite_cuisines = EXCLUDED.favorite_cuisines,
             favorite_restaurant_ids = EXCLUDED.favorite_restaurant_ids,
             usual_times = EXCLUDED.usual_times,
             walking_pace = EXCLUDED.walking_pace,
             runner_opt_in = EXCLUDED.runner_opt_in,
             default_tip_cents = EXCLUDED.default_tip_cents,
             notify_push = EXCLUDED.notify_push,
             notify_messages = EXCLUDED.notify_messages,
             share_location_when_running = EXCLUDED.share_location_when_running,
             max_dropoff_distance_m = EXCLUDED.max_dropoff_distance_m",
    )
    .bind(user_id)
    .bind(dropoff.and_then(|p| p.id.as_deref()))
    .bind(dropoff.map(|p| p.label.as_str()))
    .bind(dropoff.map(|p| p.location.lat))
    .bind(dropoff.map(|p| p.location.lng))
    .bind(dropoff.and_then(|p| p.note.as_deref()))
    .bind(&profile.diet)
    .bind(&profile.avoid)
    .bind(&profile.favorite_cuisines)
    .bind(&profile.favorite_restaurant_ids)
    .bind(Json(&profile.usual_times))
    .bind(profile.walking_pace.as_str())
    .bind(profile.runner_opt_in)
    .bind(profile.default_tip_cents)
    .bind(profile.notify_push)
    .bind(profile.notify_messages)
    .bind(profile.share_location_when_running)
    // Validated to 50..=5000, so it fits.
    .bind(profile.max_dropoff_distance_m.map(|m| m as i32))
    .execute(db)
    .await?;
    Ok(())
}
