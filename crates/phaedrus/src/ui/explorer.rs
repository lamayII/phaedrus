//! Explorer panel: instance tree with search, rename, drag-and-drop and a
//! context menu.

use bevy_egui::egui::{self, Color32, Sense, vec2};
use phaedrus_dom::types::Ref;
use phaedrus_dom::{Document, reflection};

use super::{UiState, icons, theme};
use crate::studio::Studio;

const ROW_H: f32 = 18.0;

/// Services Studio hides from the Explorer by default.
fn hidden_service(class: &str) -> bool {
    !matches!(
        class,
        "Workspace"
            | "Players"
            | "Lighting"
            | "MaterialService"
            | "NetworkClient"
            | "ReplicatedFirst"
            | "ReplicatedStorage"
            | "ServerScriptService"
            | "ServerStorage"
            | "StarterGui"
            | "StarterPack"
            | "StarterPlayer"
            | "Teams"
            | "SoundService"
            | "TextChatService"
            | "VoiceChatService"
            | "Chat"
            | "LocalizationService"
            | "TestService"
    )
}

/// Explorer sort order for services (Studio's fixed order).
fn service_rank(class: &str) -> usize {
    const ORDER: &[&str] = &[
        "Workspace", "Players", "Lighting", "MaterialService", "NetworkClient", "ReplicatedFirst",
        "ReplicatedStorage", "ServerScriptService", "ServerStorage", "StarterGui", "StarterPack",
        "StarterPlayer", "Teams", "SoundService", "TextChatService", "VoiceChatService",
    ];
    ORDER.iter().position(|c| *c == class).unwrap_or(ORDER.len())
}

fn visible_children(doc: &Document, r: Ref) -> Vec<Ref> {
    let root = doc.root();
    let mut kids: Vec<Ref> = doc
        .children(r)
        .iter()
        .copied()
        .filter(|c| r != root || doc.get(*c).is_some_and(|i| !hidden_service(&i.class)))
        .collect();
    if r == root {
        kids.sort_by_key(|c| doc.get(*c).map(|i| service_rank(&i.class)).unwrap_or(99));
    } else {
        // Priority classes first, then alphabetical (case-insensitive).
        // The sort is stable, so equal names keep their tree order.
        kids.sort_by_cached_key(|c| match doc.get(*c) {
            Some(i) => (class_rank(&i.class), i.name.to_lowercase()),
            None => (u8::MAX, String::new()),
        });
    }
    kids
}

/// Classes pinned above everything else in a parent's child list.
fn class_rank(class: &str) -> u8 {
    match class {
        "Camera" => 0,
        "Terrain" => 1,
        "Folder" => 2,
        _ => 3,
    }
}

struct Row {
    r: Ref,
    depth: usize,
    has_children: bool,
}

fn flatten(doc: &Document, state: &UiState) -> Vec<Row> {
    let mut rows = Vec::new();
    let filter = state.explorer_filter.trim().to_lowercase();
    if !filter.is_empty() {
        // Flat list of matches with their depth preserved for context.
        let root = doc.root();
        for c in visible_children(doc, root) {
            for r in std::iter::once(c).chain(doc.descendants(c)) {
                let Some(i) = doc.get(r) else { continue };
                if i.name.to_lowercase().contains(&filter) || i.class.to_lowercase().contains(&filter) {
                    rows.push(Row { r, depth: 0, has_children: false });
                }
            }
        }
        return rows;
    }
    fn walk(doc: &Document, state: &UiState, r: Ref, depth: usize, rows: &mut Vec<Row>) {
        for c in visible_children(doc, r) {
            let has_children = !doc.children(c).is_empty();
            rows.push(Row { r: c, depth, has_children });
            if has_children && state.expanded.contains(&c) {
                walk(doc, state, c, depth + 1, rows);
            }
        }
    }
    walk(doc, state, doc.root(), 0, &mut rows);
    rows
}

pub fn explorer_panel(ui: &mut egui::Ui, studio: &mut Studio, state: &mut UiState) {
    // Search box.
    egui::Frame::new().inner_margin(egui::Margin::symmetric(4, 3)).show(ui, |ui| {
        let resp = ui.add(
            egui::TextEdit::singleline(&mut state.explorer_filter)
                .hint_text("Filter workspace (Ctrl+Shift+X)")
                .desired_width(f32::INFINITY),
        );
        if state.focus_explorer_filter {
            resp.request_focus();
            state.focus_explorer_filter = false;
        }
    });

    // Reveal selected instances (expand ancestors) when selection changes
    // from the viewport.
    if let Some(target) = state.explorer_scroll_to {
        let doc = studio.doc();
        let mut cur = doc.parent(target);
        while cur.is_some() {
            state.expanded.insert(cur);
            cur = doc.parent(cur);
        }
    }

    let rows = flatten(&studio.doc(), state);
    let scroll_to = state.explorer_scroll_to.take();
    let scroll_index = scroll_to.and_then(|t| rows.iter().position(|r| r.r == t));

    let mut actions: Vec<Action> = Vec::new();
    let mut scroll = egui::ScrollArea::both().auto_shrink([false, false]);
    if let Some(i) = scroll_index {
        scroll = scroll.vertical_scroll_offset((i as f32 * ROW_H - 80.0).max(0.0));
    }
    let total_rows = rows.len();
    scroll.show_rows(ui, ROW_H, total_rows, |ui, range| {
        let doc = studio.doc();
        for row in &rows[range] {
            draw_row(ui, &doc, studio, state, row, &mut actions);
        }
        drop(doc);
        // Empty space click clears selection.
        let rest = ui.allocate_response(ui.available_size().max(vec2(0.0, 4.0)), Sense::click());
        if rest.clicked() {
            actions.push(Action::ClearSelection);
        }
        rest.context_menu(|ui| {
            context_menu(ui, studio, state);
        });
    });

    // Drop target handling.
    if state.dragging.is_some() && !ui.input(|i| i.pointer.any_down()) {
        state.dragging = None;
    }

    for a in actions {
        apply(a, studio, state);
    }
}

