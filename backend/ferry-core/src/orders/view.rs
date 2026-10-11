//! The contract's `Order`, read in three queries per call (orders, their lines, their timelines)
//! however many orders it covers, and shown per viewer.
//!
//! [`OrderRecord::as_seen_by`] gives `handoff_code` only to the order's customer, and
//! `receipt_url` only to its customer and its run's runner. `receipt_url` is a signed link to the
//! run's receipt photo that [`sign_receipts`] adds after loading (null until then, when the run
//! has no `receipt_path`, or when Storage cannot sign it). `eta_minutes` is the caller's: from a
//! `runs::live::LivePlan` on single reads and events, null in lists.

use std::collections::HashMap;

use serde::Serialize;
use sqlx::PgPool;

use super::machine::OrderStatus;
use crate::catalog::types::{LatLng, Place};
use crate::circles::PublicUser;
use crate::error::ApiResult;
use crate::runs::RunStatus;
use crate::runs::view::iso;
use crate::state::AppState;
use crate::storage;

/// `GET /v1/orders` returns at most this many orders, newest first.
pub const LIST_LIMIT: i64 = 100;

/// One order with its run's restaurant, runner and status, and both people. Callers add the
/// `WHERE`.
macro_rules! order_select {
    () => {
        concat!(
            "SELECT o.id, o.run_id, r.restaurant_id, r.runner_id, r.status AS run_status, \
                    r.receipt_path, \
                    o.customer_id, o.status, o.subtotal_cents, o.tax_cents, o.ferry_fee_cents, \
                    o.tip_cents, o.total_cents, o.dropoff_place_id, o.dropoff_label, \
                    o.dropoff_lat, o.dropoff_lng, o.dropoff_note, o.dropoff_radius_m, \
                    o.handoff_code, o.co2_saved_g, o.is_demo_history, rs.lat AS restaurant_lat, rs.lng AS restaurant_lng, \
                    c.display_name AS customer_display_name, c.avatar_seed AS customer_avatar_seed, \
                    c.bubble_color AS customer_bubble_color, \
                    u.display_name AS runner_display_name, u.avatar_seed AS runner_avatar_seed, \
                    u.bubble_color AS runner_bubble_color, ",
            iso!("o.created_at"),
            " AS created_at \
             FROM ferry.orders o \
             JOIN ferry.runs r ON r.id = o.run_id \
             JOIN ferry.restaurants rs ON rs.id = r.restaurant_id \
             JOIN ferry.users c ON c.id = o.customer_id \
             JOIN ferry.users u ON u.id = r.runner_id "
        )
    };
}

const BY_IDS: &str = concat!(
    order_select!(),
    "WHERE o.id = ANY($1) ORDER BY o.created_at, o.id"
);

/// A run's orders, oldest first; `$2` limits them to one customer when not null.
const FOR_RUN: &str = concat!(
    order_select!(),
    "WHERE o.run_id = $1 AND ($2::text IS NULL OR o.customer_id = $2) \
     ORDER BY o.created_at, o.id"
);

/// A customer's real orders (no demo history), newest first.
const FOR_CUSTOMER: &str = concat!(
    order_select!(),
    "WHERE o.customer_id = $1 AND NOT o.is_demo_history \
     ORDER BY o.created_at DESC, o.id DESC LIMIT $2"
);

/// `OrderLine` in contract/openapi.yaml.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrderLine {
    pub menu_item_id: String,
    pub name: String,
    pub qty: i64,
    pub choice_ids: Vec<String>,
    pub unit_price_cents: i64,
    pub line_total_cents: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// `OrderTimelineEntry` in contract/openapi.yaml.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrderTimelineEntry {
    pub status: OrderStatus,
    pub at: String,
}

