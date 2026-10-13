//! Runtime configuration, read from the environment.

use std::{env, net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::{Context, Result};

/// Configuration for one running instance.
#[derive(Debug, Clone)]
pub struct Config {
    /// Address the HTTP server binds to.
    ///
    /// Defaults to loopback on purpose: a fresh checkout must not become
    /// reachable from the surrounding network by accident. Set
    /// `TBLOGS_BIND=0.0.0.0:8080` once you actually intend to expose it.
    pub bind: SocketAddr,

    /// SQLite connection string, e.g. `sqlite://data/tblogs.db`.
    pub database_url: String,

    pub site_title: String,
    pub site_description: String,

    /// Absolute base URL, used to build the self-referencing links inside the
    /// RSS feed. RSS readers require absolute URLs.
    pub site_url: String,

    /// When set, `/admin` requires this token. When unset, `/admin` is open,
    /// which is only appropriate for local development.
    pub admin_token: Option<String>,

    /// Posts per page on the public index and tag pages.
    pub page_size: i64,

    /// Insert demo posts when the database is empty.
    pub seed_demo: bool,

    /// Directory of Markdown files whose contents become posts.
    ///
    /// `None` disables the whole file-backed mode and leaves the web editor as
    /// the only way to author.
    pub content_dir: Option<PathBuf>,

    /// How often to re-scan the content directory for changes.
    pub sync_interval: Duration,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let bind: SocketAddr = env_or("TBLOGS_BIND", "127.0.0.1:8080")
            .parse()
            .context("TBLOGS_BIND must look like HOST:PORT, e.g. 127.0.0.1:8080")?;

        let admin_token = env::var("TBLOGS_ADMIN_TOKEN")
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());

        let page_size: i64 = env_or("TBLOGS_PAGE_SIZE", "10")
            .parse::<i64>()
            .context("TBLOGS_PAGE_SIZE must be a positive integer")?
            .max(1);

        // An empty value means "not configured" rather than "the current
        // directory", which would otherwise import whatever happens to be in
        // the working directory.
        let content_dir = env::var("TBLOGS_CONTENT_DIR")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);

        let sync_secs: u64 = env_or("TBLOGS_SYNC_INTERVAL", "5")
            .parse::<u64>()
            .context("TBLOGS_SYNC_INTERVAL must be a whole number of seconds")?;

        Ok(Self {
            bind,
            database_url: env_or("TBLOGS_DATABASE_URL", "sqlite://data/tblogs.db"),
            site_title: env_or("TBLOGS_SITE_TITLE", "我的博客"),
            site_description: env_or("TBLOGS_SITE_DESCRIPTION", "用 Rust 构建的博客"),
            site_url: env_or("TBLOGS_SITE_URL", "http://localhost:8080")
                .trim_end_matches('/')
                .to_string(),
            admin_token,
            page_size,
            seed_demo: env_flag("TBLOGS_SEED_DEMO"),
            content_dir,
            sync_interval: Duration::from_secs(sync_secs),
        })
    }

    /// Whether the admin surface is gated behind a token.
    pub fn admin_is_gated(&self) -> bool {
        self.admin_token.is_some()
    }
}

fn env_or(key: &str, default: &str) -> String {
    env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn env_flag(key: &str) -> bool {
    matches!(
        env::var(key)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}
