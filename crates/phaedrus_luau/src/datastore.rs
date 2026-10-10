//! Offline emulator for `DataStoreService`.
//!
//! Roblox data stores need a published place and a network connection. The
//! emulator stands in for them: when enabled, `DataStoreService` members are
//! bound to an implementation that keeps its data in a JSON file per place
//! (in the app data folder) and that can simulate the failures scripts must
//! handle (`500`/`502` errors, throttling and latency).
//!
//! The Roblox-facing objects (`DataStore`, `OrderedDataStore`, pages, key
//! info, options) are written in Luau (`datastore.luau`) because their
//! methods yield. They call into the Rust backend registered here for
//! storage, validation and failure simulation.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use mlua::{AnyUserData, Function, IntoLua, Lua, MetaMethod, Table, UserData, UserDataFields, UserDataMethods, Value};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::{OutputLevel, ctx};

pub type SharedDataStore = Arc<DataStoreEmulator>;

/// Message Studio gives for data store use in an unpublished place.
const NOT_ENABLED: &str = "You must publish this place to the web to access DataStore.";
/// Largest serialized value Roblox accepts (4 MB).
const MAX_VALUE_LEN: usize = 4_194_304;
const MAX_METADATA_LEN: usize = 300;
const MAX_USER_IDS: usize = 4;
/// Versions kept per key; older ones are dropped.
const MAX_VERSIONS: usize = 100;
/// Roblox throttles writes to the same key closer together than this.
const WRITE_COOLDOWN_SECS: f64 = 6.0;
/// Deepest table nesting that can be stored.
const MAX_DEPTH: u32 = 64;

// --- configuration -----------------------------------------------------------

/// A data store request that can be made to fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Operation {
    GetAsync,
    SetAsync,
    UpdateAsync,
    IncrementAsync,
    RemoveAsync,
    GetSortedAsync,
    ListKeysAsync,
    ListVersionsAsync,
    GetVersionAsync,
    RemoveVersionAsync,
    ListDataStoresAsync,
}

impl Operation {
    pub const ALL: [Operation; 11] = [
        Operation::GetAsync,
        Operation::SetAsync,
        Operation::UpdateAsync,
        Operation::IncrementAsync,
        Operation::RemoveAsync,
        Operation::GetSortedAsync,
        Operation::ListKeysAsync,
        Operation::ListVersionsAsync,
        Operation::GetVersionAsync,
        Operation::RemoveVersionAsync,
        Operation::ListDataStoresAsync,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Operation::GetAsync => "GetAsync",
            Operation::SetAsync => "SetAsync",
            Operation::UpdateAsync => "UpdateAsync",
            Operation::IncrementAsync => "IncrementAsync",
            Operation::RemoveAsync => "RemoveAsync",
            Operation::GetSortedAsync => "GetSortedAsync",
            Operation::ListKeysAsync => "ListKeysAsync",
            Operation::ListVersionsAsync => "ListVersionsAsync",
            Operation::GetVersionAsync => "GetVersionAsync",
            Operation::RemoveVersionAsync => "RemoveVersionAsync",
            Operation::ListDataStoresAsync => "ListDataStoresAsync",
        }
    }

    /// Methods that count as this operation, for the settings UI.
    pub fn covers(self) -> &'static str {
        match self {
            Operation::GetVersionAsync => "GetVersionAsync, GetVersionAtTimeAsync",
            Operation::ListKeysAsync => "ListKeysAsync and its AdvanceToNextPageAsync",
            Operation::ListVersionsAsync => "ListVersionsAsync and its AdvanceToNextPageAsync",
            Operation::GetSortedAsync => "OrderedDataStore:GetSortedAsync and its AdvanceToNextPageAsync",
            Operation::ListDataStoresAsync => "DataStoreService:ListDataStoresAsync and its AdvanceToNextPageAsync",
            _ => "DataStore and OrderedDataStore",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|o| o.name() == name)
    }
}

/// Simulated server error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FailureCode {
    /// `500: Internal Server Error`.
    InternalError500,
    /// `502: API Services rejected request` (could not reach data stores).
    #[default]
    BadGateway502,
    /// 500 or 502, picked at random per failure.
    Either,
}

impl FailureCode {
    pub fn label(self) -> &'static str {
        match self {
            FailureCode::InternalError500 => "500 Internal Server Error",
            FailureCode::BadGateway502 => "502 Bad Gateway",
            FailureCode::Either => "500 or 502 (random)",
        }
    }
}

