//! Carbon per delivered order (spec section 5): the car trip a walked delivery replaced.
//! `co2_saved_g` is the driving miles from the restaurant to the drop-off × 400 g, the EPA's
//! figure for a typical passenger vehicle. Driving meters are OSRM's car route when it answers,
//! otherwise the straight line × 1.3 (`tracking::car_distance_m`).

use crate::state::AppState;
use crate::tracking::{self, LatLng};

/// `Impact.method`, word for word.
pub const METHOD: &str = "EPA estimate: about 400 g CO2 per car mile avoided";

/// Grams of CO2 a typical passenger vehicle emits per mile (EPA).
pub const CO2_G_PER_CAR_MILE: f64 = 400.0;

/// Meters in a statute mile.
pub const METERS_PER_MILE: f64 = 1609.344;

/// `(car_distance_m, co2_saved_g)` for a delivered order, both stored on the order
/// (`ferry.orders.car_distance_m`, `ferry.orders.co2_saved_g`). Never fails: when OSRM is down
/// the distance is the straight-line fallback.
pub async fn carbon_for_delivery(
    state: &AppState,
    restaurant: LatLng,
    dropoff: LatLng,
) -> (f64, f64) {
    carbon_for_distance(tracking::car_distance_m(state, restaurant, dropoff).await)
}

/// `(car_distance_m, co2_saved_g)` from the routing fallback alone (straight line × 1.3), with no
/// network call. The demo history uses it.
pub fn fallback_carbon(restaurant: LatLng, dropoff: LatLng) -> (f64, f64) {
    carbon_for_distance(tracking::fallback_distance_m(restaurant, dropoff))
}

/// `(car_distance_m, co2_saved_g)` for a driving distance. A distance that is negative or not
/// finite counts as 0 m, so the stored values are always finite and at least 0 (JSON has no NaN).
pub fn carbon_for_distance(car_distance_m: f64) -> (f64, f64) {
    let meters = if car_distance_m.is_finite() && car_distance_m > 0.0 {
        car_distance_m
    } else {
        0.0
    };
    (meters, meters / METERS_PER_MILE * CO2_G_PER_CAR_MILE)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::config::Config;
    use crate::tracking::{RoutingSettings, haversine_m, latlng};

    const COSMIC: LatLng = latlng(35.913211, -79.055094);
    const HINTON: LatLng = latlng(35.902364, -79.043231);

    fn close(a: f64, b: f64, tolerance: f64) -> bool {
        (a - b).abs() <= tolerance
    }

    #[test]
    fn a_car_mile_is_400_g() {
        assert_eq!(
            carbon_for_distance(METERS_PER_MILE),
            (METERS_PER_MILE, 400.0)
        );
        assert_eq!(carbon_for_distance(0.0), (0.0, 0.0));
        let (m, g) = carbon_for_distance(2506.5);
        assert_eq!(m, 2506.5);
        // OSRM's car route from Cosmic Cantina to Hinton James (docs/DECISIONS.md, Routing).
        assert!(close(g, 622.98, 0.01), "{g}");
    }

    #[test]
    fn bad_distances_count_as_zero() {
        for bad in [-5.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(carbon_for_distance(bad), (0.0, 0.0), "{bad}");
        }
    }

    #[test]
    fn the_fallback_is_straight_line_times_1_3() {
        let (m, g) = fallback_carbon(COSMIC, HINTON);
        assert!(close(m, haversine_m(COSMIC, HINTON) * 1.3, 1e-9));
        // 2094.7 m (docs/DECISIONS.md, Routing): 1.30 car miles, about 520.6 g.
        assert!(close(m, 2094.7, 0.1), "{m}");
        assert!(close(g, 520.6, 0.1), "{g}");
        assert_eq!(fallback_carbon(COSMIC, COSMIC), (0.0, 0.0));
    }

    #[test]
    fn the_method_string_is_exact() {
        assert_eq!(METHOD, "EPA estimate: about 400 g CO2 per car mile avoided");
    }

    /// An app whose OSRM is a local port with nothing listening: every route is the fallback.
    async fn state_without_osrm() -> AppState {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let vars = std::collections::HashMap::from([
            (
                "DATABASE_URL".to_owned(),
                "postgres://postgres:postgres@127.0.0.1:1/unused".to_owned(),
            ),
            ("JWT_SECRET".to_owned(), "x".repeat(32)),
            ("OSRM_BASE_URL".to_owned(), dead.clone()),
        ]);
        let config = Config::from_vars(&vars).unwrap();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(100))
            .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/unused")
            .unwrap();
        let mut state = AppState::new(pool, config).unwrap();
        // No pacing, so the retry after the refused connection does not wait a second.
        let mut settings = RoutingSettings::new(dead);
        settings.min_interval = Duration::ZERO;
        state.routing = tracking::Routing::new(state.http.clone(), settings);
        state
    }

    #[tokio::test]
    async fn delivery_carbon_uses_the_fallback_when_osrm_is_down() {
        let state = state_without_osrm().await;
        let (m, g) = carbon_for_delivery(&state, COSMIC, HINTON).await;
        assert_eq!((m, g), fallback_carbon(COSMIC, HINTON));
        assert!(close(g, 520.6, 0.1), "{g}");
    }
}
