//! Measures DOM memory for a large synthetic place, with and without
//! default-property compaction.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;
static ALLOCATED: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCATED.fetch_add(l.size(), Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        ALLOCATED.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
}
#[global_allocator]
static A: Counting = Counting;

fn main() {
    let path = std::env::args().nth(1).expect("place path");
    let count: usize = std::env::args().nth(2).and_then(|n| n.parse().ok()).unwrap_or(20_000);
    let mut dom = phaedrus_dom::format::load_file(std::path::Path::new(&path)).unwrap();
    let ws = dom.root().children().iter().copied().find(|r| dom.get_by_ref(*r).unwrap().class == "Workspace").unwrap();
    let cube = dom.get_by_ref(ws).unwrap().children().iter().copied().find(|r| dom.get_by_ref(*r).unwrap().name == "Cube").unwrap();
    for _ in 0..count {
        let c = dom.clone_within(cube);
        dom.transfer_within(c, ws);
    }
    // Round-trip through the binary format so the DOM looks freshly loaded.
    let roots = dom.root().children().to_vec();
    let mut bytes = Vec::new();
    rbx_binary::to_writer(&mut bytes, &dom, &roots).unwrap();
    drop(dom);
    let before = ALLOCATED.load(Ordering::Relaxed);
    let mut dom = rbx_binary::from_reader(&bytes[..]).unwrap();
    let loaded = ALLOCATED.load(Ordering::Relaxed) - before;
    let removed = phaedrus_dom::document::compact_defaults(&mut dom);
    let compacted = ALLOCATED.load(Ordering::Relaxed) - before;
    println!("{count} parts: loaded {:.1} MB, compacted {:.1} MB ({removed} default properties dropped)",
        loaded as f64 / 1e6, compacted as f64 / 1e6);
    // Make sure it still saves.
    let roots = dom.root().children().to_vec();
    let mut out = Vec::new();
    rbx_binary::to_writer(&mut out, &dom, &roots).unwrap();
    let back = rbx_binary::from_reader(&out[..]).unwrap();
    println!("re-saved {} bytes, {} instances", out.len(), back.descendants().count());
}
