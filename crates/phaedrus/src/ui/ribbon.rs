//! Ribbon toolbar (Home / Model / Test / View tabs).

use bevy_egui::egui::{self, Color32, RichText, Sense, vec2};
use phaedrus_dom::reflection;
use phaedrus_dom::types::Variant;

use super::emulator_window::EmulatorPage;
use super::icons::{ToolIcon, tool_icon};
use super::settings_window::SettingsPage;
use super::{UiState, theme};
use crate::settings::EmulatorSettings;
use crate::studio::{PlayState, Studio, Tool};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RibbonTab {
    Home,
    Model,
    Test,
    View,
}

fn big_button(ui: &mut egui::Ui, icon: ToolIcon, label: &str, active: bool, enabled: bool, tip: &str) -> bool {
    let size = vec2(52.0, 48.0);
    let (rect, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    if active {
        p.rect_filled(rect, 3.0, theme::BG_HOVER);
        p.rect_stroke(rect, 3.0, egui::Stroke::new(1.0, theme::ACCENT), egui::StrokeKind::Inside);
    } else if resp.hovered() && enabled {
        p.rect_filled(rect, 3.0, theme::BG_WIDGET);
    }
    let color = if enabled { theme::TEXT } else { theme::TEXT_DIM.gamma_multiply(0.6) };
    let icon_rect = egui::Rect::from_center_size(rect.center_top() + vec2(0.0, 16.0), vec2(22.0, 22.0));
    tool_icon(p, icon_rect, icon, color);
    p.text(rect.center_bottom() - vec2(0.0, 8.0), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(11.0), color);
    let resp = if tip.is_empty() { resp } else { resp.on_hover_text(tip) };
    enabled && resp.clicked()
}

fn group_sep(ui: &mut egui::Ui) {
    ui.add_space(4.0);
    let (r, _) = ui.allocate_exact_size(vec2(1.0, 46.0), Sense::hover());
    ui.painter().rect_filled(r, 0.0, theme::BORDER);
    ui.add_space(4.0);
}

pub fn ribbon(ui: &mut egui::Ui, studio: &mut Studio, state: &mut UiState, emulators: &mut EmulatorSettings) {
    // Tab row with play controls on the left, like Studio.
    ui.horizontal(|ui| {
        play_controls(ui, studio);
        ui.add_space(12.0);
        for (tab, label) in [
            (RibbonTab::Home, "Home"),
            (RibbonTab::Model, "Model"),
            (RibbonTab::Test, "Test"),
            (RibbonTab::View, "View"),
        ] {
            let active = state.ribbon_tab == tab;
            let text = RichText::new(label).color(if active { Color32::WHITE } else { theme::TEXT_DIM });
            let resp = ui.add(egui::Button::new(text).fill(if active { theme::BG_WIDGET } else { Color32::TRANSPARENT }).corner_radius(3.0));
            if resp.clicked() {
                state.ribbon_tab = tab;
            }
        }
    });
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        match state.ribbon_tab {
            RibbonTab::Home => home_tab(ui, studio, state),
            RibbonTab::Model => model_tab(ui, studio, state),
            RibbonTab::Test => test_tab(ui, studio, state, emulators),
            RibbonTab::View => view_tab(ui, state),
        }
    });
}

fn play_controls(ui: &mut egui::Ui, studio: &mut Studio) {
    let small = |ui: &mut egui::Ui, icon: ToolIcon, enabled: bool, tip: &str| -> bool {
        let (rect, resp) = ui.allocate_exact_size(vec2(22.0, 20.0), if enabled { Sense::click() } else { Sense::hover() });
        if resp.hovered() && enabled {
            ui.painter().rect_filled(rect, 3.0, theme::BG_WIDGET);
        }
        let col = if enabled { theme::TEXT } else { theme::TEXT_DIM.gamma_multiply(0.5) };
        tool_icon(ui.painter(), rect.shrink(3.0), icon, col);
        let resp = resp.on_hover_text(tip);
        enabled && resp.clicked()
    };
    let running = studio.is_running();
    if small(ui, ToolIcon::Run, !running, "Run (F8): run server scripts") {
        studio.pending_run = Some(true);
    }
    let paused = matches!(studio.play, PlayState::Running { paused: true });
    if small(ui, ToolIcon::Pause, running, if paused { "Resume" } else { "Pause" })
        && let PlayState::Running { paused } = &mut studio.play {
            *paused = !*paused;
        }
    if small(ui, ToolIcon::Stop, running, "Stop (Shift+F5)") {
        studio.pending_stop = true;
    }
}

