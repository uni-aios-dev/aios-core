use crate::app::AiosApp;
use crate::theme::AiosTheme;
use crate::widgets::status_badge::status_badge;

pub fn show(ui: &mut egui::Ui, app: &mut AiosApp, theme: &AiosTheme) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Block Marketplace")
                .color(theme.accent)
                .size(14.0)
                .strong(),
        );
    });

    ui.add_space(4.0);

    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Search:")
                .color(theme.text_dim)
                .size(12.0),
        );
        let response = ui.text_edit_singleline(&mut app.marketplace_search);
        if response.changed() {
            app.search_marketplace();
        }
    });

    ui.add_space(4.0);

    egui::TopBottomPanel::top("mp_header")
        .exact_height(28.0)
        .frame(
            egui::Frame::new()
                .fill(theme.surface_alt)
                .inner_margin(egui::Margin::symmetric(8, 4)),
        )
        .show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                let cols = ["Name", "Version", "Author", "Status", "Downloads"];
                let widths = [140.0, 80.0, 120.0, 120.0, 80.0];
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
    let visible: Vec<_> = if app.marketplace_search.is_empty() {
        app.marketplace_entries.iter().collect()
    } else {
        let q = app.marketplace_search.to_lowercase();
        app.marketplace_entries
            .iter()
            .filter(|e| {
                e.name.to_lowercase().contains(&q)
                    || e.description.to_lowercase().contains(&q)
                    || e.tags.iter().any(|t| t.to_lowercase().contains(&q))
            })
            .collect()
    };

    let available = ui.available_height() - 50.0;

    egui::ScrollArea::vertical()
        .max_height(available)
        .show_rows(ui, row_height, visible.len(), |ui, row_range| {
            for i in row_range {
                let e = visible[i];
                let is_selected = app.selected_marketplace_idx == Some(i);
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
                            let widths = [140.0, 80.0, 120.0, 120.0, 80.0];
                            ui.allocate_ui(egui::vec2(widths[0], 20.0), |ui| {
                                let resp = ui.selectable_label(
                                    is_selected,
                                    egui::RichText::new(&e.name)
                                        .color(theme.text)
                                        .size(11.0)
                                        .strong(),
                                );
                                if resp.clicked() {
                                    app.selected_marketplace_idx = Some(i);
                                }
                            });
                            ui.allocate_ui(egui::vec2(widths[1], 20.0), |ui| {
                                ui.label(
                                    egui::RichText::new(&e.version)
                                        .color(theme.text_dim)
                                        .size(11.0),
                                );
                            });
                            ui.allocate_ui(egui::vec2(widths[2], 20.0), |ui| {
                                ui.label(
                                    egui::RichText::new(&e.author).color(theme.text).size(11.0),
                                );
                            });
                            ui.allocate_ui(egui::vec2(widths[3], 20.0), |ui| {
                                status_badge(ui, theme, &e.status);
                            });
                            ui.allocate_ui(egui::vec2(widths[4], 20.0), |ui| {
                                ui.label(
                                    egui::RichText::new(e.downloads.to_string())
                                        .color(theme.text)
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
                egui::RichText::new("\u{2b07} Install")
                    .color(theme.success)
                    .size(12.0),
            )
            .clicked()
        {
            if let Some(idx) = app.selected_marketplace_idx {
                let name = app.marketplace_entries[idx].name.clone();
                app.install_block(name);
            }
        }
        if ui
            .button(
                egui::RichText::new("\u{21bb} Update")
                    .color(theme.info)
                    .size(12.0),
            )
            .clicked()
        {
            if let Some(idx) = app.selected_marketplace_idx {
                let name = app.marketplace_entries[idx].name.clone();
                app.update_block(name);
            }
        }
        if ui
            .button(
                egui::RichText::new("\u{2717} Uninstall")
                    .color(theme.danger)
                    .size(12.0),
            )
            .clicked()
        {
            if let Some(idx) = app.selected_marketplace_idx {
                let name = app.marketplace_entries[idx].name.clone();
                app.uninstall_block(name);
            }
        }
    });

    if let Some(ref msg) = app.marketplace_status {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(msg).color(theme.info).size(11.0));
    }
}
