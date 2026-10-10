//! Unit meshes for Roblox part shapes. Every part of a shape shares one mesh
//! and is sized with its transform's scale, which keeps memory flat no matter
//! how many parts a place has.

use std::collections::HashMap;
use std::path::Path;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use phaedrus_dom::Instance;
use phaedrus_dom::geometry::PartShape;
use phaedrus_dom::union::{UnionMesh, load_union_mesh};

#[derive(Resource)]
pub struct ShapeMeshes {
    pub block: Handle<Mesh>,
    pub ball: Handle<Mesh>,
    pub cylinder: Handle<Mesh>,
    pub wedge: Handle<Mesh>,
    pub corner_wedge: Handle<Mesh>,
    /// Unit quad facing +Z, used for Decal/Texture overlays.
    pub quad: Handle<Mesh>,
}

impl ShapeMeshes {
    pub fn get(&self, shape: PartShape) -> Handle<Mesh> {
        match shape {
            PartShape::Block | PartShape::Proxy | PartShape::Union(_) => self.block.clone(),
            PartShape::Ball => self.ball.clone(),
            PartShape::Cylinder => self.cylinder.clone(),
            PartShape::Wedge => self.wedge.clone(),
            PartShape::CornerWedge => self.corner_wedge.clone(),
        }
    }
}

/// Decoded union meshes keyed by [`PartShape::Union`]. Misses (data not
/// downloaded yet, or undecodable) are cached too so they are not retried
/// every time the part changes; [`Self::forget_missing`] retries them.
#[derive(Resource, Default)]
pub struct UnionMeshes {
    loaded: HashMap<u64, Option<Handle<Mesh>>>,
}

impl UnionMeshes {
    pub fn get(
        &mut self,
        key: u64,
        inst: &Instance,
        cache_dir: Option<&Path>,
        meshes: &mut Assets<Mesh>,
    ) -> Option<Handle<Mesh>> {
        self.loaded
            .entry(key)
            .or_insert_with(|| match load_union_mesh(inst, cache_dir) {
                Ok(m) => m.filter(|m| !m.indices.is_empty()).map(|m| meshes.add(union_mesh(m))),
                Err(e) => {
                    warn!("could not decode union {:?}: {e:#}", inst.name);
                    None
                }
            })
            .clone()
    }

    /// Whether `key` was looked up and had no mesh.
    pub fn is_missing(&self, key: u64) -> bool {
        matches!(self.loaded.get(&key), Some(None))
    }

    /// Drops cached misses, e.g. after new assets were downloaded.
    pub fn forget_missing(&mut self) {
        self.loaded.retain(|_, h| h.is_some());
    }

    pub fn clear(&mut self) {
        self.loaded.clear();
    }
}

fn union_mesh(m: UnionMesh) -> Mesh {
    let has_normals = m.normals.len() == m.positions.len();
    let has_colors = m.colors.len() == m.positions.len();
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, m.positions)
        .with_inserted_indices(Indices::U32(m.indices));
    if has_normals {
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, m.normals);
    } else {
        mesh.compute_normals();
    }
    if has_colors {
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, m.colors);
    }
    mesh
}

pub fn create(meshes: &mut Assets<Mesh>) -> ShapeMeshes {
    // Roblox cylinders run along the part's X axis.
    let mut cylinder: Mesh = Cylinder::new(0.5, 1.0).mesh().resolution(32).into();
    cylinder.rotate_by(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2));
    ShapeMeshes {
        block: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
        ball: meshes.add(Sphere::new(0.5).mesh().uv(32, 18)),
        cylinder: meshes.add(cylinder),
        wedge: meshes.add(wedge_mesh()),
        corner_wedge: meshes.add(corner_wedge_mesh()),
        quad: meshes.add(Rectangle::new(1.0, 1.0)),
    }
}

/// Builds a flat-shaded mesh from triangles given as vertex triples.
fn flat_mesh(tris: &[[Vec3; 3]]) -> Mesh {
    let mut positions = Vec::with_capacity(tris.len() * 3);
    let mut normals = Vec::with_capacity(tris.len() * 3);
    let mut uvs = Vec::with_capacity(tris.len() * 3);
    for t in tris {
        let n = (t[1] - t[0]).cross(t[2] - t[0]).normalize_or_zero();
        // Planar UVs from the dominant axis of the face normal.
        let a = n.abs();
        for v in t {
            positions.push(v.to_array());
            normals.push(n.to_array());
            let uv = if a.x >= a.y && a.x >= a.z {
                Vec2::new(v.z, -v.y)
            } else if a.y >= a.z {
                Vec2::new(v.x, v.z)
            } else {
                Vec2::new(v.x, -v.y)
            };
            uvs.push((uv + 0.5).to_array());
        }
    }
    let indices: Vec<u32> = (0..positions.len() as u32).collect();
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
        .with_inserted_indices(Indices::U32(indices))
}

/// Wedge: full bottom and back (+Z) faces, slope rising from the front-bottom
/// edge to the back-top edge.
fn wedge_mesh() -> Mesh {
    let h = 0.5;
    // Bottom corners.
    let bfl = Vec3::new(-h, -h, -h);
    let bfr = Vec3::new(h, -h, -h);
    let bbl = Vec3::new(-h, -h, h);
    let bbr = Vec3::new(h, -h, h);
    // Top back edge.
    let tbl = Vec3::new(-h, h, h);
    let tbr = Vec3::new(h, h, h);
    flat_mesh(&[
        // bottom
        [bfl, bfr, bbr],
        [bfl, bbr, bbl],
        // back
        [bbl, bbr, tbr],
        [bbl, tbr, tbl],
        // slope
        [bfl, tbl, tbr],
        [bfl, tbr, bfr],
        // left side
        [bfl, bbl, tbl],
        // right side
        [bfr, tbr, bbr],
    ])
}

/// Corner wedge: square base with the apex above the right-front corner.
fn corner_wedge_mesh() -> Mesh {
    let h = 0.5;
    let bfl = Vec3::new(-h, -h, -h);
    let bfr = Vec3::new(h, -h, -h);
    let bbl = Vec3::new(-h, -h, h);
    let bbr = Vec3::new(h, -h, h);
    let apex = Vec3::new(h, h, -h);
    flat_mesh(&[
        // bottom
        [bfl, bfr, bbr],
        [bfl, bbr, bbl],
        // right face (vertical)
        [bfr, apex, bbr],
        // front face (vertical)
        [bfl, apex, bfr],
        // sloped faces
        [bbl, bbr, apex],
        [bfl, bbl, apex],
    ])
}
