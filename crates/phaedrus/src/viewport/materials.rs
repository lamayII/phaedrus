//! Maps Roblox materials to shared PBR materials.

use std::collections::HashMap;

use bevy::prelude::*;
use phaedrus_dom::reflection;

/// Quantized material description; identical parts share one material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MaterialKey {
    pub rgb: [u8; 3],
    pub transparency: u8,
    pub reflectance: u8,
    pub material: u32,
}

impl MaterialKey {
    pub fn new(color: [f32; 3], transparency: f32, reflectance: f32, material: u32) -> Self {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        Self {
            rgb: [q(color[0]), q(color[1]), q(color[2])],
            transparency: q(transparency),
            reflectance: q(reflectance),
            material,
        }
    }
}

/// Quantized Decal/Texture material description.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct OverlayKey {
    image: AssetId<Image>,
    rgba: [u8; 4],
    /// UV repeat in hundredths.
    uv_scale: [i32; 2],
}

#[derive(Resource, Default)]
pub struct MaterialCache {
    map: HashMap<MaterialKey, Handle<StandardMaterial>>,
    overlays: HashMap<OverlayKey, Handle<StandardMaterial>>,
}

impl MaterialCache {
    pub fn get(&mut self, key: MaterialKey, assets: &mut Assets<StandardMaterial>) -> Handle<StandardMaterial> {
        self.map
            .entry(key)
            .or_insert_with(|| assets.add(build(key)))
            .clone()
    }

    /// Material for a Decal/Texture overlay.
    pub fn overlay(
        &mut self,
        image: &Handle<Image>,
        color: [f32; 3],
        transparency: f32,
        uv_scale: Vec2,
        assets: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        let key = OverlayKey {
            image: image.id(),
            rgba: [q(color[0]), q(color[1]), q(color[2]), q(1.0 - transparency)],
            uv_scale: [(uv_scale.x * 100.0).round() as i32, (uv_scale.y * 100.0).round() as i32],
        };
        self.overlays
            .entry(key)
            .or_insert_with(|| {
                assets.add(StandardMaterial {
                    base_color: Color::srgba_u8(key.rgba[0], key.rgba[1], key.rgba[2], key.rgba[3]),
                    base_color_texture: Some(image.clone()),
                    alpha_mode: AlphaMode::Blend,
                    perceptual_roughness: 0.8,
                    uv_transform: bevy::math::Affine2::from_scale(Vec2::new(key.uv_scale[0] as f32, key.uv_scale[1] as f32) / 100.0),
                    ..default()
                })
            })
            .clone()
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.overlays.clear();
    }
}

/// Per-material surface response, approximating Roblox's look.
struct Surface {
    roughness: f32,
    metallic: f32,
    emissive: f32,
    extra_transparency: f32,
}

fn surface(material: u32) -> Surface {
    let name = reflection::enum_item_name("Material", material).unwrap_or("Plastic");
    let (roughness, metallic, emissive, extra_transparency) = match name {
        "SmoothPlastic" => (0.35, 0.0, 0.0, 0.0),
        "Plastic" => (0.55, 0.0, 0.0, 0.0),
        "Neon" => (1.0, 0.0, 1.0, 0.0),
        "Glass" => (0.05, 0.0, 0.0, 0.0),
        "ForceField" => (1.0, 0.0, 0.6, 0.5),
        "Metal" | "DiamondPlate" => (0.35, 0.9, 0.0, 0.0),
        "Foil" => (0.15, 1.0, 0.0, 0.0),
        "CorrodedMetal" => (0.7, 0.6, 0.0, 0.0),
        "Ice" | "Glacier" => (0.1, 0.0, 0.0, 0.0),
        "Marble" => (0.25, 0.0, 0.0, 0.0),
        "Granite" | "Slate" | "Basalt" | "Rock" | "Concrete" | "Pavement" | "Asphalt" | "Brick"
        | "Cobblestone" | "Pebble" | "Limestone" | "Sandstone" | "CrackedLava" => (0.9, 0.0, 0.0, 0.0),
        "Grass" | "LeafyGrass" | "Sand" | "Fabric" | "Snow" | "Mud" | "Ground" | "Salt" => (1.0, 0.0, 0.0, 0.0),
        "Wood" | "WoodPlanks" => (0.75, 0.0, 0.0, 0.0),
        "Water" => (0.05, 0.0, 0.0, 0.3),
        _ => (0.6, 0.0, 0.0, 0.0),
    };
    Surface { roughness, metallic, emissive, extra_transparency }
}

fn build(key: MaterialKey) -> StandardMaterial {
    let s = surface(key.material);
    let transparency = (key.transparency as f32 / 255.0 + s.extra_transparency).min(1.0);
    let reflectance = key.reflectance as f32 / 255.0;
    let base = Color::srgba_u8(key.rgb[0], key.rgb[1], key.rgb[2], 255).with_alpha(1.0 - transparency);
    // Emissive is in the same photometric units as lights; at the default
    // exposure roughly 1000 nits reads as "white", so neon needs a few
    // thousand to bloom.
    let emissive = if s.emissive > 0.0 {
        let lin = LinearRgba::from(base);
        LinearRgba::rgb(lin.red, lin.green, lin.blue) * (2500.0 * s.emissive)
    } else {
        LinearRgba::BLACK
    };
    StandardMaterial {
        base_color: base,
        perceptual_roughness: (s.roughness * (1.0 - reflectance)).clamp(0.04, 1.0),
        metallic: s.metallic.max(reflectance * 0.8),
        reflectance: 0.3 + reflectance * 0.7,
        emissive,
        alpha_mode: if transparency > 0.001 { AlphaMode::Blend } else { AlphaMode::Opaque },
        ..default()
    }
}
