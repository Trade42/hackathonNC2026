//! The demo's past weeks (spec section 6, demo data): delivered runs and orders from the last 7
//! weeks, flagged `is_demo_history`, so Impact, Circle.stats and the weekly chart have something
//! to show before the first real delivery. `ferry-core seed` calls [`seed_history`] after the
//! fixture seed and before the demo balances.
//!
//! - Everything comes from contract/fixtures: the personas, their circles, the restaurants and
//!   their menus (prices and choices), the places and each customer's default tip.
//! - Times are fixed offsets from today's UTC midnight, all at least a day back and inside the
//!   8-week window whatever the weekday. Every order is created before its run's window closes
//!   and delivered after that.
//! - `car_distance_m` is the routing fallback from the fixture coordinates (straight line × 1.3;
//!   OSRM is never called while seeding) and `co2_saved_g` follows from it. `walk_distance_m` is
//!   the same estimate, rounded to the meter like the pings' measure.
//! - No ledger rows: the history exists for impact only, and no money ever moved for it.
//! - Fixed ids make it idempotent: a second seed on the same UTC day writes nothing, and a seed
//!   on a later day only moves the times. It writes only rows with `is_demo_history` (runs,
//!   their orders, and those orders' lines and events) and never touches run_demo or any other
//!   row.

use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};

use super::carbon::fallback_carbon;
use crate::demo::fixtures::{FixtureError, Fixtures};
use crate::tracking::{LatLng, Pace, eta_minutes};

/// Spec section 4's demo values (the config's defaults for TAX_RATE_BPS and FERRY_FEE_CENTS).
pub const TAX_RATE_BPS: i64 = 750;
pub const FERRY_FEE_CENTS: i64 = 150;

/// One past run. `leaves_min` is minutes after UTC midnight, `days_ago` days before today.
struct Run {
    id: &'static str,
    days_ago: i64,
    leaves_min: i64,
    restaurant: &'static str,
    runner: &'static str,
    circle: &'static str,
    /// Settled (paid out); otherwise still delivered.
    settled: bool,
    orders: &'static [Order],
}

struct Order {
    customer: &'static str,
    place: &'static str,
    lines: &'static [Line],
}

struct Line {
    item: &'static str,
    qty: i64,
    choices: &'static [&'static str],
}

