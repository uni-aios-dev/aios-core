use crate::app::AiosApp;
use crate::theme::AiosTheme;
use crate::widgets::section::section;
use crate::widgets::sparkline::progress_bar;
use crate::widgets::sparkline::sparkline;
use crate::widgets::stat_card::stat_row;
use crate::widgets::status_badge::status_badge;

pub fn show(ui: &mut egui::Ui, app: &mut AiosApp, theme: &AiosTheme) {
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

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
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
                        ui.label(
                            egui::RichText::new(app.hardware.cpu.cores.to_string())
                                .color(theme.text),
                        );
                        ui.add_space(12.0);
                        ui.label(egui::RichText::new("Threads:").color(theme.text_dim));
                        ui.label(
                            egui::RichText::new(app.hardware.cpu.threads.to_string())
                                .color(theme.text),
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
                                egui::RichText::new(format!(
                                    "{} ({} MB VRAM)",
                                    gpu.name, gpu.vram_mb
                                ))
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

            ui.columns(2, |cols| {
                section(&mut cols[0], theme, "Process Priority Distribution", |ui| {
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
                                progress_bar(ui, theme, frac, 200.0, 16.0);
                            }
                        });
                    }
                });

                section(&mut cols[1], theme, "Block Statistics", |ui| {
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
            });

            ui.add_space(8.0);

            section(ui, theme, "Processes", |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("{} running", app.processes.len()))
                            .color(theme.text_dim)
                            .size(11.0),
                    );
                    if ui
                        .button(
                            egui::RichText::new("\u{21bb} Refresh")
                                .color(theme.accent)
                                .size(11.0),
                        )
                        .clicked()
                    {
                        app.refresh_processes();
                    }
                });
                ui.add_space(4.0);
                let proc_count = app.processes.len();
                let row_height = 24.0;
                let max_height = 260.0_f32.min(ui.available_height());
                egui::ScrollArea::vertical()
                    .max_height(max_height)
                    .show_rows(ui, row_height, proc_count, |ui, row_range| {
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
                                        ui.allocate_ui(egui::vec2(widths[0], 18.0), |ui| {
                                            ui.label(
                                                egui::RichText::new(p.pid.to_string())
                                                    .color(theme.text_dim)
                                                    .monospace()
                                                    .size(11.0),
                                            );
                                        });
                                        ui.allocate_ui(egui::vec2(widths[1], 18.0), |ui| {
                                            let resp = ui.selectable_label(
                                                is_selected,
                                                egui::RichText::new(&p.name)
                                                    .color(theme.text)
                                                    .size(11.0),
                                            );
                                            if resp.clicked() {
                                                app.selected_process_idx = Some(i);
                                            }
                                        });
                                        ui.allocate_ui(egui::vec2(widths[2], 18.0), |ui| {
                                            ui.label(
                                                egui::RichText::new(&p.priority)
                                                    .color(theme.priority_color(&p.priority))
                                                    .size(11.0),
                                            );
                                        });
                                        ui.allocate_ui(egui::vec2(widths[3], 18.0), |ui| {
                                            status_badge(ui, theme, &p.state);
                                        });
                                        ui.allocate_ui(egui::vec2(widths[4], 18.0), |ui| {
                                            ui.label(
                                                egui::RichText::new(format!("{}MB", p.ram_mb))
                                                    .color(theme.text)
                                                    .monospace()
                                                    .size(11.0),
                                            );
                                        });
                                        ui.allocate_ui(egui::vec2(widths[5], 18.0), |ui| {
                                            ui.label(
                                                egui::RichText::new(format!("{}ms", p.cpu_ms))
                                                    .color(theme.text)
                                                    .monospace()
                                                    .size(11.0),
                                            );
                                        });
                                        ui.allocate_ui(egui::vec2(widths[6], 18.0), |ui| {
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
        });
}
