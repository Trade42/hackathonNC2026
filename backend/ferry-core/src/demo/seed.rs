//! `ferry-core seed`: loads every fixture in contract/fixtures into schema ferry, in one
//! transaction, idempotently.
//!
//! Each table is one upsert of the fixture rows as a JSON array (`jsonb_to_recordset`), so the
//! whole seed is a dozen round trips. A row that already matches its fixture is not written, so a
//! second seed changes only the demo run, which is timed from now on every seed.

use serde_json::{Value, json};
use sqlx::{AssertSqlSafe, PgConnection, PgPool};

use super::fixtures::{FixtureError, Fixtures};

/// The demo run leaves this many minutes after the seed. Its window closes as long after that as
/// the fixture's does (10 minutes in runs.json).
pub const DEMO_RUN_LEAVES_IN_MINUTES: i32 = 15;

/// The fictional numbers the personas get: +1 919 555 01xx, a range reserved for fiction. The
/// fixtures carry only masked phones, so the last four digits come from `phone_masked`.
pub const PERSONA_PHONE_PREFIX: &str = "+1919555";

#[derive(Debug, thiserror::Error)]
pub enum SeedError {
    #[error(transparent)]
    Fixture(#[from] FixtureError),
    #[error(
        "fixture user {user}: phone_masked must end in four digits 0100 to 0199 (the fictional 555-01xx range)"
    )]
    PersonaPhone { user: String },
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
}

/// Rows each upsert wrote (inserted or changed), in seed order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SeedReport {
    pub written: Vec<(&'static str, u64)>,
}

impl SeedReport {
    pub fn written(&self, table: &str) -> Option<u64> {
        self.written
            .iter()
            .find(|(name, _)| *name == table)
            .map(|(_, n)| *n)
    }
}

impl std::fmt::Display for SeedReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let parts: Vec<String> = self
            .written
            .iter()
            .map(|(table, n)| format!("{table} {n}"))
            .collect();
        write!(f, "{}", parts.join(", "))
    }
}

/// One table the seed upserts. `columns` lists every column it writes, with the Postgres type
/// `jsonb_to_recordset` reads it as; the JSON rows use the same names.
struct Upsert {
    table: &'static str,
    key: &'static [&'static str],
    columns: &'static [(&'static str, &'static str)],
}

impl Upsert {
    /// `INSERT ... SELECT ... FROM jsonb_to_recordset($1) ON CONFLICT (key) DO UPDATE`, updating a
    /// row only when a column differs, or `DO NOTHING` when every column is in the key. Built from
    /// the constants below only, never from input.
    fn sql(&self) -> String {
        let names: Vec<&str> = self.columns.iter().map(|(name, _)| *name).collect();
        let record: Vec<String> = self
            .columns
            .iter()
            .map(|(name, ty)| format!("{name} {ty}"))
            .collect();
        let rest: Vec<&str> = names
            .iter()
            .copied()
            .filter(|name| !self.key.contains(name))
            .collect();
        let mut sql = format!(
            "INSERT INTO ferry.{table} AS t ({cols}) SELECT {cols} \
             FROM jsonb_to_recordset($1::jsonb) AS r({record}) ON CONFLICT ({key}) ",
            table = self.table,
            cols = names.join(", "),
            record = record.join(", "),
            key = self.key.join(", "),
        );
        if rest.is_empty() {
            sql.push_str("DO NOTHING");
        } else {
            let set: Vec<String> = rest.iter().map(|c| format!("{c} = EXCLUDED.{c}")).collect();
            let old: Vec<String> = rest.iter().map(|c| format!("t.{c}")).collect();
            let new: Vec<String> = rest.iter().map(|c| format!("EXCLUDED.{c}")).collect();
            sql.push_str(&format!(
                "DO UPDATE SET {} WHERE ({}) IS DISTINCT FROM ({})",
                set.join(", "),
                old.join(", "),
                new.join(", ")
            ));
        }
        sql
    }

