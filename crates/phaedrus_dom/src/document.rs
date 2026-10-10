//! The open place: a [`WeakDom`] plus a change journal.

use std::path::{Path, PathBuf};

use anyhow::Result;
use rbx_dom_weak::{Instance, InstanceBuilder, Ustr, WeakDom, ustr};
use rbx_types::{BrickColor, CFrame, Color3, Ref, Variant, Vector3};

use crate::geometry::{self, color3uint8_to_f32};
use crate::{format, reflection};

/// One observable change to the data model. Consumers (viewport, explorer,
/// Luau `Changed` signals) read these each frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// An instance (and its subtree) was created and parented.
    Added(Ref),
    /// A property changed. `Name` changes use the name `"Name"`.
    Property(Ref, Ustr),
    /// An instance moved to a new parent. A `new` of `Ref::none()` means the
    /// instance was removed from the tree (it stays alive for undo).
    Reparented { instance: Ref, old: Ref, new: Ref },
}

pub struct Document {
    dom: WeakDom,
    pub path: Option<PathBuf>,
    /// True if the document changed since the last save.
    pub dirty: bool,
    changes: Vec<Change>,
    /// Raw operations since the last history waypoint (see [`crate::History`]).
    pub(crate) journal: Vec<RawOp>,
}

/// A reversible low-level mutation.
#[derive(Debug, Clone)]
pub(crate) enum RawOp {
    Property { r: Ref, key: Ustr, old: Option<Variant>, new: Option<Variant> },
    Name { r: Ref, old: String, new: String },
    Parent { r: Ref, old: Ref, new: Ref },
}

impl Document {
    /// A fresh place with the services Studio creates for an empty baseplate.
    pub fn new_place() -> Self {
        let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
        let root = dom.root_ref();
        let workspace = dom.insert(
            root,
            InstanceBuilder::new("Workspace").with_property("FilteringEnabled", true),
        );
        dom.insert(
            workspace,
            InstanceBuilder::new("Camera").with_property(
                "CFrame",
                CFrame::new(
                    Vector3::new(0.0, 20.0, 20.0),
                    geometry::mat_from_orientation_deg(Vector3::new(-30.0, 0.0, 0.0)),
                ),
            ),
        );
        dom.insert(
            workspace,
            InstanceBuilder::new("Part")
                .with_name("Baseplate")
                .with_property("Anchored", true)
                .with_property("Locked", true)
                .with_property("Size", Vector3::new(2048.0, 16.0, 2048.0))
                .with_property(
                    "CFrame",
                    CFrame::new(Vector3::new(0.0, -8.0, 0.0), rbx_types::Matrix3::identity()),
                )
                .with_property("Color", rbx_types::Color3uint8::new(91, 91, 91)),
        );
        dom.insert(workspace, InstanceBuilder::new("Terrain"));
        for service in [
            "Players",
            "Lighting",
            "MaterialService",
            "ReplicatedFirst",
            "ReplicatedStorage",
            "ServerScriptService",
            "ServerStorage",
            "StarterGui",
            "StarterPack",
            "StarterPlayer",
            "Teams",
            "SoundService",
            "TextChatService",
        ] {
            let r = dom.insert(root, InstanceBuilder::new(service));
            if service == "Lighting" {
                dom.insert(r, InstanceBuilder::new("Sky"));
            }
            if service == "StarterPlayer" {
                dom.insert(r, InstanceBuilder::new("StarterPlayerScripts"));
                dom.insert(r, InstanceBuilder::new("StarterCharacterScripts"));
            }
        }
        Self::from_dom(dom, None)
    }

    pub fn open(path: &Path) -> Result<Self> {
        let dom = format::load_file(path)?;
        let dom = if format::is_model_path(path) {
            wrap_model(dom)
        } else {
            dom
        };
        Ok(Self::from_dom(dom, Some(path.to_owned())))
    }

