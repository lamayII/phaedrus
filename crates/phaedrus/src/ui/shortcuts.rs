//! Studio keyboard shortcuts.

use bevy_egui::egui::{self, Key, KeyboardShortcut, Modifiers};
use phaedrus_luau::OutputLevel;

use super::UiState;
use super::explorer::{MenuCmd, run_menu_cmd};
use crate::studio::{Studio, Tool};

fn pressed(ctx: &egui::Context, mods: Modifiers, key: Key) -> bool {
    ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(mods, key)))
}

pub fn handle(ctx: &egui::Context, studio: &mut Studio, state: &mut UiState) {
    let cmd = Modifiers::COMMAND;
    let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;

    // File shortcuts work everywhere.
    if pressed(ctx, cmd, Key::S) {
        save(studio, false);
    }
    if pressed(ctx, cmd_shift, Key::S) {
        save(studio, true);
    }
    if pressed(ctx, cmd, Key::O) {
        open(studio);
    }
    if pressed(ctx, cmd, Key::N) {
        studio.replace_document(phaedrus_dom::Document::new_place());
    }
    if pressed(ctx, Modifiers::NONE, Key::F8) {
        studio.pending_run = Some(true);
    }
    if pressed(ctx, Modifiers::NONE, Key::F5) {
        studio.pending_run = Some(true);
        studio.log(OutputLevel::Info, "Play mode is not implemented yet; running server scripts (F8) instead.");
    }
    if pressed(ctx, Modifiers::SHIFT, Key::F5) {
        studio.pending_stop = true;
    }
    if pressed(ctx, cmd_shift, Key::X) {
        state.show_explorer = true;
        state.focus_explorer_filter = true;
    }
    if pressed(ctx, cmd_shift, Key::P) {
        state.show_properties = true;
        state.focus_properties_filter = true;
    }
    if pressed(ctx, cmd, Key::W) {
        // Close the active script tab.
        let i = studio.active_tab;
        studio.close_tab(i);
    }

    // Everything below is suppressed while typing in a text field or when
    // a script editor tab is active.
    if ctx.egui_wants_keyboard_input() || studio.active_script().is_some() {
        return;
    }
    if pressed(ctx, cmd, Key::Z) {
        studio.undo();
    }
    if pressed(ctx, cmd, Key::Y) || pressed(ctx, cmd_shift, Key::Z) {
        studio.redo();
    }
    for (key, tool) in [(Key::Num1, Tool::Select), (Key::Num2, Tool::Move), (Key::Num3, Tool::Scale), (Key::Num4, Tool::Rotate)] {
        if pressed(ctx, cmd, key) {
            studio.tool = tool;
        }
    }
    if pressed(ctx, cmd, Key::L) {
        studio.local_space = !studio.local_space;
    }
    if pressed(ctx, cmd, Key::D) {
        studio.duplicate_selection();
    }
    // The egui integration turns Ctrl+C/X/V into clipboard events.
    let (events, shift) = ctx.input(|i| (i.events.clone(), i.modifiers.shift));
    for e in events {
        match e {
            egui::Event::Copy => studio.copy_selection(),
            egui::Event::Cut => studio.cut_selection(),
            egui::Event::Paste(_) => studio.paste(shift),
            _ => {}
        }
    }
    if pressed(ctx, cmd, Key::G) {
        studio.group_selection();
    }
    if pressed(ctx, cmd, Key::U) {
        studio.ungroup_selection();
    }
    if pressed(ctx, cmd, Key::A) {
        // Select all top-level children of Workspace.
        let kids = {
            let doc = studio.doc();
            doc.workspace().map(|w| doc.children(w).to_vec()).unwrap_or_default()
        };
        studio.selection = kids;
    }
    if pressed(ctx, Modifiers::NONE, Key::Delete) || pressed(ctx, Modifiers::NONE, Key::Backspace) {
        studio.delete_selection();
    }
    if pressed(ctx, Modifiers::NONE, Key::F2) {
        run_menu_cmd(MenuCmd::Rename, studio, state);
    }
    if pressed(ctx, Modifiers::NONE, Key::F) && state.viewport_focused {
        studio.focus_request = true;
    }
    if pressed(ctx, Modifiers::NONE, Key::Escape) {
        studio.selection.clear();
    }
}

pub fn save(studio: &mut Studio, save_as: bool) {
    let has_path = studio.doc().path.is_some();
    if save_as || !has_path {
        let title = studio.doc().title();
        if let Some(p) = rfd::FileDialog::new()
            .add_filter("Roblox Place", &["rbxl", "rbxlx"])
            .add_filter("Roblox Model", &["rbxm", "rbxmx"])
            .set_file_name(format!("{title}.rbxl"))
            .save_file()
        {
            studio.save(Some(p));
        }
    } else {
        studio.save(None);
    }
}

pub fn open(studio: &mut Studio) {
    if studio.is_running() {
        studio.log(OutputLevel::Warn, "Stop the running session before opening another file.");
        return;
    }
    if let Some(p) = rfd::FileDialog::new()
        .add_filter("Roblox files", &["rbxl", "rbxlx", "rbxm", "rbxmx"])
        .pick_file()
    {
        studio.open(p);
    }
}
