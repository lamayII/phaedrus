use phaedrus_dom::content::{AssetKind, ImageAsset, cache_file, classify_image, missing_assets};
use phaedrus_dom::geometry::{PartShape, part_geometry};
use phaedrus_dom::mesh::{bounds_size, decode_mesh, load_mesh_part};
use phaedrus_dom::types::Content;
use phaedrus_dom::{InstanceBuilder, WeakDom};

/// A version 2.00 mesh: one quad from (10, 20, 30) to (14, 22, 30).
fn quad_mesh() -> Vec<u8> {
    let mut b = b"version 2.00\n".to_vec();
    b.extend_from_slice(&12u16.to_le_bytes()); // header size
    b.push(36); // vertex size (no colour)
    b.push(12); // face size
    b.extend_from_slice(&4u32.to_le_bytes());
    b.extend_from_slice(&2u32.to_le_bytes());
    for (x, y, u, v) in [(10.0f32, 20.0f32, 0.0f32, 1.0f32), (14.0, 20.0, 1.0, 1.0), (14.0, 22.0, 1.0, 0.0), (10.0, 22.0, 0.0, 0.0)] {
        for f in [x, y, 30.0, 0.0, 0.0, 1.0, u, v] {
            b.extend_from_slice(&f.to_le_bytes());
        }
        b.extend_from_slice(&[0, 0, 0, 0]); // tangent
    }
    for i in [0u32, 1, 2, 0, 2, 3] {
        b.extend_from_slice(&i.to_le_bytes());
    }
    b
}

fn mesh_part(url: &str) -> WeakDom {
    WeakDom::new(
        InstanceBuilder::new("DataModel").with_child(
            InstanceBuilder::new("MeshPart")
                .with_property("MeshContent", Content::from_uri(url))
                .with_property("TextureContent", Content::from_uri("rbxassetid://77"))
                .with_child(
                    InstanceBuilder::new("Decal").with_property("TextureContent", Content::from_uri("rbxassetid://88")),
                ),
        ),
    )
}

fn the_mesh_part(dom: &WeakDom) -> &phaedrus_dom::Instance {
    dom.descendants().find(|i| i.class == "MeshPart").unwrap()
}

#[test]
fn decodes_v2_mesh() {
    let m = decode_mesh(&quad_mesh()).unwrap();
    assert_eq!(m.positions.len(), 4);
    assert_eq!(m.indices, [0, 1, 2, 0, 2, 3]);
    assert_eq!(m.uvs[1], [1.0, 1.0]);
    assert_eq!(bounds_size(&m.positions), [4.0, 2.0, 0.0]);
}

#[test]
fn mesh_part_loads_centred_unit_mesh_from_cache() {
    let dom = mesh_part("rbxassetid://5551234");
    let inst = the_mesh_part(&dom);
    let g = part_geometry(inst).unwrap();
    assert!(matches!(g.shape, PartShape::Mesh(_)));
    assert_ne!(g.texture, 0);

    let dir = std::env::temp_dir().join(format!("phaedrus-mesh-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    assert!(load_mesh_part(inst, Some(&dir)).unwrap().is_none(), "not downloaded yet");
    assert_eq!(
        missing_assets(dom.descendants(), Some(&dir)),
        [(77, AssetKind::Image), (88, AssetKind::Image), (5551234, AssetKind::Mesh)]
    );

    std::fs::write(cache_file(&dir, 5551234, AssetKind::Mesh), quad_mesh()).unwrap();
    std::fs::write(dir.join("77.png"), b"png").unwrap();
    let m = load_mesh_part(inst, Some(&dir)).unwrap().unwrap();
    for p in &m.positions {
        assert!(p[0].abs() <= 0.5 + 1e-6 && p[1].abs() <= 0.5 + 1e-6, "{p:?}");
    }
    assert_eq!(missing_assets(dom.descendants(), Some(&dir)), [(88, AssetKind::Image)]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mesh_part_without_mesh_is_a_proxy() {
    let dom = WeakDom::new(InstanceBuilder::new("DataModel").with_child(InstanceBuilder::new("MeshPart")));
    let g = part_geometry(the_mesh_part(&dom)).unwrap();
    assert_eq!(g.shape, PartShape::Proxy);
    assert_eq!(g.texture, 0);
}

#[test]
fn sniffs_images() {
    assert_eq!(classify_image(b"\x89PNG\r\n\x1a\nrest"), ImageAsset::Image("png"));
    assert_eq!(classify_image(&[0xFF, 0xD8, 0xFF, 0xE0]), ImageAsset::Image("jpg"));
    assert_eq!(classify_image(b"DDS |...."), ImageAsset::Image("dds"));
    assert_eq!(classify_image(b"RIFF....WEBP"), ImageAsset::Unsupported);
}
