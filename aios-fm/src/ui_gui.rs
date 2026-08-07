use crate::engine::{FmSnapshot, JobStatus};
use crate::state::{human_size, PanelState};

/// Color palette for the GUI file-manager. Mirrors `aios_gui::AiosTheme` so
/// the tab looks native inside the dashboard.
#[derive(Debug, Clone)]
pub struct FmTheme {
    pub text: egui::Color32,
    pub muted: egui::Color32,
    pub accent: egui::Color32,
    pub danger: egui::Color32,
    pub ok: egui::Color32,
    pub selected_bg: egui::Color32,
}

impl Default for FmTheme {
    fn default() -> Self {
        Self {
            text: egui::Color32::from_rgb(220, 220, 220),
            muted: egui::Color32::from_rgb(120, 120, 120),
            accent: egui::Color32::from_rgb(90, 170, 255),
            danger: egui::Color32::from_rgb(255, 90, 90),
            ok: egui::Color32::from_rgb(90, 220, 130),
            selected_bg: egui::Color32::from_rgb(30, 50, 70),
        }
    }
}

/// A click on an entry inside a panel, reported by [`show`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FmClick {
    /// Panel index (`0` = left, `1` = right).
    pub panel: usize,
    /// Entry row index inside the panel.
    pub entry: usize,
    /// True when the row was double-clicked (activate), false on single click.
    pub double: bool,
}

/// Render the two-panel file manager into an `egui::Ui`.
///
/// Renders the engine snapshot: two synchronized panels, the active-panel
/// highlight, a jobs/progress section and the current capability ACL.
/// Returns the entry clicked by the user, if any.
pub fn show(ui: &mut egui::Ui, snap: &FmSnapshot, theme: &FmTheme) -> Option<FmClick> {
    let mut clicked = None;
    ui.columns(2, |cols| {
        for (i, panel) in snap.panels.iter().enumerate() {
            let hit = show_panel(&mut cols[i], panel, i == snap.active, theme);
            if let Some((entry, double)) = hit {
                clicked = Some(FmClick {
                    panel: i,
                    entry,
                    double,
                });
            }
        }
    });
    ui.separator();
    show_acl(ui, snap, theme);
    ui.separator();
    show_jobs(ui, snap, theme);
    clicked
}

fn show_panel(
    ui: &mut egui::Ui,
    panel: &PanelState,
    active: bool,
    theme: &FmTheme,
) -> Option<(usize, bool)> {
    let mut clicked = None;
    let title = format!("{}  {}", panel.side.name(), panel.path.to_uri());
    egui::Frame::group(ui.style())
        .fill(egui::Color32::from_rgb(18, 20, 24))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(title)
                    .color(if active { theme.accent } else { theme.text })
                    .strong(),
            );
            ui.add_space(2.0);
            egui::ScrollArea::vertical()
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    let mut clamp = panel.clone();
                    clamp.clamp_cursor(ui.available_height() as usize / 16 + 1);
                    for (i, e) in clamp.entries.iter().enumerate() {
                        let selected = i == clamp.cursor;
                        let name = if e.is_dir {
                            format!("{}/", e.name)
                        } else {
                            e.name.clone()
                        };
                        let size = human_size(e.size);
                        let acl_tag = if e.acl.is_empty() {
                            String::new()
                        } else {
                            format!("  [{}]", e.acl.join(","))
                        };
                        let text = format!("{name}  {size}{acl_tag}");
                        let color = if e.is_dir { theme.accent } else { theme.text };
                        let rich = egui::RichText::new(text).color(color);
                        let resp = ui.add(
                            egui::Label::new(rich.clone())
                                .sense(egui::Sense::click())
                                .selectable(false),
                        );
                        if selected {
                            ui.painter()
                                .rect_filled(resp.rect.expand(1.0), 4.0, theme.selected_bg);
                            ui.put(resp.rect, egui::Label::new(rich));
                        }
                        if resp.clicked() {
                            clicked = Some((i, false));
                        }
                        if resp.double_clicked() {
                            clicked = Some((i, true));
                        }
                    }
                });
        });
    clicked
}

fn show_acl(ui: &mut egui::Ui, snap: &FmSnapshot, theme: &FmTheme) {
    if snap.acl.is_empty() {
        ui.label(
            egui::RichText::new("HOST access: DENIED — grant tokens via the toolbar (g / w).")
                .color(theme.danger)
                .small(),
        );
    } else {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("HOST ACL:").small().color(theme.muted));
            for token in &snap.acl {
                ui.label(
                    egui::RichText::new(format!(" {token} "))
                        .color(theme.ok)
                        .small(),
                );
            }
        });
    }
}

fn show_jobs(ui: &mut egui::Ui, snap: &FmSnapshot, theme: &FmTheme) {
    for job in &snap.jobs {
        let color = match job.status {
            JobStatus::Running => theme.accent,
            JobStatus::Done => theme.ok,
            JobStatus::Failed => theme.danger,
            JobStatus::Canceled => theme.muted,
        };
        ui.horizontal(|ui| {
            ui.add(
                egui::ProgressBar::new(job.progress.fraction() as f32)
                    .desired_width(ui.available_width() * 0.45)
                    .text(format!("{:.0}%", job.percent())),
            );
            ui.label(egui::RichText::new(&job.label).color(color));
        });
        if let Some(err) = &job.error {
            ui.label(
                egui::RichText::new(format!("  {err}"))
                    .color(theme.danger)
                    .small(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_theme_defaults() {
        let theme = FmTheme::default();
        assert_eq!(theme.text, egui::Color32::from_rgb(220, 220, 220));
        assert_ne!(theme.accent, theme.danger);
    }
}
