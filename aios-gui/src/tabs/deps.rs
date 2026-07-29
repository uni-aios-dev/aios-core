use crate::app::AiosApp;
use crate::theme::AiosTheme;
use crate::widgets::section::section;

pub fn show(ui: &mut egui::Ui, app: &AiosApp, theme: &AiosTheme) {
    let dep_count = app.dep_edges.len();
    let block_count = app.dep_blocks.len();

    section(ui, theme, "Dependency Graph", |ui| {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("Blocks: {block_count}  |  Edges: {dep_count}"))
                    .color(theme.text)
                    .size(12.0),
            );
        });
    });

    ui.add_space(8.0);

    if !app.dep_load_order.is_empty() {
        section(ui, theme, "Load Order", |ui| {
            ui.horizontal_wrapped(|ui| {
                for (i, name) in app.dep_load_order.iter().enumerate() {
                    let is_last = i == app.dep_load_order.len() - 1;
                    ui.label(
                        egui::RichText::new(name)
                            .color(theme.accent)
                            .size(12.0)
                            .strong(),
                    );
                    if !is_last {
                        ui.label(
                            egui::RichText::new(" \u{2192} ")
                                .color(theme.muted)
                                .size(12.0),
                        );
                    }
                }
            });
        });

        ui.add_space(8.0);
    }

    section(ui, theme, "Block Dependencies", |ui| {
        let available = ui.available_height();
        egui::ScrollArea::vertical()
            .max_height(available)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let cols = ["Block", "Depends On", "Depended By"];
                    let widths = [140.0, 250.0, 250.0];
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

                ui.separator();

                for (i, name) in app.dep_blocks.iter().enumerate() {
                    let bg = if i % 2 == 0 {
                        theme.surface_alt
                    } else {
                        theme.surface
                    };

                    let deps_str = {
                        let block = app.blocks.iter().find(|b| &b.name == name);
                        match block {
                            Some(b) if !b.deps.is_empty() => b.deps.join(", "),
                            _ => "--".to_string(),
                        }
                    };
                    let dependents_str = {
                        let block = app.blocks.iter().find(|b| &b.name == name);
                        match block {
                            Some(b) if !b.dependents.is_empty() => b.dependents.join(", "),
                            _ => "--".to_string(),
                        }
                    };

                    egui::Frame::new()
                        .fill(bg)
                        .inner_margin(egui::Margin::symmetric(8, 2))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let widths = [140.0, 250.0, 250.0];
                                ui.allocate_ui(egui::vec2(widths[0], 20.0), |ui| {
                                    ui.label(
                                        egui::RichText::new(name)
                                            .color(theme.warning)
                                            .size(11.0)
                                            .strong(),
                                    );
                                });
                                ui.allocate_ui(egui::vec2(widths[1], 20.0), |ui| {
                                    ui.label(
                                        egui::RichText::new(&deps_str)
                                            .color(theme.success)
                                            .size(11.0),
                                    );
                                });
                                ui.allocate_ui(egui::vec2(widths[2], 20.0), |ui| {
                                    ui.label(
                                        egui::RichText::new(&dependents_str)
                                            .color(theme.info)
                                            .size(11.0),
                                    );
                                });
                            });
                        });
                }
            });
    });
}
