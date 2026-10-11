//! Telegram chats as `ferry.message_spaces` rows: a group maps to a circle, a DM to its user.
//!
//! - A group maps on the bot's membership event, or lazily on the first group message the bot
//!   sees (the demo group had the bot before this code shipped, so no membership event comes).
//!   In DEMO_MODE the group titled [`DEMO_GROUP_TITLE`] maps to [`DEMO_CIRCLE_ID`] while no other
//!   chat maps there, so a later group someone gives the same title gets a circle of its own;
//!   any other group gets a new circle named after its title. Linked senders join the circle when they
//!   first speak, and the person who adds the bot joins it too.
//! - A group added from meet-again's startgroup link sends `/start@<bot> <invite_id>`: the group
//!   then maps to that invite's circle, when the sender is its inviter or an accepted invitee.
//! - Removing the bot keeps the row and its circle (`bot_in_chat` false), so adding it back maps
//!   the group to the same circle. A group upgraded to a supergroup moves to its new chat id.
//! - In a DM, `bot_in_chat: false` means the user blocked the bot: they are unreachable in
//!   Telegram (meet-again then reaches them in the app only) until they unblock it. A send the
//!   sidecar refuses with 403 marks the chat the same way.
//! - When the bot is an admin of a group, it asks the sidecar for an invite link and stores it in
//!   `invite_url`, which meet-again's `Invite.thread.open_url` uses.

use sqlx::{PgConnection, PgPool};

use crate::error::ApiResult;
use crate::ids;
use crate::messaging::mask::mask_space_id;
use crate::messaging::sidecar::{self, SendRequest};
use crate::state::AppState;

/// The demo group's Telegram title (docs/DEMO.md, Channel: Telegram).
pub const DEMO_GROUP_TITLE: &str = "Hinton James 4th floor";
/// The circle the demo group maps to in DEMO_MODE.
pub const DEMO_CIRCLE_ID: &str = "cir_hinton4";
/// A new circle's emoji (`circles.emoji` is required).
pub const NEW_CIRCLE_EMOJI: &str = "👥";
/// A new circle's name when the group has no title; titles are cut to this many characters.
pub const UNTITLED_GROUP: &str = "Telegram group";
pub const MAX_CIRCLE_NAME_CHARS: usize = 60;

/// What the bot says when it joins a group.
pub const GROUP_HELLO: &str = "Hi, I'm Ferry. When someone here says \"heading to Cosmic Cantina at 6\", I'll offer to collect the group's orders. Say \"ferry\" to ask me anything.";

/// `pg_advisory_xact_lock` namespace for one chat's mapping, so two first messages of one group
/// never create two circles.
const MAP_LOCK_NAMESPACE: i32 = 0x4645_5259;

/// Who the Telegram account `telegram_user_id` is in Ferry, if anyone.
pub async fn user_for_telegram(pool: &PgPool, telegram_user_id: &str) -> ApiResult<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT id FROM ferry.users WHERE telegram_user_id = $1")
            .bind(telegram_user_id)
            .fetch_optional(pool)
            .await?,
    )
}

