//! `Vector3` and `Vector2`.

use glam::{Vec2, Vec3};
use mlua::{FromLua, Lua, MetaMethod, Result, Table, UserData, UserDataFields, UserDataMethods, Value};

use super::num;

#[derive(Debug, Clone, Copy, PartialEq, FromLua)]
pub struct Vector3(pub Vec3);

impl Vector3 {
    pub fn to_rbx(self) -> phaedrus_dom::types::Vector3 {
        phaedrus_dom::types::Vector3::new(self.0.x, self.0.y, self.0.z)
    }
    pub fn from_rbx(v: phaedrus_dom::types::Vector3) -> Self {
        Self(Vec3::new(v.x, v.y, v.z))
    }
}

fn vec3_arg(v: &Value, what: &str) -> Result<Vec3> {
    match v {
        Value::UserData(ud) => Ok(ud.borrow::<Vector3>()?.0),
        Value::Integer(_) | Value::Number(_) => {
            let n = num(v)?;
            Ok(Vec3::splat(n))
        }
        _ => Err(mlua::Error::runtime(format!("invalid argument to {what}: expected Vector3"))),
    }
}

impl UserData for Vector3 {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_meta_field(MetaMethod::Type, "Vector3");
        f.add_field_method_get("X", |_, v| Ok(v.0.x));
        f.add_field_method_get("Y", |_, v| Ok(v.0.y));
        f.add_field_method_get("Z", |_, v| Ok(v.0.z));
        f.add_field_method_get("x", |_, v| Ok(v.0.x));
        f.add_field_method_get("y", |_, v| Ok(v.0.y));
        f.add_field_method_get("z", |_, v| Ok(v.0.z));
        f.add_field_method_get("Magnitude", |_, v| Ok(v.0.length()));
        f.add_field_method_get("Unit", |_, v| Ok(Vector3(v.0.normalize_or_zero())));
    }

    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("Dot", |_, a, b: Vector3| Ok(a.0.dot(b.0)));
        m.add_method("Cross", |_, a, b: Vector3| Ok(Vector3(a.0.cross(b.0))));
        m.add_method("Lerp", |_, a, (b, t): (Vector3, f32)| Ok(Vector3(a.0.lerp(b.0, t))));
        m.add_method("Abs", |_, a, ()| Ok(Vector3(a.0.abs())));
        m.add_method("Floor", |_, a, ()| Ok(Vector3(a.0.floor())));
        m.add_method("Ceil", |_, a, ()| Ok(Vector3(a.0.ceil())));
        m.add_method("Sign", |_, a, ()| Ok(Vector3(a.0.signum())));
        m.add_method("Min", |_, a, b: Vector3| Ok(Vector3(a.0.min(b.0))));
        m.add_method("Max", |_, a, b: Vector3| Ok(Vector3(a.0.max(b.0))));
        m.add_method("Angle", |_, a, b: Vector3| Ok(a.0.angle_between(b.0)));
        m.add_method("FuzzyEq", |_, a, (b, eps): (Vector3, Option<f32>)| {
            Ok(a.0.abs_diff_eq(b.0, eps.unwrap_or(1e-5)))
        });
        m.add_meta_function(MetaMethod::Add, |_, (a, b): (Vector3, Vector3)| Ok(Vector3(a.0 + b.0)));
        m.add_meta_function(MetaMethod::Sub, |_, (a, b): (Vector3, Vector3)| Ok(Vector3(a.0 - b.0)));
        m.add_meta_function(MetaMethod::Mul, |_, (a, b): (Value, Value)| {
            Ok(Vector3(vec3_arg(&a, "*")? * vec3_arg(&b, "*")?))
        });
        m.add_meta_function(MetaMethod::Div, |_, (a, b): (Value, Value)| {
            Ok(Vector3(vec3_arg(&a, "/")? / vec3_arg(&b, "/")?))
        });
        m.add_meta_function(MetaMethod::IDiv, |_, (a, b): (Value, Value)| {
            Ok(Vector3((vec3_arg(&a, "//")? / vec3_arg(&b, "//")?).floor()))
        });
        m.add_meta_method(MetaMethod::Unm, |_, a, ()| Ok(Vector3(-a.0)));
        m.add_meta_method(MetaMethod::Eq, |_, a, b: Vector3| Ok(a.0 == b.0));
        m.add_meta_method(MetaMethod::ToString, |_, a, ()| {
            Ok(format!("{}, {}, {}", fmt(a.0.x), fmt(a.0.y), fmt(a.0.z)))
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, FromLua)]
pub struct Vector2(pub Vec2);

