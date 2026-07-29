use crate::app::AiosApp;
use crate::theme::AiosTheme;
use crate::widgets::section::section;
use crate::widgets::sparkline::{progress_bar, sparkline};

pub fn show(ui: &mut egui::Ui, app: &AiosApp, theme: &AiosTheme) {
    let ram_pct = if app.ram_total > 0 {
        app.ram_used as f32 / app.ram_total as f32
    } else {
        0.0
    };

    section(ui, theme, "RAM", |ui| {
        progress_bar(ui, theme, ram_pct, ui.available_width(), 28.0);
        ui.add_space(4.0);
        let data: Vec<f32> = app.ram_history.to_vec();
        let color = if ram_pct > 0.85 {
            theme.danger
        } else if ram_pct > 0.6 {
            theme.warning
        } else {
            theme.success
        };
        sparkline(ui, theme, &data, ui.available_width(), 80.0, color);
    });

    ui.add_space(8.0);

    section(ui, theme, "Process Priority Distribution", |ui| {
        let mut counts = [0u32; 5];
        for p in &app.processes {
            match p.priority.as_str() {
                "Critical" => counts[4] += 1,
                "High" => counts[3] += 1,
                "Normal" => counts[2] += 1,
                "Low" => counts[1] += 1,
                "Background" => counts[0] += 1,
                _ => {}
            }
        }

        let total = app.processes.len() as f32;
        let labels = ["Background", "Low", "Normal", "High", "Critical"];
        let colors = [
            theme.muted,
            theme.info,
            theme.success,
            theme.warning,
            theme.danger,
        ];

        for (i, (label, color)) in labels.iter().zip(colors.iter()).enumerate() {
            let frac = if total > 0.0 {
                counts[i] as f32 / total
            } else {
                0.0
            };
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("{label}: {}", counts[i]))
                        .color(*color)
                        .size(12.0),
                );
                if total > 0.0 {
                    progress_bar(ui, theme, frac, 200.0, 18.0);
                }
            });
        }
    });

    ui.add_space(8.0);

    section(ui, theme, "Block Statistics", |ui| {
        let total_blocks = app.blocks.len();
        let mut state_counts = std::collections::HashMap::new();
        for b in &app.blocks {
            *state_counts.entry(b.state.clone()).or_insert(0u32) += 1;
        }

        ui.label(
            egui::RichText::new(format!("Total: {total_blocks} blocks"))
                .color(theme.text)
                .size(12.0),
        );

        ui.add_space(4.0);

        for (state, count) in &state_counts {
            let color = theme.state_color(state);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("  {state}: {count}"))
                        .color(color)
                        .size(11.0),
                );
                let frac = *count as f32 / total_blocks.max(1) as f32;
                progress_bar(ui, theme, frac, 200.0, 14.0);
            });
        }
    });

    ui.add_space(8.0);

    section(ui, theme, "System Info", |ui| {
        ui.label(
            egui::RichText::new(format!("CPU: {}", app.hardware.cpu.model))
                .color(theme.text)
                .size(12.0),
        );
        ui.label(
            egui::RichText::new(format!(
                "Cores: {}  Threads: {}",
                app.hardware.cpu.cores, app.hardware.cpu.threads
            ))
            .color(theme.text_dim)
            .size(11.0),
        );
        if let Some(ref gpu) = app.hardware.gpu {
            ui.label(
                egui::RichText::new(format!("GPU: {} ({} MB)", gpu.name, gpu.vram_mb))
                    .color(theme.text)
                    .size(12.0),
            );
        }
        ui.label(
            egui::RichText::new(format!(
                "Storage: {} device(s)",
                app.hardware.storage_devices.len()
            ))
            .color(theme.text_dim)
            .size(11.0),
        );
    });
}
