//! Drives the Luau VMs.
//!
//! - The *edit* VM serves the command bar against the edit document. Its
//!   changes are undoable. It is created on first use and kept (so command
//!   bar globals persist) until the document is replaced.
//! - The *run* VM exists while running (F8). It executes server Scripts on a
//!   copy of the place, which is discarded on stop.
//!
//! Both VMs are bound to the service emulators (Test > Emulators).

use bevy::prelude::*;
use phaedrus_luau::{OutputLevel, Runtime};

use crate::emulators::DataStoreEmulatorHandle;
use crate::studio::{PlayState, Studio};

#[derive(Default)]
pub struct Scripting {
    edit: Option<Runtime>,
    run: Option<Runtime>,
    edit_generation: u64,
}

/// First system of the frame: hand this frame's document changes to every
/// consumer.
pub fn collect_changes(mut studio: ResMut<Studio>) {
    let changes = studio.doc().take_changes();
    studio.frame_changes = changes;
}

pub fn drive_scripts(
    mut scripting: NonSendMut<Scripting>,
    mut studio: ResMut<Studio>,
    time: Res<Time>,
    datastore: Res<DataStoreEmulatorHandle>,
) {
    let s = &mut *scripting;

    if studio.pending_stop {
        studio.pending_stop = false;
        if studio.is_running() {
            s.run = None;
            studio.end_run();
            studio.log(OutputLevel::Info, "Stopped.");
        }
    }
    if studio.pending_run.take().is_some() && !studio.is_running() && studio.begin_run() {
        // The edit VM refers to the edit document's instances; drop it.
        s.edit = None;
        match Runtime::new(studio.doc.clone(), studio.output.clone(), true) {
            Ok(rt) => {
                // A run is a fresh server: new request budgets.
                datastore.new_session();
                rt.set_datastore(Some(datastore.0.clone()));
                studio.log(OutputLevel::Info, "Running server scripts...");
                rt.run_server_scripts();
                s.run = Some(rt);
            }
            Err(e) => {
                studio.log(OutputLevel::Error, format!("Failed to start Luau VM: {e}"));
                studio.end_run();
            }
        }
    }

    let dt = time.delta_secs_f64();
    let commands: Vec<String> = std::mem::take(&mut studio.pending_exec);
    let changes = std::mem::take(&mut studio.frame_changes);

    match studio.play {
        PlayState::Running { paused } => {
            if let Some(rt) = &s.run {
                for c in &commands {
                    rt.exec(c, "CommandBar");
                }
                if !paused {
                    rt.step(dt, &changes);
                }
            }
        }
        PlayState::Edit => {
            if s.edit_generation != studio.generation {
                s.edit = None;
                s.edit_generation = studio.generation;
            }
            if !commands.is_empty() && s.edit.is_none() {
                match Runtime::new(studio.doc.clone(), studio.output.clone(), false) {
                    Ok(rt) => {
                        rt.set_datastore(Some(datastore.0.clone()));
                        s.edit = Some(rt);
                    }
                    Err(e) => studio.log(OutputLevel::Error, format!("Failed to start Luau VM: {e}")),
                }
            }
            let mut ran = false;
            if let Some(rt) = &s.edit {
                for c in &commands {
                    rt.exec(c, "CommandBar");
                    ran = true;
                }
                if !rt.is_idle() {
                    rt.step(dt, &changes);
                }
                if ran {
                    studio.waypoint("Command Bar");
                }
            }
        }
    }
    studio.frame_changes = changes;
}
