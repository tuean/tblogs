//! Front matter parsing and the fallbacks used when it is absent.
//!
//! This understands a deliberately small subset of YAML — the part that
//! Markdown editors such as Typora actually write:
//!
//! ```markdown
//! ---
//! title: 我的第一篇博客
//! date: 2026-01-01 10:30
//! tags: [Rust, 随笔]
//! summary: 一句话摘要
//! draft: false
//! ---
//! ```
//!
//! Supported value forms are scalars, inline `[a, b]` lists, and block lists
//! introduced by a bare `key:` followed by `- item` lines. Nested maps,
//! multi-line strings and anchors are not supported and are ignored, which is
//! why a hand-rolled parser is acceptable here rather than a YAML crate.

use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone, Utc};

/// Front matter values, all optional.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FrontMatter {
    pub title: Option<String>,
    pub slug: Option<String>,
    /// Normalised to an RFC 3339 UTC string when it could be parsed.
    pub date: Option<String>,
    pub tags: Vec<String>,
    pub summary: Option<String>,
    /// Cover image, as written by Hexo-style front matter (`image:`).
    ///
    /// Usually a remote URL, but a local path is accepted too and resolved
    /// against the content directory like any other asset reference.
    pub image: Option<String>,
    pub draft: bool,
}

/// A Markdown file split into its front matter and its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub front: FrontMatter,
    /// The Markdown with the front matter block removed.
    pub body: String,
}

/// Split a document into front matter and body.
///
/// A block only counts as front matter when the very first line is `---` and a
/// closing `---` (or `...`) line exists. Anything else is treated as ordinary
/// body text, so a document that merely starts with a horizontal rule is not
/// silently truncated.
pub fn parse(raw: &str) -> Document {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);

    // Split on `\n` *keeping* the terminator, so summing `chunk.len()` gives an
    // exact byte offset. Using `lines()` and adding one per line looks
    // equivalent but is not: `lines()` also strips a preceding `\r`, so on a
    // CRLF document — which is what most Windows-authored Markdown is — the
    // offset drifts one byte per line and eventually slices mid-character.
    let mut chunks = raw.split_inclusive('\n');

    let Some(opening) = chunks.next() else {
        return Document {
            front: FrontMatter::default(),
            body: String::new(),
        };
    };

    if opening.trim_end() != "---" {
        return Document {
            front: FrontMatter::default(),
            body: raw.to_string(),
        };
    }

    let mut block = Vec::new();
    let mut closed = false;
    let mut offset = opening.len();

    for chunk in chunks {
        offset += chunk.len();

        let line = strip_terminator(chunk);
        let trimmed = line.trim_end();

        if trimmed == "---" || trimmed == "..." {
            closed = true;
            break;
        }

        block.push(line.to_string());
    }

    if !closed {
        // An unterminated block is not front matter; keep the whole document.
        return Document {
            front: FrontMatter::default(),
            body: raw.to_string(),
        };
    }

    Document {
        front: parse_block(&block),
        // Also drop a stray `\r` left by a CRLF file.
        body: raw[offset..].trim_start_matches(['\n', '\r']).to_string(),
    }
}

/// Remove a trailing `\n` and `\r` from a `split_inclusive` chunk.
fn strip_terminator(chunk: &str) -> &str {
    let chunk = chunk.strip_suffix('\n').unwrap_or(chunk);
    chunk.strip_suffix('\r').unwrap_or(chunk)
}

/// Parse the lines between the `---` fences.
fn parse_block(lines: &[String]) -> FrontMatter {
    let mut front = FrontMatter::default();
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index].as_str();
        index += 1;

        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();

        // A bare `key:` may introduce a block list on the following lines.
        if value.is_empty() {
            let mut items = Vec::new();
            while index < lines.len() {
                let candidate = lines[index].trim();
                let Some(item) = candidate.strip_prefix('-') else {
                    break;
                };
                items.push(unquote(item.trim()));
                index += 1;
            }
            if key == "tags" || key == "tag" || key == "categories" {
                front.tags = items.into_iter().filter(|s| !s.is_empty()).collect();
            }
            continue;
        }

        match key.as_str() {
            "title" => front.title = non_empty(unquote(value)),
            "slug" => front.slug = non_empty(unquote(value)),
            "summary" | "description" | "excerpt" => {
                front.summary = non_empty(unquote(value));
            }
            "image" | "cover" | "thumbnail" => front.image = non_empty(unquote(value)),
            "date" | "published" | "pubdate" => front.date = parse_date(value),
            "draft" => front.draft = parse_bool(value),
            "tags" | "tag" | "categories" | "keywords" => {
                front.tags = parse_list(value);
            }
            _ => {}
        }
    }

    front
}

/// Parse `[a, b]`, `a, b` or a single value into a list.
fn parse_list(value: &str) -> Vec<String> {
    let inner = value
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .unwrap_or(value);

    inner
        .split(',')
        .map(|item| unquote(item.trim()))
        .filter(|item| !item.is_empty())
        .collect()
}

