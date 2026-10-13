//! Building the pages a site is made of.
//!
//! Both the live server and the static exporter need exactly the same pages, so
//! the work lives here rather than in either of them. Two rendering paths would
//! drift: a fix to one would silently miss the other.
//!
//! Nothing in this module performs I/O beyond database reads, so a caller can
//! render a page to a string and either write it to a socket or to a file.

use std::collections::HashMap;

use anyhow::Result;
use sqlx::SqlitePool;

use crate::{
    assets::Assets,
    db, markdown,
    models::{Post, PostView, Settings, Tag},
    templates::{
        FeedItem, FeedTemplate, IndexTemplate, NotFoundTemplate, PostTemplate, TagTemplate,
        TagsTemplate,
    },
    util::{self, paginate, paginate_static},
};

/// Number of entries in the RSS feed.
pub const FEED_LIMIT: i64 = 20;

/// How pagination links are written.
///
/// The live server has a query string; a static host does not, so the page
/// number has to move into the path there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkStyle {
    Query,
    Path,
}

/// Everything needed to render a page of the site.
pub struct Site<'a> {
    pub pool: &'a SqlitePool,
    pub assets: &'a Assets,
    pub settings: &'a Settings,
    pub page_size: i64,
    pub links: LinkStyle,
}

impl<'a> Site<'a> {
    /// Resolve pagination for one listing.
    fn page(&self, requested: Option<i64>, total: i64, base: &str) -> util::PageInfo {
        match self.links {
            LinkStyle::Query => paginate(requested, total, self.page_size, base),
            LinkStyle::Path => paginate_static(requested, total, self.page_size, base),
        }
    }

    fn title(&self) -> String {
        self.settings.site_title.clone()
    }

    fn description(&self) -> String {
        self.settings.site_description.clone()
    }

    /// Home page: published posts, newest first.
    pub async fn home(&self, requested: Option<i64>) -> Result<IndexTemplate> {
        let total = db::count_published(self.pool).await?;
        let page = self.page(requested, total, "");

        let posts = db::list_published(self.pool, self.page_size, page.offset).await?;
        let tag_map = db::tags_by_post(self.pool).await?;

        Ok(IndexTemplate {
            site_title: self.title(),
            site_description: self.description(),
            posts: posts
                .into_iter()
                .map(|post| self.to_view(post, &tag_map, false))
                .collect(),
            page: page.page,
            total_pages: page.total_pages,
            has_prev: page.has_prev,
            has_next: page.has_next,
            prev_href: page.prev_href,
            next_href: page.next_href,
        })
    }

    /// A single published post, or `None` when there is no such post.
    pub async fn post(&self, slug: &str) -> Result<Option<PostTemplate>> {
        let Some(post) = db::find_published_by_slug(self.pool, slug).await? else {
            return Ok(None);
        };

        let tag_map = db::tags_by_post(self.pool).await?;

        Ok(Some(PostTemplate {
            site_title: self.title(),
            site_description: self.description(),
            post: self.to_view(post, &tag_map, true),
        }))
    }

    /// The tag cloud.
    pub async fn tags(&self) -> Result<TagsTemplate> {
        Ok(TagsTemplate {
            site_title: self.title(),
            site_description: self.description(),
            tags: db::list_tags_with_counts(self.pool).await?,
        })
    }

    /// Posts carrying one tag, or `None` when the tag does not exist.
    pub async fn tag(&self, slug: &str, requested: Option<i64>) -> Result<Option<TagTemplate>> {
        let Some(tag) = db::find_tag_by_slug(self.pool, slug).await? else {
            return Ok(None);
        };

        let total = db::count_published_by_tag(self.pool, &tag.slug).await?;
        let base = util::tag_path(&tag.slug);
        let page = self.page(requested, total, &base);

        let posts =
            db::list_published_by_tag(self.pool, &tag.slug, self.page_size, page.offset).await?;
        let tag_map = db::tags_by_post(self.pool).await?;

        Ok(Some(TagTemplate {
            site_title: self.title(),
            site_description: self.description(),
            tag_name: tag.name,
            posts: posts
                .into_iter()
                .map(|post| self.to_view(post, &tag_map, false))
                .collect(),
            page: page.page,
            total_pages: page.total_pages,
            has_prev: page.has_prev,
            has_next: page.has_next,
            prev_href: page.prev_href,
            next_href: page.next_href,
        }))
    }

