//! tblogs — a small blog server built on axum, SQLite and askama.
//!
//! Run with `cargo run`, then open http://127.0.0.1:8080.
//! See `README.md` for the environment variables and `deploy/README.md` for
//! what it takes to make an instance publicly reachable.

mod auth;
mod config;
mod db;
mod error;
mod markdown;
mod models;
mod routes;
mod slug;
mod state;
mod templates;
mod util;

use anyhow::{Context, Result};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use crate::{config::Config, state::AppState};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let cfg = Config::from_env().context("invalid configuration")?;

    let pool = db::init(&cfg).await?;

    let seeded = db::seed_demo_if_enabled(&pool, &cfg).await?;
    if seeded > 0 {
        tracing::info!(count = seeded, "inserted demo posts");
    }

    let state = AppState::new(pool, cfg);
    let app = routes::router(state.clone());

    let listener = tokio::net::TcpListener::bind(state.cfg.bind)
        .await
        .with_context(|| format!("could not bind {}", state.cfg.bind))?;

    tracing::info!(address = %state.cfg.bind, "tblogs listening on http://{}", state.cfg.bind);

    if state.cfg.admin_is_gated() {
        tracing::info!("admin surface requires the configured token");
    } else {
        tracing::warn!(
            "TBLOGS_ADMIN_TOKEN is unset, so /admin is unprotected — fine for \
             local use, not for a public deployment"
        );
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;

    Ok(())
}

/// Initialise logging, honouring `RUST_LOG`.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("tblogs=info,tower_http=warn"));

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .init();
}

/// Resolve when the process is asked to stop.
async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "could not listen for the shutdown signal");
        return;
    }

    tracing::info!("shutting down");
}
