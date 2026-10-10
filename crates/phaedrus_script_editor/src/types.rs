//! The small type model behind intellisense.
//!
//! Tables live in a thread-local arena and are referred to by id: Lua OOP
//! patterns (`Class.__index = Class`, constructors returning
//! `setmetatable({}, Class)`) are full of cycles that `Rc` would leak. The
//! arena is cleared between requests once it grows large, or whenever the
//! instance tree changes (see [`reset_tables`]).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::{self, Write};
use std::rc::Rc;

#[derive(Clone, Debug, Default)]
pub enum Ty {
    Any,
    #[default]
    Nil,
    Boolean,
    Number,
    String,
    /// A type with no known members (`thread`, `buffer`, `function`, ...).
    Named(Rc<str>),
    /// An instance of a Roblox class, with its node in the tree if known.
    Instance(Rc<str>, Option<usize>),
    /// A value of a Roblox datatype such as `Vector3`.
    Data(Rc<str>),
    /// A global table of statics: a datatype's constructors or a library.
    Static(Rc<str>),
    /// The `Enum` global.
    Enums,
    /// `Enum.Material`.
    EnumType(Rc<str>),
    /// `Enum.Material.Plastic`.
    EnumItem(Rc<str>),
    /// An `RBXScriptSignal` and the parameters it fires with.
    Signal(Rc<[Param]>),
    Function(Rc<FuncTy>),
    Table(TableId),
    Array(Rc<Ty>),
    Optional(Rc<Ty>),
    /// Several values, as returned by a function.
    Tuple(Rc<[Ty]>),
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: Rc<str>,
    pub ty: Ty,
    pub optional: bool,
}

#[derive(Clone, Debug, Default)]
pub struct FuncTy {
    pub params: Vec<Param>,
    /// Type of `...`, if the function is variadic.
    pub variadic: Option<Ty>,
    pub ret: Ty,
    /// Declared with `:`; `self` is implicit.
    pub method: bool,
    pub doc: Option<Rc<str>>,
}

impl Ty {
    pub fn instance(class: &str) -> Ty {
        Ty::Instance(class.into(), None)
    }

    /// The type with `?` and multiple values stripped, as used for member
    /// lookups.
    pub fn base(&self) -> &Ty {
        match self {
            Ty::Optional(t) => t.base(),
            Ty::Tuple(ts) => ts.first().map(Ty::base).unwrap_or(&Ty::Nil),
            t => t,
        }
    }

    /// The first value of a (possibly multi-value) type.
    pub fn first(self) -> Ty {
        match self {
            Ty::Tuple(ts) => ts.first().cloned().unwrap_or(Ty::Nil),
            t => t,
        }
    }

    /// The `i`th value of a multi-value type.
    pub fn nth(&self, i: usize) -> Ty {
        match self {
            Ty::Tuple(ts) => ts.get(i).cloned().unwrap_or(Ty::Nil),
            t if i == 0 => t.clone(),
            _ => Ty::Nil,
        }
    }

    pub fn is_unknown(&self) -> bool {
        matches!(self.base(), Ty::Any | Ty::Nil)
    }

    pub fn display(&self) -> String {
        let mut s = String::new();
        self.write(&mut s, 0);
        s
    }

