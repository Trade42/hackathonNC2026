//! Walking time without a router: straight-line distance × 1.3 at the walker's pace (spec
//! section 2, the routing fallback). The catalog uses it for `walk_minutes_from_dropoff`.

use super::types::LatLng;

/// Mean Earth radius in meters (IUGG).
pub const EARTH_RADIUS_M: f64 = 6_371_008.8;

/// Streets are longer than the straight line between two points.
pub const DETOUR_FACTOR: f64 = 1.3;

/// `WalkingPace` from a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pace {
    Relaxed,
    Normal,
    Brisk,
}

impl Pace {
    /// The contract's string; anything else is `None`.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "relaxed" => Some(Pace::Relaxed),
            "normal" => Some(Pace::Normal),
            "brisk" => Some(Pace::Brisk),
            _ => None,
        }
    }

    /// Walking speed in meters per second.
    pub fn meters_per_second(self) -> f64 {
        match self {
            Pace::Relaxed => 1.2,
            Pace::Normal => 1.4,
            Pace::Brisk => 1.6,
        }
    }
}

/// Great-circle distance in meters (haversine).
pub fn haversine_m(a: LatLng, b: LatLng) -> f64 {
    let (lat1, lat2) = (a.lat.to_radians(), b.lat.to_radians());
    let dlat = lat2 - lat1;
    let dlng = (b.lng - a.lng).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlng / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.sqrt().min(1.0).asin()
}

/// Whole minutes to walk from `from` to `to` at `pace`, rounded up.
pub fn walk_minutes(from: LatLng, to: LatLng, pace: Pace) -> u32 {
    let meters = haversine_m(from, to) * DETOUR_FACTOR;
    let minutes = meters / pace.meters_per_second() / 60.0;
    // A walk across town is a few dozen minutes; the cast cannot overflow for points on Earth.
    minutes.ceil() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    const HINTON: LatLng = LatLng {
        lat: 35.902364,
        lng: -79.043231,
    };
    const COSMIC: LatLng = LatLng {
        lat: 35.913211,
        lng: -79.055094,
    };

    #[test]
    fn haversine_matches_a_known_distance() {
        // One degree of latitude is about 111.2 km.
        let d = haversine_m(
            LatLng {
                lat: 35.0,
                lng: -79.0,
            },
            LatLng {
                lat: 36.0,
                lng: -79.0,
            },
        );
        assert!((d - 111_195.0).abs() < 10.0, "{d}");
        assert_eq!(haversine_m(HINTON, HINTON), 0.0);
        assert!((haversine_m(HINTON, COSMIC) - haversine_m(COSMIC, HINTON)).abs() < 1e-9);
    }

    #[test]
    fn hinton_to_cosmic_at_each_pace() {
        // About 1.61 km in a straight line, 2.09 km on foot.
        let d = haversine_m(HINTON, COSMIC);
        assert!((1_600.0..1_625.0).contains(&d), "{d}");
        assert_eq!(walk_minutes(HINTON, COSMIC, Pace::Brisk), 22);
        assert_eq!(walk_minutes(HINTON, COSMIC, Pace::Normal), 25);
        assert_eq!(walk_minutes(HINTON, COSMIC, Pace::Relaxed), 30);
    }

    #[test]
    fn rounds_up_and_zero_stays_zero() {
        assert_eq!(walk_minutes(HINTON, HINTON, Pace::Normal), 0);
        // About 11 m away: a few seconds, so one minute.
        let next_door = LatLng {
            lat: HINTON.lat + 0.0001,
            lng: HINTON.lng,
        };
        assert_eq!(walk_minutes(HINTON, next_door, Pace::Brisk), 1);
    }

    #[test]
    fn paces() {
        assert_eq!(Pace::parse("relaxed"), Some(Pace::Relaxed));
        assert_eq!(Pace::parse("normal"), Some(Pace::Normal));
        assert_eq!(Pace::parse("brisk"), Some(Pace::Brisk));
        assert_eq!(Pace::parse("Brisk"), None);
        assert_eq!(Pace::Relaxed.meters_per_second(), 1.2);
        assert_eq!(Pace::Normal.meters_per_second(), 1.4);
        assert_eq!(Pace::Brisk.meters_per_second(), 1.6);
    }
}
