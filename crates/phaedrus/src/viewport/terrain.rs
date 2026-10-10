//! Renders Workspace.Terrain's smooth voxel terrain.
//!
//! Terrain is far heavier than parts: large maps hold tens of thousands of
//! 32³-voxel chunks. So nothing about it runs on the main thread except
//! bookkeeping:
//!
//! - `SmoothGrid` is indexed on a background task (chunks stay run-length
//!   encoded until a mesher needs them).
//! - Each chunk is meshed on a background task at a level of detail picked
//!   from its distance to the camera ([`terrain_mesh`]); far chunks use 2×,
//!   4× or 8× coarser samples. LOD changes have hysteresis so a camera
//!   resting near a threshold does not re-mesh every frame.
//! - Work is scheduled nearest-first with a cap on tasks in flight, and a
//!   chunk keeps showing its old mesh until the new one is ready.
//! - Chunks past the draw distance are hidden.
//!
//! A chunk is one draw: material colors (from `Terrain.MaterialColors`) are
//! vertex colors on a single shared material. Water is a second mesh with
//! its own translucent material.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::tasks::futures::check_ready;
use bevy::tasks::{AsyncComputeTaskPool, Task};
use phaedrus_dom::Change;
use phaedrus_dom::terrain::{self, CHUNK, TerrainAppearance, TerrainGrid, VOXEL_STUDS};

use super::camera::EditorCamera;
use super::lighting::{GraphicsSettings, draw_distance_studs};
use super::terrain_mesh::{MAX_LOD, MeshData, mesh_chunk};
use crate::studio::Studio;

const CHUNK_STUDS: f32 = CHUNK as f32 * VOXEL_STUDS;
/// A chunk changes LOD only once it is this far past the threshold.
const HYSTERESIS: f32 = 24.0;
/// Re-pick LODs after the camera moves this far.
const MOVE_THRESHOLD: f32 = 8.0;

/// Marks a terrain chunk mesh entity.
#[derive(Component)]
pub struct TerrainChunk;

struct Loaded {
    grid: Arc<TerrainGrid>,
    /// Linear color per material slot.
    palette: Arc<[[f32; 3]; 23]>,
}

/// Meshes for one chunk, built off-thread.
struct Built {
    solid: Option<Mesh>,
    water: Option<Mesh>,
}

#[derive(Default)]
struct ChunkView {
    /// LOD of the spawned meshes; `None` before the first mesh arrives.
    shown: Option<u8>,
    entities: Vec<Entity>,
    pending: Option<(u8, Task<Built>)>,
    culled: bool,
}

#[derive(Resource, Default)]
pub struct TerrainState {
    generation: u64,
    load: Option<Task<anyhow::Result<TerrainGrid>>>,
    data: Option<Loaded>,
    chunks: HashMap<IVec3, ChunkView>,
    solid_material: Handle<StandardMaterial>,
    water_material: Handle<StandardMaterial>,
    /// Camera position, draw distance and LOD distance LODs were last
    /// picked with.
    last: Option<(Vec3, f32, f32)>,
    /// Chunks still waiting for a mesh at their wanted LOD.
    outstanding: bool,
}

impl TerrainState {
    /// Whether terrain is still loading or meshing toward its wanted LODs.
    pub fn busy(&self) -> bool {
        self.load.is_some() || self.outstanding || self.chunks.values().any(|c| c.pending.is_some())
    }
}

/// Distance within which chunks render at full resolution. Each LOD after
/// that covers twice the distance of the one before.
fn lod_distance(quality: u8) -> f32 {
    96.0 + 48.0 * quality.clamp(1, 10) as f32
}

fn lod_for(distance: f32, base: f32) -> u8 {
    if distance < base {
        return 0;
    }
    ((distance / base).log2().floor() as i32 + 1).clamp(0, MAX_LOD as i32) as u8
}

/// The LOD for a chunk at `distance`, sticking with `current` until the
/// distance is clearly past the threshold.
fn wanted_lod(distance: f32, base: f32, current: Option<u8>) -> u8 {
    let lod = lod_for(distance, base);
    match current {
        Some(cur) if lod > cur && lod_for(distance - HYSTERESIS, base) <= cur => cur,
        Some(cur) if lod < cur && lod_for(distance + HYSTERESIS, base) >= cur => cur,
        _ => lod,
    }
}

fn chunk_distance(key: IVec3, p: Vec3) -> f32 {
    let min = key.as_vec3() * CHUNK_STUDS;
    p.clamp(min, min + Vec3::splat(CHUNK_STUDS)).distance(p)
}

