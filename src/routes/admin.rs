//! The admin surface: list, create, edit, publish, delete.
//!
//! Every mutation is a `POST` and is guarded by the [`Admin`] extractor, which
//! redirects to the login form when a token is configured and not presented.

use axum::{
    extract::{Form, Path, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
};
use serde::Deserialize;

use crate::{
    auth, db,
    error::AppError,
    models::{PostForm, PostFormView},
    state::AppState,
    templates::{AdminFormTemplate, AdminListTemplate, AdminLoginTemplate, AdminPostRow},
};

/// The login form body.
#[derive(Debug, Deserialize)]
pub struct LoginForm {
    #[serde(default)]
    pub token: String,
}

/// All posts, drafts included.
pub async fn list(_: auth::Admin, State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let posts = db::list_all(&state.pool).await?;
    let tag_map = db::tags_by_post(&state.pool).await?;

    let rows = posts
        .into_iter()
        .map(|post| {
            let date = post.display_date();
            let tags = tag_map.get(&post.id).cloned().unwrap_or_default();
            AdminPostRow {
                id: post.id,
                title: post.title,
                slug: post.slug,
                published: post.published,
                date,
                tags,
            }
        })
        .collect();

    super::render(&AdminListTemplate {
        site_title: state.cfg.site_title.clone(),
        site_description: state.cfg.site_description.clone(),
        posts: rows,
        gated: state.cfg.admin_is_gated(),
    })
}

/// Blank create form.
pub async fn new_form(
    _: auth::Admin,
    State(state): State<AppState>,
) -> Result<Html<String>, AppError> {
    super::render(&AdminFormTemplate {
        site_title: state.cfg.site_title.clone(),
        site_description: state.cfg.site_description.clone(),
        form: PostFormView::new_post(),
        is_new: true,
        action: "/admin/new".to_string(),
        error: String::new(),
    })
}

/// Create a post.
pub async fn create(
    _: auth::Admin,
    State(state): State<AppState>,
    Form(form): Form<PostForm>,
) -> Result<Response, AppError> {
    if form.title.trim().is_empty() {
        // Re-render with the submitted content so nothing the author typed is
        // lost to a validation error.
        return Ok(super::render(&AdminFormTemplate {
            site_title: state.cfg.site_title.clone(),
            site_description: state.cfg.site_description.clone(),
            form: form_view(&form),
            is_new: true,
            action: "/admin/new".to_string(),
            error: "标题不能为空。".to_string(),
        })?
        .into_response());
    }

    db::create_post(&state.pool, &form).await?;
    Ok(Redirect::to("/admin").into_response())
}

/// Edit form for an existing post.
pub async fn edit_form(
    _: auth::Admin,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Html<String>, AppError> {
    let post = db::find_by_id(&state.pool, id)
        .await?
        .ok_or_else(|| AppError::not_found("文章不存在"))?;

    let tag_map = db::tags_by_post(&state.pool).await?;
    let tags = tag_map
        .get(&post.id)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|tag| tag.name)
        .collect::<Vec<_>>()
        .join(", ");

    super::render(&AdminFormTemplate {
        site_title: state.cfg.site_title.clone(),
        site_description: state.cfg.site_description.clone(),
        form: PostFormView {
            slug: post.slug,
            title: post.title,
            summary: post.summary,
            body_md: post.body_md,
            published: post.published,
            tags,
        },
        is_new: false,
        action: format!("/admin/posts/{id}/edit"),
        error: String::new(),
    })
}

/// Save edits to an existing post.
pub async fn update(
    _: auth::Admin,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Form(form): Form<PostForm>,
) -> Result<Response, AppError> {
    // Check existence up front so a stale form reports 404 rather than
    // surfacing as a generic failure from the update itself.
    db::find_by_id(&state.pool, id)
        .await?
        .ok_or_else(|| AppError::not_found("文章不存在"))?;

    if form.title.trim().is_empty() {
        return Ok(super::render(&AdminFormTemplate {
            site_title: state.cfg.site_title.clone(),
            site_description: state.cfg.site_description.clone(),
            form: form_view(&form),
            is_new: false,
            action: format!("/admin/posts/{id}/edit"),
            error: "标题不能为空。".to_string(),
        })?
        .into_response());
    }

    db::update_post(&state.pool, id, &form).await?;
    Ok(Redirect::to("/admin").into_response())
}

/// Flip a post between draft and published.
pub async fn toggle_publish(
    _: auth::Admin,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let post = db::find_by_id(&state.pool, id)
        .await?
        .ok_or_else(|| AppError::not_found("文章不存在"))?;

    db::set_published(&state.pool, id, !post.published).await?;
    Ok(Redirect::to("/admin").into_response())
}

/// Delete a post.
pub async fn delete(
    _: auth::Admin,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    db::delete_post(&state.pool, id).await?;
    Ok(Redirect::to("/admin").into_response())
}

/// Login form.
pub async fn login_form(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    super::render(&AdminLoginTemplate {
        site_title: state.cfg.site_title.clone(),
        site_description: state.cfg.site_description.clone(),
        error: String::new(),
    })
}

/// Exchange the admin token for a session cookie.
pub async fn login(
    State(state): State<AppState>,
    Form(form): Form<LoginForm>,
) -> Result<Response, AppError> {
    let Some(expected) = state.cfg.admin_token.as_deref() else {
        // No token configured means the admin surface is already open.
        return Ok(Redirect::to("/admin").into_response());
    };

    if !auth::verify_token(form.token.trim(), expected) {
        tracing::warn!("failed admin login attempt");
        return Ok((
            StatusCode::UNAUTHORIZED,
            super::render(&AdminLoginTemplate {
                site_title: state.cfg.site_title.clone(),
                site_description: state.cfg.site_description.clone(),
                error: "令牌不正确。".to_string(),
            })?,
        )
            .into_response());
    }

    let cookie = auth::session_cookie(expected, state.cfg.site_url.starts_with("https://"));
    Ok(([(header::SET_COOKIE, cookie)], Redirect::to("/admin")).into_response())
}

/// Clear the session cookie.
pub async fn logout(State(state): State<AppState>) -> Result<Response, AppError> {
    let cookie = auth::clear_cookie(state.cfg.site_url.starts_with("https://"));
    Ok(([(header::SET_COOKIE, cookie)], Redirect::to("/")).into_response())
}

/// Mirror a submitted form back into the edit form.
fn form_view(form: &PostForm) -> PostFormView {
    PostFormView {
        slug: form.slug.clone(),
        title: form.title.clone(),
        summary: form.summary.clone(),
        body_md: form.body_md.clone(),
        published: form.is_publish(),
        tags: form.tags.clone(),
    }
}
