//! The `Instance` userdata.

use mlua::{
    AnyUserData, FromLua, IntoLua, Lua, MetaMethod, MultiValue, Result, Table, UserData,
    UserDataFields, UserDataMethods, Value,
};
use phaedrus_dom::document::new_instance;
use phaedrus_dom::reflection;
use phaedrus_dom::types::{Attributes, Ref, Variant};

use crate::convert::{lua_to_attribute, lua_to_variant, variant_to_lua};
use crate::signal::{Signal, SignalKey};
use crate::types::{CFrame, Vector3};
use crate::ctx;

/// A handle to an instance in the shared document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromLua)]
pub struct LInstance(pub Ref);

/// Returns the cached userdata for `r` so the same instance is always the
/// same Lua value (needed for table keys and `rawequal`).
pub fn instance_value(lua: &Lua, r: Ref) -> Result<Value> {
    if r.is_none() {
        return Ok(Value::Nil);
    }
    let cache: Table = lua.named_registry_value("phaedrus.instances")?;
    let key = r.to_string();
    if let Value::UserData(ud) = cache.raw_get::<Value>(key.as_str())? {
        return Ok(Value::UserData(ud));
    }
    let ud = lua.create_userdata(LInstance(r))?;
    cache.raw_set(key, &ud)?;
    Ok(Value::UserData(ud))
}

pub fn create_instance_cache(lua: &Lua) -> Result<()> {
    let cache = lua.create_table()?;
    let mt = lua.create_table()?;
    mt.set("__mode", "v")?;
    cache.set_metatable(Some(mt))?;
    lua.set_named_registry_value("phaedrus.instances", cache)
}

fn err(msg: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(msg.into())
}

/// Instance methods implemented in Rust. Looked up by `__index`.
const RUST_METHODS: &[&str] = &[
    "FindFirstChild",
    "FindFirstChildOfClass",
    "FindFirstChildWhichIsA",
    "FindFirstAncestor",
    "FindFirstAncestorOfClass",
    "FindFirstAncestorWhichIsA",
    "FindFirstDescendant",
    "GetChildren",
    "GetDescendants",
    "IsA",
    "IsDescendantOf",
    "IsAncestorOf",
    "Clone",
    "Destroy",
    "Remove",
    "ClearAllChildren",
    "GetFullName",
    "GetAttribute",
    "SetAttribute",
    "GetAttributes",
    "GetPropertyChangedSignal",
    "GetAttributeChangedSignal",
    "GetService",
    "FindService",
    "GetPivot",
    "PivotTo",
    "MoveTo",
    "GetBoundingBox",
    "GetExtentsSize",
    "GetTags",
    "HasTag",
    "AddTag",
    "RemoveTag",
    "GetDebugId",
];

/// Events every instance exposes.
const EVENTS: &[&str] = &[
    "Changed",
    "ChildAdded",
    "ChildRemoved",
    "DescendantAdded",
    "DescendantRemoving",
    "AncestryChanged",
    "Destroying",
    "AttributeChanged",
];

impl UserData for LInstance {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_meta_field(MetaMethod::Type, "Instance");
    }

    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_meta_method(MetaMethod::Index, |lua, this, key: String| index(lua, this.0, &key));
        m.add_meta_method(MetaMethod::NewIndex, |lua, this, (key, value): (String, Value)| {
            new_index(lua, this.0, &key, value)
        });
        m.add_meta_method(MetaMethod::ToString, |lua, this, ()| {
            let c = ctx(lua);
            let doc = c.doc.lock();
            Ok(doc.get(this.0).map(|i| i.name.clone()).unwrap_or_default())
        });
        m.add_meta_method(MetaMethod::Eq, |_, a, b: LInstance| Ok(a.0 == b.0));
    }
}

