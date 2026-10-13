//! Slug generation.
//!
//! A naive ASCII slugifier turns a Chinese title into an empty string, which
//! makes most posts unreachable. This implementation keeps non-ASCII letters,
//! so `我的第一篇博客` becomes the slug `我的第一篇博客`. Browsers percent-encode
//! those bytes when building a URL and axum decodes them again before matching,
//! so the resulting links work end to end.

/// Upper bound on slug length, counted in characters.
const MAX_SLUG_CHARS: usize = 80;

/// Convert a title into a URL-safe slug.
///
/// Runs of whitespace and punctuation collapse into a single `-`, leading and
/// trailing separators are removed, and ASCII letters are lowercased.
pub fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut separator_pending = false;

    for ch in input.chars() {
        if ch.is_alphanumeric() {
            if separator_pending && !out.is_empty() {
                out.push('-');
            }
            separator_pending = false;
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
        } else {
            separator_pending = true;
        }

        if out.chars().count() >= MAX_SLUG_CHARS {
            return out.trim_matches('-').to_string();
        }
    }

    out.trim_matches('-').to_string()
}

/// Build a guaranteed non-empty slug, falling back to `fallback` when the
/// title contains no characters that survive slugification (e.g. `"..."`).
pub fn slugify_or(input: &str, fallback: &str) -> String {
    let slug = slugify(input);
    if slug.is_empty() {
        format!("post-{fallback}")
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_titles_are_lowercased_and_dashed() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
        assert_eq!(slugify("  Spaced   Out  "), "spaced-out");
        assert_eq!(slugify("Rust & axum: a blog"), "rust-axum-a-blog");
    }

    #[test]
    fn cjk_titles_survive() {
        assert_eq!(slugify("我的第一篇博客"), "我的第一篇博客");
        assert_eq!(slugify("Rust 写博客"), "rust-写博客");
    }

    #[test]
    fn punctuation_only_titles_fall_back() {
        assert_eq!(slugify("!!!"), "");
        assert_eq!(slugify_or("!!!", "1700000000"), "post-1700000000");
    }

    #[test]
    fn slug_length_is_capped() {
        let long = "a".repeat(500);
        assert_eq!(slugify(&long).chars().count(), MAX_SLUG_CHARS);
    }
}
