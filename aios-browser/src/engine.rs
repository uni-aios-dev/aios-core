use crate::html_parser::HtmlParser;
use crate::network::NetworkClient;
use crate::script::{url_matches, ScriptEngine, ScriptReport, ScriptValue};
use crate::serialize::dom_to_html;
use crate::types::{BrowserConfig, BrowserError, DomNode, Page, UserScript};

/// A `<script>` element collected from the parsed document, in document
/// order: either inline code or a resolved external URL.
struct ScriptSpec {
    label: String,
    code: Option<String>,
    src: Option<String>,
}

pub struct BrowserEngine {
    config: BrowserConfig,
    network: NetworkClient,
    user_scripts: Vec<UserScript>,
}

impl BrowserEngine {
    pub fn new(config: BrowserConfig) -> Self {
        let network = NetworkClient::new(&config);
        Self {
            config,
            network,
            user_scripts: Vec::new(),
        }
    }

    /// Register a userscript run document-end on every page whose URL
    /// matches `pattern` (see [`url_matches`]).
    pub fn add_user_script(&mut self, pattern: impl Into<String>, source: impl Into<String>) {
        self.user_scripts.push(UserScript {
            pattern: pattern.into(),
            source: source.into(),
        });
    }

    /// Currently registered userscripts.
    pub fn user_scripts(&self) -> &[UserScript] {
        &self.user_scripts
    }

    pub async fn navigate(&self, url: &str) -> Result<Page, BrowserError> {
        let html = self.network.fetch(url).await?;
        self.build_page(url, html).await
    }

    /// Build a [`Page`] from fetched HTML: run the embedded JS engine over
    /// the document (when `config.execute_scripts`), re-serialize the
    /// mutated DOM, then extract title/text/links from the *post-script*
    /// markup. Falls back to a headless render when the plain fetch (after
    /// scripts) produced no readable text (JS-heavy sites); the dump's own
    /// scripts are not re-executed — the headless browser already ran them.
    async fn build_page(&self, url: &str, html: String) -> Result<Page, BrowserError> {
        let mut report = ScriptReport::default();
        let mut title = HtmlParser::extract_title(&html);
        let mut html_out = html;

        if self.config.execute_scripts {
            match self
                .run_page_scripts(url, &html_out, &title, &mut report)
                .await
            {
                Ok((scripted_html, scripted_title)) => {
                    html_out = scripted_html;
                    title = scripted_title;
                }
                Err(e) => report.errors.push(format!("script engine: {e}")),
            }
        }
        cap_console(&mut report.console);

        let text_content = HtmlParser::extract_text(&html_out);
        if self.config.headless_fallback && crate::headless::looks_like_js_shell(&text_content) {
            if let Ok(dumped) = crate::headless::render_to_html(url).await {
                if crate::headless::has_more_content(&text_content, &dumped) {
                    let dump_title = HtmlParser::extract_title(&dumped);
                    let dumped_text = HtmlParser::extract_text(&dumped);
                    let links = HtmlParser::extract_links(&dumped, url);
                    return Ok(Page {
                        url: url.to_string(),
                        title: if dump_title.is_empty() {
                            title
                        } else {
                            dump_title
                        },
                        text_content: dumped_text,
                        html: dumped,
                        links,
                        console: report.console,
                        scripts_executed: report.executed,
                        script_errors: report.errors,
                    });
                }
            }
        }

        let links = HtmlParser::extract_links(&html_out, url);
        Ok(Page {
            url: url.to_string(),
            title,
            text_content,
            html: html_out,
            links,
            console: report.console,
            scripts_executed: report.executed,
            script_errors: report.errors,
        })
    }

