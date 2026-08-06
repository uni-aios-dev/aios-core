use crate::app::AiosApp;
use crate::theme::AiosTheme;
use crate::widgets::section::section;

pub fn show(ui: &mut egui::Ui, app: &mut AiosApp, theme: &AiosTheme) {
    let cfg = app.ai_config.clone();
    let backend = match cfg.backend {
        aios_llm::BackendKind::Cloud(ref p) => format!("cloud/{}", aios_llm::provider_name(p)),
        aios_llm::BackendKind::MicroLocal => "local/micro".into(),
        aios_llm::BackendKind::FullLocal => "local/full".into(),
    };

    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("AI Studio")
                .color(theme.accent)
                .size(14.0)
                .strong(),
        );
        ui.add_space(12.0);
        let busy_color = if app.ai_busy {
            theme.warning
        } else {
            theme.success
        };
        ui.label(
            egui::RichText::new(format!(
                "{backend} | {} | temp {} | tokens {}",
                cfg.model, cfg.temperature, cfg.max_tokens
            ))
            .color(theme.text_dim)
            .size(11.0),
        );
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(if app.ai_busy {
                "streaming..."
            } else {
                &app.ai_status
            })
            .color(busy_color)
            .size(11.0),
        );
    });

    ui.add_space(4.0);

    let header = ui.available_height() - 60.0;
    section(ui, theme, "Conversation", |ui| {
        egui::ScrollArea::vertical()
            .max_height(header.max(80.0))
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in &app.ai_output {
                    let (color, text) = if line.starts_with('>') {
                        (theme.accent, format!("  {line}"))
                    } else if line.starts_with("[error]") {
                        (theme.danger, line.clone())
                    } else if line.starts_with("  /") {
                        (theme.info, line.clone())
                    } else {
                        (theme.text, line.clone())
                    };
                    ui.label(
                        egui::RichText::new(text)
                            .color(color)
                            .size(12.0)
                            .monospace(),
                    );
                }
                if app.ai_busy {
                    let partial = app.ai_stream.lock().unwrap().clone();
                    if partial.is_empty() {
                        ui.label(egui::RichText::new(" …").color(theme.warning).size(12.0));
                    } else {
                        ui.label(
                            egui::RichText::new(partial)
                                .color(theme.warning)
                                .size(12.0)
                                .monospace(),
                        );
                    }
                }
            });
    });

    ui.add_space(8.0);

    ui.horizontal(|ui| {
        let send_on_enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        let resp = ui.add_sized(
            egui::vec2(ui.available_width() - 110.0, 28.0),
            egui::TextEdit::singleline(&mut app.ai_input)
                .hint_text("Ask AIOS something...  (type /help for commands)")
                .desired_width(f32::INFINITY),
        );
        if send_on_enter
            && (resp.has_focus() || resp.lost_focus())
            && !app.ai_input.trim().is_empty()
        {
            app.ai_send();
        }
        let btn = egui::Button::new(
            egui::RichText::new(if app.ai_busy { "..." } else { "\u{27a4} Send" })
                .color(theme.accent)
                .size(12.0),
        )
        .fill(theme.button_bg)
        .min_size(egui::vec2(90.0, 28.0));
        if ui.add(btn).clicked() && !app.ai_busy {
            app.ai_send();
        }
    });

    ui.add_space(4.0);

    ui.label(
        egui::RichText::new(
            "Commands: /help /status /clear /history /system <text> /model <name> /backend <groq|openrouter|google|micro|full> /key <api-key> /temp <0-2> /tokens <1-8192> /preset <name> /save /load",
        )
        .color(theme.muted)
        .size(10.0),
    );
}
