//! askama template bindings.
//!
//! One struct per file in `templates/`. Every struct repeats
//! `site_title`/`site_description` because askama compiles each template
//! independently and the base layout renders both.
//!
//! Optional text is modelled as a `String` that may be empty rather than an
//! `Option`, and URLs that depend on the record are precomputed here. That
//! keeps the templates free of `unwrap()` calls and of any logic beyond
//! conditionals and loops.

use askama::Template;

use crate::models::{ChapterNav, PostView, SeriesSummary, TagCount};

#[derive(Template)]
#[template(path = "index.html")]
pub struct IndexTemplate {
    pub site_title: String,
    pub site_description: String,
    pub posts: Vec<PostView>,
    pub page: i64,
    pub total_pages: i64,
    pub has_prev: bool,
    pub has_next: bool,
    pub prev_href: String,
    pub next_href: String,
}

#[derive(Template)]
#[template(path = "post.html")]
pub struct PostTemplate {
    pub site_title: String,
    pub site_description: String,
    pub post: PostView,
    /// Reading-order navigation, present only when the post is a chapter.
    ///
    /// An `Option` rather than an always-empty struct so an ordinary post
    /// renders exactly the markup it did before series existed — including no
    /// wrapper element to style around.
    pub nav: Option<ChapterNav>,
}

#[derive(Template)]
#[template(path = "series_index.html")]
pub struct SeriesIndexTemplate {
    pub site_title: String,
    pub site_description: String,
    pub series: Vec<SeriesSummary>,
}

#[derive(Template)]
#[template(path = "series.html")]
pub struct SeriesTemplate {
    pub site_title: String,
    pub site_description: String,
    /// The series itself, for the heading and the chapter count.
    pub series: SeriesSummary,
    /// Its chapters, in reading order.
    pub chapters: Vec<PostView>,
}

#[derive(Template)]
#[template(path = "tags.html")]
pub struct TagsTemplate {
    pub site_title: String,
    pub site_description: String,
    pub tags: Vec<TagCount>,
}

#[derive(Template)]
#[template(path = "tag.html")]
pub struct TagTemplate {
    pub site_title: String,
    pub site_description: String,
    pub tag_name: String,
    pub posts: Vec<PostView>,
    pub page: i64,
    pub total_pages: i64,
    pub has_prev: bool,
    pub has_next: bool,
    pub prev_href: String,
    pub next_href: String,
}
/// One entry of the RSS feed.
#[derive(Debug, Clone)]
pub struct FeedItem {
    pub title: String,
    pub link: String,
    pub description: String,
    pub pub_date: String,
}

#[derive(Template)]
#[template(path = "feed.xml")]
pub struct FeedTemplate {
    pub site_title: String,
    pub site_description: String,
    pub site_url: String,
    pub last_build: String,
    pub items: Vec<FeedItem>,
}

/// The 404 page.
///
/// A static host needs this file to exist: Cloudflare Pages treats a project
/// without a top-level `404.html` as a single-page application and answers
/// unknown paths with the root document instead of a 404.
#[derive(Template)]
#[template(path = "not_found.html")]
pub struct NotFoundTemplate {
    pub site_title: String,
    pub site_description: String,
}
