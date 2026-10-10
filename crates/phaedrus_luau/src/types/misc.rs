//! `Color3`, `BrickColor`, `UDim`, `UDim2`, `NumberRange`, `Enum`.

use mlua::{
    FromLua, Lua, MetaMethod, Result, Table, UserData, UserDataFields, UserDataMethods, Value,
};
use phaedrus_dom::geometry::{all_brick_colors, nearest_brick_color};
use phaedrus_dom::reflection;
use phaedrus_dom::types::{BrickColor as RbxBrickColor, Color3uint8};

use super::num;
use super::vector::fmt;

#[derive(Debug, Clone, Copy, PartialEq, FromLua)]
pub struct Color3 {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl Color3 {
    pub fn to_u8(self) -> Color3uint8 {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        Color3uint8::new(q(self.r), q(self.g), q(self.b))
    }
    pub fn from_u8(c: Color3uint8) -> Self {
        Self { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0 }
    }
    fn to_hsv(self) -> (f32, f32, f32) {
        let max = self.r.max(self.g).max(self.b);
        let min = self.r.min(self.g).min(self.b);
        let d = max - min;
        let h = if d == 0.0 {
            0.0
        } else if max == self.r {
            ((self.g - self.b) / d).rem_euclid(6.0) / 6.0
        } else if max == self.g {
            ((self.b - self.r) / d + 2.0) / 6.0
        } else {
            ((self.r - self.g) / d + 4.0) / 6.0
        };
        let s = if max == 0.0 { 0.0 } else { d / max };
        (h, s, max)
    }
    pub fn from_hsv(h: f32, s: f32, v: f32) -> Self {
        let h6 = (h.rem_euclid(1.0)) * 6.0;
        let c = v * s;
        let x = c * (1.0 - (h6 % 2.0 - 1.0).abs());
        let (r, g, b) = match h6 as i32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        let m = v - c;
        Self { r: r + m, g: g + m, b: b + m }
    }
}

impl UserData for Color3 {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_meta_field(MetaMethod::Type, "Color3");
        f.add_field_method_get("R", |_, c| Ok(c.r));
        f.add_field_method_get("G", |_, c| Ok(c.g));
        f.add_field_method_get("B", |_, c| Ok(c.b));
        f.add_field_method_get("r", |_, c| Ok(c.r));
        f.add_field_method_get("g", |_, c| Ok(c.g));
        f.add_field_method_get("b", |_, c| Ok(c.b));
    }
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("Lerp", |_, a, (b, t): (Color3, f32)| {
            Ok(Color3 { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t })
        });
        m.add_method("ToHSV", |_, c, ()| Ok(c.to_hsv()));
        m.add_method("ToHex", |_, c, ()| {
            let u = c.to_u8();
            Ok(format!("{:02x}{:02x}{:02x}", u.r, u.g, u.b))
        });
        m.add_meta_method(MetaMethod::Eq, |_, a, b: Color3| Ok(*a == b));
        m.add_meta_method(MetaMethod::ToString, |_, c, ()| {
            Ok(format!("{}, {}, {}", fmt(c.r), fmt(c.g), fmt(c.b)))
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, FromLua)]
pub struct BrickColor(pub RbxBrickColor);

impl UserData for BrickColor {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_meta_field(MetaMethod::Type, "BrickColor");
        f.add_field_method_get("Name", |_, b| Ok(b.0.to_string()));
        f.add_field_method_get("Number", |_, b| Ok(b.0 as u16));
        f.add_field_method_get("Color", |_, b| Ok(Color3::from_u8(b.0.to_color3uint8())));
        f.add_field_method_get("r", |_, b| Ok(b.0.to_color3uint8().r as f32 / 255.0));
        f.add_field_method_get("g", |_, b| Ok(b.0.to_color3uint8().g as f32 / 255.0));
        f.add_field_method_get("b", |_, b| Ok(b.0.to_color3uint8().b as f32 / 255.0));
    }
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_meta_method(MetaMethod::Eq, |_, a, b: BrickColor| Ok(*a == b));
        m.add_meta_method(MetaMethod::ToString, |_, b, ()| Ok(b.0.to_string()));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, FromLua)]
