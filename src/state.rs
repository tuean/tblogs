//! Shared application state.

use std::sync::Arc;

use sqlx::SqlitePool;

use crate::config::Config;

/// Handed to every handler through axum's `State` extractor.
#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub cfg: Arc<Config>,
}

impl AppState {
    pub fn new(pool: SqlitePool, cfg: Config) -> Self {
        Self {
            pool,
            cfg: Arc::new(cfg),
        }
    }
}
