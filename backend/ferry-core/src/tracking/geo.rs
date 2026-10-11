//! Coordinates, distances, pace and ETAs for routing (spec section 2). The contract types and the
//! straight-line math are the catalog's (`catalog::types`, `catalog::walk`), so every module
//! passes the same `LatLng`, `Place` and `Pace` to tracking.

pub use crate::catalog::types::{LatLng, Place};
pub use crate::catalog::walk::{DETOUR_FACTOR, Pace, haversine_m};

/// A `LatLng`, usable in constants.
pub const fn latlng(lat: f64, lng: f64) -> LatLng {
    LatLng { lat, lng }
}

/// True when both numbers are finite and within -90..=90 and -180..=180.
pub fn is_valid(point: LatLng) -> bool {
    point.lat.is_finite()
        && point.lng.is_finite()
        && (-90.0..=90.0).contains(&point.lat)
        && (-180.0..=180.0).contains(&point.lng)
}

/// The point rounded to 4 decimals, about 11 m of latitude and 9 m of longitude in Chapel Hill:
/// the route cache's key.
pub fn grid_key(point: LatLng) -> (i64, i64) {
    (
        (point.lat * 10_000.0).round() as i64,
        (point.lng * 10_000.0).round() as i64,
    )
}

/// The routing fallback's distance: straight line × [`DETOUR_FACTOR`].
pub fn fallback_distance_m(a: LatLng, b: LatLng) -> f64 {
    haversine_m(a, b) * DETOUR_FACTOR
}

/// Minutes to walk `distance_m` at `pace`, rounded up to a whole minute, so the ETA is never
/// earlier than the walk. 0 m is 0 minutes. Always finite and at least 0.
pub fn eta_minutes(distance_m: f64, pace: Pace) -> f64 {
    if !distance_m.is_finite() || distance_m <= 0.0 {
        return 0.0;
    }
    // The tolerance keeps an exact minute exact: 84 m at 1.4 m/s is 60.000000000000014 s.
    (distance_m / pace.meters_per_second() / 60.0 - 1e-9).ceil()
}

#[cfg(test)]
mod tests {
    use super::*;

    const COSMIC: LatLng = latlng(35.913211, -79.055094);
    const HINTON: LatLng = latlng(35.902364, -79.043231);

    #[test]
    fn fallback_distance_is_straight_line_times_1_3() {
        let straight = haversine_m(COSMIC, HINTON);
        assert!((fallback_distance_m(COSMIC, HINTON) - straight * 1.3).abs() < 1e-9);
        assert_eq!(fallback_distance_m(COSMIC, COSMIC), 0.0);
    }

    #[test]
    fn fallback_eta_at_each_pace() {
        let distance = fallback_distance_m(COSMIC, HINTON);
        for (pace, speed) in [
            (Pace::Relaxed, 1.2),
            (Pace::Normal, 1.4),
            (Pace::Brisk, 1.6),
        ] {
            assert_eq!(pace.meters_per_second(), speed);
            let expected = (distance / speed / 60.0).ceil();
            assert_eq!(eta_minutes(distance, pace), expected, "{pace:?}");
        }
        // About 2,090 m: 30, 25 and 22 minutes, as the catalog's walk_minutes says.
        assert_eq!(eta_minutes(distance, Pace::Relaxed), 30.0);
        assert_eq!(eta_minutes(distance, Pace::Normal), 25.0);
        assert_eq!(eta_minutes(distance, Pace::Brisk), 22.0);
        assert_eq!(
            eta_minutes(distance, Pace::Brisk),
            f64::from(crate::catalog::walk::walk_minutes(
                COSMIC,
                HINTON,
                Pace::Brisk
            ))
        );
    }

    #[test]
    fn eta_rounds_up_and_never_goes_negative() {
        assert_eq!(eta_minutes(0.0, Pace::Normal), 0.0);
        assert_eq!(eta_minutes(1.0, Pace::Normal), 1.0);
        assert_eq!(eta_minutes(84.0, Pace::Normal), 1.0);
        assert_eq!(eta_minutes(84.1, Pace::Normal), 2.0);
        assert_eq!(eta_minutes(-5.0, Pace::Normal), 0.0);
        assert_eq!(eta_minutes(f64::NAN, Pace::Normal), 0.0);
        assert_eq!(eta_minutes(f64::INFINITY, Pace::Normal), 0.0);
    }

    #[test]
    fn validity_and_grid() {
        assert!(is_valid(COSMIC));
        assert!(is_valid(latlng(90.0, -180.0)));
        assert!(!is_valid(latlng(90.1, 0.0)));
        assert!(!is_valid(latlng(0.0, 180.5)));
        assert!(!is_valid(latlng(f64::NAN, 0.0)));
        // Points 3 m apart share a key; points 30 m apart do not.
        let near = latlng(COSMIC.lat + 0.000_02, COSMIC.lng);
        let far = latlng(COSMIC.lat + 0.000_3, COSMIC.lng);
        assert_eq!(grid_key(COSMIC), grid_key(near));
        assert_ne!(grid_key(COSMIC), grid_key(far));
    }
}
