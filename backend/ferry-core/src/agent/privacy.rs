//! What may reach Gemini (Sal's rule, review checklist Privacy): never a name, a Telegram username, a
//! phone number, an email or an address. People are labels in braces (`{runner}`, `{invitee_1}`)
//! that code fills in after Gemini answers, and so are the user's drop-off (`{dropoff}`) and their
//! circles' names (`{circle_1}`), which are free text.
//!
//! - [`Redactor::scrub`] rewrites user text before it goes out: known names, and each part of three
//!   or more letters of a person's name, become their placeholders; `@handles` anywhere in a word
//!   become `@someone`; and phone numbers, emails and street addresses become `[phone]`, `[email]`
//!   and `[address]`.
//! - [`Redactor::seal`] is the only way to make a [`Sealed`] request, the only type the Gemini
//!   client sends. It reads every string in the decoded payload and refuses one that still holds a
//!   forbidden term, an email, a phone number, a street address, an `@handle` or a `usr_` id, so a
//!   leak fails closed: the turn falls back to the deterministic answer and nothing is sent.
//! - [`Redactor::fill`] puts the names back into the reply.
//!
//! Matching is by character, case-folded one character at a time, so a letter whose lowercase is
//! longer (`İ`) changes nothing else and never splits a character. A name part that is also a word
//! of the fixed text Gemini always gets (the tools' declarations, the instructions, the menus;
//! [`Redactor::with_common_words`]) is neither scrubbed nor forbidden on its own: "Chicken" or
//! "Basil" as someone's name would otherwise refuse every request of their circle, and turn
//! "chicken burrito" into "{person_1} burrito". Their full name still is.
//!
//! What this cannot catch is in docs/DECISIONS.md (Agent, Privacy): a name Ferry never stored
//! (a stranger's, or another member's Telegram first name), a nickname or partial spelling, and a
//! name part that is a common word of the fixed text.

use std::collections::HashSet;

use serde_json::Value;

/// A request body that passed [`Redactor::seal`]: the only thing `gemini::Gemini` sends.
#[derive(Debug, Clone)]
pub struct Sealed(Value);

impl Sealed {
    pub fn as_value(&self) -> &Value {
        &self.0
    }

    /// For the Gemini client's own unit tests, which have no names to check.
    #[cfg(test)]
    pub(crate) fn unchecked(value: Value) -> Self {
        Sealed(value)
    }
}

/// A payload [`Redactor::seal`] refused. It names no term, so it can be logged.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the Gemini payload holds a name, username or contact detail; not sent")]
pub struct Leak;

/// The names and labels of one turn.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    /// (folded term, placeholder): names to rewrite in user text, longest first.
    names: Vec<(String, String)>,
    /// Folded terms that must never reach Gemini: every name, name part and username.
    forbidden: Vec<String>,
    /// (placeholder, shown): what [`Redactor::fill`] writes back.
    fills: Vec<(String, String)>,
    /// Folded words of the fixed text every request carries: a term made only of these is not
    /// registered ([`Redactor::with_common_words`]).
    common: HashSet<String>,
}

/// Names shorter than this are not matched: one letter would hit every sentence.
const MIN_TERM_CHARS: usize = 2;
/// A part of a name ("Maya" of "Maya Lopez") is matched from this length.
const MIN_PART_CHARS: usize = 3;
/// What an `@handle` becomes.
const SOMEONE: &str = "someone";

impl Redactor {
    pub fn new() -> Self {
        Self::default()
    }

