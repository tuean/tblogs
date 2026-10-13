//! Markdown rendering.
//!
//! Authoring is restricted to the admin surface or to files the operator owns,
//! so the HTML produced here is treated as trusted and injected into templates
//! with the `safe` filter.

use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd, html};

use crate::assets::Assets;

/// Extensions enabled for every document.
fn options() -> Options {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_HEADING_ATTRIBUTES);
    options
}

/// Render Markdown to an HTML fragment.
pub fn render(markdown: &str) -> String {
    let parser = Parser::new_ext(markdown, options());
    let mut out = String::with_capacity(markdown.len() + markdown.len() / 2);
    html::push_html(&mut out, parser);
    out
}

/// Render Markdown, repointing local asset references at `/media/...`.
///
/// `source_rel` is the document's path relative to the content directory, which
/// is what relative references such as `assets/a.png` are relative *to*.
/// References that are already remote, or that point outside the content
/// directory, are emitted unchanged.
pub fn render_with_assets(markdown: &str, assets: &Assets, source_rel: &str) -> String {
    let parser = Parser::new_ext(markdown, options());
    let events = parser.map(|event| rewrite_assets(event, assets, source_rel));

    let mut out = String::with_capacity(markdown.len() + markdown.len() / 2);
    html::push_html(&mut out, events);
    out
}

/// Repoint the destination of one image or link event.
fn rewrite_assets<'a>(event: Event<'a>, assets: &Assets, source_rel: &str) -> Event<'a> {
    match event {
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Image {
            link_type,
            dest_url: rewrite_dest(dest_url, assets, source_rel),
            title,
            id,
        }),

        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: rewrite_dest(dest_url, assets, source_rel),
            title,
            id,
        }),

        // Typora emits raw HTML for images it has resized, e.g.
        // `<img src="assets/a.png" style="zoom:50%;" />`, so raw HTML has to be
        // rewritten too or those images would break.
        Event::Html(raw) => Event::Html(rewrite_raw_html(raw, assets, source_rel)),
        Event::InlineHtml(raw) => Event::InlineHtml(rewrite_raw_html(raw, assets, source_rel)),

        other => other,
    }
}

fn rewrite_dest<'a>(dest: CowStr<'a>, assets: &Assets, source_rel: &str) -> CowStr<'a> {
    match assets.url_for(&dest, source_rel) {
        Some(url) => CowStr::from(url),
        None => dest,
    }
}

/// Rewrite `src="..."` and `href="..."` inside a raw HTML fragment.
fn rewrite_raw_html<'a>(html: CowStr<'a>, assets: &Assets, source_rel: &str) -> CowStr<'a> {
    if !html.contains("src=") && !html.contains("href=") {
        return html;
    }

    let mut out = String::with_capacity(html.len());
    let mut index = 0usize;

    while index < html.len() {
        let rest = &html[index..];

        let attribute = ["src=", "href="]
            .into_iter()
            .find(|attr| rest.starts_with(attr));

        if let Some(attribute) = attribute {
            let after = &rest[attribute.len()..];
            let quote = after.chars().next();

            if let Some(quote @ ('"' | '\'')) = quote
                && let Some(end) = after[1..].find(quote)
            {
                let value = &after[1..1 + end];

                out.push_str(attribute);
                out.push(quote);
                match assets.url_for(value, source_rel) {
                    Some(url) => out.push_str(&url),
                    None => out.push_str(value),
                }
                out.push(quote);

                index += attribute.len() + 1 + end + 1;
                continue;
            }
        }

        // Not an attribute we rewrite: copy one character and move on. Advance
        // by whole characters so multi-byte text is never split.
        let ch = rest.chars().next().expect("index is on a char boundary");
        out.push(ch);
        index += ch.len_utf8();
    }

    CowStr::from(out)
}

/// Whether ending this construct marks a word boundary in plain text.
///
/// Block-level elements are separate text events with no whitespace between
/// them, so `# Heading\n\nBody` would otherwise flatten to `HeadingBody`.
/// Inline ends (`Emphasis`, `Strong`, `Link`, …) are excluded on purpose:
/// `**bold**text` really is one word.
fn ends_block(tag: TagEnd) -> bool {
    matches!(
        tag,
        TagEnd::Paragraph
            | TagEnd::Heading(_)
            | TagEnd::BlockQuote(_)
            | TagEnd::CodeBlock
            | TagEnd::HtmlBlock
            | TagEnd::List(_)
            | TagEnd::Item
            | TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Table
            | TagEnd::TableHead
            | TagEnd::TableRow
            | TagEnd::TableCell
    )
}

