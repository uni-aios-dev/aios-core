//! Embedded JavaScript execution for loaded pages (boa engine).
//!
//! The engine bridges a parsed [`DomNode`] tree and plain JavaScript: the
//! tree is injected as the `__dom` global, a prelude ([`script_prelude.js`])
//! hydrates every node with a DOM API subset (`textContent`, `innerHTML`,
//! `querySelector`, `appendChild`, ...), and a `console`/`location`/`timer`
//! shim records observable side effects. After the scripts run the mutated
//! tree is read back, so script edits show up in the rendered page.
//!
//! Design constraints:
//! - No host functions: all traffic between Rust and JS is JSON injected
//!   before evaluation and `JSON.stringify` read-back after it, so scripts
//!   can never crash the host through a bad FFI call.
//! - A fresh context per page keeps evaluations deterministic and lets the
//!   stateless IPC block replay any stored page for `eval_js`.

use crate::types::DomNode;
use boa_engine::{Context, JsString, JsValue, Source};
use serde::{Deserialize, Serialize};

/// The DOM/window/console shim, evaluated once per fresh context.
const PRELUDE: &str = include_str!("script_prelude.js");

/// Upper bound of retained console lines per page (older lines are dropped).
const MAX_CONSOLE_LINES: usize = 200;
/// Cap for callbacks executed in a single timer flush.
const MAX_TIMER_FLUSH: usize = 1000;

/// A `<script>` element created by page code after the initial parse,
/// collected by [`ScriptEngine::collect_dynamic_scripts`].
#[derive(Debug, Clone, Deserialize)]
pub struct DynamicScript {
    /// The element's raw `src` attribute (exactly one of `src`/`code` is set).
    #[serde(default)]
    pub src: Option<String>,
    /// The element's inline source text (exactly one of `src`/`code` is set).
    #[serde(default)]
    pub code: Option<String>,
}

/// Collected outcome of executing a page's scripts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScriptReport {
    /// `console.*` lines emitted by page and user scripts (already capped).
    pub console: Vec<String>,
    /// Scripts that completed without throwing.
    pub executed: usize,
    /// Per-script failure notes (`"script 3: ReferenceError: ..."`).
    pub errors: Vec<String>,
}

/// A JavaScript value returned by [`ScriptEngine::evaluate`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ScriptValue {
    /// `null` or `undefined`.
    Null,
    /// Any boolean.
    Bool(bool),
    /// Any finite number (non-finite values degrade to `String`).
    Number(f64),
    /// Any string (also the fallback display form for objects).
    String(String),
    /// A JavaScript array.
    Array(Vec<ScriptValue>),
    /// A plain object: ordered `(key, value)` pairs.
    Object(Vec<(String, ScriptValue)>),
}

impl std::fmt::Display for ScriptValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScriptValue::Null => write!(f, "null"),
            ScriptValue::Bool(b) => write!(f, "{b}"),
            ScriptValue::Number(n) => write!(f, "{n}"),
            ScriptValue::String(s) => write!(f, "{s}"),
            ScriptValue::Array(items) => {
                let parts: Vec<String> = items.iter().map(|v| v.to_string()).collect();
                write!(f, "[{}]", parts.join(", "))
            }
            ScriptValue::Object(fields) => {
                let parts: Vec<String> = fields.iter().map(|(k, v)| format!("{k}: {v}")).collect();
                write!(f, "{{{}}}", parts.join(", "))
            }
        }
    }
}

/// Read-back state of a finished context: mutated tree, final title/URL and
/// the collected console lines.
#[derive(Debug)]
pub struct ScriptOutcome {
    /// The page tree after all scripts ran.
    pub dom: DomNode,
    /// Final `document.title` (scripts may have changed it).
    pub title: String,
    /// Final URL (`location.href` reads it; `location.x = ...` requests are
    /// surfaced through `nav` instead of being followed automatically).
    pub url: String,
    /// Navigation requested by a script (`location.href = ...`). Returned to
    /// the engine, which follows it through [`crate::BrowserEngine`]'s
    /// hop-capped navigation loop instead of loading it here.
    pub nav: Option<String>,
    /// Capped `console.*` lines.
    pub console: Vec<String>,
}

/// Partial view of `__meta` read back after execution (only the JSON-safe
/// scalar fields — listeners hold functions and are never read back).
#[derive(Debug, Deserialize)]
struct MetaRead {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    nav: Option<String>,
}

/// A fresh JavaScript context bound to one page DOM.
pub struct ScriptEngine {
    context: Context,
}

