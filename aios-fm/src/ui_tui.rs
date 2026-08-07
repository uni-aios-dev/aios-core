use crate::engine::{FmSnapshot, JobStatus};
use crate::state::{human_size, PanelSide, PanelState};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, Paragraph};
use ratatui::Frame;

/// Height of the header (scheme + ACL) in rows.
pub const HEADER_HEIGHT: u16 = 2;
/// Height of the footer (jobs + hotkeys) in rows.
pub const FOOTER_HEIGHT: u16 = 4;

/// A high-level action the host TUI loop can execute against the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiAction {
    MoveUp { side: PanelSide },
    MoveDown { side: PanelSide },
    Enter { side: PanelSide },
    GoUp { side: PanelSide },
    SwitchPanel,
    CopySelected,
    MoveSelected,
    DeleteSelected,
    Mkdir { side: PanelSide },
    Rename { side: PanelSide },
    ViewSelected,
    ToggleSort { side: PanelSide },
    GrantHostRead,
    GrantHostWrite,
    Refresh { side: PanelSide },
    Close,
}

/// Map a crossterm key event to a file-manager action for `side`.
pub fn key_to_action(key: KeyEvent, side: PanelSide) -> Option<TuiAction> {
    match key.code {
        KeyCode::Up => Some(TuiAction::MoveUp { side }),
        KeyCode::Down => Some(TuiAction::MoveDown { side }),
        KeyCode::Enter => Some(TuiAction::Enter { side }),
        KeyCode::Backspace | KeyCode::Left => Some(TuiAction::GoUp { side }),
        KeyCode::Tab => Some(TuiAction::SwitchPanel),
        KeyCode::F(3) => Some(TuiAction::ViewSelected),
        KeyCode::F(5) => Some(TuiAction::CopySelected),
        KeyCode::F(6) => Some(TuiAction::MoveSelected),
        KeyCode::F(7) => Some(TuiAction::Mkdir { side }),
        KeyCode::F(8) => Some(TuiAction::DeleteSelected),
        KeyCode::F(2) => Some(TuiAction::Rename { side }),
        KeyCode::F(9) => Some(TuiAction::ToggleSort { side }),
        KeyCode::Char('r') => Some(TuiAction::Refresh { side }),
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::ALT) => {
            Some(TuiAction::ViewSelected)
        }
        KeyCode::Char('g') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(TuiAction::GrantHostRead)
        }
        KeyCode::Char('w') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(TuiAction::GrantHostWrite)
        }
        KeyCode::Esc => Some(TuiAction::Close),
        _ => None,
    }
}

/// Render the full file-manager region (header, two panels, footer) into the
/// frame. `rows` is the per-panel viewport height in rows.
pub fn draw(frame: &mut Frame, area: Rect, snap: &FmSnapshot, rows: usize) {
    let layout = Layout::vertical([
        Constraint::Length(HEADER_HEIGHT),
        Constraint::Min(3),
        Constraint::Length(FOOTER_HEIGHT),
    ])
    .split(area);

    draw_header(frame, layout[0], snap);
    let mid = layout[1];
    let cols = Layout::horizontal([
        Constraint::Percentage(49),
        Constraint::Length(2),
        Constraint::Percentage(49),
    ])
    .split(mid);
    for (i, panel) in snap.panels.iter().enumerate() {
        draw_panel(frame, cols[i * 2], panel, i == snap.active, rows);
    }
    draw_footer(frame, layout[2], snap);
}

