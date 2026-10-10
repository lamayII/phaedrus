//! Script editor: Monaco hosted in a native web view (WebView2 on Windows)
//! placed over the viewport area when a script tab is active.
//!
//! The editor itself is the `phaedrus_script_editor` crate: Rust compiled to
//! WebAssembly that drives Monaco through the `monaco` crate and provides
//! Luau/Roblox intellisense. `build.rs` builds it and embeds the files, which
//! are served to the web view from memory. If it could not be built, or no
//! web view can be created, the UI falls back to a built-in egui editor.
//!
//! The web view is created hidden and only shown once the editor has
//! painted the first script, so opening a script never flashes an empty or
//! unstyled page. It is dropped when no script tab is open, so it costs no
//! memory otherwise.

mod snapshot;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use bevy::prelude::*;
use bevy::window::{PrimaryWindow, RawHandleWrapper};
use bevy_egui::egui;
use phaedrus_dom::Change;
use phaedrus_dom::types::{Ref, Variant};
use phaedrus_luau::OutputLevel;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::http::{Response, header::CONTENT_TYPE};

use crate::settings::ScriptEditorSettings;
use crate::studio::{Studio, Tab};

mod bundle {
    include!(concat!(env!("OUT_DIR"), "/script_editor_bundle.rs"));
}

const INDEX_HTML: &str = include_str!("index.html");
/// Minimum time between instance tree snapshots sent to the editor.
const TREE_SYNC_INTERVAL: Duration = Duration::from_millis(500);

enum Inbound {
    Ready,
    /// The editor painted the given script.
    Shown(String),
    Change(String),
    Save(String),
}

/// Main-thread resource owning the web view.
pub struct ScriptEditorHost {
    webview: Option<wry::WebView>,
    inbox: Rc<RefCell<Vec<Inbound>>>,
    ready: bool,
    /// The web view has painted a script and may be shown.
    revealed: bool,
    failed: bool,
    reported_failure: bool,
    shown_script: Option<Ref>,
    target_rect: Option<egui::Rect>,
    target_script: Option<Ref>,
    applied_rect: Option<egui::Rect>,
    visible: bool,
    /// Document generation the last tree snapshot was taken from.
    tree_generation: u64,
    tree_dirty: bool,
    tree_sent_at: Option<Instant>,
    /// Settings last sent to the editor.
    sent_settings: Option<ScriptEditorSettings>,
}

impl Default for ScriptEditorHost {
    fn default() -> Self {
        Self {
            webview: None,
            inbox: Rc::new(RefCell::new(Vec::new())),
            ready: false,
            revealed: false,
            failed: std::env::var_os("PHAEDRUS_NO_WEBVIEW").is_some() || bundle::BUILD_ERROR.is_some(),
            reported_failure: false,
            shown_script: None,
            target_rect: None,
            target_script: None,
            applied_rect: None,
            visible: false,
            tree_generation: 0,
            tree_dirty: true,
            tree_sent_at: None,
            sent_settings: None,
        }
    }
}

impl ScriptEditorHost {
    /// Whether the native Monaco view is (or can be) used.
    pub fn is_native_available(&self) -> bool {
        !self.failed
    }

    /// Whether the web view is currently drawn over the editor area. Until
    /// then the UI paints a placeholder there.
    pub fn is_showing(&self) -> bool {
        self.visible
    }

    /// Called by the UI each frame with the editor area and active script.
    pub fn set_target(&mut self, rect: Option<egui::Rect>, script: Option<Ref>) {
        self.target_rect = rect;
        self.target_script = script;
    }

