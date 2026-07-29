pub struct AiosTheme {
    pub accent: egui::Color32,
    pub success: egui::Color32,
    pub warning: egui::Color32,
    pub danger: egui::Color32,
    pub info: egui::Color32,
    pub muted: egui::Color32,
    pub surface: egui::Color32,
    pub surface_alt: egui::Color32,
    pub border: egui::Color32,
    pub text: egui::Color32,
    pub text_dim: egui::Color32,
}

impl Default for AiosTheme {
    fn default() -> Self {
        Self {
            accent: egui::Color32::from_rgb(0, 200, 220),
            success: egui::Color32::from_rgb(50, 200, 80),
            warning: egui::Color32::from_rgb(240, 180, 30),
            danger: egui::Color32::from_rgb(230, 60, 60),
            info: egui::Color32::from_rgb(100, 160, 255),
            muted: egui::Color32::from_rgb(120, 120, 140),
            surface: egui::Color32::from_rgb(24, 24, 32),
            surface_alt: egui::Color32::from_rgb(34, 34, 46),
            border: egui::Color32::from_rgb(55, 55, 70),
            text: egui::Color32::from_rgb(230, 230, 240),
            text_dim: egui::Color32::from_rgb(140, 140, 160),
        }
    }
}

impl AiosTheme {
    pub fn apply(&self, ctx: &egui::Context) {
        let mut style = (*ctx.style()).clone();
        let visuals = &mut style.visuals;

        visuals.dark_mode = true;
        visuals.panel_fill = self.surface;
        visuals.window_fill = self.surface;
        visuals.widgets.noninteractive.bg_fill = self.surface_alt;
        visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, self.text);
        visuals.widgets.inactive.bg_fill = self.surface_alt;
        visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, self.text);
        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(45, 45, 60);
        visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0_f32, self.accent);
        visuals.widgets.active.bg_fill = egui::Color32::from_rgb(55, 55, 75);
        visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0_f32, self.accent);
        visuals.selection.bg_fill = egui::Color32::from_rgba_premultiplied(0, 180, 200, 60);
        visuals.selection.stroke = egui::Stroke::new(1.0_f32, self.accent);
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, self.border);
        visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, self.border);

        ctx.set_style(style);
    }

    pub fn priority_color(&self, priority: &str) -> egui::Color32 {
        match priority {
            "Critical" => self.danger,
            "High" => self.warning,
            "Normal" => self.success,
            "Low" => self.info,
            _ => self.muted,
        }
    }

    pub fn state_color(&self, state: &str) -> egui::Color32 {
        match state {
            "Running" | "Active" | "Installed" => self.success,
            "Ready" | "Loaded" => self.info,
            "Suspended" | "Frozen" | "UpdateAvailable" => self.warning,
            "Crashed" | "Error" | "Deprecated" => self.danger,
            "Terminated" | "Unloaded" | "Available" => self.muted,
            _ => self.text,
        }
    }
}
