//! The public, read-only surface of the blog.

use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::header,
    response::{Html, IntoResponse},
};
use serde::Deserialize;

use crate::{
    db,
    error::AppError,
    markdown,
    models::{Post, PostView, Tag},
    state::AppState,
    templates::{IndexTemplate, PostTemplate, TagTemplate, TagsTemplate},
    util::{self, paginate},
};

/// `?page=N`
#[derive(Debug, Deserialize)]
pub struct PageQuery {
    #[serde(default)]
    pub page: Option<i64>,
}

/// Home page: published posts, newest first.
pub async fn index(
    State(state): State<AppState>,
    Query(query): Query<PageQuery>,
) -> Result<Html<String>, AppError> {
    let total = db::count_published(&state.pool).await?;
    let page = paginate(query.page, total, state.cfg.page_size, "/");

    let posts = db::list_published(&state.pool, state.cfg.page_size, page.offset).await?;
    let tag_map = db::tags_by_post(&state.pool).await?;

    let posts = posts
        .into_iter()
        .map(|post| to_view(post, &tag_map, false))
        .collect();

    super::render(&IndexTemplate {
        site_title: state.cfg.site_title.clone(),
        site_description: state.cfg.site_description.clone(),
        posts,
        page: page.page,
        total_pages: page.total_pages,
        has_prev: page.has_prev,
        has_next: page.has_next,
        prev_href: page.prev_href,
        next_href: page.next_href,
    })
}

/// A single published post.
pub async fn post_detail(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Html<String>, AppError> {
    let post = db::find_published_by_slug(&state.pool, &slug)
        .await?
        .ok_or_else(|| AppError::not_found(format!("文章「{slug}」不存在或尚未发布")))?;

    let tag_map = db::tags_by_post(&state.pool).await?;

    super::render(&PostTemplate {
        site_title: state.cfg.site_title.clone(),
        site_description: state.cfg.site_description.clone(),
        post: to_view(post, &tag_map, true),
    })
}

/// The tag cloud.
pub async fn tags_index(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let tags = db::list_tags_with_counts(&state.pool).await?;

    super::render(&TagsTemplate {
        site_title: state.cfg.site_title.clone(),
        site_description: state.cfg.site_description.clone(),
        tags,
    })
}

/// Published posts carrying one tag.
pub async fn tag_detail(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<PageQuery>,
) -> Result<Html<String>, AppError> {
    let tag = db::find_tag_by_slug(&state.pool, &slug)
        .await?
        .ok_or_else(|| AppError::not_found(format!("标签「{slug}」不存在")))?;

    let total = db::count_published_by_tag(&state.pool, &tag.slug).await?;
    let page = paginate(
        query.page,
        total,
        state.cfg.page_size,
        &util::tag_path(&tag.slug),
    );

    let posts =
        db::list_published_by_tag(&state.pool, &tag.slug, state.cfg.page_size, page.offset).await?;
    let tag_map = db::tags_by_post(&state.pool).await?;

    let posts = posts
        .into_iter()
        .map(|post| to_view(post, &tag_map, false))
        .collect();

    super::render(&TagTemplate {
        site_title: state.cfg.site_title.clone(),
        site_description: state.cfg.site_description.clone(),
        tag_name: tag.name,
        posts,
        page: page.page,
        total_pages: page.total_pages,
        has_prev: page.has_prev,
        has_next: page.has_next,
        prev_href: page.prev_href,
        next_href: page.next_href,
    })
}

/// Liveness probe that also touches the database, so it reports real
/// readiness rather than merely "the process is up".
pub async fn healthz(State(state): State<AppState>) -> Result<&'static str, AppError> {
    db::count_published(&state.pool).await?;
    Ok("ok")
}

/// The stylesheet.
///
/// It is embedded at compile time rather than served from disk, which keeps the
/// deployment to a single binary plus the migrations directory. The templates
/// are compiled in by askama for the same reason.
pub async fn stylesheet() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/static/style.css")),
    )
}

/// Build a render-ready post.
///
/// `with_body` controls whether the Markdown body is rendered. List pages show
/// only the stored summary, so rendering every body there would be wasted
/// work; those views carry an empty `body_html` that their templates never
/// reference.
fn to_view(post: Post, tags: &HashMap<i64, Vec<Tag>>, with_body: bool) -> PostView {
    let date = post.display_date();
    let body_html = if with_body {
        markdown::render(&post.body_md)
    } else {
        String::new()
    };
    let tags = tags.get(&post.id).cloned().unwrap_or_default();

    PostView {
        slug: post.slug,
        title: post.title,
        summary: post.summary,
        body_html,
        date,
        tags,
    }
}
