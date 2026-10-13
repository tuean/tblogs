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

use crate::models::{PostView, TagCount};

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
