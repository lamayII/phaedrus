//! Decodes UnionOperation render meshes with `rbx_mesh`.
//!
//! A union's triangles live in one of two places:
//! - inline in `MeshData2` (SharedString) or the legacy `MeshData`
//!   (BinaryString), as a CSGMDL v2/v4/v5 blob;
//! - behind `Content`/`AssetId` (an asset URL), in which case the blob has to be
//!   downloaded into the asset cache first (see the editor's asset loader).
//!   Inline data that is only a `CSGK` header also means "look at AssetId".

use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Cursor;
use std::path::Path;

use rbx_dom_weak::Instance;
use rbx_mesh::union_graphics::UnionGraphics;
use rbx_types::{Variant, Vector3};

/// CSG classes that carry union mesh data.
pub fn is_union_class(class: &str) -> bool {
    matches!(class, "UnionOperation" | "NegateOperation" | "IntersectOperation")
}

pub use crate::mesh::{DecodedMesh, bounds_size};

/// Where a union's geometry comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnionSource<'a> {
    Inline(&'a [u8]),
    Asset(u64),
}

/// The union's mesh source, or `None` for unions with no geometry at all.
pub fn union_source(inst: &Instance) -> Option<UnionSource<'_>> {
    let inline = inline_mesh_data(inst).filter(|b| !b.is_empty() && !b.starts_with(b"CSGK"));
    if let Some(b) = inline {
        return Some(UnionSource::Inline(b));
    }
    asset_id(inst).map(UnionSource::Asset)
}

fn inline_mesh_data(inst: &Instance) -> Option<&[u8]> {
    let props = &inst.properties;
    match props.get(&"MeshData2".into()) {
        Some(Variant::SharedString(s)) if !s.data().is_empty() => return Some(s.data()),
        _ => {}
    }
    match props.get(&"MeshData".into()) {
        Some(Variant::BinaryString(b)) => Some(b.as_ref()),
        Some(Variant::SharedString(s)) => Some(s.data()),
        _ => None,
    }
}

/// The numeric asset id of the union's mesh asset. Newer files store it in
/// `Content` (as `https://www.roblox.com/asset/?id=N`), older ones in
/// `AssetId`.
pub fn asset_id(inst: &Instance) -> Option<u64> {
    crate::content::content_asset_id(inst, &["Content", "AssetId"])
}

pub use crate::content::parse_asset_id;

/// `UsePartColor`: when false the union shows the colours of the parts it
/// was made from (stored per vertex).
pub fn uses_part_color(inst: &Instance) -> bool {
    !matches!(inst.properties.get(&"UsePartColor".into()), Some(Variant::Bool(false)))
}

pub fn initial_size(inst: &Instance) -> Option<Vector3> {
    match inst.properties.get(&"InitialSize".into())? {
        Variant::Vector3(v) => Some(*v),
        _ => None,
    }
}

/// A key identifying the decoded (and normalised) mesh: unions that share
/// mesh data and `InitialSize`, e.g. copies of one union, share one mesh.
/// `None` when the union has no geometry.
pub fn union_mesh_key(inst: &Instance) -> Option<u64> {
    let mut h = DefaultHasher::new();
    match union_source(inst)? {
        UnionSource::Inline(b) => match inst.properties.get(&"MeshData2".into()) {
            // SharedStrings carry a precomputed content hash.
            Some(Variant::SharedString(s)) if std::ptr::eq(s.data(), b) => s.hash().as_bytes().hash(&mut h),
            _ => b.hash(&mut h),
        },
        UnionSource::Asset(id) => {
            0xA55E7u32.hash(&mut h);
            id.hash(&mut h);
        }
    }
    if let Some(s) = initial_size(inst) {
        [s.x, s.y, s.z].map(f32::to_bits).hash(&mut h);
    }
    uses_part_color(inst).hash(&mut h);
    Some(h.finish())
}

/// The cache file for a downloaded union mesh asset.
pub fn asset_cache_file(cache_dir: &Path, id: u64) -> std::path::PathBuf {
    crate::content::cache_file(cache_dir, id, crate::content::AssetKind::UnionMesh)
}

