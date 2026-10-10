//! Conversions between `rbx_types::Variant` and Luau values.

use glam::Vec2;
use mlua::{IntoLua, Lua, Result, Value};
use phaedrus_dom::reflection;
use phaedrus_dom::types::{self as rt, Variant, VariantType};

use crate::instance::{LInstance, instance_value};
use crate::types::*;

/// Converts a stored property value to Luau. `enum_name` is the property's
/// enum type, used to turn `Variant::Enum` into a proper `EnumItem`.
pub fn variant_to_lua(lua: &Lua, v: &Variant, enum_name: Option<&str>) -> Result<Value> {
    Ok(match v {
        Variant::Bool(b) => Value::Boolean(*b),
        Variant::Int32(i) => Value::Integer(*i as _),
        Variant::Int64(i) => Value::Number(*i as f64),
        Variant::Float32(f) => Value::Number(*f as f64),
        Variant::Float64(f) => Value::Number(*f),
        Variant::String(s) => s.as_str().into_lua(lua)?,
        Variant::BinaryString(b) => lua.create_string(AsRef::<[u8]>::as_ref(b))?.into_lua(lua)?,
        Variant::Vector3(v) => Vector3::from_rbx(*v).into_lua(lua)?,
        Variant::Vector2(v) => Vector2(Vec2::new(v.x, v.y)).into_lua(lua)?,
        Variant::CFrame(c) => CFrame::from_rbx(c).into_lua(lua)?,
        Variant::OptionalCFrame(Some(c)) => CFrame::from_rbx(c).into_lua(lua)?,
        Variant::OptionalCFrame(None) => Value::Nil,
        Variant::Color3(c) => Color3 { r: c.r, g: c.g, b: c.b }.into_lua(lua)?,
        Variant::Color3uint8(c) => Color3::from_u8(*c).into_lua(lua)?,
        Variant::BrickColor(b) => BrickColor(*b).into_lua(lua)?,
        Variant::UDim(u) => UDim { scale: u.scale, offset: u.offset }.into_lua(lua)?,
        Variant::UDim2(u) => UDim2 {
            x: UDim { scale: u.x.scale, offset: u.x.offset },
            y: UDim { scale: u.y.scale, offset: u.y.offset },
        }
        .into_lua(lua)?,
        Variant::NumberRange(n) => NumberRange { min: n.min, max: n.max }.into_lua(lua)?,
        Variant::Enum(e) => match enum_name.and_then(|n| EnumItem::from_value(n, e.to_u32())) {
            Some(item) => item.into_lua(lua)?,
            None => Value::Integer(e.to_u32() as _),
        },
        Variant::EnumItem(e) => match EnumItem::from_value(&e.ty, e.value) {
            Some(item) => item.into_lua(lua)?,
            None => Value::Integer(e.value as _),
        },
        Variant::Ref(r) => instance_value(lua, *r)?,
        Variant::ContentId(c) => c.as_str().into_lua(lua)?,
        Variant::Content(c) => match c.value() {
            rt::ContentType::Uri(u) => u.as_str().into_lua(lua)?,
            _ => "".into_lua(lua)?,
        },
        Variant::Tags(t) => lua.create_sequence_from(t.iter().map(|s| s.to_owned()))?.into_lua(lua)?,
        other => format!("{:?}", other.ty()).into_lua(lua)?,
    })
}

