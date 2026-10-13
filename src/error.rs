//! The error type returned by handlers.

use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};

/// A handler error that renders as a small self-contained HTML page.
///
/// `message` is what the visitor sees; `detail` carries the underlying error
/// chain and only ever reaches the log, so database errors never leak out.
#[derive(Debug)]
pub struct AppError {
    status: StatusCode,
    message: String,
    detail: Option<String>,
}

impl AppError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            detail: None,
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    /// The request conflicts with how the resource is managed — for example
    /// editing a post that is owned by a file on disk.
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    /// Wrap an internal failure, keeping its chain for the log only.
    pub fn internal(message: impl Into<String>, error: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
            detail: Some(error.to_string()),
        }
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.status, self.message)
    }
}

impl std::error::Error for AppError {}

impl From<sqlx::Error> for AppError {
    fn from(err: sqlx::Error) -> Self {
        Self::internal("数据库操作失败", err)
    }
}

impl From<askama::Error> for AppError {
    fn from(err: askama::Error) -> Self {
        Self::internal("页面渲染失败", err)
    }
}

impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        Self::internal("服务器内部错误", format!("{err:#}"))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        if self.status.is_server_error() {
            tracing::error!(
                status = self.status.as_u16(),
                message = %self.message,
                detail = self.detail.as_deref().unwrap_or("-"),
                "request failed"
            );
        } else {
            tracing::debug!(
                status = self.status.as_u16(),
                message = %self.message,
                "request rejected"
            );
        }

        let body = error_page(self.status, &self.message);
        (self.status, Html(body)).into_response()
    }
}

/// Render the standalone error page.
///
/// It deliberately does not use the askama templates: an error must stay
/// renderable even when the template layer is what failed.
fn error_page(status: StatusCode, message: &str) -> String {
    let hint = if status == StatusCode::UNAUTHORIZED {
        r#"<p><a class="btn" href="/admin/login">前往登录</a></p>"#
    } else {
        ""
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{code} · {reason}</title>
<link rel="stylesheet" href="/static/style.css">
</head>
<body>
<main class="wrap" style="padding-top:4rem">
  <h1>{code} {reason}</h1>
  <p class="post-meta">{message}</p>
  {hint}
  <p><a href="/">返回首页</a></p>
</main>
</body>
</html>"#,
        code = status.as_u16(),
        reason = status.canonical_reason().unwrap_or("Error"),
        message = escape_html(message),
        hint = hint,
    )
}

/// Minimal HTML escaping for text interpolated into the error page.
fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_html_metacharacters() {
        assert_eq!(
            escape_html(r#"<script>"x" & 'y'"#),
            "&lt;script&gt;&quot;x&quot; &amp; &#39;y&#39;"
        );
    }

    #[test]
    fn error_page_includes_status_and_message() {
        let page = error_page(StatusCode::NOT_FOUND, "文章不存在");
        assert!(page.contains("404"));
        assert!(page.contains("文章不存在"));
    }

    #[test]
    fn unauthorized_page_links_to_login() {
        let page = error_page(StatusCode::UNAUTHORIZED, "需要登录");
        assert!(page.contains("/admin/login"));
    }
}
