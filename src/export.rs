//! Rendering the whole site to files.
//!
//! The server is already read-only: every page is a pure function of the
//! content package. That makes a static export a *lossless* transformation
//! rather than a reduced copy — and it means the site can live on any host that
//! serves files, with no server, no public IP and no port forwarding.
//!
//! One thing has to change. A static host cannot route a query string, so
//! pagination moves into the path: `?page=2` becomes `page/2`, written to disk
//! as `page/2.html`. Cloudflare Pages serves `posts/foo.html` for the
//! extension-less `/posts/foo`, which is exactly what the templates already
//! link to — so no URL in the site changes.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use askama::Template;
use sqlx::SqlitePool;

use crate::{
    assets::{self, Assets},
    db,
    models::Settings,
    pages::{LinkStyle, Site},
};

/// What an export produced.
#[derive(Debug, Default, Clone)]
pub struct ExportReport {
    pub html_files: usize,
    pub media_files: usize,
    pub posts: usize,
    pub tags: usize,
    pub bytes: u64,
}

/// Write the whole site into `out_dir`.
///
/// `media_src` is the package's `media/` tree; it is copied verbatim so the
/// `/media/...` URLs the pages reference resolve.
pub async fn export_site(
    pool: &SqlitePool,
    assets: &Assets,
    settings: &Settings,
    page_size: i64,
    out_dir: &Path,
    media_src: &Path,
    log: &mut dyn FnMut(&str),
) -> Result<ExportReport> {
    let mut report = ExportReport::default();

    prepare_dir(out_dir)?;

    let site = Site {
        pool,
        assets,
        settings,
        page_size,
        links: LinkStyle::Path,
    };

    // ---- home, then the later pages -------------------------------------
    let home = site.home(None).await?;
    let total_pages = home.total_pages;
    write_page(out_dir, "index.html", &home, &mut report)?;

    for page in 2..=total_pages {
        let rendered = site.home(Some(page)).await?;
        write_page(
            out_dir,
            &format!("page/{page}.html"),
            &rendered,
            &mut report,
        )?;
    }
    log(&format!("  首页 {total_pages} 页"));

    // ---- one file per published post ------------------------------------
    let total_posts = db::count_published(pool).await?;
    let posts = db::list_published(pool, total_posts, 0).await?;

    for post in &posts {
        let Some(rendered) = site.post(&post.slug).await? else {
            continue;
        };
        write_page(
            out_dir,
            &format!("posts/{}.html", safe_segment(&post.slug)?),
            &rendered,
            &mut report,
        )?;
    }
    report.posts = posts.len();
    log(&format!("  文章 {} 篇", posts.len()));

    // ---- tag cloud, then one listing (and its pages) per tag -------------
    let tags = site.tags().await?;
    write_page(out_dir, "tags.html", &tags, &mut report)?;

    for tag in &tags.tags {
        let Some(first) = site.tag(&tag.slug, None).await? else {
            continue;
        };
        let tag_pages = first.total_pages;
        let dir = format!("tags/{}", safe_segment(&tag.slug)?);

        write_page(out_dir, &format!("{dir}.html"), &first, &mut report)?;

        for page in 2..=tag_pages {
            let Some(rendered) = site.tag(&tag.slug, Some(page)).await? else {
                continue;
            };
            write_page(
                out_dir,
                &format!("{dir}/page/{page}.html"),
                &rendered,
                &mut report,
            )?;
        }
    }
    report.tags = tags.tags.len();
    log(&format!("  标签 {} 个", tags.tags.len()));

    // ---- feed, 404 and the stylesheet -----------------------------------
    let feed = site.feed().await?;
    write(out_dir, "feed.xml", feed.render()?.as_bytes(), &mut report)?;

    let not_found = site.not_found().await;
    write_page(out_dir, "404.html", &not_found, &mut report)?;

    // The server embeds these; a static host needs them as real files.
    write(
        out_dir,
        "static/style.css",
        assets::STYLESHEET.as_bytes(),
        &mut report,
    )?;

    // Browsers request /favicon.ico and /apple-touch-icon.png without being
    // asked, so those two file names are fixed rather than chosen.
    for (name, bytes) in [
        ("favicon.ico", assets::FAVICON_ICO),
        ("favicon.svg", assets::FAVICON_SVG),
        ("favicon-32.png", assets::FAVICON_PNG),
        ("apple-touch-icon.png", assets::APPLE_TOUCH_ICON),
    ] {
        write(out_dir, name, bytes, &mut report)?;
    }

    // ---- media -----------------------------------------------------------
    report.media_files = copy_tree(media_src, &out_dir.join("media"))?;
    log(&format!("  媒体 {} 个", report.media_files));

    Ok(report)
}

/// Render one page and write it.
fn write_page<T: Template>(
    root: &Path,
    relative: &str,
    page: &T,
    report: &mut ExportReport,
) -> Result<()> {
    let html = page
        .render()
        .with_context(|| format!("渲染 {relative} 失败"))?;
    write(root, relative, html.as_bytes(), report)?;
    report.html_files += 1;
    Ok(())
}