const fn line(item: &'static str, choices: &'static [&'static str]) -> Line {
    Line {
        item,
        qty: 1,
        choices,
    }
}

const HINTON4: &str = "cir_hinton4";
const CLIMB: &str = "cir_climb";

/// Seven weeks of runs, oldest first, 28 orders: 2, 3, 3, 4, 4, 5 and 7 a week as more of the
/// floor joins in. Evenings are 22:00 to 23:00 UTC (6 to 7 PM in Chapel Hill), lunch about 16:30,
/// Alpine Bagel mid-morning, Insomnia Cookies late at night. Maya and Dana run (runner_opt_in);
/// Jordan (the judge) and Leo order.
const RUNS: &[Run] = &[
    // 7 weeks ago.
    Run {
        id: "run_hist_01",
        days_ago: 46,
        leaves_min: 22 * 60 + 30,
        restaurant: "rst_cosmic",
        runner: "usr_maya",
        circle: HINTON4,
        settled: true,
        orders: &[
            Order {
                customer: "usr_judge",
                place: "place_fetzer",
                lines: &[line(
                    "itm_cosmic_chicken_burrito",
                    &["cho_cosmic_chicken_burrito_black_beans"],
                )],
            },
            Order {
                customer: "usr_dana",
                place: "place_hinton",
                lines: &[line("itm_cosmic_nachos", &[])],
            },
        ],
    },
    // 6 weeks ago.
    Run {
        id: "run_hist_02",
        days_ago: 41,
        leaves_min: 16 * 60 + 30,
        restaurant: "rst_meddeli",
        runner: "usr_dana",
        circle: HINTON4,
        settled: true,
        orders: &[
            Order {
                customer: "usr_judge",
                place: "place_davis",
                lines: &[line(
                    "itm_meddeli_falafel_wrap",
                    &["cho_meddeli_falafel_wrap_hummus"],
                )],
            },
            Order {
                customer: "usr_leo",
                place: "place_hinton",
                lines: &[line(
                    "itm_meddeli_chicken_shawarma_wrap",
                    &["cho_meddeli_chicken_shawarma_wrap_garlic"],
                )],
            },
        ],
    },
    Run {
        id: "run_hist_03",
        days_ago: 38,
        leaves_min: 2 * 60 + 30,
        restaurant: "rst_insomnia",
        runner: "usr_maya",
        circle: CLIMB,
        settled: true,
        orders: &[Order {
            customer: "usr_leo",
            place: "place_hinton",
            lines: &[Line {
                item: "itm_insomnia_classic_cookie",
                qty: 2,
                choices: &["cho_insomnia_classic_cookie_chocolate_chunk"],
            }],
        }],
    },
    // 5 weeks ago.
    Run {
        id: "run_hist_04",
        days_ago: 34,
        leaves_min: 22 * 60 + 15,
        restaurant: "rst_supdogs",
        runner: "usr_maya",
        circle: HINTON4,
        settled: true,
        orders: &[
            Order {
                customer: "usr_leo",
                place: "place_hinton",
                lines: &[
                    line(
                        "itm_supdogs_classic_dog",
                        &["cho_supdogs_classic_dog_chili"],
                    ),
                    line("itm_supdogs_tots", &[]),
                ],
            },
            Order {
                customer: "usr_judge",
                place: "place_fetzer",
                lines: &[line("itm_supdogs_veggie_dog", &[])],
            },
        ],
    },
    Run {
        id: "run_hist_05",
        days_ago: 31,
        leaves_min: 14 * 60,
        restaurant: "rst_alpine",
        runner: "usr_dana",
        circle: HINTON4,
        settled: true,
        orders: &[Order {
            customer: "usr_maya",
            place: "place_ehaus",
            lines: &[line(
                "itm_alpine_bagel_spread",
                &[
                    "cho_alpine_bagel_spread_everything",
                    "cho_alpine_bagel_spread_scallion_cream_cheese",
                ],
            )],
        }],
    },
    // 4 weeks ago.
    Run {
        id: "run_hist_06",
        days_ago: 27,
        leaves_min: 22 * 60 + 45,
        restaurant: "rst_cosmic",
        runner: "usr_maya",
        circle: HINTON4,
        settled: true,
        orders: &[
            Order {
                customer: "usr_judge",
                place: "place_fetzer",
                lines: &[line(
                    "itm_cosmic_steak_burrito",
                    &[
                        "cho_cosmic_steak_burrito_pinto_beans",
                        "cho_cosmic_steak_burrito_guac",
                    ],
                )],
            },
            Order {
                customer: "usr_dana",
                place: "place_hinton",
                lines: &[line("itm_cosmic_chips_guac", &[])],
            },
            Order {
                customer: "usr_leo",
                place: "place_hinton",
                lines: &[line(
                    "itm_cosmic_chicken_burrito",
                    &["cho_cosmic_chicken_burrito_no_beans"],
                )],
            },
        ],
    },
    Run {
        id: "run_hist_07",
        days_ago: 24,
        leaves_min: 2 * 60,
        restaurant: "rst_insomnia",
        runner: "usr_maya",
        circle: CLIMB,
        settled: true,
        orders: &[Order {
            customer: "usr_leo",
            place: "place_hinton",
            lines: &[line(
                "itm_insomnia_cookie_box",
                &["cho_insomnia_cookie_box_six"],
            )],
        }],
    },
    // 3 weeks ago.
    Run {
        id: "run_hist_08",
        days_ago: 20,
        leaves_min: 16 * 60 + 15,
        restaurant: "rst_limebasil",
        runner: "usr_dana",
        circle: HINTON4,
        settled: true,
        orders: &[
            Order {
                customer: "usr_judge",
                place: "place_davis",
                lines: &[line(
                    "itm_limebasil_banh_mi",
                    &["cho_limebasil_banh_mi_lemongrass_chicken"],
                )],
            },
            Order {
                customer: "usr_maya",
                place: "place_hinton",
                lines: &[line("itm_limebasil_egg_rolls", &[])],
            },
        ],
    },
    Run {
        id: "run_hist_09",
        days_ago: 17,
        leaves_min: 22 * 60,
        restaurant: "rst_meddeli",
        runner: "usr_maya",
        circle: HINTON4,
        settled: true,
        orders: &[
            Order {
                customer: "usr_dana",
                place: "place_hinton",
                lines: &[line(
                    "itm_meddeli_falafel_wrap",
                    &["cho_meddeli_falafel_wrap_feta"],
                )],
            },
            Order {
                customer: "usr_judge",
                place: "place_fetzer",
                lines: &[line(
                    "itm_meddeli_chicken_shawarma_wrap",
                    &["cho_meddeli_chicken_shawarma_wrap_tahini"],
                )],
            },
        ],
    },
    // 2 weeks ago.
    Run {
        id: "run_hist_10",
        days_ago: 13,
        leaves_min: 22 * 60 + 30,
        restaurant: "rst_cosmic",
        runner: "usr_maya",
        circle: HINTON4,
        settled: true,
        orders: &[
            Order {
                customer: "usr_judge",
                place: "place_fetzer",
                lines: &[line(
                    "itm_cosmic_chicken_burrito",
                    &[
                        "cho_cosmic_chicken_burrito_no_beans",
                        "cho_cosmic_chicken_burrito_guac",
                    ],
                )],
            },
            Order {
                customer: "usr_leo",
                place: "place_hinton",
                lines: &[line("itm_cosmic_nachos", &["cho_cosmic_nachos_chicken"])],
            },
        ],
    },
    Run {
        id: "run_hist_11",
        days_ago: 11,
        leaves_min: 60 + 45,
        restaurant: "rst_insomnia",
        runner: "usr_maya",
        circle: CLIMB,
        settled: true,
        orders: &[Order {
            customer: "usr_leo",
            place: "place_ehaus",
            lines: &[Line {
                item: "itm_insomnia_classic_cookie",
                qty: 3,
                choices: &["cho_insomnia_classic_cookie_snickerdoodle"],
            }],
        }],
    },
    Run {
        id: "run_hist_12",
        days_ago: 9,
        leaves_min: 16 * 60 + 45,
        restaurant: "rst_weaver",
        runner: "usr_dana",
        circle: HINTON4,
        settled: true,
        orders: &[
            Order {
                customer: "usr_judge",
                place: "place_davis",
                lines: &[line(
                    "itm_weaver_kale_salad",
                    &["cho_weaver_kale_salad_chicken"],
                )],
            },
            Order {
                customer: "usr_maya",
                place: "place_hinton",
                lines: &[line("itm_weaver_hummus_wrap", &[])],
            },
        ],
    },
    // Last week.
    Run {
        id: "run_hist_13",
        days_ago: 6,
        leaves_min: 22 * 60 + 20,
        restaurant: "rst_supdogs",
        runner: "usr_dana",
        circle: HINTON4,
        settled: true,
        orders: &[
            Order {
                customer: "usr_judge",
                place: "place_fetzer",
                lines: &[line(
                    "itm_supdogs_classic_dog",
                    &[
                        "cho_supdogs_classic_dog_cheese",
                        "cho_supdogs_classic_dog_onions",
                    ],
                )],
            },
            Order {
                customer: "usr_leo",
                place: "place_hinton",
                lines: &[line("itm_supdogs_tots", &["cho_supdogs_tots_cheese"])],
            },
            Order {
                customer: "usr_maya",
                place: "place_hinton",
                lines: &[line("itm_supdogs_veggie_dog", &[])],
            },
        ],
    },
    Run {
        id: "run_hist_14",
        days_ago: 4,
        leaves_min: 14 * 60 + 15,
        restaurant: "rst_alpine",
        runner: "usr_maya",
        circle: HINTON4,
        settled: true,
        orders: &[
            Order {
                customer: "usr_dana",
                place: "place_hinton",
                lines: &[line("itm_alpine_veggie_egg", &[])],
            },
            // Not the judge: Davis Library and Fetzer Gym are a short walk from the Student Union.
            Order {
                customer: "usr_leo",
                place: "place_hinton",
                lines: &[line(
                    "itm_alpine_bagel_spread",
                    &[
                        "cho_alpine_bagel_spread_plain",
                        "cho_alpine_bagel_spread_plain_cream_cheese",
                    ],
                )],
            },
        ],
    },
    Run {
        id: "run_hist_15",
        days_ago: 2,
        leaves_min: 22 * 60 + 40,
        restaurant: "rst_cosmic",
        runner: "usr_maya",
        circle: HINTON4,
        settled: true,
        orders: &[
            Order {
                customer: "usr_judge",
                place: "place_fetzer",
                lines: &[line(
                    "itm_cosmic_chicken_burrito",
                    &[
                        "cho_cosmic_chicken_burrito_black_beans",
                        "cho_cosmic_chicken_burrito_sour_cream",
                    ],
                )],
            },
            Order {
                customer: "usr_dana",
                place: "place_hinton",
                lines: &[line(
                    "itm_cosmic_steak_burrito",
                    &["cho_cosmic_steak_burrito_no_beans"],
                )],
            },
        ],
    },
];

// A run's timeline, in minutes from the time it leaves. Orders come in while the window is open;
// the runner pays at the restaurant, picks up, then walks each drop-off in turn.
const RUN_CREATED: i64 = -20;
const FIRST_ORDER: i64 = -18;
const ORDER_GAP: i64 = 4;
const WINDOW_CLOSES: i64 = 10;
const PURCHASED: i64 = 15;
const PICKED_UP: i64 = 18;
const DROP_GAP: i64 = 2;
const SETTLED_AFTER: i64 = 2;

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error(transparent)]
    Fixture(#[from] FixtureError),
    #[error("demo history: {0}")]
    Plan(String),
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
}

