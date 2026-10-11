//! The text side of ordering by message (spec section 3): in a group, someone replies "chicken
//! burrito no beans" and the bot reads it against the open run's menu into order lines (item id,
//! quantity, choice ids), which code then prices and asks to confirm. Pure and deterministic: no
//! I/O, no clock, no database, no Gemini.
//!
//! - [`parse_order`] reads a new order: every item in the order written, each with its quantity
//!   and choices, or the problem to ask about: an ambiguous item ("burrito"), a required option
//!   with no choice, or no item at all.
//! - [`parse_edit`] applies choice edits to lines already drafted ("add guac", "make it pinto
//!   beans", "no sour cream").
//! - [`mentions_item`] is the cheap check that a group message might be an order.
//!
//! **Words.** Text and names are normalized the same way: lowercase, accents folded, `&` as "and",
//! apostrophes dropped, other punctuation a space ("w/o" is "without", a lone "n" is "and"). A text
//! word matches a name word when the two are equal once a trailing plural "s" (or "es" after ch, sh
//! or x) is dropped from words longer than 3 letters; when the text word has at least 4 letters, is
//! not a filler word and starts the name word ("guac", "jalap", "quesa"; never the other way
//! round); or through a short alias list ("shake" for Milkshake, "fry" for Fries). `,` `.` `;` `!`
//! `?` `:`, brackets and line breaks end a clause; `+` and `/` do not.
//!
//! **Items.** An item matches where its full name appears, or where text words match a contiguous
//! run of its distinctive words (the name without "loaded", "classic", "sup", "the", "a", "and",
//! "with", "of"), with "and", "with", "w" or "plus" allowed between two of them: "nachos" is Loaded
//! Nachos, "burger" Sup Burger, "chips with guac" Chips & Guacamole. Matches never overlap: the
//! scan goes left to right, the longest match wins, a full name word for word (no prefixes) beats
//! partial matches of the same length, and several items left make the words ambiguous
//! ("burrito", "chips", "dog"; "cookie" for Classic Cookie and Cookiewich). Two rules
//! keep chat from reading as orders:
//! - a partial match whose words are all choice words on the menu ("chicken", "steak", "cheese",
//!   "beans", "guac") is never an item: such words modify an item, or mean nothing;
//! - a partial match right after a brand word ("sup", "classic", "loaded") that the menu uses but
//!   the item's name lacks belongs to another name: "heading to sup dogs" names no dog.
//!
//! After an item, a choice of it wins over an item match no longer than the choice; a longer item
//! match is a new item ("quesadilla with chicken burrito" is two items).
//!
//! **Choices.** The words after an item, up to the next item, modify it. Words right before an
//! item that are its choices ("vanilla shake", "large fries", "chicken quesadilla") modify it
//! instead, when the item before has no such choice or a clause break comes first. A choice matches
//! by its full name ("no beans", "black beans", "extra cheese", "cookies and cream") or by a key
//! word: a word of its name with at least 3 letters, not filler, in no other choice of the item
//! ("pinto", "guac", "cheddar", "jack"). A choice named "No ..." matches only by its full name or
//! as what a negation names ("without beans" is No beans). Otherwise "no", "without", "hold (the)",
//! "remove", "minus", "skip", "take off", "instead of", "don't want" before a choice drop it, and
//! "or"/"nor" carry the negation on ("no onions or relish"). An option with max 1 keeps the latest
//! choice; others keep the first `max`. Choice ids come out in menu order.
//!
//! **Quantity.** A number, a number word ("a", "two", "dozen") or "2x" right before the item or its
//! leading choices, or "x2" after it ("2x" too, unless an item follows in the same clause); 1 by
//! default, capped at 20.

use serde::{Deserialize, Serialize};

/// A choice of a menu option.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceRef {
    pub id: String,
    pub name: String,
}

/// An option of a menu item: an order line picks between `min` and `max` of its choices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionRef {
    pub id: String,
    pub name: String,
    pub min: i32,
    pub max: i32,
    pub choices: Vec<ChoiceRef>,
}

/// A menu item with its options, as the run's restaurant menu has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemRef {
    pub id: String,
    pub name: String,
    pub options: Vec<OptionRef>,
}

/// One order line as `orders::OrderLineInput` takes it (the caller converts).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedLine {
    pub menu_item_id: String,
    /// 1 to 20.
    pub qty: i64,
    /// In menu order: by option, then by choice.
    pub choice_ids: Vec<String>,
}

/// What [`parse_order`] read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseOutcome {
    /// Every item found, with its quantity and choices, in the order written.
    Lines(Vec<ParsedLine>),
    /// Words that match several items ("burrito" → chicken, steak, bean & cheese): the words as
    /// normalized, and the candidates' ids in menu order. Reported before any missing choice, for
    /// the first ambiguous item in the text.
    Ambiguous {
        words: String,
        item_ids: Vec<String>,
    },
    /// An item whose required option (min ≥ 1) got fewer choices than its min: the line so far,
    /// and the option's id. The first such line in the text, and its first such option.
    MissingChoice { line: ParsedLine, option_id: String },
    /// No menu item in the text.
    NoItems,
}

/// Quantities above this are read as this.
const MAX_QTY: i64 = 20;

/// Words of an item's name that name nothing on their own ("Loaded Nachos" is "nachos").
const ITEM_GENERIC: &[&str] = &[
    "loaded", "classic", "sup", "the", "a", "an", "and", "with", "of",
];

/// Generic words that brand an item's name ("Sup Burger"). A partial match right after one that
/// the item's name lacks is part of another name ("sup dogs" is the restaurant).
const BRAND_WORDS: &[&str] = &["loaded", "classic", "sup"];

/// Words allowed between two distinctive words of an item's name ("chips with guac").
const CONNECTORS: &[&str] = &["and", "with", "w", "plus"];

/// Words around choices that mean nothing alone ("add", "with", "please"). Never a choice's key
/// word, and never a prefix match; they still match themselves inside a full name ("Extra cheese",
/// "Cookies & cream").
const FILLER: &[&str] = &[
    "add", "with", "w", "plus", "extra", "and", "also", "too", "please", "pls", "plz", "some",
    "on", "the", "a", "an", "of", "it", "make", "to", "more", "any", "my", "only", "side",
    "instead", "no", "or", "nor", "but", "for", "me", "can", "get", "want", "like",
];

/// Phrases that drop the choice after them, longest first where one starts another.
const NEGATIONS: &[&[&str]] = &[
    &["dont", "want"],
    &["dont", "add"],
    &["dont", "put"],
    &["take", "off"],
    &["take", "out"],
    &["instead", "of"],
    &["no"],
    &["without"],
    &["hold"],
    &["minus"],
    &["remove"],
    &["skip"],
    &["nix"],
    &["sans"],
];

/// Words skipped between a negation and its choice ("hold the onions", "no more guac").
const ARTICLES: &[&str] = &[
    "the", "any", "my", "more", "some", "that", "those", "of", "a", "an",
];

/// Number words for quantities.
const NUMBERS: &[(&str, i64)] = &[
    ("a", 1),
    ("an", 1),
    ("one", 1),
    ("two", 2),
    ("three", 3),
    ("four", 4),
    ("five", 5),
    ("six", 6),
    ("seven", 7),
    ("eight", 8),
    ("nine", 9),
    ("ten", 10),
    ("eleven", 11),
    ("twelve", 12),
    ("dozen", 12),
];

/// (text stem, name stem) pairs that match although neither starts the other.
const ALIASES: &[(&str, &str)] = &[("shake", "milkshake"), ("fry", "frie")];

/// Chars that end a clause.
const BREAKS: &[char] = &[
    ',', '.', ';', '!', '?', ':', '(', ')', '[', ']', '|', '\n', '\r', '—', '–',
];

/// Parses `text` against `menu` (the open run's restaurant menu) as a new order.
///
/// Lines when every item is known and has its required choices. Otherwise the first ambiguous
/// item, else the first line missing a required choice, else NoItems when no item is named ("lol
/// same", "heading to cosmic at 6", "add guac").
pub fn parse_order(text: &str, menu: &[ItemRef]) -> ParseOutcome {
    let menu = Menu::new(menu);
    let words = tokenize(text);
    let drafts = read_order(&menu, &words);
    if drafts.is_empty() {
        return ParseOutcome::NoItems;
    }
    // An ambiguous item's options cannot be checked until it is known, so it is asked first.
    if let Some(draft) = drafts.iter().find(|d| d.items.len() > 1) {
        return ParseOutcome::Ambiguous {
            words: join(&words[draft.start..draft.end]),
            item_ids: draft
                .items
                .iter()
                .map(|&it| menu.items[it].item.id.clone())
                .collect(),
        };
    }
    let lines: Vec<ParsedLine> = drafts.iter().map(|d| d.line(&menu)).collect();
    for (draft, line) in drafts.iter().zip(&lines) {
        if let Some(option_id) = draft.missing(&menu) {
            return ParseOutcome::MissingChoice {
                line: line.clone(),
                option_id,
            };
        }
    }
    ParseOutcome::Lines(lines)
}

