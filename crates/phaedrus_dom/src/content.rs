//! Asset references in a document and where downloaded assets are cached.
//!
//! Cache layout (`%APPDATA%/phaedrus/assets`):
//! - `<id>.meshdata`: union mesh assets (see [`crate::union`]);
//! - `<id>.mesh`: MeshPart mesh files (see [`crate::mesh`]);
//! - `<id>.png|jpg|dds`: images, which is also where users can drop their
//!   own files.

use std::path::{Path, PathBuf};

use rbx_dom_weak::{Instance, WeakDom};
use rbx_types::Variant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AssetKind {
    UnionMesh,
    Mesh,
    Image,
}

impl AssetKind {
    pub fn label(self) -> &'static str {
        match self {
            AssetKind::UnionMesh => "union mesh",
            AssetKind::Mesh => "mesh",
            AssetKind::Image => "image",
        }
    }
}

/// Image extensions the renderer can load, in lookup order.
pub const IMAGE_EXTENSIONS: [&str; 4] = ["png", "jpg", "jpeg", "dds"];

/// The first content URL among `names` (canonical and legacy property
/// names, e.g. `TextureContent` then `Texture`).
pub fn content_url<'a>(inst: &'a Instance, names: &[&str]) -> Option<&'a str> {
    names.iter().find_map(|name| {
        let url = match inst.properties.get(&(*name).into())? {
            Variant::Content(c) => c.as_uri()?,
            Variant::ContentId(c) => c.as_str(),
            Variant::String(s) => s.as_str(),
            _ => return None,
        };
        (!url.trim().is_empty()).then_some(url)
    })
}

/// Parses `rbxassetid://N` and `...?id=N` content URLs.
pub fn parse_asset_id(url: &str) -> Option<u64> {
    let url = url.trim();
    let rest = url.strip_prefix("rbxassetid://").or_else(|| url.split("id=").nth(1))?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok().filter(|&id| id > 0)
}

pub fn content_asset_id(inst: &Instance, names: &[&str]) -> Option<u64> {
    parse_asset_id(content_url(inst, names)?)
}

/// Mesh asset properties of a MeshPart.
pub const MESH_PROPS: &[&str] = &["MeshContent", "MeshId"];
/// Texture properties of a MeshPart.
pub const MESH_TEXTURE_PROPS: &[&str] = &["TextureContent", "TextureID"];
/// Texture properties of a Decal or Texture.
pub const DECAL_PROPS: &[&str] = &["TextureContent", "Texture"];
/// Colour map of a SurfaceAppearance.
pub const COLOR_MAP_PROPS: &[&str] = &["ColorMapContent", "ColorMap"];

/// The assets an instance needs to render.
pub fn instance_assets(inst: &Instance) -> Vec<(u64, AssetKind)> {
    let mut out = Vec::new();
    let mut add = |names: &[&str], kind| {
        if let Some(id) = content_asset_id(inst, names) {
            out.push((id, kind));
        }
    };
    match inst.class.as_str() {
        c if crate::union::is_union_class(c) => {
            if let Some(crate::union::UnionSource::Asset(id)) = crate::union::union_source(inst) {
                out.push((id, AssetKind::UnionMesh));
            }
        }
        "MeshPart" => {
            add(MESH_PROPS, AssetKind::Mesh);
            add(MESH_TEXTURE_PROPS, AssetKind::Image);
        }
        "Decal" | "Texture" => add(DECAL_PROPS, AssetKind::Image),
        "SurfaceAppearance" => add(COLOR_MAP_PROPS, AssetKind::Image),
        _ => {}
    }
    out
}

/// The cache file for a mesh asset. Images have several possible
/// extensions; use [`cached_image`] for those.
pub fn cache_file(dir: &Path, id: u64, kind: AssetKind) -> PathBuf {
    match kind {
        AssetKind::UnionMesh => dir.join(format!("{id}.meshdata")),
        AssetKind::Mesh => dir.join(format!("{id}.mesh")),
        AssetKind::Image => dir.join(format!("{id}.png")),
    }
}

pub fn cached_image(dir: &Path, id: u64) -> Option<PathBuf> {
    IMAGE_EXTENSIONS.iter().map(|ext| dir.join(format!("{id}.{ext}"))).find(|p| p.is_file())
}

pub fn is_cached(dir: &Path, id: u64, kind: AssetKind) -> bool {
    match kind {
        AssetKind::Image => cached_image(dir, id).is_some(),
        _ => cache_file(dir, id, kind).is_file(),
    }
}

/// Assets referenced under the document root that are not in `cache_dir`,
/// sorted and deduplicated.
pub fn missing_assets<'a>(instances: impl Iterator<Item = &'a Instance>, cache_dir: Option<&Path>) -> Vec<(u64, AssetKind)> {
    let mut assets: Vec<(u64, AssetKind)> = instances.flat_map(instance_assets).collect();
    assets.sort_unstable();
    assets.dedup();
    assets.retain(|&(id, kind)| cache_dir.is_none_or(|d| !is_cached(d, id, kind)));
    assets
}

/// What a downloaded image asset turned out to be.
#[derive(Debug, PartialEq, Eq)]
pub enum ImageAsset {
    /// Image bytes and the file extension to cache them under.
    Image(&'static str),
    /// A Decal asset: a model whose Decal points at the actual image.
    Redirect(u64),
    Unsupported,
}

/// Sniffs a downloaded image asset.
pub fn classify_image(bytes: &[u8]) -> ImageAsset {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        ImageAsset::Image("png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        ImageAsset::Image("jpg")
    } else if bytes.starts_with(b"DDS ") {
        ImageAsset::Image("dds")
    } else if let Some(dom) = read_model(bytes) {
        dom.descendants()
            .filter(|i| matches!(i.class.as_str(), "Decal" | "Texture"))
            .find_map(|i| content_asset_id(i, DECAL_PROPS))
            .map_or(ImageAsset::Unsupported, ImageAsset::Redirect)
    } else {
        ImageAsset::Unsupported
    }
}

/// Parses a binary or XML model, if `bytes` is one.
pub fn read_model(bytes: &[u8]) -> Option<WeakDom> {
    if bytes.starts_with(b"<roblox!") {
        rbx_binary::from_reader(bytes).ok()
    } else if bytes.starts_with(b"<roblox") {
        rbx_xml::from_reader_default(bytes).ok()
    } else {
        None
    }
}