/// SAFETY: a `ScriptEngine` is confined to one logical task. The boa context
/// is only touched while that task is being polled and two polls of a task
/// never overlap, so moving the handle between threads across `await` points
/// never exposes boa's non-atomic `Rc`s to concurrent use — the standard
/// assumption behind embedding single-threaded script engines in async hosts
/// (needed because the engine spans `NetworkClient` fetches in the page
/// pipeline and `BrowserBlock::block_on` requires `Send` futures).
unsafe impl Send for ScriptEngine {}

impl ScriptEngine {
    /// Create a context for `dom` with the prelude loaded and the page state
    /// (`url`, `title`, `ua`) injected into `__meta`.
    pub fn new(dom: &DomNode, url: &str, title: &str, ua: &str) -> Result<Self, String> {
        let mut context = Context::default();
        let dom_json = serde_json::to_string(dom).map_err(|e| format!("dom json: {e}"))?;
        let meta = serde_json::json!({
            "url": url,
            "title": title,
            "ua": ua,
            "nav": null,
            "cookie": "",
            "local_storage": {},
            "session_storage": {},
        });
        let meta_json = serde_json::to_string(&meta).map_err(|e| format!("meta json: {e}"))?;
        let boot = format!("globalThis.__dom = {dom_json}; globalThis.__meta = {meta_json}; globalThis.__console = [];");
        eval_raw(&mut context, &boot)?;
        eval_raw(&mut context, PRELUDE)?;
        // The parser-inserted <script> batch is executed by the engine itself;
        // mark it so later collection only returns page-created elements.
        eval_raw(&mut context, "globalThis.__aiosMarkScripts();")?;
        Ok(Self { context })
    }

    /// Evaluate one script in the page context, draining the microtask queue.
    /// A failure is reported to the caller but leaves the context usable for
    /// the remaining scripts (mirroring a browser's per-script isolation).
    pub fn run(&mut self, source: &str) -> Result<(), String> {
        eval_raw(&mut self.context, source)
    }

    /// Execute queued `setTimeout`/`setInterval` callbacks once, in `ms` order,
    /// with a hard cap so a self-rescheduling interval cannot hang the engine.
    pub fn flush_timers(&mut self) {
        let _ = eval_raw(
            &mut self.context,
            &format!("globalThis.__aiosFlushTimers({MAX_TIMER_FLUSH});"),
        );
    }

    /// Fire the synthetic `DOMContentLoaded` event: `document` listeners and
    /// `on*` property handlers run, `document.readyState` moves to
    /// `"interactive"` (a `readystatechange` event fires first).
    pub fn fire_dom_content_loaded(&mut self) -> Result<(), String> {
        eval_raw(
            &mut self.context,
            r#"globalThis.__aiosFire("DOMContentLoaded");"#,
        )
    }

    /// Fire the synthetic `load` event: `document.readyState` becomes
    /// `"complete"` (`readystatechange`), then `window` and `document` load
    /// listeners plus `onload` properties run.
    pub fn fire_load(&mut self) -> Result<(), String> {
        eval_raw(&mut self.context, r#"globalThis.__aiosFire("load");"#)
    }

    /// Collect `<script>` elements that page code appended to the DOM after
    /// the initial parse. Each element is marked on collection, so it is
    /// returned exactly once; `src` is left unresolved (the engine resolves
    /// it against the page URL and fetches it).
    pub fn collect_dynamic_scripts(&mut self) -> Result<Vec<DynamicScript>, String> {
        let json = eval_json(&mut self.context, "globalThis.__aiosCollectScripts()")?;
        serde_json::from_str(&json).map_err(|e| format!("dynamic scripts: {e}"))
    }

    /// Evaluate a snippet and convert the result into a [`ScriptValue`].
    pub fn evaluate(&mut self, source: &str) -> Result<ScriptValue, String> {
        let value = self
            .context
            .eval(Source::from_bytes(source.as_bytes()))
            .map_err(|e| e.to_string())?;
        Ok(convert_value(&value, &mut self.context))
    }

    /// Consume the context: read back the mutated DOM, final metadata and the
    /// console log.
    pub fn finish(self) -> Result<ScriptOutcome, String> {
        let mut context = self.context;
        let dom_json = eval_json(&mut context, "__dom")?;
        let meta_json = eval_json(
            &mut context,
            "{title: __meta.title, url: __meta.url, nav: __meta.nav || null}",
        )?;
        let console_json = eval_json(&mut context, "__console")?;

        let dom: DomNode = serde_json::from_str(&dom_json).map_err(|e| format!("dom: {e}"))?;
        let meta: MetaRead = serde_json::from_str(&meta_json).map_err(|e| format!("meta: {e}"))?;
        let raw_console: Vec<serde_json::Value> =
            serde_json::from_str(&console_json).map_err(|e| format!("console: {e}"))?;
        let mut console: Vec<String> = raw_console
            .into_iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        if console.len() > MAX_CONSOLE_LINES {
            let drop = console.len() - MAX_CONSOLE_LINES;
            console.drain(0..drop);
        }

        Ok(ScriptOutcome {
            dom,
            title: meta.title.unwrap_or_default(),
            url: meta.url.unwrap_or_default(),
            nav: meta.nav,
            console,
        })
    }
}

/// Does a user-script glob (`*` wildcards, plain substring otherwise) match
/// `url`?
///
/// `"*"` matches everything, `"example.com"` matches any URL containing that
/// text, `"https://example.com/*"` matches the site's pages only.
pub fn url_matches(pattern: &str, url: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return url.contains(pattern);
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut idx = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == parts.len() - 1 {
            let p = *part;
            return match url.len().checked_sub(p.len()) {
                Some(end) if end >= idx => &url.as_bytes()[end..] == p.as_bytes(),
                _ => false,
            };
        }
        let rest = match url.get(idx..) {
            Some(r) => r,
            None => return false,
        };
        match rest.find(part) {
            Some(off) => idx += off + part.len(),
            None => return false,
        }
    }
    true
}

