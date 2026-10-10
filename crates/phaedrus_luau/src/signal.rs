//! `RBXScriptSignal` / `RBXScriptConnection`.

use std::collections::HashMap;

use mlua::{FromLua, Function, Lua, MetaMethod, MultiValue, Result, Table, Thread, UserData, UserDataFields, UserDataMethods, Value};
use phaedrus_dom::types::Ref;

use crate::{ctx, scheduler};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SignalKey {
    /// An event on an instance (`Changed`, `ChildAdded`, ...).
    Instance(Ref, String),
    /// `GetPropertyChangedSignal(name)`.
    Property(Ref, String),
    /// `GetAttributeChangedSignal(name)`.
    Attribute(Ref, String),
    /// A global event such as `RunService.Heartbeat`.
    Global(&'static str),
}

struct Connection {
    id: u64,
    func: Function,
    once: bool,
}

#[derive(Default)]
pub struct Signals {
    next_id: u64,
    connections: HashMap<SignalKey, Vec<Connection>>,
    waiters: HashMap<SignalKey, Vec<Thread>>,
}

impl Signals {
    pub fn has_listeners(&self, key: &SignalKey) -> bool {
        self.connections.get(key).is_some_and(|c| !c.is_empty())
            || self.waiters.get(key).is_some_and(|w| !w.is_empty())
    }

    /// True if anything at all listens on this instance (cheap pre-check
    /// before building event payloads).
    pub fn any_for(&self, r: Ref) -> bool {
        self.connections.keys().chain(self.waiters.keys()).any(|k| match k {
            SignalKey::Instance(x, _) | SignalKey::Property(x, _) | SignalKey::Attribute(x, _) => *x == r,
            SignalKey::Global(_) => false,
        })
    }

    pub fn add_waiter(&mut self, key: SignalKey, thread: Thread) {
        self.waiters.entry(key).or_default().push(thread);
    }

    pub fn clear(&mut self) {
        self.connections.clear();
        self.waiters.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.connections.values().all(|v| v.is_empty()) && self.waiters.values().all(|v| v.is_empty())
    }
}

#[derive(Debug, Clone, FromLua)]
pub struct Signal {
    pub key: SignalKey,
}

impl Signal {
    pub fn new(key: SignalKey) -> Self {
        Self { key }
    }
}

fn connect(lua: &Lua, key: &SignalKey, func: Function, once: bool) -> ScriptConnection {
    let c = ctx(lua);
    let mut s = c.signals.borrow_mut();
    s.next_id += 1;
    let id = s.next_id;
    s.connections.entry(key.clone()).or_default().push(Connection { id, func, once });
    ScriptConnection { key: key.clone(), id }
}

impl UserData for Signal {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_meta_field(MetaMethod::Type, "RBXScriptSignal");
    }

    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("Connect", |lua, s, f: Function| Ok(connect(lua, &s.key, f, false)));
        m.add_method("ConnectParallel", |lua, s, f: Function| Ok(connect(lua, &s.key, f, false)));
        m.add_method("Once", |lua, s, f: Function| Ok(connect(lua, &s.key, f, true)));
        m.add_meta_method(MetaMethod::Index, |lua, _, key: String| {
            if key == "Wait" {
                let lua_methods: Table = lua.named_registry_value("phaedrus.lua_methods")?;
                return lua_methods.get::<Value>("SignalWait");
            }
            Err(mlua::Error::runtime(format!("{key} is not a valid member of RBXScriptSignal")))
        });
        m.add_meta_method(MetaMethod::ToString, |_, s, ()| {
            Ok(match &s.key {
                SignalKey::Instance(_, n) => format!("Signal {n}"),
                SignalKey::Property(_, n) => format!("Signal {n}Changed"),
                SignalKey::Attribute(_, n) => format!("Signal {n}AttributeChanged"),
                SignalKey::Global(n) => format!("Signal {n}"),
            })
        });
    }
}

#[derive(Debug, Clone, FromLua)]
pub struct ScriptConnection {
    key: SignalKey,
    id: u64,
}

impl UserData for ScriptConnection {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_meta_field(MetaMethod::Type, "RBXScriptConnection");
        f.add_field_method_get("Connected", |lua, c| {
            let ctx = ctx(lua);
            let s = ctx.signals.borrow();
            Ok(s.connections.get(&c.key).is_some_and(|v| v.iter().any(|x| x.id == c.id)))
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("Disconnect", |lua, c, ()| {
            let ctx = ctx(lua);
            let mut s = ctx.signals.borrow_mut();
            if let Some(v) = s.connections.get_mut(&c.key) {
                v.retain(|x| x.id != c.id);
            }
            Ok(())
        });
        m.add_meta_method(MetaMethod::ToString, |_, _, ()| Ok("Connection"));
    }
}

/// Fires a signal: each connected function runs in its own new thread, and
/// threads blocked in `:Wait()` resume with the arguments.
pub fn fire(lua: &Lua, key: &SignalKey, args: MultiValue) -> Result<()> {
    let c = ctx(lua);
    let (funcs, waiters) = {
        let mut s = c.signals.borrow_mut();
        let funcs: Vec<Function> = match s.connections.get_mut(key) {
            Some(v) => {
                let f = v.iter().map(|c| c.func.clone()).collect();
                v.retain(|c| !c.once);
                f
            }
            None => Vec::new(),
        };
        let waiters = s.waiters.remove(key).unwrap_or_default();
        (funcs, waiters)
    };
    drop(c);
    for f in funcs {
        scheduler::spawn_function(lua, f, args.clone())?;
    }
    for t in waiters {
        scheduler::resume(lua, t, args.clone());
    }
    Ok(())
}
