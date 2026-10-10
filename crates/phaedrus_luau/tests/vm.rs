use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use phaedrus_dom::types::{Variant, Vector3};
use phaedrus_dom::Document;
use phaedrus_luau::{OutputLevel, Runtime, SharedOutput};

fn setup() -> (Arc<Mutex<Document>>, SharedOutput, Runtime) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test_place.rbxl");
    let doc = Arc::new(Mutex::new(Document::open(&path).unwrap()));
    let out: SharedOutput = Default::default();
    let rt = Runtime::new(doc.clone(), out.clone(), true).unwrap();
    (doc, out, rt)
}

fn errors(out: &SharedOutput) -> Vec<String> {
    out.lock().iter().filter(|l| l.level == OutputLevel::Error).map(|l| l.text.clone()).collect()
}

fn texts(out: &SharedOutput) -> Vec<String> {
    out.lock().iter().map(|l| l.text.clone()).collect()
}

#[test]
fn set_position_from_script() {
    let (doc, out, rt) = setup();
    rt.exec("workspace.Cube.Position = Vector3.new(10, 0, 0)", "cmd");
    assert!(errors(&out).is_empty(), "{:?}", errors(&out));
    let d = doc.lock();
    let ws = d.workspace().unwrap();
    let cube = d.find_first_child(ws, "Cube").unwrap();
    assert_eq!(d.get_property(cube, "Position"), Some(Variant::Vector3(Vector3::new(10.0, 0.0, 0.0))));
}

#[test]
fn datatypes_and_print() {
    let (_doc, out, rt) = setup();
    rt.exec(
        r#"
        local v = Vector3.new(1, 2, 3) + Vector3.new(1, 1, 1) * 2
        print(v, typeof(v), v.Magnitude > 0)
        local cf = CFrame.new(0, 5, 0) * CFrame.Angles(0, math.rad(90), 0)
        local p = cf * Vector3.new(1, 0, 0)
        print(math.round(p.X) + 0, math.round(p.Y) + 0, math.round(p.Z) + 0)
        print(Color3.fromRGB(255, 0, 0), BrickColor.new("Bright red").Name)
        print(Enum.Material.Plastic, Enum.Material.Plastic.Value)
        print(workspace.Cube:IsA("BasePart"), workspace.Cube.ClassName, game:GetService("Workspace") == workspace)
        print(workspace.Cube.Material == Enum.Material.Plastic)
        print(UDim2.new(0.5, 10, 1, -5))
        "#,
        "cmd",
    );
    assert!(errors(&out).is_empty(), "{:?}", errors(&out));
    let t = texts(&out);
    assert_eq!(t[0], "3, 4, 5 Vector3 true");
    assert_eq!(t[1], "0 5 -1");
    assert_eq!(t[2], "1, 0, 0 Bright red");
    assert_eq!(t[3], "Enum.Material.Plastic 256");
    assert_eq!(t[4], "true Part true");
    assert_eq!(t[5], "true");
    assert_eq!(t[6], "{0.5, 10}, {1, -5}");
}

#[test]
fn instance_new_and_events() {
    let (doc, out, rt) = setup();
    rt.exec(
        r#"
        workspace.ChildAdded:Connect(function(c) print("added", c.Name) end)
        local p = Instance.new("Part")
        p.Name = "Spawned"
        p.Anchored = true
        p.Parent = workspace
        p.Changed:Connect(function(prop) print("changed", prop) end)
        "#,
        "cmd",
    );
    let changes = doc.lock().take_changes();
    rt.step(1.0 / 60.0, &changes);
    rt.exec("workspace.Spawned.Transparency = 0.5", "cmd");
    let changes = doc.lock().take_changes();
    rt.step(1.0 / 60.0, &changes);
    assert!(errors(&out).is_empty(), "{:?}", errors(&out));
    let t = texts(&out);
    assert!(t.contains(&"added Spawned".to_string()), "{t:?}");
    assert!(t.contains(&"changed Transparency".to_string()), "{t:?}");
}

#[test]
fn task_wait_scheduling() {
    let (doc, out, rt) = setup();
    rt.exec(
        r#"
        print("before")
        task.wait(0.05)
        print("after")
        "#,
        "cmd",
    );
    assert_eq!(texts(&out), vec!["before"]);
    std::thread::sleep(std::time::Duration::from_millis(80));
    let changes = doc.lock().take_changes();
    rt.step(0.08, &changes);
    assert_eq!(texts(&out), vec!["before", "after"]);
}

#[test]
fn errors_are_reported() {
    let (_doc, out, rt) = setup();
    rt.exec("workspace.DoesNotExist.Position = Vector3.new()", "cmd");
    let e = errors(&out);
    assert_eq!(e.len(), 1);
    assert!(e[0].contains("DoesNotExist is not a valid member of Workspace"), "{}", e[0]);
}

#[test]
fn infinite_loop_times_out_fast_enough_to_test() {
    // Just make sure a busy loop that yields does not hang the VM.
    let (doc, out, rt) = setup();
    rt.exec("while true do task.wait() end", "cmd");
    for _ in 0..3 {
        let changes = doc.lock().take_changes();
        rt.step(0.016, &changes);
    }
    assert!(errors(&out).is_empty());
}

#[test]
fn size_and_color_round_trip() {
    let (doc, out, rt) = setup();
    rt.exec(
        r#"
        local p = Instance.new("Part", workspace)
        p.Name = "Sized"
        p.Size = Vector3.new(2, 3, 4)
        p.Color = Color3.fromRGB(10, 20, 30)
        print(p.Size, p.Color, p.BrickColor.Name ~= nil)
        "#,
        "cmd",
    );
    assert!(errors(&out).is_empty(), "{:?}", errors(&out));
    assert_eq!(texts(&out)[0].split(' ').next().unwrap(), "2,");
    let d = doc.lock();
    let ws = d.workspace().unwrap();
    let p = d.find_first_child(ws, "Sized").unwrap();
    assert_eq!(d.get_raw(p, "Size"), Some(Variant::Vector3(Vector3::new(2.0, 3.0, 4.0))));
    assert!(d.get_raw(p, "size").is_none());
}