    pub fn from_dom(mut dom: WeakDom, path: Option<PathBuf>) -> Self {
        compact_defaults(&mut dom);
        Self {
            dom,
            path,
            dirty: false,
            changes: Vec::new(),
            journal: Vec::new(),
        }
    }

    pub fn save(&mut self) -> Result<()> {
        let path = self
            .path
            .clone()
            .ok_or_else(|| anyhow::anyhow!("document has no path"))?;
        self.save_as(&path)
    }

    pub fn save_as(&mut self, path: &Path) -> Result<()> {
        if format::is_model_path(path) {
            let refs = self.workspace().map(|w| self.children(w).to_vec());
            format::save_file(path, &self.dom, refs.as_deref())?;
        } else {
            format::save_file(path, &self.dom, None)?;
        }
        self.path = Some(path.to_owned());
        self.dirty = false;
        Ok(())
    }

    /// Display name for title bars and tabs.
    pub fn title(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Place1".to_owned())
    }

    // --- read access -------------------------------------------------------

    pub fn dom(&self) -> &WeakDom {
        &self.dom
    }

    pub fn root(&self) -> Ref {
        self.dom.root_ref()
    }

    pub fn get(&self, r: Ref) -> Option<&Instance> {
        self.dom.get_by_ref(r)
    }

    pub fn exists(&self, r: Ref) -> bool {
        self.dom.get_by_ref(r).is_some()
    }

    pub fn children(&self, r: Ref) -> &[Ref] {
        self.dom.get_by_ref(r).map(|i| i.children()).unwrap_or(&[])
    }

    pub fn parent(&self, r: Ref) -> Ref {
        self.dom.get_by_ref(r).map(|i| i.parent()).unwrap_or_else(Ref::none)
    }

    pub fn find_first_child(&self, r: Ref, name: &str) -> Option<Ref> {
        self.children(r)
            .iter()
            .copied()
            .find(|c| self.get(*c).is_some_and(|i| i.name == name))
    }

    pub fn find_first_child_of_class(&self, r: Ref, class: &str) -> Option<Ref> {
        self.children(r)
            .iter()
            .copied()
            .find(|c| self.get(*c).is_some_and(|i| i.class == class))
    }

    /// Finds a top-level service by class name.
    pub fn service(&self, class: &str) -> Option<Ref> {
        self.find_first_child_of_class(self.root(), class)
    }

    pub fn workspace(&self) -> Option<Ref> {
        self.service("Workspace")
    }

    /// True if `r` is `ancestor` or one of its descendants.
    pub fn is_descendant_of(&self, mut r: Ref, ancestor: Ref) -> bool {
        while r.is_some() {
            if r == ancestor {
                return true;
            }
            r = self.parent(r);
        }
        false
    }

    /// True if the instance is reachable from the DataModel root.
    pub fn is_in_tree(&self, r: Ref) -> bool {
        self.is_descendant_of(r, self.root())
    }

    pub fn full_name(&self, r: Ref) -> String {
        let mut parts = Vec::new();
        let mut cur = r;
        let root = self.root();
        while cur.is_some() && cur != root {
            match self.get(cur) {
                Some(i) => parts.push(i.name.as_str()),
                None => break,
            }
            cur = self.parent(cur);
        }
        parts.reverse();
        parts.join(".")
    }

    /// Depth-first descendants of `r`, not including `r`.
    pub fn descendants(&self, r: Ref) -> Vec<Ref> {
        let mut out = Vec::new();
        let mut stack: Vec<Ref> = self.children(r).iter().rev().copied().collect();
        while let Some(c) = stack.pop() {
            out.push(c);
            stack.extend(self.children(c).iter().rev().copied());
        }
        out
    }

    pub fn is_a(&self, r: Ref, class: &str) -> bool {
        self.get(r).is_some_and(|i| reflection::is_a(&i.class, class))
    }

