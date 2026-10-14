//! Resolving local asset references for the content directory.
//!
//! When articles come from a directory of Markdown files written in an editor
//! such as Typora, images are referenced *relative to the document*:
//!
//! ```markdown
//! ![screenshot](assets/paste-20260101.png)
//! ```
//!
//! That path only makes sense next to the file on disk. On the web the article
//! lives at `/posts/{slug}`, so a browser would resolve `assets/...` against
//! `/posts/` and get a 404. This module rewrites each local reference into an
//! absolute `/media/...` URL that the server maps back onto the content
//! directory, and it does the reverse mapping for incoming media requests.
//!
//! References that are already remote (`https:`, `data:`, …) or that point at a
//! fragment are left untouched, so PicGo-style uploads and inline base64 images
//! keep working with no configuration.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Extensions that may be served from the content directory, with their types.
///
/// An allowlist on purpose: the media route maps onto a directory that also
/// holds the Markdown sources, and those must not become downloadable. `.md` is
/// the important omission.
const ALLOWED_TYPES: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("avif", "image/avif"),
    ("bmp", "image/bmp"),
    ("ico", "image/x-icon"),
    ("svg", "image/svg+xml"),
    ("mp4", "video/mp4"),
    ("webm", "video/webm"),
    ("mov", "video/quicktime"),
    ("mp3", "audio/mpeg"),
    ("m4a", "audio/mp4"),
    ("wav", "audio/wav"),
    ("pdf", "application/pdf"),
    ("zip", "application/zip"),
    ("txt", "text/plain; charset=utf-8"),
];

/// Public URL prefix under which content assets are served.
pub const MEDIA_PREFIX: &str = "/media/";

/// A canonicalised content directory.
#[derive(Debug, Clone)]
pub struct Assets {
    root: PathBuf,
}

impl Assets {
    /// Canonicalise the content directory so later path checks are reliable.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let root = root
            .canonicalize()
            .with_context(|| format!("content directory not found: {}", root.display()))?;

        if !root.is_dir() {
            anyhow::bail!("content path is not a directory: {}", root.display());
        }

        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Public URL for a reference found in the document at `source_rel`.
    ///
    /// Returns `None` when the reference should be left exactly as written —
    /// remote URLs, fragments, and anything that would escape the content
    /// directory.
    pub fn url_for(&self, reference: &str, source_rel: &str) -> Option<String> {
        let reference = reference.trim();
        if reference.is_empty() || is_external(reference) {
            return None;
        }

        // Keep any fragment (used by links, harmless on images).
        let (raw_path, fragment) = match reference.split_once('#') {
            Some((path, fragment)) => (path, Some(fragment)),
            None => (reference, None),
        };
        if raw_path.is_empty() {
            return None;
        }

        let decoded = percent_decode(raw_path);

        let relative = if decoded.starts_with('/') {
            // Either an absolute filesystem path (Typora's "image root path"
            // mode writes these) or a genuinely site-absolute URL. Only rewrite
            // when it really is inside the content directory.
            let as_path = Path::new(&decoded);

            // Resolve symlinks first. On macOS `/var` and `/home` are symlinks,
            // so a purely textual comparison against the canonical root fails
            // even when the file genuinely is inside it.
            let resolved = as_path.canonicalize().ok();

            let inside = resolved
                .as_deref()
                .and_then(|path| path.strip_prefix(&self.root).ok())
                .map(slash_path)
                .or_else(|| as_path.strip_prefix(&self.root).ok().map(slash_path))
                .or_else(|| {
                    // A site-absolute path that happens to name a file under
                    // the content root, e.g. `/assets/a.png`.
                    let candidate = decoded.trim_start_matches('/');
                    self.root
                        .join(candidate)
                        .is_file()
                        .then(|| candidate.to_string())
                });

            inside?
        } else {
            // Relative to the directory holding the document.
            let base = Path::new(source_rel)
                .parent()
                .map(slash_path)
                .unwrap_or_default();
            normalise(&format!("{base}/{decoded}"))
        };

        if relative.is_empty() || relative.starts_with("..") {
            return None;
        }

        let mut url = String::from(MEDIA_PREFIX);
        url.push_str(&encode_path(&relative));
        if let Some(fragment) = fragment {
            url.push('#');
            url.push_str(fragment);
        }

        Some(url)
    }

