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
    let total = db::count_published(&state.pool).await?;
    let page = paginate(query.page, total, state.cfg.page_size, "/");

    let posts = db::list_published(&state.pool, state.cfg.page_size, page.offset).await?;
    let tag_map = db::tags_by_post(&state.pool).await?;

    let posts = posts
        .into_iter()
        .map(|post| to_view(post, &tag_map, false, None))
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
        post: to_view(post, &tag_map, true, state.assets.as_ref()),
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
        .map(|post| to_view(post, &tag_map, false, None))
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

/// Serve an image or other asset out of the content directory.
///
/// Reachable only through [`Assets::path_for`], which rejects traversal, symlink
/// escapes, dotfiles and any extension outside the allowlist — so the Markdown
/// sources sitting in the same tree stay private.
pub async fn media(
    State(state): State<AppState>,
    Path(path): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(assets) = state.assets.as_ref() else {
        return Err(AppError::not_found("本站未配置内容目录，图片资源不可用"));
    };

    let Some(file) = assets.path_for(&path) else {
        return Err(AppError::not_found("资源不存在"));
    };

    let Some(content_type) = crate::assets::content_type(&file) else {
        return Err(AppError::not_found("资源不存在"));
    };

    let metadata = tokio::fs::metadata(&file)
        .await
        .map_err(|error| AppError::internal("读取资源失败", error))?;

    // A cheap validator, so a browser can revalidate an edited image instead of
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
            // the author's own directory under the site's origin would let it
            // run there, so scripts are sandboxed away.
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
/// only the stored summary, so rendering every body there would be wasted
/// work; those views carry an empty `body_html` that their templates never
/// reference.
///
/// When `assets` is present and the post came from a file, local image
/// references are repointed at `/media/...` so they resolve from the article
/// URL rather than from the directory the file happens to live in.
fn to_view(
    post: Post,
    tags: &HashMap<i64, Vec<Tag>>,
    with_body: bool,
    assets: Option<&Assets>,
) -> PostView {
    let date = post.display_date();

    let body_html = if !with_body {
        String::new()
    } else {
        match (assets, post.source_path.as_deref()) {
            (Some(assets), Some(source_rel)) => {
                markdown::render_with_assets(&post.body_md, assets, source_rel)
            }
            _ => markdown::render(&post.body_md),
        }
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
