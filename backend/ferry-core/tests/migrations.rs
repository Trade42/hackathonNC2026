//! The app's own pool builder and migration runner put `_sqlx_migrations` in schema ferry.
mod common;

use ferry_core::db;
use sqlx::{AssertSqlSafe, Connection, Executor, PgConnection};

/// What the test reads from its scratch database before dropping it.
struct Observed {
    applied: i64,
    applied_again: i64,
    in_ferry: Option<String>,
    in_public: Option<String>,
    search_path: String,
}

#[tokio::test]
async fn migrations_table_lands_in_schema_ferry() {
    // DATABASE_URL is the local master database, set by the guard.
    let admin_url = std::env::var("DATABASE_URL").unwrap();
    let mut admin = PgConnection::connect(&admin_url).await.unwrap();
    let name = format!("ferry_test_core_{}", ferry_core::ids::new_id("migrate"));
    let mut url = url::Url::parse(&admin_url).unwrap();
    url.set_path(&name);
    admin
        .execute(AssertSqlSafe(format!(r#"CREATE DATABASE "{name}""#)))
        .await
        .unwrap();

    // Nothing panics between CREATE and DROP, so a failure leaves no database on the shared
    // Postgres; FORCE ends any connection a failed run left open.
    let observed = observe(url.as_str()).await;
    admin
        .execute(AssertSqlSafe(format!(
            r#"DROP DATABASE "{name}" WITH (FORCE)"#
        )))
        .await
        .unwrap();

    let observed = observed.unwrap();
    // Running again is a no-op.
    assert_eq!(observed.applied_again, observed.applied);
    assert_eq!(observed.applied, db::MIGRATOR.iter().count() as i64);
    assert!(
        observed.in_ferry.is_some(),
        "ferry._sqlx_migrations is missing"
    );
    assert!(
        observed.in_public.is_none(),
        "public._sqlx_migrations exists"
    );
    assert_eq!(observed.search_path, "ferry, public");
}

/// Migrates the database at `url` twice with the app's own pool and reads back where things went.
async fn observe(url: &str) -> anyhow::Result<Observed> {
    let pool = db::pool(db::connect_options(url)?);
    let observed = async {
        let applied = db::migrate(&pool).await?;
        let applied_again = db::migrate(&pool).await?;
        let (in_ferry, in_public) = sqlx::query_as(
            "select to_regclass('ferry._sqlx_migrations')::text, to_regclass('public._sqlx_migrations')::text",
        )
        .fetch_one(&pool)
        .await?;
        let search_path = sqlx::query_scalar("show search_path")
            .fetch_one(&pool)
            .await?;
        anyhow::Ok(Observed {
            applied,
            applied_again,
            in_ferry,
            in_public,
            search_path,
        })
    }
    .await;
    pool.close().await;
    observed
}
