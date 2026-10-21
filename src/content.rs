//! Turning a directory of Markdown files into posts.
//!
//! The directory is the source of truth for every file-backed post: editing a
//! file updates the post, and deleting a file deletes the post. Posts created
//! in the web admin have no source file and are never touched by a sync.
//!
//! A file's own metadata is used to decide whether it needs reading at all, so
//! a poll of an unchanged directory costs one directory walk and no file reads.

use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    path::Path,
    time::UNIX_EPOCH,
};

use anyhow::{Context, Result};
use sqlx::SqlitePool;

use crate::{
    db, frontmatter, markdown,
    models::{SourcedPost, normalise_tags},
    slug,
};

/// File extensions treated as articles.
const MARKDOWN_EXTENSIONS: &[&str] = &["md", "markdown"];

/// One Markdown file found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    /// Path relative to the content root, with `/` separators.
    pub rel_path: String,
    /// Modification time in nanoseconds since the epoch.
    pub mtime: i64,
}

/// A cheap digest of the directory's shape, used to skip work when nothing
/// changed. Two different directories may collide; a collision only costs one
/// redundant sync, which then finds every file unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint(u64);

impl Fingerprint {
    /// The raw digest, for callers that only need a comparable scalar.
    pub fn value(self) -> u64 {
        self.0
    }
}

/// What one sync pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SyncReport {
    pub created: usize,
    pub updated: usize,
    pub removed: usize,
    pub unchanged: usize,
}

impl SyncReport {
    /// Whether the pass changed anything at all.
    pub fn changed(&self) -> bool {
        self.created > 0 || self.updated > 0 || self.removed > 0
    }

    /// A one-line summary for the log.
    pub fn summary(&self) -> String {
        format!(
            "{} created, {} updated, {} removed, {} unchanged",
            self.created, self.updated, self.removed, self.unchanged
        )
    }

    /// The same numbers packed into a query-string value for the admin notice.
    pub fn to_query(self) -> String {
        format!(
            "{},{},{},{}",
            self.created, self.updated, self.removed, self.unchanged
        )
    }

    /// Parse the value produced by [`SyncReport::to_query`].
    pub fn from_query(value: &str) -> Option<Self> {
        let numbers: Vec<usize> = value
            .split(',')
            .map(|part| part.parse().ok())
            .collect::<Option<Vec<_>>>()?;

        match numbers.as_slice() {
            [created, updated, removed, unchanged] => Some(Self {
                created: *created,
                updated: *updated,
                removed: *removed,
                unchanged: *unchanged,
            }),
            _ => None,
        }
    }
}

/// Every Markdown file under `root`, sorted for a stable fingerprint.
///
/// Hidden files and directories are skipped, which keeps editor state such as
/// `.git`, `.obsidian` and `.typora` out of the blog.
pub fn scan(root: &Path) -> Result<Vec<SourceFile>> {
    let mut files = Vec::new();
    walk(root, root, &mut files)?;
    files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(files)
}

fn walk(root: &Path, dir: &Path, files: &mut Vec<SourceFile>) -> Result<()> {
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("could not read {}", dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if name.starts_with('.') {
            continue;
        }

        let path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            walk(root, &path, files)?;
        } else if file_type.is_file() && is_markdown(&path) {
            let metadata = entry.metadata()?;
            let relative = path.strip_prefix(root).unwrap_or(&path);

            files.push(SourceFile {
                rel_path: slash_path(relative),
                mtime: mtime_nanos(&metadata),
            });
        }
    }

    Ok(())
}

/// Digest the file list so an unchanged directory can be detected cheaply.
pub fn fingerprint(files: &[SourceFile]) -> Fingerprint {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for file in files {
        file.rel_path.hash(&mut hasher);
        file.mtime.hash(&mut hasher);
    }
    Fingerprint(hasher.finish())
}

