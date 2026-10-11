//! Opening hours: whether `hours_today` ("11:00–04:00") covers a local time of day.
//!
//! The hours are local to Chapel Hill (America/New_York); the caller supplies the local time,
//! which ferry-core reads from Postgres (`now() AT TIME ZONE 'America/New_York'`) so daylight
//! saving time needs no time zone crate.

/// Minutes in a day.
const DAY: u32 = 24 * 60;

/// Whether `hours` is open at `now`, in minutes since local midnight.
///
/// `hours` is one range or several separated by commas. A range is `HH:MM` to `HH:MM` joined by
/// an en dash (as the fixtures write it), a hyphen or an em dash. An end before the start runs past
/// midnight ("11:00–04:00" is open at 01:00), an end equal to the start means open all day, and
/// `24:00` is the end of the day. Returns `None` when `hours` cannot be read.
pub fn is_open_at(hours: &str, now: u32) -> Option<bool> {
    let now = now % DAY;
    let mut open = false;
    let mut ranges = 0;
    for range in hours.split(',') {
        let (start, end) = range.split_once(['–', '-', '—'])?;
        let (start, end) = (parse_hhmm(start)?, parse_hhmm(end)?);
        if start >= DAY {
            return None;
        }
        open |= covers(start, end, now);
        ranges += 1;
    }
    (ranges > 0).then_some(open)
}

/// Minutes since midnight for `HH:MM` (surrounding spaces allowed), `00:00` to `24:00`.
pub fn parse_hhmm(text: &str) -> Option<u32> {
    let (h, m) = text.trim().split_once(':')?;
    if h.len() != 2 || m.len() != 2 {
        return None;
    }
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    let minutes = h * 60 + m;
    (m < 60 && minutes <= DAY).then_some(minutes)
}

fn covers(start: u32, end: u32, now: u32) -> bool {
    match start.cmp(&end) {
        std::cmp::Ordering::Equal => true,
        std::cmp::Ordering::Less => start <= now && now < end,
        // Past midnight: the evening part, or the early-morning part.
        std::cmp::Ordering::Greater => now >= start || now < end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(hhmm: &str) -> u32 {
        parse_hhmm(hhmm).unwrap()
    }

    #[test]
    fn same_day_range() {
        let hours = "11:00–22:00";
        assert_eq!(is_open_at(hours, at("10:59")), Some(false));
        assert_eq!(is_open_at(hours, at("11:00")), Some(true));
        assert_eq!(is_open_at(hours, at("21:59")), Some(true));
        assert_eq!(is_open_at(hours, at("22:00")), Some(false));
        assert_eq!(is_open_at(hours, at("03:00")), Some(false));
    }

    #[test]
    fn range_past_midnight() {
        // Cosmic Cantina in the fixtures.
        let hours = "11:00–04:00";
        assert_eq!(is_open_at(hours, at("10:59")), Some(false));
        assert_eq!(is_open_at(hours, at("11:00")), Some(true));
        assert_eq!(is_open_at(hours, at("23:59")), Some(true));
        assert_eq!(is_open_at(hours, at("00:00")), Some(true));
        assert_eq!(is_open_at(hours, at("03:59")), Some(true));
        assert_eq!(is_open_at(hours, at("04:00")), Some(false));
        assert_eq!(is_open_at(hours, at("07:00")), Some(false));
    }

    #[test]
    fn every_fixture_hours_string_parses() {
        let restaurants: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../contract/fixtures/restaurants.json"
        ))
        .unwrap();
        for r in restaurants.as_array().unwrap() {
            let hours = r["hours_today"].as_str().unwrap();
            assert!(is_open_at(hours, at("12:00")).is_some(), "{hours}");
        }
    }

    #[test]
    fn separators_spaces_and_several_ranges() {
        assert_eq!(is_open_at("09:00-15:00", at("12:00")), Some(true));
        assert_eq!(is_open_at("09:00 — 15:00", at("15:30")), Some(false));
        let split = "11:00–14:00, 17:00–22:00";
        assert_eq!(is_open_at(split, at("12:00")), Some(true));
        assert_eq!(is_open_at(split, at("15:00")), Some(false));
        assert_eq!(is_open_at(split, at("18:00")), Some(true));
    }

    #[test]
    fn all_day() {
        for hours in ["00:00–24:00", "00:00–00:00"] {
            for now in [at("00:00"), at("12:00"), at("23:59")] {
                assert_eq!(is_open_at(hours, now), Some(true), "{hours} {now}");
            }
        }
        // A time past the day wraps.
        assert_eq!(is_open_at("11:00–22:00", DAY + at("12:00")), Some(true));
    }

    #[test]
    fn unreadable_hours() {
        for hours in [
            "",
            "Closed",
            "11:00",
            "11–22",
            "11:00–25:00",
            "24:00–02:00",
            "11:60–22:00",
            "1:00–2:00",
            "11:00–22:00,",
        ] {
            assert_eq!(is_open_at(hours, at("12:00")), None, "{hours:?}");
        }
    }
}
