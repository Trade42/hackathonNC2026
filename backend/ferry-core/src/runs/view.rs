//! The contract's `Run`, `RunDetail` and `RunPreview`, read in one query per call and shown per
//! viewer.
//!
//! A [`RunRecord`] holds what every viewer shares. [`RunRecord::as_seen_by`] adds what depends on
//! the viewer:
//! - `runner_location`: the latest ping, only for the run's runner and customers with an active
//!   order (funded, purchased or picked_up) while the run is active (contract v1.2);
//! - `eta_minutes`: from a [`super::live::LivePlan`] when there is a recent ping, else null;
//! - `restaurant.walk_minutes_from_dropoff`: from the viewer's profile drop-off and pace, as
//!   `GET /v1/restaurants` gives it.
//!
//! The going-somewhere plan (contract v1.4: `returning_at`, `return_place`, `dropoff_radius_m`)
//! is the same for every viewer.

use std::collections::HashMap;
use std::time::Duration;

use serde::Serialize;
use sqlx::{PgExecutor, PgPool};

use super::machine::RunStatus;
use crate::catalog::hours::is_open_at;
use crate::catalog::media::public_media_url;
use crate::catalog::types::{LatLng, Place, Restaurant};
use crate::catalog::walk::{Pace, walk_minutes};
use crate::circles::PublicUser;
use crate::config::Config;
use crate::error::ApiResult;
use crate::orders::Order;
use crate::tracking::{LatestPing, RunnerLocation};

/// Lists return at most this many runs, newest first.
pub const LIST_LIMIT: i64 = 100;

/// A timestamptz SQL expression as ISO 8601 UTC with milliseconds.
macro_rules! iso {
    ($e:literal) => {
        concat!(
            "to_char((",
            $e,
            ") AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"')"
        )
    };
}
pub(crate) use iso;

/// One run with its restaurant, its runner, its counts and its latest ping, plus the local time
/// in Chapel Hill for the restaurant's opening hours. Callers add the `WHERE`.
macro_rules! run_select {
    () => {
        concat!(
            "SELECT r.id, r.circle_id, r.status, r.created_via, r.runner_id, r.is_demo_history, ",
            iso!("r.leaving_at"),
            " AS leaving_at, ",
            iso!("r.window_closes_at"),
            " AS window_closes_at, ",
            iso!("r.returning_at"),
            " AS returning_at, r.return_place_id, r.return_label, r.return_lat, r.return_lng, \
             r.return_note, r.dropoff_radius_m, \
             rs.id AS restaurant_id, rs.name AS restaurant_name, rs.area_id, rs.cuisine, \
             rs.lat AS restaurant_lat, rs.lng AS restaurant_lng, rs.address, \
             rs.is_open AS restaurant_is_open, rs.hours_today, rs.partner, rs.price_level, \
             rs.image_url, \
             u.display_name AS runner_display_name, u.avatar_seed AS runner_avatar_seed, \
             u.bubble_color AS runner_bubble_color, \
             coalesce(pr.walking_pace, 'normal') AS runner_pace, \
             (SELECT count(*) FROM ferry.orders o \
              WHERE o.run_id = r.id AND o.status NOT IN ('cancelled', 'refunded')) AS order_count, \
             ARRAY(SELECT DISTINCT o.customer_id FROM ferry.orders o \
                   WHERE o.run_id = r.id AND o.status IN ('funded', 'purchased', 'picked_up') \
                   ORDER BY o.customer_id) AS active_customers, \
             lp.lat AS ping_lat, lp.lng AS ping_lng, ",
            iso!("lp.ts"),
            " AS ping_at, extract(epoch FROM now() - lp.ts)::float8 AS ping_age_s, \
             (extract(hour FROM now() AT TIME ZONE 'America/New_York') * 60 \
              + extract(minute FROM now() AT TIME ZONE 'America/New_York'))::int AS local_minutes \
             FROM ferry.runs r \
             JOIN ferry.restaurants rs ON rs.id = r.restaurant_id \
             JOIN ferry.users u ON u.id = r.runner_id \
             LEFT JOIN ferry.profiles pr ON pr.user_id = r.runner_id \
             LEFT JOIN LATERAL ( \
                 SELECT p.lat, p.lng, p.ts FROM ferry.location_pings p \
                 WHERE p.run_id = r.id ORDER BY p.id DESC LIMIT 1 \
             ) lp ON true "
        )
    };
}

