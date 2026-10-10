//! Walks a Luau chunk and infers the types of its locals.
//!
//! This is deliberately a forward, single-pass inference rather than a real
//! type checker: each local gets the type of the expression it was first
//! given (or its annotation), tables collect the fields assigned to them,
//! functions get the type of their first `return`, and Roblox calls with
//! well-known semantics (`GetService`, `FindFirstChild`, `Instance.new`,
//! `require`, signal `Connect`, ...) are special-cased. That covers what
//! completions and hovers need while staying fast enough to re-run on every
//! keystroke.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use full_moon::LuaVersion;
use full_moon::ast::{
    self, BinOp, Call, Expression, Field, FunctionArgs, Index, LastStmt, Parameter, Prefix, Stmt, Suffix, UnOp, Var,
    luau::{IndexedTypeInfo, TypeFieldKey, TypeInfo},
};
use full_moon::node::Node;
use full_moon::tokenizer::{Symbol, TokenReference, TokenType};

use crate::api::{Api, MemberKind, api};
use crate::tree::Tree;
use crate::types::{FuncTy, Param, TableId, Ty, new_table, parse_type, table_field, with_table, with_table_mut};

const MAX_DEPTH: u32 = 150;
const MAX_REQUIRE_DEPTH: u32 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingKind {
    Local,
    Param,
    Function,
    Global,
}

/// A name and the source range it is visible in.
#[derive(Clone, Debug)]
pub struct Binding {
    pub name: String,
    pub ty: Ty,
    pub from: usize,
    pub to: usize,
    pub kind: BindingKind,
}

pub struct Analysis {
    pub bindings: Vec<Binding>,
    /// What the chunk returns (a ModuleScript's value).
    pub ret: Ty,
    pub aliases: HashMap<String, Ty>,
    pub errors: usize,
}

impl Analysis {
    /// The bindings visible at a byte offset, innermost first per name.
    pub fn visible_at(&self, offset: usize) -> Vec<&Binding> {
        let mut out: Vec<&Binding> = Vec::new();
        for b in &self.bindings {
            if b.from > offset || offset > b.to {
                continue;
            }
            match out.iter_mut().find(|o| o.name == b.name) {
                Some(o) if o.from <= b.from => *o = b,
                Some(_) => {}
                None => out.push(b),
            }
        }
        out
    }
}

/// Results of analyzing ModuleScripts, keyed by tree node. `None` marks a
/// module being analyzed (a `require` cycle).
#[derive(Default)]
pub struct ModuleCache {
    map: RefCell<HashMap<usize, Option<Ty>>>,
}

impl ModuleCache {
    pub fn clear(&self) {
        self.map.borrow_mut().clear();
    }
}

pub struct Ctx<'a> {
    pub tree: &'a Tree,
    /// Tree node of the script being analyzed.
    pub script: Option<usize>,
    pub modules: &'a ModuleCache,
    pub depth: u32,
}

pub fn parse(src: &str) -> full_moon::ast::AstResult {
    full_moon::parse_fallible(src, LuaVersion::luau())
}

/// Analyzes a whole chunk. With `record`, every binding is kept for later
/// position queries; otherwise only the return type matters.
pub fn analyze(src: &str, ctx: &Ctx, record: bool) -> Analysis {
    let parsed = parse(src);
    let mut w = Walker::new(ctx, record);
    w.scopes.push(Vec::new());
    w.stmts(parsed.ast().nodes(), usize::MAX);
    Analysis {
        bindings: w.bindings,
        ret: w.chunk_ret.unwrap_or(Ty::Any),
        aliases: w.aliases,
        errors: parsed.errors().len(),
    }
}

/// Evaluates a standalone expression (e.g. the receiver before a `.` at the
/// cursor) in the scope visible at `offset`.
pub fn eval_at(analysis: &Analysis, ctx: &Ctx, offset: usize, expr: &str) -> Ty {
    let mut w = Walker::new(ctx, false);
    w.aliases = analysis.aliases.clone();
    w.scopes.push(
        analysis
            .visible_at(offset)
            .into_iter()
            .map(|b| Local { name: b.name.clone(), ty: b.ty.clone(), to: b.to, binding: None })
            .collect(),
    );
    let parsed = parse(&format!("return {expr}\n"));
    match parsed.ast().nodes().last_stmt() {
        Some(LastStmt::Return(r)) => match r.returns().iter().next() {
            Some(e) => w.eval(e),
            None => Ty::Any,
        },
        _ => Ty::Any,
    }
}

/// The function called by `recv:name(...)`, if known.
pub fn method_fn(recv: &Ty, name: &str) -> Option<Rc<FuncTy>> {
    let api = api();
    match recv.base() {
        Ty::Instance(class, _) => match api.class_member(class, name)?.0.kind {
            MemberKind::Function(ref f) => Some(f.clone()),
            _ => None,
        },
        Ty::Signal(ps) => signal_method(ps, name),
        Ty::Data(dt) => match &api.datatype_member(dt, name)?.kind {
            MemberKind::Function(f) if f.method => Some(f.clone()),
            _ => None,
        },
        Ty::String => match &api.static_member("string", name)?.kind {
            MemberKind::Function(f) => Some(Rc::new(drop_self(f))),
            _ => None,
        },
        Ty::EnumType(e) if name == "GetEnumItems" => Some(Rc::new(FuncTy {
            ret: Ty::Array(Rc::new(Ty::EnumItem(e.clone()))),
            method: true,
            ..Default::default()
        })),
        Ty::Table(id) => match table_field(*id, name)? {
            Ty::Function(f) if f.method => Some(f),
            Ty::Function(f) => Some(Rc::new(drop_self(&f))),
            _ => None,
        },
        _ => None,
    }
}

