//! Serialize a parsed [`DomNode`] tree back into HTML text.
//!
//! Round-trips with [`crate::html_parser::HtmlParser::parse`]: what the
//! parser produced, the serializer reproduces (up to html5ever's
//! normalisation), and what scripts mutated, the serializer reflects — the
//! re-parsed document is what the text renderer and link extractor see.

use crate::types::DomNode;

/// Elements that never take a closing tag.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Elements whose content is raw text (no entity decoding on re-parse).
const RAW_TEXT: &[&str] = &["script", "style"];

/// Render `node` (and its subtree) as HTML. A `document` root renders its
/// children only.
pub fn dom_to_html(node: &DomNode) -> String {
    let mut out = String::new();
    write_node(&mut out, node, false);
    out
}

fn write_node(out: &mut String, node: &DomNode, raw_parent: bool) {
    if node.tag == "#text" {
        if raw_parent {
            out.push_str(&node.text);
        } else {
            escape_text(out, &node.text);
        }
        return;
    }
    if node.tag == "document" || node.tag.starts_with('#') {
        for child in &node.children {
            write_node(out, child, raw_parent);
        }
        return;
    }
    out.push('<');
    out.push_str(&node.tag);
    for (key, value) in &node.attrs {
        out.push(' ');
        push_attr_name(out, key);
        out.push_str("=\"");
        escape_attr(out, value);
        out.push('"');
    }
    if VOID.contains(&node.tag.as_str()) {
        out.push('>');
        return;
    }
    out.push('>');
    let raw = RAW_TEXT.contains(&node.tag.as_str());
    for child in &node.children {
        write_node(out, child, raw);
    }
    out.push_str("</");
    out.push_str(&node.tag);
    out.push('>');
}

/// Write an attribute name, dropping characters that would break the
/// surrounding markup (scripts can produce arbitrary names via
/// `setAttribute`).
fn push_attr_name(out: &mut String, name: &str) {
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | ':' | '.') {
            out.push(ch);
        }
    }
}

fn escape_text(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
}

fn escape_attr(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html_parser::HtmlParser;

    #[test]
    fn roundtrips_simple_document() {
        let html = "<html><head><title>T</title></head><body><p>Hello</p></body></html>";
        let dom = HtmlParser::parse(html, "https://example.com/");
        let out = dom_to_html(&dom);
        assert!(out.contains("<title>T</title>"), "out: {out}");
        assert!(out.contains("<p>Hello</p>"), "out: {out}");
        assert!(HtmlParser::extract_text(&out).contains("Hello"));
        assert_eq!(HtmlParser::extract_title(&out), "T");
    }

    #[test]
    fn void_elements_stay_unpaired() {
        let dom = HtmlParser::parse(r#"<img src="a.png" alt="x"><br><hr>"#, "https://e.com/");
        let out = dom_to_html(&dom);
        assert!(out.contains("<img "), "out: {out}");
        assert!(out.contains(r#"src="a.png""#), "out: {out}");
        assert!(out.contains(r#"alt="x""#), "out: {out}");
        assert!(!out.contains("</img>"), "out: {out}");
        assert!(out.contains("<br>"), "out: {out}");
        assert!(!out.contains("</br>"), "out: {out}");
    }

    #[test]
    fn escapes_text_and_attributes() {
        let dom = HtmlParser::parse(
            r#"<p title="a&quot;b">5 &amp; 6 &lt; 7</p>"#,
            "https://e.com/",
        );
        let out = dom_to_html(&dom);
        assert!(out.contains("&amp;"), "out: {out}");
        assert!(out.contains("&lt; 7"), "out: {out}");
        // Re-parse keeps the text intact.
        let text = HtmlParser::extract_text(&out);
        assert!(text.contains("5 & 6 < 7"), "text: {text}");
    }

    #[test]
    fn script_content_is_raw() {
        let dom = HtmlParser::parse("<script>if (a < b && c > d) x()</script>", "https://e.com/");
        let out = dom_to_html(&dom);
        assert!(out.contains("if (a < b && c > d) x()"), "out: {out}");
        assert!(!out.contains("&lt;"), "out: {out}");
    }

    #[test]
    fn text_node_roundtrip_preserves_links() {
        let html = r#"<a href="https://example.com/x">Click &gt; here</a>"#;
        let dom = HtmlParser::parse(html, "https://example.com/");
        let out = dom_to_html(&dom);
        let links = HtmlParser::extract_links(&out, "https://example.com/");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].href, "https://example.com/x");
    }
}