/// Choice edits for lines already drafted ("add guac", "with sour cream", "no sour cream",
/// "without jalapenos", "remove the guac", "make it pinto beans", "black beans instead", "extra
/// cheese too"): the lines with the edits applied, or None when `text` names no choice of these
/// lines' items, or names a menu item (then it is a new order, not an edit).
///
/// An added choice goes to the first line whose item has it: for an option with max 1 it replaces
/// that option's choice, otherwise it is added unless the option is full. A dropped choice leaves
/// the first line that has it, unless that would leave a required option with fewer than its min
/// (only a replacement changes that: "make it pinto beans", "no beans"). An edit that changes
/// nothing (a choice already there, or not there to drop) still counts as an edit. Edited lines
/// get their choice ids in menu order, with ids the menu lacks kept at the end; lines whose item is
/// not on `menu` are kept as they are.
pub fn parse_edit(text: &str, lines: &[ParsedLine], menu: &[ItemRef]) -> Option<Vec<ParsedLine>> {
    let menu = Menu::new(menu);
    let words = tokenize(text);
    // (line, item) for the lines whose item is on the menu; the items are the edit's targets.
    let slots: Vec<(usize, usize)> = lines
        .iter()
        .enumerate()
        .filter_map(|(l, line)| menu.position(&line.menu_item_id).map(|it| (l, it)))
        .collect();
    let targets: Vec<usize> = slots.iter().map(|&(_, it)| it).collect();
    let mut out = lines.to_vec();
    let mut changed = vec![false; lines.len()];
    let mut named = false;
    let mut i = 0;
    while i < words.len() {
        let hit = modifier_at(&menu, &words, i, words.len(), &targets);
        let hit_len = hit.as_ref().map_or(0, |h| h.end - i);
        if item_at(&menu, &words, i).is_some_and(|m| m.end - i > hit_len) {
            return None;
        }
        match hit {
            Some(hit) => {
                for edit in &hit.edits {
                    edit_lines(&menu, &slots, &mut out, &mut changed, edit);
                }
                named = true;
                i = hit.end;
            }
            None => i += 1,
        }
    }
    if !named {
        return None;
    }
    for &(l, it) in &slots {
        if changed[l] {
            menu.items[it].sort_ids(&mut out[l].choice_ids);
        }
    }
    Some(out)
}

/// Whether `text` mentions any menu item (an Ambiguous match counts): the cheap check that decides
/// whether a group message might be an order. True exactly when [`parse_order`] is not NoItems.
pub fn mentions_item(text: &str, menu: &[ItemRef]) -> bool {
    !matches!(parse_order(text, menu), ParseOutcome::NoItems)
}

// ---------- words ----------

/// A normalized word of a text or a name.
#[derive(Debug, Clone)]
struct Word {
    text: String,
    /// `text` without a plural ending, for comparing.
    stem: String,
    /// A clause break comes right before this word.
    brk: bool,
}

impl Word {
    fn new(text: String, brk: bool) -> Self {
        Self {
            stem: stem(&text),
            text,
            brk,
        }
    }

    fn is(&self, words: &[&str]) -> bool {
        words.contains(&self.text.as_str())
    }
}

/// `text` as normalized words, each knowing whether a clause break comes before it.
fn tokenize(text: &str) -> Vec<Word> {
    let chars: Vec<char> = text.chars().map(lower).collect();
    let mut words = Vec::new();
    let mut current = String::new();
    let mut brk = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let without = c == 'w'
            && current.is_empty()
            && chars.get(i + 1) == Some(&'/')
            && chars.get(i + 2) == Some(&'o')
            && chars.get(i + 3).is_none_or(|c| !c.is_alphanumeric());
        if without {
            current.push_str("without");
            flush(&mut current, &mut brk, &mut words);
            i += 3;
            continue;
        }
        match c {
            '&' => {
                flush(&mut current, &mut brk, &mut words);
                current.push_str("and");
                flush(&mut current, &mut brk, &mut words);
            }
            '×' => current.push('x'),
            c if is_apostrophe(c) => {}
            c if c.is_alphanumeric() => current.push(fold_accent(c)),
            c => {
                flush(&mut current, &mut brk, &mut words);
                if BREAKS.contains(&c) {
                    brk = true;
                }
            }
        }
        i += 1;
    }
    flush(&mut current, &mut brk, &mut words);
    words
}

/// Ends the word being built, if any: a lone "n" is "and".
fn flush(current: &mut String, brk: &mut bool, words: &mut Vec<Word>) {
    if current.is_empty() {
        return;
    }
    let text = if current == "n" {
        current.clear();
        "and".to_owned()
    } else {
        std::mem::take(current)
    };
    words.push(Word::new(text, *brk));
    *brk = false;
}

/// `word` without a plural ending: a trailing "s" on a word longer than 3 letters, or "es" after
/// ch, sh or x ("burritos" → "burrito", "sandwiches" → "sandwich", "fries" → "frie").
fn stem(word: &str) -> String {
    if word.chars().count() <= 3 {
        return word.to_owned();
    }
    let Some(base) = word.strip_suffix('s') else {
        return word.to_owned();
    };
    match base.strip_suffix('e') {
        Some(root) if root.ends_with("ch") || root.ends_with("sh") || root.ends_with('x') => {
            root.to_owned()
        }
        _ => base.to_owned(),
    }
}

/// Whether text word `text` matches name word `name` (see the module doc).
fn word_match(text: &Word, name: &Word) -> bool {
    word_equal(text, name)
        || (text.text.chars().count() >= 4
            && !text.is(FILLER)
            && name.text.starts_with(text.text.as_str()))
}

/// [`word_match`] without prefixes: the same stem, or an alias.
fn word_equal(text: &Word, name: &Word) -> bool {
    text.stem == name.stem
        || ALIASES
            .iter()
            .any(|&(t, n)| text.stem == t && name.stem == n)
}

/// Whether `name` matches the words at `i`, all before `limit`.
fn words_at(name: &[Word], words: &[Word], i: usize, limit: usize) -> bool {
    !name.is_empty()
        && i + name.len() <= limit
        && name.iter().zip(&words[i..]).all(|(n, t)| word_match(t, n))
}

fn join(words: &[Word]) -> String {
    words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
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

// ---------- the menu ----------

/// The menu with its names as words.
struct Menu<'a> {
    items: Vec<Item<'a>>,
    /// Every word of every choice name on the menu.
    choice_words: Vec<Word>,
    /// The [`BRAND_WORDS`] that some item's name has.
    brands: Vec<&'static str>,
}

struct Item<'a> {
    item: &'a ItemRef,
    name: Vec<Word>,
    /// `name` without [`ITEM_GENERIC`] words.
    distinct: Vec<Word>,
    options: Vec<Opt<'a>>,
}

struct Opt<'a> {
    opt: &'a OptionRef,
    choices: Vec<Choice<'a>>,
}

struct Choice<'a> {
    choice: &'a ChoiceRef,
    name: Vec<Word>,
    /// A "No ..." choice ("No beans"): it matches by full name or as a negation's target only.
    negative: bool,
    /// Words that name this choice alone among the item's choices.
    keys: Vec<Word>,
}

impl<'a> Menu<'a> {
    fn new(items: &'a [ItemRef]) -> Self {
        let items: Vec<Item<'a>> = items.iter().map(Item::new).collect();
        let choice_words = items
            .iter()
            .flat_map(|it| &it.options)
            .flat_map(|o| &o.choices)
            .flat_map(|c| c.name.iter().cloned())
            .collect();
        let brands = BRAND_WORDS
            .iter()
            .copied()
            .filter(|b| items.iter().any(|it| it.name.iter().any(|w| w.text == *b)))
            .collect();
        Self {
            items,
            choice_words,
            brands,
        }
    }

    fn position(&self, item_id: &str) -> Option<usize> {
        self.items.iter().position(|it| it.item.id == item_id)
    }
}

