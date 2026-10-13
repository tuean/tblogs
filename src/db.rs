//! SQLite persistence.
//!
//! All SQL in this module is a static string literal. sqlx 0.9 requires a
//! `SqlSafeStr` bound, which a `&'static str` satisfies but a runtime-built
//! `String` does not, so the column list is shared through a macro rather than
//! by concatenating at runtime.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use sqlx::{
    SqliteConnection, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

use crate::{
    models::{Post, Settings, SourcedPost, Tag, TagCount},
    slug,
};

/// The `posts` column list, shared by every query that decodes a whole row.
macro_rules! post_columns {
    () => {
        "id, slug, title, summary, body_md, published, created_at, updated_at, \
         published_at, source_path"
    };
}

/// The same list qualified with the alias `p`.
///
/// Needed wherever `posts` is joined to `tags`: both tables have an `id`, so
/// unqualified names would be ambiguous.
macro_rules! post_columns_p {
    () => {
        "p.id, p.slug, p.title, p.summary, p.body_md, p.published, \
         p.created_at, p.updated_at, p.published_at, p.source_path"
    };
}

/// Current time as an ISO-8601 UTC string, matching the stored format.
pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Compact local timestamp, used to build a fallback slug.
/// Read-only is the whole point of the split. The server owns no content, so it
/// must not be able to create, alter or corrupt it — not even by accident.
///
/// The published file is deliberately a plain rollback-journal database with no
/// write-ahead log: opening a WAL database read-only needs write access to the
/// `-shm` file, which a read-only deployment cannot be relied on to have. See
/// [`finalize_package`].
pub async fn open_read_only(path: &Path) -> Result<SqlitePool> {
    if !path.is_file() {
        anyhow::bail!("cannot open {}: file not found", path.display());
    }

    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .with_context(|| format!("could not open {} read-only", path.display()))?;

    Ok(pool)
}

/// Open (or create) the database used while building a package.
///
/// Write-ahead logging makes the import fast; [`finalize_package`] converts the
/// result back to a single self-contained file afterwards.
pub async fn open_for_build(path: &Path) -> Result<SqlitePool> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }

    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        // Required for the ON DELETE CASCADE on post_tags to actually fire.
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .with_context(|| format!("could not open {} for building", path.display()))?;

    apply_migrations(&pool).await?;

    Ok(pool)
}

/// Turn a freshly built database into a single portable file.
///
/// Folds the write-ahead log back into the main file, switches to the rollback
/// journal so the result can be opened read-only, and compacts it.
///
/// Takes a path rather than a pool on purpose: changing `journal_mode` needs
/// exclusive access, so every other connection — including the one the build
/// was using — has to be closed first. This opens its own single connection and
/// closes it again.
pub async fn finalize_package(path: &Path) -> Result<()> {
    let options = SqliteConnectOptions::new().filename(path);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .with_context(|| format!("could not reopen {} to finalize it", path.display()))?;

    let result = finalize_on(&pool).await;
    pool.close().await;
    result
}

async fn finalize_on(pool: &SqlitePool) -> Result<()> {
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(pool)
        .await
        .context("could not checkpoint the write-ahead log")?;

    // `journal_mode` reports the resulting mode as a row, so it has to be
    // queried rather than executed.
    let (mode,): (String,) = sqlx::query_as("PRAGMA journal_mode = DELETE")
        .fetch_one(pool)
        .await
        .context("could not switch the journal mode")?;

    if !mode.eq_ignore_ascii_case("delete") {
        anyhow::bail!("could not switch the journal mode; still {mode}");
    }

    sqlx::query("VACUUM")
        .execute(pool)
        .await
        .context("vacuum failed")?;

    Ok(())
}

/// Read the site settings stored in a package.
pub async fn load_settings(pool: &SqlitePool) -> Result<Settings> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT key, value FROM settings")
        .fetch_all(pool)
        .await
        .context("could not read the settings table")?;

    let mut settings = Settings::default();
    for (key, value) in rows {
        match key.as_str() {
            "site_title" => settings.site_title = value,
            "site_description" => settings.site_description = value,
            "site_url" => settings.site_url = value.trim_end_matches('/').to_string(),
            "built_at" => settings.built_at = value,
            _ => {}
        }
    }

    Ok(settings)
}

