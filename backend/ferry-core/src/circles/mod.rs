//! Circles and meet-again: /v1/circles.
//!
//! - `GET /v1/circles`: the viewer's circles, public ones first, then most recently active first.
//! - `GET /v1/circles/{id}`: one of them. A circle the viewer is not in is 404 `not_found`, the
//!   same as one that does not exist.
//! - `POST /v1/circles/{id}/meet-again`: a meetup invite to the circle's other members, through
//!   `invites::create_meetup_invite`.
//!
//! A public community (`circles.is_public`, migration `20261011000008`) is a circle anyone in it
//! can see and order on without knowing the runner, such as the university, UNC Chapel Hill
//! ([`CAMPUS_CIRCLE_ID`]), which every user joins. Public circles are listed first and list no
//! members (nobody browses every user's name). They refuse meet-again and invites (409
//! `conflict`), and are never the agent's "most recent circle".
//! - [`meet_again_last_circle`]: the same invite for the agent's "let's meet with the people I hung
//!   out with last time", sent to the user's most recent circle.
//!
//! The derived fields (docs/DECISIONS.md, Contract, "Circles and invites"):
//! - `last_met_at` is the latest of the stored `circles.last_met_at` (seeded history), each
//!   accepted meetup (its `proposed_time`, or its first acceptance when it proposed no time, once
//!   that time has passed) and each delivered run (its latest order delivery, else its
//!   `leaving_at`). Null when there is none.
//! - `last_active_at` is the latest of the stored `circles.last_active_at`, `last_met_at`, each
//!   run's start (the earlier of `created_at` and `leaving_at`) and each invite's creation or
//!   latest response.
//! - `stats.runs` counts the circle's delivered runs and `stats.co2_saved_g` sums its delivered
//!   orders' CO2 (`ferry.impact_daily`). Demo-history rows count in both.

use axum::Router;
use axum::extract::State;
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, PgPool};

use crate::error::{ApiError, ApiResult};
use crate::extract::{Json, Path};
use crate::invites::{self, Invite, InviteRecord, MeetAgainRequest};
use crate::jwt::AuthUser;
use crate::state::AppState;

/// UNC Chapel Hill, the public community every user is in (migration `20261011000008` adds each
/// user as they sign up).
pub const CAMPUS_CIRCLE_ID: &str = "cir_campus";

/// Whether `circle_id` is a public community. False for a circle that does not exist.
pub async fn is_public<'e>(db: impl PgExecutor<'e>, circle_id: &str) -> ApiResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM ferry.circles WHERE id = $1 AND is_public)",
    )
    .bind(circle_id)
    .fetch_one(db)
    .await?)
}

/// This module's routes, with full `/v1/...` paths. Merged into the app by `app::router`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/circles", get(list))
        .route("/v1/circles/{id}", get(read))
        .route("/v1/circles/{id}/meet-again", post(meet_again))
}

/// `PublicUser` in contract/openapi.yaml.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct PublicUser {
    pub id: String,
    pub display_name: String,
    pub avatar_seed: String,
    pub bubble_color: String,
}

/// `CircleStats` in contract/openapi.yaml.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CircleStats {
    pub runs: i64,
    pub co2_saved_g: f64,
}

/// `Circle` in contract/openapi.yaml.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Circle {
    pub id: String,
    pub name: String,
    pub emoji: String,
    /// Members in the order they joined; always empty for a public community.
    pub members: Vec<PublicUser>,
    /// ISO 8601 UTC with milliseconds, or null when the circle has never met.
    pub last_met_at: Option<String>,
    /// ISO 8601 UTC with milliseconds.
    pub last_active_at: String,
    pub stats: CircleStats,
    /// A public community: listed first, no members shown, no meet-again.
    #[serde(default)]
    pub is_public: bool,
}