/// Read one file and derive the post it represents.
///
/// Metadata comes from front matter when present, and otherwise from the file
/// itself: the first `# heading` becomes the title, a `YYYY-MM-DD-` file name
/// prefix becomes the date, and the file name becomes the slug.
pub fn build_post(root: &Path, file: &SourceFile) -> Result<SourcedPost> {
    let absolute = root.join(&file.rel_path);
    let raw = std::fs::read_to_string(&absolute)
        .with_context(|| format!("could not read {}", absolute.display()))?;

    let document = frontmatter::parse(&raw);

    let stem = Path::new(&file.rel_path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.rel_path.clone());

    let (name_date, stem) = frontmatter::split_date_prefix(&stem);

    // A leading `# Heading` is used as the title and removed from the body,
    // otherwise it would render twice: once in the article header and once
    // inside the prose.
    //
    // When front matter does supply a title the body is normally left exactly
    // as written — but if it opens with that same heading, keeping it would
    // duplicate the title on the page, so it is dropped as well.
    let (title, body) = match document.front.title.clone() {
        Some(title) => match take_leading_heading(&document.body) {
            Some((heading, rest)) if heading == title => (title, rest),
            _ => (title, document.body.clone()),
        },
        None => match take_leading_heading(&document.body) {
            Some((heading, rest)) => (heading, rest),
            None => (humanise(stem), document.body.clone()),
        },
    };

    let slug_source = document
        .front
        .slug
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| stem.to_string());

    let mut slug_base = slug::slugify(&slug_source);
    if slug_base.is_empty() {
        // A file named entirely with punctuation, for example.
        slug_base = slug::slugify(&file.rel_path);
    }
    if slug_base.is_empty() {
        slug_base = format!("post-{}", file.mtime);
    }

    let summary = document
        .front
        .summary
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| markdown::excerpt(&body, 160));

    Ok(SourcedPost {
        source_path: file.rel_path.clone(),
        source_mtime: file.mtime,
        title,
        slug_base,
        summary,
        body_md: body,
        // Everything in the directory is published unless it says otherwise.
        published: !document.front.draft,
        date_hint: document.front.date.clone().or(name_date),
        updated_at: iso_from_nanos(file.mtime),
        tags: normalise_tags(&document.front.tags),
        cover_image: document.front.image.clone(),
        // Carried through untouched: whether the pieces form a series, and in
        // what order, is the author's business, not the builder's.
        series: document.front.series.clone(),
        order: document.front.order,
    })
}

/// Bring the database in line with the directory.
pub async fn sync(pool: &SqlitePool, root: &Path, files: &[SourceFile]) -> Result<SyncReport> {
    let mut report = SyncReport::default();

    // One query up front: which files are already imported, and how fresh.
    let known: HashMap<String, i64> = db::list_source_mtimes(pool).await?.into_iter().collect();

    let mut seen: HashSet<&str> = HashSet::with_capacity(files.len());

    for file in files {
        seen.insert(file.rel_path.as_str());

        if known.get(&file.rel_path) == Some(&file.mtime) {
            report.unchanged += 1;
            continue;
        }

        let post = build_post(root, file)?;

        match db::sync_sourced_post(pool, &post).await? {
            db::SyncOutcome::Created => {
                tracing::info!(source = %file.rel_path, title = %post.title, "imported new article");
                report.created += 1;
            }
            db::SyncOutcome::Updated => {
                tracing::info!(source = %file.rel_path, title = %post.title, "updated article");
                report.updated += 1;
            }
        }
    }

    // Imported posts whose file is gone are removed. Posts created in the web
    // admin have no source path and are left alone.
    for (id, source_path) in db::list_sourced_posts(pool).await? {
        if !seen.contains(source_path.as_str()) {
            db::delete_post(pool, id).await?;
            tracing::info!(source = %source_path, "removed article: source file is gone");
            report.removed += 1;
        }
    }

    Ok(report)
}

/// Strip a leading `# Heading` from the body.
///
/// Only a level-one heading in the very first non-blank position counts, which
/// is the convention Markdown editors produce.
fn take_leading_heading(body: &str) -> Option<(String, String)> {
    let mut offset = 0usize;

    for line in body.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);

        if content.trim().is_empty() {
            offset += line.len();
            continue;
        }

        // Exactly one '#' followed by a space: `## Sub` and `#NoSpace` do not
        // qualify, matching CommonMark.
        let heading = content.trim().strip_prefix("# ")?.trim().to_string();
        if heading.is_empty() {
            return None;
        }

        let rest = &body[(offset + line.len()).min(body.len())..];
        return Some((heading, rest.trim_start_matches('\n').to_string()));
    }

    None
}

/// Turn a file name into something readable: `my-first-post` → `my first post`.
fn humanise(stem: &str) -> String {
    let replaced = stem.replace(['-', '_'], " ");
    let collapsed = replaced.split_whitespace().collect::<Vec<_>>().join(" ");

    if collapsed.is_empty() {
        stem.to_string()
    } else {
        collapsed
    }
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .is_some_and(|ext| MARKDOWN_EXTENSIONS.contains(&ext.as_str()))
}

fn slash_path(path: &Path) -> String {
    path.components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Modification time in nanoseconds since the epoch.
///
/// Nanosecond resolution matters: a quick edit-and-save can land inside the
/// same second, and a seconds-granularity stamp would then look unchanged.
fn mtime_nanos(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|delta| delta.as_nanos() as i64)
        .unwrap_or(0)
}