/// Error messages scripts see. They follow Roblox's `<code>: <text>` form so
/// handlers that match on the code work unchanged.
fn failure_message(code: u16) -> &'static str {
    match code {
        500 => "500: Internal Server Error. The data store service failed to process the request.",
        _ => "502: API Services rejected request with error. HTTP 502 (Bad Gateway)",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FailureRule {
    pub enabled: bool,
    pub code: FailureCode,
    /// Chance that a request fails, in percent.
    pub chance: f32,
}

impl Default for FailureRule {
    fn default() -> Self {
        Self { enabled: false, code: FailureCode::BadGateway502, chance: 25.0 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DataStoreConfig {
    /// Bind `DataStoreService` to the emulator.
    pub enabled: bool,
    /// Simulated round-trip time of every request.
    pub latency_ms: u32,
    /// Random extra latency, up to this much.
    pub latency_jitter_ms: u32,
    /// Queue requests that exceed the request budget or write the same key
    /// within 6 seconds, as Roblox does.
    pub enforce_budgets: bool,
    /// Print every request to Output.
    pub log_requests: bool,
    /// Print a warning when a failure is simulated.
    pub log_failures: bool,
    /// Every request fails with `outage_code`.
    pub outage: bool,
    pub outage_code: FailureCode,
    /// Failures only apply to these data stores (comma separated; a trailing
    /// `*` matches a prefix). Empty means all.
    pub store_filter: String,
    pub rules: BTreeMap<Operation, FailureRule>,
}

impl Default for DataStoreConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            latency_ms: 30,
            latency_jitter_ms: 20,
            enforce_budgets: true,
            log_requests: false,
            log_failures: true,
            outage: false,
            outage_code: FailureCode::BadGateway502,
            store_filter: String::new(),
            rules: BTreeMap::new(),
        }
    }
}

impl DataStoreConfig {
    pub fn rule(&self, op: Operation) -> FailureRule {
        self.rules.get(&op).copied().unwrap_or_default()
    }

    pub fn rule_mut(&mut self, op: Operation) -> &mut FailureRule {
        self.rules.entry(op).or_default()
    }

    fn filter_matches(&self, store: &str) -> bool {
        let mut any = false;
        for pat in self.store_filter.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            any = true;
            let hit = match pat.strip_suffix('*') {
                Some(prefix) => store.starts_with(prefix),
                None => store == pat,
            };
            if hit {
                return true;
            }
        }
        !any
    }
}

// --- stored data -------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionEntry {
    pub version: String,
    /// Unix milliseconds.
    pub created: i64,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub value: Json,
    #[serde(default)]
    pub user_ids: Vec<i64>,
    #[serde(default)]
    pub metadata: serde_json::Map<String, Json>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyEntry {
    pub created: i64,
    pub versions: Vec<VersionEntry>,
}

impl KeyEntry {
    fn current(&self) -> Option<&VersionEntry> {
        self.versions.last().filter(|v| !v.deleted)
    }
}

type Scopes<T> = BTreeMap<String, BTreeMap<String, T>>;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct DataFile {
    /// The place file this data belongs to (informational).
    place: String,
    next_version: u64,
    /// name -> scope -> key -> entry.
    standard: BTreeMap<String, Scopes<KeyEntry>>,
    /// name -> scope -> key -> value.
    ordered: BTreeMap<String, Scopes<i64>>,
}

/// One key, for the data browser.
#[derive(Debug, Clone)]
pub struct KeySummary {
    pub ordered: bool,
    pub store: String,
    pub scope: String,
    pub key: String,
    pub value: String,
    pub updated: i64,
    pub versions: usize,
}

struct State {
    config: DataStoreConfig,
    root: PathBuf,
    file: Option<PathBuf>,
    data: DataFile,
    dirty: bool,
    revision: u64,
    rng: u64,
    budgets: HashMap<String, f64>,
    budget_time: Instant,
    last_write: HashMap<String, Instant>,
}

/// The emulator's shared state. The app owns one and hands it to every VM.
pub struct DataStoreEmulator {
    state: Mutex<State>,
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// FNV-1a, stable across builds (unlike `DefaultHasher`).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// The data file for a place: `<stem>-<hash of full path>.json`, so two
/// places with the same file name keep separate data.
pub fn data_file_name(place: Option<&Path>) -> String {
    match place {
        None => "unsaved-place.json".to_owned(),
        Some(p) => {
            let full = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            let stem: String = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "place".into())
                .chars()
                .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
                .collect();
            let key = full.to_string_lossy().to_lowercase();
            format!("{stem}-{:08x}.json", fnv1a(key.as_bytes()) as u32)
        }
    }
}

/// Budget per minute for a `Enum.DataStoreRequestType` with no players.
fn budget_rate(kind: &str) -> f64 {
    match kind {
        "GetSortedAsync" | "GetVersionAsync" | "ListAsync" | "RemoveVersionAsync" => 5.0,
        "SetIncrementSortedAsync" | "OnUpdate" => 30.0,
        _ => 60.0,
    }
}

impl State {
    fn random(&mut self) -> f64 {
        // xorshift64*
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        (x.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 11) as f64 / (1u64 << 53) as f64
    }

    fn touch(&mut self) {
        self.dirty = true;
        self.revision += 1;
    }

    fn next_version(&mut self, time: i64) -> String {
        self.data.next_version += 1;
        format!("{:016X}.{:010}.01", time as u64, self.data.next_version)
    }

