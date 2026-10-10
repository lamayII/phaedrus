//! Test > Emulators: settings for the mock Roblox services and a browser
//! for the data they store. Settings are saved by `crate::settings`.

use bevy_egui::egui::{self, Color32, RichText};
use phaedrus_luau::datastore::{FailureCode, KeySummary, Operation};
use phaedrus_luau::{DataStoreConfig, SharedDataStore};

use super::{UiState, theme};
use crate::settings::EmulatorSettings;

/// Rows shown in the data browser before asking for a narrower filter.
const MAX_ROWS: usize = 500;
/// Characters of each value shown in the data browser.
const VALUE_PREVIEW: usize = 160;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmulatorPage {
    DataStore,
    DataStoreData,
}

impl EmulatorPage {
    fn label(self) -> &'static str {
        match self {
            EmulatorPage::DataStore => "DataStoreService",
            EmulatorPage::DataStoreData => "Stored data",
        }
    }
}

/// Data browser state.
#[derive(Default)]
pub struct DataBrowser {
    filter: String,
    /// Keys at the emulator revision they were read at.
    cache: Option<(u64, Vec<KeySummary>)>,
    confirm_clear: bool,
}

pub fn emulator_window(
    ctx: &egui::Context,
    state: &mut UiState,
    settings: &mut EmulatorSettings,
    datastore: &SharedDataStore,
) {
    let Some(mut page) = state.emulator_page else {
        state.emulator_rect = None;
        return;
    };
    let mut open = true;
    let response = egui::Window::new("Emulator Settings")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([680.0, 560.0])
        // Below the ribbon, so the Test tab's emulator buttons stay visible.
        .default_pos([290.0, 140.0])
        .show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(130.0);
                    for p in [EmulatorPage::DataStore, EmulatorPage::DataStoreData] {
                        let indent = if p == EmulatorPage::DataStoreData { "    " } else { "" };
                        if ui.selectable_label(page == p, format!("{indent}{}", p.label())).clicked() {
                            page = p;
                        }
                    }
                    ui.add_space(8.0);
                    ui.label(RichText::new("More services coming").small().color(theme::TEXT_DIM));
                });
                ui.separator();
                ui.vertical(|ui| {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match page {
                        EmulatorPage::DataStore => datastore_page(ui, &mut settings.datastore, datastore),
                        EmulatorPage::DataStoreData => data_page(ui, &mut state.data_browser, datastore),
                    });
                });
            });
        });
    state.emulator_rect = response.map(|r| r.response.rect);
    state.emulator_page = open.then_some(page);
    if !open {
        state.emulator_rect = None;
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(8.0);
    ui.label(RichText::new(title).strong());
    ui.add_space(2.0);
}

fn note(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().color(theme::TEXT_DIM));
}

fn open_in_file_manager(path: &std::path::Path) {
    let _ = std::fs::create_dir_all(path);
    let program = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program).arg(path).spawn();
}

fn code_combo(ui: &mut egui::Ui, id: egui::Id, code: &mut FailureCode) {
    egui::ComboBox::from_id_salt(id).selected_text(code.label()).width(170.0).show_ui(ui, |ui| {
        for c in [FailureCode::InternalError500, FailureCode::BadGateway502, FailureCode::Either] {
            ui.selectable_value(code, c, c.label());
        }
    });
}

