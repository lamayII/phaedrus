//! Completions, hovers and signature help, computed from source text and a
//! byte offset. Nothing here depends on Monaco; `web.rs` converts positions
//! and results.

use std::rc::Rc;

use serde::Serialize;

use crate::analysis::{Analysis, BindingKind, Ctx, analyze, eval_at, member_type, method_fn, parse, signal_method};
use crate::api::{MemberKind, api};
use crate::types::{FuncTy, Ty, table_fields, write_params};

/// Monaco's `CompletionItemKind` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(into = "u8")]
pub enum Kind {
    Method = 0,
    Function = 1,
    Field = 3,
    Variable = 4,
    Class = 5,
    Module = 8,
    Property = 9,
    Event = 10,
    Enum = 15,
    EnumMember = 16,
    Keyword = 17,
    Snippet = 27,
}

impl From<Kind> for u8 {
    fn from(k: Kind) -> u8 {
        k as u8
    }
}

#[derive(Clone, Debug)]
pub struct Completion {
    pub label: String,
    pub kind: Kind,
    pub detail: Option<String>,
    pub doc: Option<String>,
    pub insert: String,
    pub snippet: bool,
    pub sort: String,
    /// Byte offset where the replaced text starts (on the cursor's line).
    pub replace_from: usize,
    pub filter: Option<String>,
    /// Open parameter hints after inserting.
    pub params_hint: bool,
}

impl Completion {
    fn new(label: impl Into<String>, kind: Kind, replace_from: usize) -> Self {
        let label = label.into();
        Completion {
            insert: label.clone(),
            label,
            kind,
            detail: None,
            doc: None,
            snippet: false,
            sort: String::new(),
            replace_from,
            filter: None,
            params_hint: false,
        }
    }

    fn detail(mut self, d: impl Into<String>) -> Self {
        self.detail = Some(d.into());
        self
    }

    fn doc(mut self, d: Option<impl Into<String>>) -> Self {
        self.doc = d.map(Into::into);
        self
    }

    fn sort(mut self, group: u8) -> Self {
        self.sort = format!("{group}{}", self.label.to_lowercase());
        self
    }