pub fn signal_method(params: &Rc<[Param]>, name: &str) -> Option<Rc<FuncTy>> {
    let callback = Ty::Function(Rc::new(FuncTy { params: params.to_vec(), ..Default::default() }));
    let f = match name {
        "Connect" | "Once" | "ConnectParallel" => FuncTy {
            params: vec![Param { name: "callback".into(), ty: callback, optional: false }],
            ret: Ty::Data("RBXScriptConnection".into()),
            method: true,
            doc: Some(match name {
                "Connect" => "Calls the function every time the signal fires.".into(),
                "Once" => "Calls the function the next time the signal fires, then disconnects.".into(),
                _ => "Like Connect, but runs the function in parallel.".into(),
            }),
            variadic: None,
        },
        "Wait" => FuncTy {
            ret: match params.len() {
                0 => Ty::Nil,
                1 => params[0].ty.clone(),
                _ => Ty::Tuple(params.iter().map(|p| p.ty.clone()).collect()),
            },
            method: true,
            doc: Some("Yields until the signal fires and returns its arguments.".into()),
            ..Default::default()
        },
        _ => return None,
    };
    Some(Rc::new(f))
}

fn drop_self(f: &FuncTy) -> FuncTy {
    FuncTy { params: f.params.iter().skip(1).cloned().collect(), method: true, ..f.clone() }
}

// --- the walker ------------------------------------------------------------------

struct Local {
    name: String,
    ty: Ty,
    to: usize,
    /// Index into `Walker::bindings`, when recording.
    binding: Option<usize>,
}

/// Remembers which global function or static a value came from, so calls to
/// it can be special-cased (`require(...)`, `Instance.new(...)`).
#[derive(Clone)]
enum Callee {
    None,
    Global(String),
    Static(Rc<str>, String),
}

struct Walker<'c, 'a> {
    ctx: &'c Ctx<'a>,
    api: &'static Api,
    scopes: Vec<Vec<Local>>,
    bindings: Vec<Binding>,
    record: bool,
    /// Inferred return type of each function being walked.
    returns: Vec<Option<Ty>>,
    chunk_ret: Option<Ty>,
    aliases: HashMap<String, Ty>,
    depth: u32,
}

fn ident(t: &TokenReference) -> String {
    t.token().to_string()
}

fn tok_start(t: &TokenReference) -> usize {
    t.token().start_position().bytes()
}

/// Where a block ending at `t` stops: phantom tokens from error recovery
/// have no position, so an unterminated block extends to the end of file.
fn scope_end(t: &TokenReference) -> usize {
    match tok_start(t) {
        0 => usize::MAX,
        b => b,
    }
}

fn node_end(n: &impl Node) -> usize {
    n.end_position().map(|p| p.bytes()).unwrap_or(0)
}

fn node_start(n: &impl Node) -> usize {
    n.start_position().map(|p| p.bytes()).unwrap_or(0)
}

pub fn string_literal(t: &TokenReference) -> Option<String> {
    match t.token().token_type() {
        TokenType::StringLiteral { literal, .. } => Some(literal.to_string()),
        _ => None,
    }
}

fn expr_string(e: &Expression) -> Option<String> {
    match e {
        Expression::String(t) => string_literal(t),
        Expression::Parentheses { expression, .. } => expr_string(expression),
        _ => None,
    }
}

fn arg_string(args: &FunctionArgs, i: usize) -> Option<String> {
    match args {
        FunctionArgs::Parentheses { arguments, .. } => arguments.iter().nth(i).and_then(expr_string),
        FunctionArgs::String(t) if i == 0 => string_literal(t),
        _ => None,
    }
}

fn arg_count(args: &FunctionArgs) -> usize {
    match args {
        FunctionArgs::Parentheses { arguments, .. } => arguments.len(),
        _ => 1,
    }
}

impl<'c, 'a> Walker<'c, 'a> {
    fn new(ctx: &'c Ctx<'a>, record: bool) -> Self {
        Walker {
            ctx,
            api: api(),
            scopes: Vec::new(),
            bindings: Vec::new(),
            record,
            returns: Vec::new(),
            chunk_ret: None,
            aliases: HashMap::new(),
            depth: 0,
        }
    }

    // --- scopes ---

    fn declare(&mut self, name: &str, ty: Ty, from: usize, to: usize, kind: BindingKind) {
        let binding = self.record.then(|| {
            self.bindings.push(Binding { name: name.to_owned(), ty: ty.clone(), from, to, kind });
            self.bindings.len() - 1
        });
        if let Some(scope) = self.scopes.last_mut() {
            scope.push(Local { name: name.to_owned(), ty, to, binding });
        }
    }

    fn declare_global(&mut self, name: &str, ty: Ty, kind: BindingKind) {
        let binding = self.record.then(|| {
            self.bindings.push(Binding { name: name.to_owned(), ty: ty.clone(), from: 0, to: usize::MAX, kind });
            self.bindings.len() - 1
        });
        if let Some(scope) = self.scopes.first_mut() {
            scope.push(Local { name: name.to_owned(), ty, to: usize::MAX, binding });
        }
    }

    fn find_local(&mut self, name: &str) -> Option<&mut Local> {
        self.scopes.iter_mut().rev().flat_map(|s| s.iter_mut().rev()).find(|l| l.name == name)
    }