impl<'a> Item<'a> {
    fn new(item: &'a ItemRef) -> Self {
        let name = tokenize(&item.name);
        let distinct = name
            .iter()
            .filter(|w| !w.is(ITEM_GENERIC))
            .cloned()
            .collect();
        let mut options: Vec<Opt<'a>> = item
            .options
            .iter()
            .map(|opt| Opt {
                opt,
                choices: opt
                    .choices
                    .iter()
                    .map(|choice| {
                        let name = tokenize(&choice.name);
                        let negative = name.len() > 1 && name[0].text == "no";
                        Choice {
                            choice,
                            name,
                            negative,
                            keys: Vec::new(),
                        }
                    })
                    .collect(),
            })
            .collect();
        // Key words: in this choice's name and in no other positive choice's name of the item.
        let positive: Vec<(usize, usize, &Word)> = options
            .iter()
            .enumerate()
            .flat_map(|(o, opt)| {
                opt.choices
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| !c.negative)
                    .flat_map(move |(c, choice)| choice.name.iter().map(move |w| (o, c, w)))
            })
            .collect();
        let keys: Vec<(usize, usize, Word)> = positive
            .iter()
            .filter(|(o, c, w)| {
                w.text.chars().count() >= 3
                    && !w.is(FILLER)
                    && !positive
                        .iter()
                        .any(|(o2, c2, w2)| (o2, c2) != (o, c) && w2.stem == w.stem)
            })
            .map(|&(o, c, w)| (o, c, w.clone()))
            .collect();
        for (o, c, w) in keys {
            options[o].choices[c].keys.push(w);
        }
        Self {
            item,
            name,
            distinct,
            options,
        }
    }

    /// Sorts `ids` in menu order; ids the item lacks go last, in their order.
    fn sort_ids(&self, ids: &mut [String]) {
        let rank = |id: &String| {
            self.options
                .iter()
                .flat_map(|o| &o.choices)
                .position(|c| c.choice.id == *id)
                .unwrap_or(usize::MAX)
        };
        ids.sort_by_key(rank);
    }
}

// ---------- items in the text ----------

/// Words `start..end` name `items` (several when ambiguous), in menu order.
#[derive(Debug, Clone)]
struct ItemMatch {
    start: usize,
    end: usize,
    items: Vec<usize>,
}

/// The item match that starts at word `i`, if any (see the module doc).
fn item_at(menu: &Menu, words: &[Word], i: usize) -> Option<ItemMatch> {
    // (item, matched a full name)
    let mut best: Vec<(usize, bool)> = Vec::new();
    let mut best_len = 0;
    for (it, item) in menu.items.iter().enumerate() {
        let Some((len, full)) = item_len(menu, item, words, i) else {
            continue;
        };
        if len > best_len {
            best_len = len;
            best.clear();
        }
        if len == best_len {
            best.push((it, full));
        }
    }
    if best.iter().any(|&(_, full)| full) {
        best.retain(|&(_, full)| full);
    }
    (!best.is_empty()).then(|| ItemMatch {
        start: i,
        end: i + best_len,
        items: best.into_iter().map(|(it, _)| it).collect(),
    })
}

/// How many words from `i` name `item`, and whether they are its full name word for word (a
/// prefix such as "cookie" for "Cookiewich" names it, but not as a full name that beats others).
fn item_len(menu: &Menu, item: &Item, words: &[Word], i: usize) -> Option<(usize, bool)> {
    if words_at(&item.name, words, i, words.len()) {
        let exact = item
            .name
            .iter()
            .zip(&words[i..])
            .all(|(n, t)| word_equal(t, n));
        return Some((item.name.len(), exact));
    }
    // The longest run of distinctive words, connectors allowed between two of them.
    let mut best: Option<(usize, bool)> = None;
    for a in 0..item.distinct.len() {
        let (mut k, mut d) = (i, a);
        let mut matched: Option<(usize, usize)> = None;
        while d < item.distinct.len() && k < words.len() {
            if word_match(&words[k], &item.distinct[d]) {
                k += 1;
                d += 1;
                matched = Some((k, d));
            } else if matched.is_some() && words[k].is(CONNECTORS) {
                k += 1;
            } else {
                break;
            }
        }
        if let Some((end, d_end)) = matched
            && best.is_none_or(|(len, _)| end - i > len)
        {
            best = Some((end - i, a == 0 && d_end == item.distinct.len()));
        }
    }
    let (len, complete) = best?;
    // "chicken", "beans", "guac": choice words, not an item.
    let choice_words_only = !complete
        && words[i..i + len]
            .iter()
            .filter(|w| !w.is(CONNECTORS))
            .all(|t| menu.choice_words.iter().any(|w| word_match(t, w)));
    // "sup dogs": the restaurant, not a dog.
    let branded = i > 0
        && menu.brands.contains(&words[i - 1].text.as_str())
        && !item.name.iter().any(|w| w.text == words[i - 1].text);
    (!choice_words_only && !branded).then_some((len, false))
}

/// The next item in the same clause after word `i`, with no quantity word before it.
fn next_item<'f>(
    words: &[Word],
    found: &'f [Option<ItemMatch>],
    i: usize,
) -> Option<&'f ItemMatch> {
    for (word, m) in words.iter().zip(found).skip(i + 1) {
        if word.brk || qty_of(word).is_some() {
            return None;
        }
        if m.is_some() {
            return m.as_ref();
        }
    }
    None
}

// ---------- choices in the text ----------

/// A choice of a target item: `target` indexes the targets given, `opt` and `choice` the item's
/// options and that option's choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pick {
    target: usize,
    opt: usize,
    choice: usize,
}

/// A choice added or dropped, as matched in each target that has it (in target order).
#[derive(Debug, Clone)]
struct Edit {
    remove: bool,
    picks: Vec<Pick>,
}

/// The choice edits that words from some `i` up to `end` make.
#[derive(Debug, Clone)]
struct Hit {
    end: usize,
    edits: Vec<Edit>,
}

#[derive(Debug, Clone, Copy)]
enum Mode {
    /// The full name.
    Full,
    /// A "No ..." choice's name after its "no" ("without beans").
    NoTail,
    /// A key word.
    Key,
    /// The full name or a key word.
    Any,
}

/// The choice edits that start at word `i` for `targets` (item indexes), within `..limit`.
fn modifier_at(
    menu: &Menu,
    words: &[Word],
    i: usize,
    limit: usize,
    targets: &[usize],
) -> Option<Hit> {
    if i >= limit || targets.is_empty() {
        return None;
    }
    let edit = |remove: bool, at: usize, (len, picks): (usize, Vec<Pick>)| Hit {
        end: at + len,
        edits: vec![Edit { remove, picks }],
    };
    let mut hit = if let Some(found) = best_choice(menu, words, i, limit, targets, Mode::Full) {
        edit(false, i, found)
    } else if let Some(after) = negation_end(words, i, limit) {
        let j = skip_articles(words, after, limit);
        if let Some(found) = best_choice(menu, words, j, limit, targets, Mode::NoTail) {
            edit(false, j, found)
        } else {
            edit(
                true,
                j,
                best_choice(menu, words, j, limit, targets, Mode::Any)?,
            )
        }
    } else {
        edit(
            false,
            i,
            best_choice(menu, words, i, limit, targets, Mode::Key)?,
        )
    };
    // "no onions or relish": the negation goes on.
    while hit.edits.last().is_some_and(|e| e.remove)
        && hit.end < limit
        && words[hit.end].is(&["or", "nor"])
    {
        let Some((len, picks)) = best_choice(menu, words, hit.end + 1, limit, targets, Mode::Any)
        else {
            break;
        };
        hit.edits.push(Edit {
            remove: true,
            picks,
        });
        hit.end += 1 + len;
    }
    Some(hit)
}

/// The longest choice match at word `i` among `targets`' choices in `mode`, with the targets that
/// reach that length (each by its first such choice in menu order).
fn best_choice(
    menu: &Menu,
    words: &[Word],
    i: usize,
    limit: usize,
    targets: &[usize],
    mode: Mode,
) -> Option<(usize, Vec<Pick>)> {
    if i >= limit {
        return None;
    }
    let mut best = 0;
    let mut picks = Vec::new();
    for (target, &it) in targets.iter().enumerate() {
        let mut len = 0;
        let mut pick = None;
        for (opt, option) in menu.items[it].options.iter().enumerate() {
            for (choice, c) in option.choices.iter().enumerate() {
                let l = choice_len(c, words, i, limit, mode);
                if l > len {
                    len = l;
                    pick = Some(Pick {
                        target,
                        opt,
                        choice,
                    });
                }
            }
        }
        let Some(pick) = pick else {
            continue;
        };
        if len > best {
            best = len;
            picks.clear();
        }
        if len == best {
            picks.push(pick);
        }
    }
    (best > 0).then_some((best, picks))
}

