//! Global keyboard layout switcher (RU/EN) with hotkey interception API.
//!
//! Frontends feed key events into [`LayoutManager::feed_key`]; the manager
//! detects registered hotkey combos ([`Hotkey`]), flips the active
//! [`Layout`] and exposes an indicator string consumed by the kernel TUI
//! header and the GUI top panel (`[EN/RU]` style segments).
//!
//! Per-window overrides let dialogs pin a layout while the global default
//! keeps following hotkeys.

/// Keyboard layouts supported out of the box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    English,
    Russian,
}

impl Layout {
    /// Two-letter indicator ("EN"/"RU").
    pub fn code(self) -> &'static str {
        match self {
            Self::English => "EN",
            Self::Russian => "RU",
        }
    }

    /// The other layout.
    pub fn toggled(self) -> Self {
        match self {
            Self::English => Self::Russian,
            Self::Russian => Self::English,
        }
    }
}

/// Modifier bitmask fed alongside key codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub super_key: bool,
}

impl Modifiers {
    /// Convenience constructor.
    pub fn new(alt: bool, ctrl: bool, shift: bool, super_key: bool) -> Self {
        Self {
            alt,
            ctrl,
            shift,
            super_key,
        }
    }

    fn matches(self, want: Modifiers) -> bool {
        self.alt == want.alt
            && self.ctrl == want.ctrl
            && self.shift == want.shift
            && self.super_key == want.super_key
    }
}

/// Physical key codes relevant to hotkey detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    Other,
    Shift,
    Space,
}

/// Registered hotkey combos that toggle the layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hotkey {
    AltShift,
    CtrlShift,
    CmdSpace,
}

impl Hotkey {
    fn modifiers(self) -> Modifiers {
        match self {
            Self::AltShift => Modifiers::new(true, false, true, false),
            Self::CtrlShift => Modifiers::new(false, true, true, false),
            Self::CmdSpace => Modifiers::new(false, false, false, true),
        }
    }

    fn trigger(self) -> KeyCode {
        match self {
            Self::AltShift | Self::CtrlShift => KeyCode::Shift,
            Self::CmdSpace => KeyCode::Space,
        }
    }
}

/// Global layout state machine.
///
/// Plain value type; the [`crate::SysControlHub`] wraps it in an async
/// RwLock while single-threaded TUI loops may own it directly.
#[derive(Debug)]
pub struct LayoutManager {
    active: Layout,
    hotkeys: Vec<Hotkey>,
    overrides: std::collections::HashMap<String, Layout>,
}

impl Default for LayoutManager {
    fn default() -> Self {
        Self {
            active: Layout::English,
            hotkeys: vec![Hotkey::AltShift, Hotkey::CtrlShift],
            overrides: std::collections::HashMap::new(),
        }
    }
}

impl LayoutManager {
    /// Manager starting in `active` with no hotkeys registered.
    pub fn bare(active: Layout) -> Self {
        Self {
            active,
            ..Self::default()
        }
    }

    /// Register an additional hotkey (deduplicated).
    pub fn register_hotkey(&mut self, hk: Hotkey) {
        if !self.hotkeys.contains(&hk) {
            self.hotkeys.push(hk);
        }
    }

    /// Unregister a hotkey.
    pub fn unregister_hotkey(&mut self, hk: Hotkey) {
        self.hotkeys.retain(|h| *h != hk);
    }

    /// Feed one key event; flips the layout when the combo matches a
    /// registered hotkey and returns whether a flip happened.
    pub fn feed_key(&mut self, mods: Modifiers, key: KeyCode) -> bool {
        let hit = self
            .hotkeys
            .iter()
            .any(|hk| hk.modifiers().matches(mods) && hk.trigger() == key);
        hit && self.toggle()
    }

    /// Apply a hotkey directly (frontend already recognized the combo).
    ///
    /// Returns false when the hotkey is not registered.
    pub fn apply_hotkey(&mut self, hk: Hotkey) -> bool {
        if !self.hotkeys.contains(&hk) {
            return false;
        }
        self.toggle()
    }