fn tool_buttons(ui: &mut egui::Ui, studio: &mut Studio) {
    for (tool, icon, label, tip) in [
        (Tool::Select, ToolIcon::Select, "Select", "Select (Ctrl+1)"),
        (Tool::Move, ToolIcon::Move, "Move", "Move (Ctrl+2)"),
        (Tool::Scale, ToolIcon::Scale, "Scale", "Scale (Ctrl+3)"),
        (Tool::Rotate, ToolIcon::Rotate, "Rotate", "Rotate (Ctrl+4)"),
    ] {
        if big_button(ui, icon, label, studio.tool == tool, true, tip) {
            studio.tool = tool;
        }
    }
}

fn snap_controls(ui: &mut egui::Ui, studio: &mut Studio) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.checkbox(&mut studio.snap.rotate_enabled, "");
            ui.label(RichText::new("Rotate").small());
            ui.add(egui::DragValue::new(&mut studio.snap.rotate_step).range(0.1..=180.0).suffix("°").speed(0.5));
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut studio.snap.move_enabled, "");
            ui.label(RichText::new("Move  ").small());
            ui.add(egui::DragValue::new(&mut studio.snap.move_step).range(0.001..=512.0).suffix(" st").speed(0.05));
        });
        ui.horizontal(|ui| {
            let label = if studio.local_space { "Local (Ctrl+L)" } else { "World (Ctrl+L)" };
            if ui.small_button(label).clicked() {
                studio.local_space = !studio.local_space;
            }
        });
    });
}

fn home_tab(ui: &mut egui::Ui, studio: &mut Studio, state: &mut UiState) {
    tool_buttons(ui, studio);
    group_sep(ui);
    snap_controls(ui, studio);
    group_sep(ui);
    if big_button(ui, ToolIcon::Part, "Part", false, true, "Insert Part") {
        studio.insert_object("Part", None);
    }
    if big_button(ui, ToolIcon::Script, "Script", false, true, "Insert Script into ServerScriptService") {
        let sss = studio.doc().service("ServerScriptService");
        let prev = std::mem::take(&mut studio.selection);
        if let Some(s) = sss {
            studio.selection = vec![s];
        }
        if let Some(r) = studio.insert_object("Script", None) {
            studio.open_script(r);
        } else {
            studio.selection = prev;
        }
    }
    if big_button(ui, ToolIcon::Light, "Insert", false, true, "Insert Object...") {
        state.insert_dialog = Some(Default::default());
    }
    group_sep(ui);
    appearance_buttons(ui, studio);
    group_sep(ui);
    let has_sel = !studio.selection.is_empty();
    if big_button(ui, ToolIcon::Group, "Group", false, has_sel, "Group (Ctrl+G)") {
        studio.group_selection();
    }
    let locked = selection_bool(studio, "Locked");
    if big_button(ui, ToolIcon::Lock, "Lock", locked, has_sel, "Toggle Locked") {
        studio.toggle_bool_on_selection("Locked");
    }
    let anchored = selection_bool(studio, "Anchored");
    if big_button(ui, ToolIcon::Anchor, "Anchor", anchored, has_sel, "Toggle Anchored") {
        studio.toggle_bool_on_selection("Anchored");
    }
    group_sep(ui);
    if big_button(ui, ToolIcon::Explorer, "Explorer", state.show_explorer, true, "Toggle Explorer") {
        state.show_explorer = !state.show_explorer;
    }
    if big_button(ui, ToolIcon::Properties, "Properties", state.show_properties, true, "Toggle Properties") {
        state.show_properties = !state.show_properties;
    }
    if big_button(ui, ToolIcon::Output, "Output", state.show_output, true, "Toggle Output") {
        state.show_output = !state.show_output;
    }
}

fn selection_bool(studio: &Studio, name: &str) -> bool {
    let doc = studio.doc();
    studio
        .selection
        .first()
        .is_some_and(|r| matches!(doc.get_property(*r, name), Some(Variant::Bool(true))))
}

