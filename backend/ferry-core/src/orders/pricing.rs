//! Pricing an order body (`CreateOrderRequest`) from the run's restaurant's menu, exactly as
//! `POST /v1/runs/{id}/orders` and `POST /v1/runs/{id}/quote` both do.
//!
//! Each line's `unit_price_cents` is the item's price plus its choices' deltas
//! (`payments::math::unit_price_cents`), and the totals come from `payments::math::quote_with`
//! (tax at `TAX_RATE_BPS`, the `FERRY_FEE_CENTS` fee, the tip). Every problem is 400
//! `validation_failed`, with `details` listing each one as `{field, message}`:
//! - 1 to [`MAX_LINES`] lines, each with `qty` 1 to [`MAX_QTY`];
//! - the item is on this restaurant's menu;
//! - each choice is one of the item's, listed once, and each option gets between its `min` and
//!   `max` choices;
//! - the choices do not bring the unit price below zero;
//! - a note is at most 280 characters with no control characters (stored trimmed, empty as none);
//! - `tip_cents` from 0 to [`MAX_TIP_CENTS`];
//! - the drop-off has a label (1 to 100 characters), valid coordinates, a note of at most 280
//!   characters, and an `id`, when given, of a place in `GET /v1/places`;
//! - `dropoff_radius_m` (contract v1.4), when given, is 50 to 5000 meters. It is not compared with
//!   the run's drop-off zone: the client warns when the drop-off is outside it.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::json;
use sqlx::PgPool;

use super::view::OrderLine;
use crate::catalog::types::Place;
use crate::config::Config;
use crate::error::{ApiError, ApiResult};
use crate::payments::math::{self, OrderQuote, PricedLine};
use crate::profile::validate::{self, FieldError};
use crate::tracking;

pub const MAX_LINES: usize = 20;
pub const MAX_QTY: i64 = 20;
/// $100, as the profile's `default_tip_cents`.
pub const MAX_TIP_CENTS: i64 = 10_000;
pub const MAX_LABEL_CHARS: usize = 100;
pub const MAX_NOTE_CHARS: usize = 280;

/// `OrderLineInput` in contract/openapi.yaml.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OrderLineInput {
    pub menu_item_id: String,
    pub qty: i64,
    pub choice_ids: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
}

/// `CreateOrderRequest` in contract/openapi.yaml.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CreateOrderRequest {
    pub lines: Vec<OrderLineInput>,
    pub tip_cents: i64,
    pub dropoff: Place,
    /// Contract v1.4, optional: how far from the drop-off the customer will walk to meet.
    #[serde(default)]
    pub dropoff_radius_m: Option<i64>,
}

/// What placing the body would store: the priced lines, the totals, the cleaned drop-off and the
/// checked drop-off radius.
#[derive(Debug, Clone, PartialEq)]
pub struct PricedOrder {
    pub lines: Vec<OrderLine>,
    pub quote: OrderQuote,
    pub dropoff: Place,
    pub dropoff_radius_m: Option<i32>,
}

/// One option of a menu item: pick `min` to `max` of its choices.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuOption {
    pub id: String,
    pub name: String,
    pub min: i32,
    pub max: i32,
}

/// A menu item, its options and its choices (choice id to its option's index and price delta).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MenuItem {
    pub name: String,
    pub price_cents: i64,
    pub options: Vec<MenuOption>,
    pub choices: HashMap<String, (usize, i64)>,
}

/// The items of one restaurant that a body names.
pub type Menu = HashMap<String, MenuItem>;

fn push(errors: &mut Vec<FieldError>, field: impl Into<String>, message: impl Into<String>) {
    errors.push(FieldError {
        field: field.into(),
        message: message.into(),
    });
}

/// The 400 for `errors`: the first problem as the message, all of them as `details`.
pub fn invalid(errors: &[FieldError]) -> ApiError {
    let message = errors
        .first()
        .map_or("The order is not valid.".to_owned(), |e| {
            format!("{}: {}.", e.field, e.message)
        });
    ApiError::validation(message).with_details(json!(errors))
}

/// A trimmed optional text: `None` when empty; an error when too long or with control characters.
fn clean_text(
    errors: &mut Vec<FieldError>,
    field: &str,
    value: Option<&str>,
    max_chars: usize,
) -> Option<String> {
    let value = value.map(str::trim).filter(|v| !v.is_empty())?;
    if value.chars().count() > max_chars {
        push(
            errors,
            field,
            format!("must be at most {max_chars} characters"),
        );
    } else if value.chars().any(char::is_control) {
        push(errors, field, "must not contain control characters");
    }
    Some(value.to_owned())
}