    /// Gives an existing local a better type from here on.
    fn refine_local(&mut self, name: &str, ty: Ty, from: usize) {
        let record = self.record;
        let Some(local) = self.find_local(name) else { return };
        if !local.ty.is_unknown() || ty.is_unknown() {
            return;
        }
        local.ty = ty.clone();
        let to = local.to;
        if record {
            self.bindings.push(Binding { name: name.to_owned(), ty, from, to, kind: BindingKind::Local });
        }
    }

    fn set_local_type(&mut self, name: &str, ty: Ty) {
        let Some(local) = self.find_local(name) else { return };
        local.ty = ty.clone();
        if let Some(b) = local.binding {
            self.bindings[b].ty = ty;
        }
    }

    fn lookup(&self, name: &str) -> Option<Ty> {
        self.scopes.iter().rev().flat_map(|s| s.iter().rev()).find(|l| l.name == name).map(|l| l.ty.clone())
    }

    fn global(&self, name: &str) -> Ty {
        let tree = self.ctx.tree;
        match name {
            "script" => match self.ctx.script {
                Some(n) => Ty::Instance(tree.node(n).class.as_str().into(), Some(n)),
                None => Ty::instance("Script"),
            },
            "game" => Ty::Instance("DataModel".into(), tree.root()),
            "workspace" | "Workspace" => Ty::Instance("Workspace".into(), tree.service("Workspace")),
            "Enum" => Ty::Enums,
            _ => {
                if let Some(m) = self.api.global(name) {
                    m.ty()
                } else if self.api.statics.contains_key(name) {
                    Ty::Static(name.into())
                } else {
                    Ty::Any
                }
            }
        }
    }

    // --- statements ---

    fn block(&mut self, block: &ast::Block, end: usize) {
        self.scopes.push(Vec::new());
        self.stmts(block, end);
        self.scopes.pop();
    }

    fn stmts(&mut self, block: &ast::Block, end: usize) {
        for stmt in block.stmts() {
            self.stmt(stmt, end);
        }
        if let Some(LastStmt::Return(r)) = block.last_stmt() {
            let ty = self.expr_list_ty(r.returns().iter().collect());
            match self.returns.last_mut() {
                Some(slot) => {
                    if slot.as_ref().is_none_or(Ty::is_unknown) {
                        *slot = Some(ty);
                    }
                }
                None => {
                    if self.chunk_ret.is_none() {
                        self.chunk_ret = Some(ty);
                    }
                }
            }
        }
    }

    fn stmt(&mut self, stmt: &Stmt, end: usize) {
        match stmt {
            Stmt::LocalAssignment(la) => {
                let names: Vec<String> = la.names().iter().map(ident).collect();
                let annotations: Vec<Option<Ty>> =
                    la.type_specifiers().map(|t| t.map(|t| self.type_info(t.type_info()))).collect();
                let values = self.expr_list(la.expressions().iter().collect(), names.len());
                let from = node_end(la).max(node_start(la));
                for (i, n) in names.iter().enumerate() {
                    let ty = annotations.get(i).cloned().flatten().unwrap_or_else(|| values[i].clone());
                    self.declare(n, ty, from, end, BindingKind::Local);
                }
            }
            Stmt::ConstAssignment(ca) => {
                let names: Vec<String> = ca.names().iter().map(ident).collect();
                let annotations: Vec<Option<Ty>> =
                    ca.type_specifiers().map(|t| t.map(|t| self.type_info(t.type_info()))).collect();
                let values = self.expr_list(ca.expressions().iter().collect(), names.len());
                let from = node_end(ca).max(node_start(ca));
                for (i, n) in names.iter().enumerate() {
                    let ty = annotations.get(i).cloned().flatten().unwrap_or_else(|| values[i].clone());
                    self.declare(n, ty, from, end, BindingKind::Local);
                }
            }
            Stmt::LocalFunction(lf) => self.local_function(lf.name(), lf.body(), end),
            Stmt::ConstFunction(cf) => self.local_function(cf.name(), cf.body(), end),
            Stmt::FunctionDeclaration(fd) => self.function_declaration(fd),
            Stmt::Assignment(a) => {
                let vars: Vec<&Var> = a.variables().iter().collect();
                let values = self.expr_list(a.expressions().iter().collect(), vars.len());
                let from = node_end(a);
                for (var, value) in vars.into_iter().zip(values) {
                    self.assign(var, value, from);
                }
            }
            Stmt::CompoundAssignment(ca) => {
                self.eval(ca.rhs());
            }
            Stmt::FunctionCall(fc) => {
                self.chain(fc.prefix(), fc.suffixes().collect());
            }
            Stmt::Do(d) => self.block(d.block(), scope_end(d.end_token())),
            Stmt::While(w) => {
                self.eval(w.condition());
                self.block(w.block(), scope_end(w.end_token()));
            }
            Stmt::Repeat(r) => {
                self.scopes.push(Vec::new());
                self.stmts(r.block(), node_end(r.until()).max(tok_start(r.until_token())));
                self.eval(r.until());
                self.scopes.pop();
            }
            Stmt::If(i) => {
                self.eval(i.condition());
                // Each branch ends where the next one starts.
                let mut branches: Vec<(&ast::Block, usize)> = Vec::new();
                let mut next_starts: Vec<usize> = Vec::new();
                if let Some(eifs) = i.else_if() {
                    for e in eifs {
                        next_starts.push(tok_start(e.else_if_token()));
                    }
                }
                if let Some(t) = i.else_token() {
                    next_starts.push(tok_start(t));
                }
                next_starts.push(scope_end(i.end_token()));
                let fix = |b: usize| if b == 0 { usize::MAX } else { b };
                branches.push((i.block(), fix(next_starts[0])));
                if let Some(eifs) = i.else_if() {
                    for (k, e) in eifs.iter().enumerate() {
                        self.eval(e.condition());
                        branches.push((e.block(), fix(next_starts[k + 1])));
                    }
                }
                if let Some(b) = i.else_block() {
                    branches.push((b, scope_end(i.end_token())));
                }
                for (b, end) in branches {
                    self.block(b, end);
                }
            }
            Stmt::NumericFor(f) => {
                self.eval(f.start());
                self.eval(f.end());
                if let Some(s) = f.step() {
                    self.eval(s);
                }
                let to = scope_end(f.end_token());
                self.scopes.push(Vec::new());
                let ty = f.type_specifier().map(|t| self.type_info(t.type_info())).unwrap_or(Ty::Number);
                self.declare(&ident(f.index_variable()), ty, tok_start(f.do_token()), to, BindingKind::Local);
                self.stmts(f.block(), to);
                self.scopes.pop();
            }
            Stmt::GenericFor(f) => {
                let (key, value) = self.iteration(f.expressions().iter().collect());
                let annotations: Vec<Option<Ty>> =
                    f.type_specifiers().map(|t| t.map(|t| self.type_info(t.type_info()))).collect();
                let to = scope_end(f.end_token());
                let from = tok_start(f.do_token());
                self.scopes.push(Vec::new());
                for (i, n) in f.names().iter().enumerate() {
                    let inferred = match i {
                        0 => key.clone(),
                        1 => value.clone(),
                        _ => Ty::Any,
                    };
                    let ty = annotations.get(i).cloned().flatten().unwrap_or(inferred);
                    self.declare(&ident(n), ty, from, to, BindingKind::Local);
                }
                self.stmts(f.block(), to);
                self.scopes.pop();
            }
            Stmt::TypeDeclaration(td) => {
                let ty = self.type_info(td.type_definition());
                self.aliases.insert(ident(td.type_name()), ty);
            }
            Stmt::ExportedTypeDeclaration(etd) => {
                let td = etd.type_declaration();
                let ty = self.type_info(td.type_definition());
                self.aliases.insert(ident(td.type_name()), ty);
            }
            _ => {}
        }
    }

