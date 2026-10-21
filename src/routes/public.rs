//! The public, read-only surface of the blog.
//!
//! These handlers only choose a page and turn it into a response. The page
//! building itself lives in `pages`, shared with the static exporter so the two
//! cannot render the site differently.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use serde::Deserialize;

use crate::{
    db,
    error::AppError,
    pages::{LinkStyle, Site},
    state::{AppState, Snapshot},
};

/// `?page=N`
#[derive(Debug, Deserialize)]
pub struct PageQuery {
    #[serde(default)]
    pub page: Option<i64>,
}

/// Borrow the current package as a page builder.
///
/// The live server paginates through a query string; the static exporter puts
/// the page number in the path instead.
fn site<'a>(state: &'a AppState, snapshot: &'a Snapshot) -> Site<'a> {
    Site {
        pool: &snapshot.pool,
        assets: &state.assets,
        settings: &snapshot.settings,
        page_size: state.page_size,
        links: LinkStyle::Query,
    }
}

/// Home page: published posts, newest first.
pub async fn index(
    State(state): State<AppState>,
    Query(query): Query<PageQuery>,
) -> Result<Html<String>, AppError> {
    let snapshot = state.current().await;
    let page = site(&state, &snapshot).home(query.page).await?;

    super::render(&page)
}

/// A single published post.
pub async fn post_detail(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Html<String>, AppError> {
    let snapshot = state.current().await;

    let page = site(&state, &snapshot)
        .post(&slug)
        .await?
        .ok_or_else(|| AppError::not_found(format!("文章「{slug}」不存在")))?;

    super::render(&page)
}

/// The list of series.
pub async fn series_index(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let snapshot = state.current().await;
    let page = site(&state, &snapshot).series_index().await?;

    super::render(&page)
}

/// One series, with its chapters in reading order.
pub async fn series_detail(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Html<String>, AppError> {
    let snapshot = state.current().await;

    let page = site(&state, &snapshot)
        .series(&slug)
        .await?
        .ok_or_else(|| AppError::not_found(format!("小说「{slug}」不存在")))?;

    super::render(&page)
}

/// The tag cloud.
pub async fn tags_index(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let snapshot = state.current().await;
    let page = site(&state, &snapshot).tags().await?;

    super::render(&page)
}

/// Published posts carrying one tag.
pub async fn tag_detail(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<PageQuery>,
) -> Result<Html<String>, AppError> {
    let snapshot = state.current().await;

    let page = site(&state, &snapshot)
        .tag(&slug, query.page)
        .await?
        .ok_or_else(|| AppError::not_found(format!("标签「{slug}」不存在")))?;

    super::render(&page)
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
        crate::assets::STYLESHEET,
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

/// One embedded icon, with a long cache since the bytes only change with a
/// release.
fn icon(bytes: &'static [u8], content_type: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        bytes,
    )
        .into_response()
}

/// Requested automatically by browsers that do not read the `<link>` tags.
pub async fn favicon_ico() -> Response {
    icon(crate::assets::FAVICON_ICO, "image/x-icon")
}

/// The primary icon: a vector, so it stays sharp at every size.
pub async fn favicon_svg() -> Response {
    icon(crate::assets::FAVICON_SVG, "image/svg+xml")
}

/// Fallback for browsers without SVG favicon support.
pub async fn favicon_png() -> Response {
    icon(crate::assets::FAVICON_PNG, "image/png")
}

/// Home-screen icon on iOS, which fetches this path by convention.
pub async fn apple_touch_icon() -> Response {
    icon(crate::assets::APPLE_TOUCH_ICON, "image/png")
}
