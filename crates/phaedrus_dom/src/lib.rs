//! Phaedrus data model.
//!
//! Wraps the `rbx-dom` ecosystem (`rbx_dom_weak`, `rbx_binary`, `rbx_xml`,
//! `rbx_reflection_database`) into a [`Document`] type that the editor, the
//! renderer and the Luau VM all share. Every mutation that should be visible
//! to other subsystems goes through [`Document`] so it can be recorded in the
//! change journal (used for viewport sync and undo/redo).

pub mod document;
pub mod format;
pub mod geometry;
pub mod history;
pub mod reflection;
pub mod terrain;

pub use document::{Change, Document};
pub use format::{FileFormat, load_file, save_file};
pub use history::History;

pub use rbx_dom_weak::{self, Instance, InstanceBuilder, Ustr, WeakDom, ustr};
pub use rbx_types::{self as types, Ref, Variant};