    fn local_function(&mut self, name: &TokenReference, body: &ast::FunctionBody, end: usize) {
        let n = ident(name);
        // The name is in scope inside its own body.
        self.declare(&n, Ty::Function(Rc::default()), tok_start(name), end, BindingKind::Function);
        let f = self.function(body, None, None);
        self.set_local_type(&n, Ty::Function(Rc::new(f)));
    }

    fn function_declaration(&mut self, fd: &ast::FunctionDeclaration) {
        let fname = fd.name();
        let names: Vec<String> = fname.names().iter().map(ident).collect();
        let method = fname.method_name().map(ident);
        if names.len() == 1 && method.is_none() {
            let f = Ty::Function(Rc::new(self.function(fd.body(), None, None)));
            if self.lookup(&names[0]).is_some() {
                self.set_local_type(&names[0], f);
            } else {
                self.declare_global(&names[0], f, BindingKind::Function);
            }
            return;
        }
        // `function a.b.c()` / `function a.b:c()`: find the table to add to.
        let path_len = if method.is_some() { names.len() } else { names.len() - 1 };
        let mut target = self.lookup(&names[0]).unwrap_or_else(|| self.global(&names[0]));
        for n in &names[1..path_len] {
            target = self.member(&target, n);
        }
        let key = method.clone().unwrap_or_else(|| names.last().cloned().unwrap_or_default());
        let table = match target.base() {
            Ty::Table(id) => Some(*id),
            _ => None,
        };
        let self_ty = method.as_ref().map(|_| match table {
            Some(id) => Ty::Table(with_table(id, |t| t.instance).unwrap_or(id)),
            None => target.clone(),
        });
        let f = self.function(fd.body(), self_ty, None);
        if let Some(id) = table {
            with_table_mut(id, |t| {
                t.fields.insert(key, Ty::Function(Rc::new(f)));
            });
        }
    }

    fn assign(&mut self, var: &Var, value: Ty, from: usize) {
        match var {
            Var::Name(tok) => {
                let n = ident(tok);
                if self.lookup(&n).is_some() {
                    self.refine_local(&n, value, from);
                } else if self.api.global(&n).is_none() && !value.is_unknown() {
                    self.declare_global(&n, value, BindingKind::Global);
                }
            }
            Var::Expression(ve) => {
                let suffixes: Vec<&Suffix> = ve.suffixes().collect();
                let Some((last, init)) = suffixes.split_last() else { return };
                let key = match last {
                    Suffix::Index(Index::Dot { name, .. }) => ident(name),
                    Suffix::Index(Index::Brackets { expression, .. }) => match expr_string(expression) {
                        Some(s) => s,
                        None => return,
                    },
                    _ => return,
                };
                let target = self.chain(ve.prefix(), init.to_vec());
                if let Ty::Table(id) = target.base() {
                    let id = *id;
                    let existing = with_table(id, |t| t.fields.get(&key).cloned());
                    if existing.as_ref().is_none_or(Ty::is_unknown) {
                        with_table_mut(id, |t| {
                            t.fields.insert(key, value);
                        });
                    }
                }
            }
            _ => {}
        }
    }

