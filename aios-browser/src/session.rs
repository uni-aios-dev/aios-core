//! Stateful browsing session: per-tab back/forward history, bookmarks and
//! script evaluation over a [`BrowserEngine`].
//!
//! The UIs that already keep their own richer per-tab view state (scroll,
//! link selection) can keep it — this session is the shared engine-level
//! source of truth for navigation stacks and bookmarks, used by the IPC
//! block and available to any embedder.

use crate::engine::BrowserEngine;
use crate::script::ScriptValue;
use crate::types::{BrowserConfig, BrowserError, Page};
use serde::{Deserialize, Serialize};

/// Normalize user input into an absolute URL: trims whitespace and adds
/// `https://` when no scheme (`https:`, `file:`, ...) is present. Deciding
/// whether the input is a URL at all vs. a search query stays with the
/// caller.
pub fn normalize_url(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let bytes = trimmed.as_bytes();
    if bytes.first().is_some_and(u8::is_ascii_alphabetic) {
        if let Some(colon) = trimmed.find(':') {
            let scheme = &trimmed[..colon];
            if !scheme.is_empty()
                && scheme
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
            {
                return trimmed.to_string();
            }
        }
    }
    format!("https://{trimmed}")
}

/// A saved bookmark: display name plus target URL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bookmark {
    /// Human-readable label (page title by default).
    pub name: String,
    /// Absolute URL the bookmark opens.
    pub url: String,
}

/// Navigation stacks and the last loaded page of one session tab.
#[derive(Debug, Clone, Default)]
pub struct SessionTab {
    /// Back stack: previously visited URLs, most recent last.
    pub history: Vec<String>,
    /// Forward stack: URLs reachable again via [`BrowserSession::forward`].
    pub forward: Vec<String>,
    /// The tab's current URL, if it has navigated at all.
    pub current: Option<String>,
    /// Last successfully loaded page (kept for `eval_js` replay).
    pub page: Option<Page>,
}

/// Serializable projection of a session for block state persistence:
/// stacks and bookmarks only — pages are refetched on demand.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// Saved bookmarks (name + URL).
    pub bookmarks: Vec<Bookmark>,
    /// Per-tab stacks in tab order: `(history, forward, current)`.
    pub tabs: Vec<(Vec<String>, Vec<String>, Option<String>)>,
    /// Index of the active tab.
    pub active: usize,
}

/// A full browsing session: engine + tabs + bookmarks.
pub struct BrowserSession {
    engine: BrowserEngine,
    tabs: Vec<SessionTab>,
    active: usize,
    bookmarks: Vec<Bookmark>,
}

impl BrowserSession {
    /// Create a session with one empty tab.
    pub fn new(config: BrowserConfig) -> Self {
        Self {
            engine: BrowserEngine::new(config),
            tabs: vec![SessionTab::default()],
            active: 0,
            bookmarks: Vec::new(),
        }
    }

    /// The underlying engine.
    pub fn engine(&self) -> &BrowserEngine {
        &self.engine
    }

    /// The underlying engine, mutable (userscript registration).
    pub fn engine_mut(&mut self) -> &mut BrowserEngine {
        &mut self.engine
    }

    /// Register a userscript applied to every matching page.
    pub fn add_user_script(&mut self, pattern: impl Into<String>, source: impl Into<String>) {
        self.engine.add_user_script(pattern, source);
    }

    /// Navigate the active tab to `url`, recording the visit in its back
    /// history and clearing the forward stack.
    pub async fn go(&mut self, url: &str) -> Result<Page, BrowserError> {
        let normalized = normalize_url(url);
        if normalized.is_empty() {
            return Err(BrowserError::NetworkError("empty URL".into()));
        }
        let page = self.engine.navigate(&normalized).await?;
        let tab = &mut self.tabs[self.active];
        let previous = tab.current.replace(normalized);
        if let Some(prev) = previous {
            if prev != page.url {
                tab.history.push(prev);
            }
        }
        tab.forward.clear();
        tab.page = Some(page.clone());
        Ok(page)
    }

    /// Pop the active tab's back stack: move `current` to forward and take
    /// the most recent history entry. Returns the target URL, or `None`
    /// when there is nothing to go back to.
    fn back_target(&mut self) -> Option<String> {
        let tab = &mut self.tabs[self.active];
        let target = tab.history.pop()?;
        if let Some(current) = tab.current.take() {
            tab.forward.push(current);
        }
        tab.current = Some(target.clone());
        Some(target)
    }

