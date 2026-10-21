//! Data types shared between the database layer and the templates.

use sqlx::{Row, sqlite::SqliteRow};

/// A post exactly as stored in the `posts` table.
///
/// Timestamps are ISO-8601 strings (`2026-01-01T09:30:00Z`). SQLite has no
/// native date type, and storing text keeps the values readable in
/// `sqlite3` and sortable with a plain `ORDER BY`.
#[derive(Debug, Clone)]
pub struct Post {
    pub id: i64,
    pub slug: String,
    pub title: String,
    pub summary: String,
    pub body_md: String,
    pub published: bool,
    pub created_at: String,
    pub updated_at: String,
    pub published_at: Option<String>,
    /// Path of the source Markdown file, relative to the content root.
    ///
    /// Always present: every post in a package comes from a file. The server
    /// uses it to resolve that post's relative image references.
    pub source_path: String,
    /// Cover image URL, ready to use as written in a template.
    pub cover_image: Option<String>,
    /// Series display name, when this post is a chapter of one.
    pub series: Option<String>,
    /// URL form of `series`; always set when `series` is.
    pub series_slug: Option<String>,
    /// Position within the series. `None` for a series member whose front
    /// matter carried no usable `order:`, which sorts as `0`.
    pub chapter_order: Option<i64>,
}

impl Post {
    /// `YYYY-MM-DD` shown in listings: the publication date when the post is
    /// live, otherwise the last edit.
    pub fn display_date(&self) -> String {
        let raw = self
            .published_at
            .as_deref()
            .unwrap_or(self.updated_at.as_str());
        format_local_date(raw)
    }

    /// The position used for ordering, with an absent `order` read as last.
    pub fn order_key(&self) -> i64 {
        self.chapter_order.unwrap_or(CHAPTER_ORDER_LAST)
    }
}

// Hand-written rather than `#[derive(FromRow)]`, which would pull in the
// sqlx-macros proc macro. That proc macro is a large dylib that the linker on
// some macOS toolchains emits with a malformed LINKEDIT string pool, which
// makes `cargo build --release` fail outright. The mapping is trivial enough
// that spelling it out costs nothing and removes the dependency.
impl<'r> sqlx::FromRow<'r, SqliteRow> for Post {
    fn from_row(row: &'r SqliteRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            slug: row.try_get("slug")?,
            title: row.try_get("title")?,
            summary: row.try_get("summary")?,
            body_md: row.try_get("body_md")?,
            published: row.try_get("published")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            published_at: row.try_get("published_at")?,
            source_path: row.try_get("source_path")?,
            cover_image: row.try_get("cover_image")?,
            series: row.try_get("series")?,
            series_slug: row.try_get("series_slug")?,
            chapter_order: row.try_get("chapter_order")?,
        })
    }
}

/// A post derived from one Markdown file, ready to be written to the database.
///
/// Slug uniqueness is resolved by the database layer, so `slug_base` is only a
/// starting point.
#[derive(Debug, Clone)]
pub struct SourcedPost {
    /// Path relative to the content directory, with `/` separators.
    pub source_path: String,
    /// Modification time of the file, in nanoseconds since the epoch.
    pub source_mtime: i64,
    pub title: String,
    pub slug_base: String,
    pub summary: String,
    pub body_md: String,
    pub published: bool,
    /// Publication date from front matter or a `YYYY-MM-DD-` file name prefix.
    pub date_hint: Option<String>,
    /// The file's modification time as an ISO-8601 string.
    pub updated_at: String,
    pub tags: Vec<String>,
    /// Cover image exactly as the front matter wrote it; the renderer resolves
    /// it so a local path becomes a `/media/...` URL.
    pub cover_image: Option<String>,
    /// Series this document belongs to, straight from its front matter.
    pub series: Option<String>,
    /// Position within that series, when the front matter carried one.
    pub order: Option<i64>,
}

