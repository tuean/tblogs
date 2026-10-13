//! The read-only HTTP server.
//!
//! It opens the published package read-only, serves it, and watches for a new
//! package to appear. There is no authoring path and no write access to the
//! database, so the worst a compromised request can do is read public content.

use std::{
    path::Path,
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};

use anyhow::{Context, Result};

use crate::{
    assets::Assets,
    config::{self, ServerConfig},
    db,
    models::Settings,
    routes,
    state::{AppState, Snapshot},
};

/// Load the package and serve it until the process is asked to stop.
pub async fn run(config: ServerConfig) -> Result<()> {
    let snapshot = Arc::new(load_snapshot(&config).await?);

    // The media directory is produced by `build`; a package without one is a
    // broken deployment rather than a blog with no pictures.
    let media = config.media_path();
    let assets = Assets::new(media.clone()).with_context(|| {
        format!(
            "{} is missing from the data directory; the package looks incomplete",
            media.display()
        )
    })?;

    let state = AppState::new(snapshot.clone(), assets, config.page_size);

    tracing::info!(
        title = %snapshot.settings.site_title,
        url = %snapshot.settings.site_url,
        "serving content package"
    );

    if config.reload_interval.is_zero() {
        tracing::info!("automatic reloading is disabled");
    } else {
        tracing::info!(
            seconds = config.reload_interval.as_secs(),
            "watching for a newly published package"
        );
        tokio::spawn(watch_package(state.clone(), config.clone()));
    }

    let app = routes::router(state);
    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("could not bind {}", config.bind))?;

    tracing::info!(address = %config.bind, "listening on http://{}", config.bind);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;

    Ok(())
}

/// Open the published package and read the settings stored in it.
pub async fn load_snapshot(config: &ServerConfig) -> Result<Snapshot> {
    let database = config.database_path();

    let pool = db::open_read_only(&database)
        .await
        .with_context(|| config::describe_missing_data_dir(&config.data_dir, &database))?;

    let stored = db::load_settings(&pool).await?;

    let settings = stored.with_overrides(
        config.site_title.clone(),
        config.site_description.clone(),
        config.site_url.clone(),
    );

    Ok(Snapshot { pool, settings })
}

/// Reload the package whenever the database file changes.
///
/// Polling rather than a filesystem notification: it is one `stat` per tick,
/// and it behaves identically whether the new package arrived by `rsync`, by
/// `scp`, or by hand.
async fn watch_package(state: AppState, config: ServerConfig) {
    let database = config.database_path();
    let mut seen = file_stamp(&database);

    let mut ticker = tokio::time::interval(config.reload_interval);
    // A slow reload must not cause a burst of catch-up ticks.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        ticker.tick().await;

        let stamp = file_stamp(&database);
        if stamp == 0 || stamp == seen {
            continue;
        }

        match load_snapshot(&config).await {
            Ok(snapshot) => {
                let posts = db::count_published(&snapshot.pool).await.unwrap_or(0);
                state.install(snapshot).await;

                // Advance only after a successful swap, so an interrupted
                // transfer is retried on the next tick rather than ignored.
                seen = stamp;

                tracing::info!(posts, "reloaded the content package");
            }
            Err(error) => {
                tracing::warn!(%error, "package not ready yet; will retry");
            }
        }
    }
}

/// Modification time of a file in nanoseconds, or zero when it is unreadable.
fn file_stamp(path: &Path) -> u128 {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|delta| delta.as_nanos())
        .unwrap_or(0)
}

/// Resolve when the process is asked to stop.
async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "could not listen for the shutdown signal");
        return;
    }

    tracing::info!("shutting down");
}

/// Initialise logging, honouring `RUST_LOG`.
pub fn init_tracing() {
    use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("tblogs=info,tower_http=warn"));

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .init();
}

/// How long to wait between reload checks when none was configured.
pub const DEFAULT_RELOAD_INTERVAL: Duration = Duration::from_secs(5);

/// Convenience for callers that only need the stored settings.
pub async fn read_settings(config: &ServerConfig) -> Result<Settings> {
    Ok(load_snapshot(config).await?.settings)
}
