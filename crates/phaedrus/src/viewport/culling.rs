//! Draw distance culling.
//!
//! Parts are bucketed into a coarse grid of world-space cells. When the
//! camera moves, each cell's bounds are tested against the draw distance and
//! only cells that cross the threshold have their parts shown or hidden, so
//! per-frame cost depends on the number of cells, not parts. Hidden parts are
//! skipped by every render view (main camera and shadow cascades) and their
//! children (decals) hide with them.
//!
//! Bevy's own far plane does not cull (its frustum test ignores the far
//! plane), hence this.

use std::collections::HashMap;

use bevy::prelude::*;

use super::camera::EditorCamera;
use super::lighting::{GraphicsSettings, draw_distance_studs};
use super::sync::{PartIndex, PartRef};

/// Cell edge length in studs.
const CELL: f32 = 128.0;
/// Re-test cells after the camera moves this far.
const MOVE_THRESHOLD: f32 = 8.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum CellKey {
    Grid(IVec3),
    /// Parts larger than a cell (baseplates, terrain slabs) are tested on
    /// their own bounds, so they never drag a whole cell into view.
    Own(Entity),
}

struct Cell {
    min: Vec3,
    max: Vec3,
    members: Vec<Entity>,
    culled: bool,
}

impl Cell {
    fn distance(&self, p: Vec3) -> f32 {
        p.clamp(self.min, self.max).distance(p)
    }
}

#[derive(Resource, Default)]
pub struct CullGrid {
    cells: HashMap<CellKey, Cell>,
    cell_of: HashMap<Entity, CellKey>,
    /// Parts spawned, moved or changed since the last update.
    dirty: Vec<Entity>,
    /// Camera position and draw distance cells were last tested with.
    last: Option<(Vec3, f32)>,
}

impl CullGrid {
    /// Called by `sync` when a part entity is spawned or its geometry changes.
    pub fn mark_dirty(&mut self, e: Entity) {
        self.dirty.push(e);
    }

    /// Called by `sync` when a part entity is despawned.
    pub fn remove(&mut self, e: Entity) {
        if let Some(key) = self.cell_of.remove(&e)
            && let Some(cell) = self.cells.get_mut(&key)
        {
            cell.members.retain(|m| *m != e);
            if cell.members.is_empty() {
                self.cells.remove(&key);
            }
        }
    }

    pub fn clear(&mut self) {
        self.cells.clear();
        self.cell_of.clear();
        self.dirty.clear();
        self.last = None;
    }
}

/// World-space bounds of a unit-cube part with this transform.
fn world_bounds(t: &Transform) -> (Vec3, Vec3) {
    let m = Mat3::from_quat(t.rotation);
    let half = t.scale * 0.5;
    let extent = m.x_axis.abs() * half.x + m.y_axis.abs() * half.y + m.z_axis.abs() * half.z;
    (t.translation - extent, t.translation + extent)
}

fn visibility(culled: bool, transparent: bool) -> Visibility {
    if culled || transparent { Visibility::Hidden } else { Visibility::Inherited }
}

pub fn cull_by_distance(
    mut grid: ResMut<CullGrid>,
    settings: Res<GraphicsSettings>,
    index: Res<PartIndex>,
    camera: Query<&GlobalTransform, With<EditorCamera>>,
    mut parts: Query<(&PartRef, &Transform, &mut Visibility)>,
) {
    let Ok(cam) = camera.single() else { return };
    let cam_pos = cam.translation();
    let level_max = settings.draw_distance >= 10;
    let max_dist = if level_max { f32::INFINITY } else { draw_distance_studs(settings.draw_distance) };
    let grid = &mut *grid;

    // (Re)file changed parts and give them their cell's state.
    let dirty = std::mem::take(&mut grid.dirty);
    for e in dirty {
        let Ok((part, t, mut vis)) = parts.get_mut(e) else { continue };
        let (min, max) = world_bounds(t);
        let size = max - min;
        let key = if size.max_element() > CELL {
            CellKey::Own(e)
        } else {
            CellKey::Grid(((min + max) * 0.5 / CELL).floor().as_ivec3())
        };
        if grid.cell_of.get(&e) != Some(&key) {
            grid.remove(e);
            grid.cell_of.insert(e, key);
            grid.cells
                .entry(key)
                .or_insert_with(|| Cell { min, max, members: Vec::new(), culled: false })
                .members
                .push(e);
        }
        let cell = grid.cells.get_mut(&key).expect("filed above");
        // Bounds only grow; a part that moved away leaves the cell slightly
        // larger than needed, which only makes it cull later.
        cell.min = cell.min.min(min);
        cell.max = cell.max.max(max);
        cell.culled = cell.distance(cam_pos) > max_dist;
        let transparent = index.applied_transparent(part.0);
        vis.set_if_neq(visibility(cell.culled, transparent));
    }

    let moved = grid
        .last
        .is_none_or(|(p, d)| d != max_dist || p.distance_squared(cam_pos) > MOVE_THRESHOLD * MOVE_THRESHOLD);
    if !moved {
        return;
    }
    grid.last = Some((cam_pos, max_dist));
    for cell in grid.cells.values_mut() {
        let culled = cell.distance(cam_pos) > max_dist;
        if culled == cell.culled {
            continue;
        }
        cell.culled = culled;
        for &e in &cell.members {
            if let Ok((part, _, mut vis)) = parts.get_mut(e) {
                vis.set_if_neq(visibility(culled, index.applied_transparent(part.0)));
            }
        }
    }
}
