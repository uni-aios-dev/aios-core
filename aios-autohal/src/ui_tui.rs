use crate::engine::{DeviceView, DriverState, Severity, Toast, ToastKind};
use aios_security::capability::Capability;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Widget};

/// Shared title shown by the TUI tab and the GUI panel.
pub const HARDWARE_INSPECTOR_TITLE: &str = "Hardware Inspector";

fn cap_short(cap: &Capability) -> &'static str {
    cap.name().strip_prefix("CAP_").unwrap_or(cap.name())
}

/// Capability summary shown verbatim by both UIs, e.g. `HW_ACCESS/MEM_ALLOC`.
pub fn caps_summary(caps: &[Capability]) -> String {
    if caps.is_empty() {
        return "none".to_string();
    }
    caps.iter().map(cap_short).collect::<Vec<_>>().join("/")
}

/// Map a driver state to a TUI color.
pub fn status_style(state: DriverState) -> Style {
    match state.severity() {
        Severity::Good => Style::default().fg(Color::Green),
        Severity::Busy => Style::default().fg(Color::Yellow),
        Severity::Warn => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        Severity::Bad => Style::default().fg(Color::Red),
    }
}

/// Map a toast kind to a TUI color.
pub fn toast_style(kind: ToastKind) -> Style {
    match kind {
        ToastKind::Info => Style::default().fg(Color::Cyan),
        ToastKind::Success => Style::default().fg(Color::Green),
        ToastKind::Warn => Style::default().fg(Color::Yellow),
        ToastKind::Error => Style::default().fg(Color::Red),
    }
}

fn bus_order(label: &str) -> u8 {
    match label {
        "USB" => 0,
        "PCI" => 1,
        "NVMe" => 2,
        "Bluetooth" => 3,
        "ACPI" => 4,
        _ => 5,
    }
}

/// Ratatui widget rendering the device tree (grouped by bus) plus a hot-plug
/// toast strip. Renders the same [`DeviceView`] data as the egui panel.
pub struct HardwareInspector<'a> {
    pub devices: &'a [DeviceView],
    pub toasts: &'a [Toast],
    /// Index of the selected device row (including bus header rows).
    pub selected: Option<usize>,
    pub title: &'a str,
}

impl Widget for HardwareInspector<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let toast_height = (self.toasts.len() as u16).min(4) + 1;
        let (table_area, toast_area) = if toast_height > 1 {
            let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(toast_height)])
                .split(area);
            (chunks[0], chunks[1])
        } else {
            (area, Rect::default())
        };

        if self.devices.is_empty() {
            Paragraph::new("No devices detected. Run a hardware rescan to populate the tree.")
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(self.title.to_string()),
                )
                .render(table_area, buf);
        } else {
            render_device_table(self.devices, self.selected, self.title, table_area, buf);
        }

        if toast_area.height > 0 && !self.toasts.is_empty() {
            render_toast_strip(self.toasts, toast_area, buf);
        }
    }
}