    /// A redactor that leaves alone any term made only of words of `fixed` (the text every
    /// request carries: instructions, declarations, the catalog's names).
    pub fn with_common_words<'a>(fixed: impl IntoIterator<Item = &'a str>) -> Self {
        let mut common = HashSet::new();
        for text in fixed {
            common.extend(words(&fold(text)));
        }
        Self {
            common,
            ..Self::default()
        }
    }

    /// Registers a person under `label` (`runner`, `invitee_1`): every name in `names` (a display
    /// name, a Telegram first name) and `display`, and each of their parts of
    /// [`MIN_PART_CHARS`] or more letters, is rewritten to `{label}` in user text and forbidden in
    /// payloads, and [`Redactor::fill`] writes `display` back for `{label}`. Returns `{label}`.
    /// Registering the same label again adds names to it.
    pub fn person(&mut self, label: &str, display: &str, names: &[&str]) -> String {
        let mut all: Vec<&str> = names.to_vec();
        all.push(display);
        let placeholder = self.private(label, display, &all);
        for name in &all {
            for part in name.split(|c: char| c.is_whitespace() || c == '-') {
                let part = part.trim_matches(|c: char| !c.is_alphanumeric());
                if part.chars().count() >= MIN_PART_CHARS {
                    self.register(part, &placeholder);
                }
            }
        }
        let shown = display.trim();
        self.fill_as(label, if shown.is_empty() { "Someone" } else { shown });
        placeholder
    }

    /// Registers private free text (a drop-off label, a circle's name) under `label`: each of
    /// `names` as a whole is rewritten to `{label}` in user text and forbidden in payloads, and
    /// [`Redactor::fill`] writes `shown` back. Returns `{label}`.
    pub fn private(&mut self, label: &str, shown: &str, names: &[&str]) -> String {
        let placeholder = format!("{{{label}}}");
        for name in names {
            self.register(name, &placeholder);
        }
        self.fill_as(label, shown);
        placeholder
    }

    /// What [`Redactor::fill`] writes for `{label}` from now on.
    pub fn fill_as(&mut self, label: &str, shown: &str) {
        let placeholder = format!("{{{label}}}");
        match self.fills.iter_mut().find(|(p, _)| *p == placeholder) {
            Some(entry) => shown.clone_into(&mut entry.1),
            None => self.fills.push((placeholder, shown.to_owned())),
        }
    }

    fn register(&mut self, name: &str, placeholder: &str) {
        let term = normalize(name);
        if term.chars().count() < MIN_TERM_CHARS || self.is_common(&term) {
            return;
        }
        if !self.names.iter().any(|(t, _)| *t == term) {
            self.names.push((term.clone(), placeholder.to_owned()));
            // Longest first, so "Mary Ann" wins over "Mary".
            self.names
                .sort_by_key(|(t, _)| std::cmp::Reverse(t.chars().count()));
        }
        self.forbid_term(term);
    }

    /// Forbids `term` (a username, with or without `@`) in payloads without giving it a label.
    pub fn forbid(&mut self, term: &str) {
        let term = normalize(term.trim().trim_start_matches('@'));
        if term.chars().count() >= MIN_TERM_CHARS && !self.is_common(&term) {
            self.forbid_term(term);
        }
    }

    fn forbid_term(&mut self, term: String) {
        if !self.forbidden.contains(&term) {
            self.forbidden.push(term);
        }
    }

    /// Whether every word of `term` is a word of the fixed text.
    fn is_common(&self, term: &str) -> bool {
        let words = words(term);
        !words.is_empty() && words.iter().all(|w| self.common.contains(w))
    }

    /// The placeholder registered for `label`, if any.
    pub fn placeholder(&self, label: &str) -> Option<&str> {
        let wanted = format!("{{{label}}}");
        self.fills
            .iter()
            .find(|(p, _)| *p == wanted)
            .map(|(p, _)| p.as_str())
    }

    /// `text` with names, @handles, phone numbers, emails and street addresses replaced. Names
    /// and labels go first, so a drop-off label that holds an address becomes `{dropoff}` whole.
    pub fn scrub(&self, text: &str) -> String {
        let mut words: Vec<String> = Vec::new();
        for token in text.split_inclusive(char::is_whitespace) {
            let (word, space) = split_trailing_space(token);
            words.push(scrub_token(word));
            if !space.is_empty() {
                words.push(space.to_owned());
            }
        }
        let named = self.replace_names(&words.concat());
        scrub_phones(&scrub_addresses(&named))
    }

    /// `text` with every registered name and label, and every username, replaced as a whole word.
    fn replace_names(&self, text: &str) -> String {
        // Longest first: full names and labels before their parts and usernames.
        let mut terms: Vec<(&str, &str)> = self
            .names
            .iter()
            .map(|(t, p)| (t.as_str(), p.as_str()))
            .collect();
        for term in &self.forbidden {
            if !self.names.iter().any(|(t, _)| t == term) {
                terms.push((term.as_str(), SOMEONE));
            }
        }
        terms.sort_by_key(|(t, _)| std::cmp::Reverse(t.chars().count()));
        replace_terms(text, &terms)
    }

    /// `value` as a [`Sealed`] request, or [`Leak`] when any string in it (read decoded, so an
    /// escape such as `\n` hides nothing) holds a forbidden term as a whole word (any case), an
    /// email, a phone number, a street address, an `@handle` or a `usr_` id.
    pub fn seal(&self, value: Value) -> Result<Sealed, Leak> {
        let mut strings = Vec::new();
        collect_strings(&value, &mut strings);
        if strings.iter().all(|s| self.is_clean(s)) {
            Ok(Sealed(value))
        } else {
            Err(Leak)
        }
    }

    fn is_clean(&self, text: &str) -> bool {
        self.forbidden
            .iter()
            .all(|term| word_matches(text, term).is_empty())
            && !text
                .split(|c: char| c.is_whitespace() || c == '"')
                .any(|w| is_email(w.trim_matches(|c: char| !c.is_alphanumeric())))
            && scrub_phones(text) == text
            && scrub_addresses(text) == text
            && !has_handle(text)
            && !text.to_lowercase().contains("usr_")
    }

    /// `text` with every registered placeholder replaced by its display name. Any other
    /// `{label}` Gemini made up becomes "someone".
    pub fn fill(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(start) = rest.find('{') {
            out.push_str(&rest[..start]);
            let after = &rest[start..];
            match after.find('}') {
                Some(end) if is_label(&after[1..end]) => {
                    let placeholder = &after[..=end];
                    match self.fills.iter().find(|(p, _)| p == placeholder) {
                        Some((_, name)) => out.push_str(name),
                        None => out.push_str(SOMEONE),
                    }
                    rest = &after[end + 1..];
                }
                _ => {
                    out.push('{');
                    rest = &after[1..];
                }
            }
        }
        out.push_str(rest);
        out
    }
}

