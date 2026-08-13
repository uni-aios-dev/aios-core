use crate::app::AiosApp;
use crate::theme::AiosTheme;
use aios_autohal::ui_gui::HardwarePanel;
use egui::Ui;

/// Hardware & Drivers tab (F9): renders the shared `aios-autohal` panel over
/// the engine's `DeviceView`/`Toast` snapshots and feeds the emitted
/// [`GuiAction`]s back to the engine via `AiosApp::apply_hw_actions`.
pub fn show(ui: &mut Ui, app: &mut AiosApp, theme: &AiosTheme) {
    ui.add_space(4.0);
    ui.heading(
        egui::RichText::new("Hardware & Drivers")
            .color(theme.accent)
            .size(18.0),
    );
    ui.label(
        "Auto-provisioning: fingerprint -> fetch/adapt -> WASM sandbox with Capability tokens. \
         After 3 consecutive failures a device auto-rolls back to the Generic Fallback Driver.",
    );
    ui.separator();

    if app.hw_engine.is_none() {
        ui.colored_label(
            theme.danger,
            "Hardware engine unavailable. Restart the dashboard to re-initialize.",
        );
        return;
    }

    ui.horizontal(|ui| {
        if ui
            .add(egui::Button::new("Rescan").fill(theme.button_bg))
            .clicked()
        {
            if let Some(engine) = &mut app.hw_engine {
                engine.rescan(&app.hardware);
            }
            app.hw_refresh();
        }
    });
    ui.separator();

    // The panel borrows the view data immutably and returns actions; applying
    // them after the borrows end lets the engine mutate freely.
    let actions = {
        let mut panel = HardwarePanel {
            devices: &app.hw_views,
            toasts: &app.hw_toasts,
            enabled: true,
        };
        panel.show(ui)
    };
    app.apply_hw_actions(actions);
    app.hw_refresh();
}
