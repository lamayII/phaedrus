//! Helpers over the bundled Roblox reflection database.

use std::collections::HashMap;
use std::sync::OnceLock;

use rbx_reflection::{
    ClassDescriptor, DataType, PropertyDescriptor, PropertyKind,
    ReflectionDatabase, Scriptability,
};
use rbx_types::Variant;

pub use rbx_reflection;

/// The reflection database bundled with `rbx_reflection_database`.
pub fn database() -> &'static ReflectionDatabase<'static> {
    rbx_reflection_database::get_bundled()
}

pub fn class(name: &str) -> Option<&'static ClassDescriptor<'static>> {
    database().classes.get(name)
}

/// Mirrors `Instance:IsA(name)`.
pub fn is_a(class_name: &str, base: &str) -> bool {
    if class_name == base {
        return true;
    }
    let db = database();
    let Some(mut desc) = db.classes.get(class_name) else {
        return false;
    };
    loop {
        if desc.name == base {
            return true;
        }
        match desc.superclass.and_then(|s| db.classes.get(s)) {
            Some(s) => desc = s,
            None => return false,
        }
    }
}

/// Finds a property descriptor by walking the class hierarchy.
pub fn find_property(
    class_name: &str,
    property: &str,
) -> Option<&'static PropertyDescriptor<'static>> {
    let db = database();
    let mut desc = db.classes.get(class_name)?;
    loop {
        if let Some(p) = desc.properties.get(property) {
            return Some(p);
        }
        desc = db.classes.get(desc.superclass?)?;
    }
}

/// Resolves aliases (e.g. `size` -> `Size`) to the canonical name the
/// property is stored under in a [`rbx_dom_weak::Instance`]. Serialization
/// names (`SerializesAs`) are a file-format detail handled by
/// `rbx_binary`/`rbx_xml` and are deliberately not followed.
pub fn canonical_name(class_name: &str, property: &str) -> String {
    let Some(desc) = find_property(class_name, property) else {
        return property.to_owned();
    };
    match &desc.kind {
        PropertyKind::Alias { alias_for } => (*alias_for).to_owned(),
        _ => desc.name.to_owned(),
    }
}

/// The default value of a property for a freshly created instance of
/// `class_name`, if the database knows one.
pub fn default_value(class_name: &str, property: &str) -> Option<&'static Variant> {
    let db = database();
    let mut desc = db.classes.get(class_name)?;
    loop {
        if let Some(v) = desc.default_properties.get(property) {
            return Some(v);
        }
        desc = db.classes.get(desc.superclass?)?;
    }
}

/// Converts an enum item name to its integer value.
pub fn enum_value(enum_name: &str, item: &str) -> Option<u32> {
    database().enums.get(enum_name)?.items.get(item).copied()
}

/// Converts an enum integer value to its item name.
pub fn enum_item_name(enum_name: &str, value: u32) -> Option<&'static str> {
    database()
        .enums
        .get(enum_name)?
        .items
        .iter()
        .find(|(_, v)| **v == value)
        .map(|(k, _)| *k)
}

/// Sorted item names for an enum, ordered by value.
pub fn enum_items(enum_name: &str) -> Vec<(&'static str, u32)> {
    let Some(e) = database().enums.get(enum_name) else {
        return Vec::new();
    };
    let mut items: Vec<_> = e.items.iter().map(|(k, v)| (*k, *v)).collect();
    items.sort_by_key(|(_, v)| *v);
    items
}

/// The enum type name of a property, if the property is enum-typed.
pub fn property_enum(class_name: &str, property: &str) -> Option<&'static str> {
    match &find_property(class_name, property)?.data_type {
        DataType::Enum(name) => Some(name),
        _ => None,
    }
}

/// A property as it should be shown in the Properties panel.
#[derive(Debug, Clone)]
pub struct VisibleProperty {
    pub name: &'static str,
    pub category: &'static str,
    pub read_only: bool,
    pub descriptor: &'static PropertyDescriptor<'static>,
}