/// Rows each statement wrote (inserted or changed; events also count the stale ones removed).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HistoryReport {
    pub written: Vec<(&'static str, u64)>,
}

impl HistoryReport {
    pub fn written(&self, table: &str) -> Option<u64> {
        self.written
            .iter()
            .find(|(name, _)| *name == table)
            .map(|(_, n)| *n)
    }
}

impl std::fmt::Display for HistoryReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let parts: Vec<String> = self
            .written
            .iter()
            .map(|(table, n)| format!("{table} {n}"))
            .collect();
        write!(f, "{}", parts.join(", "))
    }
}

/// The JSON rows for each table, times as minutes from today's UTC midnight (`*_min`).
#[derive(Debug, Clone, Default)]
struct Rows {
    runs: Vec<Value>,
    orders: Vec<Value>,
    lines: Vec<Value>,
    events: Vec<Value>,
}

/// Builds every row from [`RUNS`] and the fixtures, checking each id against them.
fn plan(f: &Fixtures) -> Result<Rows, HistoryError> {
    let err = |what: String| HistoryError::Plan(what);
    let mut rows = Rows::default();
    for run in RUNS {
        let restaurant = f
            .restaurants
            .iter()
            .find(|r| r.id == run.restaurant)
            .ok_or_else(|| err(format!("{}: no restaurant {}", run.id, run.restaurant)))?;
        let menu = f
            .menus
            .iter()
            .find(|m| m.restaurant_id == run.restaurant)
            .ok_or_else(|| err(format!("{}: no menu for {}", run.id, run.restaurant)))?;
        let circle = f
            .circles
            .iter()
            .find(|c| c.id == run.circle)
            .ok_or_else(|| err(format!("{}: no circle {}", run.id, run.circle)))?;
        let is_member = |user: &str| circle.members.iter().any(|m| m.id == user);
        if !is_member(run.runner) {
            return Err(err(format!(
                "{}: {} is not in {}",
                run.id, run.runner, run.circle
            )));
        }

        let leaves = -run.days_ago * 24 * 60 + run.leaves_min;
        rows.runs.push(json!({
            "id": run.id,
            "restaurant_id": run.restaurant,
            "runner_id": run.runner,
            "circle_id": run.circle,
            "leaving_min": leaves,
            "window_closes_min": leaves + WINDOW_CLOSES,
            "created_min": leaves + RUN_CREATED,
        }));

        let order_status = if run.settled { "settled" } else { "delivered" };
        for (i, order) in run.orders.iter().enumerate() {
            let i = i as i64;
            let order_id = format!(
                "ord_{}{}",
                &run.id["run_".len()..],
                (b'a' + i as u8) as char
            );
            if !is_member(order.customer) || order.customer == run.runner {
                return Err(err(format!(
                    "{order_id}: {} cannot order on {}",
                    order.customer, run.id
                )));
            }
            let customer = f
                .users
                .iter()
                .find(|u| u.id == order.customer)
                .ok_or_else(|| err(format!("{order_id}: no user {}", order.customer)))?;
            let place = f
                .places
                .iter()
                .find(|p| p.id.as_deref() == Some(order.place))
                .ok_or_else(|| err(format!("{order_id}: no place {}", order.place)))?;

            let mut subtotal = 0;
            for (line_no, l) in order.lines.iter().enumerate() {
                let (name, unit) = price_line(menu, l)
                    .map_err(|why| err(format!("{order_id} line {line_no}: {why}")))?;
                subtotal += unit * l.qty;
                rows.lines.push(json!({
                    "order_id": order_id,
                    "line_no": line_no,
                    "menu_item_id": l.item,
                    "name": name,
                    "qty": l.qty,
                    "choice_ids": l.choices,
                    "unit_price_cents": unit,
                    "line_total_cents": unit * l.qty,
                }));
            }
            let tax = tax_cents(subtotal);
            let tip = customer.profile.default_tip_cents;
            let (car_m, co2_g) = fallback_carbon(restaurant.location, place.location);
            let created = leaves + FIRST_ORDER + ORDER_GAP * i;
            let delivered = leaves
                + PICKED_UP
                + walk_minutes(restaurant.location, place.location)
                + DROP_GAP * i;
            rows.orders.push(json!({
                "id": order_id,
                "run_id": run.id,
                "customer_id": order.customer,
                "status": order_status,
                "subtotal_cents": subtotal,
                "tax_cents": tax,
                "ferry_fee_cents": FERRY_FEE_CENTS,
                "tip_cents": tip,
                "total_cents": subtotal + tax + FERRY_FEE_CENTS + tip,
                "dropoff_place_id": order.place,
                "dropoff_label": place.label,
                "dropoff_lat": place.location.lat,
                "dropoff_lng": place.location.lng,
                "dropoff_note": place.note,
                "handoff_code": handoff_code(&order_id),
                "car_distance_m": car_m,
                "walk_distance_m": car_m.round(),
                "co2_saved_g": co2_g,
                "delivered_min": delivered,
                "created_min": created,
            }));

            let mut timeline = vec![
                ("funded", created),
                ("purchased", leaves + PURCHASED),
                ("picked_up", leaves + PICKED_UP),
                ("delivered", delivered),
            ];
            if run.settled {
                timeline.push(("settled", delivered + SETTLED_AFTER));
            }
            for (status, at) in timeline {
                rows.events.push(json!({
                    "order_id": order_id,
                    "run_id": run.id,
                    "status": status,
                    "ts_min": at,
                }));
            }
        }
    }
    Ok(rows)
}