const BY_ID: &str = concat!(run_select!(), "WHERE r.id = $1");

/// The runs in the viewer's circles, real activity only, optionally with one status.
const IN_CIRCLES: &str = concat!(
    run_select!(),
    "WHERE r.circle_id IN (SELECT m.circle_id FROM ferry.circle_members m WHERE m.user_id = $1) \
       AND NOT r.is_demo_history \
       AND ($2::text IS NULL OR r.status = $2) \
     ORDER BY r.created_at DESC, r.id DESC LIMIT $3"
);

/// The viewer's own active runs as runner, in any circle, real activity only, optionally with one
/// status.
const MINE: &str = concat!(
    run_select!(),
    "WHERE r.runner_id = $1 \
       AND r.status IN ('open', 'closed', 'purchased', 'delivering') \
       AND NOT r.is_demo_history \
       AND ($2::text IS NULL OR r.status = $2) \
     ORDER BY r.created_at DESC, r.id DESC LIMIT $3"
);

/// `Run` in contract/openapi.yaml.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Run {
    pub id: String,
    pub restaurant: Restaurant,
    pub runner: PublicUser,
    pub circle_id: String,
    pub status: RunStatus,
    pub leaving_at: String,
    pub window_closes_at: String,
    pub order_count: i64,
    /// `RunCreatedVia`: `message` or `app`.
    pub created_via: String,
    pub runner_location: Option<RunnerLocation>,
    pub eta_minutes: Option<f64>,
    /// Contract v1.4: when the runner expects to be back, or null.
    pub returning_at: Option<String>,
    /// Contract v1.4: where the runner heads back to, or null.
    pub return_place: Option<Place>,
    /// Contract v1.4: how far from `return_place` the runner will walk to hand food over, or null.
    pub dropoff_radius_m: Option<i32>,
}

/// `RunDetail` in contract/openapi.yaml: `Run` plus the orders the viewer may see, flattened.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunDetail {
    #[serde(flatten)]
    pub run: Run,
    pub orders: Vec<Order>,
}

/// `RunPreview` in contract/openapi.yaml: only these four fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunPreview {
    pub restaurant_name: String,
    pub runner_display_name: String,
    pub window_closes_at: String,
    pub status: RunStatus,
}

#[derive(sqlx::FromRow)]
struct RunRow {
    id: String,
    circle_id: String,
    status: String,
    created_via: String,
    runner_id: String,
    is_demo_history: bool,
    leaving_at: String,
    window_closes_at: String,
    returning_at: Option<String>,
    return_place_id: Option<String>,
    return_label: Option<String>,
    return_lat: Option<f64>,
    return_lng: Option<f64>,
    return_note: Option<String>,
    dropoff_radius_m: Option<i32>,
    restaurant_id: String,
    restaurant_name: String,
    area_id: String,
    cuisine: Vec<String>,
    restaurant_lat: f64,
    restaurant_lng: f64,
    address: String,
    restaurant_is_open: bool,
    hours_today: String,
    partner: bool,
    price_level: i16,
    image_url: Option<String>,
    runner_display_name: String,
    runner_avatar_seed: String,
    runner_bubble_color: String,
    runner_pace: String,
    order_count: i64,
    active_customers: Vec<String>,
    ping_lat: Option<f64>,
    ping_lng: Option<f64>,
    ping_at: Option<String>,
    ping_age_s: Option<f64>,
    local_minutes: i32,
}

/// A viewer's profile drop-off and walking pace, for `walk_minutes_from_dropoff`.
pub type Walker = (LatLng, Pace);

