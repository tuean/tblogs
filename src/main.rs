//! tblogs — a small blog server built on axum, SQLite and askama.
//!
//! Run with `cargo run`, then open http://127.0.0.1:8080.
//! See `README.md` for the environment variables and `deploy/README.md` for
//! what it takes to make an instance publicly reachable.

mod assets;
mod auth;
mod config;
mod content;
mod db;
mod error;
mod frontmatter;
mod markdown;
mod models;
mod routes;
mod slug;
mod state;
mod templates;
mod util;

use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use sqlx::SqlitePool;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use crate::{assets::Assets, config::Config, state::AppState};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let cfg = Config::from_env().context("invalid configuration")?;

    // Resolve the content directory before touching the database, so a typo in
    // TBLOGS_CONTENT_DIR fails immediately and loudly instead of quietly
    // serving an empty blog.
    let assets = match &cfg.content_dir {
        Some(dir) => Some(Assets::new(dir.clone()).with_context(|| {
            format!(
                "TBLOGS_CONTENT_DIR={} is not a usable directory",
                dir.display()
            )
        })?),
        None => None,
    };

    let pool = db::init(&cfg).await?;

    // Import the directory before serving, so the first request already sees
    // every article.
    if let (Some(dir), Some(assets)) = (&cfg.content_dir, &assets) {
        let files = content::scan(dir)?;
        let report = content::sync(&pool, dir, &files).await?;
        tracing::info!(
            directory = %assets.root().display(),
            report = %report.summary(),
            "initial content sync"
        );
    } else {
        // Demo content would only get in the way of a real content directory.
        let seeded = db::seed_demo_if_enabled(&pool, &cfg).await?;
        if seeded > 0 {
            tracing::info!(count = seeded, "inserted demo posts");
        }
    }

    // Keep watching for edits made in an external editor.
    if let Some(dir) = cfg.content_dir.clone() {
        let interval = cfg.sync_interval;
        if interval.is_zero() {
            tracing::info!("TBLOGS_SYNC_INTERVAL is 0; the directory is only read at startup");
        } else {
            tracing::info!(
                directory = %dir.display(),
                seconds = interval.as_secs(),
                "watching the content directory"
            );
            tokio::spawn(watch_content(pool.clone(), dir, interval));
        }
    }

    let state = AppState::new(pool, cfg, assets);
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

/// Poll the content directory and import whatever changed.
///
/// Polling rather than filesystem notifications: the scan only stats files, the
/// directory holds tens of them, and it avoids a dependency whose behaviour
/// differs across platforms and editors. Every save in Typora is atomic-rename
/// plus a fresh mtime, which a stat picks up reliably.
async fn watch_content(pool: SqlitePool, root: PathBuf, interval: Duration) {
    let mut last = match content::scan(&root) {
        Ok(files) => content::fingerprint(&files),
        Err(error) => {
            tracing::error!(%error, "could not scan the content directory");
            return;
        }
    };

    let mut ticker = tokio::time::interval(interval);
    // A slow sync must not cause a burst of catch-up ticks.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        ticker.tick().await;

        let files = match content::scan(&root) {
            Ok(files) => files,
            Err(error) => {
                tracing::warn!(%error, "content directory scan failed; will retry");
                continue;
            }
        };

        let fingerprint = content::fingerprint(&files);
        if fingerprint == last {
            continue;
        }

        match content::sync(&pool, &root, &files).await {
            Ok(report) => {
                // Advance only on success, so a failure is retried rather than
                // being treated as already handled.
                last = fingerprint;
                if report.changed() {
                    tracing::info!(report = %report.summary(), "content directory changed");
                }
            }
            Err(error) => {
                tracing::error!(%error, "content sync failed; will retry");
            }
        }
    }
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
