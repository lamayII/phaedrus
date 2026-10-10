//! The Roblox and Luau API the analyzer knows about: classes and enums from
//! `data/roblox-api.json` (see `scripts/gen-roblox-api.py`), plus globals,
//! libraries and datatypes from `data/luau-types.txt`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use serde_json::Value;

use crate::types::{FuncTy, Param, Ty, parse_type, split_top};

const API_JSON: &str = include_str!("../data/roblox-api.json");
const LUAU_TYPES: &str = include_str!("../data/luau-types.txt");

#[derive(Clone, Debug)]
pub enum MemberKind {
    Property { ty: Ty, read_only: bool },
    Function(Rc<FuncTy>),
    Event(Rc<[Param]>),
    Callback(Rc<FuncTy>),
}

#[derive(Clone, Debug)]
pub struct Member {
    pub name: Rc<str>,
    pub kind: MemberKind,
}

impl Member {
    /// The type of `x.Name`.
    pub fn ty(&self) -> Ty {
        match &self.kind {
            MemberKind::Property { ty, .. } => ty.clone(),
            MemberKind::Function(f) | MemberKind::Callback(f) => Ty::Function(f.clone()),
            MemberKind::Event(ps) => Ty::Signal(ps.clone()),
        }
    }

    pub fn doc(&self) -> Option<&str> {
        match &self.kind {
            MemberKind::Function(f) | MemberKind::Callback(f) => f.doc.as_deref(),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct Class {
    pub name: String,
    pub superclass: Option<String>,
    pub service: bool,
    pub creatable: bool,
    pub deprecated: bool,
    pub members: Vec<Member>,
}

#[derive(Debug, Default)]
pub struct Api {
    pub version: Option<String>,
    pub classes: HashMap<String, Class>,
    pub enums: BTreeMap<String, Vec<String>>,
    /// Members of a global table: a datatype's constructors or a library.
    pub statics: HashMap<String, Vec<Member>>,
    /// Members of datatype values.
    pub datatypes: HashMap<String, Vec<Member>>,
    pub globals: Vec<(Member, Option<Rc<str>>)>,
    pub services: Vec<String>,
    pub creatable: Vec<String>,
}

/// The API, loaded on first use. Types use `Rc`, so it is per thread (the
/// browser build only has one).
pub fn api() -> &'static Api {
    thread_local! {
        static API: &'static Api = Box::leak(Box::new(Api::load(API_JSON, LUAU_TYPES)));
    }
    API.with(|a| *a)
}

impl Api {
    pub fn load(api_json: &str, luau_types: &str) -> Api {
        let root: Value = serde_json::from_str(api_json).expect("roblox-api.json is valid");
        let raw_classes = root["classes"].as_array().cloned().unwrap_or_default();
        let class_names: HashSet<String> =
            raw_classes.iter().filter_map(|c| c["name"].as_str().map(str::to_owned)).collect();
        let datatype_names: HashSet<String> = luau_types
            .lines()
            .filter_map(|l| l.trim().strip_prefix("datatype "))
            .map(|n| n.trim().to_owned())
            .chain(["RBXScriptConnection".to_owned()])
            .collect();
        let is_class = |s: &str| class_names.contains(s);
        let is_datatype = |s: &str| datatype_names.contains(s);
        let ty = |s: &str| parse_type(s, &is_class, &is_datatype);

        let mut api = Api { version: root["version"].as_str().map(str::to_owned), ..Default::default() };

        for c in &raw_classes {
            let name = c["name"].as_str().unwrap_or_default().to_owned();
            let tags: Vec<&str> = c["tags"].as_array().map(|t| t.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
            let mut members = Vec::new();
            for m in c["members"].as_array().into_iter().flatten() {
                let Some(arr) = m.as_array() else { continue };
                let kind = arr[0].as_str().unwrap_or_default();
                let mname: Rc<str> = arr[1].as_str().unwrap_or_default().into();
                let kind = match kind {
                    "p" => MemberKind::Property { ty: ty(arr[2].as_str().unwrap_or("any")), read_only: arr[3].as_i64() == Some(1) },
                    "f" => MemberKind::Function(Rc::new(FuncTy {
                        params: json_params(&arr[2], &ty),
                        variadic: json_variadic(&arr[2], &ty),
                        ret: ty(arr[3].as_str().unwrap_or("nil")),
                        method: true,
                        doc: (arr.get(4).and_then(Value::as_i64) == Some(1)).then(|| "Yields.".into()),
                    })),
                    "e" => MemberKind::Event(json_params(&arr[2], &ty).into()),
                    "c" => MemberKind::Callback(Rc::new(FuncTy {
                        params: json_params(&arr[2], &ty),
                        variadic: json_variadic(&arr[2], &ty),
                        ret: ty(arr[3].as_str().unwrap_or("nil")),
                        method: false,
                        doc: None,
                    })),
                    _ => continue,
                };
                members.push(Member { name: mname, kind });
            }
            let class = Class {
                superclass: c["superclass"].as_str().map(str::to_owned),
                service: tags.contains(&"Service"),
                creatable: !tags.contains(&"NotCreatable") && !tags.contains(&"Service"),
                deprecated: tags.contains(&"Deprecated"),
                members,
                name: name.clone(),
            };
            if class.service {
                api.services.push(name.clone());
            }
            if class.creatable && !class.deprecated && !tags.contains(&"NotBrowsable") {
                api.creatable.push(name.clone());
            }
            api.classes.insert(name, class);
        }
        api.services.sort();
        api.creatable.sort();

        if let Some(enums) = root["enums"].as_object() {
            for (name, items) in enums {
                let items = items.as_array().map(|a| a.iter().filter_map(|i| i.as_str().map(str::to_owned)).collect()).unwrap_or_default();
                api.enums.insert(name.clone(), items);
            }
        }

        api.load_luau_types(luau_types, &ty);
        api
    }

    fn load_luau_types(&mut self, src: &str, ty: &dyn Fn(&str) -> Ty) {
        // (name, is_datatype) of the current block.
        let mut block: Option<(String, bool)> = None;
        for raw in src.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (line, doc) = match line.split_once("--") {
                Some((l, d)) => (l.trim(), Some(Rc::<str>::from(d.trim()))),
                None => (line, None),
            };
            if let Some(name) = line.strip_prefix("datatype ") {
                block = Some((name.trim().to_owned(), true));
                continue;
            }
            if let Some(name) = line.strip_prefix("library ") {
                block = Some((name.trim().to_owned(), false));
                continue;
            }
            if let Some(rest) = line.strip_prefix("global ") {
                if let Some(m) = parse_member(rest, false, doc.clone(), ty) {
                    self.globals.push((m, doc));
                }
                block = None;
                continue;
            }
            let Some((name, is_datatype)) = &block else { continue };
            if let Some(rest) = line.strip_prefix('.') {
                if let Some(m) = parse_member(rest, false, doc, ty) {
                    self.statics.entry(name.clone()).or_default().push(m);
                }
            } else if let Some(rest) = line.strip_prefix(':') {
                if let Some(m) = parse_member(rest, true, doc, ty) {
                    self.datatypes.entry(name.clone()).or_default().push(m);
                }
            } else if *is_datatype && let Some(m) = parse_member(line, false, doc, ty) {
                self.datatypes.entry(name.clone()).or_default().push(m);
            }
        }
        for (name, is_dt) in src.lines().filter_map(|l| {
            let l = l.trim();
            l.strip_prefix("datatype ").map(|n| (n.trim(), true)).or_else(|| l.strip_prefix("library ").map(|n| (n.trim(), false)))
        }) {
            self.statics.entry(name.to_owned()).or_default();
            if is_dt {
                self.datatypes.entry(name.to_owned()).or_default();
            }
        }
    }

    pub fn class(&self, name: &str) -> Option<&Class> {
        self.classes.get(name)
    }

    pub fn is_a(&self, class: &str, base: &str) -> bool {
        let mut cur = Some(class);
        let mut guard = 0;
        while let Some(c) = cur {
            if c == base {
                return true;
            }
            guard += 1;
            if guard > 64 {
                break;
            }
            cur = self.classes.get(c).and_then(|c| c.superclass.as_deref());
        }
        false
    }

    /// A member of a class or one of its superclasses, with the class that
    /// declares it.
    pub fn class_member(&self, class: &str, name: &str) -> Option<(&Member, &str)> {
        let mut cur = self.classes.get(class);
        while let Some(c) = cur {
            if let Ok(i) = c.members.binary_search_by(|m| (*m.name).cmp(name)) {
                return Some((&c.members[i], &c.name));
            }
            cur = c.superclass.as_deref().and_then(|s| self.classes.get(s));
        }
        None
    }

    /// Every member of a class including inherited ones, most derived first.
    pub fn class_members(&self, class: &str) -> Vec<(&Member, &str)> {
        let mut out: Vec<(&Member, &str)> = Vec::new();
        let mut seen = HashSet::new();
        let mut cur = self.classes.get(class);
        while let Some(c) = cur {
            for m in &c.members {
                if seen.insert(&*m.name) {
                    out.push((m, &c.name));
                }
            }
            cur = c.superclass.as_deref().and_then(|s| self.classes.get(s));
        }
        out
    }

    pub fn global(&self, name: &str) -> Option<&Member> {
        self.globals.iter().map(|(m, _)| m).find(|m| &*m.name == name)
    }

    pub fn static_member(&self, table: &str, name: &str) -> Option<&Member> {
        self.statics.get(table)?.iter().find(|m| &*m.name == name)
    }

    pub fn datatype_member(&self, datatype: &str, name: &str) -> Option<&Member> {
        self.datatypes.get(datatype)?.iter().find(|m| &*m.name == name)
    }

    pub fn is_datatype(&self, name: &str) -> bool {
        self.datatypes.contains_key(name)
    }
}

fn json_params(v: &Value, ty: &dyn Fn(&str) -> Ty) -> Vec<Param> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_array)
        .filter(|p| p[1].as_str() != Some("...any"))
        .map(|p| Param {
            name: p[0].as_str().unwrap_or("arg").into(),
            ty: ty(p[1].as_str().unwrap_or("any")),
            optional: p.len() > 2 || p[1].as_str().is_some_and(|t| t.ends_with('?')),
        })
        .collect()
}

