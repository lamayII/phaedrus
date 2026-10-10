//! Lighting service -> bevy lights, fog, post effects, sky and graphics
//! quality. Mirrors Roblox's lighting pipeline:
//!
//! - `Lighting.Technology` picks the shadow model: Voxel/Compatibility use
//!   soft low-resolution sun shadows only; ShadowMap adds sharp sun shadows;
//!   Future (Future Is Bright) adds local light shadows, SSAO and specular
//!   environment lighting.
//! - The graphics quality level (1-10) scales shadow resolution, cascade
//!   count, anti-aliasing, screen-space effects and how many local lights
//!   render, like Studio's "Rendering > Quality Level".
//! - The draw distance level (1-10) hides parts beyond it (see `culling`)
//!   and caps shadow and local light distance.
//!
//! Places can have tens of thousands of parts, so the expensive passes are
//! budgeted: every shadow cascade and every shadowed point light (six cube
//! faces) re-renders the parts it covers. Cascades are few and tightly fit,
//! small parts do not cast sun shadows, only the nearest few local lights
//! cast shadows and distant local lights are switched off.

use bevy::anti_alias::fxaa::Fxaa;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::light::{
    CascadeShadowConfigBuilder, DirectionalLightShadowMap, EnvironmentMapLight, GeneratedEnvironmentMapLight, GlobalAmbientLight,
    PointLightShadowMap, ShadowFilteringMethod, Skybox,
};
use bevy::pbr::{DistanceFog, FogFalloff, ScreenSpaceAmbientOcclusion, ScreenSpaceAmbientOcclusionQualityLevel};
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use bevy::render::view::Msaa;
use phaedrus_dom::types::{Ref, Variant};
use phaedrus_dom::{Document, reflection};

use super::camera::EditorCamera;
use super::sky::{self, CelestialBody, SkyState};
use crate::assets::ContentProvider;
use crate::studio::Studio;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Technology {
    Voxel,
    ShadowMap,
    Future,
}

impl Technology {
    pub fn label(self) -> &'static str {
        match self {
            Technology::Voxel => "Voxel",
            Technology::ShadowMap => "ShadowMap",
            Technology::Future => "Future",
        }
    }
}

/// User-facing render settings (File > Phaedrus Settings > Graphics).
#[derive(Resource, Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct GraphicsSettings {
    /// 1 (fastest) ..= 10 (best).
    pub quality: u8,
    /// Overrides `Lighting.Technology` when set.
    pub technology_override: Option<Technology>,
    pub show_fps: bool,
    /// Max frames per second while the window is unfocused (saves power).
    pub background_fps: u32,
    /// How far parts render: 1 (250 studs) ..= 10 (unlimited).
    pub draw_distance: u8,
}

/// Draw distance at level 10, which does not cull.
pub const MAX_DRAW_DISTANCE: f32 = f32::INFINITY;

/// Studs visible at a draw distance level. Steps grow geometrically so each
/// level is a similar visual change.
pub fn draw_distance_studs(level: u8) -> f32 {
    const STUDS: [f32; 10] = [250.0, 400.0, 600.0, 900.0, 1300.0, 1800.0, 2500.0, 3500.0, 5000.0, MAX_DRAW_DISTANCE];
    STUDS[level.clamp(1, 10) as usize - 1]
}

impl Default for GraphicsSettings {
    fn default() -> Self {
        Self { quality: 7, technology_override: None, show_fps: false, background_fps: 10, draw_distance: 10 }
    }
}