pub struct UDim {
    pub scale: f32,
    pub offset: i32,
}

impl UserData for UDim {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_meta_field(MetaMethod::Type, "UDim");
        f.add_field_method_get("Scale", |_, u| Ok(u.scale));
        f.add_field_method_get("Offset", |_, u| Ok(u.offset));
    }
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_meta_function(MetaMethod::Add, |_, (a, b): (UDim, UDim)| {
            Ok(UDim { scale: a.scale + b.scale, offset: a.offset + b.offset })
        });
        m.add_meta_function(MetaMethod::Sub, |_, (a, b): (UDim, UDim)| {
            Ok(UDim { scale: a.scale - b.scale, offset: a.offset - b.offset })
        });
        m.add_meta_method(MetaMethod::Eq, |_, a, b: UDim| Ok(*a == b));
        m.add_meta_method(MetaMethod::ToString, |_, u, ()| Ok(format!("{}, {}", fmt(u.scale), u.offset)));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, FromLua)]
pub struct UDim2 {
    pub x: UDim,
    pub y: UDim,
}

impl UserData for UDim2 {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_meta_field(MetaMethod::Type, "UDim2");
        f.add_field_method_get("X", |_, u| Ok(u.x));
        f.add_field_method_get("Y", |_, u| Ok(u.y));
        f.add_field_method_get("Width", |_, u| Ok(u.x));
        f.add_field_method_get("Height", |_, u| Ok(u.y));
    }
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("Lerp", |_, a, (b, t): (UDim2, f32)| {
            let l = |x: UDim, y: UDim| UDim {
                scale: x.scale + (y.scale - x.scale) * t,
                offset: (x.offset as f32 + (y.offset - x.offset) as f32 * t).round() as i32,
            };
            Ok(UDim2 { x: l(a.x, b.x), y: l(a.y, b.y) })
        });
        m.add_meta_function(MetaMethod::Add, |_, (a, b): (UDim2, UDim2)| {
            Ok(UDim2 {
                x: UDim { scale: a.x.scale + b.x.scale, offset: a.x.offset + b.x.offset },
                y: UDim { scale: a.y.scale + b.y.scale, offset: a.y.offset + b.y.offset },
            })
        });
        m.add_meta_method(MetaMethod::Eq, |_, a, b: UDim2| Ok(*a == b));
        m.add_meta_method(MetaMethod::ToString, |_, u, ()| {
            Ok(format!("{{{}, {}}}, {{{}, {}}}", fmt(u.x.scale), u.x.offset, fmt(u.y.scale), u.y.offset))
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, FromLua)]
pub struct NumberRange {
    pub min: f32,
    pub max: f32,
}

impl UserData for NumberRange {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_meta_field(MetaMethod::Type, "NumberRange");
        f.add_field_method_get("Min", |_, n| Ok(n.min));
        f.add_field_method_get("Max", |_, n| Ok(n.max));
    }
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_meta_method(MetaMethod::ToString, |_, n, ()| Ok(format!("{} {}", fmt(n.min), fmt(n.max))));
    }
}

/// `Enum.Material.Plastic` etc.
#[derive(Debug, Clone, PartialEq, FromLua)]
pub struct EnumItem {
    pub enum_name: &'static str,
    pub name: &'static str,
    pub value: u32,
}

impl EnumItem {
    pub fn from_value(enum_name: &str, value: u32) -> Option<Self> {
        let (enum_name, _) = reflection::database().enums.get_key_value(enum_name)?;
        let name = reflection::enum_item_name(enum_name, value)?;
        Some(Self { enum_name, name, value })
    }
}

