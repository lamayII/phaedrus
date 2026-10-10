//! Phaedrus: a clean-room Roblox Studio alternative.
//!
//! Usage: `phaedrus [place.rbxl|place.rbxlx|model.rbxm]`

mod asset_loader;
mod assets;
mod editor;
mod emulators;
mod opencloud;
mod scripting;
mod settings;
mod studio;
mod ui;
mod viewport;

use std::path::PathBuf;
use std::time::Duration;

use bevy::camera::ClearColorConfig;
use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;
use bevy::winit::{UpdateMode, WinitSettings};
use bevy_egui::{EguiGlobalSettings, EguiPlugin, PrimaryEguiContext};
use phaedrus_dom::Document;
use phaedrus_luau::OutputLevel;

use crate::studio::Studio;
use crate::viewport::lighting::GraphicsSettings;

fn main() {
    let mut studio = Studio::new(Document::new_place());
    studio.log(OutputLevel::Info, "Phaedrus started.");
    // Large places take a while to read; load in the background behind the
    // loading modal instead of delaying the window.
    if let Some(path) = std::env::args().nth(1).map(PathBuf::from) {
        studio.open(path);
    }

    let (editor_settings, graphics_settings, emulator_settings) = settings::load();
    let datastore = emulators::DataStoreEmulatorHandle::new(&emulator_settings);
    let bench = std::env::var_os("PHAEDRUS_BENCH").is_some();

    let mut app = App::new();
    app.insert_resource(editor_settings)
        .insert_resource(graphics_settings)
        .insert_resource(emulator_settings)
        .insert_resource(datastore)
        .insert_resource(ClearColor(Color::srgb(0.12, 0.12, 0.13)))
        .insert_resource(WinitSettings {
            focused_mode: UpdateMode::Continuous,
            unfocused_mode: UpdateMode::reactive_low_power(Duration::from_millis(100)),
        })
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "Phaedrus".into(),
                        resolution: (1600, 900).into(),
                        // Benchmarks measure uncapped frame rate.
                        present_mode: if bench { bevy::window::PresentMode::AutoNoVsync } else { default() },
                        ..default()
                    }),
                    ..default()
                })
                .set(bevy::log::LogPlugin {
                    filter: "wgpu=error,naga=warn,bevy_render=warn,bevy_app=warn".into(),
                    ..default()
                }),
        )
        .add_plugins(bevy::diagnostic::FrameTimeDiagnosticsPlugin::default())
        .add_plugins(EguiPlugin::default())
        .insert_resource(studio)
        .init_resource::<assets::ContentProvider>()
        .init_resource::<opencloud::ApiAuth>()
        .init_resource::<asset_loader::AssetDownloads>()
        .init_non_send::<scripting::Scripting>()
        .init_non_send::<editor::ScriptEditorHost>()
        .add_plugins((viewport::ViewportPlugin, ui::UiPlugin))
        .add_systems(Startup, setup_egui_camera)
        .add_systems(PreUpdate, (poll_loading, asset_loader::start_downloads, scripting::collect_changes).chain())
        .add_systems(Update, (emulators::sync_emulators, scripting::drive_scripts, editor::update_editor_host, apply_frame_cap, debug_screenshot, benchmark, settings::persist, asset_loader::report_progress));
    if bench {
        // Per-pass CPU/GPU timings for `PHAEDRUS_BENCH_PASSES`.
        app.add_plugins(bevy::render::diagnostic::RenderDiagnosticsPlugin);
    }
    app.run();
}

/// egui draws the whole window through its own camera; the 3D view is an
/// offscreen image shown inside the UI.
fn setup_egui_camera(mut commands: Commands, mut settings: ResMut<EguiGlobalSettings>) {
    settings.auto_create_primary_context = false;
    commands.spawn((
        PrimaryEguiContext,
        Camera2d,
        RenderLayers::none(),
        Camera { order: 10, clear_color: ClearColorConfig::Custom(Color::srgb(0.145, 0.15, 0.16)), ..default() },
    ));
}

/// Swaps in a place once its background load completes.
fn poll_loading(mut studio: ResMut<Studio>) {
    if studio.loading.is_some() {
        studio.poll_loading();
    }
}

/// Applies the background FPS cap from the graphics settings.
fn apply_frame_cap(settings: Res<GraphicsSettings>, mut winit: ResMut<WinitSettings>) {
    if !settings.is_changed() {
        return;
    }
    if std::env::var_os("PHAEDRUS_BENCH").is_some() {
        winit.unfocused_mode = UpdateMode::Continuous;
        return;
    }
    let ms = 1000 / settings.background_fps.max(1) as u64;
    winit.unfocused_mode = UpdateMode::reactive_low_power(Duration::from_millis(ms));
}