    /// Push `current` onto the back stack and take the most recent forward
    /// entry. Returns the target URL, or `None` when there is nothing to
    /// go forward to.
    fn forward_target(&mut self) -> Option<String> {
        let tab = &mut self.tabs[self.active];
        let target = tab.forward.pop()?;
        if let Some(current) = tab.current.take() {
            tab.history.push(current);
        }
        tab.current = Some(target.clone());
        Some(target)
    }

    /// Can the active tab go back?
    pub fn can_back(&self) -> bool {
        !self.tabs[self.active].history.is_empty()
    }

    /// Can the active tab go forward?
    pub fn can_forward(&self) -> bool {
        !self.tabs[self.active].forward.is_empty()
    }

    /// Go back one page. `Ok(None)` = nothing to go back to.
    pub async fn back(&mut self) -> Result<Option<Page>, BrowserError> {
        let Some(target) = self.back_target() else {
            return Ok(None);
        };
        let page = self.engine.navigate(&target).await?;
        self.tabs[self.active].page = Some(page.clone());
        Ok(Some(page))
    }

    /// Go forward one page. `Ok(None)` = nothing to go forward to.
    pub async fn forward(&mut self) -> Result<Option<Page>, BrowserError> {
        let Some(target) = self.forward_target() else {
            return Ok(None);
        };
        let page = self.engine.navigate(&target).await?;
        self.tabs[self.active].page = Some(page.clone());
        Ok(Some(page))
    }

    /// Reload the active tab's current URL. `Err` when it never navigated.
    pub async fn reload(&mut self) -> Result<Page, BrowserError> {
        let target = self.tabs[self.active]
            .current
            .clone()
            .ok_or_else(|| BrowserError::NetworkError("no page to reload".into()))?;
        let page = self.engine.navigate(&target).await?;
        self.tabs[self.active].page = Some(page.clone());
        Ok(page)
    }

    /// The active tab's current URL.
    pub fn current_url(&self) -> Option<&str> {
        self.tabs[self.active].current.as_deref()
    }

    /// The active tab's last loaded page.
    pub fn current_page(&self) -> Option<&Page> {
        self.tabs[self.active].page.as_ref()
    }

    /// Number of open tabs.
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// Index of the active tab.
    pub fn active_tab(&self) -> usize {
        self.active
    }

    /// Open a new empty tab (and activate it). Returns its index.
    pub fn new_tab(&mut self) -> usize {
        self.tabs.push(SessionTab::default());
        self.active = self.tabs.len() - 1;
        self.active
    }

