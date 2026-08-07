mod app;
mod tabs;
mod theme;
mod widgets;

use aios_hal::ai_tier::AiTier;
use aios_hal::hardware::HardwareProfile;

fn main() -> eframe::Result<()> {
    env_logger::init();

    let hardware = HardwareProfile::detect();
    let ai_tier = AiTier::from_profile(&hardware);

    let dep_blocks: Vec<String> = Vec::new();
    let dep_load_order: Vec<String> = Vec::new();
    let dep_edges: Vec<(String, String)> = Vec::new();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("AIOS Dashboard v1.0.0")
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([900.0, 600.0])
            .with_resizable(true),
        ..Default::default()
    };

    eframe::run_native(
        "AIOS Dashboard",
        native_options,
        Box::new(move |cc| {
            let theme = theme::AiosTheme::default();
            theme.apply(&cc.egui_ctx);

            let mut app = app::AiosApp::new(
                ai_tier,
                hardware,
                dep_blocks,
                dep_load_order,
                dep_edges,
                0,
                4096,
            );
            app.ai_load_persisted();
            app.fm_init();
            Ok(Box::new(app))
        }),
    )
}