fn parse_bool(value: &str) -> bool {
    matches!(
        unquote(value).to_ascii_lowercase().as_str(),
        "true" | "yes" | "on" | "1"
    )
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|v| v.strip_suffix(quote))
        {
            return inner.to_string();
        }
    }
    value.to_string()
}

fn non_empty(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Normalise a date to an RFC 3339 UTC string.
///
/// Accepts `YYYY-MM-DD`, `YYYY-MM-DD HH:MM[:SS]`, `YYYY/MM/DD`, and anything
/// chrono can already read as RFC 3339. A date without a zone is read as the
/// machine's local time, which is what an author writing front matter by hand
/// almost always means.
pub fn parse_date(value: &str) -> Option<String> {
    let value = unquote(value);
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some(
            dt.with_timezone(&Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
    }

    let normalised = value.replace('/', "-");

    for format in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(&normalised, format) {
            return localise(naive);
        }
    }

    if let Ok(date) = NaiveDate::parse_from_str(&normalised, "%Y-%m-%d") {
        return localise(date.and_hms_opt(0, 0, 0)?);
    }

    None
}

/// Interpret a naive local timestamp in the machine's time zone.
fn localise(naive: NaiveDateTime) -> Option<String> {
    let utc = match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => dt.with_timezone(&Utc),
        // During a DST fold prefer the earlier instant; during a gap there is
        // no such local time, so fall back to treating it as UTC.
        chrono::LocalResult::Ambiguous(earlier, _) => earlier.with_timezone(&Utc),
        chrono::LocalResult::None => Utc.from_utc_datetime(&naive),
    };

    Some(utc.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// A `YYYY-MM-DD-` or `YYYY-MM-DD_` prefix on a file name.
///
/// Used both to date a document by its file name and to strip that prefix when
/// deriving a title or slug.
pub fn split_date_prefix(stem: &str) -> (Option<String>, &str) {
    /// Length of `YYYY-MM-DD`.
    const PREFIX_LEN: usize = 10;

    // The prefix is ten ASCII bytes. Both guards matter: slicing at byte 10
    // panics when it lands inside a multi-byte character, and a Chinese file
    // name crosses it easily — in `图片与目录结构`, `目` spans bytes 9..12.
    if stem.len() < PREFIX_LEN + 1
        || !stem.is_char_boundary(PREFIX_LEN)
        || !stem.as_bytes()[..PREFIX_LEN]
            .iter()
            .all(|byte| byte.is_ascii_digit() || *byte == b'-')
    {
        return (None, stem);
    }

    let (head, rest) = stem.split_at(PREFIX_LEN);

    // `rest` begins with an ASCII separator, so slicing off one byte is safe.
    let separator = rest.as_bytes().first().copied();
    if !matches!(separator, Some(b'-') | Some(b'_')) {
        return (None, stem);
    }

    match parse_date(head) {
        Some(date) => (Some(date), &rest[1..]),
        None => (None, stem),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_block() {
        let doc = parse(
            "---\n\
             title: 我的第一篇博客\n\
             slug: hello\n\
             date: 2026-01-01 10:30\n\
             tags: [Rust, 随笔]\n\
             summary: 一句话\n\
             draft: true\n\
             ---\n\
             # Heading\n\nBody text.\n",
        );

        assert_eq!(doc.front.title.as_deref(), Some("我的第一篇博客"));
        assert_eq!(doc.front.slug.as_deref(), Some("hello"));
        assert_eq!(doc.front.tags, vec!["Rust", "随笔"]);
        assert_eq!(doc.front.summary.as_deref(), Some("一句话"));
        assert!(doc.front.draft);
        assert!(doc.front.date.is_some());
        assert_eq!(doc.body, "# Heading\n\nBody text.\n");
    }

    #[test]
    fn parses_the_hexo_cover_field() {
        // Hexo writes the cover as `image:`; quoted URLs are common.
        let doc = parse("---\nimage: 'https://cdn.example.com/a.jpg'\ntitle: x\n---\nbody");
        assert_eq!(
            doc.front.image.as_deref(),
            Some("https://cdn.example.com/a.jpg")
        );

        // `cover` and `thumbnail` are accepted as aliases.
        let doc = parse("---\ncover: ./local.png\n---\nbody");
        assert_eq!(doc.front.image.as_deref(), Some("./local.png"));

        // Absent means absent, not an empty string.
        let doc = parse("---\ntitle: x\n---\nbody");
        assert!(doc.front.image.is_none());
    }

    #[test]
    fn parses_block_lists_and_quoted_values() {
        let doc = parse(
            "---\n\
             title: \"Quoted: title\"\n\
             tags:\n  - alpha\n  - beta\n\
             ---\nbody",
        );
        assert_eq!(doc.front.title.as_deref(), Some("Quoted: title"));
        assert_eq!(doc.front.tags, vec!["alpha", "beta"]);
        assert_eq!(doc.body, "body");
    }

    #[test]
    fn absent_front_matter_leaves_the_body_alone() {
        let doc = parse("# Title\n\nSome text.\n");
        assert_eq!(doc.front, FrontMatter::default());
        assert_eq!(doc.body, "# Title\n\nSome text.\n");
    }

    #[test]
    fn a_leading_horizontal_rule_is_not_front_matter() {
        // No closing fence, so this must not be swallowed.
        let raw = "---\n\nJust a rule and text.\n";
        let doc = parse(raw);
        assert_eq!(doc.body, raw);
    }

    #[test]
    fn strips_a_utf8_bom() {
        let doc = parse("\u{feff}---\ntitle: x\n---\nbody");
        assert_eq!(doc.front.title.as_deref(), Some("x"));
        assert_eq!(doc.body, "body");
    }

    #[test]
    fn parses_front_matter_in_a_crlf_document() {
        // Windows line endings, which is what most editor-authored Markdown
        // uses. Regression: the body offset used to be computed by adding one
        // byte per line, which drifts by one byte per line on CRLF and panics
        // as soon as the document contains multi-byte characters.
        let raw =
            "---\r\ntitle: 中文标题\r\ntags: [a, b]\r\n---\r\n\r\n# 正文标题\r\n\r\n内容。\r\n";
        let doc = parse(raw);

        assert_eq!(doc.front.title.as_deref(), Some("中文标题"));
        assert_eq!(doc.front.tags, vec!["a", "b"]);
        assert!(
            !doc.body.contains("title:"),
            "body still contains front matter: {:?}",
            doc.body
        );
        assert!(doc.body.contains("# 正文标题"), "got {:?}", doc.body);
    }

    #[test]
    fn crlf_documents_without_front_matter_are_untouched() {
        let raw = "# 标题\r\n\r\n正文。\r\n";
        let doc = parse(raw);
        assert_eq!(doc.front, FrontMatter::default());
        assert_eq!(doc.body, raw);
    }

    #[test]
    fn a_crlf_block_ending_in_a_dots_fence_is_recognised() {
        let raw = "---\r\ntitle: x\r\n...\r\nbody\r\n";
        let doc = parse(raw);
        assert_eq!(doc.front.title.as_deref(), Some("x"));
        assert_eq!(doc.body, "body\r\n");
    }

    #[test]
    fn dates_normalise_to_rfc3339() {
        // A date with no zone means local midnight, so the UTC instant differs
        // by the machine's offset. Convert back rather than assuming UTC.
        let date = parse_date("2026-01-01").unwrap();
        let local = chrono::DateTime::parse_from_rfc3339(&date)
            .unwrap()
            .with_timezone(&Local);
        assert_eq!(
            local.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-01-01 00:00:00"
        );

        // An explicit zone is preserved exactly.
        assert_eq!(
            parse_date("2026-01-01T10:00:00+08:00").unwrap(),
            "2026-01-01T02:00:00Z"
        );

        assert!(parse_date("not a date").is_none());
        assert!(parse_date("").is_none());
    }

    #[test]
    fn splits_date_prefixes_from_file_names() {
        let (date, rest) = split_date_prefix("2026-01-01-hello-world");
        assert!(date.is_some());
        assert_eq!(rest, "hello-world");

        let (date, rest) = split_date_prefix("2026-01-01_hello");
        assert!(date.is_some());
        assert_eq!(rest, "hello");

        // Not a date, so nothing is stripped.
        let (date, rest) = split_date_prefix("hello-world");
        assert!(date.is_none());
        assert_eq!(rest, "hello-world");

        let (date, rest) = split_date_prefix("2026-13-45-bad");
        assert!(date.is_none());
        assert_eq!(rest, "2026-13-45-bad");
    }

    #[test]
    fn date_prefix_splitting_survives_multibyte_file_names() {
        // Regression: byte 10 falls inside `目` here, and slicing at a
        // non-char-boundary used to panic the whole server at startup.
        for name in [
            "图片与目录结构",
            "我的第一篇博客文章",
            "中文标题很长很长很长",
            "笔记",
        ] {
            let (date, rest) = split_date_prefix(name);
            assert!(date.is_none(), "{name} should have no date prefix");
            assert_eq!(rest, name);
        }

        // A real date prefix in front of a CJK title still splits correctly.
        let (date, rest) = split_date_prefix("2026-01-05-中文标题");
        assert!(date.is_some());
        assert_eq!(rest, "中文标题");

        // Multi-byte text exactly at the boundary must also be safe.
        let (date, rest) = split_date_prefix("中文中文中文中");
        assert!(date.is_none());
        assert_eq!(rest, "中文中文中文中");
    }

    #[test]
    fn booleans_accept_the_common_spellings() {
        assert!(parse_bool("true"));
        assert!(parse_bool("YES"));
        assert!(parse_bool("1"));
        assert!(!parse_bool("false"));
        assert!(!parse_bool(""));
    }
}
