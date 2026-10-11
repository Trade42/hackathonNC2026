//! The text side of trip detection (spec section 3): the pattern prefilter that notices "heading to
//! Cosmic Cantina at 6, anyone want anything?", the check that a group message speaks to the bot,
//! time parsing and restaurant matching. Pure and deterministic: no I/O, no clock, no database.
//!
//! - [`prefilter`] is the cheap first pass over every group message. It finds a trigger phrase
//!   ("heading to", "omw to", "anyone want anything from", ...) and the place after it, and drops
//!   the obvious non-trips ("heading to class", "going to be late"). It is deliberately loose: a
//!   Gemini classification follows on whatever it lets through.
//! - [`parse_when`] reads the first time expression ("at 6", "6:30pm", "in 20 min", "rn"), and
//!   [`resolve_minutes`] turns it into minutes after local midnight, choosing am or pm for a bare
//!   "at 6" the way people mean it.
//! - [`addressed`] says whether a group message speaks to the bot ("ferry", "@<bot username>").
//! - [`match_place`] matches a place phrase against the restaurant list, and [`clock`] formats
//!   minutes the way Chapel Hill reads a clock.
//!
//! Matching is hand-made (the spec's "regex prefilter" without a regex dependency): text is compared
//! as lowercase chars, a space in a pattern matches any run of whitespace, and a word is bounded by
//! any char that is not a letter or a digit.

use std::cmp::Reverse;

/// A trip someone announced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TripMention {
    /// The words after the trigger, up to the time or the end of the clause, trimmed, as written
    /// ("Cosmic Cantina", "cosmic", "sup dogs").
    pub place: String,
    /// The first time expression anywhere in the message, before or after the place.
    pub when: Option<When>,
}

/// When a trip happens, as said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// A clock time. Definite when `meridiem` is Some, or when `hour` is 0 or 13..=23 (a 24-hour
    /// "18:00" or "0:15", with `meridiem: None`): then `hour` is 0..=23. Otherwise `hour` is 1..=12
    /// and [`resolve_minutes`] picks am or pm. "noon" is 12 with `Some(Pm)`, "midnight" 0 with
    /// `Some(Am)`.
    At {
        hour: u8,
        minute: u8,
        meridiem: Option<Meridiem>,
    },
    /// "in 20 min", "in 1 hour", "in half an hour".
    In { minutes: u32 },
    /// "now", "right now", "rn".
    Now,
}

/// The am or pm that someone wrote (or that "noon" and "midnight" imply).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Meridiem {
    Am,
    Pm,
}

/// How a place phrase matches the restaurant list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaceMatch {
    /// Exactly one restaurant: its id.
    One(String),
    /// More than one, as ids in the order of the list given.
    Several(Vec<String>),
    None,
}

const DAY: u32 = 24 * 60;

/// How far in the past a time may be and still mean today ("at 6" said at 6:20 means now-ish).
const GRACE_MINUTES: u32 = 30;

/// A longer place is a sentence, not a restaurant.
const MAX_PLACE_WORDS: usize = 6;

/// Phrases that announce a trip, and whether the phrase is ambiguous ("going to be late"): an
/// ambiguous one counts only with a time right after the place, an invitation in the message or a
/// capitalized place. At the same position the longest phrase is tried first.
const TRIGGERS: &[(&str, bool)] = &[
    ("heading to", false),
    ("headed to", false),
    ("heading over to", false),
    ("headed over to", false),
    ("head to", false),
    ("head over to", false),
    ("going to", true),
    ("going over to", false),
    ("going to go to", false),
    ("gonna go to", false),
    ("about to go to", false),
    ("walking to", false),
    ("walking over to", false),
    ("walk to", false),
    ("running to", false),
    ("running over to", false),
    ("run to", false),
    ("on my way to", false),
    ("on our way to", false),
    ("on the way to", false),
    ("omw to", false),
    ("otw to", false),
    ("stopping by", false),
    ("stop by", false),
    ("swinging by", false),
    ("swing by", false),
    ("picking up from", false),
    ("picking up food from", false),
    ("picking up something from", false),
    ("pick up from", false),
    ("grabbing food from", false),
    ("grabbing something from", false),
    ("grab food from", false),
    ("getting food from", false),
    ("getting something from", false),
    ("get food from", false),
    ("anyone want anything from", false),
    ("anybody want anything from", false),
    ("anyone need anything from", false),
    ("anybody need anything from", false),
    ("want anything from", false),
    ("need anything from", false),
];

/// Words that end the place when they follow it after a space ("Cosmic at Franklin", "sup dogs
/// and then class", "cosmic if anyone wants"). A time ("at 6", "by 7", "in 20 min", "now", "rn",
/// "6:30pm") ends it too, and so does a lone " - ". "and" is one of them, so "lime and basil"
/// leaves the place "lime"; [`match_place`] still finds Lime & Basil from it.
const CLAUSE_WORDS: &[&str] = &[
    "at", "around", "in", "and", "if", "anyone", "anybody", "any1", "want", "wants", "wanna",
    "who", "for", "to", "then", "after", "before", "with", "w/", "lmk", "you", "u", "y'all",
    "yall", "i", "i'm", "i’m", "im", "we", "need", "needs", "so", "soon", "later", "tonight",
    "today", "tomorrow", "tmrw", "tmr", "this", "asap", "bc", "because", "brb", "rq", "too",
    "also", "but", "since", "lol", "lmao",
];

/// First words of a place that is clearly not a restaurant: where people go instead ("heading to
/// class", "the gym", "my room"), and verbs ("going to be late", "going to sleep").
const NOT_PLACES: &[&str] = &[
    // Places.
    "class",
    "classes",
    "lecture",
    "lab",
    "recitation",
    "practice",
    "rehearsal",
    "work",
    "office",
    "gym",
    "rec",
    "library",
    "lib",
    "home",
    "bed",
    "dorm",
    "room",
    "bathroom",
    "school",
    "campus",
    "church",
    "airport",
    "meeting",
    "my",
    "your",
    "his",
    "her",
    "their",
    "our",
    "a",
    "an",
    "it",
    "that",
    "there",
    "here",
    // Verbs.
    "be",
    "get",
    "go",
    "grab",
    "pick",
    "make",
    "have",
    "do",
    "try",
    "take",
    "eat",
    "see",
    "say",
    "tell",
    "call",
    "text",
    "miss",
    "need",
    "study",
    "start",
    "finish",
    "stay",
    "leave",
    "head",
    "run",
    "walk",
    "stop",
    "swing",
    "sleep",
    "nap",
    "cry",
    "die",
    "fail",
    "lose",
    "watch",
    "play",
];

/// Words that make an ambiguous "going to X" an offer ("going to Cosmic, anyone want anything?").
const INVITES: &[&str] = &[
    "anyone",
    "anybody",
    "any1",
    "want anything",
    "wants anything",
    "need anything",
    "needs anything",
    "want something",
    "wants something",
    "need something",
    "lmk",
];

/// Words of a restaurant's name that name no restaurant on their own ("the deli", "cafe").
const GENERIC_NAME_WORDS: &[&str] = &[
    "the",
    "a",
    "an",
    "of",
    "and",
    "cafe",
    "deli",
    "market",
    "street",
    "st",
    "kitchen",
    "grill",
    "bar",
    "restaurant",
    "cookies",
    "bagel",
    "food",
    "shop",
    "house",
    "co",
    "coffee",
    "pizza",
    "bakery",
    "express",
];

