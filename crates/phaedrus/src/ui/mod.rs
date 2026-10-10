//! Studio-like egui interface: menu bar, ribbon, Explorer, Properties,
//! Output, command bar and the document tab strip around the 3D viewport.

mod code_editor;
mod dialogs;
mod emulator_window;
mod explorer;
pub mod icons;
mod output;
mod properties;
mod ribbon;
mod settings_window;
mod shortcuts;
pub mod theme;

use std::collections::HashSet;

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::egui::{self, Color32, Frame, Margin, Stroke};
use bevy_egui::{EguiContexts, EguiPrimaryContextPass};
use phaedrus_dom::types::Ref;

pub use settings_window::SettingsPage;

use crate::editor::ScriptEditorHost;
use crate::emulators::DataStoreEmulatorHandle;
use crate::settings::{EmulatorSettings, ScriptEditorSettings};
use crate::studio::{PlayState, Studio, Tab};
use crate::viewport::{ViewportState, ViewportTexture};
use crate::viewport::camera::CameraSettings;
use crate::viewport::lighting::GraphicsSettings;

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UiState>()
            .add_systems(EguiPrimaryContextPass, ui_system)
            .add_systems(Update, update_window_title);
    }
}

#[derive(Default)]
pub struct OutputFilter {
    pub text: String,
    pub show_info: bool,
    pub show_warn: bool,
    pub show_error: bool,
}

#[derive(Resource)]
pub struct UiState {
    pub themed: bool,
    pub show_explorer: bool,
    pub show_properties: bool,
    pub show_output: bool,
    pub show_command_bar: bool,
    pub ribbon_tab: ribbon::RibbonTab,
    pub explorer_filter: String,
    pub expanded: HashSet<Ref>,
    pub renaming: Option<(Ref, String)>,
    pub explorer_scroll_to: Option<Ref>,
    pub properties_filter: String,
    pub prop_edit: Option<(String, String)>,
    pub output_filter: OutputFilter,
    pub command: String,
    pub command_history: Vec<String>,
    pub command_history_pos: Option<usize>,
    pub insert_dialog: Option<dialogs::InsertDialog>,
    /// Open Phaedrus Settings page, if the window is open.
    pub settings_page: Option<SettingsPage>,
    /// Where the settings window was last drawn.
    pub settings_rect: Option<egui::Rect>,
    /// Open Emulator Settings page, if the window is open.
    pub emulator_page: Option<emulator_window::EmulatorPage>,
    /// Where the emulator window was last drawn.
    pub emulator_rect: Option<egui::Rect>,
    pub data_browser: emulator_window::DataBrowser,
    pub show_camera_settings: bool,
    pub show_about: bool,
    pub focus_explorer_filter: bool,
    pub focus_properties_filter: bool,
    pub viewport_focused: bool,
    /// Explorer drag-and-drop source.
    pub dragging: Option<Vec<Ref>>,
    pub code_editor: code_editor::FallbackEditorState,
    pub pending_menu: Option<explorer::MenuCmd>,
    last_selection: Vec<Ref>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            themed: false,
            show_explorer: true,
            show_properties: true,
            show_output: true,
            show_command_bar: true,
            ribbon_tab: ribbon::RibbonTab::Home,
            explorer_filter: String::new(),
            expanded: HashSet::new(),
            renaming: None,
            explorer_scroll_to: None,
            properties_filter: String::new(),
            prop_edit: None,
            output_filter: OutputFilter { show_info: true, show_warn: true, show_error: true, ..default() },
            command: String::new(),
            command_history: Vec::new(),
            command_history_pos: None,
            insert_dialog: None,
            settings_page: None,
            settings_rect: None,
            emulator_page: None,
            emulator_rect: None,
            data_browser: Default::default(),
            show_camera_settings: false,
            show_about: false,
            focus_explorer_filter: false,
            focus_properties_filter: false,
            viewport_focused: true,
            dragging: None,
            code_editor: Default::default(),
            pending_menu: None,
            last_selection: Vec::new(),
        }
    }
}