    /// Reads a property the way scripts and the Properties panel see it,
    /// including derived properties (`Position`, `Orientation`, `BrickColor`,
    /// ...) and reflection defaults for properties missing from the file.
    pub fn get_property(&self, r: Ref, name: &str) -> Option<Variant> {
        let inst = self.get(r)?;
        let class = inst.class.as_str();
        match name {
            "Name" => return Some(Variant::String(inst.name.clone())),
            "ClassName" => return Some(Variant::String(class.to_owned())),
            "Parent" => return Some(Variant::Ref(inst.parent())),
            _ => {}
        }
        if reflection::is_a(class, "BasePart") || class == "Model" {
            match name {
                "Position" => return self.cframe_of(r).map(|c| Variant::Vector3(c.position)),
                "Orientation" | "Rotation" => {
                    return self
                        .cframe_of(r)
                        .map(|c| Variant::Vector3(geometry::mat_to_orientation_deg(&c.orientation)));
                }
                "BrickColor" => {
                    let color = inst
                        .properties
                        .get(&ustr("Color"))
                        .or_else(|| reflection::default_value(class, "Color"));
                    if let Some(Variant::Color3uint8(c)) = color {
                        return Some(Variant::BrickColor(geometry::nearest_brick_color(*c)));
                    }
                }
                _ => {}
            }
        }
        let canonical = reflection::canonical_name(class, name);
        if let Some(v) = inst.properties.get(&ustr(&canonical)) {
            return Some(v.clone());
        }
        reflection::default_value(class, &canonical).cloned()
    }

    fn cframe_of(&self, r: Ref) -> Option<CFrame> {
        let inst = self.get(r)?;
        if let Some(Variant::CFrame(c)) = inst.properties.get(&ustr("CFrame")) {
            return Some(*c);
        }
        if inst.class == "Model" {
            return Some(self.model_pivot(r));
        }
        reflection::default_value(&inst.class, "CFrame").and_then(|v| match v {
            Variant::CFrame(c) => Some(*c),
            _ => None,
        })
    }

    /// `Model:GetPivot()`: PrimaryPart's CFrame, else WorldPivotData, else the
    /// bounding-box centre of descendant parts.
    pub fn model_pivot(&self, r: Ref) -> CFrame {
        let identity = |p| CFrame::new(p, rbx_types::Matrix3::identity());
        if let Some(Variant::Ref(pp)) = self.get_raw(r, "PrimaryPart")
            && let Some(c) = self.cframe_of(pp) {
                return c;
            }
        if let Some(Variant::OptionalCFrame(Some(c))) = self.get_raw(r, "WorldPivotData") {
            return c;
        }
        match self.bounds(r) {
            Some((min, max)) => identity(Vector3::new(
                (min.x + max.x) * 0.5,
                (min.y + max.y) * 0.5,
                (min.z + max.z) * 0.5,
            )),
            None => identity(Vector3::new(0.0, 0.0, 0.0)),
        }
    }

    /// World-space AABB of all BaseParts in the subtree (including `r`).
    pub fn bounds(&self, r: Ref) -> Option<(Vector3, Vector3)> {
        let mut min = Vector3::new(f32::MAX, f32::MAX, f32::MAX);
        let mut max = Vector3::new(f32::MIN, f32::MIN, f32::MIN);
        let mut any = false;
        let mut visit = |r: Ref| {
            let Some(g) = self.get(r).and_then(geometry::part_geometry) else {
                return;
            };
            any = true;
            let half = Vector3::new(g.size.x * 0.5, g.size.y * 0.5, g.size.z * 0.5);
            let m = &g.cframe.orientation;
            // Extent of a rotated box along each world axis.
            let ext = |row: &Vector3| row.x.abs() * half.x + row.y.abs() * half.y + row.z.abs() * half.z;
            let e = Vector3::new(ext(&m.x), ext(&m.y), ext(&m.z));
            let p = g.cframe.position;
            min = Vector3::new(min.x.min(p.x - e.x), min.y.min(p.y - e.y), min.z.min(p.z - e.z));
            max = Vector3::new(max.x.max(p.x + e.x), max.y.max(p.y + e.y), max.z.max(p.z + e.z));
        };
        visit(r);
        for d in self.descendants(r) {
            visit(d);
        }
        any.then_some((min, max))
    }