/// How many words from `i` name `choice` in `mode`; 0 for none.
fn choice_len(choice: &Choice, words: &[Word], i: usize, limit: usize, mode: Mode) -> usize {
    let full = || {
        if words_at(&choice.name, words, i, limit) {
            choice.name.len()
        } else {
            0
        }
    };
    let key = || {
        let word = &words[i];
        usize::from(!word.is(FILLER) && choice.keys.iter().any(|k| word_match(word, k)))
    };
    match mode {
        Mode::Full => full(),
        Mode::NoTail if choice.negative && words_at(&choice.name[1..], words, i, limit) => {
            choice.name.len() - 1
        }
        Mode::NoTail => 0,
        Mode::Key => key(),
        Mode::Any => full().max(key()),
    }
}

/// Where a negation phrase that starts at word `i` ends.
fn negation_end(words: &[Word], i: usize, limit: usize) -> Option<usize> {
    NEGATIONS.iter().find_map(|phrase| {
        let end = i + phrase.len();
        (end <= limit && words[i..end].iter().zip(*phrase).all(|(w, p)| w.text == *p))
            .then_some(end)
    })
}

fn skip_articles(words: &[Word], mut j: usize, limit: usize) -> usize {
    while j < limit && words[j].is(ARTICLES) {
        j += 1;
    }
    j
}

// ---------- quantities ----------

/// The quantity a word gives before an item: "2", "two", "a", "2x", "x2".
fn qty_of(word: &Word) -> Option<i64> {
    let t = word.text.as_str();
    let n = number(t)
        .or_else(|| NUMBERS.iter().find(|(w, _)| *w == t).map(|&(_, n)| n))
        .or_else(|| t.strip_prefix('x').and_then(number))
        .or_else(|| t.strip_suffix('x').and_then(number))?;
    Some(n.clamp(1, MAX_QTY))
}

/// A quantity after an item at word `i`, and where it ends: "x2", "x 2", or "2x" unless an item of
/// the same clause follows (then it is that item's).
fn times_at(words: &[Word], found: &[Option<ItemMatch>], i: usize) -> Option<(i64, usize)> {
    let t = words.get(i)?.text.as_str();
    if let Some(n) = t.strip_prefix('x').and_then(number) {
        return Some((n.clamp(1, MAX_QTY), i + 1));
    }
    if t == "x"
        && let Some(next) = words.get(i + 1)
        && !next.brk
        && let Some(n) = number(&next.text)
    {
        return Some((n.clamp(1, MAX_QTY), i + 2));
    }
    let n = t.strip_suffix('x').and_then(number)?;
    next_item(words, found, i)
        .is_none()
        .then_some((n.clamp(1, MAX_QTY), i + 1))
}

/// Up to 6 ASCII digits as a number.
fn number(t: &str) -> Option<i64> {
    if t.is_empty() || t.len() > 6 || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse().ok()
}

// ---------- reading an order ----------

/// An item read from the text, with what the text says about it.
#[derive(Debug, Clone)]
struct Draft {
    /// The candidates in menu order: one when the item is known.
    items: Vec<usize>,
    /// The words that name it.
    start: usize,
    end: usize,
    qty: i64,
    /// (option, choice) of `items[0]`.
    picked: Vec<(usize, usize)>,
}

impl Draft {
    fn apply(&mut self, menu: &Menu, hit: &Hit) {
        let &[it] = self.items.as_slice() else {
            return;
        };
        for edit in &hit.edits {
            let Some(pick) = edit.picks.first() else {
                continue;
            };
            let key = (pick.opt, pick.choice);
            if edit.remove {
                self.picked.retain(|&p| p != key);
            } else {
                let max = menu.items[it].options[pick.opt].opt.max;
                add_choice(&mut self.picked, key, max);
            }
        }
    }

    fn line(&self, menu: &Menu) -> ParsedLine {
        let item = &menu.items[self.items[0]];
        let mut picked = self.picked.clone();
        picked.sort_unstable();
        ParsedLine {
            menu_item_id: item.item.id.clone(),
            qty: self.qty,
            choice_ids: picked
                .iter()
                .map(|&(o, c)| item.options[o].choices[c].choice.id.clone())
                .collect(),
        }
    }

    /// The first required option with fewer choices than its min.
    fn missing(&self, menu: &Menu) -> Option<String> {
        let item = &menu.items[self.items[0]];
        item.options
            .iter()
            .enumerate()
            .find(|(o, option)| {
                let n = self.picked.iter().filter(|p| p.0 == *o).count();
                i64::try_from(n).unwrap_or(i64::MAX) < i64::from(option.opt.min)
            })
            .map(|(_, option)| option.opt.id.clone())
    }
}

/// Adds (option, choice) `key`: an option with max 1 (or less) keeps only it, others take it
/// while they have room.
fn add_choice(picked: &mut Vec<(usize, usize)>, key: (usize, usize), max: i32) {
    if picked.contains(&key) {
        return;
    }
    if max <= 1 {
        picked.retain(|p| p.0 != key.0);
    } else if picked.iter().filter(|p| p.0 == key.0).count() >= usize::try_from(max).unwrap_or(0) {
        return;
    }
    picked.push(key);
}

/// The items of `words` in order, with their quantities and choices.
fn read_order(menu: &Menu, words: &[Word]) -> Vec<Draft> {
    let found: Vec<Option<ItemMatch>> = (0..words.len()).map(|i| item_at(menu, words, i)).collect();
    let mut drafts: Vec<Draft> = Vec::new();
    // The first word no item has taken: the next item's leading choices and quantity start here.
    let mut free = 0;
    let mut i = 0;
    while i < words.len() {
        let hit = drafts
            .last()
            .and_then(|d| modifier_at(menu, words, i, words.len(), &d.items));
        let hit_len = hit.as_ref().map_or(0, |h| h.end - i);
        if let Some(m) = &found[i]
            && m.end - i > hit_len
        {
            i = start_item(menu, words, &found, m, free, &mut drafts);
            free = i;
            continue;
        }
        if let Some(draft) = drafts.last_mut()
            && let Some((qty, end)) = times_at(words, &found, i)
        {
            draft.qty = qty;
            i = end;
            free = i;
            continue;
        }
        // "fries large, vanilla shake": a choice of the next item, not of this one.
        if let Some(next) = next_item(words, &found, i) {
            let broke = words[free..=i].iter().any(|w| w.brk);
            if (hit.is_none() || broke)
                && modifier_at(menu, words, i, next.start, &next.items).is_some()
            {
                i = next.start;
                continue;
            }
        }
        if let Some(hit) = hit {
            if let Some(draft) = drafts.last_mut() {
                draft.apply(menu, &hit);
            }
            i = hit.end;
            free = i;
            continue;
        }
        i += 1;
    }
    drafts
}

/// Starts the item `m` names, with its leading choices and quantity from the words `free..`, and
/// returns where the words after it start.
fn start_item(
    menu: &Menu,
    words: &[Word],
    found: &[Option<ItemMatch>],
    m: &ItemMatch,
    free: usize,
    drafts: &mut Vec<Draft>,
) -> usize {
    let mut draft = Draft {
        items: m.items.clone(),
        start: m.start,
        end: m.end,
        qty: 1,
        picked: Vec::new(),
    };
    // Leading choices: "vanilla shake", "large fries", "chicken quesadilla".
    let mut taken = vec![false; m.start - free];
    let mut k = free;
    while k < m.start {
        match modifier_at(menu, words, k, m.start, &draft.items) {
            Some(hit) => {
                draft.apply(menu, &hit);
                taken[k - free..hit.end - free].fill(true);
                k = hit.end;
            }
            None => k += 1,
        }
    }
    // The quantity right before the item and its leading choices.
    let mut q = m.start;
    while q > free && taken[q - 1 - free] {
        q -= 1;
    }
    if q > free
        && let Some(n) = qty_of(&words[q - 1])
    {
        draft.qty = n;
    }
    let mut end = m.end;
    if let Some((n, after)) = times_at(words, found, end) {
        draft.qty = n;
        end = after;
    }
    drafts.push(draft);
    end
}

// ---------- editing lines ----------