/// The prefilter: Some when `text` announces a trip.
///
/// A trigger (see [`TRIGGERS`]: "heading to", "omw to", "stopping by", "anyone want anything
/// from", ...) must appear as whole words, in any case. The place is what follows it up to a clause
/// end: `,` `.` `!` `?` `;` `:` `(` `)` a dash, an emoji or a newline, a time ("at 6", "in 20
/// min", "now"), a lone " - ", or a word such as "at", "and", "if", "anyone", "want", "who", "for"
/// (see [`CLAUSE_WORDS`]). A leading "the" and the punctuation around the place are dropped.
///
/// None when no trigger yields a place: empty, longer than six words, or starting with a word that
/// is clearly not a restaurant ("class", "the gym", "my room", "be", "sleep"). "going to" is
/// ambiguous, so "going to X" counts only when a time follows X, the message invites ("anyone",
/// "want anything", "lmk"), or X is capitalized in a message that is not all caps.
///
/// `when` is the first time expression anywhere in `text` ([`parse_when`]), so "at 6 heading to
/// sup dogs" and "heading to sup dogs, leaving at 6:30" both carry it.
pub fn prefilter(text: &str) -> Option<TripMention> {
    let (orig, low) = lower_chars(text);
    let mut found: Vec<(usize, usize, bool)> = Vec::new();
    for &(trigger, ambiguous) in TRIGGERS {
        for (start, end) in word_positions(&low, trigger) {
            found.push((start, end, ambiguous));
        }
    }
    // Earliest first; at one position the longest ("going to go to" before "going to").
    found.sort_by_key(|&(start, end, _)| (start, Reverse(end)));
    let invited = INVITES.iter().any(|w| contains_word(&low, w));
    // "GOING TO BE LATE" is shouting, not a capitalized name.
    let has_lowercase = orig.iter().any(|c| c.is_lowercase());
    for (_, end, ambiguous) in found {
        let Some(place) = place_after(&orig, &low, end) else {
            continue;
        };
        let capitalized =
            has_lowercase && place.text.chars().next().is_some_and(char::is_uppercase);
        if ambiguous && !(place.timed || invited || capitalized) {
            continue;
        }
        return Some(TripMention {
            place: place.text,
            when: parse_when(text),
        });
    }
    None
}

/// The first time expression in `text`, scanning word starts left to right:
///
/// - a clock after "at", "around", "by" or "@" (optionally "at like 6", "at about 6"): "at 6",
///   "at 6:30", "at 6pm", "at 6 p.m.", "at 18:00", "at noon", "at 6ish";
/// - a clock with a colon, am/pm or "ish" anywhere: "6:30pm", "6p", "0:15", "7ish";
/// - "noon", "midnight";
/// - "in 20 min(s|utes)", "in 20m", "in 1 hr/hour", "in 2 hours", "in an hour", "in half an hour",
///   "in an hour and a half", and a bare "in 20" (1 to 120 minutes) when no word follows it;
/// - "now", "right now", "rn".
///
/// A bare number without "at/around/by" and without am/pm or a colon is not a time ("2
/// burritos"), nor is a 24-hour value without a colon ("at 18"). Invalid values (25:00, 6:75,
/// 13pm) are rejected rather than read as something else.
pub fn parse_when(text: &str) -> Option<When> {
    let (_, low) = lower_chars(text);
    (0..low.len()).find_map(|i| when_at(&low, i).map(|(when, _)| when))
}

/// Minutes after local midnight today (≥ 1440 for tomorrow) when `when` happens, given the local
/// time now in minutes after midnight.
///
/// - `At` that is definite (a meridiem, or a 24-hour hour 0 or 13..=23): that time today, or
///   tomorrow if it is more than 30 minutes in the past. An unconverted `At{6, Some(Pm)}` is read
///   as 18:00.
/// - `At` without a meridiem (1..=12): the earliest of h:mm am and h:mm pm that is not more than 30
///   minutes in the past, else tomorrow's am one. "at 6" said at 17:00 is 18:00; at 05:00 it is
///   06:00; at 19:00 it is 06:00 tomorrow.
/// - `In`: now plus the minutes. `Now`: now.
///
/// A `now_minutes` of 1440 or more counts from the start of that later day.
pub fn resolve_minutes(when: When, now_minutes: u32) -> u32 {
    let day_start = now_minutes / DAY * DAY;
    let now = now_minutes % DAY;
    // Today, or tomorrow when more than the grace period has passed.
    let soonest = |t: u32| if now > t + GRACE_MINUTES { t + DAY } else { t };
    match when {
        When::Now => now_minutes,
        When::In { minutes } => now_minutes.saturating_add(minutes),
        When::At {
            hour,
            minute,
            meridiem,
        } => {
            let hour = u32::from(hour);
            let minute = u32::from(minute.min(59));
            let at = match meridiem {
                Some(Meridiem::Am) => soonest((hour % 12) * 60 + minute),
                Some(Meridiem::Pm) => soonest((hour % 12 + 12) * 60 + minute),
                None if hour == 0 || hour >= 13 => soonest((hour % 24) * 60 + minute),
                None => {
                    let am = (hour % 12) * 60 + minute;
                    let pm = am + 12 * 60;
                    if now <= am + GRACE_MINUTES {
                        am
                    } else if now <= pm + GRACE_MINUTES {
                        pm
                    } else {
                        am + DAY
                    }
                }
            };
            day_start.saturating_add(at)
        }
    }
}

/// Whether the bot is addressed in a group: the word "ferry" in any case ("Ferry," and "/ferry"
/// yes, "ferryman" and "ferrying" no), or "@<bot_username>" in any case when `bot_username` is
/// Some, also as a command suffix ("/help@Ferrygo_bot"). `_` counts as part of a word, so
/// "@other_ferry_bot" is neither. `bot_username` may carry its own `@`.
pub fn addressed(text: &str, bot_username: Option<&str>) -> bool {
    let (_, low) = lower_chars(text);
    let name_char = |c: char| c.is_alphanumeric() || c == '_';
    let free_after = |end: usize| low.get(end).is_none_or(|&c| !name_char(c));
    let says_ferry = (0..low.len()).any(|i| {
        (i == 0 || !name_char(low[i - 1])) && pattern_at(&low, i, "ferry").is_some_and(&free_after)
    });
    if says_ferry {
        return true;
    }
    let Some(user) = bot_username
        .map(|u| u.trim().trim_start_matches('@').to_lowercase())
        .filter(|u| !u.is_empty() && !u.contains(char::is_whitespace))
    else {
        return false;
    };
    let mention = format!("@{user}");
    (0..low.len()).any(|i| pattern_at(&low, i, &mention).is_some_and(&free_after))
}