fn appearance_buttons(ui: &mut egui::Ui, studio: &mut Studio) {
    let has_parts = {
        let doc = studio.doc();
        studio.selection.iter().any(|r| doc.is_a(*r, "BasePart"))
    };
    let mat_resp = ui.add_enabled_ui(has_parts, |ui| {
        let (rect, resp) = ui.allocate_exact_size(vec2(52.0, 48.0), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(rect, 3.0, theme::BG_WIDGET);
        }
        tool_icon(ui.painter(), egui::Rect::from_center_size(rect.center_top() + vec2(0.0, 16.0), vec2(22.0, 22.0)), ToolIcon::Material, theme::TEXT);
        ui.painter().text(rect.center_bottom() - vec2(0.0, 8.0), egui::Align2::CENTER_CENTER, "Material", egui::FontId::proportional(11.0), theme::TEXT);
        resp
    });
    egui::Popup::menu(&mat_resp.inner).show(|ui| {
        ui.set_min_width(140.0);
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            for (name, v) in reflection::enum_items("Material") {
                if matches!(name, "Air" | "Water") {
                    continue;
                }
                if ui.button(name).clicked() {
                    studio.set_on_selection("Material", Variant::Enum(phaedrus_dom::types::Enum::from_u32(v)));
                    ui.close();
                }
            }
        });
    });

    let color_resp = ui.add_enabled_ui(has_parts, |ui| {
        let (rect, resp) = ui.allocate_exact_size(vec2(52.0, 48.0), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(rect, 3.0, theme::BG_WIDGET);
        }
        tool_icon(ui.painter(), egui::Rect::from_center_size(rect.center_top() + vec2(0.0, 16.0), vec2(22.0, 22.0)), ToolIcon::Color, theme::TEXT);
        ui.painter().text(rect.center_bottom() - vec2(0.0, 8.0), egui::Align2::CENTER_CENTER, "Color", egui::FontId::proportional(11.0), theme::TEXT);
        resp
    });
    egui::Popup::menu(&color_resp.inner).show(|ui| {
        egui::Grid::new("brick_palette").spacing(vec2(2.0, 2.0)).show(ui, |ui| {
            for (i, bc) in phaedrus_dom::geometry::all_brick_colors().iter().enumerate().take(208) {
                let c = bc.to_color3uint8();
                let (r, resp) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::click());
                ui.painter().rect_filled(r, 1.0, Color32::from_rgb(c.r, c.g, c.b));
                if resp.on_hover_text(bc.to_string()).clicked() {
                    studio.set_on_selection("BrickColor", Variant::BrickColor(*bc));
                    ui.close();
                }
                if i % 13 == 12 {
                    ui.end_row();
                }
            }
        });
    });
}

fn model_tab(ui: &mut egui::Ui, studio: &mut Studio, state: &mut UiState) {
    tool_buttons(ui, studio);
    group_sep(ui);
    for (class, label) in [("Part", "Block"), ("WedgePart", "Wedge"), ("CornerWedgePart", "Corner"), ("TrussPart", "Truss"), ("SpawnLocation", "Spawn")] {
        if big_button(ui, ToolIcon::Part, label, false, true, &format!("Insert {class}")) {
            studio.insert_object(class, None);
        }
    }
    if big_button(ui, ToolIcon::Part, "Sphere", false, true, "Insert spherical Part")
        && let Some(r) = studio.insert_object("Part", None) {
            let _ = studio.doc().set_property(r, "Shape", Variant::Enum(phaedrus_dom::types::Enum::from_u32(0)));
            let _ = studio.doc().set_property(r, "Size", Variant::Vector3(phaedrus_dom::types::Vector3::new(2.0, 2.0, 2.0)));
            studio.waypoint("Insert Sphere");
        }
    if big_button(ui, ToolIcon::Part, "Cylinder", false, true, "Insert cylindrical Part")
        && let Some(r) = studio.insert_object("Part", None) {
            let _ = studio.doc().set_property(r, "Shape", Variant::Enum(phaedrus_dom::types::Enum::from_u32(2)));
            let _ = studio.doc().set_property(r, "Size", Variant::Vector3(phaedrus_dom::types::Vector3::new(2.0, 2.0, 2.0)));
            studio.waypoint("Insert Cylinder");
        }
    group_sep(ui);
    let has_sel = !studio.selection.is_empty();
    if big_button(ui, ToolIcon::Group, "Group", false, has_sel, "Group (Ctrl+G)") {
        studio.group_selection();
    }
    if big_button(ui, ToolIcon::Ungroup, "Ungroup", false, has_sel, "Ungroup (Ctrl+U)") {
        studio.ungroup_selection();
    }
    if big_button(ui, ToolIcon::Model, "Model", false, true, "Insert empty Model") {
        studio.insert_object("Model", None);
    }
    if big_button(ui, ToolIcon::Folder, "Folder", false, true, "Insert Folder") {
        studio.insert_object("Folder", None);
    }
    group_sep(ui);
    appearance_buttons(ui, studio);
    if big_button(ui, ToolIcon::Light, "Insert", false, true, "Insert Object...") {
        state.insert_dialog = Some(Default::default());
    }
}

