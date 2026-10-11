-- Ferry schema, part 4: messaging, invites, notifications and the agent's log.

-- An iMessage space from Photon Spectrum (space_id to circle_id). A group chat may belong to a circle;
-- a DM is with one user. Spectrum's handles stay in the sidecar's memory; this maps the stable ids.
-- line_phone_e164 is the Photon shared-pool number this conversation uses (imessage(space).phone), so
-- core can show a user which number Ferry texts them from. It is null until the sidecar reports it.
CREATE TABLE ferry.message_spaces (
    space_id        text PRIMARY KEY,
    is_group        boolean NOT NULL,
    circle_id       text REFERENCES ferry.circles (id) ON DELETE SET NULL,
    dm_user_id      text REFERENCES ferry.users (id) ON DELETE SET NULL,
    line_phone_e164 text CHECK (line_phone_e164 ~ '^\+[1-9][0-9]{1,14}$'),
    created_at      timestamptz NOT NULL DEFAULT now(),
    CHECK (is_group OR circle_id IS NULL),
    CHECK (NOT is_group OR dm_user_id IS NULL)
);
ALTER TABLE ferry.message_spaces ENABLE ROW LEVEL SECURITY;
CREATE INDEX message_spaces_circle_id_idx ON ferry.message_spaces (circle_id) WHERE circle_id IS NOT NULL;
CREATE INDEX message_spaces_dm_user_id_idx ON ferry.message_spaces (dm_user_id) WHERE dm_user_id IS NOT NULL;

-- Invite. from is from_user_id, responses are invite_responses, thread.state is thread_state.
-- thread.compose is derived: the accepted members' phones plus the agent's, and a body.
CREATE TABLE ferry.invites (
    id              text PRIMARY KEY CHECK (starts_with(id, 'inv_')),
    kind            text NOT NULL DEFAULT 'meetup' CHECK (kind IN ('meetup')),
    circle_id       text NOT NULL REFERENCES ferry.circles (id) ON DELETE CASCADE,
    from_user_id    text NOT NULL REFERENCES ferry.users (id) ON DELETE CASCADE,
    message         text NOT NULL DEFAULT '',
    proposed_time   timestamptz,
    status          text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'accepted', 'declined', 'expired')),
    thread_state    text NOT NULL DEFAULT 'none' CHECK (thread_state IN ('none', 'creating', 'ready')),
    -- The group the agent created for the accepted members, when Spectrum allows it.
    thread_space_id text REFERENCES ferry.message_spaces (space_id) ON DELETE SET NULL,
    created_at      timestamptz NOT NULL DEFAULT now()
);
ALTER TABLE ferry.invites ENABLE ROW LEVEL SECURITY;
CREATE INDEX invites_circle_id_created_at_idx ON ferry.invites (circle_id, created_at DESC);
CREATE INDEX invites_from_user_id_idx ON ferry.invites (from_user_id);
CREATE INDEX invites_thread_space_id_idx ON ferry.invites (thread_space_id) WHERE thread_space_id IS NOT NULL;

-- InviteResponse: one row per invited member.
CREATE TABLE ferry.invite_responses (
    invite_id    text NOT NULL REFERENCES ferry.invites (id) ON DELETE CASCADE,
    user_id      text NOT NULL REFERENCES ferry.users (id) ON DELETE CASCADE,
    status       text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'accepted', 'declined')),
    responded_at timestamptz,
    PRIMARY KEY (invite_id, user_id)
);
ALTER TABLE ferry.invite_responses ENABLE ROW LEVEL SECURITY;
CREATE INDEX invite_responses_user_id_idx ON ferry.invite_responses (user_id);

-- Notification. read is read_at IS NOT NULL.
CREATE TABLE ferry.notifications (
    id         text PRIMARY KEY CHECK (starts_with(id, 'ntf_')),
    user_id    text NOT NULL REFERENCES ferry.users (id) ON DELETE CASCADE,
    type       text NOT NULL CHECK (type IN ('invite', 'run_open', 'order_update', 'payout', 'system')),
    title      text NOT NULL,
    body       text NOT NULL,
    data       jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(data) = 'object'),
    read_at    timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);
ALTER TABLE ferry.notifications ENABLE ROW LEVEL SECURITY;
CREATE INDEX notifications_user_id_created_at_idx ON ferry.notifications (user_id, created_at DESC);

-- POST /v1/push/subscribe. web keeps the PushSubscriptionJSON fields (expirationTime, in ms since the
-- epoch, as a timestamptz); ios and android keep the FCM or APNs token. Re-subscribing upserts on
-- endpoint or (platform, token).
CREATE TABLE ferry.push_subscriptions (
    id              bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id         text NOT NULL REFERENCES ferry.users (id) ON DELETE CASCADE,
    platform        text NOT NULL CHECK (platform IN ('web', 'ios', 'android')),
    endpoint        text UNIQUE,
    p256dh          text,
    auth            text,
    expiration_time timestamptz,
    token           text,
    created_at      timestamptz NOT NULL DEFAULT now(),
    UNIQUE (platform, token),
    CHECK ((platform = 'web') = (endpoint IS NOT NULL)),
    CHECK ((platform = 'web') = (token IS NULL))
);
ALTER TABLE ferry.push_subscriptions ENABLE ROW LEVEL SECURITY;
CREATE INDEX push_subscriptions_user_id_idx ON ferry.push_subscriptions (user_id);

-- One agent turn: an inbound message, the tool calls with their latency, and the reply (spec
-- section 3). tool_calls is [{name, args, ok, latency_ms}]. space_id has no foreign key, so logging
-- never fails on a space that is not mapped yet.
CREATE TABLE ferry.agent_turns (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    space_id     text,
    user_id      text REFERENCES ferry.users (id) ON DELETE SET NULL,
    is_group     boolean NOT NULL DEFAULT false,
    inbound_text text NOT NULL,
    reply_text   text,
    tool_calls   jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(tool_calls) = 'array'),
    model        text,
    latency_ms   integer CHECK (latency_ms >= 0),
    error        text,
    created_at   timestamptz NOT NULL DEFAULT now()
);
ALTER TABLE ferry.agent_turns ENABLE ROW LEVEL SECURITY;
CREATE INDEX agent_turns_space_id_created_at_idx ON ferry.agent_turns (space_id, created_at);
CREATE INDEX agent_turns_user_id_created_at_idx ON ferry.agent_turns (user_id, created_at);