/// Converts a Luau value to a `Variant` for property `name` on `class`.
/// `current` is the existing value, used to pick a type when reflection
/// does not know the property.
pub fn lua_to_variant(
    class: &str,
    name: &str,
    value: &Value,
    current: Option<&Variant>,
) -> std::result::Result<Variant, String> {
    let expected = reflection::find_property(class, name).map(|p| p.data_type.ty());
    let expected = expected.or_else(|| current.map(|c| c.ty()));
    let enum_name = reflection::property_enum(class, name);
    let bad = |want: &str| {
        Err(format!(
            "Unable to assign property {name}. {want} expected, got {}",
            lua_type_name(value)
        ))
    };
    match expected {
        Some(VariantType::Bool) => match value {
            Value::Boolean(b) => Ok(Variant::Bool(*b)),
            Value::Nil => Ok(Variant::Bool(false)),
            _ => bad("bool"),
        },
        Some(t @ (VariantType::Float32 | VariantType::Float64 | VariantType::Int32 | VariantType::Int64)) => {
            let n = match value {
                Value::Integer(i) => *i as f64,
                Value::Number(n) => *n,
                Value::String(s) => match s.to_str().ok().and_then(|s| s.trim().parse().ok()) {
                    Some(n) => n,
                    None => return bad("number"),
                },
                _ => return bad("number"),
            };
            Ok(match t {
                VariantType::Float32 => Variant::Float32(n as f32),
                VariantType::Float64 => Variant::Float64(n),
                VariantType::Int32 => Variant::Int32(n as i32),
                _ => Variant::Int64(n as i64),
            })
        }
        Some(VariantType::String) => match value {
            Value::String(s) => Ok(Variant::String(s.to_str().map_err(|e| e.to_string())?.to_owned())),
            Value::Integer(i) => Ok(Variant::String(i.to_string())),
            Value::Number(n) => Ok(Variant::String(n.to_string())),
            _ => bad("string"),
        },
        Some(VariantType::Enum) => {
            let item_value = match value {
                Value::UserData(ud) if ud.is::<EnumItem>() => {
                    let item = ud.borrow::<EnumItem>().map_err(|e| e.to_string())?;
                    if let Some(en) = enum_name
                        && item.enum_name != en {
                            return bad(&format!("Enum.{en}"));
                        }
                    item.value
                }
                Value::Integer(i) => *i as u32,
                Value::Number(n) => *n as u32,
                Value::String(s) => {
                    let s = s.to_str().map_err(|e| e.to_string())?;
                    match enum_name.and_then(|en| reflection::enum_value(en, &s)) {
                        Some(v) => v,
                        None => return bad("EnumItem"),
                    }
                }
                _ => return bad("EnumItem"),
            };
            Ok(Variant::Enum(rt::Enum::from_u32(item_value)))
        }
        Some(VariantType::Ref) => match value {
            Value::Nil => Ok(Variant::Ref(rt::Ref::none())),
            Value::UserData(ud) if ud.is::<LInstance>() => {
                Ok(Variant::Ref(ud.borrow::<LInstance>().map_err(|e| e.to_string())?.0))
            }
            _ => bad("Instance"),
        },
        Some(VariantType::Color3uint8) => match value {
            Value::UserData(ud) if ud.is::<Color3>() => {
                Ok(Variant::Color3uint8(ud.borrow::<Color3>().map_err(|e| e.to_string())?.to_u8()))
            }
            _ => bad("Color3"),
        },
        Some(VariantType::ContentId) => match value {
            Value::String(s) => Ok(Variant::ContentId(s.to_str().map_err(|e| e.to_string())?.to_owned().into())),
            _ => bad("string"),
        },
        Some(VariantType::Content) => match value {
            Value::String(s) => Ok(Variant::Content(rt::Content::from_uri(
                s.to_str().map_err(|e| e.to_string())?.to_owned(),
            ))),
            _ => bad("string"),
        },
        Some(VariantType::OptionalCFrame) => match value {
            Value::Nil => Ok(Variant::OptionalCFrame(None)),
            Value::UserData(ud) if ud.is::<CFrame>() => Ok(Variant::OptionalCFrame(Some(
                ud.borrow::<CFrame>().map_err(|e| e.to_string())?.to_rbx(),
            ))),
            _ => bad("CFrame"),
        },
        _ => untyped_to_variant(value).ok_or_else(|| {
            format!("Unable to assign property {name}. Unsupported value type {}", lua_type_name(value))
        }),
    }
}

/// Best-effort conversion when the expected type is unknown or matches the
/// Lua userdata type directly (Vector3, CFrame, Color3, ...).
pub fn untyped_to_variant(value: &Value) -> Option<Variant> {
    Some(match value {
        Value::Boolean(b) => Variant::Bool(*b),
        Value::Integer(i) => Variant::Float64(*i as f64),
        Value::Number(n) => Variant::Float64(*n),
        Value::String(s) => Variant::String(s.to_str().ok()?.to_owned()),
        Value::UserData(ud) => {
            if let Ok(v) = ud.borrow::<Vector3>() {
                Variant::Vector3(v.to_rbx())
            } else if let Ok(v) = ud.borrow::<Vector2>() {
                Variant::Vector2(rt::Vector2::new(v.0.x, v.0.y))
            } else if let Ok(c) = ud.borrow::<CFrame>() {
                Variant::CFrame(c.to_rbx())
            } else if let Ok(c) = ud.borrow::<Color3>() {
                Variant::Color3(rt::Color3::new(c.r, c.g, c.b))
            } else if let Ok(b) = ud.borrow::<BrickColor>() {
                Variant::BrickColor(b.0)
            } else if let Ok(u) = ud.borrow::<UDim>() {
                Variant::UDim(rt::UDim::new(u.scale, u.offset))
            } else if let Ok(u) = ud.borrow::<UDim2>() {
                Variant::UDim2(rt::UDim2::new(
                    rt::UDim::new(u.x.scale, u.x.offset),
                    rt::UDim::new(u.y.scale, u.y.offset),
                ))
            } else if let Ok(n) = ud.borrow::<NumberRange>() {
                Variant::NumberRange(rt::NumberRange::new(n.min, n.max))
            } else if let Ok(e) = ud.borrow::<EnumItem>() {
                Variant::Enum(rt::Enum::from_u32(e.value))
            } else if let Ok(i) = ud.borrow::<LInstance>() {
                Variant::Ref(i.0)
            } else {
                return None;
            }
        }
        Value::Nil => Variant::Ref(rt::Ref::none()),
        _ => return None,
    })
}

/// Attribute values are restricted to a subset of types.
pub fn lua_to_attribute(value: &Value) -> Option<Variant> {
    match value {
        Value::Integer(i) => Some(Variant::Float64(*i as f64)),
        Value::Number(n) => Some(Variant::Float64(*n)),
        _ => untyped_to_variant(value),
    }
}

pub fn lua_type_name(v: &Value) -> String {
    match v {
        Value::UserData(ud) => ud
            .metatable()
            .ok()
            .and_then(|mt| mt.get::<String>("__type").ok())
            .unwrap_or_else(|| "userdata".into()),
        other => other.type_name().to_owned(),
    }
}
