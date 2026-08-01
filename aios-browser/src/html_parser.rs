use crate::types::{DomNode, Link};
use scraper::node::Node;
use scraper::{Html, Selector};

/// Tags whose content is invisible in a text-only render (scripts, styling,
/// metadata, embedded media).
const SKIP_TAGS: &[&str] = &[
    "script",
    "style",
    "head",
    "noscript",
    "template",
    "svg",
    "iframe",
    "canvas",
    "audio",
    "video",
    "source",
    "track",
    "title",
    "link",
    "meta",
    "base",
    "math",
    "annotation",
    "select",
    "textarea",
];

/// Tags treated as block-level: their content starts on a fresh line.
fn is_block(tag: &str) -> bool {
    matches!(
        tag,
        "html"
            | "body"
            | "p"
            | "div"
            | "section"
            | "article"
            | "main"
            | "header"
            | "footer"
            | "aside"
            | "blockquote"
            | "figure"
            | "figcaption"
            | "address"
            | "fieldset"
            | "form"
            | "details"
            | "summary"
            | "dl"
            | "dt"
            | "dd"
            | "menu"
            | "dir"
            | "tr"
            | "li"
            | "pre"
            | "ul"
            | "ol"
            | "table"
            | "thead"
            | "tbody"
            | "tfoot"
            | "hr"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
    )
}

/// Level for heading tags (`h1`..`h6`), else `None`.
fn heading_level(tag: &str) -> Option<usize> {
    let mut chars = tag.chars();
    if chars.next() != Some('h') {
        return None;
    }
    let rest: String = chars.collect();
    rest.parse::<usize>().ok().filter(|n| (1..=6).contains(n))
}

pub struct HtmlParser;

impl HtmlParser {
    /// Parse an HTML document into a [`DomNode`] tree.
    ///
    /// Uses the WHATWG-compliant html5ever engine (via `scraper`), so messy
    /// real-world markup is normalized to a well-formed tree.
    pub fn parse(html: &str, _base_url: &str) -> DomNode {
        let document = Html::parse_document(html);
        let root = document.tree.root();
        let mut dom = DomNode {
            tag: "document".into(),
            attrs: Vec::new(),
            children: Vec::new(),
            text: String::new(),
        };
        for child in root.children() {
            if let Some(node) = Self::build_node(&child) {
                dom.children.push(node);
            }
        }
        dom.text = Self::extract_text(html);
        dom
    }

    /// Convert HTML to structured plain text.
    ///
    /// Unlike a naive tag-stripper this preserves the document skeleton:
    /// paragraphs and block elements become separate lines, headings are
    /// prefixed with `#` markers, lists get bullets/numbers, tables are laid
    /// out with `|` separators, and `pre` blocks keep their formatting.
    pub fn extract_text(html: &str) -> String {
        let document = Html::parse_document(html);
        let mut out = String::new();
        if let Some(root) = document.tree.root().children().next() {
            Self::render_flow(&mut out, &root);
        }
        Self::finalize(&mut out);
        out.trim().to_string()
    }