fn render_device_table(
    devices: &[DeviceView],
    selected: Option<usize>,
    title: &str,
    area: Rect,
    buf: &mut Buffer,
) {
    let mut grouped: Vec<(String, Vec<&DeviceView>)> = Vec::new();
    for dev in devices {
        let label = dev.fingerprint.bus.label().to_string();
        match grouped.iter_mut().find(|(name, _)| name == &label) {
            Some((_, list)) => list.push(dev),
            None => grouped.push((label, vec![dev])),
        }
    }
    grouped.sort_by_key(|a| bus_order(&a.0));

    let mut rows: Vec<Row> = Vec::new();
    let mut idx = 0usize;
    for (bus_label, list) in &grouped {
        let header_row = Row::new(vec![Cell::from(Span::styled(
            format!(" {bus_label}"),
            Style::default().add_modifier(Modifier::BOLD),
        ))])
        .style(Style::default().bg(Color::DarkGray));
        rows.push(header_row);
        idx += 1;

        for dev in list {
            let caps = caps_summary(&dev.capabilities);
            let cells = vec![
                Cell::from(bus_label.clone()),
                Cell::from(dev.fingerprint.display_name()),
                Cell::from(format!(
                    "{:04X}:{:04X}",
                    dev.fingerprint.vendor_id, dev.fingerprint.device_id
                )),
                Cell::from(dev.driver_name.clone()),
                Cell::from(dev.source.clone().unwrap_or_default()),
                Cell::from(Span::styled(dev.state.label(), status_style(dev.state))),
                Cell::from(caps),
            ];
            let mut row = Row::new(cells);
            if Some(idx) == selected {
                row = row.style(Style::default().add_modifier(Modifier::REVERSED));
            }
            rows.push(row);
            idx += 1;
        }
    }

    let widths = [
        Constraint::Length(9),
        Constraint::Length(24),
        Constraint::Length(13),
        Constraint::Length(22),
        Constraint::Length(13),
        Constraint::Length(16),
        Constraint::Length(20),
    ];
    let header = Row::new(vec![
        Cell::from("Bus"),
        Cell::from("Device"),
        Cell::from("VID:PID"),
        Cell::from("Driver"),
        Cell::from("Source"),
        Cell::from("Status"),
        Cell::from("Capabilities"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));
    let table = Table::new(rows, widths).header(header).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title.to_string()),
    );
    table.render(area, buf);
}

fn render_toast_strip(toasts: &[Toast], area: Rect, buf: &mut Buffer) {
    let lines: Vec<Line> = toasts
        .iter()
        .rev()
        .take(4)
        .map(|t| Line::from(Span::styled(t.message.clone(), toast_style(t.kind))))
        .collect();
    Paragraph::new(lines)
        .block(Block::default().title(" Events "))
        .render(area, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::{BusType, HardwareFingerprint};
    use ratatui::{backend::TestBackend, Terminal};

    fn c270_view() -> DeviceView {
        DeviceView {
            fingerprint: HardwareFingerprint {
                bus: BusType::USB,
                vendor_id: 0x046D,
                device_id: 0x0825,
                class_code: 0,
                serial_or_acpi: None,
            },
            driver_id: "driver.usb.046d.0825".into(),
            driver_name: "Logitech C270 Webcam".into(),
            source: Some("Builtin".into()),
            state: DriverState::Active,
            failures: 0,
            progress: 100,
            capabilities: vec![Capability::HwAccess],
            last_error: None,
        }
    }

    #[test]
    fn test_caps_summary() {
        assert_eq!(caps_summary(&[]), "none");
        assert_eq!(
            caps_summary(&[Capability::HwAccess, Capability::MemAlloc]),
            "HW_ACCESS/MEM_ALLOC"
        );
    }

    #[test]
    fn test_state_labels_match_brief() {
        assert_eq!(DriverState::Active.label(), "Active");
        assert_eq!(DriverState::Downloading.label(), "Downloading...");
        assert_eq!(DriverState::Generic.label(), "Fallback/Generic");
    }

    #[test]
    fn test_inspector_renders_device_table() {
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let devices = vec![c270_view()];
        let toasts = vec![Toast {
            message: "[Hardware] Detected USB 046D:0825 -> looking up driver...".into(),
            kind: ToastKind::Info,
            created_ms: 0,
        }];

        terminal
            .draw(|f| {
                f.render_widget(
                    HardwareInspector {
                        devices: &devices,
                        toasts: &toasts,
                        selected: None,
                        title: HARDWARE_INSPECTOR_TITLE,
                    },
                    f.area(),
                );
            })
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        let content: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(content.contains("Hardware Inspector"));
        assert!(content.contains("USB"));
        assert!(content.contains("Logitech C270 Webcam"));
        assert!(content.contains("046D:0825"));
        assert!(content.contains("Active"));
        assert!(content.contains("HW_ACCESS"));
        assert!(content.contains("Events"));
    }

    #[test]
    fn test_inspector_empty_state() {
        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                f.render_widget(
                    HardwareInspector {
                        devices: &[],
                        toasts: &[],
                        selected: None,
                        title: HARDWARE_INSPECTOR_TITLE,
                    },
                    f.area(),
                );
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let content: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(content.contains("No devices detected"));
    }
}
