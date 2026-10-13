//! Small helpers shared by the routes.

/// Pagination state resolved for one request.
///
/// The hrefs are precomputed so templates only need a boolean guard, which
/// keeps them free of `Option` unwrapping.
#[derive(Debug, Clone)]
pub struct PageInfo {
    pub page: i64,
    pub total_pages: i64,
    pub offset: i64,
    pub has_prev: bool,
    pub has_next: bool,
    pub prev_href: String,
    pub next_href: String,
}

/// Resolve a requested page against the total number of items.
///
/// `requested` comes straight from the query string, so it is clamped into the
/// valid range rather than trusted — `?page=0`, `?page=-5` and `?page=999999`
/// all land somewhere sensible instead of producing a negative SQL `OFFSET`.
pub fn paginate(requested: Option<i64>, total_items: i64, page_size: i64, base: &str) -> PageInfo {
    let page_size = page_size.max(1);
    let total_pages = ((total_items + page_size - 1) / page_size).max(1);
    let page = requested.unwrap_or(1).clamp(1, total_pages);
    let offset = (page - 1) * page_size;

    PageInfo {
        page,
        total_pages,
        offset,
        has_prev: page > 1,
        has_next: page < total_pages,
        prev_href: format!("{base}?page={}", page - 1),
        next_href: format!("{base}?page={}", page + 1),
    }
}

/// Resolve a page for a static export.
///
/// The live server paginates with a query string (`/?page=2`), which a static
/// host cannot route. Here the page number moves into the path, and the files
/// are written as `page/2.html` so the extension-less URL resolves on a host
/// like Cloudflare Pages.
///
/// `base` is the page-one path: an empty string for the index, `/tags/{slug}`
/// for a tag listing.
pub fn paginate_static(
    requested: Option<i64>,
    total_items: i64,
    page_size: i64,
    base: &str,
) -> PageInfo {
    let page_size = page_size.max(1);
    let total_pages = ((total_items + page_size - 1) / page_size).max(1);
    let page = requested.unwrap_or(1).clamp(1, total_pages);
    let offset = (page - 1) * page_size;

    let href = |number: i64| -> String {
        if number <= 1 {
            if base.is_empty() {
                "/".to_string()
            } else {
                base.to_string()
            }
        } else {
            format!("{base}/page/{number}")
        }
    };

    PageInfo {
        page,
        total_pages,
        offset,
        has_prev: page > 1,
        has_next: page < total_pages,
        prev_href: href(page - 1),
        next_href: href(page + 1),
    }
}

/// Percent-encode one URL path segment.
///
/// Slugs may contain CJK characters, which have to be encoded before they can
/// appear in the absolute links inside the RSS feed.
pub fn encode_path_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());

    for byte in segment.as_bytes() {
        match byte {
            // RFC 3986 unreserved characters pass through untouched.
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }

    out
}

/// Canonical path of a post.
pub fn post_path(slug: &str) -> String {
    format!("/posts/{}", encode_path_segment(slug))
}

/// Canonical path of a tag listing.
pub fn tag_path(slug: &str) -> String {
    format!("/tags/{}", encode_path_segment(slug))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_out_of_range_pages() {
        // 25 items, 10 per page => 3 pages.
        let first = paginate(Some(0), 25, 10, "/");
        assert_eq!(first.page, 1);
        assert_eq!(first.offset, 0);
        assert!(!first.has_prev);
        assert!(first.has_next);

        let last = paginate(Some(999), 25, 10, "/");
        assert_eq!(last.page, 3);
        assert_eq!(last.offset, 20);
        assert!(last.has_prev);
        assert!(!last.has_next);
    }

    #[test]
    fn empty_result_set_still_has_one_page() {
        let info = paginate(None, 0, 10, "/");
        assert_eq!(info.total_pages, 1);
        assert_eq!(info.page, 1);
        assert!(!info.has_next);
    }

    #[test]
    fn builds_titles_for_both_bases() {
        assert_eq!(paginate(Some(2), 25, 10, "/").prev_href, "/?page=1");
        assert_eq!(
            paginate(Some(1), 25, 10, "/tags/rust").next_href,
            "/tags/rust?page=2"
        );
    }

    #[test]
    fn static_pagination_moves_the_page_into_the_path() {
        // The index: page one is the root, later pages live under /page/.
        let first = paginate_static(None, 25, 10, "");
        assert_eq!(first.page, 1);
        assert_eq!(first.next_href, "/page/2");
        assert_eq!(first.prev_href, "/");

        let third = paginate_static(Some(3), 25, 10, "");
        assert_eq!(third.prev_href, "/page/2");
        assert!(!third.has_next);

        // A tag listing nests under its own path.
        let tag = paginate_static(Some(2), 25, 10, "/tags/rust");
        assert_eq!(tag.prev_href, "/tags/rust");
        assert_eq!(tag.next_href, "/tags/rust/page/3");
    }

    #[test]
    fn static_pagination_clamps_like_the_query_version() {
        let info = paginate_static(Some(999), 25, 10, "");
        assert_eq!(info.page, 3);
        assert_eq!(info.offset, 20);

        let empty = paginate_static(None, 0, 10, "/tags/x");
        assert_eq!(empty.total_pages, 1);
        assert_eq!(empty.prev_href, "/tags/x");
    }

    #[test]
    fn encodes_non_ascii_slugs() {
        assert_eq!(encode_path_segment("rust"), "rust");
        assert_eq!(
            encode_path_segment("我的博客"),
            "%E6%88%91%E7%9A%84%E5%8D%9A%E5%AE%A2"
        );
        assert_eq!(post_path("a b"), "/posts/a%20b");
    }
}
