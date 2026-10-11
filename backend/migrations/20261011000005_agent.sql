-- Ferry schema, part 9: the agent in Telegram (spec section 3; Sal's REQUEST-telegram.md; docs/DECISIONS.md,
-- Agent).
--
-- message_spaces: whether the bot is in the chat now. In a group, false once the bot was removed (the row and
-- its circle stay, so adding the bot back maps the group to the same circle). In a DM, false once the user
-- blocked the bot: the user is unreachable in Telegram until they unblock it. bot_is_admin is whether the bot
-- is an administrator of the group (it can make invite links only then).
ALTER TABLE ferry.message_spaces
    ADD COLUMN bot_in_chat  boolean NOT NULL DEFAULT true,
    ADD COLUMN bot_is_admin boolean NOT NULL DEFAULT false,
    ADD COLUMN updated_at   timestamptz NOT NULL DEFAULT now(),
    ADD CONSTRAINT message_spaces_bot_is_admin_group_check CHECK (NOT bot_is_admin OR is_group);

-- agent_turns: inbound_key identifies the inbound event a turn answers (a hash of its channel, kind, chat,
-- sender, time, text and button data). The unique index is the claim that keeps the bot from answering one
-- message twice, even when the sidecar or Telegram delivers it again. Null for turns that answer no inbound
-- event, such as a run's state change.
ALTER TABLE ferry.agent_turns
    ADD COLUMN inbound_key text CHECK (inbound_key ~ '^[0-9a-f]{64}$');
CREATE UNIQUE INDEX agent_turns_inbound_key_idx ON ferry.agent_turns (inbound_key);

-- A trip the agent heard someone announce ("heading to Cosmic Cantina at 6"). asked: the bot asked "Open a
-- Ferry to ...?" with Yes and No buttons; confirmed and declined are the announcer's answer; opened means a
-- run was opened for it (run_id), or the announcer's open run there was reused. The buttons carry the id
-- (trip:<id>:yes), and only the announcer's press counts. space_id has no foreign key, like agent_turns, so a
-- group upgrade can move it without a cascade. announced_status is the run status the bot last announced in
-- space_id, so each change of a run it manages is announced once. run_id is set null when the demo reset
-- deletes the run, so nothing here requires it.
CREATE TABLE ferry.trip_intents (
    id               bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    space_id         text NOT NULL,
    user_id          text NOT NULL REFERENCES ferry.users (id) ON DELETE CASCADE,
    circle_id        text REFERENCES ferry.circles (id) ON DELETE SET NULL,
    restaurant_id    text NOT NULL REFERENCES ferry.restaurants (id) ON DELETE CASCADE,
    leaving_at       timestamptz NOT NULL,
    window_closes_at timestamptz NOT NULL,
    status           text NOT NULL DEFAULT 'asked'
                     CHECK (status IN ('asked', 'confirmed', 'declined', 'opened')),
    -- How the restaurant was recognized: Gemini's classification, or the patterns alone (Gemini down).
    detected_by      text NOT NULL CHECK (detected_by IN ('gemini', 'patterns')),
    run_id           text REFERENCES ferry.runs (id) ON DELETE SET NULL,
    announced_status text CHECK (announced_status IN ('open', 'closed', 'purchased', 'delivering', 'delivered', 'cancelled')),
    created_at       timestamptz NOT NULL DEFAULT now(),
    decided_at       timestamptz,
    CHECK (window_closes_at >= leaving_at),
    CHECK ((status = 'asked') = (decided_at IS NULL))
);
ALTER TABLE ferry.trip_intents ENABLE ROW LEVEL SECURITY;
-- "Ask once": the space's recent intents.
CREATE INDEX trip_intents_space_id_created_at_idx ON ferry.trip_intents (space_id, created_at DESC);
-- The foreign keys.
CREATE INDEX trip_intents_user_id_idx ON ferry.trip_intents (user_id);
CREATE INDEX trip_intents_circle_id_idx ON ferry.trip_intents (circle_id) WHERE circle_id IS NOT NULL;
CREATE INDEX trip_intents_restaurant_id_idx ON ferry.trip_intents (restaurant_id);
CREATE INDEX trip_intents_run_id_idx ON ferry.trip_intents (run_id) WHERE run_id IS NOT NULL;

-- An order the bot drafted from someone's text ("chicken burrito no beans") and priced, waiting for the
-- drafter's Place order button (order:<id>:place); nothing else places it. request is the CreateOrderRequest
-- that placing sends, so the order is exactly what was priced; total_cents is the total the bot showed.
-- placing is the claim a press takes (a conditional update from pending, committed before the order is placed),
-- so two presses never place twice; placed sets order_id; cancelled ends it. A refusal before the order commits
-- puts a placing draft back to pending. Only the drafter's press counts. A draft goes with its run (the demo
-- reset deletes runs), and order_id is set null when the reset deletes the order.
CREATE TABLE ferry.order_drafts (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id     text NOT NULL REFERENCES ferry.users (id) ON DELETE CASCADE,
    run_id      text NOT NULL REFERENCES ferry.runs (id) ON DELETE CASCADE,
    space_id    text NOT NULL,
    request     jsonb NOT NULL CHECK (jsonb_typeof(request) = 'object'),
    total_cents bigint NOT NULL CHECK (total_cents >= 0),
    status      text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'placing', 'placed', 'cancelled')),
    order_id    text REFERENCES ferry.orders (id) ON DELETE SET NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    decided_at  timestamptz,
    CHECK ((status = 'pending') = (decided_at IS NULL))
);
ALTER TABLE ferry.order_drafts ENABLE ROW LEVEL SECURITY;
-- A user's latest pending draft in a chat ("add guac", "place it").
CREATE INDEX order_drafts_user_id_created_at_idx ON ferry.order_drafts (user_id, created_at DESC);
-- The foreign keys.
CREATE INDEX order_drafts_run_id_idx ON ferry.order_drafts (run_id);
CREATE INDEX order_drafts_order_id_idx ON ferry.order_drafts (order_id) WHERE order_id IS NOT NULL;

-- Telegram accounts with no Ferry user that the bot has already told how to sign in. Unknown senders get one
-- line, then silence (spec section 3). Only the id is kept, never a name or username.
CREATE TABLE ferry.unknown_senders (
    telegram_user_id text PRIMARY KEY CHECK (telegram_user_id ~ '^[1-9][0-9]{0,19}$'),
    told_at          timestamptz NOT NULL DEFAULT now()
);
ALTER TABLE ferry.unknown_senders ENABLE ROW LEVEL SECURITY;

-- The lockdown's revoke block (20261010000005_lockdown.sql), for the new tables.
REVOKE ALL ON ALL TABLES IN SCHEMA ferry FROM PUBLIC;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA ferry FROM PUBLIC;

DO $$
DECLARE
    api_role text;
BEGIN
    FOREACH api_role IN ARRAY ARRAY['anon', 'authenticated'] LOOP
        IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = api_role) THEN
            EXECUTE format('REVOKE ALL ON ALL TABLES IN SCHEMA ferry FROM %I', api_role);
            EXECUTE format('REVOKE ALL ON ALL SEQUENCES IN SCHEMA ferry FROM %I', api_role);
        END IF;
    END LOOP;
END
$$;
