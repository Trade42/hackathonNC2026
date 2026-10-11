//! The runner's walk from where they are now: to the restaurant first while the run is still
//! before pickup (open, closed or purchased), then to each active order's drop-off in walking
//! order. Every ETA in the track and in `runner.location` comes from here.
//!
//! - **Walking order** is nearest neighbor by straight-line distance, starting at the restaurant
//!   (before pickup) or at the runner (delivering). Orders at the same spot are consecutive.
//! - **Distances** are OSRM's walking legs, each cached (or the fallback). Legs between fixed
//!   points (the restaurant and drop-offs) stay cached while the runner moves.
//! - **ETAs** are the walking distance to that point, cumulative, at the runner's pace, rounded up
//!   to a whole minute (`geo::eta_minutes`). Time spent waiting at the restaurant is not known and
//!   not counted.

use super::geo::{LatLng, Pace, Place, eta_minutes, haversine_m};
use super::routing::{Profile, Route, Routing};

/// An order the runner still has to deliver (status funded, purchased or picked_up).
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveOrder {
    pub id: String,
    pub customer_id: String,
    pub dropoff: Place,
}

/// One drop-off on the walk, in walking order.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedStop {
    pub order: ActiveOrder,
    /// Walking meters from the plan's origin to this stop, through every earlier stop.
    pub distance_m: f64,
    pub eta_minutes: f64,
}

/// The walk from `origin`.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub origin: LatLng,
    /// The restaurant, when the walk goes there first.
    pub restaurant: Option<LatLng>,
    /// Leg `i` ends at waypoint `i`: the restaurant (when set), then each stop.
    pub legs: Vec<Route>,
    pub stops: Vec<PlannedStop>,
    pub pace: Pace,
}

impl Plan {
    /// Plans the walk. `restaurant` is `Some` before pickup. Several legs may call OSRM; each is
    /// cached, so a moving runner usually costs one call (the first leg).
    pub async fn build(
        routing: &Routing,
        origin: LatLng,
        restaurant: Option<LatLng>,
        orders: &[ActiveOrder],
        pace: Pace,
    ) -> Self {
        let start = restaurant.unwrap_or(origin);
        let ordered = walking_order(start, orders);
        let mut waypoints: Vec<LatLng> = restaurant.into_iter().collect();
        waypoints.extend(ordered.iter().map(|o| o.dropoff.location));

        let mut legs = Vec::with_capacity(waypoints.len());
        let mut from = origin;
        for &to in &waypoints {
            legs.push(routing.route(Profile::Foot, from, to).await);
            from = to;
        }

        let offset = usize::from(restaurant.is_some());
        let mut walked = legs.iter().take(offset).map(|l| l.distance_m).sum::<f64>();
        let stops = ordered
            .into_iter()
            .zip(legs.iter().skip(offset))
            .map(|(order, leg)| {
                walked += leg.distance_m;
                PlannedStop {
                    order,
                    distance_m: walked,
                    eta_minutes: eta_minutes(walked, pace),
                }
            })
            .collect();
        Self {
            origin,
            restaurant,
            legs,
            stops,
            pace,
        }
    }

    /// The first leg: to the restaurant before pickup, else to the first stop. `None` when there
    /// is nowhere left to walk.
    pub fn next_leg(&self) -> Option<&Route> {
        self.legs.first()
    }

    /// The earliest of `customer_id`'s stops.
    pub fn first_stop_of(&self, customer_id: &str) -> Option<&PlannedStop> {
        self.stops
            .iter()
            .find(|s| s.order.customer_id == customer_id)
    }

    /// The ETA in `user_id`'s `runner.location` copy: the runner gets the next leg's, a customer
    /// their own first stop's. `None` for anyone else. Always finite.
    pub fn eta_for(&self, user_id: &str, runner_id: &str) -> Option<f64> {
        if user_id == runner_id {
            return Some(
                self.next_leg()
                    .map_or(0.0, |leg| eta_minutes(leg.distance_m, self.pace)),
            );
        }
        self.first_stop_of(user_id).map(|s| s.eta_minutes)
    }

    /// The route a customer sees: from the runner through the restaurant (before pickup)
    /// straight to `stop`, leaving out other customers' drop-offs. `None` when any leg is the
    /// fallback (no geometry).
    pub async fn route_to(&self, routing: &Routing, stop: &PlannedStop) -> Option<Vec<[f64; 2]>> {
        let target = stop.order.dropoff.location;
        let mut legs = Vec::with_capacity(2);
        match self.restaurant {
            Some(restaurant) => {
                legs.push(self.legs.first()?.clone());
                legs.push(routing.route(Profile::Foot, restaurant, target).await);
            }
            None => legs.push(routing.route(Profile::Foot, self.origin, target).await),
        }
        join_geometry(&legs)
    }
}

/// `orders` in nearest-neighbor order from `start`, by straight-line distance. Ties keep the
/// input order, so the same orders always plan the same way.
pub fn walking_order(start: LatLng, orders: &[ActiveOrder]) -> Vec<ActiveOrder> {
    let mut left: Vec<&ActiveOrder> = orders.iter().collect();
    let mut ordered = Vec::with_capacity(left.len());
    let mut at = start;
    while !left.is_empty() {
        let mut best = 0;
        let mut best_d = f64::INFINITY;
        for (i, order) in left.iter().enumerate() {
            let d = haversine_m(at, order.dropoff.location);
            if d < best_d {
                best = i;
                best_d = d;
            }
        }
        let next = left.remove(best);
        at = next.dropoff.location;
        ordered.push(next.clone());
    }
    ordered
}

