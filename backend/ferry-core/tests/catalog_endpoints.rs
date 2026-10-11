//! The catalog endpoints against the seeded fixtures: areas, restaurants with their filters,
//! walk minutes and images, menus, places and the public config. Every 200 body is checked
//! against contract/openapi.yaml.
mod catalog_support;
mod common;

use axum::http::StatusCode;
use catalog_support::{assert_error, assert_get_200, by_id, fixture, get, seed, state};
use serde_json::{Value, json};
use sqlx::PgPool;

/// A signed-in user with no profile, so no drop-off and no walk minutes.
const NO_PROFILE: Option<&str> = Some("usr_no_profile");

const DEMO: &[(&str, &str)] = &[("DEMO_MODE", "true")];

#[sqlx::test(migrations = "../migrations")]
async fn areas_equal_the_fixtures(pool: PgPool) {
    seed(&pool).await;
    let state = state(pool, DEMO);
    let (status, body) = get(&state, "/v1/areas", NO_PROFILE).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_get_200("/v1/areas", &body);
    assert_eq!(by_id(&body), by_id(&fixture("areas")));
    // By name, a stable order.
    let names: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Campus", "Carrboro", "Franklin St"]);
}

#[sqlx::test(migrations = "../migrations")]
async fn places_equal_the_fixtures_and_every_one_has_an_id(pool: PgPool) {
    seed(&pool).await;
    let state = state(pool, DEMO);
    let (status, body) = get(&state, "/v1/places", NO_PROFILE).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_get_200("/v1/places", &body);
    assert_eq!(by_id(&body), by_id(&fixture("places")));
    let labels: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["label"].as_str().unwrap())
        .collect();
    let mut sorted = labels.clone();
    sorted.sort();
    assert_eq!(labels, sorted, "places come by label");
}