enum Action {
    Select(Ref),
    Toggle(Ref),
    SelectRange(Ref),
    ClearSelection,
    Expand(Ref, bool),
    OpenScript(Ref),
    Rename(Ref, String),
    Drop(Vec<Ref>, Ref),
}

fn draw_row(ui: &mut egui::Ui, doc: &Document, studio: &Studio, state: &mut UiState, row: &Row, actions: &mut Vec<Action>) {
    let Some(inst) = doc.get(row.r) else { return };
    let width = ui.available_width().max(160.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::click_and_drag());
    let selected = studio.is_selected(row.r);
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, 0.0, theme::SELECTION);
    } else if resp.hovered() {
        painter.rect_filled(rect, 0.0, theme::BG_HOVER);
    }
    // Drop highlight.
    if state.dragging.is_some() && resp.contains_pointer() {
        painter.rect_stroke(rect, 0.0, egui::Stroke::new(1.0, theme::ACCENT), egui::StrokeKind::Inside);
    }
    let indent = 6.0 + row.depth as f32 * 16.0;
    let arrow_rect = egui::Rect::from_min_size(rect.min + vec2(indent, 2.0), vec2(14.0, 14.0));
    if row.has_children {
        let open = state.expanded.contains(&row.r);
        let c = arrow_rect.center();
        let pts = if open {
            vec![c + vec2(-3.5, -1.5), c + vec2(3.5, -1.5), c + vec2(0.0, 2.5)]
        } else {
            vec![c + vec2(-1.5, -3.5), c + vec2(2.5, 0.0), c + vec2(-1.5, 3.5)]
        };
        painter.add(egui::Shape::convex_polygon(pts, theme::TEXT_DIM, egui::Stroke::NONE));
        let ar = ui.interact(arrow_rect, ui.id().with(("arrow", row.r)), Sense::click());
        if ar.clicked() {
            actions.push(Action::Expand(row.r, !open));
        }
    }
    let icon_rect = egui::Rect::from_min_size(rect.min + vec2(indent + 16.0, 2.0), vec2(14.0, 14.0));
    icons::class_icon(painter, icon_rect, &inst.class);
    let text_pos = rect.min + vec2(indent + 35.0, 0.0);

    if let Some((rr, buf)) = &mut state.renaming
        && *rr == row.r {
            let edit_rect = egui::Rect::from_min_size(text_pos, vec2((rect.right() - text_pos.x - 4.0).max(60.0), ROW_H));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(edit_rect));
            let te = child.add(egui::TextEdit::singleline(buf).desired_width(edit_rect.width()));
            te.request_focus();
            if te.lost_focus() {
                let commit = !child.input(|i| i.key_pressed(egui::Key::Escape));
                let name = buf.clone();
                let r = row.r;
                state.renaming = None;
                if commit && !name.is_empty() {
                    actions.push(Action::Rename(r, name));
                }
            }
            return;
        }
    painter.text(
        text_pos + vec2(0.0, ROW_H * 0.5),
        egui::Align2::LEFT_CENTER,
        &inst.name,
        egui::FontId::proportional(12.5),
        if selected { Color32::WHITE } else { theme::TEXT },
    );

    let modifiers = ui.input(|i| i.modifiers);
    if resp.clicked() {
        if modifiers.ctrl || modifiers.command {
            actions.push(Action::Toggle(row.r));
        } else if modifiers.shift {
            actions.push(Action::SelectRange(row.r));
        } else {
            actions.push(Action::Select(row.r));
        }
    }
    if resp.double_clicked() {
        if reflection::is_a(&inst.class, "LuaSourceContainer") {
            actions.push(Action::OpenScript(row.r));
        } else if row.has_children {
            actions.push(Action::Expand(row.r, !state.expanded.contains(&row.r)));
        }
    }
    if resp.drag_started() && doc.parent(row.r) != doc.root() {
        let dragged = if selected { studio.selection_roots_in(doc) } else { vec![row.r] };
        state.dragging = Some(dragged);
    }
    if let Some(dragged) = &state.dragging
        && resp.contains_pointer() && ui.input(|i| i.pointer.any_released()) && !dragged.contains(&row.r) {
            actions.push(Action::Drop(dragged.clone(), row.r));
        }
    resp.context_menu(|ui| {
        if !selected {
            actions.push(Action::Select(row.r));
        }
        context_menu(ui, studio, state);
    });
}

