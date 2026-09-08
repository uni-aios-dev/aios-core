use ratatui::style::{Color, Modifier, Style};

/// A named color palette driving the whole Far/MC-style shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeKind {
    /// Classic blue/cyan high-contrast (default).
    FarClassic,
    /// Midnight Commander-like dark navy.
    MidnightDark,
    /// Pure monochrome for legacy terminals.
    Monochrome,
}

/// Resolved styles for one theme. Kept small and cache-friendly: a full set of
/// box-drawing borders, selection bars and status accents that every widget
/// reads from instead of hard-coding colors.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub panel_border: Style,
    pub panel_border_active: Style,
    pub title: Style,
    pub title_active: Style,
    pub text: Style,
    pub text_dim: Style,
    pub selection_bar: Style,
    pub selection_bar_active: Style,
    pub header: Style,
    pub ok: Style,
    pub warn: Style,
    pub error: Style,
    pub accent: Style,
    pub prompt: Style,
    pub key_hint: Style,
}

impl Theme {
    /// The protocol-wide default palette — deep blue background, cyan borders,
    /// white data, black-on-cyan selection bars.
    pub fn far_classic() -> Self {
        Self {
            panel_border: Style::default().fg(Color::DarkGray),
            panel_border_active: Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            title: Style::default().fg(Color::Cyan),
            title_active: Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            text: Style::default().fg(Color::White),
            text_dim: Style::default().fg(Color::Gray),
            selection_bar: Style::default()
                .fg(Color::White)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
            selection_bar_active: Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            header: Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            ok: Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            warn: Style::default().fg(Color::Yellow),
            error: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            accent: Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            prompt: Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            key_hint: Style::default().fg(Color::White).bg(Color::Blue),
        }
    }

    /// Midnight Commander flavour — near-black navy ground with brighter text.
    pub fn midnight_dark() -> Self {
        let base = Self::far_classic();
        Self {
            panel_border: Style::default().fg(Color::Blue),
            panel_border_active: Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            title: Style::default().fg(Color::White),
            ..base
        }
    }

    /// Legacy ANSI-safe theme with no background or accent colors.
    pub fn monochrome() -> Self {
        Self {
            panel_border: Style::default().fg(Color::Gray),
            panel_border_active: Style::default().add_modifier(Modifier::BOLD),
            title: Style::default().add_modifier(Modifier::BOLD),
            title_active: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            text: Style::default(),
            text_dim: Style::default().fg(Color::DarkGray),
            selection_bar: Style::default().add_modifier(Modifier::REVERSED),
            selection_bar_active: Style::default()
                .add_modifier(Modifier::REVERSED | Modifier::BOLD),
            header: Style::default().add_modifier(Modifier::BOLD),
            ok: Style::default().add_modifier(Modifier::BOLD),
            warn: Style::default().add_modifier(Modifier::BOLD),
            error: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            accent: Style::default().add_modifier(Modifier::BOLD),
            prompt: Style::default().add_modifier(Modifier::BOLD),
            key_hint: Style::default().add_modifier(Modifier::REVERSED),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::far_classic()
    }
}

impl From<ThemeKind> for Theme {
    fn from(kind: ThemeKind) -> Self {
        match kind {
            ThemeKind::FarClassic => Self::far_classic(),
            ThemeKind::MidnightDark => Self::midnight_dark(),
            ThemeKind::Monochrome => Self::monochrome(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_theme_default_is_far_classic() {
        let t = Theme::default();
        assert_eq!(t.panel_border_active.fg, Some(Color::Cyan));
    }

    #[test]
    fn test_theme_from_kind() {
        assert_eq!(
            Theme::from(ThemeKind::FarClassic).panel_border_active.fg,
            Some(Color::Cyan)
        );
        assert_eq!(
            Theme::from(ThemeKind::Monochrome).panel_border_active.fg,
            None,
            "Monochrome strips color from active border"
        );
    }

    #[test]
    fn test_theme_kind_is_copy() {
        let a = ThemeKind::FarClassic;
        let b = a;
        assert_eq!(a, b);
    }
}
