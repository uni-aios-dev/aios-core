use crate::html_parser::HtmlParser;
use crate::network::NetworkClient;
use crate::script::{url_matches, ScriptEngine, ScriptReport, ScriptValue};
use crate::serialize::dom_to_html;
use crate::types::{BrowserConfig, BrowserError, DomNode, Page, UserScript};

/// Additional page loads a single `navigate()` may trigger through script
/// navigation (`location.href = ...`) and `<meta http-equiv="refresh">`.
const MAX_NAV_HOPS: usize = 5;
/// Upper bound of discovery rounds for `<script>` elements created by page
/// code (each round executes everything collected since the previous one).
const MAX_DYNAMIC_ROUNDS: usize = 8;
/// Upper bound of page-created `<script>` elements executed per page.
const MAX_DYNAMIC_SCRIPTS: usize = 20;

/// A `<script>` element collected from the parsed document, in document
/// order: either inline code or a resolved external URL.
struct ScriptSpec {
    label: String,
    code: Option<String>,
    src: Option<String>,
}

/// Result of one page's script pass: the mutated markup, the final
/// `document.title` and any navigation the scripts requested (to be
/// followed by [`BrowserEngine::build_page`]'s navigation loop).
struct PageRun {
    html: String,
    title: String,
    nav: Option<String>,
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
    /// markup. Script navigation (`location.href`) and zero-delay
    /// `<meta http-equiv="refresh">` are followed up to [`MAX_NAV_HOPS`]
    /// times; each hop loads the target and repeats the pipeline, so the
    /// returned [`Page::url`] is the final URL. Falls back to a headless
    /// render when the plain fetch (after scripts) produced no readable
    /// text (JS-heavy sites); the dump's own scripts are not re-executed —
    /// the headless browser already ran them.
    async fn build_page(&self, url: &str, html: String) -> Result<Page, BrowserError> {
        let mut report = ScriptReport::default();
        let mut cur_url = url.to_string();
        let mut html_out = html;
        let mut title = HtmlParser::extract_title(&html_out);
        let mut hops = 0usize;

        loop {
            let mut nav = None;
            if self.config.execute_scripts {
                match self
                    .run_page_scripts(&cur_url, &html_out, &title, &mut report)
                    .await
                {
                    Ok(run) => {
                        html_out = run.html;
                        title = run.title;
                        nav = run.nav;
                    }
                    Err(e) => report.errors.push(format!("script engine: {e}")),
                }
            }
            if nav.is_none() {
                if let Some(refresh) = HtmlParser::extract_meta_refresh(&html_out) {
                    if refresh.delay_secs == 0 {
                        nav = refresh.target;
                    } else {
                        report.console.push(format!(
                            "[nav] meta refresh in {}s ignored (delay > 0)",
                            refresh.delay_secs
                        ));
                    }
                }
            }
            let Some(nav) = nav else { break };

            let resolved = match url::Url::parse(&cur_url).and_then(|base| base.join(&nav)) {
                Ok(u) => u.to_string(),
                Err(e) => {
                    report
                        .errors
                        .push(format!("[nav] cannot resolve {nav:?}: {e}"));
                    break;
                }
            };
            if !(resolved.starts_with("http://") || resolved.starts_with("https://")) {
                report
                    .console
                    .push(format!("[nav] refused non-http target {resolved}"));
                break;
            }
            if resolved == cur_url {
                report
                    .console
                    .push(format!("[nav] already on {resolved}, stopping"));
                break;
            }
            if hops >= MAX_NAV_HOPS {
                report.console.push(format!(
                    "[nav] hop limit {MAX_NAV_HOPS} reached, staying on {cur_url}"
                ));
                break;
            }
            hops += 1;
            report.console.push(format!("[nav] -> {resolved}"));
            match self.network.fetch(&resolved).await {
                Ok(next_html) => {
                    cur_url = resolved;
                    html_out = next_html;
                    title = HtmlParser::extract_title(&html_out);
                }
                Err(e) => {
                    report
                        .errors
                        .push(format!("[nav] failed to load {resolved}: {e}"));
                    break;
                }
            }
        }
        cap_console(&mut report.console);

        let text_content = HtmlParser::extract_text(&html_out);
        if self.config.headless_fallback && crate::headless::looks_like_js_shell(&text_content) {
            if let Ok(dumped) = crate::headless::render_to_html(&cur_url).await {
                if crate::headless::has_more_content(&text_content, &dumped) {
                    let dump_title = HtmlParser::extract_title(&dumped);
                    let dumped_text = HtmlParser::extract_text(&dumped);
                    let links = HtmlParser::extract_links(&dumped, &cur_url);
                    return Ok(Page {
                        url: cur_url,
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

        let links = HtmlParser::extract_links(&html_out, &cur_url);
        Ok(Page {
            url: cur_url,
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
    /// discover and run `<script>` elements created by page code (capped
    /// rounds), fire the synthetic `DOMContentLoaded`/`load` lifecycle events
    /// with a timer flush in between, then read back the mutated DOM.
    /// Returns the re-serialized post-script HTML, the final
    /// `document.title` and any navigation the scripts requested.
    async fn run_page_scripts(
        &self,
        url: &str,
        html: &str,
        title: &str,
        report: &mut ScriptReport,
    ) -> Result<PageRun, String> {
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

        let mut rounds = 0usize;
        let mut dynamic = 0usize;
        self.run_dynamic_rounds(url, &mut engine, report, &mut rounds, &mut dynamic)
            .await;
        if let Err(e) = engine.fire_dom_content_loaded() {
            report.errors.push(format!("DOMContentLoaded: {e}"));
        }
        self.run_dynamic_rounds(url, &mut engine, report, &mut rounds, &mut dynamic)
            .await;
        engine.flush_timers();
        self.run_dynamic_rounds(url, &mut engine, report, &mut rounds, &mut dynamic)
            .await;
        if let Err(e) = engine.fire_load() {
            report.errors.push(format!("load: {e}"));
        }
        self.run_dynamic_rounds(url, &mut engine, report, &mut rounds, &mut dynamic)
            .await;
        if rounds >= MAX_DYNAMIC_ROUNDS {
            report.console.push(format!(
                "[js] dynamic script round limit reached ({MAX_DYNAMIC_ROUNDS})"
            ));
        }

        let outcome = engine.finish()?;
        report.console.extend(outcome.console);
        cap_console(&mut report.console);

        let new_title = if outcome.title.is_empty() {
            title.to_string()
        } else {
            outcome.title
        };
        Ok(PageRun {
            html: dom_to_html(&outcome.dom),
            title: new_title,
            nav: outcome.nav,
        })
    }

    /// Collect and execute `<script>` elements created by page code since the
    /// last round, up to [`MAX_DYNAMIC_ROUNDS`] rounds and
    /// [`MAX_DYNAMIC_SCRIPTS`] scripts overall (both shared across the
    /// lifecycle phases of one page). External `src` scripts are fetched
    /// through the page's [`NetworkClient`].
    async fn run_dynamic_rounds(
        &self,
        url: &str,
        engine: &mut ScriptEngine,
        report: &mut ScriptReport,
        rounds: &mut usize,
        dynamic: &mut usize,
    ) {
        let base = url::Url::parse(url).ok();
        while *rounds < MAX_DYNAMIC_ROUNDS && *dynamic < MAX_DYNAMIC_SCRIPTS {
            let specs = match engine.collect_dynamic_scripts() {
                Ok(s) if s.is_empty() => return,
                Ok(s) => s,
                Err(e) => {
                    report.errors.push(format!("dynamic scripts: {e}"));
                    return;
                }
            };
            *rounds += 1;
            for spec in specs {
                if *dynamic >= MAX_DYNAMIC_SCRIPTS {
                    report.console.push(format!(
                        "[js] dynamic script limit reached ({MAX_DYNAMIC_SCRIPTS})"
                    ));
                    return;
                }
                *dynamic += 1;
                let label = format!("dynamic script #{}", *dynamic);
                match spec.code {
                    Some(code) => match engine.run(&code) {
                        Ok(()) => report.executed += 1,
                        Err(e) => report.errors.push(format!("{label}: {e}")),
                    },
                    None => {
                        let src = spec.src.unwrap_or_default();
                        let resolved = base
                            .as_ref()
                            .and_then(|b| b.join(&src).ok())
                            .map(|u| u.to_string());
                        match resolved {
                            Some(resolved) => match self.network.fetch(&resolved).await {
                                Ok(code) => match engine.run(&code) {
                                    Ok(()) => report.executed += 1,
                                    Err(e) => report.errors.push(format!("{label}: {e}")),
                                },
                                Err(e) => report
                                    .errors
                                    .push(format!("{label} [src {src}]: fetch failed: {e}")),
                            },
                            None => report
                                .errors
                                .push(format!("{label}: cannot resolve src {src}")),
                        }
                    }
                }
            }
        }
    }

    /// Evaluate `source` against a post-load page: the stored HTML is
    /// parsed into a fresh context (the page's own scripts are not
    /// re-run — the HTML already contains their output), the page is marked
    /// loaded (`readyState === "complete"`), the snippet runs, and its value
    /// plus the console lines are returned.
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
        engine.fire_load().map_err(BrowserError::ScriptError)?;
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

    /* ---- helpers ------------------------------------------------------- */

    /// Minimal HTTP test server: serves canned bodies by exact path (built
    /// from the bound base URL), one request per connection. Returns base.
    fn spawn_server(
        routes_for: impl FnOnce(&str) -> Vec<(String, String)> + Send + 'static,
    ) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let routes = routes_for(&base);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut req = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            req.extend_from_slice(&chunk[..n]);
                            if req.windows(4).any(|w| w == b"\r\n\r\n") || req.len() > 65_536 {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                let text = String::from_utf8_lossy(&req);
                let path = text
                    .lines()
                    .next()
                    .and_then(|line| line.split(' ').nth(1))
                    .unwrap_or("/");
                let body = routes
                    .iter()
                    .find(|(p, _)| p == path)
                    .map(|(_, b)| b.clone())
                    .unwrap_or_else(|| "<html><body>not found</body></html>".to_string());
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        });
        // Keep the engine's client from routing loopback through a proxy.
        std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
        std::env::set_var("no_proxy", "127.0.0.1,localhost");
        base
    }

    /// Engine with headless fallback off (short text) and a short timeout.
    fn test_engine(timeout_secs: u64) -> BrowserEngine {
        BrowserEngine::new(BrowserConfig {
            headless_fallback: false,
            timeout_secs,
            ..Default::default()
        })
    }

    /* ---- lifecycle events ---------------------------------------------- */

    #[tokio::test]
    async fn dom_content_loaded_and_load_handlers_run() {
        let html = r#"
            <html><head><title>L</title></head>
            <body><div id="out"></div>
            <script>
              var d = document.getElementById("out");
              console.log("during scripts", document.readyState);
              document.addEventListener("DOMContentLoaded", function (e) {
                d.textContent += "dcl";
                console.log("at dcl", document.readyState, e.type, e.target === document);
              });
              window.addEventListener("load", function (e) {
                d.textContent += "|load";
                console.log("at load", document.readyState, e.type);
              });
              window.onload = function () { d.textContent += "|onload"; };
            </script>
            </body></html>"#;
        let engine = test_engine(10);
        let page = engine
            .build_page("https://example.com/", html.to_string())
            .await
            .unwrap();
        assert!(
            page.text_content.contains("dcl|load|onload"),
            "text: {}",
            page.text_content
        );
        assert!(
            page.console.contains(&"during scripts loading".to_string()),
            "console: {:?}",
            page.console
        );
        assert!(
            page.console
                .contains(&"at dcl interactive DOMContentLoaded true".to_string()),
            "console: {:?}",
            page.console
        );
        assert!(
            page.console.contains(&"at load complete load".to_string()),
            "console: {:?}",
            page.console
        );
        assert!(
            page.script_errors.is_empty(),
            "errors: {:?}",
            page.script_errors
        );
    }

    #[tokio::test]
    async fn load_handler_error_is_captured_not_fatal() {
        let html = r#"
            <html><body><p>visible text</p>
            <script>window.addEventListener("load", function () { throw new Error("boom"); });</script>
            </body></html>"#;
        let engine = test_engine(10);
        let page = engine
            .build_page("https://example.com/", html.to_string())
            .await
            .unwrap();
        assert!(page.text_content.contains("visible text"));
        assert!(
            page.console
                .iter()
                .any(|l| l.contains("load handler error") && l.contains("boom")),
            "console: {:?}",
            page.console
        );
        assert!(
            page.script_errors.is_empty(),
            "errors: {:?}",
            page.script_errors
        );
    }

    #[tokio::test]
    async fn readystatechange_fires_on_document() {
        let html = r#"
            <html><body>
            <script>
              document.onreadystatechange = function () {
                console.log("ready", document.readyState);
              };
            </script>
            </body></html>"#;
        let engine = test_engine(10);
        let page = engine
            .build_page("https://example.com/", html.to_string())
            .await
            .unwrap();
        assert!(
            page.console.contains(&"ready interactive".to_string())
                && page.console.contains(&"ready complete".to_string()),
            "console: {:?}",
            page.console
        );
    }

    /* ---- dynamic <script> ---------------------------------------------- */

    #[tokio::test]
    async fn dynamic_inline_script_runs() {
        let html = r#"
            <html><head><title>D</title></head>
            <body><div id="out">-</div>
            <script>
              var s = document.createElement("script");
              s.textContent = 'document.getElementById("out").textContent = "dynamic ran";';
              document.body.appendChild(s);
            </script>
            </body></html>"#;
        let engine = test_engine(10);
        let page = engine
            .build_page("https://example.com/", html.to_string())
            .await
            .unwrap();
        assert!(
            page.text_content.contains("dynamic ran"),
            "text: {}",
            page.text_content
        );
        assert_eq!(page.scripts_executed, 2);
        assert!(
            page.script_errors.is_empty(),
            "errors: {:?}",
            page.script_errors
        );
    }

    #[tokio::test]
    async fn dynamic_script_chain_runs_in_order() {
        let html = r#"
            <html><body><div id="out"></div>
            <script>
              var s = document.createElement("script");
              s.textContent = 'document.getElementById("out").textContent += "B";'
                + 'var t = document.createElement("script");'
                + 't.textContent = \'document.getElementById("out").textContent += "C";\';'
                + 'document.body.appendChild(t);';
              document.body.appendChild(s);
            </script>
            </body></html>"#;
        let engine = test_engine(10);
        let page = engine
            .build_page("https://example.com/", html.to_string())
            .await
            .unwrap();
        assert!(
            page.text_content.contains("BC"),
            "text: {}",
            page.text_content
        );
        assert_eq!(page.scripts_executed, 3);
        assert!(
            page.script_errors.is_empty(),
            "errors: {:?}",
            page.script_errors
        );
    }

    #[tokio::test]
    async fn dynamic_round_limit_terminates_self_replicating_script() {
        let html = r#"
            <html><body><div id="out">-</div>
            <script>
              globalThis.REPL = 'window.__c = (window.__c || 0) + 1;'
                + ' document.getElementById("out").textContent = "cycles " + window.__c;'
                + ' var s = document.createElement("script");'
                + ' s.textContent = globalThis.REPL;'
                + ' document.body.appendChild(s);';
              var first = document.createElement("script");
              first.textContent = globalThis.REPL;
              document.body.appendChild(first);
            </script>
            </body></html>"#;
        let engine = test_engine(10);
        let page = engine
            .build_page("https://example.com/", html.to_string())
            .await
            .unwrap();
        assert!(
            page.text_content.contains("cycles 8"),
            "text: {}",
            page.text_content
        );
        assert_eq!(page.scripts_executed, 9);
        assert!(
            page.console
                .iter()
                .any(|l| l.contains("dynamic script round limit reached (8)")),
            "console: {:?}",
            page.console
        );
        assert!(
            page.script_errors.is_empty(),
            "errors: {:?}",
            page.script_errors
        );
    }

    #[tokio::test]
    async fn dynamic_external_script_is_fetched() {
        let base = spawn_server(|_| {
            vec![(
                "/app.js".to_string(),
                r#"document.getElementById("out").textContent = "from external";"#.to_string(),
            )]
        });
        let html = r#"
            <html><body><div id="out">-</div>
            <script>
              var s = document.createElement("script");
              s.src = "/app.js";
              document.body.appendChild(s);
            </script>
            </body></html>"#;
        let engine = test_engine(10);
        let page = engine
            .build_page(&format!("{base}/"), html.to_string())
            .await
            .unwrap();
        assert!(
            page.text_content.contains("from external"),
            "text: {}",
            page.text_content
        );
        assert_eq!(page.scripts_executed, 2);
        assert!(
            page.script_errors.is_empty(),
            "errors: {:?}",
            page.script_errors
        );
    }

    #[tokio::test]
    async fn dynamic_external_fetch_failure_is_reported() {
        let html = r#"
            <html><body><p>content stays</p>
            <script>
              var s = document.createElement("script");
              s.src = "http://invalid.nonexistent.domain/app.js";
              document.body.appendChild(s);
            </script>
            </body></html>"#;
        let engine = test_engine(5);
        let page = engine
            .build_page("https://example.com/", html.to_string())
            .await
            .unwrap();
        assert!(page.text_content.contains("content stays"));
        assert_eq!(page.scripts_executed, 1);
        assert_eq!(page.script_errors.len(), 1);
        assert!(
            page.script_errors[0].contains("dynamic script #1"),
            "errors: {:?}",
            page.script_errors
        );
        assert!(
            page.script_errors[0].contains("fetch failed"),
            "errors: {:?}",
            page.script_errors
        );
    }

    /* ---- navigation: location.href + meta refresh ---------------------- */

    #[tokio::test]
    async fn js_navigation_is_followed() {
        let base = spawn_server(|_| {
            vec![
                (
                    "/".to_string(),
                    r#"<html><head><title>First</title></head><body><p>first page</p><script>location.href = "/next";</script></body></html>"#
                        .to_string(),
                ),
                (
                    "/next".to_string(),
                    r#"<html><head><title>Second</title></head><body><p>second page</p></body></html>"#
                        .to_string(),
                ),
            ]
        });
        let engine = test_engine(10);
        let page = engine.navigate(&format!("{base}/")).await.unwrap();
        assert_eq!(page.url, format!("{base}/next"));
        assert_eq!(page.title, "Second");
        assert!(page.text_content.contains("second page"));
        assert!(
            page.console
                .iter()
                .any(|l| l == &format!("[nav] -> {base}/next")),
            "console: {:?}",
            page.console
        );
        assert_eq!(page.scripts_executed, 1);
    }

    #[tokio::test]
    async fn meta_refresh_is_followed_without_scripts() {
        let base = spawn_server(|_| {
            vec![
                (
                    "/".to_string(),
                    r#"<html><head><title>First</title><meta http-equiv="refresh" content="0; url=/second"></head><body><p>first page</p></body></html>"#
                        .to_string(),
                ),
                (
                    "/second".to_string(),
                    r#"<html><head><title>Second</title></head><body><p>second page</p></body></html>"#
                        .to_string(),
                ),
            ]
        });
        let engine = test_engine(10);
        let page = engine.navigate(&format!("{base}/")).await.unwrap();
        assert_eq!(page.url, format!("{base}/second"));
        assert_eq!(page.title, "Second");
        assert!(page.text_content.contains("second page"));
        assert_eq!(page.scripts_executed, 0);
    }

    #[tokio::test]
    async fn meta_refresh_delay_is_ignored() {
        let base = spawn_server(|_| {
            vec![
                (
                    "/".to_string(),
                    r#"<html><head><title>Here</title><meta http-equiv="refresh" content="5; url=/later"></head><body><p>staying put</p></body></html>"#
                        .to_string(),
                ),
                (
                    "/later".to_string(),
                    r#"<html><body><p>should not load</p></body></html>"#.to_string(),
                ),
            ]
        });
        let engine = test_engine(10);
        let page = engine.navigate(&format!("{base}/")).await.unwrap();
        assert_eq!(page.url, format!("{base}/"));
        assert!(page.text_content.contains("staying put"));
        assert!(
            page.console
                .iter()
                .any(|l| l.contains("meta refresh in 5s ignored")),
            "console: {:?}",
            page.console
        );
    }

    #[tokio::test]
    async fn navigation_hop_limit_stops_chain() {
        let base = spawn_server(|_| {
            let mut routes = Vec::new();
            for i in 0..7 {
                routes.push((
                    format!("/h{i}"),
                    format!(
                        r#"<html><head><title>H{i}</title><meta http-equiv="refresh" content="0; url=/h{}"></head><body><p>hop {i}</p></body></html>"#,
                        i + 1
                    ),
                ));
            }
            routes
        });
        let engine = test_engine(10);
        let page = engine.navigate(&format!("{base}/h0")).await.unwrap();
        assert_eq!(page.url, format!("{base}/h5"));
        assert_eq!(page.title, "H5");
        assert!(page.text_content.contains("hop 5"));
        assert!(
            page.console
                .iter()
                .any(|l| l.contains("hop limit 5 reached")),
            "console: {:?}",
            page.console
        );
    }

    #[tokio::test]
    async fn same_url_navigation_loop_stops() {
        let base = spawn_server(|_| {
            vec![(
                "/".to_string(),
                r#"<html><head><title>Self</title><meta http-equiv="refresh" content="0; url=/"></head><body><p>self loop</p></body></html>"#
                    .to_string(),
            )]
        });
        let engine = test_engine(10);
        let page = engine.navigate(&format!("{base}/")).await.unwrap();
        assert_eq!(page.url, format!("{base}/"));
        assert!(page.text_content.contains("self loop"));
        assert!(
            page.console.iter().any(|l| l.contains("already on")),
            "console: {:?}",
            page.console
        );
    }

    #[tokio::test]
    async fn nav_fetch_failure_keeps_current_page() {
        let html = r#"
            <html><body><p>current content</p>
            <script>location.href = "http://invalid.nonexistent.domain/next";</script>
            </body></html>"#;
        let engine = test_engine(5);
        let page = engine
            .build_page("https://example.com/", html.to_string())
            .await
            .unwrap();
        assert_eq!(page.url, "https://example.com/");
        assert!(page.text_content.contains("current content"));
        assert!(
            page.script_errors.iter().any(|e| e.contains("[nav]")),
            "errors: {:?}",
            page.script_errors
        );
    }

    #[tokio::test]
    async fn non_http_nav_is_refused() {
        let html = r#"
            <html><body><p>stay here</p>
            <script>location.href = "mailto:someone@example.com";</script>
            </body></html>"#;
        let engine = test_engine(10);
        let page = engine
            .build_page("https://example.com/", html.to_string())
            .await
            .unwrap();
        assert_eq!(page.url, "https://example.com/");
        assert!(page.text_content.contains("stay here"));
        assert!(
            page.console
                .iter()
                .any(|l| l.contains("refused non-http target")),
            "console: {:?}",
            page.console
        );
        assert!(
            page.script_errors.is_empty(),
            "errors: {:?}",
            page.script_errors
        );
    }
}
