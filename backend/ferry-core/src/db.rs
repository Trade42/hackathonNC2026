//! The Postgres pool and the embedded migrations.
//!
//! Supabase is reached through the session pooler on port 5432; the transaction pooler on 6543
//! does not support prepared statements, so a URL on 6543 is refused. TLS follows the URL's
//! `sslmode`. Every connection uses `search_path = ferry, public`.

use std::str::FromStr;
use std::time::Duration;

use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions, PgSslMode};
use sqlx::{Executor, PgConnection};

/// At most 5 connections per instance: the laptop and the deployed server share the pooler.
pub const MAX_CONNECTIONS: u32 = 5;
pub const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
/// Idle connections go back to the pooler after this long.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// Supabase's transaction pooler port.
pub const TRANSACTION_POOLER_PORT: u16 = 6543;

pub static MIGRATOR: Migrator = sqlx::migrate!("../migrations");

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    // The URL is left out of the message: it holds the database password.
    #[error("DATABASE_URL is not a valid Postgres URL")]
    InvalidUrl,
    #[error(
        "DATABASE_URL uses port 6543, Supabase's transaction pooler, which does not support prepared statements. \
         Use the session pooler on port 5432 (Supabase dashboard > Connect > Session pooler)."
    )]
    TransactionPooler,
    #[error(
        "DATABASE_URL points at the local master database `postgres`, which tests and other workers share. \
         Create your own database (ferry_dev_<task>) and point DATABASE_URL at it."
    )]
    LocalMasterDatabase,
    #[error("cannot create schema ferry: {0}")]
    CreateSchema(#[source] sqlx::Error),
    #[error("migrations failed: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("cannot count applied migrations: {0}")]
    Count(#[source] sqlx::Error),
}

/// Parses and checks `DATABASE_URL`: refuses port 6543 and the shared local master database.
pub fn connect_options(url: &str) -> Result<PgConnectOptions, DbError> {
    if !(url.starts_with("postgres://") || url.starts_with("postgresql://")) {
        return Err(DbError::InvalidUrl);
    }
    // Never print `options` with {:?}: its Debug output includes the password.
    let options = PgConnectOptions::from_str(url).map_err(|_| DbError::InvalidUrl)?;
    if options.get_port() == TRANSACTION_POOLER_PORT {
        return Err(DbError::TransactionPooler);
    }
    // Without a database name, Postgres connects to the one named after the user.
    let database = options.get_database().unwrap_or(options.get_username());
    if is_local_host(options.get_host()) && database == "postgres" {
        return Err(DbError::LocalMasterDatabase);
    }
    Ok(options)
}

/// `host:port/database sslmode=...` for logs; never the user or password.
pub fn describe(options: &PgConnectOptions) -> String {
    let sslmode = match options.get_ssl_mode() {
        PgSslMode::Disable => "disable",
        PgSslMode::Allow => "allow",
        PgSslMode::Prefer => "prefer",
        PgSslMode::Require => "require",
        PgSslMode::VerifyCa => "verify-ca",
        PgSslMode::VerifyFull => "verify-full",
    };
    format!(
        "{}:{}/{} sslmode={sslmode}",
        options.get_host(),
        options.get_port(),
        options.get_database().unwrap_or("postgres")
    )
}

/// The app's pool. It connects lazily: the first query (the migrations, at startup) opens the
/// first connection, and each new connection sets `search_path = ferry, public`.
pub fn pool(options: PgConnectOptions) -> PgPool {
    PgPoolOptions::new()
        .max_connections(MAX_CONNECTIONS)
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .idle_timeout(IDLE_TIMEOUT)
        .after_connect(|conn: &mut PgConnection, _meta| {
            Box::pin(async move {
                conn.execute("SET search_path = ferry, public").await?;
                Ok(())
            })
        })
        .connect_lazy_with(options)
}

/// Creates schema `ferry` if missing, then runs the embedded migrations. sqlx's
/// `_sqlx_migrations` table lands in `ferry` through the pool's `search_path`. Returns how many
/// migrations are applied in total.
pub async fn migrate(pool: &PgPool) -> Result<i64, DbError> {
    pool.execute("CREATE SCHEMA IF NOT EXISTS ferry")
        .await
        .map_err(DbError::CreateSchema)?;
    MIGRATOR.run(pool).await?;
    sqlx::query_scalar("SELECT count(*) FROM ferry._sqlx_migrations WHERE success")
        .fetch_one(pool)
        .await
        .map_err(DbError::Count)
}

fn is_local_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1") || host.starts_with('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_the_transaction_pooler() {
        let err = connect_options(
            "postgresql://postgres.abc:pw@aws-0-us-east-1.pooler.supabase.com:6543/postgres",
        )
        .unwrap_err();
        assert!(matches!(err, DbError::TransactionPooler));
        assert!(err.to_string().contains("6543"));
        assert!(!err.to_string().contains("pw"));
    }

    #[test]
    fn accepts_the_session_pooler() {
        let options = connect_options(
            "postgres://postgres.abc:pw@aws-0-us-east-1.pooler.supabase.com:5432/postgres?sslmode=require",
        )
        .unwrap();
        assert_eq!(
            describe(&options),
            "aws-0-us-east-1.pooler.supabase.com:5432/postgres sslmode=require"
        );
    }

    #[test]
    fn refuses_the_local_master_database() {
        for url in [
            "postgres://postgres:postgres@127.0.0.1:55432/postgres",
            "postgres://postgres:postgres@localhost:55432",
        ] {
            assert!(
                matches!(connect_options(url), Err(DbError::LocalMasterDatabase)),
                "{url}"
            );
        }
        assert!(
            connect_options("postgres://postgres:postgres@127.0.0.1:55432/ferry_dev_x").is_ok()
        );
    }

    #[test]
    fn invalid_url_hides_the_url() {
        for url in [
            "mysql://u:hunter2@host/db",
            "postgres://u:hunter2@host:notaport/db",
        ] {
            let err = connect_options(url).unwrap_err();
            assert!(matches!(err, DbError::InvalidUrl), "{url}");
            assert!(!err.to_string().contains("hunter2"));
        }
    }

    #[test]
    fn readme_is_not_a_migration() {
        // sqlx skips names that are not <version>_<description>.sql, so the directory's README
        // embeds as nothing.
        assert!(MIGRATOR.iter().all(|m| !m.description.contains("README")));
    }
}