/// The circle the group `space_id` maps to, mapping it now if it has no circle yet (see the
/// module docs). Marks the bot as in the chat, since it hears the group.
pub async fn ensure_group(
    state: &AppState,
    space_id: &str,
    chat_title: Option<&str>,
) -> ApiResult<String> {
    let mut tx = state.pool.begin().await?;
    lock_space(&mut tx, space_id).await?;
    let existing: Option<Option<String>> = sqlx::query_scalar(
        "UPDATE ferry.message_spaces SET bot_in_chat = true, updated_at = now()
          WHERE space_id = $1 AND is_group
         RETURNING circle_id",
    )
    .bind(space_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(Some(circle_id)) = existing {
        tx.commit().await?;
        return Ok(circle_id);
    }
    let circle_id =
        circle_for_new_group(&mut tx, state.config.demo_mode, space_id, chat_title).await?;
    sqlx::query(
        "INSERT INTO ferry.message_spaces (space_id, is_group, circle_id)
         VALUES ($1, true, $2)
         ON CONFLICT (space_id) DO UPDATE
            SET circle_id = EXCLUDED.circle_id, bot_in_chat = true, updated_at = now()",
    )
    .bind(space_id)
    .bind(&circle_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(space = %mask_space_id(space_id), circle_id, "Telegram group mapped to a circle");
    Ok(circle_id)
}

async fn lock_space(tx: &mut PgConnection, space_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock($1, hashtext($2))")
        .bind(MAP_LOCK_NAMESPACE)
        .bind(space_id)
        .execute(tx)
        .await?;
    Ok(())
}

/// The demo circle for the demo group in DEMO_MODE (when it is seeded and no other chat maps to
/// it yet), else a new circle named after the title.
async fn circle_for_new_group(
    tx: &mut PgConnection,
    demo_mode: bool,
    space_id: &str,
    chat_title: Option<&str>,
) -> ApiResult<String> {
    let title = chat_title.map(str::trim).filter(|t| !t.is_empty());
    if demo_mode && title == Some(DEMO_GROUP_TITLE) {
        // Two groups with the title mapping at once take turns here.
        lock_space(&mut *tx, DEMO_CIRCLE_ID).await?;
        let free: Option<String> = sqlx::query_scalar(
            "SELECT c.id FROM ferry.circles c
              WHERE c.id = $1
                AND NOT EXISTS (SELECT 1 FROM ferry.message_spaces s
                                 WHERE s.circle_id = c.id AND s.space_id <> $2)",
        )
        .bind(DEMO_CIRCLE_ID)
        .bind(space_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(id) = free {
            return Ok(id);
        }
    }
    let name: String = title
        .unwrap_or(UNTITLED_GROUP)
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_CIRCLE_NAME_CHARS)
        .collect();
    let id = ids::new_id(ids::CIRCLE);
    sqlx::query("INSERT INTO ferry.circles (id, name, emoji) VALUES ($1, $2, $3)")
        .bind(&id)
        .bind(name.trim())
        .bind(NEW_CIRCLE_EMOJI)
        .execute(&mut *tx)
        .await?;
    Ok(id)
}

/// Adds `user_id` to `circle_id` unless they are in it already.
pub async fn add_member(pool: &PgPool, circle_id: &str, user_id: &str) -> ApiResult<()> {
    let added = sqlx::query(
        "INSERT INTO ferry.circle_members (circle_id, user_id) VALUES ($1, $2)
         ON CONFLICT DO NOTHING",
    )
    .bind(circle_id)
    .bind(user_id)
    .execute(pool)
    .await?
    .rows_affected();
    if added > 0 {
        tracing::info!(
            circle_id,
            user_id,
            "joined the circle from its Telegram group"
        );
    }
    Ok(())
}

/// Records the DM `space_id` (the user's Telegram id) as `user_id`'s and reachable: they just
/// wrote to the bot.
pub async fn ensure_dm(pool: &PgPool, space_id: &str, user_id: &str) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO ferry.message_spaces (space_id, is_group, dm_user_id)
         VALUES ($1, false, $2)
         ON CONFLICT (space_id) DO UPDATE
            SET dm_user_id = EXCLUDED.dm_user_id, bot_in_chat = true, updated_at = now()
          WHERE ferry.message_spaces.dm_user_id IS DISTINCT FROM EXCLUDED.dm_user_id
             OR NOT ferry.message_spaces.bot_in_chat",
    )
    .bind(space_id)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Marks the chat `space_id` as not reachable: the bot was removed from the group, or the user
/// blocked it. A DM with no row yet gets one, tied to the user when the account is linked.
pub async fn mark_unreachable(pool: &PgPool, space_id: &str) -> ApiResult<()> {
    let is_group = space_id.starts_with('-');
    let user_id = if is_group {
        None
    } else {
        user_for_telegram(pool, space_id).await?
    };
    sqlx::query(
        "INSERT INTO ferry.message_spaces (space_id, is_group, dm_user_id, bot_in_chat)
         VALUES ($1, $2, $3, false)
         ON CONFLICT (space_id) DO UPDATE
            SET bot_in_chat = false, bot_is_admin = false, invite_url = NULL, updated_at = now()",
    )
    .bind(space_id)
    .bind(is_group)
    .bind(user_id)
    .execute(pool)
    .await?;
    tracing::info!(space = %mask_space_id(space_id), "Telegram chat marked unreachable");
    Ok(())
}

/// The Telegram chat to DM `user_id` in: their Telegram id, unless they never linked one or
/// blocked the bot. None means the app only.
pub async fn reachable_dm(pool: &PgPool, user_id: &str) -> ApiResult<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT u.telegram_user_id FROM ferry.users u
          WHERE u.id = $1 AND u.telegram_user_id IS NOT NULL
            AND NOT EXISTS (SELECT 1 FROM ferry.message_spaces s
                             WHERE s.space_id = u.telegram_user_id AND NOT s.bot_in_chat)",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?)
}

/// The fields of a `kind: "membership"` event this module reads.
#[derive(Debug, Clone, Copy)]
pub struct Membership<'a> {
    pub space_id: &'a str,
    pub is_group: bool,
    /// Who added, promoted or removed the bot (or, in a DM, the user).
    pub sender_id: &'a str,
    pub chat_title: Option<&'a str>,
    pub bot_in_chat: bool,
    pub bot_is_admin: Option<bool>,
    pub migrated_from_space_id: Option<&'a str>,
}