/// Every string in `value`, object keys left out (they are code's own names).
fn collect_strings<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(s) => out.push(s),
        Value::Array(items) => items.iter().for_each(|v| collect_strings(v, out)),
        Value::Object(map) => map.values().for_each(|v| collect_strings(v, out)),
        _ => {}
    }
}

fn is_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 40
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// One character case-folded: the first character of its lowercase, so the length in characters
/// never changes (`İ` is `i`).
fn fold_char(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn fold(text: &str) -> String {
    text.chars().map(fold_char).collect()
}

/// Folded, with surrounding whitespace trimmed and inner runs of whitespace collapsed.
fn normalize(value: &str) -> String {
    fold(&value.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The words of `text`: its runs of letters and digits.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

fn split_trailing_space(token: &str) -> (&str, &str) {
    let trimmed = token.trim_end_matches(char::is_whitespace);
    (trimmed, &token[trimmed.len()..])
}

/// One whitespace-free token: an email becomes `[email]`, and each `@handle` in it `@someone`.
fn scrub_token(word: &str) -> String {
    if is_email(word.trim_matches(|c: char| !c.is_alphanumeric())) {
        return "[email]".to_owned();
    }
    let mut out = String::with_capacity(word.len());
    let mut chars = word.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == '@' && chars.peek().is_some_and(|n| n.is_alphanumeric()) {
            while chars
                .peek()
                .is_some_and(|n| n.is_alphanumeric() || *n == '_')
            {
                chars.next();
            }
            out.push_str(SOMEONE);
        }
    }
    out
}

/// Whether `text` holds an `@` followed by a letter or digit, other than `@someone`.
fn has_handle(text: &str) -> bool {
    text.match_indices('@').any(|(i, _)| {
        let rest = &text[i + 1..];
        if !rest.chars().next().is_some_and(char::is_alphanumeric) {
            return false;
        }
        let scrubbed = rest.strip_prefix(SOMEONE).is_some_and(|after| {
            !after
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        });
        !scrubbed
    })
}

fn is_email(word: &str) -> bool {
    let Some((user, domain)) = word.split_once('@') else {
        return false;
    };
    !user.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && user
            .chars()
            .all(|c| c.is_alphanumeric() || "._%+-".contains(c))
        && domain
            .chars()
            .all(|c| c.is_alphanumeric() || ".-".contains(c))
}

/// Runs of 7 or more digits, allowing spaces, dots, dashes, parentheses and a leading `+` between
/// them, become `[phone]`.
fn scrub_phones(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let starts = chars[i].is_ascii_digit() || (chars[i] == '+' || chars[i] == '(');
        if starts && (i == 0 || !chars[i - 1].is_alphanumeric()) {
            let mut j = i;
            let mut digits = 0;
            let mut last_digit_end = i;
            while j < chars.len() && (chars[j].is_ascii_digit() || " .-()+".contains(chars[j])) {
                if chars[j].is_ascii_digit() {
                    digits += 1;
                    last_digit_end = j + 1;
                }
                j += 1;
            }
            let bounded = last_digit_end == chars.len() || !chars[last_digit_end].is_alphanumeric();
            if digits >= 7 && bounded {
                out.push_str("[phone]");
                i = last_digit_end;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Street words that end an address such as "410 W Franklin St".
const STREET_SUFFIXES: [&str; 18] = [
    "st",
    "street",
    "ave",
    "avenue",
    "rd",
    "road",
    "dr",
    "drive",
    "ln",
    "lane",
    "blvd",
    "boulevard",
    "ct",
    "court",
    "way",
    "pl",
    "place",
    "hwy",
];

/// A word without the punctuation around it, folded.
fn bare(word: &str) -> String {
    fold(word.trim_matches(|c: char| !c.is_alphanumeric()))
}

/// A house number (1 to 5 digits, punctuation around it allowed) followed within 4 words by a
/// street suffix (punctuation after it allowed: "Street.", "St,"): the number through the suffix
/// becomes `[address]`, and the punctuation around it stays.
fn scrub_addresses(text: &str) -> String {
    let tokens: Vec<&str> = text.split_inclusive(char::is_whitespace).collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < tokens.len() {
        let number = bare(tokens[i]);
        let is_number =
            (1..=5).contains(&number.len()) && number.bytes().all(|b| b.is_ascii_digit());
        if is_number {
            let end = (i + 1..tokens.len().min(i + 6))
                .find(|&j| STREET_SUFFIXES.contains(&bare(tokens[j]).as_str()));
            if let Some(end) = end {
                let first = tokens[i].trim();
                let leading: String = first.chars().take_while(|c| !c.is_alphanumeric()).collect();
                let last = tokens[end];
                let word = last.trim_end();
                let trailing = &word[word
                    .char_indices()
                    .rev()
                    .find(|(_, c)| c.is_alphanumeric())
                    .map_or(0, |(i, c)| i + c.len_utf8())..];
                out.push_str(&leading);
                out.push_str("[address]");
                out.push_str(trailing);
                out.push_str(&last[word.len()..]);
                i = end + 1;
                continue;
            }
        }
        out.push_str(tokens[i]);
        i += 1;
    }
    out
}

/// Byte ranges of `term` (folded) in `haystack` as a whole word, ignoring case, matched character
/// by character. `_` counts as a boundary, so a fixture id such as `usr_maya` holds the name
/// `maya`.
fn word_matches(haystack: &str, term: &str) -> Vec<(usize, usize)> {
    let term: Vec<char> = term.chars().collect();
    let hay: Vec<(usize, char)> = haystack
        .char_indices()
        .map(|(i, c)| (i, fold_char(c)))
        .collect();
    let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
    let mut found = Vec::new();
    if term.is_empty() {
        return found;
    }
    let mut i = 0;
    while i + term.len() <= hay.len() {
        let here = hay[i..i + term.len()]
            .iter()
            .map(|(_, c)| *c)
            .eq(term.iter().copied());
        let before = i.checked_sub(1).map(|j| hay[j].1);
        let after = hay.get(i + term.len()).map(|(_, c)| *c);
        if here && boundary(before) && boundary(after) {
            let start = hay[i].0;
            let end = hay
                .get(i + term.len())
                .map_or(haystack.len(), |(byte, _)| *byte);
            found.push((start, end));
            i += term.len();
        } else {
            i += 1;
        }
    }
    found
}

/// `text` with each `(term, replacement)` replaced as a whole word, in one pass: `terms` come
/// longest first, an earlier term's match wins, and a replacement is never matched again (so
/// "{person_1}" never becomes "{{person_2}_1}").
fn replace_terms(text: &str, terms: &[(&str, &str)]) -> String {
    let mut taken: Vec<(usize, usize, &str)> = Vec::new();
    for (term, replacement) in terms {
        for (start, end) in word_matches(text, term) {
            if taken.iter().all(|(s, e, _)| end <= *s || start >= *e) {
                taken.push((start, end, replacement));
            }
        }
    }
    taken.sort_by_key(|(start, ..)| *start);
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (start, end, replacement) in taken {
        out.push_str(&text[last..start]);
        out.push_str(replacement);
        last = end;
    }
    out.push_str(&text[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn redactor() -> Redactor {
        let mut r = Redactor::with_common_words(["Chicken Burrito, Basil Pesto Pasta; Max Shake"]);
        r.person("runner", "Maya Lopez", &["Maya L"]);
        r.person("you", "Dana", &["Jordy"]);
        r.forbid("@maya_l");
        r.forbid("sam_k99");
        r.private(
            "dropoff",
            "Zelda's place, 123 Main Street",
            &["Zelda's place, 123 Main Street"],
        );
        r.private(
            "circle_1",
            "🌊 Sam Smith's apt",
            &["Sam Smith's apt", "🌊 Sam Smith's apt"],
        );
        r
    }

    #[test]
    fn scrubs_names_mentions_and_contacts() {
        let r = redactor();
        assert_eq!(
            r.scrub("where is maya? tell @maya_l and Jordy"),
            "where is {runner}? tell @someone and {you}"
        );
        assert_eq!(r.scrub("Maya L is late"), "{runner} is late");
        assert_eq!(r.scrub("Maya Lopez is late"), "{runner} is late");
        // Each part of three or more letters of a name.
        assert_eq!(
            r.scrub("is maya bringing my food?"),
            "is {runner} bringing my food?"
        );
        assert_eq!(r.scrub("LOPEZ!"), "{runner}!");
        assert_eq!(r.scrub("ask maya_l"), "ask someone");
        // Whole words only.
        assert_eq!(r.scrub("Mayanna and jordanian"), "Mayanna and jordanian");
        assert_eq!(
            r.scrub("call me at +1 (919) 555-0102 or maya@example.com"),
            "call me at [phone] or [email]"
        );
        assert_eq!(
            r.scrub("drop it at 410 W Franklin St, thanks"),
            "drop it at [address], thanks"
        );
        assert_eq!(
            r.scrub("heading to Cosmic Cantina at 6, anyone?"),
            "heading to Cosmic Cantina at 6, anyone?"
        );
        assert_eq!(r.scrub("2 burritos at 6:30"), "2 burritos at 6:30");
    }

    #[test]
    fn scrubs_the_reviews_inputs() {
        let r = redactor();
        // An @handle anywhere in a word.
        assert_eq!(
            r.scrub("ping my roommate (@sam_k99) about dinner"),
            "ping my roommate (@someone) about dinner"
        );
        assert_eq!(r.scrub("cc:@bob,@al_9!"), "cc:@someone,@someone!");
        // Trailing and leading punctuation around an address.
        assert_eq!(
            r.scrub("bring it to 412 Pittsboro Street."),
            "bring it to [address]."
        );
        assert_eq!(r.scrub("(412 Pittsboro St)"), "([address])");
        assert_eq!(r.scrub("at 5 Elm Rd!? ok"), "at [address]!? ok");
        // A letter whose lowercase is longer changes nothing else, and never panics.
        assert_eq!(
            r.scrub("Back from İstanbul!\nDana says hi"),
            "Back from İstanbul!\n{you} says hi"
        );
        assert_eq!(r.scrub("İ maya’Å"), "İ {runner}’Å");
        assert_eq!(r.scrub("İİİ MAYA İ"), "İİİ {runner} İ");
        // The drop-off and the circle as a whole.
        assert_eq!(
            r.scrub("bring it to Zelda's place, 123 Main Street"),
            "bring it to {dropoff}"
        );
        assert_eq!(
            r.scrub("thanks from 🌊 Sam Smith's apt"),
            "thanks from {circle_1}"
        );
        // A replacement is never matched again.
        let mut odd = Redactor::new();
        odd.person("person_1", "Person", &[]);
        odd.person("person_2", "Ann", &[]);
        assert_eq!(odd.scrub("person and ann"), "{person_1} and {person_2}");
    }

    #[test]
    fn common_words_are_not_names() {
        let mut r = Redactor::with_common_words(["Chicken Burrito", "Basil", "Max Shake"]);
        r.person("person_1", "Chicken Little", &[]);
        r.person("person_2", "Basil", &[]);
        r.person("person_3", "Max Power", &[]);
        // "chicken" and "basil" stay food, and nothing that holds them is refused.
        assert_eq!(
            r.scrub("chicken burrito no beans"),
            "chicken burrito no beans"
        );
        assert_eq!(r.scrub("add basil"), "add basil");
        assert!(
            r.seal(json!({"text": "Chicken Burrito with Basil, a Max Shake"}))
                .is_ok()
        );
        // Full names, and parts that are not common words, still are.
        assert_eq!(
            r.scrub("chicken little and power"),
            "{person_1} and {person_3}"
        );
        assert!(r.seal(json!({"text": "chicken little"})).is_err());
        assert!(r.seal(json!({"text": "max power"})).is_err());
    }

    #[test]
    fn seal_refuses_names_and_contacts() {
        let r = redactor();
        assert!(r.seal(json!({"text": "{runner} is 4 min away"})).is_ok());
        assert_eq!(
            r.seal(json!({"text": "MAYA is 4 min away"})).unwrap_err(),
            Leak
        );
        assert!(r.seal(json!({"a": ["maya_l"]})).is_err());
        assert!(r.seal(json!({"a": "x@y.com"})).is_err());
        assert!(r.seal(json!({"a": "+19195550102"})).is_err());
        // Labels, other ids and short numbers are fine.
        assert!(
            r.seal(json!({"run": "run_demo", "eta_minutes": 4, "price_cents": 899}))
                .is_ok()
        );
        // A fixture user id holds a name, and no user id ever goes out.
        assert!(r.seal(json!({"user": "usr_maya"})).is_err());
        assert!(r.seal(json!({"user": "USR_x1"})).is_err());
    }

    #[test]
    fn seal_reads_decoded_strings_and_refuses_the_reviews_inputs() {
        let r = redactor();
        // Behind an escaped line break, the name is still a word.
        assert!(
            r.seal(json!({"text": "Back from İstanbul!\nDana says hi"}))
                .is_err()
        );
        assert!(
            r.seal(json!({"text": "Back from İstanbul!\n{you} says hi"}))
                .is_ok()
        );
        assert!(r.seal(json!({"text": "is lopez coming"})).is_err());
        assert!(r.seal(json!({"text": "ping (@sam_k99)"})).is_err());
        assert!(r.seal(json!({"text": "ping @someone"})).is_ok());
        assert!(r.seal(json!({"text": "to 412 Pittsboro Street."})).is_err());
        assert!(
            r.seal(json!({"text": "to Zelda's place, 123 Main Street"}))
                .is_err()
        );
        assert!(r.seal(json!({"text": "from 🌊 Sam Smith's apt"})).is_err());
        assert!(
            r.seal(json!({"parts": [{"text": "to {dropoff}, from {circle_1}"}]}))
                .is_ok()
        );
        // An unregistered handle or address is refused even without a name list.
        let empty = Redactor::new();
        assert!(empty.seal(json!({"text": "hi @someone_else"})).is_err());
        assert!(empty.seal(json!({"text": "10 Downing St"})).is_err());
        assert!(
            empty
                .seal(json!({"text": "Chicken Burrito $8.99, 2 at 6:30"}))
                .is_ok()
        );
    }

    #[test]
    fn fill_puts_names_back() {
        let r = redactor();
        assert_eq!(
            r.fill("{runner} is 4 min away (0.3 mi), seen 20 s ago."),
            "Maya Lopez is 4 min away (0.3 mi), seen 20 s ago."
        );
        assert_eq!(r.fill("{stranger} and {you}"), "someone and Dana");
        assert_eq!(r.fill("to {dropoff}"), "to Zelda's place, 123 Main Street");
        assert_eq!(r.fill("a {b c} {"), "a {b c} {");
    }
}
