//! Menu bar and floating windows (Insert Object, Camera settings, About).
//! Phaedrus Settings lives in `settings_window.rs`.

use bevy::app::AppExit;
use bevy::prelude::MessageWriter;
use bevy_egui::egui::{self, RichText};
use phaedrus_dom::reflection;

use super::explorer::MenuCmd;
use super::shortcuts::{open, save};
use super::settings_window::SettingsPage;
use super::{UiState, icons, theme};
use crate::studio::{Studio, Tool};
use crate::viewport::camera::CameraSettings;
use crate::viewport::lighting::GraphicsSettings;

#[derive(Default)]
pub struct InsertDialog {
    pub search: String,
    pub focus_set: bool,
}

pub fn menu_bar(
    ui: &mut egui::Ui,
    studio: &mut Studio,
    state: &mut UiState,
    graphics: &mut GraphicsSettings,
    exit: &mut MessageWriter<AppExit>,
) {
    ui.menu_button("File", |ui| {
        if ui.add(egui::Button::new("New Place").shortcut_text("Ctrl+N")).clicked() {
            studio.replace_document(phaedrus_dom::Document::new_place());
            ui.close();
        }
        if ui.add(egui::Button::new("Open...").shortcut_text("Ctrl+O")).clicked() {
            ui.close();
            open(studio);
        }
        ui.separator();
        if ui.add(egui::Button::new("Save").shortcut_text("Ctrl+S")).clicked() {
            ui.close();
            save(studio, false);
        }
        if ui.add(egui::Button::new("Save As...").shortcut_text("Ctrl+Shift+S")).clicked() {
            ui.close();
            save(studio, true);
        }
        if ui.add_enabled(!studio.selection.is_empty(), egui::Button::new("Export Selection...")).clicked() {
            ui.close();
            export_selection(studio);
        }
        ui.separator();
        if ui.button("Phaedrus Settings...").clicked() {
            state.settings_page = Some(SettingsPage::ScriptEditor);
            ui.close();
        }
        ui.separator();
        if ui.button("Exit").clicked() {
            exit.write(AppExit::Success);
        }
    });
    ui.menu_button("Edit", |ui| {
        let undo_label = match studio.history.undo_name() {
            Some(n) => format!("Undo {n}"),
            None => "Undo".into(),
        };
        if ui.add_enabled(studio.history.can_undo(), egui::Button::new(undo_label).shortcut_text("Ctrl+Z")).clicked() {
            studio.undo();
            ui.close();
        }
        let redo_label = match studio.history.redo_name() {
            Some(n) => format!("Redo {n}"),
            None => "Redo".into(),
        };
        if ui.add_enabled(studio.history.can_redo(), egui::Button::new(redo_label).shortcut_text("Ctrl+Y")).clicked() {
            studio.redo();
            ui.close();
        }
        ui.separator();
        for (label, sc, cmd) in [
            ("Cut", "Ctrl+X", MenuCmd::Cut),
            ("Copy", "Ctrl+C", MenuCmd::Copy),
            ("Paste Into", "Ctrl+Shift+V", MenuCmd::PasteInto),
            ("Duplicate", "Ctrl+D", MenuCmd::Duplicate),
            ("Delete", "Del", MenuCmd::Delete),
            ("Rename", "F2", MenuCmd::Rename),
            ("Group", "Ctrl+G", MenuCmd::Group),
            ("Ungroup", "Ctrl+U", MenuCmd::Ungroup),
        ] {
            if ui.add(egui::Button::new(label).shortcut_text(sc)).clicked() {
                state.pending_menu = Some(cmd);
                ui.close();
            }
        }
        if ui.add(egui::Button::new("Paste").shortcut_text("Ctrl+V")).clicked() {
            studio.paste(false);
            ui.close();
        }
        ui.separator();
        for (label, sc, tool) in [
            ("Select", "Ctrl+1", Tool::Select),
            ("Move", "Ctrl+2", Tool::Move),
            ("Scale", "Ctrl+3", Tool::Scale),
            ("Rotate", "Ctrl+4", Tool::Rotate),
        ] {
            if ui.add(egui::Button::selectable(studio.tool == tool, label).shortcut_text(sc)).clicked() {
                studio.tool = tool;
                ui.close();
            }
        }
    });
    ui.menu_button("View", |ui| {
        ui.checkbox(&mut state.show_explorer, "Explorer");
        ui.checkbox(&mut state.show_properties, "Properties");
        ui.checkbox(&mut state.show_output, "Output");
        ui.checkbox(&mut state.show_command_bar, "Command Bar");
        ui.separator();
        ui.checkbox(&mut graphics.show_fps, "Show FPS");
        if ui.button("Graphics Settings...").clicked() {
            state.settings_page = Some(SettingsPage::Graphics);
            ui.close();
        }
        if ui.button("Camera Settings...").clicked() {
            state.show_camera_settings = true;
            ui.close();
        }
    });
    ui.menu_button("Insert", |ui| {
        for class in ["Part", "WedgePart", "CornerWedgePart", "TrussPart", "SpawnLocation", "Model", "Folder", "Script", "LocalScript", "ModuleScript", "PointLight", "SpotLight", "Texture", "Decal"] {
            if ui.button(class).clicked() {
                if let Some(r) = studio.insert_object(class, None)
                    && reflection::is_a(class, "LuaSourceContainer") {
                        studio.open_script(r);
                    }
                ui.close();
            }
        }
        ui.separator();
        if ui.button("Insert Object...").clicked() {
            state.insert_dialog = Some(Default::default());
            ui.close();
        }
    });
    ui.menu_button("Test", |ui| {
        if ui.add_enabled(!studio.is_running(), egui::Button::new("Run").shortcut_text("F8")).clicked() {
            studio.pending_run = Some(true);
            ui.close();
        }
        if ui.add_enabled(studio.is_running(), egui::Button::new("Stop").shortcut_text("Shift+F5")).clicked() {
            studio.pending_stop = true;
            ui.close();
        }
    });
    ui.menu_button("Help", |ui| {
        if ui.button("About Phaedrus").clicked() {
            state.show_about = true;
            ui.close();
        }
    });
}