/// The line's item name and unit price (the item's price plus its choices' deltas), after
/// checking its choices against the menu's options and their min and max.
fn price_line(menu: &crate::catalog::types::Menu, l: &Line) -> Result<(String, i64), String> {
    let item = menu
        .categories
        .iter()
        .flat_map(|c| &c.items)
        .find(|i| i.id == l.item)
        .ok_or_else(|| format!("{} is not on the menu", l.item))?;
    let mut unit = item.price_cents;
    let mut used = 0;
    for option in &item.options {
        let picked: Vec<_> = option
            .choices
            .iter()
            .filter(|c| l.choices.contains(&c.id.as_str()))
            .collect();
        let n = picked.len() as i32;
        if n < option.min || n > option.max {
            return Err(format!(
                "{} takes {} to {} of {}, not {n}",
                l.item, option.min, option.max, option.id
            ));
        }
        unit += picked.iter().map(|c| c.price_delta_cents).sum::<i64>();
        used += picked.len();
    }
    if used != l.choices.len() {
        return Err(format!("{} has a choice from another item", l.item));
    }
    if l.qty < 1 {
        return Err(format!("{} needs a quantity of at least 1", l.item));
    }
    Ok((item.name.clone(), unit))
}

/// Tax at [`TAX_RATE_BPS`], rounded half up to the cent.
fn tax_cents(subtotal_cents: i64) -> i64 {
    (subtotal_cents * TAX_RATE_BPS + 5_000) / 10_000
}

