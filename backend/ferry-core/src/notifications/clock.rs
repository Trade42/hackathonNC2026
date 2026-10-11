//! ISO 8601 UTC timestamps (`2026-10-10T22:04:12.000Z`) for the stream envelope's `at` and the
//! ticket's `expires_at`, without a date-time dependency.

use std::time::{SystemTime, UNIX_EPOCH};

/// Now, as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub fn now_rfc3339() -> String {
    rfc3339(SystemTime::now())
}

/// `time` as `YYYY-MM-DDTHH:MM:SS.mmmZ`, in UTC with millisecond precision. Times before the Unix
/// epoch print as the epoch.
pub fn rfc3339(time: SystemTime) -> String {
    let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since_epoch.as_secs();
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        since_epoch.subsec_millis()
    )
}

/// Days since 1970-01-01 to (year, month, day) in the proleptic Gregorian calendar: Howard
/// Hinnant's `civil_from_days` (https://howardhinnant.github.io/date_algorithms.html).
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
    use std::time::Duration;

    fn at(secs: u64, millis: u64) -> String {
        rfc3339(UNIX_EPOCH + Duration::from_secs(secs) + Duration::from_millis(millis))
    }

    #[test]
    fn matches_gnu_date() {
        // Expected values from `date -u -d @<secs> +%FT%T.000Z`.
        assert_eq!(at(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(at(951_782_400, 0), "2000-02-29T00:00:00.000Z");
        assert_eq!(at(1_791_669_852, 0), "2026-10-10T22:04:12.000Z");
        assert_eq!(at(1_791_676_799, 0), "2026-10-10T23:59:59.000Z");
        assert_eq!(at(4_102_444_800, 0), "2100-01-01T00:00:00.000Z");
        assert_eq!(at(1_709_251_199, 0), "2024-02-29T23:59:59.000Z");
    }

    #[test]
    fn millisecond_precision() {
        assert_eq!(at(1_791_669_852, 7), "2026-10-10T22:04:12.007Z");
        assert_eq!(at(1_791_669_852, 999), "2026-10-10T22:04:12.999Z");
    }

    #[test]
    fn before_the_epoch_is_the_epoch() {
        assert_eq!(
            rfc3339(UNIX_EPOCH - Duration::from_secs(10)),
            "1970-01-01T00:00:00.000Z"
        );
    }
}
