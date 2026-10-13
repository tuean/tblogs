//! Shared application state.

use std::sync::Arc;

use sqlx::SqlitePool;

use crate::{assets::Assets, config::Config};

/// Handed to every handler through axum's `State` extractor.
#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub cfg: Arc<Config>,
    /// Resolves local asset references, present only when the content
    /// directory is configured.
    pub assets: Option<Assets>,
}

impl AppState {
    pub fn new(pool: SqlitePool, cfg: Config, assets: Option<Assets>) -> Self {
        Self {
            pool,
            cfg: Arc::new(cfg),
            assets,
        }
    }
}