/// Minutes to walk the routing fallback's distance at the normal pace.
fn walk_minutes(from: LatLng, to: LatLng) -> i64 {
    eta_minutes(fallback_carbon(from, to).0, Pace::Normal) as i64
}

/// A fixed 4-digit code per order id.
fn handoff_code(order_id: &str) -> String {
    let hash = order_id
        .bytes()
        .fold(17u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    format!("{:04}", hash % 10_000)
}

/// History runs, delivered, created by message. A row with the same id is rewritten only when it
/// is history itself and differs.
const RUNS_SQL: &str = "\
WITH a AS (SELECT date_trunc('day', now(), 'UTC') AS at)
INSERT INTO ferry.runs AS t (id, restaurant_id, runner_id, circle_id, status, created_via,
                             leaving_at, window_closes_at, receipt_total_cents, receipt_path,
                             is_demo_history, created_at)
SELECT r.id, r.restaurant_id, r.runner_id, r.circle_id, 'delivered', 'message',
       a.at + make_interval(mins => r.leaving_min),
       a.at + make_interval(mins => r.window_closes_min),
       NULL, NULL, true,
       a.at + make_interval(mins => r.created_min)
  FROM jsonb_to_recordset($1::jsonb) AS r(
           id text, restaurant_id text, runner_id text, circle_id text,
           leaving_min integer, window_closes_min integer, created_min integer)
 CROSS JOIN a
ON CONFLICT (id) DO UPDATE SET
    restaurant_id = EXCLUDED.restaurant_id, runner_id = EXCLUDED.runner_id,
    circle_id = EXCLUDED.circle_id, status = EXCLUDED.status, created_via = EXCLUDED.created_via,
    leaving_at = EXCLUDED.leaving_at, window_closes_at = EXCLUDED.window_closes_at,
    receipt_total_cents = EXCLUDED.receipt_total_cents, receipt_path = EXCLUDED.receipt_path,
    created_at = EXCLUDED.created_at
 WHERE t.is_demo_history
   AND (t.restaurant_id, t.runner_id, t.circle_id, t.status, t.created_via, t.leaving_at,
        t.window_closes_at, t.receipt_total_cents, t.receipt_path, t.created_at)
       IS DISTINCT FROM
       (EXCLUDED.restaurant_id, EXCLUDED.runner_id, EXCLUDED.circle_id, EXCLUDED.status,
        EXCLUDED.created_via, EXCLUDED.leaving_at, EXCLUDED.window_closes_at,
        EXCLUDED.receipt_total_cents, EXCLUDED.receipt_path, EXCLUDED.created_at)";

/// History orders, only on history runs.
const ORDERS_SQL: &str = "\
WITH a AS (SELECT date_trunc('day', now(), 'UTC') AS at)
INSERT INTO ferry.orders AS t (id, run_id, customer_id, status, subtotal_cents, tax_cents,
                               ferry_fee_cents, tip_cents, total_cents, dropoff_place_id,
                               dropoff_label, dropoff_lat, dropoff_lng, dropoff_note, handoff_code,
                               car_distance_m, walk_distance_m, co2_saved_g, delivered_at,
                               is_demo_history, created_at)
SELECT r.id, r.run_id, r.customer_id, r.status, r.subtotal_cents, r.tax_cents,
       r.ferry_fee_cents, r.tip_cents, r.total_cents, r.dropoff_place_id,
       r.dropoff_label, r.dropoff_lat, r.dropoff_lng, r.dropoff_note, r.handoff_code,
       r.car_distance_m, r.walk_distance_m, r.co2_saved_g,
       a.at + make_interval(mins => r.delivered_min),
       true,
       a.at + make_interval(mins => r.created_min)
  FROM jsonb_to_recordset($1::jsonb) AS r(
           id text, run_id text, customer_id text, status text, subtotal_cents bigint,
           tax_cents bigint, ferry_fee_cents bigint, tip_cents bigint, total_cents bigint,
           dropoff_place_id text, dropoff_label text, dropoff_lat double precision,
           dropoff_lng double precision, dropoff_note text, handoff_code text,
           car_distance_m double precision, walk_distance_m double precision,
           co2_saved_g double precision, delivered_min integer, created_min integer)
 CROSS JOIN a
 WHERE EXISTS (SELECT 1 FROM ferry.runs x WHERE x.id = r.run_id AND x.is_demo_history)
ON CONFLICT (id) DO UPDATE SET
    run_id = EXCLUDED.run_id, customer_id = EXCLUDED.customer_id, status = EXCLUDED.status,
    subtotal_cents = EXCLUDED.subtotal_cents, tax_cents = EXCLUDED.tax_cents,
    ferry_fee_cents = EXCLUDED.ferry_fee_cents, tip_cents = EXCLUDED.tip_cents,
    total_cents = EXCLUDED.total_cents, dropoff_place_id = EXCLUDED.dropoff_place_id,
    dropoff_label = EXCLUDED.dropoff_label, dropoff_lat = EXCLUDED.dropoff_lat,
    dropoff_lng = EXCLUDED.dropoff_lng, dropoff_note = EXCLUDED.dropoff_note,
    handoff_code = EXCLUDED.handoff_code, car_distance_m = EXCLUDED.car_distance_m,
    walk_distance_m = EXCLUDED.walk_distance_m, co2_saved_g = EXCLUDED.co2_saved_g,
    delivered_at = EXCLUDED.delivered_at, created_at = EXCLUDED.created_at
 WHERE t.is_demo_history
   AND (t.run_id, t.customer_id, t.status, t.subtotal_cents, t.tax_cents, t.ferry_fee_cents,
        t.tip_cents, t.total_cents, t.dropoff_place_id, t.dropoff_label, t.dropoff_lat,
        t.dropoff_lng, t.dropoff_note, t.handoff_code, t.car_distance_m, t.walk_distance_m,
        t.co2_saved_g, t.delivered_at, t.created_at)
       IS DISTINCT FROM
       (EXCLUDED.run_id, EXCLUDED.customer_id, EXCLUDED.status, EXCLUDED.subtotal_cents,
        EXCLUDED.tax_cents, EXCLUDED.ferry_fee_cents, EXCLUDED.tip_cents, EXCLUDED.total_cents,
        EXCLUDED.dropoff_place_id, EXCLUDED.dropoff_label, EXCLUDED.dropoff_lat,
        EXCLUDED.dropoff_lng, EXCLUDED.dropoff_note, EXCLUDED.handoff_code,
        EXCLUDED.car_distance_m, EXCLUDED.walk_distance_m, EXCLUDED.co2_saved_g,
        EXCLUDED.delivered_at, EXCLUDED.created_at)";

/// History orders' lines, only on history orders.
const LINES_SQL: &str = "\
INSERT INTO ferry.order_lines AS t (order_id, line_no, menu_item_id, name, qty, choice_ids,
                                    unit_price_cents, line_total_cents, note)
