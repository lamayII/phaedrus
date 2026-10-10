//! Decodes MeshPart `.mesh` files (versions 1–5) with `rbx_mesh`, and the
//! decoded-mesh type shared with unions.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Cursor;
use std::path::Path;

use rbx_dom_weak::Instance;
use rbx_mesh::mesh::{Face2, Lod3, Mesh, Vertex2, Vertex2Truncated, Vertices2};

use crate::content::{AssetKind, MESH_PROPS, cache_file, content_asset_id};

/// A decoded mesh, normalised to a unit cube centred on the origin like the
/// primitive shape meshes, so the part's transform scale is its `Size`.
#[derive(Debug, Clone, Default)]
pub struct DecodedMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// Per-vertex texture coordinates; empty when the format has none.
    pub uvs: Vec<[f32; 2]>,
    /// Per-vertex RGBA (0..1); empty when the format has none.
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
}

impl DecodedMesh {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        if self.indices.iter().any(|&i| i as usize >= self.positions.len()) {
            anyhow::bail!("mesh index out of range");
        }
        Ok(())
    }
}

/// The MeshPart's mesh asset id.
pub fn mesh_asset_id(inst: &Instance) -> Option<u64> {
    content_asset_id(inst, MESH_PROPS)
}

/// A key identifying the decoded mesh: MeshParts using the same mesh share
/// it, since normalisation only depends on the mesh itself.
pub fn mesh_key(inst: &Instance) -> Option<u64> {
    let id = mesh_asset_id(inst)?;
    let mut h = DefaultHasher::new();
    0x4D35u32.hash(&mut h);
    id.hash(&mut h);
    Some(h.finish())
}

/// Decodes the MeshPart's mesh from `cache_dir`. `Ok(None)` means it has
/// not been downloaded (yet).
pub fn load_mesh_part(inst: &Instance, cache_dir: Option<&Path>) -> anyhow::Result<Option<DecodedMesh>> {
    let (Some(id), Some(dir)) = (mesh_asset_id(inst), cache_dir) else { return Ok(None) };
    let bytes = match std::fs::read(cache_file(dir, id, AssetKind::Mesh)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut mesh = decode_mesh(&bytes)?;
    if mesh.indices.is_empty() {
        return Ok(None);
    }
    normalise_to_bounds(&mut mesh);
    Ok(Some(mesh))
}

/// Decodes a `.mesh` file in its own units, using the highest-detail LOD.
pub fn decode_mesh(bytes: &[u8]) -> anyhow::Result<DecodedMesh> {
    let (vertices, faces, lods): (Vec<Vertex2Truncated>, Vec<Face2>, Vec<Lod3>) =
        match rbx_mesh::read_mesh_versioned(Cursor::new(bytes))? {
            Mesh::V1(m) => {
                // Version 1 stores unindexed triangles.
                let mut out = DecodedMesh::default();
                for v in m.vertices {
                    out.positions.push(v.pos);
                    out.normals.push(v.norm);
                    out.uvs.push([v.tex[0], v.tex[1]]);
                }
                out.indices = (0..out.positions.len() as u32).collect();
                out.indices.truncate(out.indices.len() / 3 * 3);
                out.validate()?;
                return Ok(out);
            }
            Mesh::V2(m) => (truncate(m.vertices), m.faces, Vec::new()),
            Mesh::V3(m) => (truncate(m.vertices), m.faces, m.lods),
            Mesh::V4(m) => (m.vertices.into_iter().map(truncate_one).collect(), m.faces, m.lods),
            Mesh::V5(m) => (m.vertices.into_iter().map(truncate_one).collect(), m.faces, m.lods),
        };
    let mut out = DecodedMesh::default();
    for v in vertices {
        out.positions.push(v.pos);
        out.normals.push(v.norm);
        out.uvs.push(v.tex);
    }
    // LOD entries are face offsets; the first range is the full-detail mesh.
    let range = match lods.as_slice() {
        [a, b, ..] if a.0 < b.0 && (b.0 as usize) <= faces.len() => a.0 as usize..b.0 as usize,
        _ => 0..faces.len(),
    };
    out.indices = faces[range].iter().flat_map(|f| f.0.iter().map(|v| v.0)).collect();
    out.validate()?;
    Ok(out)
}

fn truncate(v: Vertices2) -> Vec<Vertex2Truncated> {
    match v {
        Vertices2::Full(v) => v.into_iter().map(truncate_one).collect(),
        Vertices2::Truncated(v) => v,
    }
}

fn truncate_one(v: Vertex2) -> Vertex2Truncated {
    Vertex2Truncated { pos: v.pos, norm: v.norm, tex: v.tex, tangent: v.tangent }
}

/// Extent of the positions along each axis.
pub fn bounds_size(positions: &[[f32; 3]]) -> [f32; 3] {
    let (lo, hi) = bounds(positions);
    [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]]
}

fn bounds(positions: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
    if positions.is_empty() {
        return ([-0.5; 3], [0.5; 3]);
    }
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    for p in positions {
        for a in 0..3 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    (lo, hi)
}

/// Scales by `size` (and recentres on `centre`) into a unit cube. Normals
/// get the inverse-transpose of that scale so they stay perpendicular to
/// the squashed surface.
pub(crate) fn normalise(mesh: &mut DecodedMesh, centre: [f32; 3], size: [f32; 3]) {
    let size = size.map(|s| if s.abs() > 1e-4 { s } else { 1e-4 });
    for p in &mut mesh.positions {
        for a in 0..3 {
            p[a] = (p[a] - centre[a]) / size[a];
        }
    }
    for n in &mut mesh.normals {
        let m = [n[0] * size[0], n[1] * size[1], n[2] * size[2]];
        let len = (m[0] * m[0] + m[1] * m[1] + m[2] * m[2]).sqrt();
        *n = if len > 0.0 { m.map(|v| v / len) } else { [0.0, 1.0, 0.0] };
    }
}

/// MeshParts stretch the mesh's bounding box to the part's size.
fn normalise_to_bounds(mesh: &mut DecodedMesh) {
    let (lo, hi) = bounds(&mesh.positions);
    let centre = [0, 1, 2].map(|a| (lo[a] + hi[a]) * 0.5);
    let size = [0, 1, 2].map(|a| hi[a] - lo[a]);
    normalise(mesh, centre, size);
}
