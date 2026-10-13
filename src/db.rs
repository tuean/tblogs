//! SQLite persistence.
//!
//! All SQL in this module is a static string literal. sqlx 0.9 requires a
//! `SqlSafeStr` bound, which a `&'static str` satisfies but a runtime-built
//! `String` does not, so the column list is shared through a macro rather than
//! by concatenating at runtime.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{Context, Result};
use sqlx::{
    SqliteConnection, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

use crate::{
    config::Config,
    markdown,
    models::{Post, PostForm, SourcedPost, Tag, TagCount},
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
fn fallback_seed() -> String {
    chrono::Local::now().format("%Y%m%d%H%M%S").to_string()
}

/// Open the database, creating the file and parent directory if needed, then
/// apply any pending migrations.
pub async fn init(cfg: &Config) -> Result<SqlitePool> {
    ensure_parent_dir(&cfg.database_url)?;

    let options = SqliteConnectOptions::from_str(&cfg.database_url)
        .with_context(|| format!("invalid TBLOGS_DATABASE_URL: {}", cfg.database_url))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        // Required for the ON DELETE CASCADE on post_tags to actually fire.
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .context("could not open the SQLite database")?;

    apply_migrations(&pool).await?;

    Ok(pool)
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

/// Create the directory holding the SQLite file, so a fresh clone works
/// without a manual `mkdir data`.
fn ensure_parent_dir(database_url: &str) -> Result<()> {
    let without_scheme = database_url
        .strip_prefix("sqlite://")
        .or_else(|| database_url.strip_prefix("sqlite:"))
        .unwrap_or(database_url);

    let path = without_scheme.split('?').next().unwrap_or(without_scheme);

    if path.is_empty() || path == ":memory:" {
        return Ok(());
    }

    if let Some(parent) = Path::new(path).parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }

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

/// Insert a post, returning its new id.
pub async fn create_post(pool: &SqlitePool, form: &PostForm) -> Result<i64> {
    let now = now_iso();
    let summary = effective_summary(form);

    let base = if form.slug.trim().is_empty() {
        slug::slugify_or(&form.title, &fallback_seed())
    } else {
        slug::slugify_or(&form.slug, &fallback_seed())
    };
    let slug = unique_post_slug(pool, &base, None).await?;

    let published = form.is_publish();
    let published_at = published.then(|| now.clone());

    let mut tx = pool.begin().await?;

    let result = sqlx::query(
        "INSERT INTO posts
             (slug, title, summary, body_md, published, created_at, updated_at, published_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&slug)
    .bind(form.title.trim())
    .bind(&summary)
    .bind(&form.body_md)
    .bind(published)
    .bind(&now)
    .bind(&now)
    .bind(&published_at)
    .execute(&mut *tx)
    .await?;

    let id = result.last_insert_rowid();
    sync_tags(&mut tx, id, &form.tag_names()).await?;

    tx.commit().await?;
    Ok(id)
}

/// Update an existing post.
pub async fn update_post(pool: &SqlitePool, id: i64, form: &PostForm) -> Result<()> {
    let existing = find_by_id(pool, id)
        .await?
        .with_context(|| format!("post {id} no longer exists"))?;

    let now = now_iso();
    let summary = effective_summary(form);

    let base = if form.slug.trim().is_empty() {
        let derived = slug::slugify(&form.title);
        if derived.is_empty() {
            existing.slug.clone()
        } else {
            derived
        }
    } else {
        slug::slugify_or(&form.slug, &existing.slug)
    };
    let slug = unique_post_slug(pool, &base, Some(id)).await?;

    let published = form.is_publish();

    // Stamp published_at the first time a post goes live, preserve it on
    // subsequent edits, and clear it when the post is pulled back to draft.
    let published_at = match (published, existing.published_at.clone()) {
        (true, Some(ts)) => Some(ts),
        (true, None) => Some(now.clone()),
        (false, _) => None,
    };

    let mut tx = pool.begin().await?;

    sqlx::query(
        "UPDATE posts
            SET slug = ?, title = ?, summary = ?, body_md = ?,
                published = ?, updated_at = ?, published_at = ?
          WHERE id = ?",
    )
    .bind(&slug)
    .bind(form.title.trim())
    .bind(&summary)
    .bind(&form.body_md)
    .bind(published)
    .bind(&now)
    .bind(&published_at)
    .bind(id)
    .execute(&mut *tx)
    .await?;

    sync_tags(&mut tx, id, &form.tag_names()).await?;

    tx.commit().await?;
    Ok(())
}

/// Publish or unpublish a post without touching its content.
pub async fn set_published(pool: &SqlitePool, id: i64, published: bool) -> Result<()> {
    let existing = find_by_id(pool, id)
        .await?
        .with_context(|| format!("post {id} no longer exists"))?;

    let now = now_iso();
    let published_at = if published {
        existing.published_at.or_else(|| Some(now.clone()))
    } else {
        None
    };

    sqlx::query("UPDATE posts SET published = ?, published_at = ?, updated_at = ? WHERE id = ?")
        .bind(published)
        .bind(published_at)
        .bind(&now)
        .bind(id)
        .execute(pool)
        .await?;

    Ok(())
}

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
    let rows: Vec<(String, Option<i64>)> =
        sqlx::query_as("SELECT source_path, source_mtime FROM posts WHERE source_path IS NOT NULL")
            .fetch_all(pool)
            .await?;

    Ok(rows
        .into_iter()
        .map(|(path, mtime)| (path, mtime.unwrap_or(0)))
        .collect())
}

/// `(id, source_path)` for every imported post.
pub async fn list_sourced_posts(pool: &SqlitePool) -> Result<Vec<(i64, String)>> {
    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, source_path FROM posts WHERE source_path IS NOT NULL")
            .fetch_all(pool)
            .await?;

    Ok(rows)
}

/// Summary to store: the author's own text, or one derived from the body.
fn effective_summary(form: &PostForm) -> String {
    let provided = form.summary.trim();
    if provided.is_empty() {
        markdown::excerpt(&form.body_md, 160)
    } else {
        provided.to_string()
    }
}

/// Replace a post's tag links, creating tags as needed.
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

// ---------------------------------------------------------------- demo data

/// Insert sample posts when the database is empty and seeding is enabled.
pub async fn seed_demo_if_enabled(pool: &SqlitePool, cfg: &Config) -> Result<usize> {
    if !cfg.seed_demo {
        return Ok(0);
    }

    let (existing,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM posts")
        .fetch_one(pool)
        .await?;

    if existing > 0 {
        return Ok(0);
    }

    for (title, summary, tags, body) in DEMO_POSTS {
        let form = PostForm {
            title: (*title).to_string(),
            slug: String::new(),
            summary: (*summary).to_string(),
            body_md: (*body).to_string(),
            tags: (*tags).to_string(),
            status: "published".to_string(),
        };
        create_post(pool, &form).await?;
    }

    Ok(DEMO_POSTS.len())
}

/// `(title, summary, tags, body_markdown)`
type DemoPost = (&'static str, &'static str, &'static str, &'static str);

const DEMO_POSTS: &[DemoPost] = &[
    (
        "你好，世界",
        "第一篇文章，用来验证中文标题、中文 slug 与 Markdown 渲染。",
        "随笔, Rust",
        r#"# 你好，世界

这是 **tblogs** 的第一篇文章。它同时验证了三件事：

1. 中文标题能生成可用的 URL slug；
2. Markdown 会被渲染成 HTML；
3. 标签能正确关联并出现在列表页。

## 为什么用 Rust 写博客

单文件二进制、无运行时依赖、内存安全。对于一台放在家里的小机器来说，
这些都是很实际的好处。

> 站点的所有内容都存在一个 SQLite 文件里，备份就是复制一个文件。

行内代码长这样：`cargo run`。
"#,
    ),
    (
        "Markdown 渲染能力一览",
        "表格、代码块、引用、任务列表——本文用来检查渲染器的各项扩展。",
        "Markdown, 功能",
        r#"# Markdown 渲染能力一览

## 表格

| 功能 | 状态 |
|------|------|
| 文章 CRUD | 已完成 |
| Markdown 渲染 | 已完成 |
| 标签 | 已完成 |
| RSS | 已完成 |

## 代码块

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let pool = db::init(&cfg).await?;
    serve(pool).await
}
```

## 引用与列表

> 简单的系统更容易长期维护。

- [x] 数据库迁移
- [x] 分页
- [ ] 图片上传（未在本次范围内）

~~删除线~~ 与 **粗体**、*斜体*。
"#,
    ),
    (
        "部署与 DDNS 的注意事项",
        "从内网到公网需要满足哪些条件，以及为什么企业网络通常做不到。",
        "部署, 运维",
        r#"# 部署与 DDNS 的注意事项

把服务映射到公网，需要同时满足三个条件：

1. 你**拥有**一个公网 IP（而不是运营商 NAT 之后的共享出口）；
2. 你能在**拥有该公网 IP 的路由器**上配置端口转发；
3. 运营商没有封禁 80/443 等入站端口。

DDNS 只解决第 1 条里「IP 会变」的问题——它负责把域名指向当前 IP，
**并不负责**端口转发，也无法绕过运营商 NAT。

## 企业网络为什么不行

办公网络里，你看到的公网 IP 属于公司的 NAT 出口，由整栋楼共用：

- 你没有那台出口设备的配置权限；
- 企业防火墙默认丢弃外部发起的入站连接；
- 让办公电脑对公网提供服务，通常违反公司安全策略。

更合适的做法是把服务部署到云主机，或使用反向隧道，详见仓库的
`deploy/` 目录。
"#,
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// An in-memory database with migrations applied.
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

    fn form(title: &str, body: &str, tags: &str, published: bool) -> PostForm {
        PostForm {
            title: title.into(),
            slug: String::new(),
            summary: String::new(),
            body_md: body.into(),
            tags: tags.into(),
            status: if published { "published" } else { "draft" }.into(),
        }
    }

    #[tokio::test]
    async fn draft_posts_stay_out_of_the_public_list() {
        let pool = test_pool().await;
        create_post(&pool, &form("公开", "# hi", "", true))
            .await
            .unwrap();
        create_post(&pool, &form("草稿", "# hi", "", false))
            .await
            .unwrap();

        let listed = list_published(&pool, 10, 0).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "公开");
        assert_eq!(count_published(&pool).await.unwrap(), 1);
        assert_eq!(list_all(&pool).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn duplicate_slugs_are_disambiguated() {
        let pool = test_pool().await;
        let first = create_post(&pool, &form("Same Title", "a", "", true))
            .await
            .unwrap();
        let second = create_post(&pool, &form("Same Title", "b", "", true))
            .await
            .unwrap();

        let a = find_by_id(&pool, first).await.unwrap().unwrap();
        let b = find_by_id(&pool, second).await.unwrap().unwrap();

        assert_eq!(a.slug, "same-title");
        assert_eq!(b.slug, "same-title-2");
    }

    #[tokio::test]
    async fn cjk_titles_produce_a_reachable_slug() {
        let pool = test_pool().await;
        let id = create_post(&pool, &form("我的第一篇博客", "正文", "", true))
            .await
            .unwrap();
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
        let id = create_post(&pool, &form("有标签", "正文", "rust, axum", true))
            .await
            .unwrap();

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
    async fn published_at_is_stamped_then_cleared() {
        let pool = test_pool().await;
        let id = create_post(&pool, &form("草稿", "正文", "", false))
            .await
            .unwrap();
        assert!(
            find_by_id(&pool, id)
                .await
                .unwrap()
                .unwrap()
                .published_at
                .is_none()
        );

        set_published(&pool, id, true).await.unwrap();
        let stamped = find_by_id(&pool, id).await.unwrap().unwrap().published_at;
        assert!(stamped.is_some());

        set_published(&pool, id, false).await.unwrap();
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
    async fn summary_falls_back_to_the_body() {
        let pool = test_pool().await;
        let id = create_post(&pool, &form("标题", "## 小标题\n\n正文内容。", "", true))
            .await
            .unwrap();
        let post = find_by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(post.summary, "小标题 正文内容。");
    }

    #[tokio::test]
    async fn pagination_slices_the_result_set() {
        let pool = test_pool().await;
        for i in 0..5 {
            create_post(&pool, &form(&format!("文章 {i}"), "正文", "", true))
                .await
                .unwrap();
        }

        assert_eq!(list_published(&pool, 2, 0).await.unwrap().len(), 2);
        assert_eq!(list_published(&pool, 2, 4).await.unwrap().len(), 1);
        assert_eq!(list_published(&pool, 2, 99).await.unwrap().len(), 0);
    }
}
