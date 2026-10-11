//! The agent's tools (spec section 3), written in Rust. Gemini's function declarations are
//! generated from [`Tool`]'s definitions ([`declarations`]), and the deterministic fallback calls
//! the same functions.
//!
//! Each tool returns a [`ToolOutput`]: `result` is what Gemini sees (ids, labels, numbers and
//! restaurant names; people only as placeholders such as `{runner}`), and `say` is the answer in
//! Ferry's words, also in `result`, which Gemini is told to repeat and the fallback sends. Facts,
//! totals, ETAs, permissions and money come from this code, never from Gemini.
//!
//! Phase A's tools need no runs-orders: get_profile, list_restaurants, get_menu, get_wallet,
//! explain_impact, get_eta, start_meet_again and respond_invite. Phase B's go through runs-orders'
//! functions, with exactly the endpoints' rules: list_open_runs, open_run, draft_order_from_text,
//! confirm_order, cancel_order and get_order_status. No tool moves money: confirm_order shows the
//! user's draft again with its Place order button, and cancel_order shows what can be cancelled,
//! each with its own button. Only the user's press of a button places or cancels an order.

use serde_json::{Map, Value, json};

use super::patterns::{self, PlaceMatch, match_place};
use super::turn::Turn;
use super::{meet, ordering, trips};
use crate::catalog::handlers::{self, RestaurantFilter};
use crate::catalog::public_config::DEFAULT_WEB_URL;
use crate::catalog::types::{Menu, Restaurant};
use crate::circles;
use crate::error::{ApiError, ApiResult, ErrorCode};
use crate::extract::{Path, Query};
use crate::impact::{self, ImpactScope};
use crate::invites::{self, InviteRecord, NewInvite};
use crate::jwt::AuthUser;
use crate::messaging::sidecar::Button;
use crate::orders;
use crate::payments::{Payments, wallet};
use crate::profile::load_user;
use crate::tracking;
use axum::extract::State;

/// A parameter's JSON type in a declaration (Gemini's OpenAPI subset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    String,
    Boolean,
    /// A string from a fixed list.
    Enum(&'static [&'static str]),
}

/// One parameter of a tool.
#[derive(Debug, Clone, Copy)]
pub struct Param {
    pub name: &'static str,
    pub kind: Kind,
    pub description: &'static str,
    pub required: bool,
}

/// Every tool the agent has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    GetProfile,
    ListRestaurants,
    GetMenu,
    GetWallet,
    ExplainImpact,
    GetEta,
    StartMeetAgain,
    RespondInvite,
    ListOpenRuns,
    OpenRun,
    DraftOrderFromText,
    ConfirmOrder,
    CancelOrder,
    GetOrderStatus,
}