    /// Filesystem path for an incoming `/media/...` request.
    ///
    /// Returns `None` for anything that escapes the content directory, names a
    /// dotfile, or is not an allowlisted asset type.
    pub fn path_for(&self, relative: &str) -> Option<PathBuf> {
        let decoded = percent_decode(relative);

        // Reject traversal and dotfiles before touching the filesystem.
        if decoded
            .split('/')
            .any(|part| part == ".." || part.starts_with('.'))
        {
            return None;
        }

        let candidate = self.root.join(decoded.trim_start_matches('/'));
        let canonical = candidate.canonicalize().ok()?;

        // canonicalize resolves symlinks, so this is the check that actually
        // stops a link inside the content directory from exposing /etc.
        if !canonical.starts_with(&self.root) || !canonical.is_file() {
            return None;
        }

        content_type(&canonical).map(|_| canonical)
    }
}

/// MIME type for a path, or `None` when the extension is not allowlisted.
pub fn content_type(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    ALLOWED_TYPES
        .iter()
        .find(|(ext, _)| *ext == extension)
        .map(|(_, mime)| *mime)
}

/// Whether a reference is a remote URL, a data URI, or a bare fragment.
fn is_external(reference: &str) -> bool {
    if reference.starts_with('#') || reference.starts_with("//") {
        return true;
    }

    match reference.find(':') {
        Some(index) => {
            let scheme = &reference[..index];
            !scheme.is_empty()
                && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        }
        None => false,
    }
}

/// Render a path with forward slashes.
fn slash_path(path: &Path) -> String {
    path.components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Resolve `.` and `..` without touching the filesystem.
///
/// A leading `..` survives, which is how the caller detects that a reference
/// points outside the content directory.
fn normalise(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();

    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => match parts.last() {
                Some(&"..") | None => parts.push(".."),
                Some(_) => {
                    parts.pop();
                }
            },
            other => parts.push(other),
        }
    }

    parts.join("/")
}

/// Percent-encode each segment of a path, leaving `/` as a separator.
fn encode_path(path: &str) -> String {
    path.split('/')
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

/// Percent-encode one path segment, leaving RFC 3986 unreserved characters.
fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());

    for byte in segment.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }

    out
}

/// Percent-decode a URL path, tolerating invalid escapes.
///
/// Editors are inconsistent about encoding: Typora writes raw UTF-8 and spaces
/// for local files, but a hand-edited link may already be encoded. Decoding
/// first means both forms land on the same file.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2]))
        {
            out.push(high * 16 + low);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }

    String::from_utf8_lossy(&out).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// The site stylesheet, compiled into the binary.
///
/// Embedded so the live server needs no asset tree beside it, and so the static
/// exporter can write out exactly the same bytes. Sharing the constant is the
/// point: two copies would be free to drift.
pub const STYLESHEET: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/static/style.css"));