SELECT r.order_id, r.line_no, r.menu_item_id, r.name, r.qty, r.choice_ids,
       r.unit_price_cents, r.line_total_cents, NULL
  FROM jsonb_to_recordset($1::jsonb) AS r(
           order_id text, line_no integer, menu_item_id text, name text, qty integer,
           choice_ids text[], unit_price_cents bigint, line_total_cents bigint)
 WHERE EXISTS (SELECT 1 FROM ferry.orders o WHERE o.id = r.order_id AND o.is_demo_history)
ON CONFLICT (order_id, line_no) DO UPDATE SET
    menu_item_id = EXCLUDED.menu_item_id, name = EXCLUDED.name, qty = EXCLUDED.qty,
    choice_ids = EXCLUDED.choice_ids, unit_price_cents = EXCLUDED.unit_price_cents,
    line_total_cents = EXCLUDED.line_total_cents, note = EXCLUDED.note
 WHERE (t.menu_item_id, t.name, t.qty, t.choice_ids, t.unit_price_cents, t.line_total_cents,
        t.note)
       IS DISTINCT FROM
       (EXCLUDED.menu_item_id, EXCLUDED.name, EXCLUDED.qty, EXCLUDED.choice_ids,
        EXCLUDED.unit_price_cents, EXCLUDED.line_total_cents, EXCLUDED.note)";