/// Matches `place` (from [`TripMention`], or any short phrase such as "menu for sup dogs"'s "sup
/// dogs") against `restaurants` as (id, name) pairs.
///
/// Both sides are normalized: lowercase, accents folded, `&` → "and", apostrophes dropped, other
/// punctuation a space, spaces collapsed; a leading "the" is dropped. A restaurant matches when:
///
/// 1. its full name equals, or is contained (as whole words) in, the phrase; or else
/// 2. the phrase starts a word of the name and continues inside it, is at least 4 characters and
///    has a word that is not generic ("cosmic", "sup dogs", "insom"); or the phrase is one
///    significant word of the name ("sup"); or the name's words joined equal the phrase without
///    spaces ("supdogs").
///
/// When any restaurant matches by rule 1, only those count, so a full name beats a longer name
/// that starts with it. Generic words alone ("the deli", "cafe", "street market") match nothing.
/// One for exactly one restaurant, Several (in the input order) for more, None otherwise.
pub fn match_place(place: &str, restaurants: &[(String, String)]) -> PlaceMatch {
    let phrase = strip_the(normalize_name(place));
    if phrase.is_empty() {
        return PlaceMatch::None;
    }
    // (id, normalized name) by rule 1 and by rule 2.
    let mut full: Vec<(&str, String)> = Vec::new();
    let mut partial: Vec<(&str, String)> = Vec::new();
    for (id, name) in restaurants {
        let name = strip_the(normalize_name(name));
        if name.is_empty() {
            continue;
        }
        let bucket = if has_words(&phrase, &name) {
            &mut full
        } else if partial_match(&phrase, &name) {
            &mut partial
        } else {
            continue;
        };
        if !bucket.iter().any(|(i, _)| i == id) {
            bucket.push((id, name));
        }
    }
    // A longer full name in the phrase beats one it contains: "cosmic cantina durham" is not
    // also "cosmic cantina".
    let longest = |name: &String| {
        !full
            .iter()
            .any(|(_, other)| other != name && has_words(other, name))
    };
    let mut ids: Vec<String> = if full.is_empty() {
        partial.iter().map(|(id, _)| (*id).to_owned()).collect()
    } else {
        full.iter()
            .filter(|(_, name)| longest(name))
            .map(|(id, _)| (*id).to_owned())
            .collect()
    };
    match ids.len() {
        0 => PlaceMatch::None,
        1 => PlaceMatch::One(ids.remove(0)),
        _ => PlaceMatch::Several(ids),
    }
}

/// Formats minutes after local midnight (any number of days on) as Chapel Hill's clock: 12-hour,
/// no am/pm, no leading zero on the hour. 370 is "6:10" (the demo's "open until 6:10"), 1450 is
/// "12:10".
pub fn clock(minutes: u32) -> String {
    let minutes = minutes % DAY;
    let hour = match minutes / 60 % 12 {
        0 => 12,
        h => h,
    };
    format!("{hour}:{:02}", minutes % 60)
}

// ---------- meet-again with a place and a time ----------

/// A meet-again someone asked for in words: "anyone want to go to Cosmic at 7?", "ask the people
/// from last time if they want Cosmic at 7", "anyone want to get Sup Dogs at 8 tomorrow?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeetMention {
    /// The place as written ("Cosmic", "Sup Dogs"); None for a meal or "food" without a place
    /// ("anyone want to get dinner at 7?").
    pub place: Option<String>,
    /// The first time expression in the message.
    pub when: Option<When>,
    /// The message says tomorrow ([`says_tomorrow`]).
    pub tomorrow: bool,
}

/// Phrases before the place of a meet-again. At one position the longest is tried first, so
/// "want to go to" wins over "want".
const MEET_TRIGGERS: &[&str] = &[
    "want to go to",
    "wanna go to",
    "want to head to",
    "wanna head to",
    "want to hit",
    "wanna hit",
    "want to get",
    "wanna get",
    "want to grab",
    "wanna grab",
    "want to eat at",
    "wanna eat at",
    "want to try",
    "wanna try",
    "down for",
    "down to go to",
    "go to",
    "meet at",
    "meet up at",
    "meet us at",
    "meet me at",
    "join me at",
    "join us at",
    "want",
    "wants",
];

/// Words that make a message a meet-again: it asks other people.
const MEET_ASKS: &[&str] = &[
    "anyone",
    "anybody",
    "any1",
    "everyone",
    "everybody",
    "who wants",
    "who's down",
    "who’s down",
    "whos down",
    "who is down",
    "y'all",
    "y’all",
    "yall",
    "you guys",
    "people",
    "the group",
    "last time",
    "ask",
    "invite",
    "let's",
    "let’s",
    "lets",
];

/// What people get together for, which is not a place ("get dinner at 7").
const MEALS: &[&str] = &[
    "food",
    "dinner",
    "lunch",
    "breakfast",
    "brunch",
    "coffee",
    "drinks",
    "dessert",
    "a bite",
    "some food",
];

/// Words after "want" that make no meet-again ("anyone want anything?" is a runner's offer).
const NOT_MEET: &[&str] = &["anything", "something", "stuff", "smth", "sth"];

/// The fallback's reading of a meet-again with a place or a time (Gemini down): Some when `text`
/// asks other people ([`MEET_ASKS`]: "anyone", "who's down", "ask the people from last time") to
/// go somewhere ([`MEET_TRIGGERS`]: "want to go to X", "want to get X", "want X", "meet at X") or
/// to get a meal at a time ("anyone want to get dinner at 7?"). The place ends as a trip's does
/// (at a time, "tomorrow", "if", punctuation), and is never "class", "my room" or another of the
/// trip prefilter's non-places. None without a place and without a time, and None for a trip
/// ([`prefilter`]: "heading to Cosmic at 6, anyone want anything?" is a run, not a meetup).
pub fn meet_mention(text: &str) -> Option<MeetMention> {
    let (orig, low) = lower_chars(text);
    if !MEET_ASKS.iter().any(|w| contains_word(&low, w)) || prefilter(text).is_some() {
        return None;
    }
    let mut found: Vec<(usize, usize)> = MEET_TRIGGERS
        .iter()
        .flat_map(|trigger| word_positions(&low, trigger))
        .collect();
    found.sort_by_key(|&(start, end)| (start, Reverse(end)));
    let when = parse_when(text);
    let tomorrow = says_tomorrow(text);
    for (_, end) in found {
        let Some(place) = place_after(&orig, &low, end) else {
            continue;
        };
        let lowered = place.text.to_lowercase();
        if NOT_MEET.contains(&lowered.as_str()) {
            continue;
        }
        let place = (!MEALS.contains(&lowered.as_str())).then_some(place.text);
        if place.is_none() && when.is_none() {
            continue;
        }
        return Some(MeetMention {
            place,
            when,
            tomorrow,
        });
    }
    None
}

/// Whether `text` says tomorrow: "tomorrow", "tmrw", "tmr", "tomorow", "2morrow".
pub fn says_tomorrow(text: &str) -> bool {
    let (_, low) = lower_chars(text);
    ["tomorrow", "tmrw", "tmr", "tomorow", "2morrow"]
        .iter()
        .any(|w| contains_word(&low, w))
}

/// When a meetup is, in minutes after local midnight today (1440 and more is tomorrow), given the
/// local time now. A meetup is always ahead, so this is the next time the words name after now,
/// with no grace for a time just past:
///
/// - `At` with am/pm or a 24-hour hour: today, or tomorrow once it has passed. With `tomorrow`,
///   tomorrow.
/// - `At` with a bare hour (1 to 12): the next of h am and h pm. With `tomorrow`, tomorrow evening
///   (h pm; 12 is noon), the way people plan a meetup ("Sup Dogs at 8 tomorrow" is 8 PM).
/// - `In`: now plus the minutes; `tomorrow` does not apply.
/// - `Now`, and `In` zero minutes: None. An invite's time is its deadline, so "now" proposes no
///   time rather than one that has passed by the time anyone reads it.
pub fn meetup_minutes(when: When, now_minutes: u32, tomorrow: bool) -> Option<u32> {
    let now = now_minutes % DAY;
    match when {
        When::Now | When::In { minutes: 0 } => None,
        When::In { minutes } => Some(now + minutes),
        When::At {
            hour,
            minute,
            meridiem,
        } => {
            let hour = u32::from(hour);
            let minute = u32::from(minute.min(59));
            let definite = match meridiem {
                Some(Meridiem::Am) => Some((hour % 12) * 60 + minute),
                Some(Meridiem::Pm) => Some((hour % 12 + 12) * 60 + minute),
                None if hour == 0 || hour >= 13 => Some((hour % 24) * 60 + minute),
                None => None,
            };
            Some(match (definite, tomorrow) {
                (Some(at), true) => DAY + at,
                (Some(at), false) if at > now => at,
                (Some(at), false) => DAY + at,
                (None, true) if hour == 12 => DAY + 12 * 60 + minute,
                (None, true) => DAY + (hour + 12) * 60 + minute,
                (None, false) => {
                    let am = (hour % 12) * 60 + minute;
                    [am, am + 12 * 60, DAY + am]
                        .into_iter()
                        .find(|&at| at > now)
                        .unwrap_or(DAY + am)
                }
            })
        }
    }
}