/// Site icons. Embedded like the stylesheet so the server carries no asset
/// tree, and so the static exporter writes exactly the same bytes.
///
/// Sources live in `static/`; `scripts/make-icon.py` regenerates them.
pub const FAVICON_SVG: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/static/favicon.svg"));
pub const FAVICON_ICO: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/static/favicon.ico"));
pub const FAVICON_PNG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/static/favicon-32.png"
));
pub const APPLE_TOUCH_ICON: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/static/apple-touch-icon.png"
));

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A scratch content directory, removed on drop.
    struct Scratch {
        dir: PathBuf,
    }

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("tblogs-assets-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(dir.join("assets")).unwrap();
            fs::create_dir_all(dir.join("notes/assets")).unwrap();
            Self { dir }
        }

        fn write(&self, rel: &str, contents: &str) -> PathBuf {
            let path = self.dir.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            path
        }

        fn assets(&self) -> Assets {
            Assets::new(&self.dir).unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn rewrites_a_relative_reference() {
        let scratch = Scratch::new("relative");
        let assets = scratch.assets();

        assert_eq!(
            assets.url_for("assets/a.png", "post.md").as_deref(),
            Some("/media/assets/a.png")
        );
    }

    #[test]
    fn resolves_relative_to_the_document_directory() {
        let scratch = Scratch::new("nested");
        let assets = scratch.assets();

        assert_eq!(
            assets.url_for("assets/a.png", "notes/post.md").as_deref(),
            Some("/media/notes/assets/a.png")
        );
    }

    #[test]
    fn handles_typora_dot_slash_and_parent_segments() {
        let scratch = Scratch::new("dots");
        let assets = scratch.assets();

        assert_eq!(
            assets.url_for("./assets/a.png", "notes/post.md").as_deref(),
            Some("/media/notes/assets/a.png")
        );
        assert_eq!(
            assets
                .url_for("../assets/a.png", "notes/post.md")
                .as_deref(),
            Some("/media/assets/a.png")
        );
        // Escaping the content directory is refused rather than rewritten.
        assert_eq!(assets.url_for("../../etc/passwd", "notes/post.md"), None);
    }

    #[test]
    fn leaves_remote_and_fragment_references_alone() {
        let scratch = Scratch::new("remote");
        let assets = scratch.assets();

        for reference in [
            "https://cdn.example.com/a.png",
            "http://example.com/a.png",
            "data:image/png;base64,AAAA",
            "#section",
            "//cdn.example.com/a.png",
        ] {
            assert_eq!(
                assets.url_for(reference, "post.md"),
                None,
                "should not rewrite {reference}"
            );
        }
    }

    #[test]
    fn rewrites_absolute_paths_inside_the_content_directory() {
        let scratch = Scratch::new("absolute");
        let assets = scratch.assets();
        let file = scratch.write("assets/a.png", "png");

        // Typora's absolute-path mode writes the real filesystem path.
        assert_eq!(
            assets
                .url_for(&file.to_string_lossy(), "post.md")
                .as_deref(),
            Some("/media/assets/a.png")
        );

        // A site-absolute path that happens to exist under the content root.
        assert_eq!(
            assets.url_for("/assets/a.png", "post.md").as_deref(),
            Some("/media/assets/a.png")
        );

        // A site-absolute path with no matching file stays untouched.
        assert_eq!(assets.url_for("/img/logo.png", "post.md"), None);
    }

    #[test]
    fn encodes_non_ascii_and_spaces() {
        let scratch = Scratch::new("encode");
        let assets = scratch.assets();

        assert_eq!(
            assets.url_for("assets/我的 图片.png", "post.md").as_deref(),
            Some("/media/assets/%E6%88%91%E7%9A%84%20%E5%9B%BE%E7%89%87.png")
        );
    }

    #[test]
    fn decodes_references_that_are_already_encoded() {
        let scratch = Scratch::new("decode");
        let assets = scratch.assets();

        assert_eq!(
            assets.url_for("assets/%E6%88%91.png", "post.md").as_deref(),
            Some("/media/assets/%E6%88%91.png")
        );
    }

    #[test]
    fn keeps_fragments_on_rewritten_links() {
        let scratch = Scratch::new("fragment");
        let assets = scratch.assets();

        assert_eq!(
            assets
                .url_for("assets/doc.pdf#page=2", "post.md")
                .as_deref(),
            Some("/media/assets/doc.pdf#page=2")
        );
    }

    #[test]
    fn rejects_traversal_and_disallowed_types() {
        let scratch = Scratch::new("reject");
        scratch.write("assets/a.png", "png");
        scratch.write("secret.md", "# secret");
        let assets = scratch.assets();

        assert!(assets.path_for("assets/a.png").is_some());
        // The Markdown sources themselves must never be downloadable.
        assert!(assets.path_for("secret.md").is_none());
        assert!(assets.path_for("../secret.md").is_none());
        assert!(assets.path_for("../../etc/hosts").is_none());
        assert!(assets.path_for(".ssh/id_rsa").is_none());
        assert!(assets.path_for("assets/missing.png").is_none());
    }

    #[test]
    fn path_lookup_round_trips_a_rewritten_url() {
        let scratch = Scratch::new("roundtrip");
        scratch.write("notes/assets/图 1.png", "png");
        let assets = scratch.assets();

        let url = assets
            .url_for("assets/图 1.png", "notes/post.md")
            .expect("should rewrite");
        let relative = url.strip_prefix(MEDIA_PREFIX).unwrap();

        let resolved = assets.path_for(relative).expect("should resolve");
        assert!(
            resolved.ends_with("notes/assets/图 1.png"),
            "got {}",
            resolved.display()
        );
    }

    #[test]
    fn content_types_come_from_the_allowlist() {
        assert_eq!(content_type(Path::new("a.png")), Some("image/png"));
        assert_eq!(content_type(Path::new("a.JPG")), Some("image/jpeg"));
        assert_eq!(content_type(Path::new("a.md")), None);
        assert_eq!(content_type(Path::new("a.exe")), None);
        assert_eq!(content_type(Path::new("noextension")), None);
    }

    #[test]
    fn normalise_resolves_dot_segments() {
        assert_eq!(normalise("a/./b"), "a/b");
        assert_eq!(normalise("a/b/../c"), "a/c");
        assert_eq!(normalise("a//b"), "a/b");
        assert_eq!(normalise("../a"), "../a");
        assert_eq!(normalise("a/../../b"), "../b");
    }
}