impl Tool {
    pub const ALL: [Tool; 14] = [
        Tool::GetProfile,
        Tool::ListRestaurants,
        Tool::GetMenu,
        Tool::GetWallet,
        Tool::ExplainImpact,
        Tool::GetEta,
        Tool::StartMeetAgain,
        Tool::RespondInvite,
        Tool::ListOpenRuns,
        Tool::OpenRun,
        Tool::DraftOrderFromText,
        Tool::ConfirmOrder,
        Tool::CancelOrder,
        Tool::GetOrderStatus,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Tool::GetProfile => "get_profile",
            Tool::ListRestaurants => "list_restaurants",
            Tool::GetMenu => "get_menu",
            Tool::GetWallet => "get_wallet",
            Tool::ExplainImpact => "explain_impact",
            Tool::GetEta => "get_eta",
            Tool::StartMeetAgain => "start_meet_again",
            Tool::RespondInvite => "respond_invite",
            Tool::ListOpenRuns => "list_open_runs",
            Tool::OpenRun => "open_run",
            Tool::DraftOrderFromText => "draft_order_from_text",
            Tool::ConfirmOrder => "confirm_order",
            Tool::CancelOrder => "cancel_order",
            Tool::GetOrderStatus => "get_order_status",
        }
    }

    /// Whether the tool changes something (an invite, an answer, a run, a draft), rather than
    /// only reading. None moves money.
    pub fn changes_something(self) -> bool {
        matches!(
            self,
            Tool::StartMeetAgain | Tool::RespondInvite | Tool::OpenRun | Tool::DraftOrderFromText
        )
    }

    pub fn from_name(name: &str) -> Option<Tool> {
        Tool::ALL.into_iter().find(|t| t.name() == name)
    }

    pub fn description(self) -> &'static str {
        match self {
            Tool::GetProfile => {
                "The user's saved Ferry defaults: drop-off place, diet, foods to avoid, favorite cuisines and restaurants, usual times, walking pace and default tip."
            }
            Tool::ListRestaurants => {
                "Restaurants Ferry runs go to, with whether each is open now, today's hours and the walk from the user's drop-off. Use for \"what's open\" or \"where can we get X\"."
            }
            Tool::GetMenu => {
                "A restaurant's menu: items with prices and their options. Prices are estimates when the result says so."
            }
            Tool::GetWallet => {
                "The user's Ferry balance and latest transactions (Nessie sandbox). Use for \"what do I owe\", balance and money questions."
            }
            Tool::ExplainImpact => {
                "CO2 saved by walking deliveries instead of car deliveries (EPA estimate), for the user, their circle or the whole campus."
            }
            Tool::GetEta => {
                "Where the user's food is: the runner's distance, minutes away and how old the location is. Use for \"how long until my food gets here\"."
            }
            Tool::StartMeetAgain => {
                "Pings people to meet up, at a place and a time when the user names them: in a group, the group's members; in a DM, the people from the user's most recent circle. Each gets a Telegram message (\"<user> wants to go to Cosmic Cantina at 7:00 PM. You in?\") with I'm in and Can't make it buttons, and a notification in the app; every answer comes back to the user. Use for \"anyone want to go to Cosmic at 7?\", \"ask the people from last time if they want Sup Dogs at 8 tomorrow\" and \"let's meet with the people I hung out with last time\"."
            }
            Tool::RespondInvite => {
                "Accepts or declines a meet-up invite the user received. Without invite_id it answers the user's latest open invite."
            }
            Tool::ListOpenRuns => {
                "Ferry runs taking orders now (in this group, or in the user's circles in a DM): restaurant, runner, when orders close and how many orders."
            }
            Tool::OpenRun => {
                "Opens a Ferry run with the user as the runner, so friends in the circle can order until the window closes. Use when the user says they are going to a restaurant and wants to take orders."
            }
            Tool::DraftOrderFromText => {
                "Turns the user's order words (\"chicken burrito no beans\") into a priced draft on an open run, shown with Place order and Cancel buttons. Nothing is charged until the user taps Place order."
            }
            Tool::ConfirmOrder => {
                "Shows the user's pending order draft again, with its total and the Place order button. It never places the order: only the user's tap on Place order does. Use when the user says yes, place it, or asks about their draft."
            }
            Tool::CancelOrder => {
                "Shows the user's orders that can still be cancelled, each with a Cancel button. It never cancels anything: only the user's tap does, and a cancelled order comes back to their Ferry balance in full."
            }
            Tool::GetOrderStatus => {
                "The status of the user's latest order, with the runner's distance, minutes away and how old the location is when known."
            }
        }
    }

    pub fn params(self) -> &'static [Param] {
        match self {
            Tool::GetProfile | Tool::GetWallet | Tool::GetEta | Tool::ListOpenRuns => &[],
            Tool::OpenRun => &[
                Param {
                    name: "restaurant",
                    kind: Kind::String,
                    description: "The restaurant's id (rst_...) or its name.",
                    required: true,
                },
                Param {
                    name: "when",
                    kind: Kind::String,
                    description: "When the user leaves, in their words: \"6:30pm\", \"at 7\", \"in 20 min\", \"now\". Defaults to 15 minutes from now.",
                    required: false,
                },
            ],
            Tool::DraftOrderFromText => &[
                Param {
                    name: "text",
                    kind: Kind::String,
                    description: "The order in the user's words, such as \"2 chicken burritos with guac\".",
                    required: true,
                },
                Param {
                    name: "run_id",
                    kind: Kind::String,
                    description: "The run (run_...) to order on, when known from list_open_runs.",
                    required: false,
                },
            ],
            Tool::ConfirmOrder => &[],
            Tool::CancelOrder | Tool::GetOrderStatus => &[Param {
                name: "order_id",
                kind: Kind::String,
                description: "The order (ord_...), when known; defaults to the user's latest order.",
                required: false,
            }],
            Tool::ListRestaurants => &[
                Param {
                    name: "q",
                    kind: Kind::String,
                    description: "Part of a restaurant name or a cuisine, such as \"mexican\".",
                    required: false,
                },
                Param {
                    name: "open_now",
                    kind: Kind::Boolean,
                    description: "Only restaurants open now.",
                    required: false,
                },
            ],
            Tool::GetMenu => &[Param {
                name: "restaurant",
                kind: Kind::String,
                description: "The restaurant's id (rst_...) or its name.",
                required: true,
            }],
            Tool::ExplainImpact => &[Param {
                name: "scope",
                kind: Kind::Enum(&["me", "circle", "campus"]),
                description: "Whose impact: the user (me), their circle, or the whole campus. Defaults to the circle in a group and to the user in a DM.",
                required: false,
            }],
            Tool::StartMeetAgain => &[
                Param {
                    name: "place",
                    kind: Kind::String,
                    description: "Where to go, in the user's words or a restaurant's id (rst_...): \"Cosmic\", \"Sup Dogs\", \"the Pit\". Leave it out when the user named no place.",
                    required: false,
                },
                Param {
                    name: "when",
                    kind: Kind::String,
                    description: "When, in the user's words: \"at 7\", \"8pm\", \"8 tomorrow\", \"in 30 min\". Never a date or a timestamp. Leave it out when the user named no time.",
                    required: false,
                },
                Param {
                    name: "message",
                    kind: Kind::String,
                    description: "A short note for the invite in the user's words, only when they asked to add one.",
                    required: false,
                },
            ],
            Tool::RespondInvite => &[
                Param {
                    name: "accept",
                    kind: Kind::Boolean,
                    description: "true to accept, false to decline.",
                    required: true,
                },
                Param {
                    name: "invite_id",
                    kind: Kind::String,
                    description: "The invite's id (inv_...), when known.",
                    required: false,
                },
            ],
        }
    }

    /// The Gemini function declaration, generated from the definition above: `{name,
    /// description, parameters}` with a flat object schema (no `$ref`, no
    /// `additionalProperties`), and no `parameters` for a tool that takes none.
    pub fn declaration(self) -> Value {
        let mut declaration = json!({"name": self.name(), "description": self.description()});
        let params = self.params();
        if !params.is_empty() {
            let mut properties = Map::new();
            for p in params {
                let schema = match p.kind {
                    Kind::String => json!({"type": "string", "description": p.description}),
                    Kind::Boolean => json!({"type": "boolean", "description": p.description}),
                    Kind::Enum(values) => {
                        json!({"type": "string", "enum": values, "description": p.description})
                    }
                };
                properties.insert(p.name.to_owned(), schema);
            }
            let required: Vec<&str> = params
                .iter()
                .filter(|p| p.required)
                .map(|p| p.name)
                .collect();
            declaration["parameters"] =
                json!({"type": "object", "properties": properties, "required": required});
        }
        declaration
    }
}

