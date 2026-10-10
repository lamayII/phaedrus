//! Extracts render-relevant data from parts without pulling in a math crate.

use std::sync::OnceLock;

use rbx_dom_weak::Instance;
use rbx_types::{BrickColor, CFrame, Color3, Color3uint8, Matrix3, Variant, Vector3};

use crate::reflection;

/// The primitive shape a part renders as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PartShape {
    Ball,
    Block,
    Cylinder,
    Wedge,
    CornerWedge,
    /// A CSG union, keyed by [`crate::union::union_mesh_key`]. Renders as its
    /// bounding box until the mesh is available.
    Union(u64),
    /// Meshes and other geometry we cannot rebuild locally render as their
    /// bounding box.
    Proxy,
}

impl PartShape {
    /// Values of the `Enum.PartType` enum.
    pub fn from_part_type(value: u32) -> Self {
        match value {
            0 => Self::Ball,
            1 => Self::Block,
            2 => Self::Cylinder,
            3 => Self::Wedge,
            4 => Self::CornerWedge,
            _ => Self::Block,
        }
    }
}

/// Everything the renderer needs to draw one BasePart.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PartGeometry {
    pub shape: PartShape,
    pub cframe: CFrame,
    pub size: Vector3,
    /// Linear-ish sRGB, 0..1.
    pub color: [f32; 3],
    pub transparency: f32,
    pub reflectance: f32,
    /// `Enum.Material` value.
    pub material: u32,
    pub cast_shadow: bool,
}

/// A stored property, or the class default when the property was compacted
/// away (see [`crate::document::compact_defaults`]).
fn prop<'a>(inst: &'a Instance, name: &str) -> Option<&'a Variant> {
    inst.properties
        .get(&name.into())
        .or_else(|| reflection::default_value(&inst.class, name))
}

/// Returns geometry for any instance that is a BasePart, otherwise `None`.
pub fn part_geometry(inst: &Instance) -> Option<PartGeometry> {
    let class = inst.class.as_str();
    if !reflection::is_a(class, "BasePart") {
        return None;
    }
    let shape = match class {
        "Part" | "SpawnLocation" | "Seat" | "SkateboardPlatform" | "Platform" => {
            match prop(inst, "Shape") {
                Some(Variant::Enum(e)) => PartShape::from_part_type(e.to_u32()),
                _ => PartShape::Block,
            }
        }
        "WedgePart" => PartShape::Wedge,
        "CornerWedgePart" => PartShape::CornerWedge,
        "TrussPart" | "VehicleSeat" => PartShape::Block,
        "Terrain" => return None,
        c if crate::union::is_union_class(c) => {
            crate::union::union_mesh_key(inst).map_or(PartShape::Proxy, PartShape::Union)
        }
        _ => PartShape::Proxy,
    };
    let cframe = match prop(inst, "CFrame") {
        Some(Variant::CFrame(c)) => *c,
        _ => CFrame::new(Vector3::new(0.0, 0.0, 0.0), Matrix3::identity()),
    };
    let size = match prop(inst, "Size") {
        Some(Variant::Vector3(v)) => *v,
        _ => Vector3::new(4.0, 1.0, 2.0),
    };
    // Unions that keep their parts' colours draw them from vertex colours.
    let color = if matches!(shape, PartShape::Union(_)) && !crate::union::uses_part_color(inst) {
        [1.0; 3]
    } else {
        part_color(inst).unwrap_or([0.64, 0.635, 0.647])
    };
    let f32_prop = |name: &str, default: f32| match prop(inst, name) {
        Some(Variant::Float32(v)) => *v,
        Some(Variant::Float64(v)) => *v as f32,
        _ => default,
    };
    let material = match prop(inst, "Material") {
        Some(Variant::Enum(e)) => e.to_u32(),
        _ => 256,
    };
    let cast_shadow = !matches!(
        prop(inst, "CastShadow"),
        Some(Variant::Bool(false))
    );
    Some(PartGeometry {
        shape,
        cframe,
        size,
        color,
        transparency: f32_prop("Transparency", 0.0),
        reflectance: f32_prop("Reflectance", 0.0),
        material,
        cast_shadow,
    })
}

fn part_color(inst: &Instance) -> Option<[f32; 3]> {
    match prop(inst, "Color")? {
        Variant::Color3uint8(c) => Some(color3uint8_to_f32(*c)),
        Variant::Color3(c) => Some([c.r, c.g, c.b]),
        Variant::BrickColor(b) => Some(color3uint8_to_f32(b.to_color3uint8())),
        _ => None,
    }
}

pub fn color3uint8_to_f32(c: Color3uint8) -> [f32; 3] {
    [c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0]
}

pub fn color3_to_u8(c: Color3) -> Color3uint8 {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color3uint8::new(q(c.r), q(c.g), q(c.b))
}