/// `PHAEDRUS_BENCH=<frames>` waits for the opened place to load, then turns
/// the camera a full circle over that many frames (default 600) with vsync
/// off, prints frame-time statistics and exits. `PHAEDRUS_QUALITY`,
/// `PHAEDRUS_TECH` and `PHAEDRUS_DRAW_DISTANCE` override the saved settings,
/// `PHAEDRUS_BENCH_CAM=x,y,z` places the camera, `PHAEDRUS_BENCH_EXEC` runs a
/// command-bar snippet once the place loads and `PHAEDRUS_BENCH_SHOT` saves a
/// screenshot when measuring starts.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn benchmark(
    mut commands: Commands,
    time: Res<Time>,
    mut studio: ResMut<Studio>,
    mut graphics: ResMut<GraphicsSettings>,
    mut frame: Local<u32>,
    mut samples: Local<Vec<f32>>,
    mut start_yaw: Local<f32>,
    mut cam: Query<(&mut Transform, &mut viewport::camera::EditorCamera)>,
    parts: Query<&Transform, (With<viewport::sync::PartRef>, Without<viewport::camera::EditorCamera>)>,
    diagnostics: Res<bevy::diagnostic::DiagnosticsStore>,
    lights: Query<(&Visibility, Option<&PointLight>, Option<&SpotLight>), With<viewport::lighting::LocalLight>>,
    terrain: Res<viewport::terrain::TerrainState>,
    mut exit: MessageWriter<AppExit>,
) {
    const WARMUP: u32 = 120;
    let Some(frames) = std::env::var("PHAEDRUS_BENCH").ok().map(|v| v.parse().unwrap_or(600u32)) else { return };
    // Wait for the place passed on the command line to finish loading.
    if studio.loading.is_some() || (std::env::args().nth(1).is_some() && studio.generation <= 1) {
        return;
    }
    // Let terrain finish meshing before measuring.
    if *frame == WARMUP - 1 && terrain.busy() {
        return;
    }
    *frame += 1;
    if *frame == 1 {
        if let Some(q) = std::env::var("PHAEDRUS_QUALITY").ok().and_then(|v| v.parse().ok()) {
            graphics.quality = q;
        }
        if let Ok(code) = std::env::var("PHAEDRUS_BENCH_EXEC") {
            studio.pending_exec.push(code);
        }
        if let Some(d) = std::env::var("PHAEDRUS_DRAW_DISTANCE").ok().and_then(|v| v.parse().ok()) {
            graphics.draw_distance = d;
        }
        graphics.technology_override = match std::env::var("PHAEDRUS_TECH").as_deref() {
            Ok("voxel") => Some(viewport::lighting::Technology::Voxel),
            Ok("shadowmap") => Some(viewport::lighting::Technology::ShadowMap),
            Ok("future") => Some(viewport::lighting::Technology::Future),
            _ => graphics.technology_override,
        };
    }
    if *frame == 2 {
        // Look over the densest area (median part position) unless a
        // position is given, so the run measures the place, not empty sky.
        let given = std::env::var("PHAEDRUS_BENCH_CAM").ok().and_then(|v| {
            let c: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            (c.len() == 3).then(|| Vec3::new(c[0], c[1], c[2]))
        });
        let pos = given.or_else(|| {
            let mut xs: Vec<Vec3> = parts.iter().map(|t| t.translation).collect();
            if xs.is_empty() {
                return None;
            }
            let mid = xs.len() / 2;
            let median = |xs: &mut Vec<Vec3>, f: fn(&Vec3) -> f32| {
                xs.sort_by(|a, b| f(a).total_cmp(&f(b)));
                f(&xs[mid])
            };
            Some(Vec3::new(median(&mut xs, |v| v.x), median(&mut xs, |v| v.y) + 30.0, median(&mut xs, |v| v.z)))
        });
        if let (Some(pos), Ok((mut t, mut ec))) = (pos, cam.single_mut()) {
            ec.pitch = -0.25;
            t.translation = pos;
            t.rotation = ec.rotation();
            println!("BENCH camera at {pos}");
        }
    }
    if *frame == WARMUP
        && let Some(path) = std::env::var_os("PHAEDRUS_BENCH_SHOT")
    {
        commands
            .spawn(bevy::render::view::screenshot::Screenshot::primary_window())
            .observe(bevy::render::view::screenshot::save_to_disk(PathBuf::from(path)));
    }
    if *frame <= WARMUP {
        if let Ok((_, ec)) = cam.single() {
            *start_yaw = ec.yaw;
        }
        return;
    }
    samples.push(time.delta_secs() * 1000.0);
    let n = samples.len() as u32;
    if let Ok((mut t, mut ec)) = cam.single_mut() {
        ec.yaw = *start_yaw + n as f32 / frames as f32 * std::f32::consts::TAU;
        t.rotation = ec.rotation();
    }
    if n < frames {
        return;
    }
    let mut sorted = samples.clone();
    sorted.sort_by(f32::total_cmp);
    let avg = sorted.iter().sum::<f32>() / sorted.len() as f32;
    let p99 = sorted[(sorted.len() * 99 / 100).min(sorted.len() - 1)];
    let msg = format!(
        "BENCH parts={} lights={}/{} shadowed={} quality={} draw_distance={} frames={} avg={:.2}ms ({:.1} fps) p99={:.2}ms (1% low {:.1} fps)",
        parts.iter().count(),
        lights.iter().filter(|(v, ..)| **v != Visibility::Hidden).count(),
        lights.iter().count(),
        lights
            .iter()
            .filter(|(v, p, s)| {
                **v != Visibility::Hidden
                    && (p.is_some_and(|p| p.shadow_maps_enabled) || s.is_some_and(|s| s.shadow_maps_enabled))
            })
            .count(),
        graphics.quality,
        graphics.draw_distance,
        sorted.len(),
        avg,
        1000.0 / avg,
        p99,
        1000.0 / p99,
    );
    println!("{msg}");
    info!("{msg}");
    if std::env::var_os("PHAEDRUS_BENCH_PASSES").is_some() {
        // Per-pass CPU/GPU times from bevy's render diagnostics.
        let mut rows: Vec<_> = diagnostics
            .iter()
            .filter(|d| d.path().as_str().starts_with("render/"))
            .filter_map(|d| Some((d.path().as_str().to_owned(), d.average()?)))
            .collect();
        rows.sort_by(|a, b| b.1.total_cmp(&a.1));
        for (path, avg) in rows.iter().take(40) {
            println!("PASS {avg:8.3} {path}");
        }
    }
    exit.write(AppExit::Success);
}