    /// Key and value types of a generic `for` loop.
    fn iteration(&mut self, exprs: Vec<&Expression>) -> (Ty, Ty) {
        if let [Expression::FunctionCall(fc)] = exprs.as_slice() {
            let suffixes: Vec<&Suffix> = fc.suffixes().collect();
            if let (Prefix::Name(n), [Suffix::Call(Call::AnonymousCall(args))]) = (fc.prefix(), suffixes.as_slice()) {
                let n = ident(n);
                if (n == "pairs" || n == "ipairs") && self.lookup(&n).is_none() {
                    let arg = self.args(args, &[]).into_iter().next().unwrap_or(Ty::Any);
                    let key = if n == "ipairs" { Ty::Number } else { key_of(&arg) };
                    return (key, element_of(&arg));
                }
            }
        }
        let tys: Vec<Ty> = exprs.iter().map(|e| self.eval(e)).collect();
        match tys.first().map(Ty::base) {
            Some(Ty::Function(f)) => (f.ret.nth(0), f.ret.nth(1)),
            Some(t @ (Ty::Array(_) | Ty::Table(_))) => (key_of(t), element_of(t)),
            _ => (Ty::Any, Ty::Any),
        }
    }

    fn function(&mut self, body: &ast::FunctionBody, self_ty: Option<Ty>, expected: Option<&[Param]>) -> FuncTy {
        let from = tok_start(body.parameters_parentheses().tokens().0);
        let to = scope_end(body.end_token());
        let annotations: Vec<Option<Ty>> =
            body.type_specifiers().map(|t| t.map(|t| self.type_info(t.type_info()))).collect();
        let declared_ret = body.return_type().map(|t| self.type_info(t.type_info()));
        self.scopes.push(Vec::new());
        let method = self_ty.is_some();
        if let Some(s) = self_ty {
            self.declare("self", s, from, to, BindingKind::Param);
        }
        let mut params = Vec::new();
        let mut variadic = None;
        for (i, p) in body.parameters().iter().enumerate() {
            match p {
                Parameter::Name(tok) => {
                    let n = ident(tok);
                    let ty = annotations
                        .get(i)
                        .cloned()
                        .flatten()
                        .or_else(|| expected.and_then(|e| e.get(i)).map(|p| p.ty.clone()))
                        .unwrap_or(Ty::Any);
                    self.declare(&n, ty.clone(), from, to, BindingKind::Param);
                    params.push(Param { name: n.into(), ty, optional: false });
                }
                Parameter::Ellipsis(_) => variadic = Some(annotations.get(i).cloned().flatten().unwrap_or(Ty::Any)),
                _ => {}
            }
        }
        self.returns.push(None);
        self.stmts(body.block(), to);
        let inferred = self.returns.pop().flatten();
        self.scopes.pop();
        FuncTy { params, variadic, ret: declared_ret.or(inferred).unwrap_or(Ty::Nil), method, doc: None }
    }

    // --- expressions ---

    /// Evaluates expressions for `n` targets, expanding the last one's values.
    fn expr_list(&mut self, exprs: Vec<&Expression>, n: usize) -> Vec<Ty> {
        let mut out = Vec::new();
        let count = exprs.len();
        for (i, e) in exprs.into_iter().enumerate() {
            let t = self.eval(e);
            if i + 1 == count {
                match t {
                    Ty::Tuple(ts) => out.extend(ts.iter().cloned()),
                    t => out.push(t),
                }
            } else {
                out.push(t.first());
            }
        }
        out.resize(n.max(out.len()), Ty::Nil);
        out
    }

    fn expr_list_ty(&mut self, exprs: Vec<&Expression>) -> Ty {
        let n = exprs.len();
        let tys = self.expr_list(exprs, 0);
        match tys.len() {
            0 => Ty::Nil,
            1 if n == 1 => tys.into_iter().next().unwrap(),
            _ => Ty::Tuple(tys.into()),
        }
    }

    fn eval(&mut self, e: &Expression) -> Ty {
        self.eval_expected(e, None)
    }

    fn eval_expected(&mut self, e: &Expression, expected: Option<&Ty>) -> Ty {
        if self.depth > MAX_DEPTH {
            return Ty::Any;
        }
        self.depth += 1;
        let t = self.eval_inner(e, expected);
        self.depth -= 1;
        t
    }

