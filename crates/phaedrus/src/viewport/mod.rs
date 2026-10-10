//! 3D viewport: renders Workspace, lighting and editing tools with bevy.

pub mod camera;
pub mod culling;
pub mod lighting;
pub mod materials;
pub mod meshes;
pub mod sky;
pub mod sync;
pub mod terrain;
pub mod terrain_mesh;
pub mod tools;

use bevy::asset::RenderAssetUsages;
use bevy::camera::RenderTarget;
use bevy::gizmos::config::{DefaultGizmoConfigGroup, GizmoConfigStore};
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use phaedrus_dom::types::Variant;

use crate::studio::Studio;
use camera::EditorCamera;

/// Where the viewport is on screen and whether it has input focus. Written
/// by the UI each frame.
#[derive(Resource, Default)]
pub struct ViewportState {
    /// Logical-pixel rect of the 3D view inside the window.
    pub rect: Option<bevy_egui::egui::Rect>,
    /// Cursor is over the 3D view (and not over a UI element).
    pub hovered: bool,
    /// No text field has keyboard focus.
    pub keyboard_focus: bool,
    /// The current mouse drag began inside the view.
    pub drag_started_inside: bool,
    /// Whether the 3D view is visible (a script tab may cover it).
    pub visible: bool,
    /// UI pixels per point (window scale factor).
    pub scale: f32,
}

impl ViewportState {
    /// Window cursor position (logical) to render-target pixel coordinates.
    pub fn window_to_target(&self, window_pos: Vec2) -> Option<Vec2> {
        let r = self.rect?;
        let local = window_pos - Vec2::new(r.min.x, r.min.y);
        (local.x >= 0.0 && local.y >= 0.0 && local.x <= r.width() && local.y <= r.height())
            .then(|| local * self.scale.max(0.1))
    }

    /// Render-target pixel coordinates back to window logical coordinates.
    pub fn target_to_window(&self, p: Vec2) -> Option<Vec2> {
        let r = self.rect?;
        Some(Vec2::new(r.min.x, r.min.y) + p / self.scale.max(0.1))
    }
}

/// The offscreen image the 3D camera renders into; the UI shows it inside
/// the viewport rect. Rendering offscreen keeps screen-space effects (SSAO,
/// bloom) correct regardless of where the view sits in the window.
#[derive(Resource)]
pub struct ViewportTexture {
    pub handle: Handle<Image>,
    pub size: UVec2,
}

pub struct ViewportPlugin;

impl Plugin for ViewportPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ViewportState>()
            .init_resource::<sync::PartIndex>()
            .init_resource::<culling::CullGrid>()
            .init_resource::<terrain::TerrainState>()
            .init_resource::<materials::MaterialCache>()
            .init_resource::<meshes::UnionMeshes>()
            .init_resource::<tools::ToolState>()
            .init_resource::<camera::CameraSettings>()
            .init_resource::<lighting::GraphicsSettings>()
            .init_resource::<lighting::AppliedLighting>()
            .init_resource::<sky::SkyState>()
            .init_resource::<sky::CurrentSkyParams>()
            .add_systems(Startup, (setup, lighting::spawn_sun, sky::spawn_celestial_bodies))
            .add_systems(
                Update,
                (
                    reset_camera_on_load,
                    sync::sync_parts,
                    terrain::sync_terrain,
                    lighting::apply_lighting,
                    lighting::sync_local_lights,
                    sky::sync_body_sizes,
                    camera::camera_controls,
                    camera::focus_selection,
                    culling::cull_by_distance,
                    terrain::update_terrain,
                    tools::tool_input,
                    lighting::place_celestial_bodies,
                    lighting::budget_local_lights,
                    lighting::freeze_environment_map,
                    tools::draw_tool_gizmos,
                    apply_viewport_rect,
                )
                    .chain(),
            );
    }
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut gizmo_store: ResMut<GizmoConfigStore>,
) {
    commands.insert_resource(meshes::create(&mut meshes));
    let size = UVec2::new(1280, 720);
    let handle = images.add(new_target(size));
    commands.insert_resource(ViewportTexture { handle: handle.clone(), size });
    commands.spawn((
        Camera3d::default(),
        Camera { order: 0, clear_color: ClearColorConfig::Custom(Color::srgb(0.5, 0.6, 0.75)), ..default() },
        RenderTarget::Image(handle.into()),
        bevy::camera::Hdr,
        Projection::Perspective(PerspectiveProjection {
            fov: 70f32.to_radians(),
            near: 0.1,
            far: 20_000.0,
            ..default()
        }),
        Transform::from_xyz(0.0, 20.0, 30.0).looking_at(Vec3::ZERO, Vec3::Y),
        EditorCamera { yaw: 0.0, pitch: -0.5, focus_distance: 30.0 },
    ));
    let (config, _) = gizmo_store.config_mut::<DefaultGizmoConfigGroup>();
    config.line.width = 2.0;
    config.depth_bias = -1.0;
}

/// Moves the editor camera to Workspace.CurrentCamera's CFrame when a
/// document is loaded.
fn reset_camera_on_load(
    studio: Res<Studio>,
    mut last: Local<u64>,
    mut cam: Query<(&mut Transform, &mut EditorCamera, &mut Projection)>,
) {
    if *last == studio.generation {
        return;
    }
    *last = studio.generation;
    let Ok((mut t, mut ec, mut proj)) = cam.single_mut() else { return };
    let doc = studio.doc();
    let Some(ws) = doc.workspace() else { return };
    let Some(camera) = doc.find_first_child_of_class(ws, "Camera") else { return };
    if let Some(Variant::CFrame(c)) = doc.get_property(camera, "CFrame") {
        *t = sync::cframe_to_transform(&c);
        *ec = EditorCamera::from_transform(&t);
    }
    if let (Some(Variant::Float32(fov)), Projection::Perspective(p)) = (doc.get_property(camera, "FieldOfView"), &mut *proj) {
        p.fov = fov.clamp(1.0, 120.0).to_radians();
    }
}

/// Resizes the offscreen target to match the viewport rect.
fn apply_viewport_rect(
    state: Res<ViewportState>,
    mut texture: ResMut<ViewportTexture>,
    mut images: ResMut<Assets<Image>>,
    mut cam: Query<&mut Camera, With<EditorCamera>>,
) {
    let Ok(mut camera) = cam.single_mut() else { return };
    if camera.is_active != state.visible {
        camera.is_active = state.visible;
    }
    let Some(rect) = state.rect else { return };
    let size = UVec2::new(
        (rect.width() * state.scale).round().max(1.0) as u32,
        (rect.height() * state.scale).round().max(1.0) as u32,
    )
    .min(UVec2::splat(8192));
    if size == texture.size {
        return;
    }
    // Replace rather than resize: resizing copies the old contents, which
    // a render target does not need.
    let _ = images.insert(texture.handle.id(), new_target(size));
    texture.size = size;
}

fn new_target(size: UVec2) -> Image {
    let mut image = Image::new_target_texture(size.x, size.y, TextureFormat::Rgba8UnormSrgb, None);
    image.asset_usage = RenderAssetUsages::RENDER_WORLD;
    image
}
