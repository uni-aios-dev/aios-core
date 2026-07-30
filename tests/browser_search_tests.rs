use aios_browser::html_parser::HtmlParser;
use aios_browser::types::BrowserConfig;
use aios_browser::BrowserEngine;

#[test]
fn test_html_parser_extract_text() {
    let html = "<html><head><title>Test</title></head><body><p>Hello world</p></body></html>";
    let text = HtmlParser::extract_text(html);
    assert_eq!(text, "Hello world");
}

#[test]
fn test_html_parser_links() {
    let html = r#"<a href="https://example.com">Example Site</a><a href="/about">About</a>"#;
    let links = HtmlParser::extract_links(html, "https://base.com");
    assert_eq!(links.len(), 2);
    assert_eq!(links[0].href, "https://example.com");
    assert_eq!(links[1].href, "https://base.com/about");
}

#[test]
fn test_html_parser_title() {
    let html = "<html><title>My Page</title></html>";
    assert_eq!(HtmlParser::extract_title(html), "My Page");
}

#[test]
fn test_html_parser_strips_script() {
    let html = "<html><script>alert('x')</script><body><p>Hello</p></body></html>";
    let text = HtmlParser::extract_text(html);
    assert_eq!(text, "Hello");
}

#[test]
fn test_browser_config_default() {
    let cfg = BrowserConfig::default();
    assert_eq!(cfg.user_agent, "AIOS-Browser/0.1");
    assert_eq!(cfg.timeout_secs, 30);
    assert!(cfg.sandbox_enabled);
}

#[test]
fn test_html_parser_complex_links() {
    let html = r#"<a href="https://example.com/page?q=1&lang=en">Link</a>"#;
    let links = HtmlParser::extract_links(html, "https://base.com");
    assert_eq!(links.len(), 1);
    assert!(links[0].href.contains("example.com"));
}

use aios_search::backends::DuckDuckGoBackend;

#[test]
fn test_duckduckgo_parse_results() {
    let html = r##"
    <div class="result">
        <a class="result__a" href="https://example.com">Example</a>
        <a class="result__snippet" href="#">Test snippet content</a>
    </div>
    "##;
    let results = DuckDuckGoBackend::parse_html_response(html);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].url, "https://example.com");
}
