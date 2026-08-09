//! Optional headless Chromium-class rendering fallback for JS-heavy pages.
//!
//! Some sites serve an almost empty HTML shell and render the actual content
//! with client-side JavaScript. When the plain HTTP fetch yields no meaningful
//! text, [`render_to_html`] launches a headless Chromium-class browser
//! (`msedge`, `chromium`, `google-chrome`, ...) with `--dump-dom` and returns
//! the fully rendered DOM so the regular HTML parser can extract real content.
use crate::types::BrowserError;
use std::path::{Path, PathBuf};

/// Env var overriding the headless browser binary (bare name or full path).
pub const ENV_BROWSER: &str = "AIOS_HEADLESS_BROWSER";
/// Env var: add `--no-sandbox` to the browser invocation (unprivileged containers).
pub const ENV_NO_SANDBOX: &str = "AIOS_HEADLESS_NO_SANDBOX";
/// Hard cap on the dumped DOM, keeps a pathological page from exhausting memory.
const MAX_DUMP_BYTES: usize = 4 * 1024 * 1024;
/// How long a headless render may take before we give up.
const RENDER_TIMEOUT_SECS: u64 = 30;

/// Ordered candidate browser names and well-known install locations.
pub(crate) fn candidates() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    if let Ok(over) = std::env::var(ENV_BROWSER) {
        if !over.trim().is_empty() {
            v.push(PathBuf::from(over));
        }
    }
    for name in [
        "msedge",
        "microsoft-edge",
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
        "chrome",
        "brave-browser",
    ] {
        v.push(PathBuf::from(name));
    }
    if cfg!(windows) {
        let pf86 =
            std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| "C:\\Program Files (x86)".into());
        let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into());
        v.push(PathBuf::from(format!(
            r"{pf86}\Microsoft\Edge\Application\msedge.exe"
        )));
        v.push(PathBuf::from(format!(
            r"{pf}\Microsoft\Edge\Application\msedge.exe"
        )));
        v.push(PathBuf::from(format!(
            r"{pf}\Google\Chrome\Application\chrome.exe"
        )));
    } else if cfg!(target_os = "macos") {
        v.push(PathBuf::from(
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        ));
        v.push(PathBuf::from(
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        ));
    }
    v
}

/// Resolve a bare executable name through `PATH` (append `.exe` on Windows).
fn resolve_on_path(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) && !name.to_lowercase().ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let path = std::env::var("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join(&exe))
        .find(|p| p.is_file())
}

/// First candidate that resolves to an executable on this machine, else `None`.
pub fn find_browser() -> Option<PathBuf> {
    for cand in candidates() {
        let found = if cand.is_absolute() || cand.components().count() > 1 {
            cand.is_file().then_some(cand)
        } else {
            cand.to_str().and_then(resolve_on_path)
        };
        if let Some(p) = found {
            return Some(p);
        }
    }
    None
}

/// Pure construction of the headless invocation so the CLI can be unit-tested.
pub fn headless_invocation(binary: &Path, url: &str) -> (PathBuf, Vec<String>) {
    let mut args: Vec<String> = vec![
        "--headless".into(),
        "--disable-gpu".into(),
        "--disable-extensions".into(),
        "--no-first-run".into(),
        "--virtual-time-budget=5000".into(),
        "--dump-dom".into(),
    ];
    if std::env::var_os(ENV_NO_SANDBOX).is_some() {
        args.push("--no-sandbox".into());
    }
    args.push(url.to_string());
    (binary.to_path_buf(), args)
}

/// Render `url` with a specific binary (used by tests and callers that already
/// resolved the browser).
pub async fn render_with_binary(binary: &Path, url: &str) -> Result<String, BrowserError> {
    let (binary, args) = headless_invocation(binary, url);
    let future = tokio::task::spawn_blocking(move || {
        let mut cmd = std::process::Command::new(&binary);
        cmd.args(&args);
        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(std::io::Error::other(stderr));
        }
        Ok(output.stdout)
    });
    let timed =
        tokio::time::timeout(std::time::Duration::from_secs(RENDER_TIMEOUT_SECS), future).await;
    let mut bytes = timed
        .map_err(|_| BrowserError::Timeout)?
        .map_err(|e| BrowserError::NetworkError(format!("headless task failed: {e}")))?
        .map_err(|e| BrowserError::NetworkError(format!("headless render failed: {e}")))?;
    bytes.truncate(MAX_DUMP_BYTES);
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Render `url` in the first available headless browser.
pub async fn render_to_html(url: &str) -> Result<String, BrowserError> {
    let binary = find_browser()
        .ok_or_else(|| BrowserError::CapabilityDenied("no headless browser found".into()))?;
    render_with_binary(&binary, url).await
}

/// True when the plain fetch produced almost no readable text — the signature
/// of a JS-rendered SPA shell.
pub fn looks_like_js_shell(text: &str) -> bool {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .take(200)
        .count()
        < 80
}

/// Adopt the headless dump only when it is meaningfully richer than the plain
/// fetch; otherwise the original HTML stays authoritative.
pub fn has_more_content(current_text: &str, dumped_html: &str) -> bool {
    let dumped = crate::html_parser::HtmlParser::extract_text(dumped_html);
    let current = current_text.chars().filter(|c| !c.is_whitespace()).count();
    let rendered = dumped.chars().filter(|c| !c.is_whitespace()).count();
    rendered > current + 60
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_candidates_include_common_names() {
        let list = candidates();
        assert!(!list.is_empty());
        assert!(list.iter().any(|p| p.to_string_lossy().contains("msedge")));
    }

    #[test]
    fn test_headless_invocation_args() {
        let (binary, args) = headless_invocation(Path::new("edge"), "https://example.com/");
        assert_eq!(binary, PathBuf::from("edge"));
        assert!(args.iter().any(|a| a == "--headless"));
        assert!(args.iter().any(|a| a == "--dump-dom"));
        assert!(args.iter().any(|a| a.starts_with("--virtual-time-budget")));
        assert_eq!(
            args.last().map(String::as_str),
            Some("https://example.com/")
        );
    }

    #[test]
    fn test_render_with_missing_binary_errors() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt
            .block_on(render_with_binary(
                Path::new("definitely-not-a-real-browser-binary-xyz"),
                "https://example.com/",
            ))
            .unwrap_err();
        assert!(matches!(
            err,
            BrowserError::NetworkError(_) | BrowserError::Timeout
        ));
    }

    #[test]
    fn test_looks_like_js_shell() {
        assert!(looks_like_js_shell(""));
        assert!(looks_like_js_shell("   \n  Loading...  "));
        let rich = "word ".repeat(300);
        assert!(!looks_like_js_shell(&rich));
    }

    #[test]
    fn test_has_more_content() {
        let current = "<div>Loading...</div>";
        let dumped = format!(
            "<div><h1>Full article</h1><p>{}...</p></div>",
            "sentences ".repeat(40)
        );
        assert!(has_more_content(current, &dumped));
        assert!(!has_more_content(current, current));
        let same = "<div><p>Short body text</p></div>";
        assert!(!has_more_content(same, same));
    }
}
