//! Shared server state.

use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::RwLock;

use crate::{assets::Assets, models::Settings};

/// A consistent view of one published package.
///
/// The database handle and the settings it stores are swapped together, so a
/// request never reads a title from one package and posts from another.
pub struct Snapshot {
    pub pool: SqlitePool,
    pub settings: Settings,
}

impl Snapshot {
    pub fn title(&self) -> String {
        self.settings.site_title.clone()
    }

    pub fn description(&self) -> String {
        self.settings.site_description.clone()
    }

    pub fn site_url(&self) -> &str {
        &self.settings.site_url
    }
}

/// Handed to every handler through axum's `State` extractor.
#[derive(Clone)]
pub struct AppState {
    snapshot: Arc<RwLock<Arc<Snapshot>>>,
    /// Resolves local asset references against the published `media/` tree.
    pub assets: Assets,
    pub page_size: i64,
}

impl AppState {
    pub fn new(snapshot: Arc<Snapshot>, assets: Assets, page_size: i64) -> Self {
        Self {
            snapshot: Arc::new(RwLock::new(snapshot)),
            assets,
            page_size,
        }
    }

    /// The package currently being served.
    ///
    /// The lock is held only long enough to clone an `Arc`, so a reload in
    /// progress never stalls a request for more than that.
    pub async fn current(&self) -> Arc<Snapshot> {
        self.snapshot.read().await.clone()
    }

    /// Serve a different package from now on.
    async fn replace(&self, snapshot: Arc<Snapshot>) {
        *self.snapshot.write().await = snapshot;
    }

    /// Swap in a freshly loaded package. Used by the reload watcher.
    pub async fn install(&self, snapshot: Snapshot) {
        self.replace(Arc::new(snapshot)).await;
    }
}
