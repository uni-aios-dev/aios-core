//! AIOS Webview — native full-featured browser embedding (WebView2 / WebKitGTK / WKWebView).
//!
//! Runs a real browser engine in its own window with cookies, JavaScript and
//! history out of the box. The window lives on a single dedicated background
//! thread that is started on first use and reused afterwards (GTK may only be
//! initialized by one thread, and wry pumps it from the window's event loop),
//! so the caller (TUI or GUI) never blocks. Navigation commands are sent over
//! an event-loop proxy and applied on the browser's event loop.
//!
//! The full wry/winit engine is behind the optional `webview` feature. The
//! launcher (used by the TUI `W` key) is always available regardless of the
//! engine being compiled.

pub mod launcher;

/// Shared URL resolution used by both the launcher and the engine.
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

#[cfg(feature = "webview")]
mod engine {
    use std::path::PathBuf;
    #[cfg(target_os = "linux")]
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::sync::Mutex;
    use std::thread;
    use std::time::Duration;
    #[cfg(target_os = "linux")]
    use std::time::Instant;

    use winit::application::ApplicationHandler;
    use winit::dpi::LogicalSize;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
    use winit::window::{Window, WindowId};
    use wry::WebViewBuilder;

    /// Commands sent from any thread to the browser's event loop.
    #[derive(Debug)]
    enum Command {
        /// Show the window and load a fully resolved URL.
        Navigate(String),
        /// Go back in history.
        Back,
        /// Go forward in history.
        Forward,
        /// Hide the browser window, keeping the engine alive.
        Close,
    }

    /// Messages sent from the browser thread back to the opener.
    enum ThreadMsg {
        /// Window and webview created (or error description).
        Ready(Result<(), String>),
    }

    /// Proxy to the single engine thread; `None` until the first successful open.
    static BROWSER_PROXY: Mutex<Option<EventLoopProxy<Command>>> = Mutex::new(None);

    /// Set once GTK is bound to the engine thread; GTK allows initialization
    /// from exactly one thread for the whole process lifetime.
    #[cfg(target_os = "linux")]
    static GTK_TAKEN: AtomicBool = AtomicBool::new(false);

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

        fn present(&self) {
            if let Some(window) = self.window.as_ref() {
                window.set_visible(true);
            }
        }

