//! Markdown rendering.
//!
//! Authoring is restricted to the admin surface, so the HTML produced here is
//! treated as trusted and injected into templates with the `safe` filter.

use pulldown_cmark::{Event, Options, Parser, TagEnd, html};

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
}
