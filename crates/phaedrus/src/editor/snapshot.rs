//! Snapshot of the instance tree for the script editor's intellisense:
//! nested `[id, name, class, [children...], source?]`, with the source of
//! every ModuleScript so `require` can be followed. Class and enum data is
//! compiled into the editor itself.

use phaedrus_dom::Document;
use phaedrus_dom::types::{Ref, Variant};
use serde_json::{Value, json};

const NODE_LIMIT: usize = 20_000;
/// ModuleScripts larger than this are listed without their source.
const MODULE_SOURCE_LIMIT: usize = 256 * 1024;

pub fn tree_json(doc: &Document) -> String {
    let mut budget = NODE_LIMIT;
    node(doc, doc.root(), &mut budget).to_string()
}

fn node(doc: &Document, r: Ref, budget: &mut usize) -> Value {
    let Some(inst) = doc.get(r) else { return Value::Null };
    *budget = budget.saturating_sub(1);
    let mut children = Vec::new();
    for c in doc.children(r) {
        if *budget == 0 {
            break;
        }
        children.push(node(doc, *c, budget));
    }
    let mut v = json!([r.to_string(), inst.name, inst.class.as_str(), children]);
    if inst.class == "ModuleScript"
        && let Some(Variant::String(src)) = doc.get_property(r, "Source")
        && src.len() <= MODULE_SOURCE_LIMIT
    {
        v.as_array_mut().unwrap().push(Value::String(src));
    }
    v
}
