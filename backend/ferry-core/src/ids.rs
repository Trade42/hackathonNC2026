//! Opaque, prefixed IDs: `<prefix>_<12 random characters from [0-9a-z]>`.

use rand::RngExt;

// The contract's prefixes (Contract v1.1, Conventions). Fixture IDs such as `run_demo` use the
// same prefixes with readable suffixes; clients must not parse either.
pub const USER: &str = "usr";
pub const CIRCLE: &str = "cir";
pub const AREA: &str = "area";
pub const RESTAURANT: &str = "rst";
pub const ITEM: &str = "itm";
pub const OPTION: &str = "opt";
pub const CHOICE: &str = "cho";
pub const PLACE: &str = "place";
pub const RUN: &str = "run";
pub const ORDER: &str = "ord";
pub const REQUEST: &str = "req";
pub const INVITE: &str = "inv";
pub const NOTIFICATION: &str = "ntf";
pub const TXN: &str = "txn";

pub const CONTRACT_PREFIXES: [&str; 14] = [
    USER,
    CIRCLE,
    AREA,
    RESTAURANT,
    ITEM,
    OPTION,
    CHOICE,
    PLACE,
    RUN,
    ORDER,
    REQUEST,
    INVITE,
    NOTIFICATION,
    TXN,
];

/// Length of the random part: 36^12 is about 4.7e18 values.
pub const RANDOM_LEN: usize = 12;

const ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// A new ID such as `ord_8kq2m4x0p1zt`. `prefix` is one of the constants above, or a module's
/// own lowercase prefix for an internal ID.
pub fn new_id(prefix: &str) -> String {
    debug_assert!(
        !prefix.is_empty() && prefix.bytes().all(|b| b.is_ascii_lowercase()),
        "ID prefixes are lowercase letters"
    );
    let mut rng = rand::rng();
    let mut id = String::with_capacity(prefix.len() + 1 + RANDOM_LEN);
    id.push_str(prefix);
    id.push('_');
    for _ in 0..RANDOM_LEN {
        id.push(ALPHABET[rng.random_range(0..ALPHABET.len())] as char);
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn shape_for_every_contract_prefix() {
        for prefix in CONTRACT_PREFIXES {
            let id = new_id(prefix);
            let (p, rest) = id.split_once('_').unwrap();
            assert_eq!(p, prefix);
            assert_eq!(rest.len(), RANDOM_LEN);
            assert!(rest.bytes().all(|b| ALPHABET.contains(&b)), "{id}");
        }
    }

    #[test]
    fn unique() {
        let ids: HashSet<String> = (0..10_000).map(|_| new_id(ORDER)).collect();
        assert_eq!(ids.len(), 10_000);
    }
}
