use std::io::BufReader;
fn main() {
    let path = std::env::args().nth(1).expect("path");
    let dom = rbx_binary::from_reader(BufReader::new(std::fs::File::open(path).unwrap())).unwrap();
    fn walk(dom: &rbx_dom_weak::WeakDom, r: rbx_dom_weak::types::Ref, depth: usize, props: bool) {
        let i = dom.get_by_ref(r).unwrap();
        println!("{}{} ({})", "  ".repeat(depth), i.name, i.class);
        let show = props || i.class == "Texture" || i.class == "Camera" || i.class == "Part" || i.class.contains("Script") || i.class == "Lighting" || i.class == "Sky" || i.class == "MeshPart" || i.class == "WedgePart" || i.class=="CornerWedgePart" || i.class=="TrussPart";
        if show {
            let mut keys: Vec<_> = i.properties.iter().collect();
            keys.sort_by_key(|(k, _)| k.as_str());
            for (k, v) in keys {
                let s = format!("{:?}", v);
                println!("{}  .{} = {}", "  ".repeat(depth), k, &s[..s.len().min(160)]);
            }
        }
        for c in i.children() { walk(dom, *c, depth + 1, false); }
    }
    walk(&dom, dom.root_ref(), 0, false);
}