    fn refill_budgets(&mut self) {
        let elapsed = self.budget_time.elapsed().as_secs_f64();
        self.budget_time = Instant::now();
        for (kind, tokens) in self.budgets.iter_mut() {
            let rate = budget_rate(kind);
            *tokens = (*tokens + rate / 60.0 * elapsed).min(rate * 3.0);
        }
    }

    fn budget(&mut self, kind: &str) -> f64 {
        self.refill_budgets();
        *self.budgets.entry(kind.to_owned()).or_insert_with(|| budget_rate(kind))
    }

    fn entry(&self, name: &str, scope: &str, key: &str) -> Option<&KeyEntry> {
        self.data.standard.get(name)?.get(scope)?.get(key)
    }

    fn write_version(&mut self, name: &str, scope: &str, key: &str, value: Json, user_ids: Vec<i64>, metadata: serde_json::Map<String, Json>) -> String {
        let time = now_ms();
        let version = self.next_version(time);
        let entry = self
            .data
            .standard
            .entry(name.to_owned())
            .or_default()
            .entry(scope.to_owned())
            .or_default()
            .entry(key.to_owned())
            .or_insert_with(|| KeyEntry { created: time, versions: Vec::new() });
        if entry.current().is_none() && entry.versions.last().is_some_and(|v| v.deleted) {
            // A key written after removal starts a new life.
            entry.created = time;
        }
        entry.versions.push(VersionEntry { version: version.clone(), created: time, deleted: false, value, user_ids, metadata });
        if entry.versions.len() > MAX_VERSIONS {
            let excess = entry.versions.len() - MAX_VERSIONS;
            entry.versions.drain(..excess);
        }
        self.touch();
        version
    }

    fn load(&mut self, file: Option<PathBuf>) {
        self.data = file
            .as_ref()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        self.file = file;
        self.dirty = false;
        self.revision += 1;
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let Some(file) = &self.file else { return Ok(()) };
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(&self.data).map_err(std::io::Error::other)?;
        // Write then rename, so a crash mid-write cannot corrupt the data.
        let tmp = file.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, file)?;
        self.dirty = false;
        Ok(())
    }
}

impl DataStoreEmulator {
    /// Creates an emulator that keeps its files in `root`.
    pub fn new(root: PathBuf, config: DataStoreConfig) -> SharedDataStore {
        let seed = (now_ms() as u64) ^ 0x9e37_79b9_7f4a_7c15;
        Arc::new(Self {
            state: Mutex::new(State {
                config,
                root,
                file: None,
                data: DataFile::default(),
                dirty: false,
                revision: 0,
                rng: seed | 1,
                budgets: HashMap::new(),
                budget_time: Instant::now(),
                last_write: HashMap::new(),
            }),
        })
    }

    pub fn config(&self) -> DataStoreConfig {
        self.state.lock().config.clone()
    }

    pub fn set_config(&self, config: DataStoreConfig) {
        self.state.lock().config = config;
    }

    pub fn is_enabled(&self) -> bool {
        self.state.lock().config.enabled
    }

    /// Switches to the data of another place (saving the current one first).
    pub fn set_place(&self, place: Option<&Path>) -> std::io::Result<()> {
        let mut s = self.state.lock();
        let file = s.root.join(data_file_name(place));
        if s.file.as_ref() == Some(&file) {
            return Ok(());
        }
        let result = s.flush();
        s.load(Some(file));
        if let Some(p) = place {
            s.data.place = p.display().to_string();
        }
        result
    }

    /// The data file in use.
    pub fn data_file(&self) -> Option<PathBuf> {
        self.state.lock().file.clone()
    }

    pub fn root(&self) -> PathBuf {
        self.state.lock().root.clone()
    }

    /// Writes pending changes to disk.
    pub fn flush(&self) -> std::io::Result<()> {
        self.state.lock().flush()
    }

    /// Bumped on every change to the stored data.
    pub fn revision(&self) -> u64 {
        self.state.lock().revision
    }

    /// Resets request budgets and throttles, as a new server would.
    pub fn new_session(&self) {
        let mut s = self.state.lock();
        s.budgets.clear();
        s.budget_time = Instant::now();
        s.last_write.clear();
    }

    /// Deletes every data store of the current place.
    pub fn clear(&self) {
        let mut s = self.state.lock();
        s.data.standard.clear();
        s.data.ordered.clear();
        s.touch();
    }

    pub fn delete_key(&self, ordered: bool, store: &str, scope: &str, key: &str) {
        let mut s = self.state.lock();
        let removed = if ordered {
            s.data.ordered.get_mut(store).and_then(|m| m.get_mut(scope)).and_then(|m| m.remove(key)).is_some()
        } else {
            s.data.standard.get_mut(store).and_then(|m| m.get_mut(scope)).and_then(|m| m.remove(key)).is_some()
        };
        if removed {
            s.data.standard.retain(|_, scopes| {
                scopes.retain(|_, keys| !keys.is_empty());
                !scopes.is_empty()
            });
            s.data.ordered.retain(|_, scopes| {
                scopes.retain(|_, keys| !keys.is_empty());
                !scopes.is_empty()
            });
            s.touch();
        }
    }