// ---------- the place after a trigger ----------

struct Place {
    text: String,
    /// A time came right after the place ("Cosmic at 7").
    timed: bool,
}

/// The place that starts after the trigger ending at `from`, if it passes the checks.
fn place_after(orig: &[char], low: &[char], from: usize) -> Option<Place> {
    let mut start = skip_ws(low, from);
    // Quotes and markup before the name: "heading to \"Cosmic\"", "omw to *sup dogs*".
    while start < low.len()
        && !low[start].is_alphanumeric()
        && !low[start].is_whitespace()
        && !ends_clause(low, start)
    {
        start += 1;
    }
    if start >= low.len() || clause_starts(low, start).is_some() {
        return None;
    }
    let mut end = low.len();
    let mut timed = false;
    let mut j = start;
    while j < low.len() {
        if ends_clause(low, j) {
            end = j;
            break;
        }
        if low[j].is_whitespace() {
            // Not past a line break: the next line is another clause.
            let k = skip_spaces(low, j);
            if let Some(is_time) = clause_starts(low, k) {
                end = j;
                timed = is_time;
                break;
            }
            j = k;
            continue;
        }
        j += 1;
    }
    let raw: String = orig[start..end].iter().collect();
    let text = tidy_place(&raw)?;
    let first = text
        .split_whitespace()
        .next()?
        .to_lowercase()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_owned();
    if text.split_whitespace().count() > MAX_PLACE_WORDS || NOT_PLACES.contains(&first.as_str()) {
        return None;
    }
    Some(Place { text, timed })
}

/// Some when a new clause starts at `k`: true for a time ("at 6", "in 20 min", "now"), false
/// for a clause word or a lone dash. None when the place goes on.
fn clause_starts(low: &[char], k: usize) -> Option<bool> {
    if k >= low.len() {
        return None;
    }
    if when_at(low, k).is_some() {
        return Some(true);
    }
    let dash = low[k] == '-' && low.get(k + 1).is_none_or(|c| c.is_whitespace());
    (dash || any_word_at(low, k, CLAUSE_WORDS).is_some()).then_some(false)
}

/// Whether the char at `j` ends the place's clause: sentence punctuation, a bracket, a dash, a
/// newline, or a non-ASCII symbol such as an emoji. `:` counts only before a space or the end, so
/// a clock's colon does not.
fn ends_clause(low: &[char], j: usize) -> bool {
    match low[j] {
        ',' | '.' | '!' | '?' | ';' | '(' | ')' | '[' | ']' | '\n' | '\r' | '—' | '–' | '|' => {
            true
        }
        ':' => low.get(j + 1).is_none_or(|c| c.is_whitespace()),
        c => !c.is_ascii() && !c.is_alphanumeric() && !c.is_whitespace() && !is_apostrophe(c),
    }
}

/// The place with surrounding punctuation, a leading "the" and extra spaces removed.
fn tidy_place(raw: &str) -> Option<String> {
    let trim = |s: &str| s.trim_matches(|c: char| !c.is_alphanumeric()).to_owned();
    let mut place = trim(raw);
    if let Some((first, rest)) = place.split_once(char::is_whitespace)
        && first.eq_ignore_ascii_case("the")
    {
        place = trim(rest);
    }
    let place = place.split_whitespace().collect::<Vec<_>>().join(" ");
    (!place.is_empty()).then_some(place)
}

// ---------- times ----------

/// The time expression that starts at word start `i`, and where it ends.
fn when_at(low: &[char], i: usize) -> Option<(When, usize)> {
    if i >= low.len() || !word_start(low, i) {
        return None;
    }
    if let Some(end) = any_word_at(low, i, &["right now", "now", "rn"]) {
        return Some((When::Now, end));
    }
    if let Some(end) = word_at(low, i, "in") {
        return in_at(low, end);
    }
    if let Some(end) = any_word_at(low, i, &["at", "around", "by", "@"]) {
        let mut j = skip_ws(low, end);
        if let Some(e) = any_word_at(low, j, &["like", "about", "around", "abt", "~"]) {
            j = skip_ws(low, e);
        }
        return clock_at(low, j, true);
    }
    clock_at(low, i, false)
}

/// A clock time at `j`: "noon", "midnight", or digits with an optional ":mm", am/pm and "ish".
/// Without a prefix ("at", "around", "by") it needs a colon, am/pm or "ish".
fn clock_at(low: &[char], j: usize, prefixed: bool) -> Option<(When, usize)> {
    if let Some(end) = word_at(low, j, "noon") {
        return Some((at(12, 0, Some(Meridiem::Pm)), end));
    }
    if let Some(end) = word_at(low, j, "midnight") {
        return Some((at(0, 0, Some(Meridiem::Am)), end));
    }
    if j >= low.len() || !word_start(low, j) {
        return None;
    }
    let (hour, mut k) = number_at(low, j, 2)?;
    let mut minute = None;
    if low.get(k) == Some(&':') {
        let (m, end) = number_at(low, k + 1, 2)?;
        if end - (k + 1) != 2 {
            return None;
        }
        minute = Some(m);
        k = end;
    }
    let (meridiem, end) = meridiem_at(low, k);
    k = end;
    let ish = match pattern_at(low, k, "ish") {
        Some(end) => {
            k = end;
            true
        }
        None => false,
    };
    if !word_end(low, k) || !(prefixed || minute.is_some() || meridiem.is_some() || ish) {
        return None;
    }
    let m = minute.unwrap_or(0);
    if m > 59 {
        return None;
    }
    let when = match meridiem {
        Some(meridiem) if (1..=12).contains(&hour) => {
            let hour = hour % 12 + if meridiem == Meridiem::Pm { 12 } else { 0 };
            at(hour, m, Some(meridiem))
        }
        Some(_) => return None,
        None if (1..=12).contains(&hour) => at(hour, m, None),
        // A 24-hour clock needs its colon: "at 18:00", "0:15", not "at 18".
        None if hour <= 23 && minute.is_some() => at(hour, m, None),
        None => return None,
    };
    Some((when, k))
}

/// am/pm right after a clock ("6pm", "6p", "6 p.m."), and where it ends; (None, `k`) without one.
/// A lone "a" or "p" counts only when attached ("7a"), not after a space ("at 6 a few of us").
fn meridiem_at(low: &[char], k: usize) -> (Option<Meridiem>, usize) {
    const ATTACHED: [&str; 8] = ["a.m.", "p.m.", "a.m", "p.m", "am", "pm", "a", "p"];
    const SPACED: [&str; 6] = ["a.m.", "p.m.", "a.m", "p.m", "am", "pm"];
    let s = skip_ws(low, k);
    let options: &[&str] = if s == k { &ATTACHED } else { &SPACED };
    for option in options {
        if let Some(end) = pattern_at(low, s, option).filter(|&end| word_end(low, end)) {
            let meridiem = if option.starts_with('a') {
                Meridiem::Am
            } else {
                Meridiem::Pm
            };
            return (Some(meridiem), end);
        }
    }
    (None, k)
}