fn iso_from_nanos(nanos: i64) -> String {
    let seconds = nanos.div_euclid(1_000_000_000);
    chrono::DateTime::from_timestamp(seconds, 0)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(rel_path: &str, mtime: i64) -> SourceFile {
        SourceFile {
            rel_path: rel_path.to_string(),
            mtime,
        }
    }

    #[test]
    fn fingerprint_tracks_content_and_names() {
        let a = vec![file("a.md", 1), file("b.md", 2)];
        assert_eq!(fingerprint(&a), fingerprint(&a.clone()));

        // A different modification time changes the digest.
        let b = vec![file("a.md", 1), file("b.md", 3)];
        assert_ne!(fingerprint(&a), fingerprint(&b));

        // So does a set of different files.
        let c = vec![file("a.md", 1), file("c.md", 2)];
        assert_ne!(fingerprint(&a), fingerprint(&c));
    }

    #[test]
    fn takes_a_leading_h1_as_the_title() {
        let (title, rest) = take_leading_heading("# Hello\n\nBody.\n").unwrap();
        assert_eq!(title, "Hello");
        assert_eq!(rest, "Body.\n");
    }

    #[test]
    fn takes_a_leading_h1_after_blank_lines() {
        let (title, rest) = take_leading_heading("\n\n# 中文标题\n正文\n").unwrap();
        assert_eq!(title, "中文标题");
        assert_eq!(rest, "正文\n");
    }

    #[test]
    fn ignores_h2_and_headings_that_are_not_first() {
        assert!(take_leading_heading("## Sub\n\nBody").is_none());
        assert!(take_leading_heading("#NoSpace\n\nBody").is_none());
        assert!(take_leading_heading("Intro text\n\n# Later\n").is_none());
        assert!(take_leading_heading("").is_none());
    }

    #[test]
    fn the_cover_field_reaches_the_post() {
        let root = std::env::temp_dir().join(format!("tblogs-cover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let source = SourceFile {
            rel_path: "post.md".to_string(),
            mtime: 1,
        };

        std::fs::write(
            root.join("post.md"),
            "---\ntitle: With cover\nimage: https://cdn.example.com/a.jpg\n---\n\nBody.\n",
        )
        .unwrap();

        let post = build_post(&root, &source).unwrap();
        assert_eq!(
            post.cover_image.as_deref(),
            Some("https://cdn.example.com/a.jpg")
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn humanises_file_names() {
        assert_eq!(humanise("my-first-post"), "my first post");
        assert_eq!(humanise("my_first_post"), "my first post");
        // CJK names have nothing to replace.
        assert_eq!(humanise("我的文章"), "我的文章");
    }

    #[test]
    fn a_heading_repeating_the_front_matter_title_is_dropped() {
        let root = std::env::temp_dir().join(format!("tblogs-dup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let source = |name: &str| SourceFile {
            rel_path: name.to_string(),
            mtime: 1,
        };

        // Same title in front matter and as the opening heading: keep one.
        std::fs::write(
            root.join("same.md"),
            "---\ntitle: Hello\n---\n\n# Hello\n\nBody.\n",
        )
        .unwrap();
        let post = build_post(&root, &source("same.md")).unwrap();
        assert_eq!(post.title, "Hello");
        assert!(!post.body_md.contains("# Hello"), "got {:?}", post.body_md);
        assert!(post.body_md.contains("Body."));

        // A different heading is real content and must survive.
        std::fs::write(
            root.join("different.md"),
            "---\ntitle: Hello\n---\n\n# Section One\n\nBody.\n",
        )
        .unwrap();
        let post = build_post(&root, &source("different.md")).unwrap();
        assert_eq!(post.title, "Hello");
        assert!(
            post.body_md.contains("# Section One"),
            "got {:?}",
            post.body_md
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn sync_report_round_trips_through_the_query_string() {
        let report = SyncReport {
            created: 1,
            updated: 2,
            removed: 3,
            unchanged: 4,
        };
        assert_eq!(SyncReport::from_query(&report.to_query()), Some(report));
        assert_eq!(SyncReport::from_query("nonsense"), None);
        assert_eq!(SyncReport::from_query("1,2"), None);
        assert!(report.changed());
        assert!(!SyncReport::default().changed());
    }

    #[test]
    fn scans_recursively_and_skips_hidden_entries() {
        let root = std::env::temp_dir().join(format!("tblogs-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();

        std::fs::write(root.join("a.md"), "# A").unwrap();
        std::fs::write(root.join("notes/b.markdown"), "# B").unwrap();
        std::fs::write(root.join("notes/skip.txt"), "not markdown").unwrap();
        std::fs::write(root.join(".git/ignored.md"), "# Nope").unwrap();
        std::fs::write(root.join(".hidden.md"), "# Nope").unwrap();

        let files = scan(&root).unwrap();
        let names: Vec<&str> = files.iter().map(|f| f.rel_path.as_str()).collect();
        assert_eq!(names, vec!["a.md", "notes/b.markdown"]);

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A scratch content directory, removed on drop.
    struct Scratch {
        dir: std::path::PathBuf,
    }

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("tblogs-sync-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("notes")).unwrap();
            Self { dir }
        }

        fn write(&self, rel: &str, contents: &str) {
            let path = self.dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    async fn empty_pool() -> SqlitePool {
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        use std::str::FromStr;

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::from_str("sqlite::memory:")
                    .unwrap()
                    .foreign_keys(true),
            )
            .await
            .unwrap();

        db::create_schema(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn imports_a_directory_then_tracks_edits_and_deletions() {
        let scratch = Scratch::new("lifecycle");
        let pool = empty_pool().await;

        scratch.write("2026-01-01-hello.md", "# Hello\n\nFirst body.\n");
        scratch.write(
            "notes/nested.md",
            "---\ntitle: Nested\ntags: [alpha, beta]\n---\n\nNested body.\n",
        );

        // First pass imports both.
        let files = scan(&scratch.dir).unwrap();
        let report = sync(&pool, &scratch.dir, &files).await.unwrap();
        assert_eq!(report.created, 2, "{report:?}");

        // The date prefix names the slug and dates the post; the `# heading`
        // becomes the title and is stripped from the body.
        let hello = db::find_by_source(&pool, "2026-01-01-hello.md")
            .await
            .unwrap()
            .expect("hello.md should be imported");
        assert_eq!(hello.slug, "hello");
        assert_eq!(hello.title, "Hello");
        assert_eq!(hello.body_md.trim(), "First body.");
        assert!(hello.published);

        // Front matter wins where present.
        let nested = db::find_by_source(&pool, "notes/nested.md")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(nested.slug, "nested");
        assert_eq!(nested.title, "Nested");

        // A second pass over an untouched directory does nothing at all.
        let report = sync(&pool, &scratch.dir, &files).await.unwrap();
        assert_eq!(report.unchanged, 2, "{report:?}");
        assert!(!report.changed());

        // Editing a file re-imports it. The mtime is bumped explicitly so the
        // test does not depend on filesystem timestamp resolution.
        scratch.write(
            "notes/nested.md",
            "---\ntitle: Nested v2\ndraft: true\n---\n\nRewritten.\n",
        );
        let mut files = scan(&scratch.dir).unwrap();
        files
            .iter_mut()
            .find(|f| f.rel_path == "notes/nested.md")
            .unwrap()
            .mtime += 1_000_000_000;

        let report = sync(&pool, &scratch.dir, &files).await.unwrap();
        assert_eq!(report.updated, 1, "{report:?}");

        let nested = db::find_by_source(&pool, "notes/nested.md")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(nested.title, "Nested v2");
        assert!(nested.body_md.contains("Rewritten"));
        // `draft: true` pulls it out of the public listing.
        assert!(!nested.published);
        assert!(
            db::find_published_by_slug(&pool, "nested")
                .await
                .unwrap()
                .is_none()
        );

        // Deleting the file removes its post ...
        std::fs::remove_file(scratch.dir.join("2026-01-01-hello.md")).unwrap();
        let files = scan(&scratch.dir).unwrap();
        let report = sync(&pool, &scratch.dir, &files).await.unwrap();
        assert_eq!(report.removed, 1, "{report:?}");
        assert!(
            db::find_by_source(&pool, "2026-01-01-hello.md")
                .await
                .unwrap()
                .is_none()
        );

        // The other file is untouched by that removal.
        assert!(
            db::find_by_source(&pool, "notes/nested.md")
                .await
                .unwrap()
                .is_some()
        );

        std::fs::remove_dir_all(&scratch.dir).unwrap();
    }

    #[tokio::test]
    async fn colliding_slugs_from_two_files_are_disambiguated() {
        let scratch = Scratch::new("collide");
        let pool = empty_pool().await;

        // Same file name in two directories.
        scratch.write("a/post.md", "# A\n\nBody A.\n");
        scratch.write("b/post.md", "# B\n\nBody B.\n");

        let files = scan(&scratch.dir).unwrap();
        let report = sync(&pool, &scratch.dir, &files).await.unwrap();
        assert_eq!(report.created, 2, "{report:?}");

        let a = db::find_by_source(&pool, "a/post.md")
            .await
            .unwrap()
            .unwrap();
        let b = db::find_by_source(&pool, "b/post.md")
            .await
            .unwrap()
            .unwrap();

        // Both are reachable, and the slug stays stable across re-syncs.
        assert_ne!(a.slug, b.slug);
        let mut slugs = vec![a.slug.clone(), b.slug.clone()];
        slugs.sort();
        assert_eq!(slugs, vec!["post", "post-2"]);

        let report = sync(&pool, &scratch.dir, &files).await.unwrap();
        assert_eq!(report.unchanged, 2);

        let a_again = db::find_by_source(&pool, "a/post.md")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(a_again.slug, a.slug);

        std::fs::remove_dir_all(&scratch.dir).unwrap();
    }
}