    fn eval_inner(&mut self, e: &Expression, expected: Option<&Ty>) -> Ty {
        match e {
            Expression::Number(_) => Ty::Number,
            Expression::String(_) => Ty::String,
            Expression::InterpolatedString(s) => {
                for x in s.expressions() {
                    self.eval(x);
                }
                Ty::String
            }
            Expression::Symbol(tok) => match tok.token().token_type() {
                TokenType::Symbol { symbol: Symbol::True | Symbol::False } => Ty::Boolean,
                TokenType::Symbol { symbol: Symbol::Nil } => Ty::Nil,
                _ => Ty::Any,
            },
            Expression::TableConstructor(tc) => self.table(tc),
            Expression::Function(f) => {
                let expected_params = match expected.map(Ty::base) {
                    Some(Ty::Function(ef)) => Some(ef.params.clone()),
                    _ => None,
                };
                Ty::Function(Rc::new(self.function(f.body(), None, expected_params.as_deref())))
            }
            Expression::FunctionCall(fc) => self.chain(fc.prefix(), fc.suffixes().collect()),
            Expression::Var(Var::Name(tok)) => {
                let n = ident(tok);
                self.lookup(&n).unwrap_or_else(|| self.global(&n))
            }
            Expression::Var(Var::Expression(ve)) => self.chain(ve.prefix(), ve.suffixes().collect()),
            Expression::Parentheses { expression, .. } => self.eval(expression).first(),
            Expression::UnaryOperator { unop, expression } => {
                let t = self.eval(expression);
                match unop {
                    UnOp::Not(_) => Ty::Boolean,
                    UnOp::Minus(_) if matches!(t.base(), Ty::Data(_)) => t.first(),
                    _ => Ty::Number,
                }
            }
            Expression::BinaryOperator { lhs, binop, rhs } => {
                let l = self.eval(lhs).first();
                let r = self.eval(rhs).first();
                binop_type(binop, l, r)
            }
            Expression::IfExpression(ie) => {
                self.eval(ie.condition());
                let a = self.eval(ie.if_expression());
                for ei in ie.else_if_expressions().into_iter().flatten() {
                    self.eval(ei.condition());
                    self.eval(ei.expression());
                }
                let b = self.eval(ie.else_expression());
                if a.is_unknown() { b } else { a }
            }
            Expression::TypeAssertion { expression, type_assertion } => {
                self.eval(expression);
                self.type_info(type_assertion.cast_to())
            }
            _ => Ty::Any,
        }
    }

    fn table(&mut self, tc: &ast::TableConstructor) -> Ty {
        let mut fields: Vec<(String, Ty)> = Vec::new();
        let mut array: Option<Ty> = None;
        for field in tc.fields() {
            match field {
                Field::NameKey { key, value, .. } => {
                    let t = self.eval(value).first();
                    fields.push((ident(key), t));
                }
                Field::ExpressionKey { key, value, .. } => {
                    let t = self.eval(value).first();
                    match expr_string(key) {
                        Some(k) => fields.push((k, t)),
                        None => {
                            self.eval(key);
                        }
                    }
                }
                Field::NoKey(e) => {
                    let t = self.eval(e).first();
                    if array.is_none() {
                        array = Some(t);
                    }
                }
                _ => {}
            }
        }
        let id = new_table();
        with_table_mut(id, |t| {
            t.fields.extend(fields);
            t.array = array;
        });
        Ty::Table(id)
    }

    fn chain(&mut self, prefix: &Prefix, suffixes: Vec<&Suffix>) -> Ty {
        let (mut cur, mut callee) = match prefix {
            Prefix::Name(tok) => {
                let n = ident(tok);
                match self.lookup(&n) {
                    Some(t) => (t, Callee::None),
                    None => (self.global(&n), Callee::Global(n)),
                }
            }
            Prefix::Expression(e) => (self.eval(e).first(), Callee::None),
            _ => (Ty::Any, Callee::None),
        };
        for s in suffixes {
            let mut next_callee = Callee::None;
            cur = match s {
                Suffix::Index(Index::Dot { name, .. }) => {
                    let n = ident(name);
                    if let Ty::Static(t) = &cur {
                        next_callee = Callee::Static(t.clone(), n.clone());
                    }
                    self.member(&cur, &n)
                }
                Suffix::Index(Index::Brackets { expression, .. }) => match expr_string(expression) {
                    Some(k) => self.member(&cur, &k),
                    None => {
                        self.eval(expression);
                        element_of(&cur)
                    }
                },
                Suffix::Call(Call::AnonymousCall(args)) => self.call(&cur, &callee, args),
                Suffix::Call(Call::MethodCall(mc)) => self.method_call(&cur, &ident(mc.name()), mc.args()),
                _ => Ty::Any,
            };
            callee = next_callee;
        }
        cur
    }

    /// The type of `recv.name`.
    fn member(&self, recv: &Ty, name: &str) -> Ty {
        member_type(self.ctx.tree, recv, name)
    }

    fn args(&mut self, args: &FunctionArgs, expected: &[Ty]) -> Vec<Ty> {
        match args {
            FunctionArgs::Parentheses { arguments, .. } => {
                arguments.iter().enumerate().map(|(i, e)| self.eval_expected(e, expected.get(i)).first()).collect()
            }
            FunctionArgs::String(_) => vec![Ty::String],
            FunctionArgs::TableConstructor(tc) => vec![self.table(tc)],
            _ => Vec::new(),
        }
    }

    fn call(&mut self, f: &Ty, callee: &Callee, args: &FunctionArgs) -> Ty {
        let func = match f.base() {
            Ty::Function(func) => Some(func.clone()),
            _ => None,
        };
        let expected: Vec<Ty> = func.iter().flat_map(|f| f.params.iter().map(|p| p.ty.clone())).collect();
        let argv = self.args(args, &expected);
        match callee {
            Callee::Global(g) => match g.as_str() {
                "require" => {
                    if let Some(Ty::Instance(_, Some(n))) = argv.first().map(Ty::base) {
                        return self.require(*n);
                    }
                    return Ty::Any;
                }
                "setmetatable" => {
                    if let (Some(Ty::Table(t)), Some(Ty::Table(m))) = (argv.first().map(Ty::base), argv.get(1).map(Ty::base)) {
                        set_metatable(*t, *m);
                    }
                    return argv.into_iter().next().unwrap_or(Ty::Any);
                }
                "assert" => return argv.first().map(|t| t.base().clone()).unwrap_or(Ty::Any),
                _ => {}
            },
            Callee::Static(t, n) => match (&**t, n.as_str()) {
                ("Instance", "new") => {
                    return match arg_string(args, 0) {
                        Some(c) if self.api.class(&c).is_some() => Ty::instance(&c),
                        _ => Ty::instance("Instance"),
                    };
                }
                ("Instance", "fromExisting") | ("table", "clone") | ("table", "freeze") => {
                    return argv.into_iter().next().unwrap_or(Ty::Any);
                }
                _ => {}
            },
            Callee::None => {}
        }
        func.map(|f| f.ret.clone()).unwrap_or(Ty::Any)
    }

