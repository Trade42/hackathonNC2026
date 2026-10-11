-- Ferry schema, part 13: public communities, starting with the university (contract v1.5.1). Sal's decision
-- (2026-10-11, 13:10 UTC and 13:25 UTC): Ferry should work like DoorDash, not only between people who already
-- know each other, through nearby public communities such as the university. A user with no Telegram group had
-- no circle, so they could neither post a trip nor order on one.
--
-- A public community is an ordinary circle with is_public set. Its members are not listed, it has no meet-again
-- or invites, and a run opened in it pings nobody (see docs/DECISIONS.md, Public communities). The existing
-- rules do the rest: a trip posted to it is seen and ordered on by every member, POST /v1/requests matches it,
-- and friends' circles stay private. The first one is UNC Chapel Hill (cir_campus), and every user is in it: the
-- backfill below adds today's users and a trigger adds each new one. A dorm or a club becomes public later by
-- data alone.
--
-- Additive and idempotent: running this file twice changes nothing the first run did not.

-- Every existing circle stays private. A constant default adds the column without rewriting the table.
ALTER TABLE ferry.circles ADD COLUMN IF NOT EXISTS is_public boolean NOT NULL DEFAULT false;

-- The university community. last_active_at and created_at take their defaults.
INSERT INTO ferry.circles (id, name, emoji, is_public)
VALUES ('cir_campus', 'UNC Chapel Hill', '🐏', true)
ON CONFLICT (id) DO NOTHING;

-- Every existing user joins it.
INSERT INTO ferry.circle_members (circle_id, user_id)
SELECT 'cir_campus', u.id FROM ferry.users u
ON CONFLICT (circle_id, user_id) DO NOTHING;

-- Every new user joins it too (only this default community: joining others is a later step). The insert reads
-- cir_campus from ferry.circles rather than naming it in VALUES, so a database without that row (a test that
-- deleted it) still signs users up: they just join nothing.
-- SECURITY INVOKER: it runs as whoever inserts the user (the server's role, the table owner), with an empty
-- search_path and schema-qualified names.
CREATE OR REPLACE FUNCTION ferry.join_campus()
RETURNS trigger
LANGUAGE plpgsql
SECURITY INVOKER
SET search_path = ''
AS $$
BEGIN
    INSERT INTO ferry.circle_members (circle_id, user_id)
    SELECT c.id, NEW.id FROM ferry.circles c WHERE c.id = 'cir_campus'
    ON CONFLICT (circle_id, user_id) DO NOTHING;
    RETURN NULL;
END
$$;

CREATE OR REPLACE TRIGGER users_join_campus
    AFTER INSERT ON ferry.users
    FOR EACH ROW EXECUTE FUNCTION ferry.join_campus();

-- No one but the owner may call the function (functions are executable by PUBLIC by default).
REVOKE ALL ON FUNCTION ferry.join_campus() FROM PUBLIC;

DO $$
DECLARE
    api_role text;
BEGIN
    FOREACH api_role IN ARRAY ARRAY['anon', 'authenticated'] LOOP
        IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = api_role) THEN
            EXECUTE format('REVOKE ALL ON FUNCTION ferry.join_campus() FROM %I', api_role);
        END IF;
    END LOOP;
END
$$;
