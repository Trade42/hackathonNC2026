//! The real ferry-core binary starts with a malformed `DEMO_TELEGRAM_PERSONAS`, whether DEMO_MODE
//! is on or off (PR #17 review MINOR 3). The binary runs with an empty process environment and an
//! empty env file, so it never reads the root .env, on this test's own local database and an
//! OS-assigned port, and is killed once it listens.
mod common;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use sqlx::PgPool;

/// Not a `persona=@username` list, and easy to find if the server ever echoed it.
const MALFORMED: &str = "maya@not_a_pair_zq7";

/// Runs the binary with `demo_mode` and [`MALFORMED`] until it says it is listening, and returns
/// every line it printed by then, without color codes. Panics if it exits or takes over 60 s.
fn start_server(database_url: &str, env_file: &std::path::Path, demo_mode: &str) -> Vec<String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ferry-core"))
        .env_clear()
        .env("FERRY_ENV_FILE", env_file)
        .env("DATABASE_URL", database_url)
        .env("PORT", "0")
        .env("DEMO_MODE", demo_mode)
        .env("DEMO_TELEGRAM_PERSONAS", MALFORMED)
        .env("RUST_LOG", "info")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the ferry-core binary runs");
    let (lines_tx, lines) = mpsc::channel::<String>();
    for stream in [
        Box::new(child.stdout.take().unwrap()) as Box<dyn std::io::Read + Send>,
        Box::new(child.stderr.take().unwrap()),
    ] {
        let tx = lines_tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
    }
    drop(lines_tx);

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut seen = Vec::new();
    let listening = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match lines.recv_timeout(left) {
            Ok(line) => {
                let line = strip_ansi(&line);
                let done = line.contains("ferry-core listening on");
                seen.push(line);
                if done {
                    break true;
                }
            }
            Err(_) => break false,
        }
    };
    let exited = child.try_wait().unwrap();
    // Only this test's own child, by its handle.
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        listening && exited.is_none(),
        "ferry-core did not start (DEMO_MODE={demo_mode}, exit {exited:?}):\n{}",
        seen.join("\n")
    );
    seen
}

fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The lines about DEMO_TELEGRAM_PERSONAS other than the config summary, which only says
/// `DEMO_TELEGRAM_PERSONAS=` `set`, `unset`, `ignored` or `invalid`.
fn persona_lines(lines: &[String]) -> Vec<&String> {
    lines
        .iter()
        .filter(|l| l.contains("DEMO_TELEGRAM_PERSONAS") && !l.contains("DEMO_TELEGRAM_PERSONAS="))
        .collect()
}

// An empty database: the binary runs every migration itself at startup, as in production (it keeps
// sqlx's table in schema ferry, unlike `#[sqlx::test]`'s migrator).
#[sqlx::test(migrations = false)]
async fn a_malformed_demo_telegram_personas_never_stops_the_server(pool: PgPool) {
    // This test's database, on the local server the guard set DATABASE_URL to.
    let database = pool.connect_options().get_database().unwrap().to_owned();
    let mut url = url::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
    url.set_path(&database);
    let env_file = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("startup-config.env");
    std::fs::write(&env_file, "").unwrap();

    // DEMO_MODE off: not parsed, one warning.
    let lines = start_server(url.as_str(), &env_file, "false");
    let about = persona_lines(&lines);
    assert_eq!(about.len(), 1, "{about:#?}");
    assert!(
        about[0].contains("WARN") && about[0].contains("DEMO_MODE is off"),
        "{about:#?}"
    );
    assert!(lines.iter().any(|l| l.contains("DEMO_MODE=false")));
    assert!(
        lines
            .iter()
            .any(|l| l.contains("DEMO_TELEGRAM_PERSONAS=ignored")),
        "{lines:#?}"
    );
    assert_not_echoed(&lines);

    let confirm_code_column: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns
                         WHERE table_schema = 'ferry' AND table_name = 'telegram_challenges'
                           AND column_name = 'confirm_code')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(confirm_code_column, "the binary ran the migrations");

    // DEMO_MODE on: one error line, no personas, and the server still starts.
    let lines = start_server(url.as_str(), &env_file, "true");
    let about = persona_lines(&lines);
    assert_eq!(about.len(), 1, "{about:#?}");
    assert!(
        about[0].contains("ERROR") && about[0].contains("demo personas are disabled"),
        "{about:#?}"
    );
    assert!(lines.iter().any(|l| l.contains("DEMO_MODE=true")));
    assert!(
        lines
            .iter()
            .any(|l| l.contains("DEMO_TELEGRAM_PERSONAS=invalid")),
        "{lines:#?}"
    );
    assert_not_echoed(&lines);
}

fn assert_not_echoed(lines: &[String]) {
    for line in lines {
        assert!(!line.contains("not_a_pair"), "the value was echoed: {line}");
    }
}
