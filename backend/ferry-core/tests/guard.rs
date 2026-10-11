//! Proves the test guard: every test binary sees a local DATABASE_URL and no Supabase settings.
mod common;

use ferry_core::test_support::{is_local_database_url, redacted};
use sqlx::PgPool;

#[test]
fn database_url_seen_by_tests_is_local() {
    // The same call #[sqlx::test] makes; it also loads the root .env without overriding.
    let url = dotenvy::var("DATABASE_URL").expect("DATABASE_URL is set by the guard");
    println!("DATABASE_URL seen by this test binary: {}", redacted(&url));
    assert!(is_local_database_url(&url), "DATABASE_URL is not local");
    assert_eq!(dotenvy::var("SUPABASE_URL").ok().as_deref(), Some(""));
    assert_eq!(
        dotenvy::var("SUPABASE_SECRET_KEY").ok().as_deref(),
        Some("")
    );
}

#[sqlx::test]
async fn sqlx_test_connects_to_local_postgres(pool: PgPool) {
    let (addr, port): (Option<String>, Option<i32>) =
        sqlx::query_as("select host(inet_server_addr()), inet_server_port()")
            .fetch_one(&pool)
            .await
            .unwrap();
    println!("#[sqlx::test] connected to server {addr:?} port {port:?}");
    assert!(
        matches!(addr.as_deref(), Some("127.0.0.1" | "::1")),
        "#[sqlx::test] reached a non-local server"
    );
}

#[test]
fn every_integration_test_includes_the_guard() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut missing = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "rs") {
            let source = std::fs::read_to_string(&path).unwrap();
            if !source.lines().any(|l| l.trim() == "mod common;") {
                missing.push(path.file_name().unwrap().to_string_lossy().into_owned());
            }
        }
    }
    assert!(
        missing.is_empty(),
        "tests without `mod common;`: {missing:?}"
    );
}