    /// Every live key, for the data browser. Values are compact JSON cut to
    /// `max_value_len` characters.
    pub fn summary(&self, max_value_len: usize) -> Vec<KeySummary> {
        let s = self.state.lock();
        let cut = |text: String| {
            if text.chars().count() > max_value_len {
                let mut t: String = text.chars().take(max_value_len).collect();
                t.push('…');
                t
            } else {
                text
            }
        };
        let mut out = Vec::new();
        for (store, scopes) in &s.data.standard {
            for (scope, keys) in scopes {
                for (key, entry) in keys {
                    let Some(cur) = entry.current() else { continue };
                    out.push(KeySummary {
                        ordered: false,
                        store: store.clone(),
                        scope: scope.clone(),
                        key: key.clone(),
                        value: cut(cur.value.to_string()),
                        updated: cur.created,
                        versions: entry.versions.len(),
                    });
                }
            }
        }
        for (store, scopes) in &s.data.ordered {
            for (scope, keys) in scopes {
                for (key, value) in keys {
                    out.push(KeySummary {
                        ordered: true,
                        store: store.clone(),
                        scope: scope.clone(),
                        key: key.clone(),
                        value: value.to_string(),
                        updated: 0,
                        versions: 0,
                    });
                }
            }
        }
        out
    }
}

impl Drop for DataStoreEmulator {
    fn drop(&mut self) {
        let _ = self.state.get_mut().flush();
    }
}

// --- Lua <-> JSON ------------------------------------------------------------

fn cannot_store(what: &str) -> String {
    format!("104: Cannot store {what} in data store. Data stores can only accept valid UTF-8 characters.")
}

fn number_to_json(n: f64) -> Result<Json, String> {
    if !n.is_finite() {
        return Err(cannot_store("NaN or inf"));
    }
    if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
        return Ok(Json::from(n as i64));
    }
    serde_json::Number::from_f64(n).map(Json::Number).ok_or_else(|| cannot_store("number"))
}

/// Converts a Luau value to JSON the way Roblox serializes data store
/// values: arrays (keys `1..n`) and string-keyed dictionaries only.
pub fn lua_to_json(value: &Value, depth: u32) -> Result<Json, String> {
    if depth > MAX_DEPTH {
        return Err(cannot_store("table nested more than 64 levels (or a cyclic table)"));
    }
    Ok(match value {
        Value::Nil => Json::Null,
        Value::Boolean(b) => Json::Bool(*b),
        Value::Integer(i) => Json::from(*i),
        Value::Number(n) => number_to_json(*n)?,
        Value::String(s) => Json::String(s.to_str().map_err(|_| cannot_store("string"))?.to_owned()),
        Value::Table(t) => {
            let mut ints: Vec<(i64, Json)> = Vec::new();
            let mut strings = serde_json::Map::new();
            for pair in t.pairs::<Value, Value>() {
                let (k, v) = pair.map_err(|e| e.to_string())?;
                let v = lua_to_json(&v, depth + 1)?;
                match k {
                    Value::String(s) => {
                        strings.insert(s.to_str().map_err(|_| cannot_store("string"))?.to_owned(), v);
                    }
                    Value::Integer(i) => ints.push((i, v)),
                    Value::Number(n) if n.fract() == 0.0 => ints.push((n as i64, v)),
                    other => return Err(cannot_store(&format!("table with {} keys", other.type_name()))),
                }
            }
            if !ints.is_empty() && !strings.is_empty() {
                return Err(cannot_store("mixed table (array and dictionary keys)"));
            }
            if strings.is_empty() {
                ints.sort_by_key(|(i, _)| *i);
                if ints.iter().enumerate().any(|(n, (i, _))| *i != n as i64 + 1) {
                    return Err(cannot_store("array with holes or keys not starting at 1"));
                }
                Json::Array(ints.into_iter().map(|(_, v)| v).collect())
            } else {
                Json::Object(strings)
            }
        }
        other => return Err(cannot_store(&crate::convert::lua_type_name(other))),
    })
}

pub fn json_to_lua(lua: &Lua, value: &Json) -> mlua::Result<Value> {
    Ok(match value {
        Json::Null => Value::Nil,
        Json::Bool(b) => Value::Boolean(*b),
        Json::Number(n) => Value::Number(n.as_f64().unwrap_or(0.0)),
        Json::String(s) => s.as_str().into_lua(lua)?,
        Json::Array(a) => {
            let t = lua.create_table_with_capacity(a.len(), 0)?;
            for v in a {
                t.raw_push(json_to_lua(lua, v)?)?;
            }
            Value::Table(t)
        }
        Json::Object(o) => {
            let t = lua.create_table_with_capacity(0, o.len())?;
            for (k, v) in o {
                t.raw_set(k.as_str(), json_to_lua(lua, v)?)?;
            }
            Value::Table(t)
        }
    })
}

// --- Lua bindings ------------------------------------------------------------

fn rt_err(msg: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(msg.into())
}