    async fn run(&self, conn: &mut PgConnection, rows: Vec<Value>) -> Result<u64, sqlx::Error> {
        let result = sqlx::query(AssertSqlSafe(self.sql()))
            .bind(sqlx::types::Json(rows))
            .execute(conn)
            .await?;
        Ok(result.rows_affected())
    }
}

const TEXT: &str = "text";
const TEXTS: &str = "text[]";
const FLOAT: &str = "double precision";
const BOOL: &str = "boolean";
const INT: &str = "integer";
const BIGINT: &str = "bigint";
const TIME: &str = "timestamptz";

const USERS: Upsert = Upsert {
    table: "users",
    key: &["id"],
    columns: &[
        ("id", TEXT),
        ("phone_e164", TEXT),
        ("display_name", TEXT),
        ("avatar_seed", TEXT),
        ("bubble_color", TEXT),
        ("onboarding_complete", BOOL),
    ],
};

const PLACES: Upsert = Upsert {
    table: "places",
    key: &["id"],
    columns: &[
        ("id", TEXT),
        ("label", TEXT),
        ("lat", FLOAT),
        ("lng", FLOAT),
        ("note", TEXT),
    ],
};

const PROFILES: Upsert = Upsert {
    table: "profiles",
    key: &["user_id"],
    columns: &[
        ("user_id", TEXT),
        ("dropoff_place_id", TEXT),
        ("dropoff_label", TEXT),
        ("dropoff_lat", FLOAT),
        ("dropoff_lng", FLOAT),
        ("dropoff_note", TEXT),
        ("diet", TEXTS),
        ("avoid", TEXTS),
        ("favorite_cuisines", TEXTS),
        ("favorite_restaurant_ids", TEXTS),
        ("usual_times", "jsonb"),
        ("walking_pace", TEXT),
        ("runner_opt_in", BOOL),
        ("default_tip_cents", BIGINT),
        ("notify_push", BOOL),
        ("notify_messages", BOOL),
        ("share_location_when_running", BOOL),
    ],
};

const CIRCLES: Upsert = Upsert {
    table: "circles",
    key: &["id"],
    columns: &[
        ("id", TEXT),
        ("name", TEXT),
        ("emoji", TEXT),
        ("last_met_at", TIME),
        ("last_active_at", TIME),
    ],
};

const CIRCLE_MEMBERS: Upsert = Upsert {
    table: "circle_members",
    key: &["circle_id", "user_id"],
    columns: &[("circle_id", TEXT), ("user_id", TEXT)],
};

const AREAS: Upsert = Upsert {
    table: "areas",
    key: &["id"],
    columns: &[
        ("id", TEXT),
        ("name", TEXT),
        ("center_lat", FLOAT),
        ("center_lng", FLOAT),
    ],
};

const RESTAURANTS: Upsert = Upsert {
    table: "restaurants",
    key: &["id"],
    columns: &[
        ("id", TEXT),
        ("name", TEXT),
        ("area_id", TEXT),
        ("cuisine", TEXTS),
        ("lat", FLOAT),
        ("lng", FLOAT),
        ("address", TEXT),
        ("is_open", BOOL),
        ("hours_today", TEXT),
        ("partner", BOOL),
        ("price_level", "smallint"),
        ("image_url", TEXT),
        ("prices_are_estimates", BOOL),
    ],
};

const MENU_CATEGORIES: Upsert = Upsert {
    table: "menu_categories",
    key: &["id"],
    columns: &[
        ("id", TEXT),
        ("restaurant_id", TEXT),
        ("name", TEXT),
        ("sort_order", INT),
    ],
};

const MENU_ITEMS: Upsert = Upsert {
    table: "menu_items",
    key: &["id"],
    columns: &[
        ("id", TEXT),
        ("restaurant_id", TEXT),
        ("category_id", TEXT),
        ("name", TEXT),
        ("description", TEXT),
        ("price_cents", BIGINT),
        ("tags", TEXTS),
        ("image_url", TEXT),
        ("sort_order", INT),
    ],
};

