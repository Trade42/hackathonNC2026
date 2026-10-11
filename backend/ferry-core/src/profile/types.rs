//! The contract's User, Profile and their parts, with exactly its field names.

use serde::{Deserialize, Serialize};

/// `LatLng`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LatLng {
    pub lat: f64,
    pub lng: f64,
}

/// `Place`: `id` and `note` are optional, so they are left out rather than sent as null.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Place {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub label: String,
    pub location: LatLng,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// `Weekday`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

/// `UsualTime`: `start` and `end` are `"HH:MM"`, 24-hour.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsualTime {
    pub label: String,
    pub days: Vec<Weekday>,
    pub start: String,
    pub end: String,
}

/// `WalkingPace`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WalkingPace {
    Relaxed,
    #[default]
    Normal,
    Brisk,
}

impl WalkingPace {
    /// The `ferry.profiles.walking_pace` value.
    pub fn as_str(self) -> &'static str {
        match self {
            WalkingPace::Relaxed => "relaxed",
            WalkingPace::Normal => "normal",
            WalkingPace::Brisk => "brisk",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "relaxed" => Some(WalkingPace::Relaxed),
            "normal" => Some(WalkingPace::Normal),
            "brisk" => Some(WalkingPace::Brisk),
            _ => None,
        }
    }
}

/// `Profile`: every field but `max_dropoff_distance_m` is required in the contract, and a request
/// missing one is refused. `dropoff_default` is required but may be null.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    #[serde(deserialize_with = "required_nullable")]
    pub dropoff_default: Option<Place>,
    pub diet: Vec<String>,
    pub avoid: Vec<String>,
    pub favorite_cuisines: Vec<String>,
    pub favorite_restaurant_ids: Vec<String>,
    pub usual_times: Vec<UsualTime>,
    pub walking_pace: WalkingPace,
    pub runner_opt_in: bool,
    pub default_tip_cents: i64,
    pub notify_push: bool,
    pub notify_messages: bool,
    pub share_location_when_running: bool,
    /// Contract v1.4: how far the user would walk to hand food over when they run, 50 to 5000 m;
    /// the default for a new run's `dropoff_radius_m`. Optional in a request, and replaced like
    /// every other answer, so a save without it clears it. Always sent, as null when unset.
    #[serde(default)]
    pub max_dropoff_distance_m: Option<i64>,
}

impl Default for Profile {
    /// The answers of someone who has not done the questionnaire: `ferry.profiles`' defaults.
    fn default() -> Self {
        Self {
            dropoff_default: None,
            diet: Vec::new(),
            avoid: Vec::new(),
            favorite_cuisines: Vec::new(),
            favorite_restaurant_ids: Vec::new(),
            usual_times: Vec::new(),
            walking_pace: WalkingPace::Normal,
            runner_opt_in: false,
            default_tip_cents: 0,
            notify_push: true,
            notify_messages: true,
            share_location_when_running: false,
            max_dropoff_distance_m: None,
        }
    }
}

/// `User`: PublicUser plus phone_masked, telegram_username, profile and onboarding_complete,
/// flattened.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct User {
    pub id: String,
    pub display_name: String,
    pub avatar_seed: String,
    pub bubble_color: String,
    /// Null for a user with no phone, such as every Telegram sign-up (contract v1.3).
    pub phone_masked: Option<String>,
    /// The Telegram username without the `@`; null without one, and for users who never signed in
    /// with Telegram (contract v1.3).
    pub telegram_username: Option<String>,
    pub profile: Profile,
    pub onboarding_complete: bool,
}

/// `UpdateProfileRequest`.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProfileRequest {
    #[serde(default)]
    pub display_name: Option<String>,
    pub profile: Profile,
}

/// `MeStats`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MeStats {
    pub orders: i64,
    pub runs: i64,
    pub earned_cents: i64,
    pub co2_saved_g: f64,
}

/// A field that must be present but may be null. Plain `Option` would also accept a missing field.
fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn profile_json() -> serde_json::Value {
        json!({
            "dropoff_default": null,
            "diet": [], "avoid": [], "favorite_cuisines": [], "favorite_restaurant_ids": [],
            "usual_times": [{"label": "Dinner", "days": ["mon", "fri"], "start": "18:00", "end": "19:30"}],
            "walking_pace": "brisk", "runner_opt_in": true, "default_tip_cents": 200,
            "notify_push": true, "notify_messages": false, "share_location_when_running": false,
            "max_dropoff_distance_m": 400
        })
    }

    #[test]
    fn profile_round_trips_with_the_contract_names() {
        let profile: Profile = serde_json::from_value(profile_json()).unwrap();
        assert_eq!(profile.walking_pace, WalkingPace::Brisk);
        assert_eq!(
            profile.usual_times[0].days,
            vec![Weekday::Mon, Weekday::Fri]
        );
        assert_eq!(serde_json::to_value(&profile).unwrap(), profile_json());
    }

    #[test]
    fn max_dropoff_distance_is_optional_and_sent_as_null() {
        let mut value = profile_json();
        value
            .as_object_mut()
            .unwrap()
            .remove("max_dropoff_distance_m");
        let profile: Profile = serde_json::from_value(value).unwrap();
        assert_eq!(profile.max_dropoff_distance_m, None);
        let sent = serde_json::to_value(&profile).unwrap();
        assert_eq!(sent["max_dropoff_distance_m"], serde_json::Value::Null);
        let mut value = profile_json();
        value["max_dropoff_distance_m"] = serde_json::Value::Null;
        let profile: Profile = serde_json::from_value(value).unwrap();
        assert_eq!(profile.max_dropoff_distance_m, None);
    }

    #[test]
    fn dropoff_default_must_be_present_even_when_null() {
        let mut value = profile_json();
        value.as_object_mut().unwrap().remove("dropoff_default");
        let err = serde_json::from_value::<Profile>(value).unwrap_err();
        assert!(err.to_string().contains("dropoff_default"), "{err}");
    }

    #[test]
    fn place_leaves_out_missing_id_and_note() {
        let place = Place {
            id: None,
            label: "Pit".into(),
            location: LatLng { lat: 1.0, lng: 2.0 },
            note: None,
        };
        assert_eq!(
            serde_json::to_value(&place).unwrap(),
            json!({"label": "Pit", "location": {"lat": 1.0, "lng": 2.0}})
        );
    }

    #[test]
    fn unknown_enum_values_are_refused() {
        let mut value = profile_json();
        value["walking_pace"] = json!("sprint");
        assert!(serde_json::from_value::<Profile>(value).is_err());
        let mut value = profile_json();
        value["usual_times"][0]["days"] = json!(["monday"]);
        assert!(serde_json::from_value::<Profile>(value).is_err());
    }

    #[test]
    fn pace_strings_match_the_schema_check() {
        for pace in [
            WalkingPace::Relaxed,
            WalkingPace::Normal,
            WalkingPace::Brisk,
        ] {
            assert_eq!(WalkingPace::parse(pace.as_str()), Some(pace));
            assert_eq!(serde_json::to_value(pace).unwrap(), json!(pace.as_str()));
        }
    }
}
