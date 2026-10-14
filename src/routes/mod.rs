//! HTTP routing.
//!
//! The server exposes a public, read-only surface. Authoring happens locally in
//! `tblogs-admin`, so there are no write routes and nothing to authenticate.

pub mod feed;
pub mod public;

use askama::Template;
use axum::{Router, response::Html, routing::get};
use tower_http::trace::TraceLayer;

use crate::{error::AppError, state::AppState};

/// Render a template, turning a failure into a 500 page.
pub fn render<T: Template>(template: &T) -> Result<Html<String>, AppError> {
    Ok(Html(template.render()?))
}

/// Build the application router.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(public::index))
        .route("/posts/{slug}", get(public::post_detail))
        .route("/tags", get(public::tags_index))
        .route("/tags/{slug}", get(public::tag_detail))
        .route("/feed.xml", get(feed::rss))
        .route("/healthz", get(public::healthz))
        // Embedded in the binary at compile time: there is no asset tree to
        // deploy and no build-machine path to resolve.
        .route("/static/style.css", get(public::stylesheet))
        // Icons, embedded the same way. Browsers ask for /favicon.ico and
        // /apple-touch-icon.png on their own, so those two paths are fixed.
        .route("/favicon.ico", get(public::favicon_ico))
        .route("/favicon.svg", get(public::favicon_svg))
        .route("/favicon-32.png", get(public::favicon_png))
        .route("/apple-touch-icon.png", get(public::apple_touch_icon))
        // Images and other assets from the published package. The wildcard is
        // needed because a document may reference a nested folder such as
        // `assets/diagram/flow.png`.
        .route("/media/{*path}", get(public::media))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