/// Every tool's declaration, for `tools: [{functionDeclarations}]`.
pub fn declarations() -> Vec<Value> {
    Tool::ALL.iter().map(|t| t.declaration()).collect()
}

/// What a tool produced.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub ok: bool,
    /// What Gemini sees: no names, no addresses. Holds `say` when there is one.
    pub result: Value,
    /// The answer in Ferry's words, with placeholders for people.
    pub say: Option<String>,
}

impl ToolOutput {
    fn ok(mut result: Value, say: String) -> Self {
        result["say"] = Value::String(say.clone());
        Self {
            ok: true,
            result,
            say: Some(say),
        }
    }

    /// A refusal or failure the user should hear about, such as "no order on the way".
    fn error(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            ok: false,
            result: json!({"error": message, "say": message}),
            say: Some(message),
        }
    }
}

/// Runs `tool` for the turn's user. Bad arguments and failures become an error result, never a
/// panic, so Gemini can recover.
pub async fn run(turn: &mut Turn, tool: Tool, args: &Value) -> ToolOutput {
    let args = match args {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    let outcome = match tool {
        Tool::GetProfile => get_profile(turn).await,
        Tool::ListRestaurants => list_restaurants(turn, &args).await,
        Tool::GetMenu => get_menu(turn, &args).await,
        Tool::GetWallet => get_wallet(turn).await,
        Tool::ExplainImpact => explain_impact(turn, &args).await,
        Tool::GetEta => get_eta(turn).await,
        Tool::StartMeetAgain => start_meet_again(turn, &args).await,
        Tool::RespondInvite => respond_invite(turn, &args).await,
        Tool::ListOpenRuns => list_open_runs(turn).await,
        Tool::OpenRun => open_run(turn, &args).await,
        Tool::DraftOrderFromText => draft_order_from_text(turn, &args).await,
        Tool::ConfirmOrder => confirm_order(turn).await,
        Tool::CancelOrder => cancel_order(turn, &args).await,
        Tool::GetOrderStatus => get_order_status(turn, &args).await,
    };
    outcome.unwrap_or_else(|err| {
        tracing::warn!(tool = tool.name(), code = ?err.code, "agent tool failed");
        ToolOutput::error(match err.code {
            ErrorCode::NotFound | ErrorCode::Forbidden | ErrorCode::Conflict => err.message,
            ErrorCode::ValidationFailed => format!("That didn't work: {}", err.message),
            _ => "Ferry couldn't look that up just now. Try again in a moment.".to_owned(),
        })
    })
}

// ---------- Formatting ----------

/// Integer cents as dollars: `899` is `$8.99`, `-150` is `-$1.50`.
pub fn dollars(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.unsigned_abs();
    format!("{sign}${}.{:02}", abs / 100, abs % 100)
}

/// Meters as miles with one decimal: `483` is `0.3 mi`.
pub fn miles(meters: f64) -> String {
    format!("{:.1} mi", meters / 1609.344)
}

/// "A", "A and B", "A, B and C".
pub fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

fn string_arg<'a>(args: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn web_url(turn: &Turn) -> String {
    web_url_of(&turn.state.config)
}

/// `PUBLIC_WEB_URL`, or the dev default.
pub fn web_url_of(config: &crate::config::Config) -> String {
    config
        .public_web_url
        .clone()
        .unwrap_or_else(|| DEFAULT_WEB_URL.to_owned())
}

/// A link button when the URL is https; Telegram refuses links to local addresses.
pub fn link_button(label: &str, url: &str) -> Option<Button> {
    url.starts_with("https://").then(|| Button::url(label, url))
}

fn auth(turn: &Turn) -> AuthUser {
    AuthUser {
        user_id: turn.user_id.clone(),
    }
}

// ---------- get_profile ----------