/// The bot was added to, promoted in or removed from a chat, or a group moved to a new id.
/// Returns what the bot says (a hello when it joins a group).
pub async fn on_membership(
    state: &AppState,
    event: &Membership<'_>,
) -> ApiResult<Vec<SendRequest>> {
    let space = mask_space_id(event.space_id);
    if let Some(old) = event
        .migrated_from_space_id
        .filter(|old| *old != event.space_id)
    {
        move_space(&state.pool, old, event.space_id).await?;
        tracing::info!(from = %mask_space_id(old), to = %space, "Telegram group moved to a new id");
    }
    if !event.is_group {
        if event.bot_in_chat {
            sqlx::query(
                "UPDATE ferry.message_spaces SET bot_in_chat = true, updated_at = now()
                  WHERE space_id = $1 AND NOT is_group",
            )
            .bind(event.space_id)
            .execute(&state.pool)
            .await?;
            tracing::info!(%space, "Telegram user unblocked the bot");
        } else {
            mark_unreachable(&state.pool, event.space_id).await?;
        }
        return Ok(Vec::new());
    }
    if !event.bot_in_chat {
        mark_unreachable(&state.pool, event.space_id).await?;
        return Ok(Vec::new());
    }

    let was_in_chat: bool = sqlx::query_scalar(
        "SELECT COALESCE((SELECT bot_in_chat AND circle_id IS NOT NULL
                            FROM ferry.message_spaces WHERE space_id = $1), false)",
    )
    .bind(event.space_id)
    .fetch_one(&state.pool)
    .await?;
    let circle_id = ensure_group(state, event.space_id, event.chat_title).await?;
    if let Some(user_id) = user_for_telegram(&state.pool, event.sender_id).await? {
        add_member(&state.pool, &circle_id, &user_id).await?;
    }
    if let Some(admin) = event.bot_is_admin {
        sqlx::query(
            "UPDATE ferry.message_spaces SET bot_is_admin = $2, updated_at = now()
              WHERE space_id = $1",
        )
        .bind(event.space_id)
        .bind(admin)
        .execute(&state.pool)
        .await?;
        if admin {
            spawn_store_invite_link(state, event.space_id);
        }
    }
    // A migration's membership event is not a join: the bot was there all along.
    let joined = !was_in_chat && event.migrated_from_space_id.is_none();
    Ok(if joined {
        vec![SendRequest {
            space_id: event.space_id.to_owned(),
            text: GROUP_HELLO.to_owned(),
            buttons: Vec::new(),
        }]
    } else {
        Vec::new()
    })
}

