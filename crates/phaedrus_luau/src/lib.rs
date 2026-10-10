//! Luau VM for Phaedrus.
//!
//! Runs Roblox scripts against the shared [`Document`]. The VM exposes a
//! subset of the Roblox API: `game`, `workspace`, `script`, `Instance`, the
//! common datatypes (`Vector3`, `CFrame`, `Color3`, ...), `Enum`, signals and
//! the `task` library. Scripts run as coroutines on a cooperative scheduler
//! that the host steps once per frame.

pub mod convert;
pub mod datastore;
pub mod instance;
mod scheduler;
mod services;
pub mod signal;
pub mod types;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use mlua::{AppDataRef, Function, IntoLua, Lua, LuaOptions, MultiValue, StdLib, Table, Value, Variadic, VmState};
use parking_lot::Mutex;
use phaedrus_dom::reflection;
use phaedrus_dom::types::{Ref, Variant};
use phaedrus_dom::{Change, Document};

pub use crate::datastore::{DataStoreConfig, DataStoreEmulator, SharedDataStore};
use crate::instance::instance_value;
use crate::signal::{SignalKey, Signals};

pub use mlua;

/// The document shared between the editor and the VM.
pub type SharedDoc = Arc<Mutex<Document>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct OutputLine {
    pub level: OutputLevel,
    pub text: String,
    pub time: SystemTime,
    /// Script that produced the line, if any.
    pub source: Option<String>,
}

pub type SharedOutput = Arc<Mutex<Vec<OutputLine>>>;

/// Maximum time a script may run without yielding.
const SCRIPT_TIMEOUT_SECS: f64 = 10.0;

/// Per-VM state reachable from every Lua callback through app data.
pub struct Ctx {
    pub doc: SharedDoc,
    pub output: SharedOutput,
    pub(crate) sched: RefCell<scheduler::Scheduler>,
    pub(crate) signals: RefCell<Signals>,
    pub(crate) destroyed: RefCell<HashSet<Ref>>,
    pub(crate) pending_attr_changes: RefCell<Vec<(Ref, String)>>,
    modules: RefCell<HashMap<Ref, Value>>,
    start: Instant,
    pub(crate) resume_started: Cell<Option<Instant>>,
    pub(crate) wait_sentinel: Table,
    pub(crate) signal_sentinel: Table,
    pub(crate) run_mode: bool,
    /// The DataStoreService emulator, if the host provides one.
    pub(crate) datastore: RefCell<Option<SharedDataStore>>,
}

impl Ctx {
    pub fn clock(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    pub fn print(&self, level: OutputLevel, text: String) {
        self.output.lock().push(OutputLine { level, text, time: SystemTime::now(), source: None });
    }
}

pub(crate) fn ctx(lua: &Lua) -> AppDataRef<'_, Ctx> {
    lua.app_data_ref::<Ctx>().expect("phaedrus Ctx missing")
}

const PRELUDE: &str = r#"
local WAIT, SIGNALWAIT, clock, spawn_fn, defer_fn, cancel_fn = ...
local yield = coroutine.yield

local task = {}
function task.wait(t)
    return yield(WAIT, tonumber(t) or 0)
end
task.spawn = spawn_fn
task.defer = defer_fn
task.cancel = cancel_fn
function task.delay(t, f, ...)
    local args = table.pack(...)
    return spawn_fn(function()
        task.wait(t)
        if type(f) == "function" then
            f(table.unpack(args, 1, args.n))
        else
            coroutine.resume(f, table.unpack(args, 1, args.n))
        end
    end)
end
task.synchronize = function() end
task.desynchronize = function() end

local function legacy_wait(t)
    local elapsed = yield(WAIT, math.max(tonumber(t) or 0, 0.03))
    return elapsed, clock()
end

local lua_methods = {}

function lua_methods.WaitForChild(self, name, timeout)
    local child = self:FindFirstChild(name)
    local waited = 0
    local warned = false
    while not child do
        waited += task.wait()
        child = self:FindFirstChild(name)
        if timeout and waited >= timeout then
            return nil
        end
        if not timeout and not warned and waited > 5 then
            warned = true
            warn(`Infinite yield possible on '{self:GetFullName()}:WaitForChild("{name}")'`)
        end
    end
    return child