impl UiState {
    /// Shows the Test tab and the Emulator Settings window.
    pub fn open_emulators(&mut self, stored_data: bool) {
        self.ribbon_tab = ribbon::RibbonTab::Test;
        self.emulator_page = Some(if stored_data {
            emulator_window::EmulatorPage::DataStoreData
        } else {
            emulator_window::EmulatorPage::DataStore
        });
    }
}

fn panel_frame() -> Frame {
    Frame::new().fill(theme::BG_PANEL).inner_margin(Margin::same(0)).stroke(Stroke::new(1.0, theme::BORDER))
}

/// Panel title bar like Studio's ("Explorer", "Properties", ...).
pub fn panel_header(ui: &mut egui::Ui, title: &str, open: &mut bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 20.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::BG_TOP);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, title, egui::FontId::proportional(11.5), theme::TEXT_DIM);
    let close = egui::Rect::from_center_size(rect.right_center() - egui::vec2(10.0, 0.0), egui::vec2(14.0, 14.0));
    let resp = ui.interact(close, ui.id().with(("close", title)), egui::Sense::click());
    let col = if resp.hovered() { theme::TEXT } else { theme::TEXT_DIM };
    ui.painter().text(close.center(), egui::Align2::CENTER_CENTER, "×", egui::FontId::proportional(13.0), col);
    if resp.clicked() {
        *open = false;
    }
}

