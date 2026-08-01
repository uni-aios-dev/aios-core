use crate::app::AiosApp;
use crate::theme::AiosTheme;
use crate::widgets::section::section;

pub fn show(ui: &mut egui::Ui, app: &mut AiosApp, theme: &AiosTheme) {
    section(ui, theme, "Web Browser", |ui| {
        ui.horizontal(|ui| {
            if ui
                .add(
                    egui::Button::new(egui::RichText::new("\u{25c0} Back").color(theme.text))
                        .corner_radius(4.0),
                )
                .clicked()
            {
                if let Err(e) = app.browser_back() {
                    app.browser_status = Some(e);
                }
            }
            if ui
                .add(
                    egui::Button::new(egui::RichText::new("Forward \u{25b6}").color(theme.text))
                        .corner_radius(4.0),
                )
                .clicked()
            {
                if let Err(e) = app.browser_forward() {
                    app.browser_status = Some(e);
                }
            }
            ui.separator();
            if app.browser_active() {
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("\u{2715} Close").color(theme.danger),
                        )
                        .corner_radius(4.0),
                    )
                    .clicked()
                {
                    app.close_browser();
                }
            } else if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("\u{1f310} Open Browser").color(theme.accent),
                    )
                    .corner_radius(4.0),
                )
                .clicked()
            {
                if let Err(e) = app.open_browser() {
                    app.browser_status = Some(e);
                }
            }
        });

        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Address").color(theme.text_dim));
            let edit = egui::TextEdit::singleline(&mut app.browser_addr)
                .hint_text("URL or search query")
                .desired_width(ui.available_width() - 60.0);
            let resp = ui.add(edit);
            let submitted = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if submitted {
                let input = app.browser_addr.clone();
                if !input.trim().is_empty() {
                    if let Err(e) = app.navigate_browser(&input) {
                        app.browser_status = Some(e);
                    }
                }
            }
        });

        ui.add_space(6.0);

        match &app.browser_status {
            Some(status) => {
                ui.label(egui::RichText::new(status).color(theme.info).size(12.0));
            }
            None => {
                ui.label(
                    egui::RichText::new(
                        "Browser closed. Enter a URL or search query above and press Enter to open it.",
                    )
                    .color(theme.muted)
                    .size(12.0),
                );
            }
        }
    });

    ui.add_space(8.0);

    section(ui, theme, "Controls", |ui| {
        ui.label(
            egui::RichText::new(
                "The browser runs the native engine (WebView2 / WebKitGTK / WKWebView) with full \
                 cookies and JavaScript support.",
            )
            .color(theme.text)
            .size(12.0),
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Type an address (e.g. example.com), a full http(s) URL, or a plain search query — \
                 Enter resolves and opens it.",
            )
            .color(theme.text_dim)
            .size(11.0),
        );
        ui.label(
            egui::RichText::new("F7 switches to this tab; W opens the GUI dashboard from the TUI.")
                .color(theme.text_dim)
                .size(11.0),
        );
    });
}