    /// Extract every visible hyperlink as an absolute URL plus its text.
    pub fn extract_links(html: &str, base_url: &str) -> Vec<Link> {
        let document = Html::parse_document(html);
        let selector = Selector::parse("a[href]").unwrap();
        let base = url::Url::parse(base_url).ok();
        let mut links: Vec<Link> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for element in document.select(&selector) {
            let raw = element.value().attr("href").unwrap_or("").trim();
            if raw.is_empty()
                || raw.starts_with('#')
                || raw.starts_with("javascript:")
                || raw.starts_with("mailto:")
                || raw.starts_with("tel:")
            {
                continue;
            }
            let resolved = base
                .as_ref()
                .and_then(|b| b.join(raw).ok())
                .map(|u| {
                    let strip_root =
                        u.path() == "/" && u.query().is_none() && u.fragment().is_none();
                    let mut s = u.to_string();
                    if strip_root {
                        s = s.trim_end_matches('/').to_string();
                    }
                    s
                })
                .unwrap_or_else(|| raw.to_string());
            if !(resolved.starts_with("http://") || resolved.starts_with("https://")) {
                continue;
            }
            if !seen.insert(resolved.clone()) {
                continue;
            }
            let text = element
                .text()
                .collect::<Vec<_>>()
                .join(" ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            links.push(Link {
                href: resolved,
                text,
            });
        }
        links
    }

    /// Extract the document `<title>`.
    pub fn extract_title(html: &str) -> String {
        let document = Html::parse_document(html);
        let selector = Selector::parse("title").unwrap();
        document
            .select(&selector)
            .next()
            .map(|el| {
                el.text()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default()
    }

    /// Render a subtree in flow (block) context.
    fn render_flow(out: &mut String, node: &ego_tree::NodeRef<'_, Node>) {
        let Node::Element(el) = node.value() else {
            return;
        };
        let tag = el.name();
        if SKIP_TAGS.contains(&tag) {
            return;
        }

        if let Some(level) = heading_level(tag) {
            blank(out);
            for _ in 0..level {
                out.push('#');
            }
            out.push(' ');
            Self::render_inline(out, node);
            blank(out);
            return;
        }

        match tag {
            "ul" | "ol" | "menu" | "dir" => {
                blank(out);
                let mut index = 0usize;
                for child in node.children() {
                    if let Node::Element(cel) = child.value() {
                        if cel.name() == "li" {
                            if tag == "ol" {
                                index += 1;
                                out.push_str(&format!("  {index}. "));
                            } else {
                                out.push_str("  • ");
                            }
                            Self::render_inline(out, &child);
                            nl(out);
                        }
                    }
                }
                blank(out);
            }
            "li" => {
                blank(out);
                out.push_str("  • ");
                Self::render_inline(out, node);
                nl(out);
            }
            "tr" => {
                nl(out);
                let mut first = true;
                for child in node.children() {
                    if let Node::Element(cel) = child.value() {
                        if matches!(cel.name(), "td" | "th") {
                            if !first {
                                out.push_str("  |  ");
                            }
                            first = false;
                            Self::render_inline(out, &child);
                        }
                    }
                }
                nl(out);
            }
            "pre" => {
                blank(out);
                for child in node.children() {
                    if let Node::Text(text) = child.value() {
                        out.push_str(text.trim_end());
                    }
                }
                blank(out);
            }
            "hr" => {
                blank(out);
                out.push_str("────────────────────────────────────");
                blank(out);
            }
            "img" => {
                let alt = el.attr("alt").unwrap_or("image").trim();
                out.push('[');
                out.push_str(alt);
                out.push(']');
            }
            _ if is_block(tag) => {
                blank(out);
                Self::render_children_flow(out, node);
                blank(out);
            }
            _ => Self::render_inline(out, node),
        }
    }

    /// Render children of a block container, mixing block and inline rules.
    fn render_children_flow(out: &mut String, node: &ego_tree::NodeRef<'_, Node>) {
        for child in node.children() {
            match child.value() {
                Node::Element(_) => Self::render_flow(out, &child),
                Node::Text(text) => push_text(out, text),
                _ => {}
            }
        }
    }

    /// Render a subtree in inline context (text flows on one line).
    fn render_inline(out: &mut String, node: &ego_tree::NodeRef<'_, Node>) {
        match node.value() {
            Node::Text(text) => push_text(out, text),
            Node::Element(el) => {
                let tag = el.name();
                if SKIP_TAGS.contains(&tag) {
                    return;
                }
                match tag {
                    "br" => out.push(' '),
                    "img" => {
                        let alt = el.attr("alt").unwrap_or("image").trim();
                        out.push('[');
                        out.push_str(alt);
                        out.push(']');
                    }
                    _ => {
                        for child in node.children() {
                            Self::render_inline(out, &child);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Build a [`DomNode`] from an html5ever tree node.
    fn build_node(node: &ego_tree::NodeRef<'_, Node>) -> Option<DomNode> {
        match node.value() {
            Node::Element(el) => {
                let children = node
                    .children()
                    .filter_map(|child| Self::build_node(&child))
                    .collect();
                let attrs = el
                    .attrs()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect();
                Some(DomNode {
                    tag: el.name().to_string(),
                    attrs,
                    children,
                    text: String::new(),
                })
            }
            Node::Text(text) => Some(DomNode {
                tag: "#text".into(),
                attrs: Vec::new(),
                children: Vec::new(),
                text: text.to_string(),
            }),
            _ => None,
        }
    }

    /// Collapse repeated blank lines to a single empty separator.
    fn finalize(out: &mut String) {
        let mut result = String::with_capacity(out.len());
        let mut blank = 0usize;
        for line in out.split('\n') {
            if line.trim().is_empty() {
                blank += 1;
                if blank <= 1 {
                    result.push('\n');
                }
            } else {
                blank = 0;
                result.push_str(line.trim_end());
                result.push('\n');
            }
        }
        *out = result;
    }
}

/// Append whitespace-collapsed text, separating it from the previous token
/// with a single space when needed.
fn push_text(out: &mut String, text: &str) {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return;
    }
    if !out.is_empty() && !out.ends_with(' ') && !out.ends_with('\n') {
        out.push(' ');
    }
    out.push_str(&collapsed);
}

/// Ensure `out` ends with a single newline.
fn nl(out: &mut String) {
    while out.ends_with('\n') {
        out.pop();
    }
    if !out.is_empty() {
        out.push('\n');
    }
}

/// Ensure `out` ends with a blank line separating blocks.
fn blank(out: &mut String) {
    while out.ends_with('\n') {
        out.pop();
    }
    if !out.is_empty() {
        out.push('\n');
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_text_simple() {
        let html = "<html><body><p>Hello world</p></body></html>";
        let text = HtmlParser::extract_text(html);
        assert!(text.contains("Hello world"));
    }

    #[test]
    fn test_extract_text_strips_scripts() {
        let html = "<html><script>alert('x')</script><body><p>Hello</p></body></html>";
        let text = HtmlParser::extract_text(html);
        assert!(text.contains("Hello"));
        assert!(!text.contains("alert"));
    }

    #[test]
    fn test_extract_text_preserves_paragraphs() {
        let html = "<p>First paragraph.</p><p>Second paragraph.</p>";
        let text = HtmlParser::extract_text(html);
        assert!(text.contains("First paragraph.\n\nSecond paragraph."));
    }

    #[test]
    fn test_extract_text_heading_markers() {
        let html = "<h1>Title</h1><h3>Sub</h3>";
        let text = HtmlParser::extract_text(html);
        assert!(text.contains("# Title"));
        assert!(text.contains("### Sub"));
    }

    #[test]
    fn test_extract_text_lists() {
        let html = "<ul><li>Alpha</li><li>Beta</li></ul>";
        let text = HtmlParser::extract_text(html);
        assert!(text.contains("• Alpha"));
        assert!(text.contains("• Beta"));

        let ordered = "<ol><li>One</li><li>Two</li></ol>";
        let text = HtmlParser::extract_text(ordered);
        assert!(text.contains("1. One"));
        assert!(text.contains("2. Two"));
    }

    #[test]
    fn test_extract_text_pre_keeps_layout() {
        let html = "<pre>line1\n  line2</pre>";
        let text = HtmlParser::extract_text(html);
        assert!(text.contains("line1\n  line2"));
    }

    #[test]
    fn test_extract_text_handles_broken_nesting() {
        let html = "<p>one<div>two<p>three</p></div>four</p>";
        let text = HtmlParser::extract_text(html);
        assert!(text.contains("one"));
        assert!(text.contains("two"));
        assert!(text.contains("three"));
        assert!(text.contains("four"));
    }

    #[test]
    fn test_extract_text_ignores_head() {
        let html = "<head><title>Hidden</title></head><body><p>Visible</p></body>";
        let text = HtmlParser::extract_text(html);
        assert!(text.contains("Visible"));
        assert!(!text.contains("Hidden"));
    }

    #[test]
    fn test_extract_title() {
        let html = "<html><title>  My  Page </title><body><p>Content</p></body></html>";
        assert_eq!(HtmlParser::extract_title(html), "My Page");
    }

    #[test]
    fn test_extract_links_absolute() {
        let html = r#"<a href="https://example.com">Example</a>"#;
        let links = HtmlParser::extract_links(html, "https://base.com/");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].href, "https://example.com");
        assert_eq!(links[0].text, "Example");
    }

    #[test]
    fn test_extract_links_relative_resolution() {
        let html = r#"<a href="/path/to/page">Rel</a><a href="other.html">Next</a>"#;
        let links = HtmlParser::extract_links(html, "https://example.com/base/");
        assert!(links
            .iter()
            .any(|l| l.href == "https://example.com/path/to/page"));
        assert!(links
            .iter()
            .any(|l| l.href == "https://example.com/base/other.html"));
    }

    #[test]
    fn test_extract_links_protocol_relative() {
        let html = r#"<a href="//cdn.example.com/app.js">CDN</a>"#;
        let links = HtmlParser::extract_links(html, "https://example.com/");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].href, "https://cdn.example.com/app.js");
    }

    #[test]
    fn test_extract_links_filters_non_http() {
        let html = r##"
            <a href="#section">Anchor</a>
            <a href="javascript:void(0)">JS</a>
            <a href="mailto:a@b.c">Mail</a>
        "##;
        let links = HtmlParser::extract_links(html, "https://example.com/");
        assert!(links.is_empty());
    }

    #[test]
    fn test_extract_links_dedupes() {
        let html = r#"<a href="https://example.com/x">A</a><a href="/x">B</a>"#;
        let links = HtmlParser::extract_links(html, "https://example.com/");
        assert_eq!(links.len(), 1);
    }

    #[test]
    fn test_parse_builds_dom_tree() {
        let html = "<html><body><p>Hi <b>there</b></p></body></html>";
        let dom = HtmlParser::parse(html, "https://example.com/");
        assert_eq!(dom.tag, "document");
        assert!(!dom.children.is_empty());
    }
}
