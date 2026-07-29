use crate::app::AiosApp;
use crate::theme::AiosTheme;
use crate::widgets::section::section;
use crate::widgets::sparkline::progress_bar;
use crate::widgets::sparkline::sparkline;
use crate::widgets::stat_card::stat_row;

pub fn show(ui: &mut egui::Ui, app: &AiosApp, theme: &AiosTheme) {
    let ram_pct = if app.ram_total > 0 {
        app.ram_used as f32 / app.ram_total as f32
    } else {
        0.0
    };
    let blocks = app.blocks.len();
    let procs = app.processes.len();

    stat_row(
        ui,
        theme,
        &[
            (
                "RAM",
                &format!("{}/{} MB", app.ram_used, app.ram_total),
                theme.info,
            ),
            ("Blocks", &blocks.to_string(), theme.success),
            ("Processes", &procs.to_string(), theme.success),
            (
                "Watchdog",
                match app.watchdog_state {
                    0 => "OK",
                    1 => "Suspended",
                    2 => "Recovering",
                    _ => "Safe Mode",
                },
                if app.watchdog_state == 0 {
                    theme.success
                } else {
                    theme.danger
                },
            ),
        ],
    );

    ui.add_space(8.0);

    ui.columns(2, |cols| {
        section(&mut cols[0], theme, "System", |ui| {
            ui.label(
                egui::RichText::new(&app.hardware.cpu.model)
                    .color(theme.text)
                    .size(13.0),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("CPU Cores:").color(theme.text_dim));
                ui.label(egui::RichText::new(app.hardware.cpu.cores.to_string()).color(theme.text));
                ui.add_space(12.0);
                ui.label(egui::RichText::new("Threads:").color(theme.text_dim));
                ui.label(
                    egui::RichText::new(app.hardware.cpu.threads.to_string()).color(theme.text),
                );
            });
            ui.horizontal(|ui| {
                let features = [
                    ("AVX2", app.hardware.cpu.has_avx2),
                    ("AVX-512", app.hardware.cpu.has_avx512),
                    ("SSE4.2", app.hardware.cpu.has_sse42),
                ];
                for (name, has) in features {
                    let color = if has { theme.success } else { theme.muted };
                    let mark = if has { "\u{2713}" } else { "\u{2717}" };
                    ui.label(egui::RichText::new(format!("{mark} {name}")).color(color));
                }
            });
            ui.add_space(4.0);
            if let Some(ref gpu) = app.hardware.gpu {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("GPU:").color(theme.text_dim));
                    ui.label(
                        egui::RichText::new(format!("{} ({} MB VRAM)", gpu.name, gpu.vram_mb))
                            .color(theme.text),
                    );
                });
            } else {
                ui.label(egui::RichText::new("GPU: None detected").color(theme.muted));
            }
            if !app.hardware.storage_devices.is_empty() {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Storage:").color(theme.text_dim));
                    ui.label(
                        egui::RichText::new(format!(
                            "{} device(s)",
                            app.hardware.storage_devices.len()
                        ))
                        .color(theme.text),
                    );
                });
            }
        });

        section(&mut cols[1], theme, "RAM Usage", |ui| {
            progress_bar(ui, theme, ram_pct, ui.available_width(), 24.0);
            ui.add_space(8.0);
            if !app.ram_history.is_empty() {
                let data: Vec<f32> = app.ram_history.to_vec();
                let color = if ram_pct > 0.85 {
                    theme.danger
                } else if ram_pct > 0.6 {
                    theme.warning
                } else {
                    theme.success
                };
                sparkline(ui, theme, &data, ui.available_width(), 60.0, color);
            }
        });
    });

    ui.add_space(8.0);

    section(ui, theme, "Activity Log", |ui| {
        let log_height = (ui.available_height()).min(200.0);
        egui::ScrollArea::vertical()
            .max_height(log_height)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for msg in app.log_messages.iter().rev().take(50) {
                    let color = if msg.contains("error") || msg.contains("crash") {
                        theme.danger
                    } else if msg.contains("warn") {
                        theme.warning
                    } else if msg.contains("success") || msg.contains("loaded") {
                        theme.success
                    } else {
                        theme.text_dim
                    };
                    ui.label(egui::RichText::new(msg).color(color).monospace().size(11.0));
                }
            });
    });
}