/// `Order` in contract/openapi.yaml.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Order {
    pub id: String,
    pub run_id: String,
    pub restaurant_id: String,
    pub customer: PublicUser,
    pub runner: PublicUser,
    pub lines: Vec<OrderLine>,
    pub subtotal_cents: i64,
    pub tax_cents: i64,
    pub ferry_fee_cents: i64,
    pub tip_cents: i64,
    pub total_cents: i64,
    pub status: OrderStatus,
    pub dropoff: Place,
    /// Only in the customer's copy.
    pub handoff_code: Option<String>,
    pub eta_minutes: Option<f64>,
    pub co2_saved_g: Option<f64>,
    pub receipt_url: Option<String>,
    pub timeline: Vec<OrderTimelineEntry>,
    /// Contract v1.4: the request's drop-off radius, or null.
    pub dropoff_radius_m: Option<i32>,
}

#[derive(sqlx::FromRow)]
struct OrderRow {
    id: String,
    run_id: String,
    restaurant_id: String,
    runner_id: String,
    run_status: String,
    receipt_path: Option<String>,
    customer_id: String,
    status: String,
    subtotal_cents: i64,
    tax_cents: i64,
    ferry_fee_cents: i64,
    tip_cents: i64,
    total_cents: i64,
    dropoff_place_id: Option<String>,
    dropoff_label: String,
    dropoff_lat: f64,
    dropoff_lng: f64,
    dropoff_note: Option<String>,
    dropoff_radius_m: Option<i32>,
    handoff_code: String,
    co2_saved_g: Option<f64>,
    is_demo_history: bool,
    restaurant_lat: f64,
    restaurant_lng: f64,
    customer_display_name: String,
    customer_avatar_seed: String,
    customer_bubble_color: String,
    runner_display_name: String,
    runner_avatar_seed: String,
    runner_bubble_color: String,
    created_at: String,
}

#[derive(sqlx::FromRow)]
struct LineRow {
    order_id: String,
    menu_item_id: String,
    name: String,
    qty: i32,
    choice_ids: Vec<String>,
    unit_price_cents: i64,
    line_total_cents: i64,
    note: Option<String>,
}

#[derive(sqlx::FromRow)]
struct EventRow {
    order_id: String,
    status: String,
    at: String,
}

/// An order as stored, with what only some viewers get (`handoff_code`, `receipt_url`).
#[derive(Debug, Clone, PartialEq)]
pub struct OrderRecord {
    pub id: String,
    pub run_id: String,
    pub restaurant_id: String,
    pub runner_id: String,
    pub run_status: RunStatus,
    /// The run's receipt photo in the `receipts` bucket (`POST /v1/runs/{id}/purchased`).
    pub receipt_path: Option<String>,
    /// A signed link to `receipt_path`, set by [`sign_receipts`].
    pub receipt_url: Option<String>,
    pub customer_id: String,
    pub status: OrderStatus,
    pub customer: PublicUser,
    pub runner: PublicUser,
    pub lines: Vec<OrderLine>,
    pub subtotal_cents: i64,
    pub tax_cents: i64,
    pub ferry_fee_cents: i64,
    pub tip_cents: i64,
    pub total_cents: i64,
    pub dropoff: Place,
    pub dropoff_radius_m: Option<i32>,
    pub handoff_code: String,
    pub co2_saved_g: Option<f64>,
    pub is_demo_history: bool,
    pub restaurant_location: LatLng,
    pub created_at: String,
    pub timeline: Vec<OrderTimelineEntry>,
}