/// Applies `edit` to `out` (see [`parse_edit`]); `slots` are (line, item) per target.
fn edit_lines(
    menu: &Menu,
    slots: &[(usize, usize)],
    out: &mut [ParsedLine],
    changed: &mut [bool],
    edit: &Edit,
) {
    let option_count = |ids: &[String], option: &Opt| {
        ids.iter()
            .filter(|id| option.choices.iter().any(|c| c.choice.id == **id))
            .count()
    };
    if edit.remove {
        // The first line that has the choice.
        let Some((line, option, id)) = edit.picks.iter().find_map(|p| {
            let (line, it) = slots[p.target];
            let option = &menu.items[it].options[p.opt];
            let id = &option.choices[p.choice].choice.id;
            out[line]
                .choice_ids
                .contains(id)
                .then_some((line, option, id))
        }) else {
            return;
        };
        let left = option_count(&out[line].choice_ids, option) - 1;
        if i64::try_from(left).unwrap_or(i64::MAX) < i64::from(option.opt.min) {
            return;
        }
        out[line].choice_ids.retain(|x| x != id);
        changed[line] = true;
        return;
    }
    // The first line whose item has the choice.
    let Some(pick) = edit.picks.first() else {
        return;
    };
    let (line, it) = slots[pick.target];
    let option = &menu.items[it].options[pick.opt];
    let id = &option.choices[pick.choice].choice.id;
    let ids = &mut out[line].choice_ids;
    if ids.contains(id) {
        return;
    }
    if option.opt.max <= 1 {
        ids.retain(|x| !option.choices.iter().any(|c| c.choice.id == *x));
    } else if option_count(ids, option) >= usize::try_from(option.opt.max).unwrap_or(0) {
        return;
    }
    ids.push(id.clone());
    changed[line] = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    type Opt<'a> = (&'a str, &'a str, i32, i32, &'a [(&'a str, &'a str)]);

    /// (text, lines drafted, what parse_edit returns)
    type EditCase<'a> = (&'a str, Vec<ParsedLine>, Option<Vec<ParsedLine>>);

    fn item(id: &str, name: &str, options: &[Opt]) -> ItemRef {
        ItemRef {
            id: id.to_owned(),
            name: name.to_owned(),
            options: options
                .iter()
                .map(|&(id, name, min, max, choices)| OptionRef {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    min,
                    max,
                    choices: choices
                        .iter()
                        .map(|&(id, name)| ChoiceRef {
                            id: id.to_owned(),
                            name: name.to_owned(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    /// Cosmic Cantina, from contract/fixtures/menus.json.
    fn cosmic() -> Vec<ItemRef> {
        let burrito = |key: &str, name: &str, extras: &[(&str, &str)]| {
            let id = |s: &str| format!("{s}_cosmic_{key}_burrito");
            let cho = |s: &str| format!("cho_cosmic_{key}_burrito_{s}");
            let beans = [
                (cho("black_beans"), "Black beans"),
                (cho("pinto_beans"), "Pinto beans"),
                (cho("no_beans"), "No beans"),
            ];
            let beans: Vec<(&str, &str)> = beans
                .iter()
                .take(if extras.is_empty() { 2 } else { 3 })
                .map(|(id, name)| (id.as_str(), *name))
                .collect();
            let extras: Vec<(String, &str)> =
                extras.iter().map(|&(s, name)| (cho(s), name)).collect();
            let extras: Vec<(&str, &str)> = extras
                .iter()
                .map(|(id, name)| (id.as_str(), *name))
                .collect();
            let beans_id = format!("opt_cosmic_{key}_burrito_beans");
            let extras_id = format!("opt_cosmic_{key}_burrito_extras");
            let mut options: Vec<Opt> = vec![(&beans_id, "Beans", 1, 1, &beans)];
            if !extras.is_empty() {
                options.push((&extras_id, "Extras", 0, 3, &extras));
            }
            item(&id("itm"), name, &options)
        };
        vec![
            burrito(
                "chicken",
                "Chicken Burrito",
                &[
                    ("guac", "Guacamole"),
                    ("sour_cream", "Sour cream"),
                    ("extra_cheese", "Extra cheese"),
                    ("jalapenos", "Jalapenos"),
                ],
            ),
            burrito(
                "steak",
                "Steak Burrito",
                &[
                    ("guac", "Guacamole"),
                    ("sour_cream", "Sour cream"),
                    ("extra_cheese", "Extra cheese"),
                ],
            ),
            burrito("bean_cheese", "Bean & Cheese Burrito", &[]),
            item(
                "itm_cosmic_quesadilla",
                "Quesadilla",
                &[(
                    "opt_cosmic_quesadilla_filling",
                    "Filling",
                    1,
                    1,
                    &[
                        ("cho_cosmic_quesadilla_cheese_only", "Cheese only"),
                        ("cho_cosmic_quesadilla_veggie", "Grilled veggies"),
                        ("cho_cosmic_quesadilla_chicken", "Chicken"),
                        ("cho_cosmic_quesadilla_steak", "Steak"),
                    ],
                )],
            ),
            item(
                "itm_cosmic_nachos",
                "Loaded Nachos",
                &[
                    (
                        "opt_cosmic_nachos_protein",
                        "Add protein",
                        0,
                        1,
                        &[
                            ("cho_cosmic_nachos_chicken", "Chicken"),
                            ("cho_cosmic_nachos_steak", "Steak"),
                        ],
                    ),
                    (
                        "opt_cosmic_nachos_extras",
                        "Extras",
                        0,
                        2,
                        &[
                            ("cho_cosmic_nachos_guac", "Guacamole"),
                            ("cho_cosmic_nachos_extra_queso", "Extra queso"),
                        ],
                    ),
                ],
            ),
            item("itm_cosmic_chips_salsa", "Chips & Salsa", &[]),
            item("itm_cosmic_chips_guac", "Chips & Guacamole", &[]),
        ]
    }

    /// Sup Dogs, from contract/fixtures/menus.json.
    fn supdogs() -> Vec<ItemRef> {
        vec![
            item(
                "itm_supdogs_classic_dog",
                "Classic Dog",
                &[(
                    "opt_supdogs_classic_dog_toppings",
                    "Toppings",
                    0,
                    5,
                    &[
                        ("cho_supdogs_classic_dog_chili", "Chili"),
                        ("cho_supdogs_classic_dog_cheese", "Cheese"),
                        ("cho_supdogs_classic_dog_slaw", "Slaw"),
                        ("cho_supdogs_classic_dog_onions", "Onions"),
                        ("cho_supdogs_classic_dog_relish", "Relish"),
                    ],
                )],
            ),
            item("itm_supdogs_veggie_dog", "Veggie Dog", &[]),
            item(
                "itm_supdogs_sup_burger",
                "Sup Burger",
                &[
                    (
                        "opt_supdogs_sup_burger_cheese",
                        "Cheese",
                        1,
                        1,
                        &[
                            ("cho_supdogs_sup_burger_american", "American"),
                            ("cho_supdogs_sup_burger_cheddar", "Cheddar"),
                            ("cho_supdogs_sup_burger_pepper_jack", "Pepper jack"),
                            ("cho_supdogs_sup_burger_no_cheese", "No cheese"),
                        ],
                    ),
                    (
                        "opt_supdogs_sup_burger_addons",
                        "Add-ons",
                        0,
                        3,
                        &[
                            ("cho_supdogs_sup_burger_bacon", "Bacon"),
                            ("cho_supdogs_sup_burger_fried_egg", "Fried egg"),
                            ("cho_supdogs_sup_burger_extra_patty", "Extra patty"),
                        ],
                    ),
                ],
            ),
            item(
                "itm_supdogs_tots",
                "Tots",
                &[(
                    "opt_supdogs_tots_toppings",
                    "Toppings",
                    0,
                    2,
                    &[
                        ("cho_supdogs_tots_cheese", "Cheese sauce"),
                        ("cho_supdogs_tots_chili", "Chili"),
                    ],
                )],
            ),
            item(
                "itm_supdogs_fries",
                "Fries",
                &[(
                    "opt_supdogs_fries_size",
                    "Size",
                    1,
                    1,
                    &[
                        ("cho_supdogs_fries_regular", "Regular"),
                        ("cho_supdogs_fries_large", "Large"),
                    ],
                )],
            ),
            item(
                "itm_supdogs_shake",
                "Milkshake",
                &[(
                    "opt_supdogs_shake_flavor",
                    "Flavor",
                    1,
                    1,
                    &[
                        ("cho_supdogs_shake_vanilla", "Vanilla"),
                        ("cho_supdogs_shake_chocolate", "Chocolate"),
                        ("cho_supdogs_shake_strawberry", "Strawberry"),
                        ("cho_supdogs_shake_cookies_cream", "Cookies & cream"),
                    ],
                )],
            ),
        ]
    }

    /// A line of item `itm_<item>` with choices `cho_<choice>`.
    fn l(item: &str, qty: i64, choices: &[&str]) -> ParsedLine {
        ParsedLine {
            menu_item_id: format!("itm_{item}"),
            qty,
            choice_ids: choices.iter().map(|c| format!("cho_{c}")).collect(),
        }
    }

    fn lines(lines: &[ParsedLine]) -> ParseOutcome {
        ParseOutcome::Lines(lines.to_vec())
    }

    fn ambiguous(words: &str, items: &[&str]) -> ParseOutcome {
        ParseOutcome::Ambiguous {
            words: words.to_owned(),
            item_ids: items.iter().map(|i| format!("itm_{i}")).collect(),
        }
    }

    fn missing(line: ParsedLine, option: &str) -> ParseOutcome {
        ParseOutcome::MissingChoice {
            line,
            option_id: format!("opt_{option}"),
        }
    }

    const BURRITOS: [&str; 3] = [
        "cosmic_chicken_burrito",
        "cosmic_steak_burrito",
        "cosmic_bean_cheese_burrito",
    ];

    #[test]
    fn parses_cosmic_orders() {
        let menu = cosmic();
        let cases: Vec<(&str, ParseOutcome)> = vec![
            (
                "chicken burrito no beans",
                lines(&[l(
                    "cosmic_chicken_burrito",
                    1,
                    &["cosmic_chicken_burrito_no_beans"],
                )]),
            ),
            (
                "Chicken Burrito, NO BEANS!!",
                lines(&[l(
                    "cosmic_chicken_burrito",
                    1,
                    &["cosmic_chicken_burrito_no_beans"],
                )]),
            ),
            (
                "2 chicken burritos with black beans and guac",
                lines(&[l(
                    "cosmic_chicken_burrito",
                    2,
                    &[
                        "cosmic_chicken_burrito_black_beans",
                        "cosmic_chicken_burrito_guac",
                    ],
                )]),
            ),
            (
                "a quesadilla with steak and chips and salsa",
                lines(&[
                    l("cosmic_quesadilla", 1, &["cosmic_quesadilla_steak"]),
                    l("cosmic_chips_salsa", 1, &[]),
                ]),
            ),
            (
                "loaded nachos w chicken + guac",
                lines(&[l(
                    "cosmic_nachos",
                    1,
                    &["cosmic_nachos_chicken", "cosmic_nachos_guac"],
                )]),
            ),
            (
                "steak burrito pinto beans no sour cream",
                lines(&[l(
                    "cosmic_steak_burrito",
                    1,
                    &["cosmic_steak_burrito_pinto_beans"],
                )]),
            ),
            ("chips and guac", lines(&[l("cosmic_chips_guac", 1, &[])])),
            (
                "chips & guacamole",
                lines(&[l("cosmic_chips_guac", 1, &[])]),
            ),
            ("chips n guac", lines(&[l("cosmic_chips_guac", 1, &[])])),
            ("chips with guac", lines(&[l("cosmic_chips_guac", 1, &[])])),
            ("chips and salsa", lines(&[l("cosmic_chips_salsa", 1, &[])])),
            ("nachos", lines(&[l("cosmic_nachos", 1, &[])])),
            (
                "bean and cheese burrito, pinto",
                lines(&[l(
                    "cosmic_bean_cheese_burrito",
                    1,
                    &["cosmic_bean_cheese_burrito_pinto_beans"],
                )]),
            ),
            (
                "bean & cheese burrito black beans",
                lines(&[l(
                    "cosmic_bean_cheese_burrito",
                    1,
                    &["cosmic_bean_cheese_burrito_black_beans"],
                )]),
            ),
            // Choices before the item.
            (
                "chicken quesadilla",
                lines(&[l("cosmic_quesadilla", 1, &["cosmic_quesadilla_chicken"])]),
            ),
            (
                "two steak quesadillas",
                lines(&[l("cosmic_quesadilla", 2, &["cosmic_quesadilla_steak"])]),
            ),
            (
                "quesa with grilled veggies",
                lines(&[l("cosmic_quesadilla", 1, &["cosmic_quesadilla_veggie"])]),
            ),
            (
                "cheese quesadilla",
                lines(&[l(
                    "cosmic_quesadilla",
                    1,
                    &["cosmic_quesadilla_cheese_only"],
                )]),
            ),
            (
                "chicken burrito no beans, chicken quesadilla",
                lines(&[
                    l(
                        "cosmic_chicken_burrito",
                        1,
                        &["cosmic_chicken_burrito_no_beans"],
                    ),
                    l("cosmic_quesadilla", 1, &["cosmic_quesadilla_chicken"]),
                ]),
            ),
            // Accents, prefixes, "w/" and "w/o".
            (
                "chicken burrito w/ black beans & jalapeños",
                lines(&[l(
                    "cosmic_chicken_burrito",
                    1,
                    &[
                        "cosmic_chicken_burrito_black_beans",
                        "cosmic_chicken_burrito_jalapenos",
                    ],
                )]),
            ),
            (
                "chicken burrito w/o beans",
                lines(&[l(
                    "cosmic_chicken_burrito",
                    1,
                    &["cosmic_chicken_burrito_no_beans"],
                )]),
            ),
            (
                "chicken burrito without beans, extra jalap",
                lines(&[l(
                    "cosmic_chicken_burrito",
                    1,
                    &[
                        "cosmic_chicken_burrito_no_beans",
                        "cosmic_chicken_burrito_jalapenos",
                    ],
                )]),
            ),
            // Max 1: the latest choice wins.
            (
                "chicken burrito black beans actually pinto",
                lines(&[l(
                    "cosmic_chicken_burrito",
                    1,
                    &["cosmic_chicken_burrito_pinto_beans"],
                )]),
            ),
            (
                "steak burrito black beans instead of pinto",
                lines(&[l(
                    "cosmic_steak_burrito",
                    1,
                    &["cosmic_steak_burrito_black_beans"],
                )]),
            ),
            // Max 3: the first three.
            (
                "chicken burrito black beans guac sour cream extra cheese jalapenos",
                lines(&[l(
                    "cosmic_chicken_burrito",
                    1,
                    &[
                        "cosmic_chicken_burrito_black_beans",
                        "cosmic_chicken_burrito_guac",
                        "cosmic_chicken_burrito_sour_cream",
                        "cosmic_chicken_burrito_extra_cheese",
                    ],
                )]),
            ),
            // "with cheese" is Extra cheese on a burrito.
            (
                "steak burrito no beans with cheese",
                lines(&[l(
                    "cosmic_steak_burrito",
                    1,
                    &[
                        "cosmic_steak_burrito_no_beans",
                        "cosmic_steak_burrito_extra_cheese",
                    ],
                )]),
            ),
            // Negations drop the choice, through "or".
            (
                "chicken burrito pinto, guac and sour cream, hold the guac",
                lines(&[l(
                    "cosmic_chicken_burrito",
                    1,
                    &[
                        "cosmic_chicken_burrito_pinto_beans",
                        "cosmic_chicken_burrito_sour_cream",
                    ],
                )]),
            ),
            (
                "chicken burrito black beans no sour cream or guac",
                lines(&[l(
                    "cosmic_chicken_burrito",
                    1,
                    &["cosmic_chicken_burrito_black_beans"],
                )]),
            ),
            // Several items, each with its own choices.
            (
                "chicken burrito no beans and steak burrito black beans with guac",
                lines(&[
                    l(
                        "cosmic_chicken_burrito",
                        1,
                        &["cosmic_chicken_burrito_no_beans"],
                    ),
                    l(
                        "cosmic_steak_burrito",
                        1,
                        &[
                            "cosmic_steak_burrito_black_beans",
                            "cosmic_steak_burrito_guac",
                        ],
                    ),
                ]),
            ),
            (
                "nachos with steak and extra queso, 2 chips & salsa",
                lines(&[
                    l(
                        "cosmic_nachos",
                        1,
                        &["cosmic_nachos_steak", "cosmic_nachos_extra_queso"],
                    ),
                    l("cosmic_chips_salsa", 2, &[]),
                ]),
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(parse_order(text, &menu), expected, "{text:?}");
        }
    }

    #[test]
    fn parses_supdogs_orders() {
        let menu = supdogs();
        let cases: Vec<(&str, ParseOutcome)> = vec![
            (
                "sup burger cheddar with bacon, fries large, vanilla shake",
                lines(&[
                    l(
                        "supdogs_sup_burger",
                        1,
                        &["supdogs_sup_burger_cheddar", "supdogs_sup_burger_bacon"],
                    ),
                    l("supdogs_fries", 1, &["supdogs_fries_large"]),
                    l("supdogs_shake", 1, &["supdogs_shake_vanilla"]),
                ]),
            ),
            (
                "large fries",
                lines(&[l("supdogs_fries", 1, &["supdogs_fries_large"])]),
            ),
            (
                "2 large fries",
                lines(&[l("supdogs_fries", 2, &["supdogs_fries_large"])]),
            ),
            (
                "a cookies and cream shake",
                lines(&[l("supdogs_shake", 1, &["supdogs_shake_cookies_cream"])]),
            ),
            (
                "chocolate milkshakes x2",
                lines(&[l("supdogs_shake", 2, &["supdogs_shake_chocolate"])]),
            ),
            ("veggie dog", lines(&[l("supdogs_veggie_dog", 1, &[])])),
            (
                "classic dog with chili, slaw and onions",
                lines(&[l(
                    "supdogs_classic_dog",
                    1,
                    &[
                        "supdogs_classic_dog_chili",
                        "supdogs_classic_dog_slaw",
                        "supdogs_classic_dog_onions",
                    ],
                )]),
            ),
            (
                "tots with cheese",
                lines(&[l("supdogs_tots", 1, &["supdogs_tots_cheese"])]),
            ),
            (
                "classic dog with cheese, chili tots",
                lines(&[
                    l("supdogs_classic_dog", 1, &["supdogs_classic_dog_cheese"]),
                    l("supdogs_tots", 1, &["supdogs_tots_chili"]),
                ]),
            ),
            (
                "sup burger no cheese",
                lines(&[l(
                    "supdogs_sup_burger",
                    1,
                    &["supdogs_sup_burger_no_cheese"],
                )]),
            ),
            (
                "burger without cheese",
                lines(&[l(
                    "supdogs_sup_burger",
                    1,
                    &["supdogs_sup_burger_no_cheese"],
                )]),
            ),
            (
                "burger pepper jack, fried egg and extra patty",
                lines(&[l(
                    "supdogs_sup_burger",
                    1,
                    &[
                        "supdogs_sup_burger_pepper_jack",
                        "supdogs_sup_burger_fried_egg",
                        "supdogs_sup_burger_extra_patty",
                    ],
                )]),
            ),
            (
                "burger, jack",
                lines(&[l(
                    "supdogs_sup_burger",
                    1,
                    &["supdogs_sup_burger_pepper_jack"],
                )]),
            ),
            // "cheese" is a word of "No cheese" only: it never selects it.
            (
                "sup burger with cheese",
                missing(l("supdogs_sup_burger", 1, &[]), "supdogs_sup_burger_cheese"),
            ),
            (
                "fries",
                missing(l("supdogs_fries", 1, &[]), "supdogs_fries_size"),
            ),
            (
                "dog",
                ambiguous("dog", &["supdogs_classic_dog", "supdogs_veggie_dog"]),
            ),
            (
                "2 dogs",
                ambiguous("dogs", &["supdogs_classic_dog", "supdogs_veggie_dog"]),
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(parse_order(text, &menu), expected, "{text:?}");
        }
    }

    #[test]
    fn quantities() {
        let menu = cosmic();
        let qty = |text: &str| match parse_order(text, &menu) {
            ParseOutcome::Lines(lines) => lines.iter().map(|l| l.qty).collect::<Vec<_>>(),
            other => panic!("{text:?}: {other:?}"),
        };
        let cases: &[(&str, &[i64])] = &[
            ("nachos", &[1]),
            ("an order of nachos", &[1]),
            ("one nachos", &[1]),
            ("3 nachos", &[3]),
            ("three nachos", &[3]),
            ("ten nachos", &[10]),
            ("a dozen nachos", &[12]),
            ("nachos x2", &[2]),
            ("nachos x 4", &[4]),
            ("nachos 2x", &[2]),
            ("2x nachos", &[2]),
            ("chicken burrito x2 no beans", &[2]),
            ("chicken burrito no beans x3", &[3]),
            ("chicken burrito 2x, no beans", &[2]),
            ("2x steak burrito black beans", &[2]),
            ("100 chicken burritos no beans", &[20]),
            ("0 nachos", &[1]),
            ("nachos 2x chips and salsa", &[1, 2]),
            ("2 nachos and 3 chips and salsa", &[2, 3]),
            ("nachos x2, chips and salsa x3", &[2, 3]),
        ];
        for (text, expected) in cases {
            assert_eq!(qty(text), *expected, "{text:?}");
        }
    }

    #[test]
    fn asks_about_problems() {
        let menu = cosmic();
        let cases: Vec<(&str, ParseOutcome)> = vec![
            (
                "chicken burrito",
                missing(
                    l("cosmic_chicken_burrito", 1, &[]),
                    "cosmic_chicken_burrito_beans",
                ),
            ),
            (
                "2 chicken burritos with guac",
                missing(
                    l(
                        "cosmic_chicken_burrito",
                        2,
                        &["cosmic_chicken_burrito_guac"],
                    ),
                    "cosmic_chicken_burrito_beans",
                ),
            ),
            // "beans" is not a choice of its own: it is asked about, not read as an item.
            (
                "chicken burrito with beans and cheese",
                missing(
                    l(
                        "cosmic_chicken_burrito",
                        1,
                        &["cosmic_chicken_burrito_extra_cheese"],
                    ),
                    "cosmic_chicken_burrito_beans",
                ),
            ),
            (
                "quesadilla",
                missing(l("cosmic_quesadilla", 1, &[]), "cosmic_quesadilla_filling"),
            ),
            // A full item name after an item is a new item, not the choice it starts with.
            (
                "quesadilla with chicken burrito",
                missing(l("cosmic_quesadilla", 1, &[]), "cosmic_quesadilla_filling"),
            ),
            (
                "chicken burrito black beans and a steak burrito",
                missing(
                    l("cosmic_steak_burrito", 1, &[]),
                    "cosmic_steak_burrito_beans",
                ),
            ),
            ("burrito no beans", ambiguous("burrito", &BURRITOS)),
            ("2 burritos", ambiguous("burritos", &BURRITOS)),
            (
                "chips",
                ambiguous("chips", &["cosmic_chips_salsa", "cosmic_chips_guac"]),
            ),
            // Ambiguous comes before a missing choice, wherever it is.
            (
                "chicken burrito and a burrito",
                ambiguous("burrito", &BURRITOS),
            ),
            ("nachos and burritos", ambiguous("burritos", &BURRITOS)),
        ];
        for (text, expected) in cases {
            assert_eq!(parse_order(text, &menu), expected, "{text:?}");
        }
    }

    #[test]
    fn prefixes_do_not_beat_other_names() {
        let menu = vec![
            item("itm_classic_cookie", "Classic Cookie", &[]),
            item("itm_cookiewich", "Cookiewich", &[]),
            item("itm_quesadilla", "Quesadilla", &[]),
        ];
        let cases = [
            (
                "a cookie",
                ambiguous("cookie", &["classic_cookie", "cookiewich"]),
            ),
            ("cookiewich", lines(&[l("cookiewich", 1, &[])])),
            ("2 cookiewiches", lines(&[l("cookiewich", 2, &[])])),
            ("quesa", lines(&[l("quesadilla", 1, &[])])),
        ];
        for (text, expected) in cases {
            assert_eq!(parse_order(text, &menu), expected, "{text:?}");
        }
    }

    #[test]
    fn finds_no_items() {
        let cases: &[(&str, Vec<ItemRef>)] = &[
            ("lol same", cosmic()),
            ("how long until my food gets here?", cosmic()),
            ("heading to cosmic at 6", cosmic()),
            (
                "heading to Cosmic Cantina at 6, anyone want anything?",
                cosmic(),
            ),
            ("", cosmic()),
            ("!!", cosmic()),
            ("thanks", cosmic()),
            // Choice words alone are not items.
            ("add guac", cosmic()),
            ("with sour cream", cosmic()),
            ("no cheese pls", cosmic()),
            ("i love cheese", cosmic()),
            ("guac is life", cosmic()),
            ("chicken", cosmic()),
            ("make it pinto beans", cosmic()),
            // The restaurant's name is not an item.
            ("heading to sup dogs at 6", supdogs()),
            ("omw to Sup Dogs, anyone want anything?", supdogs()),
            ("chicken burrito", supdogs()),
            ("chicken burrito", Vec::new()),
        ];
        for (text, menu) in cases {
            assert_eq!(parse_order(text, menu), ParseOutcome::NoItems, "{text:?}");
        }
    }

    #[test]
    fn mentions_items() {
        let cases: &[(&str, Vec<ItemRef>, bool)] = &[
            ("chicken burrito no beans", cosmic(), true),
            ("can i get 2 chicken burritos", cosmic(), true),
            ("burrito pls", cosmic(), true),
            ("chips", cosmic(), true),
            ("nachos?", cosmic(), true),
            ("quesadilla", cosmic(), true),
            ("a vanilla shake", supdogs(), true),
            ("dog", supdogs(), true),
            ("tots", supdogs(), true),
            ("lol same", cosmic(), false),
            ("heading to cosmic at 6", cosmic(), false),
            ("heading to sup dogs at 6", supdogs(), false),
            ("add guac", cosmic(), false),
            ("no sour cream", cosmic(), false),
            ("how long until my food gets here?", cosmic(), false),
            ("nachos", supdogs(), false),
        ];
        for (text, menu, expected) in cases {
            assert_eq!(mentions_item(text, menu), *expected, "{text:?}");
        }
    }

    #[test]
    fn edits_lines() {
        let menu = cosmic();
        let no_beans = l(
            "cosmic_chicken_burrito",
            1,
            &["cosmic_chicken_burrito_no_beans"],
        );
        let with_guac = l(
            "cosmic_chicken_burrito",
            1,
            &[
                "cosmic_chicken_burrito_no_beans",
                "cosmic_chicken_burrito_guac",
            ],
        );
        let black = l(
            "cosmic_chicken_burrito",
            1,
            &["cosmic_chicken_burrito_black_beans"],
        );
        let full_extras = l(
            "cosmic_chicken_burrito",
            2,
            &[
                "cosmic_chicken_burrito_black_beans",
                "cosmic_chicken_burrito_guac",
                "cosmic_chicken_burrito_sour_cream",
                "cosmic_chicken_burrito_extra_cheese",
            ],
        );
        let steak = l(
            "cosmic_steak_burrito",
            1,
            &["cosmic_steak_burrito_pinto_beans"],
        );
        let quesadilla = l("cosmic_quesadilla", 1, &["cosmic_quesadilla_chicken"]);
        let cases: Vec<EditCase> = vec![
            (
                "add guac",
                vec![no_beans.clone()],
                Some(vec![with_guac.clone()]),
            ),
            (
                "with guacamole pls",
                vec![no_beans.clone()],
                Some(vec![with_guac.clone()]),
            ),
            (
                "make it pinto beans",
                vec![no_beans.clone()],
                Some(vec![l(
                    "cosmic_chicken_burrito",
                    1,
                    &["cosmic_chicken_burrito_pinto_beans"],
                )]),
            ),
            (
                "black beans instead",
                vec![with_guac.clone()],
                Some(vec![l(
                    "cosmic_chicken_burrito",
                    1,
                    &[
                        "cosmic_chicken_burrito_black_beans",
                        "cosmic_chicken_burrito_guac",
                    ],
                )]),
            ),
            (
                "no guac",
                vec![with_guac.clone()],
                Some(vec![no_beans.clone()]),
            ),
            (
                "remove the guac",
                vec![with_guac.clone()],
                Some(vec![no_beans.clone()]),
            ),
            (
                "actually without guac",
                vec![with_guac.clone()],
                Some(vec![no_beans.clone()]),
            ),
            (
                "with sour cream, extra cheese too",
                vec![no_beans.clone()],
                Some(vec![l(
                    "cosmic_chicken_burrito",
                    1,
                    &[
                        "cosmic_chicken_burrito_no_beans",
                        "cosmic_chicken_burrito_sour_cream",
                        "cosmic_chicken_burrito_extra_cheese",
                    ],
                )]),
            ),
            (
                "no sour cream or extra cheese",
                vec![full_extras.clone()],
                Some(vec![l(
                    "cosmic_chicken_burrito",
                    2,
                    &[
                        "cosmic_chicken_burrito_black_beans",
                        "cosmic_chicken_burrito_guac",
                    ],
                )]),
            ),
            // "no beans" is itself a choice: it replaces.
            (
                "no beans",
                vec![black.clone()],
                Some(vec![no_beans.clone()]),
            ),
            (
                "without beans",
                vec![black.clone()],
                Some(vec![no_beans.clone()]),
            ),
            // A required option's only choice stays; a full option takes no more.
            (
                "remove the black beans",
                vec![black.clone()],
                Some(vec![black.clone()]),
            ),
            (
                "add jalapenos",
                vec![full_extras.clone()],
                Some(vec![full_extras.clone()]),
            ),
            // Already there, or not there to drop: still an edit.
            (
                "add guac",
                vec![with_guac.clone()],
                Some(vec![with_guac.clone()]),
            ),
            (
                "no sour cream",
                vec![no_beans.clone()],
                Some(vec![no_beans.clone()]),
            ),
            // The first line whose item has the choice.
            (
                "add guac",
                vec![quesadilla.clone(), steak.clone(), no_beans.clone()],
                Some(vec![
                    quesadilla.clone(),
                    l(
                        "cosmic_steak_burrito",
                        1,
                        &[
                            "cosmic_steak_burrito_pinto_beans",
                            "cosmic_steak_burrito_guac",
                        ],
                    ),
                    no_beans.clone(),
                ]),
            ),
            (
                "jalapenos",
                vec![steak.clone(), no_beans.clone()],
                Some(vec![
                    steak.clone(),
                    l(
                        "cosmic_chicken_burrito",
                        1,
                        &[
                            "cosmic_chicken_burrito_no_beans",
                            "cosmic_chicken_burrito_jalapenos",
                        ],
                    ),
                ]),
            ),
            (
                "make it steak",
                vec![quesadilla.clone()],
                Some(vec![l(
                    "cosmic_quesadilla",
                    1,
                    &["cosmic_quesadilla_steak"],
                )]),
            ),
            // A menu item is a new order; no choice of these lines is no edit.
            ("steak burrito", vec![no_beans.clone()], None),
            ("add a steak burrito", vec![no_beans.clone()], None),
            ("add chips", vec![no_beans.clone()], None),
            ("and nachos with guac", vec![no_beans.clone()], None),
            ("thanks", vec![no_beans.clone()], None),
            ("", vec![no_beans.clone()], None),
            ("add steak", vec![no_beans.clone()], None),
            ("extra queso", vec![no_beans.clone()], None),
            ("add guac", Vec::new(), None),
        ];
        for (text, current, expected) in cases {
            assert_eq!(parse_edit(text, &current, &menu), expected, "{text:?}");
        }
    }

    #[test]
    fn edits_keep_unknown_lines_and_ids() {
        let menu = cosmic();
        let unknown = ParsedLine {
            menu_item_id: "itm_gone".to_owned(),
            qty: 1,
            choice_ids: vec!["cho_gone".to_owned()],
        };
        let odd = ParsedLine {
            menu_item_id: "itm_cosmic_chicken_burrito".to_owned(),
            qty: 1,
            choice_ids: vec![
                "cho_gone".to_owned(),
                "cho_cosmic_chicken_burrito_guac".to_owned(),
                "cho_cosmic_chicken_burrito_black_beans".to_owned(),
            ],
        };
        let edited = parse_edit("add sour cream", &[unknown.clone(), odd], &menu);
        assert_eq!(
            edited,
            Some(vec![
                unknown,
                ParsedLine {
                    menu_item_id: "itm_cosmic_chicken_burrito".to_owned(),
                    qty: 1,
                    choice_ids: vec![
                        "cho_cosmic_chicken_burrito_black_beans".to_owned(),
                        "cho_cosmic_chicken_burrito_guac".to_owned(),
                        "cho_cosmic_chicken_burrito_sour_cream".to_owned(),
                        "cho_gone".to_owned(),
                    ],
                },
            ])
        );
    }

    #[test]
    fn normalizes_words() {
        let words = |text: &str| {
            tokenize(text)
                .iter()
                .map(|w| (w.text.clone(), w.stem.clone(), w.brk))
                .collect::<Vec<_>>()
        };
        let w = |text: &str, stem: &str, brk: bool| (text.to_owned(), stem.to_owned(), brk);
        assert_eq!(
            words("Jalapeños & Chips, n' Cookies-and-cream w/o beans!"),
            vec![
                w("jalapenos", "jalapeno", false),
                w("and", "and", false),
                w("chips", "chip", false),
                w("and", "and", true),
                w("cookies", "cookie", false),
                w("and", "and", false),
                w("cream", "cream", false),
                w("without", "without", false),
                w("beans", "bean", false),
            ]
        );
        assert_eq!(
            words("don't + x2 ×3"),
            vec![
                w("dont", "dont", false),
                w("x2", "x2", false),
                w("x3", "x3", false),
            ]
        );
        let cases = [
            ("burritos", "burrito"),
            ("nachos", "nacho"),
            ("fries", "frie"),
            ("dogs", "dog"),
            ("sandwiches", "sandwich"),
            ("dishes", "dish"),
            ("boxes", "box"),
            ("tots", "tot"),
            ("bus", "bus"),
            ("dog", "dog"),
            ("salsa", "salsa"),
        ];
        for (word, expected) in cases {
            assert_eq!(stem(word), expected, "{word:?}");
        }
    }
}
