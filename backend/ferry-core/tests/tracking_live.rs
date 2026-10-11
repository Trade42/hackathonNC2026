//! Manual only: the routing client against the live FOSSGIS OSRM server. Ignored by default, so
//! `cargo test` never calls it. Run it by hand:
//!
//! ```sh
//! cargo test --test tracking_live -- --ignored --nocapture
//! ```

mod common;

use ferry_core::state::http_client;
use ferry_core::tracking::{
    LatLng, OSRM_DEFAULT_BASE_URL, Pace, RouteSource, Routing, RoutingSettings,
    fallback_distance_m, latlng,
};

/// Cosmic Cantina and the Hinton James lobby (contract/fixtures: rst_cosmic, place_hinton).
const COSMIC: LatLng = latlng(35.913211, -79.055094);
const HINTON: LatLng = latlng(35.902364, -79.043231);

#[tokio::test]
#[ignore = "manual: calls the live FOSSGIS OSRM server"]
async fn cosmic_cantina_to_hinton_james_on_the_live_server() {
    let routing = Routing::new(
        http_client().unwrap(),
        RoutingSettings::new(OSRM_DEFAULT_BASE_URL),
    );
    let walk = routing.walk(COSMIC, HINTON, Pace::Normal).await;
    let car = routing
        .route(ferry_core::tracking::Profile::Car, COSMIC, HINTON)
        .await;
    println!(
        "walking: {:.1} m ({:?}, {} positions), {} min at normal pace",
        walk.distance_m,
        walk.source,
        walk.route.as_ref().map_or(0, |r| r.coordinates.len()),
        walk.eta_minutes
    );
    println!("car: {:.1} m ({:?})", car.distance_m, car.source);
    println!(
        "fallback (straight line x 1.3): {:.1} m",
        fallback_distance_m(COSMIC, HINTON)
    );
    assert_eq!(walk.source, RouteSource::Osrm);
    assert_eq!(car.source, RouteSource::Osrm);
    assert!(walk.route.is_some());
}