impl UserData for Vector2 {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_meta_field(MetaMethod::Type, "Vector2");
        f.add_field_method_get("X", |_, v| Ok(v.0.x));
        f.add_field_method_get("Y", |_, v| Ok(v.0.y));
        f.add_field_method_get("Magnitude", |_, v| Ok(v.0.length()));
        f.add_field_method_get("Unit", |_, v| Ok(Vector2(v.0.normalize_or_zero())));
    }

    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("Dot", |_, a, b: Vector2| Ok(a.0.dot(b.0)));
        m.add_method("Cross", |_, a, b: Vector2| Ok(a.0.perp_dot(b.0)));
        m.add_method("Lerp", |_, a, (b, t): (Vector2, f32)| Ok(Vector2(a.0.lerp(b.0, t))));
        m.add_meta_function(MetaMethod::Add, |_, (a, b): (Vector2, Vector2)| Ok(Vector2(a.0 + b.0)));
        m.add_meta_function(MetaMethod::Sub, |_, (a, b): (Vector2, Vector2)| Ok(Vector2(a.0 - b.0)));
        m.add_meta_function(MetaMethod::Mul, |_, (a, b): (Value, Value)| {
            Ok(Vector2(vec2_arg(&a)? * vec2_arg(&b)?))
        });
        m.add_meta_function(MetaMethod::Div, |_, (a, b): (Value, Value)| {
            Ok(Vector2(vec2_arg(&a)? / vec2_arg(&b)?))
        });
        m.add_meta_method(MetaMethod::Unm, |_, a, ()| Ok(Vector2(-a.0)));
        m.add_meta_method(MetaMethod::Eq, |_, a, b: Vector2| Ok(a.0 == b.0));
        m.add_meta_method(MetaMethod::ToString, |_, a, ()| {
            Ok(format!("{}, {}", fmt(a.0.x), fmt(a.0.y)))
        });
    }
}

fn vec2_arg(v: &Value) -> Result<Vec2> {
    match v {
        Value::UserData(ud) => Ok(ud.borrow::<Vector2>()?.0),
        _ => Ok(Vec2::splat(num(v)?)),
    }
}

/// Formats a float like Roblox's tostring (no trailing `.0`).
pub fn fmt(v: f32) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

pub fn register(lua: &Lua, g: &Table) -> Result<()> {
    let v3 = lua.create_table()?;
    v3.set(
        "new",
        lua.create_function(|_, (x, y, z): (Option<f32>, Option<f32>, Option<f32>)| {
            Ok(Vector3(Vec3::new(x.unwrap_or(0.0), y.unwrap_or(0.0), z.unwrap_or(0.0))))
        })?,
    )?;
    v3.set("zero", Vector3(Vec3::ZERO))?;
    v3.set("one", Vector3(Vec3::ONE))?;
    v3.set("xAxis", Vector3(Vec3::X))?;
    v3.set("yAxis", Vector3(Vec3::Y))?;
    v3.set("zAxis", Vector3(Vec3::Z))?;
    g.set("Vector3", v3)?;

    let v2 = lua.create_table()?;
    v2.set(
        "new",
        lua.create_function(|_, (x, y): (Option<f32>, Option<f32>)| {
            Ok(Vector2(Vec2::new(x.unwrap_or(0.0), y.unwrap_or(0.0))))
        })?,
    )?;
    v2.set("zero", Vector2(Vec2::ZERO))?;
    v2.set("one", Vector2(Vec2::ONE))?;
    v2.set("xAxis", Vector2(Vec2::X))?;
    v2.set("yAxis", Vector2(Vec2::Y))?;
    g.set("Vector2", v2)?;
    Ok(())
}