    fn method_call(&mut self, recv: &Ty, name: &str, args: &FunctionArgs) -> Ty {
        let func = method_fn(recv, name);
        let expected: Vec<Ty> = func.iter().flat_map(|f| f.params.iter().map(|p| p.ty.clone())).collect();
        self.args(args, &expected);
        let tree = self.ctx.tree;
        if let Ty::Instance(class, node) = recv.base() {
            let node = *node;
            let found = |n: Option<usize>, fallback: &str| match n {
                Some(n) => Ty::Instance(tree.node(n).class.as_str().into(), Some(n)),
                None => Ty::instance(fallback),
            };
            match name {
                "GetService" | "FindService" => {
                    if let Some(s) = arg_string(args, 0).filter(|s| self.api.class(s).is_some()) {
                        return Ty::Instance(s.as_str().into(), tree.service(&s));
                    }
                }
                "FindFirstChild" | "WaitForChild" => {
                    let recursive = name == "FindFirstChild" && arg_count(args) > 1;
                    let child = match (node, arg_string(args, 0)) {
                        (Some(n), Some(s)) if recursive => tree.descendant(n, &s),
                        (Some(n), Some(s)) => tree.child(n, &s),
                        _ => None,
                    };
                    return found(child, "Instance");
                }
                "FindFirstDescendant" => {
                    let d = node.zip(arg_string(args, 0)).and_then(|(n, s)| tree.descendant(n, &s));
                    return found(d, "Instance");
                }
                "FindFirstChildOfClass" | "FindFirstChildWhichIsA" => {
                    if let Some(c) = arg_string(args, 0).filter(|c| self.api.class(c).is_some()) {
                        let which = name.ends_with("WhichIsA");
                        let n = node.and_then(|n| tree.child_of_class(n, &c, self.api, which));
                        return n.map(|n| found(Some(n), &c)).unwrap_or_else(|| Ty::instance(&c));
                    }
                }
                "FindFirstAncestorOfClass" | "FindFirstAncestorWhichIsA" => {
                    if let Some(c) = arg_string(args, 0).filter(|c| self.api.class(c).is_some()) {
                        let which = name.ends_with("WhichIsA");
                        let api = self.api;
                        let n = node.and_then(|n| {
                            tree.ancestor(n, |a| if which { api.is_a(&a.class, &c) } else { a.class == c })
                        });
                        return n.map(|n| found(Some(n), &c)).unwrap_or_else(|| Ty::instance(&c));
                    }
                }
                "FindFirstAncestor" => {
                    let a = node.zip(arg_string(args, 0)).and_then(|(n, s)| tree.ancestor(n, |a| a.name == s));
                    return found(a, "Instance");
                }
                "Clone" => return Ty::Instance(class.clone(), node),
                "GetPropertyChangedSignal" => return Ty::Signal(Rc::new([])),
                _ => {}
            }
        }
        func.map(|f| f.ret.clone()).unwrap_or(Ty::Any)
    }

    fn require(&mut self, n: usize) -> Ty {
        let tree = self.ctx.tree;
        let node = tree.node(n);
        if node.class != "ModuleScript" || self.ctx.depth >= MAX_REQUIRE_DEPTH {
            return Ty::Any;
        }
        if let Some(cached) = self.ctx.modules.map.borrow().get(&n) {
            return cached.clone().unwrap_or(Ty::Any);
        }
        let Some(src) = node.source.as_deref() else { return Ty::Any };
        self.ctx.modules.map.borrow_mut().insert(n, None);
        let ctx = Ctx { tree, script: Some(n), modules: self.ctx.modules, depth: self.ctx.depth + 1 };
        let ret = analyze(src, &ctx, false).ret.first();
        self.ctx.modules.map.borrow_mut().insert(n, Some(ret.clone()));
        ret
    }

    // --- type annotations ---