impl OrderRecord {
    fn from_row(row: OrderRow) -> ApiResult<Self> {
        Ok(Self {
            run_status: RunStatus::from_db(&row.run_status)?,
            receipt_path: row.receipt_path,
            receipt_url: None,
            status: OrderStatus::from_db(&row.status)?,
            customer: PublicUser {
                id: row.customer_id.clone(),
                display_name: row.customer_display_name,
                avatar_seed: row.customer_avatar_seed,
                bubble_color: row.customer_bubble_color,
            },
            runner: PublicUser {
                id: row.runner_id.clone(),
                display_name: row.runner_display_name,
                avatar_seed: row.runner_avatar_seed,
                bubble_color: row.runner_bubble_color,
            },
            dropoff: Place {
                id: row.dropoff_place_id,
                label: row.dropoff_label,
                location: LatLng {
                    lat: row.dropoff_lat,
                    lng: row.dropoff_lng,
                },
                note: row.dropoff_note,
            },
            restaurant_location: LatLng {
                lat: row.restaurant_lat,
                lng: row.restaurant_lng,
            },
            id: row.id,
            run_id: row.run_id,
            restaurant_id: row.restaurant_id,
            runner_id: row.runner_id,
            customer_id: row.customer_id,
            lines: Vec::new(),
            subtotal_cents: row.subtotal_cents,
            tax_cents: row.tax_cents,
            ferry_fee_cents: row.ferry_fee_cents,
            tip_cents: row.tip_cents,
            total_cents: row.total_cents,
            dropoff_radius_m: row.dropoff_radius_m,
            handoff_code: row.handoff_code,
            co2_saved_g: row.co2_saved_g,
            is_demo_history: row.is_demo_history,
            created_at: row.created_at,
            timeline: Vec::new(),
        })
    }

    /// True for the order's customer and its run's runner: the only people who may read it.
    pub fn is_party(&self, viewer: &str) -> bool {
        viewer == self.customer_id || viewer == self.runner_id
    }

    /// The order as `viewer` sees it: `handoff_code` only for the customer, `receipt_url` only for
    /// the customer and the runner. `eta_minutes` is the caller's, kept only while the order is
    /// active.
    pub fn as_seen_by(&self, viewer: &str, eta_minutes: Option<f64>) -> Order {
        Order {
            id: self.id.clone(),
            run_id: self.run_id.clone(),
            restaurant_id: self.restaurant_id.clone(),
            customer: self.customer.clone(),
            runner: self.runner.clone(),
            lines: self.lines.clone(),
            subtotal_cents: self.subtotal_cents,
            tax_cents: self.tax_cents,
            ferry_fee_cents: self.ferry_fee_cents,
            tip_cents: self.tip_cents,
            total_cents: self.total_cents,
            status: self.status,
            dropoff: self.dropoff.clone(),
            handoff_code: (viewer == self.customer_id).then(|| self.handoff_code.clone()),
            eta_minutes: eta_minutes.filter(|eta| self.status.is_active() && eta.is_finite()),
            co2_saved_g: self.co2_saved_g,
            receipt_url: self.receipt_url.clone().filter(|_| self.is_party(viewer)),
            timeline: self.timeline.clone(),
            dropoff_radius_m: self.dropoff_radius_m,
        }
    }
}

/// Sets `receipt_url` on every order whose run has a receipt photo: a signed link valid 1 hour
/// (`storage::signed_receipt_urls`, cached, one Storage call for all the misses). A path that does
/// not start with `<run_id>/`, or that Storage cannot sign, leaves `receipt_url` null; this never
/// fails the read.
pub async fn sign_receipts(state: &AppState, records: &mut [OrderRecord]) {
    let mut paths: Vec<String> = records
        .iter()
        .filter_map(|o| {
            o.receipt_path
                .clone()
                .filter(|p| is_run_receipt(&o.run_id, p))
        })
        .collect();
    if paths.is_empty() {
        return;
    }
    paths.sort();
    paths.dedup();
    let links = storage::signed_receipt_urls(&state.http, &state.config, &paths).await;
    for record in records {
        record.receipt_url = record
            .receipt_path
            .as_ref()
            .filter(|p| is_run_receipt(&record.run_id, p))
            .and_then(|p| links.get(p).cloned());
    }
}

/// True when `path` is a file under `<run_id>/`, as `POST /v1/runs/{id}/purchased` requires.
fn is_run_receipt(run_id: &str, path: &str) -> bool {
    path.strip_prefix(run_id)
        .and_then(|rest| rest.strip_prefix('/'))
        .is_some_and(|name| !name.is_empty())
}

/// The orders with these ids, oldest first.
pub async fn load_by_ids(pool: &PgPool, ids: &[String]) -> ApiResult<Vec<OrderRecord>> {
    let rows: Vec<OrderRow> = sqlx::query_as(BY_IDS).bind(ids).fetch_all(pool).await?;
    attach(pool, rows).await
}