        fn hide(&self) {
            if let Some(window) = self.window.as_ref() {
                window.set_visible(false);
            }
        }
    }

    impl ApplicationHandler<Command> for BrowserApp {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.window.is_some() {
                if let Some(tx) = self.tx.take() {
                    let _ = tx.send(ThreadMsg::Ready(Ok(())));
                }
                return;
            }
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

        fn window_event(
            &mut self,
            _event_loop: &ActiveEventLoop,
            _id: WindowId,
            event: WindowEvent,
        ) {
            if let WindowEvent::CloseRequested = event {
                self.hide();
            }
        }

        fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: Command) {
            match event {
                Command::Navigate(url) => {
                    self.present();
                    if let Some(webview) = self.webview.as_ref() {
                        if let Err(e) = webview.load_url(&url) {
                            log::error!("webview load_url failed: {e}");
                        }
                    }
                }
                Command::Back => {
                    self.present();
                    if let Some(webview) = self.webview.as_ref() {
                        if let Err(e) = webview.go_back() {
                            log::error!("webview back failed: {e}");
                        }
                    }
                }
                Command::Forward => {
                    self.present();
                    if let Some(webview) = self.webview.as_ref() {
                        if let Err(e) = webview.go_forward() {
                            log::error!("webview forward failed: {e}");
                        }
                    }
                }
                Command::Close => self.hide(),
            }
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            // winit sleeps until the next X event, but GTK/WebKit sources (their
            // own X connection, web-process IPC) only wake the glib context —
            // so drain it here and re-arm a short deadline to poll again.
            #[cfg(target_os = "linux")]
            {
                while gtk::events_pending() {
                    gtk::main_iteration_do(false);
                }
                event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                    Instant::now() + Duration::from_millis(16),
                ));
            }
            #[cfg(not(target_os = "linux"))]
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

    /// Handle to a live browser window running on a process-wide background thread.
    ///
    /// The window is hidden (not destroyed) when the handle is dropped, and the
    /// next [`WebBrowser::open`] call presents it again with the requested URL.
    /// All methods are non-blocking: commands are posted to the browser's event
    /// loop and applied there asynchronously.
    pub struct WebBrowser {
        proxy: EventLoopProxy<Command>,
        _thread: Option<thread::JoinHandle<()>>,
    }

    impl WebBrowser {
        /// Open the browser window on `target`.
        ///
        /// The first call starts the engine thread and blocks only until the
        /// native window and webview are created (a few seconds at most). Later
        /// calls reuse the running engine and just present and navigate the
        /// existing window.
        pub fn open(target: &str) -> Result<WebBrowser, String> {
            let url = crate::resolve_target(target);
            let mut slot = BROWSER_PROXY
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(proxy) = slot.clone() {
                proxy.send_event(Command::Navigate(url)).map_err(|e| {
                    *slot = None;
                    format!("browser engine is not running: {e}")
                })?;
                return Ok(WebBrowser {
                    proxy,
                    _thread: None,
                });
            }
            #[cfg(target_os = "linux")]
            {
                if GTK_TAKEN.load(Ordering::Acquire) {
                    return Err(
                        "browser engine thread exited after GTK initialization; restart AIOS to use the native browser"
                            .to_string(),
                    );
                }
            }
            let (ready_tx, ready_rx) = mpsc::channel::<ThreadMsg>();
            let (proxy_tx, proxy_rx) = mpsc::channel::<EventLoopProxy<Command>>();
            let thread = thread::Builder::new()
                .name("aios-webview".into())
                .spawn(move || {
                    let run = || -> Result<(), String> {
                        // Headless live boot (initramfs → /dev/console): no
                        // graphical session exists, so DISPLAY and friends are
                        // unset. Default to the standard X server started on
                        // :0 and a writable data dir before the loop connects,
                        // otherwise the window cannot be mapped.
                        if std::env::var_os("DISPLAY").is_none() {
                            std::env::set_var("DISPLAY", ":0");
                        }
                        if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
                            std::env::set_var("XDG_RUNTIME_DIR", "/run");
                        }
                        if std::env::var_os("AIOS_DATA_DIR").is_none() {
                            std::env::set_var("AIOS_DATA_DIR", "/tmp/aios-webview");
                        }
                        #[cfg(target_os = "linux")]
                        {
                            // The live system mounts a read-only root: point the
                            // user directories at writable tmpfs storage before
                            // GTK/WebKit first touch them, then bind GTK to this
                            // thread (wry requires gtk::init on the webview thread).
                            if std::env::var_os("HOME").is_none() {
                                let _ = std::fs::create_dir_all("/tmp/aios-home");
                                std::env::set_var("HOME", "/tmp/aios-home");
                            }
                            for (key, dir) in [
                                ("XDG_CACHE_HOME", "/tmp/aios-xdg/cache"),
                                ("XDG_CONFIG_HOME", "/tmp/aios-xdg/config"),
                                ("XDG_DATA_HOME", "/tmp/aios-xdg/data"),
                            ] {
                                if std::env::var_os(key).is_none() {
                                    let _ = std::fs::create_dir_all(dir);
                                    std::env::set_var(key, dir);
                                }
                            }
                            gtk::init().map_err(|e| format!("gtk init failed: {e}"))?;
                            GTK_TAKEN.store(true, Ordering::Release);
                        }
                        let mut builder = EventLoop::<Command>::with_user_event();
                        #[cfg(target_os = "linux")]
                        {
                            // The webview thread is not the process main thread;
                            // opt out of the default main-thread-only restriction.
                            use winit::platform::x11::EventLoopBuilderExtX11;
                            builder.with_any_thread(true);
                        }
                        let event_loop = builder.build().map_err(|e| e.to_string())?;
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
                ThreadMsg::Ready(Ok(())) => {
                    *slot = Some(proxy.clone());
                    Ok(WebBrowser {
                        proxy,
                        _thread: Some(thread),
                    })
                }
                ThreadMsg::Ready(Err(e)) => Err(format!("failed to create webview: {e}")),
            }
        }

        /// Navigate the browser to `target` (URL, host or search query),
        /// presenting the window if it was hidden.
        pub fn navigate(&self, target: &str) -> Result<(), String> {
            self.proxy
                .send_event(Command::Navigate(crate::resolve_target(target)))
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

        /// Hide the browser window; the engine thread keeps running and the
        /// next [`WebBrowser::open`] call presents the window again.
        pub fn close(&self) {
            let _ = self.proxy.send_event(Command::Close);
        }
    }

    impl Drop for WebBrowser {
        fn drop(&mut self) {
            self.close();
        }
    }
}

#[cfg(feature = "webview")]
pub use engine::WebBrowser;

#[cfg(test)]
mod tests {
    use super::resolve_target;

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
        assert!(resolve_target("hello world").contains("hello+world"));
        assert!(resolve_target("c++").contains("c%2B%2B"));
    }

    #[test]
    fn resolve_ip_address_like_host() {
        assert_eq!(resolve_target("192.168.1.1"), "https://192.168.1.1");
    }
}