    /// Execute the document's scripts (inline in order, externals fetched
    /// first so document order is preserved) plus matching userscripts,
    /// then flush queued timers. Returns the re-serialized post-script HTML
    /// and the final `document.title`.
    async fn run_page_scripts(
        &self,
        url: &str,
        html: &str,
        title: &str,
        report: &mut ScriptReport,
    ) -> Result<(String, String), String> {
        let dom = HtmlParser::parse(html, url);
        let specs = collect_scripts(&dom, url, report);

        let mut sources: Vec<(String, String)> = Vec::new();
        for spec in specs {
            if let Some(code) = spec.code {
                sources.push((spec.label, code));
            } else if let Some(src) = spec.src {
                match self.network.fetch(&src).await {
                    Ok(code) => sources.push((spec.label, code)),
                    Err(e) => report
                        .errors
                        .push(format!("{}: fetch failed: {e}", spec.label)),
                }
            }
        }

        let mut engine = ScriptEngine::new(&dom, url, title, &self.config.user_agent)?;
        for (label, code) in &sources {
            match engine.run(code) {
                Ok(()) => report.executed += 1,
                Err(e) => report.errors.push(format!("{label}: {e}")),
            }
        }
        for user in &self.user_scripts {
            if url_matches(&user.pattern, url) {
                match engine.run(&user.source) {
                    Ok(()) => report.executed += 1,
                    Err(e) => report
                        .errors
                        .push(format!("user script '{}': {e}", user.pattern)),
                }
            }
        }
        engine.flush_timers();

        let outcome = engine.finish()?;
        if let Some(nav) = &outcome.nav {
            report
                .console
                .push(format!("[js] navigation to {nav} requested (not followed)"));
        }
        report.console.extend(outcome.console);
        cap_console(&mut report.console);

        let new_title = if outcome.title.is_empty() {
            title.to_string()
        } else {
            outcome.title
        };
        Ok((dom_to_html(&outcome.dom), new_title))
    }

    /// Evaluate `source` against a post-load page: the stored HTML is
    /// parsed into a fresh context (the page's own scripts are not
    /// re-run — the HTML already contains their output), the snippet runs,
    /// and its value plus the console lines are returned.
    pub fn evaluate(
        &self,
        page_html: &str,
        url: &str,
        source: &str,
    ) -> Result<(ScriptValue, Vec<String>), BrowserError> {
        let dom = HtmlParser::parse(page_html, url);
        let title = HtmlParser::extract_title(page_html);
        let mut engine = ScriptEngine::new(&dom, url, &title, &self.config.user_agent)
            .map_err(BrowserError::ScriptError)?;
        let value = engine.evaluate(source).map_err(BrowserError::ScriptError)?;
        engine.flush_timers();
        let outcome = engine.finish().map_err(BrowserError::ScriptError)?;
        Ok((value, outcome.console))
    }

    pub fn config(&self) -> &BrowserConfig {
        &self.config
    }
}

/// Walk the document in order and collect every executable `<script>`:
/// inline code as-is, external `src` resolved against `base_url`.
/// Non-JS types are skipped; `module` is skipped with a console note
/// (the embedded engine has no module loader).
fn collect_scripts(dom: &DomNode, base_url: &str, report: &mut ScriptReport) -> Vec<ScriptSpec> {
    let mut out = Vec::new();
    let mut index = 0usize;
    let base = url::Url::parse(base_url).ok();
    walk(dom, &mut |node| {
        if node.tag != "script" {
            return;
        }
        index += 1;
        let get = |k: &str| {
            node.attrs
                .iter()
                .find(|(name, _)| name == k)
                .map(|(_, v)| v.as_str())
                .unwrap_or("")
        };
        let raw_type = get("type").to_ascii_lowercase();
        let kind = raw_type.split(';').next().unwrap_or("").trim().to_string();
        match kind.as_str() {
            ""
            | "text/javascript"
            | "application/javascript"
            | "text/ecmascript"
            | "application/ecmascript" => {}
            "module" => {
                report.console.push(format!(
                    "[js] skipped module script #{index} (no module loader)"
                ));
                return;
            }
            _ => return,
        }

        let src = get("src");
        if !src.is_empty() {
            let label = format!("script #{index} [src {}]", src);
            let resolved = base
                .as_ref()
                .and_then(|b| b.join(src).ok())
                .map(|u| u.to_string());
            match resolved {
                Some(url) => out.push(ScriptSpec {
                    label,
                    code: None,
                    src: Some(url),
                }),
                None => report
                    .errors
                    .push(format!("script #{index}: cannot resolve src {src}")),
            }
            return;
        }

        let mut code = String::new();
        for child in &node.children {
            if child.tag == "#text" {
                code.push_str(&child.text);
            }
        }
        if code.trim().is_empty() {
            return;
        }
        out.push(ScriptSpec {
            label: format!("script #{index}"),
            code: Some(code),
            src: None,
        });
    });
    out
}

