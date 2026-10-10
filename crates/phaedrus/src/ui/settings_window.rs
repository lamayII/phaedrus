//! File > Phaedrus Settings: script editor, graphics and Open Cloud API
//! settings in one window. Changes apply immediately and are saved by
//! `crate::settings` (the API key goes to the OS credential store instead).

use bevy_egui::egui::{self, RichText};
use phaedrus_script_editor::settings::{EditorColors, EditorSettings};

use super::{UiState, theme};
use crate::opencloud::{ApiAuth, KeyStatus};
use crate::viewport::lighting::{GraphicsSettings, MAX_DRAW_DISTANCE, Technology, draw_distance_studs, profile};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsPage {
    ScriptEditor,
    Graphics,
    ApiManager,
}

impl SettingsPage {
    fn label(self) -> &'static str {
        match self {
            SettingsPage::ScriptEditor => "Script Editor",
            SettingsPage::Graphics => "Graphics",
            SettingsPage::ApiManager => "API Manager",
        }
    }
}

/// Colours that have no effect yet, with why.
fn unused_color_note(label: &str) -> Option<&'static str> {
    label.starts_with("Debugger").then_some("Reserved for the debugger")
}

pub fn settings_window(
    ctx: &egui::Context,
    state: &mut UiState,
    editor: &mut EditorSettings,
    graphics: &mut GraphicsSettings,
    api: &mut ApiAuth,
) {
    let Some(mut page) = state.settings_page else {
        state.settings_rect = None;
        return;
    };
    let mut open = true;
    let response = egui::Window::new("Phaedrus Settings")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([640.0, 540.0])
        .show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(130.0);
                    for p in [SettingsPage::ScriptEditor, SettingsPage::Graphics, SettingsPage::ApiManager] {
                        if ui.selectable_label(page == p, p.label()).clicked() {
                            page = p;
                        }
                    }
                });
                ui.separator();
                ui.vertical(|ui| {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match page {
                        SettingsPage::ScriptEditor => script_editor_page(ui, editor),
                        SettingsPage::Graphics => graphics_page(ui, graphics),
                        SettingsPage::ApiManager => api_manager_page(ui, api),
                    });
                });
            });
        });
    state.settings_rect = response.map(|r| r.response.rect);
    state.settings_page = open.then_some(page);
    if !open {
        state.settings_rect = None;
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(title).strong());
    ui.add_space(2.0);
}

fn script_editor_page(ui: &mut egui::Ui, s: &mut EditorSettings) {
    ui.label(
        RichText::new("Changes apply to open scripts immediately. Move this window off the script editor to see them.")
            .small()
            .color(theme::TEXT_DIM),
    );
    section(ui, "Text");
    egui::Grid::new("editor_text").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Font");
        ui.add(egui::TextEdit::singleline(&mut s.font_family).desired_width(260.0));
        ui.end_row();
        ui.label("Font size");
        ui.add(egui::DragValue::new(&mut s.font_size).range(8..=32));
        ui.end_row();
        ui.label("Tab width");
        ui.add(egui::DragValue::new(&mut s.tab_size).range(1..=8));
        ui.end_row();
        ui.label("Indent with");
        ui.horizontal(|ui| {
            ui.radio_value(&mut s.insert_spaces, false, "Tabs");
            ui.radio_value(&mut s.insert_spaces, true, "Spaces");
        });
        ui.end_row();
        ui.label("Ruler at column");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut s.ruler_column).range(0..=200));
            ui.label(RichText::new("0 = off").small().color(theme::TEXT_DIM));
        });
        ui.end_row();
    });

    section(ui, "Editing");
    ui.checkbox(&mut s.auto_indent, "Auto-indent scoped statements")
        .on_hover_text("Indent after lines like `function f()`, `if x then` and `for ... do`, and outdent `end`, `else` and `until` as they are typed.");
    ui.checkbox(&mut s.auto_close_blocks, "Insert matching \"end\" on Enter")
        .on_hover_text("Pressing Enter after an unclosed `function`, `then` or `do` adds the `end` below the cursor.");
    ui.checkbox(&mut s.auto_close_brackets, "Auto-close brackets and quotes");

    section(ui, "Display");
    ui.checkbox(&mut s.minimap, "Show minimap");
    ui.checkbox(&mut s.show_whitespace, "Show whitespace");
    ui.checkbox(&mut s.rainbow_brackets, "Color brackets by depth")
        .on_hover_text("When off, brackets use the Script Bracket Color.");

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Colors").strong());
        if ui.small_button("Reset colors").clicked() {
            s.colors = EditorColors::default();
        }
    });
    ui.add_space(2.0);
    egui::Grid::new("editor_colors").num_columns(2).spacing([12.0, 4.0]).striped(true).show(ui, |ui| {
        for (label, color) in s.colors.entries_mut() {
            let resp = ui.label(label);
            if let Some(note) = unused_color_note(label) {
                resp.on_hover_text(note);
            }
            ui.horizontal(|ui| {
                ui.color_edit_button_srgb(&mut color.0);
                ui.label(RichText::new(color.hex()).small().monospace().color(theme::TEXT_DIM));
            });
            ui.end_row();
        }
    });

    ui.add_space(8.0);
    if ui.button("Restore script editor defaults").clicked() {
        *s = EditorSettings::default();
    }
}