fn json_variadic(v: &Value, _ty: &dyn Fn(&str) -> Ty) -> Option<Ty> {
    v.as_array()?.iter().filter_map(Value::as_array).any(|p| p[1].as_str() == Some("...any")).then_some(Ty::Any)
}

/// Parses `name(params): ret`, `name: Type` or `name(params)`.
fn parse_member(s: &str, method: bool, doc: Option<Rc<str>>, ty: &dyn Fn(&str) -> Ty) -> Option<Member> {
    let s = s.trim();
    let name_end = s.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
    let name: Rc<str> = s[..name_end].into();
    let rest = &s[name_end..];
    if let Some(after) = rest.strip_prefix('(') {
        let close = matching_paren(after)?;
        let params_src = &after[..close];
        let ret_src = after[close + 1..].trim().strip_prefix(':').map(str::trim).unwrap_or("nil");
        let mut params = Vec::new();
        let mut variadic = None;
        for p in split_top(params_src) {
            let (pname, pty) = p.split_once(':').map(|(a, b)| (a.trim(), b.trim())).unwrap_or((p, "any"));
            if pname == "..." {
                variadic = Some(ty(pty));
                continue;
            }
            params.push(Param { name: pname.into(), ty: ty(pty), optional: pty.ends_with('?') });
        }
        let f = FuncTy { params, variadic, ret: ty(ret_src), method, doc };
        Some(Member { name, kind: MemberKind::Function(Rc::new(f)) })
    } else {
        let t = rest.trim().strip_prefix(':')?.trim();
        Some(Member { name, kind: MemberKind::Property { ty: ty(t), read_only: false } })
    }
}