/// Evaluate raw source, draining microtasks when the engine exposes a job
/// queue. Errors carry the JS message plus stack when available.
fn eval_raw(context: &mut Context, source: &str) -> Result<(), String> {
    match context.eval(Source::from_bytes(source.as_bytes())) {
        Ok(_) => {
            drain_jobs(context);
            Ok(())
        }
        Err(e) => {
            drain_jobs(context);
            Err(format_js_error(&e, context))
        }
    }
}

/// Render a boa error with its message and (when available) stack.
fn format_js_error(e: &boa_engine::JsError, context: &mut Context) -> String {
    if let Ok(native) = e.try_native(context) {
        let message = native.message();
        if !message.is_empty() {
            return message.to_string();
        }
    }
    e.to_string()
}

/// Best-effort microtask drain: `Promise.then` callbacks queued by a script
/// run before the next script starts.
fn drain_jobs(context: &mut Context) {
    #[allow(unused_must_use)]
    context.run_jobs();
}

/// Evaluate `expr` wrapped in `JSON.stringify` and return the raw JSON text.
fn eval_json(context: &mut Context, expr: &str) -> Result<String, String> {
    let js = format!("JSON.stringify({expr})");
    let value = context
        .eval(Source::from_bytes(js.as_bytes()))
        .map_err(|e| e.to_string())?;
    value_to_string(&value, context)
}

/// Convert a JS string primitive to a Rust `String`.
fn value_to_string(value: &JsValue, context: &mut Context) -> Result<String, String> {
    if let Some(s) = value.as_string() {
        return s.to_std_string().map_err(|e| format!("js string: {e}"));
    }
    match value.to_string(context) {
        Ok(s) => s.to_std_string().map_err(|e| format!("js string: {e}")),
        Err(e) => Err(e.to_string()),
    }
}