/// The legs' geometries end to end, without repeating a shared point. `None` if any leg has no
/// geometry; a single point is doubled so the LineString has its 2 positions.
pub fn join_geometry(legs: &[Route]) -> Option<Vec<[f64; 2]>> {
    let mut joined: Vec<[f64; 2]> = Vec::new();
    for leg in legs {
        for &position in leg.geometry.as_ref()? {
            if joined.last() != Some(&position) {
                joined.push(position);
            }
        }
    }
    match joined.len() {
        0 => None,
        1 => Some(vec![joined[0], joined[0]]),
        _ => Some(joined),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracking::geo::{fallback_distance_m, latlng};
    use crate::tracking::routing::RouteSource;
    use crate::tracking::routing::tests::{COSMIC, HINTON, Mode, client, stub};

    const EHAUS: LatLng = latlng(35.904385, -79.042914);
    const DAVIS: LatLng = latlng(35.910957, -79.047906);

    fn order(id: &str, customer: &str, at: LatLng) -> ActiveOrder {
        ActiveOrder {
            id: id.into(),
            customer_id: customer.into(),
            dropoff: Place {
                id: None,
                label: id.into(),
                location: at,
                note: None,
            },
        }
    }

    #[test]
    fn nearest_neighbor_order() {
        let orders = [
            order("ord_hinton", "usr_a", HINTON),
            order("ord_davis", "usr_b", DAVIS),
            order("ord_ehaus", "usr_c", EHAUS),
            order("ord_hinton2", "usr_d", HINTON),
        ];
        let ids: Vec<String> = walking_order(COSMIC, &orders)
            .into_iter()
            .map(|o| o.id)
            .collect();
        // From Franklin St: Davis is closest, then Ehringhaus, then both Hinton orders together.
        assert_eq!(ids, ["ord_davis", "ord_ehaus", "ord_hinton", "ord_hinton2"]);
        assert!(walking_order(COSMIC, &[]).is_empty());
    }

    #[test]
    fn geometry_joins_without_repeats() {
        let leg = |coords: Vec<[f64; 2]>| Route {
            distance_m: 1.0,
            geometry: Some(coords),
            source: RouteSource::Osrm,
        };
        let joined = join_geometry(&[
            leg(vec![[0.0, 0.0], [1.0, 1.0]]),
            leg(vec![[1.0, 1.0], [2.0, 2.0]]),
        ]);
        assert_eq!(joined, Some(vec![[0.0, 0.0], [1.0, 1.0], [2.0, 2.0]]));
        assert_eq!(
            join_geometry(&[leg(vec![[3.0, 3.0], [3.0, 3.0]])]),
            Some(vec![[3.0, 3.0], [3.0, 3.0]])
        );
        let fallback = Route {
            distance_m: 1.0,
            geometry: None,
            source: RouteSource::Fallback,
        };
        assert_eq!(
            join_geometry(&[leg(vec![[0.0, 0.0], [1.0, 1.0]]), fallback]),
            None
        );
        assert_eq!(join_geometry(&[]), None);
    }

    #[tokio::test]
    async fn before_pickup_the_walk_starts_with_the_restaurant() {
        // Every OSRM leg is 1000 m.
        let server = stub(Mode::Ok { distance: 1000.0 }).await;
        let routing = client(&server.base_url);
        let runner = latlng(35.9135, -79.0560);
        let orders = [
            order("ord_a", "usr_a", HINTON),
            order("ord_b", "usr_b", DAVIS),
        ];
        let plan = Plan::build(&routing, runner, Some(COSMIC), &orders, Pace::Normal).await;
        assert_eq!(plan.legs.len(), 3);
        // Runner to restaurant 1000 m, then Davis (nearest) at 2000 m, then Hinton at 3000 m.
        assert_eq!(plan.stops[0].order.id, "ord_b");
        assert_eq!(plan.stops[0].distance_m, 2000.0);
        assert_eq!(plan.stops[1].distance_m, 3000.0);
        // 3000 m at 1.4 m/s is 35.7 minutes: 36.
        assert_eq!(plan.stops[1].eta_minutes, 36.0);
        assert_eq!(plan.eta_for("usr_a", "usr_runner"), Some(36.0));
        // The runner's ETA is to the restaurant: 1000 m, 12 minutes.
        assert_eq!(plan.eta_for("usr_runner", "usr_runner"), Some(12.0));
        assert_eq!(plan.eta_for("usr_member", "usr_runner"), None);

        // Usr_a's route leaves out Davis: runner to restaurant, then restaurant to Hinton.
        let route = plan.route_to(&routing, &plan.stops[1]).await.unwrap();
        assert!(route.len() >= 2);
    }

    #[tokio::test]
    async fn delivering_starts_at_the_runner_and_falls_back_without_osrm() {
        let routing = client("http://127.0.0.1:1");
        let runner = DAVIS;
        let orders = [order("ord_a", "usr_a", HINTON)];
        let plan = Plan::build(&routing, runner, None, &orders, Pace::Brisk).await;
        assert_eq!(plan.legs.len(), 1);
        assert_eq!(plan.legs[0].source, RouteSource::Fallback);
        let expected = fallback_distance_m(DAVIS, HINTON);
        assert!((plan.stops[0].distance_m - expected).abs() < 1e-9);
        assert_eq!(
            plan.stops[0].eta_minutes,
            eta_minutes(expected, Pace::Brisk)
        );
        assert_eq!(plan.route_to(&routing, &plan.stops[0]).await, None);

        // Nothing left to deliver: no legs, and the runner's ETA is 0.
        let empty = Plan::build(&routing, runner, None, &[], Pace::Brisk).await;
        assert!(empty.next_leg().is_none());
        assert_eq!(empty.eta_for("usr_runner", "usr_runner"), Some(0.0));
    }
}