#[allow(clippy::too_many_arguments)]
fn ui_system(
    mut contexts: EguiContexts,
    mut studio: ResMut<Studio>,
    mut state: ResMut<UiState>,
    mut viewport: ResMut<ViewportState>,
    mut graphics: ResMut<GraphicsSettings>,
    mut editor_settings: ResMut<ScriptEditorSettings>,
    mut camera_settings: ResMut<CameraSettings>,
    mut emulator_settings: ResMut<EmulatorSettings>,
    datastore: Res<DataStoreEmulatorHandle>,
    mut editor_host: NonSendMut<ScriptEditorHost>,
    viewport_texture: Res<ViewportTexture>,
    mut viewport_tex_id: Local<Option<egui::TextureId>>,
    diagnostics: Res<bevy::diagnostic::DiagnosticsStore>,
    buttons: Res<ButtonInput<MouseButton>>,
    mut exit: MessageWriter<AppExit>,
) -> Result {
    let tex_id = *viewport_tex_id.get_or_insert_with(|| {
        contexts.add_image(bevy_egui::EguiTextureHandle::Weak(viewport_texture.handle.id()))
    });
    let ctx = contexts.ctx_mut()?.clone();
    viewport.scale = ctx.pixels_per_point();
    if !state.themed {
        theme::apply(&ctx);
        state.themed = true;
    }
    let mut root = egui::Ui::new(
        ctx.clone(),
        "root".into(),
        egui::UiBuilder::new().layer_id(egui::LayerId::background()).max_rect(ctx.content_rect()),
    );

    if studio.loading.is_none() {
        shortcuts::handle(&ctx, &mut studio, &mut state);
    }
    if studio.selection != state.last_selection {
        state.explorer_scroll_to = studio.selection.last().copied().filter(|r| !state.last_selection.contains(r));
        state.last_selection = studio.selection.clone();
    }

    // Menu bar.
    egui::Panel::top("menu_bar")
        .frame(Frame::new().fill(theme::BG_DARKEST).inner_margin(Margin::symmetric(4, 1)))
        .show(&mut root, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                dialogs::menu_bar(ui, &mut studio, &mut state, &mut graphics, &mut exit);
            });
        });

    // Ribbon.
    egui::Panel::top("ribbon")
        .frame(Frame::new().fill(theme::BG_TOP).inner_margin(Margin::symmetric(6, 3)).stroke(Stroke::new(1.0, theme::BORDER)))
        .show(&mut root, |ui| {
            ribbon::ribbon(ui, &mut studio, &mut state, &mut emulator_settings);
        });

    // Status bar + command bar.
    egui::Panel::bottom("status_bar")
        .frame(Frame::new().fill(theme::BG_DARKEST).inner_margin(Margin::symmetric(6, 2)))
        .show(&mut root, |ui| {
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if graphics.show_fps
                        && let Some(fps) = diagnostics
                            .get(&bevy::diagnostic::FrameTimeDiagnosticsPlugin::FPS)
                            .and_then(|d| d.smoothed())
                        {
                            ui.label(egui::RichText::new(format!("{fps:.0} FPS")).small().color(theme::TEXT_DIM));
                        }
                    let mode = match studio.play {
                        PlayState::Edit => "Edit",
                        PlayState::Running { paused: false } => "Running",
                        PlayState::Running { paused: true } => "Paused",
                    };
                    ui.label(egui::RichText::new(mode).small().color(if studio.is_running() { theme::ACCENT } else { theme::TEXT_DIM }));
                    ui.label(egui::RichText::new(&studio.status).small().color(theme::TEXT_DIM));
                    if state.show_command_bar {
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            output::command_bar(ui, &mut studio, &mut state);
                        });
                    }
                });
            });
        });

    // Properties (left).
    if state.show_properties {
        egui::Panel::left("properties")
            .resizable(true)
            .default_size(270.0)
            .min_size(180.0)
            .frame(panel_frame())
            .show(&mut root, |ui| {
                let mut open = true;
                panel_header(ui, "Properties", &mut open);
                properties::properties_panel(ui, &mut studio, &mut state);
                state.show_properties = open;
            });
    }

    // Explorer (right).
    if state.show_explorer {
        egui::Panel::right("explorer")
            .resizable(true)
            .default_size(270.0)
            .min_size(180.0)
            .frame(panel_frame())
            .show(&mut root, |ui| {
                let mut open = true;
                panel_header(ui, "Explorer", &mut open);
                explorer::explorer_panel(ui, &mut studio, &mut state);
                state.show_explorer = open;
            });
    }

    // Output (bottom of centre).
    if state.show_output {
        egui::Panel::bottom("output")
            .resizable(true)
            .default_size(180.0)
            .min_size(60.0)
            .frame(panel_frame())
            .show(&mut root, |ui| {
                let mut open = true;
                panel_header(ui, "Output", &mut open);
                output::output_panel(ui, &mut studio, &mut state);
                state.show_output = open;
            });
    }

    // Centre: tab strip + viewport or script editor.
    let mut editor_rect = None;
    egui::CentralPanel::no_frame().show(&mut root, |ui| {
        tab_strip(ui, &mut studio);
        let rect = ui.available_rect_before_wrap();
        match studio.tabs.get(studio.active_tab).cloned() {
            Some(Tab::Script(r)) => {
                viewport.visible = false;
                viewport.hovered = false;
                editor_rect = Some(rect);
                if !editor_host.is_native_available() {
                    code_editor::fallback_editor(ui, rect, r, &mut studio, &mut state, &editor_settings.colors);
                } else {
                    // The native Monaco view is drawn over this rect. It
                    // stays hidden until it has painted, so fill the area in
                    // the editor's colours meanwhile.
                    ui.allocate_rect(rect, egui::Sense::hover());
                    if !editor_host.is_showing() {
                        let [r, g, b] = editor_settings.colors.background.0;
                        ui.painter().rect_filled(rect, 0.0, Color32::from_rgb(r, g, b));
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            if state.settings_rect.is_some_and(|s| s.intersects(rect)) {
                                "Move Phaedrus Settings off this area to see the script"
                            } else if state.emulator_rect.is_some_and(|s| s.intersects(rect)) {
                                "Move Emulator Settings off this area to see the script"
                            } else {
                                "Loading script editor…"
                            },
                            egui::FontId::proportional(13.0),
                            Color32::from_gray(140),
                        );
                    }
                }
            }
            _ => {
                viewport.visible = true;
                viewport.rect = Some(rect);
                let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
                ui.painter().image(
                    tex_id,
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
                // Frame around the 3D view like Studio.
                ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.0, theme::BORDER), egui::StrokeKind::Inside);
                let over_ui = ctx.is_pointer_over_egui() && !resp.hovered();
                viewport.hovered = resp.hovered() && !over_ui;
                if resp.hovered() && (buttons.just_pressed(MouseButton::Left) || buttons.just_pressed(MouseButton::Right) || buttons.just_pressed(MouseButton::Middle)) {
                    viewport.drag_started_inside = true;
                    state.viewport_focused = true;
                } else if !buttons.pressed(MouseButton::Left) && !buttons.pressed(MouseButton::Right) && !buttons.pressed(MouseButton::Middle) {
                    viewport.drag_started_inside = false;
                }
                if !resp.hovered() && buttons.just_pressed(MouseButton::Left) {
                    state.viewport_focused = false;
                }
                if studio.is_running() {
                    ui.painter().text(rect.left_top() + egui::vec2(8.0, 6.0), egui::Align2::LEFT_TOP, "Running (F8) · Shift+F5 to stop", egui::FontId::proportional(12.0), Color32::from_rgba_unmultiplied(255, 255, 255, 150));
                }
            }
        }
    });
    viewport.keyboard_focus = state.viewport_focused && !ctx.egui_wants_keyboard_input();
    if studio.loading.is_some() {
        // The modal owns input while a place loads; the native script
        // editor would draw over it, so hide that too.
        viewport.hovered = false;
        viewport.keyboard_focus = false;
        editor_host.set_target(None, None);
    } else if editor_rect.is_some_and(|e| {
        state.settings_rect.is_some_and(|s| e.intersects(s)) || state.emulator_rect.is_some_and(|s| e.intersects(s))
    }) {
        // The native editor would cover the settings windows.
        editor_host.set_target(None, None);
    } else {
        editor_host.set_target(editor_rect, studio.active_script());
    }

    dialogs::windows(&ctx, &mut studio, &mut state, &mut camera_settings);
    settings_window::settings_window(&ctx, &mut state, &mut editor_settings, &mut graphics);
    emulator_window::emulator_window(&ctx, &mut state, &mut emulator_settings, &datastore);
    dialogs::loading_modal(&ctx, &mut studio);
    Ok(())
}

