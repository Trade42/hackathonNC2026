-- Ferry schema, part 7: a Telegram group's invite link, for meet-again (Sal's REQUEST-telegram.md;
-- docs/DECISIONS.md, Contract, circles-invites).
--
-- A Telegram group the bot is in maps to a circle through its ferry.message_spaces row. invite_url is
-- that group's invite link (https://t.me/+..., which the bot can create only where it is an admin), so
-- Invite.thread.open_url can send accepted members to the circle's group. Null when the bot has no link
-- for the group; open_url then falls back to the bot's startgroup link. Only group rows have one.

ALTER TABLE ferry.message_spaces
    ADD COLUMN invite_url text
        CHECK (invite_url ~ '^https://t\.me/[^[:space:]]+$' AND length(invite_url) <= 256),
    ADD CONSTRAINT message_spaces_invite_url_group_check CHECK (invite_url IS NULL OR is_group);
