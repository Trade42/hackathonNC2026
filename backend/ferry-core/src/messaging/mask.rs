//! Telegram ids and usernames in logs, masked as the sidecar masks them (messaging/README.md):
//! the last 3 characters stay, `•••321` and `@•••pez`. A DM's space id is the user's id, so it is
//! masked too; a group's negative id is not personal and stays whole.

/// `987654321` becomes `•••321`.
pub fn mask_telegram_id(id: &str) -> String {
    format!("•••{}", last_chars(id, 3))
}

/// `maya_l` (or `@maya_l`) becomes `@•••a_l`.
pub fn mask_telegram_username(username: &str) -> String {
    format!("@•••{}", last_chars(username.trim_start_matches('@'), 3))
}

/// A Telegram space id: a group's (negative) id as is, a DM's (the user's id) masked.
pub fn mask_space_id(space_id: &str) -> String {
    if space_id.starts_with('-') {
        space_id.to_owned()
    } else {
        mask_telegram_id(space_id)
    }
}

/// The last `n` characters, or fewer when the value is that short; nothing when it is only that
/// long, so a short value never prints whole.
fn last_chars(value: &str, n: usize) -> &str {
    let count = value.chars().count();
    if count <= n {
        return "";
    }
    let start = value
        .char_indices()
        .nth(count - n)
        .map_or(value.len(), |(i, _)| i);
    &value[start..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_last_three() {
        assert_eq!(mask_telegram_id("987654321"), "•••321");
        assert_eq!(mask_telegram_username("maya_l"), "@•••a_l");
        assert_eq!(mask_telegram_username("@maya_l"), "@•••a_l");
        assert_eq!(mask_space_id("987654321"), "•••321");
        assert_eq!(mask_space_id("-1001234567890"), "-1001234567890");
    }

    #[test]
    fn short_values_never_print_whole() {
        assert_eq!(mask_telegram_id("123"), "•••");
        assert_eq!(mask_telegram_id(""), "•••");
        assert_eq!(mask_telegram_username("abc"), "@•••");
        assert_eq!(mask_telegram_username("añb_é"), "@•••b_é");
    }
}
