//! Building a content package from a directory of Markdown files.
//!
//! This is the whole job of the authoring client: read the directory, write a
//! self-contained SQLite database, and mirror the assets that the site needs to
//! serve alongside it. The result is a single directory that can be shipped to
//! any machine.
//!
//! The database is built from scratch every time. That keeps the logic simple
//! (no migration chain, no drift between a long-lived database and the files)
//! and guarantees the published artifact is exactly reproducible from the
//! Markdown, at the cost of re-importing every file — which for a personal blog
//! is tens of files.

use std::path::Path;

use anyhow::{Context, Result};

use crate::{
    assets,
    config::{DATABASE_FILE, MEDIA_DIR},
    content, db,
    models::Settings,
};

/// What one build produced.
#[derive(Debug, Default, Clone)]
pub struct BuildReport {
    pub posts: usize,
    pub published: usize,
    pub drafts: usize,
    pub tags: usize,
    pub assets: usize,
    pub bytes: u64,
}

impl BuildReport {
    /// A human-readable summary for the console.
    pub fn summary(&self) -> String {
        format!(
            "{} posts ({} published, {} draft), {} tags, {} assets, {}",
            self.posts,
            self.published,
            self.drafts,
            self.tags,
            self.assets,
            humansize(self.bytes)
        )
    }
}

/// Build `<out>/site.db` and `<out>/media/` from `content_dir`.
///
/// `out` is created if needed. An existing package there is replaced, but only
/// after the new one has been built successfully, so a failed build never
/// leaves a half-written package behind.
pub async fn build_package(
    content_dir: &Path,
    out: &Path,
    settings: &Settings,
    log: &(dyn Fn(&str) + Sync),
) -> Result<BuildReport> {
    // Fail before doing any work if the content directory is unusable.
    let assets_resolver = assets::Assets::new(content_dir.to_path_buf())?;

    std::fs::create_dir_all(out).with_context(|| format!("could not create {}", out.display()))?;

    // Stage into a scratch directory so the previous package stays intact until
    // the new one is complete.
    let staging = out.join(".staging");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .with_context(|| format!("could not clear {}", staging.display()))?;
    }
    std::fs::create_dir_all(&staging)
        .with_context(|| format!("could not create {}", staging.display()))?;

    let result = build_into(&assets_resolver, &staging, settings, log).await;

    let report = match result {
        Ok(report) => report,
        Err(error) => {
            // Leave no debris behind on failure.
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
    };

    // Swap the staged package into place.
    let staged_db = staging.join(DATABASE_FILE);
    let staged_media = staging.join(MEDIA_DIR);

    let final_db = out.join(DATABASE_FILE);
    let final_media = out.join(MEDIA_DIR);

    if final_db.exists() {
        std::fs::remove_file(&final_db)
            .with_context(|| format!("could not replace {}", final_db.display()))?;
    }
    std::fs::rename(&staged_db, &final_db).with_context(|| {
        format!(
            "could not move the new database into {}",
            final_db.display()
        )
    })?;

    if final_media.exists() {
        std::fs::remove_dir_all(&final_media)
            .with_context(|| format!("could not replace {}", final_media.display()))?;
    }
    std::fs::rename(&staged_media, &final_media).with_context(|| {
        format!(
            "could not move the new assets into {}",
            final_media.display()
        )
    })?;

    let _ = std::fs::remove_dir_all(&staging);

    Ok(report)
}

/// Do the work inside the staging directory.
async fn build_into(
    content: &assets::Assets,
    staging: &Path,
    settings: &Settings,
    log: &(dyn Fn(&str) + Sync),
) -> Result<BuildReport> {
    let files = content::scan(content.root())?;
    log(&format!("found {} markdown files", files.len()));

    let database = staging.join(DATABASE_FILE);
    let pool = db::open_for_build(&database).await?;

    // Import every file. The database is empty, so this is all inserts.
    let sync = content::sync(&pool, content.root(), &files).await?;

    // Count what actually landed rather than trusting the scan: a file may be
    // skipped or fail to parse into a post.
    let all = db::list_all(&pool).await?;
    let published = all.iter().filter(|post| post.published).count();
    let tags = db::list_tags_with_counts(&pool).await?.len();

    db::save_settings(&pool, settings).await?;

    // Fold the write-ahead log back in so the file is portable and can be
    // opened read-only by the server. The pool has to go first: switching the
    // journal mode needs exclusive access to the database.
    pool.close().await;
    db::finalize_package(&database).await?;

    log(&format!(
        "imported {} created, {} updated, {} removed",
        sync.created, sync.updated, sync.removed
    ));

    let assets = mirror_assets(content.root(), &staging.join(MEDIA_DIR))?;
    log(&format!("mirrored {assets} assets"));

    let bytes = std::fs::metadata(&database)
        .map(|meta| meta.len())
        .unwrap_or(0);

    Ok(BuildReport {
        posts: all.len(),
        published,
        drafts: all.len() - published,
        tags,
        assets,
        bytes,
    })
}