/// Settings derived from the quality level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QualityProfile {
    pub shadows: bool,
    pub shadow_map_size: usize,
    pub cascades: usize,
    pub shadow_distance: f32,
    /// Parts whose largest dimension is below this many studs cast no sun
    /// shadow; at these shadow resolutions they would be a few texels.
    pub min_caster_size: f32,
    pub shadow_filter: ShadowFilteringMethod,
    pub msaa: Msaa,
    pub ssao: Option<ScreenSpaceAmbientOcclusionQualityLevel>,
    pub bloom: bool,
    pub env_map: bool,
    pub local_shadows: bool,
    /// Most local lights rendered at once (nearest to the camera first).
    pub max_lights: usize,
    /// Local lights farther than this from the camera are switched off.
    pub light_distance: f32,
    /// Most local lights casting shadows at once (nearest first).
    pub max_shadow_lights: usize,
    pub sky_resolution: u32,
}

pub fn profile(quality: u8, tech: Technology) -> QualityProfile {
    let q = quality.clamp(1, 10);
    let mut p = QualityProfile {
        shadows: q >= 2,
        // Cascades split the shadow distance, so a few smaller maps keep
        // texel density near the camera while costing far less than one
        // 4096 map per cascade did. Gaussian filtering hides the lower
        // resolution as soft edges, close to Roblox's own shadows.
        shadow_map_size: match q {
            0..=5 => 1024,
            _ => 2048,
        },
        cascades: match q {
            0..=3 => 1,
            4..=6 => 2,
            _ => 3,
        },
        shadow_distance: 60.0 + q as f32 * 30.0,
        min_caster_size: match q {
            0..=3 => 2.0,
            4..=6 => 1.0,
            _ => 0.5,
        },
        shadow_filter: if q <= 3 { ShadowFilteringMethod::Hardware2x2 } else { ShadowFilteringMethod::Gaussian },
        msaa: match q {
            0..=2 => Msaa::Off,
            3..=5 => Msaa::Sample2,
            _ => Msaa::Sample4,
        },
        ssao: match q {
            0..=6 => None,
            7..=9 => Some(ScreenSpaceAmbientOcclusionQualityLevel::Medium),
            _ => Some(ScreenSpaceAmbientOcclusionQualityLevel::High),
        },
        bloom: q >= 4,
        env_map: q >= 5,
        local_shadows: q >= 6,
        max_lights: match q {
            0..=3 => 16,
            4..=6 => 48,
            _ => 128,
        },
        light_distance: 100.0 + q as f32 * 40.0,
        max_shadow_lights: match q {
            0..=7 => 2,
            8..=9 => 4,
            _ => 6,
        },
        sky_resolution: if q >= 6 { 128 } else { 64 },
    };
    match tech {
        Technology::Voxel => {
            // Voxel lighting has soft, coarse sun shadows and no local
            // light shadows or SSAO.
            p.shadow_map_size = p.shadow_map_size.min(1024);
            p.cascades = p.cascades.min(2);
            p.ssao = None;
            p.local_shadows = false;
            p.env_map = false;
            p.shadow_filter = ShadowFilteringMethod::Gaussian;
        }
        Technology::ShadowMap => {
            p.local_shadows = false;
            p.ssao = None;
        }
        Technology::Future => {}
    }
    p
}

