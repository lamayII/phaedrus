//! Output window and command bar.

use bevy_egui::egui::{self, RichText};
use phaedrus_luau::OutputLevel;

use super::{UiState, theme};
use crate::studio::Studio;

/// Keeps the output buffer bounded so long sessions don't grow memory.
const MAX_LINES: usize = 5000;

pub fn output_panel(ui: &mut egui::Ui, studio: &mut Studio, state: &mut UiState) {
    {
        let mut out = studio.output.lock();
        if out.len() > MAX_LINES {
            let excess = out.len() - MAX_LINES;
            out.drain(..excess);
        }
    }
    egui::Frame::new().inner_margin(egui::Margin::symmetric(6, 3)).show(ui, |ui| {
        ui.horizontal(|ui| {
            let f = &mut state.output_filter;
            ui.toggle_value(&mut f.show_info, "Messages");
            ui.toggle_value(&mut f.show_warn, "Warnings");
            ui.toggle_value(&mut f.show_error, "Errors");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Clear").on_hover_text("Clear output").clicked() {
                    studio.output.lock().clear();
                }
                ui.add(egui::TextEdit::singleline(&mut f.text).hint_text("Filter...").desired_width(180.0));
            });
        });
    });
    let filter = state.output_filter.text.to_lowercase();
    let lines = studio.output.lock();
    let visible: Vec<_> = lines
        .iter()
        .filter(|l| match l.level {
            OutputLevel::Info => state.output_filter.show_info,
            OutputLevel::Warn => state.output_filter.show_warn,
            OutputLevel::Error => state.output_filter.show_error,
        })
        .filter(|l| filter.is_empty() || l.text.to_lowercase().contains(&filter))
        .collect();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show_rows(ui, 16.0, visible.len(), |ui, range| {
            for line in &visible[range] {
                let time = line
                    .time
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| {
                        let secs = d.as_secs();
                        let local = secs % 86400;
                        format!("{:02}:{:02}:{:02}.{:03}", local / 3600, (local / 60) % 60, local % 60, d.subsec_millis())
                    })
                    .unwrap_or_default();
                let color = match line.level {
                    OutputLevel::Info => theme::TEXT,
                    OutputLevel::Warn => theme::WARN,
                    OutputLevel::Error => theme::ERROR,
                };
                let first = line.text.lines().next().unwrap_or("");
                ui.horizontal(|ui| {
                    ui.label(RichText::new(time).monospace().color(theme::TEXT_DIM));
                    let resp = ui.label(RichText::new(first).monospace().color(color));
                    if line.text.contains('\n') {
                        resp.on_hover_text(RichText::new(&line.text).monospace());
                    }
                    if let Some(src) = &line.source {
                        ui.label(RichText::new(format!("- {src}")).monospace().color(theme::INFO_BLUE));
                    }
                });
            }
        });
}

pub fn command_bar(ui: &mut egui::Ui, studio: &mut Studio, state: &mut UiState) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(">").monospace().color(theme::TEXT_DIM));
        let run_w = 50.0;
        let resp = ui.add(
            egui::TextEdit::singleline(&mut state.command)
                .font(egui::TextStyle::Monospace)
                .hint_text("Run a command (Enter), Up/Down for history")
                .desired_width(ui.available_width() - run_w),
        );
        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if resp.has_focus() {
            let (up, down) = ui.input(|i| (i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown)));
            if up && !state.command_history.is_empty() {
                let pos = state.command_history_pos.map(|p| p.saturating_sub(1)).unwrap_or(state.command_history.len() - 1);
                state.command_history_pos = Some(pos);
                state.command = state.command_history[pos].clone();
            }
            if down
                && let Some(p) = state.command_history_pos {
                    if p + 1 < state.command_history.len() {
                        state.command_history_pos = Some(p + 1);
                        state.command = state.command_history[p + 1].clone();
                    } else {
                        state.command_history_pos = None;
                        state.command.clear();
                    }
                }
        }
        let run = ui.button("▶ Run").clicked();
        if (enter || run) && !state.command.trim().is_empty() {
            let cmd = std::mem::take(&mut state.command);
            studio.log(OutputLevel::Info, format!("> {cmd}"));
            if state.command_history.last() != Some(&cmd) {
                state.command_history.push(cmd.clone());
            }
            state.command_history_pos = None;
            studio.pending_exec.push(cmd);
            resp.request_focus();
        }
    });
}
