//! The catalog's read endpoints: areas, restaurants, menus and drop-off places. All need a
//! signed-in user; the demo seeder fills the tables from contract/fixtures.

use std::collections::HashMap;

use axum::extract::State;
use serde::Deserialize;
use sqlx::PgPool;

use super::hours::is_open_at;
use super::media::public_media_url;
use super::types::{
    Area, LatLng, Menu, MenuCategory, MenuChoice, MenuItem, MenuOption, Place, Restaurant,
};
use super::walk::{Pace, walk_minutes};
use crate::error::{ApiError, ApiResult};
use crate::extract::{Json, Path, Query};
use crate::jwt::AuthUser;
use crate::state::AppState;

/// `GET /v1/areas`, by name.
pub async fn list_areas(_user: AuthUser, State(pool): State<PgPool>) -> ApiResult<Json<Vec<Area>>> {
    let rows: Vec<(String, String, f64, f64)> = sqlx::query_as(
        "SELECT id, name, center_lat, center_lng FROM ferry.areas ORDER BY name, id",
    )
    .fetch_all(&pool)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, name, lat, lng)| Area {
                id,
                name,
                center: LatLng { lat, lng },
            })
            .collect(),
    ))
}

/// `GET /v1/places`: the named drop-off places, by label.
pub async fn list_places(
    _user: AuthUser,
    State(pool): State<PgPool>,
) -> ApiResult<Json<Vec<Place>>> {
    let rows: Vec<(String, String, f64, f64, Option<String>)> =
        sqlx::query_as("SELECT id, label, lat, lng, note FROM ferry.places ORDER BY label, id")
            .fetch_all(&pool)
            .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, label, lat, lng, note)| Place {
                id: Some(id),
                label,
                location: LatLng { lat, lng },
                note,
            })
            .collect(),
    ))
}

/// The query of `GET /v1/restaurants`; every parameter is optional.
#[derive(Debug, Deserialize)]
pub struct RestaurantFilter {
    pub area_id: Option<String>,
    pub q: Option<String>,
    pub open_now: Option<bool>,
}

#[derive(sqlx::FromRow)]
struct RestaurantRow {
    id: String,
    name: String,
    area_id: String,
    cuisine: Vec<String>,
    lat: f64,
    lng: f64,
    address: String,
    is_open: bool,
    hours_today: String,
    partner: bool,
    price_level: i16,
    image_url: Option<String>,
}

/// `GET /v1/restaurants?area_id=&q=&open_now=`, by name.
///
/// `q` matches a substring of the name or of any cuisine, ignoring case. `is_open` comes from
/// `hours_today` at the current time in America/New_York (the stored flag only when the hours
/// cannot be read); DEMO_MODE makes every restaurant open. `open_now=true` keeps the open ones.
/// `walk_minutes_from_dropoff` is from the viewer's profile drop-off at their walking pace.
pub async fn list_restaurants(
    user: AuthUser,
    State(state): State<AppState>,
    Query(filter): Query<RestaurantFilter>,
) -> ApiResult<Json<Vec<Restaurant>>> {
    let area_id = filter
        .area_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let q = filter.q.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let rows: Vec<RestaurantRow> = sqlx::query_as(
        "SELECT id, name, area_id, cuisine, lat, lng, address, is_open, hours_today, partner, \
                price_level, image_url \
         FROM ferry.restaurants \
         WHERE ($1::text IS NULL OR area_id = $1) \
           AND ($2::text IS NULL \
                OR strpos(lower(name), lower($2)) > 0 \
                OR EXISTS (SELECT 1 FROM unnest(cuisine) AS c(name) \
                           WHERE strpos(lower(c.name), lower($2)) > 0)) \
         ORDER BY name, id",
    )
    .bind(area_id)
    .bind(q)
    .fetch_all(&state.pool)
    .await?;

    let local_minutes = if state.config.demo_mode || rows.is_empty() {
        None
    } else {
        Some(new_york_minutes(&state.pool).await?)
    };
    let walker = viewer_walker(&state.pool, &user.user_id).await?;
    let supabase_url = state.config.supabase_url.as_deref();

    let restaurants = rows
        .into_iter()
        .map(|row| {
            let location = LatLng {
                lat: row.lat,
                lng: row.lng,
            };
            let is_open = match local_minutes {
                None => true,
                Some(now) => is_open_at(&row.hours_today, now).unwrap_or(row.is_open),
            };
            Restaurant {
                walk_minutes_from_dropoff: walker
                    .map(|(dropoff, pace)| walk_minutes(dropoff, location, pace)),
                image_url: public_media_url(row.image_url.as_deref(), supabase_url),
                id: row.id,
                name: row.name,
                area_id: row.area_id,
                cuisine: row.cuisine,
                location,
                address: row.address,
                is_open,
                hours_today: row.hours_today,
                partner: row.partner,
                price_level: row.price_level,
            }
        })
        .filter(|r| filter.open_now != Some(true) || r.is_open)
        .collect();
    Ok(Json(restaurants))
}

