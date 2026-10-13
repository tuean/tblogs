//! Data types shared between the database layer and the templates.

use serde::Deserialize;
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
}

impl Post {
    /// `YYYY-MM-DD` shown in listings: the publication date when the post is
    /// live, otherwise the last edit.
    pub fn display_date(&self) -> String {
        let raw = self
            .published_at
            .as_deref()
            .unwrap_or(self.updated_at.as_str());
        raw.chars().take(10).collect()
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
        })
    }
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
}

/// A post prepared for the admin edit form, which edits raw Markdown.
#[derive(Debug, Clone)]
pub struct PostFormView {
    pub slug: String,
    pub title: String,
    pub summary: String,
    pub body_md: String,
    pub published: bool,
    /// Comma-separated tag names, as the form field expects.
    pub tags: String,
}

impl PostFormView {
    /// A blank form for creating a post.
    pub fn new_post() -> Self {
        Self {
            slug: String::new(),
            title: String::new(),
            summary: String::new(),
            body_md: String::new(),
            published: false,
            tags: String::new(),
        }
    }
}

/// A tag together with how many published posts carry it.
#[derive(Debug, Clone)]
pub struct TagCount {
    pub name: String,
    pub slug: String,
    pub count: i64,
}

/// The submitted contents of the post form.
///
/// `status` arrives from a `<select>` rather than a checkbox so that a
/// deliberately unchecked box cannot be confused with a missing field.
#[derive(Debug, Deserialize)]
pub struct PostForm {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub body_md: String,
    #[serde(default)]
    pub tags: String,
    #[serde(default = "default_status")]
    pub status: String,
}

fn default_status() -> String {
    "draft".to_string()
}

impl PostForm {
    pub fn is_publish(&self) -> bool {
        self.status == "published"
    }

    /// Split the comma/whitespace separated tag field into clean names.
    ///
    /// Accepts both `,` and `，` (the full-width comma common on Chinese
    /// keyboards) plus whitespace, and removes duplicates case-insensitively.
    pub fn tag_names(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut names = Vec::new();

        for raw in self
            .tags
            .split([',', '，', '\n', '\t'])
            .flat_map(|part| part.split_whitespace())
        {
            let name = raw.trim().trim_start_matches('#').trim();
            if name.is_empty() {
                continue;
            }
            if seen.insert(name.to_lowercase()) {
                names.push(name.to_string());
            }
        }

        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form_with_tags(tags: &str) -> PostForm {
        PostForm {
            title: "t".into(),
            slug: String::new(),
            summary: String::new(),
            body_md: String::new(),
            tags: tags.into(),
            status: "draft".into(),
        }
    }

    #[test]
    fn tag_names_split_on_both_comma_styles() {
        let form = form_with_tags("rust, axum，sqlite  #gpui");
        assert_eq!(form.tag_names(), vec!["rust", "axum", "sqlite", "gpui"]);
    }

    #[test]
    fn tag_names_drop_duplicates_case_insensitively() {
        let form = form_with_tags("Rust, rust, RUST");
        assert_eq!(form.tag_names(), vec!["Rust"]);
    }

    #[test]
    fn status_select_maps_to_published_flag() {
        let mut form = form_with_tags("");
        assert!(!form.is_publish());
        form.status = "published".into();
        assert!(form.is_publish());
    }
}