/// `PHAEDRUS_SCREENSHOT=out.png` saves a screenshot after startup and exits
/// (used for automated visual checks). `PHAEDRUS_EXEC` runs a command-bar
/// snippet first, `PHAEDRUS_SELECT` selects instances by name and
/// `PHAEDRUS_OPEN_SETTINGS=graphics|editor` opens Phaedrus Settings.
fn debug_screenshot(
    mut commands: Commands,
    mut frame: Local<u32>,
    mut studio: ResMut<Studio>,
    mut graphics: ResMut<GraphicsSettings>,
    mut ui_state: ResMut<ui::UiState>,
    mut emulators: ResMut<settings::EmulatorSettings>,
    mut exit: MessageWriter<AppExit>,
) {
    let Some(path) = std::env::var_os("PHAEDRUS_SCREENSHOT") else { return };
    *frame += 1;
    if *frame == 2 {
        if let Some(q) = std::env::var("PHAEDRUS_QUALITY").ok().and_then(|v| v.parse().ok()) {
            graphics.quality = q;
        }
        graphics.technology_override = match std::env::var("PHAEDRUS_TECH").as_deref() {
            Ok("voxel") => Some(viewport::lighting::Technology::Voxel),
            Ok("shadowmap") => Some(viewport::lighting::Technology::ShadowMap),
            Ok("future") => Some(viewport::lighting::Technology::Future),
            _ => None,
        };
    }
    if *frame == 2 && let Ok(page) = std::env::var("PHAEDRUS_OPEN_EMULATORS") {
        // Screenshot runs never save settings, so this stays local.
        emulators.datastore.enabled = true;
        ui_state.open_emulators(page == "data");
    }
    if *frame == 5 {
        if let Ok(code) = std::env::var("PHAEDRUS_EXEC") {
            studio.pending_exec.push(code);
        }
        if let Ok(names) = std::env::var("PHAEDRUS_SELECT") {
            let found: Vec<_> = {
                let doc = studio.doc();
                let ws = doc.workspace().unwrap();
                names.split(',').filter_map(|n| doc.find_first_child(ws, n.trim())).collect()
            };
            studio.selection = found;
        }
        if let Ok(tool) = std::env::var("PHAEDRUS_TOOL") {
            studio.tool = match tool.as_str() {
                "move" => studio::Tool::Move,
                "scale" => studio::Tool::Scale,
                "rotate" => studio::Tool::Rotate,
                _ => studio::Tool::Select,
            };
        }
        match std::env::var("PHAEDRUS_OPEN_SETTINGS").as_deref() {
            Ok("graphics") => ui_state.settings_page = Some(ui::SettingsPage::Graphics),
            Ok(_) => ui_state.settings_page = Some(ui::SettingsPage::ScriptEditor),
            Err(_) => {}
        }
        if std::env::var_os("PHAEDRUS_OPEN_SCRIPT").is_some() {
            let sss = studio.doc().service("ServerScriptService");
            studio.selection = sss.into_iter().collect();
            if let Some(r) = studio.insert_object("Script", None) {
                studio.open_script(r);
            }
        }
    }
    if *frame == 30 && std::env::var_os("PHAEDRUS_RUN").is_some() {
        studio.pending_run = Some(true);
    }
    let wait: u32 = std::env::var("PHAEDRUS_SCREENSHOT_FRAME").ok().and_then(|v| v.parse().ok()).unwrap_or(120);
    if *frame == wait {
        commands
            .spawn(bevy::render::view::screenshot::Screenshot::primary_window())
            .observe(bevy::render::view::screenshot::save_to_disk(PathBuf::from(path)));
    }
    if *frame == wait + 30 {
        exit.write(AppExit::Success);
    }
}