/// The span after "in" (which ends at `from`): "20 min", "1 hr", "an hour", "half an hour", or a
/// bare "20" that no word follows.
fn in_at(low: &[char], from: usize) -> Option<(When, usize)> {
    let j = skip_ws(low, from);
    if j == from {
        return None;
    }
    const SPANS: [(&str, u32); 7] = [
        ("an hour and a half", 90),
        ("half an hour", 30),
        ("half a hour", 30),
        ("half hour", 30),
        ("a half hour", 30),
        ("an hour", 60),
        ("a hour", 60),
    ];
    for (span, minutes) in SPANS {
        if let Some(end) = word_at(low, j, span) {
            return Some((When::In { minutes }, end));
        }
    }
    let (n, k) = number_at(low, j, 4)?;
    // "in 1.5 hours", "in 1:30": not parsed, and not read as "in 1".
    if matches!(low.get(k), Some('.' | ',' | ':'))
        && low.get(k + 1).is_some_and(char::is_ascii_digit)
    {
        return None;
    }
    let u = skip_ws(low, k);
    const MINUTES: [&str; 5] = ["minutes", "minute", "mins", "min", "m"];
    const HOURS: [&str; 5] = ["hours", "hour", "hrs", "hr", "h"];
    let unit = |units: &[&str]| {
        units
            .iter()
            .find_map(|unit| pattern_at(low, u, unit).filter(|&end| word_end(low, end)))
    };
    let (minutes, end) = if let Some(end) = unit(&MINUTES) {
        (n, end)
    } else if let Some(end) = unit(&HOURS) {
        (n.checked_mul(60)?, end)
    } else if low.get(u).is_none_or(|c| !c.is_alphanumeric()) && (1..=120).contains(&n) {
        // "back in 20", "in 20!"; but "in 2 classes" is not a time.
        (n, k)
    } else {
        return None;
    };
    (minutes <= DAY).then_some((When::In { minutes }, end))
}

fn at(hour: u32, minute: u32, meridiem: Option<Meridiem>) -> When {
    When::At {
        hour: hour as u8,
        minute: minute as u8,
        meridiem,
    }
}

/// The number of 1 to `max_digits` ASCII digits at `i`, and where it ends. None for no digit or
/// for more digits than `max_digits`.
fn number_at(low: &[char], i: usize, max_digits: usize) -> Option<(u32, usize)> {
    let end = i + low[i.min(low.len())..]
        .iter()
        .take_while(|c| c.is_ascii_digit())
        .count();
    if end == i || end - i > max_digits {
        return None;
    }
    let value = low[i..end].iter().collect::<String>().parse().ok()?;
    Some((value, end))
}

// ---------- restaurant names ----------

/// Lowercase, accents folded, `&` as "and", apostrophes dropped, other punctuation a space, and
/// single spaces between words.
fn normalize_name(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '&' => out.push_str(" and "),
            c if is_apostrophe(c) => {}
            c if c.is_alphanumeric() => out.push(fold_accent(lower(c))),
            _ => out.push(' '),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_the(normalized: String) -> String {
    match normalized.strip_prefix("the ") {
        Some(rest) => rest.to_owned(),
        None if normalized == "the" => String::new(),
        None => normalized,
    }
}

/// Rule 2 of [`match_place`], for normalized `phrase` and `name`.
fn partial_match(phrase: &str, name: &str) -> bool {
    let significant = |word: &str| !GENERIC_NAME_WORDS.contains(&word);
    // The phrase starts at a word of the name: "cosm" in "cosmic cantina", not "anti".
    let inside = format!(" {name}").contains(&format!(" {phrase}"));
    let contained = inside && phrase.chars().count() >= 4 && phrase.split(' ').any(significant);
    let one_word =
        !phrase.contains(' ') && significant(phrase) && name.split(' ').any(|word| word == phrase);
    let joined = name.replace(' ', "") == phrase.replace(' ', "");
    contained || one_word || joined
}

/// Whether normalized `haystack` holds normalized `needle` as whole words.
fn has_words(haystack: &str, needle: &str) -> bool {
    format!(" {haystack} ").contains(&format!(" {needle} "))
}

fn fold_accent(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' => 'a',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ò' | 'ó' | 'ô' | 'ö' | 'õ' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ñ' => 'n',
        'ç' => 'c',
        _ => c,
    }
}

// ---------- matching chars ----------

/// `text` as chars, and the same chars lowercased one for one (a char whose lowercase is several
/// chars stays as it is), so an index means the same position in both.
fn lower_chars(text: &str) -> (Vec<char>, Vec<char>) {
    let orig: Vec<char> = text.chars().collect();
    let low = orig.iter().map(|&c| lower(c)).collect();
    (orig, low)
}

