//! Checks on `PUT /v1/me/profile` beyond what deserializing already enforces (field names, types,
//! required fields and the enums). The size caps are sanity limits; docs/DECISIONS.md lists them.

use serde::Serialize;

use super::types::{Profile, UpdateProfileRequest};

pub const MAX_DISPLAY_NAME_CHARS: usize = 50;
pub const MAX_LABEL_CHARS: usize = 100;
pub const MAX_NOTE_CHARS: usize = 280;
/// Per entry of diet, avoid and favorite_cuisines.
pub const MAX_TAG_CHARS: usize = 60;
/// Per list: diet, avoid, favorite_cuisines, favorite_restaurant_ids.
pub const MAX_LIST_ITEMS: usize = 50;
pub const MAX_USUAL_TIMES: usize = 20;
/// $100.
pub const MAX_DEFAULT_TIP_CENTS: i64 = 10_000;
/// A drop-off radius in whole meters (contract v1.4): `Profile.max_dropoff_distance_m`, and a
/// run's, an order's or a pickup request's `dropoff_radius_m`. The migration's CHECKs match.
pub const MIN_DROPOFF_RADIUS_M: i64 = 50;
pub const MAX_DROPOFF_RADIUS_M: i64 = 5_000;

/// One problem with one field, as listed in the error's `details`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

fn push(errors: &mut Vec<FieldError>, field: impl Into<String>, message: impl Into<String>) {
    errors.push(FieldError {
        field: field.into(),
        message: message.into(),
    });
}

/// `"HH:MM"`, 24-hour: the contract's pattern `^([01][0-9]|2[0-3]):[0-5][0-9]$`.
pub fn is_hh_mm(value: &str) -> bool {
    let b = value.as_bytes();
    b.len() == 5
        && b[2] == b':'
        && matches!(
            (b[0], b[1]),
            (b'0' | b'1', b'0'..=b'9') | (b'2', b'0'..=b'3')
        )
        && matches!((b[3], b[4]), (b'0'..=b'5', b'0'..=b'9'))
}

fn check_text(
    errors: &mut Vec<FieldError>,
    field: &str,
    value: &str,
    max_chars: usize,
    allow_empty: bool,
) {
    if !allow_empty && value.trim().is_empty() {
        push(errors, field, "must not be empty");
    } else if value.chars().count() > max_chars {
        push(
            errors,
            field,
            format!("must be at most {max_chars} characters"),
        );
    } else if value.chars().any(char::is_control) {
        push(errors, field, "must not contain control characters");
    }
}

fn check_tags(errors: &mut Vec<FieldError>, field: &str, values: &[String]) {
    if values.len() > MAX_LIST_ITEMS {
        push(
            errors,
            field,
            format!("must have at most {MAX_LIST_ITEMS} entries"),
        );
        return;
    }
    for (i, value) in values.iter().enumerate() {
        check_text(
            errors,
            &format!("{field}[{i}]"),
            value,
            MAX_TAG_CHARS,
            false,
        );
    }
}

/// The problem with a drop-off radius, if any: when given, [`MIN_DROPOFF_RADIUS_M`] to
/// [`MAX_DROPOFF_RADIUS_M`].
pub fn check_radius(errors: &mut Vec<FieldError>, field: &str, radius_m: Option<i64>) {
    if let Some(radius_m) = radius_m
        && !(MIN_DROPOFF_RADIUS_M..=MAX_DROPOFF_RADIUS_M).contains(&radius_m)
    {
        push(
            errors,
            field,
            format!("must be {MIN_DROPOFF_RADIUS_M} to {MAX_DROPOFF_RADIUS_M} meters"),
        );
    }
}

/// Every problem with the request that needs no database. Restaurant and place ids are checked
/// against the database by the handler.
pub fn check_request(request: &UpdateProfileRequest) -> Vec<FieldError> {
    let mut errors = Vec::new();
    if let Some(name) = &request.display_name {
        check_text(
            &mut errors,
            "display_name",
            name.trim(),
            MAX_DISPLAY_NAME_CHARS,
            false,
        );
    }
    check_profile(&mut errors, &request.profile);
    errors
}