/// Reloads terrain when a document is opened or Terrain's voxels or colors
/// change.
pub fn sync_terrain(
    mut commands: Commands,
    studio: Res<Studio>,
    mut state: ResMut<TerrainState>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let reload = state.generation != studio.generation || {
        let doc = studio.doc();
        let is_terrain = |r| doc.get(r).is_some_and(|i| i.class == "Terrain");
        studio.frame_changes.iter().any(|c| match c {
            Change::Property(r, name) => terrain::is_render_property(name) && is_terrain(*r),
            Change::Added(r) | Change::Reparented { instance: r, .. } => is_terrain(*r),
        })
    };
    if !reload {
        return;
    }
    state.generation = studio.generation;
    for (_, view) in state.chunks.drain() {
        for e in view.entities {
            commands.entity(e).despawn();
        }
    }
    state.data = None;
    state.load = None;
    state.last = None;

    let doc = studio.doc();
    let Some(t) = doc.workspace().and_then(|ws| doc.find_first_child_of_class(ws, "Terrain")) else { return };
    let appearance = TerrainAppearance::from_document(&doc, t);
    let Some(bytes) = terrain::smooth_grid_bytes(&doc, t) else { return };
    drop(doc);

    state.solid_material = materials.add(StandardMaterial {
        // Vertex colors carry the material colors.
        base_color: Color::WHITE,
        perceptual_roughness: 0.85,
        reflectance: 0.3,
        ..default()
    });
    let [r, g, b] = appearance.colors[terrain::WATER as usize];
    state.water_material = materials.add(StandardMaterial {
        base_color: Color::srgba(r, g, b, (1.0 - appearance.water_transparency).clamp(0.05, 1.0)),
        perceptual_roughness: 0.05,
        reflectance: 0.2 + 0.5 * appearance.water_reflectance,
        alpha_mode: AlphaMode::Blend,
        // Visible from below the surface too.
        cull_mode: None,
        double_sided: true,
        ..default()
    });
    let palette = appearance.colors.map(|[r, g, b]| {
        let lin = LinearRgba::from(Color::srgb(r, g, b));
        [lin.red, lin.green, lin.blue]
    });
    let palette = Arc::new(palette);
    state.load = Some(AsyncComputeTaskPool::get().spawn(async move { TerrainGrid::decode(bytes) }));
    // Stash the palette with an empty grid until the index is built.
    state.data = Some(Loaded { grid: Arc::new(TerrainGrid::default()), palette });
}