/// What every viewer of a run shares; see the module docs for what [`Self::as_seen_by`] adds.
#[derive(Debug, Clone, PartialEq)]
pub struct RunRecord {
    pub id: String,
    pub circle_id: String,
    pub status: RunStatus,
    pub created_via: String,
    pub runner_id: String,
    pub is_demo_history: bool,
    pub leaving_at: String,
    pub window_closes_at: String,
    pub returning_at: Option<String>,
    pub return_place: Option<Place>,
    pub dropoff_radius_m: Option<i32>,
    /// `walk_minutes_from_dropoff` is null here; `as_seen_by` sets it per viewer.
    pub restaurant: Restaurant,
    pub runner: PublicUser,
    pub runner_pace: Pace,
    /// Live orders: not cancelled or refunded.
    pub order_count: i64,
    /// Customers with an active order (funded, purchased or picked_up), sorted.
    pub active_customers: Vec<String>,
    pub ping: Option<LatestPing>,
}

impl RunRecord {
    fn from_row(row: RunRow, config: &Config) -> ApiResult<Self> {
        let location = LatLng {
            lat: row.restaurant_lat,
            lng: row.restaurant_lng,
        };
        // As GET /v1/restaurants: DEMO_MODE opens everything; else the hours at Chapel Hill's
        // local time, and the stored flag only when the hours cannot be read.
        let is_open = config.demo_mode
            || u32::try_from(row.local_minutes)
                .ok()
                .and_then(|now| is_open_at(&row.hours_today, now))
                .unwrap_or(row.restaurant_is_open);
        let ping = match (row.ping_lat, row.ping_lng, row.ping_at, row.ping_age_s) {
            (Some(lat), Some(lng), Some(at), Some(age)) => Some(LatestPing {
                location: LatLng { lat, lng },
                at,
                age: if age.is_finite() && age > 0.0 {
                    Duration::from_secs_f64(age)
                } else {
                    Duration::ZERO
                },
            }),
            _ => None,
        };
        // The migration keeps label, lat and lng all set or all null.
        let return_place = match (row.return_label, row.return_lat, row.return_lng) {
            (Some(label), Some(lat), Some(lng)) => Some(Place {
                id: row.return_place_id,
                label,
                location: LatLng { lat, lng },
                note: row.return_note,
            }),
            _ => None,
        };
        Ok(Self {
            status: RunStatus::from_db(&row.status)?,
            restaurant: Restaurant {
                image_url: public_media_url(
                    row.image_url.as_deref(),
                    config.supabase_url.as_deref(),
                ),
                id: row.restaurant_id,
                name: row.restaurant_name,
                area_id: row.area_id,
                cuisine: row.cuisine,
                location,
                address: row.address,
                is_open,
                hours_today: row.hours_today,
                partner: row.partner,
                price_level: row.price_level,
                walk_minutes_from_dropoff: None,
            },
            runner: PublicUser {
                id: row.runner_id.clone(),
                display_name: row.runner_display_name,
                avatar_seed: row.runner_avatar_seed,
                bubble_color: row.runner_bubble_color,
            },
            runner_pace: Pace::parse(&row.runner_pace).unwrap_or(Pace::Normal),
            id: row.id,
            circle_id: row.circle_id,
            created_via: row.created_via,
            runner_id: row.runner_id,
            is_demo_history: row.is_demo_history,
            leaving_at: row.leaving_at,
            window_closes_at: row.window_closes_at,
            returning_at: row.returning_at,
            return_place,
            dropoff_radius_m: row.dropoff_radius_m,
            order_count: row.order_count,
            active_customers: row.active_customers,
            ping,
        })
    }

    /// True when `viewer` may see the runner's location: the run is active and the viewer is its
    /// runner or has an active order on it. Nobody may after delivery or cancellation.
    pub fn may_see_location(&self, viewer: &str) -> bool {
        self.status.is_active()
            && (viewer == self.runner_id || self.active_customers.iter().any(|c| c == viewer))
    }