/// Prices `lines` against `menu`, or lists every problem.
pub fn price_lines(
    menu: &Menu,
    lines: &[OrderLineInput],
) -> Result<Vec<OrderLine>, Vec<FieldError>> {
    let mut errors = Vec::new();
    if lines.is_empty() {
        push(&mut errors, "lines", "must have at least one line");
    } else if lines.len() > MAX_LINES {
        push(
            &mut errors,
            "lines",
            format!("must have at most {MAX_LINES} lines"),
        );
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let mut priced = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        let at = |field: &str| format!("lines[{i}].{field}");
        let before = errors.len();
        if !(1..=MAX_QTY).contains(&line.qty) {
            push(&mut errors, at("qty"), format!("must be 1 to {MAX_QTY}"));
        }
        let note = clean_text(
            &mut errors,
            &at("note"),
            line.note.as_deref(),
            MAX_NOTE_CHARS,
        );
        let Some(item) = menu.get(&line.menu_item_id) else {
            push(
                &mut errors,
                at("menu_item_id"),
                format!("{:?} is not on this restaurant's menu", line.menu_item_id),
            );
            continue;
        };
        let mut picked = vec![0i32; item.options.len()];
        let mut deltas = Vec::with_capacity(line.choice_ids.len());
        let mut seen = HashSet::new();
        for (j, choice) in line.choice_ids.iter().enumerate() {
            if !seen.insert(choice.as_str()) {
                push(
                    &mut errors,
                    format!("lines[{i}].choice_ids[{j}]"),
                    format!("{choice:?} is listed twice"),
                );
                continue;
            }
            match item.choices.get(choice) {
                Some((option, delta)) => {
                    picked[*option] += 1;
                    deltas.push(*delta);
                }
                None => push(
                    &mut errors,
                    format!("lines[{i}].choice_ids[{j}]"),
                    format!("{choice:?} is not a choice of {}", item.name),
                ),
            }
        }
        for (option, count) in item.options.iter().zip(&picked) {
            if *count < option.min || *count > option.max {
                let wanted = if option.min == option.max {
                    format!("exactly {}", option.min)
                } else {
                    format!("{} to {}", option.min, option.max)
                };
                push(
                    &mut errors,
                    at("choice_ids"),
                    format!("pick {wanted} for {} ({count} picked)", option.name),
                );
            }
        }
        if errors.len() > before {
            continue;
        }
        let unit = match math::unit_price_cents(item.price_cents, &deltas) {
            Ok(unit) => unit,
            Err(err) => {
                push(&mut errors, at("choice_ids"), err.to_string());
                continue;
            }
        };
        let total = match math::line_total_cents(PricedLine {
            unit_price_cents: unit,
            qty: line.qty,
        }) {
            Ok(total) => total,
            Err(err) => {
                push(&mut errors, at("qty"), err.to_string());
                continue;
            }
        };
        priced.push(OrderLine {
            menu_item_id: line.menu_item_id.clone(),
            name: item.name.clone(),
            qty: line.qty,
            choice_ids: line.choice_ids.clone(),
            unit_price_cents: unit,
            line_total_cents: total,
            note,
        });
    }
    if errors.is_empty() {
        Ok(priced)
    } else {
        Err(errors)
    }
}

/// The tip's problem, if any.
pub fn check_tip(errors: &mut Vec<FieldError>, tip_cents: i64) {
    if !(0..=MAX_TIP_CENTS).contains(&tip_cents) {
        push(errors, "tip_cents", format!("must be 0 to {MAX_TIP_CENTS}"));
    }
}

/// The drop-off, cleaned (label and note trimmed), with its problems; the place id's existence is
/// checked by the caller.
pub fn check_dropoff(errors: &mut Vec<FieldError>, dropoff: &Place) -> Place {
    check_place(errors, "dropoff", dropoff)
}