fn lower(c: char) -> char {
    let mut lowered = c.to_lowercase();
    match (lowered.next(), lowered.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

fn is_apostrophe(c: char) -> bool {
    matches!(c, '\'' | '’' | '‘' | '`' | 'ʼ')
}

/// Whether a word may start at `i`: the start of the text, or after a char that is not a letter
/// or a digit.
fn word_start(low: &[char], i: usize) -> bool {
    i == 0 || !low[i - 1].is_alphanumeric()
}

/// Whether a word may end at `i`: the end of the text, or before a char that is not a letter or a
/// digit.
fn word_end(low: &[char], i: usize) -> bool {
    low.get(i).is_none_or(|c| !c.is_alphanumeric())
}

/// Where `pattern` (lowercase) ends when it matches `low` at `i`. A space in `pattern` matches one
/// or more whitespace chars. No word boundaries are checked.
fn pattern_at(low: &[char], i: usize, pattern: &str) -> Option<usize> {
    let mut j = i;
    for p in pattern.chars() {
        if p == ' ' {
            let after = skip_ws(low, j);
            if after == j {
                return None;
            }
            j = after;
        } else if low.get(j) == Some(&p) {
            j += 1;
        } else {
            return None;
        }
    }
    Some(j)
}

/// [`pattern_at`] as whole words: a pattern that starts (ends) with a letter or digit must start
/// (end) at a word boundary.
fn word_at(low: &[char], i: usize, pattern: &str) -> Option<usize> {
    let first = pattern.chars().next()?;
    let last = pattern.chars().next_back()?;
    if first.is_alphanumeric() && !word_start(low, i) {
        return None;
    }
    let end = pattern_at(low, i, pattern)?;
    if last.is_alphanumeric() && !word_end(low, end) {
        return None;
    }
    Some(end)
}

/// The end of the first of `patterns` that matches as whole words at `i`.
fn any_word_at(low: &[char], i: usize, patterns: &[&str]) -> Option<usize> {
    patterns.iter().find_map(|p| word_at(low, i, p))
}

/// Every (start, end) where `pattern` occurs as whole words.
fn word_positions(low: &[char], pattern: &str) -> Vec<(usize, usize)> {
    (0..low.len())
        .filter_map(|i| word_at(low, i, pattern).map(|end| (i, end)))
        .collect()
}

fn contains_word(low: &[char], pattern: &str) -> bool {
    (0..low.len()).any(|i| word_at(low, i, pattern).is_some())
}

fn skip_ws(low: &[char], mut i: usize) -> usize {
    while low.get(i).is_some_and(|c| c.is_whitespace()) {
        i += 1;
    }
    i
}

/// [`skip_ws`] that stops at a line break.
fn skip_spaces(low: &[char], mut i: usize) -> usize {
    while low
        .get(i)
        .is_some_and(|&c| c.is_whitespace() && c != '\n' && c != '\r')
    {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::Meridiem::{Am, Pm};
    use super::*;

    /// contract/fixtures/restaurants.json, as (id, name).
    fn restaurants() -> Vec<(String, String)> {
        [
            ("rst_cosmic", "Cosmic Cantina"),
            ("rst_supdogs", "Sup Dogs"),
            ("rst_limebasil", "Lime & Basil"),
            ("rst_meddeli", "Mediterranean Deli"),
            ("rst_insomnia", "Insomnia Cookies"),
            ("rst_alpine", "Alpine Bagel Cafe"),
            ("rst_weaver", "Weaver Street Market"),
        ]
        .into_iter()
        .map(|(id, name)| (id.to_owned(), name.to_owned()))
        .collect()
    }

    fn at_(hour: u8, minute: u8, meridiem: Option<Meridiem>) -> When {
        When::At {
            hour,
            minute,
            meridiem,
        }
    }

    fn one(id: &str) -> PlaceMatch {
        PlaceMatch::One(id.to_owned())
    }

    #[test]
    fn prefilter_finds_trips() {
        let cases: &[(&str, &str, Option<When>)] = &[
            (
                "heading to Cosmic Cantina at 6, anyone want anything?",
                "Cosmic Cantina",
                Some(at_(6, 0, None)),
            ),
            (
                "Heading to sup dogs at 6:30pm",
                "sup dogs",
                Some(at_(18, 30, Some(Pm))),
            ),
            ("anyone want anything from insomnia?", "insomnia", None),
            (
                "omw to lime & basil in 20 min",
                "lime & basil",
                Some(When::In { minutes: 20 }),
            ),
            ("going to Cosmic at 7", "Cosmic", Some(at_(7, 0, None))),
            (
                "going to cosmic now, anyone want anything",
                "cosmic",
                Some(When::Now),
            ),
            ("going to cosmic, lmk if you want something", "cosmic", None),
            (
                "at 6 heading to sup dogs",
                "sup dogs",
                Some(at_(6, 0, None)),
            ),
            (
                "heading to sup dogs, leaving at 6:30",
                "sup dogs",
                Some(at_(6, 30, None)),
            ),
            // "and" ends the place; match_place still finds Lime & Basil from "lime".
            ("omw to lime and basil", "lime", None),
            (
                "stopping by the Mediterranean Deli around 7 if anyone wants something",
                "Mediterranean Deli",
                Some(at_(7, 0, None)),
            ),
            (
                "HEADED TO WEAVER STREET MARKET RN",
                "WEAVER STREET MARKET",
                Some(When::Now),
            ),
            (
                "picking up food from Alpine Bagel Cafe 🥯 who wants",
                "Alpine Bagel Cafe",
                None,
            ),
            (
                "swinging by insomnia cookies after class lmk",
                "insomnia cookies",
                None,
            ),
            (
                "grabbing food from cosmic in an hour",
                "cosmic",
                Some(When::In { minutes: 60 }),
            ),
            ("on my way to Sup Dogs - want anything?", "Sup Dogs", None),
            (
                "I'm going to go to cosmic at 6",
                "cosmic",
                Some(at_(6, 0, None)),
            ),
            (
                "heading to class then going to Cosmic at 6",
                "Cosmic",
                Some(at_(6, 0, None)),
            ),
            ("anybody want anything from the deli", "deli", None),
            (
                "heading to \"Cosmic\" at 6pm",
                "Cosmic",
                Some(at_(18, 0, Some(Pm))),
            ),
            ("going over to sup dogs for lunch", "sup dogs", None),
            (
                "Heading to Lime & Basil in half an hour",
                "Lime & Basil",
                Some(When::In { minutes: 30 }),
            ),
            (
                "headed to cosmic, back in 20",
                "cosmic",
                Some(When::In { minutes: 20 }),
            ),
            (
                "omw to cosmic at like 6:15",
                "cosmic",
                Some(at_(6, 15, None)),
            ),
            ("heading to Cosmic at Franklin", "Cosmic", None),
            ("heading  to\n sup   dogs\nanyone?", "sup dogs", None),
            ("Heading to Cosmic Cantina 🌯🌯", "Cosmic Cantina", None),
            (
                "walking to weaver street rn want anything",
                "weaver street",
                Some(When::Now),
            ),
        ];
        for (text, place, when) in cases {
            let found = prefilter(text).unwrap_or_else(|| panic!("no trip in {text:?}"));
            assert_eq!(found.place, *place, "place of {text:?}");
            assert_eq!(found.when, *when, "when of {text:?}");
        }
    }

    #[test]
    fn prefilter_drops_non_trips() {
        let cases = [
            "going to be late lol",
            "heading to class",
            "I'm going to sleep",
            "lol same",
            "heading to the gym at 6",
            "heading to my room",
            "going to be there at 6",
            "heading to bed now",
            "on my way to the library",
            "on my way to the Library",
            "heading to class now",
            "heading to",
            "heading to, uh",
            "omw",
            "GOING TO CRY",
            "I'm going to get food at 6",
            "heading to this place",
            "heading to one two three four five six seven",
            // Ambiguous "going to" with nothing that makes it an offer.
            "going to cosmic",
            "are we going to the pit later",
            // Triggers are whole words.
            "foreheading tom",
            "",
        ];
        for text in cases {
            assert_eq!(prefilter(text), None, "{text:?}");
        }
        // Six words is still a place.
        assert_eq!(
            prefilter("heading to one two three four five six").map(|t| t.place),
            Some("one two three four five six".to_owned())
        );
    }

    #[test]
    fn prefilter_places_match_restaurants() {
        let all = restaurants();
        let cases = [
            (
                "heading to Cosmic Cantina at 6, anyone want anything?",
                "rst_cosmic",
            ),
            ("Heading to sup dogs at 6:30pm", "rst_supdogs"),
            ("anyone want anything from insomnia?", "rst_insomnia"),
            ("omw to lime & basil in 20 min", "rst_limebasil"),
            ("omw to lime and basil", "rst_limebasil"),
            ("stopping by the mediterranean deli at 7", "rst_meddeli"),
            ("picking up from alpine bagel rn", "rst_alpine"),
            ("headed to weaver street in 10 min", "rst_weaver"),
        ];
        for (text, id) in cases {
            let trip = prefilter(text).unwrap_or_else(|| panic!("no trip in {text:?}"));
            assert_eq!(match_place(&trip.place, &all), one(id), "{text:?}");
        }
    }

    #[test]
    fn parses_times() {
        let cases: &[(&str, When)] = &[
            ("at 6", at_(6, 0, None)),
            ("at 6:30", at_(6, 30, None)),
            ("at 6pm", at_(18, 0, Some(Pm))),
            ("at 6 pm", at_(18, 0, Some(Pm))),
            ("AT 6 PM", at_(18, 0, Some(Pm))),
            ("at 6 p.m. sharp", at_(18, 0, Some(Pm))),
            ("6:30pm", at_(18, 30, Some(Pm))),
            ("leaving 6:30", at_(6, 30, None)),
            ("6p?", at_(18, 0, Some(Pm))),
            ("at 7a", at_(7, 0, Some(Am))),
            ("at 9am", at_(9, 0, Some(Am))),
            ("at 12am", at_(0, 0, Some(Am))),
            ("at 12pm", at_(12, 0, Some(Pm))),
            ("at 12:30", at_(12, 30, None)),
            ("at 18:00", at_(18, 0, None)),
            ("at 23:59", at_(23, 59, None)),
            ("0:15", at_(0, 15, None)),
            ("around 7", at_(7, 0, None)),
            ("by 7", at_(7, 0, None)),
            ("at like 6", at_(6, 0, None)),
            ("at around 6:45", at_(6, 45, None)),
            ("cosmic @ 6:30", at_(6, 30, None)),
            ("at 6ish", at_(6, 0, None)),
            ("7ish?", at_(7, 0, None)),
            ("at noon", at_(12, 0, Some(Pm))),
            ("before noon", at_(12, 0, Some(Pm))),
            ("at midnight", at_(0, 0, Some(Am))),
            ("in 20 min", When::In { minutes: 20 }),
            ("in 20 mins", When::In { minutes: 20 }),
            ("in 20 minutes", When::In { minutes: 20 }),
            ("in 20min", When::In { minutes: 20 }),
            ("in 15m", When::In { minutes: 15 }),
            ("in 1 hr", When::In { minutes: 60 }),
            ("in 1 hour", When::In { minutes: 60 }),
            ("in 2 hours", When::In { minutes: 120 }),
            ("in an hour", When::In { minutes: 60 }),
            ("in an hour and a half", When::In { minutes: 90 }),
            ("in half an hour", When::In { minutes: 30 }),
            ("in a half hour", When::In { minutes: 30 }),
            ("back in 20", When::In { minutes: 20 }),
            ("in 20!", When::In { minutes: 20 }),
            ("now", When::Now),
            ("leaving right now", When::Now),
            ("omw rn", When::Now),
            // The first expression wins.
            ("at 6 or in 20 min", at_(6, 0, None)),
            ("rn, back at 6", When::Now),
            ("2 burritos at 6", at_(6, 0, None)),
        ];
        for (text, when) in cases {
            assert_eq!(parse_when(text), Some(*when), "{text:?}");
        }
    }

    #[test]
    fn rejects_non_times() {
        let cases = [
            "2 burritos please",
            "at 25:00",
            "at 6:75",
            "6:75",
            "25:00",
            "at 13pm",
            "at 0pm",
            "at 18",
            "at 0",
            "at 6:3",
            "2:1 odds",
            "123:45",
            "room 101",
            "6 pack",
            "$6.50",
            "at the pit",
            "I know",
            "snow day",
            "afternoon",
            "in the morning",
            "I'm in 2 classes",
            "in 1.5 hours",
            "in 500",
            "in 2000 min",
            "rnd",
            "ferry",
            "",
        ];
        for text in cases {
            assert_eq!(parse_when(text), None, "{text:?}");
        }
    }

    #[test]
    fn resolves_minutes() {
        let six = at_(6, 0, None);
        let cases: &[(When, u32, u32)] = &[
            // "at 6": the earliest of 6am and 6pm not more than 30 minutes past, else tomorrow 6am.
            (six, 17 * 60, 18 * 60),
            (six, 5 * 60, 6 * 60),
            (six, 19 * 60, DAY + 6 * 60),
            (six, 6 * 60 + 30, 6 * 60),
            (six, 6 * 60 + 31, 18 * 60),
            (six, 18 * 60 + 30, 18 * 60),
            (six, 18 * 60 + 31, DAY + 6 * 60),
            (at_(12, 0, None), 10 * 60, 12 * 60),
            (at_(12, 30, None), 10, 30),
            (at_(11, 0, None), 13 * 60, 23 * 60),
            // Definite times: today, or tomorrow after the grace.
            (at_(18, 30, Some(Pm)), 1000, 18 * 60 + 30),
            (at_(18, 30, Some(Pm)), 19 * 60, 18 * 60 + 30),
            (at_(18, 30, Some(Pm)), 19 * 60 + 1, DAY + 18 * 60 + 30),
            (at_(9, 0, Some(Am)), 8 * 60, 9 * 60),
            (at_(9, 0, Some(Am)), 12 * 60, DAY + 9 * 60),
            (at_(18, 0, None), 10 * 60, 18 * 60),
            (at_(0, 15, None), 10, 15),
            (at_(0, 15, None), 21 * 60, DAY + 15),
            (at_(12, 0, Some(Pm)), 11 * 60 + 40, 12 * 60),
            (at_(0, 0, Some(Am)), 23 * 60, DAY),
            // An unconverted hour with a meridiem.
            (at_(6, 0, Some(Pm)), 10 * 60, 18 * 60),
            (at_(12, 0, Some(Am)), 23 * 60 + 50, DAY),
            (When::In { minutes: 20 }, 1430, 1450),
            (When::Now, 500, 500),
            // A later day counts from its own midnight.
            (six, DAY + 17 * 60, DAY + 18 * 60),
        ];
        for (when, now, expected) in cases {
            assert_eq!(resolve_minutes(*when, *now), *expected, "{when:?} at {now}");
        }
    }

    #[test]
    fn parsed_times_resolve() {
        let resolve = |text: &str, now: u32| resolve_minutes(parse_when(text).unwrap(), now);
        assert_eq!(resolve("heading to cosmic at 6", 17 * 60), 18 * 60);
        assert_eq!(resolve("at 6:30pm", 17 * 60), 18 * 60 + 30);
        assert_eq!(resolve("at noon", 11 * 60), 12 * 60);
        assert_eq!(resolve("at 18:00", 17 * 60), 18 * 60);
        assert_eq!(resolve("in 20 min", 17 * 60), 17 * 60 + 20);
    }

    #[test]
    fn addressed_checks() {
        let bot = Some("Ferrygo_bot");
        let cases: &[(&str, Option<&str>, bool)] = &[
            ("@Ferrygo_bot what's open", bot, true),
            ("@ferrygo_bot hi", bot, true),
            ("/help@Ferrygo_bot", bot, true),
            ("thanks @FERRYGO_BOT!", bot, true),
            ("@Ferrygo_bot", Some("@Ferrygo_bot"), true),
            ("hey ferry", None, true),
            ("Ferry: menu", None, true),
            ("Ferry, what's open", None, true),
            ("FERRY what's open", None, true),
            ("/ferry", None, true),
            ("ferry's menu", None, true),
            ("hey ferry", bot, true),
            ("ferrying stuff", None, false),
            ("the ferryman", None, false),
            ("@other_ferry_bot", None, false),
            ("@Ferrygo_bot what's open", None, false),
            ("@Ferrygo_botx hi", bot, false),
            ("hi", bot, false),
            ("hi", Some(""), false),
            ("", bot, false),
        ];
        for (text, username, expected) in cases {
            assert_eq!(
                addressed(text, *username),
                *expected,
                "{text:?} {username:?}"
            );
        }
    }

    #[test]
    fn matches_places() {
        let all = restaurants();
        let cases = [
            ("cosmic", one("rst_cosmic")),
            ("Cosmic Cantina", one("rst_cosmic")),
            ("the cosmic cantina", one("rst_cosmic")),
            ("cosmic cantina on franklin", one("rst_cosmic")),
            ("cosm", one("rst_cosmic")),
            ("sup dogs", one("rst_supdogs")),
            ("SUP DOGS!", one("rst_supdogs")),
            ("supdogs", one("rst_supdogs")),
            ("sup", one("rst_supdogs")),
            ("lime and basil", one("rst_limebasil")),
            ("lime & basil", one("rst_limebasil")),
            ("Lime&Basil", one("rst_limebasil")),
            ("lime", one("rst_limebasil")),
            ("basil", one("rst_limebasil")),
            ("limeandbasil", one("rst_limebasil")),
            ("insomnia", one("rst_insomnia")),
            ("Insomnia Cookies", one("rst_insomnia")),
            ("mediterranean", one("rst_meddeli")),
            ("Mediterranean Deli", one("rst_meddeli")),
            ("alpine", one("rst_alpine")),
            ("alpine bagel", one("rst_alpine")),
            ("Alpine Bagel Café", one("rst_alpine")),
            ("weaver", one("rst_weaver")),
            ("weaver street", one("rst_weaver")),
            // Generic words alone name nothing.
            ("the deli", PlaceMatch::None),
            ("deli", PlaceMatch::None),
            ("cafe", PlaceMatch::None),
            ("cookies", PlaceMatch::None),
            ("street market", PlaceMatch::None),
            ("the", PlaceMatch::None),
            ("pizza", PlaceMatch::None),
            ("anti", PlaceMatch::None),
            ("cos", PlaceMatch::None),
            ("", PlaceMatch::None),
            ("  !! ", PlaceMatch::None),
            (
                "cosmic cantina or sup dogs",
                PlaceMatch::Several(vec!["rst_cosmic".into(), "rst_supdogs".into()]),
            ),
        ];
        for (place, expected) in cases {
            assert_eq!(match_place(place, &all), expected, "{place:?}");
        }
    }

    #[test]
    fn full_names_win_and_ties_are_several() {
        let all: Vec<(String, String)> = [
            ("rst_b", "Cosmic Cantina Durham"),
            ("rst_a", "Cosmic Cantina"),
            ("rst_c", "Cosmic Cafe"),
        ]
        .into_iter()
        .map(|(id, name)| (id.to_owned(), name.to_owned()))
        .collect();
        assert_eq!(match_place("cosmic cantina", &all), one("rst_a"));
        assert_eq!(match_place("cosmic cantina durham", &all), one("rst_b"));
        assert_eq!(
            match_place("cosmic", &all),
            PlaceMatch::Several(vec!["rst_b".into(), "rst_a".into(), "rst_c".into()])
        );
        assert_eq!(match_place("cosmic", &[]), PlaceMatch::None);
    }

    #[test]
    fn formats_clock() {
        let cases = [
            (370, "6:10"),
            (1450, "12:10"),
            (0, "12:00"),
            (5, "12:05"),
            (12 * 60, "12:00"),
            (12 * 60 + 5, "12:05"),
            (18 * 60, "6:00"),
            (DAY - 1, "11:59"),
            (2 * DAY + 65, "1:05"),
        ];
        for (minutes, expected) in cases {
            assert_eq!(clock(minutes), expected, "{minutes}");
        }
    }

    // ---------- meet-again ----------

    /// The brief's phrases (meet-place, 2026-10-11).
    const MEET_PHRASES: [&str; 3] = [
        "anyone want to go to Cosmic at 7?",
        "ask the people from last time if they want Cosmic at 7",
        "anyone want to get Sup Dogs at 8 tomorrow?",
    ];

    #[test]
    fn meet_phrases_are_not_trips() {
        // dispatch runs the trip prefilter before the turn: a meet-again must not open a run.
        for text in MEET_PHRASES {
            assert_eq!(prefilter(text), None, "{text}");
        }
        // A trip stays a trip.
        assert!(prefilter("heading to Cosmic Cantina at 6, anyone want anything?").is_some());
    }

    #[test]
    fn meet_mentions() {
        let at = |hour, meridiem| When::At {
            hour,
            minute: 0,
            meridiem,
        };
        let mention = |place: Option<&str>, when, tomorrow| {
            Some(MeetMention {
                place: place.map(str::to_owned),
                when,
                tomorrow,
            })
        };
        let cases = [
            (
                MEET_PHRASES[0],
                mention(Some("Cosmic"), Some(at(7, None)), false),
            ),
            (
                MEET_PHRASES[1],
                mention(Some("Cosmic"), Some(at(7, None)), false),
            ),
            (
                MEET_PHRASES[2],
                mention(Some("Sup Dogs"), Some(at(8, None)), true),
            ),
            (
                "who's down for sup dogs at 9pm",
                mention(Some("sup dogs"), Some(at(21, Some(Pm))), false),
            ),
            (
                "anyone wanna grab Lime & Basil tmrw at noon?",
                mention(Some("Lime & Basil"), Some(at(12, Some(Pm))), true),
            ),
            (
                "let's meet at the Pit at 1",
                mention(Some("Pit"), Some(at(1, None)), false),
            ),
            (
                "anyone want to get dinner at 7?",
                mention(None, Some(at(7, None)), false),
            ),
            (
                "anyone want to go to Cosmic?",
                mention(Some("Cosmic"), None, false),
            ),
            // Not a meet-again: nobody is asked, no place and no time, or not a place.
            ("I want to go to Cosmic at 7", None),
            ("anyone want food?", None),
            ("anyone want anything at 5?", None),
            ("going to Target at 5, anyone want anything?", None),
            (
                "heading to Cosmic at 6, anyone want to go to Sup Dogs?",
                None,
            ),
            ("anyone want to study?", None),
            ("anyone want to go to class at 9?", None),
            ("let's meet with the people I hung out with last time", None),
            ("count me in", None),
            ("can't make it", None),
            ("how long until my food gets here?", None),
        ];
        for (text, expected) in cases {
            assert_eq!(meet_mention(text), expected, "{text}");
        }
    }

    #[test]
    fn tomorrow_words() {
        for yes in ["tomorrow", "at 8 TOMORROW", "tmrw at 8", "tmr?", "2morrow"] {
            assert!(says_tomorrow(yes), "{yes}");
        }
        for no in ["today", "tomorrowland", "at 8", ""] {
            assert!(!says_tomorrow(no), "{no}");
        }
    }

    #[test]
    fn meetup_times_are_always_ahead() {
        let at = |hour, minute, meridiem| When::At {
            hour,
            minute,
            meridiem,
        };
        let h = |hour: u32, minute: u32| hour * 60 + minute;
        let cases = [
            // A bare hour: the next of am and pm, with no grace for one just past.
            (at(7, 0, None), h(15, 0), false, Some(h(19, 0))),
            (at(7, 0, None), h(5, 0), false, Some(h(7, 0))),
            (at(7, 0, None), h(7, 0), false, Some(h(19, 0))),
            (at(7, 0, None), h(19, 10), false, Some(DAY + h(7, 0))),
            (at(12, 0, None), h(9, 0), false, Some(h(12, 0))),
            // am/pm and 24-hour times: today, or tomorrow once passed.
            (at(19, 0, Some(Pm)), h(15, 0), false, Some(h(19, 0))),
            (at(19, 0, Some(Pm)), h(19, 5), false, Some(DAY + h(19, 0))),
            (at(18, 30, None), h(9, 0), false, Some(h(18, 30))),
            (at(9, 0, Some(Am)), h(10, 0), false, Some(DAY + h(9, 0))),
            // Tomorrow: a bare hour is the evening's, 12 is noon; am/pm as said.
            (at(8, 0, None), h(15, 0), true, Some(DAY + h(20, 0))),
            (at(8, 0, None), h(1, 0), true, Some(DAY + h(20, 0))),
            (at(12, 0, None), h(15, 0), true, Some(DAY + h(12, 0))),
            (at(9, 0, Some(Am)), h(15, 0), true, Some(DAY + h(9, 0))),
            // In: from now; now means no time.
            (When::In { minutes: 30 }, h(23, 50), false, Some(h(24, 20))),
            (When::In { minutes: 30 }, h(10, 0), true, Some(h(10, 30))),
            (When::In { minutes: 0 }, h(10, 0), false, None),
            (When::Now, h(10, 0), false, None),
        ];
        for (when, now, tomorrow, expected) in cases {
            assert_eq!(
                meetup_minutes(when, now, tomorrow),
                expected,
                "{when:?} at {now} tomorrow {tomorrow}"
            );
        }
    }
}
