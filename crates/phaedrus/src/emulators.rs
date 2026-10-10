//! Service emulators (Test > Emulators): offline stand-ins for Roblox web
//! services that need a published place. The Luau VMs bind to them while
//! they are enabled.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use bevy::prelude::*;
use phaedrus_luau::{DataStoreEmulator, OutputLevel, SharedDataStore};

use crate::settings::{EmulatorSettings, app_data_dir};
use crate::studio::Studio;

/// Data store writes are saved at most this often.
const FLUSH_INTERVAL: Duration = Duration::from_millis(250);

/// The DataStoreService emulator. Its data lives in
/// `<app data>/datastores/<place>-<hash>.json`, one file per place file.
#[derive(Resource, Clone, Deref)]
pub struct DataStoreEmulatorHandle(pub SharedDataStore);

impl DataStoreEmulatorHandle {
    pub fn new(settings: &EmulatorSettings) -> Self {
        let root = app_data_dir().unwrap_or_else(|| PathBuf::from(".phaedrus")).join("datastores");
        Self(DataStoreEmulator::new(root, settings.datastore.clone()))
    }
}

/// Keeps the emulators in step with their settings and the open place, and
/// saves data store changes.
pub fn sync_emulators(
    settings: Res<EmulatorSettings>,
    datastore: Res<DataStoreEmulatorHandle>,
    studio: Res<Studio>,
    mut place: Local<Option<Option<PathBuf>>>,
    mut last_flush: Local<Option<Instant>>,
) {
    if settings.is_changed() {
        datastore.set_config(settings.datastore.clone());
    }
    let current = studio.doc().path.clone();
    if place.as_ref() != Some(&current) {
        if let Err(e) = datastore.set_place(current.as_deref()) {
            studio.log(OutputLevel::Warn, format!("Could not save DataStore emulator data: {e}"));
        }
        *place = Some(current);
    }
    if last_flush.is_none_or(|t| t.elapsed() >= FLUSH_INTERVAL) {
        *last_flush = Some(Instant::now());
        if let Err(e) = datastore.flush() {
            warn!("could not save DataStore emulator data: {e}");
        }
    }
}