/// Moves what core keyed on the group's old chat id to its new one: the space row (and the
/// invites that point at it) and the trip intents.
pub async fn move_space(pool: &PgPool, old: &str, new: &str) -> ApiResult<()> {
    let mut tx = pool.begin().await?;
    lock_space(&mut tx, old).await?;
    lock_space(&mut tx, new).await?;
    let moved = sqlx::query(
        "INSERT INTO ferry.message_spaces
                (space_id, is_group, circle_id, dm_user_id, line_phone_e164, invite_url,
                 bot_in_chat, bot_is_admin, created_at)
         SELECT $2, is_group, circle_id, dm_user_id, line_phone_e164, invite_url,
                bot_in_chat, bot_is_admin, created_at
           FROM ferry.message_spaces WHERE space_id = $1
         ON CONFLICT (space_id) DO UPDATE
            SET circle_id = COALESCE(ferry.message_spaces.circle_id, EXCLUDED.circle_id),
                invite_url = COALESCE(ferry.message_spaces.invite_url, EXCLUDED.invite_url),
                updated_at = now()",
    )
    .bind(old)
    .bind(new)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if moved > 0 {
        sqlx::query("UPDATE ferry.invites SET thread_space_id = $2 WHERE thread_space_id = $1")
            .bind(old)
            .bind(new)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM ferry.message_spaces WHERE space_id = $1")
            .bind(old)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE ferry.trip_intents SET space_id = $2 WHERE space_id = $1")
        .bind(old)
        .bind(new)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Asks the sidecar for the group's invite link on its own task and stores it. Nothing happens
/// when the bot turns out not to be an admin.
pub fn spawn_store_invite_link(state: &AppState, space_id: &str) {
    let state = state.clone();
    let space_id = space_id.to_owned();
    tokio::spawn(async move {
        let url = match sidecar::invite_link(&state.http, &state.config, &space_id).await {
            Ok(url) => url,
            Err(err) => {
                tracing::info!(space = %mask_space_id(&space_id), "no group invite link: {err}");
                return;
            }
        };
        if !is_invite_url(&url) {
            tracing::warn!(space = %mask_space_id(&space_id), "the sidecar's invite link is not a t.me link");
            return;
        }
        let stored = sqlx::query(
            "UPDATE ferry.message_spaces SET invite_url = $2, updated_at = now()
              WHERE space_id = $1 AND is_group",
        )
        .bind(&space_id)
        .bind(&url)
        .execute(&state.pool)
        .await;
        if let Err(err) = stored {
            tracing::warn!(space = %mask_space_id(&space_id), "storing the invite link failed: {err}");
        }
    });
}

/// What `message_spaces.invite_url`'s CHECK accepts.
pub fn is_invite_url(url: &str) -> bool {
    url.len() <= 256
        && url
            .strip_prefix("https://t.me/")
            .is_some_and(|rest| !rest.is_empty() && !rest.chars().any(char::is_whitespace))
}

/// What the bot says after a startgroup link maps a group.
pub fn mapped_line(emoji: &str, name: &str) -> String {
    format!("This group is now {emoji} {name} on Ferry.")
}

/// A group's `/start@<bot> <payload>`, sent when someone adds the bot through a startgroup link.
/// A payload that is a meet-again invite id maps the group to the invite's circle, when `user_id`
/// is its inviter or an accepted invitee (only they ever get that link). Returns what the bot
/// says, or nothing for any other payload.
pub async fn on_startgroup(
    state: &AppState,
    space_id: &str,
    user_id: &str,
    payload: &str,
) -> ApiResult<Vec<SendRequest>> {
    if !payload.starts_with("inv_") {
        return Ok(Vec::new());
    }
    let circle: Option<(String, String, String)> = sqlx::query_as(
        "SELECT c.id, c.name, c.emoji FROM ferry.invites i
           JOIN ferry.circles c ON c.id = i.circle_id
          WHERE i.id = $1
            AND (i.from_user_id = $2 OR EXISTS (
                  SELECT 1 FROM ferry.invite_responses r
                   WHERE r.invite_id = i.id AND r.user_id = $2 AND r.status = 'accepted'))",
    )
    .bind(payload)
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some((circle_id, name, emoji)) = circle else {
        tracing::info!(space = %mask_space_id(space_id), "startgroup payload not usable by its sender");
        return Ok(Vec::new());
    };

    let mut tx = state.pool.begin().await?;
    lock_space(&mut tx, space_id).await?;
    let previous: Option<Option<String>> =
        sqlx::query_scalar("SELECT circle_id FROM ferry.message_spaces WHERE space_id = $1")
            .bind(space_id)
            .fetch_optional(&mut *tx)
            .await?;
    sqlx::query(
        "INSERT INTO ferry.message_spaces (space_id, is_group, circle_id) VALUES ($1, true, $2)
         ON CONFLICT (space_id) DO UPDATE
            SET circle_id = EXCLUDED.circle_id, bot_in_chat = true, updated_at = now()",
    )
    .bind(space_id)
    .bind(&circle_id)
    .execute(&mut *tx)
    .await?;
    // The membership event that came just before may have made a circle for this group alone;
    // drop it when nothing else uses it.
    if let Some(Some(old)) = previous.filter(|p| p.as_deref() != Some(circle_id.as_str())) {
        sqlx::query(
            "DELETE FROM ferry.circles c WHERE c.id = $1
               AND NOT EXISTS (SELECT 1 FROM ferry.runs r WHERE r.circle_id = c.id)
               AND NOT EXISTS (SELECT 1 FROM ferry.invites i WHERE i.circle_id = c.id)
               AND NOT EXISTS (SELECT 1 FROM ferry.message_spaces s
                                WHERE s.circle_id = c.id AND s.space_id <> $2)",
        )
        .bind(&old)
        .bind(space_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    add_member(&state.pool, &circle_id, user_id).await?;
    tracing::info!(space = %mask_space_id(space_id), circle_id, "Telegram group mapped from a startgroup link");
    Ok(vec![SendRequest {
        space_id: space_id.to_owned(),
        text: mapped_line(&emoji, &name),
        buttons: Vec::new(),
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_urls_fit_the_check() {
        assert!(is_invite_url("https://t.me/+AbCdEf123"));
        assert!(!is_invite_url("https://t.me/"));
        assert!(!is_invite_url("http://t.me/+x"));
        assert!(!is_invite_url("https://t.me/+a b"));
        assert!(!is_invite_url(&format!(
            "https://t.me/+{}",
            "a".repeat(260)
        )));
    }
}