fn emulator(lua: &Lua) -> mlua::Result<SharedDataStore> {
    ctx(lua).datastore.borrow().clone().ok_or_else(|| rt_err(NOT_ENABLED))
}

fn user_ids(v: Option<Table>) -> mlua::Result<Vec<i64>> {
    let Some(t) = v else { return Ok(Vec::new()) };
    let mut ids = Vec::new();
    for v in t.sequence_values::<f64>() {
        ids.push(v.map_err(|_| rt_err("userIds must be an array of numbers"))? as i64);
    }
    if ids.len() > MAX_USER_IDS {
        return Err(rt_err(format!("Too many user IDs: at most {MAX_USER_IDS} are allowed")));
    }
    Ok(ids)
}

fn metadata(v: Option<Table>) -> mlua::Result<serde_json::Map<String, Json>> {
    let Some(t) = v else { return Ok(Default::default()) };
    match lua_to_json(&Value::Table(t), 0).map_err(rt_err)? {
        Json::Object(m) => {
            if Json::Object(m.clone()).to_string().len() > MAX_METADATA_LEN {
                return Err(rt_err(format!("Metadata exceeds the {MAX_METADATA_LEN} character limit")));
            }
            Ok(m)
        }
        Json::Array(a) if a.is_empty() => Ok(Default::default()),
        _ => Err(rt_err("Metadata must be a dictionary with string keys")),
    }
}

/// `{CreatedTime, UpdatedTime, Version, UserIds, Metadata}` for `DataStoreKeyInfo`.
fn key_info(lua: &Lua, entry: &KeyEntry, version: &VersionEntry) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("CreatedTime", entry.created as f64)?;
    t.set("UpdatedTime", version.created as f64)?;
    t.set("Version", version.version.as_str())?;
    t.set("UserIds", lua.create_sequence_from(version.user_ids.iter().map(|i| *i as f64))?)?;
    t.set("Metadata", json_to_lua(lua, &Json::Object(version.metadata.clone()))?)?;
    Ok(t)
}

fn value_and_info(lua: &Lua, entry: &KeyEntry, version: &VersionEntry) -> mlua::Result<(Value, Value)> {
    if version.deleted {
        return Ok((Value::Nil, Value::Nil));
    }
    Ok((json_to_lua(lua, &version.value)?, Value::Table(key_info(lua, entry, version)?)))
}

fn parse_encoded(encoded: &mlua::LuaString) -> mlua::Result<Json> {
    serde_json::from_slice(&encoded.as_bytes()).map_err(|e| rt_err(format!("corrupt encoded value: {e}")))
}

/// An Instance-like object of the Luau layer. Luau's `typeof` ignores
/// `__type` on `newproxy` values, so the objects are host userdata whose
/// metamethods forward to Luau functions.
struct LuauObject {
    name: String,
    index: Function,
    newindex: Function,
}

impl UserData for LuauObject {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_meta_field(MetaMethod::Type, "Instance");
    }

    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_meta_function(MetaMethod::Index, |_, (this, key): (AnyUserData, Value)| {
            let index = this.borrow::<LuauObject>()?.index.clone();
            index.call::<Value>((this, key))
        });
        m.add_meta_function(MetaMethod::NewIndex, |_, (this, key, value): (AnyUserData, Value, Value)| {
            let newindex = this.borrow::<LuauObject>()?.newindex.clone();
            newindex.call::<()>((this, key, value))
        });
        m.add_meta_method(MetaMethod::ToString, |_, this, ()| Ok(this.name.clone()));
    }
}

