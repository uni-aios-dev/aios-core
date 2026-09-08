use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::theme::Theme;

/// A single labeled function key shown on the bottom bar, e.g. `1 Help`.
pub struct FKey<'a> {
    pub key: &'a str,
    pub label: &'a str,
}

/// Renders the top system status strip: version, node, network, layout,
/// uptime, battery, RAM — one dense line inside a double-ruled block.
pub fn draw_top_bar(
    f: &mut Frame<'_>,
    area: Rect,
    theme: &Theme,
    segments: &[(String, ratatui::style::Style)],
) {
    let mut spans: Vec<Span> = Vec::with_capacity(segments.len() * 2);
    for (i, (text, style)) in segments.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" │ ", theme.text_dim));
        }
        spans.push(Span::styled(text.as_str(), *style));
    }
    if spans.is_empty() {
        spans.push(Span::styled(" AIOS ", theme.accent));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.panel_border)
        .title(" AIOS ")
        .title_style(theme.title_active);
    let para = Paragraph::new(Line::from(spans)).block(block);
    f.render_widget(para, area);
}

/// Renders the classic bottom function-key strip with inverted key numbers.
pub fn draw_bottom_keys(f: &mut Frame<'_>, area: Rect, theme: &Theme, keys: &[FKey<'_>]) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.panel_border);
    let para = Paragraph::new(fkey_line(theme, keys)).block(block);
    f.render_widget(para, area);
}

fn fkey_line<'a>(theme: &Theme, keys: &[FKey<'a>]) -> Line<'a> {
    let mut spans: Vec<Span<'a>> = Vec::new();
    for (i, key) in keys.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            key.key,
            style_key_num(theme, key.key.len().max(1)),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::raw(key.label));
    }
    if keys.is_empty() {
        spans.push(Span::styled("F1 Help  F10 Quit", theme.text_dim));
    }
    Line::from(spans)
}

fn style_key_num(theme: &Theme, _len: usize) -> ratatui::style::Style {
    theme.key_hint.patch(Modifier::BOLD)
}

/// Convenience for building a centered command-line prompt `AIOS> _`.
pub fn command_prompt<'a>(theme: &Theme, text: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled("AIOS>", theme.prompt),
        Span::styled(" ", theme.text),
        Span::styled(text, theme.text),
        Span::styled("▌", theme.accent),
    ])
}

/// Shortcut to reduce boilerplate in callers that only set a title.
pub fn titled_block<'a>(theme: &Theme, title: &'a str, active: bool) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(if active {
            theme.panel_border_active
        } else {
            theme.panel_border
        })
        .title(title)
        .title_style(if active {
            theme.title_active
        } else {
            theme.title
        })
        .title_alignment(ratatui::layout::Alignment::Left)
}

/// Splits the main screen into the classic Far zones and returns them.
pub fn far_layout(area: Rect) -> [Rect; 4] {
    let zone = Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);
    [
        zone[0], // top status bar
        zone[1], // main split (left/right panels)
        zone[2], // command prompt
        zone[3], // bottom function keys
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::Theme;

    #[test]
    fn test_fkey_line_builds_spans() {
        let theme = Theme::far_classic();
        let keys = [
            FKey {
                key: "1",
                label: "Help",
            },
            FKey {
                key: "10",
                label: "Quit",
            },
        ];
        let line = fkey_line(&theme, &keys);
        assert_eq!(line.spans.len(), 7);
    }

    #[test]
    fn test_fkey_line_empty() {
        let theme = Theme::far_classic();
        let line = fkey_line(&theme, &[]);
        assert!(!line.spans.is_empty());
    }

    #[test]
    fn test_command_prompt_shape() {
        let theme = Theme::far_classic();
        let line = command_prompt(&theme, "store install foo");
        assert_eq!(line.spans.len(), 4);
    }

    #[test]
    fn test_far_layout_four_zones() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let z = far_layout(area);
        assert_eq!(z.len(), 4);
        assert_eq!(z[0].height, 3);
        assert_eq!(z[1].y, 3);
    }
}