/// Picks each chunk's LOD, schedules meshing and swaps in finished meshes.
pub fn update_terrain(
    mut commands: Commands,
    mut state: ResMut<TerrainState>,
    settings: Res<GraphicsSettings>,
    mut meshes: ResMut<Assets<Mesh>>,
    camera: Query<&GlobalTransform, With<EditorCamera>>,
    mut visibility: Query<&mut Visibility, With<TerrainChunk>>,
) {
    let state = &mut *state;
    if let Some(task) = state.load.as_mut()
        && let Some(result) = check_ready(task)
    {
        state.load = None;
        match result {
            Ok(grid) => {
                log::info!(
                    "terrain: {} chunks ({} with surface detail)",
                    grid.chunk_count(),
                    grid.encoded_chunk_count()
                );
                let mut keys: Vec<IVec3> = Vec::with_capacity(grid.chunk_count() * 2);
                for k in grid.chunk_keys() {
                    let k = IVec3::from_array(k);
                    // A chunk's mesh owns the surface between it and its
                    // +X/+Y/+Z neighbours, so the faces on the minus sides of
                    // terrain belong to the (empty) chunks there.
                    keys.extend([k, k - IVec3::X, k - IVec3::Y, k - IVec3::Z]);
                }
                for k in keys {
                    state.chunks.entry(k).or_default();
                }
                if let Some(data) = &mut state.data {
                    data.grid = Arc::new(grid);
                }
                state.last = None;
            }
            Err(e) => log::warn!("terrain: could not read SmoothGrid: {e:#}"),
        }
    }
    let Some(data) = &state.data else { return };

    // Swap in finished meshes.
    for (&key, view) in &mut state.chunks {
        let Some((lod, task)) = &mut view.pending else { continue };
        let Some(built) = check_ready(task) else { continue };
        let lod = *lod;
        view.pending = None;
        for e in view.entities.drain(..) {
            commands.entity(e).despawn();
        }
        let vis = if view.culled { Visibility::Hidden } else { Visibility::Inherited };
        let at = Transform::from_translation(key.as_vec3() * CHUNK_STUDS);
        if let Some(mesh) = built.solid {
            let e = commands
                .spawn((TerrainChunk, Mesh3d(meshes.add(mesh)), MeshMaterial3d(state.solid_material.clone()), at, vis))
                .id();
            view.entities.push(e);
        }
        if let Some(mesh) = built.water {
            let e = commands
                .spawn((
                    TerrainChunk,
                    Mesh3d(meshes.add(mesh)),
                    MeshMaterial3d(state.water_material.clone()),
                    at,
                    vis,
                    NotShadowCaster,
                ))
                .id();
            view.entities.push(e);
        }
        view.shown = Some(lod);
    }

    let Ok(cam) = camera.single() else { return };
    let cam_pos = cam.translation();
    let max_dist = if settings.draw_distance >= 10 { f32::INFINITY } else { draw_distance_studs(settings.draw_distance) };
    let base = lod_distance(settings.quality);
    let moved = state.last.is_none_or(|(p, d, b)| {
        d != max_dist || b != base || p.distance_squared(cam_pos) > MOVE_THRESHOLD * MOVE_THRESHOLD
    });
    if !moved && !state.outstanding {
        return;
    }
    state.last = Some((cam_pos, max_dist, base));

    // Pick LODs, update culling, and collect the chunks that need a mesh.
    let mut work: Vec<(f32, IVec3, u8)> = Vec::new();
    let mut running = 0;
    for (&key, view) in &mut state.chunks {
        let d = chunk_distance(key, cam_pos);
        let culled = d > max_dist;
        if culled != view.culled {
            view.culled = culled;
            let vis = if culled { Visibility::Hidden } else { Visibility::Inherited };
            for &e in &view.entities {
                if let Ok(mut v) = visibility.get_mut(e) {
                    v.set_if_neq(vis);
                }
            }
        }
        if culled {
            // Out of range: stop any meshing, keep what is built.
            view.pending = None;
            continue;
        }
        let lod = wanted_lod(d, base, view.shown);
        match &view.pending {
            Some((p, _)) if *p == lod => running += 1,
            _ if view.shown == Some(lod) => view.pending = None,
            _ => {
                view.pending = None;
                work.push((d, key, lod));
            }
        }
    }
    state.outstanding = !work.is_empty();
    if work.is_empty() {
        return;
    }

    let pool = AsyncComputeTaskPool::get();
    let capacity = (pool.thread_num() * 2).max(4).saturating_sub(running);
    work.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    for &(_, key, lod) in work.iter().take(capacity) {
        let grid = data.grid.clone();
        let palette = data.palette.clone();
        let task = pool.spawn(async move {
            let m = mesh_chunk(&grid, &palette, key.to_array(), lod);
            Built { solid: to_mesh(m.solid), water: to_mesh(m.water) }
        });
        if let Some(view) = state.chunks.get_mut(&key) {
            view.pending = Some((lod, task));
        }
    }
}

fn to_mesh(data: MeshData) -> Option<Mesh> {
    if data.is_empty() {
        return None;
    }
    // Kept in the main world too so viewport ray casts hit terrain.
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, data.positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, data.normals);
    if !data.colors.is_empty() {
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, data.colors);
    }
    mesh.insert_indices(Indices::U32(data.indices));
    Some(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lod_bands_double() {
        let base = 100.0;
        assert_eq!(lod_for(0.0, base), 0);
        assert_eq!(lod_for(99.0, base), 0);
        assert_eq!(lod_for(150.0, base), 1);
        assert_eq!(lod_for(250.0, base), 2);
        assert_eq!(lod_for(450.0, base), 3);
        assert_eq!(lod_for(1e6, base), MAX_LOD);
    }

    #[test]
    fn lod_has_hysteresis() {
        let base = 100.0;
        // Just past the 0/1 threshold, a full-res chunk stays full-res...
        assert_eq!(wanted_lod(110.0, base, Some(0)), 0);
        // ...until clearly past it.
        assert_eq!(wanted_lod(130.0, base, Some(0)), 1);
        // And a coarse chunk just inside stays coarse.
        assert_eq!(wanted_lod(90.0, base, Some(1)), 1);
        assert_eq!(wanted_lod(70.0, base, Some(1)), 0);
        assert_eq!(wanted_lod(110.0, base, None), 1);
    }
}