    /// Reads a stored property without derivation or defaults.
    pub fn get_raw(&self, r: Ref, name: &str) -> Option<Variant> {
        self.get(r)?.properties.get(&ustr(name)).cloned()
    }

    // --- mutation ----------------------------------------------------------

    /// Changes collected since the last [`Document::take_changes`].
    pub fn changes(&self) -> &[Change] {
        &self.changes
    }

    pub fn take_changes(&mut self) -> Vec<Change> {
        std::mem::take(&mut self.changes)
    }

    fn record(&mut self, change: Change) {
        self.dirty = true;
        self.changes.push(change);
    }

    /// Writes a property, handling derived properties. Returns the previous
    /// stored values so callers can build undo records.
    pub fn set_property(&mut self, r: Ref, name: &str, value: Variant) -> Result<(), String> {
        let class = self
            .get(r)
            .map(|i| i.class)
            .ok_or_else(|| "instance no longer exists".to_owned())?;
        match (name, value) {
            ("Name", Variant::String(s)) => {
                self.set_name(r, s);
                Ok(())
            }
            ("Name", _) => Err("Name must be a string".into()),
            ("ClassName", _) => Err("ClassName is read-only".into()),
            ("Parent", Variant::Ref(p)) => self.set_parent(r, p),
            ("Position", Variant::Vector3(p)) if self.cframe_of(r).is_some() => {
                let mut c = self.cframe_of(r).unwrap();
                c.position = p;
                self.set_cframe(r, c)
            }
            ("Orientation" | "Rotation", Variant::Vector3(o)) if self.cframe_of(r).is_some() => {
                let mut c = self.cframe_of(r).unwrap();
                c.orientation = geometry::mat_from_orientation_deg(o);
                self.set_cframe(r, c)
            }
            ("CFrame", Variant::CFrame(c)) if class == "Model" => self.set_cframe(r, c),
            ("BrickColor", Variant::BrickColor(b)) => {
                self.write_raw(r, "Color", Variant::Color3uint8(b.to_color3uint8()));
                Ok(())
            }
            ("Color", Variant::Color3(c)) if reflection::is_a(&class, "BasePart") => {
                self.write_raw(r, "Color", Variant::Color3uint8(geometry::color3_to_u8(c)));
                Ok(())
            }
            (_, value) => {
                let canonical = reflection::canonical_name(&class, name);
                self.write_raw(r, &canonical, value);
                Ok(())
            }
        }
    }

    pub fn set_name(&mut self, r: Ref, name: String) {
        if let Some(inst) = self.dom.get_by_ref_mut(r) {
            if inst.name == name {
                return;
            }
            let old = std::mem::replace(&mut inst.name, name.clone());
            self.journal.push(RawOp::Name { r, old, new: name });
            self.record(Change::Property(r, ustr("Name")));
        }
    }

    /// Writes a stored property verbatim and records the change.
    pub fn write_raw(&mut self, r: Ref, name: &str, value: Variant) {
        self.write_raw_opt(r, ustr(name), Some(value));
    }

    pub(crate) fn write_raw_opt(&mut self, r: Ref, key: Ustr, value: Option<Variant>) {
        if let Some(inst) = self.dom.get_by_ref_mut(r) {
            if inst.properties.get(&key) == value.as_ref() {
                return;
            }
            let old = match &value {
                Some(v) => inst.properties.insert(key, v.clone()),
                None => inst.properties.remove(&key),
            };
            self.journal.push(RawOp::Property { r, key, old, new: value });
            self.record(Change::Property(r, key));
        }
    }

