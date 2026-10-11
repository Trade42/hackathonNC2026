//! The catalog's contract types (contract/openapi.yaml): LatLng, Place, Area, Restaurant and Menu
//! with its categories, items, options and choices. The demo seeder reads the fixtures into the
//! same types, so a field added to the contract shows up in both places.

use serde::{Deserialize, Serialize};

/// `LatLng`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LatLng {
    pub lat: f64,
    pub lng: f64,
}

/// `Place`: a named drop-off spot, or a map pin without an id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Place {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub label: String,
    pub location: LatLng,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// `Area`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Area {
    pub id: String,
    pub name: String,
    pub center: LatLng,
}

/// `Restaurant`. `is_open` and `walk_minutes_from_dropoff` are computed per request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Restaurant {
    pub id: String,
    pub name: String,
    pub area_id: String,
    pub cuisine: Vec<String>,
    pub location: LatLng,
    pub address: String,
    pub is_open: bool,
    pub hours_today: String,
    pub partner: bool,
    /// `PriceLevel`: 1, 2 or 3.
    pub price_level: i16,
    /// Rounded up to whole minutes, so always an integer on the wire.
    pub walk_minutes_from_dropoff: Option<u32>,
    pub image_url: Option<String>,
}

/// `MenuChoice`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MenuChoice {
    pub id: String,
    pub name: String,
    pub price_delta_cents: i64,
}

/// `MenuOption`: pick between `min` and `max` of `choices`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MenuOption {
    pub id: String,
    pub name: String,
    pub min: i32,
    pub max: i32,
    pub choices: Vec<MenuChoice>,
}

/// `MenuItem`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MenuItem {
    pub id: String,
    pub name: String,
    pub description: String,
    pub price_cents: i64,
    pub tags: Vec<String>,
    pub image_url: Option<String>,
    pub options: Vec<MenuOption>,
}

/// `MenuCategory`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MenuCategory {
    pub id: String,
    pub name: String,
    pub items: Vec<MenuItem>,
}

/// `Menu`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Menu {
    pub restaurant_id: String,
    pub prices_are_estimates: bool,
    pub categories: Vec<MenuCategory>,
}