/// All defined BrickColors (index order).
pub fn all_brick_colors() -> &'static [BrickColor] {
    static ALL: OnceLock<Vec<BrickColor>> = OnceLock::new();
    ALL.get_or_init(|| (0..=1100u16).filter_map(BrickColor::from_number).collect())
}

/// The BrickColor closest to an RGB color, matching how Roblox derives
/// `BasePart.BrickColor` from `BasePart.Color`.
pub fn nearest_brick_color(c: Color3uint8) -> BrickColor {
    *all_brick_colors()
        .iter()
        .min_by_key(|b| {
            let o = b.to_color3uint8();
            let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
            d(o.r, c.r) + d(o.g, c.g) + d(o.b, c.b)
        })
        .unwrap_or(&BrickColor::MediumStoneGrey)
}

// --- CFrame helpers -------------------------------------------------------
//
// rbx_types stores `Matrix3` as rows: `x` is the first row, i.e.
// (R00, R01, R02). The columns are the RightVector, UpVector and -LookVector.

pub fn mat_column(m: &Matrix3, i: usize) -> Vector3 {
    let pick = |r: &Vector3| match i {
        0 => r.x,
        1 => r.y,
        _ => r.z,
    };
    Vector3::new(pick(&m.x), pick(&m.y), pick(&m.z))
}

pub fn mat_from_columns(x: Vector3, y: Vector3, z: Vector3) -> Matrix3 {
    Matrix3::new(
        Vector3::new(x.x, y.x, z.x),
        Vector3::new(x.y, y.y, z.y),
        Vector3::new(x.z, y.z, z.z),
    )
}

pub fn mat_mul(a: &Matrix3, b: &Matrix3) -> Matrix3 {
    let row = |r: &Vector3| {
        let c0 = mat_column(b, 0);
        let c1 = mat_column(b, 1);
        let c2 = mat_column(b, 2);
        Vector3::new(dot(r, &c0), dot(r, &c1), dot(r, &c2))
    };
    Matrix3::new(row(&a.x), row(&a.y), row(&a.z))
}

pub fn mat_mul_vec(m: &Matrix3, v: &Vector3) -> Vector3 {
    Vector3::new(dot(&m.x, v), dot(&m.y, v), dot(&m.z, v))
}

pub fn dot(a: &Vector3, b: &Vector3) -> f32 {
    a.x * b.x + a.y * b.y + a.z * b.z
}

/// Rotation matrix from Roblox `Orientation` (degrees, applied Y then X then
/// Z, i.e. `CFrame.fromOrientation`).
pub fn mat_from_orientation_deg(o: Vector3) -> Matrix3 {
    let (rx, ry, rz) = (o.x.to_radians(), o.y.to_radians(), o.z.to_radians());
    mat_mul(&mat_mul(&rot_y(ry), &rot_x(rx)), &rot_z(rz))
}

/// Inverse of [`mat_from_orientation_deg`] (`CFrame:ToOrientation`).
pub fn mat_to_orientation_deg(m: &Matrix3) -> Vector3 {
    // R = Ry * Rx * Rz. Row 1 col 2 = -sin(x).
    let sx = (-m.y.z).clamp(-1.0, 1.0);
    let x = sx.asin();
    let (y, z) = if sx.abs() < 0.99999 {
        (m.x.z.atan2(m.z.z), m.y.x.atan2(m.y.y))
    } else {
        ((-m.z.x).atan2(m.x.x), 0.0)
    };
    let clean = |v: f32| {
        let d = v.to_degrees();
        let r = (d * 1000.0).round() / 1000.0;
        if r == -0.0 { 0.0 } else { r }
    };
    Vector3::new(clean(x), clean(y), clean(z))
}

pub fn rot_x(a: f32) -> Matrix3 {
    let (s, c) = a.sin_cos();
    Matrix3::new(
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, c, -s),
        Vector3::new(0.0, s, c),
    )
}

pub fn rot_y(a: f32) -> Matrix3 {
    let (s, c) = a.sin_cos();
    Matrix3::new(
        Vector3::new(c, 0.0, s),
        Vector3::new(0.0, 1.0, 0.0),
        Vector3::new(-s, 0.0, c),
    )
}

pub fn rot_z(a: f32) -> Matrix3 {
    let (s, c) = a.sin_cos();
    Matrix3::new(
        Vector3::new(c, -s, 0.0),
        Vector3::new(s, c, 0.0),
        Vector3::new(0.0, 0.0, 1.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orientation_round_trip() {
        for o in [
            Vector3::new(10.0, 20.0, 30.0),
            Vector3::new(-45.0, 90.0, 0.0),
            Vector3::new(0.0, -135.0, 60.0),
        ] {
            let m = mat_from_orientation_deg(o);
            let back = mat_to_orientation_deg(&m);
            let m2 = mat_from_orientation_deg(back);
            for (a, b) in [(m.x, m2.x), (m.y, m2.y), (m.z, m2.z)] {
                assert!((a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3 && (a.z - b.z).abs() < 1e-3);
            }
        }
    }
}
