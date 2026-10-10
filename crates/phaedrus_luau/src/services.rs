//! Members of specific services that are not plain properties.

use mlua::{IntoLua, Lua, Result, Value};
use phaedrus_dom::types::Ref;

use crate::Ctx;
use crate::instance::instance_value;
use crate::signal::{Signal, SignalKey};

pub fn index(lua: &Lua, c: &Ctx, r: Ref, key: &str) -> Result<Option<Value>> {
    let class = {
        let doc = c.doc.lock();
        match doc.get(r) {
            Some(i) => i.class.to_string(),
            None => return Ok(None),
        }
    };
    let run_mode = c.run_mode;
    Ok(match (class.as_str(), key) {
        ("RunService", "Heartbeat" | "Stepped" | "RenderStepped" | "PreSimulation" | "PostSimulation" | "PreRender" | "PreAnimation") => {
            let name: &'static str = match key {
                "Heartbeat" => "Heartbeat",
                "Stepped" => "Stepped",
                "RenderStepped" => "RenderStepped",
                "PreSimulation" => "PreSimulation",
                "PostSimulation" => "PostSimulation",
                "PreRender" => "PreRender",
                _ => "PreAnimation",
            };
            Some(Signal::new(SignalKey::Global(name)).into_lua(lua)?)
        }
        ("RunService", "IsStudio") => Some(lua.create_function(|_, _: Value| Ok(true))?.into_lua(lua)?),
        ("RunService", "IsServer") => Some(lua.create_function(|_, _: Value| Ok(true))?.into_lua(lua)?),
        ("RunService", "IsClient") => Some(lua.create_function(|_, _: Value| Ok(false))?.into_lua(lua)?),
        ("RunService", "IsRunning" | "IsRunMode") => {
            Some(lua.create_function(move |_, _: Value| Ok(run_mode))?.into_lua(lua)?)
        }
        ("RunService", "IsEdit") => Some(lua.create_function(move |_, _: Value| Ok(!run_mode))?.into_lua(lua)?),
        ("Players", "LocalPlayer") => Some(Value::Nil),
        ("Players", "GetPlayers") => Some(lua.create_function(|lua, _: Value| lua.create_table())?.into_lua(lua)?),
        ("Players", "PlayerAdded" | "PlayerRemoving") => {
            Some(Signal::new(SignalKey::Instance(r, key.to_owned())).into_lua(lua)?)
        }
        ("Workspace", "CurrentCamera") => {
            let cam = c.doc.lock().find_first_child_of_class(r, "Camera");
            Some(cam.map_or(Ok(Value::Nil), |cam| instance_value(lua, cam))?)
        }
        ("Workspace", "Terrain") => {
            let t = c.doc.lock().find_first_child_of_class(r, "Terrain");
            Some(t.map_or(Ok(Value::Nil), |t| instance_value(lua, t))?)
        }
        ("Workspace", "GetServerTimeNow") => {
            Some(lua.create_function(|_, _: Value| {
                Ok(std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0))
            })?.into_lua(lua)?)
        }
        ("DataStoreService", _) => crate::datastore::service_member(lua, key)?,
        ("DataModel", "PlaceId" | "GameId" | "CreatorId") => Some(Value::Integer(0)),
        ("DataModel", "JobId") => Some("".into_lua(lua)?),
        _ => None,
    })
}
