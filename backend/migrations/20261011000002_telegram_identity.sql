-- Ferry schema, part 8: Telegram identity and Telegram sign-in (contract v1.3; Sal's REQUEST-telegram.md;
-- docs/DECISIONS.md, Contract, auth-telegram).
--
-- Users are identified by their Telegram user id, not a phone. A Telegram sign-up has no phone, so
-- phone_e164 becomes nullable, and every user keeps at least one of the two. The seeded personas keep
-- their phones and stay valid. telegram_user_id is the Bot API's user id as a decimal string (positive,
-- at most 52 significant bits today). telegram_username is the @username without the @, as the sidecar
-- forwards it (sender_username); null when the account has none.

ALTER TABLE ferry.users
    ALTER COLUMN phone_e164 DROP NOT NULL,
    ADD COLUMN telegram_user_id  text UNIQUE CHECK (telegram_user_id ~ '^[1-9][0-9]{0,19}$'),
    ADD COLUMN telegram_username text CHECK (telegram_username ~ '^[A-Za-z0-9_]{1,64}$'),
    ADD CONSTRAINT users_phone_or_telegram_check
        CHECK (phone_e164 IS NOT NULL OR telegram_user_id IS NOT NULL);

-- Telegram sign-in. POST /v1/auth/telegram/start makes a row and returns bot_url
-- https://t.me/<bot>?start=<start_code>. The user taps Start, and the sidecar forwards "/start <start_code>"
-- from their Telegram user id: that binds the row to the id (telegram_user_id, bound_at) and the bot asks
-- them to confirm, with buttons that carry confirm_token. The bound user's Confirm verifies the row
-- (user_id, is_new, verified_at); Cancel expires it. GET /v1/auth/telegram/{id} reads status and, for a
-- short time after verification, issues a token, which is never stored.
--
-- id is the opaque challenge_id, the only key to the poll's token, so it is long and random and never
-- leaves the client that started it. start_code is in bot_url (and may be shown as a QR code), so it is not
-- a secret: binding needs it, and confirming needs the bound Telegram user to press the button. client_ip
-- is the start's client address (an IPv6 address cut to its /64), or 'direct', for the per-address limit.
CREATE TABLE ferry.telegram_challenges (
    id               text PRIMARY KEY CHECK (id ~ '^chl_[0-9a-z]{32}$'),
    start_code       text NOT NULL UNIQUE CHECK (start_code ~ '^[A-Za-z0-9_-]{16,64}$'),
    status           text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'verified', 'expired')),
    client_ip        text NOT NULL CHECK (length(client_ip) <= 64),
    telegram_user_id text CHECK (telegram_user_id ~ '^[1-9][0-9]{0,19}$'),
    confirm_token    text UNIQUE CHECK (confirm_token ~ '^[A-Za-z0-9_-]{16,32}$'),
    bound_at         timestamptz,
    user_id          text REFERENCES ferry.users (id) ON DELETE CASCADE,
    is_new           boolean,
    created_at       timestamptz NOT NULL DEFAULT now(),
    expires_at       timestamptz NOT NULL,
    verified_at      timestamptz,
    -- Bound means all three; a verified row was bound first.
    CHECK ((telegram_user_id IS NULL) = (confirm_token IS NULL) AND (confirm_token IS NULL) = (bound_at IS NULL)),
    CHECK ((status = 'verified') = (user_id IS NOT NULL AND is_new IS NOT NULL AND verified_at IS NOT NULL)),
    CHECK (status <> 'verified' OR telegram_user_id IS NOT NULL)
);
ALTER TABLE ferry.telegram_challenges ENABLE ROW LEVEL SECURITY;
-- The per-address limit: an address's starts in the last 10 minutes.
CREATE INDEX telegram_challenges_client_ip_created_at_idx ON ferry.telegram_challenges (client_ip, created_at DESC);
-- The global limit: every start in the last 10 minutes.
CREATE INDEX telegram_challenges_created_at_idx ON ferry.telegram_challenges (created_at DESC);
-- The foreign key (a user's delete cascades here).
CREATE INDEX telegram_challenges_user_id_idx ON ferry.telegram_challenges (user_id) WHERE user_id IS NOT NULL;

-- The lockdown's revoke block (20261010000005_lockdown.sql), for the new table.
REVOKE ALL ON ALL TABLES IN SCHEMA ferry FROM PUBLIC;

DO $$
DECLARE
    api_role text;
BEGIN
    FOREACH api_role IN ARRAY ARRAY['anon', 'authenticated'] LOOP
        IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = api_role) THEN
            EXECUTE format('REVOKE ALL ON ALL TABLES IN SCHEMA ferry FROM %I', api_role);
        END IF;
    END LOOP;
END
$$;