    /// The run as `viewer` sees it. `eta_minutes` is the caller's (null in lists); it is dropped
    /// for a viewer who may not see the runner's location.
    pub fn as_seen_by(
        &self,
        viewer: &str,
        walker: Option<Walker>,
        eta_minutes: Option<f64>,
    ) -> Run {
        let visible = self.may_see_location(viewer);
        let mut restaurant = self.restaurant.clone();
        restaurant.walk_minutes_from_dropoff =
            walker.map(|(dropoff, pace)| walk_minutes(dropoff, restaurant.location, pace));
        Run {
            id: self.id.clone(),
            restaurant,
            runner: self.runner.clone(),
            circle_id: self.circle_id.clone(),
            status: self.status,
            leaving_at: self.leaving_at.clone(),
            window_closes_at: self.window_closes_at.clone(),
            order_count: self.order_count,
            created_via: self.created_via.clone(),
            runner_location: self
                .ping
                .as_ref()
                .filter(|_| visible)
                .map(LatestPing::runner_location),
            eta_minutes: eta_minutes.filter(|eta| visible && eta.is_finite()),
            returning_at: self.returning_at.clone(),
            return_place: self.return_place.clone(),
            dropoff_radius_m: self.dropoff_radius_m,
        }
    }

    /// The four public fields.
    pub fn preview(&self) -> RunPreview {
        RunPreview {
            restaurant_name: self.restaurant.name.clone(),
            runner_display_name: self.runner.display_name.clone(),
            window_closes_at: self.window_closes_at.clone(),
            status: self.status,
        }
    }
}

/// The run with `run_id`, or `None`.
pub async fn load_one<'e>(
    executor: impl PgExecutor<'e>,
    config: &Config,
    run_id: &str,
) -> ApiResult<Option<RunRecord>> {
    let row: Option<RunRow> = sqlx::query_as(BY_ID)
        .bind(run_id)
        .fetch_optional(executor)
        .await?;
    row.map(|row| RunRecord::from_row(row, config)).transpose()
}

/// `GET /v1/runs`: the runs in `viewer`'s circles, or with `mine` the viewer's own active runs in
/// any circle, both without demo history; `status` narrows either. Newest first, at most
/// [`LIST_LIMIT`].
pub async fn load_list(
    pool: &PgPool,
    config: &Config,
    viewer: &str,
    status: Option<RunStatus>,
    mine: bool,
) -> ApiResult<Vec<RunRecord>> {
    let rows: Vec<RunRow> = sqlx::query_as(if mine { MINE } else { IN_CIRCLES })
        .bind(viewer)
        .bind(status.map(RunStatus::as_str))
        .bind(LIST_LIMIT)
        .fetch_all(pool)
        .await?;
    rows.into_iter()
        .map(|row| RunRecord::from_row(row, config))
        .collect()
}