/// The wanted timeline rows, shared by the two event statements.
const WANTED_EVENTS: &str = "\
WITH a AS (SELECT date_trunc('day', now(), 'UTC') AS at),
want AS (
    SELECT w.order_id, w.run_id, w.status, a.at + make_interval(mins => w.ts_min) AS ts
      FROM jsonb_to_recordset($1::jsonb) AS w(order_id text, run_id text, status text, ts_min integer)
     CROSS JOIN a
)";

/// Removes a history order's events that are not in its wanted timeline (after a seed on an
/// earlier day). order_events has no natural key, so events are matched by every column.
fn stale_events_sql() -> String {
    format!(
        "{WANTED_EVENTS}
DELETE FROM ferry.order_events e
 USING ferry.orders o
 WHERE o.id = e.order_id AND o.is_demo_history
   AND e.order_id IN (SELECT order_id FROM want)
   AND NOT EXISTS (SELECT 1 FROM want w
                    WHERE w.order_id = e.order_id AND w.run_id = e.run_id
                      AND w.status = e.status AND w.ts = e.ts)"
    )
}

/// Adds the wanted events a history order does not have yet.
fn missing_events_sql() -> String {
    format!(
        "{WANTED_EVENTS}
INSERT INTO ferry.order_events (order_id, run_id, ts, status)
SELECT w.order_id, w.run_id, w.ts, w.status
  FROM want w
 WHERE EXISTS (SELECT 1 FROM ferry.orders o WHERE o.id = w.order_id AND o.is_demo_history)
   AND NOT EXISTS (SELECT 1 FROM ferry.order_events e
                    WHERE e.order_id = w.order_id AND e.run_id = w.run_id
                      AND e.status = w.status AND e.ts = w.ts)"
    )
}

/// Seeds the demo history in one transaction. Needs the fixtures seeded first (`demo::seed`).
pub async fn seed_history(pool: &PgPool) -> Result<HistoryReport, HistoryError> {
    let mut tx = pool.begin().await?;
    let report = seed_history_on(&mut tx).await?;
    tx.commit().await?;
    Ok(report)
}

