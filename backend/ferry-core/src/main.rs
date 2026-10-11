use std::net::SocketAddr;
use std::process::ExitCode;

use anyhow::Context;
use ferry_core::config::Config;
use ferry_core::state::AppState;
use ferry_core::{app, db, demo, storage};
use tracing_subscriber::EnvFilter;

/// Without RUST_LOG: info, minus the Postgres notices from `IF NOT EXISTS` on every start.
const DEFAULT_LOG_FILTER: &str = "info,sqlx::postgres::notice=warn";

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER)),
        )
        .init();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!("ferry-core failed to start: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    // `ferry-core seed` or `ferry-core reset` runs once after the migrations, instead of serving.
    let command = demo::Command::from_args(std::env::args().skip(1))?;
    let (config, env_file) = Config::load()?;
    match &env_file {
        Some(path) => tracing::info!("env file: {}", path.display()),
        None => tracing::info!("env file: none found; using the process environment only"),
    }
    tracing::info!("config: {}", config.summary());

    let options = db::connect_options(config.database_url.expose())?;
    tracing::info!("database: {}", db::describe(&options));
    let pool = db::pool(options);
    let applied = db::migrate(&pool).await?;
    tracing::info!(
        "migrations ran: {} embedded, {applied} applied in schema ferry",
        db::MIGRATOR.iter().count()
    );
    if let Some(command) = command {
        let result = demo::run_command(command, &pool, &config).await;
        pool.close().await;
        return result;
    }

    let state = AppState::new(pool, config).context("building the HTTP client")?;
    ferry_core::payments::start(&state);
    ferry_core::tracking::start(&state);
    ferry_core::requests::start(&state);
    storage::ensure_buckets_at_startup(&state.http, &state.config).await;

    let addr = SocketAddr::from(([0, 0, 0, 0], state.config.port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("cannot listen on {addr}"))?;
    tracing::info!("ferry-core listening on http://{addr}");
    demo::simulator::resume_at_startup(&state);
    axum::serve(listener, app::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("ferry-core stopped");
    Ok(())
}

/// Ctrl-C or SIGTERM.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