    fn write(&self, out: &mut String, depth: usize) {
        match self {
            Ty::Any => out.push_str("any"),
            Ty::Nil => out.push_str("nil"),
            Ty::Boolean => out.push_str("boolean"),
            Ty::Number => out.push_str("number"),
            Ty::String => out.push_str("string"),
            Ty::Named(n) | Ty::Instance(n, _) | Ty::Data(n) => out.push_str(n),
            Ty::Static(n) => {
                let _ = write!(out, "typeof({n})");
            }
            Ty::Enums => out.push_str("Enums"),
            Ty::EnumType(e) => {
                let _ = write!(out, "typeof(Enum.{e})");
            }
            Ty::EnumItem(e) => {
                let _ = write!(out, "Enum.{e}");
            }
            Ty::Signal(ps) => {
                out.push_str("RBXScriptSignal");
                if !ps.is_empty() {
                    out.push('<');
                    for (i, p) in ps.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        p.ty.write(out, depth + 1);
                    }
                    out.push('>');
                }
            }
            Ty::Function(f) => {
                out.push('(');
                write_params(out, f, depth);
                out.push_str(") -> ");
                f.ret.write(out, depth + 1);
            }
            Ty::Table(id) => write_table(*id, out, depth),
            Ty::Array(t) => {
                out.push('{');
                t.write(out, depth + 1);
                out.push('}');
            }
            Ty::Optional(t) => {
                let needs_parens = matches!(**t, Ty::Function(_));
                if needs_parens {
                    out.push('(');
                }
                t.write(out, depth + 1);
                if needs_parens {
                    out.push(')');
                }
                out.push('?');
            }
            Ty::Tuple(ts) => {
                out.push('(');
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    t.write(out, depth + 1);
                }
                out.push(')');
            }
        }
    }
}

pub fn write_params(out: &mut String, f: &FuncTy, depth: usize) {
    let mut first = true;
    for p in &f.params {
        if !first {
            out.push_str(", ");
        }
        first = false;
        out.push_str(&p.name);
        out.push_str(": ");
        p.ty.write(out, depth + 1);
        if p.optional && !matches!(p.ty, Ty::Optional(_)) {
            out.push('?');
        }
    }
    if let Some(v) = &f.variadic {
        if !first {
            out.push_str(", ");
        }
        out.push_str("...: ");
        v.write(out, depth + 1);
    }
}

fn write_table(id: TableId, out: &mut String, depth: usize) {
    let (name, fields) = with_table(id, |t| {
        (t.name.clone(), t.fields.iter().take(4).map(|(k, v)| (k.clone(), v.clone())).collect::<Vec<_>>())
    });
    if let Some(n) = name {
        out.push_str(&n);
        return;
    }
    if depth > 1 || fields.is_empty() {
        out.push_str("{}");
        return;
    }
    out.push_str("{ ");
    for (i, (k, v)) in fields.iter().enumerate() {
        if i == 3 {
            out.push_str(", ...");
            break;
        }
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(k);
        out.push_str(": ");
        v.write(out, depth + 1);
    }
    out.push_str(" }");
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display())
    }
}

// --- tables ------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TableId(u32);

#[derive(Debug, Default)]
pub struct TableTy {
    /// Shown instead of the fields, e.g. the local it was declared as.
    pub name: Option<Rc<str>>,
    pub fields: BTreeMap<String, Ty>,
    /// Element type of the array part.
    pub array: Option<Ty>,
    pub meta: Option<TableId>,
    /// For a class table: the object table that `setmetatable(obj, Class)`
    /// was first called with, used as `self` in its methods.
    pub instance: Option<TableId>,
}

thread_local! {
    static TABLES: RefCell<Vec<TableTy>> = const { RefCell::new(Vec::new()) };
}

const TABLE_LIMIT: usize = 100_000;

pub fn new_table() -> TableId {
    TABLES.with_borrow_mut(|t| {
        t.push(TableTy::default());
        TableId((t.len() - 1) as u32)
    })
}

pub fn with_table<R>(id: TableId, f: impl FnOnce(&TableTy) -> R) -> R {
    TABLES.with_borrow(|t| f(&t[id.0 as usize]))
}

pub fn with_table_mut<R>(id: TableId, f: impl FnOnce(&mut TableTy) -> R) -> R {
    TABLES.with_borrow_mut(|t| f(&mut t[id.0 as usize]))
}

/// Whether the arena is big enough that it (and everything holding table
/// ids) should be dropped before the next request.
pub fn tables_need_reset() -> bool {
    TABLES.with_borrow(|t| t.len() > TABLE_LIMIT)
}