/// Write the site settings into a package being built.
pub async fn save_settings(pool: &SqlitePool, settings: &Settings) -> Result<()> {
    let mut tx = pool.begin().await?;

    for (key, value) in [
        ("site_title", settings.site_title.as_str()),
        ("site_description", settings.site_description.as_str()),
        ("site_url", settings.site_url.as_str()),
        ("built_at", settings.built_at.as_str()),
    ] {
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES (?, ?)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

/// Apply every pending migration.
///
/// The migration directory is resolved at runtime with `Migrator::new` rather
/// than through the `sqlx::migrate!` macro, so the sqlx-macros proc macro is
/// not needed at all. See the note on `FromRow` in `models.rs`.
pub(crate) async fn apply_migrations(pool: &SqlitePool) -> Result<()> {
    let dir = migrations_dir()?;

    let migrator = sqlx::migrate::Migrator::new(dir.as_path())
        .await
        .with_context(|| format!("could not read {}", dir.display()))?;

    migrator
        .run(pool)
        .await
        .context("database migration failed")?;

    Ok(())
}

/// Locate the directory holding the `.sql` migrations.
///
/// Checked in order: `TBLOGS_MIGRATIONS_DIR`, then `./migrations` relative to
/// the working directory (which the systemd unit pins to the install root),
/// then `migrations/` beside the executable.
///
/// A compile-time path would be wrong here: the binary is normally built on one
/// machine and copied to another, where the build directory does not exist.
fn migrations_dir() -> Result<PathBuf> {
    let mut candidates = Vec::new();

    if let Ok(dir) = std::env::var("TBLOGS_MIGRATIONS_DIR")
        && !dir.trim().is_empty()
    {
        candidates.push(PathBuf::from(dir));
    }

    candidates.push(PathBuf::from("migrations"));

    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        candidates.push(parent.join("migrations"));
    }

    for candidate in &candidates {
        if candidate.is_dir() {
            return Ok(candidate.clone());
        }
    }

    let looked_in = candidates
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");

    anyhow::bail!("no migrations directory found; looked in: {looked_in}")
}

// ---------------------------------------------------------------- queries

/// Published posts, newest first.
pub async fn list_published(pool: &SqlitePool, limit: i64, offset: i64) -> Result<Vec<Post>> {
    let rows = sqlx::query_as::<_, Post>(concat!(
        "SELECT ",
        post_columns!(),
        " FROM posts WHERE published = 1
          ORDER BY COALESCE(published_at, created_at) DESC, id DESC
          LIMIT ? OFFSET ?"
    ))
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

/// Every post, drafts included, for the admin list.
pub async fn list_all(pool: &SqlitePool) -> Result<Vec<Post>> {
    let rows = sqlx::query_as::<_, Post>(concat!(
        "SELECT ",
        post_columns!(),
        " FROM posts ORDER BY updated_at DESC, id DESC"
    ))
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

pub async fn count_published(pool: &SqlitePool) -> Result<i64> {
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM posts WHERE published = 1")
        .fetch_one(pool)
        .await?;
    Ok(count)
}

pub async fn find_published_by_slug(pool: &SqlitePool, slug: &str) -> Result<Option<Post>> {
    let row = sqlx::query_as::<_, Post>(concat!(
        "SELECT ",
        post_columns!(),
        " FROM posts WHERE slug = ? AND published = 1"
    ))
    .bind(slug)
    .fetch_optional(pool)
    .await?;

    Ok(row)
}

pub async fn find_by_id(pool: &SqlitePool, id: i64) -> Result<Option<Post>> {
    let row = sqlx::query_as::<_, Post>(concat!(
        "SELECT ",
        post_columns!(),
        " FROM posts WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(row)
}

/// Tags for every post, keyed by post id.
///
/// Deliberately unfiltered: a lean blog has few tags, and loading them all
/// avoids building a dynamic `IN (...)` list for each page.
pub async fn tags_by_post(pool: &SqlitePool) -> Result<HashMap<i64, Vec<Tag>>> {
    let rows: Vec<(i64, i64, String, String)> = sqlx::query_as(
        "SELECT pt.post_id, t.id, t.name, t.slug
           FROM post_tags pt
           JOIN tags t ON t.id = pt.tag_id
          ORDER BY t.name COLLATE NOCASE",
    )
    .fetch_all(pool)
    .await?;

    let mut map: HashMap<i64, Vec<Tag>> = HashMap::new();
    for (post_id, id, name, slug) in rows {
        map.entry(post_id).or_default().push(Tag { id, name, slug });
    }

    Ok(map)
}

/// Tags that have at least one published post, most used first.
pub async fn list_tags_with_counts(pool: &SqlitePool) -> Result<Vec<TagCount>> {
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT t.name, t.slug, COUNT(p.id) AS cnt
           FROM tags t
           JOIN post_tags pt ON pt.tag_id = t.id
           JOIN posts p      ON p.id = pt.post_id AND p.published = 1
          GROUP BY t.id
          ORDER BY cnt DESC, t.name COLLATE NOCASE",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(name, slug, count)| TagCount { name, slug, count })
        .collect())
}

pub async fn find_tag_by_slug(pool: &SqlitePool, slug: &str) -> Result<Option<Tag>> {
    let row = sqlx::query_as::<_, Tag>("SELECT id, name, slug FROM tags WHERE slug = ?")
        .bind(slug)
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

pub async fn list_published_by_tag(
    pool: &SqlitePool,
    tag_slug: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<Post>> {
    let rows = sqlx::query_as::<_, Post>(concat!(
        "SELECT ",
        post_columns_p!(),
        " FROM posts p
           JOIN post_tags pt ON pt.post_id = p.id
           JOIN tags t       ON t.id = pt.tag_id
          WHERE p.published = 1 AND t.slug = ?
          ORDER BY COALESCE(p.published_at, p.created_at) DESC, p.id DESC
          LIMIT ? OFFSET ?"
    ))
    .bind(tag_slug)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

pub async fn count_published_by_tag(pool: &SqlitePool, tag_slug: &str) -> Result<i64> {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)
           FROM posts p
           JOIN post_tags pt ON pt.post_id = p.id
           JOIN tags t       ON t.id = pt.tag_id
          WHERE p.published = 1 AND t.slug = ?",
    )
    .bind(tag_slug)
    .fetch_one(pool)
    .await?;

    Ok(count)
}

// ---------------------------------------------------------------- writes

/// Remove a post and any tags it left orphaned.
pub async fn delete_post(pool: &SqlitePool, id: i64) -> Result<()> {
    let mut tx = pool.begin().await?;

    sqlx::query("DELETE FROM posts WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;

    // post_tags cascades, which can leave tags with no posts behind.
    sqlx::query("DELETE FROM tags WHERE id NOT IN (SELECT tag_id FROM post_tags)")
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(())
}

// ------------------------------------------------------- file-backed posts

/// What a sync pass did to one post.
///
/// There is no `Unchanged` case: a file whose modification time still matches
/// is skipped before this is ever called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOutcome {
    Created,
    Updated,
}

/// Insert or update the post backed by a particular source file.
///
/// The file is authoritative: title, body, tags, slug and publication state are
/// all taken from it. Only `created_at` and an already-set `published_at` are
/// preserved, so editing a file does not shuffle it to the top of the feed.
pub async fn sync_sourced_post(pool: &SqlitePool, input: &SourcedPost) -> Result<SyncOutcome> {
    let existing = find_by_source(pool, &input.source_path).await?;
    let exclude = existing.as_ref().map(|post| post.id);

    let slug = unique_post_slug(pool, &input.slug_base, exclude).await?;
    let now = now_iso();

    let published_at = if input.published {
        existing
            .as_ref()
            .and_then(|post| post.published_at.clone())
            .or_else(|| input.date_hint.clone())
            .or_else(|| Some(now.clone()))
    } else {
        None
    };

    let created_at = existing
        .as_ref()
        .map(|post| post.created_at.clone())
        .or_else(|| input.date_hint.clone())
        .unwrap_or_else(|| now.clone());

    let mut tx = pool.begin().await?;

    let id = match &existing {
        Some(post) => {
            sqlx::query(
                "UPDATE posts
                    SET slug = ?, title = ?, summary = ?, body_md = ?, published = ?,
                        created_at = ?, updated_at = ?, published_at = ?,
                        source_mtime = ?
                  WHERE id = ?",
            )
            .bind(&slug)
            .bind(&input.title)
            .bind(&input.summary)
            .bind(&input.body_md)
            .bind(input.published)
            .bind(&created_at)
            .bind(&input.updated_at)
            .bind(&published_at)
            .bind(input.source_mtime)
            .bind(post.id)
            .execute(&mut *tx)
            .await?;

            post.id
        }
        None => {
            let result = sqlx::query(
                "INSERT INTO posts
                     (slug, title, summary, body_md, published, created_at, updated_at,
                      published_at, source_path, source_mtime)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&slug)
            .bind(&input.title)
            .bind(&input.summary)
            .bind(&input.body_md)
            .bind(input.published)
            .bind(&created_at)
            .bind(&input.updated_at)
            .bind(&published_at)
            .bind(&input.source_path)
            .bind(input.source_mtime)
            .execute(&mut *tx)
            .await?;

            result.last_insert_rowid()
        }
    };

    sync_tags(&mut tx, id, &input.tags).await?;
    tx.commit().await?;

    Ok(match existing {
        Some(_) => SyncOutcome::Updated,
        None => SyncOutcome::Created,
    })
}

/// The post backed by a given source file, if it has been imported.
pub async fn find_by_source(pool: &SqlitePool, source_path: &str) -> Result<Option<Post>> {
    let row = sqlx::query_as::<_, Post>(concat!(
        "SELECT ",
        post_columns!(),
        " FROM posts WHERE source_path = ?"
    ))
    .bind(source_path)
    .fetch_optional(pool)
    .await?;

    Ok(row)
}

/// `(source_path, source_mtime)` for every imported post.
///
/// Used to skip files whose modification time has not changed.
pub async fn list_source_mtimes(pool: &SqlitePool) -> Result<Vec<(String, i64)>> {
    let rows: Vec<(String, i64)> = sqlx::query_as("SELECT source_path, source_mtime FROM posts")
        .fetch_all(pool)
        .await?;

    Ok(rows)
}

/// `(id, source_path)` for every post, used to detect deleted files.
pub async fn list_sourced_posts(pool: &SqlitePool) -> Result<Vec<(i64, String)>> {
    let rows: Vec<(i64, String)> = sqlx::query_as("SELECT id, source_path FROM posts")
        .fetch_all(pool)
        .await?;

    Ok(rows)
}

async fn sync_tags(conn: &mut SqliteConnection, post_id: i64, names: &[String]) -> Result<()> {
    sqlx::query("DELETE FROM post_tags WHERE post_id = ?")
        .bind(post_id)
        .execute(&mut *conn)
        .await?;

    for name in names {
        let tag_id = upsert_tag(conn, name).await?;
        sqlx::query("INSERT OR IGNORE INTO post_tags (post_id, tag_id) VALUES (?, ?)")
            .bind(post_id)
            .bind(tag_id)
            .execute(&mut *conn)
            .await?;
    }

    Ok(())
}

/// Find a tag by name or create it, returning its id.
async fn upsert_tag(conn: &mut SqliteConnection, name: &str) -> Result<i64> {
    // Keep tag names to a sane length; they are rendered in listings.
    let name: String = name.trim().chars().take(50).collect();
    if name.is_empty() {
        anyhow::bail!("refusing to create an empty tag");
    }

    if let Some((id,)) = sqlx::query_as::<_, (i64,)>("SELECT id FROM tags WHERE name = ?")
        .bind(&name)
        .fetch_optional(&mut *conn)
        .await?
    {
        return Ok(id);
    }

    let base = slug::slugify_or(&name, "tag");
    let mut candidate = base.clone();
    let mut suffix = 2;

    loop {
        let taken: Option<(i64,)> = sqlx::query_as("SELECT id FROM tags WHERE slug = ?")
            .bind(&candidate)
            .fetch_optional(&mut *conn)
            .await?;

        if taken.is_none() {
            break;
        }

        candidate = format!("{base}-{suffix}");
        suffix += 1;
        if suffix > 1000 {
            anyhow::bail!("too many tags sharing the slug {base:?}");
        }
    }

    let result = sqlx::query("INSERT INTO tags (name, slug) VALUES (?, ?)")
        .bind(&name)
        .bind(&candidate)
        .execute(&mut *conn)
        .await?;

    Ok(result.last_insert_rowid())
}

/// Find a slug not yet used by another post.
async fn unique_post_slug(
    pool: &SqlitePool,
    base: &str,
    exclude_id: Option<i64>,
) -> Result<String> {
    let mut candidate = base.to_string();
    let mut suffix = 2;

    loop {
        let taken: Option<(i64,)> =
            sqlx::query_as("SELECT id FROM posts WHERE slug = ? AND (? IS NULL OR id <> ?)")
                .bind(&candidate)
                .bind(exclude_id)
                .bind(exclude_id)
                .fetch_optional(pool)
                .await?;

        if taken.is_none() {
            return Ok(candidate);
        }

        candidate = format!("{base}-{suffix}");
        suffix += 1;
        if suffix > 1000 {
            anyhow::bail!("too many posts sharing the slug {base:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use crate::models::normalise_tags;

    /// An in-memory database with the schema applied.
    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::from_str("sqlite::memory:")
                    .unwrap()
                    .foreign_keys(true),
            )
            .await
            .unwrap();

        apply_migrations(&pool).await.unwrap();
        pool
    }

    /// A post as the build step would produce it.
    ///
    /// The summary is derived the same way `content::build_post` does it, since
    /// that is now the only place summaries are made.
    fn sourced(source: &str, title: &str, body: &str, tags: &str, published: bool) -> SourcedPost {
        let names: Vec<String> = tags
            .split(',')
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect();

        SourcedPost {
            source_path: source.to_string(),
            source_mtime: 1,
            title: title.to_string(),
            slug_base: slug::slugify(source.trim_end_matches(".md")),
            summary: crate::markdown::excerpt(body, 160),
            body_md: body.to_string(),
            published,
            date_hint: None,
            updated_at: now_iso(),
            tags: normalise_tags(&names),
        }
    }

    /// Import a post and return its id.
    async fn import(pool: &SqlitePool, post: &SourcedPost) -> i64 {
        sync_sourced_post(pool, post).await.unwrap();
        find_by_source(pool, &post.source_path)
            .await
            .unwrap()
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn drafts_stay_out_of_the_public_list() {
        let pool = test_pool().await;
        import(&pool, &sourced("live.md", "公开", "# hi", "", true)).await;
        import(&pool, &sourced("draft.md", "草稿", "# hi", "", false)).await;

        let listed = list_published(&pool, 10, 0).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "公开");
        assert_eq!(count_published(&pool).await.unwrap(), 1);
        assert_eq!(list_all(&pool).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn duplicate_slug_bases_are_disambiguated() {
        let pool = test_pool().await;

        // Two files whose derived slug would collide.
        let mut first = sourced("a/post.md", "Same", "a", "", true);
        let mut second = sourced("b/post.md", "Same", "b", "", true);
        first.slug_base = "same".into();
        second.slug_base = "same".into();

        let first_id = import(&pool, &first).await;
        let second_id = import(&pool, &second).await;

        let a = find_by_id(&pool, first_id).await.unwrap().unwrap();
        let b = find_by_id(&pool, second_id).await.unwrap().unwrap();

        assert_eq!(a.slug, "same");
        assert_eq!(b.slug, "same-2");
    }

    #[tokio::test]
    async fn cjk_slugs_are_reachable() {
        let pool = test_pool().await;
        let id = import(
            &pool,
            &sourced("我的第一篇博客.md", "标题", "正文", "", true),
        )
        .await;

        let post = find_by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(post.slug, "我的第一篇博客");
        assert!(
            find_published_by_slug(&pool, "我的第一篇博客")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn tags_are_linked_and_orphans_are_cleaned_up() {
        let pool = test_pool().await;
        let id = import(
            &pool,
            &sourced("tagged.md", "有标签", "正文", "rust, axum", true),
        )
        .await;

        let counts = list_tags_with_counts(&pool).await.unwrap();
        assert_eq!(counts.len(), 2);

        let by_tag = list_published_by_tag(&pool, "rust", 10, 0).await.unwrap();
        assert_eq!(by_tag.len(), 1);
        assert_eq!(count_published_by_tag(&pool, "rust").await.unwrap(), 1);

        delete_post(&pool, id).await.unwrap();
        assert!(list_tags_with_counts(&pool).await.unwrap().is_empty());
        assert!(find_tag_by_slug(&pool, "rust").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn publishing_stamps_the_date_and_unpublishing_clears_it() {
        let pool = test_pool().await;

        let draft = sourced("d.md", "草稿", "正文", "", false);
        let id = import(&pool, &draft).await;
        assert!(
            find_by_id(&pool, id)
                .await
                .unwrap()
                .unwrap()
                .published_at
                .is_none()
        );

        // The same file, now published. A date hint from front matter or the
        // file name would be used; here it falls back to "now".
        let mut live = draft.clone();
        live.published = true;
        sync_sourced_post(&pool, &live).await.unwrap();

        let stamped = find_by_id(&pool, id).await.unwrap().unwrap().published_at;
        assert!(stamped.is_some(), "publishing should stamp the date");

        // And back to a draft.
        let mut again = draft.clone();
        again.published = false;
        sync_sourced_post(&pool, &again).await.unwrap();
        assert!(
            find_by_id(&pool, id)
                .await
                .unwrap()
                .unwrap()
                .published_at
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_date_hint_is_used_for_publication() {
        let pool = test_pool().await;

        let mut post = sourced("dated.md", "有日期", "正文", "", true);
        post.date_hint = Some("2026-03-04T05:06:07Z".to_string());

        let id = import(&pool, &post).await;
        let stored = find_by_id(&pool, id).await.unwrap().unwrap();

        assert_eq!(stored.published_at.as_deref(), Some("2026-03-04T05:06:07Z"));
        // created_at follows the same hint, so a feed keeps its order.
        assert_eq!(stored.created_at, "2026-03-04T05:06:07Z");
    }

    #[tokio::test]
    async fn resyncing_the_same_source_updates_rather_than_duplicating() {
        let pool = test_pool().await;

        let first = sourced("post.md", "Before", "old body", "", true);
        let id = import(&pool, &first).await;

        let mut second = sourced("post.md", "After", "new body", "tag", true);
        second.source_mtime = 2;

        let outcome = sync_sourced_post(&pool, &second).await.unwrap();
        assert_eq!(outcome, SyncOutcome::Updated);

        assert_eq!(list_all(&pool).await.unwrap().len(), 1);
        let post = find_by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(post.title, "After");
        assert_eq!(post.body_md, "new body");

        // The stored modification time moved too, so the next build can skip it.
        let mtimes = list_source_mtimes(&pool).await.unwrap();
        assert_eq!(mtimes, vec![("post.md".to_string(), 2)]);
    }

    #[tokio::test]
    async fn publication_date_survives_an_edit() {
        let pool = test_pool().await;

        let mut first = sourced("post.md", "Title", "body", "", true);
        first.date_hint = Some("2026-01-01T00:00:00Z".to_string());
        let id = import(&pool, &first).await;

        // Editing must not shuffle the post to the top of the feed.
        let mut second = sourced("post.md", "Title v2", "body v2", "", true);
        second.source_mtime = 2;
        sync_sourced_post(&pool, &second).await.unwrap();

        let post = find_by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(post.published_at.as_deref(), Some("2026-01-01T00:00:00Z"));
    }

    #[tokio::test]
    async fn settings_round_trip_through_the_database() {
        let pool = test_pool().await;

        // A database with no settings row still yields usable defaults.
        let defaults = load_settings(&pool).await.unwrap();
        assert!(!defaults.site_title.is_empty());

        let settings = Settings {
            site_title: "标题".into(),
            site_description: "描述".into(),
            site_url: "https://example.test".into(),
            built_at: "2026-01-01T00:00:00Z".into(),
        };
        save_settings(&pool, &settings).await.unwrap();

        // Saving twice must update rather than duplicate.
        save_settings(&pool, &settings).await.unwrap();
        assert_eq!(load_settings(&pool).await.unwrap(), settings);
    }

    #[tokio::test]
    async fn pagination_slices_the_result_set() {
        let pool = test_pool().await;
        for i in 0..5 {
            let post = sourced(
                &format!("post-{i}.md"),
                &format!("文章 {i}"),
                "正文",
                "",
                true,
            );
            import(&pool, &post).await;
        }

        assert_eq!(list_published(&pool, 2, 0).await.unwrap().len(), 2);
        assert_eq!(list_published(&pool, 2, 4).await.unwrap().len(), 1);
        assert_eq!(list_published(&pool, 2, 99).await.unwrap().len(), 0);
    }
}