/// Decodes the union's mesh, reading `AssetId` meshes from `cache_dir`.
/// `Ok(None)` means the data is not available locally (yet).
pub fn load_union_mesh(inst: &Instance, cache_dir: Option<&Path>) -> anyhow::Result<Option<DecodedMesh>> {
    let Some(source) = union_source(inst) else { return Ok(None) };
    let mesh = match source {
        UnionSource::Inline(b) => decode_union_graphics(b)?,
        UnionSource::Asset(id) => {
            let Some(dir) = cache_dir else { return Ok(None) };
            match std::fs::read(asset_cache_file(dir, id)) {
                Ok(b) => decode_union_asset(b)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e.into()),
            }
        }
    };
    Ok(mesh.map(|m| finish_union_mesh(inst, m)))
}

/// Decodes a downloaded union mesh asset, in studs at the union's
/// `InitialSize`. Needs no instance, so it can run on any thread; see
/// [`finish_union_mesh`].
pub fn decode_union_asset(bytes: Vec<u8>) -> anyhow::Result<Option<DecodedMesh>> {
    match mesh_data_from_asset(bytes)? {
        Some(b) => decode_union_graphics(&b),
        None => Ok(None),
    }
}

/// Fits a decoded union mesh to `inst`: unit-cube scale and its colour mode.
pub fn finish_union_mesh(inst: &Instance, mut mesh: DecodedMesh) -> DecodedMesh {
    normalise(&mut mesh, initial_size(inst));
    if uses_part_color(inst) {
        mesh.colors.clear();
    }
    mesh
}

/// Union assets are delivered as a model holding one `PartOperationAsset`,
/// whose `MeshData` is the CSGMDL blob. Raw CSGMDL data passes through.
/// `Ok(None)` for assets with empty `MeshData` (unions with no geometry).
pub fn mesh_data_from_asset(bytes: Vec<u8>) -> anyhow::Result<Option<Vec<u8>>> {
    if !bytes.starts_with(b"<roblox") {
        return Ok(Some(bytes));
    }
    let dom = crate::content::read_model(&bytes).ok_or_else(|| anyhow::anyhow!("union asset is not a readable model"))?;
    Ok(dom.descendants().find_map(|i| inline_mesh_data(i).filter(|b| !b.is_empty()).map(<[u8]>::to_vec)))
}

/// Decodes a CSGMDL blob. `Ok(None)` for `CSGK` placeholders, which hold no
/// geometry.
pub fn decode_union_graphics(bytes: &[u8]) -> anyhow::Result<Option<DecodedMesh>> {
    let mesh = match rbx_mesh::read_union_graphics_versioned(Cursor::new(bytes))? {
        UnionGraphics::CSGK(_) => return Ok(None),
        UnionGraphics::V2(m) => from_mesh2(m.mesh),
        UnionGraphics::V4(m) => from_mesh2(m.mesh),
        UnionGraphics::V5(m) => {
            let n = m.positions.len();
            let mut out = DecodedMesh {
                normals: if m.normals.len() == n { m.normals.into_iter().map(|q| q.0).collect() } else { Vec::new() },
                colors: if m.colors.len() == n { m.colors.into_iter().map(rgba).collect() } else { Vec::new() },
                uvs: if m.tex.len() == n { m.tex } else { Vec::new() },
                positions: m.positions,
                indices: m.faces.indices,
            };
            out.indices.truncate(out.indices.len() / 3 * 3);
            out
        }
    };
    mesh.validate()?;
    Ok(Some(mesh))
}

fn from_mesh2(m: rbx_mesh::union_graphics::Mesh2) -> DecodedMesh {
    let mut out = DecodedMesh::default();
    for v in m.vertices {
        out.positions.push(v.pos);
        out.normals.push(v.norm);
        out.uvs.push(v.tex);
        out.colors.push(rgba(v.color));
    }
    out.indices = m.faces.into_iter().flat_map(|f| f.map(|v| v.0)).collect();
    out
}

fn rgba(c: [u8; 4]) -> [f32; 4] {
    c.map(|v| v as f32 / 255.0)
}

/// Scales positions from studs at `InitialSize` to a unit cube. Without an
/// `InitialSize`, the mesh bounds are used. Union meshes are already centred
/// on the part.
fn normalise(mesh: &mut DecodedMesh, initial: Option<Vector3>) {
    let size = initial.map(|s| [s.x, s.y, s.z]).unwrap_or_else(|| bounds_size(&mesh.positions));
    crate::mesh::normalise(mesh, [0.0; 3], size);
}
