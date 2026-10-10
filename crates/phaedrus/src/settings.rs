//! User settings (File > Phaedrus Settings, Test > Emulators): script
//! editor, graphics and service emulators, saved to `settings.json` in the user's config directory
//! (`%APPDATA%/phaedrus` on Windows).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use bevy::prelude::*;
use phaedrus_luau::DataStoreConfig;
use phaedrus_script_editor::settings::EditorSettings;
use serde::{Deserialize, Serialize};

use crate::viewport::lighting::GraphicsSettings;

/// Settings are written at most this often, so dragging a slider or colour
/// picker does not write every frame.
const SAVE_DELAY: Duration = Duration::from_millis(500);

#[derive(Resource, Clone, Debug, Default, PartialEq, Deref, DerefMut)]
pub struct ScriptEditorSettings(pub EditorSettings);

/// Test > Emulators: mock Roblox services for offline testing.
#[derive(Resource, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EmulatorSettings {
    pub datastore: DataStoreConfig,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SettingsFile {
    script_editor: EditorSettings,
    graphics: GraphicsSettings,
    emulators: EmulatorSettings,
}

/// Phaedrus' folder in the user's app data directory (`%APPDATA%/phaedrus`
/// on Windows).
pub fn app_data_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(dir.join("phaedrus"))
}

fn settings_path() -> Option<PathBuf> {
    Some(app_data_dir()?.join("settings.json"))
}

/// Loads saved settings, falling back to defaults for anything missing.
pub fn load() -> (ScriptEditorSettings, GraphicsSettings, EmulatorSettings) {
    let file = settings_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| match serde_json::from_str::<SettingsFile>(&s) {
            Ok(f) => Some(f),
            Err(e) => {
                warn!("ignoring unreadable settings.json: {e}");
                None
            }
        })
        .unwrap_or_default();
    (ScriptEditorSettings(file.script_editor), file.graphics, file.emulators)
}

/// Writes settings when they change.
pub fn persist(
    editor: Res<ScriptEditorSettings>,
    graphics: Res<GraphicsSettings>,
    emulators: Res<EmulatorSettings>,
    mut saved: Local<Option<SettingsFile>>,
    mut changed_at: Local<Option<Instant>>,
) {
    // Benchmark and screenshot runs override settings from the environment;
    // never save those over the user's own.
    if std::env::var_os("PHAEDRUS_BENCH").is_some() || std::env::var_os("PHAEDRUS_SCREENSHOT").is_some() {
        return;
    }
    let current = SettingsFile { script_editor: editor.0.clone(), graphics: graphics.clone(), emulators: emulators.clone() };
    let Some(last) = saved.as_ref() else {
        // First frame: what was loaded is what is on disk.
        *saved = Some(current);
        return;
    };
    if *last == current {
        *changed_at = None;
        return;
    }
    if changed_at.get_or_insert_with(Instant::now).elapsed() < SAVE_DELAY {
        return;
    }
    if let Some(path) = settings_path() {
        let result = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, serde_json::to_string_pretty(&current).unwrap_or_default()));
        if let Err(e) = result {
            warn!("could not save {}: {e}", path.display());
        }
    }
    *saved = Some(current);
    *changed_at = None;
}