/// Builds the backend table the Luau layer calls.
fn backend(lua: &Lua) -> mlua::Result<Table> {
    let b = lua.create_table()?;

    b.set("object", lua.create_function(|_, (name, index, newindex): (String, Function, Function)| {
        Ok(LuauObject { name, index, newindex })
    })?)?;

    b.set("enabled", lua.create_function(|lua, ()| {
        Ok(ctx(lua).datastore.borrow().as_ref().is_some_and(|d| d.is_enabled()))
    })?)?;

    b.set("latency", lua.create_function(|lua, ()| {
        let e = emulator(lua)?;
        let mut s = e.state.lock();
        let jitter = s.config.latency_jitter_ms as f64 * s.random();
        Ok((s.config.latency_ms as f64 + jitter) / 1000.0)
    })?)?;

    // Takes a request from the budget. Returns seconds to wait when the
    // request must queue instead.
    b.set("acquire", lua.create_function(|lua, (kind, name, scope, key, write): (String, String, String, Option<String>, bool)| {
        let e = emulator(lua)?;
        let mut s = e.state.lock();
        let enforce = s.config.enforce_budgets;
        if enforce && write && let Some(key) = &key {
            let id = format!("{name}\u{0}{scope}\u{0}{key}");
            if let Some(last) = s.last_write.get(&id) {
                let since = last.elapsed().as_secs_f64();
                if since < WRITE_COOLDOWN_SECS {
                    return Ok(Some(WRITE_COOLDOWN_SECS - since));
                }
            }
        }
        let tokens = s.budget(&kind);
        if enforce && tokens < 1.0 {
            return Ok(Some((1.0 - tokens) / (budget_rate(&kind) / 60.0)));
        }
        if let Some(t) = s.budgets.get_mut(&kind) {
            *t = (*t - 1.0).max(0.0);
        }
        if write && let Some(key) = key {
            s.last_write.insert(format!("{name}\u{0}{scope}\u{0}{key}"), Instant::now());
        }
        Ok(None)
    })?)?;

    b.set("budget", lua.create_function(|lua, kind: String| {
        let e = emulator(lua)?;
        let mut s = e.state.lock();
        Ok(s.budget(&kind).floor())
    })?)?;

    // Decides whether this request fails. Returns the error message if so.
    b.set("check", lua.create_function(|lua, (op, store, key): (String, String, Option<String>)| {
        let e = emulator(lua)?;
        let (log_requests, log_failures, code) = {
            let mut s = e.state.lock();
            let cfg = &s.config;
            let op_kind = Operation::from_name(&op);
            let matches = cfg.filter_matches(&store);
            let code = if !matches {
                None
            } else if cfg.outage {
                Some(cfg.outage_code)
            } else {
                match op_kind.map(|o| cfg.rule(o)) {
                    Some(rule) if rule.enabled && rule.chance > 0.0 => {
                        let chance = rule.chance as f64 / 100.0;
                        (s.random() < chance).then_some(rule.code)
                    }
                    _ => None,
                }
            };
            let code = code.map(|c| match c {
                FailureCode::InternalError500 => 500,
                FailureCode::BadGateway502 => 502,
                FailureCode::Either => {
                    if s.random() < 0.5 { 500 } else { 502 }
                }
            });
            (s.config.log_requests, s.config.log_failures, code)
        };
        let target = match &key {
            Some(k) => format!("{store}:{op}(\"{k}\")"),
            None if store.is_empty() => op.clone(),
            None => format!("{store}:{op}"),
        };
        let c = ctx(lua);
        if log_requests {
            c.print(OutputLevel::Info, format!("[DataStore emulator] {target}"));
        }
        Ok(code.map(|code| {
            if log_failures {
                c.print(OutputLevel::Warn, format!("[DataStore emulator] simulated {code} error on {target}"));
            }
            failure_message(code).to_owned()
        }))
    })?)?;

    // Validates and serializes a value up front, as Roblox does when the
    // request is made (later edits to the table do not affect the write).
    b.set("encode", lua.create_function(|lua, value: Value| {
        let json = lua_to_json(&value, 0).map_err(rt_err)?;
        let text = json.to_string();
        if text.len() > MAX_VALUE_LEN {
            return Err(rt_err("105: Serialized value exceeds 4MB limit."));
        }
        lua.create_string(text)
    })?)?;

    b.set("get", lua.create_function(|lua, (name, scope, key): (String, String, String)| {
        let e = emulator(lua)?;
        let s = e.state.lock();
        match s.entry(&name, &scope, &key) {
            Some(entry) => match entry.current() {
                Some(v) => value_and_info(lua, entry, v),
                None => Ok((Value::Nil, Value::Nil)),
            },
            None => Ok((Value::Nil, Value::Nil)),
        }
    })?)?;

    b.set("set", lua.create_function(|lua, (name, scope, key, encoded, ids, meta): (String, String, String, mlua::LuaString, Option<Table>, Option<Table>)| {
        let value = parse_encoded(&encoded)?;
        let ids = user_ids(ids)?;
        let meta = metadata(meta)?;
        let e = emulator(lua)?;
        let mut s = e.state.lock();
        Ok(s.write_version(&name, &scope, &key, value, ids, meta))
    })?)?;

    b.set("increment", lua.create_function(|lua, (name, scope, key, delta, ids, meta): (String, String, String, f64, Option<Table>, Option<Table>)| {
        let ids = user_ids(ids)?;
        let meta = metadata(meta)?;
        let e = emulator(lua)?;
        let mut s = e.state.lock();
        let current = match s.entry(&name, &scope, &key).and_then(|e| e.current()) {
            None => 0.0,
            Some(v) => match &v.value {
                Json::Number(n) if n.as_f64().is_some_and(|f| f.fract() == 0.0) => n.as_f64().unwrap_or(0.0),
                Json::Null => 0.0,
                _ => return Err(rt_err("IncrementAsync: the stored value is not an integer")),
            },
        };
        let next = current + delta.trunc();
        s.write_version(&name, &scope, &key, number_to_json(next).map_err(rt_err)?, ids, meta);
        let entry = s.entry(&name, &scope, &key).expect("just written");
        let version = entry.versions.last().expect("just written");
        Ok((next, key_info(lua, entry, version)?))
    })?)?;

    b.set("remove", lua.create_function(|lua, (name, scope, key): (String, String, String)| {
        let e = emulator(lua)?;
        let mut s = e.state.lock();
        let previous = match s.entry(&name, &scope, &key) {
            Some(entry) => match entry.current() {
                Some(v) => value_and_info(lua, entry, v)?,
                None => return Ok((Value::Nil, Value::Nil)),
            },
            None => return Ok((Value::Nil, Value::Nil)),
        };
        let time = now_ms();
        let version = s.next_version(time);
        if let Some(entry) = s.data.standard.get_mut(&name).and_then(|m| m.get_mut(&scope)).and_then(|m| m.get_mut(&key)) {
            entry.versions.push(VersionEntry {
                version,
                created: time,
                deleted: true,
                value: Json::Null,
                user_ids: Vec::new(),
                metadata: Default::default(),
            });
        }
        s.touch();
        Ok(previous)
    })?)?;

    b.set("list_keys", lua.create_function(|lua, (name, scope, all_scopes, prefix, exclude_deleted): (String, String, bool, String, bool)| {
        let e = emulator(lua)?;
        let s = e.state.lock();
        let mut keys = Vec::new();
        if let Some(scopes) = s.data.standard.get(&name) {
            for (sc, entries) in scopes {
                if !all_scopes && *sc != scope {
                    continue;
                }
                for (k, entry) in entries {
                    if exclude_deleted && entry.current().is_none() {
                        continue;
                    }
                    let full = if all_scopes { format!("{sc}/{k}") } else { k.clone() };
                    if full.starts_with(&prefix) {
                        keys.push(full);
                    }
                }
            }
        }
        lua.create_sequence_from(keys)
    })?)?;

    b.set("list_stores", lua.create_function(|lua, prefix: String| {
        let e = emulator(lua)?;
        let s = e.state.lock();
        let out = lua.create_table()?;
        for (name, scopes) in &s.data.standard {
            if name.starts_with('$') || !name.starts_with(&prefix) {
                continue;
            }
            let entries = scopes.values().flat_map(|m| m.values());
            let created = entries.clone().map(|e| e.created).min().unwrap_or(0);
            let updated = entries.filter_map(|e| e.versions.last()).map(|v| v.created).max().unwrap_or(0);
            let t = lua.create_table()?;
            t.set("DataStoreName", name.as_str())?;
            t.set("CreatedTime", created as f64)?;
            t.set("UpdatedTime", updated as f64)?;
            out.raw_push(t)?;
        }
        Ok(out)
    })?)?;

    b.set("list_versions", lua.create_function(|lua, (name, scope, key, ascending, min, max): (String, String, String, bool, f64, f64)| {
        let e = emulator(lua)?;
        let s = e.state.lock();
        let out = lua.create_table()?;
        if let Some(entry) = s.entry(&name, &scope, &key) {
            let mut versions: Vec<&VersionEntry> = entry
                .versions
                .iter()
                .filter(|v| (min <= 0.0 || v.created as f64 >= min) && (max <= 0.0 || v.created as f64 <= max))
                .collect();
            if !ascending {
                versions.reverse();
            }
            for v in versions {
                let t = lua.create_table()?;
                t.set("Version", v.version.as_str())?;
                t.set("CreatedTime", v.created as f64)?;
                t.set("IsDeleted", v.deleted)?;
                out.raw_push(t)?;
            }
        }
        Ok(out)
    })?)?;

    b.set("get_version", lua.create_function(|lua, (name, scope, key, version): (String, String, String, String)| {
        let e = emulator(lua)?;
        let s = e.state.lock();
        let Some(entry) = s.entry(&name, &scope, &key) else { return Ok((Value::Nil, Value::Nil)) };
        match entry.versions.iter().find(|v| v.version == version) {
            Some(v) => value_and_info(lua, entry, v),
            None => Ok((Value::Nil, Value::Nil)),
        }
    })?)?;

    b.set("get_version_at", lua.create_function(|lua, (name, scope, key, time): (String, String, String, f64)| {
        let e = emulator(lua)?;
        let s = e.state.lock();
        let Some(entry) = s.entry(&name, &scope, &key) else { return Ok((Value::Nil, Value::Nil)) };
        match entry.versions.iter().rev().find(|v| v.created as f64 <= time) {
            Some(v) => value_and_info(lua, entry, v),
            None => Ok((Value::Nil, Value::Nil)),
        }
    })?)?;

    b.set("remove_version", lua.create_function(|lua, (name, scope, key, version): (String, String, String, String)| {
        let e = emulator(lua)?;
        let mut s = e.state.lock();
        let Some(entry) = s.data.standard.get_mut(&name).and_then(|m| m.get_mut(&scope)).and_then(|m| m.get_mut(&key)) else {
            return Ok(());
        };
        let Some(i) = entry.versions.iter().position(|v| v.version == version) else { return Ok(()) };
        if i + 1 == entry.versions.len() && !entry.versions[i].deleted {
            return Err(rt_err("RemoveVersionAsync: cannot remove the latest version of a key"));
        }
        entry.versions.remove(i);
        s.touch();
        Ok(())
    })?)?;

    // --- OrderedDataStore -------------------------------------------------

    b.set("ordered_get", lua.create_function(|lua, (name, scope, key): (String, String, String)| {
        let e = emulator(lua)?;
        let s = e.state.lock();
        Ok(s.data.ordered.get(&name).and_then(|m| m.get(&scope)).and_then(|m| m.get(&key)).map(|v| *v as f64))
    })?)?;

    b.set("ordered_set", lua.create_function(|lua, (name, scope, key, value): (String, String, String, f64)| {
        let e = emulator(lua)?;
        let mut s = e.state.lock();
        s.data.ordered.entry(name).or_default().entry(scope).or_default().insert(key, value as i64);
        s.touch();
        Ok(())
    })?)?;

    b.set("ordered_remove", lua.create_function(|lua, (name, scope, key): (String, String, String)| {
        let e = emulator(lua)?;
        let mut s = e.state.lock();
        let old = s.data.ordered.get_mut(&name).and_then(|m| m.get_mut(&scope)).and_then(|m| m.remove(&key));
        if old.is_some() {
            s.touch();
        }
        Ok(old.map(|v| v as f64))
    })?)?;

    b.set("sorted", lua.create_function(|lua, (name, scope, ascending, min, max): (String, String, bool, Option<f64>, Option<f64>)| {
        let e = emulator(lua)?;
        let s = e.state.lock();
        let mut items: Vec<(&String, i64)> = s
            .data
            .ordered
            .get(&name)
            .and_then(|m| m.get(&scope))
            .map(|m| m.iter().map(|(k, v)| (k, *v)).collect())
            .unwrap_or_default();
        items.retain(|(_, v)| min.is_none_or(|m| *v as f64 >= m) && max.is_none_or(|m| *v as f64 <= m));
        items.sort_by(|a, b| if ascending { a.1.cmp(&b.1).then(a.0.cmp(b.0)) } else { b.1.cmp(&a.1).then(a.0.cmp(b.0)) });
        let out = lua.create_table_with_capacity(items.len(), 0)?;
        for (k, v) in items {
            let t = lua.create_table()?;
            t.set("key", k.as_str())?;
            t.set("value", v as f64)?;
            out.raw_push(t)?;
        }
        Ok(out)
    })?)?;

    Ok(b)
}

