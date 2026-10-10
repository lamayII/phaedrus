//! Picking, selection boxes and the Move / Scale / Rotate handles.

use bevy::picking::mesh_picking::ray_cast::{MeshRayCast, MeshRayCastSettings, RayCastVisibility};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use phaedrus_dom::geometry::part_geometry;
use phaedrus_dom::types::{CFrame, Ref, Variant, Vector3};
use phaedrus_dom::Document;

use super::camera::EditorCamera;
use super::sync::{PartRef, cframe_to_transform, transform_to_cframe};
use super::terrain::TerrainChunk;
use super::ViewportState;
use crate::studio::{Studio, Tool};

const SELECT_COLOR: Color = Color::srgb(0.05, 0.63, 1.0);
const HOVER_COLOR: Color = Color::srgba(0.05, 0.63, 1.0, 0.55);
const AXIS_COLORS: [Color; 3] = [
    Color::srgb(0.95, 0.25, 0.25),
    Color::srgb(0.3, 0.9, 0.3),
    Color::srgb(0.25, 0.45, 1.0),
];
const HANDLE_PICK_PX: f32 = 14.0;

/// One draggable handle.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Handle {
    /// 0 = X, 1 = Y, 2 = Z.
    axis: usize,
    /// +1 or -1 for move/scale handles.
    sign: f32,
}

#[derive(Debug, Clone)]
struct DragStart {
    handle: Handle,
    tool: Tool,
    /// Axis direction in world space.
    axis_dir: Vec3,
    /// Pivot (selection centre) at drag start.
    pivot: Vec3,
    /// Param along the axis (move/scale) or start angle (rotate).
    start_param: f32,
    /// Original CFrames (and sizes) of the dragged instances.
    originals: Vec<(Ref, CFrame, Option<Vector3>)>,
    last_applied: f32,
}

/// Free-drag of parts across a horizontal plane (Select tool).
#[derive(Debug, Clone)]
struct FreeDrag {
    plane_y: f32,
    start_hit: Vec3,
    originals: Vec<(Ref, CFrame)>,
    moved: bool,
}

#[derive(Resource, Default)]
pub struct ToolState {
    hovered: Option<Ref>,
    hovered_handle: Option<Handle>,
    drag: Option<DragStart>,
    free_drag: Option<FreeDrag>,
    press_pos: Option<Vec2>,
    /// Last hover ray and its hit. Ray casts test every part, so they are
    /// skipped while neither the ray nor the document changed.
    last_cast: Option<(Ray3d, Option<(Ref, Vec3)>)>,
}

/// World-space oriented box of the selection: centre transform and size.
fn selection_box(doc: &Document, selection: &[Ref], local: bool) -> Option<(Transform, Vec3)> {
    if selection.len() == 1 && local {
        let r = selection[0];
        if let Some(g) = doc.get(r).and_then(part_geometry) {
            let t = cframe_to_transform(&g.cframe);
            return Some((t, Vec3::new(g.size.x, g.size.y, g.size.z)));
        }
    }
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    let mut any = false;
    for r in selection {
        if let Some((a, b)) = doc.bounds(*r) {
            min = min.min(Vec3::new(a.x, a.y, a.z));
            max = max.max(Vec3::new(b.x, b.y, b.z));
            any = true;
        }
    }
    any.then(|| (Transform::from_translation((min + max) * 0.5), max - min))
}

/// Selects the top-level model under Workspace that contains `r`, like
/// Studio's default click behaviour. Alt selects the part itself.
fn click_target(doc: &Document, r: Ref, alt: bool) -> Ref {
    if alt {
        return r;
    }
    let Some(ws) = doc.workspace() else { return r };
    let mut cur = r;
    let mut best = r;
    while cur.is_some() && cur != ws {
        if doc.is_a(cur, "Model") {
            best = cur;
        }
        cur = doc.parent(cur);
    }
    best
}

/// Returns the cursor in window coordinates and the world ray under it.
fn cursor_ray(
    window: &Window,
    viewport: &ViewportState,
    camera: &Camera,
    cam_t: &GlobalTransform,
) -> Option<(Vec2, Ray3d)> {
    let pos = window.cursor_position()?;
    let target = viewport.window_to_target(pos)?;
    let ray = camera.viewport_to_world(cam_t, target).ok()?;
    Some((pos, ray))
}