/// Drops every table. Callers must drop all `Ty` values first.
pub fn reset_tables() {
    TABLES.with_borrow_mut(|t| t.clear());
}

/// Looks up `key` on a table, following `__index` metatables.
pub fn table_field(id: TableId, key: &str) -> Option<Ty> {
    let mut cur = id;
    for _ in 0..16 {
        let (found, meta) = with_table(cur, |t| (t.fields.get(key).cloned(), t.meta));
        if found.is_some() {
            return found;
        }
        let index = with_table(meta?, |t| t.fields.get("__index").cloned());
        match index {
            Some(Ty::Table(next)) => cur = next,
            _ => return None,
        }
    }
    None
}

/// Every field visible on a table, including inherited ones.
pub fn table_fields(id: TableId) -> Vec<(String, Ty)> {
    let mut out: Vec<(String, Ty)> = Vec::new();
    let mut cur = Some(id);
    for _ in 0..16 {
        let Some(c) = cur else { break };
        let meta = with_table(c, |t| {
            for (k, v) in &t.fields {
                if !k.starts_with("__") && !out.iter().any(|(n, _)| n == k) {
                    out.push((k.clone(), v.clone()));
                }
            }
            t.meta
        });
        cur = meta.and_then(|m| match with_table(m, |t| t.fields.get("__index").cloned()) {
            Some(Ty::Table(next)) => Some(next),
            _ => None,
        });
    }
    out
}

// --- parsing type strings ------------------------------------------------------

/// Parses the type syntax used by the API data: primitives, class and
/// datatype names, `Enum.X`, `T?`, `{T}`, `{[K]: V}`, `(A, B)` and `...T`.
pub fn parse_type(s: &str, is_class: &dyn Fn(&str) -> bool, is_datatype: &dyn Fn(&str) -> bool) -> Ty {
    let s = s.trim();
    if let Some(inner) = s.strip_suffix('?') {
        let t = parse_type(inner, is_class, is_datatype);
        return if matches!(t, Ty::Any | Ty::Nil) { t } else { Ty::Optional(Rc::new(t)) };
    }
    if let Some(inner) = s.strip_prefix("...") {
        return parse_type(inner, is_class, is_datatype);
    }
    if let Some(inner) = s.strip_prefix('(').and_then(|x| x.strip_suffix(')')) {
        let parts: Vec<Ty> = split_top(inner).into_iter().map(|p| parse_type(p, is_class, is_datatype)).collect();
        return match parts.len() {
            0 => Ty::Nil,
            1 => parts.into_iter().next().unwrap(),
            _ => Ty::Tuple(parts.into()),
        };
    }
    if let Some(inner) = s.strip_prefix('{').and_then(|x| x.strip_suffix('}')) {
        if inner.trim_start().starts_with('[') {
            return Ty::Named(s.into());
        }
        return Ty::Array(Rc::new(parse_type(inner, is_class, is_datatype)));
    }
    if let Some(e) = s.strip_prefix("Enum.") {
        return Ty::EnumItem(e.into());
    }
    match s {
        "" | "any" => Ty::Any,
        "nil" => Ty::Nil,
        "boolean" | "bool" => Ty::Boolean,
        "number" => Ty::Number,
        "string" => Ty::String,
        "RBXScriptSignal" => Ty::Signal(Rc::new([])),
        _ if is_class(s) => Ty::instance(s),
        _ if is_datatype(s) => Ty::Data(s.into()),
        _ => Ty::Named(s.into()),
    }
}

/// Splits on commas that are not nested in brackets.
pub fn split_top(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '{' | '[' => depth += 1,
            ')' | '}' | ']' => depth -= 1,
            ',' if depth == 0 => {
                out.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = s[start..].trim();
    if !last.is_empty() {
        out.push(last);
    }
    out
}
