//! `Impact.by_week`: the last [`WEEKS`] ISO weeks, Monday 00:00 UTC to Monday, oldest first, every
//! week present even when it has no deliveries. Days are counted from 1970-01-01 (UTC), so no
//! date-time dependency is needed.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// How many weeks `by_week` covers, the current one included.
pub const WEEKS: i64 = 8;

/// `ImpactWeek` in contract/openapi.yaml.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImpactWeek {
    /// Monday 00:00 UTC, as `YYYY-MM-DDT00:00:00Z`.
    pub week_start: String,
    pub deliveries: i64,
    pub co2_saved_g: f64,
}

/// Deliveries and CO2 on one UTC day, `day` counted from 1970-01-01.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DayTotal {
    pub day: i64,
    pub deliveries: i64,
    pub co2_saved_g: f64,
}

/// Today's UTC day, counted from 1970-01-01.
pub fn today() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    (secs / 86_400) as i64
}

/// The Monday on or before `day`. 1970-01-01 was a Thursday, 3 days after a Monday.
pub fn monday_of(day: i64) -> i64 {
    day - (day + 3).rem_euclid(7)
}

/// The [`WEEKS`] weeks ending with the one that holds `today`, oldest first. Days outside them are
/// left out; weeks with no days are zero.
pub fn by_week(today: i64, days: &[DayTotal]) -> Vec<ImpactWeek> {
    let first = monday_of(today) - 7 * (WEEKS - 1);
    let mut weeks: Vec<ImpactWeek> = (0..WEEKS)
        .map(|i| ImpactWeek {
            week_start: week_start(first + 7 * i),
            deliveries: 0,
            co2_saved_g: 0.0,
        })
        .collect();
    for d in days {
        let index = (d.day - first).div_euclid(7);
        if let Some(week) = usize::try_from(index).ok().and_then(|i| weeks.get_mut(i)) {
            week.deliveries += d.deliveries;
            week.co2_saved_g += d.co2_saved_g;
        }
    }
    weeks
}

/// Midnight UTC of `day` as an RFC 3339 date-time.
pub fn week_start(day: i64) -> String {
    let (year, month, date) = civil_from_days(day);
    format!("{year:04}-{month:02}-{date:02}T00:00:00Z")
}

/// Days since 1970-01-01 to (year, month, day) in the proleptic Gregorian calendar: Howard
/// Hinnant's `civil_from_days` (https://howardhinnant.github.io/date_algorithms.html), as in
/// `notifications::clock`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Day numbers from `date -u -d <date> +%s` / 86400.
    const SAT_2026_10_10: i64 = 20_736;
    const SUN_2026_10_11: i64 = 20_737;
    const MON_2026_10_05: i64 = 20_731;
    const MON_2026_10_12: i64 = 20_738;
    const THU_2026_01_01: i64 = 20_454;

    fn day(day: i64, deliveries: i64, co2_saved_g: f64) -> DayTotal {
        DayTotal {
            day,
            deliveries,
            co2_saved_g,
        }
    }

    #[test]
    fn day_numbers_match_gnu_date() {
        assert_eq!(week_start(0), "1970-01-01T00:00:00Z");
        assert_eq!(week_start(SAT_2026_10_10), "2026-10-10T00:00:00Z");
        assert_eq!(week_start(MON_2026_10_05), "2026-10-05T00:00:00Z");
        assert_eq!(week_start(THU_2026_01_01), "2026-01-01T00:00:00Z");
        // 2000-02-29 and 2024-02-29 (leap days).
        assert_eq!(week_start(11_016), "2000-02-29T00:00:00Z");
        assert_eq!(week_start(19_782), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn weeks_start_on_monday_utc() {
        assert_eq!(monday_of(SAT_2026_10_10), MON_2026_10_05);
        assert_eq!(monday_of(SUN_2026_10_11), MON_2026_10_05);
        assert_eq!(monday_of(MON_2026_10_05), MON_2026_10_05);
        assert_eq!(monday_of(MON_2026_10_12), MON_2026_10_12);
        // Across the new year: Thursday 2026-01-01 is in the week of Monday 2025-12-29.
        assert_eq!(
            week_start(monday_of(THU_2026_01_01)),
            "2025-12-29T00:00:00Z"
        );
        // 1970-01-01 (a Thursday) and the days before the epoch.
        assert_eq!(week_start(monday_of(0)), "1969-12-29T00:00:00Z");
        // Wednesday 1969-12-31 is in the week of Monday 1969-12-29.
        assert_eq!(monday_of(-1), -3);
        for d in -30..30 {
            let m = monday_of(d);
            assert!(m <= d && d - m < 7, "{d}");
            assert_eq!((m + 3).rem_euclid(7), 0, "{d}");
        }
    }

    #[test]
    fn eight_contiguous_weeks_oldest_first_with_zero_weeks() {
        let weeks = by_week(SAT_2026_10_10, &[]);
        let starts: Vec<&str> = weeks.iter().map(|w| w.week_start.as_str()).collect();
        assert_eq!(
            starts,
            [
                "2026-08-17T00:00:00Z",
                "2026-08-24T00:00:00Z",
                "2026-08-31T00:00:00Z",
                "2026-09-07T00:00:00Z",
                "2026-09-14T00:00:00Z",
                "2026-09-21T00:00:00Z",
                "2026-09-28T00:00:00Z",
                "2026-10-05T00:00:00Z",
            ]
        );
        assert!(
            weeks
                .iter()
                .all(|w| w.deliveries == 0 && w.co2_saved_g == 0.0)
        );
        // The same window from any day of the week.
        assert_eq!(by_week(MON_2026_10_05, &[]), weeks);
        assert_eq!(by_week(SUN_2026_10_11, &[]), weeks);
        assert_eq!(
            by_week(MON_2026_10_12, &[])[7].week_start,
            "2026-10-12T00:00:00Z"
        );
    }

    #[test]
    fn days_fall_into_their_week() {
        let first = MON_2026_10_05 - 49; // Monday 2026-08-17
        let weeks = by_week(
            SAT_2026_10_10,
            &[
                day(SAT_2026_10_10, 2, 900.0),
                day(MON_2026_10_05, 1, 500.0),
                // Sunday 2026-10-04 is the week before.
                day(MON_2026_10_05 - 1, 3, 1200.0),
                day(first, 1, 100.0),
                // Before the window and after today: left out.
                day(first - 1, 7, 7000.0),
                day(SAT_2026_10_10 + 3, 5, 5000.0),
            ],
        );
        let got: Vec<(i64, f64)> = weeks
            .iter()
            .map(|w| (w.deliveries, w.co2_saved_g))
            .collect();
        assert_eq!(
            got,
            [
                (1, 100.0),
                (0, 0.0),
                (0, 0.0),
                (0, 0.0),
                (0, 0.0),
                (0, 0.0),
                (3, 1200.0),
                (3, 1400.0),
            ]
        );
    }

    #[test]
    fn a_day_after_today_in_the_current_week_still_counts() {
        // Clock skew between the database and the server: Sunday counts in the current week.
        let weeks = by_week(SAT_2026_10_10, &[day(SUN_2026_10_11, 1, 10.0)]);
        assert_eq!(weeks[7].deliveries, 1);
    }
}