/// Copy every serveable asset from the content directory into `out`,
/// preserving relative paths.
///
/// Only files whose extension the server is willing to serve are copied, so the
/// package never ships anything the site could not hand out anyway — and in
/// particular never the Markdown sources. Because the paths are preserved, the
/// `/media/...` URLs the renderer produces resolve exactly as they did against
/// the original directory.
pub fn mirror_assets(content_dir: &Path, out: &Path) -> Result<usize> {
    std::fs::create_dir_all(out).with_context(|| format!("could not create {}", out.display()))?;

    let mut copied = 0;

    for_each_asset(content_dir, &mut |path| {
        let relative = path.strip_prefix(content_dir).unwrap_or(path);
        let destination = out.join(relative);

        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }

        std::fs::copy(path, &destination).with_context(|| {
            format!(
                "could not copy {} to {}",
                path.display(),
                destination.display()
            )
        })?;

        copied += 1;
        Ok(())
    })?;

    Ok(copied)
}

/// Count the assets a build would publish, without copying anything.
///
/// Used by `doctor` to report what a build would include.
pub fn count_assets(content_dir: &Path) -> Result<usize> {
    let mut count = 0;
    for_each_asset(content_dir, &mut |_| {
        count += 1;
        Ok(())
    })?;
    Ok(count)
}

/// Walk the content directory, calling `visit` for each serveable asset.
///
/// Hidden files and directories are skipped, which keeps editor state such as
/// `.git` and `.obsidian` out of the published package.
fn for_each_asset(dir: &Path, visit: &mut dyn FnMut(&Path) -> Result<()>) -> Result<()> {
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
            for_each_asset(&path, visit)?;
            continue;
        }

        if file_type.is_file() && assets::content_type(&path).is_some() {
            visit(&path)?;
        }
    }

    Ok(())
}

/// Compact byte count for the console.
fn humansize(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;

    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch {
        dir: std::path::PathBuf,
    }

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("tblogs-build-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("assets")).unwrap();
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

    #[test]
    fn mirroring_copies_assets_but_not_sources() {
        let scratch = Scratch::new("mirror");
        scratch.write("assets/a.png", "png");
        scratch.write("assets/deep/b.jpg", "jpg");
        scratch.write("notes/post.md", "# Post");
        scratch.write("assets/readme.txt", "text");
        scratch.write(".hidden/x.png", "hidden");
        scratch.write("archive.zip", "zip");

        let out = scratch.dir.join("out");
        let copied = mirror_assets(&scratch.dir, &out).unwrap();

        // a.png, deep/b.jpg, readme.txt, archive.zip
        assert_eq!(copied, 4, "unexpected asset count");

        assert!(out.join("assets/a.png").is_file());
        assert!(out.join("assets/deep/b.jpg").is_file());
        assert!(out.join("archive.zip").is_file());
        // The Markdown source must never be published.
        assert!(!out.join("notes/post.md").exists());
        // Hidden directories are skipped.
        assert!(!out.join(".hidden/x.png").exists());

        std::fs::remove_dir_all(&scratch.dir).unwrap();
    }

    #[tokio::test]
    async fn building_produces_a_readable_package() {
        let scratch = Scratch::new("package");
        scratch.write(
            "2026-01-05-hello.md",
            "---\ntags: [Rust, rust]\n---\n\n# Hello\n\n![a](assets/a.png)\n",
        );
        scratch.write("draft.md", "---\ntitle: Draft\ndraft: true\n---\n\nWIP\n");
        scratch.write("assets/a.png", "png");

        let out = scratch.dir.join("dist");
        let settings = Settings {
            site_title: "Test".into(),
            site_description: "Desc".into(),
            site_url: "https://example.test".into(),
            built_at: db::now_iso(),
        };

        let report = build_package(&scratch.dir, &out, &settings, &|_| {})
            .await
            .unwrap();

        assert_eq!(report.posts, 2);
        assert_eq!(report.published, 1);
        assert_eq!(report.drafts, 1);
        // "Rust" and "rust" collapse into one tag.
        assert_eq!(report.tags, 1);
        assert_eq!(report.assets, 1);

        // The package opens read-only, which is how the server consumes it.
        let pool = db::open_read_only(&out.join(DATABASE_FILE)).await.unwrap();
        let settings = db::load_settings(&pool).await.unwrap();
        assert_eq!(settings.site_title, "Test");
        assert_eq!(settings.site_url, "https://example.test");
        pool.close().await;

        assert!(out.join(MEDIA_DIR).join("assets/a.png").is_file());
        // Staging is cleaned up.
        assert!(!out.join(".staging").exists());

        std::fs::remove_dir_all(&scratch.dir).unwrap();
    }

    #[tokio::test]
    async fn a_failed_build_leaves_no_staging_behind() {
        let scratch = Scratch::new("failed");
        let out = scratch.dir.join("dist");
        std::fs::create_dir_all(&out).unwrap();

        // The content directory does not exist, so the build must fail.
        let missing = scratch.dir.join("nope");
        let settings = Settings::default();

        let result = build_package(&missing, &out, &settings, &|_| {}).await;
        assert!(result.is_err());
        assert!(!out.join(".staging").exists());

        std::fs::remove_dir_all(&scratch.dir).unwrap();
    }

    #[test]
    fn humansize_reads_sensibly() {
        assert_eq!(humansize(512), "512 B");
        assert_eq!(humansize(2048), "2.0 KiB");
        assert_eq!(humansize(5 * 1024 * 1024), "5.0 MiB");
    }
}