const ITEM_OPTIONS: Upsert = Upsert {
    table: "item_options",
    key: &["id"],
    columns: &[
        ("id", TEXT),
        ("menu_item_id", TEXT),
        ("name", TEXT),
        ("min_choices", INT),
        ("max_choices", INT),
        ("sort_order", INT),
    ],
};

const ITEM_CHOICES: Upsert = Upsert {
    table: "item_choices",
    key: &["id"],
    columns: &[
        ("id", TEXT),
        ("option_id", TEXT),
        ("name", TEXT),
        ("price_delta_cents", BIGINT),
        ("sort_order", INT),
    ],
};

/// Every fixture run, open and timed from now: the earliest fixture `leaving_at` becomes now plus
/// `$2` minutes and every other time keeps its distance from it. A seeded run is never demo
/// history, and its receipt is cleared.
const RUNS_SQL: &str = "\
WITH r AS (
    SELECT * FROM jsonb_to_recordset($1::jsonb) AS r(
        id text, restaurant_id text, runner_id text, circle_id text, status text,
        created_via text, leaving_at timestamptz, window_closes_at timestamptz)
), anchor AS (
    SELECT min(leaving_at) AS at FROM r
)
INSERT INTO ferry.runs AS t (id, restaurant_id, runner_id, circle_id, status, created_via,
                             leaving_at, window_closes_at, receipt_total_cents, receipt_path,
                             is_demo_history)
SELECT r.id, r.restaurant_id, r.runner_id, r.circle_id, r.status, r.created_via,
       now() + make_interval(mins => $2) + (r.leaving_at - anchor.at),
       now() + make_interval(mins => $2) + (r.window_closes_at - anchor.at),
       NULL, NULL, false
FROM r CROSS JOIN anchor
ON CONFLICT (id) DO UPDATE SET
    restaurant_id = EXCLUDED.restaurant_id, runner_id = EXCLUDED.runner_id,
    circle_id = EXCLUDED.circle_id, status = EXCLUDED.status, created_via = EXCLUDED.created_via,
    leaving_at = EXCLUDED.leaving_at, window_closes_at = EXCLUDED.window_closes_at,
    receipt_total_cents = NULL, receipt_path = NULL, is_demo_history = false";

/// The persona's phone: [`PERSONA_PHONE_PREFIX`] plus the last four digits of `phone_masked`,
/// which must be 0100 to 0199 (fixtures: +19195550101 to +19195550104).
pub fn persona_phone(user_id: &str, phone_masked: &str) -> Result<String, SeedError> {
    let digits: Vec<char> = phone_masked.chars().filter(char::is_ascii_digit).collect();
    let last4: String = digits[digits.len().saturating_sub(4)..].iter().collect();
    if last4.len() == 4 && last4.starts_with("01") {
        Ok(format!("{PERSONA_PHONE_PREFIX}{last4}"))
    } else {
        Err(SeedError::PersonaPhone {
            user: user_id.to_owned(),
        })
    }
}

/// Seeds every fixture in one transaction.
pub async fn seed_pool(pool: &PgPool) -> Result<SeedReport, SeedError> {
    let mut tx = pool.begin().await?;
    let report = seed(&mut tx).await?;
    tx.commit().await?;
    Ok(report)
}