    /// Inserts `name()` and places the cursor between the parentheses when
    /// the function takes arguments.
    fn call(mut self, f: &FuncTy) -> Self {
        if f.params.is_empty() && f.variadic.is_none() {
            self.insert = format!("{}()", self.label);
        } else {
            self.insert = format!("{}($0)", self.label);
            self.snippet = true;
            self.params_hint = true;
        }
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    pub label: String,
    /// Byte ranges of each parameter within `label`.
    pub params: Vec<(usize, usize)>,
    pub active: usize,
    pub doc: Option<String>,
}

const KEYWORDS: &[&str] = &[
    "and", "break", "continue", "do", "else", "elseif", "end", "export", "false", "for", "function", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "type", "typeof", "until", "while",
];

const SNIPPETS: &[(&str, &str, &str)] = &[
    ("for i", "for ${1:i} = ${2:1}, ${3:10} do\n\t$0\nend", "Numeric for loop"),
    ("for pairs", "for ${1:key}, ${2:value} in pairs(${3:t}) do\n\t$0\nend", "Loop over a table"),
    ("for ipairs", "for ${1:i}, ${2:v} in ipairs(${3:t}) do\n\t$0\nend", "Loop over an array"),
    ("function", "local function ${1:name}(${2})\n\t$0\nend", "Local function"),
    ("if", "if ${1:condition} then\n\t$0\nend", "If statement"),
    ("while", "while ${1:condition} do\n\t$0\nend", "While loop"),
    ("GetService", "local ${1:Players} = game:GetService(\"${1:Players}\")", "Get a service"),
];

// --- text helpers ------------------------------------------------------------

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

pub fn line_start(text: &str, offset: usize) -> usize {
    text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

/// Byte offset of a Monaco position (1-based line, UTF-16 column).
pub fn byte_offset(text: &str, line: u32, column: u32) -> usize {
    let mut offset = 0;
    for (i, l) in text.split('\n').enumerate() {
        if i + 1 == line as usize {
            let mut units = 0;
            for (b, ch) in l.char_indices() {
                if units + 1 >= column as usize {
                    return offset + b;
                }
                units += ch.len_utf16();
            }
            return offset + l.len();
        }
        offset += l.len() + 1;
    }
    text.len()
}

/// Monaco column (1-based, UTF-16) of a byte offset on the line of `offset`.
pub fn column_of(text: &str, at: usize) -> u32 {
    let ls = line_start(text, at);
    text[ls..at].encode_utf16().count() as u32 + 1
}

fn line_end(text: &str, offset: usize) -> usize {
    text[offset..].find('\n').map(|i| offset + i).unwrap_or(text.len())
}

/// Scans a line prefix for strings and comments. Returns whether the end is
/// inside a comment, and the position of an unterminated string's quote.
fn scan_line(line: &str) -> (bool, Option<usize>) {
    let b = line.as_bytes();
    let mut i = 0;
    let mut open: Option<(usize, u8)> = None;
    while i < b.len() {
        let c = b[i];
        match open {
            Some((_, q)) => {
                if c == b'\\' {
                    i += 1;
                } else if c == q {
                    open = None;
                }
            }
            None => {
                if c == b'-' && b.get(i + 1) == Some(&b'-') {
                    return (true, None);
                }
                if c == b'"' || c == b'\'' || c == b'`' {
                    open = Some((i, c));
                }
            }
        }
        i += 1;
    }
    (false, open.map(|(p, _)| p))
}

/// Index of the bracket opening the one closing at `close`.
fn match_open(b: &[u8], close: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut j = close + 1;
    while j > 0 {
        j -= 1;
        match b[j] {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            q @ (b'"' | b'\'') => loop {
                if j == 0 {
                    return None;
                }
                j -= 1;
                if b[j] == q && (j == 0 || b[j - 1] != b'\\') {
                    break;
                }
            },
            _ => {}
        }
    }
    None
}

/// The prefix expression (`a.b:c("x")[1]`) that ends at the end of `s`.
pub fn expr_before(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    let mut i = b.len();
    while i > 0 {
        let c = b[i - 1];
        let group;
        if is_ident(c) {
            while i > 0 && is_ident(b[i - 1]) {
                i -= 1;
            }
            group = false;
        } else if c == b')' || c == b']' {
            i = match_open(b, i - 1)?;
            group = true;
        } else if c == b'"' || c == b'\'' {
            let mut j = i - 1;
            loop {
                if j == 0 {
                    return None;
                }
                j -= 1;
                if b[j] == c && (j == 0 || b[j - 1] != b'\\') {
                    break;
                }
            }
            i = j;
            group = true;
        } else {
            break;
        }
        if i == 0 {
            break;
        }
        let p = b[i - 1];
        let double = i >= 2 && b[i - 2] == p;
        if (p == b'.' || p == b':') && !double {
            i -= 1;
            continue;
        }
        if group && (is_ident(p) || p == b')' || p == b']' || p == b'"' || p == b'\'') {
            continue;
        }
        break;
    }
    let e = s[i..].trim();
    if e.is_empty() || e.as_bytes()[0].is_ascii_digit() || KEYWORDS.contains(&e) {
        return None;
    }
    Some(e)
}

/// Splits `recv:method` / `recv.field` at the last top-level separator.
fn split_last_member(expr: &str) -> Option<(&str, char, &str)> {
    let b = expr.as_bytes();
    let name_len = b.iter().rev().take_while(|c| is_ident(**c)).count();
    let sep_at = b.len().checked_sub(name_len + 1)?;
    let sep = b[sep_at] as char;
    if (sep == '.' || sep == ':') && sep_at > 0 {
        Some((&expr[..sep_at], sep, &expr[sep_at + 1..]))
    } else {
        None
    }
}

/// Analyzes the document for a query at `offset`. If the file does not
/// parse (usually because of the half-typed line), the cursor's line is
/// blanked out so the rest of the file still yields its scopes.
fn analyze_for_cursor(text: &str, offset: usize, ctx: &Ctx) -> Analysis {
    let a = analyze(text, ctx, true);
    if a.errors == 0 {
        return a;
    }
    let ls = line_start(text, offset);
    let le = line_end(text, offset);
    let mut patched = String::with_capacity(text.len());
    patched.push_str(&text[..ls]);
    patched.push_str(&" ".repeat(le - ls));
    patched.push_str(&text[le..]);
    let b = analyze(&patched, ctx, true);
    if b.errors < a.errors { b } else { a }
}

// --- completions ---------------------------------------------------------------

pub fn complete(text: &str, offset: usize, ctx: &Ctx) -> Vec<Completion> {
    let ls = line_start(text, offset);
    let line = &text[ls..offset];
    let (in_comment, string_quote) = scan_line(line);
    if in_comment {
        return Vec::new();
    }
    if let Some(q) = string_quote {
        return string_completions(text, offset, ls + q, ctx);
    }
    let word_len = line.bytes().rev().take_while(|c| is_ident(*c)).count();
    let word_start = offset - word_len;
    if text[word_start..offset].bytes().next().is_some_and(|c| c.is_ascii_digit()) {
        return Vec::new();
    }
    let before = &text[ls..word_start];
    let sep = before.as_bytes().last().copied();
    let double = before.len() >= 2 && before.as_bytes()[before.len() - 2] == sep.unwrap_or(0);
    if matches!(sep, Some(b'.' | b':')) && !double {
        let Some(recv_src) = expr_before(&before[..before.len() - 1]) else { return Vec::new() };
        let analysis = analyze_for_cursor(text, offset, ctx);
        let recv = eval_at(&analysis, ctx, offset, recv_src);
        return member_completions(ctx, &recv, sep == Some(b':'), word_start);
    }
    let prev_word = before.trim_end().rsplit(|c: char| !(c.is_alphanumeric() || c == '_')).next().unwrap_or("");
    if matches!(prev_word, "local" | "function" | "for") && before.ends_with(char::is_whitespace) {
        return Vec::new();
    }
    let analysis = analyze_for_cursor(text, offset, ctx);
    scope_completions(&analysis, offset, word_start)
}

/// Completions inside a string literal: service names for `GetService("`,
/// class names for `Instance.new("` and `IsA("`, children for
/// `FindFirstChild("` and `x["`.
fn string_completions(text: &str, offset: usize, quote: usize, ctx: &Ctx) -> Vec<Completion> {
    let api = api();
    let from = quote + 1;
    let ls = line_start(text, offset);
    let before = text[ls..quote].trim_end();
    let Some(before_open) = before.strip_suffix('(').or_else(|| before.strip_suffix('[')) else { return Vec::new() };
    let is_index = before.ends_with('[');
    let callee = expr_before(before_open.trim_end());
    let classes = |pred: &dyn Fn(&str) -> bool, kind: Kind| -> Vec<Completion> {
        let mut names: Vec<&String> = api.classes.keys().filter(|c| pred(c)).collect();
        names.sort();
        names.into_iter().map(|c| Completion::new(c.clone(), kind, from)).collect()
    };
    let analysis = || analyze_for_cursor(text, offset, ctx);
    if is_index {
        let Some(recv_src) = callee else { return Vec::new() };
        let recv = eval_at(&analysis(), ctx, offset, recv_src);
        return child_completions(ctx, &recv, from, false);
    }
    let Some(callee) = callee else { return Vec::new() };
    if callee == "Instance.new" {
        return api.creatable.iter().map(|c| Completion::new(c.clone(), Kind::Class, from)).collect();
    }
    let Some((recv_src, ':', method)) = split_last_member(callee) else { return Vec::new() };
    match method {
        "GetService" | "FindService" => {
            api.services.iter().map(|s| Completion::new(s.clone(), Kind::Module, from)).collect()
        }
        "IsA" | "FindFirstChildOfClass" | "FindFirstChildWhichIsA" | "FindFirstAncestorOfClass"
        | "FindFirstAncestorWhichIsA" => classes(&|c| !api.classes[c].deprecated, Kind::Class),
        "FindFirstChild" | "WaitForChild" | "FindFirstDescendant" => {
            let recv = eval_at(&analysis(), ctx, offset, recv_src);
            child_completions(ctx, &recv, from, false)
        }
        "GetPropertyChangedSignal" => {
            let recv = eval_at(&analysis(), ctx, offset, recv_src);
            let Ty::Instance(class, _) = recv.base() else { return Vec::new() };
            api.class_members(class)
                .into_iter()
                .filter(|(m, _)| matches!(m.kind, MemberKind::Property { .. }))
                .map(|(m, _)| Completion::new(&*m.name, Kind::Property, from).detail(m.ty().display()))
                .collect()
        }
        "GetAttribute" | "SetAttribute" | "GetAttributeChangedSignal" => Vec::new(),
        _ => Vec::new(),
    }
}

fn child_completions(ctx: &Ctx, recv: &Ty, from: usize, as_member: bool) -> Vec<Completion> {
    let Ty::Instance(class, Some(node)) = recv.base() else { return Vec::new() };
    let api = api();
    let tree = ctx.tree;
    let mut out = Vec::new();
    for &c in &tree.node(*node).children {
        let child = tree.node(c);
        if as_member && api.class_member(class, &child.name).is_some() {
            continue;
        }
        if out.iter().any(|o: &Completion| o.label == child.name) {
            continue;
        }
        let mut item = Completion::new(child.name.clone(), Kind::Field, from).detail(child.class.clone()).sort(0);
        if as_member && !is_identifier(&child.name) {
            // `parent.["My Part"]` is not valid; replace the dot too.
            item.replace_from = from - 1;
            item.insert = format!("[\"{}\"]", child.name.replace('\\', "\\\\").replace('"', "\\\""));
            item.filter = Some(format!(".{}", child.name));
        }
        out.push(item);
    }
    out
}

fn is_identifier(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty() && !b[0].is_ascii_digit() && b.iter().all(|c| is_ident(*c)) && !KEYWORDS.contains(&s)
}

fn func_detail(f: &FuncTy) -> String {
    let mut s = String::from("(");
    write_params(&mut s, f, 0);
    s.push_str("): ");
    s.push_str(&ret_display(&f.ret));
    s
}

/// A return type for signatures: nothing is shown as `()`.
fn ret_display(t: &Ty) -> String {
    match t {
        Ty::Nil => "()".to_owned(),
        t => t.display(),
    }
}

fn member_completions(ctx: &Ctx, recv: &Ty, colon: bool, from: usize) -> Vec<Completion> {
    let api = api();
    let mut out = Vec::new();
    match recv.base() {
        Ty::Instance(class, _) => {
            if !colon {
                out.extend(child_completions(ctx, recv, from, true));
            }
            for (m, owner) in api.class_members(class) {
                let item = match (&m.kind, colon) {
                    (MemberKind::Function(f), true) => Completion::new(&*m.name, Kind::Method, from)
                        .detail(func_detail(f))
                        .doc(Some(member_doc(owner, m.doc())))
                        .call(f),
                    (MemberKind::Property { ty, read_only }, false) => Completion::new(&*m.name, Kind::Property, from)
                        .detail(format!("{}{}", ty.display(), if *read_only { " (read-only)" } else { "" }))
                        .doc(Some(member_doc(owner, None))),
                    (MemberKind::Event(_), false) => Completion::new(&*m.name, Kind::Event, from)
                        .detail(m.ty().display())
                        .doc(Some(member_doc(owner, None))),
                    (MemberKind::Callback(f), false) => Completion::new(&*m.name, Kind::Property, from)
                        .detail(format!("callback {}", func_detail(f)))
                        .doc(Some(member_doc(owner, None))),
                    _ => continue,
                };
                out.push(item.sort(1));
            }
        }
        Ty::Data(dt) => {
            for m in api.datatypes.get(&**dt).into_iter().flatten() {
                match (&m.kind, colon) {
                    (MemberKind::Function(f), true) if f.method => {
                        out.push(Completion::new(&*m.name, Kind::Method, from).detail(func_detail(f)).doc(m.doc()).call(f))
                    }
                    (MemberKind::Property { ty, .. }, false) => {
                        out.push(Completion::new(&*m.name, Kind::Property, from).detail(ty.display()))
                    }
                    _ => {}
                }
            }
        }
        Ty::Static(t) if !colon => {
            for m in api.statics.get(&**t).into_iter().flatten() {
                out.push(match &m.kind {
                    MemberKind::Function(f) => {
                        Completion::new(&*m.name, Kind::Function, from).detail(func_detail(f)).doc(m.doc()).call(f)
                    }
                    _ => Completion::new(&*m.name, Kind::Property, from).detail(m.ty().display()),
                });
            }
        }
        Ty::String if colon => {
            for m in api.statics.get("string").into_iter().flatten() {
                if let MemberKind::Function(f) = &m.kind {
                    let f = FuncTy { params: f.params.iter().skip(1).cloned().collect(), ..(**f).clone() };
                    out.push(Completion::new(&*m.name, Kind::Method, from).detail(func_detail(&f)).call(&f));
                }
            }
        }
        Ty::Enums if !colon => {
            for e in api.enums.keys() {
                out.push(Completion::new(e.clone(), Kind::Enum, from));
            }
        }
        Ty::EnumType(e) => {
            if colon {
                if let Some(f) = method_fn(recv, "GetEnumItems") {
                    out.push(Completion::new("GetEnumItems", Kind::Method, from).detail(func_detail(&f)).call(&f));
                }
            } else {
                for item in api.enums.get(&**e).into_iter().flatten() {
                    out.push(Completion::new(item.clone(), Kind::EnumMember, from).detail(format!("Enum.{e}")));
                }
            }
        }
        Ty::EnumItem(e) if !colon => {
            out.push(Completion::new("Name", Kind::Property, from).detail("string"));
            out.push(Completion::new("Value", Kind::Property, from).detail("number"));
            out.push(Completion::new("EnumType", Kind::Property, from).detail(format!("Enum.{e}")));
        }
        Ty::Signal(ps) if colon => {
            for name in ["Connect", "Once", "Wait", "ConnectParallel"] {
                if let Some(f) = signal_method(ps, name) {
                    out.push(Completion::new(name, Kind::Method, from).detail(func_detail(&f)).doc(f.doc.as_deref()).call(&f));
                }
            }
        }
        Ty::Table(id) => {
            for (name, ty) in table_fields(*id) {
                match (&ty, colon) {
                    (Ty::Function(_), true) => {
                        if let Some(f) = method_fn(recv, &name) {
                            out.push(Completion::new(name, Kind::Method, from).detail(func_detail(&f)).call(&f));
                        }
                    }
                    (Ty::Function(f), false) => {
                        out.push(Completion::new(name, Kind::Function, from).detail(func_detail(f)).call(f))
                    }
                    (_, false) => out.push(Completion::new(name, Kind::Field, from).detail(ty.display())),
                    _ => {}
                }
            }
        }
        _ => {}
    }
    out
}

fn member_doc(owner: &str, doc: Option<&str>) -> String {
    match doc {
        Some(d) => format!("{owner} · {d}"),
        None => owner.to_owned(),
    }
}

fn scope_completions(analysis: &Analysis, offset: usize, from: usize) -> Vec<Completion> {
    let api = api();
    let mut out: Vec<Completion> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for b in analysis.visible_at(offset) {
        if !seen.insert(b.name.clone()) {
            continue;
        }
        let kind = match (&b.ty, b.kind) {
            (Ty::Function(_), _) | (_, BindingKind::Function) => Kind::Function,
            _ => Kind::Variable,
        };
        let detail = match (&b.ty, b.kind) {
            (Ty::Function(f), _) => func_detail(f),
            (t, _) => t.display(),
        };
        out.push(Completion::new(b.name.clone(), kind, from).detail(detail).sort(0));
    }
    let mut global = |name: &str, kind: Kind, detail: String, doc: Option<&str>| {
        if seen.insert(name.to_owned()) {
            out.push(Completion::new(name, kind, from).detail(detail).doc(doc).sort(1));
        }
    };
    global("game", Kind::Variable, "DataModel".into(), Some("The root of the place."));
    global("workspace", Kind::Variable, "Workspace".into(), Some("The Workspace service."));
    global("script", Kind::Variable, "the running script".into(), None);
    global("Enum", Kind::Enum, "Enums".into(), None);
    for (m, doc) in &api.globals {
        match &m.kind {
            MemberKind::Function(f) => global(&m.name, Kind::Function, func_detail(f), f.doc.as_deref()),
            _ => global(&m.name, Kind::Variable, m.ty().display(), doc.as_deref()),
        }
    }
    let mut statics: Vec<&String> = api.statics.keys().collect();
    statics.sort();
    for s in statics {
        let kind = if api.is_datatype(s) { Kind::Class } else { Kind::Module };
        global(s, kind, if kind == Kind::Class { "datatype".into() } else { "library".into() }, None);
    }
    for k in KEYWORDS {
        if seen.insert((*k).to_owned()) {
            out.push(Completion::new(*k, Kind::Keyword, from).sort(2));
        }
    }
    for (label, body, doc) in SNIPPETS {
        let mut c = Completion::new(*label, Kind::Snippet, from).detail(*doc).sort(3);
        c.insert = (*body).to_owned();
        c.snippet = true;
        out.push(c);
    }
    out
}

// --- hover -------------------------------------------------------------------

/// Markdown describing the identifier at `offset`.
pub fn hover(text: &str, offset: usize, ctx: &Ctx) -> Option<String> {
    let b = text.as_bytes();
    let mut s = offset.min(b.len());
    while s > 0 && is_ident(b[s - 1]) {
        s -= 1;
    }
    let mut e = offset.min(b.len());
    while e < b.len() && is_ident(b[e]) {
        e += 1;
    }
    if s == e || b[s].is_ascii_digit() {
        return None;
    }
    let word = &text[s..e];
    if KEYWORDS.contains(&word) {
        return None;
    }
    let ls = line_start(text, s);
    let (in_comment, in_string) = scan_line(&text[ls..s]);
    if in_comment || in_string.is_some() {
        return None;
    }
    let analysis = analyze(text, ctx, true);
    let before = &text[ls..s];
    let sep = before.as_bytes().last().copied();
    let double = before.len() >= 2 && before.as_bytes()[before.len() - 2] == sep.unwrap_or(0);
    if matches!(sep, Some(b'.' | b':')) && !double {
        let recv_src = expr_before(&before[..before.len() - 1])?;
        let recv = eval_at(&analysis, ctx, s, recv_src);
        return member_hover(ctx, &recv, word, sep == Some(b':'));
    }
    let code = |s: String| Some(format!("```luau\n{s}\n```"));
    if let Some(b) = analysis.visible_at(s).into_iter().find(|b| b.name == word) {
        return match (&b.ty, b.kind) {
            (Ty::Function(f), _) => code(format!("function {word}{}", func_detail(f))),
            (t, BindingKind::Param) => code(format!("(parameter) {word}: {}", t.display())),
            (t, BindingKind::Global) => code(format!("(global) {word}: {}", t.display())),
            (t, _) => code(format!("local {word}: {}", t.display())),
        };
    }
    let api = api();
    if let Some(m) = api.global(word) {
        let doc = api.globals.iter().find(|(g, _)| &*g.name == word).and_then(|(_, d)| d.clone());
        let sig = match &m.kind {
            MemberKind::Function(f) => format!("function {word}{}", func_detail(f)),
            _ => format!("(global) {word}: {}", m.ty().display()),
        };
        return Some(with_doc(code(sig)?, doc.as_deref()));
    }
    match word {
        "game" => code("(global) game: DataModel".into()),
        "workspace" => code("(global) workspace: Workspace".into()),
        "script" => {
            let t = eval_at(&analysis, ctx, s, "script");
            code(format!("(global) script: {}", t.display()))
        }
        "Enum" => code("(global) Enum: Enums".into()),
        _ if api.statics.contains_key(word) => {
            code(format!("{} {word}", if api.is_datatype(word) { "(datatype)" } else { "(library)" }))
        }
        _ if api.class(word).is_some() => {
            let c = api.class(word)?;
            code(format!("class {word}{}", c.superclass.as_ref().map(|s| format!(" : {s}")).unwrap_or_default()))
        }
        _ => None,
    }
}

fn with_doc(s: String, doc: Option<&str>) -> String {
    match doc {
        Some(d) => format!("{s}\n\n{d}"),
        None => s,
    }
}

fn member_hover(ctx: &Ctx, recv: &Ty, name: &str, colon: bool) -> Option<String> {
    let api = api();
    let code = |s: String| format!("```luau\n{s}\n```");
    if let Ty::Instance(class, node) = recv.base() {
        if let Some((m, owner)) = api.class_member(class, name) {
            let s = match &m.kind {
                MemberKind::Property { ty, read_only } => {
                    let ty = if name == "Parent" { member_type(ctx.tree, recv, name) } else { ty.clone() };
                    format!("(property) {owner}.{name}: {}{}", ty.display(), if *read_only { "  -- read-only" } else { "" })
                }
                MemberKind::Function(f) => format!("(method) {owner}:{name}{}", func_detail(f)),
                MemberKind::Event(_) => format!("(event) {owner}.{name}: {}", m.ty().display()),
                MemberKind::Callback(f) => format!("(callback) {owner}.{name}{}", func_detail(f)),
            };
            return Some(with_doc(code(s), m.doc()));
        }
        if let Some(c) = node.and_then(|n| ctx.tree.child(n, name)) {
            let n = ctx.tree.node(c);
            return Some(code(format!("(instance) {}: {}", ctx.tree.full_name(c), n.class)));
        }
        return None;
    }
    if colon && let Some(f) = method_fn(recv, name) {
        return Some(with_doc(code(format!("(method) {name}{}", func_detail(&f))), f.doc.as_deref()));
    }
    let t = member_type(ctx.tree, recv, name);
    if t.is_unknown() {
        return None;
    }
    let s = match (&t, recv.base()) {
        (Ty::Function(f), _) => return Some(with_doc(code(format!("function {name}{}", func_detail(f))), f.doc.as_deref())),
        (_, Ty::EnumType(e)) => format!("Enum.{e}.{name}"),
        (_, Ty::Enums) => format!("Enum.{name}"),
        (t, _) => format!("(field) {name}: {}", t.display()),
    };
    Some(code(s))
}

// --- signature help ---------------------------------------------------------------

/// The call surrounding `offset` and which argument the cursor is in.
pub fn signature_help(text: &str, offset: usize, ctx: &Ctx) -> Option<Signature> {
    // Scan forward from a window before the cursor, tracking open parens.
    let window_start = line_start(text, offset.saturating_sub(4000));
    let b = text.as_bytes();
    let mut stack: Vec<(usize, usize)> = Vec::new(); // (paren position, commas)
    let mut i = window_start;
    while i < offset {
        let c = b[i];
        match c {
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < offset && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'"' | b'\'' | b'`' => {
                i += 1;
                while i < offset && b[i] != c && b[i] != b'\n' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'(' | b'{' | b'[' => stack.push((i, 0)),
            b')' | b'}' | b']' => {
                stack.pop();
            }
            b',' => {
                if let Some(top) = stack.last_mut() {
                    top.1 += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    let &(paren, commas) = stack.last()?;
    if b[paren] != b'(' {
        return None;
    }
    let ls = line_start(text, paren);
    let callee = expr_before(text[ls..paren].trim_end())?;
    let analysis = analyze_for_cursor(text, offset, ctx);
    let (name, f) = match split_last_member(callee) {
        Some((recv_src, ':', method)) => {
            let recv = eval_at(&analysis, ctx, offset, recv_src);
            (method.to_owned(), method_fn(&recv, method)?)
        }
        _ => {
            let name = callee.rsplit(['.', ':']).next().unwrap_or(callee).to_owned();
            match eval_at(&analysis, ctx, offset, callee).base() {
                Ty::Function(f) => (name, f.clone()),
                _ => return None,
            }
        }
    };
    Some(build_signature(&name, &f, commas))
}

fn build_signature(name: &str, f: &Rc<FuncTy>, active: usize) -> Signature {
    let mut label = format!("{name}(");
    let mut params = Vec::new();
    for (i, p) in f.params.iter().enumerate() {
        if i > 0 {
            label.push_str(", ");
        }
        let start = label.len();
        label.push_str(&p.name);
        label.push_str(": ");
        label.push_str(&p.ty.display());
        if p.optional && !matches!(p.ty, Ty::Optional(_)) {
            label.push('?');
        }
        params.push((start, label.len()));
    }
    if let Some(v) = &f.variadic {
        if !f.params.is_empty() {
            label.push_str(", ");
        }
        let start = label.len();
        label.push_str("...: ");
        label.push_str(&v.display());
        params.push((start, label.len()));
    }
    label.push_str("): ");
    label.push_str(&ret_display(&f.ret));
    let active = if f.variadic.is_some() { active.min(params.len().saturating_sub(1)) } else { active };
    Signature { label, params, active, doc: f.doc.as_deref().map(str::to_owned) }
}

// --- auto-closing blocks ------------------------------------------------------

/// Whether a line opens a block that needs an `end`: it ends with `then` or
/// `do`, or with a function header (`function f(a)`, `= function()`,
/// `function(x): number`).
fn opens_block(line: &str) -> bool {
    let code = match scan_line(line) {
        (true, _) => line.find("--").map(|i| &line[..i]).unwrap_or(line),
        _ => line,
    };
    let code = code.trim_end();
    let last_word = code.rsplit(|c: char| !(c.is_alphanumeric() || c == '_')).next().unwrap_or("");
    if matches!(last_word, "then" | "do") {
        return true;
    }
    let Some(f) = code.rfind("function") else { return false };
    let before_ok = f == 0 || !is_ident(code.as_bytes()[f - 1]);
    let rest = &code[f + "function".len()..];
    let after_ok = rest.bytes().next().is_none_or(|c| !is_ident(c));
    let Some(close) = rest.rfind(')') else { return false };
    let tail = rest[close + 1..].trim();
    before_ok && after_ok && rest.contains('(') && (tail.is_empty() || (tail.starts_with(':') && !tail.contains('=')))
}

/// What to insert after Enter was pressed at the end of an opening line:
/// `text` already contains the new line, and `insert_at` is the end of that
/// line. Returns `end` (or `end)` when the block is a function argument)
/// only if the parser agrees the block is unclosed.
pub fn block_closer(text: &str, insert_at: usize, opener_line: &str) -> Option<&'static str> {
    if !opens_block(opener_line) {
        return None;
    }
    let base = parse(text).errors().len();
    if base == 0 {
        return None;
    }
    let indent: String = opener_line.chars().take_while(|c| c.is_whitespace()).collect();
    let mut best: Option<(&'static str, usize)> = None;
    for closer in ["end", "end)", "end))", "end}"] {
        let candidate = format!("{}\n{indent}{closer}{}", &text[..insert_at], &text[insert_at..]);
        let errors = parse(&candidate).errors().len();
        if errors < best.map_or(base, |(_, e)| e) {
            best = Some((closer, errors));
        }
    }
    best.map(|(c, _)| c)
}
