use crate::theme::AiosTheme;

pub fn stat_card(
    ui: &mut egui::Ui,
    theme: &AiosTheme,
    label: &str,
    value: &str,
    color: egui::Color32,
) {
    egui::Frame::new()
        .fill(theme.surface_alt)
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(16, 12))
        .stroke(egui::Stroke::new(1.0_f32, theme.border))
        .show(ui, |ui| {
            ui.set_min_width(140.0);
            ui.label(egui::RichText::new(label).color(theme.text_dim).size(11.0));
            ui.add_space(2.0);
            ui.label(egui::RichText::new(value).color(color).size(22.0).strong());
        });
}

pub fn stat_row(ui: &mut egui::Ui, theme: &AiosTheme, cards: &[(&str, &str, egui::Color32)]) {
    ui.horizontal(|ui| {
        for (label, value, color) in cards {
            stat_card(ui, theme, label, value, *color);
            ui.add_space(8.0);
        }
    });
}