/// Seeds every fixture on `conn`, which the caller has inside a transaction. Parents go before
/// the rows that reference them.
pub async fn seed(conn: &mut PgConnection) -> Result<SeedReport, SeedError> {
    let f = Fixtures::load()?;
    let mut report = SeedReport::default();

    let mut users = Vec::new();
    let mut profiles = Vec::new();
    for u in &f.users {
        users.push(json!({
            "id": u.id,
            "phone_e164": persona_phone(&u.id, &u.phone_masked)?,
            "display_name": u.display_name,
            "avatar_seed": u.avatar_seed,
            "bubble_color": u.bubble_color,
            "onboarding_complete": u.onboarding_complete,
        }));
        let p = &u.profile;
        let dropoff = p.dropoff_default.as_ref();
        profiles.push(json!({
            "user_id": u.id,
            "dropoff_place_id": dropoff.and_then(|d| d.id.as_deref()),
            "dropoff_label": dropoff.map(|d| &d.label),
            "dropoff_lat": dropoff.map(|d| d.location.lat),
            "dropoff_lng": dropoff.map(|d| d.location.lng),
            "dropoff_note": dropoff.and_then(|d| d.note.as_deref()),
            "diet": p.diet,
            "avoid": p.avoid,
            "favorite_cuisines": p.favorite_cuisines,
            "favorite_restaurant_ids": p.favorite_restaurant_ids,
            "usual_times": p.usual_times,
            "walking_pace": p.walking_pace,
            "runner_opt_in": p.runner_opt_in,
            "default_tip_cents": p.default_tip_cents,
            "notify_push": p.notify_push,
            "notify_messages": p.notify_messages,
            "share_location_when_running": p.share_location_when_running,
        }));
    }
    let places = f
        .places
        .iter()
        .filter_map(|p| {
            // Every place in places.json has an id; one without could not be stored by id.
            Some(json!({
                "id": p.id.as_deref()?,
                "label": p.label,
                "lat": p.location.lat,
                "lng": p.location.lng,
                "note": p.note,
            }))
        })
        .collect();
    let circles = f
        .circles
        .iter()
        .map(|c| {
            json!({
                "id": c.id,
                "name": c.name,
                "emoji": c.emoji,
                "last_met_at": c.last_met_at,
                "last_active_at": c.last_active_at,
            })
        })
        .collect();
    let members = f
        .circles
        .iter()
        .flat_map(|c| {
            c.members
                .iter()
                .map(move |m| json!({"circle_id": c.id, "user_id": m.id}))
        })
        .collect();
    let areas = f
        .areas
        .iter()
        .map(|a| {
            json!({
                "id": a.id,
                "name": a.name,
                "center_lat": a.center.lat,
                "center_lng": a.center.lng,
            })
        })
        .collect();
    let restaurants = f
        .restaurants
        .iter()
        .map(|r| {
            // Menu.prices_are_estimates lives on the restaurant; without a menu, the column's
            // default (true) applies.
            let estimates = f
                .menus
                .iter()
                .find(|m| m.restaurant_id == r.id)
                .is_none_or(|m| m.prices_are_estimates);
            json!({
                "id": r.id,
                "name": r.name,
                "area_id": r.area_id,
                "cuisine": r.cuisine,
                "lat": r.location.lat,
                "lng": r.location.lng,
                "address": r.address,
                "is_open": r.is_open,
                "hours_today": r.hours_today,
                "partner": r.partner,
                "price_level": r.price_level,
                "image_url": r.image_url,
                "prices_are_estimates": estimates,
            })
        })
        .collect();
    let (mut categories, mut items, mut options, mut choices) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for menu in &f.menus {
        for (ci, category) in menu.categories.iter().enumerate() {
            categories.push(json!({
                "id": category.id,
                "restaurant_id": menu.restaurant_id,
                "name": category.name,
                "sort_order": ci,
            }));
            for (ii, item) in category.items.iter().enumerate() {
                items.push(json!({
                    "id": item.id,
                    "restaurant_id": menu.restaurant_id,
                    "category_id": category.id,
                    "name": item.name,
                    "description": item.description,
                    "price_cents": item.price_cents,
                    "tags": item.tags,
                    "image_url": item.image_url,
                    "sort_order": ii,
                }));
                for (oi, option) in item.options.iter().enumerate() {
                    options.push(json!({
                        "id": option.id,
                        "menu_item_id": item.id,
                        "name": option.name,
                        "min_choices": option.min,
                        "max_choices": option.max,
                        "sort_order": oi,
                    }));
                    for (hi, choice) in option.choices.iter().enumerate() {
                        choices.push(json!({
                            "id": choice.id,
                            "option_id": option.id,
                            "name": choice.name,
                            "price_delta_cents": choice.price_delta_cents,
                            "sort_order": hi,
                        }));
                    }
                }
            }
        }
    }

    for (spec, rows) in [
        (&USERS, users),
        (&PLACES, places),
        (&PROFILES, profiles),
        (&CIRCLES, circles),
        (&CIRCLE_MEMBERS, members),
        (&AREAS, areas),
        (&RESTAURANTS, restaurants),
        (&MENU_CATEGORIES, categories),
        (&MENU_ITEMS, items),
        (&ITEM_OPTIONS, options),
        (&ITEM_CHOICES, choices),
    ] {
        let written = spec.run(conn, rows).await?;
        report.written.push((spec.table, written));
    }

    let runs: Vec<Value> = f
        .runs
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "restaurant_id": r.restaurant.id,
                "runner_id": r.runner.id,
                "circle_id": r.circle_id,
                "status": r.status,
                "created_via": r.created_via,
                "leaving_at": r.leaving_at,
                "window_closes_at": r.window_closes_at,
            })
        })
        .collect();
    let written = sqlx::query(RUNS_SQL)
        .bind(sqlx::types::Json(runs))
        .bind(DEMO_RUN_LEAVES_IN_MINUTES)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    report.written.push(("runs", written));
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persona_phones_follow_the_masked_digits() {
        assert_eq!(
            persona_phone("usr_judge", "+1 ••• ••• 0101").unwrap(),
            "+19195550101"
        );
        assert_eq!(
            persona_phone("usr_leo", "+1 ••• ••• 0104").unwrap(),
            "+19195550104"
        );
        for masked in ["+1 ••• ••• 1234", "+1 ••• ••• 01", "", "••••"]
        {
            assert!(persona_phone("usr_x", masked).is_err(), "{masked:?}");
        }
    }

    #[test]
    fn every_fixture_user_gets_a_distinct_fictional_phone() {
        let f = Fixtures::load().unwrap();
        let mut phones: Vec<String> = f
            .users
            .iter()
            .map(|u| persona_phone(&u.id, &u.phone_masked).unwrap())
            .collect();
        phones.sort();
        assert_eq!(
            phones,
            [
                "+19195550101",
                "+19195550102",
                "+19195550103",
                "+19195550104"
            ]
        );
    }

    #[test]
    fn upsert_sql_updates_only_changed_rows() {
        assert_eq!(
            AREAS.sql(),
            "INSERT INTO ferry.areas AS t (id, name, center_lat, center_lng) \
             SELECT id, name, center_lat, center_lng \
             FROM jsonb_to_recordset($1::jsonb) AS r(id text, name text, \
             center_lat double precision, center_lng double precision) ON CONFLICT (id) \
             DO UPDATE SET name = EXCLUDED.name, center_lat = EXCLUDED.center_lat, \
             center_lng = EXCLUDED.center_lng \
             WHERE (t.name, t.center_lat, t.center_lng) IS DISTINCT FROM \
             (EXCLUDED.name, EXCLUDED.center_lat, EXCLUDED.center_lng)"
        );
        assert!(
            CIRCLE_MEMBERS
                .sql()
                .ends_with("ON CONFLICT (circle_id, user_id) DO NOTHING")
        );
    }

    #[test]
    fn every_upsert_names_its_key_columns() {
        for spec in [
            &USERS,
            &PLACES,
            &PROFILES,
            &CIRCLES,
            &CIRCLE_MEMBERS,
            &AREAS,
            &RESTAURANTS,
            &MENU_CATEGORIES,
            &MENU_ITEMS,
            &ITEM_OPTIONS,
            &ITEM_CHOICES,
        ] {
            for key in spec.key {
                assert!(
                    spec.columns.iter().any(|(name, _)| name == key),
                    "{}: {key}",
                    spec.table
                );
            }
        }
    }
}