fn tab_strip(ui: &mut egui::Ui, studio: &mut Studio) {
    let titles: Vec<(usize, String, bool)> = {
        let doc = studio.doc();
        studio
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| match t {
                Tab::Viewport => (i, format!("{}{}", doc.title(), if doc.dirty { " *" } else { "" }), false),
                Tab::Script(r) => (i, doc.get(*r).map(|x| x.name.clone()).unwrap_or_else(|| "(removed)".into()), true),
            })
            .collect()
    };
    let mut close = None;
    let mut activate = None;
    Frame::new().fill(theme::BG_TOP).inner_margin(Margin::symmetric(4, 2)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (i, title, closable) in &titles {
                let active = *i == studio.active_tab;
                let fill = if active { theme::BG_PANEL } else { theme::BG_TOP };
                Frame::new().fill(fill).inner_margin(Margin::symmetric(8, 2)).corner_radius(2.0).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let r = ui.add(egui::Label::new(egui::RichText::new(title).color(if active { theme::TEXT } else { theme::TEXT_DIM })).sense(egui::Sense::click()));
                        if r.clicked() {
                            activate = Some(*i);
                        }
                        if *closable && ui.add(egui::Label::new(egui::RichText::new("×").color(theme::TEXT_DIM)).sense(egui::Sense::click())).clicked() {
                            close = Some(*i);
                        }
                    });
                });
            }
        });
    });
    if let Some(i) = activate {
        studio.active_tab = i;
    }
    if let Some(i) = close {
        studio.close_tab(i);
    }
}

fn update_window_title(studio: Res<Studio>, mut window: Query<&mut Window, With<PrimaryWindow>>, mut last: Local<String>) {
    let title = {
        let doc = studio.doc();
        format!("{}{} - Phaedrus", doc.title(), if doc.dirty { "*" } else { "" })
    };
    if *last != title {
        if let Ok(mut w) = window.single_mut() {
            w.title = title.clone();
        }
        *last = title;
    }
}

/// Saves the place (asking for a path if it has none).
pub fn save_place(studio: &mut Studio) {
    shortcuts::save(studio, false);
}
