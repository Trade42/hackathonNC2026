//! Ordering by text (spec section 3, Phase B): "chicken burrito no beans" in the group, or in a
//! DM, becomes a priced draft that only the drafter's Place order button places.
//!
//! 1. The open runs the user may order on: runs taking orders (`open`, window not passed) in the
//!    group's circle, or in any of the user's circles in a DM, where the user is not the runner.
//! 2. [`super::order_text`] parses the text against the run's menu: items, quantities, choices.
//!    Words that fit several items ask "Which one ...?", and a required option with no choice asks
//!    for it.
//! 3. `orders::price_order` prices it exactly as placing would (the same 400, 403 and 409), with
//!    the user's saved drop-off and default tip. The draft is stored (`ferry.order_drafts`) and
//!    the bot shows the lines and the total with Place order and Cancel buttons
//!    (`order:<draft id>:place|cancel`). Only the drafter's press counts.
//! 4. **Only the Place order button places it** ([`place_draft`]). No text does: Gemini's
//!    confirm_order and the fallback's "place it" show the draft again with its buttons
//!    ([`show_pending`]), and "cancel my order" shows what can be cancelled, each with its own
//!    button ([`cancel_choices`]; `cancel:<order id>` for a placed order).
//! 5. Placing claims the draft first: a conditional `pending → placing` update, committed on its
//!    own, so a second press finds the claim. Then `orders::place_order` runs on a task of its own
//!    with no connection or lock held, so no caller's time budget can cut it in half, and the
//!    order id is stored. A refusal that comes only before the order commits (402, 409, 400, 403)
//!    puts the draft back to pending, or cancels it when the window closed, with that refusal's
//!    own line; no order is looked up. When placing fails in a way that does not say whether the
//!    order committed, the orders table answers: an order on that run for that customer since the
//!    claim, linked to no draft, is this draft's, and the user hears it is placed, never "try
//!    again". While another draft of the same user on the same run is placing too, that order may
//!    be the other's, so the answer is [`PLACING_UNSURE`] and the claim stays.
//! 6. While a draft is pending ([`DRAFT_MINUTES`]), "add guac" or "make it pinto beans" from the
//!    same person in the same chat edits it and prices it again; an edit that loses the race to a
//!    Place order press changes nothing.
//! 7. Lines name the drop-off as `{dropoff}` ([`DROPOFF`]): Gemini sees the placeholder, and the
//!    chat sees the user's own label in a DM but only a known place's name (`ferry.places`) in a
//!    group, never a free-text label ([`dropoff_shown`]).
//!
//! Totals, prices, permissions and money all come from runs-orders; this module only reads text
//! and keeps the draft.

use serde_json::{Value, json};
use sqlx::PgPool;

use super::order_text::{self, ChoiceRef, ItemRef, OptionRef, ParseOutcome, ParsedLine};
use super::tools::{and_list, dollars, link_button, status_words, web_url_of};
use crate::catalog::types::{LatLng, Place};
use crate::error::{ApiError, ApiResult, ErrorCode};
use crate::messaging::sidecar::{Button, SendRequest};
use crate::orders::{self, CreateOrderRequest};
use crate::payments::OrderQuote;
use crate::profile::load_user;
use crate::state::AppState;

/// The first part of a draft's button data: `order:<draft id>:place|cancel`.
pub const BUTTON_PREFIX: &str = "order";
/// The first part of a placed order's Cancel button: `cancel:<order id>`.
pub const CANCEL_PREFIX: &str = "cancel";
/// How long a pending draft takes edits and is shown again for "place it".
pub const DRAFT_MINUTES: i32 = 15;
/// Where a line names the drop-off; [`DraftReply::text`] and [`Placed::say`] fill it.
pub const DROPOFF: &str = "{dropoff}";
/// A group's name for a drop-off that is not a known place.
pub const GROUP_DROPOFF: &str = "your drop-off";
/// The most orders "cancel my order" offers at once.
const MAX_CANCEL_CHOICES: i64 = 3;

/// What a press of Place order hears when the order may have committed but Ferry cannot tell.
pub const PLACING_UNSURE: &str = "Ferry couldn't confirm that order just now. Check your orders in the Ferry app before you tap Place order again.";
/// What a press hears while another press of the same draft is placing it.
pub const PLACING_NOW: &str = "That order is being placed right now.";
/// What an edit hears when a Place order press claimed the draft first.
pub const EDIT_TOO_LATE: &str =
    "That order is already being placed, so I left it as it was. Send another order to add more.";

pub fn button_data(id: i64, place: bool) -> String {
    format!(
        "{BUTTON_PREFIX}:{id}:{}",
        if place { "place" } else { "cancel" }
    )
}

/// Reads `order:<id>:place|cancel` back.
pub fn parse_button(data: &str) -> Option<(i64, bool)> {
    let mut parts = data.split(':');
    let (prefix, id, action) = (parts.next()?, parts.next()?, parts.next()?);
    if prefix != BUTTON_PREFIX || parts.next().is_some() {
        return None;
    }
    let id = id.parse().ok().filter(|id| *id > 0)?;
    match action {
        "place" => Some((id, true)),
        "cancel" => Some((id, false)),
        _ => None,
    }
}

/// `cancel:<order id>`, a placed order's Cancel button.
pub fn cancel_button_data(order_id: &str) -> String {
    format!("{CANCEL_PREFIX}:{order_id}")
}

/// Reads `cancel:<order id>` back: an `ord_` id of letters, digits and `_`.
pub fn parse_cancel_button(data: &str) -> Option<&str> {
    let id = data.strip_prefix(CANCEL_PREFIX)?.strip_prefix(':')?;
    let ok = id.starts_with("ord_")
        && id.len() <= 40
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    ok.then_some(id)
}