fn draw_header(frame: &mut Frame, area: Rect, snap: &FmSnapshot) {
    let scheme = match snap.panels.first().map(|p| p.path.scheme) {
        Some(s) => s.as_uri(),
        None => "?",
    };
    let acl = if snap.acl.is_empty() {
        "host access DENIED (grant with g/w)".to_string()
    } else {
        format!("host ACL: {}", snap.acl.join(", "))
    };
    let line = Line::from(vec![
        Span::styled(
            " AIOS File Manager ",
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ),
        Span::raw(format!("  {scheme}  ")),
        Span::styled(acl, Style::default().fg(Color::Yellow)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_panel(frame: &mut Frame, area: Rect, panel: &PanelState, active: bool, rows: usize) {
    let mut p = panel.clone();
    p.clamp_cursor(rows);
    let title = format!(" {} {} ", p.side.name(), p.path.to_uri());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(if active {
            BorderType::Thick
        } else {
            BorderType::Plain
        })
        .border_style(Style::default().fg(if active { Color::Cyan } else { Color::DarkGray }))
        .title(Line::from(Span::styled(
            title,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )));

    let items: Vec<ListItem> = p
        .entries
        .iter()
        .enumerate()
        .skip(p.offset)
        .map(|(i, e)| {
            let name = if e.is_dir {
                format!("{}/", e.name)
            } else {
                e.name.clone()
            };
            let size = human_size(e.size);
            let acl_tag = if e.acl.is_empty() {
                String::new()
            } else {
                format!(" [{}]", e.acl.join(","))
            };
            let text = format!("{name:<36} {size:>10}{acl_tag}");
            let style = if i == p.cursor {
                Style::default()
                    .bg(Color::Cyan)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            } else if e.is_dir {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(Line::styled(text, style))
        })
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn draw_footer(frame: &mut Frame, area: Rect, snap: &FmSnapshot) {
    let mut lines: Vec<Line> = snap
        .jobs
        .iter()
        .take(2)
        .map(|j| {
            let (color, tag) = match j.status {
                JobStatus::Running => (Color::Cyan, "RUN"),
                JobStatus::Done => (Color::Green, "OK"),
                JobStatus::Failed => (Color::Red, "ERR"),
                JobStatus::Canceled => (Color::Yellow, "CANCEL"),
            };
            let bar = progress_bar(j.progress.fraction(), 24);
            let msg = j.error.clone().unwrap_or_default();
            Line::from(vec![
                Span::styled(
                    format!(" [{tag}] "),
                    Style::default().fg(Color::Black).bg(color),
                ),
                Span::raw(format!(" {:<32} ", j.label)),
                Span::raw(format!("{bar} {:.0}%", j.percent())),
                if msg.is_empty() {
                    Span::raw("")
                } else {
                    Span::styled(format!("  {msg}"), Style::default().fg(Color::Red))
                },
            ])
        })
        .collect();

    let hotkeys = Line::from(vec![
        Span::styled(
            " Tab:swap ",
            Style::default().fg(Color::Black).bg(Color::DarkGray),
        ),
        Span::raw(" Enter:open "),
        Span::styled(
            " F3:AI ",
            Style::default().fg(Color::Black).bg(Color::DarkGray),
        ),
        Span::raw(" F5:copy "),
        Span::styled(
            " F6:move ",
            Style::default().fg(Color::Black).bg(Color::DarkGray),
        ),
        Span::raw(" F7:mkdir "),
        Span::styled(
            " F8:del ",
            Style::default().fg(Color::Black).bg(Color::DarkGray),
        ),
        Span::raw(" F2:ren F9:sort r:refresh g/w:host-ACL Esc:close"),
    ]);
    lines.push(hotkeys);

    let para = Paragraph::new(lines);
    frame.render_widget(para, area);
}

/// Render a `[####....]` progress bar `width` characters wide.
pub fn progress_bar(fraction: f64, width: usize) -> String {
    let width = width.max(1);
    let filled = (fraction * width as f64).round() as usize;
    let filled = filled.min(width);
    let mut s = String::with_capacity(width);
    for i in 0..width {
        s.push(if i < filled { '#' } else { '.' });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new_with_kind(code, modifiers, KeyEventKind::Press)
    }

    #[test]
    fn test_progress_bar_bounds() {
        assert_eq!(progress_bar(0.0, 4), "....");
        assert_eq!(progress_bar(0.5, 4), "##..");
        assert_eq!(progress_bar(1.0, 4), "####");
        assert_eq!(progress_bar(2.0, 4), "####");
    }

    #[test]
    fn test_key_mapping() {
        assert_eq!(
            key_to_action(key(KeyCode::F(5), KeyModifiers::NONE), PanelSide::Left),
            Some(TuiAction::CopySelected)
        );
        assert_eq!(
            key_to_action(key(KeyCode::Char('a'), KeyModifiers::ALT), PanelSide::Right),
            Some(TuiAction::ViewSelected)
        );
        assert_eq!(
            key_to_action(key(KeyCode::Tab, KeyModifiers::NONE), PanelSide::Left),
            Some(TuiAction::SwitchPanel)
        );
        assert_eq!(
            key_to_action(key(KeyCode::Esc, KeyModifiers::NONE), PanelSide::Left),
            Some(TuiAction::Close)
        );
    }
}