const LIBRARY: &str = include_str!("datastore.luau");

/// Loads the Luau layer. Its service methods and option constructor are
/// kept in the registry for `services.rs` and `Instance.new`.
pub fn register(lua: &Lua, task: &Table) -> mlua::Result<()> {
    let backend = backend(lua)?;
    let warn: Function = lua.globals().get("warn")?;
    let (service, new_option): (Table, Function) = lua
        .load(LIBRARY)
        .set_name("=phaedrus.datastore")
        .call((backend, task.clone(), warn))?;
    lua.set_named_registry_value("phaedrus.datastore.service", service)?;
    lua.set_named_registry_value("phaedrus.datastore.new_option", new_option)?;
    Ok(())
}

/// Classes `Instance.new` creates through the emulator's Luau layer.
pub const OPTION_CLASSES: &[&str] =
    &["DataStoreOptions", "DataStoreGetOptions", "DataStoreSetOptions", "DataStoreIncrementOptions"];

/// `DataStoreService` members, looked up by `services.rs`.
pub fn service_member(lua: &Lua, key: &str) -> mlua::Result<Option<Value>> {
    let service: Table = lua.named_registry_value("phaedrus.datastore.service")?;
    match service.raw_get::<Value>(key)? {
        Value::Nil => Ok(None),
        v => Ok(Some(v)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip_rules() {
        let lua = Lua::new();
        let v: Value = lua.load("return {1, 2, {a = true, b = 'x'}}").eval().unwrap();
        let j = lua_to_json(&v, 0).unwrap();
        assert_eq!(j.to_string(), r#"[1,2,{"a":true,"b":"x"}]"#);
        let mixed: Value = lua.load("return {1, a = 2}").eval().unwrap();
        assert!(lua_to_json(&mixed, 0).unwrap_err().starts_with("104"));
        let holes: Value = lua.load("return {[1] = 1, [3] = 3}").eval().unwrap();
        assert!(lua_to_json(&holes, 0).is_err());
        let f: Value = lua.load("return function() end").eval().unwrap();
        assert!(lua_to_json(&f, 0).unwrap_err().contains("function"));
    }

    #[test]
    fn filter() {
        let mut c = DataStoreConfig { store_filter: "Player*, Coins".into(), ..Default::default() };
        assert!(c.filter_matches("PlayerData"));
        assert!(c.filter_matches("Coins"));
        assert!(!c.filter_matches("Other"));
        c.store_filter.clear();
        assert!(c.filter_matches("Other"));
    }

    #[test]
    fn data_file_names_differ_by_path() {
        let a = data_file_name(Some(Path::new("C:/a/place.rbxl")));
        let b = data_file_name(Some(Path::new("C:/b/place.rbxl")));
        assert_ne!(a, b);
        assert!(a.starts_with("place-"));
    }
}