/// A draft's Place order and Cancel buttons.
fn draft_buttons(id: i64) -> Vec<Button> {
    vec![
        Button::callback("Place order", button_data(id, true)),
        Button::callback("Cancel", button_data(id, false)),
    ]
}

/// How a drop-off shows in a chat: the user's own label in a DM; in a group only the name of a
/// known place (`ferry.places`), never a free-text label, else [`GROUP_DROPOFF`].
pub async fn dropoff_shown(
    pool: &PgPool,
    place_id: Option<&str>,
    label: &str,
    is_group: bool,
) -> ApiResult<String> {
    if !is_group && !label.trim().is_empty() {
        return Ok(label.trim().to_owned());
    }
    let Some(place_id) = place_id.filter(|_| is_group) else {
        return Ok(GROUP_DROPOFF.to_owned());
    };
    let known: Option<String> = sqlx::query_scalar("SELECT label FROM ferry.places WHERE id = $1")
        .bind(place_id)
        .fetch_optional(pool)
        .await?;
    Ok(known
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| GROUP_DROPOFF.to_owned()))
}

/// A run taking orders.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OpenRun {
    pub run_id: String,
    pub restaurant_id: String,
    pub restaurant: String,
    pub runner_name: String,
    /// When orders close, Chapel Hill's clock ("6:10").
    pub closes: String,
}

/// The runs `user_id` may order on now: in `circle_id` (a group) or in any of their circles (a
/// DM), open with the window not passed, not their own; the soonest to close first.
pub async fn orderable_runs(
    pool: &PgPool,
    user_id: &str,
    circle_id: Option<&str>,
) -> ApiResult<Vec<OpenRun>> {
    Ok(sqlx::query_as(
        "SELECT r.id AS run_id, r.restaurant_id, rest.name AS restaurant,
                u.display_name AS runner_name,
                to_char(r.window_closes_at AT TIME ZONE 'America/New_York', 'FMHH12:MI') AS closes
           FROM ferry.runs r
           JOIN ferry.restaurants rest ON rest.id = r.restaurant_id
           JOIN ferry.users u ON u.id = r.runner_id
          WHERE r.status = 'open' AND r.window_closes_at > now() AND r.runner_id <> $1
            AND NOT r.is_demo_history
            AND CASE WHEN $2::text IS NULL
                     THEN r.circle_id IN (SELECT circle_id FROM ferry.circle_members WHERE user_id = $1)
                     ELSE r.circle_id = $2 END
          ORDER BY r.window_closes_at, r.id",
    )
    .bind(user_id)
    .bind(circle_id)
    .fetch_all(pool)
    .await?)
}

/// `restaurant_id`'s menu as the parser reads it, in menu order.
pub async fn menu_refs(pool: &PgPool, restaurant_id: &str) -> ApiResult<Vec<ItemRef>> {
    let items: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, name FROM ferry.menu_items WHERE restaurant_id = $1 ORDER BY sort_order, id",
    )
    .bind(restaurant_id)
    .fetch_all(pool)
    .await?;
    let options: Vec<(String, String, String, i32, i32)> = sqlx::query_as(
        "SELECT o.menu_item_id, o.id, o.name, o.min_choices, o.max_choices
           FROM ferry.item_options o JOIN ferry.menu_items i ON i.id = o.menu_item_id
          WHERE i.restaurant_id = $1 ORDER BY o.sort_order, o.id",
    )
    .bind(restaurant_id)
    .fetch_all(pool)
    .await?;
    let choices: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT c.option_id, c.id, c.name FROM ferry.item_choices c
           JOIN ferry.item_options o ON o.id = c.option_id
           JOIN ferry.menu_items i ON i.id = o.menu_item_id
          WHERE i.restaurant_id = $1 ORDER BY c.sort_order, c.id",
    )
    .bind(restaurant_id)
    .fetch_all(pool)
    .await?;
    Ok(items
        .into_iter()
        .map(|(id, name)| ItemRef {
            options: options
                .iter()
                .filter(|(item, ..)| *item == id)
                .map(|(_, oid, oname, min, max)| OptionRef {
                    id: oid.clone(),
                    name: oname.clone(),
                    min: *min,
                    max: *max,
                    choices: choices
                        .iter()
                        .filter(|(opt, ..)| opt == oid)
                        .map(|(_, cid, cname)| ChoiceRef {
                            id: cid.clone(),
                            name: cname.clone(),
                        })
                        .collect(),
                })
                .collect(),
            id,
            name,
        })
        .collect())
}

/// The open run whose menu `text` names an item of, if any: what makes a group message an order.
pub async fn run_for_text(
    pool: &PgPool,
    user_id: &str,
    circle_id: Option<&str>,
    text: &str,
) -> ApiResult<Option<OpenRun>> {
    for run in orderable_runs(pool, user_id, circle_id).await? {
        let menu = menu_refs(pool, &run.restaurant_id).await?;
        if order_text::mentions_item(text, &menu) {
            return Ok(Some(run));
        }
    }
    Ok(None)
}

/// A draft waiting for its Place order.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Draft {
    pub id: i64,
    pub run_id: String,
    pub request: Value,
    pub total_cents: i64,
}