/// Sort position given to a chapter whose front matter carried no usable
/// `order:`.
///
/// `i64::MAX`, so such a chapter sorts after every position an author could
/// plausibly type. The alternative — treating it as `0` — reads better to an
/// author looking for the gap, but it lets one hand-broken file corrupt the
/// reading sequence: an unnumbered chapter would appear *in front of* chapter
/// one and become its 「上一章」. A reader following the navigation must never be
/// sent backwards, so the unnumbered chapter goes last, where it is still
/// plainly visible as the only unnumbered entry in the contents.
///
/// The same value is written literally into the series queries in `db.rs`,
/// because `concat!` only accepts literals; a test pins the two together.
pub const CHAPTER_ORDER_LAST: i64 = i64::MAX;

/// One series, as shown on the `/series` index.
#[derive(Debug, Clone)]
pub struct SeriesSummary {
    /// Display name, i.e. the `series` value itself.
    pub title: String,
    pub slug: String,
    pub chapter_count: i64,
}

// Hand-written for the same reason as `Post`: no sqlx-macros proc macro.
impl<'r> sqlx::FromRow<'r, SqliteRow> for SeriesSummary {
    fn from_row(row: &'r SqliteRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            title: row.try_get("title")?,
            slug: row.try_get("slug")?,
            chapter_count: row.try_get("chapter_count")?,
        })
    }
}

/// Enough of a chapter to link to it from the navigation row.
#[derive(Debug, Clone)]
pub struct ChapterLink {
    pub title: String,
    pub slug: String,
}

/// The reading-order navigation shown on a chapter page.
///
/// `prev` and `next` are the chapters adjacent in `chapter_order`, which is not
/// the same thing as the posts adjacent in publication time — that is the whole
/// point, since a novel is published in whatever order the author gets round to
/// it but must be read in one.
#[derive(Debug, Clone)]
pub struct ChapterNav {
    pub series: String,
    pub series_slug: String,
    /// Position of the chapter being viewed.
    pub order: i64,
    pub prev: Option<ChapterLink>,
    pub next: Option<ChapterLink>,
}

/// A date for display, in the machine's time zone.
///
/// Timestamps are stored in UTC because that sorts correctly and is
/// unambiguous, but a date an author typed by hand — `date: 2026-01-05`, or a
/// `2026-01-05-` file name prefix — means local midnight. Slicing the UTC
/// string would then show the previous day anywhere east of Greenwich.
fn format_local_date(raw: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|stamp| {
            stamp
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_else(|_| raw.chars().take(10).collect())
}

/// A tag as stored in the `tags` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub id: i64,
    pub name: String,
    pub slug: String,
}

impl<'r> sqlx::FromRow<'r, SqliteRow> for Tag {
    fn from_row(row: &'r SqliteRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            slug: row.try_get("slug")?,
        })
    }
}

/// Site-wide values baked into the content package at build time.
///
/// Keeping them in the database means a published server needs no
/// configuration to render correctly: titles, meta tags and absolute RSS links
/// all travel with the content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub site_title: String,
    pub site_description: String,
    /// Absolute public base URL, without a trailing slash.
    pub site_url: String,
    /// When the package was produced, surfaced as a build stamp.
    pub built_at: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            site_title: "我的博客".to_string(),
            site_description: String::new(),
            site_url: "http://localhost:8080".to_string(),
            built_at: String::new(),
        }
    }
}

impl Settings {
    /// Apply overrides on top of the stored values.
    pub fn with_overrides(
        mut self,
        title: Option<String>,
        description: Option<String>,
        url: Option<String>,
    ) -> Self {
        if let Some(title) = title {
            self.site_title = title;
        }
        if let Some(description) = description {
            self.site_description = description;
        }
        if let Some(url) = url {
            self.site_url = url;
        }
        self
    }

    /// `YYYY-MM-DD` build date, empty when the package predates the field.
    pub fn build_date(&self) -> String {
        if self.built_at.is_empty() {
            return String::new();
        }
        format_local_date(&self.built_at)
    }
}

/// A tag together with how many published posts carry it.
#[derive(Debug, Clone)]
pub struct TagCount {
    pub name: String,
    pub slug: String,
    pub count: i64,
}

