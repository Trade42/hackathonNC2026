# migrations

SQLx migrations for schema `ferry`, embedded into ferry-core with `sqlx::migrate!("../migrations")` and run at startup. Files are named `<version>_<description>.sql`. sqlx skips any other file name, including this README.

Before running them, ferry-core executes `CREATE SCHEMA IF NOT EXISTS ferry`, and every pool connection uses `search_path = ferry, public`, so sqlx's own `_sqlx_migrations` table lands in schema `ferry`.