async fn get_profile(turn: &mut Turn) -> ApiResult<ToolOutput> {
    let user = load_user(&turn.state.pool, &turn.user_id)
        .await?
        .ok_or_else(|| ApiError::not_found("I can't find your Ferry account."))?;
    let p = user.profile;
    let favorites: Vec<(String, String)> =
        sqlx::query_as("SELECT id, name FROM ferry.restaurants WHERE id = ANY($1) ORDER BY name")
            .bind(&p.favorite_restaurant_ids)
            .fetch_all(&turn.state.pool)
            .await?;
    let scrub = |s: &str| turn.redactor.scrub(s);
    // The drop-off is a placeholder: its label is free text (`turn.redactor` fills it back).
    let dropoff = p
        .dropoff_default
        .as_ref()
        .map(|_| super::ordering::DROPOFF.to_owned());
    let result = json!({
        "dropoff_default": dropoff,
        "diet": p.diet.iter().map(|s| scrub(s)).collect::<Vec<_>>(),
        "avoid": p.avoid.iter().map(|s| scrub(s)).collect::<Vec<_>>(),
        "favorite_cuisines": p.favorite_cuisines.iter().map(|s| scrub(s)).collect::<Vec<_>>(),
        "favorite_restaurants": favorites.iter()
            .map(|(id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>(),
        "usual_times": p.usual_times.iter().map(|t| json!({
            "label": scrub(&t.label), "days": t.days, "start": t.start, "end": t.end
        })).collect::<Vec<_>>(),
        "walking_pace": p.walking_pace.as_str(),
        "default_tip": dollars(p.default_tip_cents),
        "default_tip_cents": p.default_tip_cents,
        "runner_opt_in": p.runner_opt_in,
    });
    let mut parts = Vec::new();
    if let Some(dropoff) = &dropoff {
        parts.push(format!("drop-off {dropoff}"));
    }
    parts.push(format!("tip {}", dollars(p.default_tip_cents)));
    if !favorites.is_empty() {
        let names: Vec<String> = favorites.iter().map(|(_, n)| n.clone()).collect();
        parts.push(format!("favorites {}", and_list(&names)));
    }
    if !p.avoid.is_empty() {
        parts.push(format!("avoiding {}", scrub(&p.avoid.join(", "))));
    }
    Ok(ToolOutput::ok(
        result,
        format!("Your Ferry defaults: {}.", parts.join(", ")),
    ))
}

// ---------- list_restaurants ----------

/// The restaurants as the catalog serves them to this user (open now from today's hours, walk
/// minutes from their drop-off).
pub async fn restaurants(
    turn: &Turn,
    q: Option<&str>,
    open_now: bool,
) -> ApiResult<Vec<Restaurant>> {
    let filter = RestaurantFilter {
        area_id: None,
        q: q.map(str::to_owned),
        open_now: open_now.then_some(true),
    };
    let crate::extract::Json(list) =
        handlers::list_restaurants(auth(turn), State(turn.state.clone()), Query(filter)).await?;
    Ok(list)
}

async fn list_restaurants(turn: &mut Turn, args: &Map<String, Value>) -> ApiResult<ToolOutput> {
    let q = string_arg(args, "q").map(|q| turn.redactor.scrub(q));
    let open_now = args
        .get("open_now")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let list = restaurants(turn, q.as_deref(), open_now).await?;
    let result = json!({
        "restaurants": list.iter().map(|r| json!({
            "id": r.id, "name": r.name, "cuisine": r.cuisine, "is_open": r.is_open,
            "hours_today": r.hours_today, "price_level": r.price_level,
            "walk_minutes_from_dropoff": r.walk_minutes_from_dropoff,
        })).collect::<Vec<_>>(),
    });
    let open: Vec<String> = list
        .iter()
        .filter(|r| r.is_open)
        .map(|r| format!("{} ({})", r.name, r.hours_today))
        .collect();
    let say = match (&q, open.is_empty()) {
        (Some(q), true) if list.is_empty() => format!("No restaurant matches \"{q}\"."),
        (_, true) => "Nothing that matches is open right now.".to_owned(),
        (_, false) => format!("Open now: {}.", and_list(&open)),
    };
    Ok(ToolOutput::ok(result, say))
}

// ---------- get_menu ----------

/// The restaurant `name_or_id` names: an id, or a name matched like a trip's place.
pub async fn resolve_restaurant(turn: &Turn, name_or_id: &str) -> ApiResult<(String, String)> {
    let all: Vec<(String, String)> =
        sqlx::query_as("SELECT id, name FROM ferry.restaurants ORDER BY name, id")
            .fetch_all(&turn.state.pool)
            .await?;
    if let Some(found) = all.iter().find(|(id, _)| id == name_or_id) {
        return Ok(found.clone());
    }
    match match_place(name_or_id, &all) {
        PlaceMatch::One(id) => Ok(all.into_iter().find(|(i, _)| *i == id).expect("matched id")),
        PlaceMatch::Several(ids) => {
            let names: Vec<String> = all
                .iter()
                .filter(|(id, _)| ids.contains(id))
                .map(|(_, n)| n.clone())
                .collect();
            Err(ApiError::not_found(format!(
                "Which one: {}?",
                and_list(&names).replace(" and ", " or ")
            )))
        }
        PlaceMatch::None => Err(ApiError::not_found(
            "I don't know that restaurant. Ask me what's open to see the list.",
        )),
    }
}

async fn get_menu(turn: &mut Turn, args: &Map<String, Value>) -> ApiResult<ToolOutput> {
    let wanted = string_arg(args, "restaurant")
        .ok_or_else(|| ApiError::validation("Which restaurant's menu?"))?
        .to_owned();
    let (restaurant_id, name) = resolve_restaurant(turn, &wanted).await?;
    let crate::extract::Json(menu): crate::extract::Json<Menu> = handlers::get_menu(
        auth(turn),
        State(turn.state.clone()),
        Path(restaurant_id.clone()),
    )
    .await?;
    let items: Vec<_> = menu
        .categories
        .iter()
        .flat_map(|c| c.items.iter())
        .collect();
    let result = json!({
        "restaurant_id": restaurant_id,
        "restaurant": name,
        "prices_are_estimates": menu.prices_are_estimates,
        "items": items.iter().map(|i| json!({
            "id": i.id, "name": i.name, "price": dollars(i.price_cents), "price_cents": i.price_cents,
            "tags": i.tags,
            "options": i.options.iter().map(|o| json!({
                "name": o.name, "min": o.min, "max": o.max,
                "choices": o.choices.iter().map(|c| json!({
                    "id": c.id, "name": c.name, "price_delta_cents": c.price_delta_cents
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    });
    let shown: Vec<String> = items
        .iter()
        .take(4)
        .map(|i| format!("{} {}", i.name, dollars(i.price_cents)))
        .collect();
    let more = items.len().saturating_sub(shown.len());
    let mut say = format!("{name}: {}", shown.join(", "));
    if more > 0 {
        say.push_str(&format!(" and {more} more"));
    }
    say.push('.');
    if menu.prices_are_estimates {
        say.push_str(" Prices are estimates.");
    }
    Ok(ToolOutput::ok(result, say))
}

// ---------- get_wallet ----------

async fn get_wallet(turn: &mut Turn) -> ApiResult<ToolOutput> {
    let payments = Payments::from_state(&turn.state);
    let mut conn = turn.state.pool.acquire().await?;
    let wallet = wallet::load(&mut conn, &turn.user_id, payments.wallet_mode()).await?;
    let recent: Vec<Value> = wallet
        .transactions
        .iter()
        .take(3)
        .map(|t| json!({"kind": t.kind, "amount": dollars(t.amount_cents), "description": t.description, "at": t.at}))
        .collect();
    let result = json!({
        "balance": dollars(wallet.balance_cents),
        "balance_cents": wallet.balance_cents,
        "account": wallet.account_label,
        "recent": recent,
    });
    let mut say = format!("Your Ferry balance is {}.", dollars(wallet.balance_cents));
    if let Some(last) = wallet.transactions.first() {
        say.push_str(&format!(
            " Latest: {} {}.",
            last.description,
            dollars(last.amount_cents)
        ));
    }
    Ok(ToolOutput::ok(result, say))
}

// ---------- explain_impact ----------

async fn explain_impact(turn: &mut Turn, args: &Map<String, Value>) -> ApiResult<ToolOutput> {
    let scope = match string_arg(args, "scope") {
        Some("me") => ImpactScope::Me,
        Some("campus") => ImpactScope::Campus,
        Some("circle") => ImpactScope::Circle,
        Some(_) => return Err(ApiError::validation("scope is me, circle or campus.")),
        None if turn.circle_id.is_some() => ImpactScope::Circle,
        None => ImpactScope::Me,
    };
    let circle_id = match (scope, &turn.circle_id) {
        (ImpactScope::Circle, Some(id)) => Some(id.clone()),
        (ImpactScope::Circle, None) => {
            circles::most_recent_circle(&turn.state.pool, &turn.user_id).await?
        }
        _ => None,
    };
    if scope == ImpactScope::Circle && circle_id.is_none() {
        return Err(ApiError::not_found("You're not in a circle yet."));
    }
    let report =
        impact::impact_for(&turn.state.pool, &turn.user_id, scope, circle_id.as_deref()).await?;
    let result = json!({
        "scope": scope,
        "deliveries": report.deliveries,
        "car_miles_avoided": (report.car_miles_avoided * 10.0).round() / 10.0,
        "co2_saved_g": report.co2_saved_g.round(),
        "method": report.method,
    });
    let whose = match scope {
        ImpactScope::Me => "You've",
        ImpactScope::Circle => "Your circle has",
        ImpactScope::Campus => "Ferry on campus has",
    };
    let say = if report.deliveries == 0 {
        format!("{whose} no walking deliveries yet, so no CO2 saved so far.")
    } else {
        format!(
            "{whose} saved {} CO2 over {} walking deliver{} ({}).",
            grams(report.co2_saved_g),
            report.deliveries,
            if report.deliveries == 1 { "y" } else { "ies" },
            report.method
        )
    };
    Ok(ToolOutput::ok(result, say))
}

/// "480 g", or "5.8 kg" from 1 kg up.
pub fn grams(g: f64) -> String {
    if g >= 1000.0 {
        format!("{:.1} kg", g / 1000.0)
    } else {
        format!("{} g", g.round())
    }
}

// ---------- get_eta ----------

#[derive(sqlx::FromRow)]
struct ActiveOrderRow {
    order_id: String,
    order_status: String,
    run_id: String,
    run_status: String,
    restaurant: String,
    runner_name: String,
}

async fn get_eta(turn: &mut Turn) -> ApiResult<ToolOutput> {
    let row: Option<ActiveOrderRow> = sqlx::query_as(
        "SELECT o.id AS order_id, o.status AS order_status, r.id AS run_id, r.status AS run_status,
                rest.name AS restaurant, u.display_name AS runner_name
           FROM ferry.orders o
           JOIN ferry.runs r ON r.id = o.run_id
           JOIN ferry.restaurants rest ON rest.id = r.restaurant_id
           JOIN ferry.users u ON u.id = r.runner_id
          WHERE o.customer_id = $1 AND o.status IN ('funded', 'purchased', 'picked_up')
          ORDER BY o.created_at DESC, o.id DESC
          LIMIT 1",
    )
    .bind(&turn.user_id)
    .fetch_optional(&turn.state.pool)
    .await?;
    let Some(order) = row else {
        return Ok(ToolOutput::ok(
            json!({"active_order": false}),
            "You have no order on the way right now.".to_owned(),
        ));
    };
    let runner = turn
        .redactor
        .person("runner", &order.runner_name, &[&order.runner_name]);
    let track = tracking::track_for(&turn.state, &order.run_id, &turn.user_id).await?;
    let ping = tracking::latest_ping(&turn.state.pool, &order.run_id).await?;
    let mut result = json!({
        "active_order": true,
        "order_id": order.order_id,
        "order_status": order.order_status,
        "run_status": order.run_status,
        "restaurant": order.restaurant,
        "runner": runner,
    });
    let ping = ping.filter(|_| track.runner_location.is_some());
    let Some(ping) = ping else {
        let say = format!(
            "{runner} hasn't shared a location yet. Your order from {} is {}.",
            order.restaurant,
            status_words(&order.order_status)
        );
        return Ok(ToolOutput::ok(result, say));
    };
    let seen = match ping.seen_ago().as_str() {
        "just now" => "seen just now".to_owned(),
        ago => format!("seen {ago}"),
    };
    result["seen"] = json!(seen);
    result["seen_seconds_ago"] = json!(ping.age.as_secs());
    let say = match (track.eta_minutes, track.distance_m) {
        (Some(minutes), Some(meters)) => {
            result["eta_minutes"] = json!(minutes);
            result["distance"] = json!(miles(meters));
            format!(
                "{runner} is {minutes} min away ({}), {seen}.",
                miles(meters)
            )
        }
        _ => format!("{runner} was {seen}; there's no ETA for your stop yet."),
    };
    Ok(ToolOutput::ok(result, say))
}

/// An order status in words, for a sentence.
pub fn status_words(status: &str) -> &'static str {
    match status {
        "funded" => "paid and waiting for pickup",
        "purchased" => "bought and waiting at the restaurant",
        "picked_up" => "picked up and on its way",
        "delivered" => "delivered",
        "settled" => "delivered and settled",
        "cancelled" => "cancelled",
        _ => "in progress",
    }
}

// ---------- start_meet_again ----------

async fn start_meet_again(turn: &mut Turn, args: &Map<String, Value>) -> ApiResult<ToolOutput> {
    // Gemini wrote these from scrubbed text, so names come back from their placeholders.
    let message = string_arg(args, "message").map(|m| turn.redactor.fill(m));
    let place_words = string_arg(args, "place").map(|p| turn.redactor.fill(p));
    // The time is Gemini's words, read by code on Chapel Hill's clock; never a timestamp.
    let (when, tomorrow) = match string_arg(args, "when") {
        Some(words) => meet::read_when(words).map_err(ApiError::validation)?,
        None => (None, false),
    };
    let pool = &turn.state.pool;
    let place = match &place_words {
        Some(words) => Some(meet::resolve_place(pool, words).await?),
        None => None,
    };
    let proposed_time = meet::resolve_time(pool, when, tomorrow).await?;
    let circle_id = meet::circle_for(turn).await?;
    let created = invites::create_invite(
        &turn.state,
        &circle_id,
        &turn.user_id,
        NewInvite {
            message,
            proposed_time,
            place,
        },
    )
    .await?;
    let record = &created.record;
    let sent = record.responses.len() - created.app_only.len();

    let mut invitees = Vec::new();
    let mut app_only_labels = Vec::new();
    for (i, response) in record.responses.iter().enumerate() {
        let label = turn.redactor.person(
            &format!("invitee_{}", i + 1),
            &response.user.display_name,
            &[&response.user.display_name],
        );
        if created.app_only.contains(&response.user.id) {
            app_only_labels.push(label.clone());
        }
        invitees.push(label);
    }
    // A typed place may hold a name ("Maya's room"): Gemini sees it scrubbed, the reply filled.
    let plan = record.plan().map(|plan| turn.redactor.scrub(&plan));
    let say = meet::sent_line(&invitees, &app_only_labels, plan.as_deref());
    let result = json!({
        "invite_id": record.id,
        "invited": invitees,
        "plan": plan,
        "telegram_messages_sent": sent,
        "app_only": app_only_labels,
    });
    if let Some(url) = &record.deep_link
        && let Some(button) = link_button("Open the invite", url)
    {
        turn.add_buttons(&[button]);
    }
    Ok(ToolOutput::ok(result, say))
}

// ---------- respond_invite ----------

async fn respond_invite(turn: &mut Turn, args: &Map<String, Value>) -> ApiResult<ToolOutput> {
    let accept = args
        .get("accept")
        .and_then(Value::as_bool)
        .ok_or_else(|| ApiError::validation("Say whether you accept or decline."))?;
    let invite_id = match string_arg(args, "invite_id") {
        Some(id) => id.to_owned(),
        None => latest_open_invite(turn)
            .await?
            .ok_or_else(|| ApiError::not_found("You have no open invite to answer."))?,
    };
    let record = invites::respond_to_invite(&turn.state, &invite_id, &turn.user_id, accept).await?;
    // Gemini sees the inviter as a placeholder and a typed place scrubbed.
    let inviter = match record.from.display_name.trim() {
        "" => String::new(),
        name => turn.redactor.person("inviter", name, &[name]),
    };
    let plan = record.plan().map(|plan| turn.redactor.scrub(&plan));
    let (say, button) = invite_answer(&record, &turn.user_id, accept, &inviter, plan.as_deref());
    turn.add_buttons(&button.into_iter().collect::<Vec<_>>());
    Ok(ToolOutput::ok(
        json!({"invite_id": record.id, "accepted": accept, "status": record.status.as_str()}),
        say,
    ))
}

/// The newest invite the user was invited to and has not answered, before its deadline.
async fn latest_open_invite(turn: &Turn) -> ApiResult<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT i.id FROM ferry.invites i
           JOIN ferry.invite_responses r ON r.invite_id = i.id
          WHERE r.user_id = $1 AND r.status = 'pending'
            AND now() < COALESCE(i.proposed_time, i.created_at + interval '24 hours')
          ORDER BY i.created_at DESC, i.id DESC
          LIMIT 1",
    )
    .bind(&turn.user_id)
    .fetch_optional(&turn.state.pool)
    .await?)
}

/// What the bot says to `user_id` after they answered an invite, and the group link button for
/// an acceptance (`Invite.thread.open_url`, which only accepted invitees get): an extra after
/// "I'm in", never the point. `inviter` is how to name the inviter ("them" when empty), and `plan`
/// what the invite proposes ([`InviteRecord::plan`]).
pub fn invite_answer(
    record: &InviteRecord,
    user_id: &str,
    accept: bool,
    inviter: &str,
    plan: Option<&str>,
) -> (String, Option<Button>) {
    let inviter = match inviter.trim() {
        "" => "them",
        name => name,
    };
    if !accept {
        return (
            format!("Got it, you can't make it. I let {inviter} know."),
            None,
        );
    }
    let seen = record.as_seen_by(user_id);
    let button = seen
        .thread
        .open_url
        .as_deref()
        .and_then(|url| link_button("Open the group", url));
    let say = match plan {
        Some(plan) => format!("You're in: {plan}. I let {inviter} know."),
        None => format!("You're in! I let {inviter} know."),
    };
    (say, button)
}

// ---------- list_open_runs ----------

async fn list_open_runs(turn: &mut Turn) -> ApiResult<ToolOutput> {
    let runs = ordering::orderable_runs(&turn.state.pool, &turn.user_id, turn.circle_id.as_deref())
        .await?;
    let mut listed = Vec::new();
    let mut said = Vec::new();
    for (i, run) in runs.iter().enumerate() {
        let runner = turn.redactor.person(
            &format!("runner_{}", i + 1),
            &run.runner_name,
            &[&run.runner_name],
        );
        listed.push(json!({
            "run_id": run.run_id, "restaurant_id": run.restaurant_id, "restaurant": run.restaurant,
            "runner": runner, "orders_close": run.closes,
        }));
        said.push(format!(
            "{} with {runner}, orders close {}",
            run.restaurant, run.closes
        ));
    }
    let say = if said.is_empty() {
        "No Ferry is taking orders right now.".to_owned()
    } else {
        format!("Taking orders now: {}.", said.join("; "))
    };
    Ok(ToolOutput::ok(json!({"runs": listed}), say))
}

// ---------- open_run ----------

async fn open_run(turn: &mut Turn, args: &Map<String, Value>) -> ApiResult<ToolOutput> {
    let wanted = string_arg(args, "restaurant")
        .ok_or_else(|| ApiError::validation("Which restaurant?"))?
        .to_owned();
    let (restaurant_id, _) = resolve_restaurant(turn, &wanted).await?;
    let when = string_arg(args, "when").and_then(patterns::parse_when);
    let line = trips::open_now(turn, &restaurant_id, when).await?;
    let opened = !line.buttons.is_empty() || line.text.starts_with("Ferry to ");
    turn.add_buttons(&line.buttons);
    Ok(ToolOutput {
        ok: opened,
        result: json!({"opened": opened, "say": line.text}),
        say: Some(line.text),
    })
}

// ---------- draft_order_from_text ----------

async fn draft_order_from_text(
    turn: &mut Turn,
    args: &Map<String, Value>,
) -> ApiResult<ToolOutput> {
    let text = string_arg(args, "text")
        .ok_or_else(|| ApiError::validation("What would you like to order?"))?
        .to_owned();
    let pool = &turn.state.pool;
    let runs = ordering::orderable_runs(pool, &turn.user_id, turn.circle_id.as_deref()).await?;
    let wanted = string_arg(args, "run_id");
    let run = match wanted.and_then(|id| runs.iter().find(|r| r.run_id == id)) {
        Some(run) => run.clone(),
        None => match ordering::run_for_text(pool, &turn.user_id, turn.circle_id.as_deref(), &text)
            .await?
        {
            Some(run) => run,
            None => match runs.as_slice() {
                [] => return Ok(ToolOutput::error("No Ferry is taking orders right now.")),
                [only] => only.clone(),
                several => {
                    let names: Vec<String> = several.iter().map(|r| r.restaurant.clone()).collect();
                    return Ok(ToolOutput::error(format!(
                        "Which Ferry: {}?",
                        and_list(&names).replace(" and ", " or ")
                    )));
                }
            },
        },
    };
    let reply = ordering::draft_from_text(
        &turn.state,
        &turn.user_id,
        &turn.space_id,
        turn.is_group,
        &run,
        &text,
    )
    .await?;
    Ok(draft_output(turn, reply, json!({"run_id": run.run_id})))
}

/// A draft line as a tool's output: Gemini sees `{dropoff}`, the turn's reply gets the drop-off as
/// this chat shows it, and the draft's buttons go under the reply.
fn draft_output(turn: &mut Turn, reply: ordering::DraftReply, mut result: Value) -> ToolOutput {
    if !reply.dropoff.is_empty() {
        turn.redactor.fill_as("dropoff", &reply.dropoff);
    }
    turn.add_buttons(&reply.buttons);
    result["draft_id"] = json!(reply.draft_id.map(|id| id.to_string()));
    result["total"] = json!(reply.total_cents.map(dollars));
    result["placed"] = json!(false);
    result["say"] = json!(reply.say);
    ToolOutput {
        ok: reply.draft_id.is_some(),
        result,
        say: Some(reply.say),
    }
}

// ---------- confirm_order ----------

/// The user's pending draft in this chat, shown again with its total and its Place order and
/// Cancel buttons. Never places it: only the drafter's tap on Place order does.
async fn confirm_order(turn: &mut Turn) -> ApiResult<ToolOutput> {
    let shown =
        ordering::show_pending(&turn.state, &turn.user_id, &turn.space_id, turn.is_group).await?;
    Ok(match shown {
        Some(reply) => {
            let mut output = draft_output(turn, reply, json!({"needs_tap": true}));
            output.ok = true;
            output
        }
        None => ToolOutput::error("You have no order waiting to be placed."),
    })
}

// ---------- cancel_order ----------

/// What can still be cancelled (the pending draft here, funded orders), each with its own button.
/// Never cancels anything: only the user's tap does.
async fn cancel_order(turn: &mut Turn, args: &Map<String, Value>) -> ApiResult<ToolOutput> {
    let order_id = string_arg(args, "order_id").filter(|id| id.starts_with("ord_"));
    let reply =
        ordering::cancel_choices(&turn.state, &turn.user_id, &turn.space_id, order_id).await?;
    turn.add_buttons(&reply.buttons);
    let choices = reply.buttons.len();
    Ok(ToolOutput::ok(
        json!({"cancelled": false, "choices": choices, "needs_tap": choices > 0}),
        reply.say,
    ))
}

async fn restaurant_name(turn: &Turn, restaurant_id: &str) -> ApiResult<String> {
    Ok(
        sqlx::query_scalar("SELECT name FROM ferry.restaurants WHERE id = $1")
            .bind(restaurant_id)
            .fetch_optional(&turn.state.pool)
            .await?
            .unwrap_or_else(|| "Ferry".to_owned()),
    )
}

// ---------- get_order_status ----------

async fn get_order_status(turn: &mut Turn, args: &Map<String, Value>) -> ApiResult<ToolOutput> {
    let order_id = match string_arg(args, "order_id") {
        Some(id) => id.to_owned(),
        None => ordering::latest_order(&turn.state.pool, &turn.user_id, false)
            .await?
            .ok_or_else(|| ApiError::not_found("You have no Ferry orders yet."))?,
    };
    let report = orders::order_status(&turn.state, &turn.user_id, &order_id).await?;
    let order = &report.order;
    let restaurant = restaurant_name(turn, &order.restaurant_id).await?;
    let runner = turn.redactor.person(
        "runner",
        &order.runner.display_name,
        &[&order.runner.display_name],
    );
    let status = order.status.as_str();
    let mut say = format!("Your {restaurant} order is {}.", status_words(status));
    let mut result = json!({
        "order_id": order.id, "restaurant": restaurant, "status": status,
        "total": dollars(order.total_cents), "runner": runner,
    });
    if let (Some(minutes), Some(meters), Some(seen)) =
        (order.eta_minutes, report.distance_m, &report.seen_ago)
    {
        let seen = if seen == "just now" {
            "seen just now".to_owned()
        } else {
            format!("seen {seen}")
        };
        say.push_str(&format!(
            " {runner} is {minutes} min away ({}), {seen}.",
            miles(meters)
        ));
        result["eta_minutes"] = json!(minutes);
        result["distance"] = json!(miles(meters));
        result["seen"] = json!(seen);
    }
    Ok(ToolOutput::ok(result, say))
}

/// The app's address for a line without a button.
pub fn app_link(turn: &Turn) -> String {
    web_url(turn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations_are_generated_from_the_definitions() {
        let all = declarations();
        assert_eq!(all.len(), Tool::ALL.len());
        let names: Vec<&str> = all.iter().map(|d| d["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "get_profile",
                "list_restaurants",
                "get_menu",
                "get_wallet",
                "explain_impact",
                "get_eta",
                "start_meet_again",
                "respond_invite",
                "list_open_runs",
                "open_run",
                "draft_order_from_text",
                "confirm_order",
                "cancel_order",
                "get_order_status"
            ]
        );
        for d in &all {
            assert!(d["description"].as_str().is_some_and(|s| !s.is_empty()));
            if let Some(params) = d.get("parameters") {
                assert_eq!(params["type"], "object");
                let props = params["properties"].as_object().unwrap();
                for required in params["required"].as_array().unwrap() {
                    assert!(props.contains_key(required.as_str().unwrap()), "{d}");
                }
                // Gemini's OpenAPI subset: no $ref and no additionalProperties anywhere.
                let text = d.to_string();
                assert!(!text.contains("$ref") && !text.contains("additionalProperties"));
            }
        }
        assert!(all[0].get("parameters").is_none(), "get_profile takes none");
        assert_eq!(
            Tool::GetMenu.declaration()["parameters"]["required"],
            json!(["restaurant"])
        );
        assert_eq!(
            Tool::ExplainImpact.declaration()["parameters"]["properties"]["scope"]["enum"],
            json!(["me", "circle", "campus"])
        );
        for tool in Tool::ALL {
            assert_eq!(Tool::from_name(tool.name()), Some(tool));
        }
        assert_eq!(Tool::from_name("place_bet"), None);
    }

    #[test]
    fn formats() {
        assert_eq!(dollars(899), "$8.99");
        assert_eq!(dollars(5000), "$50.00");
        assert_eq!(dollars(-150), "-$1.50");
        assert_eq!(dollars(0), "$0.00");
        assert_eq!(miles(483.0), "0.3 mi");
        assert_eq!(miles(1609.344), "1.0 mi");
        assert_eq!(grams(480.4), "480 g");
        assert_eq!(grams(5760.0), "5.8 kg");
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(and_list(&s(&[])), "");
        assert_eq!(and_list(&s(&["A"])), "A");
        assert_eq!(and_list(&s(&["A", "B"])), "A and B");
        assert_eq!(and_list(&s(&["A", "B", "C"])), "A, B and C");
    }
}