fn index(lua: &Lua, r: Ref, key: &str) -> Result<Value> {
    let c = ctx(lua);
    // Properties first.
    {
        let doc = c.doc.lock();
        let inst = doc.get(r).ok_or_else(|| err("instance no longer exists"))?;
        let class = inst.class.as_str();
        let is_property = matches!(key, "Name" | "ClassName" | "Parent")
            || reflection::find_property(class, key).is_some()
            || (key == "Position" || key == "Orientation" || key == "BrickColor")
                && reflection::is_a(class, "BasePart");
        if is_property && !EVENTS.contains(&key) {
            let value = doc.get_property(r, key);
            let enum_name = reflection::property_enum(class, key);
            drop(doc);
            return match value {
                Some(v) => variant_to_lua(lua, &v, enum_name),
                None => Ok(Value::Nil),
            };
        }
    }
    if RUST_METHODS.contains(&key) {
        let methods: Table = lua.named_registry_value("phaedrus.methods")?;
        return methods.get(key);
    }
    // Methods implemented in the Luau prelude (yielding ones like WaitForChild).
    let lua_methods: Table = lua.named_registry_value("phaedrus.lua_methods")?;
    if let Value::Function(f) = lua_methods.get::<Value>(key)? {
        return Ok(Value::Function(f));
    }
    if EVENTS.contains(&key) {
        return Signal::new(SignalKey::Instance(r, key.to_owned())).into_lua(lua);
    }
    // Service-specific members.
    if let Some(v) = crate::services::index(lua, &c, r, key)? {
        return Ok(v);
    }
    let doc = c.doc.lock();
    if let Some(child) = doc.find_first_child(r, key) {
        drop(doc);
        return instance_value(lua, child);
    }
    let class = doc.get(r).map(|i| i.class.to_string()).unwrap_or_default();
    let path = doc.full_name(r);
    Err(err(format!("{key} is not a valid member of {class} \"{path}\"")))
}

fn new_index(lua: &Lua, r: Ref, key: &str, value: Value) -> Result<()> {
    let c = ctx(lua);
    if key == "Parent" && c.destroyed.borrow().contains(&r) {
        return Err(err(format!(
            "The Parent property of {} is locked, current parent: NULL, new parent {}",
            c.doc.lock().get(r).map(|i| i.name.clone()).unwrap_or_default(),
            crate::convert::lua_type_name(&value)
        )));
    }
    let mut doc = c.doc.lock();
    let inst = doc.get(r).ok_or_else(|| err("instance no longer exists"))?;
    let class = inst.class.to_string();
    if let Some(desc) = reflection::find_property(&class, key)
        && matches!(desc.scriptability, reflection::rbx_reflection::Scriptability::Read)
            && !matches!(key, "Name" | "Parent")
        {
            return Err(err(format!("Unable to assign property {key}. Property is read only")));
        }
    let variant = match key {
        "Name" => match &value {
            Value::String(s) => Variant::String(s.to_str()?.to_owned()),
            Value::Integer(_) | Value::Number(_) => Variant::String(value.to_string()?),
            _ => return Err(err("Unable to assign property Name. string expected")),
        },
        "Parent" => match &value {
            Value::Nil => Variant::Ref(Ref::none()),
            Value::UserData(ud) if ud.is::<LInstance>() => Variant::Ref(ud.borrow::<LInstance>()?.0),
            _ => return Err(err("Unable to assign property Parent. Instance expected")),
        },
        "Position" | "Orientation" | "Rotation" if reflection::is_a(&class, "BasePart") => match &value {
            Value::UserData(ud) if ud.is::<Vector3>() => Variant::Vector3(ud.borrow::<Vector3>()?.to_rbx()),
            _ => return Err(err(format!("Unable to assign property {key}. Vector3 expected"))),
        },
        "BrickColor" => match &value {
            Value::UserData(ud) if ud.is::<crate::types::BrickColor>() => {
                Variant::BrickColor(ud.borrow::<crate::types::BrickColor>()?.0)
            }
            _ => return Err(err("Unable to assign property BrickColor. BrickColor expected")),
        },
        _ => {
            if reflection::find_property(&class, key).is_none() {
                let path = doc.full_name(r);
                return Err(err(format!("{key} is not a valid member of {class} \"{path}\"")));
            }
            let current = doc.get_property(r, key);
            lua_to_variant(&class, key, &value, current.as_ref()).map_err(err)?
        }
    };
    doc.set_property(r, key, variant).map_err(err)
}

