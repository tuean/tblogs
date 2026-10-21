//! SQLite persistence.
//!
//! All SQL in this module is a static string literal. sqlx 0.9 requires a
//! `SqlSafeStr` bound, which a `&'static str` satisfies but a runtime-built
//! `String` does not, so the column list is shared through a macro rather than
//! by concatenating at runtime.

use std::{collections::HashMap, path::Path};

use anyhow::{Context, Result};
use sqlx::{
    SqliteConnection, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

use crate::{
    models::{Post, SeriesSummary, Settings, SourcedPost, Tag, TagCount},
    slug,
};

/// The `posts` column list, shared by every query that decodes a whole row.
macro_rules! post_columns {
    () => {
        "id, slug, title, summary, body_md, published, created_at, updated_at, \
         published_at, source_path, cover_image, series, series_slug, chapter_order"
    };
}

/// The same list qualified with the alias `p`.
///
/// Needed wherever `posts` is joined to `tags`: both tables have an `id`, so
/// unqualified names would be ambiguous.
macro_rules! post_columns_p {
    () => {
        "p.id, p.slug, p.title, p.summary, p.body_md, p.published, \
         p.created_at, p.updated_at, p.published_at, p.source_path, p.cover_image, \
         p.series, p.series_slug, p.chapter_order"
    };
}

// The effective position of a chapter within its series.
//
// `chapter_order` is nullable and SQLite sorts NULL before every number, which
// would put an unordered chapter first *by accident of the storage engine*.
// Every series query therefore spells out the same `COALESCE` with the sentinel
// from `models::CHAPTER_ORDER_LAST`, so an unordered chapter sorts last: one
// hand-broken file must not insert a chapter in front of chapter one.
//
// The number is written literally because `concat!` only accepts literals.
// Three occurrences, one meaning, and a test that pins it to the constant.

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

    create_schema(&pool).await?;

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

/// The package schema, compiled into the binary.
///
/// The database is recreated from scratch on every build, so there is exactly
/// one schema and no migration history to track.
///
/// Embedding it makes the client a single portable executable: nothing has to
/// be located on disk at run time. That matters as soon as it is installed
/// somewhere like `~/.cargo/bin`, where no sibling `migrations/` directory
/// exists — which is exactly how the earlier runtime lookup failed.
const SCHEMA: &str = include_str!("../migrations/0001_schema.sql");

/// Create the package schema in a freshly opened database.
pub(crate) async fn create_schema(pool: &SqlitePool) -> Result<()> {
    sqlx::raw_sql(SCHEMA)
        .execute(pool)
        .await
        .context("could not create the package schema")?;

    Ok(())
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

// ---------------------------------------------------------------- series
//
// Chapters are ordinary posts that happen to carry a series name and a
// position. Nothing here changes how they appear in the feed, on the index or
// under a tag: those all stay newest-first. Series ordering is a separate axis,
// used only by a series page and by a chapter's own navigation.

/// Every series with its chapter count, most recently updated first.
pub async fn list_series(pool: &SqlitePool) -> Result<Vec<SeriesSummary>> {
    let rows = sqlx::query_as::<_, SeriesSummary>(
        "SELECT series AS title, series_slug AS slug, COUNT(*) AS chapter_count
           FROM posts
          WHERE published = 1 AND series_slug IS NOT NULL
          GROUP BY series_slug, series
          ORDER BY MAX(COALESCE(published_at, created_at)) DESC, series",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

/// One series, or `None` when no published post claims that slug.
pub async fn find_series(pool: &SqlitePool, series_slug: &str) -> Result<Option<SeriesSummary>> {
    let row = sqlx::query_as::<_, SeriesSummary>(
        "SELECT series AS title, series_slug AS slug, COUNT(*) AS chapter_count
           FROM posts
          WHERE published = 1 AND series_slug = ?
          GROUP BY series_slug, series",
    )
    .bind(series_slug)
    .fetch_optional(pool)
    .await?;

    Ok(row)
}

/// The chapters of one series, in reading order.
///
/// `chapter_order` ascending is the entire reason a series page exists: the
/// blog index and the feed stay newest-first, but a novel has to be walked from
/// the beginning. `id` breaks ties so that two chapters sharing an order still
/// render in a stable sequence instead of shuffling between requests.
pub async fn list_series_chapters(pool: &SqlitePool, series_slug: &str) -> Result<Vec<Post>> {
    let rows = sqlx::query_as::<_, Post>(concat!(
        "SELECT ",
        post_columns!(),
        " FROM posts WHERE published = 1 AND series_slug = ?
          ORDER BY COALESCE(chapter_order, 9223372036854775807) ASC, id ASC"
    ))
    .bind(series_slug)
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

/// The chapters immediately before and after `post` within its series.
///
/// Neighbours are by position, not by publication time: chapters are written
/// and published in whatever order suits the author, but read in one. The
/// comparison includes `id` so that chapters sharing an `order` still have a
/// defined predecessor and successor rather than both being "next".
pub async fn chapter_neighbours(
    pool: &SqlitePool,
    post: &Post,
) -> Result<(Option<Post>, Option<Post>)> {
    let Some(series_slug) = post.series_slug.as_deref() else {
        return Ok((None, None));
    };
    let order = post.order_key();

    let previous = sqlx::query_as::<_, Post>(concat!(
        "SELECT ",
        post_columns!(),
        " FROM posts
          WHERE published = 1 AND series_slug = ?
            AND (COALESCE(chapter_order, 9223372036854775807) < ?
                 OR (COALESCE(chapter_order, 9223372036854775807) = ? AND id < ?))
          ORDER BY COALESCE(chapter_order, 9223372036854775807) DESC, id DESC
          LIMIT 1"
    ))
    .bind(series_slug)
    .bind(order)
    .bind(order)
    .bind(post.id)
    .fetch_optional(pool)
    .await?;

    let next = sqlx::query_as::<_, Post>(concat!(
        "SELECT ",
        post_columns!(),
        " FROM posts
          WHERE published = 1 AND series_slug = ?
            AND (COALESCE(chapter_order, 9223372036854775807) > ?
                 OR (COALESCE(chapter_order, 9223372036854775807) = ? AND id > ?))
          ORDER BY COALESCE(chapter_order, 9223372036854775807) ASC, id ASC
          LIMIT 1"
    ))
    .bind(series_slug)
    .bind(order)
    .bind(order)
    .bind(post.id)
    .fetch_optional(pool)
    .await?;

    Ok((previous, next))
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

    // Slugified once here so every chapter of one series agrees on its URL
    // without having to be told twice. A series name that slugifies to nothing
    // (all punctuation) leaves the slug NULL: the chapters still carry the
    // series name and stay ordered, they simply have no series page.
    let series_slug = input
        .series
        .as_deref()
        .map(slug::slugify)
        .filter(|value| !value.is_empty());

    let mut tx = pool.begin().await?;

    let id = match &existing {
        Some(post) => {
            sqlx::query(
                "UPDATE posts
                    SET slug = ?, title = ?, summary = ?, body_md = ?, published = ?,
                        created_at = ?, updated_at = ?, published_at = ?,
                        source_mtime = ?, cover_image = ?,
                        series = ?, series_slug = ?, chapter_order = ?
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
            .bind(&input.cover_image)
            .bind(&input.series)
            .bind(&series_slug)
            .bind(input.order)
            .bind(post.id)
            .execute(&mut *tx)
            .await?;

            post.id
        }
        None => {
            let result = sqlx::query(
                "INSERT INTO posts
                     (slug, title, summary, body_md, published, created_at, updated_at,
                      published_at, source_path, source_mtime, cover_image,
                      series, series_slug, chapter_order)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
            .bind(&input.cover_image)
            .bind(&input.series)
            .bind(&series_slug)
            .bind(input.order)
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

        create_schema(&pool).await.unwrap();
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
            cover_image: None,
            series: None,
            order: None,
        }
    }

    /// A chapter of `series` at position `order`.
    ///
    /// Built on top of [`sourced`] so a chapter differs from an ordinary post in
    /// exactly the two fields the feature adds.
    fn chapter(source: &str, title: &str, body: &str, series: &str, order: i64) -> SourcedPost {
        SourcedPost {
            series: Some(series.to_string()),
            order: Some(order),
            ..sourced(source, title, body, "", true)
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

    // ------------------------------------------------------------ series

    /// The stored slug for a series name, as the builder computes it.
    fn series_of(pool: &SqlitePool, name: &str) -> String {
        let _ = pool;
        slug::slugify(name)
    }

    #[tokio::test]
    async fn a_series_lists_its_chapters_in_reading_order() {
        let pool = test_pool().await;
        let series = "渡记·卷一";
        let series_slug = series_of(&pool, series);

        // Imported last-chapter-first, so publication time runs exactly
        // opposite to reading order. This is the case a plain
        // `ORDER BY published_at` gets backwards.
        for (file, title, order) in [
            ("c3.md", "第三章", 3),
            ("c1.md", "第一章", 1),
            ("c2.md", "第二章", 2),
        ] {
            import(&pool, &chapter(file, title, "正文", series, order)).await;
        }

        let chapters = list_series_chapters(&pool, &series_slug).await.unwrap();
        let titles: Vec<&str> = chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["第一章", "第二章", "第三章"]);

        // The index is untouched: it stays newest-first.
        let feed = list_published(&pool, 10, 0).await.unwrap();
        assert_eq!(feed.first().map(|p| p.title.as_str()), Some("第二章"));
    }

    #[tokio::test]
    async fn chapters_sharing_an_order_still_have_one_sequence() {
        let pool = test_pool().await;
        let series = "渡记·卷一";
        let series_slug = series_of(&pool, series);

        for (file, title) in [("a.md", "甲"), ("b.md", "乙"), ("c.md", "丙")] {
            import(&pool, &chapter(file, title, "正文", series, 5)).await;
        }

        // Read twice: a tie broken by anything unstable (a hash order, a plan
        // change) would shuffle the chapters between requests.
        let first = list_series_chapters(&pool, &series_slug).await.unwrap();
        let second = list_series_chapters(&pool, &series_slug).await.unwrap();

        let titles = |posts: &[Post]| posts.iter().map(|p| p.title.clone()).collect::<Vec<_>>();
        assert_eq!(titles(&first), vec!["甲", "乙", "丙"]);
        assert_eq!(titles(&first), titles(&second));

        // And each still has a defined predecessor and successor rather than
        // both neighbours claiming to be "next".
        let middle = &first[1];
        let (prev, next) = chapter_neighbours(&pool, middle).await.unwrap();
        assert_eq!(prev.map(|p| p.title), Some("甲".to_string()));
        assert_eq!(next.map(|p| p.title), Some("丙".to_string()));
    }

    #[tokio::test]
    async fn the_ends_of_a_series_have_only_one_neighbour() {
        let pool = test_pool().await;
        let series = "渡记·卷一";
        let series_slug = series_of(&pool, series);

        for (file, title, order) in [
            ("c1.md", "第一章", 1),
            ("c2.md", "第二章", 2),
            ("c3.md", "第三章", 3),
        ] {
            import(&pool, &chapter(file, title, "正文", series, order)).await;
        }

        let chapters = list_series_chapters(&pool, &series_slug).await.unwrap();

        let (prev, next) = chapter_neighbours(&pool, &chapters[0]).await.unwrap();
        assert!(prev.is_none(), "chapter 1 has no previous");
        assert_eq!(next.map(|p| p.title), Some("第二章".to_string()));

        let (prev, next) = chapter_neighbours(&pool, &chapters[2]).await.unwrap();
        assert_eq!(prev.map(|p| p.title), Some("第二章".to_string()));
        assert!(next.is_none(), "the last chapter has no next");
    }

    #[tokio::test]
    async fn a_post_outside_a_series_gets_no_navigation() {
        let pool = test_pool().await;
        import(&pool, &sourced("plain.md", "普通文章", "正文", "", true)).await;
        // A chapter in another series must not become its neighbour either.
        import(
            &pool,
            &chapter("other.md", "别处的第一章", "正文", "另一部", 1),
        )
        .await;

        let plain = find_published_by_slug(&pool, "plain")
            .await
            .unwrap()
            .unwrap();
        assert!(plain.series.is_none());
        assert!(plain.series_slug.is_none());
        assert!(plain.chapter_order.is_none());
        // The fallback position exists, but nothing consults it for a row with
        // no series: there is no list to sort it into and no neighbours to find.
        assert_eq!(plain.order_key(), crate::models::CHAPTER_ORDER_LAST);

        let (prev, next) = chapter_neighbours(&pool, &plain).await.unwrap();
        assert!(prev.is_none() && next.is_none());
    }

    #[tokio::test]
    async fn two_series_do_not_interfere() {
        let pool = test_pool().await;
        let (one, other) = ("渡记·卷一", "渡记·卷二");
        let one_slug = series_of(&pool, one);
        let other_slug = series_of(&pool, other);

        import(&pool, &chapter("a1.md", "卷一·一", "正文", one, 1)).await;
        import(&pool, &chapter("a2.md", "卷一·二", "正文", one, 2)).await;
        import(&pool, &chapter("b1.md", "卷二·一", "正文", other, 1)).await;

        let first = list_series_chapters(&pool, &one_slug).await.unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(first[1].title, "卷一·二");

        let second = list_series_chapters(&pool, &other_slug).await.unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].title, "卷二·一");

        // The last chapter of volume one must not point into volume two.
        let (_, next) = chapter_neighbours(&pool, &first[1]).await.unwrap();
        assert!(next.is_none(), "series boundaries must hold");

        // Each series is counted and located separately. Compared as a set:
        // every import in this test shares a timestamp, so which series comes
        // first is the alphabetical tiebreak, not a property worth asserting
        // here — that is covered by the ordering test below.
        let all = list_series(&pool).await.unwrap();
        assert_eq!(all.len(), 2);
        let mut counts: Vec<(String, i64)> = all
            .iter()
            .map(|s| (s.title.clone(), s.chapter_count))
            .collect();
        counts.sort();
        assert_eq!(counts, vec![(one.to_string(), 2), (other.to_string(), 1)]);

        let found = find_series(&pool, &one_slug).await.unwrap().unwrap();
        assert_eq!(found.title, one);
        assert_eq!(found.chapter_count, 2);

        assert!(find_series(&pool, "没有这一部").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn the_series_index_lists_the_most_recently_updated_first() {
        let pool = test_pool().await;

        // Explicit dates, because the ordering has to be decided by the data
        // rather than by how fast the two imports happened to run.
        let mut old = chapter("a1.md", "旧作·一", "正文", "旧作", 1);
        old.date_hint = Some("2026-01-01T00:00:00Z".into());
        import(&pool, &old).await;

        let mut new = chapter("b1.md", "新作·一", "正文", "新作", 1);
        new.date_hint = Some("2026-06-01T00:00:00Z".into());
        import(&pool, &new).await;

        let titles: Vec<String> = list_series(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.title)
            .collect();
        assert_eq!(titles, vec!["新作", "旧作"]);
    }

    #[tokio::test]
    async fn an_unordered_chapter_sorts_last_and_cannot_break_the_sequence() {
        let pool = test_pool().await;
        let series = "渡记·卷一";
        let series_slug = series_of(&pool, series);

        import(&pool, &chapter("c1.md", "第一章", "正文", series, 1)).await;
        import(&pool, &chapter("c2.md", "第二章", "正文", series, 2)).await;
        // Front matter whose `order:` did not survive parsing.
        let mut unordered = chapter("c9.md", "忘了编号", "正文", series, 1);
        unordered.order = None;
        import(&pool, &unordered).await;

        let chapters = list_series_chapters(&pool, &series_slug).await.unwrap();
        let titles: Vec<&str> = chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["第一章", "第二章", "忘了编号"]);

        // The important part: the first chapter still has nothing before it, so
        // a reader following the navigation is never sent backwards into the
        // unnumbered chapter.
        let (prev, next) = chapter_neighbours(&pool, &chapters[0]).await.unwrap();
        assert!(
            prev.is_none(),
            "one broken file must not give chapter 1 a previous"
        );
        assert_eq!(next.map(|p| p.title), Some("第二章".to_string()));

        // The unnumbered chapter is the last one, and has no next.
        let (prev, next) = chapter_neighbours(&pool, &chapters[2]).await.unwrap();
        assert_eq!(prev.map(|p| p.title), Some("第二章".to_string()));
        assert!(next.is_none());
    }

    #[tokio::test]
    async fn the_order_sentinel_sorts_after_any_realistic_position() {
        let pool = test_pool().await;
        let series = "渡记·卷一";
        let series_slug = series_of(&pool, series);

        // An absurd but real position, to show the sentinel is above it.
        import(
            &pool,
            &chapter("big.md", "很大的编号", "正文", series, i64::MAX - 1),
        )
        .await;
        let mut unordered = chapter("none.md", "没有编号", "正文", series, 1);
        unordered.order = None;
        import(&pool, &unordered).await;

        let chapters = list_series_chapters(&pool, &series_slug).await.unwrap();
        assert_eq!(chapters[0].title, "很大的编号");
        assert_eq!(chapters[1].title, "没有编号");

        // And the model agrees with the SQL about which position that is.
        assert_eq!(chapters[1].order_key(), crate::models::CHAPTER_ORDER_LAST);
    }

    #[tokio::test]
    async fn a_series_name_that_slugifies_to_nothing_has_no_page() {
        let pool = test_pool().await;

        // The chapters still carry the name and stay ordered; they simply have
        // no URL to live at, which is better than a `/series/` index page.
        import(&pool, &chapter("a.md", "甲", "正文", "···", 1)).await;
        import(&pool, &chapter("b.md", "乙", "正文", "···", 2)).await;

        let post = find_published_by_slug(&pool, "a").await.unwrap().unwrap();
        assert_eq!(post.series.as_deref(), Some("···"));
        assert!(post.series_slug.is_none());

        assert!(list_series(&pool).await.unwrap().is_empty());
        let (prev, next) = chapter_neighbours(&pool, &post).await.unwrap();
        assert!(prev.is_none() && next.is_none());
    }

    #[tokio::test]
    async fn a_draft_chapter_stays_out_of_its_series() {
        let pool = test_pool().await;
        let series = "渡记·卷一";
        let series_slug = series_of(&pool, series);

        import(&pool, &chapter("c1.md", "第一章", "正文", series, 1)).await;
        let mut draft = chapter("c2.md", "第二章（未完成）", "正文", series, 2);
        draft.published = false;
        import(&pool, &draft).await;

        let chapters = list_series_chapters(&pool, &series_slug).await.unwrap();
        assert_eq!(
            chapters.len(),
            1,
            "a draft is not part of the public series"
        );

        // The published first chapter therefore has no next yet.
        let (_, next) = chapter_neighbours(&pool, &chapters[0]).await.unwrap();
        assert!(next.is_none());

        assert_eq!(
            find_series(&pool, &series_slug)
                .await
                .unwrap()
                .unwrap()
                .chapter_count,
            1
        );
    }
}