fn export_selection(studio: &mut Studio) {
    let roots = studio.selection_roots();
    if roots.is_empty() {
        return;
    }
    let Some(path) = rfd::FileDialog::new()
        .add_filter("Roblox Model", &["rbxm", "rbxmx"])
        .set_file_name("Model.rbxm")
        .save_file()
    else {
        return;
    };
    let result = {
        let doc = studio.doc();
        phaedrus_dom::save_file(&path, doc.dom(), Some(&roots))
    };
    match result {
        Ok(()) => studio.log(phaedrus_luau::OutputLevel::Info, format!("Exported {}", path.display())),
        Err(e) => studio.log(phaedrus_luau::OutputLevel::Error, format!("Export failed: {e:#}")),
    }
}

pub fn windows(
    ctx: &egui::Context,
    studio: &mut Studio,
    state: &mut UiState,
    camera: &mut CameraSettings,
) {
    if let Some(cmd) = state.pending_menu.take() {
        super::explorer::run_menu_cmd(cmd, studio, state);
    }

    // Insert Object.
    let mut close_insert = false;
    if let Some(dialog) = &mut state.insert_dialog {
        let mut open = true;
        egui::Window::new("Insert Object")
            .open(&mut open)
            .default_size([300.0, 420.0])
            .collapsible(false)
            .show(ctx, |ui| {
                let resp = ui.add(egui::TextEdit::singleline(&mut dialog.search).hint_text("Search classes").desired_width(f32::INFINITY));
                if !dialog.focus_set {
                    resp.request_focus();
                    dialog.focus_set = true;
                }
                let q = dialog.search.to_lowercase();
                let classes: Vec<&str> = reflection::creatable_classes()
                    .into_iter()
                    .filter(|c| q.is_empty() || c.to_lowercase().contains(&q))
                    .collect();
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                if enter
                    && let Some(c) = classes.first() {
                        insert(studio, c);
                        close_insert = true;
                    }
                egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, 20.0, classes.len(), |ui, range| {
                    for c in &classes[range] {
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                            icons::class_icon(ui.painter(), r, c);
                            if ui.selectable_label(false, *c).clicked() {
                                insert(studio, c);
                                close_insert = true;
                            }
                        });
                    }
                });
            });
        if !open {
            close_insert = true;
        }
    }
    if close_insert {
        state.insert_dialog = None;
    }

    // Camera settings.
    if state.show_camera_settings {
        let mut open = true;
        egui::Window::new("Camera")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                egui::Grid::new("camera").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label("Fly speed");
                    ui.add(egui::Slider::new(&mut camera.speed, 1.0..=500.0).logarithmic(true).suffix(" st/s"));
                    ui.end_row();
                    ui.label("Accelerate when held");
                    ui.checkbox(&mut camera.accelerate, "");
                    ui.end_row();
                    ui.add_enabled(camera.accelerate, egui::Label::new("Max speed multiplier"));
                    ui.add_enabled(camera.accelerate, egui::Slider::new(&mut camera.max_boost, 1.0..=10.0).suffix("\u{d7}"));
                    ui.end_row();
                    ui.add_enabled(camera.accelerate, egui::Label::new("Time to max speed"));
                    ui.add_enabled(camera.accelerate, egui::Slider::new(&mut camera.ramp_time, 0.1..=10.0).suffix(" s"));
                    ui.end_row();
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Reset").clicked() {
                        *camera = CameraSettings::default();
                    }
                    ui.label(
                        RichText::new(format!(
                            "Current: {:.0} st/s ({:.1}\u{d7}) \u{b7} Shift: quarter speed",
                            camera.speed * camera.boost,
                            camera.boost
                        ))
                        .small()
                        .color(theme::TEXT_DIM),
                    );
                });
            });
        state.show_camera_settings = open;
    }

    if state.show_about {
        let mut open = true;
        egui::Window::new("About Phaedrus").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.heading("Phaedrus");
            ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
            ui.label("A clean-room Roblox Studio alternative written in Rust.");
            ui.label(RichText::new("Not affiliated with Roblox Corporation.").small().color(theme::TEXT_DIM));
        });
        state.show_about = open;
    }
}

fn insert(studio: &mut Studio, class: &str) {
    if let Some(r) = studio.insert_object(class, None)
        && reflection::is_a(class, "LuaSourceContainer") {
            studio.open_script(r);
        }
}

/// Blocks the editor while a place file loads in the background.
pub fn loading_modal(ctx: &egui::Context, studio: &mut Studio) {
    let Some(load) = &studio.loading else { return };
    let name = load
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| load.path.display().to_string());
    let elapsed = load.started.elapsed().as_secs();
    let mut cancel = false;
    egui::Modal::new(egui::Id::new("loading_modal")).show(ctx, |ui| {
        ui.set_width(320.0);
        ui.vertical_centered(|ui| {
            ui.add_space(6.0);
            ui.add(egui::Spinner::new().size(28.0));
            ui.add_space(8.0);
            ui.label(RichText::new(format!("Loading {name}")).strong().color(theme::TEXT));
            ui.label(RichText::new(format!("{elapsed}s")).small().color(theme::TEXT_DIM));
            ui.add_space(8.0);
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
            ui.add_space(4.0);
        });
    });
    if cancel {
        studio.cancel_loading();
    }
}