    /// Moves a part, or a model and all of its descendant parts.
    pub fn set_cframe(&mut self, r: Ref, target: CFrame) -> Result<(), String> {
        let class = self.get(r).map(|i| i.class).ok_or("instance no longer exists")?;
        if class == "Model" {
            let pivot = self.model_pivot(r);
            // delta = target * pivot^-1
            let inv_rot = pivot.orientation.transpose();
            let delta_rot = geometry::mat_mul(&target.orientation, &inv_rot);
            for d in self.descendants(r) {
                if let Some(c) = self.cframe_of(d).filter(|_| self.is_a(d, "BasePart")) {
                    let rel = Vector3::new(
                        c.position.x - pivot.position.x,
                        c.position.y - pivot.position.y,
                        c.position.z - pivot.position.z,
                    );
                    let p = geometry::mat_mul_vec(&delta_rot, &rel);
                    let new = CFrame::new(
                        Vector3::new(
                            target.position.x + p.x,
                            target.position.y + p.y,
                            target.position.z + p.z,
                        ),
                        geometry::mat_mul(&delta_rot, &c.orientation),
                    );
                    self.write_raw(d, "CFrame", Variant::CFrame(new));
                }
            }
            if self.get_raw(r, "WorldPivotData").is_some() {
                self.write_raw(r, "WorldPivotData", Variant::OptionalCFrame(Some(target)));
            }
            self.record(Change::Property(r, ustr("CFrame")));
            return Ok(());
        }
        self.write_raw(r, "CFrame", Variant::CFrame(target));
        Ok(())
    }

    /// Creates an instance under `parent` and returns its referent.
    pub fn insert(&mut self, parent: Ref, builder: InstanceBuilder) -> Ref {
        // Insert detached, then parent, so undo can simply detach again.
        let r = self.dom.insert(Ref::none(), builder);
        if parent.is_some() {
            self.dom.transfer_within(r, parent);
            self.journal.push(RawOp::Parent { r, old: Ref::none(), new: parent });
            self.record(Change::Added(r));
        }
        r
    }

    /// Creates an instance of `class` with reflection defaults applied for
    /// the properties that matter visually.
    pub fn create(&mut self, class: &str, parent: Ref) -> Ref {
        self.insert(parent, new_instance(class))
    }

    /// Reparents an instance. `Ref::none()` detaches it (like `Parent = nil`).
    pub fn set_parent(&mut self, r: Ref, new_parent: Ref) -> Result<(), String> {
        if r == self.root() {
            return Err("cannot reparent the DataModel".into());
        }
        if !self.exists(r) {
            return Err("instance no longer exists".into());
        }
        if new_parent.is_some() {
            if !self.exists(new_parent) {
                return Err("new parent does not exist".into());
            }
            if self.is_descendant_of(new_parent, r) {
                return Err("attempt to set parent to self or descendant".into());
            }
        }
        let old = self.parent(r);
        if old == new_parent {
            return Ok(());
        }
        self.dom.transfer_within(r, new_parent);
        self.journal.push(RawOp::Parent { r, old, new: new_parent });
        self.record(Change::Reparented { instance: r, old, new: new_parent });
        Ok(())
    }

    /// Deep-copies an instance. The copy has no parent.
    pub fn clone_instance(&mut self, r: Ref) -> Option<Ref> {
        self.exists(r).then(|| self.dom.clone_within(r))
    }

    /// Permanently frees a detached subtree (used when undo history drops it).
    pub fn purge(&mut self, r: Ref) {
        if self.exists(r) && self.parent(r).is_none() && r != self.root() {
            self.dom.destroy(r);
        }
    }

    /// Copies a subtree into a standalone DOM (clipboard, model export).
    pub fn export(&self, refs: &[Ref]) -> WeakDom {
        let mut out = WeakDom::new(InstanceBuilder::new("DataModel"));
        let root = out.root_ref();
        let roots = self.dom.clone_multiple_into_external(refs, &mut out);
        for r in roots {
            out.transfer_within(r, root);
        }
        out
    }