    /// The RSS feed.
    pub async fn feed(&self) -> Result<FeedTemplate> {
        let posts = db::list_published(self.pool, FEED_LIMIT, 0).await?;

        Ok(FeedTemplate {
            site_title: self.title(),
            site_description: self.description(),
            site_url: self.settings.site_url.clone(),
            last_build: chrono::Utc::now().to_rfc2822(),
            items: posts
                .into_iter()
                .map(|post| {
                    let link = format!("{}{}", self.settings.site_url, util::post_path(&post.slug));

                    // Prefer the author's summary; fall back to the body when a
                    // post was written without one.
                    let description = if post.summary.trim().is_empty() {
                        markdown::excerpt(&post.body_md, 200)
                    } else {
                        post.summary.clone()
                    };

                    FeedItem {
                        title: post.title.clone(),
                        pub_date: rfc2822(&post),
                        link,
                        description,
                    }
                })
                .collect(),
        })
    }

    /// The 404 page.
    pub async fn not_found(&self) -> NotFoundTemplate {
        NotFoundTemplate {
            site_title: self.title(),
            site_description: self.description(),
        }
    }

    /// Build a render-ready post.
    ///
    /// `with_body` controls whether the Markdown body is rendered. List pages
    /// show only the stored summary, so rendering every body there would be
    /// wasted work; those views carry an empty `body_html` that their templates
    /// never reference.
    ///
    /// Local image references are resolved against the post's own source path,
    /// which is why the published asset layout mirrors the author's directory.
    fn to_view(&self, post: Post, tags: &HashMap<i64, Vec<Tag>>, with_body: bool) -> PostView {
        let date = post.display_date();

        let body_html = if with_body {
            markdown::render_with_assets(&post.body_md, self.assets, &post.source_path)
        } else {
            String::new()
        };

        let tags = tags.get(&post.id).cloned().unwrap_or_default();

        // A cover is usually a remote URL, which the resolver leaves untouched;
        // a local path is repointed at /media/ exactly like an inline image.
        let cover = post
            .cover_image
            .as_deref()
            .map(|raw| {
                self.assets
                    .url_for(raw, &post.source_path)
                    .unwrap_or_else(|| raw.to_string())
            })
            .unwrap_or_default();

        PostView {
            slug: post.slug,
            title: post.title,
            summary: post.summary,
            body_html,
            date,
            tags,
            cover,
        }
    }
}

/// Publication time in RFC 2822, which RSS 2.0 requires.
fn rfc2822(post: &Post) -> String {
    let raw = post
        .published_at
        .as_deref()
        .unwrap_or(post.created_at.as_str());

    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.to_rfc2822())
        .unwrap_or_else(|_| chrono::Utc::now().to_rfc2822())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc2822_is_produced_for_a_valid_timestamp() {
        let post = Post {
            id: 1,
            slug: "s".into(),
            title: "t".into(),
            summary: String::new(),
            body_md: String::new(),
            published: true,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            published_at: Some("2026-03-04T05:06:07Z".into()),
            source_path: "s.md".into(),
            cover_image: None,
        };

        let stamp = rfc2822(&post);
        assert!(stamp.contains("2026"), "got {stamp}");
        assert!(stamp.contains("Mar"), "got {stamp}");
    }

    #[test]
    fn rfc2822_falls_back_rather_than_panicking() {
        let mut post = Post {
            id: 1,
            slug: "s".into(),
            title: "t".into(),
            summary: String::new(),
            body_md: String::new(),
            published: true,
            created_at: "not-a-date".into(),
            updated_at: "not-a-date".into(),
            published_at: None,
            source_path: "s.md".into(),
            cover_image: None,
        };

        // A junk timestamp must not panic; it yields the current time instead.
        assert!(!rfc2822(&post).is_empty());

        post.published_at = Some("2026-03-04T05:06:07Z".into());
        assert!(rfc2822(&post).contains("Mar"));
    }
}