/// Projects a world point to window coordinates.
fn project(viewport: &ViewportState, camera: &Camera, cam_t: &GlobalTransform, p: Vec3) -> Option<Vec2> {
    let t = camera.world_to_viewport(cam_t, p).ok()?;
    viewport.target_to_window(t)
}

/// Closest-point parameter on line (origin + t*dir) to a ray.
fn axis_param(origin: Vec3, dir: Vec3, ray: &Ray3d) -> Option<f32> {
    let rd = *ray.direction;
    let w0 = origin - ray.origin;
    let a = dir.dot(dir);
    let b = dir.dot(rd);
    let c = rd.dot(rd);
    let d = dir.dot(w0);
    let e = rd.dot(w0);
    let denom = a * c - b * b;
    if denom.abs() < 1e-6 {
        return None;
    }
    Some((b * e - c * d) / denom)
}

/// Angle of the ray's hit on the plane through `pivot` with normal `axis`.
fn ring_angle(pivot: Vec3, axis: Vec3, ray: &Ray3d) -> Option<f32> {
    let denom = axis.dot(*ray.direction);
    if denom.abs() < 1e-5 {
        return None;
    }
    let t = (pivot - ray.origin).dot(axis) / denom;
    if t < 0.0 {
        return None;
    }
    let hit = ray.origin + *ray.direction * t - pivot;
    let (u, v) = axis.any_orthonormal_pair();
    Some(hit.dot(v).atan2(hit.dot(u)))
}

fn snap(v: f32, step: f32, enabled: bool) -> f32 {
    if enabled && step > 0.0 { (v / step).round() * step } else { v }
}

fn handle_points(t: &Transform, size: Vec3, tool: Tool, cam_pos: Vec3) -> Vec<(Handle, Vec3, Vec3)> {
    let mut out = Vec::new();
    let axes = [*t.right(), *t.up(), *t.back()];
    let dist = (cam_pos - t.translation).length();
    let pad = (dist * 0.03).max(0.5);
    match tool {
        Tool::Move | Tool::Scale => {
            for (axis, dir) in axes.iter().enumerate() {
                for sign in [1.0, -1.0] {
                    let half = size[axis] * 0.5;
                    let p = t.translation + *dir * sign * (half + pad);
                    out.push((Handle { axis, sign }, p, *dir * sign));
                }
            }
        }
        Tool::Rotate => {
            // Picking for rings is done separately.
        }
        Tool::Select => {}
    }
    out
}

fn ring_radius(size: Vec3, cam_dist: f32) -> f32 {
    (size.length() * 0.5).max(cam_dist * 0.08).max(1.0)
}

