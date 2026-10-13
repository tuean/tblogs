//! Configuration for the serving side.
//!
//! Almost everything the server needs is baked into the content package by
//! `tblogs-admin build`, so a deployed instance is configured by pointing it at
//! a data directory. The values here exist to override that when needed, and
//! they all have sensible defaults so `tblogs-server` runs with no arguments.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use clap::Parser;

/// Name of the content database inside the data directory.
pub const DATABASE_FILE: &str = "site.db";

/// Name of the asset directory inside the data directory.
pub const MEDIA_DIR: &str = "media";

/// Command line for `tblogs-server`.
///
/// Every option also reads an environment variable, so the same knobs work from
/// a systemd unit without repeating them on the command line.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "tblogs-server",
    about = "Serve a tblogs content package over HTTP (read-only)",
    version
)]
pub struct ServerArgs {
    /// Address to listen on.
    ///
    /// Defaults to loopback: a freshly built server should not become reachable
    /// from the network until that is deliberate.
    #[arg(long, env = "TBLOGS_BIND", default_value = "127.0.0.1:8080")]
    pub bind: SocketAddr,

    /// Directory holding `site.db` and `media/`.
    #[arg(long, env = "TBLOGS_DATA_DIR", default_value = "./data")]
    pub data_dir: PathBuf,

    /// Posts per page on the index and tag pages.
    #[arg(long, env = "TBLOGS_PAGE_SIZE", default_value_t = 10)]
    pub page_size: i64,

    /// How often to check whether a new package has been published, in seconds.
    /// Zero disables reloading.
    #[arg(long, env = "TBLOGS_RELOAD_INTERVAL", default_value_t = 5)]
    pub reload_interval_secs: u64,

    /// Override the site title stored in the package.
    #[arg(long, env = "TBLOGS_SITE_TITLE")]
    pub site_title: Option<String>,

    /// Override the site description stored in the package.
    #[arg(long, env = "TBLOGS_SITE_DESCRIPTION")]
    pub site_description: Option<String>,

    /// Override the public base URL stored in the package.
    ///
    /// RSS links must be absolute, so this has to be right for the feed to work
    /// in a reader.
    #[arg(long, env = "TBLOGS_SITE_URL")]
    pub site_url: Option<String>,
}

/// Resolved server configuration.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub data_dir: PathBuf,
    pub page_size: i64,
    pub reload_interval: Duration,
    /// Overrides applied on top of whatever the package stored.
    pub site_title: Option<String>,
    pub site_description: Option<String>,
    pub site_url: Option<String>,
}

impl ServerConfig {
    pub fn from_args(args: ServerArgs) -> Self {
        Self {
            bind: args.bind,
            data_dir: args.data_dir,
            page_size: args.page_size.max(1),
            reload_interval: Duration::from_secs(args.reload_interval_secs),
            site_title: non_empty(args.site_title),
            site_description: non_empty(args.site_description),
            site_url: non_empty(args.site_url).map(|url| url.trim_end_matches('/').to_string()),
        }
    }

    /// `site.db` inside the data directory.
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join(DATABASE_FILE)
    }

    /// `media/` inside the data directory.
    pub fn media_path(&self) -> PathBuf {
        self.data_dir.join(MEDIA_DIR)
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Whether a path looks like a data directory produced by `tblogs-admin build`.
pub fn describe_missing_data_dir(data_dir: &Path, database: &Path) -> String {
    format!(
        "no content package at {} — expected {} and a {}/ directory next to it.\n\
         Build and publish one with:\n  \
         tblogs-admin build --content-dir <your markdown folder> --out dist\n  \
         tblogs-admin publish --to user@host:{}",
        data_dir.display(),
        database.display(),
        MEDIA_DIR,
        data_dir.display()
    )
}
