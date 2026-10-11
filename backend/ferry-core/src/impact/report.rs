//! `Impact` for a scope: totals over every delivered or settled order the scope covers, the last
//! 8 weeks of them by week, and whether any of them is seeded demo history.
//!
//! `circle` and `campus` read `ferry.impact_daily`. `me` has no view (a user is not a column of
//! it), so it groups the orders directly, with `/v1/me/stats`'s rule: orders the user placed or
//! ran.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use super::carbon::METHOD;
use super::weeks::{self, DayTotal, ImpactWeek};
use crate::error::{ApiError, ApiResult};

/// `ImpactScope` in contract/openapi.yaml.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImpactScope {
    Me,
    Circle,
    Campus,
}

/// `Impact` in contract/openapi.yaml.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Impact {
    pub scope: ImpactScope,
    /// Delivered or settled orders, all time.
    pub deliveries: i64,
    /// Their car distance in miles, all time.
    pub car_miles_avoided: f64,
    /// Their CO2 estimate in grams, all time.
    pub co2_saved_g: f64,
    /// The last 8 ISO weeks, oldest first, the current week last.
    pub by_week: Vec<ImpactWeek>,
    /// [`METHOD`].
    pub method: &'static str,
    /// Whether any order counted is seeded demo history.
    pub includes_demo_history: bool,
}

/// What an Impact sums, without any permission check (see [`impact_for`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target<'a> {
    /// Orders the user placed or ran.
    Me { user_id: &'a str },
    /// Orders on the circle's runs.
    Circle { circle_id: &'a str },
    /// Every order.
    Campus,
}

impl Target<'_> {
    pub fn scope(&self) -> ImpactScope {
        match self {
            Target::Me { .. } => ImpactScope::Me,
            Target::Circle { .. } => ImpactScope::Circle,
            Target::Campus => ImpactScope::Campus,
        }
    }
}

/// One UTC day of one scope: (days since 1970-01-01, deliveries, car miles, grams of CO2,
/// is_demo_history). A day can have a real row and a demo-history row. The queries sort them, so
/// the same rows always add up to the same floating-point totals.
type DayRow = (i32, i64, f64, f64, bool);

const CAMPUS_SQL: &str = "\
SELECT (d.day - DATE '1970-01-01')::integer, d.deliveries::bigint,
       d.car_miles_avoided::double precision, d.co2_saved_g::double precision, d.is_demo_history
  FROM ferry.impact_daily d
 ORDER BY d.day, d.circle_id, d.is_demo_history";

const CIRCLE_SQL: &str = "\
SELECT (d.day - DATE '1970-01-01')::integer, d.deliveries::bigint,
       d.car_miles_avoided::double precision, d.co2_saved_g::double precision, d.is_demo_history
  FROM ferry.impact_daily d
 WHERE d.circle_id = $1
 ORDER BY d.day, d.is_demo_history";

/// impact_daily's columns for the orders a user placed or ran (`/v1/me/stats`'s rule).
const ME_SQL: &str = "\
SELECT ((o.delivered_at AT TIME ZONE 'UTC')::date - DATE '1970-01-01')::integer,
       count(*)::bigint,
       (coalesce(sum(o.car_distance_m), 0) / 1609.344)::double precision,
       coalesce(sum(o.co2_saved_g), 0)::double precision,
       o.is_demo_history
  FROM ferry.orders o
  JOIN ferry.runs r ON r.id = o.run_id
 WHERE (o.customer_id = $1 OR r.runner_id = $1)
   AND o.status IN ('delivered', 'settled')
 GROUP BY 1, 5
 ORDER BY 1, 5";

/// The Impact of `target`, with no permission check. Totals are all time; `by_week` is the last
/// 8 weeks to today (UTC).
pub async fn compute(pool: &PgPool, target: &Target<'_>) -> Result<Impact, sqlx::Error> {
    let rows: Vec<DayRow> = match target {
        Target::Me { user_id } => {
            sqlx::query_as(ME_SQL)
                .bind(*user_id)
                .fetch_all(pool)
                .await?
        }
        Target::Circle { circle_id } => {
            sqlx::query_as(CIRCLE_SQL)
                .bind(*circle_id)
                .fetch_all(pool)
                .await?
        }
        Target::Campus => sqlx::query_as(CAMPUS_SQL).fetch_all(pool).await?,
    };
    Ok(summarize(target.scope(), weeks::today(), &rows))
}