#[allow(clippy::too_many_arguments)]
pub fn tool_input(
    mut studio: ResMut<Studio>,
    mut state: ResMut<ToolState>,
    viewport: Res<ViewportState>,
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    window: Query<&Window, With<PrimaryWindow>>,
    camera: Query<(&Camera, &GlobalTransform), With<EditorCamera>>,
    parts: Query<&PartRef>,
    terrain: Query<(), With<TerrainChunk>>,
    mut ray_cast: MeshRayCast,
) {
    let Ok(window) = window.single() else { return };
    let Ok((camera, cam_t)) = camera.single() else { return };
    let Some((cursor, ray)) = cursor_ray(window, &viewport, camera, cam_t) else {
        state.hovered = None;
        return;
    };
    let cam_pos = cam_t.translation();
    let tool = studio.tool;
    let local = studio.local_space;

    // Continue an active handle drag.
    if let Some(drag) = state.drag.clone() {
        if !buttons.pressed(MouseButton::Left) {
            state.drag = None;
            let name = match drag.tool {
                Tool::Move => "Move",
                Tool::Scale => "Resize",
                Tool::Rotate => "Rotate",
                Tool::Select => "Drag",
            };
            studio.waypoint(name);
            return;
        }
        apply_handle_drag(&mut studio, &mut state, &drag, &ray);
        return;
    }
    if let Some(free) = state.free_drag.clone() {
        if !buttons.pressed(MouseButton::Left) {
            state.free_drag = None;
            if free.moved {
                studio.waypoint("Drag");
            }
            return;
        }
        apply_free_drag(&mut studio, &mut state, &free, &ray);
        return;
    }

    if !viewport.hovered {
        state.hovered = None;
        state.hovered_handle = None;
        return;
    }

    // Hover: part under cursor.
    let hit = match state.last_cast {
        Some((last, hit)) if last == ray && studio.frame_changes.is_empty() => hit,
        _ => {
            let hit = ray_cast
                .cast_ray(ray, &MeshRayCastSettings::default().with_visibility(RayCastVisibility::Visible))
                .iter()
                // Terrain is not selectable but hides the parts behind it.
                .find(|(e, _)| parts.contains(*e) || terrain.contains(*e))
                .and_then(|(e, h)| parts.get(*e).ok().map(|p| (p.0, h.point)));
            state.last_cast = Some((ray, hit));
            hit
        }
    };
    let alt = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);
    state.hovered = {
        let doc = studio.doc();
        hit.map(|(r, _)| click_target(&doc, r, alt))
            .filter(|r| !matches!(doc.get_property(*r, "Locked"), Some(Variant::Bool(true))))
    };

    // Handle hover.
    state.hovered_handle = None;
    let sel_box = {
        let doc = studio.doc();
        let roots: Vec<Ref> = studio.selection.clone();
        selection_box(&doc, &roots, local)
    };
    if let Some((t, size)) = sel_box {
        match tool {
            Tool::Move | Tool::Scale => {
                let mut best = HANDLE_PICK_PX;
                for (h, p, _) in handle_points(&t, size, tool, cam_pos) {
                    if let Some(sp) = project(&viewport, camera, cam_t, p) {
                        let d = sp.distance(cursor);
                        if d < best {
                            best = d;
                            state.hovered_handle = Some(h);
                        }
                    }
                }
            }
            Tool::Rotate => {
                let radius = ring_radius(size, (cam_pos - t.translation).length());
                let axes = [*t.right(), *t.up(), *t.back()];
                let mut best = HANDLE_PICK_PX;
                for (axis, dir) in axes.iter().enumerate() {
                    let (u, v) = dir.any_orthonormal_pair();
                    let mut prev: Option<Vec2> = None;
                    for i in 0..=48 {
                        let a = i as f32 / 48.0 * std::f32::consts::TAU;
                        let p = t.translation + (u * a.cos() + v * a.sin()) * radius;
                        let Some(sp) = project(&viewport, camera, cam_t, p) else {
                            prev = None;
                            continue;
                        };
                        if let Some(pp) = prev {
                            let d = dist_to_segment(cursor, pp, sp);
                            if d < best {
                                best = d;
                                state.hovered_handle = Some(Handle { axis, sign: 1.0 });
                            }
                        }
                        prev = Some(sp);
                    }
                }
            }
            Tool::Select => {}
        }
    }

    if buttons.just_pressed(MouseButton::Left) {
        state.press_pos = Some(cursor);
        // Start a handle drag.
        if let (Some(h), Some((t, _size))) = (state.hovered_handle, sel_box) {
            let axes = [*t.right(), *t.up(), *t.back()];
            let axis_dir = axes[h.axis];
            let start_param = match tool {
                Tool::Rotate => ring_angle(t.translation, axis_dir, &ray).unwrap_or(0.0),
                _ => axis_param(t.translation, axis_dir, &ray).unwrap_or(0.0),
            };
            let originals = {
                // selection_roots() takes the document lock itself, so it
                // must run before we lock here.
                let roots = studio.selection_roots();
                let doc = studio.doc();
                roots
                    .into_iter()
                    .flat_map(|r| {
                        // Models move by moving their parts.
                        let mut v = vec![r];
                        v.extend(doc.descendants(r));
                        v
                    })
                    .filter_map(|r| {
                        let g = doc.get(r).and_then(part_geometry)?;
                        Some((r, g.cframe, Some(g.size)))
                    })
                    .collect()
            };
            state.drag = Some(DragStart {
                handle: h,
                tool,
                axis_dir,
                pivot: t.translation,
                start_param,
                originals,
                last_applied: 0.0,
            });
            return;
        }
        // Start a free drag on an already-selected part.
        if let Some((r, point)) = hit {
            let target = click_target(&studio.doc(), r, alt);
            if studio.is_selected(target) && tool != Tool::Rotate {
                let originals = {
                    let roots = studio.selection_roots();
                    let doc = studio.doc();
                    roots
                        .into_iter()
                        .flat_map(|r| {
                            let mut v = vec![r];
                            v.extend(doc.descendants(r));
                            v
                        })
                        .filter_map(|r| doc.get(r).and_then(part_geometry).map(|g| (r, g.cframe)))
                        .collect()
                };
                state.free_drag = Some(FreeDrag { plane_y: point.y, start_hit: point, originals, moved: false });
            }
        }
    }

    if buttons.just_released(MouseButton::Left) {
        let clicked = state.press_pos.is_some_and(|p| p.distance(cursor) < 4.0);
        state.press_pos = None;
        if clicked {
            let additive = keys.pressed(KeyCode::ControlLeft)
                || keys.pressed(KeyCode::ControlRight)
                || keys.pressed(KeyCode::ShiftLeft)
                || keys.pressed(KeyCode::ShiftRight);
            match state.hovered {
                Some(r) if additive => studio.toggle_select(r),
                Some(r) => studio.select_only(r),
                None if !additive => studio.selection.clear(),
                None => {}
            }
        }
    }
}