/// Depth-first preorder walk over element nodes.
fn walk(node: &DomNode, f: &mut impl FnMut(&DomNode)) {
    f(node);
    for child in &node.children {
        walk(child, f);
    }
}

/// Enforce the console line cap after post-processing pushes.
fn cap_console(console: &mut Vec<String>) {
    const MAX: usize = 200;
    if console.len() > MAX {
        let drop = console.len() - MAX;
        console.drain(0..drop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_browser_engine_creation() {
        let config = BrowserConfig::default();
        let engine = BrowserEngine::new(config);
        assert_eq!(engine.config().user_agent, "AIOS-Browser/0.1");
        assert!(engine.config().execute_scripts);
        assert!(engine.user_scripts().is_empty());
    }

    #[test]
    fn test_browser_config_defaults() {
        let config = BrowserConfig::default();
        assert_eq!(config.timeout_secs, 30);
        assert!(config.sandbox_enabled);
        assert!(config.headless_fallback);
        assert!(config.execute_scripts);
    }

    #[tokio::test]
    async fn test_build_page_rich_text_skips_fallback() {
        let html = format!(
            "<html><head><title>T</title></head><body><p>{}</p></body></html>",
            "word ".repeat(300)
        );
        let engine = BrowserEngine::new(BrowserConfig::default());
        let page = engine
            .build_page("https://example.com/", html.to_string())
            .await
            .unwrap();
        assert_eq!(page.title, "T");
        assert!(page.text_content.contains("word"));
        assert_eq!(page.url, "https://example.com/");
    }

    #[tokio::test]
    async fn test_build_page_shell_no_crash_with_fallback_disabled() {
        let html = "<html><body><div id=\"app\">Loading...</div></body></html>";
        let cfg = BrowserConfig {
            headless_fallback: false,
            ..Default::default()
        };
        let engine = BrowserEngine::new(cfg);
        let page = engine
            .build_page("https://example.com/", html.to_string())
            .await
            .unwrap();
        assert_eq!(page.text_content, "Loading...");
    }

    #[tokio::test]
    async fn test_navigate_invalid_url() {
        let config = BrowserConfig::default();
        let engine = BrowserEngine::new(config);
        let result = engine.navigate("http://invalid.nonexistent.domain").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn inline_script_mutates_rendered_text() {
        let html = r#"
            <html><head><title>Page</title></head>
            <body><div id="out"></div>
            <script>document.getElementById("out").textContent = "rendered by JS";</script>
            </body></html>"#;
        let engine = BrowserEngine::new(BrowserConfig {
            headless_fallback: false,
            ..Default::default()
        });
        let page = engine
            .build_page("https://example.com/", html.to_string())
            .await
            .unwrap();
        assert!(
            page.text_content.contains("rendered by JS"),
            "text: {}",
            page.text_content
        );
        assert_eq!(page.scripts_executed, 1);
        assert!(
            page.script_errors.is_empty(),
            "errors: {:?}",
            page.script_errors
        );
    }

    #[tokio::test]
    async fn script_error_is_captured_not_fatal() {
        let html = r#"
            <html><body>
            <script>undefinedThing()</script>
            <p>visible</p>
            </body></html>"#;
        let engine = BrowserEngine::new(BrowserConfig {
            headless_fallback: false,
            ..Default::default()
        });
        let page = engine
            .build_page("https://example.com/", html.to_string())
            .await
            .unwrap();
        assert!(page.text_content.contains("visible"));
        assert_eq!(page.scripts_executed, 0);
        assert_eq!(page.script_errors.len(), 1);
        assert!(
            page.script_errors[0].contains("script #1"),
            "errors: {:?}",
            page.script_errors
        );
    }

    #[tokio::test]
    async fn execute_scripts_off_keeps_placeholder() {
        let html = r#"
            <html><body><div id="out">Loading...</div>
            <script>document.getElementById("out").textContent = "should not run";</script>
            </body></html>"#;
        let engine = BrowserEngine::new(BrowserConfig {
            headless_fallback: false,
            execute_scripts: false,
            ..Default::default()
        });
        let page = engine
            .build_page("https://example.com/", html.to_string())
            .await
            .unwrap();
        assert!(page.text_content.contains("Loading..."));
        assert!(!page.text_content.contains("should not run"));
        assert_eq!(page.scripts_executed, 0);
    }

    #[tokio::test]
    async fn console_and_title_reach_page() {
        let html = r#"
            <html><head><title>Old</title></head><body>
            <script>console.log("boot", 1); document.title = "New Title";</script>
            </body></html>"#;
        let engine = BrowserEngine::new(BrowserConfig {
            headless_fallback: false,
            ..Default::default()
        });
        let page = engine
            .build_page("https://example.com/", html.to_string())
            .await
            .unwrap();
        assert_eq!(page.title, "New Title");
        assert_eq!(page.console, vec!["boot 1".to_string()]);
    }

    #[tokio::test]
    async fn script_created_link_is_extracted() {
        let html = r#"
            <html><body><div id="nav"></div>
            <script>
              document.getElementById("nav").innerHTML =
                '<a href="https://example.com/generated">Generated</a>';
            </script>
            </body></html>"#;
        let engine = BrowserEngine::new(BrowserConfig {
            headless_fallback: false,
            ..Default::default()
        });
        let page = engine
            .build_page("https://example.com/", html.to_string())
            .await
            .unwrap();
        assert!(
            page.links
                .iter()
                .any(|l| l.href == "https://example.com/generated"),
            "links: {:?}",
            page.links
        );
    }

    #[tokio::test]
    async fn user_script_runs_document_end() {
        let html = "<html><head><title>Base</title></head><body><p>x</p></body></html>";
        let mut engine = BrowserEngine::new(BrowserConfig {
            headless_fallback: false,
            ..Default::default()
        });
        engine.add_user_script("*", r#"document.title = "UserScript";"#);
        let page = engine
            .build_page("https://example.com/", html.to_string())
            .await
            .unwrap();
        assert_eq!(page.title, "UserScript");
        assert_eq!(page.scripts_executed, 1);
    }

    #[tokio::test]
    async fn external_script_fetch_failure_is_reported() {
        let html = r#"
            <html><body>
            <script src="http://invalid.nonexistent.domain/app.js"></script>
            <p>content stays</p>
            </body></html>"#;
        let engine = BrowserEngine::new(BrowserConfig {
            headless_fallback: false,
            timeout_secs: 5,
            ..Default::default()
        });
        let page = engine
            .build_page("https://example.com/", html.to_string())
            .await
            .unwrap();
        assert!(page.text_content.contains("content stays"));
        assert_eq!(page.scripts_executed, 0);
        assert_eq!(page.script_errors.len(), 1);
        assert!(
            page.script_errors[0].contains("fetch failed"),
            "errors: {:?}",
            page.script_errors
        );
    }

    #[test]
    fn evaluate_returns_value_and_console() {
        let engine = BrowserEngine::new(BrowserConfig {
            headless_fallback: false,
            ..Default::default()
        });
        let html = "<html><head><title>T</title></head><body><p id='a'>hello</p></body></html>";
        let (value, console) = engine
            .evaluate(
                html,
                "https://example.com/",
                r#"document.getElementById('a').textContent"#,
            )
            .unwrap();
        assert_eq!(value, ScriptValue::String("hello".into()));
        assert!(console.is_empty());
    }

    #[test]
    fn evaluate_sees_scripted_dom() {
        let engine = BrowserEngine::new(BrowserConfig {
            headless_fallback: false,
            ..Default::default()
        });
        // Post-script HTML (as stored in Page.html): the injected text is
        // already part of the markup the evaluation replays.
        let html = r#"<html><head><title>T</title></head><body><p>stale</p></body></html>"#;
        let (value, _) = engine
            .evaluate(html, "https://example.com/", r#"document.title"#)
            .unwrap();
        assert_eq!(value, ScriptValue::String("T".into()));
    }
}
