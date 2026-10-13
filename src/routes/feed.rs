//! The RSS 2.0 feed.

use askama::Template;
use axum::{
    extract::State,
    http::header,
    response::{IntoResponse, Response},
};

use crate::{
    db,
    error::AppError,
    markdown,
    models::Post,
    state::AppState,
    templates::{FeedItem, FeedTemplate},
    util,
};

/// Number of entries in the feed.
const FEED_LIMIT: i64 = 20;

pub async fn rss(State(state): State<AppState>) -> Result<Response, AppError> {
    let snapshot = state.current().await;

    let posts = db::list_published(&snapshot.pool, FEED_LIMIT, 0).await?;

    let items = posts
        .into_iter()
        .map(|post| {
            let link = format!("{}{}", snapshot.site_url(), util::post_path(&post.slug));

            // Prefer the author's summary; fall back to the body when a post
            // was written without one.
            let description = if post.summary.trim().is_empty() {
                markdown::excerpt(&post.body_md, 200)
            } else {
                post.summary.clone()
            };

            FeedItem {
                title: post.title.clone(),
                pub_date: rfc2822(&post),
                link,
                description,
            }
        })
        .collect();

    let xml = FeedTemplate {
        site_title: snapshot.title(),
        site_description: snapshot.description(),
        site_url: snapshot.site_url().to_string(),
        last_build: chrono::Utc::now().to_rfc2822(),
        items,
    }
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

/// Publication time in RFC 2822, which RSS 2.0 requires.
fn rfc2822(post: &Post) -> String {
    let raw = post
        .published_at
        .as_deref()
        .unwrap_or(post.created_at.as_str());

    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.to_rfc2822())
        .unwrap_or_else(|_| chrono::Utc::now().to_rfc2822())
}