fn context_menu(ui: &mut egui::Ui, studio: &Studio, state: &mut UiState) {
    // Commands are deferred through `UiState::pending_menu` so the studio
    // borrow stays shared here.
    let has_sel = !studio.selection.is_empty();
    if ui.button("Insert Object...").clicked() {
        state.insert_dialog = Some(Default::default());
        ui.close();
    }
    ui.separator();
    for (label, shortcut, cmd) in [
        ("Cut", "Ctrl+X", MenuCmd::Cut),
        ("Copy", "Ctrl+C", MenuCmd::Copy),
        ("Paste Into", "Ctrl+Shift+V", MenuCmd::PasteInto),
        ("Duplicate", "Ctrl+D", MenuCmd::Duplicate),
        ("Delete", "Del", MenuCmd::Delete),
        ("Rename", "F2", MenuCmd::Rename),
    ] {
        let enabled = has_sel || matches!(cmd, MenuCmd::PasteInto);
        if ui.add_enabled(enabled, egui::Button::new(label).shortcut_text(shortcut)).clicked() {
            state.pending_menu = Some(cmd);
            ui.close();
        }
    }
    ui.separator();
    for (label, shortcut, cmd) in [
        ("Group", "Ctrl+G", MenuCmd::Group),
        ("Ungroup", "Ctrl+U", MenuCmd::Ungroup),
        ("Select Children", "", MenuCmd::SelectChildren),
        ("Zoom to", "F", MenuCmd::ZoomTo),
    ] {
        if ui.add_enabled(has_sel, egui::Button::new(label).shortcut_text(shortcut)).clicked() {
            state.pending_menu = Some(cmd);
            ui.close();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuCmd {
    Cut,
    Copy,
    PasteInto,
    Duplicate,
    Delete,
    Rename,
    Group,
    Ungroup,
    SelectChildren,
    ZoomTo,
}

pub fn run_menu_cmd(cmd: MenuCmd, studio: &mut Studio, state: &mut UiState) {
    match cmd {
        MenuCmd::Cut => studio.cut_selection(),
        MenuCmd::Copy => studio.copy_selection(),
        MenuCmd::PasteInto => studio.paste(true),
        MenuCmd::Duplicate => studio.duplicate_selection(),
        MenuCmd::Delete => studio.delete_selection(),
        MenuCmd::Rename => {
            if let Some(&r) = studio.selection.first() {
                let name = studio.doc().get(r).map(|i| i.name.clone()).unwrap_or_default();
                state.renaming = Some((r, name));
            }
        }
        MenuCmd::Group => studio.group_selection(),
        MenuCmd::Ungroup => studio.ungroup_selection(),
        MenuCmd::SelectChildren => {
            let kids: Vec<Ref> = {
                let doc = studio.doc();
                studio.selection.iter().flat_map(|r| doc.children(*r).to_vec()).collect()
            };
            for r in &studio.selection {
                state.expanded.insert(*r);
            }
            studio.selection = kids;
        }
        MenuCmd::ZoomTo => studio.focus_request = true,
    }
}

fn apply(a: Action, studio: &mut Studio, state: &mut UiState) {
    match a {
        Action::Select(r) => studio.select_only(r),
        Action::Toggle(r) => studio.toggle_select(r),
        Action::SelectRange(r) => {
            // Select siblings between the last selected and r.
            let Some(&anchor) = studio.selection.last() else {
                studio.select_only(r);
                return;
            };
            let range = {
                let doc = studio.doc();
                let parent = doc.parent(r);
                if doc.parent(anchor) != parent {
                    vec![r]
                } else {
                    let kids = visible_children(&doc, parent);
                    let a = kids.iter().position(|x| *x == anchor).unwrap_or(0);
                    let b = kids.iter().position(|x| *x == r).unwrap_or(0);
                    kids[a.min(b)..=a.max(b)].to_vec()
                }
            };
            studio.selection = range;
        }
        Action::ClearSelection => studio.selection.clear(),
        Action::Expand(r, open) => {
            if open {
                state.expanded.insert(r);
            } else {
                state.expanded.remove(&r);
            }
        }
        Action::OpenScript(r) => studio.open_script(r),
        Action::Rename(r, name) => {
            let _ = studio.doc().set_property(r, "Name", phaedrus_dom::Variant::String(name));
            studio.waypoint("Rename");
        }
        Action::Drop(refs, target) => {
            let mut moved = false;
            {
                let mut doc = studio.doc();
                for r in refs {
                    if doc.set_parent(r, target).is_ok() {
                        moved = true;
                    }
                }
            }
            if moved {
                state.expanded.insert(target);
                studio.waypoint("Reparent");
            }
            state.dragging = None;
        }
    }
}
