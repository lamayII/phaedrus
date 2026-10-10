use std::path::Path;

use phaedrus_dom::geometry::{PartShape, part_geometry};
use phaedrus_dom::{Document, History, Variant, types::Vector3};

fn test_place() -> Document {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test_place.rbxl");
    Document::open(&path).expect("test_place.rbxl loads")
}

#[test]
fn loads_primitives() {
    let doc = test_place();
    let ws = doc.workspace().expect("workspace");
    let mut shapes = Vec::new();
    for c in doc.children(ws) {
        if let Some(g) = doc.get(*c).and_then(part_geometry) {
            shapes.push((doc.get(*c).unwrap().name.clone(), g.shape));
        }
    }
    let has = |s| shapes.iter().any(|(_, shape)| *shape == s);
    assert!(has(PartShape::Ball));
    assert!(has(PartShape::Block));
    assert!(has(PartShape::Cylinder));
    assert!(has(PartShape::Wedge));
    assert!(has(PartShape::CornerWedge));
    assert!(doc.service("Lighting").is_some());
}

#[test]
fn derived_properties_and_undo() {
    let mut doc = test_place();
    let mut history = History::new();
    let ws = doc.workspace().unwrap();
    let cube = doc.find_first_child(ws, "Cube").unwrap();
    doc.set_property(cube, "Position", Variant::Vector3(Vector3::new(10.0, 0.0, 0.0))).unwrap();
    history.set_waypoint(&mut doc, "Move");
    assert_eq!(doc.get_property(cube, "Position"), Some(Variant::Vector3(Vector3::new(10.0, 0.0, 0.0))));
    assert!(matches!(doc.get_property(cube, "BrickColor"), Some(Variant::BrickColor(_))));

    doc.set_parent(cube, phaedrus_dom::Ref::none()).unwrap();
    history.set_waypoint(&mut doc, "Delete");
    assert!(!doc.is_in_tree(cube));

    history.undo(&mut doc);
    assert!(doc.is_in_tree(cube));
    history.undo(&mut doc);
    assert_ne!(doc.get_property(cube, "Position"), Some(Variant::Vector3(Vector3::new(10.0, 0.0, 0.0))));
    history.redo(&mut doc);
    assert_eq!(doc.get_property(cube, "Position"), Some(Variant::Vector3(Vector3::new(10.0, 0.0, 0.0))));
}

#[test]
fn round_trip_binary_and_xml() {
    let mut doc = test_place();
    let dir = std::env::temp_dir().join("phaedrus_test");
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["rt.rbxl", "rt.rbxlx"] {
        let p = dir.join(name);
        doc.save_as(&p).unwrap();
        let back = Document::open(&p).unwrap();
        let ws = back.workspace().unwrap();
        assert!(back.find_first_child(ws, "Cube").is_some(), "{name}");
    }
}

#[test]
fn abandoned_redo_frees_instances() {
    let mut doc = test_place();
    let mut history = History::new();
    let ws = doc.workspace().unwrap();
    let r = doc.create("Part", ws);
    history.set_waypoint(&mut doc, "Insert");
    history.undo(&mut doc);
    assert!(doc.exists(r) && !doc.is_in_tree(r));
    // A new action abandons the redo stack; the part can never come back.
    let cube = doc.find_first_child(ws, "Cube").unwrap();
    doc.set_property(cube, "Name", Variant::String("Box".into())).unwrap();
    history.set_waypoint(&mut doc, "Rename");
    assert!(!doc.exists(r));
    history.undo(&mut doc);
    assert_eq!(doc.get(cube).unwrap().name, "Cube");
}
