use crate::app::AiosApp;
use crate::theme::AiosTheme;
use crate::widgets::status_badge::status_badge;

pub fn show(ui: &mut egui::Ui, app: &mut AiosApp, theme: &AiosTheme) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("Processes ({})", app.processes.len()))
                .color(theme.accent)
                .size(14.0)
                .strong(),
        );
        ui.add_space(12.0);
        if ui
            .button(
                egui::RichText::new("\u{21bb} Refresh")
                    .color(theme.accent)
                    .size(12.0),
            )
            .clicked()
        {
            app.refresh_processes();
        }
    });

    ui.add_space(4.0);

    let header_height = 28.0;
    egui::TopBottomPanel::top("process_header")
        .exact_height(header_height)
        .frame(
            egui::Frame::new()
                .fill(theme.surface_alt)
                .inner_margin(egui::Margin::symmetric(8, 4)),
        )
        .show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                let cols = ["PID", "Name", "Priority", "State", "RAM", "CPU", "Crashes"];
                let widths = [60.0, 140.0, 80.0, 80.0, 70.0, 70.0, 60.0];
                for (col, w) in cols.iter().zip(widths.iter()) {
                    ui.allocate_ui(egui::vec2(*w, 20.0), |ui| {
                        ui.label(
                            egui::RichText::new(*col)
                                .color(theme.accent)
                                .size(11.0)
                                .strong(),
                        );
                    });
                }
            });
        });

    let row_height = 26.0;
    let available = ui.available_height();

    egui::ScrollArea::vertical()
        .max_height(available)
        .show_rows(ui, row_height, app.processes.len(), |ui, row_range| {
            for i in row_range {
                let p = &app.processes[i];
                let is_selected = app.selected_process_idx == Some(i);
                let bg = if is_selected {
                    theme.accent.linear_multiply(0.15)
                } else if i % 2 == 0 {
                    theme.surface_alt
                } else {
                    theme.surface
                };

                egui::Frame::new()
                    .fill(bg)
                    .inner_margin(egui::Margin::symmetric(8, 2))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let widths = [60.0, 140.0, 80.0, 80.0, 70.0, 70.0, 60.0];

                            ui.allocate_ui(egui::vec2(widths[0], 20.0), |ui| {
                                ui.label(
                                    egui::RichText::new(p.pid.to_string())
                                        .color(theme.text_dim)
                                        .monospace()
                                        .size(11.0),
                                );
                            });
                            ui.allocate_ui(egui::vec2(widths[1], 20.0), |ui| {
                                let resp = ui.selectable_label(
                                    is_selected,
                                    egui::RichText::new(&p.name).color(theme.text).size(11.0),
                                );
                                if resp.clicked() {
                                    app.selected_process_idx = Some(i);
                                }
                            });
                            ui.allocate_ui(egui::vec2(widths[2], 20.0), |ui| {
                                let color = theme.priority_color(&p.priority);
                                ui.label(egui::RichText::new(&p.priority).color(color).size(11.0));
                            });
                            ui.allocate_ui(egui::vec2(widths[3], 20.0), |ui| {
                                status_badge(ui, theme, &p.state);
                            });
                            ui.allocate_ui(egui::vec2(widths[4], 20.0), |ui| {
                                ui.label(
                                    egui::RichText::new(format!("{}MB", p.ram_mb))
                                        .color(theme.text)
                                        .monospace()
                                        .size(11.0),
                                );
                            });
                            ui.allocate_ui(egui::vec2(widths[5], 20.0), |ui| {
                                ui.label(
                                    egui::RichText::new(format!("{}ms", p.cpu_ms))
                                        .color(theme.text)
                                        .monospace()
                                        .size(11.0),
                                );
                            });
                            ui.allocate_ui(egui::vec2(widths[6], 20.0), |ui| {
                                let color = if p.crashes > 0 {
                                    theme.danger
                                } else {
                                    theme.success
                                };
                                ui.label(
                                    egui::RichText::new(p.crashes.to_string())
                                        .color(color)
                                        .monospace()
                                        .size(11.0),
                                );
                            });
                        });
                    });
            }
        });

    ui.add_space(4.0);

    ui.horizontal(|ui| {
        if ui
            .button(
                egui::RichText::new("\u{2717} Kill Selected")
                    .color(theme.danger)
                    .size(12.0),
            )
            .clicked()
        {
            if let Some(idx) = app.selected_process_idx {
                let pid = app.processes[idx].pid;
                app.kill_process(pid);
            }
        }
        if ui
            .button(
                egui::RichText::new("\u{23f8} Suspend")
                    .color(theme.warning)
                    .size(12.0),
            )
            .clicked()
        {
            if let Some(idx) = app.selected_process_idx {
                let pid = app.processes[idx].pid;
                app.suspend_process(pid);
            }
        }
        if ui
            .button(
                egui::RichText::new("\u{25b6} Resume")
                    .color(theme.success)
                    .size(12.0),
            )
            .clicked()
        {
            if let Some(idx) = app.selected_process_idx {
                let pid = app.processes[idx].pid;
                app.resume_process(pid);
            }
        }
        if let Some(idx) = app.selected_process_idx {
            let p = &app.processes[idx];
            ui.add_space(20.0);
            ui.label(
                egui::RichText::new(format!(
                    "Selected: {} (PID {}) — Priority: {} — State: {}",
                    p.name, p.pid, p.priority, p.state
                ))
                .color(theme.text_dim)
                .size(11.0),
            );
        }
    });
}
