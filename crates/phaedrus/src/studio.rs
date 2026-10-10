//! Editor-wide state: the open document, selection, undo history, tools and
//! play state. Everything the UI and the viewport share lives here.

use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use bevy::prelude::*;
use parking_lot::{Mutex, MutexGuard};
use phaedrus_dom::document::new_instance;
use phaedrus_dom::types::{CFrame, Matrix3, Ref, Variant, Vector3};
use phaedrus_dom::{Change, Document, History, WeakDom, reflection};
use phaedrus_luau::{OutputLevel, OutputLine, SharedDoc, SharedOutput};

/// Transform tools, matching Studio's Ctrl+1..4 shortcuts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tool {
    #[default]
    Select,
    Move,
    Scale,
    Rotate,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snap {
    pub move_enabled: bool,
    pub move_step: f32,
    pub rotate_enabled: bool,
    /// Degrees.
    pub rotate_step: f32,
}

impl Default for Snap {
    fn default() -> Self {
        Self { move_enabled: true, move_step: 1.0, rotate_enabled: true, rotate_step: 15.0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayState {
    Edit,
    /// Server scripts are running (Studio's "Run", F8).
    Running { paused: bool },
}

/// A document tab in the centre area.
#[derive(Debug, Clone, PartialEq)]
pub enum Tab {
    Viewport,
    Script(Ref),
}

#[derive(Resource)]
pub struct Studio {
    pub doc: SharedDoc,
    pub history: History,
    pub selection: Vec<Ref>,
    pub output: SharedOutput,
    pub tool: Tool,
    pub snap: Snap,
    /// Transform tools operate in local space when true (Ctrl+L).
    pub local_space: bool,
    pub play: PlayState,
    /// Edit-mode state stashed while a run is in progress.
    stash: Option<Stash>,
    pub clipboard: Option<WeakDom>,
    /// Bumped whenever the whole document is replaced.
    pub generation: u64,
    /// Changes collected at the start of this frame.
    pub frame_changes: Vec<Change>,
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    /// Commands queued for the scripting system (command bar input).
    pub pending_exec: Vec<String>,
    pub pending_run: Option<bool>,
    pub pending_stop: bool,
    /// Request to frame the camera on the selection.
    pub focus_request: bool,
    pub status: String,
    /// A place file being read on a background thread.
    pub loading: Option<PendingLoad>,
}

/// A file load running off the main thread so the UI stays responsive.
pub struct PendingLoad {
    pub path: PathBuf,
    pub started: Instant,
    handle: JoinHandle<anyhow::Result<Document>>,
}

impl Studio {
    pub fn new(doc: Document) -> Self {
        Self {
            doc: Arc::new(Mutex::new(doc)),
            history: History::new(),
            selection: Vec::new(),
            output: SharedOutput::default(),
            tool: Tool::Select,
            snap: Snap::default(),
            local_space: false,
            play: PlayState::Edit,
            stash: None,
            clipboard: None,
            generation: 1,
            frame_changes: Vec::new(),
            tabs: vec![Tab::Viewport],
            active_tab: 0,
            pending_exec: Vec::new(),
            pending_run: None,
            pending_stop: false,
            focus_request: false,
            status: String::new(),
            loading: None,
        }
    }

    pub fn doc(&self) -> MutexGuard<'_, Document> {
        self.doc.lock()
    }

    pub fn log(&self, level: OutputLevel, text: impl Into<String>) {
        self.output.lock().push(OutputLine {
            level,
            text: text.into(),
            time: std::time::SystemTime::now(),
            source: Some("Studio".into()),
        });
    }

    pub fn is_running(&self) -> bool {
        matches!(self.play, PlayState::Running { .. })
    }

    /// Commits pending edits as one undo step. Edits during a run are not
    /// recorded (they are discarded on stop).
    pub fn waypoint(&mut self, name: &str) {
        let mut doc = self.doc.lock();
        if self.is_running() {
            self.history.discard_pending(&mut doc);
        } else {
            self.history.set_waypoint(&mut doc, name);
        }
    }

    // --- files -------------------------------------------------------------

    pub fn replace_document(&mut self, doc: Document) {
        *self.doc.lock() = doc;
        self.history.clear();
        self.selection.clear();
        self.tabs = vec![Tab::Viewport];
        self.active_tab = 0;
        self.generation += 1;
        self.frame_changes.clear();
    }

    /// Starts loading `path` in the background; [`Self::poll_loading`]
    /// swaps it in once it is ready.
    pub fn open(&mut self, path: PathBuf) {
        if self.loading.is_some() {
            self.log(OutputLevel::Warn, "A file is already loading.");
            return;
        }
        if self.is_running() {
            self.log(OutputLevel::Warn, "Stop the running session before opening another file.");
            return;
        }
        let thread_path = path.clone();
        let spawned = std::thread::Builder::new()
            .name("place-loader".into())
            .spawn(move || Document::open(&thread_path));
        match spawned {
            Ok(handle) => {
                self.status = format!("Loading {}", path.display());
                self.loading = Some(PendingLoad { path, started: Instant::now(), handle });
            }
            Err(e) => self.log(OutputLevel::Error, format!("Failed to open {}: {e}", path.display())),
        }
    }

    /// Abandons the current load. The loader thread runs to completion in
    /// the background and its result is dropped.
    pub fn cancel_loading(&mut self) {
        if let Some(load) = self.loading.take() {
            self.status = String::new();
            self.log(OutputLevel::Info, format!("Cancelled opening {}", load.path.display()));
        }
    }

    /// Installs the loaded document once the loader thread has finished.
    pub fn poll_loading(&mut self) {
        if !self.loading.as_ref().is_some_and(|l| l.handle.is_finished()) {
            return;
        }
        let load = self.loading.take().unwrap();
        let path = load.path;
        let result = load.handle.join().unwrap_or_else(|_| Err(anyhow::anyhow!("loader thread panicked")));
        match result {
            Ok(doc) => {
                let title = doc.title();
                self.replace_document(doc);
                self.log(
                    OutputLevel::Info,
                    format!("Opened {} in {:.2}s", path.display(), load.started.elapsed().as_secs_f32()),
                );
                self.status = format!("Opened {title}");
            }
            Err(e) => {
                self.status = String::new();
                self.log(OutputLevel::Error, format!("Failed to open {}: {e:#}", path.display()));
            }
        }
    }

    pub fn save(&mut self, path: Option<PathBuf>) {
        if self.is_running() {
            self.log(OutputLevel::Warn, "Stop the running session before saving.");
            return;
        }
        let result = {
            let mut doc = self.doc.lock();
            match path {
                Some(p) => doc.save_as(&p).map(|_| p),
                None => doc.save().map(|_| doc.path.clone().unwrap_or_default()),
            }
        };
        match result {
            Ok(p) => self.log(OutputLevel::Info, format!("Saved {}", p.display())),
            Err(e) => self.log(OutputLevel::Error, format!("Save failed: {e:#}")),
        }
    }

    // --- selection ----------------------------------------------------------

    pub fn select_only(&mut self, r: Ref) {
        self.selection.clear();
        self.selection.push(r);
    }

    pub fn toggle_select(&mut self, r: Ref) {
        if let Some(i) = self.selection.iter().position(|s| *s == r) {
            self.selection.remove(i);
        } else {
            self.selection.push(r);
        }
    }

    pub fn is_selected(&self, r: Ref) -> bool {
        self.selection.contains(&r)
    }

    /// Drops selected instances that were removed from the tree.
    pub fn prune_selection(&mut self) {
        let doc = self.doc.lock();
        self.selection.retain(|r| doc.is_in_tree(*r));
    }

    /// Selection minus instances whose ancestor is also selected, and minus
    /// services (which cannot be moved or deleted).
    pub fn selection_roots(&self) -> Vec<Ref> {
        self.selection_roots_in(&self.doc.lock())
    }

    /// [`Self::selection_roots`] for callers already holding the document
    /// lock (the mutex is not re-entrant).
    pub fn selection_roots_in(&self, doc: &Document) -> Vec<Ref> {
        let root = doc.root();
        self.selection
            .iter()
            .copied()
            .filter(|r| *r != root && doc.parent(*r) != root)
            .filter(|r| {
                !self.selection.iter().any(|o| o != r && doc.is_descendant_of(*r, *o))
            })
            .collect()
    }

    // --- edit actions -------------------------------------------------------

    pub fn undo(&mut self) {
        if self.is_running() {
            return;
        }
        let name = self.history.undo_name().map(str::to_owned);
        let done = {
            let mut doc = self.doc.lock();
            self.history.undo(&mut doc)
        };
        if done {
            self.status = format!("Undo {}", name.unwrap_or_default());
        }
        self.prune_selection();
    }

    pub fn redo(&mut self) {
        if self.is_running() {
            return;
        }
        let name = self.history.redo_name().map(str::to_owned);
        let done = {
            let mut doc = self.doc.lock();
            self.history.redo(&mut doc)
        };
        if done {
            self.status = format!("Redo {}", name.unwrap_or_default());
        }
        self.prune_selection();
    }

    pub fn delete_selection(&mut self) {
        let roots = self.selection_roots();
        if roots.is_empty() {
            return;
        }
        {
            let mut doc = self.doc.lock();
            for r in &roots {
                let _ = doc.set_parent(*r, Ref::none());
            }
        }
        self.selection.clear();
        self.waypoint("Delete");
    }

    pub fn copy_selection(&mut self) {
        let roots = self.selection_roots();
        if roots.is_empty() {
            return;
        }
        self.clipboard = Some(self.doc.lock().export(&roots));
    }

    pub fn cut_selection(&mut self) {
        self.copy_selection();
        self.delete_selection();
    }

    /// Pastes into the parent of the first selected instance (Studio's
    /// default "Paste"), or Workspace.
    pub fn paste(&mut self, into_selection: bool) {
        let Some(clip) = self.clipboard.take() else { return };
        let new = {
            let mut doc = self.doc.lock();
            let target = self
                .selection
                .first()
                .copied()
                .filter(|r| doc.exists(*r))
                .map(|r| if into_selection { r } else { doc.parent(r) })
                .filter(|r| r.is_some())
                .or_else(|| doc.workspace())
                .unwrap_or_else(|| doc.root());
            doc.import(&clip, target)
        };
        self.clipboard = Some(clip);
        self.selection = new;
        self.waypoint("Paste");
    }

    pub fn duplicate_selection(&mut self) {
        let roots = self.selection_roots();
        let mut new = Vec::new();
        {
            let mut doc = self.doc.lock();
            for r in roots {
                let parent = doc.parent(r);
                if let Some(c) = doc.clone_instance(r) {
                    let _ = doc.set_parent(c, parent);
                    new.push(c);
                }
            }
        }
        if !new.is_empty() {
            self.selection = new;
            self.waypoint("Duplicate");
        }
    }

    /// Ctrl+G: wraps the selection in a new Model.
    pub fn group_selection(&mut self) {
        let roots = self.selection_roots();
        if roots.is_empty() {
            return;
        }
        let model = {
            let mut doc = self.doc.lock();
            let parent = doc.parent(roots[0]);
            let model = doc.insert(parent, new_instance("Model"));
            for r in &roots {
                let _ = doc.set_parent(*r, model);
            }
            model
        };
        self.selection = vec![model];
        self.waypoint("Group");
    }

    /// Ctrl+U: moves a Model's children to its parent and removes it.
    pub fn ungroup_selection(&mut self) {
        let roots = self.selection_roots();
        let mut new_sel = Vec::new();
        {
            let mut doc = self.doc.lock();
            for r in roots {
                if !doc.is_a(r, "Model") && doc.get(r).is_none_or(|i| i.class != "Folder") {
                    continue;
                }
                let parent = doc.parent(r);
                for c in doc.children(r).to_vec() {
                    let _ = doc.set_parent(c, parent);
                    new_sel.push(c);
                }
                let _ = doc.set_parent(r, Ref::none());
            }
        }
        if !new_sel.is_empty() {
            self.selection = new_sel;
            self.waypoint("Ungroup");
        }
    }

    /// Inserts a new instance. Parts go into Workspace in front of the
    /// camera; other classes go under the first selected instance.
    pub fn insert_object(&mut self, class: &str, at: Option<Vec3>) -> Option<Ref> {
        reflection::class(class)?;
        let r = {
            let mut doc = self.doc.lock();
            let is_part = reflection::is_a(class, "BasePart");
            let parent = if is_part {
                self.selection
                    .first()
                    .copied()
                    .filter(|r| doc.is_a(*r, "Model") || doc.get(*r).is_some_and(|i| i.class == "Folder"))
                    .or_else(|| doc.workspace())
            } else {
                self.selection.first().copied().filter(|r| doc.exists(*r)).or_else(|| doc.workspace())
            }
            .unwrap_or_else(|| doc.root());
            let mut builder = new_instance(class);
            if is_part {
                let p = at.unwrap_or(Vec3::new(0.0, 0.5, 0.0));
                builder = builder.with_property(
                    "CFrame",
                    CFrame::new(Vector3::new(p.x, p.y, p.z), Matrix3::identity()),
                );
                if class == "Part" {
                    builder = builder.with_property("Anchored", true);
                }
            }
            doc.insert(parent, builder)
        };
        self.selection = vec![r];
        self.waypoint(&format!("Insert {class}"));
        Some(r)
    }

    /// Sets a property on every selected instance that has it.
    pub fn set_on_selection(&mut self, name: &str, value: Variant) {
        {
            let mut doc = self.doc.lock();
            for r in &self.selection {
                if doc.get_property(*r, name).is_some() {
                    let _ = doc.set_property(*r, name, value.clone());
                }
            }
        }
        self.waypoint(&format!("Set {name}"));
    }

    pub fn toggle_bool_on_selection(&mut self, name: &str) {
        let current = {
            let doc = self.doc.lock();
            self.selection
                .iter()
                .find_map(|r| match doc.get_property(*r, name) {
                    Some(Variant::Bool(b)) => Some(b),
                    _ => None,
                })
        };
        if let Some(b) = current {
            self.set_on_selection(name, Variant::Bool(!b));
        }
    }

    pub fn open_script(&mut self, r: Ref) {
        if let Some(i) = self.tabs.iter().position(|t| *t == Tab::Script(r)) {
            self.active_tab = i;
        } else {
            self.tabs.push(Tab::Script(r));
            self.active_tab = self.tabs.len() - 1;
        }
    }

    pub fn close_tab(&mut self, i: usize) {
        if i == 0 || i >= self.tabs.len() {
            return;
        }
        self.tabs.remove(i);
        if self.active_tab >= i {
            self.active_tab = self.active_tab.saturating_sub(1);
        }
    }

    pub fn active_script(&self) -> Option<Ref> {
        match self.tabs.get(self.active_tab) {
            Some(Tab::Script(r)) => Some(*r),
            _ => None,
        }
    }

    // --- play ---------------------------------------------------------------

    /// Starts a run on a copy of the document. The edit document (with its
    /// undo history and selection) is stashed and restored on stop.
    pub fn begin_run(&mut self) -> bool {
        if self.is_running() {
            return false;
        }
        let mut doc = self.doc.lock();
        self.history.set_waypoint(&mut doc, "Before run");
        let copy = match phaedrus_dom::format::clone_dom(doc.dom()) {
            Ok(dom) => Document::from_dom(dom, doc.path.clone()),
            Err(e) => {
                drop(doc);
                self.log(OutputLevel::Error, format!("Could not copy place for run: {e:#}"));
                return false;
            }
        };
        let original = std::mem::replace(&mut *doc, copy);
        drop(doc);
        self.stash = Some(Stash { doc: original, selection: std::mem::take(&mut self.selection), tabs: self.tabs.clone(), active_tab: self.active_tab });
        self.tabs.truncate(1);
        self.active_tab = 0;
        self.play = PlayState::Running { paused: false };
        self.generation += 1;
        self.frame_changes.clear();
        true
    }

    /// Discards the run copy and restores the edit document.
    pub fn end_run(&mut self) {
        if !self.is_running() {
            return;
        }
        self.play = PlayState::Edit;
        if let Some(stash) = self.stash.take() {
            *self.doc.lock() = stash.doc;
            self.selection = stash.selection;
            self.tabs = stash.tabs;
            self.active_tab = stash.active_tab;
        }
        self.generation += 1;
        self.frame_changes.clear();
    }
}

/// The edit-mode state kept aside during a run.
struct Stash {
    doc: Document,
    selection: Vec<Ref>,
    tabs: Vec<Tab>,
    active_tab: usize,
}