/// The order with this id, or `None`.
pub async fn load_one(pool: &PgPool, id: &str) -> ApiResult<Option<OrderRecord>> {
    Ok(load_by_ids(pool, &[id.to_owned()]).await?.pop())
}

/// A run's orders, oldest first, or only `customer`'s when given.
pub async fn load_for_run(
    pool: &PgPool,
    run_id: &str,
    customer: Option<&str>,
) -> ApiResult<Vec<OrderRecord>> {
    let rows: Vec<OrderRow> = sqlx::query_as(FOR_RUN)
        .bind(run_id)
        .bind(customer)
        .fetch_all(pool)
        .await?;
    attach(pool, rows).await
}

/// `customer`'s orders, newest first, without demo history, at most [`LIST_LIMIT`].
pub async fn load_for_customer(pool: &PgPool, customer: &str) -> ApiResult<Vec<OrderRecord>> {
    let rows: Vec<OrderRow> = sqlx::query_as(FOR_CUSTOMER)
        .bind(customer)
        .bind(LIST_LIMIT)
        .fetch_all(pool)
        .await?;
    attach(pool, rows).await
}

/// Adds every order's lines and timeline, in one query each.
async fn attach(pool: &PgPool, rows: Vec<OrderRow>) -> ApiResult<Vec<OrderRecord>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
    let lines: Vec<LineRow> = sqlx::query_as(
        "SELECT order_id, menu_item_id, name, qty, choice_ids, unit_price_cents, \
                line_total_cents, note \
         FROM ferry.order_lines WHERE order_id = ANY($1) ORDER BY order_id, line_no",
    )
    .bind(&ids)
    .fetch_all(pool)
    .await?;
    let events: Vec<EventRow> = sqlx::query_as(concat!(
        "SELECT order_id, status, ",
        iso!("ts"),
        " AS at FROM ferry.order_events WHERE order_id = ANY($1) ORDER BY ts, id"
    ))
    .bind(&ids)
    .fetch_all(pool)
    .await?;

    let mut lines_by: HashMap<String, Vec<OrderLine>> = HashMap::new();
    for line in lines {
        lines_by.entry(line.order_id).or_default().push(OrderLine {
            menu_item_id: line.menu_item_id,
            name: line.name,
            qty: i64::from(line.qty),
            choice_ids: line.choice_ids,
            unit_price_cents: line.unit_price_cents,
            line_total_cents: line.line_total_cents,
            note: line.note,
        });
    }
    let mut events_by: HashMap<String, Vec<OrderTimelineEntry>> = HashMap::new();
    for event in events {
        events_by
            .entry(event.order_id)
            .or_default()
            .push(OrderTimelineEntry {
                status: OrderStatus::from_db(&event.status)?,
                at: event.at,
            });
    }
    rows.into_iter()
        .map(|row| {
            let mut record = OrderRecord::from_row(row)?;
            record.lines = lines_by.remove(&record.id).unwrap_or_default();
            record.timeline = events_by.remove(&record.id).unwrap_or_default();
            Ok(record)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(id: &str) -> PublicUser {
        PublicUser {
            id: id.into(),
            display_name: id.trim_start_matches("usr_").into(),
            avatar_seed: id.into(),
            bubble_color: "#336699".into(),
        }
    }

    fn record(status: OrderStatus) -> OrderRecord {
        OrderRecord {
            id: "ord_x".into(),
            run_id: "run_x".into(),
            restaurant_id: "rst_cosmic".into(),
            runner_id: "usr_maya".into(),
            run_status: RunStatus::Delivering,
            receipt_path: Some("run_x/receipt.jpg".into()),
            receipt_url: Some(
                "https://abc.supabase.co/storage/v1/object/sign/receipts/run_x/receipt.jpg?token=t"
                    .into(),
            ),
            customer_id: "usr_judge".into(),
            status,
            customer: person("usr_judge"),
            runner: person("usr_maya"),
            lines: Vec::new(),
            subtotal_cents: 899,
            tax_cents: 67,
            ferry_fee_cents: 150,
            tip_cents: 200,
            total_cents: 1316,
            dropoff: Place {
                id: Some("place_fetzer".into()),
                label: "Fetzer Gym (HackNC)".into(),
                location: LatLng {
                    lat: 35.909152,
                    lng: -79.046905,
                },
                note: None,
            },
            dropoff_radius_m: Some(100),
            handoff_code: "4829".into(),
            co2_saved_g: None,
            is_demo_history: false,
            restaurant_location: LatLng {
                lat: 35.913211,
                lng: -79.055094,
            },
            created_at: "2026-10-10T22:01:00.000Z".into(),
            timeline: Vec::new(),
        }
    }

    #[test]
    fn handoff_code_only_for_the_customer() {
        let order = record(OrderStatus::PickedUp);
        assert_eq!(
            order.as_seen_by("usr_judge", None).handoff_code.as_deref(),
            Some("4829")
        );
        assert_eq!(order.as_seen_by("usr_maya", None).handoff_code, None);
        assert!(order.is_party("usr_judge") && order.is_party("usr_maya"));
        assert!(!order.is_party("usr_dana"));
    }

    #[test]
    fn receipt_url_only_for_the_customer_and_the_runner() {
        let order = record(OrderStatus::PickedUp);
        let link = order.receipt_url.clone();
        assert!(link.is_some());
        assert_eq!(order.as_seen_by("usr_judge", None).receipt_url, link);
        assert_eq!(order.as_seen_by("usr_maya", None).receipt_url, link);
        assert_eq!(order.as_seen_by("usr_dana", None).receipt_url, None);
        assert_eq!(order.as_seen_by("", None).receipt_url, None);
        let unsigned = OrderRecord {
            receipt_url: None,
            ..record(OrderStatus::PickedUp)
        };
        assert_eq!(unsigned.as_seen_by("usr_judge", None).receipt_url, None);
    }

    #[test]
    fn receipt_paths_must_be_under_the_run() {
        assert!(is_run_receipt("run_x", "run_x/receipt.jpg"));
        assert!(is_run_receipt("run_x", "run_x/a/b.jpg"));
        assert!(!is_run_receipt("run_x", "run_x/"));
        assert!(!is_run_receipt("run_x", "run_x"));
        assert!(!is_run_receipt("run_x", "run_xy/receipt.jpg"));
        assert!(!is_run_receipt("run_x", "run_y/receipt.jpg"));
        assert!(!is_run_receipt("run_x", "/run_x/receipt.jpg"));
    }

    #[test]
    fn eta_only_while_active() {
        assert_eq!(
            record(OrderStatus::PickedUp)
                .as_seen_by("usr_judge", Some(4.0))
                .eta_minutes,
            Some(4.0)
        );
        for status in [
            OrderStatus::Delivered,
            OrderStatus::Settled,
            OrderStatus::Cancelled,
        ] {
            assert_eq!(
                record(status)
                    .as_seen_by("usr_judge", Some(4.0))
                    .eta_minutes,
                None
            );
        }
    }

    #[test]
    fn serializes_every_required_field() {
        let value =
            serde_json::to_value(record(OrderStatus::Funded).as_seen_by("usr_maya", None)).unwrap();
        for field in [
            "id",
            "run_id",
            "restaurant_id",
            "customer",
            "runner",
            "lines",
            "subtotal_cents",
            "tax_cents",
            "ferry_fee_cents",
            "tip_cents",
            "total_cents",
            "status",
            "dropoff",
            "handoff_code",
            "eta_minutes",
            "co2_saved_g",
            "receipt_url",
            "timeline",
            "dropoff_radius_m",
        ] {
            assert!(value.get(field).is_some(), "{field} missing: {value}");
        }
        assert_eq!(value["handoff_code"], serde_json::Value::Null);
        assert_eq!(value["dropoff_radius_m"], 100);
    }
}
