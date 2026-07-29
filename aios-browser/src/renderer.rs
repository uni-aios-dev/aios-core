use crate::types::{DomNode, Page};

pub struct Renderer;

impl Renderer {
    pub fn to_text(node: &DomNode, indent: usize) -> String {
        let mut result = String::new();
        let prefix = "  ".repeat(indent);

        match node.tag.as_str() {
            "document" | "html" | "body" => {
                for child in &node.children {
                    result.push_str(&Self::to_text(child, indent));
                }
                if !node.text.trim().is_empty() {
                    result.push_str(&prefix);
                    result.push_str(node.text.trim());
                    result.push('\n');
                }
            }
            "p" | "div" | "span" | "section" | "article" | "main" => {
                if !node.text.trim().is_empty() {
                    result.push_str(&prefix);
                    result.push_str(node.text.trim());
                    result.push('\n');
                }
                for child in &node.children {
                    result.push_str(&Self::to_text(child, indent + 1));
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                result.push_str(&prefix);
                let level = node.tag[1..].parse::<usize>().unwrap_or(1);
                result.push_str(&"#".repeat(level));
                result.push(' ');
                result.push_str(node.text.trim());
                result.push('\n');
            }
            "a" => {
                let href = node
                    .attrs
                    .iter()
                    .find(|(k, _)| k == "href")
                    .map(|(_, v)| v.as_str())
                    .unwrap_or("");
                if !href.is_empty() {
                    result.push_str(&prefix);
                    result.push('[');
                    result.push_str(node.text.trim());
                    result.push_str("](");
                    result.push_str(href);
                    result.push_str(")\n");
                }
            }
            "ul" | "ol" => {
                for child in &node.children {
                    result.push_str(&prefix);
                    result.push_str("  • ");
                    result.push_str(child.text.trim());
                    result.push('\n');
                }
            }
            "li" => {
                result.push_str(&prefix);
                result.push_str("  - ");
                result.push_str(node.text.trim());
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
                result.push('[');
                result.push_str(alt);
                result.push_str("]\n");
            }
            _ => {
                if !node.text.trim().is_empty() {
                    result.push_str(&prefix);
                    result.push_str(node.text.trim());
                    result.push('\n');
                }
                for child in &node.children {
                    result.push_str(&Self::to_text(child, indent + 1));
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