/// Derive a plain-text excerpt, used for list pages and the RSS feed.
///
/// Markdown syntax is dropped rather than escaped, so the result is safe to
/// place in both HTML and XML text nodes.
pub fn excerpt(markdown: &str, max_chars: usize) -> String {
    let mut text = String::new();
    for event in Parser::new_ext(markdown, options()) {
        match event {
            Event::Text(t) | Event::Code(t) => text.push_str(&t),
            Event::SoftBreak | Event::HardBreak => text.push(' '),
            Event::End(tag) if ends_block(tag) => text.push(' '),
            _ => {}
        }
    }

    // Collapse whitespace runs; CJK text has no spaces so it passes through.
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");

    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }

    let truncated: String = collapsed.chars().take(max_chars).collect();
    format!("{}…", truncated.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_headings_and_emphasis() {
        let html = render("# Title\n\nSome *emphasis* here.");
        assert!(html.contains("<h1>Title</h1>"), "got: {html}");
        assert!(html.contains("<em>emphasis</em>"), "got: {html}");
    }

    #[test]
    fn renders_tables_and_code_fences() {
        let html = render("| a | b |\n|---|---|\n| 1 | 2 |\n\n```rust\nfn main() {}\n```\n");
        assert!(html.contains("<table>"), "got: {html}");
        assert!(html.contains("language-rust"), "got: {html}");
    }

    #[test]
    fn excerpt_strips_markdown_syntax() {
        let text = excerpt("## Heading\n\nSome **bold** and `code`.", 200);
        assert_eq!(text, "Heading Some bold and code.");
    }

    #[test]
    fn excerpt_truncates_on_char_boundaries() {
        // Would panic if truncation sliced a multi-byte character in half.
        let text = excerpt("中文标题内容很长的段落", 4);
        assert_eq!(text, "中文标题…");
    }

    /// A scratch content directory, removed on drop.
    struct Scratch {
        dir: std::path::PathBuf,
    }

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("tblogs-md-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("notes/assets")).unwrap();
            std::fs::write(dir.join("notes/assets/a.png"), "png").unwrap();
            Self { dir }
        }

        fn assets(&self) -> Assets {
            Assets::new(&self.dir).unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn rewrites_relative_image_sources() {
        let scratch = Scratch::new("img");
        let html = render_with_assets("![shot](assets/a.png)", &scratch.assets(), "notes/post.md");

        assert!(
            html.contains(r#"src="/media/notes/assets/a.png""#),
            "got: {html}"
        );
    }

    #[test]
    fn leaves_remote_images_untouched() {
        let scratch = Scratch::new("remote");
        let html = render_with_assets(
            "![shot](https://cdn.example.com/a.png)",
            &scratch.assets(),
            "notes/post.md",
        );

        assert!(
            html.contains("https://cdn.example.com/a.png"),
            "got: {html}"
        );
        assert!(!html.contains("/media/"), "got: {html}");
    }

    #[test]
    fn rewrites_images_inside_raw_html() {
        let scratch = Scratch::new("rawhtml");
        // This is what Typora emits for a resized image.
        let html = render_with_assets(
            "<img src=\"assets/a.png\" alt=\"x\" style=\"zoom:50%;\" />",
            &scratch.assets(),
            "notes/post.md",
        );

        assert!(
            html.contains(r#"src="/media/notes/assets/a.png""#),
            "got: {html}"
        );
        // The rest of the tag must survive the rewrite intact.
        assert!(html.contains("zoom:50%"), "got: {html}");
    }

    #[test]
    fn rewrites_links_as_well_as_images() {
        let scratch = Scratch::new("link");
        let html = render_with_assets(
            "see [the diagram](assets/a.png)",
            &scratch.assets(),
            "notes/post.md",
        );

        assert!(
            html.contains(r#"href="/media/notes/assets/a.png""#),
            "got: {html}"
        );
    }

    #[test]
    fn raw_html_rewrite_preserves_multibyte_text() {
        let scratch = Scratch::new("multibyte");
        // The byte-by-byte scanner must not split these characters.
        let html = render_with_assets(
            "<img src=\"assets/a.png\" alt=\"中文说明 🎉\" />",
            &scratch.assets(),
            "notes/post.md",
        );

        assert!(html.contains("中文说明 🎉"), "got: {html}");
        assert!(html.contains("/media/notes/assets/a.png"), "got: {html}");
    }

    #[test]
    fn plain_render_is_unaffected_by_asset_rewriting() {
        // Without a content directory the output must be exactly as before.
        let html = render("![shot](assets/a.png)");
        assert!(html.contains(r#"src="assets/a.png""#), "got: {html}");
        assert!(!html.contains("/media/"), "got: {html}");
    }
}
