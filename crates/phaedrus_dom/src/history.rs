//! Undo/redo built on the document's raw-op journal, modelled after
//! `ChangeHistoryService` waypoints.

use rbx_types::Ref;

use crate::document::{Document, RawOp};

struct Waypoint {
    name: String,
    ops: Vec<RawOp>,
}

#[derive(Default)]
pub struct History {
    undo: Vec<Waypoint>,
    redo: Vec<Waypoint>,
    /// Maximum number of waypoints kept. Old waypoints are dropped to bound
    /// memory use.
    pub limit: usize,
}

impl History {
    pub fn new() -> Self {
        Self { undo: Vec::new(), redo: Vec::new(), limit: 256 }
    }

    /// Commits everything done since the previous waypoint as one undo step.
    pub fn set_waypoint(&mut self, doc: &mut Document, name: impl Into<String>) {
        if doc.journal.is_empty() {
            return;
        }
        let ops = std::mem::take(&mut doc.journal);
        self.undo.push(Waypoint { name: name.into(), ops });
        let mut dropped: Vec<Waypoint> = std::mem::take(&mut self.redo);
        if self.undo.len() > self.limit {
            dropped.push(self.undo.remove(0));
        }
        self.purge_unreachable(doc, &dropped);
    }

    /// Drops uncommitted ops without undoing them (e.g. script mutations that
    /// should not appear in history).
    pub fn discard_pending(&mut self, doc: &mut Document) {
        doc.journal.clear();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_name(&self) -> Option<&str> {
        self.undo.last().map(|w| w.name.as_str())
    }

    pub fn redo_name(&self) -> Option<&str> {
        self.redo.last().map(|w| w.name.as_str())
    }

    pub fn undo(&mut self, doc: &mut Document) -> bool {
        // Anything pending becomes its own waypoint first so it is not lost.
        self.set_waypoint(doc, "Uncommitted");
        let Some(wp) = self.undo.pop() else { return false };
        for op in wp.ops.iter().rev() {
            apply(doc, op, true);
        }
        doc.journal.clear();
        self.redo.push(wp);
        true
    }

    pub fn redo(&mut self, doc: &mut Document) -> bool {
        let Some(wp) = self.redo.pop() else { return false };
        for op in &wp.ops {
            apply(doc, op, false);
        }
        doc.journal.clear();
        self.undo.push(wp);
        true
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

fn apply(doc: &mut Document, op: &RawOp, reverse: bool) {
    match op {
        RawOp::Property { r, key, old, new } => {
            let v = if reverse { old } else { new };
            doc.write_raw_opt(*r, *key, v.clone());
        }
        RawOp::Name { r, old, new } => {
            doc.set_name(*r, if reverse { old.clone() } else { new.clone() });
        }
        RawOp::Parent { r, old, new } => {
            let target = if reverse { *old } else { *new };
            let _ = doc.set_parent(*r, target);
        }
    }
}

impl History {
    /// Frees detached instances that only the dropped waypoints referred
    /// to: nothing can bring them back into the tree any more.
    fn purge_unreachable(&self, doc: &mut Document, dropped: &[Waypoint]) {
        let referenced = |r: Ref| {
            self.undo.iter().chain(&self.redo).flat_map(|w| &w.ops).any(|op| match op {
                RawOp::Parent { r: x, .. } | RawOp::Property { r: x, .. } | RawOp::Name { r: x, .. } => *x == r,
            })
        };
        for op in dropped.iter().flat_map(|w| &w.ops) {
            if let RawOp::Parent { r, .. } = op {
                if doc.exists(*r) && doc.parent(*r).is_none() && !referenced(*r) {
                    doc.purge(*r);
                }
            }
        }
    }
}
