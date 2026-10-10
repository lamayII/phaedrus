//! Mirrors BaseParts under Workspace into ECS entities.

use std::collections::{HashMap, HashSet};

use bevy::camera::visibility::NoCpuCulling;
use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use phaedrus_dom::geometry::{PartGeometry, PartShape, part_geometry};
use phaedrus_dom::types::{Ref, Variant};
use phaedrus_dom::{Change, Document};

use super::culling::CullGrid;
use super::lighting::{GraphicsSettings, Technology, profile};
use super::materials::{MaterialCache, MaterialKey};
use super::meshes::{PartMeshes, ShapeMeshes};
use crate::asset_loader::AssetDownloads;
use crate::assets::ContentProvider;
use crate::studio::Studio;

/// Links an entity back to its instance.
#[derive(Component, Debug, Clone, Copy)]
pub struct PartRef(pub Ref);

/// A Decal/Texture drawn on a part face.
#[derive(Component)]
pub struct Overlay;

#[derive(Resource, Default)]
pub struct PartIndex {
    pub map: HashMap<Ref, Entity>,
    /// Last geometry applied, to skip redundant updates.
    applied: HashMap<Ref, PartGeometry>,
    /// Overlay entities per part.
    overlays: HashMap<Ref, Vec<Entity>>,
    generation: u64,
    /// Bumped when part entities are spawned or despawned (not when they
    /// move or change appearance).
    structure: u64,
    /// `QualityProfile::min_caster_size` the shadow flags were set with.
    min_caster_size: f32,
    /// [`AssetDownloads::version`] last seen, to pick up downloaded assets.
    downloads_seen: u64,
    /// Parts drawn without an asset (mesh or texture) that was not
    /// available; redrawn when downloads land.
    pending: HashSet<Ref>,
}

impl PartIndex {
    pub fn structure_version(&self) -> u64 {
        self.structure
    }

    /// Whether the part is fully transparent (and so hidden).
    pub fn applied_transparent(&self, r: Ref) -> bool {
        self.applied.get(&r).is_some_and(|g| g.transparency >= 0.999)
    }
}

/// Whether a part casts a sun shadow. Tiny parts are skipped: at cascade
/// resolution their shadow is a few texels, but each caster is still drawn
/// once per cascade.
fn casts_shadow(g: &PartGeometry, min_size: f32) -> bool {
    g.cast_shadow && g.transparency < 0.5 && g.size.x.max(g.size.y).max(g.size.z) >= min_size
}

pub fn rbx_to_transform(g: &PartGeometry) -> Transform {
    let c = &g.cframe;
    let m = &c.orientation;
    // rbx_types stores rows; bevy wants columns.
    let rot = Mat3::from_cols(
        Vec3::new(m.x.x, m.y.x, m.z.x),
        Vec3::new(m.x.y, m.y.y, m.z.y),
        Vec3::new(m.x.z, m.y.z, m.z.z),
    );
    let size = Vec3::new(g.size.x, g.size.y, g.size.z);
    let scale = match g.shape {
        PartShape::Ball => Vec3::splat(size.min_element()),
        PartShape::Cylinder => {
            let d = size.y.min(size.z);
            Vec3::new(size.x, d, d)
        }
        _ => size,
    };
    Transform {
        translation: Vec3::new(c.position.x, c.position.y, c.position.z),
        rotation: Quat::from_mat3(&rot).normalize(),
        scale: scale.max(Vec3::splat(0.001)),
    }
}

pub fn cframe_to_transform(c: &phaedrus_dom::types::CFrame) -> Transform {
    let m = &c.orientation;
    let rot = Mat3::from_cols(
        Vec3::new(m.x.x, m.y.x, m.z.x),
        Vec3::new(m.x.y, m.y.y, m.z.y),
        Vec3::new(m.x.z, m.y.z, m.z.z),
    );
    Transform {
        translation: Vec3::new(c.position.x, c.position.y, c.position.z),
        rotation: Quat::from_mat3(&rot).normalize(),
        scale: Vec3::ONE,
    }
}

