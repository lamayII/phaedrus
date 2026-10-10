//! Decodes every downloaded union asset in a folder.
//! Usage: `cargo run -p phaedrus_dom --example asset_peek -- %APPDATA%/phaedrus/assets`
use phaedrus_dom::union::{decode_union_graphics, mesh_data_from_asset};
fn main() {
    let dir = std::env::args().nth(1).expect("dir");
    let (mut ok, mut empty, mut failed) = (0, 0, 0);
    for e in std::fs::read_dir(dir).unwrap().flatten().filter(|e| e.path().extension().is_some_and(|x| x == "meshdata")) {
        match mesh_data_from_asset(std::fs::read(e.path()).unwrap()).and_then(|b| b.map_or(Ok(None), |b| decode_union_graphics(&b))) {
            Ok(Some(_)) => ok += 1,
            Ok(None) => empty += 1,
            Err(err) => { failed += 1; if failed <= 3 { eprintln!("{}: {err:#}", e.path().display()); } }
        }
    }
    println!("{ok} decoded, {empty} without geometry, {failed} failed");
}