fn dist_to_segment(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_squared().max(1e-6)).clamp(0.0, 1.0);
    p.distance(a + ab * t)
}

fn apply_handle_drag(studio: &mut Studio, state: &mut ToolState, drag: &DragStart, ray: &Ray3d) {
    let snap_cfg = studio.snap;
    match drag.tool {
        Tool::Move => {
            let Some(p) = axis_param(drag.pivot, drag.axis_dir, ray) else { return };
            let delta = snap(p - drag.start_param, snap_cfg.move_step, snap_cfg.move_enabled);
            if delta == drag.last_applied {
                return;
            }
            let offset = drag.axis_dir * delta;
            let mut doc = studio.doc();
            for (r, cf, _) in &drag.originals {
                let mut c = *cf;
                c.position = Vector3::new(c.position.x + offset.x, c.position.y + offset.y, c.position.z + offset.z);
                doc.write_raw(*r, "CFrame", Variant::CFrame(c));
            }
            drop(doc);
            if let Some(d) = state.drag.as_mut() {
                d.last_applied = delta;
            }
        }
        Tool::Scale => {
            let Some(p) = axis_param(drag.pivot, drag.axis_dir, ray) else { return };
            let delta = snap(p - drag.start_param, snap_cfg.move_step, snap_cfg.move_enabled);
            if delta == drag.last_applied {
                return;
            }
            let mut doc = studio.doc();
            // Resize single parts along the handle's local axis; the
            // opposite face stays put.
            for (r, cf, size) in &drag.originals {
                let Some(size) = size else { continue };
                let t = cframe_to_transform(cf);
                let axes = [*t.right(), *t.up(), *t.back()];
                // Find the part axis most aligned with the drag axis.
                let (axis, align) = axes
                    .iter()
                    .enumerate()
                    .map(|(i, a)| (i, a.dot(drag.axis_dir)))
                    .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
                    .unwrap();
                let mut s = Vec3::new(size.x, size.y, size.z);
                let min_size = if snap_cfg.move_enabled { snap_cfg.move_step.max(0.001) } else { 0.001 };
                let new_len = (s[axis] + delta).max(min_size);
                let applied = new_len - s[axis];
                s[axis] = new_len;
                let shift = axes[axis] * align.signum() * applied * 0.5;
                let mut c = *cf;
                c.position = Vector3::new(c.position.x + shift.x, c.position.y + shift.y, c.position.z + shift.z);
                doc.write_raw(*r, "Size", Variant::Vector3(Vector3::new(s.x, s.y, s.z)));
                doc.write_raw(*r, "CFrame", Variant::CFrame(c));
            }
            drop(doc);
            if let Some(d) = state.drag.as_mut() {
                d.last_applied = delta;
            }
        }
        Tool::Rotate => {
            let Some(a) = ring_angle(drag.pivot, drag.axis_dir, ray) else { return };
            let mut delta = (a - drag.start_param).to_degrees();
            delta = (delta + 540.0).rem_euclid(360.0) - 180.0;
            let delta = snap(delta, snap_cfg.rotate_step, snap_cfg.rotate_enabled);
            if delta == drag.last_applied {
                return;
            }
            let rot = Quat::from_axis_angle(drag.axis_dir, delta.to_radians());
            let mut doc = studio.doc();
            for (r, cf, _) in &drag.originals {
                let mut t = cframe_to_transform(cf);
                t.translation = drag.pivot + rot * (t.translation - drag.pivot);
                t.rotation = (rot * t.rotation).normalize();
                doc.write_raw(*r, "CFrame", Variant::CFrame(transform_to_cframe(&t)));
            }
            drop(doc);
            if let Some(d) = state.drag.as_mut() {
                d.last_applied = delta;
            }
        }
        Tool::Select => {}
    }
}

