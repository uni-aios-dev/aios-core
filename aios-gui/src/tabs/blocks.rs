use crate::app::AiosApp;
use crate::theme::AiosTheme;
use crate::widgets::status_badge::status_badge;

pub fn show(ui: &mut egui::Ui, app: &mut AiosApp, theme: &AiosTheme) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("WASM Blocks ({})", app.blocks.len()))
                .color(theme.accent)
                .size(14.0)
                .strong(),
        );
        ui.add_space(8.0);
        if ui
            .button(
                egui::RichText::new("\u{21bb} Refresh")
                    .color(theme.accent)
                    .size(12.0),
            )
            .clicked()
        {
            app.refresh_blocks();
        }
        ui.add_space(8.0);
        if ui
            .button(
                egui::RichText::new("+ Load Block")
                    .color(theme.success)
                    .size(12.0),
            )
            .clicked()
        {
            app.show_load_dialog = true;
            app.load_name_buf.clear();
            app.load_version_buf.clear();
            app.load_step = 0;
        }
    });

    ui.add_space(4.0);

    let header_height = 28.0;
    egui::TopBottomPanel::top("block_header")
        .exact_height(header_height)
        .frame(
            egui::Frame::new()
                .fill(theme.surface_alt)
                .inner_margin(egui::Margin::symmetric(8, 4)),
        )
        .show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                let cols = ["ID", "Name", "Version", "State", "Size", "Dependencies"];
                let widths = [50.0, 120.0, 80.0, 100.0, 80.0, 200.0];
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
    let available = ui.available_height() - 50.0;

    egui::ScrollArea::vertical()
        .max_height(available)
        .show_rows(ui, row_height, app.blocks.len(), |ui, row_range| {
            for i in row_range {
                let b = &app.blocks[i];
                let is_selected = app.selected_block_idx == Some(i);
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
                            let widths = [50.0, 120.0, 80.0, 100.0, 80.0, 200.0];
                            ui.allocate_ui(egui::vec2(widths[0], 20.0), |ui| {
                                ui.label(
                                    egui::RichText::new(b.id.to_string())
                                        .color(theme.text_dim)
                                        .monospace()
                                        .size(11.0),
                                );
                            });
                            ui.allocate_ui(egui::vec2(widths[1], 20.0), |ui| {
                                let resp = ui.selectable_label(
                                    is_selected,
                                    egui::RichText::new(&b.name)
                                        .color(theme.text)
                                        .size(11.0)
                                        .strong(),
                                );
                                if resp.clicked() {
                                    app.selected_block_idx = Some(i);
                                }
                            });
                            ui.allocate_ui(egui::vec2(widths[2], 20.0), |ui| {
                                ui.label(
                                    egui::RichText::new(&b.version)
                                        .color(theme.text_dim)
                                        .size(11.0),
                                );
                            });
                            ui.allocate_ui(egui::vec2(widths[3], 20.0), |ui| {
                                status_badge(ui, theme, &b.state);
                            });
                            ui.allocate_ui(egui::vec2(widths[4], 20.0), |ui| {
                                let size_str = if b.size >= 1024 * 1024 {
                                    format!("{:.1} MB", b.size as f64 / 1048576.0)
                                } else if b.size >= 1024 {
                                    format!("{:.1} KB", b.size as f64 / 1024.0)
                                } else {
                                    format!("{} B", b.size)
                                };
                                ui.label(
                                    egui::RichText::new(size_str)
                                        .color(theme.text)
                                        .monospace()
                                        .size(11.0),
                                );
                            });
                            ui.allocate_ui(egui::vec2(widths[5], 20.0), |ui| {
                                let deps_str = if b.deps.is_empty() {
                                    "--".to_string()
                                } else {
                                    b.deps.join(", ")
                                };
                                ui.label(
                                    egui::RichText::new(deps_str)
                                        .color(theme.text_dim)
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
                egui::RichText::new("\u{2717} Unload")
                    .color(theme.danger)
                    .size(12.0),
            )
            .clicked()
        {
            if let Some(idx) = app.selected_block_idx {
                let id = app.blocks[idx].id;
                app.unload_block(id);
            }
        }
        if ui
            .button(
                egui::RichText::new("\u{1f504} Hot-Swap")
                    .color(theme.warning)
                    .size(12.0),
            )
            .clicked()
        {
            if let Some(_idx) = app.selected_block_idx {
                app.add_log("Hot-swap: select new binary to swap".into());
            }
        }
        if let Some(idx) = app.selected_block_idx {
            let b = &app.blocks[idx];
            ui.add_space(20.0);
            ui.label(
                egui::RichText::new(format!(
                    "Selected: {} v{} — {} — {}",
                    b.name, b.version, b.state, b.size
                ))
                .color(theme.text_dim)
                .size(11.0),
            );
        }
    });

    if app.show_load_dialog {
        egui::Window::new("Load Block")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(
                egui::Frame::new()
                    .fill(theme.surface)
                    .corner_radius(8.0)
                    .inner_margin(16.0)
                    .stroke(egui::Stroke::new(1.0_f32, theme.accent)),
            )
            .show(ui.ctx(), |ui| {
                if app.load_step == 0 {
                    ui.label(
                        egui::RichText::new("Block name:")
                            .color(theme.text)
                            .size(13.0),
                    );
                    let response = ui.text_edit_singleline(&mut app.load_name_buf);
                    response.request_focus();
                    ui.horizontal(|ui| {
                        if (ui
                            .button(egui::RichText::new("Next \u{2192}").color(theme.accent))
                            .clicked()
                            || response.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                            && !app.load_name_buf.trim().is_empty()
                        {
                            app.load_step = 1;
                            app.load_version_buf.clear();
                        }
                        if ui
                            .button(egui::RichText::new("Cancel").color(theme.muted))
                            .clicked()
                        {
                            app.show_load_dialog = false;
                        }
                    });
                } else {
                    ui.label(
                        egui::RichText::new(format!("Version for '{}':", app.load_name_buf))
                            .color(theme.text)
                            .size(13.0),
                    );
                    let response = ui.text_edit_singleline(&mut app.load_version_buf);
                    response.request_focus();
                    ui.horizontal(|ui| {
                        if ui
                            .button(egui::RichText::new("Load \u{2713}").color(theme.success))
                            .clicked()
                            || response.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            let name = app.load_name_buf.trim().to_string();
                            let version = app.load_version_buf.trim().to_string();
                            if !name.is_empty() && !version.is_empty() {
                                app.load_block(name, version);
                                app.show_load_dialog = false;
                            }
                        }
                        if ui
                            .button(egui::RichText::new("Cancel").color(theme.muted))
                            .clicked()
                        {
                            app.show_load_dialog = false;
                        }
                    });
                }
            });
    }
}
