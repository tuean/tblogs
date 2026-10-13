//! Admin authentication.
//!
//! A deliberately small gate: a shared token compared against a cookie. This
//! is adequate for a locally bound instance, and it is *not* a substitute for
//! real sessions and HTTPS before the service is exposed publicly — see the
//! note in `deploy/README.md`.

use axum::{
    extract::FromRequestParts,
    http::{HeaderMap, header, request::Parts},
    response::{IntoResponse, Redirect, Response},
};

use crate::state::AppState;

/// Name of the cookie carrying the admin token.
pub const COOKIE_NAME: &str = "tblogs_admin";

/// Cookie lifetime: seven days.
const COOKIE_MAX_AGE_SECS: i64 = 60 * 60 * 24 * 7;

/// Extractor that enforces the admin gate on a handler.
///
/// When no token is configured the admin surface is open, which keeps local
/// development frictionless. When a token is configured, a request without a
/// matching cookie is redirected to the login form.
pub struct Admin;

impl FromRequestParts<AppState> for Admin {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let Some(expected) = state.cfg.admin_token.as_deref() else {
            return Ok(Admin);
        };

        match cookie_value(&parts.headers, COOKIE_NAME) {
            Some(presented) if verify_token(&presented, expected) => Ok(Admin),
            _ => Err(Redirect::to("/admin/login").into_response()),
        }
    }
}

/// Check a presented token against the configured one.
pub fn verify_token(presented: &str, expected: &str) -> bool {
    constant_time_eq(presented.as_bytes(), expected.as_bytes())
}

/// Read one cookie out of a request's `Cookie` header.
pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;

    raw.split(';').find_map(|pair| {
        let pair = pair.trim();
        let value = pair.strip_prefix(name)?.strip_prefix('=')?;
        Some(value.to_string())
    })
}

/// Build the `Set-Cookie` value that signs an admin in.
///
/// `Secure` is added automatically once the site URL is HTTPS, so the cookie
/// is never sent in the clear on a public deployment.
pub fn session_cookie(token: &str, secure: bool) -> String {
    format!(
        "{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={COOKIE_MAX_AGE_SECS}{}",
        if secure { "; Secure" } else { "" }
    )
}

/// Build the `Set-Cookie` value that signs an admin out.
pub fn clear_cookie(secure: bool) -> String {
    format!(
        "{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{}",
        if secure { "; Secure" } else { "" }
    )
}

/// Compare two byte strings without an early exit.
///
/// Guards the token check against a timing oracle.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut difference = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        difference |= x ^ y;
    }

    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_with_cookie(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, value.parse().unwrap());
        headers
    }

    #[test]
    fn finds_a_cookie_among_several() {
        let headers = headers_with_cookie("theme=dark; tblogs_admin=secret; other=1");
        assert_eq!(
            cookie_value(&headers, COOKIE_NAME).as_deref(),
            Some("secret")
        );
    }

    #[test]
    fn missing_cookie_is_none() {
        let headers = headers_with_cookie("theme=dark");
        assert!(cookie_value(&headers, COOKIE_NAME).is_none());
        assert!(cookie_value(&HeaderMap::new(), COOKIE_NAME).is_none());
    }

    #[test]
    fn constant_time_eq_matches_std_eq() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn secure_flag_tracks_https() {
        assert!(session_cookie("t", true).contains("; Secure"));
        assert!(!session_cookie("t", false).contains("Secure"));
        assert!(clear_cookie(false).contains("Max-Age=0"));
    }
}
