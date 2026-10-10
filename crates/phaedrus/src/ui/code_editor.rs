//! Built-in script editor used when the Monaco web view is unavailable.

use bevy_egui::egui::{self, Color32, FontId, text::LayoutJob};
use phaedrus_dom::types::{Ref, Variant};
use phaedrus_script_editor::settings::{EditorColors, Rgb};

use super::UiState;
use crate::studio::Studio;

#[derive(Default)]
pub struct FallbackEditorState {
    /// Script currently loaded into `buffer`.
    current: Option<Ref>,
    buffer: String,
    dirty: bool,
}

const KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in", "local", "nil", "not",
    "or", "repeat", "return", "then", "true", "until", "while", "continue", "export", "type",
];
const GLOBALS: &[&str] = &[
    "game", "workspace", "script", "print", "warn", "error", "task", "wait", "Instance", "Vector3", "Vector2",
    "CFrame", "Color3", "BrickColor", "UDim", "UDim2", "Enum", "math", "string", "table", "typeof", "type",
    "pairs", "ipairs", "require", "tick", "time", "coroutine", "tostring", "tonumber", "select", "pcall",
];

/// Very small Luau highlighter: keywords, globals, strings, numbers, comments.
pub fn highlight(text: &str, colors: &EditorColors) -> LayoutJob {
    let col = |rgb: Rgb| Color32::from_rgb(rgb.0[0], rgb.0[1], rgb.0[2]);
    let mut job = LayoutJob::default();
    let font = FontId::monospace(13.0);
    let push = |job: &mut LayoutJob, s: &str, color: Color32| {
        job.append(s, 0.0, egui::TextFormat::simple(font.clone(), color));
    };
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        let start = i;
        if text[i..].starts_with("--") {
            let end = text[i..].find('\n').map(|n| i + n).unwrap_or(text.len());
            push(&mut job, &text[i..end], col(colors.comment));
            i = end;
        } else if c == '"' || c == '\'' || c == '`' {
            i += 1;
            while i < bytes.len() && bytes[i] as char != c && bytes[i] != b'\n' {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(bytes.len());
            push(&mut job, &text[start..i], col(colors.string));
        } else if c.is_ascii_digit() {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.' || bytes[i] == b'_') {
                i += 1;
            }
            push(&mut job, &text[start..i], col(colors.number));
        } else if c.is_ascii_alphabetic() || c == '_' {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &text[start..i];
            let color = if KEYWORDS.contains(&word) {
                col(colors.keyword)
            } else if GLOBALS.contains(&word) {
                col(colors.builtin_function)
            } else {
                col(colors.text)
            };
            push(&mut job, word, color);
        } else {
            let ch_len = text[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            i += ch_len;
            push(&mut job, &text[start..i], col(colors.operator));
        }
    }
    job
}

pub fn fallback_editor(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    script: Ref,
    studio: &mut Studio,
    state: &mut UiState,
    colors: &EditorColors,
) {
    let ed = &mut state.code_editor;
    if ed.current != Some(script) {
        if ed.dirty
            && let Some(prev) = ed.current {
                commit(studio, prev, &ed.buffer);
            }
        ed.current = Some(script);
        ed.buffer = match studio.doc().get_property(script, "Source") {
            Some(Variant::String(s)) => s,
            _ => String::new(),
        };
        ed.dirty = false;
    }
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    egui::Frame::new().fill(Color32::from_rgb(colors.background.0[0], colors.background.0[1], colors.background.0[2])).show(&mut child, |ui| {
        ui.set_min_size(rect.size());
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap: f32| {
                let mut job = highlight(text.as_str(), colors);
                job.wrap.max_width = wrap;
                ui.fonts_mut(|f| f.layout_job(job))
            };
            let resp = ui.add(
                egui::TextEdit::multiline(&mut ed.buffer)
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .desired_rows(40)
                    .lock_focus(true)
                    .frame(egui::Frame::NONE)
                    .layouter(&mut layouter),
            );
            if resp.changed() {
                ed.dirty = true;
            }
            if resp.lost_focus() && ed.dirty {
                commit(studio, script, &ed.buffer);
                ed.dirty = false;
            }
        });
    });
    if child.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S)) && ed.dirty {
        commit(studio, script, &ed.buffer);
        ed.dirty = false;
    }
}

fn commit(studio: &mut Studio, script: Ref, source: &str) {
    let changed = studio.doc().get_property(script, "Source") != Some(Variant::String(source.to_owned()));
    if changed {
        let _ = studio.doc().set_property(script, "Source", Variant::String(source.to_owned()));
        studio.waypoint("Edit Script");
    }
}