fn graphics_page(ui: &mut egui::Ui, graphics: &mut GraphicsSettings) {
    section(ui, "Graphics & Lighting");
    egui::Grid::new("gfx").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Quality level");
        ui.add(egui::Slider::new(&mut graphics.quality, 1..=10));
        ui.end_row();
        ui.label("Draw distance");
        ui.horizontal(|ui| {
            ui.add(egui::Slider::new(&mut graphics.draw_distance, 1..=10))
                .on_hover_text("Parts farther than this from the camera are not drawn. Lower is faster in large places.");
            let studs = draw_distance_studs(graphics.draw_distance);
            let text = if studs >= MAX_DRAW_DISTANCE { "unlimited".to_owned() } else { format!("{studs:.0} studs") };
            ui.label(RichText::new(text).small().color(theme::TEXT_DIM));
        });
        ui.end_row();
        ui.label("Lighting technology");
        let label = match graphics.technology_override {
            None => "From place (Lighting.Technology)".to_owned(),
            Some(t) => t.label().to_owned(),
        };
        egui::ComboBox::from_id_salt("tech").selected_text(label).show_ui(ui, |ui| {
            ui.selectable_value(&mut graphics.technology_override, None, "From place");
            for t in [Technology::Voxel, Technology::ShadowMap, Technology::Future] {
                ui.selectable_value(&mut graphics.technology_override, Some(t), t.label());
            }
        });
        ui.end_row();
        ui.label("Background FPS cap");
        ui.add(egui::Slider::new(&mut graphics.background_fps, 1..=60));
        ui.end_row();
        ui.label("Show FPS");
        ui.checkbox(&mut graphics.show_fps, "");
        ui.end_row();
    });
    ui.separator();
    let p = profile(graphics.quality, graphics.technology_override.unwrap_or(Technology::Future));
    ui.label(
        RichText::new(format!(
            "Shadows: {} ({}px, {} cascades, {:.0} studs) · Lights: {} within {:.0} studs, {} with shadows · MSAA: {:?} · SSAO: {} · Bloom: {} · Env lighting: {}",
            if p.shadows { "on" } else { "off" },
            p.shadow_map_size,
            p.cascades,
            p.shadow_distance.min(draw_distance_studs(graphics.draw_distance)),
            p.max_lights,
            p.light_distance.min(draw_distance_studs(graphics.draw_distance)),
            if p.local_shadows { p.max_shadow_lights } else { 0 },
            p.msaa,
            if p.ssao.is_some() { "on" } else { "off" },
            if p.bloom { "on" } else { "off" },
            if p.env_map { "on" } else { "off" },
        ))
        .small()
        .color(theme::TEXT_DIM),
    );
    ui.add_space(8.0);
    if ui.button("Restore graphics defaults").clicked() {
        *graphics = GraphicsSettings::default();
    }
}

