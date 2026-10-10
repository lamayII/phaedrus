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

/// A decoded union mesh, normalised to a unit cube like the primitive shape
/// meshes so the part's transform scale is simply its `Size`.
#[derive(Debug, Clone, Default)]
pub struct UnionMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// Per-vertex RGBA (0..1); empty when the format has none.
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
}

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
    ["Content", "AssetId"].into_iter().find_map(|name| {
        let url = match inst.properties.get(&name.into())? {
            Variant::Content(c) => c.as_uri()?,
            Variant::ContentId(c) => c.as_str(),
            Variant::String(s) => s.as_str(),
            _ => return None,
        };
        parse_asset_id(url)
    })
}

/// Parses `rbxassetid://N` and `...?id=N` content URLs.
pub fn parse_asset_id(url: &str) -> Option<u64> {
    let url = url.trim();
    let rest = url.strip_prefix("rbxassetid://").or_else(|| url.split("id=").nth(1))?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok().filter(|&id| id > 0)
}

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
    cache_dir.join(format!("{id}.meshdata"))
}

/// Decodes the union's mesh, reading `AssetId` meshes from `cache_dir`.
/// `Ok(None)` means the data is not available locally (yet).
pub fn load_union_mesh(inst: &Instance, cache_dir: Option<&Path>) -> anyhow::Result<Option<UnionMesh>> {
    let Some(source) = union_source(inst) else { return Ok(None) };
    let owned;
    let bytes = match source {
        UnionSource::Inline(b) => b,
        UnionSource::Asset(id) => {
            let Some(dir) = cache_dir else { return Ok(None) };
            match std::fs::read(asset_cache_file(dir, id)) {
                Ok(b) => {
                    let Some(b) = mesh_data_from_asset(b)? else { return Ok(None) };
                    owned = b;
                    &owned[..]
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e.into()),
            }
        }
    };
    let Some(mut mesh) = decode_union_graphics(bytes)? else { return Ok(None) };
    normalise(&mut mesh, initial_size(inst));
    if uses_part_color(inst) {
        mesh.colors.clear();
    }
    Ok(Some(mesh))
}

/// Union assets are delivered as a model holding one `PartOperationAsset`,
/// whose `MeshData` is the CSGMDL blob. Raw CSGMDL data passes through.
/// `Ok(None)` for assets with empty `MeshData` (unions with no geometry).
pub fn mesh_data_from_asset(bytes: Vec<u8>) -> anyhow::Result<Option<Vec<u8>>> {
    let dom = if bytes.starts_with(b"<roblox!") {
        rbx_binary::from_reader(&bytes[..])?
    } else if bytes.starts_with(b"<roblox") {
        rbx_xml::from_reader_default(&bytes[..])?
    } else {
        return Ok(Some(bytes));
    };
    Ok(dom.descendants().find_map(|i| inline_mesh_data(i).filter(|b| !b.is_empty()).map(<[u8]>::to_vec)))
}

/// Decodes a CSGMDL blob. `Ok(None)` for `CSGK` placeholders, which hold no
/// geometry.
pub fn decode_union_graphics(bytes: &[u8]) -> anyhow::Result<Option<UnionMesh>> {
    let mesh = match rbx_mesh::read_union_graphics_versioned(Cursor::new(bytes))? {
        UnionGraphics::CSGK(_) => return Ok(None),
        UnionGraphics::V2(m) => from_mesh2(m.mesh),
        UnionGraphics::V4(m) => from_mesh2(m.mesh),
        UnionGraphics::V5(m) => {
            let n = m.positions.len();
            let mut out = UnionMesh {
                normals: if m.normals.len() == n { m.normals.into_iter().map(|q| q.0).collect() } else { Vec::new() },
                colors: if m.colors.len() == n { m.colors.into_iter().map(rgba).collect() } else { Vec::new() },
                positions: m.positions,
                indices: m.faces.indices,
            };
            out.indices.truncate(out.indices.len() / 3 * 3);
            out
        }
    };
    if mesh.indices.iter().any(|&i| i as usize >= mesh.positions.len()) {
        anyhow::bail!("union mesh index out of range");
    }
    Ok(Some(mesh))
}

fn from_mesh2(m: rbx_mesh::union_graphics::Mesh2) -> UnionMesh {
    let mut out = UnionMesh::default();
    for v in m.vertices {
        out.positions.push(v.pos);
        out.normals.push(v.norm);
        out.colors.push(rgba(v.color));
    }
    out.indices = m.faces.into_iter().flat_map(|f| f.map(|v| v.0)).collect();
    out
}

fn rgba(c: [u8; 4]) -> [f32; 4] {
    c.map(|v| v as f32 / 255.0)
}

/// Scales positions from studs at `InitialSize` to a unit cube. Without an
/// `InitialSize`, the mesh bounds are used. Normals get the inverse-transpose
/// of that scale so they stay perpendicular to the squashed surface.
fn normalise(mesh: &mut UnionMesh, initial: Option<Vector3>) {
    let size = initial.map(|s| [s.x, s.y, s.z]).unwrap_or_else(|| bounds_size(&mesh.positions));
    let size = size.map(|s| if s.abs() > 1e-4 { s } else { 1e-4 });
    for p in &mut mesh.positions {
        for a in 0..3 {
            p[a] /= size[a];
        }
    }
    for n in &mut mesh.normals {
        let m = [n[0] * size[0], n[1] * size[1], n[2] * size[2]];
        let len = (m[0] * m[0] + m[1] * m[1] + m[2] * m[2]).sqrt();
        *n = if len > 0.0 { m.map(|v| v / len) } else { [0.0, 1.0, 0.0] };
    }
}

/// Extent of the positions along each axis.
pub fn bounds_size(positions: &[[f32; 3]]) -> [f32; 3] {
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    for p in positions {
        for a in 0..3 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    if positions.is_empty() {
        return [1.0; 3];
    }
    [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]]
}

/// Asset ids of union meshes in the document that are not in `cache_dir`.
pub fn missing_union_assets<'a>(instances: impl Iterator<Item = &'a Instance>, cache_dir: Option<&Path>) -> Vec<u64> {
    let mut ids: Vec<u64> = instances
        .filter(|i| is_union_class(&i.class))
        .filter_map(|i| match union_source(i)? {
            UnionSource::Asset(id) => Some(id),
            UnionSource::Inline(_) => None,
        })
        .filter(|&id| cache_dir.is_none_or(|d| !asset_cache_file(d, id).is_file()))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}