fn apply_free_drag(studio: &mut Studio, state: &mut ToolState, free: &FreeDrag, ray: &Ray3d) {
    let dy = ray.direction.y;
    if dy.abs() < 1e-4 {
        return;
    }
    let t = (free.plane_y - ray.origin.y) / dy;
    if t <= 0.0 {
        return;
    }
    let hit = ray.origin + *ray.direction * t;
    let snap_cfg = studio.snap;
    let mut delta = hit - free.start_hit;
    delta.x = snap(delta.x, snap_cfg.move_step, snap_cfg.move_enabled);
    delta.z = snap(delta.z, snap_cfg.move_step, snap_cfg.move_enabled);
    delta.y = 0.0;
    if delta == Vec3::ZERO && !free.moved {
        return;
    }
    let mut doc = studio.doc();
    for (r, cf) in &free.originals {
        let mut c = *cf;
        c.position = Vector3::new(c.position.x + delta.x, c.position.y, c.position.z + delta.z);
        doc.write_raw(*r, "CFrame", Variant::CFrame(c));
    }
    drop(doc);
    if let Some(f) = state.free_drag.as_mut() {
        f.moved = true;
    }
}

/// Draws selection boxes, hover box and tool handles.
pub fn draw_tool_gizmos(
    mut gizmos: Gizmos,
    studio: Res<Studio>,
    state: Res<ToolState>,
    camera: Query<&GlobalTransform, With<EditorCamera>>,
) {
    let Ok(cam_t) = camera.single() else { return };
    let cam_pos = cam_t.translation();
    let doc = studio.doc();

    let draw_box = |gizmos: &mut Gizmos, r: Ref, color: Color| {
        if let Some(g) = doc.get(r).and_then(part_geometry) {
            let mut t = cframe_to_transform(&g.cframe);
            t.scale = Vec3::new(g.size.x, g.size.y, g.size.z);
            gizmos.cube(t, color);
        } else if let Some((a, b)) = doc.bounds(r) {
            let min = Vec3::new(a.x, a.y, a.z);
            let max = Vec3::new(b.x, b.y, b.z);
            gizmos.cube(Transform::from_translation((min + max) * 0.5).with_scale(max - min), color);
        }
    };

    if let Some(h) = state.hovered
        && !studio.is_selected(h) {
            draw_box(&mut gizmos, h, HOVER_COLOR);
        }
    for r in &studio.selection {
        if !doc.exists(*r) {
            continue;
        }
        draw_box(&mut gizmos, *r, SELECT_COLOR);
    }

    let Some((t, size)) = selection_box(&doc, &studio.selection, studio.local_space) else { return };
    let dist = (cam_pos - t.translation).length();
    let handle_size = (dist * 0.035).max(0.25);
    match studio.tool {
        Tool::Move => {
            for (h, p, dir) in handle_points(&t, size, Tool::Move, cam_pos) {
                let hot = state.hovered_handle == Some(h) || state.drag.as_ref().is_some_and(|d| d.handle == h);
                let color = if hot { Color::srgb(1.0, 0.85, 0.2) } else { AXIS_COLORS[h.axis] };
                let base = p - dir * handle_size * 1.5;
                gizmos.arrow(base, p + dir * handle_size * 1.5, color).with_tip_length(handle_size * 1.2);
                gizmos.sphere(Isometry3d::from_translation(base), handle_size * 0.15, color);
            }
        }
        Tool::Scale => {
            for (h, p, _) in handle_points(&t, size, Tool::Scale, cam_pos) {
                let hot = state.hovered_handle == Some(h) || state.drag.as_ref().is_some_and(|d| d.handle == h);
                let color = if hot { Color::srgb(1.0, 0.85, 0.2) } else { AXIS_COLORS[h.axis] };
                gizmos.sphere(Isometry3d::from_translation(p), handle_size * 0.6, color);
            }
        }
        Tool::Rotate => {
            let radius = ring_radius(size, dist);
            let axes = [*t.right(), *t.up(), *t.back()];
            for (axis, dir) in axes.iter().enumerate() {
                let hot = state.hovered_handle.is_some_and(|h| h.axis == axis)
                    || state.drag.as_ref().is_some_and(|d| d.handle.axis == axis);
                let color = if hot { Color::srgb(1.0, 0.85, 0.2) } else { AXIS_COLORS[axis] };
                let rot = Quat::from_rotation_arc(Vec3::Z, *dir);
                gizmos.circle(Isometry3d::new(t.translation, rot), radius, color).resolution(64);
            }
        }
        Tool::Select => {}
    }
}
