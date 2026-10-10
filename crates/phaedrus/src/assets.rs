//! Resolves Roblox content URLs to local images.
//!
//! Phaedrus is local-only, so `rbxassetid://` content cannot be downloaded.
//! Lookup order:
//! 1. `rbxasset://path` maps into a local Roblox Studio install's `content`
//!    folder, if one exists.
//! 2. `rbxassetid://ID` maps to `<asset cache>/ID.png|jpg|dds` where the asset
//!    cache is `%APPDATA%/phaedrus/assets` (users can drop files there).
//! 3. Otherwise callers fall back to procedural placeholders.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use bevy::asset::RenderAssetUsages;
use bevy::image::{CompressedImageFormats, ImageSampler, ImageType};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

#[derive(Resource)]
pub struct ContentProvider {
    roblox_content: Option<PathBuf>,
    roblox_platform_content: Option<PathBuf>,
    asset_cache: Option<PathBuf>,
    loaded: HashMap<String, Option<Handle<Image>>>,
    grid: Option<Handle<Image>>,
}

impl Default for ContentProvider {
    fn default() -> Self {
        let install = find_roblox_install();
        Self {
            roblox_content: install.as_ref().map(|p| p.join("content")),
            roblox_platform_content: install.as_ref().map(|p| p.join("PlatformContent").join("pc")),
            asset_cache: std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("phaedrus").join("assets")),
            loaded: HashMap::new(),
            grid: None,
        }
    }
}

/// Finds the newest Roblox install that has a `content` folder.
fn find_roblox_install() -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Roblox").join("Versions");
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(base)
        .ok()?
        .flatten()
        .filter(|e| e.path().join("content").is_dir())
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    candidates.sort();
    candidates.pop().map(|(_, p)| p)
}

impl ContentProvider {
    /// Resolves a content URL to a file path, if available locally.
    pub fn resolve(&self, url: &str) -> Option<PathBuf> {
        let url = url.trim();
        if let Some(rest) = url.strip_prefix("rbxasset://") {
            for root in [&self.roblox_content, &self.roblox_platform_content].into_iter().flatten() {
                let p = root.join(rest);
                if p.is_file() {
                    return Some(p);
                }
            }
            return None;
        }
        let id = url
            .strip_prefix("rbxassetid://")
            .or_else(|| url.split("id=").nth(1))
            .map(|s| s.trim_matches(|c: char| !c.is_ascii_digit()))?;
        let dir = self.asset_cache.as_ref()?;
        ["png", "jpg", "jpeg", "dds"]
            .iter()
            .map(|ext| dir.join(format!("{id}.{ext}")))
            .find(|p| p.is_file())
    }

    /// Path inside the Roblox install's content folder (e.g. `sky/sun.jpg`).
    pub fn roblox_file(&self, rel: &str) -> Option<PathBuf> {
        let p = self.roblox_content.as_ref()?.join(rel);
        p.is_file().then_some(p)
    }

    /// Loads an image for a content URL, caching the result (including
    /// misses so the file system is not hit every frame).
    pub fn image(&mut self, url: &str, images: &mut Assets<Image>) -> Option<Handle<Image>> {
        if url.is_empty() {
            return None;
        }
        if let Some(h) = self.loaded.get(url) {
            return h.clone();
        }
        let handle = self.resolve(url).and_then(|p| load_image_file(&p)).map(|img| images.add(img));
        self.loaded.insert(url.to_owned(), handle.clone());
        handle
    }

    /// A procedural stand-in for the default baseplate grid texture.
    pub fn grid_texture(&mut self, images: &mut Assets<Image>) -> Handle<Image> {
        self.grid.get_or_insert_with(|| images.add(make_grid_image())).clone()
    }
}

pub fn load_image_file(path: &Path) -> Option<Image> {
    let bytes = std::fs::read(path).ok()?;
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Image::from_buffer(
        &bytes,
        ImageType::Extension(&ext),
        CompressedImageFormats::all(),
        true,
        ImageSampler::linear(),
        RenderAssetUsages::RENDER_WORLD,
    )
    .map_err(|e| warn!("failed to decode {}: {e}", path.display()))
    .ok()
}

/// White grid lines on a transparent background: a thick border with 4x4
/// subdivisions, tinted by the Texture's `Color3`.
fn make_grid_image() -> Image {
    const N: usize = 256;
    let mut data = vec![0u8; N * N * 4];
    for y in 0..N {
        for x in 0..N {
            let edge = |v: usize| !(3..N - 3).contains(&v);
            let sub = |v: usize| v % (N / 4) < 1;
            let a = if edge(x) || edge(y) {
                255
            } else if sub(x) || sub(y) {
                110
            } else {
                0
            };
            let i = (y * N + x) * 4;
            data[i..i + 4].copy_from_slice(&[255, 255, 255, a]);
        }
    }
    let mut img = Image::new(
        Extent3d { width: N as u32, height: N as u32, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    with_mipmaps(&mut img);
    img.sampler = ImageSampler::Descriptor(bevy::image::ImageSamplerDescriptor {
        address_mode_u: bevy::image::ImageAddressMode::Repeat,
        address_mode_v: bevy::image::ImageAddressMode::Repeat,
        mag_filter: bevy::image::ImageFilterMode::Linear,
        min_filter: bevy::image::ImageFilterMode::Linear,
        mipmap_filter: bevy::image::ImageFilterMode::Linear,
        anisotropy_clamp: 16,
        ..default()
    });
    img
}

/// Appends a box-filtered mip chain to a square RGBA8 image. Large tiled
/// surfaces such as the baseplate shimmer badly without mips.
pub fn with_mipmaps(img: &mut Image) {
    let Some(base) = img.data.clone() else { return };
    let mut size = img.texture_descriptor.size.width as usize;
    if size != img.texture_descriptor.size.height as usize || !size.is_power_of_two() {
        return;
    }
    let mut all = base.clone();
    let mut prev = base;
    let mut levels = 1;
    while size > 1 {
        let half = size / 2;
        let mut next = vec![0u8; half * half * 4];
        for y in 0..half {
            for x in 0..half {
                for c in 0..4 {
                    let px = |xx: usize, yy: usize| prev[(yy * size + xx) * 4 + c] as u32;
                    let sum = px(2 * x, 2 * y) + px(2 * x + 1, 2 * y) + px(2 * x, 2 * y + 1) + px(2 * x + 1, 2 * y + 1);
                    next[(y * half + x) * 4 + c] = (sum / 4) as u8;
                }
            }
        }
        all.extend_from_slice(&next);
        prev = next;
        size = half;
        levels += 1;
    }
    img.data = Some(all);
    img.texture_descriptor.mip_level_count = levels;
}