/// Lighting values read from the Lighting service and its effects.
#[derive(Debug, Clone, PartialEq)]
pub struct LightingParams {
    pub technology: Technology,
    pub clock_time: f32,
    pub latitude: f32,
    pub brightness: f32,
    pub ambient: [f32; 3],
    pub outdoor_ambient: [f32; 3],
    pub color_shift_top: [f32; 3],
    pub global_shadows: bool,
    pub shadow_softness: f32,
    pub exposure: f32,
    pub fog_color: [f32; 3],
    pub fog_start: f32,
    pub fog_end: f32,
    pub env_diffuse: f32,
    pub env_specular: f32,
    pub atmosphere: Option<AtmosphereParams>,
    pub bloom: Option<(f32, f32, f32)>,
    pub sky: sky::SkyParams,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AtmosphereParams {
    pub density: f32,
    pub color: [f32; 3],
    pub decay: [f32; 3],
    pub haze: f32,
}

fn f32_of(doc: &Document, r: Ref, name: &str, default: f32) -> f32 {
    match doc.get_property(r, name) {
        Some(Variant::Float32(v)) => v,
        Some(Variant::Float64(v)) => v as f32,
        Some(Variant::Int32(v)) => v as f32,
        _ => default,
    }
}

fn bool_of(doc: &Document, r: Ref, name: &str, default: bool) -> bool {
    match doc.get_property(r, name) {
        Some(Variant::Bool(b)) => b,
        _ => default,
    }
}

/// Parses "HH:MM:SS" into hours.
fn parse_time_of_day(s: &str) -> Option<f32> {
    let mut parts = s.split(':').map(|p| p.trim().parse::<f32>().ok());
    let h = parts.next()??;
    let m = parts.next().flatten().unwrap_or(0.0);
    let sec = parts.next().flatten().unwrap_or(0.0);
    Some((h + m / 60.0 + sec / 3600.0).rem_euclid(24.0))
}

pub fn read_lighting(doc: &Document) -> LightingParams {
    let Some(l) = doc.service("Lighting") else {
        return LightingParams {
            technology: Technology::ShadowMap,
            clock_time: 14.0,
            latitude: 23.5,
            brightness: 2.0,
            ambient: [0.27; 3],
            outdoor_ambient: [0.5; 3],
            color_shift_top: [0.0; 3],
            global_shadows: true,
            shadow_softness: 0.2,
            exposure: 0.0,
            fog_color: [0.75; 3],
            fog_start: 0.0,
            fog_end: 100000.0,
            env_diffuse: 1.0,
            env_specular: 1.0,
            atmosphere: None,
            bloom: None,
            sky: sky::SkyParams::default(),
        };
    };
    let tech_name = match doc.get_property(l, "Technology") {
        Some(Variant::Enum(e)) => reflection::enum_item_name("Technology", e.to_u32()).unwrap_or("ShadowMap"),
        _ => "ShadowMap",
    };
    let technology = match tech_name {
        "Future" | "Unified" => Technology::Future,
        "ShadowMap" => Technology::ShadowMap,
        _ => Technology::Voxel,
    };
    let clock_time = match doc.get_property(l, "TimeOfDay") {
        Some(Variant::String(s)) => parse_time_of_day(&s),
        _ => None,
    }
    .unwrap_or_else(|| f32_of(doc, l, "ClockTime", 14.0));

    let mut atmosphere = None;
    let mut bloom = None;
    let mut sky_inst = None;
    for &c in doc.children(l) {
        let Some(inst) = doc.get(c) else { continue };
        match inst.class.as_str() {
            "Atmosphere" => {
                atmosphere = Some(AtmosphereParams {
                    density: f32_of(doc, c, "Density", 0.3),
                    color: doc.color_of(c, "Color").unwrap_or([0.78, 0.67, 0.42]),
                    decay: doc.color_of(c, "Decay").unwrap_or([0.36, 0.24, 0.0]),
                    haze: f32_of(doc, c, "Haze", 0.0),
                });
            }
            "BloomEffect" if bool_of(doc, c, "Enabled", true) => {
                bloom = Some((
                    f32_of(doc, c, "Intensity", 1.0),
                    f32_of(doc, c, "Size", 24.0),
                    f32_of(doc, c, "Threshold", 2.0),
                ));
            }
            "Sky" => sky_inst = Some(c),
            _ => {}
        }
    }
    let mut sky = sky::SkyParams::default();
    if let Some(s) = sky_inst {
        sky.celestial_bodies = bool_of(doc, s, "CelestialBodiesShown", true);
        sky.sun_size = f32_of(doc, s, "SunAngularSize", 21.0);
        sky.moon_size = f32_of(doc, s, "MoonAngularSize", 11.0);
        sky.star_count = f32_of(doc, s, "StarCount", 3000.0) as u32;
        let content = |name: &str| match doc.get_property(s, name) {
            Some(Variant::Content(c)) => match c.value() {
                phaedrus_dom::types::ContentType::Uri(u) => u.clone(),
                _ => String::new(),
            },
            Some(Variant::ContentId(c)) => c.as_str().to_owned(),
            _ => String::new(),
        };
        let pick = |a: &str, b: &str| {
            let v = content(a);
            if v.is_empty() { content(b) } else { v }
        };
        sky.faces = [
            pick("SkyboxRightContent", "SkyboxRt"),
            pick("SkyboxLeftContent", "SkyboxLf"),
            pick("SkyboxUpContent", "SkyboxUp"),
            pick("SkyboxDownContent", "SkyboxDn"),
            pick("SkyboxBackContent", "SkyboxBk"),
            pick("SkyboxFrontContent", "SkyboxFt"),
        ];
        sky.sun_texture = pick("SunTextureContent", "SunTextureId");
        sky.moon_texture = pick("MoonTextureContent", "MoonTextureId");
    } else {
        sky.celestial_bodies = true;
    }
    LightingParams {
        technology,
        clock_time,
        latitude: f32_of(doc, l, "GeographicLatitude", 0.0),
        brightness: f32_of(doc, l, "Brightness", 2.0),
        ambient: doc.color_of(l, "Ambient").unwrap_or([0.27; 3]),
        outdoor_ambient: doc.color_of(l, "OutdoorAmbient").unwrap_or([0.5; 3]),
        color_shift_top: doc.color_of(l, "ColorShift_Top").unwrap_or([0.0; 3]),
        global_shadows: bool_of(doc, l, "GlobalShadows", true),
        shadow_softness: f32_of(doc, l, "ShadowSoftness", 0.2),
        exposure: f32_of(doc, l, "ExposureCompensation", 0.0),
        fog_color: doc.color_of(l, "FogColor").unwrap_or([0.75; 3]),
        fog_start: f32_of(doc, l, "FogStart", 0.0),
        fog_end: f32_of(doc, l, "FogEnd", 100000.0),
        env_diffuse: f32_of(doc, l, "EnvironmentDiffuseScale", 0.0),
        env_specular: f32_of(doc, l, "EnvironmentSpecularScale", 0.0),
        atmosphere,
        bloom,
        sky,
    }
}

/// Direction toward the sun for a clock time and latitude. At noon the sun is
/// high in the sky (tilted south by the latitude); it rises in the east.
pub fn sun_direction(clock_time: f32, latitude: f32) -> Vec3 {
    let hour_angle = (clock_time - 12.0) / 24.0 * std::f32::consts::TAU;
    let tilt = (latitude + 23.5).to_radians() * 0.5;
    let v = Vec3::new(-hour_angle.sin(), hour_angle.cos(), 0.0);
    // Tilt the orbit plane around X so the noon sun leans toward +Z.
    Quat::from_rotation_x(tilt) * v
}

#[derive(Component)]
pub struct Sun;

/// A bevy light spawned for a Light instance.
#[derive(Component)]
pub struct LocalLight {
    /// The instance's `Shadows` property (and the quality allows it).
    wants_shadows: bool,
}

/// Applied state, to avoid touching components every frame.
#[derive(Resource, Default)]
pub struct AppliedLighting {
    params: Option<LightingParams>,
    settings: Option<GraphicsSettings>,
    generation: u64,
}

pub fn spawn_sun(mut commands: Commands) {
    commands.spawn((
        Sun,
        DirectionalLight { shadow_maps_enabled: true, ..default() },
        Transform::default(),
    ));
}

#[allow(clippy::too_many_arguments)]
pub fn apply_lighting(
    mut commands: Commands,
    studio: Res<Studio>,
    settings: Res<GraphicsSettings>,
    mut applied: ResMut<AppliedLighting>,
    mut sky_state: ResMut<SkyState>,
    mut images: ResMut<Assets<Image>>,
    mut content: ResMut<ContentProvider>,
    mut shadow_map: ResMut<DirectionalLightShadowMap>,
    mut point_shadow_map: ResMut<PointLightShadowMap>,
    mut ambient: ResMut<GlobalAmbientLight>,
    mut sun: Query<(Entity, &mut DirectionalLight, &mut Transform), With<Sun>>,
    camera: Query<Entity, With<EditorCamera>>,
) {
    let lighting_changed = applied.generation != studio.generation
        || studio.frame_changes.iter().any(|c| match c {
            phaedrus_dom::Change::Property(r, _) | phaedrus_dom::Change::Added(r) => {
                let doc = studio.doc();
                doc.service("Lighting").is_some_and(|l| doc.is_descendant_of(*r, l))
            }
            phaedrus_dom::Change::Reparented { old, new, .. } => {
                let doc = studio.doc();
                doc.service("Lighting").is_some_and(|l| *old == l || *new == l)
            }
        });
    if !lighting_changed && applied.params.is_some() && applied.settings.as_ref() == Some(&settings) {
        return;
    }
    let params = read_lighting(&studio.doc());
    applied.generation = studio.generation;
    let tech = settings.technology_override.unwrap_or(params.technology);
    let prof = profile(settings.quality, tech);

    // Sun by day, moon by night (Roblox swaps the light source).
    let sun_dir = sun_direction(params.clock_time, params.latitude);
    let smooth = |e0: f32, e1: f32, x: f32| {
        let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let day = smooth(-0.04, 0.2, sun_dir.y);
    let moon_dir = -sun_dir;
    let moon_up = smooth(-0.02, 0.15, moon_dir.y) * (1.0 - day);
    if let Ok((e, mut light, mut t)) = sun.single_mut() {
        let tint = Vec3::from(params.color_shift_top) * 0.5 + Vec3::ONE;
        let (dir, color, illuminance) = if day > 0.0 || moon_up <= 0.0 {
            let warm = 1.0 - (sun_dir.y * 3.0).clamp(0.0, 1.0);
            let c = Vec3::new(1.0, 1.0 - 0.25 * warm, 1.0 - 0.5 * warm) * tint;
            (sun_dir, c, params.brightness * 4000.0 * day)
        } else {
            (moon_dir, Vec3::new(0.62, 0.72, 1.0), params.brightness * 900.0 * moon_up)
        };
        light.color = Color::linear_rgb(color.x, color.y, color.z);
        light.illuminance = illuminance;
        light.shadow_maps_enabled = prof.shadows && params.global_shadows && illuminance > 0.0;
        light.shadow_depth_bias = 0.04;
        light.shadow_normal_bias = 1.2;
        *t = Transform::IDENTITY.looking_to(-dir, Vec3::Y);
        // Nothing past the draw distance renders, so it needs no shadow.
        let shadow_distance = prof.shadow_distance.min(draw_distance_studs(settings.draw_distance));
        commands.entity(e).insert(
            CascadeShadowConfigBuilder {
                num_cascades: prof.cascades,
                minimum_distance: 0.1,
                // Tight first cascade: most visible shadow detail is close.
                first_cascade_far_bound: (shadow_distance / (2.0 * prof.cascades as f32)).max(12.0),
                maximum_distance: shadow_distance,
                overlap_proportion: 0.15,
            }
            .build(),
        );
    }
    shadow_map.size = prof.shadow_map_size;
    point_shadow_map.size = (prof.shadow_map_size / 2).max(256);

    // Ambient: Roblox's OutdoorAmbient lights surfaces that see the sky,
    // Ambient lights everything. Approximate with one global term.
    let amb = Vec3::from(params.outdoor_ambient) * 0.6 + Vec3::from(params.ambient) * 0.4;
    ambient.color = Color::srgb(amb.x, amb.y, amb.z);
    ambient.brightness = 900.0 * (0.55 + 0.45 * day);

    // Sky.
    let sky_image = sky::update_sky(&mut sky_state, &params, sun_dir, prof.sky_resolution, &mut images, &mut content);

    if let Ok(cam) = camera.single() {
        let mut ec = commands.entity(cam);
        ec.insert((
            prof.msaa,
            Tonemapping::AcesFitted,
            bevy::camera::Exposure { ev100: 9.7 - params.exposure },
            prof.shadow_filter,
            Skybox { image: Some(sky_image.clone()), brightness: 1500.0 * (0.35 + 0.65 * day), ..default() },
        ));
        if prof.env_map {
            // Dropping the old filtered maps makes bevy allocate new ones
            // at the new intensity; `freeze_environment_map` stops the
            // per-frame refiltering once they are filled.
            ec.remove::<EnvironmentMapLight>().insert((
                GeneratedEnvironmentMapLight {
                    environment_map: sky_image,
                    intensity: 600.0 * (0.35 + 0.65 * day) * params.env_diffuse.max(params.env_specular).max(0.25),
                    ..default()
                },
                EnvMapBaking(0),
            ));
        } else {
            ec.remove::<(GeneratedEnvironmentMapLight, EnvironmentMapLight, EnvMapBaking)>();
        }
        match prof.ssao {
            Some(level) => {
                // SSAO needs MSAA off; use FXAA instead.
                ec.insert((ScreenSpaceAmbientOcclusion { quality_level: level, ..default() }, Msaa::Off, Fxaa::default()));
            }
            None => {
                ec.remove::<(ScreenSpaceAmbientOcclusion, Fxaa)>();
            }
        }
        let bloom_cfg = params.bloom.filter(|_| prof.bloom);
        match bloom_cfg {
            Some((intensity, size, _threshold)) => {
                ec.insert(Bloom {
                    intensity: (intensity * 0.12).clamp(0.0, 1.0),
                    low_frequency_boost: (size / 56.0).clamp(0.0, 1.0),
                    ..Bloom::NATURAL
                });
            }
            None => {
                ec.remove::<Bloom>();
            }
        }
        // Fog: explicit Lighting fog wins; otherwise Atmosphere haze.
        let fog = if params.fog_end < 10000.0 {
            Some(DistanceFog {
                color: Color::srgb(params.fog_color[0], params.fog_color[1], params.fog_color[2]),
                falloff: FogFalloff::Linear { start: params.fog_start, end: params.fog_end.max(params.fog_start + 1.0) },
                ..default()
            })
        } else if let Some(a) = params.atmosphere {
            let c = Vec3::from(a.color).lerp(Vec3::splat(0.75), 0.3) * (0.25 + 0.75 * day);
            Some(DistanceFog {
                color: Color::srgba(c.x, c.y, c.z, 1.0),
                falloff: FogFalloff::Exponential { density: 0.0004 + a.density * 0.002 + a.haze * 0.0006 },
                ..default()
            })
        } else {
            None
        };
        match fog {
            Some(f) => {
                ec.insert(f);
            }
            None => {
                ec.remove::<DistanceFog>();
            }
        }
    }
    applied.params = Some(params);
    applied.settings = Some(settings.clone());
}

/// Counts frames since a generated environment map was requested.
#[derive(Component)]
pub struct EnvMapBaking(u8);

/// Bevy refilters a `GeneratedEnvironmentMapLight` from the sky cubemap
/// every frame. The sky only changes with Lighting, so once the filtered
/// maps exist keep them (as the plain `EnvironmentMapLight` bevy created)
/// and stop regenerating. `apply_lighting` restarts this on change.
pub fn freeze_environment_map(
    mut commands: Commands,
    mut cams: Query<(Entity, &mut EnvMapBaking, Has<EnvironmentMapLight>)>,
) {
    // Filtering needs the sky image on the GPU first; give it a few frames.
    const FRAMES: u8 = 8;
    for (e, mut baking, has_maps) in &mut cams {
        if !has_maps {
            continue;
        }
        baking.0 += 1;
        if baking.0 >= FRAMES {
            commands.entity(e).remove::<(GeneratedEnvironmentMapLight, EnvMapBaking)>();
        }
    }
}

/// Spawns bevy lights for PointLight/SpotLight/SurfaceLight instances.
pub fn sync_local_lights(
    mut commands: Commands,
    studio: Res<Studio>,
    settings: Res<GraphicsSettings>,
    index: Res<super::sync::PartIndex>,
    lights: Query<Entity, With<LocalLight>>,
    mut last_gen: Local<(u64, u8, Option<Technology>, u64)>,
) {
    // Rebuild only when parts are added or removed (lights hang off part
    // entities) or a light changes; moving parts must not respawn every
    // light, which used to happen every frame while scripts ran.
    let key = (studio.generation, settings.quality, settings.technology_override, index.structure_version());
    let relevant = *last_gen != key || {
        let doc = studio.doc();
        studio.frame_changes.iter().any(|c| match c {
            phaedrus_dom::Change::Property(r, _) => doc.is_a(*r, "Light"),
            phaedrus_dom::Change::Added(r) | phaedrus_dom::Change::Reparented { instance: r, .. } => {
                doc.is_a(*r, "Light") || doc.descendants(*r).iter().any(|d| doc.is_a(*d, "Light"))
            }
        })
    };
    if !relevant {
        return;
    }
    *last_gen = key;
    for e in &lights {
        commands.entity(e).despawn();
    }
    let doc = studio.doc();
    let Some(ws) = doc.workspace() else { return };
    let tech = settings.technology_override.unwrap_or(read_lighting(&doc).technology);
    let prof = profile(settings.quality, tech);
    for r in doc.descendants(ws) {
        let Some(inst) = doc.get(r) else { continue };
        if !reflection::is_a(&inst.class, "Light") || !bool_of(&doc, r, "Enabled", true) {
            continue;
        }
        let parent = doc.parent(r);
        let Some(&parent_entity) = index.map.get(&parent) else { continue };
        let color = doc.color_of(r, "Color").unwrap_or([1.0; 3]);
        let brightness = f32_of(&doc, r, "Brightness", 1.0);
        let range = f32_of(&doc, r, "Range", 8.0);
        let wants_shadows = bool_of(&doc, r, "Shadows", false) && prof.local_shadows;
        let color = Color::srgb(color[0], color[1], color[2]);
        // Lights start off; `budget_local_lights` turns on the nearest ones
        // and picks which cast shadows.
        let shadows = false;
        let mut ec = commands.spawn((LocalLight { wants_shadows }, Visibility::Hidden, ChildOf(parent_entity)));
        match inst.class.as_str() {
            "SpotLight" | "SurfaceLight" => {
                let angle = f32_of(&doc, r, "Angle", 90.0).to_radians().min(3.1);
                ec.insert((
                    SpotLight {
                        color,
                        intensity: brightness * 40_000.0,
                        range,
                        outer_angle: angle * 0.5,
                        inner_angle: angle * 0.35,
                        shadow_maps_enabled: shadows,
                        ..default()
                    },
                    Transform::IDENTITY.looking_to(Vec3::NEG_Z, Vec3::Y),
                ));
            }
            _ => {
                ec.insert((
                    PointLight {
                        color,
                        intensity: brightness * 40_000.0,
                        range,
                        shadow_maps_enabled: shadows,
                        ..default()
                    },
                    Transform::IDENTITY,
                ));
            }
        }
    }
}

/// Limits local lights to the nearest `max_lights` within `light_distance`
/// (and the draw distance) of the camera, and lets only the nearest
/// `max_shadow_lights` shadow-casting ones render shadow maps. Each shadowed
/// point light re-renders the surrounding parts into six cube faces, which
/// is what made light-heavy places slow.
#[allow(clippy::type_complexity)]
pub fn budget_local_lights(
    studio: Res<Studio>,
    settings: Res<GraphicsSettings>,
    camera: Query<&GlobalTransform, With<EditorCamera>>,
    mut lights: Query<(
        Entity,
        &LocalLight,
        &GlobalTransform,
        &mut Visibility,
        Option<&mut PointLight>,
        Option<&mut SpotLight>,
    )>,
    // World positions are only known after transform propagation, the frame
    // after a light spawns, and change when its part moves.
    moved: Query<(), (With<LocalLight>, Changed<GlobalTransform>)>,
    mut last: Local<Option<(Vec3, u8, u8, Option<Technology>)>>,
    mut order: Local<Vec<(f32, Entity)>>,
) {
    let Ok(cam) = camera.single() else { return };
    let cam_pos = cam.translation();
    // Re-rank when the camera has moved a few studs, lights moved or were
    // respawned, or settings changed; otherwise the last assignment holds.
    let key = (settings.quality, settings.draw_distance, settings.technology_override);
    let unchanged = last.is_some_and(|(p, q, d, t)| p.distance_squared(cam_pos) < 4.0 * 4.0 && (q, d, t) == key);
    if unchanged && moved.is_empty() && studio.frame_changes.is_empty() {
        return;
    }
    *last = Some((cam_pos, key.0, key.1, key.2));
    let tech = settings.technology_override.unwrap_or_else(|| read_lighting(&studio.doc()).technology);
    let prof = profile(settings.quality, tech);
    let max_dist = prof.light_distance.min(draw_distance_studs(settings.draw_distance));

    order.clear();
    for (e, _, t, ..) in &lights {
        let d = t.translation().distance(cam_pos);
        if d <= max_dist {
            order.push((d, e));
        }
    }
    order.sort_by(|a, b| a.0.total_cmp(&b.0));
    order.truncate(prof.max_lights);

    let mut shadowed = 0;
    let mut on = std::collections::HashSet::with_capacity(order.len());
    for &(_, e) in order.iter() {
        on.insert(e);
    }
    for &(_, e) in order.iter() {
        let Ok((_, light, _, mut vis, point, spot)) = lights.get_mut(e) else { continue };
        let shadows = light.wants_shadows && shadowed < prof.max_shadow_lights;
        shadowed += shadows as usize;
        // Visible, not Inherited: Roblox lights still shine from inside
        // fully transparent parts, which are hidden entities here.
        vis.set_if_neq(Visibility::Visible);
        if let Some(mut p) = point
            && p.shadow_maps_enabled != shadows
        {
            p.shadow_maps_enabled = shadows;
        }
        if let Some(mut s) = spot
            && s.shadow_maps_enabled != shadows
        {
            s.shadow_maps_enabled = shadows;
        }
    }
    for (e, _, _, mut vis, ..) in &mut lights {
        if !on.contains(&e) {
            vis.set_if_neq(Visibility::Hidden);
        }
    }
}

/// Keeps the sun/moon billboards around the camera.
pub fn place_celestial_bodies(
    camera: Query<&Transform, (With<EditorCamera>, Without<CelestialBody>)>,
    mut bodies: Query<(&CelestialBody, &mut Transform, &mut Visibility)>,
    sky_state: Res<SkyState>,
) {
    let Ok(cam) = camera.single() else { return };
    for (body, mut t, mut vis) in &mut bodies {
        let dir = if body.is_moon { -sky_state.sun_dir } else { sky_state.sun_dir };
        let show = sky_state.celestial_bodies && dir.y > -0.1;
        *vis = if show { Visibility::Visible } else { Visibility::Hidden };
        let distance = 4000.0;
        let size = distance * (body.angular_size_deg.to_radians()).tan() * 2.0;
        t.translation = cam.translation + dir * distance;
        t.rotation = Transform::IDENTITY.looking_to(-dir, Vec3::Y).rotation;
        t.scale = Vec3::splat(size);
    }
}