/// A timestamptz SQL expression as the contract's ISO 8601 UTC with milliseconds.
macro_rules! iso {
    ($e:literal) => {
        concat!(
            "to_char((",
            $e,
            ") AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"')"
        )
    };
}
pub(crate) use iso;

/// The circles `$1` (a user id) is in, or only circle `$2` when `$2` is not null, with the derived
/// fields as timestamptz (see the module docs for the rules).
macro_rules! viewer_circles {
    () => {
        "SELECT c.id, c.name, c.emoji, c.is_public, met.at AS last_met_at, \
                GREATEST(c.last_active_at, met.at, act.run_at, act.invite_at) AS last_active_at, \
                st.runs, st.co2_saved_g \
         FROM ferry.circles c \
         JOIN ferry.circle_members viewer ON viewer.circle_id = c.id AND viewer.user_id = $1 \
         CROSS JOIN LATERAL ( \
             SELECT GREATEST( \
                 c.last_met_at, \
                 (SELECT max(COALESCE(i.proposed_time, a.first_at)) \
                  FROM ferry.invites i \
                  CROSS JOIN LATERAL ( \
                      SELECT min(r.responded_at) AS first_at FROM ferry.invite_responses r \
                      WHERE r.invite_id = i.id AND r.status = 'accepted' \
                  ) a \
                  WHERE i.circle_id = c.id AND a.first_at IS NOT NULL \
                    AND COALESCE(i.proposed_time, a.first_at) <= now()), \
                 (SELECT max(COALESCE(d.at, r.leaving_at)) \
                  FROM ferry.runs r \
                  CROSS JOIN LATERAL ( \
                      SELECT max(o.delivered_at) AS at FROM ferry.orders o WHERE o.run_id = r.id \
                  ) d \
                  WHERE r.circle_id = c.id AND r.status = 'delivered') \
             ) AS at \
         ) met \
         CROSS JOIN LATERAL ( \
             SELECT \
                 (SELECT max(LEAST(r.created_at, r.leaving_at)) FROM ferry.runs r \
                  WHERE r.circle_id = c.id) AS run_at, \
                 (SELECT max(GREATEST(i.created_at, ( \
                      SELECT max(r.responded_at) FROM ferry.invite_responses r \
                      WHERE r.invite_id = i.id))) \
                  FROM ferry.invites i WHERE i.circle_id = c.id) AS invite_at \
         ) act \
         CROSS JOIN LATERAL ( \
             SELECT \
                 (SELECT count(*) FROM ferry.runs r \
                  WHERE r.circle_id = c.id AND r.status = 'delivered') AS runs, \
                 (SELECT COALESCE(sum(d.co2_saved_g), 0)::double precision \
                  FROM ferry.impact_daily d WHERE d.circle_id = c.id) AS co2_saved_g \
         ) st \
         WHERE ($2::text IS NULL OR c.id = $2)"
    };
}

#[derive(sqlx::FromRow)]
struct CircleRow {
    id: String,
    name: String,
    emoji: String,
    is_public: bool,
    last_met_at: Option<String>,
    last_active_at: String,
    runs: i64,
    co2_saved_g: f64,
}

#[derive(sqlx::FromRow)]
struct MemberRow {
    circle_id: String,
    #[sqlx(flatten)]
    user: PublicUser,
}

