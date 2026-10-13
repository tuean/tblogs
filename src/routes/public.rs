//! The public, read-only surface of the blog.

use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use serde::Deserialize;

use crate::{
    assets::Assets,
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
    let snapshot = state.current().await;

    let total = db::count_published(&snapshot.pool).await?;
    let page = paginate(query.page, total, state.page_size, "/");

    let posts = db::list_published(&snapshot.pool, state.page_size, page.offset).await?;
    let tag_map = db::tags_by_post(&snapshot.pool).await?;

    let posts = posts
        .into_iter()
        .map(|post| to_view(post, &tag_map, false, &state.assets))
        .collect();

    super::render(&IndexTemplate {
        site_title: snapshot.title(),
        site_description: snapshot.description(),
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
    let snapshot = state.current().await;

    let post = db::find_published_by_slug(&snapshot.pool, &slug)
        .await?
        .ok_or_else(|| AppError::not_found(format!("文章「{slug}」不存在")))?;

    let tag_map = db::tags_by_post(&snapshot.pool).await?;

    super::render(&PostTemplate {
        site_title: snapshot.title(),
        site_description: snapshot.description(),
        post: to_view(post, &tag_map, true, &state.assets),
    })
}

/// The tag cloud.
pub async fn tags_index(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let snapshot = state.current().await;

    let tags = db::list_tags_with_counts(&snapshot.pool).await?;

    super::render(&TagsTemplate {
        site_title: snapshot.title(),
        site_description: snapshot.description(),
        tags,
    })
}

/// Published posts carrying one tag.
pub async fn tag_detail(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<PageQuery>,
) -> Result<Html<String>, AppError> {
    let snapshot = state.current().await;

    let tag = db::find_tag_by_slug(&snapshot.pool, &slug)
        .await?
        .ok_or_else(|| AppError::not_found(format!("标签「{slug}」不存在")))?;

    let total = db::count_published_by_tag(&snapshot.pool, &tag.slug).await?;
    let page = paginate(
        query.page,
        total,
        state.page_size,
        &util::tag_path(&tag.slug),
    );

    let posts =
        db::list_published_by_tag(&snapshot.pool, &tag.slug, state.page_size, page.offset).await?;
    let tag_map = db::tags_by_post(&snapshot.pool).await?;

    let posts = posts
        .into_iter()
        .map(|post| to_view(post, &tag_map, false, &state.assets))
        .collect();

    super::render(&TagTemplate {
        site_title: snapshot.title(),
        site_description: snapshot.description(),
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

/// Liveness probe that also touches the database, so it reports real readiness
/// rather than merely "the process is up".
pub async fn healthz(State(state): State<AppState>) -> Result<&'static str, AppError> {
    let snapshot = state.current().await;
    db::count_published(&snapshot.pool).await?;
    Ok("ok")
}

/// The stylesheet.
///
/// Embedded at compile time rather than served from disk, which keeps a
/// deployment to a binary and a data directory. The templates are compiled in
/// by askama for the same reason.
pub async fn stylesheet() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/static/style.css")),
    )
}

/// Serve an image or other asset out of the published package.
///
/// Reachable only through [`Assets::path_for`], which rejects traversal, symlink
/// escapes, dotfiles and any extension outside the allowlist. The published
/// layout mirrors the author's directory, so the `/media/...` URLs written at
/// build time resolve exactly as they did locally.
pub async fn media(
    State(state): State<AppState>,
    Path(path): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(file) = state.assets.path_for(&path) else {
        return Err(AppError::not_found("资源不存在"));
    };

    let Some(content_type) = crate::assets::content_type(&file) else {
        return Err(AppError::not_found("资源不存在"));
    };

    let metadata = tokio::fs::metadata(&file)
        .await
        .map_err(|error| AppError::internal("读取资源失败", error))?;

    // A cheap validator, so a browser can revalidate a replaced image instead of
    // trusting a long max-age.
    let stamp = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|delta| delta.as_nanos())
        .unwrap_or(0);
    let etag = format!("\"{:x}-{:x}\"", metadata.len(), stamp);

    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        == Some(etag.as_str())
    {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response());
    }

    let bytes = tokio::fs::read(&file)
        .await
        .map_err(|error| AppError::internal("读取资源失败", error))?;

    Ok((
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (header::CACHE_CONTROL, "public, max-age=600".to_string()),
            (header::ETAG, etag),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            // An SVG is a document that can carry script. Serving content from
            // the site's own origin would let it run there, so scripts are
            // sandboxed away.
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; sandbox".to_string(),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// Build a render-ready post.
///
/// `with_body` controls whether the Markdown body is rendered. List pages show
/// only the stored summary, so rendering every body there would be wasted work;
/// those views carry an empty `body_html` that their templates never reference.
///
/// Local image references are resolved against the post's own source path, which
/// is why the published asset layout mirrors the author's directory.
fn to_view(
    post: Post,
    tags: &HashMap<i64, Vec<Tag>>,
    with_body: bool,
    assets: &Assets,
) -> PostView {
    let date = post.display_date();

    let body_html = if with_body {
        markdown::render_with_assets(&post.body_md, assets, &post.source_path)
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
