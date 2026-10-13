//! The RSS 2.0 feed.

use askama::Template;
use axum::{
    extract::State,
    http::header,
    response::{IntoResponse, Response},
};

use crate::{
    error::AppError,
    pages::{LinkStyle, Site},
    state::AppState,
};

pub async fn rss(State(state): State<AppState>) -> Result<Response, AppError> {
    let snapshot = state.current().await;

    let xml = Site {
        pool: &snapshot.pool,
        assets: &state.assets,
        settings: &snapshot.settings,
        page_size: state.page_size,
        links: LinkStyle::Query,
    }
    .feed()
    .await?
    .render()?;

    Ok((
        [
            (header::CONTENT_TYPE, "application/rss+xml; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=600"),
        ],
        xml,
    )
        .into_response())
}