/// `user_id`'s newest pending draft in `space_id`, from the last [`DRAFT_MINUTES`].
pub async fn pending_draft(
    pool: &PgPool,
    user_id: &str,
    space_id: &str,
) -> ApiResult<Option<Draft>> {
    Ok(sqlx::query_as(
        "SELECT id, run_id, request, total_cents FROM ferry.order_drafts
          WHERE user_id = $1 AND space_id = $2 AND status = 'pending'
            AND created_at > now() - make_interval(mins => $3)
          ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(user_id)
    .bind(space_id)
    .bind(DRAFT_MINUTES)
    .fetch_optional(pool)
    .await?)
}

/// What the bot answers about a draft: the line, its buttons, and the draft when one was stored.
#[derive(Debug, Clone)]
pub struct DraftReply {
    /// The line, with [`DROPOFF`] where the drop-off goes: what Gemini may see.
    pub say: String,
    /// The drop-off as this chat shows it ([`dropoff_shown`]).
    pub dropoff: String,
    pub buttons: Vec<Button>,
    pub draft_id: Option<i64>,
    /// The total shown, when a draft was stored.
    pub total_cents: Option<i64>,
}

impl DraftReply {
    fn line(say: impl Into<String>) -> Self {
        Self {
            say: say.into(),
            dropoff: String::new(),
            buttons: Vec::new(),
            draft_id: None,
            total_cents: None,
        }
    }

    /// The line for Telegram, with the drop-off filled in.
    pub fn text(&self) -> String {
        self.say.replace(DROPOFF, &self.dropoff)
    }

    pub fn into_send(self, space_id: &str) -> SendRequest {
        SendRequest {
            space_id: space_id.to_owned(),
            text: self.text(),
            buttons: self.buttons,
        }
    }
}

/// Drafts an order on `run` from `text` for `user_id`, to place in `space_id`.
pub async fn draft_from_text(
    state: &AppState,
    user_id: &str,
    space_id: &str,
    is_group: bool,
    run: &OpenRun,
    text: &str,
) -> ApiResult<DraftReply> {
    let menu = menu_refs(&state.pool, &run.restaurant_id).await?;
    let lines = match order_text::parse_order(text, &menu) {
        ParseOutcome::Lines(lines) => lines,
        ParseOutcome::Ambiguous { item_ids, .. } => {
            let names: Vec<String> = item_ids
                .iter()
                .filter_map(|id| menu.iter().find(|i| i.id == *id))
                .map(|i| i.name.clone())
                .collect();
            return Ok(DraftReply::line(format!(
                "Which one: {}?",
                and_list(&names).replace(" and ", " or ")
            )));
        }
        ParseOutcome::MissingChoice { line, option_id } => {
            let item = menu.iter().find(|i| i.id == line.menu_item_id);
            let option = item.and_then(|i| i.options.iter().find(|o| o.id == option_id));
            return Ok(DraftReply::line(match (item, option) {
                (Some(item), Some(option)) => {
                    let choices: Vec<String> =
                        option.choices.iter().map(|c| c.name.clone()).collect();
                    format!(
                        "{}: which {}? {}.",
                        item.name,
                        option.name.to_lowercase(),
                        and_list(&choices).replace(" and ", " or ")
                    )
                }
                _ => "Which option would you like?".to_owned(),
            }));
        }
        ParseOutcome::NoItems => {
            return Ok(DraftReply::line(format!(
                "I couldn't find that on the {} menu.",
                run.restaurant
            )));
        }
    };
    let new = NewDraft {
        user_id,
        space_id,
        is_group,
        run,
        menu: &menu,
        lines: &lines,
        replaces: None,
    };
    price_and_store(state, &new).await
}

/// Edits `draft` from `text` ("add guac", "make it pinto beans"): Some(reply) when the text is an
/// edit of its choices, None when it is not (then it is a new message to answer as usual).
pub async fn edit_draft(
    state: &AppState,
    user_id: &str,
    space_id: &str,
    is_group: bool,
    draft: &Draft,
    text: &str,
) -> ApiResult<Option<DraftReply>> {
    let Some(run) = orderable_runs(&state.pool, user_id, None)
        .await?
        .into_iter()
        .find(|r| r.run_id == draft.run_id)
    else {
        return Ok(None);
    };
    let Ok(current) = serde_json::from_value::<Vec<ParsedLine>>(draft.request["lines"].clone())
    else {
        return Ok(None);
    };
    let menu = menu_refs(&state.pool, &run.restaurant_id).await?;
    let Some(lines) = order_text::parse_edit(text, &current, &menu) else {
        return Ok(None);
    };
    if lines == current {
        let items: Vec<String> = lines.iter().map(|l| describe_line(l, &menu)).collect();
        return Ok(Some(DraftReply::line(format!(
            "Your draft is already {}. Tap Place order when it's right.",
            and_list(&items)
        ))));
    }
    let new = NewDraft {
        user_id,
        space_id,
        is_group,
        run: &run,
        menu: &menu,
        lines: &lines,
        replaces: Some(draft.id),
    };
    Ok(Some(price_and_store(state, &new).await?))
}

/// Whether `text` is an edit of `draft`'s choices (no new item), without pricing anything.
pub async fn is_edit(state: &AppState, draft: &Draft, text: &str) -> ApiResult<bool> {
    let restaurant_id: Option<String> =
        sqlx::query_scalar("SELECT restaurant_id FROM ferry.runs WHERE id = $1")
            .bind(&draft.run_id)
            .fetch_optional(&state.pool)
            .await?;
    let Some(restaurant_id) = restaurant_id else {
        return Ok(false);
    };
    let Ok(current) = serde_json::from_value::<Vec<ParsedLine>>(draft.request["lines"].clone())
    else {
        return Ok(false);
    };
    let menu = menu_refs(&state.pool, &restaurant_id).await?;
    Ok(order_text::parse_edit(text, &current, &menu).is_some())
}

/// A draft to price and store.
struct NewDraft<'a> {
    user_id: &'a str,
    space_id: &'a str,
    is_group: bool,
    run: &'a OpenRun,
    menu: &'a [ItemRef],
    lines: &'a [ParsedLine],
    /// The pending draft this one replaces (an edit).
    replaces: Option<i64>,
}