#[sqlx::test(migrations = "../migrations")]
async fn places_added_later_are_listed_too(pool: PgPool) {
    seed(&pool).await;
    sqlx::query(
        "INSERT INTO ferry.places (id, label, lat, lng) VALUES ('place_union', 'Student Union', 35.9104, -79.0475)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = state(pool, DEMO);
    let (_, body) = get(&state, "/v1/places", NO_PROFILE).await;
    assert_get_200("/v1/places", &body);
    let places = by_id(&body);
    assert_eq!(
        places.len(),
        fixture("places").as_array().unwrap().len() + 1
    );
    // No note: the field is left out, as Place's optional fields are.
    assert_eq!(
        places["place_union"],
        json!({"id": "place_union", "label": "Student Union", "location": {"lat": 35.9104, "lng": -79.0475}})
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn restaurants_equal_the_fixtures(pool: PgPool) {
    seed(&pool).await;
    let state = state(pool, DEMO);
    let (status, body) = get(&state, "/v1/restaurants", NO_PROFILE).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_get_200("/v1/restaurants", &body);
    // DEMO_MODE opens every restaurant, as the fixtures have it; no drop-off, no walk minutes.
    assert_eq!(by_id(&body), by_id(&fixture("restaurants")));
}

/// The ids `GET uri` returns, in order.
async fn ids(state: &ferry_core::state::AppState, uri: &str) -> Vec<String> {
    let (status, body) = get(state, uri, NO_PROFILE).await;
    assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    assert_get_200("/v1/restaurants", &body);
    body.as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_owned())
        .collect()
}

#[sqlx::test(migrations = "../migrations")]
async fn restaurant_area_and_search_filters(pool: PgPool) {
    seed(&pool).await;
    let state = state(pool, DEMO);
    let fixtures = fixture("restaurants");
    let in_area = |area: &str| {
        let mut ids: Vec<String> = fixtures
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["area_id"] == area)
            .map(|r| r["id"].as_str().unwrap().to_owned())
            .collect();
        ids.sort();
        ids
    };
    for area in ["area_franklin", "area_campus", "area_carrboro"] {
        let mut got = ids(&state, &format!("/v1/restaurants?area_id={area}")).await;
        got.sort();
        assert_eq!(got, in_area(area), "{area}");
    }
    assert!(
        ids(&state, "/v1/restaurants?area_id=area_nowhere")
            .await
            .is_empty()
    );

    // Names and cuisines, any case, any part of the word.
    for (q, expected) in [
        ("cosmic", vec!["rst_cosmic"]),
        ("COSMIC%20cantina", vec!["rst_cosmic"]),
        ("Viet", vec!["rst_limebasil"]),
        ("dog", vec!["rst_supdogs"]),
        ("BAGEL", vec!["rst_alpine"]),
        ("hot%20bar", vec!["rst_weaver"]),
        ("middle%20eastern", vec!["rst_meddeli"]),
        ("sushi", vec![]),
        // LIKE wildcards are plain characters.
        ("%25", vec![]),
        ("_", vec![]),
    ] {
        let got = ids(&state, &format!("/v1/restaurants?q={q}")).await;
        assert_eq!(got, expected, "q={q}");
    }
    // An empty or blank q is no filter.
    assert_eq!(
        ids(&state, "/v1/restaurants?q=").await.len(),
        fixtures.as_array().unwrap().len()
    );
    assert_eq!(
        ids(&state, "/v1/restaurants?q=%20%20").await.len(),
        fixtures.as_array().unwrap().len()
    );
    // Filters combine.
    assert_eq!(
        ids(&state, "/v1/restaurants?area_id=area_franklin&q=american").await,
        ["rst_supdogs"]
    );
    assert!(
        ids(&state, "/v1/restaurants?area_id=area_campus&q=american")
            .await
            .is_empty()
    );
    // A bad boolean is the contract's 400.
    let (status, body) = get(&state, "/v1/restaurants?open_now=maybe", NO_PROFILE).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_error(&body, "validation_failed");
}

/// Minutes since midnight in America/New_York, as the database sees it.
async fn new_york_minutes(pool: &PgPool) -> i32 {
    sqlx::query_scalar(
        "SELECT (extract(hour FROM t) * 60 + extract(minute FROM t))::int \
         FROM (SELECT now() AT TIME ZONE 'America/New_York' AS t) AS local",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

fn hhmm(minutes: i32) -> String {
    let m = minutes.rem_euclid(24 * 60);
    format!("{:02}:{:02}", m / 60, m % 60)
}

#[sqlx::test(migrations = "../migrations")]
async fn open_now_follows_the_hours_in_new_york(pool: PgPool) {
    seed(&pool).await;
    let now = new_york_minutes(&pool).await;
    // Open for the hour around now in New York; in UTC, 4 or 5 hours away, it would be closed.
    let open = format!("{}–{}", hhmm(now - 30), hhmm(now + 30));
    // Opens in an hour.
    let closed = format!("{}–{}", hhmm(now + 60), hhmm(now + 120));
    for (id, hours, stored) in [
        ("rst_cosmic", open.as_str(), false),
        ("rst_supdogs", closed.as_str(), true),
        // Unreadable hours fall back to the stored flag.
        ("rst_limebasil", "varies", false),
        ("rst_meddeli", "varies", true),
    ] {
        sqlx::query("UPDATE ferry.restaurants SET hours_today = $2, is_open = $3 WHERE id = $1")
            .bind(id)
            .bind(hours)
            .bind(stored)
            .execute(&pool)
            .await
            .unwrap();
    }

    let live = state(pool.clone(), &[]);
    let (status, all) = get(&live, "/v1/restaurants", NO_PROFILE).await;
    assert_eq!(status, StatusCode::OK, "{all}");
    assert_get_200("/v1/restaurants", &all);
    let all = by_id(&all);
    assert_eq!(all["rst_cosmic"]["is_open"], true, "{open}");
    assert_eq!(all["rst_supdogs"]["is_open"], false, "{closed}");
    assert_eq!(all["rst_limebasil"]["is_open"], false);
    assert_eq!(all["rst_meddeli"]["is_open"], true);
    assert_eq!(all["rst_cosmic"]["hours_today"], open.as_str());

    let open_ids: Vec<String> = all
        .values()
        .filter(|r| r["is_open"] == true)
        .map(|r| r["id"].as_str().unwrap().to_owned())
        .collect();
    let mut got = ids(&live, "/v1/restaurants?open_now=true").await;
    got.sort();
    assert_eq!(got, open_ids);
    assert!(got.contains(&"rst_cosmic".to_owned()));
    assert!(!got.contains(&"rst_supdogs".to_owned()));
    // open_now=false is no filter.
    assert_eq!(
        ids(&live, "/v1/restaurants?open_now=false").await.len(),
        all.len()
    );

    // DEMO_MODE: everything is open, whatever the hours say.
    let demo = state(pool, DEMO);
    assert_eq!(
        ids(&demo, "/v1/restaurants?open_now=true").await.len(),
        all.len()
    );
}

/// Straight-line meters, computed here independently of the server's code.
fn haversine(a: &Value, b: &Value) -> f64 {
    let rad = |v: &Value| v.as_f64().unwrap().to_radians();
    let (lat1, lat2) = (rad(&a["lat"]), rad(&b["lat"]));
    let dlat = lat2 - lat1;
    let dlng = rad(&b["lng"]) - rad(&a["lng"]);
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlng / 2.0).sin().powi(2);
    2.0 * 6_371_008.8 * h.sqrt().asin()
}

#[sqlx::test(migrations = "../migrations")]
async fn walk_minutes_from_each_personas_dropoff(pool: PgPool) {
    seed(&pool).await;
    let state = state(pool, DEMO);
    let users = fixture("users");
    for user in users.as_array().unwrap() {
        let id = user["id"].as_str().unwrap();
        let dropoff = &user["profile"]["dropoff_default"]["location"];
        let speed = match user["profile"]["walking_pace"].as_str().unwrap() {
            "relaxed" => 1.2,
            "normal" => 1.4,
            "brisk" => 1.6,
            other => panic!("pace {other}"),
        };
        let (status, body) = get(&state, "/v1/restaurants", Some(id)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_get_200("/v1/restaurants", &body);
        for r in body.as_array().unwrap() {
            let expected = (haversine(dropoff, &r["location"]) * 1.3 / speed / 60.0).ceil();
            assert_eq!(
                r["walk_minutes_from_dropoff"],
                json!(expected as u64),
                "{id} to {}",
                r["id"]
            );
        }
    }

    // place_hinton to Cosmic Cantina, about 2.1 km on foot, at the three paces.
    for (user, pace, minutes) in [
        ("usr_maya", "brisk", 22),
        ("usr_dana", "normal", 25),
        ("usr_leo", "relaxed", 30),
    ] {
        let (_, body) = get(&state, "/v1/restaurants?q=cosmic", Some(user)).await;
        assert_eq!(
            body[0]["walk_minutes_from_dropoff"],
            json!(minutes),
            "{user} ({pace}) from place_hinton"
        );
    }

    // A profile without a drop-off has no walk minutes.
    sqlx::query(
        "UPDATE ferry.profiles SET dropoff_place_id = NULL, dropoff_label = NULL, \
         dropoff_lat = NULL, dropoff_lng = NULL, dropoff_note = NULL WHERE user_id = 'usr_leo'",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    let (_, body) = get(&state, "/v1/restaurants", Some("usr_leo")).await;
    assert_get_200("/v1/restaurants", &body);
    assert!(
        body.as_array()
            .unwrap()
            .iter()
            .all(|r| r["walk_minutes_from_dropoff"].is_null())
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn menus_equal_the_fixtures(pool: PgPool) {
    seed(&pool).await;
    let state = state(pool, DEMO);
    let menus = fixture("menus");
    assert_eq!(
        menus.as_array().unwrap().len(),
        fixture("restaurants").as_array().unwrap().len()
    );
    for menu in menus.as_array().unwrap() {
        let id = menu["restaurant_id"].as_str().unwrap();
        let path = format!("/v1/restaurants/{id}/menu");
        let (status, body) = get(&state, &path, NO_PROFILE).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_get_200("/v1/restaurants/{id}/menu", &body);
        // Equal as JSON, so categories, items, options and choices come in the fixture's order.
        assert_eq!(&body, menu, "{id}");
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn unknown_restaurant_menu_is_not_found(pool: PgPool) {
    seed(&pool).await;
    let state = state(pool, DEMO);
    let (status, body) = get(&state, "/v1/restaurants/rst_nope/menu", NO_PROFILE).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_error(&body, "not_found");
}

#[sqlx::test(migrations = "../migrations")]
async fn a_restaurant_without_a_menu_has_no_categories(pool: PgPool) {
    seed(&pool).await;
    sqlx::query(
        "INSERT INTO ferry.restaurants (id, name, area_id, lat, lng, address, hours_today, price_level, prices_are_estimates) \
         VALUES ('rst_empty', 'Empty', 'area_campus', 35.91, -79.05, '1 South Rd', '09:00–17:00', 1, false)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = state(pool, DEMO);
    let (status, body) = get(&state, "/v1/restaurants/rst_empty/menu", NO_PROFILE).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_get_200("/v1/restaurants/{id}/menu", &body);
    assert_eq!(
        body,
        json!({"restaurant_id": "rst_empty", "prices_are_estimates": false, "categories": []})
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn image_urls_are_absolute_or_null(pool: PgPool) {
    seed(&pool).await;
    for sql in [
        "UPDATE ferry.restaurants SET image_url = 'restaurants/rst_cosmic.jpg' WHERE id = 'rst_cosmic'",
        "UPDATE ferry.restaurants SET image_url = 'https://img.example/sup.jpg' WHERE id = 'rst_supdogs'",
        "UPDATE ferry.menu_items SET image_url = 'menu/burrito.jpg' WHERE id = 'itm_cosmic_chicken_burrito'",
    ] {
        sqlx::query(sql).execute(&pool).await.unwrap();
    }
    let media = "https://abc.supabase.co/storage/v1/object/public/media";

    let with_supabase = state(
        pool.clone(),
        &[
            ("DEMO_MODE", "true"),
            ("SUPABASE_URL", "https://abc.supabase.co"),
        ],
    );
    let (_, body) = get(&with_supabase, "/v1/restaurants", NO_PROFILE).await;
    assert_get_200("/v1/restaurants", &body);
    let restaurants = by_id(&body);
    assert_eq!(
        restaurants["rst_cosmic"]["image_url"],
        format!("{media}/restaurants/rst_cosmic.jpg")
    );
    assert_eq!(
        restaurants["rst_supdogs"]["image_url"],
        "https://img.example/sup.jpg"
    );
    assert!(restaurants["rst_weaver"]["image_url"].is_null());
    let (_, menu) = get(
        &with_supabase,
        "/v1/restaurants/rst_cosmic/menu",
        NO_PROFILE,
    )
    .await;
    assert_get_200("/v1/restaurants/{id}/menu", &menu);
    assert_eq!(
        menu["categories"][0]["items"][0]["image_url"],
        format!("{media}/menu/burrito.jpg")
    );

    // Without SUPABASE_URL a stored path cannot become an absolute URL, so it is null.
    let without = state(pool, DEMO);
    let (_, body) = get(&without, "/v1/restaurants", NO_PROFILE).await;
    assert_get_200("/v1/restaurants", &body);
    let restaurants = by_id(&body);
    assert!(restaurants["rst_cosmic"]["image_url"].is_null());
    assert_eq!(
        restaurants["rst_supdogs"]["image_url"],
        "https://img.example/sup.jpg"
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn catalog_routes_need_a_token(pool: PgPool) {
    seed(&pool).await;
    let state = state(pool, DEMO);
    for uri in [
        "/v1/areas",
        "/v1/restaurants",
        "/v1/restaurants/rst_cosmic/menu",
        "/v1/places",
    ] {
        let (status, body) = get(&state, uri, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_error(&body, "unauthorized");
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn public_config_without_a_token(pool: PgPool) {
    // Nothing set: the dev web URL, no agent number, no Supabase.
    let bare = state(pool.clone(), &[]);
    let (status, body) = get(&bare, "/v1/public/config", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_get_200("/v1/public/config", &body);
    assert_eq!(
        body,
        json!({
            "web_url": "http://localhost:5173",
            "demo_mode": false,
            "agent": {
                "imessage_phone_e164": null,
                "display": "Ferry",
                "telegram_url": null,
                "telegram_add_to_group_url": null,
                "whatsapp_url": null
            },
            "downloads": {"macos": null, "android": null, "ios": null},
            "supabase": null
        })
    );

    let configured = state(
        pool,
        &[
            ("PUBLIC_WEB_URL", "https://ferry-hacknc.vercel.app"),
            ("DEMO_MODE", "true"),
            ("AGENT_PHONE_E164", "+19195550100"),
            ("AGENT_PHONE_DISPLAY", "(919) 555-0100"),
            ("SUPABASE_URL", "https://abc.supabase.co"),
            ("SUPABASE_PUBLISHABLE_KEY", "sb_publishable_for_clients"),
            ("SUPABASE_SECRET_KEY", "sb_secret_server_only"),
            ("TELEGRAM_BOT_USERNAME", "Ferrygo_bot"),
        ],
    );
    let (status, body) = get(&configured, "/v1/public/config", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_get_200("/v1/public/config", &body);
    assert_eq!(body["web_url"], "https://ferry-hacknc.vercel.app");
    assert_eq!(body["demo_mode"], true);
    // iMessage is off (contract v1.3): no number even when AGENT_PHONE_E164 is set.
    assert_eq!(body["agent"]["imessage_phone_e164"], json!(null));
    assert_eq!(body["agent"]["display"], "Ferry");
    assert_eq!(body["agent"]["telegram_url"], "https://t.me/Ferrygo_bot");
    assert_eq!(
        body["agent"]["telegram_add_to_group_url"],
        "https://t.me/Ferrygo_bot?startgroup=ferry"
    );
    assert_eq!(
        body["supabase"],
        json!({"url": "https://abc.supabase.co", "publishable_key": "sb_publishable_for_clients"})
    );
    assert!(!body.to_string().contains("sb_secret"), "{body}");
}

/// The contract check bites: a missing field, a relative image URL, a wrong price level and a
/// null agent display each fail it.
#[test]
fn the_contract_check_rejects_wrong_shapes() {
    let restaurant = fixture("restaurants")[0].clone();
    let mut missing = restaurant.clone();
    missing
        .as_object_mut()
        .unwrap()
        .remove("walk_minutes_from_dropoff");
    let mut relative = restaurant.clone();
    relative["image_url"] = json!("restaurants/rst_cosmic.jpg");
    let mut level = restaurant;
    level["price_level"] = json!(4);
    for bad in [missing, relative, level] {
        let body = json!([bad]);
        let failed = std::panic::catch_unwind(|| assert_get_200("/v1/restaurants", &body)).is_err();
        assert!(failed, "accepted {body}");
    }
    let config = json!({
        "web_url": "http://localhost:5173",
        "demo_mode": false,
        "agent": {"imessage_phone_e164": null, "display": null, "telegram_url": null, "whatsapp_url": null},
        "downloads": {"macos": null, "android": null, "ios": null},
        "supabase": null
    });
    assert!(std::panic::catch_unwind(|| assert_get_200("/v1/public/config", &config)).is_err());
}
