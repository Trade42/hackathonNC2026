//! contract/fixtures, embedded at compile time so the binary seeds without the repo beside it.
//! These files are the seed's only source of demo data.

use serde::Deserialize;
use serde_json::Value;

use crate::catalog::types::{Area, Menu, Place, Restaurant};

pub const USERS_JSON: &str = include_str!("../../../../contract/fixtures/users.json");
pub const CIRCLES_JSON: &str = include_str!("../../../../contract/fixtures/circles.json");
pub const AREAS_JSON: &str = include_str!("../../../../contract/fixtures/areas.json");
pub const RESTAURANTS_JSON: &str = include_str!("../../../../contract/fixtures/restaurants.json");
pub const MENUS_JSON: &str = include_str!("../../../../contract/fixtures/menus.json");
pub const PLACES_JSON: &str = include_str!("../../../../contract/fixtures/places.json");
pub const RUNS_JSON: &str = include_str!("../../../../contract/fixtures/runs.json");

/// A `User` fixture: PublicUser plus the masked phone, the profile and onboarding.
#[derive(Debug, Clone, Deserialize)]
pub struct FixtureUser {
    pub id: String,
    pub display_name: String,
    pub avatar_seed: String,
    pub bubble_color: String,
    pub phone_masked: String,
    pub profile: FixtureProfile,
    pub onboarding_complete: bool,
}

/// A `Profile` fixture. `usual_times` is stored whole as jsonb.
#[derive(Debug, Clone, Deserialize)]
pub struct FixtureProfile {
    pub dropoff_default: Option<Place>,
    pub diet: Vec<String>,
    pub avoid: Vec<String>,
    pub favorite_cuisines: Vec<String>,
    pub favorite_restaurant_ids: Vec<String>,
    pub usual_times: Value,
    pub walking_pace: String,
    pub runner_opt_in: bool,
    pub default_tip_cents: i64,
    pub notify_push: bool,
    pub notify_messages: bool,
    pub share_location_when_running: bool,
}

/// An id inside a fixture's nested object (`members[]`, `run.restaurant`, `run.runner`).
#[derive(Debug, Clone, Deserialize)]
pub struct IdOnly {
    pub id: String,
}

/// A `Circle` fixture. `stats` is derived from runs and is not stored.
#[derive(Debug, Clone, Deserialize)]
pub struct FixtureCircle {
    pub id: String,
    pub name: String,
    pub emoji: String,
    pub members: Vec<IdOnly>,
    pub last_met_at: Option<String>,
    pub last_active_at: String,
}

/// A `Run` fixture. `order_count`, `runner_location` and `eta_minutes` are derived.
#[derive(Debug, Clone, Deserialize)]
pub struct FixtureRun {
    pub id: String,
    pub restaurant: IdOnly,
    pub runner: IdOnly,
    pub circle_id: String,
    pub status: String,
    pub leaving_at: String,
    pub window_closes_at: String,
    pub created_via: String,
}

/// Every fixture file, parsed.
#[derive(Debug, Clone)]
pub struct Fixtures {
    pub users: Vec<FixtureUser>,
    pub circles: Vec<FixtureCircle>,
    pub areas: Vec<Area>,
    pub restaurants: Vec<Restaurant>,
    pub menus: Vec<Menu>,
    pub places: Vec<Place>,
    pub runs: Vec<FixtureRun>,
}

#[derive(Debug, thiserror::Error)]
#[error("contract/fixtures/{file} does not parse: {source}")]
pub struct FixtureError {
    pub file: &'static str,
    #[source]
    pub source: serde_json::Error,
}

impl Fixtures {
    /// Parses the embedded fixture files.
    pub fn load() -> Result<Self, FixtureError> {
        fn parse<T: serde::de::DeserializeOwned>(
            file: &'static str,
            json: &str,
        ) -> Result<T, FixtureError> {
            serde_json::from_str(json).map_err(|source| FixtureError { file, source })
        }
        Ok(Self {
            users: parse("users.json", USERS_JSON)?,
            circles: parse("circles.json", CIRCLES_JSON)?,
            areas: parse("areas.json", AREAS_JSON)?,
            restaurants: parse("restaurants.json", RESTAURANTS_JSON)?,
            menus: parse("menus.json", MENUS_JSON)?,
            places: parse("places.json", PLACES_JSON)?,
            runs: parse("runs.json", RUNS_JSON)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_fixture_file_parses() {
        let f = Fixtures::load().unwrap();
        assert!(!f.users.is_empty());
        assert!(!f.circles.is_empty());
        assert!(!f.areas.is_empty());
        assert!(!f.restaurants.is_empty());
        assert!(!f.menus.is_empty());
        assert!(!f.places.is_empty());
        assert!(f.runs.iter().any(|r| r.id == "run_demo"));
    }

    /// The catalog types keep every field of the fixtures, so nothing is dropped on the way into
    /// the database.
    #[test]
    fn catalog_types_round_trip_the_fixtures() {
        fn round_trip<T: serde::de::DeserializeOwned + serde::Serialize>(json: &str) {
            let original: Value = serde_json::from_str(json).unwrap();
            let typed: Vec<T> = serde_json::from_str(json).unwrap();
            assert_eq!(serde_json::to_value(&typed).unwrap(), original);
        }
        round_trip::<Area>(AREAS_JSON);
        round_trip::<Restaurant>(RESTAURANTS_JSON);
        round_trip::<Menu>(MENUS_JSON);
        round_trip::<Place>(PLACES_JSON);
    }

    /// The user, circle and run fixtures hold only the fields the structs read plus the derived
    /// ones the seed leaves out on purpose.
    #[test]
    fn people_and_run_fixtures_have_no_unread_fields() {
        fn keys(json: &str) -> Vec<String> {
            let v: Value = serde_json::from_str(json).unwrap();
            let mut keys: Vec<String> = v
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|o| o.as_object().unwrap().keys().cloned())
                .collect();
            keys.sort();
            keys.dedup();
            keys
        }
        assert_eq!(
            keys(USERS_JSON),
            [
                "avatar_seed",
                "bubble_color",
                "display_name",
                "id",
                "onboarding_complete",
                "phone_masked",
                "profile",
                // Contract v1.3: null for every persona, and not stored (no column yet).
                "telegram_username"
            ]
        );
        let profiles: Value = serde_json::from_str(USERS_JSON).unwrap();
        for user in profiles.as_array().unwrap() {
            let mut fields: Vec<&str> = user["profile"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            fields.sort();
            assert_eq!(
                fields,
                [
                    "avoid",
                    "default_tip_cents",
                    "diet",
                    "dropoff_default",
                    "favorite_cuisines",
                    "favorite_restaurant_ids",
                    "notify_messages",
                    "notify_push",
                    "runner_opt_in",
                    "share_location_when_running",
                    "usual_times",
                    "walking_pace"
                ]
            );
        }
        // stats is derived from runs.
        assert_eq!(
            keys(CIRCLES_JSON),
            [
                "emoji",
                "id",
                "last_active_at",
                "last_met_at",
                "members",
                "name",
                "stats"
            ]
        );
        // order_count, runner_location and eta_minutes are derived; restaurant and runner are
        // stored by id.
        assert_eq!(
            keys(RUNS_JSON),
            [
                "circle_id",
                "created_via",
                "eta_minutes",
                "id",
                "leaving_at",
                "order_count",
                "restaurant",
                "runner",
                "runner_location",
                "status",
                "window_closes_at"
            ]
        );
    }
}
