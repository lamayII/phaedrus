//! Roblox datatypes exposed to Luau.

pub mod cframe;
pub mod misc;
pub mod vector;

use mlua::{Lua, Result, Table, Value};

pub use cframe::CFrame;
pub use misc::{BrickColor, Color3, EnumItem, EnumType, NumberRange, UDim, UDim2};
pub use vector::{Vector2, Vector3};

/// Reads a Lua number (or numeric string) as f32.
pub fn num(v: &Value) -> Result<f32> {
    match v {
        Value::Integer(i) => Ok(*i as f32),
        Value::Number(n) => Ok(*n as f32),
        Value::String(s) => s
            .to_str()?
            .trim()
            .parse()
            .map_err(|_| mlua::Error::runtime("expected number")),
        _ => Err(mlua::Error::runtime(format!("expected number, got {}", v.type_name()))),
    }
}

pub fn register(lua: &Lua, globals: &Table) -> Result<()> {
    vector::register(lua, globals)?;
    cframe::register(lua, globals)?;
    misc::register(lua, globals)?;
    Ok(())
}