/// The Impact a viewer asked for, with `GET /v1/impact`'s rules: `circle` needs `circle_id`
/// (400 `validation_failed`), a circle that exists (404) and the viewer among its members (403).
/// `circle_id` is ignored for `me` and `campus`. The agent's explain_impact tool calls this.
pub async fn impact_for(
    pool: &PgPool,
    viewer_id: &str,
    scope: ImpactScope,
    circle_id: Option<&str>,
) -> ApiResult<Impact> {
    let target = match scope {
        ImpactScope::Me => Target::Me { user_id: viewer_id },
        ImpactScope::Campus => Target::Campus,
        ImpactScope::Circle => {
            let circle_id = circle_id.filter(|id| !id.is_empty()).ok_or_else(|| {
                ApiError::validation("circle_id is required when scope is circle.")
            })?;
            let member: Option<bool> = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM ferry.circle_members m
                                 WHERE m.circle_id = c.id AND m.user_id = $2)
                   FROM ferry.circles c WHERE c.id = $1",
            )
            .bind(circle_id)
            .bind(viewer_id)
            .fetch_optional(pool)
            .await?;
            match member {
                None => return Err(ApiError::not_found("No circle has that id.")),
                Some(false) => {
                    return Err(ApiError::forbidden("You are not a member of this circle."));
                }
                Some(true) => Target::Circle { circle_id },
            }
        }
    };
    Ok(compute(pool, &target).await?)
}

/// Totals over every row, and the rows' days bucketed into weeks to `today`.
fn summarize(scope: ImpactScope, today: i64, rows: &[DayRow]) -> Impact {
    let mut impact = Impact {
        scope,
        deliveries: 0,
        car_miles_avoided: 0.0,
        co2_saved_g: 0.0,
        by_week: Vec::new(),
        method: METHOD,
        includes_demo_history: false,
    };
    let mut days = Vec::with_capacity(rows.len());
    for &(day, deliveries, car_miles, co2_saved_g, is_demo_history) in rows {
        impact.deliveries += deliveries;
        impact.car_miles_avoided += car_miles;
        impact.co2_saved_g += co2_saved_g;
        impact.includes_demo_history |= is_demo_history;
        days.push(DayTotal {
            day: i64::from(day),
            deliveries,
            co2_saved_g,
        });
    }
    impact.by_week = weeks::by_week(today, &days);
    impact
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAT_2026_10_10: i32 = 20_736;

    #[test]
    fn nothing_delivered_is_zero_with_eight_zero_weeks() {
        let impact = summarize(ImpactScope::Me, SAT_2026_10_10.into(), &[]);
        assert_eq!(impact.deliveries, 0);
        assert_eq!(impact.car_miles_avoided, 0.0);
        assert_eq!(impact.co2_saved_g, 0.0);
        assert!(!impact.includes_demo_history);
        assert_eq!(impact.by_week.len(), 8);
        assert!(impact.by_week.iter().all(|w| w.deliveries == 0));
        assert_eq!(impact.method, METHOD);
    }

    #[test]
    fn totals_are_all_time_and_weeks_are_the_window() {
        let rows = [
            (SAT_2026_10_10, 2, 2.6, 1040.0, false),
            (SAT_2026_10_10, 1, 1.3, 520.0, true),
            // Long ago: in the totals, not in a week.
            (SAT_2026_10_10 - 400, 4, 4.0, 1600.0, false),
        ];
        let impact = summarize(ImpactScope::Campus, SAT_2026_10_10.into(), &rows);
        assert_eq!(impact.scope, ImpactScope::Campus);
        assert_eq!(impact.deliveries, 7);
        assert!((impact.car_miles_avoided - 7.9).abs() < 1e-9);
        assert_eq!(impact.co2_saved_g, 3160.0);
        assert!(impact.includes_demo_history);
        let last = impact.by_week.last().unwrap();
        assert_eq!(last.week_start, "2026-10-05T00:00:00Z");
        assert_eq!((last.deliveries, last.co2_saved_g), (3, 1560.0));
        let in_weeks: i64 = impact.by_week.iter().map(|w| w.deliveries).sum();
        assert_eq!(in_weeks, 3);
    }

    #[test]
    fn scopes_serialize_as_the_contract_enum() {
        for (scope, text) in [
            (ImpactScope::Me, "me"),
            (ImpactScope::Circle, "circle"),
            (ImpactScope::Campus, "campus"),
        ] {
            assert_eq!(serde_json::to_value(scope).unwrap(), text);
            assert_eq!(
                serde_json::from_value::<ImpactScope>(text.into()).unwrap(),
                scope
            );
        }
        assert!(serde_json::from_value::<ImpactScope>("planet".into()).is_err());
        assert_eq!(Target::Campus.scope(), ImpactScope::Campus);
        assert_eq!(Target::Me { user_id: "usr_x" }.scope(), ImpactScope::Me);
    }
}