/// Builds (and caches) the list of user-visible properties per class.
pub fn visible_properties(class_name: &str) -> &'static [VisibleProperty] {
    static CACHE: OnceLock<std::sync::Mutex<HashMap<String, &'static [VisibleProperty]>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let mut cache = cache.lock().unwrap();
    if let Some(v) = cache.get(class_name) {
        return v;
    }
    let mut out = Vec::new();
    let db = database();
    let mut current = db.classes.get(class_name);
    while let Some(desc) = current {
        for (name, prop) in &desc.properties {
            if matches!(prop.kind, PropertyKind::Alias { .. }) {
                continue;
            }
            let readable = matches!(
                prop.scriptability,
                Scriptability::ReadWrite | Scriptability::Read
            );
            if !readable || prop.tags.contains(&rbx_reflection::PropertyTag::Deprecated) {
                continue;
            }
            if prop.tags.contains(&rbx_reflection::PropertyTag::Hidden)
                || prop.tags.contains(&rbx_reflection::PropertyTag::NotBrowsable)
            {
                continue;
            }
            out.push(VisibleProperty {
                name,
                category: category_for(desc.name, name),
                read_only: matches!(prop.scriptability, Scriptability::Read)
                    || prop.tags.contains(&rbx_reflection::PropertyTag::ReadOnly),
                descriptor: prop,
            });
        }
        current = desc.superclass.and_then(|s| db.classes.get(s));
    }
    out.sort_by(|a, b| {
        category_rank(a.category)
            .cmp(&category_rank(b.category))
            .then(a.name.cmp(b.name))
    });
    let leaked: &'static [VisibleProperty] = Box::leak(out.into_boxed_slice());
    cache.insert(class_name.to_owned(), leaked);
    leaked
}

const CATEGORY_ORDER: &[&str] = &[
    "Data",
    "Appearance",
    "Text",
    "Transform",
    "Pivot",
    "Behavior",
    "Collision",
    "Part",
    "Assembly",
    "Surface",
    "Lighting",
    "Environment",
    "Camera",
    "Image",
    "Script",
    "Other",
];

/// Sort key for a Properties-panel category.
pub fn category_rank(cat: &str) -> usize {
    CATEGORY_ORDER
        .iter()
        .position(|c| *c == cat)
        .unwrap_or(CATEGORY_ORDER.len())
}

/// Roblox Studio groups properties into categories. The reflection database
/// we bundle does not carry categories, so approximate them from names.
fn category_for(declaring_class: &str, prop: &str) -> &'static str {
    match prop {
        "Name" | "ClassName" | "Parent" | "Archivable" => "Data",
        "Color" | "BrickColor" | "Material" | "MaterialVariant" | "Reflectance"
        | "Transparency" | "CastShadow" | "Texture" | "Color3" | "Face" => "Appearance",
        "Position" | "Orientation" | "Origin" | "Size" | "CFrame" | "Rotation" => "Transform",
        "PivotOffset" | "WorldPivot" => "Pivot",
        "Anchored" | "Locked" | "Massless" | "Enabled" | "Disabled" | "RunContext" => "Behavior",
        "CanCollide" | "CanTouch" | "CanQuery" | "CollisionGroup" => "Collision",
        "Shape" => "Part",
        "Source" | "LinkedSource" => "Script",
        _ if prop.ends_with("Surface") || prop.ends_with("SurfaceInput") => "Surface",
        _ if prop.starts_with("Assembly") => "Assembly",
        _ if prop.starts_with("Text") || prop == "Font" || prop == "FontFace" => "Text",
        _ if prop.starts_with("Image") => "Image",
        _ => match declaring_class {
            "Lighting" => "Lighting",
            "Sky" | "Atmosphere" | "Clouds" => "Environment",
            "Camera" => "Camera",
            "Instance" => "Data",
            _ => "Other",
        },
    }
}

/// Names of every creatable class (used by "Insert Object").
pub fn creatable_classes() -> Vec<&'static str> {
    let mut v: Vec<_> = database()
        .classes
        .values()
        .filter(|c| {
            !c.tags.contains(&rbx_reflection::ClassTag::NotCreatable)
                && !c.tags.contains(&rbx_reflection::ClassTag::Service)
                && !c.tags.contains(&rbx_reflection::ClassTag::Deprecated)
        })
        .map(|c| c.name)
        .collect();
    v.sort_unstable();
    v
}

pub fn is_service(class_name: &str) -> bool {
    class(class_name).is_some_and(|c| c.tags.contains(&rbx_reflection::ClassTag::Service))
}
