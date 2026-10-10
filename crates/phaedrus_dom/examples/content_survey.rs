//! Counts content-typed properties by class and property name.
use std::collections::BTreeMap;
use rbx_dom_weak::types::Variant;
fn main() {
    let dom = rbx_binary::from_reader(std::io::BufReader::new(std::fs::File::open(std::env::args().nth(1).unwrap()).unwrap())).unwrap();
    let mut by: BTreeMap<String, (usize, usize, String)> = BTreeMap::new();
    for i in dom.descendants() {
        for (k, v) in i.properties.iter() {
            let url = match v { Variant::Content(c) => c.as_uri().map(str::to_owned), Variant::ContentId(c) => Some(c.as_str().to_owned()), _ => continue };
            let e = by.entry(format!("{}.{}", i.class, k)).or_default();
            e.0 += 1;
            if let Some(u) = url.filter(|u| !u.is_empty()) { e.1 += 1; if e.2.is_empty() { e.2 = u; } }
        }
    }
    for (k, (n, set, ex)) in by { if set > 0 { println!("{k}: {set}/{n}  e.g. {ex}"); } }
    let mut mp = 0;
    for i in dom.descendants().filter(|i| i.class == "MeshPart") { mp += 1; if mp == 1 { let mut ks: Vec<_> = i.properties.keys().map(|k| k.to_string()).collect(); ks.sort(); println!("MeshPart props: {ks:?}"); } }
}