    fn create(&mut self, handle: &RawHandleWrapper, rect: egui::Rect, background: [u8; 3]) -> Result<(), String> {
        let inbox = self.inbox.clone();
        // SAFETY: this runs on the main thread (non-send system), which owns
        // the window.
        let window = unsafe { handle.get_handle() };
        let webview = wry::WebViewBuilder::new()
            .with_bounds(to_wry_rect(rect))
            .with_visible(false)
            .with_background_color((background[0], background[1], background[2], 255))
            .with_devtools(cfg!(debug_assertions))
            .with_custom_protocol("phaedrus".into(), move |_id, request| {
                let path = request.uri().path().trim_start_matches('/');
                let body: Option<&'static [u8]> = if path.is_empty() || path == "index.html" {
                    Some(INDEX_HTML.as_bytes())
                } else {
                    bundle::FILES.iter().find(|(name, _)| *name == path).map(|(_, bytes)| *bytes)
                };
                match body {
                    Some(bytes) => Response::builder()
                        .header(CONTENT_TYPE, mime_for(path))
                        .body(bytes.into())
                        .unwrap(),
                    None => Response::builder().status(404).body(Vec::new().into()).unwrap(),
                }
            })
            .with_ipc_handler(move |req| {
                let body = req.body();
                let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else { return };
                let text = |k: &str| v[k].as_str().unwrap_or_default().to_owned();
                let msg = match v.get("type").and_then(|t| t.as_str()) {
                    Some("ready") => Inbound::Ready,
                    Some("shown") => Inbound::Shown(text("script")),
                    Some("change") => Inbound::Change(text("source")),
                    Some("save") => Inbound::Save(text("source")),
                    _ => return,
                };
                inbox.borrow_mut().push(msg);
            })
            .with_url("phaedrus://localhost/index.html")
            .build_as_child(&window)
            .map_err(|e| e.to_string())?;
        self.webview = Some(webview);
        self.ready = false;
        self.revealed = false;
        self.visible = false;
        self.shown_script = None;
        self.applied_rect = Some(rect);
        self.tree_dirty = true;
        Ok(())
    }

    fn drop_webview(&mut self) {
        self.webview = None;
        self.ready = false;
        self.revealed = false;
        self.visible = false;
        self.shown_script = None;
    }

    fn set_visible(&mut self, visible: bool) {
        if self.visible == visible {
            return;
        }
        if let Some(w) = &self.webview {
            let _ = w.set_visible(visible);
            if !visible {
                let _ = w.focus_parent();
            }
        }
        self.visible = visible;
    }
}

fn mime_for(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("js") => "text/javascript",
        Some("wasm") => "application/wasm",
        Some("css") => "text/css",
        Some("json") => "application/json",
        _ => "text/html",
    }
}

fn to_wry_rect(r: egui::Rect) -> wry::Rect {
    wry::Rect {
        position: LogicalPosition::new(r.min.x as f64, r.min.y as f64).into(),
        size: LogicalSize::new(r.width().max(1.0) as f64, r.height().max(1.0) as f64).into(),
    }
}

fn js_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// Whether this frame's changes affect what intellisense knows about the
/// tree: its shape, names, or ModuleScript sources.
fn tree_changed(studio: &Studio) -> bool {
    let doc = studio.doc();
    studio.frame_changes.iter().any(|c| match c {
        Change::Added(_) | Change::Reparented { .. } => true,
        Change::Property(r, name) => {
            name == "Name" || (name == "Source" && doc.get(*r).is_some_and(|i| i.class == "ModuleScript"))
        }
    })
}

