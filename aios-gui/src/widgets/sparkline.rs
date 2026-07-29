use crate::theme::AiosTheme;

pub fn sparkline(
    ui: &mut egui::Ui,
    theme: &AiosTheme,
    data: &[f32],
    width: f32,
    height: f32,
    color: egui::Color32,
) {
    let (rect, _response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    ui.painter().rect_filled(rect, 4.0, theme.surface_alt);

    if data.len() < 2 {
        return;
    }

    let min = data.iter().copied().fold(f32::INFINITY, f32::min);
    let max = data.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let range = (max - min).max(1.0);

    let points: Vec<egui::Pos2> = data
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let x = rect.left() + (i as f32 / (data.len() - 1) as f32) * rect.width();
            let y = rect.bottom() - ((v - min) / range) * rect.height();
            egui::pos2(x, y)
        })
        .collect();

    for window in points.windows(2) {
        ui.painter()
            .line_segment([window[0], window[1]], egui::Stroke::new(2.0_f32, color));
    }

    if let Some(&last) = points.last() {
        ui.painter().circle_filled(last, 3.0, color);
    }
}

pub fn progress_bar(ui: &mut egui::Ui, theme: &AiosTheme, fraction: f32, width: f32, height: f32) {
    let (rect, _response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    ui.painter().rect_filled(rect, 4.0, theme.surface_alt);

    let fill_rect = egui::Rect::from_min_size(
        rect.min,
        egui::vec2(rect.width() * fraction.clamp(0.0, 1.0), rect.height()),
    );

    let color = if fraction > 0.85 {
        theme.danger
    } else if fraction > 0.6 {
        theme.warning
    } else {
        theme.success
    };

    ui.painter().rect_filled(fill_rect, 4.0, color);

    let text = format!("{:.0}%", fraction * 100.0);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(11.0),
        theme.text,
    );
}