fn check_profile(errors: &mut Vec<FieldError>, profile: &Profile) {
    if let Some(place) = &profile.dropoff_default {
        let field = "profile.dropoff_default";
        if let Some(id) = &place.id {
            check_text(errors, &format!("{field}.id"), id, MAX_LABEL_CHARS, false);
        }
        check_text(
            errors,
            &format!("{field}.label"),
            &place.label,
            MAX_LABEL_CHARS,
            false,
        );
        if let Some(note) = &place.note {
            check_text(errors, &format!("{field}.note"), note, MAX_NOTE_CHARS, true);
        }
        let (lat, lng) = (place.location.lat, place.location.lng);
        if !(lat.is_finite() && (-90.0..=90.0).contains(&lat)) {
            push(
                errors,
                format!("{field}.location.lat"),
                "must be between -90 and 90",
            );
        }
        if !(lng.is_finite() && (-180.0..=180.0).contains(&lng)) {
            push(
                errors,
                format!("{field}.location.lng"),
                "must be between -180 and 180",
            );
        }
    }
    check_tags(errors, "profile.diet", &profile.diet);
    check_tags(errors, "profile.avoid", &profile.avoid);
    check_tags(
        errors,
        "profile.favorite_cuisines",
        &profile.favorite_cuisines,
    );
    if profile.favorite_restaurant_ids.len() > MAX_LIST_ITEMS {
        push(
            errors,
            "profile.favorite_restaurant_ids",
            format!("must have at most {MAX_LIST_ITEMS} entries"),
        );
    }
    if profile.usual_times.len() > MAX_USUAL_TIMES {
        push(
            errors,
            "profile.usual_times",
            format!("must have at most {MAX_USUAL_TIMES} entries"),
        );
    } else {
        for (i, time) in profile.usual_times.iter().enumerate() {
            let field = format!("profile.usual_times[{i}]");
            check_text(
                errors,
                &format!("{field}.label"),
                &time.label,
                MAX_TAG_CHARS,
                false,
            );
            for (name, value) in [("start", &time.start), ("end", &time.end)] {
                if !is_hh_mm(value) {
                    push(
                        errors,
                        format!("{field}.{name}"),
                        "must be HH:MM, 24-hour (00:00 to 23:59)",
                    );
                }
            }
        }
    }
    if profile.default_tip_cents < 0 {
        push(errors, "profile.default_tip_cents", "must not be negative");
    } else if profile.default_tip_cents > MAX_DEFAULT_TIP_CENTS {
        push(
            errors,
            "profile.default_tip_cents",
            format!("must be at most {MAX_DEFAULT_TIP_CENTS}"),
        );
    }
    check_radius(
        errors,
        "profile.max_dropoff_distance_m",
        profile.max_dropoff_distance_m,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::types::{LatLng, Place, UsualTime, Weekday};

    fn request() -> UpdateProfileRequest {
        UpdateProfileRequest {
            display_name: Some("Maya".into()),
            profile: Profile {
                dropoff_default: Some(Place {
                    id: Some("place_hinton".into()),
                    label: "Hinton James lobby".into(),
                    location: LatLng {
                        lat: 35.902364,
                        lng: -79.043231,
                    },
                    note: Some("Main lobby.".into()),
                }),
                usual_times: vec![UsualTime {
                    label: "Dinner".into(),
                    days: vec![Weekday::Mon],
                    start: "18:00".into(),
                    end: "23:59".into(),
                }],
                default_tip_cents: 200,
                diet: vec!["vegetarian".into()],
                ..Profile::default()
            },
        }
    }

    fn fields(request: &UpdateProfileRequest) -> Vec<String> {
        check_request(request)
            .into_iter()
            .map(|e| e.field)
            .collect()
    }

    #[test]
    fn a_good_request_passes() {
        assert!(check_request(&request()).is_empty());
        let mut r = request();
        r.display_name = None;
        r.profile.dropoff_default = None;
        assert!(check_request(&r).is_empty());
    }

    #[test]
    fn hh_mm() {
        for good in ["00:00", "09:05", "19:59", "23:59", "20:00"] {
            assert!(is_hh_mm(good), "{good}");
        }
        for bad in [
            "24:00", "9:00", "09:60", "0900", "09:5", "ab:cd", "", "09:00 ", "29:00", "１9:00",
        ] {
            assert!(!is_hh_mm(bad), "{bad}");
        }
    }

    #[test]
    fn each_bad_field_is_named() {
        let mut r = request();
        r.display_name = Some("   ".into());
        r.profile.usual_times[0].start = "25:00".into();
        r.profile.usual_times[0].end = "7:30".into();
        r.profile.default_tip_cents = -1;
        r.profile.max_dropoff_distance_m = Some(MIN_DROPOFF_RADIUS_M - 1);
        let place = r.profile.dropoff_default.as_mut().unwrap();
        place.location.lat = 91.0;
        place.location.lng = -181.0;
        place.label = String::new();
        assert_eq!(
            fields(&r),
            [
                "display_name",
                "profile.dropoff_default.label",
                "profile.dropoff_default.location.lat",
                "profile.dropoff_default.location.lng",
                "profile.usual_times[0].start",
                "profile.usual_times[0].end",
                "profile.default_tip_cents",
                "profile.max_dropoff_distance_m",
            ]
        );
    }

    #[test]
    fn walk_distance_is_50_to_5000_meters_or_none() {
        for ok in [
            None,
            Some(MIN_DROPOFF_RADIUS_M),
            Some(500),
            Some(MAX_DROPOFF_RADIUS_M),
        ] {
            let mut r = request();
            r.profile.max_dropoff_distance_m = ok;
            assert!(check_request(&r).is_empty(), "{ok:?}");
        }
        for bad in [
            0,
            -50,
            MIN_DROPOFF_RADIUS_M - 1,
            MAX_DROPOFF_RADIUS_M + 1,
            i64::MAX,
        ] {
            let mut r = request();
            r.profile.max_dropoff_distance_m = Some(bad);
            assert_eq!(fields(&r), ["profile.max_dropoff_distance_m"], "{bad}");
        }
    }

    #[test]
    fn caps() {
        let mut r = request();
        r.display_name = Some("x".repeat(MAX_DISPLAY_NAME_CHARS + 1));
        r.profile.avoid = vec!["peanuts".into(); MAX_LIST_ITEMS + 1];
        r.profile.favorite_cuisines = vec![String::new()];
        r.profile.default_tip_cents = MAX_DEFAULT_TIP_CENTS + 1;
        assert_eq!(
            fields(&r),
            [
                "display_name",
                "profile.avoid",
                "profile.favorite_cuisines[0]",
                "profile.default_tip_cents",
            ]
        );
        let mut r = request();
        r.display_name = Some("x".repeat(MAX_DISPLAY_NAME_CHARS));
        r.profile.default_tip_cents = MAX_DEFAULT_TIP_CENTS;
        assert!(check_request(&r).is_empty());
    }

    #[test]
    fn control_characters_are_refused() {
        let mut r = request();
        r.display_name = Some("Ma\u{0}ya".into());
        assert_eq!(fields(&r), ["display_name"]);
    }
}