end

function lua_methods.SignalWait(self)
    return yield(SIGNALWAIT, self)
end

return task, legacy_wait, lua_methods
"#;

/// A Luau VM bound to a document.
pub struct Runtime {
    lua: Lua,
    run_mode: bool,
}

impl Runtime {
    /// Creates a VM. `run_mode` is true while play-testing (affects
    /// `RunService:IsRunning()`).
    pub fn new(doc: SharedDoc, output: SharedOutput, run_mode: bool) -> mlua::Result<Self> {
        let lua = Lua::new_with(
            StdLib::ALL_SAFE,
            LuaOptions::default(),
        )?;
        lua.set_compiler(mlua::chunk::Compiler::new().set_optimization_level(1).set_debug_level(1));

        let wait_sentinel = lua.create_table()?;
        let signal_sentinel = lua.create_table()?;
        lua.set_app_data(Ctx {
            doc,
            output,
            sched: Default::default(),
            signals: Default::default(),
            destroyed: Default::default(),
            pending_attr_changes: Default::default(),
            modules: Default::default(),
            start: Instant::now(),
            resume_started: Cell::new(None),
            wait_sentinel: wait_sentinel.clone(),
            signal_sentinel: signal_sentinel.clone(),
            run_mode,
            datastore: RefCell::new(None),
        });

        // Abort scripts that run too long without yielding.
        lua.set_interrupt(|lua| {
            let c = ctx(lua);
            if let Some(start) = c.resume_started.get()
                && start.elapsed().as_secs_f64() > SCRIPT_TIMEOUT_SECS {
                    c.resume_started.set(Some(Instant::now()));
                    return Err(mlua::Error::runtime("Script timeout: exhausted allowed execution time"));
                }
            Ok(VmState::Continue)
        });

        instance::create_instance_cache(&lua)?;
        instance::register_methods(&lua)?;
        let globals = lua.globals();
        types::register(&lua, &globals)?;
        register_globals(&lua, &globals)?;

        let clock = lua.create_function(|lua, ()| Ok(ctx(lua).clock()))?;
        let spawn_fn = lua.create_function(|lua, (f, args): (Value, Variadic<Value>)| {
            let args = MultiValue::from_iter(args);
            match f {
                Value::Function(f) => scheduler::spawn_function(lua, f, args),
                Value::Thread(t) => {
                    scheduler::resume(lua, t.clone(), args);
                    Ok(t)
                }
                _ => Err(mlua::Error::runtime("task.spawn expects a function or thread")),
            }
        })?;
        let defer_fn = lua.create_function(|lua, (f, args): (Value, Variadic<Value>)| {
            let thread = match f {
                Value::Function(f) => lua.create_thread(f)?,
                Value::Thread(t) => t,
                _ => return Err(mlua::Error::runtime("task.defer expects a function or thread")),
            };
            ctx(lua).sched.borrow_mut().defer(thread.clone(), MultiValue::from_iter(args));
            Ok(thread)
        })?;
        let cancel_fn = lua.create_function(|lua, t: mlua::Thread| {
            ctx(lua).sched.borrow_mut().cancel(&t);
            Ok(())
        })?;
        let (task, legacy_wait, lua_methods): (Table, Function, Table) = lua
            .load(PRELUDE)
            .set_name("=phaedrus.prelude")
            .call((wait_sentinel, signal_sentinel, clock.clone(), spawn_fn.clone(), defer_fn.clone(), cancel_fn))?;
        globals.set("task", task.clone())?;
        globals.set("wait", legacy_wait)?;
        globals.set("delay", task.get::<Function>("delay")?)?;
        globals.set("spawn", defer_fn)?;
        globals.set("tick", lua.create_function(|_, ()| {
            Ok(SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0))
        })?)?;
        globals.set("time", clock.clone())?;
        globals.set("elapsedTime", clock)?;
        lua.set_named_registry_value("phaedrus.lua_methods", lua_methods)?;
        datastore::register(&lua, &task)?;

