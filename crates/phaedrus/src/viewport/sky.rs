//! Skybox and celestial bodies.
//!
//! Sky textures referenced by `Sky` instances usually point at
//! `rbxassetid://` content we cannot download, so the sky is generated
//! procedurally from the sun position (day gradient, sunset glow, night with
//! stars). If all six faces resolve to local files, those are used instead.
//! The sun and moon are camera-following billboards.

use bevy::asset::RenderAssetUsages;
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
};

use super::lighting::LightingParams;
use crate::assets::{ContentProvider, load_image_file};

#[derive(Debug, Clone, PartialEq)]
pub struct SkyParams {
    pub celestial_bodies: bool,
    pub sun_size: f32,
    pub moon_size: f32,
    pub star_count: u32,
    /// Content URLs in bevy cubemap order: +X, -X, +Y, -Y, +Z, -Z.
    pub faces: [String; 6],
    pub sun_texture: String,
    pub moon_texture: String,
}

impl Default for SkyParams {
    fn default() -> Self {
        Self {
            celestial_bodies: true,
            sun_size: 21.0,
            moon_size: 11.0,
            star_count: 3000,
            faces: Default::default(),
            sun_texture: String::new(),
            moon_texture: String::new(),
        }
    }
}

#[derive(Resource, Default)]
pub struct SkyState {
    pub sun_dir: Vec3,
    pub celestial_bodies: bool,
    image: Option<Handle<Image>>,
    key: Option<(IVec3, u32, u32, [String; 6])>,
}

#[derive(Component)]
pub struct CelestialBody {
    pub is_moon: bool,
    pub angular_size_deg: f32,
}

pub fn spawn_celestial_bodies(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    content: Res<ContentProvider>,
) {
    let quad = meshes.add(Rectangle::new(1.0, 1.0));
    for is_moon in [false, true] {
        let file = if is_moon { "sky/moon.jpg" } else { "sky/sun.jpg" };
        let tex = content
            .roblox_file(file)
            .and_then(|p| load_image_file(&p))
            .unwrap_or_else(|| disc_image(is_moon));
        let material = materials.add(StandardMaterial {
            base_color: Color::WHITE,
            base_color_texture: Some(images.add(tex)),
            emissive: if is_moon { LinearRgba::rgb(1.5, 1.5, 1.6) } else { LinearRgba::rgb(30.0, 28.0, 24.0) },
            unlit: true,
            alpha_mode: AlphaMode::Add,
            fog_enabled: false,
            double_sided: true,
            cull_mode: None,
            ..default()
        });
        commands.spawn((
            CelestialBody { is_moon, angular_size_deg: if is_moon { 11.0 } else { 21.0 } },
            Mesh3d(quad.clone()),
            MeshMaterial3d(material),
            Transform::default(),
            NotShadowCaster,
            NotShadowReceiver,
        ));
    }
}

pub fn sync_body_sizes(sky: Res<SkyState>, params: Res<CurrentSkyParams>, mut bodies: Query<&mut CelestialBody>) {
    if !params.is_changed() && !sky.is_changed() {
        return;
    }
    for mut b in &mut bodies {
        b.angular_size_deg = if b.is_moon { params.0.moon_size } else { params.0.sun_size };
    }
}

/// The last sky parameters read from the document.
#[derive(Resource, Default)]
pub struct CurrentSkyParams(pub SkyParams);

/// Radial-gradient stand-in for the sun/moon textures.
fn disc_image(is_moon: bool) -> Image {
    const N: usize = 128;
    let mut data = vec![0u8; N * N * 4];
    for y in 0..N {
        for x in 0..N {
            let dx = (x as f32 + 0.5) / N as f32 * 2.0 - 1.0;
            let dy = (y as f32 + 0.5) / N as f32 * 2.0 - 1.0;
            let r = (dx * dx + dy * dy).sqrt();
            let v = if is_moon {
                if r < 0.45 { 1.0 } else { ((0.5 - r) / 0.05).clamp(0.0, 1.0) }
            } else {
                let core = if r < 0.12 { 1.0 } else { 0.0 };
                let glow = (1.0 - r).clamp(0.0, 1.0).powf(4.0) * 0.8;
                (core + glow).min(1.0)
            };
            let b = (v * 255.0) as u8;
            let i = (y * N + x) * 4;
            data[i..i + 4].copy_from_slice(&[b, b, b, 255]);
        }
    }
    Image::new(
        Extent3d { width: N as u32, height: N as u32, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    )
}

/// Regenerates the sky cubemap when the sun moves noticeably or the sky
/// textures change. Returns the current cubemap.
pub fn update_sky(
    state: &mut SkyState,
    params: &LightingParams,
    sun_dir: Vec3,
    resolution: u32,
    images: &mut Assets<Image>,
    content: &mut ContentProvider,
) -> Handle<Image> {
    state.sun_dir = sun_dir;
    state.celestial_bodies = params.sky.celestial_bodies;
    let key = (
        (sun_dir * 64.0).as_ivec3(),
        resolution,
        params.sky.star_count,
        params.sky.faces.clone(),
    );
    if let (Some(img), Some(k)) = (&state.image, &state.key)
        && *k == key {
            return img.clone();
        }
    let image = custom_cubemap(&params.sky.faces, content)
        .unwrap_or_else(|| procedural_cubemap(sun_dir, resolution, params.sky.star_count, params.atmosphere.map(|a| a.color)));
    let handle = match &state.image {
        Some(h) => {
            let _ = images.insert(h.id(), image);
            h.clone()
        }
        None => images.add(image),
    };
    state.image = Some(handle.clone());
    state.key = Some(key);
    handle
}

fn custom_cubemap(faces: &[String; 6], content: &ContentProvider) -> Option<Image> {
    if faces.iter().any(|f| f.is_empty()) {
        return None;
    }
    let imgs: Vec<Image> = faces
        .iter()
        .map(|f| content.resolve(f).and_then(|p| load_image_file(&p)))
        .collect::<Option<_>>()?;
    let size = imgs[0].texture_descriptor.size;
    let format = imgs[0].texture_descriptor.format;
    if imgs.iter().any(|i| i.texture_descriptor.size != size || i.texture_descriptor.format != format) {
        return None;
    }
    let mut data = Vec::new();
    for i in &imgs {
        data.extend_from_slice(i.data.as_ref()?);
    }
    Some(make_cube(size.width, format, data))
}

fn make_cube(res: u32, format: TextureFormat, data: Vec<u8>) -> Image {
    let mut image = Image::new(
        Extent3d { width: res, height: res, depth_or_array_layers: 6 },
        TextureDimension::D2,
        data,
        format,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::Cube),
        ..default()
    });
    image
}