fn test_tab(ui: &mut egui::Ui, studio: &mut Studio, state: &mut UiState, emulators: &mut EmulatorSettings) {
    let running = studio.is_running();
    if big_button(ui, ToolIcon::Run, "Run", false, !running, "Run server scripts (F8)") {
        studio.pending_run = Some(true);
    }
    let paused = matches!(studio.play, PlayState::Running { paused: true });
    if big_button(ui, ToolIcon::Pause, if paused { "Resume" } else { "Pause" }, paused, running, "Pause / resume scripts")
        && let PlayState::Running { paused } = &mut studio.play {
            *paused = !*paused;
        }
    if big_button(ui, ToolIcon::Stop, "Stop", false, running, "Stop (Shift+F5)") {
        studio.pending_stop = true;
    }
    group_sep(ui);
    emulator_controls(ui, state, emulators);
    group_sep(ui);
    ui.vertical(|ui| {
        ui.add_space(6.0);
        ui.label(RichText::new("Play (F5) with a character and client/server").small().color(theme::TEXT_DIM));
        ui.label(RichText::new("simulation is planned; Run executes server Scripts.").small().color(theme::TEXT_DIM));
    });
}

/// The Emulators group: toggles for mock Roblox services and their settings.
fn emulator_controls(ui: &mut egui::Ui, state: &mut UiState, emulators: &mut EmulatorSettings) {
    let ds = &mut emulators.datastore;
    let tip = if ds.enabled {
        "DataStoreService emulator is on: data stores read and write local data for this place. Click to turn off."
    } else {
        "DataStoreService emulator is off: data store calls fail as in an unpublished place. Click to turn on."
    };
    if big_button(ui, ToolIcon::Database, "DataStore", ds.enabled, true, tip) {
        ds.enabled = !ds.enabled;
    }
    let open = state.emulator_page.is_some();
    if big_button(ui, ToolIcon::Gear, "Emulators", open, true, "Emulator settings: simulated errors, latency, budgets and stored data") {
        state.emulator_page = if open { None } else { Some(EmulatorPage::DataStore) };
    }
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        ui.add_space(4.0);
        let (text, color) = if !ds.enabled {
            ("DataStoreService: offline", theme::TEXT_DIM)
        } else if ds.outage {
            ("DataStoreService: outage simulated", egui::Color32::from_rgb(230, 120, 90))
        } else {
            ("DataStoreService: emulated", theme::ACCENT)
        };
        ui.label(RichText::new(text).small().color(color));
        let failing = ds.rules.values().filter(|r| r.enabled && r.chance > 0.0).count();
        if ds.enabled && failing > 0 && !ds.outage {
            ui.label(RichText::new(format!("{failing} request type(s) set to fail")).small().color(theme::TEXT_DIM));
        }
        if ui.small_button("Stored data…").clicked() {
            state.emulator_page = Some(EmulatorPage::DataStoreData);
        }
    });
}

fn view_tab(ui: &mut egui::Ui, state: &mut UiState) {
    for (icon, label, flag) in [
        (ToolIcon::Explorer, "Explorer", &mut state.show_explorer),
        (ToolIcon::Properties, "Properties", &mut state.show_properties),
        (ToolIcon::Output, "Output", &mut state.show_output),
        (ToolIcon::Script, "Command", &mut state.show_command_bar),
    ] {
        if big_button(ui, icon, label, *flag, true, "") {
            *flag = !*flag;
        }
    }
    group_sep(ui);
    let graphics_open = state.settings_page == Some(SettingsPage::Graphics);
    if big_button(ui, ToolIcon::Gear, "Graphics", graphics_open, true, "Graphics & lighting settings") {
        state.settings_page = if graphics_open { None } else { Some(SettingsPage::Graphics) };
    }
    if big_button(ui, ToolIcon::Move, "Camera", state.show_camera_settings, true, "Camera speed settings") {
        state.show_camera_settings = !state.show_camera_settings;
    }
}