    fn type_info(&mut self, t: &TypeInfo) -> Ty {
        match t {
            TypeInfo::Basic(tok) => {
                let n = ident(tok);
                if let Some(a) = self.aliases.get(&n) {
                    return a.clone();
                }
                named_type(self.api, &n)
            }
            TypeInfo::String(_) => Ty::String,
            TypeInfo::Boolean(_) => Ty::Boolean,
            TypeInfo::Optional { base, .. } => match self.type_info(base) {
                t @ (Ty::Any | Ty::Nil) => t,
                t => Ty::Optional(Rc::new(t)),
            },
            TypeInfo::Array { type_info, .. } => Ty::Array(Rc::new(self.type_info(type_info))),
            TypeInfo::Typeof { inner, .. } => self.eval(inner).first(),
            TypeInfo::Module { module, type_info, .. } => match (ident(module).as_str(), &**type_info) {
                ("Enum", IndexedTypeInfo::Basic(n)) => Ty::EnumItem(ident(n).into()),
                _ => Ty::Any,
            },
            TypeInfo::Table { fields, .. } => {
                let mut out = Vec::new();
                for f in fields {
                    if let TypeFieldKey::Name(n) = f.key() {
                        out.push((ident(n), self.type_info(f.value())));
                    }
                }
                let id = new_table();
                with_table_mut(id, |t| t.fields.extend(out));
                Ty::Table(id)
            }
            TypeInfo::Callback { arguments, return_type, .. } => {
                let params = arguments
                    .iter()
                    .enumerate()
                    .map(|(i, a)| Param {
                        name: a.name().map(|(n, _)| ident(n)).unwrap_or_else(|| format!("arg{}", i + 1)).into(),
                        ty: self.type_info(a.type_info()),
                        optional: false,
                    })
                    .collect();
                let ret = self.type_info(return_type);
                Ty::Function(Rc::new(FuncTy { params, ret, ..Default::default() }))
            }
            TypeInfo::Union(u) => {
                let tys: Vec<Ty> = u.types().iter().map(|t| self.type_info(t)).collect();
                let has_nil = tys.iter().any(|t| matches!(t, Ty::Nil));
                match tys.into_iter().find(|t| !matches!(t, Ty::Nil)) {
                    Some(t) if has_nil => Ty::Optional(Rc::new(t)),
                    Some(t) => t,
                    None => Ty::Nil,
                }
            }
            TypeInfo::Tuple { types, .. } => {
                let tys: Vec<Ty> = types.iter().map(|t| self.type_info(t)).collect();
                if tys.len() == 1 { tys.into_iter().next().unwrap() } else { Ty::Tuple(tys.into()) }
            }
            TypeInfo::Variadic { type_info, .. } => self.type_info(type_info),
            _ => Ty::Any,
        }
    }
}

/// A type named in an annotation: primitives, classes and datatypes.
pub fn named_type(api: &Api, n: &str) -> Ty {
    match n {
        "table" | "function" | "thread" | "buffer" | "userdata" => Ty::Named(n.into()),
        _ => parse_type(n, &|s| api.class(s).is_some(), &|s| api.is_datatype(s)),
    }
}

fn binop_type(binop: &BinOp, l: Ty, r: Ty) -> Ty {
    let op = binop.token().token().to_string();
    match op.as_str() {
        "and" => r,
        "or" => match l.base() {
            Ty::Any | Ty::Nil | Ty::Boolean => r,
            _ => l.base().clone(),
        },
        "==" | "~=" | "<" | "<=" | ">" | ">=" => Ty::Boolean,
        ".." => Ty::String,
        _ => match (l.base(), r.base()) {
            (Ty::Data(a), Ty::Data(b)) if &**a == "CFrame" && &**b == "Vector3" => r.base().clone(),
            (Ty::Data(_), _) => l.base().clone(),
            (_, Ty::Data(_)) => r.base().clone(),
            _ => Ty::Number,
        },
    }
}

fn set_metatable(t: TableId, m: TableId) {
    with_table_mut(t, |tt| tt.meta = Some(m));
    // `setmetatable(obj, Class)` with `Class.__index = Class`, or
    // `setmetatable(obj, {__index = Class})`: remember obj as Class's
    // instance shape so `self` in Class's methods sees its fields.
    let class = match with_table(m, |mt| mt.fields.get("__index").cloned()) {
        Some(Ty::Table(c)) => c,
        _ => m,
    };
    with_table_mut(class, |c| {
        if c.instance.is_none() && class != t {
            c.instance = Some(t);
        }
    });
}

/// The type of `recv.name`, without side effects.
pub fn member_type(tree: &Tree, recv: &Ty, name: &str) -> Ty {
    let api = api();
    match recv.base() {
        Ty::Instance(class, node) => {
            if name == "Parent"
                && let Some(p) = node.and_then(|n| tree.node(n).parent)
            {
                return Ty::Instance(tree.node(p).class.as_str().into(), Some(p));
            }
            if let Some((m, _)) = api.class_member(class, name) {
                return m.ty();
            }
            if let Some(c) = node.and_then(|n| tree.child(n, name)) {
                return Ty::Instance(tree.node(c).class.as_str().into(), Some(c));
            }
            if &**class == "DataModel" && api.class(name).is_some_and(|c| c.service) {
                return Ty::Instance(name.into(), tree.service(name));
            }
            Ty::Any
        }
        Ty::Data(dt) => api.datatype_member(dt, name).map(|m| m.ty()).unwrap_or(Ty::Any),
        Ty::Static(t) => api.static_member(t, name).map(|m| m.ty()).unwrap_or(Ty::Any),
        Ty::Enums if api.enums.contains_key(name) => Ty::EnumType(name.into()),
        Ty::EnumType(e) if api.enums.get(&**e).is_some_and(|items| items.iter().any(|i| i == name)) => {
            Ty::EnumItem(e.clone())
        }
        Ty::EnumItem(e) => match name {
            "Name" => Ty::String,
            "Value" => Ty::Number,
            "EnumType" => Ty::EnumType(e.clone()),
            _ => Ty::Any,
        },
        Ty::Table(id) => table_field(*id, name).unwrap_or(Ty::Any),
        _ => Ty::Any,
    }
}

pub fn element_of(t: &Ty) -> Ty {
    match t.base() {
        Ty::Array(e) => (**e).clone(),
        Ty::Table(id) => with_table(*id, |t| t.array.clone()).unwrap_or(Ty::Any),
        _ => Ty::Any,
    }
}

fn key_of(t: &Ty) -> Ty {
    match t.base() {
        Ty::Array(_) => Ty::Number,
        Ty::Table(id) if with_table(*id, |t| t.array.is_some()) => Ty::Number,
        Ty::Table(_) => Ty::String,
        _ => Ty::Any,
    }
}