/// Prices the draft's lines on its run with the user's drop-off and default tip, stores it
/// (replacing the edited draft only while that one is still pending), and returns the prompt with
/// Place order and Cancel.
async fn price_and_store(state: &AppState, new: &NewDraft<'_>) -> ApiResult<DraftReply> {
    let Some(user) = load_user(&state.pool, new.user_id).await? else {
        return Ok(DraftReply::line("I can't find your Ferry account."));
    };
    let Some(dropoff) = user.profile.dropoff_default.as_ref() else {
        return Ok(DraftReply::line(
            "Set your drop-off in the Ferry app first, so the runner knows where to bring it.",
        ));
    };
    let dropoff = Place {
        id: dropoff.id.clone(),
        label: dropoff.label.clone(),
        location: LatLng {
            lat: dropoff.location.lat,
            lng: dropoff.location.lng,
        },
        note: dropoff.note.clone(),
    };
    let request_json = json!({
        "lines": new.lines,
        "tip_cents": user.profile.default_tip_cents,
        "dropoff": dropoff,
    });
    let request: CreateOrderRequest =
        serde_json::from_value(request_json.clone()).map_err(ApiError::internal)?;
    let priced = match orders::price_order(state, new.user_id, &new.run.run_id, &request).await {
        Ok(priced) => priced,
        Err(err) => return Ok(DraftReply::line(refusal(&err))),
    };
    let quote = priced.quote;
    let mut tx = state.pool.begin().await?;
    if let Some(old) = new.replaces {
        // A Place order press may have claimed the old draft since it was read: then it stays
        // as it is and nothing new is stored, so there is never a second live draft.
        let replaced: Option<i64> = sqlx::query_scalar(
            "UPDATE ferry.order_drafts SET status = 'cancelled', decided_at = now()
              WHERE id = $1 AND status = 'pending'
             RETURNING id",
        )
        .bind(old)
        .fetch_optional(&mut *tx)
        .await?;
        if replaced.is_none() {
            tracing::info!(draft = old, "an edit lost the race to Place order");
            return Ok(DraftReply::line(EDIT_TOO_LATE));
        }
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO ferry.order_drafts (user_id, run_id, space_id, request, total_cents)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(new.user_id)
    .bind(&new.run.run_id)
    .bind(new.space_id)
    .bind(&request_json)
    .bind(quote.total_cents)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(draft = id, run_id = %new.run.run_id, "order drafted from text");
    let shown = dropoff_shown(
        &state.pool,
        priced.dropoff.id.as_deref(),
        &priced.dropoff.label,
        new.is_group,
    )
    .await?;
    Ok(DraftReply {
        say: draft_line(new.lines, new.menu, &new.run.restaurant, &quote),
        dropoff: shown,
        buttons: draft_buttons(id),
        draft_id: Some(id),
        total_cents: Some(quote.total_cents),
    })
}

/// "Chicken Burrito (No beans) from Cosmic Cantina: $8.99 + tax $0.67 + Ferry fee $1.50 + tip
/// $2.00 = $13.16, to {dropoff}. Place the order?"
fn draft_line(
    lines: &[ParsedLine],
    menu: &[ItemRef],
    restaurant: &str,
    quote: &OrderQuote,
) -> String {
    let items: Vec<String> = lines.iter().map(|l| describe_line(l, menu)).collect();
    let tip = if quote.tip_cents > 0 {
        format!(" + tip {}", dollars(quote.tip_cents))
    } else {
        String::new()
    };
    format!(
        "{} from {restaurant}: {} + tax {} + Ferry fee {}{tip} = {}, to {DROPOFF}. Place the order?",
        and_list(&items),
        dollars(quote.subtotal_cents),
        dollars(quote.tax_cents),
        dollars(quote.ferry_fee_cents),
        dollars(quote.total_cents),
    )
}

/// `user_id`'s pending draft in `space_id` shown again, priced now, with its Place order and
/// Cancel buttons: what "place it" or "yes" in words gets (Gemini's confirm_order, the fallback).
/// Nothing is placed. None when there is no pending draft.
pub async fn show_pending(
    state: &AppState,
    user_id: &str,
    space_id: &str,
    is_group: bool,
) -> ApiResult<Option<DraftReply>> {
    let Some(draft) = pending_draft(&state.pool, user_id, space_id).await? else {
        return Ok(None);
    };
    let restaurant: Option<(String, String)> = sqlx::query_as(
        "SELECT rest.id, rest.name FROM ferry.runs r
           JOIN ferry.restaurants rest ON rest.id = r.restaurant_id WHERE r.id = $1",
    )
    .bind(&draft.run_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some((restaurant_id, restaurant)) = restaurant else {
        return Ok(None);
    };
    let (Ok(lines), Ok(request)) = (
        serde_json::from_value::<Vec<ParsedLine>>(draft.request["lines"].clone()),
        serde_json::from_value::<CreateOrderRequest>(draft.request.clone()),
    ) else {
        return Ok(None);
    };
    let priced = match orders::price_order(state, user_id, &draft.run_id, &request).await {
        Ok(priced) => priced,
        Err(err) => {
            if err.code == ErrorCode::WindowClosed {
                sqlx::query(
                    "UPDATE ferry.order_drafts SET status = 'cancelled', decided_at = now()
                      WHERE id = $1 AND status = 'pending'",
                )
                .bind(draft.id)
                .execute(&state.pool)
                .await?;
            }
            return Ok(Some(DraftReply::line(refusal(&err))));
        }
    };
    if priced.quote.total_cents != draft.total_cents {
        // The total the bot shows is the one stored.
        sqlx::query(
            "UPDATE ferry.order_drafts SET total_cents = $2 WHERE id = $1 AND status = 'pending'",
        )
        .bind(draft.id)
        .bind(priced.quote.total_cents)
        .execute(&state.pool)
        .await?;
    }
    let menu = menu_refs(&state.pool, &restaurant_id).await?;
    let shown = dropoff_shown(
        &state.pool,
        priced.dropoff.id.as_deref(),
        &priced.dropoff.label,
        is_group,
    )
    .await?;
    Ok(Some(DraftReply {
        say: draft_line(&lines, &menu, &restaurant, &priced.quote),
        dropoff: shown,
        buttons: draft_buttons(draft.id),
        draft_id: Some(draft.id),
        total_cents: Some(priced.quote.total_cents),
    }))
}

/// "2× Chicken Burrito (No beans, Guacamole)".
pub fn describe_line(line: &ParsedLine, menu: &[ItemRef]) -> String {
    let Some(item) = menu.iter().find(|i| i.id == line.menu_item_id) else {
        return line.menu_item_id.clone();
    };
    let choices: Vec<&str> = line
        .choice_ids
        .iter()
        .filter_map(|id| {
            item.options
                .iter()
                .flat_map(|o| &o.choices)
                .find(|c| c.id == *id)
                .map(|c| c.name.as_str())
        })
        .collect();
    let qty = if line.qty > 1 {
        format!("{}× ", line.qty)
    } else {
        String::new()
    };
    if choices.is_empty() {
        format!("{qty}{}", item.name)
    } else {
        format!("{qty}{} ({})", item.name, choices.join(", "))
    }
}

/// A refusal from orders, in the bot's words. Only for refusals that came before an order
/// committed: [`settle`] sends it only for those ([`refused_before_commit`]) or after the orders
/// table showed nothing committed.
fn refusal(err: &ApiError) -> String {
    match err.code {
        ErrorCode::WindowClosed => "Too late: that Ferry stopped taking orders.".to_owned(),
        ErrorCode::InsufficientFunds => {
            "Your Ferry balance is short for this order. Top up in the app, then tap Place order again."
                .to_owned()
        }
        ErrorCode::Forbidden | ErrorCode::NotFound | ErrorCode::Conflict => err.message.clone(),
        ErrorCode::ValidationFailed => format!("That order didn't work: {}", err.message),
        _ => "Ferry couldn't price that just now. Try again in a moment.".to_owned(),
    }
}

// ---------- Placing ----------

/// What placing a draft said, and the order when one was placed.
#[derive(Debug, Clone)]
pub struct Placed {
    /// The line, with `{runner}` and [`DROPOFF`] where the runner's name and the drop-off go:
    /// [`Placed::say`] fills them for Telegram.
    pub template: String,
    /// The runner's name, or "The runner" when they have none.
    pub runner: String,
    /// The drop-off as this chat shows it.
    pub dropoff: String,
    pub buttons: Vec<Button>,
    /// The order, when one was placed (now or by an earlier press).
    pub order_id: Option<String>,
}

impl Placed {
    fn line(say: &str) -> Self {
        Self {
            template: say.to_owned(),
            runner: String::new(),
            dropoff: String::new(),
            buttons: Vec::new(),
            order_id: None,
        }
    }

    /// The line for Telegram, with the runner's name and the drop-off in it.
    pub fn say(&self) -> String {
        self.template
            .replace("{runner}", &self.runner)
            .replace(DROPOFF, &self.dropoff)
    }
}

#[derive(sqlx::FromRow)]
struct DraftState {
    user_id: String,
    status: String,
}

/// Places draft `id` for `user_id` (only their own pending draft), the one way an order is placed
/// from Telegram: a press of its Place order button. Claims the draft (`pending → placing`,
/// committed), then places the order on a task of its own ([`settle`] finishes it). A press that
/// finds the claim taken says so and places nothing. Err only when the claim itself could not be
/// read or made, so nothing was placed.
pub async fn place_draft(
    state: &AppState,
    user_id: &str,
    id: i64,
    is_group: bool,
) -> ApiResult<Placed> {
    let claimed: Option<(String, Value)> = sqlx::query_as(
        "UPDATE ferry.order_drafts SET status = 'placing', decided_at = now()
          WHERE id = $1 AND user_id = $2 AND status = 'pending'
         RETURNING run_id, request",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some((run_id, request)) = claimed else {
        return not_claimed(state, user_id, id, is_group).await;
    };
    tracing::info!(draft = id, "draft claimed for placing");
    let request: CreateOrderRequest = match serde_json::from_value(request) {
        Ok(request) => request,
        Err(_) => {
            release(&state.pool, id, true).await?;
            return Ok(Placed::line(
                "That draft can't be placed; send your order again.",
            ));
        }
    };
    // The money step runs on its own task: a caller that gives up (a timeout, a dropped future)
    // never stops it between the order's commit and storing the order id.
    let task = {
        let state = state.clone();
        let user_id = user_id.to_owned();
        tokio::spawn(async move {
            let placed = orders::place_order(&state, &user_id, &run_id, &request)
                .await
                .map(|order| order.id);
            settle(&state, id, placed, is_group).await
        })
    };
    Ok(match task.await {
        Ok(placed) => placed,
        Err(err) => {
            // A panic: the order's transaction rolled back unless it had committed.
            tracing::error!(draft = id, "placing a draft panicked: {err}");
            settle(
                state,
                id,
                Err(ApiError::internal(std::io::Error::other(
                    "placing panicked",
                ))),
                is_group,
            )
            .await
        }
    })
}

/// Why a press of Place order on draft `id` claimed nothing.
async fn not_claimed(
    state: &AppState,
    user_id: &str,
    id: i64,
    is_group: bool,
) -> ApiResult<Placed> {
    let row: Option<DraftState> =
        sqlx::query_as("SELECT user_id, status FROM ferry.order_drafts WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?;
    let Some(row) = row else {
        return Ok(Placed::line("That order draft is gone."));
    };
    if row.user_id != user_id {
        return Ok(Placed::line(
            "Only the person who drafted that order can place it.",
        ));
    }
    Ok(match row.status.as_str() {
        "placed" => Placed::line("That order is placed already."),
        // Another press holds the claim. If its order already committed, this press can say so;
        // while another of the user's drafts on that run is placing too, it can't tell whose.
        "placing" => match committed_order(&state.pool, id).await {
            Ok(Committed::Order(order_id)) => {
                let placed = finish_placed(state, id, &order_id, is_group).await;
                Placed {
                    template: "That order is placed already.".to_owned(),
                    ..placed
                }
            }
            _ => Placed::line(PLACING_NOW),
        },
        _ => Placed::line("That draft was cancelled; send your order again."),
    })
}

/// Whether `orders::place_order` raises `code` only before its order commits. Its refusals (the
/// run's lock and access, the window, pricing, the 402 hold) all come before `tx.commit()`; after
/// it only `get_order` can fail. 404 is not here: `get_order` can return it after the commit.
fn refused_before_commit(code: ErrorCode) -> bool {
    matches!(
        code,
        ErrorCode::InsufficientFunds
            | ErrorCode::WindowClosed
            | ErrorCode::ValidationFailed
            | ErrorCode::Forbidden
            | ErrorCode::Conflict
    )
}

/// Finishes claimed draft `id` once placing returned: stores the order id; on a refusal that comes
/// only before the commit (402, 409, 400, 403), releases the claim and says why; and on any other
/// failure finds out whether the order committed anyway (then it is placed, and the user is never
/// told to try again) or not (then the claim is released). Released means back to pending, or
/// cancelled when the window closed. Public for the test of a failure after the commit.
pub async fn settle(
    state: &AppState,
    id: i64,
    placed: ApiResult<String>,
    is_group: bool,
) -> Placed {
    let refused = match placed {
        Ok(order_id) => return finish_placed(state, id, &order_id, is_group).await,
        Err(err) => err,
    };
    if refused_before_commit(refused.code) {
        // Nothing committed, so no order is looked up: another draft's order is never this one's.
        return release_refused(state, id, &refused).await;
    }
    match committed_order(&state.pool, id).await {
        Ok(Committed::Order(order_id)) => {
            tracing::warn!(draft = id, code = ?refused.code, "placing failed after the order committed; it stands");
            finish_placed(state, id, &order_id, is_group).await
        }
        Ok(Committed::Nothing) => release_refused(state, id, &refused).await,
        Ok(Committed::Unsure) => {
            // Another draft of this user on this run is placing too: the order found may be its.
            tracing::warn!(
                draft = id,
                code = ?refused.code,
                "placing failed while another draft of the user's on the run was placing"
            );
            Placed::line(PLACING_UNSURE)
        }
        Err(err) => {
            // Unknown: the claim stays, so no press can place it again.
            tracing::warn!(draft = id, code = ?err.code, "could not tell whether a draft's order committed");
            Placed::line(PLACING_UNSURE)
        }
    }
}

/// Releases claimed draft `id` after `refused` (nothing committed) and says why.
async fn release_refused(state: &AppState, id: i64, refused: &ApiError) -> Placed {
    let cancel = refused.code == ErrorCode::WindowClosed;
    if let Err(err) = release(&state.pool, id, cancel).await {
        tracing::warn!(draft = id, code = ?err.code, "releasing a draft's claim failed");
        return Placed::line(PLACING_UNSURE);
    }
    tracing::info!(draft = id, code = ?refused.code, "placing a drafted order was refused");
    Placed::line(&refusal(refused))
}

/// What the orders table says about claimed draft `id`'s order.
#[derive(Debug, PartialEq, Eq)]
enum Committed {
    /// Its order: the draft already links it, or the one order since the claim.
    Order(String),
    /// No order since the claim, or the draft is no longer claimed.
    Nothing,
    /// An order since the claim, but another draft of the same user on the same run is placing too,
    /// so it may be that one's.
    Unsure,
}

/// The order that claimed draft `id` placed, if one committed: the draft's customer's order on its
/// run created since the claim, that no draft links to yet. One statement, so one snapshot: an
/// order of another draft's claim on the same run is visible only with that claim, and then the
/// answer is [`Committed::Unsure`].
async fn committed_order(pool: &PgPool, id: i64) -> ApiResult<Committed> {
    let row: Option<(String, Option<String>, bool, Option<String>)> = sqlx::query_as(
        "SELECT d.status, d.order_id,
                EXISTS (SELECT 1 FROM ferry.order_drafts x
                         WHERE x.user_id = d.user_id AND x.run_id = d.run_id
                           AND x.status = 'placing' AND x.id <> d.id),
                (SELECT o.id FROM ferry.orders o
                  WHERE o.customer_id = d.user_id AND o.run_id = d.run_id
                    AND o.created_at >= d.decided_at
                    AND NOT EXISTS (SELECT 1 FROM ferry.order_drafts x WHERE x.order_id = o.id)
                  ORDER BY o.created_at, o.id
                  LIMIT 1)
           FROM ferry.order_drafts d
          WHERE d.id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        Some((status, Some(linked), ..)) if status == "placed" => Committed::Order(linked),
        Some((status, _, shared, Some(found))) if status == "placing" => {
            if shared {
                Committed::Unsure
            } else {
                Committed::Order(found)
            }
        }
        _ => Committed::Nothing,
    })
}

/// Gives up draft `id`'s claim: back to pending (to press again), or cancelled.
async fn release(pool: &PgPool, id: i64, cancel: bool) -> ApiResult<()> {
    sqlx::query(
        "UPDATE ferry.order_drafts
            SET status = CASE WHEN $2 THEN 'cancelled' ELSE 'pending' END,
                decided_at = CASE WHEN $2 THEN now() END
          WHERE id = $1 AND status = 'placing'",
    )
    .bind(id)
    .bind(cancel)
    .execute(pool)
    .await?;
    Ok(())
}

/// Stores `order_id` on claimed draft `id` and returns the "Order placed" line. The order stands
/// whatever happens here, so a failure is logged and the line still says it is placed.
async fn finish_placed(state: &AppState, id: i64, order_id: &str, is_group: bool) -> Placed {
    let mark = || {
        sqlx::query(
            "UPDATE ferry.order_drafts SET status = 'placed', order_id = $2, decided_at = now()
              WHERE id = $1 AND status = 'placing'",
        )
        .bind(id)
        .bind(order_id)
        .execute(&state.pool)
    };
    if let Err(first) = mark().await {
        tracing::warn!(
            draft = id,
            "storing a placed draft's order failed once: {first}"
        );
        if let Err(err) = mark().await {
            tracing::error!(draft = id, "a placed draft keeps its claim: {err}");
        }
    }
    tracing::info!(draft = id, order_id, "drafted order placed");
    match placed_line(state, order_id, is_group).await {
        Ok(placed) => placed,
        Err(_) => Placed {
            order_id: Some(order_id.to_owned()),
            ..Placed::line("Order placed. It's in your orders in the Ferry app.")
        },
    }
}

#[derive(sqlx::FromRow)]
struct PlacedRow {
    total_cents: i64,
    dropoff_place_id: Option<String>,
    dropoff_label: String,
    runner: String,
}

/// "Order placed: $13.16 is held from your Ferry balance. {runner} brings it to {dropoff}.", with
/// a Track it button.
async fn placed_line(state: &AppState, order_id: &str, is_group: bool) -> ApiResult<Placed> {
    let row: PlacedRow = sqlx::query_as(
        "SELECT o.total_cents, o.dropoff_place_id, o.dropoff_label, u.display_name AS runner
           FROM ferry.orders o
           JOIN ferry.runs r ON r.id = o.run_id
           JOIN ferry.users u ON u.id = r.runner_id
          WHERE o.id = $1",
    )
    .bind(order_id)
    .fetch_one(&state.pool)
    .await?;
    let dropoff = dropoff_shown(
        &state.pool,
        row.dropoff_place_id.as_deref(),
        &row.dropoff_label,
        is_group,
    )
    .await?;
    let url = format!("{}/o/{order_id}", web_url_of(&state.config));
    Ok(Placed {
        template: format!(
            "Order placed: {} is held from your Ferry balance. {{runner}} brings it to {DROPOFF}.",
            dollars(row.total_cents)
        ),
        runner: match row.runner.trim() {
            "" => "The runner".to_owned(),
            name => name.to_owned(),
        },
        dropoff,
        buttons: link_button("Track it", &url).into_iter().collect(),
        order_id: Some(order_id.to_owned()),
    })
}

/// Cancels draft `id` for `user_id` (only their own pending one).
pub async fn cancel_draft(state: &AppState, user_id: &str, id: i64) -> ApiResult<String> {
    let cancelled = sqlx::query(
        "UPDATE ferry.order_drafts SET status = 'cancelled', decided_at = now()
          WHERE id = $1 AND user_id = $2 AND status = 'pending'",
    )
    .bind(id)
    .bind(user_id)
    .execute(&state.pool)
    .await?
    .rows_affected();
    Ok(if cancelled > 0 {
        "OK, I dropped that order. Nothing was charged.".to_owned()
    } else {
        "There's no open draft of yours to drop.".to_owned()
    })
}

// ---------- Cancelling ----------

/// What "cancel my order" gets, in words: the user's pending draft in this chat and their orders
/// that can still be cancelled (funded), newest first, each with its own button. Nothing is
/// cancelled until they tap one. `order_id` narrows it to that order.
pub async fn cancel_choices(
    state: &AppState,
    user_id: &str,
    space_id: &str,
    order_id: Option<&str>,
) -> ApiResult<DraftReply> {
    let mut named = Vec::new();
    let mut buttons = Vec::new();
    if order_id.is_none()
        && let Some(draft) = pending_draft(&state.pool, user_id, space_id).await?
    {
        let restaurant: String = sqlx::query_scalar(
            "SELECT rest.name FROM ferry.runs r
               JOIN ferry.restaurants rest ON rest.id = r.restaurant_id WHERE r.id = $1",
        )
        .bind(&draft.run_id)
        .fetch_one(&state.pool)
        .await?;
        named.push(format!(
            "your {restaurant} draft ({}, not placed yet)",
            dollars(draft.total_cents)
        ));
        buttons.push(Button::callback(
            &format!("Drop the {restaurant} draft"),
            button_data(draft.id, false),
        ));
    }
    let orders: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT o.id, rest.name, o.total_cents FROM ferry.orders o
           JOIN ferry.runs r ON r.id = o.run_id
           JOIN ferry.restaurants rest ON rest.id = r.restaurant_id
          WHERE o.customer_id = $1 AND o.status = 'funded' AND NOT o.is_demo_history
            AND ($2::text IS NULL OR o.id = $2)
          ORDER BY o.created_at DESC, o.id DESC
          LIMIT $3",
    )
    .bind(user_id)
    .bind(order_id)
    .bind(MAX_CANCEL_CHOICES)
    .fetch_all(&state.pool)
    .await?;
    for (id, restaurant, total) in &orders {
        named.push(format!("your {restaurant} order ({})", dollars(*total)));
        buttons.push(Button::callback(
            &format!("Cancel {restaurant} order"),
            cancel_button_data(id),
        ));
    }
    let say = match named.as_slice() {
        [] => return Ok(DraftReply::line(no_cancel_line(state, user_id).await?)),
        [one] => format!(
            "Tap the button to cancel {one}. Nothing is cancelled until you do, and a cancelled order comes back to your Ferry balance in full."
        ),
        several => format!(
            "Which one? Tap the button for {}. Nothing is cancelled until you do.",
            and_list(several).replace(" and ", " or ")
        ),
    };
    Ok(DraftReply {
        buttons,
        ..DraftReply::line(say)
    })
}

/// Why there is nothing to cancel: an order already bought, or none at all.
async fn no_cancel_line(state: &AppState, user_id: &str) -> ApiResult<String> {
    let bought: Option<(String, String)> = sqlx::query_as(
        "SELECT rest.name, o.status FROM ferry.orders o
           JOIN ferry.runs r ON r.id = o.run_id
           JOIN ferry.restaurants rest ON rest.id = r.restaurant_id
          WHERE o.customer_id = $1 AND o.status IN ('purchased', 'picked_up')
            AND NOT o.is_demo_history
          ORDER BY o.created_at DESC, o.id DESC LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await?;
    Ok(match bought {
        Some((restaurant, status)) => format!(
            "Your {restaurant} order is {}, so it can't be cancelled any more.",
            status_words(&status)
        ),
        None => "You have no order to cancel.".to_owned(),
    })
}

/// A press of a placed order's Cancel (`cancel:<order id>`) by `user_id`: only its customer's
/// press cancels it, through orders' cancel (the whole amount back to their balance).
pub async fn cancel_placed(state: &AppState, user_id: &str, order_id: &str) -> ApiResult<String> {
    let order: Option<(String, String)> = sqlx::query_as(
        "SELECT o.customer_id, rest.name FROM ferry.orders o
           JOIN ferry.runs r ON r.id = o.run_id
           JOIN ferry.restaurants rest ON rest.id = r.restaurant_id
          WHERE o.id = $1",
    )
    .bind(order_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some((customer, restaurant)) = order.filter(|(customer, _)| customer == user_id) else {
        return Ok("Only the person who placed that order can cancel it.".to_owned());
    };
    debug_assert_eq!(customer, user_id);
    match orders::cancel_order(state, user_id, order_id).await {
        Ok(order) => Ok(format!(
            "Cancelled your {restaurant} order: {} is back in your Ferry balance.",
            dollars(order.total_cents)
        )),
        Err(err) if err.code == ErrorCode::Conflict => {
            let status: String =
                sqlx::query_scalar("SELECT status FROM ferry.orders WHERE id = $1")
                    .bind(order_id)
                    .fetch_one(&state.pool)
                    .await?;
            Ok(format!(
                "That {restaurant} order is {}, so it can't be cancelled now.",
                status_words(&status)
            ))
        }
        Err(err) => Err(err),
    }
}

// ---------- Buttons ----------

/// A press of a draft's Place order or Cancel, or of a placed order's Cancel, by `user_id` in
/// `space_id`. Returns what to send and what was said. Place order's line is sent by the task that
/// places, so no time budget of the caller stands between the order and its line; it comes back
/// only as what was said.
pub async fn on_button(
    state: &AppState,
    space_id: &str,
    user_id: &str,
    data: &str,
    is_group: bool,
) -> ApiResult<(Vec<SendRequest>, Option<String>)> {
    let line = |text: String, buttons: Vec<Button>| SendRequest {
        space_id: space_id.to_owned(),
        text,
        buttons,
    };
    if let Some(order_id) = parse_cancel_button(data) {
        let text = cancel_placed(state, user_id, order_id).await?;
        return Ok((vec![line(text, Vec::new())], None));
    }
    let Some((id, place)) = parse_button(data) else {
        return Ok((Vec::new(), None));
    };
    if !place {
        let text = cancel_draft(state, user_id, id).await?;
        return Ok((vec![line(text, Vec::new())], None));
    }
    let task = {
        let state = state.clone();
        let (space_id, user_id) = (space_id.to_owned(), user_id.to_owned());
        tokio::spawn(async move {
            let send = match place_draft(&state, &user_id, id, is_group).await {
                Ok(placed) => SendRequest {
                    space_id,
                    text: placed.say(),
                    buttons: placed.buttons,
                },
                Err(err) => {
                    // The claim was never made: nothing was placed.
                    tracing::warn!(draft = id, code = ?err.code, "a Place order press failed before its claim");
                    SendRequest {
                        space_id,
                        text: "Ferry couldn't place that just now. Nothing was charged; tap Place order again in a moment.".to_owned(),
                        buttons: Vec::new(),
                    }
                }
            };
            super::dispatch::deliver(&state, std::slice::from_ref(&send)).await;
            send.text
        })
    };
    let said = task.await.ok();
    Ok((Vec::new(), said))
}

/// `user_id`'s newest order that is still active (funded, purchased or picked up), or else their
/// newest order: the one "where's my order" means.
pub async fn latest_order(
    pool: &PgPool,
    user_id: &str,
    active_only: bool,
) -> ApiResult<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM ferry.orders
          WHERE customer_id = $1 AND NOT is_demo_history
            AND (NOT $2 OR status IN ('funded', 'purchased', 'picked_up'))
          ORDER BY (status IN ('funded', 'purchased', 'picked_up')) DESC, created_at DESC, id DESC
          LIMIT 1",
    )
    .bind(user_id)
    .bind(active_only)
    .fetch_optional(pool)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_round_trip() {
        for place in [true, false] {
            let data = button_data(7, place);
            assert!(data.len() <= 64);
            assert_eq!(parse_button(&data), Some((7, place)));
        }
        for bad in [
            "order:0:place",
            "order:x:place",
            "order:1:yes",
            "trip:1:place",
            "",
        ] {
            assert_eq!(parse_button(bad), None, "{bad}");
        }
    }

    #[test]
    fn lines_are_described_from_the_menu() {
        let menu = vec![ItemRef {
            id: "itm_b".into(),
            name: "Chicken Burrito".into(),
            options: vec![OptionRef {
                id: "opt_beans".into(),
                name: "Beans".into(),
                min: 1,
                max: 1,
                choices: vec![ChoiceRef {
                    id: "cho_none".into(),
                    name: "No beans".into(),
                }],
            }],
        }];
        let line = ParsedLine {
            menu_item_id: "itm_b".into(),
            qty: 1,
            choice_ids: vec!["cho_none".into()],
        };
        assert_eq!(describe_line(&line, &menu), "Chicken Burrito (No beans)");
        let two = ParsedLine { qty: 2, ..line };
        assert_eq!(describe_line(&two, &menu), "2× Chicken Burrito (No beans)");
    }
}