impl UserData for EnumItem {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_meta_field(MetaMethod::Type, "EnumItem");
        f.add_field_method_get("Name", |_, e| Ok(e.name));
        f.add_field_method_get("Value", |_, e| Ok(e.value));
        f.add_field_method_get("EnumType", |_, e| Ok(EnumType(e.enum_name)));
    }
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("IsA", |_, e, name: String| Ok(e.enum_name == name));
        m.add_meta_method(MetaMethod::Eq, |_, a, b: EnumItem| Ok(*a == b));
        m.add_meta_method(MetaMethod::ToString, |_, e, ()| Ok(format!("Enum.{}.{}", e.enum_name, e.name)));
    }
}

/// `Enum.Material`.
#[derive(Debug, Clone, Copy, PartialEq, FromLua)]
pub struct EnumType(pub &'static str);

impl UserData for EnumType {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_meta_field(MetaMethod::Type, "Enum");
    }
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("GetEnumItems", |_, e, ()| {
            Ok(reflection::enum_items(e.0)
                .into_iter()
                .map(|(name, value)| EnumItem { enum_name: e.0, name, value })
                .collect::<Vec<_>>())
        });
        m.add_method("FromName", |_, e, name: String| {
            Ok(reflection::enum_value(e.0, &name).and_then(|v| EnumItem::from_value(e.0, v)))
        });
        m.add_method("FromValue", |_, e, v: u32| Ok(EnumItem::from_value(e.0, v)));
        m.add_meta_method(MetaMethod::Index, |_, e, key: String| {
            let value = reflection::enum_value(e.0, &key).ok_or_else(|| {
                mlua::Error::runtime(format!("{key} is not a valid member of \"Enum.{}\"", e.0))
            })?;
            Ok(EnumItem::from_value(e.0, value))
        });
        m.add_meta_method(MetaMethod::ToString, |_, e, ()| Ok(e.0));
    }
}

struct Enums;

impl UserData for Enums {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_meta_field(MetaMethod::Type, "Enums");
    }
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("GetEnums", |_, _, ()| {
            Ok(reflection::database().enums.keys().map(|k| EnumType(k)).collect::<Vec<_>>())
        });
        m.add_meta_method(MetaMethod::Index, |_, _, key: String| {
            let (name, _) = reflection::database()
                .enums
                .get_key_value(key.as_str())
                .ok_or_else(|| mlua::Error::runtime(format!("{key} is not a valid member of \"Enums\"")))?;
            Ok(EnumType(name))
        });
        m.add_meta_method(MetaMethod::ToString, |_, _, ()| Ok("Enums"));
    }
}