        let root = ctx(&lua).doc.lock().root();
        let game = instance_value(&lua, root)?;
        globals.set("game", game.clone())?;
        globals.set("Game", game)?;
        let ws = ctx(&lua).doc.lock().workspace();
        if let Some(ws) = ws {
            let ws = instance_value(&lua, ws)?;
            globals.set("workspace", ws.clone())?;
            globals.set("Workspace", ws)?;
        }
        drop(globals);
        Ok(Self { lua, run_mode })
    }

    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    /// Binds `DataStoreService` to `emulator` (while the emulator is
    /// enabled). Without one, data store calls fail as in an unpublished
    /// place.
    pub fn set_datastore(&self, emulator: Option<SharedDataStore>) {
        *ctx(&self.lua).datastore.borrow_mut() = emulator;
    }

    pub fn is_run_mode(&self) -> bool {
        self.run_mode
    }

    fn environment(&self, script: Option<Ref>) -> mlua::Result<Table> {
        let env = self.lua.create_table()?;
        let mt = self.lua.create_table()?;
        mt.set("__index", self.lua.globals())?;
        env.set_metatable(Some(mt))?;
        env.set("script", match script {
            Some(r) => instance_value(&self.lua, r)?,
            None => Value::Nil,
        })?;
        Ok(env)
    }

    /// Executes source as the command bar does (`script` is nil). The code
    /// runs as a thread and may yield.
    pub fn exec(&self, source: &str, chunk_name: &str) {
        let f = self
            .lua
            .load(source)
            .set_name(format!("={chunk_name}"))
            .set_environment(match self.environment(None) {
                Ok(e) => e,
                Err(e) => return self.report(e),
            })
            .into_function();
        match f {
            Ok(f) => {
                if let Err(e) = scheduler::spawn_function(&self.lua, f, ()) {
                    self.report(e);
                }
            }
            Err(e) => self.report(e),
        }
    }

    /// Starts a Script/LocalScript instance.
    pub fn run_script(&self, r: Ref) {
        let (source, name) = {
            let c = ctx(&self.lua);
            let doc = c.doc.lock();
            let source = match doc.get_property(r, "Source") {
                Some(Variant::String(s)) => s,
                _ => return,
            };
            (source, doc.full_name(r))
        };
        let env = match self.environment(Some(r)) {
            Ok(e) => e,
            Err(e) => return self.report(e),
        };
        match self.lua.load(&source).set_name(format!("={name}")).set_environment(env).into_function() {
            Ok(f) => {
                if let Err(e) = scheduler::spawn_function(&self.lua, f, ()) {
                    self.report(e);
                }
            }
            Err(e) => self.report(e),
        }
    }

    /// Runs every enabled server Script in the containers Roblox runs them
    /// from (Workspace and ServerScriptService), in tree order.
    pub fn run_server_scripts(&self) {
        let scripts = {
            let c = ctx(&self.lua);
            let doc = c.doc.lock();
            let mut out = Vec::new();
            for service in ["Workspace", "ServerScriptService"] {
                let Some(s) = doc.service(service) else { continue };
                for d in doc.descendants(s) {
                    let Some(inst) = doc.get(d) else { continue };
                    if inst.class != "Script" {
                        continue;
                    }
                    let disabled = matches!(doc.get_property(d, "Disabled"), Some(Variant::Bool(true)))
                        || matches!(doc.get_property(d, "Enabled"), Some(Variant::Bool(false)));
                    // RunContext.Client (2) scripts are client-only.
                    let client_only = matches!(doc.get_property(d, "RunContext"), Some(Variant::Enum(e)) if e.to_u32() == 2);
                    if !disabled && !client_only {
                        out.push(d);
                    }
                }
            }
            out
        };
        for s in scripts {
            self.run_script(s);
        }
    }

    /// Advances the VM by one frame: dispatches document change events,
    /// fires RunService signals and resumes waiting threads.
    pub fn step(&self, dt: f64, changes: &[Change]) {
        self.dispatch_changes(changes);
        let now = ctx(&self.lua).clock();
        let fire = |name: &'static str, args: MultiValue| {
            if let Err(e) = signal::fire(&self.lua, &SignalKey::Global(name), args) {
                self.report(e);
            }
        };
        let has = |name: &'static str| ctx(&self.lua).signals.borrow().has_listeners(&SignalKey::Global(name));
        if has("Stepped") {
            fire("Stepped", (now, dt).into_lua_multi_or_empty(&self.lua));
        }
        if has("PreSimulation") {
            fire("PreSimulation", (dt,).into_lua_multi_or_empty(&self.lua));
        }
        if has("PostSimulation") {
            fire("PostSimulation", (dt,).into_lua_multi_or_empty(&self.lua));
        }
        if has("Heartbeat") {
            fire("Heartbeat", (dt,).into_lua_multi_or_empty(&self.lua));
        }
        if has("RenderStepped") {
            fire("RenderStepped", (dt,).into_lua_multi_or_empty(&self.lua));
        }
        if has("PreRender") {
            fire("PreRender", (dt,).into_lua_multi_or_empty(&self.lua));
        }
        scheduler::step(&self.lua);
    }

    /// True if nothing is scheduled or connected (the VM can be dropped).
    pub fn is_idle(&self) -> bool {
        let c = ctx(&self.lua);
        c.sched.borrow().is_idle() && c.signals.borrow().is_empty()
    }

    pub fn used_memory(&self) -> usize {
        self.lua.used_memory()
    }

    fn report(&self, e: mlua::Error) {
        ctx(&self.lua).print(OutputLevel::Error, scheduler::format_error(&e));
    }

    fn dispatch_changes(&self, changes: &[Change]) {
        let lua = &self.lua;
        let c = ctx(lua);
        if c.signals.borrow().is_empty() {
            c.pending_attr_changes.borrow_mut().clear();
            return;
        }
        let attr_changes = std::mem::take(&mut *c.pending_attr_changes.borrow_mut());
        drop(c);
        let mut events: Vec<(SignalKey, MultiValue)> = Vec::new();
        {
            let c = ctx(lua);
            let signals = c.signals.borrow();
            let doc = c.doc.lock();
            let inst_val = |r: Ref| instance_value(lua, r).unwrap_or(Value::Nil);
            let mut push = |key: SignalKey, args: MultiValue| {
                if signals.has_listeners(&key) {
                    events.push((key, args));
                }
            };
            let ancestors = |mut r: Ref| {
                let mut v = Vec::new();
                while r.is_some() {
                    v.push(r);
                    r = doc.parent(r);
                }
                v
            };
            for change in changes {
                match change {
                    Change::Property(r, key) => {
                        if !signals.any_for(*r) {
                            continue;
                        }
                        let is_value_base = doc.is_a(*r, "ValueBase") && key.as_str() == "Value";
                        let arg = if is_value_base {
                            doc.get_property(*r, "Value")
                                .and_then(|v| convert::variant_to_lua(lua, &v, None).ok())
                                .unwrap_or(Value::Nil)
                        } else {
                            key.as_str().into_lua(lua).unwrap_or(Value::Nil)
                        };
                        push(SignalKey::Instance(*r, "Changed".into()), MultiValue::from_iter([arg]));
                        push(SignalKey::Property(*r, key.to_string()), MultiValue::new());
                        // Derived properties change with their source.
                        if key.as_str() == "CFrame" {
                            push(SignalKey::Property(*r, "Position".into()), MultiValue::new());
                            push(SignalKey::Property(*r, "Orientation".into()), MultiValue::new());
                        }
                        if key.as_str() == "Color" {
                            push(SignalKey::Property(*r, "BrickColor".into()), MultiValue::new());
                        }
                    }
                    Change::Added(r) => {
                        let parent = doc.parent(*r);
                        let child = inst_val(*r);
                        push(SignalKey::Instance(parent, "ChildAdded".into()), MultiValue::from_iter([child.clone()]));
                        for a in ancestors(parent) {
                            push(SignalKey::Instance(a, "DescendantAdded".into()), MultiValue::from_iter([child.clone()]));
                        }
                    }
                    Change::Reparented { instance, old, new } => {
                        let child = inst_val(*instance);
                        if old.is_some() {
                            push(SignalKey::Instance(*old, "ChildRemoved".into()), MultiValue::from_iter([child.clone()]));
                            for a in ancestors(*old) {
                                push(SignalKey::Instance(a, "DescendantRemoving".into()), MultiValue::from_iter([child.clone()]));
                            }
                        }
                        if new.is_some() {
                            push(SignalKey::Instance(*new, "ChildAdded".into()), MultiValue::from_iter([child.clone()]));
                            for a in ancestors(*new) {
                                push(SignalKey::Instance(a, "DescendantAdded".into()), MultiValue::from_iter([child.clone()]));
                            }
                        }
                        push(
                            SignalKey::Instance(*instance, "AncestryChanged".into()),
                            MultiValue::from_iter([child.clone(), inst_val(*new)]),
                        );
                        push(SignalKey::Property(*instance, "Parent".into()), MultiValue::new());
                        push(
                            SignalKey::Instance(*instance, "Changed".into()),
                            MultiValue::from_iter(["Parent".into_lua(lua).unwrap_or(Value::Nil)]),
                        );
                    }
                }
            }
            for (r, name) in attr_changes {
                push(
                    SignalKey::Instance(r, "AttributeChanged".into()),
                    MultiValue::from_iter([name.as_str().into_lua(lua).unwrap_or(Value::Nil)]),
                );
                push(SignalKey::Attribute(r, name), MultiValue::new());
            }
        }
        for (key, args) in events {
            if let Err(e) = signal::fire(lua, &key, args) {
                self.report(e);
            }
        }
    }
}