/// Direction for a cubemap texel (bevy/wgpu face order and orientation).
fn texel_dir(face: usize, u: f32, v: f32) -> Vec3 {
    match face {
        0 => Vec3::new(1.0, -v, -u),
        1 => Vec3::new(-1.0, -v, u),
        2 => Vec3::new(u, 1.0, v),
        3 => Vec3::new(u, -1.0, -v),
        4 => Vec3::new(u, -v, 1.0),
        _ => Vec3::new(-u, -v, -1.0),
    }
    .normalize()
}

fn hash(p: IVec3) -> f32 {
    let mut h = (p.x as u32).wrapping_mul(0x8da6b343)
        ^ (p.y as u32).wrapping_mul(0xd8163841)
        ^ (p.z as u32).wrapping_mul(0xcb1ab31f);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1e995);
    h ^= h >> 15;
    (h & 0xffffff) as f32 / 16777215.0
}

fn procedural_cubemap(sun: Vec3, res: u32, star_count: u32, atmosphere: Option<[f32; 3]>) -> Image {
    let res = res.max(16);
    let day = (sun.y * 4.0 + 0.4).clamp(0.0, 1.0);
    let sunset = (1.0 - (sun.y * 5.0).abs()).clamp(0.0, 1.0);
    let zenith_day = Vec3::new(0.18, 0.42, 0.95);
    let horizon_day = Vec3::new(0.66, 0.80, 0.96);
    let zenith_night = Vec3::new(0.004, 0.008, 0.025);
    let horizon_night = Vec3::new(0.02, 0.03, 0.06);
    let sunset_col = Vec3::new(1.0, 0.45, 0.18);
    let haze = atmosphere.map(Vec3::from).unwrap_or(Vec3::ONE);
    let star_density = (star_count as f32 / 3000.0).clamp(0.0, 3.0) * 0.004;

    let mut data = Vec::with_capacity((res * res * 6 * 4) as usize);
    for face in 0..6 {
        for y in 0..res {
            for x in 0..res {
                let u = (x as f32 + 0.5) / res as f32 * 2.0 - 1.0;
                let v = (y as f32 + 0.5) / res as f32 * 2.0 - 1.0;
                let d = texel_dir(face, u, v);
                let h = d.y.max(0.0);
                let t = h.powf(0.45);
                let zenith = zenith_night.lerp(zenith_day, day);
                let horizon = horizon_night.lerp(horizon_day * haze.lerp(Vec3::ONE, 0.6), day);
                let mut c = horizon.lerp(zenith, t);
                // Sunset band toward the sun.
                let toward = d.dot(sun).max(0.0);
                let band = (1.0 - h).powf(6.0) * sunset * toward.powf(2.0);
                c = c.lerp(sunset_col, band * 0.85);
                // Sun glow.
                c += Vec3::new(1.0, 0.9, 0.7) * toward.powf(64.0) * 0.6 * day.max(sunset);
                // Ground below the horizon.
                if d.y < 0.0 {
                    let g = horizon * 0.55;
                    c = horizon.lerp(g, (-d.y * 6.0).clamp(0.0, 1.0));
                }
                // Stars at night.
                let night = 1.0 - day;
                if night > 0.05 && d.y > 0.02 {
                    // Round star dots: each grid cell may hold one star at a
                    // jittered centre.
                    let p = d * (res as f32 * 0.8);
                    let cell = p.floor();
                    let ci = cell.as_ivec3();
                    let n = hash(ci);
                    if n < star_density * 40.0 {
                        let jitter = Vec3::new(hash(ci + IVec3::X * 7), hash(ci + IVec3::Y * 13), hash(ci + IVec3::Z * 29));
                        let center = cell + Vec3::splat(0.2) + jitter * 0.6;
                        let dist = (p - center).length();
                        let b = (1.0 - dist / 0.45).clamp(0.0, 1.0) * night * (0.4 + 0.6 * hash(ci + IVec3::ONE * 3));
                        c += Vec3::splat(b);
                    }
                }
                let srgb = Color::linear_rgb(c.x, c.y, c.z).to_srgba();
                data.extend_from_slice(&[
                    (srgb.red.clamp(0.0, 1.0) * 255.0) as u8,
                    (srgb.green.clamp(0.0, 1.0) * 255.0) as u8,
                    (srgb.blue.clamp(0.0, 1.0) * 255.0) as u8,
                    255,
                ]);
            }
        }
    }
    make_cube(res, TextureFormat::Rgba8UnormSrgb, data)
}