pub fn transform_to_cframe(t: &Transform) -> phaedrus_dom::types::CFrame {
    use phaedrus_dom::types::{Matrix3, Vector3 as V};
    let r = Mat3::from_quat(t.rotation);
    phaedrus_dom::types::CFrame::new(
        V::new(t.translation.x, t.translation.y, t.translation.z),
        Matrix3::new(
            V::new(r.x_axis.x, r.y_axis.x, r.z_axis.x),
            V::new(r.x_axis.y, r.y_axis.y, r.z_axis.y),
            V::new(r.x_axis.z, r.y_axis.z, r.z_axis.z),
        ),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn sync_parts(
    mut commands: Commands,
    studio: Res<Studio>,
    mut index: ResMut<PartIndex>,
    shapes: Res<ShapeMeshes>,
    mut part_meshes: ResMut<PartMeshes>,
    mut meshes: ResMut<Assets<Mesh>>,
    downloads: Res<AssetDownloads>,
    mut mat_cache: ResMut<MaterialCache>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut content: ResMut<ContentProvider>,
    settings: Res<GraphicsSettings>,
    mut grid: ResMut<CullGrid>,
    mut transforms: Query<&mut Transform, With<PartRef>>,
) {
    let doc = studio.doc();
    // Technology does not affect the caster size.
    let min_caster_size = profile(settings.quality, Technology::Future).min_caster_size;
    let mut ctx = SyncCtx {
        commands: &mut commands,
        shapes: &shapes,
        part_meshes: &mut part_meshes,
        meshes: &mut meshes,
        mat_cache: &mut mat_cache,
        materials: &mut materials,
        images: &mut images,
        content: &mut content,
        grid: &mut grid,
        min_caster_size,
    };

    if index.min_caster_size != min_caster_size && index.generation == studio.generation {
        index.min_caster_size = min_caster_size;
        for (r, g) in &index.applied {
            let Some(&e) = index.map.get(r) else { continue };
            if casts_shadow(g, min_caster_size) {
                ctx.commands.entity(e).remove::<NotShadowCaster>();
            } else {
                ctx.commands.entity(e).insert(NotShadowCaster);
            }
        }
    }

    if index.generation != studio.generation {
        // Full rebuild.
        for (_, e) in index.map.drain() {
            ctx.commands.entity(e).despawn();
        }
        index.applied.clear();
        index.overlays.clear();
        index.pending.clear();
        ctx.mat_cache.clear();
        ctx.part_meshes.clear();
        ctx.grid.clear();
        index.generation = studio.generation;
        index.structure += 1;
        index.min_caster_size = min_caster_size;
        if let Some(ws) = doc.workspace() {
            for r in doc.descendants(ws) {
                upsert(&doc, &mut index, &mut ctx, r, &mut transforms);
            }
        }
        log_missing(&studio, &index, &ctx);
        return;
    }

    if index.downloads_seen != downloads.version() {
        index.downloads_seen = downloads.version();
        refresh_pending(&doc, &mut index, &mut ctx);
    }

    if studio.frame_changes.is_empty() {
        return;
    }
    let ws = doc.workspace();
    let mut touched: HashSet<Ref> = HashSet::new();
    let mut overlay_parents: HashSet<Ref> = HashSet::new();
    for change in &studio.frame_changes {
        match change {
            Change::Added(r) | Change::Reparented { instance: r, .. } => {
                touched.insert(*r);
                for d in doc.descendants(*r) {
                    touched.insert(d);
                }
                if let Change::Reparented { old, .. } = change {
                    overlay_parents.insert(*old);
                }
                overlay_parents.insert(doc.parent(*r));
            }
            Change::Property(r, _) => {
                touched.insert(*r);
                if doc.get(*r).is_some_and(|i| is_overlay_class(&i.class) || i.class == "SurfaceAppearance") {
                    overlay_parents.insert(doc.parent(*r));
                }
            }
        }
    }
    for r in touched {
        let in_ws = ws.is_some_and(|w| doc.is_descendant_of(r, w));
        if in_ws && doc.exists(r) {
            upsert(&doc, &mut index, &mut ctx, r, &mut transforms);
        } else {
            remove(&mut index, &mut ctx, r);
        }
    }
    // Decals, Textures and SurfaceAppearances are drawn through their part.
    for p in overlay_parents {
        refresh_part(&doc, &mut index, &mut ctx, p);
    }
}

struct SyncCtx<'a, 'w, 's> {
    commands: &'a mut Commands<'w, 's>,
    shapes: &'a ShapeMeshes,
    part_meshes: &'a mut PartMeshes,
    meshes: &'a mut Assets<Mesh>,
    mat_cache: &'a mut MaterialCache,
    materials: &'a mut Assets<StandardMaterial>,
    images: &'a mut Assets<Image>,
    content: &'a mut ContentProvider,
    grid: &'a mut CullGrid,
    min_caster_size: f32,
}

/// The mesh a part draws with, and whether it is a stand-in for a mesh that
/// is not available (yet): unions and MeshParts draw as their bounding box
/// until then.
fn mesh_for(ctx: &mut SyncCtx, g: &PartGeometry, inst: &phaedrus_dom::Instance) -> (Handle<Mesh>, bool) {
    if !matches!(g.shape, PartShape::Union(_) | PartShape::Mesh(_)) {
        return (ctx.shapes.get(g.shape), false);
    }
    match ctx.part_meshes.get(g.shape, inst, ctx.content.asset_cache_dir(), ctx.meshes) {
        Some(h) => (h, false),
        None => (ctx.shapes.get(g.shape), true),
    }
}

/// A MeshPart's texture: its SurfaceAppearance's colour map, else its
/// `TextureContent`.
fn mesh_texture_url(doc: &Document, r: Ref, inst: &phaedrus_dom::Instance) -> Option<String> {
    use phaedrus_dom::content::{COLOR_MAP_PROPS, MESH_TEXTURE_PROPS, content_url};
    if inst.class != "MeshPart" {
        return None;
    }
    let surface = doc.children(r).iter().filter_map(|c| doc.get(*c)).find(|c| c.class == "SurfaceAppearance");
    surface
        .and_then(|s| content_url(s, COLOR_MAP_PROPS))
        .or_else(|| content_url(inst, MESH_TEXTURE_PROPS))
        .map(str::to_owned)
}

/// The part's material, and whether its texture is missing (it then draws
/// untextured).
fn material_for(doc: &Document, ctx: &mut SyncCtx, r: Ref, g: &PartGeometry, inst: &phaedrus_dom::Instance) -> (Handle<StandardMaterial>, bool) {
    // Textures need the mesh's UVs; the bounding-box stand-in has none that match.
    let textured = matches!(g.shape, PartShape::Mesh(k) if !ctx.part_meshes.is_missing(k));
    if textured && let Some(url) = mesh_texture_url(doc, r, inst) {
        return match ctx.content.image(&url, ctx.images) {
            Some(image) => (ctx.mat_cache.textured(key_of(g), &image, ctx.materials), false),
            None => (ctx.mat_cache.get(key_of(g), ctx.materials), true),
        };
    }
    (ctx.mat_cache.get(key_of(g), ctx.materials), false)
}

/// Redraws a part's mesh, material and overlays, e.g. after an asset it
/// uses was downloaded or one of its SurfaceAppearances changed.
fn refresh_part(doc: &Document, index: &mut PartIndex, ctx: &mut SyncCtx, r: Ref) {
    let (Some(&e), Some(g), Some(inst)) = (index.map.get(&r), index.applied.get(&r).copied(), doc.get(r)) else { return };
    index.pending.remove(&r);
    let (mesh, mesh_missing) = mesh_for(ctx, &g, inst);
    let (material, texture_missing) = material_for(doc, ctx, r, &g, inst);
    ctx.commands.entity(e).insert((Mesh3d(mesh), MeshMaterial3d(material)));
    if mesh_missing || texture_missing {
        index.pending.insert(r);
    }
    rebuild_overlays(doc, index, ctx, r, e);
}

/// Retries parts that were drawn without an asset, after downloads landed.
fn refresh_pending(doc: &Document, index: &mut PartIndex, ctx: &mut SyncCtx) {
    if index.pending.is_empty() {
        return;
    }
    ctx.part_meshes.forget_missing();
    ctx.content.forget_missing();
    for r in std::mem::take(&mut index.pending) {
        refresh_part(doc, index, ctx, r);
    }
}

/// Reports how many unions and MeshParts are still bounding boxes.
fn log_missing(studio: &Studio, index: &PartIndex, ctx: &SyncCtx) {
    let mut counts = [(0usize, 0usize); 2];
    for g in index.applied.values() {
        let (slot, key) = match g.shape {
            PartShape::Union(k) => (0, k),
            PartShape::Mesh(k) => (1, k),
            _ => continue,
        };
        counts[slot].0 += 1;
        counts[slot].1 += usize::from(ctx.part_meshes.is_missing(key));
    }
    for ((total, boxed), what) in counts.into_iter().zip(["unions", "MeshParts"]) {
        if boxed > 0 {
            studio.log(
                phaedrus_luau::OutputLevel::Info,
                format!("{} of {total} {what} rendered; {boxed} show as bounding boxes until their meshes are downloaded.", total - boxed),
            );
        }
    }
}

fn is_overlay_class(class: &str) -> bool {
    class == "Decal" || class == "Texture"
}

fn upsert(
    doc: &Document,
    index: &mut PartIndex,
    ctx: &mut SyncCtx,
    r: Ref,
    transforms: &mut Query<&mut Transform, With<PartRef>>,
) {
    let Some(inst) = doc.get(r) else { return };
    // Decals/Textures are rebuilt through their parent part.
    let Some(g) = part_geometry(inst) else { return };
    let transparent = g.transparency >= 0.999;
    if let Some(&e) = index.map.get(&r) {
        let Some(old) = index.applied.get(&r).copied() else { return };
        if old == g {
            return;
        }
        let only_moved = old.shape == g.shape
            && old.size == g.size
            && old.color == g.color
            && old.transparency == g.transparency
            && old.reflectance == g.reflectance
            && old.material == g.material
            && old.cast_shadow == g.cast_shadow
            && old.texture == g.texture;
        if only_moved {
            if let Ok(mut t) = transforms.get_mut(e) {
                *t = rbx_to_transform(&g);
            } else {
                ctx.commands.entity(e).insert(rbx_to_transform(&g));
            }
        } else {
            let (mesh, mesh_missing) = mesh_for(ctx, &g, inst);
            let (material, texture_missing) = material_for(doc, ctx, r, &g, inst);
            if mesh_missing || texture_missing {
                index.pending.insert(r);
            } else {
                index.pending.remove(&r);
            }
            let mut ec = ctx.commands.entity(e);
            ec.insert((
                rbx_to_transform(&g),
                Mesh3d(mesh),
                MeshMaterial3d(material),
                if transparent { Visibility::Hidden } else { Visibility::Inherited },
            ));
            if casts_shadow(&g, ctx.min_caster_size) {
                ec.remove::<NotShadowCaster>();
            } else {
                ec.insert(NotShadowCaster);
            }
            if old.size != g.size {
                rebuild_overlays(doc, index, ctx, r, e);
            }
        }
        index.applied.insert(r, g);
        ctx.grid.mark_dirty(e);
        return;
    }
    let (mesh, mesh_missing) = mesh_for(ctx, &g, inst);
    let (material, texture_missing) = material_for(doc, ctx, r, &g, inst);
    if mesh_missing || texture_missing {
        index.pending.insert(r);
    }
    let mut ec = ctx.commands.spawn((
        PartRef(r),
        // Frustum culling happens on the GPU during mesh preprocessing.
        // Bevy's CPU culling re-tests every part for every view each frame
        // and, as the camera turns, re-extracts and re-bins every part that
        // enters or leaves view; with 100k+ parts that dominated frame time.
        // Distance culling is done per grid cell in `culling`.
        NoCpuCulling,
        rbx_to_transform(&g),
        Mesh3d(mesh),
        MeshMaterial3d(material),
        if transparent { Visibility::Hidden } else { Visibility::Inherited },
    ));
    if !casts_shadow(&g, ctx.min_caster_size) {
        ec.insert(NotShadowCaster);
    }
    let e = ec.id();
    index.map.insert(r, e);
    index.structure += 1;
    ctx.grid.mark_dirty(e);
    index.applied.insert(r, g);
    rebuild_overlays(doc, index, ctx, r, e);
}

fn key_of(g: &PartGeometry) -> MaterialKey {
    MaterialKey::new(g.color, g.transparency, g.reflectance, g.material)
}

fn remove(index: &mut PartIndex, ctx: &mut SyncCtx, r: Ref) {
    if let Some(e) = index.map.remove(&r) {
        ctx.commands.entity(e).despawn();
        ctx.grid.remove(e);
        index.structure += 1;
    }
    index.applied.remove(&r);
    index.overlays.remove(&r);
    index.pending.remove(&r);
}

/// NormalId -> (local translation axis, rotation of a +Z quad onto that face).
fn face_frame(face: u32) -> (Vec3, Quat) {
    use std::f32::consts::{FRAC_PI_2, PI};
    match face {
        0 => (Vec3::X, Quat::from_rotation_y(FRAC_PI_2)),
        1 => (Vec3::Y, Quat::from_rotation_x(-FRAC_PI_2)),
        2 => (Vec3::Z, Quat::IDENTITY),
        3 => (Vec3::NEG_X, Quat::from_rotation_y(-FRAC_PI_2)),
        4 => (Vec3::NEG_Y, Quat::from_rotation_x(FRAC_PI_2)),
        _ => (Vec3::NEG_Z, Quat::from_rotation_y(PI)),
    }
}

fn rebuild_overlays(doc: &Document, index: &mut PartIndex, ctx: &mut SyncCtx, part: Ref, part_entity: Entity) {
    if let Some(old) = index.overlays.remove(&part) {
        for e in old {
            ctx.commands.entity(e).despawn();
        }
    }
    let Some(g) = index.applied.get(&part).copied() else { return };
    if !matches!(g.shape, PartShape::Block | PartShape::Proxy) {
        return;
    }
    let size = Vec3::new(g.size.x, g.size.y, g.size.z);
    let mut spawned = Vec::new();
    for &c in doc.children(part) {
        let Some(inst) = doc.get(c) else { continue };
        if !is_overlay_class(&inst.class) {
            continue;
        }
        let face = match doc.get_property(c, "Face") {
            Some(Variant::Enum(e)) => e.to_u32(),
            _ => 5,
        };
        let transparency = match doc.get_property(c, "Transparency") {
            Some(Variant::Float32(t)) => t,
            _ => 0.0,
        };
        if transparency >= 0.999 {
            continue;
        }
        let color = doc.color_of(c, "Color3").unwrap_or([1.0, 1.0, 1.0]);
        let url = match doc.get_property(c, "TextureContent").or_else(|| doc.get_property(c, "Texture")) {
            Some(Variant::Content(content)) => match content.value() {
                phaedrus_dom::types::ContentType::Uri(u) => u.clone(),
                _ => String::new(),
            },
            Some(Variant::ContentId(id)) => id.as_str().to_owned(),
            Some(Variant::String(s)) => s,
            _ => String::new(),
        };
        let is_texture = inst.class == "Texture";
        let found = ctx.content.image(&url, ctx.images);
        if found.is_none() && !url.trim().is_empty() {
            index.pending.insert(part);
        }
        let image = found.or_else(|| {
            // Unknown textures render as the stock grid, which is what most
            // baseplates use.
            is_texture.then(|| ctx.content.grid_texture(ctx.images))
        });
        let Some(image) = image else { continue };

        let (axis, rot) = face_frame(face);
        // Face extents in studs (u along the quad's local X, v along Y).
        let (fu, fv) = match face {
            0 | 3 => (size.z, size.y),
            1 | 4 => (size.x, size.z),
            _ => (size.x, size.y),
        };
        let uv_scale = if is_texture {
            let su = match doc.get_property(c, "StudsPerTileU") {
                Some(Variant::Float32(v)) if v > 0.0 => v,
                _ => 2.0,
            };
            let sv = match doc.get_property(c, "StudsPerTileV") {
                Some(Variant::Float32(v)) if v > 0.0 => v,
                _ => 2.0,
            };
            Vec2::new(fu / su, fv / sv)
        } else {
            Vec2::ONE
        };
        // Identical overlays (e.g. the same Texture on every brick) share a
        // material so they batch into one draw.
        let material = ctx.mat_cache.overlay(&image, color, transparency, uv_scale, ctx.materials);
        // The overlay is a child of the part, so it inherits the part's
        // scale; offset it a hair above the face in unit space.
        let axis_size = (axis.abs() * size).max_element().max(0.001);
        let offset = 0.5 + 0.004 / axis_size;
        let e = ctx
            .commands
            .spawn((
                Overlay,
                Mesh3d(ctx.shapes.quad.clone()),
                MeshMaterial3d(material),
                Transform { translation: axis * offset, rotation: rot, scale: Vec3::ONE },
                NotShadowCaster,
                ChildOf(part_entity),
            ))
            .id();
        spawned.push(e);
    }
    if !spawned.is_empty() {
        index.overlays.insert(part, spawned);
    }
}
