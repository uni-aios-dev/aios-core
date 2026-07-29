use crate::theme::AiosTheme;

pub fn badge(ui: &mut egui::Ui, _theme: &AiosTheme, text: &str, color: egui::Color32) {
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_string(), egui::FontId::proportional(11.0), color);
    let rect = galley.rect;
    let size = egui::vec2(rect.width() + 12.0, rect.height() + 6.0);
    let (rect, _response) = ui.allocate_exact_size(size, egui::Sense::hover());
    let bg = egui::Color32::from_rgba_premultiplied(color.r(), color.g(), color.b(), 30);
    ui.painter().rect_filled(rect, 4.0, bg);
    ui.painter().rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0_f32, color),
        egui::StrokeKind::Inside,
    );
    ui.painter().galley(
        egui::pos2(rect.left() + 6.0, rect.top() + 3.0),
        galley,
        color,
    );
}

pub fn status_badge(ui: &mut egui::Ui, theme: &AiosTheme, status: &str) {
    let color = theme.state_color(status);
    badge(ui, theme, status, color);
}
