//! AIOS Webview — native full-featured browser embedding (WebView2 / WebKitGTK / WKWebView).
//!
//! Runs a real browser engine in its own window with cookies, JavaScript and
//! history out of the box. The window is created on a dedicated background
//! thread so the caller (TUI or GUI) never blocks. Navigation commands are
//! sent over an event-loop proxy and applied on the browser's event loop.

pub mod launcher;

use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};
use wry::WebViewBuilder;

/// Commands sent from any thread to the browser's event loop.
#[derive(Debug)]
enum Command {
    /// Load a fully resolved URL.
    Navigate(String),
    /// Go back in history.
    Back,
    /// Go forward in history.
    Forward,
    /// Close the browser window and stop the event loop.
    Quit,
}

/// Messages sent from the browser thread back to the opener.
enum ThreadMsg {
    /// Window and webview created (or error description).
    Ready(Result<(), String>),
}

/// The window host driving the winit event loop.
struct BrowserApp {
    window: Option<Window>,
    webview: Option<wry::WebView>,
    url: String,
    tx: Option<mpsc::Sender<ThreadMsg>>,
}

impl BrowserApp {
    fn build_window(event_loop: &ActiveEventLoop) -> Result<Window, String> {
        event_loop
            .create_window(
                Window::default_attributes()
                    .with_title("AIOS Browser")
                    .with_inner_size(LogicalSize::new(1100.0, 750.0)),
            )
            .map_err(|e| e.to_string())
    }

    fn build_webview(window: &Window, url: &str) -> Result<wry::WebView, String> {
        let context = Box::leak(Box::new(wry::WebContext::new(profile_dir())));
        WebViewBuilder::new_with_web_context(context)
            .with_url(url)
            .build(window)
            .map_err(|e| e.to_string())
    }
}