fn datastore_page(ui: &mut egui::Ui, ds: &mut DataStoreConfig, emulator: &SharedDataStore) {
    ui.checkbox(&mut ds.enabled, RichText::new("Enable DataStoreService emulator").strong());
    note(
        ui,
        "When on, DataStoreService, DataStore, OrderedDataStore and their pages run against local data \
         saved for this place file. When off, data store calls fail as they do in an unpublished place.",
    );

    section(ui, "Storage");
    match emulator.data_file() {
        Some(file) => {
            ui.label(RichText::new(file.display().to_string()).small().monospace());
        }
        None => note(ui, "No place loaded."),
    }
    note(ui, "Each place file gets its own data. Unsaved places share one scratch file.");
    ui.horizontal(|ui| {
        if ui.button("Open data folder").clicked() {
            open_in_file_manager(&emulator.root());
        }
    });

    section(ui, "Requests");
    egui::Grid::new("ds_requests").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Latency");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut ds.latency_ms).range(0..=10_000).suffix(" ms"));
            ui.label("+ up to");
            ui.add(egui::DragValue::new(&mut ds.latency_jitter_ms).range(0..=10_000).suffix(" ms"));
            ui.label(RichText::new("random").small().color(theme::TEXT_DIM));
        });
        ui.end_row();
        ui.label("Request budgets");
        ui.checkbox(&mut ds.enforce_budgets, "Queue requests over budget")
            .on_hover_text(
                "Like Roblox: requests beyond GetRequestBudgetForRequestType wait for budget, and writes to the \
                 same key less than 6 seconds apart are queued, with a warning in Output.",
            );
        ui.end_row();
        ui.label("Output");
        ui.vertical(|ui| {
            ui.checkbox(&mut ds.log_requests, "Log every request");
            ui.checkbox(&mut ds.log_failures, "Warn when a failure is simulated");
        });
        ui.end_row();
    });

    section(ui, "Simulated connection failures");
    note(
        ui,
        "Make requests fail so you can test error handling (pcall, retries, backoff). Failures raise errors \
         in Roblox's format, e.g. \"502: API Services rejected request with error. HTTP 502 (Bad Gateway)\" \
         or \"500: Internal Server Error. …\".",
    );
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.checkbox(&mut ds.outage, "Simulate outage: every request fails with");
        code_combo(ui, egui::Id::new("ds_outage_code"), &mut ds.outage_code);
    });
    ui.horizontal(|ui| {
        ui.label("Only for data stores");
        ui.add(
            egui::TextEdit::singleline(&mut ds.store_filter)
                .hint_text("all data stores")
                .desired_width(220.0),
        )
        .on_hover_text("Comma-separated data store names. End a name with * to match a prefix, e.g. Player*.");
    });
    ui.add_space(4.0);
    egui::Grid::new("ds_rules").num_columns(3).spacing([10.0, 4.0]).striped(true).show(ui, |ui| {
        ui.label(RichText::new("Fail request").small().color(theme::TEXT_DIM));
        ui.label(RichText::new("Error").small().color(theme::TEXT_DIM));
        ui.label(RichText::new("Chance").small().color(theme::TEXT_DIM));
        ui.end_row();
        for op in Operation::ALL {
            let rule = ds.rule_mut(op);
            ui.checkbox(&mut rule.enabled, RichText::new(op.name()).monospace()).on_hover_text(op.covers());
            ui.add_enabled_ui(rule.enabled, |ui| code_combo(ui, egui::Id::new(("ds_rule", op)), &mut rule.code));
            ui.add_enabled(rule.enabled, egui::Slider::new(&mut rule.chance, 0.0..=100.0).suffix("%").max_decimals(0));
            ui.end_row();
        }
    });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.button("Turn off all failures").clicked() {
            ds.outage = false;
            for op in Operation::ALL {
                ds.rule_mut(op).enabled = false;
            }
        }
        if ui.button("Restore defaults").clicked() {
            *ds = DataStoreConfig { enabled: ds.enabled, ..Default::default() };
        }
    });
    if ds.outage || ds.rules.values().any(|r| r.enabled && r.chance > 0.0) {
        ui.add_space(4.0);
        ui.label(RichText::new("Failures are active.").small().color(Color32::from_rgb(230, 150, 90)));
    }
}

fn data_page(ui: &mut egui::Ui, browser: &mut DataBrowser, emulator: &SharedDataStore) {
    let revision = emulator.revision();
    if browser.cache.as_ref().is_none_or(|(r, _)| *r != revision) {
        browser.cache = Some((revision, emulator.summary(VALUE_PREVIEW)));
    }
    let Some((_, keys)) = &browser.cache else { return };

    match emulator.data_file() {
        Some(file) => note(ui, &format!("Stored in {}", file.display())),
        None => note(ui, "No place loaded."),
    }
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut browser.filter).hint_text("Filter by store, scope, key or value").desired_width(260.0));
        if browser.confirm_clear {
            ui.label(RichText::new("Delete all data for this place?").color(Color32::from_rgb(230, 120, 90)));
            if ui.button("Delete").clicked() {
                emulator.clear();
                browser.confirm_clear = false;
            }
            if ui.button("Cancel").clicked() {
                browser.confirm_clear = false;
            }
        } else if ui.add_enabled(!keys.is_empty(), egui::Button::new("Clear all data")).clicked() {
            browser.confirm_clear = true;
        }
    });
    ui.add_space(4.0);

    let filter = browser.filter.to_lowercase();
    let shown: Vec<&KeySummary> = keys
        .iter()
        .filter(|k| {
            filter.is_empty()
                || [&k.store, &k.scope, &k.key, &k.value].iter().any(|s| s.to_lowercase().contains(&filter))
        })
        .collect();
    if keys.is_empty() {
        note(ui, "No data yet. Data appears here as scripts write it.");
        return;
    }
    note(ui, &format!("{} of {} keys", shown.len().min(MAX_ROWS), keys.len()));

    let mut delete = None;
    egui::Grid::new("ds_data").num_columns(6).spacing([10.0, 3.0]).striped(true).show(ui, |ui| {
        for h in ["Store", "Scope", "Key", "Value", "Versions", ""] {
            ui.label(RichText::new(h).small().color(theme::TEXT_DIM));
        }
        ui.end_row();
        for k in shown.iter().take(MAX_ROWS) {
            let store = if k.ordered { format!("{} (ordered)", k.store) } else { k.store.clone() };
            ui.label(RichText::new(store).small());
            ui.label(RichText::new(&k.scope).small());
            ui.label(RichText::new(&k.key).small().monospace());
            ui.allocate_ui_with_layout(egui::vec2(240.0, 16.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_min_width(240.0);
                ui.add(egui::Label::new(RichText::new(&k.value).small().monospace()).truncate()).on_hover_text(&k.value);
            });
            ui.label(RichText::new(if k.ordered { "-".to_owned() } else { k.versions.to_string() }).small());
            if ui.small_button("Delete").on_hover_text("Delete this key and its versions").clicked() {
                delete = Some((k.ordered, k.store.clone(), k.scope.clone(), k.key.clone()));
            }
            ui.end_row();
        }
    });
    if shown.len() > MAX_ROWS {
        note(ui, "Narrow the filter to see more keys.");
    }
    if let Some((ordered, store, scope, key)) = delete {
        emulator.delete_key(ordered, &store, &scope, &key);
    }
}
