use crate::app::AiosApp;
use crate::theme::AiosTheme;
use crate::widgets::section::section;

pub fn show(ui: &mut egui::Ui, app: &mut AiosApp, theme: &AiosTheme) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Network Settings")
                .color(theme.accent)
                .size(14.0)
                .strong(),
        );
        ui.add_space(12.0);
        if ui
            .button(
                egui::RichText::new("\u{21bb} Reset")
                    .color(theme.warning)
                    .size(12.0),
            )
            .clicked()
        {
            app.net_reset();
        }
    });

    ui.add_space(4.0);

    ui.columns(2, |cols| {
        section(&mut cols[0], theme, "Connection", |ui| {
            ui.label(
                egui::RichText::new("Hostname")
                    .color(theme.text_dim)
                    .size(11.0),
            );
            ui.text_edit_singleline(&mut app.net_config.hostname);

            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Listen Port")
                    .color(theme.text_dim)
                    .size(11.0),
            );
            ui.add(
                egui::DragValue::new(&mut app.net_config.listen_port)
                    .range(1..=65535)
                    .speed(1),
            );

            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Connect Timeout (ms)")
                    .color(theme.text_dim)
                    .size(11.0),
            );
            ui.add(
                egui::DragValue::new(&mut app.net_config.connect_timeout_ms)
                    .range(100..=120_000)
                    .speed(100),
            );

            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Max Connections")
                    .color(theme.text_dim)
                    .size(11.0),
            );
            ui.add(
                egui::DragValue::new(&mut app.net_config.max_connections)
                    .range(1..=4096)
                    .speed(1),
            );

            ui.add_space(6.0);
            ui.checkbox(
                &mut app.net_config.allow_private_access,
                "Allow private/LAN access",
            );
        });

        section(&mut cols[1], theme, "DNS & User-Agent", |ui| {
            ui.label(
                egui::RichText::new("DNS Primary")
                    .color(theme.text_dim)
                    .size(11.0),
            );
            let mut primary = app.net_config.dns.primary.clone().unwrap_or_default();
            if ui.text_edit_singleline(&mut primary).changed() {
                app.net_config.dns.primary = if primary.trim().is_empty() {
                    None
                } else {
                    Some(primary.trim().to_string())
                };
            }

            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("DNS Secondary")
                    .color(theme.text_dim)
                    .size(11.0),
            );
            let mut secondary = app.net_config.dns.secondary.clone().unwrap_or_default();
            if ui.text_edit_singleline(&mut secondary).changed() {
                app.net_config.dns.secondary = if secondary.trim().is_empty() {
                    None
                } else {
                    Some(secondary.trim().to_string())
                };
            }

            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("User-Agent")
                    .color(theme.text_dim)
                    .size(11.0),
            );
            ui.add(
                egui::TextEdit::singleline(&mut app.net_config.user_agent)
                    .desired_width(f32::INFINITY),
            );
        });
    });

    ui.add_space(8.0);

    ui.horizontal(|ui| {
        if ui
            .button(
                egui::RichText::new("\u{2713} Save Configuration")
                    .color(theme.success)
                    .size(12.0),
            )
            .clicked()
        {
            app.net_save();
        }
        if let Some(ref status) = app.net_status {
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new(status)
                    .color(theme.info)
                    .size(11.0)
                    .monospace(),
            );
        }
    });

    ui.add_space(8.0);

    section(ui, theme, "Current Configuration (JSON)", |ui| {
        let json = app.net_config.to_json();
        egui::ScrollArea::vertical()
            .max_height(ui.available_height())
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(json)
                        .color(theme.text_dim)
                        .size(11.0)
                        .monospace(),
                );
            });
    });
}
