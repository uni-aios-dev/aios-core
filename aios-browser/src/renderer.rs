use crate::types::{DomNode, Page};

/// Renders a parsed [`DomNode`] tree back into readable plain text.
pub struct Renderer;

impl Renderer {
    /// Collect the inline text of a subtree without adding line breaks.
    fn inline(node: &DomNode) -> String {
        let mut raw = String::new();
        Self::inline_raw(node, &mut raw);
        raw.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn inline_raw(node: &DomNode, out: &mut String) {
        match node.tag.as_str() {
            "#text" => out.push_str(&node.text),
            "img" => {
                let alt = node
                    .attrs
                    .iter()
                    .find(|(k, _)| k == "alt")
                    .map(|(_, v)| v.as_str())
                    .unwrap_or("image");
                out.push('[');
                out.push_str(alt);
                out.push(']');
            }
            "br" => out.push(' '),
            "script" | "style" | "head" | "noscript" | "template" | "svg" | "iframe" | "title" => {}
            _ => {
                for child in &node.children {
                    Self::inline_raw(child, out);
                }
            }
        }
    }

    pub fn to_text(node: &DomNode, indent: usize) -> String {
        let mut result = String::new();
        let prefix = "  ".repeat(indent);

        match node.tag.as_str() {
            "#text" => {
                let t = node.text.split_whitespace().collect::<Vec<_>>().join(" ");
                if !t.is_empty() {
                    result.push_str(&prefix);
                    result.push_str(&t);
                    result.push('\n');
                }
            }
            "document" | "html" | "body" | "div" | "section" | "article" | "main" | "header"
            | "footer" | "aside" | "blockquote" | "figure" | "figcaption" | "address" | "form"
            | "fieldset" | "details" | "summary" => {
                for child in &node.children {
                    result.push_str(&Self::to_text(child, indent));
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = node.tag[1..].parse::<usize>().unwrap_or(1);
                result.push_str(&prefix);
                result.push_str(&"#".repeat(level));
                result.push(' ');
                result.push_str(&Self::inline(node));
                result.push('\n');
            }
            "p" => {
                let text = Self::inline(node);
                if !text.is_empty() {
                    result.push_str(&prefix);
                    result.push_str(&text);
                    result.push('\n');
                }
            }
            "ul" => {
                for child in &node.children {
                    result.push_str(&format!("{}  • ", prefix));
                    result.push_str(&Self::inline(child));
                    result.push('\n');
                }
            }
            "ol" => {
                for (i, child) in node.children.iter().enumerate() {
                    result.push_str(&format!("{}  {}. ", prefix, i + 1));
                    result.push_str(&Self::inline(child));
                    result.push('\n');
                }
            }
            "li" => {
                for child in &node.children {
                    result.push_str(&Self::to_text(child, indent + 1));
                }
            }
            "tr" => {
                for (i, child) in node.children.iter().enumerate() {
                    if i > 0 {
                        result.push_str("  |  ");
                    }
                    result.push_str(&Self::inline(child));
                }
                result.push('\n');
            }
            "pre" => {
                result.push_str(&prefix);
                result.push_str(&node.text);
                result.push('\n');
            }
            "img" => {
                let alt = node
                    .attrs
                    .iter()
                    .find(|(k, _)| k == "alt")
                    .map(|(_, v)| v.as_str())
                    .unwrap_or("image");
                result.push_str(&prefix);
                result.push_str(&format!("[{alt}]"));
                result.push('\n');
            }
            "br" => result.push('\n'),
            "script" | "style" | "head" | "noscript" | "template" | "svg" | "iframe" | "title" => {}
            _ => {
                for child in &node.children {
                    result.push_str(&Self::to_text(child, indent));
                }
            }
        }

        result
    }

    pub fn render_page(page: &Page) -> String {
        let mut output = String::new();

        output.push_str(&format!("═══ {} ═══\n\n", page.title));
        output.push_str(&format!("URL: {}\n\n", page.url));

        output.push_str(&page.text_content);
        output.push('\n');

        if !page.links.is_empty() {
            output.push_str("\n─── Links ───\n");
            for (i, link) in page.links.iter().enumerate() {
                output.push_str(&format!("{}. [{}]({})\n", i + 1, link.text, link.href));
            }
        }

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html_parser::HtmlParser;

    #[test]
    fn render_heading_and_paragraph() {
        let dom = HtmlParser::parse(
            "<h1>Hello</h1><p>World is <b>big</b>.</p>",
            "https://example.com/",
        );
        let text = Renderer::to_text(&dom, 0);
        assert!(text.contains("# Hello"));
        assert!(text.contains("World is big."));
    }

    #[test]
    fn render_lists() {
        let dom = HtmlParser::parse("<ul><li>a</li><li>b</li></ul>", "https://example.com/");
        let text = Renderer::to_text(&dom, 0);
        assert!(text.contains("• a"));
        assert!(text.contains("• b"));
    }

    #[test]
    fn render_links_as_text() {
        let dom = HtmlParser::parse("<p>see <a href='/x'>docs</a></p>", "https://example.com/");
        let text = Renderer::to_text(&dom, 0);
        assert!(text.contains("see docs"));
    }
}
