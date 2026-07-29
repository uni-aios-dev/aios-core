use crate::theme::AiosTheme;

pub fn section(
    ui: &mut egui::Ui,
    theme: &AiosTheme,
    title: &str,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(title)
                .color(theme.accent)
                .size(14.0)
                .strong(),
        );
        ui.add_space(4.0);
        let available_width = ui.available_width();
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(available_width, 1.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 0.0, theme.border);
    });
    ui.add_space(4.0);

    egui::Frame::new()
        .fill(theme.surface_alt)
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| add_contents(ui));
}