pub fn register(lua: &Lua, g: &Table) -> Result<()> {
    let c3 = lua.create_table()?;
    c3.set(
        "new",
        lua.create_function(|_, (r, gg, b): (Option<f32>, Option<f32>, Option<f32>)| {
            Ok(Color3 { r: r.unwrap_or(0.0), g: gg.unwrap_or(0.0), b: b.unwrap_or(0.0) })
        })?,
    )?;
    c3.set(
        "fromRGB",
        lua.create_function(|_, (r, gg, b): (Option<f32>, Option<f32>, Option<f32>)| {
            Ok(Color3 {
                r: r.unwrap_or(0.0) / 255.0,
                g: gg.unwrap_or(0.0) / 255.0,
                b: b.unwrap_or(0.0) / 255.0,
            })
        })?,
    )?;
    c3.set("fromHSV", lua.create_function(|_, (h, s, v): (f32, f32, f32)| Ok(Color3::from_hsv(h, s, v)))?)?;
    c3.set(
        "fromHex",
        lua.create_function(|_, hex: String| {
            let h = hex.trim_start_matches('#');
            let parse = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0);
            Ok(Color3::from_u8(Color3uint8::new(parse(0), parse(2), parse(4))))
        })?,
    )?;
    c3.set("toHSV", lua.create_function(|_, c: Color3| Ok(c.to_hsv()))?)?;
    g.set("Color3", c3)?;

    let bc = lua.create_table()?;
    bc.set(
        "new",
        lua.create_function(|_, args: mlua::Variadic<Value>| {
            let first = args.first().cloned().unwrap_or(Value::Nil);
            let color = match &first {
                Value::String(s) => RbxBrickColor::from_name(&s.to_str()?).unwrap_or(RbxBrickColor::MediumStoneGrey),
                Value::Integer(_) | Value::Number(_) if args.len() >= 3 => {
                    let c = Color3 { r: num(&args[0])?, g: num(&args[1])?, b: num(&args[2])? };
                    nearest_brick_color(c.to_u8())
                }
                Value::Integer(_) | Value::Number(_) => {
                    RbxBrickColor::from_number(num(&first)? as u16).unwrap_or(RbxBrickColor::MediumStoneGrey)
                }
                Value::UserData(ud) => nearest_brick_color(ud.borrow::<Color3>()?.to_u8()),
                _ => RbxBrickColor::MediumStoneGrey,
            };
            Ok(BrickColor(color))
        })?,
    )?;
    bc.set(
        "random",
        lua.create_function(|_, ()| {
            let all = all_brick_colors();
            let i = (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0) as usize)
                % all.len();
            Ok(BrickColor(all[i]))
        })?,
    )?;
    for (name, color) in [
        ("White", RbxBrickColor::White),
        ("Gray", RbxBrickColor::MediumStoneGrey),
        ("DarkGray", RbxBrickColor::DarkStoneGrey),
        ("Black", RbxBrickColor::Black),
        ("Red", RbxBrickColor::BrightRed),
        ("Yellow", RbxBrickColor::BrightYellow),
        ("Green", RbxBrickColor::DarkGreen),
        ("Blue", RbxBrickColor::BrightBlue),
    ] {
        bc.set(name, lua.create_function(move |_, ()| Ok(BrickColor(color)))?)?;
    }
    g.set("BrickColor", bc)?;

    let udim = lua.create_table()?;
    udim.set(
        "new",
        lua.create_function(|_, (s, o): (Option<f32>, Option<i32>)| {
            Ok(UDim { scale: s.unwrap_or(0.0), offset: o.unwrap_or(0) })
        })?,
    )?;
    g.set("UDim", udim)?;

    let udim2 = lua.create_table()?;
    udim2.set(
        "new",
        lua.create_function(|_, (xs, xo, ys, yo): (Value, Option<i32>, Option<f32>, Option<i32>)| {
            if let Value::UserData(ud) = &xs {
                // UDim2.new(UDim, UDim) is not valid in Roblox but harmless.
                let x = *ud.borrow::<UDim>()?;
                return Ok(UDim2 { x, y: UDim { scale: 0.0, offset: 0 } });
            }
            let xs = if xs.is_nil() { 0.0 } else { num(&xs)? };
            Ok(UDim2 {
                x: UDim { scale: xs, offset: xo.unwrap_or(0) },
                y: UDim { scale: ys.unwrap_or(0.0), offset: yo.unwrap_or(0) },
            })
        })?,
    )?;
    udim2.set(
        "fromScale",
        lua.create_function(|_, (x, y): (Option<f32>, Option<f32>)| {
            Ok(UDim2 {
                x: UDim { scale: x.unwrap_or(0.0), offset: 0 },
                y: UDim { scale: y.unwrap_or(0.0), offset: 0 },
            })
        })?,
    )?;
    udim2.set(
        "fromOffset",
        lua.create_function(|_, (x, y): (Option<i32>, Option<i32>)| {
            Ok(UDim2 {
                x: UDim { scale: 0.0, offset: x.unwrap_or(0) },
                y: UDim { scale: 0.0, offset: y.unwrap_or(0) },
            })
        })?,
    )?;
    g.set("UDim2", udim2)?;

    let nr = lua.create_table()?;
    nr.set(
        "new",
        lua.create_function(|_, (a, b): (f32, Option<f32>)| Ok(NumberRange { min: a, max: b.unwrap_or(a) }))?,
    )?;
    g.set("NumberRange", nr)?;

    g.set("Enum", Enums)?;
    Ok(())
}
