//! Decodes every union in a place and reports how its mesh is stored.
//! Usage: `cargo run -p phaedrus_dom --example unions -- place.rbxl`
use std::io::BufReader;

use phaedrus_dom::union::{UnionSource, bounds_size, decode_union_graphics, initial_size, is_union_class, union_source};

fn main() {
    let path = std::env::args().nth(1).expect("path");
    let dom = rbx_binary::from_reader(BufReader::new(std::fs::File::open(path).unwrap())).unwrap();
    let (mut total, mut inline_ok, mut csgk, mut assets, mut none, mut failed, mut bad_bounds) = (0, 0, 0, 0, 0, 0, 0);
    let mut worst = 0.0f32;
    for i in dom.descendants().filter(|i| is_union_class(&i.class)) {
        total += 1;
        match union_source(i) {
            None => none += 1,
            Some(UnionSource::Asset(_)) => assets += 1,
            Some(UnionSource::Inline(b)) => match decode_union_graphics(b) {
                Ok(Some(m)) => {
                    inline_ok += 1;
                    if let Some(s) = initial_size(i) {
                        let b = bounds_size(&m.positions);
                        let err = (b[0] - s.x).abs().max((b[1] - s.y).abs()).max((b[2] - s.z).abs());
                        worst = worst.max(err);
                        if err > 0.05 * s.x.max(s.y).max(s.z) {
                            bad_bounds += 1;
                        }
                    }
                }
                Ok(None) => csgk += 1,
                Err(e) => {
                    failed += 1;
                    if failed <= 3 {
                        eprintln!("{}: {e}", i.name);
                    }
                }
            },
        }
    }
    println!("{total} unions: {inline_ok} inline decoded, {csgk} CSGK, {assets} via AssetId, {none} without data, {failed} failed");
    println!("{bad_bounds} with bounds >5% off InitialSize (worst {worst:.3} studs)");
}