    /// Close tab `index`. Refuses to close the last remaining tab.
    pub fn close_tab(&mut self, index: usize) -> Result<(), String> {
        if index >= self.tabs.len() {
            return Err(format!("no such tab: {index}"));
        }
        if self.tabs.len() == 1 {
            return Err("cannot close the last tab".into());
        }
        self.tabs.remove(index);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        } else if self.active > index {
            self.active -= 1;
        }
        Ok(())
    }

    /// Activate tab `index`.
    pub fn select_tab(&mut self, index: usize) -> Result<(), String> {
        if index >= self.tabs.len() {
            return Err(format!("no such tab: {index}"));
        }
        self.active = index;
        Ok(())
    }

    /// Saved bookmarks, in display order.
    pub fn bookmarks(&self) -> &[Bookmark] {
        &self.bookmarks
    }

    /// Add a bookmark; re-bookmarking the same URL renames it.
    pub fn add_bookmark(&mut self, name: impl Into<String>, url: impl Into<String>) -> usize {
        let url = normalize_url(&url.into());
        let name = name.into();
        if let Some(existing) = self.bookmarks.iter_mut().find(|b| b.url == url) {
            existing.name = name;
            return self
                .bookmarks
                .iter()
                .position(|b| b.url == url)
                .unwrap_or(0);
        }
        self.bookmarks.push(Bookmark { name, url });
        self.bookmarks.len() - 1
    }

    /// Remove the bookmark at `index`, if any.
    pub fn remove_bookmark(&mut self, index: usize) -> Option<Bookmark> {
        if index < self.bookmarks.len() {
            Some(self.bookmarks.remove(index))
        } else {
            None
        }
    }

    /// Persist bookmarks as JSON at `path`.
    pub fn save_bookmarks(&self, path: &std::path::Path) -> Result<(), BrowserError> {
        let json = serde_json::to_string_pretty(&self.bookmarks)
            .map_err(|e| BrowserError::Io(e.to_string()))?;
        std::fs::write(path, json).map_err(|e| BrowserError::Io(e.to_string()))
    }

    /// Load bookmarks from a JSON file at `path`, replacing the current
    /// list. Returns how many were loaded.
    pub fn load_bookmarks(&mut self, path: &std::path::Path) -> Result<usize, BrowserError> {
        let json = std::fs::read_to_string(path).map_err(|e| BrowserError::Io(e.to_string()))?;
        let bookmarks: Vec<Bookmark> =
            serde_json::from_str(&json).map_err(|e| BrowserError::Io(e.to_string()))?;
        let count = bookmarks.len();
        self.bookmarks = bookmarks;
        Ok(count)
    }

    /// Evaluate `source` in a fresh context replaying the active tab's
    /// last loaded page (post-script HTML). The page's own scripts are not
    /// re-run — the stored HTML already contains their output.
    pub fn eval_js(&self, source: &str) -> Result<(ScriptValue, Vec<String>), BrowserError> {
        let page = self
            .current_page()
            .ok_or_else(|| BrowserError::ScriptError("no page loaded".into()))?;
        self.engine.evaluate(&page.html, &page.url, source)
    }

    /// Project the session into a [`SessionSnapshot`] for state persistence.
    pub fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            bookmarks: self.bookmarks.clone(),
            tabs: self
                .tabs
                .iter()
                .map(|t| (t.history.clone(), t.forward.clone(), t.current.clone()))
                .collect(),
            active: self.active,
        }
    }

    /// Restore stacks and bookmarks from a snapshot. Pages are dropped
    /// (they are refetched on the next navigation).
    pub fn restore(&mut self, snapshot: SessionSnapshot) {
        self.bookmarks = snapshot.bookmarks;
        if snapshot.tabs.is_empty() {
            self.tabs = vec![SessionTab::default()];
            self.active = 0;
            return;
        }
        self.tabs = snapshot
            .tabs
            .into_iter()
            .map(|(history, forward, current)| SessionTab {
                history,
                forward,
                current,
                page: None,
            })
            .collect();
        self.active = snapshot.active.min(self.tabs.len() - 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> BrowserSession {
        BrowserSession::new(BrowserConfig {
            headless_fallback: false,
            ..Default::default()
        })
    }

    #[test]
    fn normalize_url_adds_https_and_trims() {
        assert_eq!(normalize_url("example.com"), "https://example.com");
        assert_eq!(normalize_url("  example.com  "), "https://example.com");
        assert_eq!(normalize_url("http://plain.test/"), "http://plain.test/");
        assert_eq!(normalize_url("https://secure.test"), "https://secure.test");
        assert_eq!(normalize_url("file:///tmp/x.html"), "file:///tmp/x.html");
        assert_eq!(normalize_url(""), "");
        assert_eq!(normalize_url("   "), "");
    }

    #[test]
    fn back_and_forward_move_between_stacks() {
        let mut s = session();
        {
            let tab = &mut s.tabs[0];
            tab.history.push("https://a.test/".into());
            tab.current = Some("https://b.test/".into());
        }
        assert!(s.can_back());
        assert!(!s.can_forward());

        let target = s.back_target().unwrap();
        assert_eq!(target, "https://a.test/");
        assert_eq!(s.tabs[0].current.as_deref(), Some("https://a.test/"));
        assert_eq!(s.tabs[0].forward, vec!["https://b.test/".to_string()]);
        assert!(!s.can_back());
        assert!(s.can_forward());

        let target = s.forward_target().unwrap();
        assert_eq!(target, "https://b.test/");
        assert!(s.can_back());
        assert!(!s.can_forward());

        assert!(s.back_target().is_some());
        assert!(s.back_target().is_none());
    }

    #[tokio::test]
    async fn back_on_fresh_session_is_none() {
        let mut s = session();
        assert!(s.back().await.unwrap().is_none());
        assert!(s.forward().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn reload_without_page_errors() {
        let mut s = session();
        let err = s.reload().await.unwrap_err();
        assert!(err.to_string().contains("no page"));
    }

    #[test]
    fn tab_lifecycle() {
        let mut s = session();
        assert_eq!(s.tab_count(), 1);
        assert_eq!(s.active_tab(), 0);
        assert!(s.close_tab(0).is_err(), "last tab must not close");

        let idx = s.new_tab();
        assert_eq!(idx, 1);
        assert_eq!(s.active_tab(), 1);
        assert!(s.close_tab(5).is_err());
        s.close_tab(1).unwrap();
        assert_eq!(s.tab_count(), 1);
        assert_eq!(s.active_tab(), 0);

        assert!(s.select_tab(3).is_err());
        s.select_tab(0).unwrap();
        assert_eq!(s.active_tab(), 0);
    }

    #[test]
    fn bookmarks_add_dedupe_remove() {
        let mut s = session();
        s.add_bookmark("One", "https://one.test/");
        s.add_bookmark("Two", "https://two.test/");
        assert_eq!(s.bookmarks().len(), 2);

        // Re-bookmarking the same URL renames in place.
        s.add_bookmark("One renamed", "https://one.test/");
        assert_eq!(s.bookmarks().len(), 2);
        assert_eq!(s.bookmarks()[0].name, "One renamed");

        // The bookmark URL is normalized too.
        s.add_bookmark("Three", "three.test");
        assert_eq!(s.bookmarks()[2].url, "https://three.test");

        let removed = s.remove_bookmark(1).unwrap();
        assert_eq!(removed.url, "https://two.test/");
        assert_eq!(s.bookmarks().len(), 2);
        assert!(s.remove_bookmark(99).is_none());
    }

    #[test]
    fn bookmarks_persist_through_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bookmarks.json");
        let mut s = session();
        s.add_bookmark("Alpha", "https://alpha.test/");
        s.add_bookmark("Beta", "https://beta.test/");
        s.save_bookmarks(&path).unwrap();

        let mut restored = session();
        let count = restored.load_bookmarks(&path).unwrap();
        assert_eq!(count, 2);
        assert_eq!(restored.bookmarks(), s.bookmarks());

        let missing = restored.load_bookmarks(&dir.path().join("nope.json"));
        assert!(missing.is_err());
    }

    #[test]
    fn eval_js_without_page_errors() {
        let s = session();
        let err = s.eval_js("1 + 1").unwrap_err();
        assert!(err.to_string().contains("no page"));
    }

    #[test]
    fn eval_js_replays_stored_page() {
        let mut s = session();
        s.tabs[0].page = Some(Page {
            url: "https://example.com/".into(),
            title: "T".into(),
            text_content: "hello".into(),
            html: "<html><head><title>T</title></head><body><p>hello</p></body></html>".into(),
            links: Vec::new(),
            console: Vec::new(),
            scripts_executed: 0,
            script_errors: Vec::new(),
        });
        let (value, _console) = s.eval_js("document.title").unwrap();
        assert_eq!(value, ScriptValue::String("T".into()));
        let (sum, _) = s.eval_js("21 * 2").unwrap();
        assert_eq!(sum, ScriptValue::Number(42.0));
    }

    #[test]
    fn snapshot_roundtrip_preserves_stacks_and_bookmarks() {
        let mut s = session();
        {
            let tab = &mut s.tabs[0];
            tab.history.push("https://old.test/".into());
            tab.current = Some("https://now.test/".into());
            tab.forward.push("https://next.test/".into());
        }
        s.new_tab();
        s.add_bookmark("N", "https://now.test/");

        let snap = s.snapshot();
        let mut other = session();
        other.restore(snap);
        assert_eq!(other.tab_count(), 2);
        assert_eq!(other.active_tab(), 1);
        assert_eq!(other.tabs[0].history, vec!["https://old.test/".to_string()]);
        assert_eq!(
            other.tabs[0].forward,
            vec!["https://next.test/".to_string()]
        );
        assert_eq!(other.tabs[0].current.as_deref(), Some("https://now.test/"));
        assert_eq!(other.bookmarks().len(), 1);
        // Pages are not restored (refetched on demand).
        assert!(other.tabs[0].page.is_none());
    }

    #[test]
    fn restore_from_empty_snapshot_keeps_one_tab() {
        let mut s = session();
        s.new_tab();
        s.restore(SessionSnapshot::default());
        assert_eq!(s.tab_count(), 1);
        assert_eq!(s.active_tab(), 0);
    }
}