/// A `Place` in a request, cleaned (label and note trimmed), with its problems named under
/// `field`: as [`check_dropoff`] for the drop-off, and for a run's `return_place` (contract v1.4).
/// The place id's existence is checked by [`check_place_id`].
pub fn check_place(errors: &mut Vec<FieldError>, field: &str, place: &Place) -> Place {
    let label = place.label.trim();
    if label.is_empty() {
        push(errors, format!("{field}.label"), "must not be empty");
    } else if label.chars().count() > MAX_LABEL_CHARS {
        push(
            errors,
            format!("{field}.label"),
            format!("must be at most {MAX_LABEL_CHARS} characters"),
        );
    } else if label.chars().any(char::is_control) {
        push(
            errors,
            format!("{field}.label"),
            "must not contain control characters",
        );
    }
    if !tracking::is_valid(place.location) {
        push(
            errors,
            format!("{field}.location"),
            "lat must be -90 to 90 and lng -180 to 180",
        );
    }
    let note = clean_text(
        errors,
        &format!("{field}.note"),
        place.note.as_deref(),
        MAX_NOTE_CHARS,
    );
    Place {
        id: place.id.clone(),
        label: label.to_owned(),
        location: place.location,
        note,
    }
}

/// The problem with `place`'s id, if any: when given, it must be a place in `GET /v1/places`.
pub async fn check_place_id(
    pool: &PgPool,
    errors: &mut Vec<FieldError>,
    field: &str,
    place: &Place,
) -> ApiResult<()> {
    if let Some(place_id) = place.id.as_deref() {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM ferry.places WHERE id = $1)")
                .bind(place_id)
                .fetch_one(pool)
                .await?;
        if !exists {
            push(
                errors,
                format!("{field}.id"),
                format!("no place has id {place_id:?}"),
            );
        }
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
struct ItemRow {
    id: String,
    name: String,
    price_cents: i64,
}

#[derive(sqlx::FromRow)]
struct OptionRow {
    id: String,
    menu_item_id: String,
    name: String,
    min_choices: i32,
    max_choices: i32,
}

#[derive(sqlx::FromRow)]
struct ChoiceRow {
    id: String,
    option_id: String,
    price_delta_cents: i64,
}

/// The items of `restaurant_id` among `item_ids`, with their options and choices, in three
/// queries.
pub async fn load_menu(pool: &PgPool, restaurant_id: &str, item_ids: &[String]) -> ApiResult<Menu> {
    let items: Vec<ItemRow> = sqlx::query_as(
        "SELECT id, name, price_cents FROM ferry.menu_items \
         WHERE restaurant_id = $1 AND id = ANY($2)",
    )
    .bind(restaurant_id)
    .bind(item_ids)
    .fetch_all(pool)
    .await?;
    let ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
    let options: Vec<OptionRow> = sqlx::query_as(
        "SELECT id, menu_item_id, name, min_choices, max_choices FROM ferry.item_options \
         WHERE menu_item_id = ANY($1) ORDER BY menu_item_id, sort_order, id",
    )
    .bind(&ids)
    .fetch_all(pool)
    .await?;
    let choices: Vec<ChoiceRow> = sqlx::query_as(
        "SELECT c.id, c.option_id, c.price_delta_cents FROM ferry.item_choices c \
         JOIN ferry.item_options o ON o.id = c.option_id \
         WHERE o.menu_item_id = ANY($1)",
    )
    .bind(&ids)
    .fetch_all(pool)
    .await?;

    let mut menu: Menu = items
        .into_iter()
        .map(|i| {
            (
                i.id,
                MenuItem {
                    name: i.name,
                    price_cents: i.price_cents,
                    ..MenuItem::default()
                },
            )
        })
        .collect();
    // Option id to (item id, index of the option in the item).
    let mut option_at: HashMap<String, (String, usize)> = HashMap::new();
    for option in options {
        if let Some(item) = menu.get_mut(&option.menu_item_id) {
            option_at.insert(
                option.id.clone(),
                (option.menu_item_id.clone(), item.options.len()),
            );
            item.options.push(MenuOption {
                id: option.id,
                name: option.name,
                min: option.min_choices,
                max: option.max_choices,
            });
        }
    }
    for choice in choices {
        if let Some((item_id, index)) = option_at.get(&choice.option_id)
            && let Some(item) = menu.get_mut(item_id)
        {
            item.choices
                .insert(choice.id, (*index, choice.price_delta_cents));
        }
    }
    Ok(menu)
}

/// Prices `request` for a run at `restaurant_id`: the lines, the totals and the drop-off, or 400
/// `validation_failed` with every problem.
pub async fn price(
    pool: &PgPool,
    config: &Config,
    restaurant_id: &str,
    request: &CreateOrderRequest,
) -> ApiResult<PricedOrder> {
    let mut errors = Vec::new();
    let item_ids: Vec<String> = request
        .lines
        .iter()
        .map(|l| l.menu_item_id.clone())
        .collect();
    let menu = if request.lines.is_empty() || request.lines.len() > MAX_LINES {
        Menu::new()
    } else {
        load_menu(pool, restaurant_id, &item_ids).await?
    };
    let lines = price_lines(&menu, &request.lines).unwrap_or_else(|line_errors| {
        errors.extend(line_errors);
        Vec::new()
    });
    check_tip(&mut errors, request.tip_cents);
    let dropoff = check_dropoff(&mut errors, &request.dropoff);
    check_place_id(pool, &mut errors, "dropoff", &dropoff).await?;
    validate::check_radius(&mut errors, "dropoff_radius_m", request.dropoff_radius_m);
    if !errors.is_empty() {
        return Err(invalid(&errors));
    }
    let priced: Vec<PricedLine> = lines
        .iter()
        .map(|l| PricedLine {
            unit_price_cents: l.unit_price_cents,
            qty: l.qty,
        })
        .collect();
    let quote = math::quote_with(config, &priced, request.tip_cents)?;
    Ok(PricedOrder {
        lines,
        quote,
        dropoff,
        // Checked above: 50 to 5000, so it fits.
        dropoff_radius_m: request.dropoff_radius_m.map(|m| m as i32),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::types::LatLng;

    /// Cosmic Cantina's chicken burrito as the fixtures have it, plus a discount choice.
    fn menu() -> Menu {
        let mut burrito = MenuItem {
            name: "Chicken Burrito".into(),
            price_cents: 899,
            options: vec![
                MenuOption {
                    id: "opt_beans".into(),
                    name: "Beans".into(),
                    min: 1,
                    max: 1,
                },
                MenuOption {
                    id: "opt_extras".into(),
                    name: "Extras".into(),
                    min: 0,
                    max: 3,
                },
                MenuOption {
                    id: "opt_size".into(),
                    name: "Size".into(),
                    min: 0,
                    max: 1,
                },
            ],
            choices: HashMap::new(),
        };
        for (id, option, delta) in [
            ("cho_black", 0, 0),
            ("cho_no_beans", 0, 0),
            ("cho_guac", 1, 150),
            ("cho_sour_cream", 1, 75),
            ("cho_cheese", 1, 100),
            ("cho_jalapenos", 1, 50),
            ("cho_tiny", 2, -1000),
        ] {
            burrito.choices.insert(id.into(), (option, delta));
        }
        let chips = MenuItem {
            name: "Chips & Salsa".into(),
            price_cents: 349,
            ..MenuItem::default()
        };
        Menu::from([
            ("itm_burrito".to_owned(), burrito),
            ("itm_chips".to_owned(), chips),
        ])
    }

    fn line(item: &str, qty: i64, choices: &[&str]) -> OrderLineInput {
        OrderLineInput {
            menu_item_id: item.into(),
            qty,
            choice_ids: choices.iter().map(|c| c.to_string()).collect(),
            note: None,
        }
    }

    fn fields(errors: &[FieldError]) -> Vec<&str> {
        errors.iter().map(|e| e.field.as_str()).collect()
    }

    #[test]
    fn prices_the_demo_burrito_and_chips() {
        let lines = price_lines(
            &menu(),
            &[
                line("itm_burrito", 1, &["cho_no_beans", "cho_guac"]),
                line("itm_chips", 2, &[]),
            ],
        )
        .unwrap();
        assert_eq!(lines[0].name, "Chicken Burrito");
        assert_eq!(lines[0].unit_price_cents, 1049);
        assert_eq!(lines[0].line_total_cents, 1049);
        assert_eq!(lines[0].choice_ids, vec!["cho_no_beans", "cho_guac"]);
        assert_eq!(lines[1].unit_price_cents, 349);
        assert_eq!(lines[1].line_total_cents, 698);
    }

    #[test]
    fn option_min_and_max() {
        // Beans is exactly one.
        let errors = price_lines(&menu(), &[line("itm_burrito", 1, &[])]).unwrap_err();
        assert_eq!(fields(&errors), vec!["lines[0].choice_ids"]);
        assert!(
            errors[0].message.contains("exactly 1 for Beans"),
            "{errors:?}"
        );
        let errors = price_lines(
            &menu(),
            &[line("itm_burrito", 1, &["cho_black", "cho_no_beans"])],
        )
        .unwrap_err();
        assert!(errors[0].message.contains("(2 picked)"), "{errors:?}");
        // Extras is 0 to 3.
        let four = [
            "cho_black",
            "cho_guac",
            "cho_sour_cream",
            "cho_cheese",
            "cho_jalapenos",
        ];
        let errors = price_lines(&menu(), &[line("itm_burrito", 1, &four)]).unwrap_err();
        assert!(
            errors[0].message.contains("0 to 3 for Extras"),
            "{errors:?}"
        );
        let three = ["cho_black", "cho_guac", "cho_sour_cream", "cho_cheese"];
        assert_eq!(
            price_lines(&menu(), &[line("itm_burrito", 1, &three)]).unwrap()[0].unit_price_cents,
            899 + 150 + 75 + 100
        );
    }

    #[test]
    fn unknown_duplicate_and_foreign_choices() {
        let errors = price_lines(
            &menu(),
            &[line("itm_burrito", 1, &["cho_black", "cho_black"])],
        )
        .unwrap_err();
        assert_eq!(fields(&errors), vec!["lines[0].choice_ids[1]"]);
        let errors = price_lines(&menu(), &[line("itm_chips", 1, &["cho_guac"])]).unwrap_err();
        assert_eq!(fields(&errors), vec!["lines[0].choice_ids[0]"]);
        let errors = price_lines(&menu(), &[line("itm_tacos", 1, &[])]).unwrap_err();
        assert_eq!(fields(&errors), vec!["lines[0].menu_item_id"]);
    }

    #[test]
    fn quantities_lines_notes_and_negative_prices() {
        assert_eq!(
            fields(&price_lines(&menu(), &[]).unwrap_err()),
            vec!["lines"]
        );
        let many: Vec<OrderLineInput> = (0..21).map(|_| line("itm_chips", 1, &[])).collect();
        assert_eq!(
            fields(&price_lines(&menu(), &many).unwrap_err()),
            vec!["lines"]
        );
        for qty in [0, -1, 21] {
            let errors = price_lines(&menu(), &[line("itm_chips", qty, &[])]).unwrap_err();
            assert_eq!(fields(&errors), vec!["lines[0].qty"]);
        }
        // A discount below zero is refused, not stored.
        let errors = price_lines(
            &menu(),
            &[line("itm_burrito", 1, &["cho_black", "cho_tiny"])],
        )
        .unwrap_err();
        assert_eq!(fields(&errors), vec!["lines[0].choice_ids"]);

        let mut noted = line("itm_chips", 1, &[]);
        noted.note = Some("  extra salsa  ".into());
        assert_eq!(
            price_lines(&menu(), &[noted.clone()]).unwrap()[0]
                .note
                .as_deref(),
            Some("extra salsa")
        );
        noted.note = Some("   ".into());
        assert_eq!(
            price_lines(&menu(), &[noted.clone()]).unwrap()[0].note,
            None
        );
        noted.note = Some("x".repeat(281));
        assert_eq!(
            fields(&price_lines(&menu(), &[noted]).unwrap_err()),
            vec!["lines[0].note"]
        );
    }

    #[test]
    fn tip_and_dropoff() {
        let mut errors = Vec::new();
        check_tip(&mut errors, 0);
        check_tip(&mut errors, MAX_TIP_CENTS);
        assert!(errors.is_empty());
        check_tip(&mut errors, -1);
        check_tip(&mut errors, MAX_TIP_CENTS + 1);
        assert_eq!(fields(&errors), vec!["tip_cents", "tip_cents"]);

        let mut errors = Vec::new();
        let place = check_dropoff(
            &mut errors,
            &Place {
                id: Some("place_fetzer".into()),
                label: " Fetzer Gym ".into(),
                location: LatLng {
                    lat: 35.909152,
                    lng: -79.046905,
                },
                note: Some("".into()),
            },
        );
        assert!(errors.is_empty());
        assert_eq!(place.label, "Fetzer Gym");
        assert_eq!(place.note, None);

        check_dropoff(
            &mut errors,
            &Place {
                id: None,
                label: "  ".into(),
                location: LatLng {
                    lat: 91.0,
                    lng: 0.0,
                },
                note: None,
            },
        );
        assert_eq!(fields(&errors), vec!["dropoff.label", "dropoff.location"]);
    }
}
