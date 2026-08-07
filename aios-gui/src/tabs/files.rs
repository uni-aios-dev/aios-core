use aios_fm::commands::Command;
use aios_fm::engine::FileManager;
use aios_fm::state::PanelSide;
use aios_fm::ui_tui::TuiAction;
use aios_vfs::ai_preview::AiLineKind;

use crate::app::{AiosApp, FmInput};
use crate::theme::AiosTheme;

pub fn show(ui: &mut egui::Ui, app: &mut AiosApp, theme: &AiosTheme) {
    let Some(fm) = app.fm.clone() else {
        ui.label(
            egui::RichText::new("File manager not initialized.")
                .color(theme.danger)
                .size(13.0),
        );
        return;
    };

    show_toolbar(ui, app, &fm, theme);
    ui.add_space(4.0);

    if app.fm_input.is_some() {
        show_input_modal(ui, app, &fm, theme);
        ui.add_space(4.0);
    }

    let snap = fm.snapshot();
    let fm_theme = fm_theme(theme);
    if let Some(click) = aios_fm::ui_gui::show(ui, &snap, &fm_theme) {
        handle_click(&fm, &snap, click);
    }

    if let Some(preview) = &app.fm_preview {
        ui.add_space(4.0);
        show_preview(ui, preview, theme);
    }

    if let Some(err) = &app.fm_error {
        ui.add_space(4.0);
        ui.colored_label(theme.danger, err);
    }
}

fn show_toolbar(ui: &mut egui::Ui, app: &mut AiosApp, fm: &FileManager, theme: &AiosTheme) {
    let side = fm.active_side();
    let has_selection = fm.selected(side).is_some();
    ui.horizontal_wrapped(|ui| {
        toolbar_button(ui, theme, "Refresh", || {
            app.fm_act(TuiAction::Refresh { side })
        });
        toolbar_button(ui, theme, "Switch", || app.fm_act(TuiAction::SwitchPanel));
        toolbar_button(ui, theme, "Sort", || {
            app.fm_act(TuiAction::ToggleSort { side })
        });
        toolbar_button(ui, theme, "Up", || app.fm_act(TuiAction::GoUp { side }));
        toolbar_button(ui, theme, "Mkdir", || app.fm_act(TuiAction::Mkdir { side }));
        if has_selection {
            toolbar_button(ui, theme, "Rename", || {
                app.fm_act(TuiAction::Rename { side })
            });
            toolbar_button(ui, theme, "View", || app.fm_act(TuiAction::ViewSelected));
            toolbar_button(ui, theme, "Copy", || app.fm_act(TuiAction::CopySelected));
            toolbar_button(ui, theme, "Move", || app.fm_act(TuiAction::MoveSelected));
            toolbar_button(ui, theme, "Delete", || {
                app.fm_act(TuiAction::DeleteSelected)
            });
        }
        toolbar_button(ui, theme, "HOST r", || app.fm_act(TuiAction::GrantHostRead));
        toolbar_button(ui, theme, "HOST w", || {
            app.fm_act(TuiAction::GrantHostWrite)
        });
    });
}

fn toolbar_button(ui: &mut egui::Ui, theme: &AiosTheme, label: &str, act: impl FnOnce()) {
    if ui
        .add(
            egui::Button::new(egui::RichText::new(label).color(theme.text).size(11.0))
                .fill(theme.button_bg)
                .corner_radius(4.0),
        )
        .clicked()
    {
        act();
    }
}

fn show_input_modal(ui: &mut egui::Ui, app: &mut AiosApp, fm: &FileManager, theme: &AiosTheme) {
    let mode = match &app.fm_input {
        Some(FmInput::Mkdir) => "New directory name",
        Some(FmInput::Rename) => "New name",
        None => return,
    };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(mode).color(theme.accent).size(12.0));
        let resp = ui.text_edit_singleline(&mut app.fm_input_buf);
        let submit = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
            || ui.button("OK").clicked();
        let cancel =
            ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
        if submit {
            app.fm_confirm_input();
        }
        if cancel {
            app.fm_input = None;
            app.fm_input_buf.clear();
        }
    });
    let _ = fm;
}

fn handle_click(
    fm: &FileManager,
    snap: &aios_fm::engine::FmSnapshot,
    click: aios_fm::ui_gui::FmClick,
) {
    let panel = &snap.panels[click.panel];
    let Some(entry) = panel.entries.get(click.entry) else {
        return;
    };
    let side = if click.panel == 0 {
        PanelSide::Left
    } else {
        PanelSide::Right
    };
    let path = panel.path.join(&entry.name);
    if click.double {
        if entry.is_dir {
            fm.send(Command::Navigate { side, path });
        } else {
            fm.send(Command::View { path });
        }
    } else {
        fm.set_active(click.panel);
        fm.set_cursor(side, click.entry);
    }
}

fn show_preview(ui: &mut egui::Ui, preview: &aios_vfs::ai_preview::AiPreview, theme: &AiosTheme) {
    egui::CollapsingHeader::new(
        egui::RichText::new(format!("AI Preview — {}", preview.title))
            .color(theme.accent)
            .size(13.0)
            .strong(),
    )
    .default_open(true)
    .show(ui, |ui| {
        egui::ScrollArea::vertical()
            .max_height(180.0)
            .show(ui, |ui| {
                for (kind, text) in &preview.lines {
                    let color = match kind {
                        AiLineKind::Info => theme.text,
                        AiLineKind::Success => theme.success,
                        AiLineKind::Warning => theme.warning,
                        AiLineKind::Error => theme.danger,
                        AiLineKind::Muted => theme.muted,
                    };
                    ui.colored_label(color, text);
                }
            });
    });
}

fn fm_theme(theme: &AiosTheme) -> aios_fm::ui_gui::FmTheme {
    aios_fm::ui_gui::FmTheme {
        text: theme.text,
        muted: theme.muted,
        accent: theme.accent,
        danger: theme.danger,
        ok: theme.success,
        selected_bg: theme.surface_alt,
    }
}