/// The circles `viewer` is in, public ones first and then most recently active first, or only
/// `only` when given (empty when the viewer is not in it). A public circle's member list is empty.
pub async fn circles_of(pool: &PgPool, viewer: &str, only: Option<&str>) -> ApiResult<Vec<Circle>> {
    let rows = sqlx::query_as::<_, CircleRow>(concat!(
        "SELECT x.id, x.name, x.emoji, x.is_public, ",
        iso!("x.last_met_at"),
        " AS last_met_at, ",
        iso!("x.last_active_at"),
        " AS last_active_at, x.runs, x.co2_saved_g FROM (",
        viewer_circles!(),
        ") x ORDER BY x.is_public DESC, x.last_active_at DESC, x.id"
    ))
    .bind(viewer)
    .bind(only)
    .fetch_all(pool)
    .await?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    let ids: Vec<String> = rows
        .iter()
        .filter(|r| !r.is_public)
        .map(|r| r.id.clone())
        .collect();
    let members = sqlx::query_as::<_, MemberRow>(
        "SELECT m.circle_id, u.id, u.display_name, u.avatar_seed, u.bubble_color \
         FROM ferry.circle_members m JOIN ferry.users u ON u.id = m.user_id \
         WHERE m.circle_id = ANY($1) \
         ORDER BY m.circle_id, m.joined_at, u.id",
    )
    .bind(&ids)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| Circle {
            members: members
                .iter()
                .filter(|m| m.circle_id == row.id)
                .map(|m| m.user.clone())
                .collect(),
            id: row.id,
            name: row.name,
            emoji: row.emoji,
            last_met_at: row.last_met_at,
            last_active_at: row.last_active_at,
            stats: CircleStats {
                runs: row.runs,
                co2_saved_g: row.co2_saved_g,
            },
            is_public: row.is_public,
        })
        .collect())
}

/// The id of `user_id`'s most recent circle that has someone else in it: latest `last_met_at`
/// first (never-met circles last), then latest `last_active_at`. Never a public community, whose
/// "someone else" may be anyone.
pub async fn most_recent_circle(pool: &PgPool, user_id: &str) -> ApiResult<Option<String>> {
    let id = sqlx::query_scalar::<_, String>(concat!(
        "SELECT x.id FROM (",
        viewer_circles!(),
        ") x WHERE NOT x.is_public AND EXISTS ( \
             SELECT 1 FROM ferry.circle_members o WHERE o.circle_id = x.id AND o.user_id <> $1 \
         ) \
         ORDER BY x.last_met_at DESC NULLS LAST, x.last_active_at DESC, x.id LIMIT 1"
    ))
    .bind(user_id)
    .bind(None::<&str>)
    .fetch_optional(pool)
    .await?;
    Ok(id)
}

/// For the agent's "let's meet with the people I hung out with last time": a meetup invite to the
/// other members of `user_id`'s most recent circle ([`most_recent_circle`]), created, notified and
/// published exactly as `POST /v1/circles/{id}/meet-again` does. Returns the whole record: the
/// caller shows `record.as_seen_by(user_id)` and sends `record.dms()` to the invitees. 404
/// `not_found` when the user shares no circle with anyone.
pub async fn meet_again_last_circle(
    state: &AppState,
    user_id: &str,
    request: MeetAgainRequest,
) -> ApiResult<InviteRecord> {
    let circle_id = most_recent_circle(&state.pool, user_id)
        .await?
        .ok_or_else(|| ApiError::not_found("You're not in a circle with anyone yet."))?;
    invites::create_meetup_invite(state, &circle_id, user_id, request).await
}

/// `GET /v1/circles`.
async fn list(State(pool): State<PgPool>, user: AuthUser) -> ApiResult<Json<Vec<Circle>>> {
    Ok(Json(circles_of(&pool, &user.user_id, None).await?))
}

/// `GET /v1/circles/{id}`: members only.
async fn read(
    State(pool): State<PgPool>,
    user: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Circle>> {
    circles_of(&pool, &user.user_id, Some(&id))
        .await?
        .into_iter()
        .next()
        .map(Json)
        .ok_or_else(|| ApiError::not_found("No such circle."))
}

/// `POST /v1/circles/{id}/meet-again`: the body is optional.
async fn meet_again(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    body: Option<Json<MeetAgainRequest>>,
) -> ApiResult<Json<Invite>> {
    let request = body.map(|Json(request)| request).unwrap_or_default();
    let record = invites::create_meetup_invite(&state, &id, &user.user_id, request).await?;
    Ok(Json(record.as_seen_by(&user.user_id)))
}