/// Write one file, creating parent directories as needed.
fn write(root: &Path, relative: &str, bytes: &[u8], report: &mut ExportReport) -> Result<()> {
    let path = root.join(relative);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建目录 {} 失败", parent.display()))?;
    }

    std::fs::write(&path, bytes).with_context(|| format!("写入 {} 失败", path.display()))?;
    report.bytes += bytes.len() as u64;

    Ok(())
}

/// Reject a slug that would escape the output directory or name a directory.
///
/// Slugs are generated locally, but this output is written straight to disk and
/// is the one place where a bad slug could do more than produce a wrong URL.
fn safe_segment(slug: &str) -> Result<String> {
    if slug.is_empty()
        || slug.contains('/')
        || slug.contains('\\')
        || slug.contains('\0')
        || slug == "."
        || slug == ".."
    {
        bail!("拒绝作为文件名使用的 slug: {slug:?}");
    }

    Ok(slug.to_string())
}

/// Remove any previous export and recreate the directory.
///
/// Starting clean matters: a post that was renamed or unpublished would
/// otherwise leave its old page behind, still served, still indexed.
fn prepare_dir(out_dir: &Path) -> Result<()> {
    if out_dir.exists() {
        // Refuse obviously wrong targets rather than deleting them recursively.
        if out_dir.parent().is_none() || out_dir.file_name().is_none() {
            bail!("拒绝清空 {}：看起来不是导出目录", out_dir.display());
        }
        std::fs::remove_dir_all(out_dir)
            .with_context(|| format!("清空 {} 失败", out_dir.display()))?;
    }

    std::fs::create_dir_all(out_dir).with_context(|| format!("创建 {} 失败", out_dir.display()))?;

    Ok(())
}

/// Recursively copy a directory tree, returning the number of files copied.
fn copy_tree(src: &Path, dest: &Path) -> Result<usize> {
    if !src.exists() {
        return Ok(0);
    }

    let mut copied = 0;
    let mut stack: Vec<(PathBuf, PathBuf)> = vec![(src.to_path_buf(), dest.to_path_buf())];

    while let Some((from, to)) = stack.pop() {
        std::fs::create_dir_all(&to).with_context(|| format!("创建目录 {} 失败", to.display()))?;

        for entry in
            std::fs::read_dir(&from).with_context(|| format!("读取 {} 失败", from.display()))?
        {
            let entry = entry?;
            let name = entry.file_name();

            // Dotfiles are never published by the package builder; skip them
            // here too so the two stay in step.
            if name.to_string_lossy().starts_with('.') {
                continue;
            }

            let file_type = entry.file_type()?;
            let target = to.join(&name);

            if file_type.is_dir() {
                stack.push((entry.path(), target));
            } else if file_type.is_file() {
                std::fs::copy(entry.path(), &target)
                    .with_context(|| format!("复制到 {} 失败", target.display()))?;
                copied += 1;
            }
        }
    }

    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_slugs_that_are_not_one_path_segment() {
        assert_eq!(safe_segment("hello-world").unwrap(), "hello-world");
        assert_eq!(
            safe_segment("一些常用的bash脚本").unwrap(),
            "一些常用的bash脚本"
        );

        assert!(safe_segment("").is_err());
        assert!(safe_segment("..").is_err());
        assert!(safe_segment(".").is_err());
        assert!(safe_segment("a/b").is_err());
        assert!(safe_segment("a\\b").is_err());
        assert!(safe_segment("a\0b").is_err());
    }

    #[test]
    fn prepare_dir_clears_stale_output() {
        let dir = std::env::temp_dir().join(format!("tblogs-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stale.html"), b"old").unwrap();

        prepare_dir(&dir).unwrap();
        assert!(!dir.join("stale.html").exists(), "陈旧页面应被清除");
        assert!(dir.is_dir());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn copy_tree_mirrors_nested_files_and_skips_dotfiles() {
        let base = std::env::temp_dir().join(format!("tblogs-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        let src = base.join("src");
        std::fs::create_dir_all(src.join("nested")).unwrap();
        std::fs::write(src.join("a.png"), b"a").unwrap();
        std::fs::write(src.join("nested/b.png"), b"b").unwrap();
        std::fs::write(src.join(".hidden"), b"x").unwrap();

        let dest = base.join("dest");
        let copied = copy_tree(&src, &dest).unwrap();

        assert_eq!(copied, 2, "点文件不应被复制");
        assert_eq!(std::fs::read(dest.join("a.png")).unwrap(), b"a");
        assert_eq!(std::fs::read(dest.join("nested/b.png")).unwrap(), b"b");
        assert!(!dest.join(".hidden").exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn copy_tree_tolerates_a_missing_source() {
        let missing = std::env::temp_dir().join("tblogs-copy-missing-does-not-exist");
        let dest = std::env::temp_dir().join(format!("tblogs-copy-dest-{}", std::process::id()));

        assert_eq!(copy_tree(&missing, &dest).unwrap(), 0);
    }
}
