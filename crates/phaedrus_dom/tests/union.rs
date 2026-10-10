use phaedrus_dom::geometry::{PartShape, part_geometry};
use phaedrus_dom::types::{Content, SharedString, Vector3};
use phaedrus_dom::union::{
    asset_cache_file, bounds_size, decode_union_graphics, load_union_mesh, mesh_data_from_asset, missing_union_assets,
    parse_asset_id, union_mesh_key,
};
use phaedrus_dom::{InstanceBuilder, Variant, WeakDom};

/// A CSGMDL v5 union mesh with `InitialSize` (20.707275, 0.4, 6).
const V5: &[u8] = include_bytes!("fixtures/union_v5.meshdata");
const V5_SIZE: [f32; 3] = [20.707275, 0.39999962, 6.0];

fn union(props: Vec<(&str, Variant)>) -> WeakDom {
    let mut b = InstanceBuilder::new("UnionOperation").with_property("InitialSize", Vector3::new(V5_SIZE[0], V5_SIZE[1], V5_SIZE[2]));
    for (k, v) in props {
        b = b.with_property(k, v);
    }
    WeakDom::new(InstanceBuilder::new("DataModel").with_child(b))
}

fn the_union(dom: &WeakDom) -> &phaedrus_dom::Instance {
    dom.descendants().find(|i| i.class == "UnionOperation").unwrap()
}

fn close(a: [f32; 3], b: [f32; 3], tol: f32) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() <= tol)
}

#[test]
fn decodes_v5_at_initial_size() {
    let m = decode_union_graphics(V5).unwrap().expect("v5 has geometry");
    assert!(m.indices.len() >= 3 && m.indices.len().is_multiple_of(3));
    assert_eq!(m.normals.len(), m.positions.len());
    assert!(close(bounds_size(&m.positions), V5_SIZE, 1e-3));
}

#[test]
fn csgk_placeholder_has_no_geometry() {
    let mut csgk = b"CSGK".to_vec();
    csgk.extend_from_slice(&[b'0'; 32]);
    assert!(decode_union_graphics(&csgk).unwrap().is_none());
}

#[test]
fn inline_union_renders_as_unit_mesh() {
    let dom = union(vec![("MeshData2", Variant::SharedString(SharedString::new(V5.to_vec())))]);
    let inst = the_union(&dom);
    let g = part_geometry(inst).unwrap();
    assert!(matches!(g.shape, PartShape::Union(_)));
    let m = load_union_mesh(inst, None).unwrap().unwrap();
    assert!(close(bounds_size(&m.positions), [1.0; 3], 1e-3));
    assert!(m.normals.iter().all(|n| ((n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt() - 1.0).abs() < 1e-3));
    assert!(m.colors.is_empty(), "UsePartColor defaults to true");
}

#[test]
fn use_part_color_false_keeps_vertex_colors() {
    let data = Variant::SharedString(SharedString::new(V5.to_vec()));
    let a = union(vec![("MeshData2", data.clone())]);
    let b = union(vec![("MeshData2", data), ("UsePartColor", Variant::Bool(false))]);
    let (a, b) = (the_union(&a), the_union(&b));
    assert_ne!(union_mesh_key(a), union_mesh_key(b));
    assert_eq!(part_geometry(b).unwrap().color, [1.0; 3]);
    assert!(!load_union_mesh(b, None).unwrap().unwrap().colors.is_empty());
}

#[test]
fn asset_union_reads_from_cache() {
    let url = "https://www.roblox.com/asset/?id=4500696697";
    assert_eq!(parse_asset_id(url), Some(4500696697));
    assert_eq!(parse_asset_id("rbxassetid://123"), Some(123));
    assert_eq!(parse_asset_id("rbxasset://textures/x.png"), None);

    let dom = union(vec![("Content", Variant::Content(Content::from_uri(url)))]);
    let inst = the_union(&dom);
    assert!(matches!(part_geometry(inst).unwrap().shape, PartShape::Union(_)));

    let dir = std::env::temp_dir().join(format!("phaedrus-union-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    assert!(load_union_mesh(inst, Some(&dir)).unwrap().is_none(), "not downloaded yet");
    assert_eq!(missing_union_assets(dom.descendants(), Some(&dir)), [4500696697]);

    // Asset Delivery serves union meshes wrapped in a binary model.
    std::fs::write(asset_cache_file(&dir, 4500696697), wrapped_asset(V5)).unwrap();
    assert!(load_union_mesh(inst, Some(&dir)).unwrap().is_some());
    assert!(missing_union_assets(dom.descendants(), Some(&dir)).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn union_without_data_is_a_proxy() {
    let dom = union(vec![]);
    assert_eq!(part_geometry(the_union(&dom)).unwrap().shape, PartShape::Proxy);
}

/// A union asset as Asset Delivery serves it: a binary model holding a
/// `PartOperationAsset` whose `MeshData` is the CSGMDL blob.
fn wrapped_asset(mesh: &[u8]) -> Vec<u8> {
    let dom = WeakDom::new(
        InstanceBuilder::new("DataModel").with_child(
            InstanceBuilder::new("PartOperationAsset")
                .with_property("MeshData", Variant::BinaryString(mesh.to_vec().into()))
                .with_property("ChildData", Variant::BinaryString(Vec::new().into())),
        ),
    );
    let mut out = Vec::new();
    rbx_binary::to_writer(&mut out, &dom, dom.root().children()).unwrap();
    out
}

#[test]
fn unwraps_part_operation_asset() {
    let inner = mesh_data_from_asset(wrapped_asset(V5)).unwrap().unwrap();
    assert_eq!(inner, V5);
    assert!(decode_union_graphics(&inner).unwrap().is_some());
    // Raw CSGMDL passes through; empty assets have no geometry.
    assert_eq!(mesh_data_from_asset(V5.to_vec()).unwrap().unwrap(), V5);
    assert!(mesh_data_from_asset(wrapped_asset(&[])).unwrap().is_none());
}
