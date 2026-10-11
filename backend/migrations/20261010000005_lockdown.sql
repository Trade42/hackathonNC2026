-- Ferry schema, part 5: lock the Supabase API roles out (spec sections 2 and 8).
--
-- The Data API serves only exposed schemas through the anon and authenticated roles, and ferry is not
-- exposed. As a second wall, those roles hold no privilege on ferry or anything in it, now or later.
-- Plain Postgres (the local test cluster) has no such roles, so every statement naming them runs
-- only when the role exists. Ferry creates no functions, so PUBLIC's default EXECUTE never applies.
-- Every later migration that creates a table must enable RLS on it, and should end with this file's
-- revoke block; infra/db/check-migrations.mjs verifies both.

REVOKE ALL ON SCHEMA ferry FROM PUBLIC;
REVOKE ALL ON ALL TABLES IN SCHEMA ferry FROM PUBLIC;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA ferry FROM PUBLIC;
REVOKE ALL ON ALL ROUTINES IN SCHEMA ferry FROM PUBLIC;

DO $$
DECLARE
    api_role text;
BEGIN
    FOREACH api_role IN ARRAY ARRAY['anon', 'authenticated'] LOOP
        IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = api_role) THEN
            EXECUTE format('REVOKE ALL ON SCHEMA ferry FROM %I', api_role);
            EXECUTE format('REVOKE ALL ON ALL TABLES IN SCHEMA ferry FROM %I', api_role);
            EXECUTE format('REVOKE ALL ON ALL SEQUENCES IN SCHEMA ferry FROM %I', api_role);
            EXECUTE format('REVOKE ALL ON ALL ROUTINES IN SCHEMA ferry FROM %I', api_role);
            EXECUTE format('ALTER DEFAULT PRIVILEGES IN SCHEMA ferry REVOKE ALL ON TABLES FROM %I', api_role);
            EXECUTE format('ALTER DEFAULT PRIVILEGES IN SCHEMA ferry REVOKE ALL ON SEQUENCES FROM %I', api_role);
            EXECUTE format('ALTER DEFAULT PRIVILEGES IN SCHEMA ferry REVOKE ALL ON ROUTINES FROM %I', api_role);
            EXECUTE format('ALTER DEFAULT PRIVILEGES IN SCHEMA ferry REVOKE ALL ON TYPES FROM %I', api_role);
        END IF;
    END LOOP;
END
$$;

-- sqlx keeps its bookkeeping table _sqlx_migrations in the connection's current schema, which on
-- Supabase is public. There Supabase's default privileges give anon and authenticated every privilege
-- on it, and the Data API would serve it at /rest/v1/_sqlx_migrations. Enable RLS on it and revoke
-- the API roles, wherever it lives and only when this role owns it. Nothing is created in public.
DO $$
DECLARE
    sqlx_table regclass;
    api_role   text;
BEGIN
    FOR sqlx_table IN
        SELECT c.oid::regclass
        FROM pg_class c
        JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE c.relname = '_sqlx_migrations'
          AND c.relkind = 'r'
          AND n.nspname IN ('public', 'ferry')
          AND pg_has_role(current_user, c.relowner, 'USAGE')
    LOOP
        EXECUTE format('ALTER TABLE %s ENABLE ROW LEVEL SECURITY', sqlx_table);
        FOREACH api_role IN ARRAY ARRAY['anon', 'authenticated'] LOOP
            IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = api_role) THEN
                EXECUTE format('REVOKE ALL ON TABLE %s FROM %I', sqlx_table, api_role);
            END IF;
        END LOOP;
    END LOOP;
END
$$;