/// A post prepared for rendering.
#[derive(Debug, Clone)]
pub struct PostView {
    pub slug: String,
    pub title: String,
    pub summary: String,
    /// Rendered HTML; injected into templates with `|safe`.
    pub body_html: String,
    pub date: String,
    pub tags: Vec<Tag>,
    /// Resolved cover image URL, empty when the post has none. Kept as an
    /// empty string rather than an `Option` so templates stay free of
    /// unwrapping.
    pub cover: String,
    /// True when this post is a chapter of a series.
    ///
    /// A chapter is read at length, so the page switches to the reading
    /// layout: serif text, a narrower column, indented paragraphs. An ordinary
    /// post keeps the denser blog layout.
    pub reading: bool,
}

/// Clean up tag names coming from front matter.
///
/// Trims, drops empties, deduplicates case-insensitively while keeping the
/// first spelling seen, and caps the length so a stray paragraph cannot become
/// a tag. Front matter is hand-written, so it needs the same tolerance the web
/// form used to apply.
pub fn normalise_tags(names: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut tags = Vec::new();

    for name in names {
        let name = name.trim().trim_start_matches('#').trim();
        if name.is_empty() {
            continue;
        }

        let name: String = name.chars().take(50).collect();
        if seen.insert(name.to_lowercase()) {
            tags.push(name);
        }
    }

    tags
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(names: &[&str]) -> Vec<String> {
        let owned: Vec<String> = names.iter().map(|name| (*name).to_string()).collect();
        normalise_tags(&owned)
    }

    #[test]
    fn tags_are_trimmed_and_deduplicated_case_insensitively() {
        assert_eq!(tags(&["Rust", " rust ", "RUST"]), vec!["Rust"]);
        assert_eq!(tags(&["a", "", "  "]), vec!["a"]);
    }

    #[test]
    fn tags_lose_a_leading_hash() {
        // Front matter sometimes carries `tags: [#rust]`.
        assert_eq!(tags(&["#rust", "#  axum"]), vec!["rust", "axum"]);
    }

    #[test]
    fn tag_length_is_capped() {
        let long = "x".repeat(200);
        assert_eq!(tags(&[&long])[0].chars().count(), 50);
    }

    fn post_with(published_at: Option<&str>, updated_at: &str) -> Post {
        Post {
            id: 1,
            slug: "s".into(),
            title: "t".into(),
            summary: String::new(),
            body_md: String::new(),
            published: published_at.is_some(),
            created_at: updated_at.to_string(),
            updated_at: updated_at.to_string(),
            published_at: published_at.map(str::to_string),
            source_path: "post.md".into(),
            cover_image: None,
            series: None,
            series_slug: None,
            chapter_order: None,
        }
    }

    #[test]
    fn display_date_is_the_local_date_not_the_utc_one() {
        // A hand-written `2026-01-05` becomes UTC midnight minus the offset.
        // Displaying the raw UTC prefix would show 2026-01-04 in Asia/Shanghai.
        let post = post_with(Some("2026-01-04T16:00:00Z"), "2026-01-04T16:00:00Z");

        let expected = chrono::DateTime::parse_from_rfc3339("2026-01-04T16:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d")
            .to_string();

        assert_eq!(post.display_date(), expected);
    }

    #[test]
    fn display_date_prefers_publication_and_falls_back_for_junk() {
        let post = post_with(Some("2026-03-04T05:06:07Z"), "2026-01-01T00:00:00Z");
        assert!(
            post.display_date().starts_with("2026-03-0"),
            "got {}",
            post.display_date()
        );

        // An unparseable value degrades to its first ten characters rather than
        // panicking.
        let broken = post_with(Some("not-a-timestamp"), "also-junk");
        assert_eq!(broken.display_date(), "not-a-time");
    }

    #[test]
    fn settings_overrides_only_replace_what_is_given() {
        let base = Settings {
            site_title: "before".into(),
            site_description: "desc".into(),
            site_url: "https://a.example".into(),
            built_at: "2026-01-01T00:00:00Z".into(),
        };

        let updated = base
            .clone()
            .with_overrides(Some("after".into()), None, None);
        assert_eq!(updated.site_title, "after");
        assert_eq!(updated.site_description, "desc");
        assert_eq!(updated.site_url, "https://a.example");
    }
}