/// Registers the Rust-implemented instance methods table.
pub fn register_methods(lua: &Lua) -> Result<()> {
    let t = lua.create_table()?;
    let this = |v: &AnyUserData| -> Result<Ref> { Ok(v.borrow::<LInstance>()?.0) };

    t.set("FindFirstChild", lua.create_function(move |lua, (s, name, recursive): (AnyUserData, String, Option<bool>)| {
        let r = this(&s)?;
        let c = ctx(lua);
        let found = {
            let doc = c.doc.lock();
            if recursive.unwrap_or(false) {
                doc.descendants(r).into_iter().find(|d| doc.get(*d).is_some_and(|i| i.name == name))
            } else {
                doc.find_first_child(r, &name)
            }
        };
        found.map_or(Ok(Value::Nil), |f| instance_value(lua, f))
    })?)?;

    fn find_child_by(
        lua: &Lua,
        s: &AnyUserData,
        recursive: bool,
        pred: impl Fn(&str) -> bool,
    ) -> Result<Value> {
        let r = s.borrow::<LInstance>()?.0;
        let c = ctx(lua);
        let found = {
            let doc = c.doc.lock();
            let list = if recursive { doc.descendants(r) } else { doc.children(r).to_vec() };
            list.into_iter().find(|d| doc.get(*d).is_some_and(|i| pred(&i.class)))
        };
        found.map_or(Ok(Value::Nil), |f| instance_value(lua, f))
    }
    t.set("FindFirstChildOfClass", lua.create_function(|lua, (s, class): (AnyUserData, String)| {
        find_child_by(lua, &s, false, |c| c == class)
    })?)?;
    t.set("FindFirstChildWhichIsA", lua.create_function(|lua, (s, class, rec): (AnyUserData, String, Option<bool>)| {
        find_child_by(lua, &s, rec.unwrap_or(false), |c| reflection::is_a(c, &class))
    })?)?;
    t.set("FindFirstDescendant", lua.create_function(move |lua, (s, name): (AnyUserData, String)| {
        let r = this(&s)?;
        let c = ctx(lua);
        let found = {
            let doc = c.doc.lock();
            doc.descendants(r).into_iter().find(|d| doc.get(*d).is_some_and(|i| i.name == name))
        };
        found.map_or(Ok(Value::Nil), |f| instance_value(lua, f))
    })?)?;

    fn find_ancestor(lua: &Lua, s: &AnyUserData, pred: impl Fn(&str, &str) -> bool) -> Result<Value> {
        let r = s.borrow::<LInstance>()?.0;
        let c = ctx(lua);
        let found = {
            let doc = c.doc.lock();
            let mut cur = doc.parent(r);
            let mut found = None;
            while cur.is_some() {
                if let Some(i) = doc.get(cur)
                    && pred(&i.name, &i.class) {
                        found = Some(cur);
                        break;
                    }
                cur = doc.parent(cur);
            }
            found
        };
        found.map_or(Ok(Value::Nil), |f| instance_value(lua, f))
    }
    t.set("FindFirstAncestor", lua.create_function(|lua, (s, name): (AnyUserData, String)| {
        find_ancestor(lua, &s, |n, _| n == name)
    })?)?;
    t.set("FindFirstAncestorOfClass", lua.create_function(|lua, (s, class): (AnyUserData, String)| {
        find_ancestor(lua, &s, |_, c| c == class)
    })?)?;
    t.set("FindFirstAncestorWhichIsA", lua.create_function(|lua, (s, class): (AnyUserData, String)| {
        find_ancestor(lua, &s, |_, c| reflection::is_a(c, &class))
    })?)?;

    t.set("GetChildren", lua.create_function(move |lua, s: AnyUserData| {
        let r = this(&s)?;
        let kids = ctx(lua).doc.lock().children(r).to_vec();
        let out = lua.create_table_with_capacity(kids.len(), 0)?;
        for k in kids {
            out.push(instance_value(lua, k)?)?;
        }
        Ok(out)
    })?)?;
    t.set("GetDescendants", lua.create_function(move |lua, s: AnyUserData| {
        let r = this(&s)?;
        let all = ctx(lua).doc.lock().descendants(r);
        let out = lua.create_table_with_capacity(all.len(), 0)?;
        for k in all {
            out.push(instance_value(lua, k)?)?;
        }
        Ok(out)
    })?)?;
    t.set("IsA", lua.create_function(move |lua, (s, class): (AnyUserData, String)| {
        Ok(ctx(lua).doc.lock().is_a(this(&s)?, &class))
    })?)?;
    t.set("IsDescendantOf", lua.create_function(move |lua, (s, other): (AnyUserData, Option<LInstance>)| {
        let r = this(&s)?;
        let Some(o) = other else { return Ok(false) };
        Ok(r != o.0 && ctx(lua).doc.lock().is_descendant_of(r, o.0))
    })?)?;
    t.set("IsAncestorOf", lua.create_function(move |lua, (s, other): (AnyUserData, Option<LInstance>)| {
        let r = this(&s)?;
        let Some(o) = other else { return Ok(false) };
        Ok(r != o.0 && ctx(lua).doc.lock().is_descendant_of(o.0, r))
    })?)?;
    t.set("Clone", lua.create_function(move |lua, s: AnyUserData| {
        let r = this(&s)?;
        let new = ctx(lua).doc.lock().clone_instance(r);
        new.map_or(Ok(Value::Nil), |n| instance_value(lua, n))
    })?)?;
    let destroy = lua.create_function(move |lua, s: AnyUserData| {
        let r = this(&s)?;
        destroy_instance(lua, r)
    })?;
    t.set("Destroy", destroy.clone())?;
    t.set("Remove", lua.create_function(move |lua, s: AnyUserData| {
        let r = this(&s)?;
        ctx(lua).doc.lock().set_parent(r, Ref::none()).map_err(err)
    })?)?;
    t.set("ClearAllChildren", lua.create_function(move |lua, s: AnyUserData| {
        let r = this(&s)?;
        let kids = ctx(lua).doc.lock().children(r).to_vec();
        for k in kids {
            destroy_instance(lua, k)?;
        }
        Ok(())
    })?)?;
    t.set("GetFullName", lua.create_function(move |lua, s: AnyUserData| {
        Ok(ctx(lua).doc.lock().full_name(this(&s)?))
    })?)?;
    t.set("GetDebugId", lua.create_function(move |_, s: AnyUserData| Ok(this(&s)?.to_string()))?)?;

    t.set("GetAttribute", lua.create_function(move |lua, (s, name): (AnyUserData, String)| {
        let r = this(&s)?;
        let v = match ctx(lua).doc.lock().get_raw(r, "Attributes") {
            Some(Variant::Attributes(a)) => a.get(name.as_str()).cloned(),
            _ => None,
        };
        v.map_or(Ok(Value::Nil), |v| variant_to_lua(lua, &v, None))
    })?)?;
    t.set("GetAttributes", lua.create_function(move |lua, s: AnyUserData| {
        let r = this(&s)?;
        let out = lua.create_table()?;
        if let Some(Variant::Attributes(a)) = ctx(lua).doc.lock().get_raw(r, "Attributes") {
            for (k, v) in a.iter() {
                out.set(k.as_str(), variant_to_lua(lua, v, None)?)?;
            }
        }
        Ok(out)
    })?)?;
    t.set("SetAttribute", lua.create_function(move |lua, (s, name, value): (AnyUserData, String, Value)| {
        let r = this(&s)?;
        let c = ctx(lua);
        let mut doc = c.doc.lock();
        let mut attrs = match doc.get_raw(r, "Attributes") {
            Some(Variant::Attributes(a)) => a,
            _ => Attributes::new(),
        };
        if value.is_nil() {
            attrs.remove(name.as_str());
        } else {
            let v = lua_to_attribute(&value).ok_or_else(|| err("unsupported attribute type"))?;
            attrs.insert(name.clone(), v);
        }
        doc.write_raw(r, "Attributes", Variant::Attributes(attrs));
        drop(doc);
        c.pending_attr_changes.borrow_mut().push((r, name));
        Ok(())
    })?)?;
    t.set("GetPropertyChangedSignal", lua.create_function(move |_, (s, prop): (AnyUserData, String)| {
        Ok(Signal::new(SignalKey::Property(this(&s)?, prop)))
    })?)?;
    t.set("GetAttributeChangedSignal", lua.create_function(move |_, (s, prop): (AnyUserData, String)| {
        Ok(Signal::new(SignalKey::Attribute(this(&s)?, prop)))
    })?)?;
    let get_service = lua.create_function(move |lua, (s, name): (AnyUserData, String)| {
        let r = this(&s)?;
        let c = ctx(lua);
        let found = {
            let mut doc = c.doc.lock();
            if r != doc.root() {
                return Err(err("GetService can only be called on game"));
            }
            match doc.service(&name) {
                Some(s) => Some(s),
                None if reflection::is_service(&name) => Some(doc.create(&name, r)),
                None => None,
            }
        };
        match found {
            Some(f) => instance_value(lua, f),
            None => Err(err(format!("'{name}' is not a valid Service name"))),
        }
    })?;
    t.set("GetService", get_service.clone())?;
    t.set("FindService", lua.create_function(move |lua, (s, name): (AnyUserData, String)| {
        let _ = this(&s)?;
        let found = ctx(lua).doc.lock().service(&name);
        found.map_or(Ok(Value::Nil), |f| instance_value(lua, f))
    })?)?;
    t.set("GetPivot", lua.create_function(move |lua, s: AnyUserData| {
        let r = this(&s)?;
        let c = ctx(lua);
        let doc = c.doc.lock();
        let cf = match doc.get_property(r, "CFrame") {
            Some(Variant::CFrame(c)) if !doc.is_a(r, "Model") => c,
            _ => doc.model_pivot(r),
        };
        Ok(CFrame::from_rbx(&cf))
    })?)?;
    t.set("PivotTo", lua.create_function(move |lua, (s, cf): (AnyUserData, CFrame)| {
        let r = this(&s)?;
        ctx(lua).doc.lock().set_cframe(r, cf.to_rbx()).map_err(err)
    })?)?;
    t.set("MoveTo", lua.create_function(move |lua, (s, pos): (AnyUserData, Vector3)| {
        let r = this(&s)?;
        let c = ctx(lua);
        let mut doc = c.doc.lock();
        let mut pivot = doc.model_pivot(r);
        pivot.position = pos.to_rbx();
        doc.set_cframe(r, pivot).map_err(err)
    })?)?;
    t.set("GetBoundingBox", lua.create_function(move |lua, s: AnyUserData| {
        let r = this(&s)?;
        let c = ctx(lua);
        let doc = c.doc.lock();
        let (min, max) = doc.bounds(r).ok_or_else(|| err("model has no parts"))?;
        let min = Vector3::from_rbx(min).0;
        let max = Vector3::from_rbx(max).0;
        Ok((CFrame::from_pos((min + max) * 0.5), Vector3(max - min)))
    })?)?;
    t.set("GetExtentsSize", lua.create_function(move |lua, s: AnyUserData| {
        let r = this(&s)?;
        let c = ctx(lua);
        let doc = c.doc.lock();
        let (min, max) = doc.bounds(r).ok_or_else(|| err("model has no parts"))?;
        Ok(Vector3(Vector3::from_rbx(max).0 - Vector3::from_rbx(min).0))
    })?)?;
    t.set("GetTags", lua.create_function(move |lua, s: AnyUserData| {
        let r = this(&s)?;
        let tags: Vec<String> = match ctx(lua).doc.lock().get_raw(r, "Tags") {
            Some(Variant::Tags(t)) => t.iter().map(|s| s.to_owned()).collect(),
            _ => Vec::new(),
        };
        Ok(tags)
    })?)?;
    t.set("HasTag", lua.create_function(move |lua, (s, tag): (AnyUserData, String)| {
        let r = this(&s)?;
        Ok(matches!(ctx(lua).doc.lock().get_raw(r, "Tags"), Some(Variant::Tags(t)) if t.iter().any(|x| x == tag)))
    })?)?;
    t.set("AddTag", lua.create_function(move |lua, (s, tag): (AnyUserData, String)| {
        let r = this(&s)?;
        let c = ctx(lua);
        let mut doc = c.doc.lock();
        let mut tags = match doc.get_raw(r, "Tags") {
            Some(Variant::Tags(t)) => t,
            _ => Default::default(),
        };
        if !tags.iter().any(|x| x == tag) {
            tags.push(&tag);
            doc.write_raw(r, "Tags", Variant::Tags(tags));
        }
        Ok(())
    })?)?;
    t.set("RemoveTag", lua.create_function(move |lua, (s, tag): (AnyUserData, String)| {
        let r = this(&s)?;
        let c = ctx(lua);
        let mut doc = c.doc.lock();
        if let Some(Variant::Tags(t)) = doc.get_raw(r, "Tags") {
            let kept: Vec<String> = t.iter().filter(|x| *x != tag).map(|s| s.to_owned()).collect();
            doc.write_raw(r, "Tags", Variant::Tags(kept.into()));
        }
        Ok(())
    })?)?;
    lua.set_named_registry_value("phaedrus.methods", t)?;

    // Instance.new
    let instance_tbl = lua.create_table()?;
    instance_tbl.set("new", lua.create_function(|lua, (class, parent): (String, Option<LInstance>)| {
        if crate::datastore::OPTION_CLASSES.contains(&class.as_str()) {
            let new_option: mlua::Function = lua.named_registry_value("phaedrus.datastore.new_option")?;
            return new_option.call::<Value>(class);
        }
        let Some(desc) = reflection::class(&class) else {
            return Err(err(format!("Unable to create an Instance of type \"{class}\"")));
        };
        if desc.tags.contains(&reflection::rbx_reflection::ClassTag::NotCreatable)
            || desc.tags.contains(&reflection::rbx_reflection::ClassTag::Service)
        {
            return Err(err(format!("Unable to create an Instance of type \"{class}\"")));
        }
        let c = ctx(lua);
        let r = {
            let mut doc = c.doc.lock();
            let r = doc.insert(Ref::none(), new_instance(&class));
            if let Some(p) = parent {
                doc.set_parent(r, p.0).map_err(err)?;
            }
            r
        };
        instance_value(lua, r)
    })?)?;
    lua.globals().set("Instance", instance_tbl)?;
    Ok(())
}

/// `Instance:Destroy()`: fires `Destroying`, detaches, and locks `Parent`.
pub fn destroy_instance(lua: &Lua, r: Ref) -> Result<()> {
    let c = ctx(lua);
    let subtree = {
        let doc = c.doc.lock();
        if !doc.exists(r) {
            return Ok(());
        }
        let mut v = vec![r];
        v.extend(doc.descendants(r));
        v
    };
    for d in &subtree {
        crate::signal::fire(lua, &SignalKey::Instance(*d, "Destroying".into()), MultiValue::new())?;
    }
    c.doc.lock().set_parent(r, Ref::none()).map_err(err)?;
    c.destroyed.borrow_mut().extend(subtree);
    Ok(())
}