/// [`seed_history`] on `conn`, which the caller has inside a transaction.
pub async fn seed_history_on(conn: &mut PgConnection) -> Result<HistoryReport, HistoryError> {
    let rows = plan(&Fixtures::load()?)?;
    let mut report = HistoryReport::default();
    for (table, sql, data) in [
        ("runs", RUNS_SQL, rows.runs),
        ("orders", ORDERS_SQL, rows.orders),
        ("order_lines", LINES_SQL, rows.lines),
    ] {
        let written = sqlx::query(sql)
            .bind(sqlx::types::Json(data))
            .execute(&mut *conn)
            .await?
            .rows_affected();
        report.written.push((table, written));
    }
    let events = sqlx::types::Json(rows.events);
    let mut written = 0;
    // Built from the constants above only.
    for sql in [stale_events_sql(), missing_events_sql()] {
        written += sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(&events)
            .execute(&mut *conn)
            .await?
            .rows_affected();
    }
    report.written.push(("order_events", written));
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Rows {
        plan(&Fixtures::load().unwrap()).unwrap()
    }

    fn int(v: &Value, key: &str) -> i64 {
        v[key].as_i64().unwrap_or_else(|| panic!("{key} in {v}"))
    }

    #[test]
    fn every_id_comes_from_the_fixtures() {
        let rows = rows();
        assert_eq!(rows.runs.len(), 15);
        assert_eq!(rows.orders.len(), 28);
        for o in &rows.orders {
            assert!(o["id"].as_str().unwrap().starts_with("ord_hist_"), "{o}");
        }
        // Ids that generated ids (12 characters of [0-9a-z]) can never take.
        for r in &rows.runs {
            let id = r["id"].as_str().unwrap();
            assert!(
                id.starts_with("run_hist_") && id.len() == "run_hist_01".len(),
                "{id}"
            );
        }
        let in_circle = |c: &str| rows.runs.iter().filter(|r| r["circle_id"] == c).count();
        assert_eq!((in_circle(HINTON4), in_circle(CLIMB)), (12, 3));
    }

    #[test]
    fn times_are_past_and_in_order() {
        let rows = rows();
        for r in &rows.runs {
            let leaves = int(r, "leaving_min");
            assert!(int(r, "created_min") < leaves);
            assert!(int(r, "window_closes_min") > leaves);
            // At least a day back, and less than 7 weeks: always inside the 8-week chart.
            assert!(leaves + 24 * 60 < 0 && leaves > -49 * 24 * 60, "{r}");
        }
        for o in &rows.orders {
            let run = rows.runs.iter().find(|r| r["id"] == o["run_id"]).unwrap();
            let (created, delivered) = (int(o, "created_min"), int(o, "delivered_min"));
            assert!(created > int(run, "created_min"), "{o}");
            assert!(created < int(run, "window_closes_min"), "{o}");
            assert!(delivered > created + 20, "{o}");
            assert!(delivered < 0, "{o}");
        }
        // Every order's timeline is in order and ends at its delivery (or just after it).
        for o in &rows.orders {
            let ts: Vec<i64> = rows
                .events
                .iter()
                .filter(|e| e["order_id"] == o["id"])
                .map(|e| int(e, "ts_min"))
                .collect();
            assert!(ts.windows(2).all(|w| w[0] < w[1]), "{o}");
            assert!(ts.contains(&int(o, "delivered_min")), "{o}");
        }
    }

    #[test]
    fn weekly_counts_grow() {
        let rows = rows();
        let mut weeks = [0; 7];
        for o in &rows.orders {
            let days_ago = (-int(o, "delivered_min")).div_euclid(24 * 60) + 1;
            weeks[(days_ago / 7).min(6) as usize] += 1;
        }
        weeks.reverse();
        assert_eq!(weeks, [2, 3, 3, 4, 4, 5, 7]);
    }

    #[test]
    fn money_adds_up() {
        let rows = rows();
        for o in &rows.orders {
            let lines: i64 = rows
                .lines
                .iter()
                .filter(|l| l["order_id"] == o["id"])
                .map(|l| {
                    assert_eq!(
                        int(l, "line_total_cents"),
                        int(l, "unit_price_cents") * int(l, "qty")
                    );
                    int(l, "line_total_cents")
                })
                .sum();
            assert_eq!(int(o, "subtotal_cents"), lines, "{o}");
            assert_eq!(int(o, "tax_cents"), tax_cents(lines));
            assert_eq!(int(o, "ferry_fee_cents"), 150);
            assert_eq!(
                int(o, "total_cents"),
                lines + int(o, "tax_cents") + 150 + int(o, "tip_cents")
            );
            assert!(o["handoff_code"].as_str().unwrap().len() == 4);
        }
        // The judge's first burrito: 899 + 0 for black beans, 7.5% tax is 67, tip 200.
        let first = &rows.orders[0];
        assert_eq!(first["id"], "ord_hist_01a");
        assert_eq!(
            (
                int(first, "subtotal_cents"),
                int(first, "tax_cents"),
                int(first, "tip_cents")
            ),
            (899, 67, 200)
        );
        assert_eq!(int(first, "total_cents"), 1316);
        assert_eq!(tax_cents(1000), 75);
        assert_eq!(tax_cents(1), 0);
        assert_eq!(tax_cents(7), 1);
    }

    #[test]
    fn carbon_comes_from_the_fixture_coordinates() {
        let rows = rows();
        let f = Fixtures::load().unwrap();
        for o in &rows.orders {
            let run = rows.runs.iter().find(|r| r["id"] == o["run_id"]).unwrap();
            let restaurant = f
                .restaurants
                .iter()
                .find(|r| r.id == run["restaurant_id"])
                .unwrap();
            let place = LatLng {
                lat: o["dropoff_lat"].as_f64().unwrap(),
                lng: o["dropoff_lng"].as_f64().unwrap(),
            };
            let (m, g) = fallback_carbon(restaurant.location, place);
            assert_eq!(o["car_distance_m"].as_f64().unwrap(), m);
            assert_eq!(o["co2_saved_g"].as_f64().unwrap(), g);
            assert!(g > 100.0 && g < 1500.0, "{o}");
        }
    }

    #[test]
    fn bad_lines_are_refused() {
        let f = Fixtures::load().unwrap();
        let cosmic = f
            .menus
            .iter()
            .find(|m| m.restaurant_id == "rst_cosmic")
            .unwrap();
        let ok = line(
            "itm_cosmic_chicken_burrito",
            &[
                "cho_cosmic_chicken_burrito_no_beans",
                "cho_cosmic_chicken_burrito_guac",
            ],
        );
        assert_eq!(
            price_line(cosmic, &ok).unwrap(),
            ("Chicken Burrito".to_owned(), 1049)
        );
        for bad in [
            // The beans option needs exactly one choice.
            line("itm_cosmic_chicken_burrito", &[]),
            line(
                "itm_cosmic_chicken_burrito",
                &[
                    "cho_cosmic_chicken_burrito_no_beans",
                    "cho_cosmic_chicken_burrito_black_beans",
                ],
            ),
            line("itm_cosmic_nachos", &["cho_cosmic_chicken_burrito_guac"]),
            line("itm_supdogs_tots", &[]),
        ] {
            assert!(price_line(cosmic, &bad).is_err(), "{}", bad.item);
        }
    }

    #[test]
    fn handoff_codes_are_four_digits_and_fixed() {
        assert_eq!(handoff_code("ord_hist_01a"), handoff_code("ord_hist_01a"));
        for id in ["ord_hist_01a", "ord_hist_15b", "x"] {
            let code = handoff_code(id);
            assert!(
                code.len() == 4 && code.bytes().all(|b| b.is_ascii_digit()),
                "{code}"
            );
        }
    }
}
