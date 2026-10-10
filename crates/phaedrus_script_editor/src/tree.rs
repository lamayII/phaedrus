//! Snapshot of the place's instance tree, sent by the host. Used to resolve
//! paths like `script.Parent.Config` and to read ModuleScript sources for
//! `require`.

use std::collections::HashMap;

use serde_json::Value;

use crate::api::Api;

#[derive(Debug)]
pub struct Node {
    pub id: String,
    pub name: String,
    pub class: String,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    /// Source of ModuleScripts.
    pub source: Option<String>,
}

#[derive(Debug, Default)]
pub struct Tree {
    pub nodes: Vec<Node>,
    pub by_id: HashMap<String, usize>,
}

impl Tree {
    /// Parses the host's format: nested `[id, name, class, [children...], source?]`.
    pub fn from_json(json: &str) -> Tree {
        let mut tree = Tree::default();
        if let Ok(v) = serde_json::from_str::<Value>(json) {
            tree.add(&v, None);
        }
        tree
    }

    fn add(&mut self, v: &Value, parent: Option<usize>) {
        let Some(a) = v.as_array() else { return };
        let idx = self.nodes.len();
        let id = a.first().and_then(Value::as_str).unwrap_or_default().to_owned();
        self.nodes.push(Node {
            id: id.clone(),
            name: a.get(1).and_then(Value::as_str).unwrap_or_default().to_owned(),
            class: a.get(2).and_then(Value::as_str).unwrap_or("Instance").to_owned(),
            parent,
            children: Vec::new(),
            source: a.get(4).and_then(Value::as_str).map(str::to_owned),
        });
        self.by_id.insert(id, idx);
        if let Some(p) = parent {
            self.nodes[p].children.push(idx);
        }
        for c in a.get(3).and_then(Value::as_array).into_iter().flatten() {
            self.add(c, Some(idx));
        }
    }

    pub fn root(&self) -> Option<usize> {
        (!self.nodes.is_empty()).then_some(0)
    }

    pub fn node(&self, i: usize) -> &Node {
        &self.nodes[i]
    }

    pub fn child(&self, n: usize, name: &str) -> Option<usize> {
        self.nodes[n].children.iter().copied().find(|&c| self.nodes[c].name == name)
    }

    pub fn descendant(&self, n: usize, name: &str) -> Option<usize> {
        let mut stack: Vec<usize> = self.nodes[n].children.iter().rev().copied().collect();
        while let Some(c) = stack.pop() {
            if self.nodes[c].name == name {
                return Some(c);
            }
            stack.extend(self.nodes[c].children.iter().rev().copied());
        }
        None
    }

    pub fn child_of_class(&self, n: usize, class: &str, api: &Api, which_is_a: bool) -> Option<usize> {
        self.nodes[n].children.iter().copied().find(|&c| {
            let cc = &self.nodes[c].class;
            if which_is_a { api.is_a(cc, class) } else { cc == class }
        })
    }

    pub fn ancestor(&self, n: usize, pred: impl Fn(&Node) -> bool) -> Option<usize> {
        let mut cur = self.nodes[n].parent;
        while let Some(p) = cur {
            if pred(&self.nodes[p]) {
                return Some(p);
            }
            cur = self.nodes[p].parent;
        }
        None
    }

    pub fn service(&self, class: &str) -> Option<usize> {
        let root = self.root()?;
        self.nodes[root].children.iter().copied().find(|&c| self.nodes[c].class == class)
    }

    pub fn full_name(&self, n: usize) -> String {
        let mut parts = Vec::new();
        let mut cur = Some(n);
        while let Some(c) = cur {
            if self.nodes[c].parent.is_none() {
                break;
            }
            parts.push(self.nodes[c].name.as_str());
            cur = self.nodes[c].parent;
        }
        parts.reverse();
        parts.join(".")
    }
}