/// Creates/positions the web view and exchanges messages with it.
pub fn update_editor_host(
    mut host: NonSendMut<ScriptEditorHost>,
    mut studio: ResMut<Studio>,
    settings: Res<ScriptEditorSettings>,
    window: Query<&RawHandleWrapper, With<PrimaryWindow>>,
) {
    let host = &mut *host;
    let any_script_tab = studio.tabs.iter().any(|t| matches!(t, Tab::Script(_)));
    if !any_script_tab && host.webview.is_some() {
        host.drop_webview();
        return;
    }
    if host.failed {
        if any_script_tab && !host.reported_failure {
            host.reported_failure = true;
            if let Some(e) = bundle::BUILD_ERROR {
                studio.log(OutputLevel::Warn, format!("The Monaco script editor was not built ({e}); using the built-in editor."));
            }
        }
        return;
    }
    if host.tree_generation != studio.generation || tree_changed(&studio) {
        host.tree_generation = studio.generation;
        host.tree_dirty = true;
    }
    let (Some(rect), Some(script)) = (host.target_rect, host.target_script) else {
        host.set_visible(false);
        return;
    };
    if host.webview.is_none() {
        let Ok(handle) = window.single() else { return };
        if let Err(e) = host.create(handle, rect, settings.colors.background.0) {
            host.failed = true;
            host.reported_failure = true;
            studio.log(OutputLevel::Warn, format!("Monaco editor unavailable ({e}); using the built-in editor."));
            return;
        }
    }
    host.set_visible(host.revealed);
    if host.applied_rect != Some(rect) {
        if let Some(w) = &host.webview {
            let _ = w.set_bounds(to_wry_rect(rect));
        }
        host.applied_rect = Some(rect);
    }

    // Messages from the editor.
    let messages: Vec<Inbound> = host.inbox.borrow_mut().drain(..).collect();
    for m in messages {
        match m {
            Inbound::Ready => {
                host.ready = true;
                host.sent_settings = None;
                host.shown_script = None;
                host.tree_dirty = true;
                host.tree_sent_at = None;
            }
            Inbound::Shown(id) => {
                if host.shown_script.is_some_and(|r| r.to_string() == id) && !host.revealed {
                    host.revealed = true;
                    host.set_visible(true);
                    if let Some(w) = &host.webview {
                        let _ = w.focus();
                    }
                }
            }
            Inbound::Change(src) => {
                if let Some(r) = host.shown_script {
                    let _ = studio.doc().set_property(r, "Source", Variant::String(src));
                }
            }
            Inbound::Save(src) => {
                if let Some(r) = host.shown_script {
                    let _ = studio.doc().set_property(r, "Source", Variant::String(src));
                    studio.waypoint("Edit Script");
                    crate::ui::save_place(&mut studio);
                }
            }
        }
    }
    if !host.ready {
        return;
    }
    let Some(webview) = &host.webview else { return };
    // Settings go first so a newly loaded script is never shown unstyled.
    if host.sent_settings.as_ref() != Some(&*settings) {
        let json = serde_json::to_string(&settings.0).unwrap_or_default();
        let _ = webview.evaluate_script(&format!("window.phaedrus && window.phaedrus.setSettings({});", js_string(&json)));
        host.sent_settings = Some(settings.clone());
    }
    if host.tree_dirty && host.tree_sent_at.is_none_or(|t| t.elapsed() >= TREE_SYNC_INTERVAL) {
        let json = snapshot::tree_json(&studio.doc());
        let _ = webview.evaluate_script(&format!("window.phaedrus && window.phaedrus.setTree({});", js_string(&json)));
        host.tree_dirty = false;
        host.tree_sent_at = Some(Instant::now());
    }
    if host.shown_script != Some(script) {
        // Commit the previous script as an undo step.
        if host.shown_script.is_some() {
            studio.waypoint("Edit Script");
        }
        let (name, source, class) = {
            let doc = studio.doc();
            let source = match doc.get_property(script, "Source") {
                Some(Variant::String(s)) => s,
                _ => String::new(),
            };
            let inst = doc.get(script);
            (
                inst.map(|i| i.name.clone()).unwrap_or_default(),
                source,
                inst.map(|i| i.class.to_string()).unwrap_or_default(),
            )
        };
        let _ = webview.evaluate_script(&format!(
            "window.phaedrus && window.phaedrus.load({}, {}, {}, {}, {});",
            js_string(&source),
            js_string(&name),
            js_string(&class),
            js_string(&script.to_string()),
            studio.is_running()
        ));
        if host.revealed {
            let _ = webview.focus();
        }
        host.shown_script = Some(script);
    }
}
