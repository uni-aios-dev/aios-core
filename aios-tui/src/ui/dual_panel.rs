use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::Frame;

use super::status_bar::titled_block;
use super::theme::Theme;

/// Which of the two panels currently owns keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelSide {
    Left,
    Right,
}

/// Renders a 50/50 split with an active/inactive border highlight. The caller
/// provides a closure that draws the contents of each panel onto its Rect; the
/// wrapper only owns the frame, borders and focus styling.
#[allow(clippy::too_many_arguments)]
pub fn draw_dual_panel(
    f: &mut Frame<'_>,
    area: Rect,
    theme: &Theme,
    left_title: &str,
    right_title: &str,
    active: PanelSide,
    zoom: bool,
    draw_left: &dyn Fn(&mut Frame<'_>, Rect),
    draw_right: &dyn Fn(&mut Frame<'_>, Rect),
) {
    if zoom {
        let block = titled_block(theme, left_title, active == PanelSide::Left);
        let inner = inset(area, 1);
        f.render_widget(block, area);
        draw_left(f, inner);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let left_rect = chunks[0];
    let right_rect = chunks[1];

    let left_block = titled_block(theme, left_title, active == PanelSide::Left);
    let right_block = titled_block(theme, right_title, active == PanelSide::Right);

    f.render_widget(left_block, left_rect);
    f.render_widget(right_block, right_rect);

    draw_left(f, inside_borders(left_rect));
    draw_right(f, inside_borders(right_rect));
}

/// Shrink a `Rect` so a widget with a full border fits inside it without
/// overwriting the border drawn by the panel wrapper.
fn inside_borders(area: Rect) -> Rect {
    let inner = inset(area, 1);
    // Leave one extra row for the title line of the panel.
    Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: inner.height.saturating_sub(1),
    }
}

fn inset(area: Rect, n: u16) -> Rect {
    Rect {
        x: area.x.saturating_add(n),
        y: area.y.saturating_add(n),
        width: area.width.saturating_sub(n.saturating_mul(2)),
        height: area.height.saturating_sub(n.saturating_mul(2)),
    }
}

/// Renders a centered modal overlay spanning most of the screen, used for
/// help and dialogs on top of the panel grid.
pub fn draw_modal(
    f: &mut Frame<'_>,
    area: Rect,
    theme: &Theme,
    title: &str,
    body: &dyn Fn(&mut Frame<'_>, Rect),
) {
    let w = (area.width * 3 / 4)
        .max(40)
        .min(area.width.saturating_sub(2));
    let h = (area.height * 7 / 8)
        .max(10)
        .min(area.height.saturating_sub(2));
    let modal = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    let block = titled_block(theme, title, true);
    f.render_widget(block, modal);
    let inner = inside_borders(modal);
    body(f, inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::Theme;

    #[test]
    fn test_panel_side_equality() {
        assert_eq!(PanelSide::Left, PanelSide::Left);
        assert_ne!(PanelSide::Left, PanelSide::Right);
    }

    #[test]
    fn test_inside_borders_shrinks() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let inner = inside_borders(area);
        assert_eq!(inner.width, 78);
        assert_eq!(inner.y, 2);
    }

    #[test]
    fn test_inset_saturates() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 4,
            height: 4,
        };
        let inner = inset(area, 8);
        assert_eq!(inner.width, 0);
        assert_eq!(inner.x, 8);
    }

    #[test]
    fn test_draw_modal_sizes() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 50,
        };
        let theme = Theme::far_classic();
        let _ = |f: &mut ratatui::Frame| draw_modal(f, area, &theme, "test", &|_, _| {});
    }
}
