use ratatui::layout::Constraint;
use ratatui::style::Style;
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Cell, Row, Table, TableState};
use ratatui::Frame;

use crate::ui::theme::Theme;

/// Theme-aware helper for building the classic dense sysadmin tables
/// (processes, blocks, store) with a full-row selection bar.
#[allow(clippy::too_many_arguments)]
pub fn draw_dense_table(
    f: &mut Frame<'_>,
    area: ratatui::layout::Rect,
    theme: &Theme,
    title: String,
    active: bool,
    header_cells: &[Span<'_>],
    rows: &[Vec<Span<'_>>],
    widths: &[Constraint],
    selected: usize,
) {
    let border_style = if active {
        theme.panel_border_active
    } else {
        theme.panel_border
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title)
        .title_style(if active {
            theme.title_active
        } else {
            theme.title
        });

    let header = Row::new(header_cells.iter().cloned().map(Cell::from)).style(theme.header);

    let table_rows: Vec<Row> = rows
        .iter()
        .enumerate()
        .map(|(i, cells)| {
            let row = Row::new(cells.iter().cloned().map(Cell::from));
            if i == selected {
                row.style(if active {
                    theme.selection_bar_active
                } else {
                    theme.selection_bar
                })
            } else {
                row
            }
        })
        .collect();

    let mut state = TableState::default();
    state.select(Some(selected));

    let table = Table::new(table_rows, widths.iter().copied())
        .header(header)
        .block(block)
        .row_highlight_style(theme.selection_bar_active);

    f.render_stateful_widget(table, area, &mut state);
}

/// Simple bordered list used where a table is overkill (shell/log content).
pub fn draw_bordered_list(theme: &Theme, title: &str, active: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(if active {
            theme.panel_border_active
        } else {
            theme.panel_border
        })
        .title(title.to_string())
        .title_style(if active {
            theme.title_active
        } else {
            theme.title
        })
        .title_alignment(ratatui::layout::Alignment::Left)
}

/// Renders a bare selection bar without borders (used inside pre-drawn lists).
pub fn selection_bar_style(theme: &Theme, active: bool) -> Style {
    if active {
        theme.selection_bar_active
    } else {
        theme.selection_bar
    }
}

/// Truncate a string to `width` glyphs, appending an ellipsis when trimmed.
pub fn truncate(s: &str, width: usize) -> String {
    let mut out: String = s.chars().take(width).collect();
    if s.chars().count() > width {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::Theme;

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("abc", 10), "abc");
    }

    #[test]
    fn test_truncate_long() {
        let t = truncate("abcdefghij", 4);
        assert_eq!(t.chars().count(), 5);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn test_selection_bar_styles() {
        let theme = Theme::far_classic();
        assert_eq!(
            selection_bar_style(&theme, true).bg,
            Some(ratatui::style::Color::Cyan)
        );
        assert_eq!(
            selection_bar_style(&theme, false).fg,
            Some(ratatui::style::Color::White)
        );
    }
}