/// Minutes since midnight in Chapel Hill, from the database clock and its time zone data.
async fn new_york_minutes(pool: &PgPool) -> Result<u32, ApiError> {
    let minutes: i32 = sqlx::query_scalar(
        "SELECT (extract(hour FROM t) * 60 + extract(minute FROM t))::int \
         FROM (SELECT now() AT TIME ZONE 'America/New_York' AS t) AS local",
    )
    .fetch_one(pool)
    .await?;
    u32::try_from(minutes).map_err(ApiError::internal)
}

/// The viewer's drop-off and pace, when their profile has a drop-off.
async fn viewer_walker(pool: &PgPool, user_id: &str) -> Result<Option<(LatLng, Pace)>, ApiError> {
    let row: Option<(Option<f64>, Option<f64>, String)> = sqlx::query_as(
        "SELECT dropoff_lat, dropoff_lng, walking_pace FROM ferry.profiles WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        Some((Some(lat), Some(lng), pace)) => Some((
            LatLng { lat, lng },
            Pace::parse(&pace).unwrap_or(Pace::Normal),
        )),
        _ => None,
    })
}

#[derive(sqlx::FromRow)]
struct ItemRow {
    id: String,
    category_id: String,
    name: String,
    description: String,
    price_cents: i64,
    tags: Vec<String>,
    image_url: Option<String>,
}

#[derive(sqlx::FromRow)]
struct OptionRow {
    id: String,
    menu_item_id: String,
    name: String,
    min_choices: i32,
    max_choices: i32,
}

#[derive(sqlx::FromRow)]
struct ChoiceRow {
    id: String,
    option_id: String,
    name: String,
    price_delta_cents: i64,
}

/// `GET /v1/restaurants/{id}/menu`: categories, items, options and choices, each in menu order.
/// `not_found` for an unknown restaurant.
pub async fn get_menu(
    _user: AuthUser,
    State(state): State<AppState>,
    Path(restaurant_id): Path<String>,
) -> ApiResult<Json<Menu>> {
    // One snapshot for the five reads, so a reseed in between cannot mix two menus.
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let prices_are_estimates: bool =
        sqlx::query_scalar("SELECT prices_are_estimates FROM ferry.restaurants WHERE id = $1")
            .bind(&restaurant_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| ApiError::not_found(format!("No restaurant {restaurant_id}.")))?;
    let categories: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, name FROM ferry.menu_categories WHERE restaurant_id = $1 \
         ORDER BY sort_order, id",
    )
    .bind(&restaurant_id)
    .fetch_all(&mut *tx)
    .await?;
    let items: Vec<ItemRow> = sqlx::query_as(
        "SELECT id, category_id, name, description, price_cents, tags, image_url \
         FROM ferry.menu_items WHERE restaurant_id = $1 ORDER BY sort_order, id",
    )
    .bind(&restaurant_id)
    .fetch_all(&mut *tx)
    .await?;
    let options: Vec<OptionRow> = sqlx::query_as(
        "SELECT o.id, o.menu_item_id, o.name, o.min_choices, o.max_choices \
         FROM ferry.item_options o JOIN ferry.menu_items i ON i.id = o.menu_item_id \
         WHERE i.restaurant_id = $1 ORDER BY o.sort_order, o.id",
    )
    .bind(&restaurant_id)
    .fetch_all(&mut *tx)
    .await?;
    let choices: Vec<ChoiceRow> = sqlx::query_as(
        "SELECT c.id, c.option_id, c.name, c.price_delta_cents \
         FROM ferry.item_choices c \
         JOIN ferry.item_options o ON o.id = c.option_id \
         JOIN ferry.menu_items i ON i.id = o.menu_item_id \
         WHERE i.restaurant_id = $1 ORDER BY c.sort_order, c.id",
    )
    .bind(&restaurant_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    // Assemble bottom-up; each list keeps the order its query returned.
    let mut choices_by_option: HashMap<String, Vec<MenuChoice>> = HashMap::new();
    for c in choices {
        choices_by_option
            .entry(c.option_id)
            .or_default()
            .push(MenuChoice {
                id: c.id,
                name: c.name,
                price_delta_cents: c.price_delta_cents,
            });
    }
    let mut options_by_item: HashMap<String, Vec<MenuOption>> = HashMap::new();
    for o in options {
        let choices = choices_by_option.remove(&o.id).unwrap_or_default();
        options_by_item
            .entry(o.menu_item_id)
            .or_default()
            .push(MenuOption {
                id: o.id,
                name: o.name,
                min: o.min_choices,
                max: o.max_choices,
                choices,
            });
    }
    let supabase_url = state.config.supabase_url.as_deref();
    let mut items_by_category: HashMap<String, Vec<MenuItem>> = HashMap::new();
    for i in items {
        let options = options_by_item.remove(&i.id).unwrap_or_default();
        items_by_category
            .entry(i.category_id)
            .or_default()
            .push(MenuItem {
                image_url: public_media_url(i.image_url.as_deref(), supabase_url),
                id: i.id,
                name: i.name,
                description: i.description,
                price_cents: i.price_cents,
                tags: i.tags,
                options,
            });
    }
    let categories = categories
        .into_iter()
        .map(|(id, name)| MenuCategory {
            items: items_by_category.remove(&id).unwrap_or_default(),
            id,
            name,
        })
        .collect();
    Ok(Json(Menu {
        restaurant_id,
        prices_are_estimates,
        categories,
    }))
}