    fn toggle(&mut self) -> bool {
        self.active = self.active.toggled();
        true
    }

    /// Active global layout.
    pub fn active(&self) -> Layout {
        self.active
    }

    /// Pin `layout` for a named window/dialog.
    pub fn set_window_layout(&mut self, window: &str, layout: Layout) {
        self.overrides.insert(window.to_string(), layout);
    }

    /// Clear a window override.
    pub fn clear_window_layout(&mut self, window: &str) {
        self.overrides.remove(window);
    }

    /// Effective layout for a window: override when present, global otherwise.
    pub fn effective_layout(&self, window: &str) -> Layout {
        self.overrides.get(window).copied().unwrap_or(self.active)
    }

    /// Current indicator ("EN"/"RU").
    pub fn indicator(&self) -> &'static str {
        self.active.code()
    }

    /// Status-bar segment with the active layout first (`[EN/RU]`).
    pub fn status_segment(&self) -> String {
        format!("[{}/{}]", self.active.code(), self.active.toggled().code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_shift_flips_en_to_ru_and_back() {
        let mut m = LayoutManager::default();
        assert!(m.feed_key(Modifiers::new(true, false, true, false), KeyCode::Shift));
        assert_eq!(m.indicator(), "RU");
        assert!(m.feed_key(Modifiers::new(true, false, true, false), KeyCode::Shift));
        assert_eq!(m.indicator(), "EN");
    }

    #[test]
    fn partial_modifier_combos_do_not_trigger() {
        let mut m = LayoutManager::default();
        assert!(!m.feed_key(Modifiers::new(true, false, false, false), KeyCode::Shift));
        assert!(!m.feed_key(Modifiers::new(false, false, true, false), KeyCode::Shift));
        assert_eq!(m.indicator(), "EN");
    }

    #[test]
    fn cmd_space_requires_registration() {
        let mut m = LayoutManager::default();
        assert!(!m.apply_hotkey(Hotkey::CmdSpace));
        m.register_hotkey(Hotkey::CmdSpace);
        assert!(m.feed_key(Modifiers::new(false, false, false, true), KeyCode::Space));
        assert_eq!(m.indicator(), "RU");
        m.unregister_hotkey(Hotkey::CmdSpace);
        assert!(!m.apply_hotkey(Hotkey::CmdSpace));
        assert_eq!(m.indicator(), "RU");
    }

    #[test]
    fn register_is_deduplicated() {
        let mut m = LayoutManager::bare(Layout::Russian);
        m.register_hotkey(Hotkey::CtrlShift);
        m.register_hotkey(Hotkey::CtrlShift);
        assert!(m.apply_hotkey(Hotkey::CtrlShift));
        assert_eq!(m.active(), Layout::English);
    }

    #[test]
    fn window_override_pins_layout_independent_of_global() {
        let mut m = LayoutManager::default();
        m.set_window_layout("password-dialog", Layout::Russian);
        assert_eq!(m.effective_layout("password-dialog"), Layout::Russian);
        assert_eq!(m.effective_layout("main"), Layout::English);
        m.feed_key(Modifiers::new(false, true, true, false), KeyCode::Shift);
        assert_eq!(m.effective_layout("main"), Layout::Russian);
        assert_eq!(m.effective_layout("password-dialog"), Layout::Russian);
        m.clear_window_layout("password-dialog");
        assert_eq!(m.effective_layout("password-dialog"), Layout::Russian);
    }

    #[test]
    fn indicators_render_active_first_pair() {
        let m = LayoutManager::default();
        assert_eq!(m.status_segment(), "[EN/RU]");
        let ru = LayoutManager::bare(Layout::Russian);
        assert_eq!(ru.status_segment(), "[RU/EN]");
        assert_eq!(ru.indicator(), "RU");
    }

    #[test]
    fn layout_toggle_roundtrip_all_variants() {
        assert_eq!(Layout::English.toggled(), Layout::Russian);
        assert_eq!(Layout::Russian.toggled(), Layout::English);
        assert_eq!(Layout::English.code(), "EN");
        assert_eq!(Layout::Russian.code(), "RU");
    }
}
