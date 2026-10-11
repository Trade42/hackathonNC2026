-- Ferry schema, part 6: what the sign-in rate limits need beyond the per-phone count (contract v1.2,
-- review MINOR 2; docs/DECISIONS.md, Contract, auth-profile).
--
-- client_ip is the client's address as POST /v1/auth/text/start saw it (an IPv6 address is cut to its
-- /64), or 'direct' when no proxy header named one. It is null on rows made before this migration.
-- new_recipient is true when the code went to a phone Ferry had never texted (no user and no earlier
-- challenge for it): Photon's shared pool allows 50 such first messages per line per day.

ALTER TABLE ferry.verify_challenges
    ADD COLUMN client_ip     text CHECK (length(client_ip) <= 64),
    ADD COLUMN new_recipient boolean NOT NULL DEFAULT false;

-- The per-IP count: an address's codes in the last 10 minutes.
CREATE INDEX verify_challenges_client_ip_created_at_idx
    ON ferry.verify_challenges (client_ip, created_at DESC) WHERE client_ip IS NOT NULL;
-- The global counts: every code in the last 10 minutes, new recipients in the last 24 hours.
CREATE INDEX verify_challenges_created_at_idx ON ferry.verify_challenges (created_at DESC);
