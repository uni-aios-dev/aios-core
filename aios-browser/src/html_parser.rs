use crate::types::{DomNode, Link};

pub struct HtmlParser;

impl HtmlParser {
    pub fn parse(html: &str, base_url: &str) -> DomNode {
        let mut root = DomNode {
            tag: "document".into(),
            attrs: Vec::new(),
            children: Vec::new(),
            text: String::new(),
        };

        let stripped = Self::strip_comments(html);
        let body = Self::extract_body(&stripped);
        let children = Self::parse_elements(&body, base_url);
        root.children = children;

        root
    }

    pub fn extract_text(html: &str) -> String {
        let re_script = regex_lite::Regex::new(r"(?is)<script[^>]*>.*?</script>").unwrap();
        let re_style = regex_lite::Regex::new(r"(?is)<style[^>]*>.*?</style>").unwrap();
        let re_head = regex_lite::Regex::new(r"(?is)<head[^>]*>.*?</head>").unwrap();
        let re_tags = regex_lite::Regex::new(r"<[^>]*>").unwrap();
        let re_whitespace = regex_lite::Regex::new(r"\s+").unwrap();

        let text = re_script.replace_all(html, "");
        let text = re_style.replace_all(&text, "");
        let text = re_head.replace_all(&text, "");
        let text = re_tags.replace_all(&text, " ");
        let text = re_whitespace.replace_all(&text, " ");

        text.trim().to_string()
    }

    pub fn extract_links(html: &str, base_url: &str) -> Vec<Link> {
        let mut links = Vec::new();
        let re =
            regex_lite::Regex::new(r#"(?is)<a\s[^>]*href\s*=\s*"([^"]*)"[^>]*>(.*?)</a>"#).unwrap();

        for cap in re.captures_iter(html) {
            let href = cap.get(1).map(|m| m.as_str()).unwrap_or("").to_string();
            let text = Self::extract_text(cap.get(2).map(|m| m.as_str()).unwrap_or(""));
            let href = Self::resolve_url(&href, base_url);
            if !href.is_empty() {
                links.push(Link { href, text });
            }
        }
        links
    }

    pub fn extract_title(html: &str) -> String {
        let re = regex_lite::Regex::new(r"(?is)<title[^>]*>(.*?)</title>").unwrap();
        re.captures(html)
            .and_then(|cap| cap.get(1))
            .map(|m| Self::extract_text(m.as_str()))
            .unwrap_or_default()
    }

    fn strip_comments(html: &str) -> String {
        let re = regex_lite::Regex::new(r"(?is)<!--.*?-->").unwrap();
        re.replace_all(html, "").to_string()
    }

    fn extract_body(html: &str) -> String {
        let re = regex_lite::Regex::new(r"(?is)<body[^>]*>(.*)</body>").unwrap();
        re.captures(html)
            .and_then(|cap| cap.get(1))
            .map(|m| m.as_str().to_string())
            .unwrap_or_else(|| html.to_string())
    }

    #[allow(clippy::only_used_in_recursion)]
    fn parse_elements(html: &str, base_url: &str) -> Vec<DomNode> {
        let mut nodes = Vec::new();
        let re_tag = regex_lite::Regex::new(r"(?is)<(\w+)([^>]*)>(.*?)</\1>").unwrap();

        for cap in re_tag.captures_iter(html) {
            let tag = cap.get(1).map(|m| m.as_str()).unwrap_or("").to_lowercase();
            let attrs_str = cap.get(2).map(|m| m.as_str()).unwrap_or("");
            let inner = cap.get(3).map(|m| m.as_str()).unwrap_or("");
            let attrs = Self::parse_attrs(attrs_str);
            let children = Self::parse_elements(inner, base_url);
            let text = Self::extract_text(inner);

            nodes.push(DomNode {
                tag,
                attrs,
                children,
                text,
            });
        }

        nodes
    }

    fn parse_attrs(attrs_str: &str) -> Vec<(String, String)> {
        let mut attrs = Vec::new();
        let re = regex_lite::Regex::new(r#"(\w+)\s*=\s*"([^"]*)""#).unwrap();
        for cap in re.captures_iter(attrs_str) {
            if let (Some(name), Some(value)) = (cap.get(1), cap.get(2)) {
                attrs.push((name.as_str().to_string(), value.as_str().to_string()));
            }
        }
        attrs
    }

    fn resolve_url(href: &str, base_url: &str) -> String {
        if href.starts_with("http://") || href.starts_with("https://") {
            href.to_string()
        } else if href.starts_with('/') {
            let base = base_url.trim_end_matches('/');
            let domain = base.split('/').take(3).collect::<Vec<_>>().join("/");
            format!("{domain}{href}")
        } else if href.starts_with('#') {
            String::new()
        } else {
            format!(
                "{}/{}",
                base_url.trim_end_matches('/'),
                href.trim_start_matches('/')
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_text_simple() {
        let html = "<html><body><p>Hello world</p></body></html>";
        let text = HtmlParser::extract_text(html);
        assert_eq!(text, "Hello world");
    }

    #[test]
    fn test_extract_text_strips_scripts() {
        let html = "<html><script>alert('x')</script><body><p>Hello</p></body></html>";
        let text = HtmlParser::extract_text(html);
        assert_eq!(text, "Hello");
    }

    #[test]
    fn test_extract_text_strips_head() {
        let html = "<html><head><title>Test</title></head><body><p>Hello world</p></body></html>";
        let text = HtmlParser::extract_text(html);
        assert_eq!(text, "Hello world");
    }

    #[test]
    fn test_extract_links() {
        let html = r#"<a href="https://example.com">Example</a>"#;
        let links = HtmlParser::extract_links(html, "https://base.com");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].href, "https://example.com");
        assert_eq!(links[0].text, "Example");
    }

    #[test]
    fn test_extract_title() {
        let html = "<html><title>My Page</title><body><p>Content</p></body></html>";
        assert_eq!(HtmlParser::extract_title(html), "My Page");
    }

    #[test]
    fn test_resolve_absolute_url() {
        let result = HtmlParser::resolve_url("https://example.com/page", "https://base.com");
        assert_eq!(result, "https://example.com/page");
    }

    #[test]
    fn test_resolve_relative_url() {
        let result = HtmlParser::resolve_url("/path/to/page", "https://example.com/base/");
        assert_eq!(result, "https://example.com/path/to/page");
    }

    #[test]
    fn test_strip_comments() {
        let html = "<!-- comment --><p>text</p>";
        assert_eq!(HtmlParser::extract_text(html), "text");
    }
}