    /// Copies every child of `source`'s root into this document under
    /// `parent`, returning the new top-level refs.
    pub fn import(&mut self, source: &WeakDom, parent: Ref) -> Vec<Ref> {
        let children = source.root().children().to_vec();
        let new = source.clone_multiple_into_external(&children, &mut self.dom);
        for r in &new {
            self.dom.transfer_within(*r, parent);
            self.journal.push(RawOp::Parent { r: *r, old: Ref::none(), new: parent });
            self.record(Change::Added(*r));
        }
        new
    }

    /// Converts a part color (or any color property) to an f32 triple.
    pub fn color_of(&self, r: Ref, name: &str) -> Option<[f32; 3]> {
        match self.get_property(r, name)? {
            Variant::Color3(Color3 { r, g, b }) => Some([r, g, b]),
            Variant::Color3uint8(c) => Some(color3uint8_to_f32(c)),
            Variant::BrickColor(b) => Some(color3uint8_to_f32(b.to_color3uint8())),
            _ => None,
        }
    }
}

/// Builds a new instance with sensible Studio-like defaults.
pub fn new_instance(class: &str) -> InstanceBuilder {
    let mut b = InstanceBuilder::new(class);
    if reflection::is_a(class, "BasePart") {
        let size = match class {
            "Part" | "SpawnLocation" => Vector3::new(4.0, 1.0, 2.0),
            "WedgePart" | "CornerWedgePart" => Vector3::new(4.0, 1.0, 2.0),
            "TrussPart" => Vector3::new(2.0, 10.0, 2.0),
            _ => Vector3::new(4.0, 1.0, 2.0),
        };
        b = b
            .with_property("Size", size)
            .with_property("Anchored", false)
            .with_property("Color", BrickColor::MediumStoneGrey.to_color3uint8())
            .with_property(
                "CFrame",
                CFrame::new(Vector3::new(0.0, 0.5, 0.0), rbx_types::Matrix3::identity()),
            );
    }
    if reflection::is_a(class, "LuaSourceContainer") {
        let source = match class {
            "ModuleScript" => "local module = {}\n\nreturn module\n",
            _ => "print(\"Hello world!\")\n",
        };
        b = b.with_property("Source", source.to_owned());
    }
    b
}

/// Wraps a loaded model (`.rbxm`) inside a DataModel/Workspace so it can be
/// edited like a place.
fn wrap_model(mut model: WeakDom) -> WeakDom {
    let mut place = Document::new_place();
    let ws = place.workspace().unwrap();
    let children = model.root().children().to_vec();
    for c in children {
        model.transfer(c, &mut place.dom, ws);
    }
    place.dom
}

/// Drops stored properties whose value equals the class default from the
/// reflection database. Place files store nearly every property of every
/// instance; most are defaults, and keeping them costs several KB per part.
/// Reads fall back to the defaults ([`Document::get_property`]) and the
/// binary/XML serializers write defaults for missing properties, so this is
/// invisible to users while cutting DOM memory substantially.
pub fn compact_defaults(dom: &mut WeakDom) -> usize {
    let refs: Vec<Ref> = dom.descendants().map(|i| i.referent()).collect();
    let mut removed = 0;
    for r in refs {
        let Some(inst) = dom.get_by_ref_mut(r) else { continue };
        let class = inst.class;
        inst.properties.retain(|key, value| {
            // Keep identity/bookkeeping properties that tools rely on.
            if matches!(key.as_str(), "UniqueId" | "HistoryId" | "Capabilities" | "Source") {
                return true;
            }
            let is_default = reflection::default_value(&class, key).is_some_and(|d| d == value);
            if is_default {
                removed += 1;
            }
            !is_default
        });
        inst.properties.shrink_to_fit();
    }
    removed
}
