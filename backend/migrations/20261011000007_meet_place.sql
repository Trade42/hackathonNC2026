-- Ferry schema, part 12: meet-again with a place and a time (contract v1.5.0). Sal's decision (2026-10-11,
-- about 01:08 EDT): meet-again becomes a ping with a place and a time, not a way back into an old group.
--
-- invites: the place the inviter proposes, copied inline like a run's return place, so the invite keeps
-- showing what was proposed even if the catalog changes. place_id is a restaurant's (rst_) or a known place's
-- (place_) id; it has no foreign key because it can point at either table. place_label is the restaurant's
-- name, the place's label, or the words someone typed to the bot (then with no id and no location). lat and
-- lng go together. Every column is new and nullable with no default, so adding it rewrites nothing and every
-- existing invite reads as "no place"; each CHECK allows null, so existing rows pass it.
ALTER TABLE ferry.invites
    ADD COLUMN place_id    text,
    ADD COLUMN place_label text,
    ADD COLUMN place_lat   double precision,
    ADD COLUMN place_lng   double precision,
    ADD CONSTRAINT invites_place_id_check
        CHECK (starts_with(place_id, 'rst_') OR starts_with(place_id, 'place_')),
    ADD CONSTRAINT invites_place_label_check CHECK (char_length(place_label) BETWEEN 1 AND 100),
    ADD CONSTRAINT invites_place_lat_check CHECK (place_lat BETWEEN -90 AND 90),
    ADD CONSTRAINT invites_place_lng_check CHECK (place_lng BETWEEN -180 AND 180),
    ADD CONSTRAINT invites_place_location_check CHECK ((place_lat IS NULL) = (place_lng IS NULL)),
    ADD CONSTRAINT invites_place_parts_check
        CHECK (place_label IS NOT NULL OR (place_id IS NULL AND place_lat IS NULL));

-- A meet-again the bot read without Gemini ("anyone want to go to Cosmic at 7?") and asked about once before
-- sending: "Ask Jordan, Leo and Maya to Cosmic Cantina at 7:00 PM?" with Send and Cancel buttons
-- (meet:<id>:send|cancel). Only the asker's press counts. asked is the question; sent means the invite went
-- out (invite_id); cancelled is the asker's Cancel. A press claims the row with a conditional update from
-- asked, so two presses never send twice; a refusal from invites puts it back to asked. circle_id is the
-- circle the question named (the group's, or the asker's most recent in a DM). The place columns are the
-- invite's, with the same rules. space_id has no foreign key, like trip_intents.
CREATE TABLE ferry.meet_drafts (
    id            bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id       text NOT NULL REFERENCES ferry.users (id) ON DELETE CASCADE,
    space_id      text NOT NULL,
    circle_id     text NOT NULL REFERENCES ferry.circles (id) ON DELETE CASCADE,
    place_id      text CHECK (starts_with(place_id, 'rst_') OR starts_with(place_id, 'place_')),
    place_label   text CHECK (char_length(place_label) BETWEEN 1 AND 100),
    place_lat     double precision CHECK (place_lat BETWEEN -90 AND 90),
    place_lng     double precision CHECK (place_lng BETWEEN -180 AND 180),
    proposed_time timestamptz,
    status        text NOT NULL DEFAULT 'asked' CHECK (status IN ('asked', 'sent', 'cancelled')),
    invite_id     text REFERENCES ferry.invites (id) ON DELETE SET NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    decided_at    timestamptz,
    CHECK ((place_lat IS NULL) = (place_lng IS NULL)),
    CHECK (place_label IS NOT NULL OR (place_id IS NULL AND place_lat IS NULL)),
    CHECK ((status = 'asked') = (decided_at IS NULL))
);
ALTER TABLE ferry.meet_drafts ENABLE ROW LEVEL SECURITY;
-- The foreign keys.
CREATE INDEX meet_drafts_user_id_idx ON ferry.meet_drafts (user_id);
CREATE INDEX meet_drafts_circle_id_idx ON ferry.meet_drafts (circle_id);
CREATE INDEX meet_drafts_invite_id_idx ON ferry.meet_drafts (invite_id) WHERE invite_id IS NOT NULL;

-- The lockdown's revoke block (20261010000005_lockdown.sql), for the new table.
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
