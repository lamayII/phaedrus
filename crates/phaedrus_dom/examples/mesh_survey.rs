//! Decodes every downloaded `.mesh` file in a folder.
//! Usage: `cargo run -p phaedrus_dom --example mesh_survey -- %APPDATA%/phaedrus/assets`
use phaedrus_dom::mesh::{bounds_size, decode_mesh};
fn main() {
    let dir = std::env::args().nth(1).expect("dir");
    let (mut ok, mut failed, mut tris) = (0, 0, 0usize);
    let (mut uv_lo, mut uv_hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for e in std::fs::read_dir(dir).unwrap().flatten().filter(|e| e.path().extension().is_some_and(|x| x == "mesh")) {
        match decode_mesh(&std::fs::read(e.path()).unwrap()) {
            Ok(m) => {
                ok += 1;
                tris += m.indices.len() / 3;
                for uv in &m.uvs { for a in 0..2 { uv_lo[a] = uv_lo[a].min(uv[a]); uv_hi[a] = uv_hi[a].max(uv[a]); } }
                if ok <= 2 { println!("{}: {} verts, bounds {:?}", e.path().display(), m.positions.len(), bounds_size(&m.positions)); }
            }
            Err(err) => { failed += 1; if failed <= 3 { eprintln!("{}: {err:#}", e.path().display()); } }
        }
    }
    println!("{ok} decoded ({tris} triangles), {failed} failed; uv range {uv_lo:?}..{uv_hi:?}");
}