/// Recursively convert a JS value into a [`ScriptValue`].
fn convert_value(value: &JsValue, context: &mut Context) -> ScriptValue {
    if value.is_null() || value.is_undefined() {
        return ScriptValue::Null;
    }
    if let Some(b) = value.as_boolean() {
        return ScriptValue::Bool(b);
    }
    if let Some(n) = value.as_number() {
        return if n.is_finite() {
            ScriptValue::Number(n)
        } else {
            ScriptValue::String(n.to_string())
        };
    }
    if value.as_string().is_some() {
        return match value_to_string(value, context) {
            Ok(s) => ScriptValue::String(s),
            Err(_) => ScriptValue::Null,
        };
    }
    if let Some(obj) = value.as_object() {
        // Array detection: a `length` property that is a number.
        if let Ok(len_v) = obj.get(JsString::from("length"), context) {
            if let Some(len) = len_v.as_number() {
                let mut items = Vec::new();
                for i in 0..(len as usize) {
                    match obj.get(i, context) {
                        Ok(el) => items.push(convert_value(&el, context)),
                        Err(_) => items.push(ScriptValue::Null),
                    }
                }
                return ScriptValue::Array(items);
            }
        }
        if let Ok(keys) = obj.own_property_keys(context) {
            let mut fields = Vec::new();
            for key in keys {
                let label = key.to_string();
                match obj.get(key, context) {
                    Ok(v) => fields.push((label, convert_value(&v, context))),
                    Err(_) => fields.push((label, ScriptValue::Null)),
                }
            }
            return ScriptValue::Object(fields);
        }
    }
    ScriptValue::String(value.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html_parser::HtmlParser;

    fn engine_for(html: &str) -> ScriptEngine {
        let dom = HtmlParser::parse(html, "https://example.com/");
        ScriptEngine::new(&dom, "https://example.com/", "Test", "AIOS-Test/0.1").unwrap()
    }

    #[test]
    fn evaluates_primitives() {
        let mut e = engine_for("<html><body></body></html>");
        assert_eq!(e.evaluate("1 + 1").unwrap(), ScriptValue::Number(2.0));
        assert_eq!(
            e.evaluate("'a' + 'b'").unwrap(),
            ScriptValue::String("ab".into())
        );
        assert_eq!(e.evaluate("true").unwrap(), ScriptValue::Bool(true));
        assert_eq!(e.evaluate("null").unwrap(), ScriptValue::Null);
        assert_eq!(e.evaluate("undefined").unwrap(), ScriptValue::Null);
        assert_eq!(
            e.evaluate("[1, 2]").unwrap(),
            ScriptValue::Array(vec![ScriptValue::Number(1.0), ScriptValue::Number(2.0),])
        );
    }

    #[test]
    fn text_content_mutation_reaches_dom() {
        let mut e = engine_for(r#"<html><body><div id="app">Loading…</div></body></html>"#);
        e.run(r#"document.getElementById("app").textContent = "Hello from JS";"#)
            .unwrap();
        let out = e.finish().unwrap();
        let html = crate::serialize::dom_to_html(&out.dom);
        assert!(html.contains("Hello from JS"), "html was: {html}");
        assert!(!html.contains("Loading"), "html was: {html}");
    }

    #[test]
    fn query_selector_and_inner_html() {
        let mut e = engine_for(r#"<html><body><p class="msg">old</p></body></html>"#);
        e.run(r#"document.querySelector(".msg").innerHTML = "<b>new</b>";"#)
            .unwrap();
        let out = e.finish().unwrap();
        let html = crate::serialize::dom_to_html(&out.dom);
        assert!(html.contains("<b>new</b>"), "html was: {html}");
    }

    #[test]
    fn console_lines_are_captured() {
        let mut e = engine_for("<html><body></body></html>");
        e.run(r#"console.log("hello", 42); console.error("bad");"#)
            .unwrap();
        let out = e.finish().unwrap();
        assert_eq!(out.console.len(), 2);
        assert_eq!(out.console[0], "hello 42");
        assert_eq!(out.console[1], "[error] bad");
    }

    #[test]
    fn title_mutation_reaches_meta() {
        let mut e = engine_for("<html><head><title>Old</title></head><body></body></html>");
        e.run(r#"document.title = "Renamed by script";"#).unwrap();
        let out = e.finish().unwrap();
        assert_eq!(out.title, "Renamed by script");
    }

    #[test]
    fn script_error_is_reported_and_next_script_runs() {
        let mut e = engine_for("<html><body></body></html>");
        let first = e.run("notDefinedAnywhere.push(1)");
        assert!(first.is_err(), "undefined variable must surface");
        e.run(
            r##"globalThis.__dom.children.push({
                tag: "p", attrs: [], text: "",
                children: [{ tag: "#text", attrs: [], text: "after", children: [] }],
            });"##,
        )
        .unwrap();
        let out = e.finish().unwrap();
        let html = crate::serialize::dom_to_html(&out.dom);
        assert!(html.contains("after"), "html was: {html}");
    }

    #[test]
    fn timers_flush_once_after_load() {
        let mut e = engine_for(r#"<html><body><div id="d"></div></body></html>"#);
        e.run(r#"setTimeout(() => { document.getElementById("d").textContent = "timer fired"; }, 0);"#)
            .unwrap();
        e.flush_timers();
        let out = e.finish().unwrap();
        let html = crate::serialize::dom_to_html(&out.dom);
        assert!(html.contains("timer fired"), "html was: {html}");
    }

    #[test]
    fn append_child_and_attributes() {
        let mut e = engine_for("<html><body><ul id='list'></ul></body></html>");
        e.run(
            r#"
            const li = document.createElement("li");
            li.textContent = "item 1";
            li.setAttribute("data-n", "1");
            document.getElementById("list").appendChild(li);
            "#,
        )
        .unwrap();
        let out = e.finish().unwrap();
        let html = crate::serialize::dom_to_html(&out.dom);
        assert!(html.contains(r#"data-n="1""#), "html was: {html}");
        assert!(html.contains("item 1"), "html was: {html}");
    }

    #[test]
    fn location_href_request_is_surfaced_not_followed() {
        let mut e = engine_for("<html><body></body></html>");
        e.run(r#"location.href = "https://example.org/next";"#)
            .unwrap();
        let out = e.finish().unwrap();
        assert_eq!(out.nav.as_deref(), Some("https://example.org/next"));
    }

    #[test]
    fn url_matches_glob_and_substring() {
        assert!(url_matches("*", "https://anywhere.test/x"));
        assert!(url_matches("example.com", "https://example.com/page"));
        assert!(!url_matches("example.com", "https://other.test/"));
        assert!(url_matches(
            "https://example.com/*",
            "https://example.com/a/b"
        ));
        assert!(!url_matches("https://example.com/*", "https://other.com/a"));
        assert!(url_matches("*.js", "https://cdn.test/app.min.js"));
        assert!(!url_matches("*.js", "https://cdn.test/app.css"));
    }

    #[test]
    fn evaluate_reports_script_errors() {
        let mut e = engine_for("<html><body></body></html>");
        let err = e.evaluate("throw new Error('boom')").unwrap_err();
        assert!(err.contains("boom"), "error was: {err}");
    }

    #[test]
    fn lifecycle_events_reach_handlers() {
        let mut e = engine_for("<html><head><title>T</title></head><body></body></html>");
        e.run(
            r#"
            document.addEventListener("DOMContentLoaded", () => { document.title = "dcl seen"; });
            window.addEventListener("load", () => { document.title = document.title + " + load"; });
            "#,
        )
        .unwrap();
        e.fire_dom_content_loaded().unwrap();
        e.fire_load().unwrap();
        let out = e.finish().unwrap();
        assert_eq!(out.title, "dcl seen + load");
    }

    #[test]
    fn ready_state_walks_loading_interactive_complete() {
        let mut e = engine_for("<html><body></body></html>");
        assert_eq!(
            e.evaluate("document.readyState").unwrap(),
            ScriptValue::String("loading".into())
        );
        e.fire_dom_content_loaded().unwrap();
        assert_eq!(
            e.evaluate("document.readyState").unwrap(),
            ScriptValue::String("interactive".into())
        );
        e.fire_load().unwrap();
        assert_eq!(
            e.evaluate("document.readyState").unwrap(),
            ScriptValue::String("complete".into())
        );
    }

    #[test]
    fn manual_event_dispatch_runs_node_listener() {
        let mut e = engine_for(r#"<html><body><div id="d"></div></body></html>"#);
        e.run(
            r#"
            const d = document.getElementById("d");
            d.addEventListener("ping", (ev) => { d.textContent = "got " + ev.detail; });
            d.dispatchEvent(new CustomEvent("ping", { detail: 7 }));
            "#,
        )
        .unwrap();
        let out = e.finish().unwrap();
        let html = crate::serialize::dom_to_html(&out.dom);
        assert!(html.contains("got 7"), "html: {html}");
    }

    #[test]
    fn dynamic_scripts_collected_once_and_typed() {
        let mut e = engine_for(r#"<html><body><script>1</script></body></html>"#);
        // Parser-inserted scripts are pre-marked: nothing dynamic yet.
        assert!(e.collect_dynamic_scripts().unwrap().is_empty());
        e.run(
            r#"
            const s = document.createElement("script");
            s.textContent = "1 + 1";
            document.body.appendChild(s);
            const ext = document.createElement("script");
            ext.setAttribute("src", "/late.js");
            document.body.appendChild(ext);
            const mod = document.createElement("script");
            mod.setAttribute("type", "module");
            mod.textContent = "export {}";
            document.body.appendChild(mod);
            "#,
        )
        .unwrap();
        let specs = e.collect_dynamic_scripts().unwrap();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].code.as_deref(), Some("1 + 1"));
        assert_eq!(specs[0].src, None);
        assert_eq!(specs[1].src.as_deref(), Some("/late.js"));
        assert_eq!(specs[1].code, None);
        // Each element is returned exactly once.
        assert!(e.collect_dynamic_scripts().unwrap().is_empty());
        let out = e.finish().unwrap();
        assert!(
            out.console
                .iter()
                .any(|l| l.contains("skipped module script")),
            "console: {:?}",
            out.console
        );
    }
}