/// The profile drop-off and pace of each of `user_ids` that has a drop-off, in one query.
pub async fn walkers(pool: &PgPool, user_ids: &[String]) -> ApiResult<HashMap<String, Walker>> {
    let rows: Vec<(String, Option<f64>, Option<f64>, String)> = sqlx::query_as(
        "SELECT user_id, dropoff_lat, dropoff_lng, walking_pace FROM ferry.profiles \
         WHERE user_id = ANY($1)",
    )
    .bind(user_ids)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(user, lat, lng, pace)| {
            Some((
                user,
                (
                    LatLng {
                        lat: lat?,
                        lng: lng?,
                    },
                    Pace::parse(&pace).unwrap_or(Pace::Normal),
                ),
            ))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(status: RunStatus) -> RunRecord {
        RunRecord {
            id: "run_x".into(),
            circle_id: "cir_x".into(),
            status,
            created_via: "app".into(),
            runner_id: "usr_maya".into(),
            is_demo_history: false,
            leaving_at: "2026-10-10T22:00:00.000Z".into(),
            window_closes_at: "2026-10-10T22:10:00.000Z".into(),
            returning_at: Some("2026-10-10T22:40:00.000Z".into()),
            return_place: Some(Place {
                id: Some("place_hinton".into()),
                label: "Hinton James lobby".into(),
                location: LatLng {
                    lat: 35.902364,
                    lng: -79.043231,
                },
                note: None,
            }),
            dropoff_radius_m: Some(400),
            restaurant: Restaurant {
                id: "rst_cosmic".into(),
                name: "Cosmic Cantina".into(),
                area_id: "area_franklin".into(),
                cuisine: vec!["mexican".into()],
                location: LatLng {
                    lat: 35.913211,
                    lng: -79.055094,
                },
                address: "118 E Franklin St".into(),
                is_open: true,
                hours_today: "11:00–04:00".into(),
                partner: false,
                price_level: 1,
                walk_minutes_from_dropoff: None,
                image_url: None,
            },
            runner: PublicUser {
                id: "usr_maya".into(),
                display_name: "Maya".into(),
                avatar_seed: "maya".into(),
                bubble_color: "#F97316".into(),
            },
            runner_pace: Pace::Brisk,
            order_count: 1,
            active_customers: vec!["usr_judge".into()],
            ping: Some(LatestPing {
                location: LatLng {
                    lat: 35.91,
                    lng: -79.05,
                },
                at: "2026-10-10T22:20:00.000Z".into(),
                age: Duration::from_secs(20),
            }),
        }
    }

    #[test]
    fn location_and_eta_only_for_the_runner_and_active_customers() {
        let run = record(RunStatus::Delivering);
        for viewer in ["usr_maya", "usr_judge"] {
            let seen = run.as_seen_by(viewer, None, Some(4.0));
            assert!(seen.runner_location.is_some(), "{viewer}");
            assert_eq!(seen.eta_minutes, Some(4.0));
        }
        let other = run.as_seen_by("usr_dana", None, Some(4.0));
        assert_eq!(other.runner_location, None);
        assert_eq!(other.eta_minutes, None);
    }

    #[test]
    fn nobody_sees_the_runner_after_the_run_ends() {
        for status in [RunStatus::Delivered, RunStatus::Cancelled] {
            let run = record(status);
            for viewer in ["usr_maya", "usr_judge", "usr_dana"] {
                let seen = run.as_seen_by(viewer, None, Some(4.0));
                assert_eq!(seen.runner_location, None);
                assert_eq!(seen.eta_minutes, None);
            }
        }
    }

    #[test]
    fn walk_minutes_follow_the_viewers_dropoff() {
        let run = record(RunStatus::Open);
        let hinton = LatLng {
            lat: 35.902364,
            lng: -79.043231,
        };
        let seen = run.as_seen_by("usr_dana", Some((hinton, Pace::Brisk)), None);
        assert_eq!(seen.restaurant.walk_minutes_from_dropoff, Some(22));
        let seen = run.as_seen_by("usr_dana", None, None);
        assert_eq!(seen.restaurant.walk_minutes_from_dropoff, None);
    }

    #[test]
    fn every_viewer_sees_the_same_plan() {
        let run = record(RunStatus::Delivering);
        for viewer in ["usr_maya", "usr_judge", "usr_dana"] {
            let value = serde_json::to_value(run.as_seen_by(viewer, None, None)).unwrap();
            assert_eq!(
                value["returning_at"], "2026-10-10T22:40:00.000Z",
                "{viewer}"
            );
            assert_eq!(
                value["return_place"],
                serde_json::json!({
                    "id": "place_hinton",
                    "label": "Hinton James lobby",
                    "location": {"lat": 35.902364, "lng": -79.043231}
                })
            );
            assert_eq!(value["dropoff_radius_m"], 400);
        }
        let mut bare = run.clone();
        bare.returning_at = None;
        bare.return_place = None;
        bare.dropoff_radius_m = None;
        let value = serde_json::to_value(bare.as_seen_by("usr_dana", None, None)).unwrap();
        for field in ["returning_at", "return_place", "dropoff_radius_m"] {
            assert_eq!(value.get(field), Some(&serde_json::Value::Null), "{field}");
        }
    }

    #[test]
    fn preview_has_four_fields() {
        let value = serde_json::to_value(record(RunStatus::Open).preview()).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "restaurant_name": "Cosmic Cantina",
                "runner_display_name": "Maya",
                "window_closes_at": "2026-10-10T22:10:00.000Z",
                "status": "open"
            })
        );
    }
}