fn api_manager_page(ui: &mut egui::Ui, api: &mut ApiAuth) {
    api.poll();
    ui.label(
        RichText::new(
            "Phaedrus downloads union meshes through Roblox Open Cloud. Without an API key, places still open, \
             but those unions render as bounding boxes.",
        )
        .small()
        .color(theme::TEXT_DIM),
    );

    section(ui, "API key");
    let draft_id = ui.id().with("api_key_draft");
    let show_id = ui.id().with("api_key_show");
    let mut draft: String = ui.data_mut(|d| d.get_temp(draft_id)).unwrap_or_default();
    let mut show: bool = ui.data_mut(|d| d.get_temp(show_id)).unwrap_or(false);
    egui::Grid::new("api_key").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Active key");
        match api.key() {
            Some(k) => ui.label(RichText::new(k.hint()).monospace()),
            None => ui.label(RichText::new("None").color(theme::WARN)),
        };
        ui.end_row();
        ui.label(if api.key().is_some() { "Replace key" } else { "Paste key" });
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut draft).password(!show).desired_width(260.0).hint_text("Open Cloud API key"));
            ui.checkbox(&mut show, "Show");
        });
        ui.end_row();
    });
    ui.horizontal(|ui| {
        if ui.add_enabled(!draft.trim().is_empty(), egui::Button::new("Save key")).clicked() {
            match api.set_key(&draft) {
                Ok(()) => {
                    draft.clear();
                    show = false;
                }
                Err(e) => api.status = KeyStatus::Invalid(e.to_owned()),
            }
        }
        if ui.add_enabled(api.key().is_some(), egui::Button::new("Remove key")).clicked() {
            api.remove_key();
        }
    });
    ui.data_mut(|d| {
        d.insert_temp(draft_id, draft);
        d.insert_temp(show_id, show);
    });

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Test with universe ID");
        ui.add(egui::TextEdit::singleline(&mut api.test_universe).desired_width(120.0));
        let checking = api.status == KeyStatus::Checking;
        if ui.add_enabled(api.key().is_some() && !checking, egui::Button::new("Test")).clicked() {
            api.start_check();
        }
    });
    match &api.status {
        KeyStatus::Unchecked => {}
        KeyStatus::Checking => {
            ui.label(RichText::new("Checking...").color(theme::TEXT_DIM));
        }
        KeyStatus::Valid(m) => {
            ui.label(RichText::new(m).color(theme::INFO_BLUE));
        }
        KeyStatus::Invalid(m) => {
            ui.label(RichText::new(m).color(theme::ERROR));
        }
    }
    if let Some(e) = &api.storage_error {
        ui.label(RichText::new(e).color(theme::WARN));
    }
    ui.label(
        RichText::new("The test only confirms the key is accepted. Union meshes that fail to download are reported in Output.")
            .small()
            .color(theme::TEXT_DIM),
    );

    section(ui, "Creating a key");
    doc_lines(
        ui,
        &[
            "1. Open create.roblox.com > Creator Dashboard > Open Cloud > API Keys and click Create API Key. \
             Create it under your own account: the asset scope is not available for group keys.",
            "2. Under Access Permissions, add the \"Legacy Assets\" API system with the legacy-asset:manage \
             scope. Phaedrus uses it only to download asset content (GET /asset-delivery-api/v1/assetId/{id}).",
            "3. Reading universe info (the Test button above, GET /cloud/v2/universes/{id}) needs no extra scope. \
             Do not add write scopes: Phaedrus never changes anything on Roblox.",
            "4. Set Accepted IP Addresses to your own public IP address rather than 0.0.0.0/0, and set an \
             expiration date.",
            "5. Copy the key and paste it above. Roblox shows it only once.",
        ],
    );
    ui.label(
        RichText::new("A key can only download assets its owner can access. Assets from other creators may be skipped.")
            .small()
            .color(theme::TEXT_DIM),
    );

    section(ui, "Keeping the key safe");
    ui.label(
        RichText::new("Anyone with this key can act as your account within its scopes until it expires or is revoked.")
            .color(theme::WARN),
    );
    doc_lines(
        ui,
        &[
            "• Phaedrus stores the key in the Windows Credential Manager, not in settings.json or place files, \
             and never prints it to Output. Anyone signed into your Windows account can still read it.",
            "• Never paste the key into scripts, places, models, screenshots, bug reports, chats or git \
             repositories.",
            "• Grant only legacy-asset:manage, restrict the key to your IP address, and give it an expiration date.",
            "• Phaedrus sends the key only to apis.roblox.com, over HTTPS. Asset files are then downloaded \
             from Roblox's CDN without the key.",
            "• If the key may have leaked, revoke it at once (Creator Dashboard > Open Cloud > API Keys), \
             click Remove key here, and create a new one.",
        ],
    );
}

fn doc_lines(ui: &mut egui::Ui, lines: &[&str]) {
    for line in lines {
        ui.label(RichText::new(*line).small());
    }
}