fn matching_paren(s: &str) -> Option<usize> {
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '{' => depth += 1,
            ')' if depth == 0 => return Some(i),
            ')' | '}' => depth -= 1,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_classes_and_datatypes() {
        let api = api();
        assert!(api.classes.len() > 500);
        assert!(api.is_a("Part", "BasePart"));
        assert!(api.is_a("Part", "Instance"));
        let (m, owner) = api.class_member("Part", "FindFirstChild").unwrap();
        assert_eq!(owner, "Instance");
        assert!(matches!(m.kind, MemberKind::Function(_)));
        assert!(matches!(api.class_member("Part", "Touched").unwrap().0.kind, MemberKind::Event(_)));
        assert!(api.services.iter().any(|s| s == "Players"));
        assert!(api.creatable.iter().any(|s| s == "Part"));
        assert!(!api.creatable.iter().any(|s| s == "Workspace"));
        assert!(api.static_member("Vector3", "new").is_some());
        assert!(api.datatype_member("Vector3", "Magnitude").is_some());
        assert!(api.datatype_member("CFrame", "Inverse").is_some());
        assert!(api.static_member("math", "clamp").is_some());
        assert!(api.global("print").is_some());
        match api.global("game").unwrap().ty() {
            Ty::Instance(c, _) => assert_eq!(&*c, "DataModel"),
            t => panic!("{t:?}"),
        }
        assert!(api.enums["Material"].iter().any(|i| i == "Plastic"));
    }
}