trait IntoLuaMultiOrEmpty {
    fn into_lua_multi_or_empty(self, lua: &Lua) -> MultiValue;
}

impl<T: mlua::IntoLuaMulti> IntoLuaMultiOrEmpty for T {
    fn into_lua_multi_or_empty(self, lua: &Lua) -> MultiValue {
        self.into_lua_multi(lua).unwrap_or_default()
    }
}

fn register_globals(lua: &Lua, g: &Table) -> mlua::Result<()> {
    fn join(lua: &Lua, args: Variadic<Value>) -> mlua::Result<String> {
        let tostring: Function = lua.globals().get("tostring")?;
        let mut parts = Vec::with_capacity(args.len());
        for a in args.iter() {
            parts.push(tostring.call::<String>(a.clone())?);
        }
        Ok(parts.join(" "))
    }
    g.set("print", lua.create_function(|lua, args: Variadic<Value>| {
        let text = join(lua, args)?;
        ctx(lua).print(OutputLevel::Info, text);
        Ok(())
    })?)?;
    g.set("warn", lua.create_function(|lua, args: Variadic<Value>| {
        let text = join(lua, args)?;
        ctx(lua).print(OutputLevel::Warn, text);
        Ok(())
    })?)?;
    g.set("require", lua.create_function(|lua, target: Value| {
        let r = match &target {
            Value::UserData(ud) if ud.is::<instance::LInstance>() => ud.borrow::<instance::LInstance>()?.0,
            Value::Integer(_) | Value::Number(_) => {
                return Err(mlua::Error::runtime("require by asset id is not supported offline"));
            }
            _ => return Err(mlua::Error::runtime("Attempted to call require with invalid argument(s).")),
        };
        if let Some(v) = ctx(lua).modules.borrow().get(&r) {
            return Ok(v.clone());
        }
        let (source, name) = {
            let c = ctx(lua);
            let doc = c.doc.lock();
            if !doc.is_a(r, "ModuleScript") {
                return Err(mlua::Error::runtime("Attempted to call require with a non-ModuleScript"));
            }
            let source = match doc.get_property(r, "Source") {
                Some(Variant::String(s)) => s,
                _ => String::new(),
            };
            (source, doc.full_name(r))
        };
        let env = lua.create_table()?;
        let mt = lua.create_table()?;
        mt.set("__index", lua.globals())?;
        env.set_metatable(Some(mt))?;
        env.set("script", instance_value(lua, r)?)?;
        let result: MultiValue = lua.load(&source).set_name(format!("={name}")).set_environment(env).call(())?;
        if result.len() != 1 {
            return Err(mlua::Error::runtime("Module code did not return exactly one value"));
        }
        let v = result.into_iter().next().unwrap();
        ctx(lua).modules.borrow_mut().insert(r, v.clone());
        Ok(v)
    })?)?;
    // typeof already honours __type in Luau; keep it.
    let settings = lua.create_table()?;
    g.set("settings", lua.create_function(move |_, ()| Ok(settings.clone()))?)?;
    g.set("version", lua.create_function(|_, ()| Ok(format!("phaedrus-{}", env!("CARGO_PKG_VERSION"))))?)?;
    let _ = reflection::database();
    Ok(())
}
