//! The guard that keeps every test on the local Postgres and away from Supabase.
//!
//! `#[sqlx::test]` reads `DATABASE_URL` through `dotenvy::var`, which also loads the first `.env`
//! it finds walking up from the test's working directory (the crate root), and that is the root
//! `.env` with the Supabase pooler URL and keys. dotenvy never overrides a variable that is already
//! set, so [`force_local_test_env`] sets the variables first, from a constructor that runs before
//! the test harness (`tests/common/mod.rs`, and `lib.rs` for the unit tests).

use std::str::FromStr;

use sqlx::postgres::PgConnectOptions;

/// The local Postgres 17 that tests use when `TEST_DATABASE_URL` is not a local URL.
pub const LOCAL_TEST_DATABASE_URL: &str = "postgres://postgres:postgres@127.0.0.1:55432/postgres";

/// True when `url` is a Postgres URL that sqlx connects to over TCP on 127.0.0.1, localhost or ::1.
///
/// The host is the one sqlx parses, not the URL's own: `?host=` and `?hostaddr=` override the
/// URL's host, and `?host=/path` switches to a Unix socket, which is refused too.
pub fn is_local_database_url(url: &str) -> bool {
    let postgres =
        url::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "postgres" | "postgresql"));
    postgres
        && PgConnectOptions::from_str(url).is_ok_and(|options| {
            options.get_socket().is_none()
                // The URL's own IPv6 host keeps its brackets; `?hostaddr=::1` does not.
                && matches!(
                    options.get_host(),
                    "127.0.0.1" | "localhost" | "[::1]" | "::1"
                )
        })
}

/// Points `DATABASE_URL` at the local test database and blanks the Supabase Storage settings.
///
/// The URL is `TEST_DATABASE_URL` from the process environment when it is local, otherwise
/// [`LOCAL_TEST_DATABASE_URL`]. `SUPABASE_URL` and `SUPABASE_SECRET_KEY` become empty strings,
/// which the config reads as unset, so no test can call Supabase Storage either. Panics if
/// `DATABASE_URL` is not local afterwards.
///
/// # Safety
///
/// Changes the process environment, so it must run before any other thread exists: call it only
/// from a `#[ctor]`.
pub unsafe fn force_local_test_env() {
    let url = match std::env::var("TEST_DATABASE_URL") {
        Ok(url) if is_local_database_url(&url) => url,
        Ok(url) if !url.is_empty() => {
            eprintln!(
                "ferry-core tests: TEST_DATABASE_URL is not on 127.0.0.1 or localhost; using {}",
                redacted(LOCAL_TEST_DATABASE_URL)
            );
            LOCAL_TEST_DATABASE_URL.to_owned()
        }
        _ => LOCAL_TEST_DATABASE_URL.to_owned(),
    };
    // SAFETY: the caller guarantees no other thread exists yet.
    unsafe {
        std::env::set_var("DATABASE_URL", &url);
        std::env::set_var("SUPABASE_URL", "");
        std::env::set_var("SUPABASE_SECRET_KEY", "");
    }
    let seen = std::env::var("DATABASE_URL").unwrap_or_default();
    assert!(
        is_local_database_url(&seen),
        "ferry-core tests: DATABASE_URL must be on 127.0.0.1 or localhost, refusing to run"
    );
}

/// `host:port` that sqlx connects to for a database URL, for logs and test output; never the user
/// or password. Like [`is_local_database_url`], it honors `?host=` and `?hostaddr=`.
pub fn redacted(url: &str) -> String {
    match PgConnectOptions::from_str(url) {
        Ok(options) => match options.get_socket() {
            Some(socket) => format!("socket {}", socket.display()),
            None => format!("{}:{}", options.get_host(), options.get_port()),
        },
        Err(_) => "(unparseable URL)".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_urls() {
        assert!(is_local_database_url(LOCAL_TEST_DATABASE_URL));
        assert!(is_local_database_url("postgresql://u:p@localhost/db"));
        assert!(is_local_database_url("postgres://u:p@[::1]:5432/db"));
        assert!(!is_local_database_url(
            "postgres://postgres.abc:pw@aws-0-us-east-1.pooler.supabase.com:5432/postgres"
        ));
        assert!(!is_local_database_url(
            "postgres://x:y@example.invalid:5432/z"
        ));
        assert!(!is_local_database_url(
            "postgres://127.0.0.1.example.com/db"
        ));
        assert!(!is_local_database_url("mysql://127.0.0.1/db"));
        assert!(!is_local_database_url(""));
    }

    /// sqlx takes the host from `?host=` and `?hostaddr=`, so the URL's own host is not enough.
    #[test]
    fn query_string_hosts_cannot_fool_the_guard() {
        assert!(!is_local_database_url(
            "postgres://u:p@127.0.0.1:5432/db?host=aws-0-us-east-1.pooler.supabase.com"
        ));
        assert!(!is_local_database_url(
            "postgres://u:p@localhost/db?hostaddr=203.0.113.7"
        ));
        assert!(!is_local_database_url(
            "postgres://u:p@127.0.0.1/db?sslmode=disable&host=db.example.com"
        ));
        assert!(!is_local_database_url(
            "postgres://u:p@127.0.0.1/db?host=/var/run/postgresql"
        ));
        // Query strings as such are fine, and so is a local override.
        assert!(is_local_database_url(
            "postgres://u:p@127.0.0.1:55432/db?sslmode=disable"
        ));
        assert!(is_local_database_url(
            "postgres://u:p@127.0.0.1:55432/db?hostaddr=::1"
        ));
    }

    #[test]
    fn redacted_drops_credentials() {
        assert_eq!(
            redacted("postgres://u:secret@127.0.0.1:55432/db"),
            "127.0.0.1:55432"
        );
        assert_eq!(
            redacted("postgres://u:secret@db.example.com/db"),
            "db.example.com:5432"
        );
        assert_eq!(
            redacted("postgres://u:secret@127.0.0.1:55432/db?host=db.example.com"),
            "db.example.com:55432"
        );
    }
}
