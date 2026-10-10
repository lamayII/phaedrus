//! Dark theme modelled on Roblox Studio's.

use bevy_egui::egui::{self, Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Visuals};

pub const BG_DARKEST: Color32 = Color32::from_rgb(23, 24, 26);
pub const BG_TOP: Color32 = Color32::from_rgb(30, 31, 34);
pub const BG_PANEL: Color32 = Color32::from_rgb(37, 38, 41);
pub const BG_WIDGET: Color32 = Color32::from_rgb(47, 48, 52);
pub const BG_HOVER: Color32 = Color32::from_rgb(58, 60, 64);
pub const BORDER: Color32 = Color32::from_rgb(20, 20, 22);
pub const TEXT: Color32 = Color32::from_rgb(214, 214, 214);
pub const TEXT_DIM: Color32 = Color32::from_rgb(150, 150, 154);
pub const ACCENT: Color32 = Color32::from_rgb(0, 162, 255);
pub const SELECTION: Color32 = Color32::from_rgb(44, 93, 135);
pub const WARN: Color32 = Color32::from_rgb(255, 170, 40);
pub const ERROR: Color32 = Color32::from_rgb(255, 75, 75);
pub const INFO_BLUE: Color32 = Color32::from_rgb(120, 180, 255);

pub fn apply(ctx: &egui::Context) {
    let mut visuals = Visuals::dark();
    visuals.panel_fill = BG_PANEL;
    visuals.window_fill = BG_PANEL;
    visuals.extreme_bg_color = BG_DARKEST;
    visuals.faint_bg_color = BG_WIDGET;
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = SELECTION;
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.hyperlink_color = ACCENT;
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    visuals.window_corner_radius = CornerRadius::same(4);
    visuals.menu_corner_radius = CornerRadius::same(3);
    for (w, fill) in [
        (&mut visuals.widgets.noninteractive, BG_PANEL),
        (&mut visuals.widgets.inactive, BG_WIDGET),
        (&mut visuals.widgets.hovered, BG_HOVER),
        (&mut visuals.widgets.active, Color32::from_rgb(70, 72, 78)),
        (&mut visuals.widgets.open, BG_HOVER),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.corner_radius = CornerRadius::same(3);
    }
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.inactive.bg_stroke = Stroke::NONE;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(80, 82, 88));
    ctx.set_visuals(visuals);

    let mut style = (*ctx.global_style()).clone();
    style.spacing.item_spacing = egui::vec2(6.0, 3.0);
    style.spacing.button_padding = egui::vec2(6.0, 2.0);
    style.spacing.interact_size.y = 20.0;
    style.spacing.indent = 14.0;
    style.text_styles = [
        (TextStyle::Small, FontId::new(10.5, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(12.5, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(12.5, FontFamily::Proportional)),
        (TextStyle::Heading, FontId::new(15.0, FontFamily::Proportional)),
        (TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace)),
    ]
    .into();
    ctx.set_global_style(style);
}
