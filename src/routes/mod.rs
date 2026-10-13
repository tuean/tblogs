//! HTTP routing.

pub mod admin;
pub mod feed;
pub mod public;

use askama::Template;
use axum::{
    Router,
    response::Html,
    routing::{get, post},
};
use tower_http::trace::TraceLayer;

use crate::{error::AppError, state::AppState};

/// Render a template, turning a failure into a 500 page.
pub fn render<T: Template>(template: &T) -> Result<Html<String>, AppError> {
    Ok(Html(template.render()?))
}

/// Build the application router.
pub fn router(state: AppState) -> Router {
    Router::new()
        // Public surface.
        .route("/", get(public::index))
        .route("/posts/{slug}", get(public::post_detail))
        .route("/tags", get(public::tags_index))
        .route("/tags/{slug}", get(public::tag_detail))
        .route("/feed.xml", get(feed::rss))
        .route("/healthz", get(public::healthz))
        // The stylesheet is embedded in the binary at compile time, so there is
        // no asset tree to deploy and no build-machine path to resolve.
        .route("/static/style.css", get(public::stylesheet))
        // Images and other assets from the content directory. The wildcard is
        // required because a Markdown file may reference a nested folder such
        // as `assets/diagram/flow.png`.
        .route("/media/{*path}", get(public::media))
        // Admin surface. Every mutation is a POST because HTML forms cannot
        // issue PUT or DELETE.
        .route("/admin", get(admin::list))
        .route("/admin/login", get(admin::login_form).post(admin::login))
        .route("/admin/logout", post(admin::logout))
        .route("/admin/new", get(admin::new_form).post(admin::create))
        .route(
            "/admin/posts/{id}/edit",
            get(admin::edit_form).post(admin::update),
        )
        .route("/admin/posts/{id}/delete", post(admin::delete))
        .route("/admin/posts/{id}/publish", post(admin::toggle_publish))
        .route("/admin/sync", post(admin::resync))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