impl ApplicationHandler<Command> for BrowserApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let result = (|| {
            let window = Self::build_window(event_loop)?;
            let webview = Self::build_webview(&window, &self.url)?;
            self.window = Some(window);
            self.webview = Some(webview);
            Ok(())
        })();
        let failed = result.is_err();
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(ThreadMsg::Ready(result));
        }
        if failed {
            event_loop.exit();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if let WindowEvent::CloseRequested = event {
            event_loop.exit();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Command) {
        match event {
            Command::Navigate(url) => {
                if let Some(webview) = self.webview.as_ref() {
                    if let Err(e) = webview.load_url(&url) {
                        log::error!("webview load_url failed: {e}");
                    }
                }
            }
            Command::Back => {
                if let Some(webview) = self.webview.as_ref() {
                    if let Err(e) = webview.go_back() {
                        log::error!("webview back failed: {e}");
                    }
                }
            }
            Command::Forward => {
                if let Some(webview) = self.webview.as_ref() {
                    if let Err(e) = webview.go_forward() {
                        log::error!("webview forward failed: {e}");
                    }
                }
            }
            Command::Quit => event_loop.exit(),
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
    }
}

/// Persistent browser profile directory so cookies and storage survive restarts.
///
/// Honors `AIOS_DATA_DIR` when set explicitly, otherwise falls back to the OS
/// data directory (`dirs::data_dir()/aios/webview`). Returns `None` when the
/// directory cannot be created — the engine then falls back to an in-memory
/// session profile.
fn profile_dir() -> Option<PathBuf> {
    let base = std::env::var_os("AIOS_DATA_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::data_dir().map(|d| d.join("aios")))?;
    let dir = base.join("webview");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Resolve a raw address-bar input into a loadable URL.
///
/// - Empty input → `about:blank`
/// - Full `http(s)://` URL → used as-is
/// - Host with a dot and no spaces → prefixed with `https://`
/// - Anything else → DuckDuckGo (HTML edition) search query
pub fn resolve_target(input: &str) -> String {
    let s = input.trim();
    if s.is_empty() {
        return String::from("about:blank");
    }
    if s.starts_with("http://") || s.starts_with("https://") {
        return s.to_string();
    }
    if s.contains('.') && !s.chars().any(char::is_whitespace) {
        return format!("https://{s}");
    }
    let q = url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
    format!("https://html.duckduckgo.com/html/?q={q}")
}

/// Handle to a live browser window running on a background thread.
///
/// All methods are non-blocking: commands are posted to the browser's event
/// loop and applied there asynchronously. Dropping the handle closes the
/// window.
pub struct WebBrowser {
    proxy: EventLoopProxy<Command>,
    _thread: thread::JoinHandle<()>,
}

impl WebBrowser {
    /// Open a new browser window and navigate it to `target`.
    ///
    /// Blocks only until the native window and webview are created (a few
    /// seconds at most), then returns immediately.
    pub fn open(target: &str) -> Result<WebBrowser, String> {
        let url = resolve_target(target);
        let (ready_tx, ready_rx) = mpsc::channel::<ThreadMsg>();
        let (proxy_tx, proxy_rx) = mpsc::channel::<EventLoopProxy<Command>>();
        let thread = thread::Builder::new()
            .name("aios-webview".into())
            .spawn(move || {
                let run = || -> Result<(), String> {
                    let event_loop = EventLoop::<Command>::with_user_event()
                        .build()
                        .map_err(|e| e.to_string())?;
                    let proxy = event_loop.create_proxy();
                    let _ = proxy_tx.send(proxy);
                    let mut app = BrowserApp {
                        window: None,
                        webview: None,
                        url,
                        tx: Some(ready_tx),
                    };
                    event_loop.run_app(&mut app).map_err(|e| e.to_string())
                };
                if let Err(e) = run() {
                    log::error!("webview thread failed: {e}");
                }
            })
            .map_err(|e| e.to_string())?;
        let proxy = proxy_rx
            .recv_timeout(Duration::from_secs(15))
            .map_err(|e| format!("webview event loop did not start: {e}"))?;
        match ready_rx
            .recv_timeout(Duration::from_secs(30))
            .map_err(|e| format!("webview did not become ready: {e}"))?
        {
            ThreadMsg::Ready(Ok(())) => Ok(WebBrowser {
                proxy,
                _thread: thread,
            }),
            ThreadMsg::Ready(Err(e)) => Err(format!("failed to create webview: {e}")),
        }
    }

    /// Navigate the browser to `target` (URL, host or search query).
    pub fn navigate(&self, target: &str) -> Result<(), String> {
        self.proxy
            .send_event(Command::Navigate(resolve_target(target)))
            .map_err(|e| e.to_string())
    }

    /// Go back one page in history.
    pub fn back(&self) -> Result<(), String> {
        self.proxy
            .send_event(Command::Back)
            .map_err(|e| e.to_string())
    }

    /// Go forward one page in history.
    pub fn forward(&self) -> Result<(), String> {
        self.proxy
            .send_event(Command::Forward)
            .map_err(|e| e.to_string())
    }

    /// Close the browser window and stop its event loop.
    pub fn close(&self) {
        let _ = self.proxy.send_event(Command::Quit);
    }
}

impl Drop for WebBrowser {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_empty_input() {
        assert_eq!(resolve_target(""), "about:blank");
        assert_eq!(resolve_target("   "), "about:blank");
    }

    #[test]
    fn resolve_full_url_passthrough() {
        assert_eq!(resolve_target("https://example.com"), "https://example.com");
        assert_eq!(resolve_target("http://a.b/c?d=1"), "http://a.b/c?d=1");
    }

    #[test]
    fn resolve_bare_host_gets_https() {
        assert_eq!(resolve_target("example.com"), "https://example.com");
        assert_eq!(
            resolve_target("  wiki.example.org  "),
            "https://wiki.example.org"
        );
    }

    #[test]
    fn resolve_query_goes_to_duckduckgo() {
        assert!(resolve_target("hello world").starts_with("https://html.duckduckgo.com/html/?q="));
        assert!(resolve_target("how to rust").contains("how+to+rust"));
        assert!(resolve_target("c++").contains("c%2B%2B"));
    }

    #[test]
    fn resolve_ip_address_like_host() {
        assert_eq!(resolve_target("192.168.1.1"), "https://192.168.1.1");
    }
}
