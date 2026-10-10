//! Browser side: creates Monaco through the `monaco` crate, registers Luau
//! and the intellisense providers, and talks to the host over `window.ipc`.
//!
//! Host → editor: the exported [`load`], [`set_tree`], [`set_settings`] and
//! [`focus`]. Editor → host: JSON messages `ready`, `shown`, `change` and
//! `save`.

use std::cell::RefCell;

use js_sys::{Array, Function, JSON, Object, Reflect, RegExp};
use monaco::sys::editor::{self as med, EndOfLineSequence, IStandaloneCodeEditor, IStandaloneEditorConstructionOptions, IStandaloneThemeData, ITextModel};
use monaco::sys::languages::{
    self as mlang, CompletionItemProvider, HoverProvider, ILanguageExtensionPoint, LanguageConfiguration,
    SignatureHelpProvider,
};
use monaco::sys::{IPosition, KeyCode, KeyMod};
use serde::Serialize;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::analysis::{Ctx, ModuleCache};
use crate::features::{self, Completion, byte_offset, column_of};
use crate::settings::EditorSettings;
use crate::syntax;
use crate::tree::Tree;
use crate::types;

const LANGUAGE: &str = "luau";
const CHANGE_DEBOUNCE_MS: i32 = 250;

#[derive(Default)]
struct State {
    editor: Option<IStandaloneCodeEditor>,
    model: Option<ITextModel>,
    tree: Tree,
    modules: ModuleCache,
    /// Tree id of the loaded script.
    script: Option<String>,
    suppress_change: bool,
    change_timer: Option<i32>,
    settings: EditorSettings,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn post(msg: serde_json::Value) {
    let Some(window) = web_sys::window() else { return };
    let Ok(ipc) = Reflect::get(&window, &"ipc".into()) else { return };
    if let Ok(f) = Reflect::get(&ipc, &"postMessage".into()).and_then(|f| f.dyn_into::<Function>()) {
        let _ = f.call1(&ipc, &msg.to_string().into());
    }
}

/// Another handle to a JS object (some binding types do not derive Clone).
fn dup<T: JsCast>(v: &T) -> T {
    v.as_ref().clone().unchecked_into()
}

fn json(src: &str) -> JsValue {
    JSON::parse(src).expect("static JSON is valid")
}

fn to_js<T: Serialize>(v: &T) -> JsValue {
    v.serialize(&serde_wasm_bindgen::Serializer::json_compatible()).unwrap_or(JsValue::NULL)
}

fn set(obj: &Object, key: &str, value: &JsValue) {
    let _ = Reflect::set(obj, &key.into(), value);
}

#[wasm_bindgen(start)]
pub fn start() {
    register_language();
    register_providers();
    create_editor();
}

/// Replaces the editor contents with a script.
#[wasm_bindgen]
pub fn load(source: String, name: String, class: String, script_id: String, read_only: bool) {
    let (editor, model) = STATE.with_borrow_mut(|s| {
        s.script = Some(script_id.clone());
        if let (Some(t), Some(w)) = (s.change_timer.take(), web_sys::window()) {
            w.clear_timeout_with_handle(t);
        }
        s.suppress_change = true;
        (s.editor.as_ref().map(dup), s.model.clone())
    });
    let (Some(editor), Some(model)) = (editor, model) else { return };
    // Normalise line endings so byte offsets line up with Monaco positions.
    model.set_value(&source.replace("\r\n", "\n"));
    // An empty or one-line script would otherwise get the OS default (CRLF)
    // for new lines.
    model.set_eol(EndOfLineSequence::Lf);
    STATE.with_borrow_mut(|s| s.suppress_change = false);
    let opts = Object::new();
    set(&opts, "readOnly", &read_only.into());
    editor.update_options(opts.unchecked_ref());
    editor.set_scroll_top(0.0);
    editor.focus();
    if let Some(status) = web_sys::window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id("status")) {
        let suffix = if read_only { " — read-only while running" } else { "" };
        status.set_text_content(Some(&format!("{name} ({class}){suffix}")));
    }
    // Tell the host the script is in place so it can reveal the (hidden)
    // web view. This cannot wait for a paint: a hidden WebView2 runs no
    // animation frames and throttles timers. Monaco's pending render runs
    // before the first frame once the view is visible, and until then
    // WebView2 shows its dark default background.
    post(serde_json::json!({ "type": "shown", "script": script_id }));
}

/// Replaces the instance tree snapshot used for paths and `require`.
#[wasm_bindgen(js_name = setTree)]
pub fn set_tree(json: String) {
    let tree = Tree::from_json(&json);
    STATE.with_borrow_mut(|s| {
        s.tree = tree;
        s.modules.clear();
    });
    types::reset_tables();
}

#[wasm_bindgen]
pub fn focus() {
    if let Some(e) = STATE.with_borrow(|s| s.editor.as_ref().map(dup)) {
        e.focus();
    }
}

fn create_editor() {
    let document = web_sys::window().and_then(|w| w.document()).expect("document");
    let root: web_sys::HtmlElement = document.get_element_by_id("root").expect("#root").unchecked_into();
    let model = med::create_model("", Some(LANGUAGE), None).expect("create model");
    let settings = EditorSettings::default();
    let mut options = syntax::editor_options(&settings);
    options["theme"] = define_theme(&settings).into();
    for (k, v) in [
        ("automaticLayout", serde_json::json!(true)),
        ("scrollBeyondLastLine", serde_json::json!(false)),
        ("smoothScrolling", serde_json::json!(true)),
        ("wordBasedSuggestions", serde_json::json!(false)),
        ("suggest", serde_json::json!({ "showWords": false, "snippetsPreventQuickSuggestions": false })),
        ("quickSuggestions", serde_json::json!({ "other": true, "comments": false, "strings": true })),
        ("fixedOverflowWidgets", serde_json::json!(true)),
    ] {
        options[k] = v;
    }
    let options: IStandaloneEditorConstructionOptions = json(&options.to_string()).unchecked_into();
    let editor = med::create(&root, Some(&options), None);
    let code_editor: &med::ICodeEditor = editor.as_ref();
    code_editor.set_model(Some(&model));
    model.update_options(json(&syntax::model_options(&settings).to_string()).unchecked_ref());

    let on_change = Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
        let Some(window) = web_sys::window() else { return };
        let (skip, close_blocks) = STATE.with_borrow_mut(|s| {
            if let Some(t) = s.change_timer.take() {
                window.clear_timeout_with_handle(t);
            }
            (s.suppress_change, s.settings.auto_close_blocks)
        });
        if skip {
            return;
        }
        if close_blocks && let Some(line) = enter_pressed_on(&event) {
            // Monaco must finish applying this change before we edit again.
            let f = Closure::once_into_js(move || close_block(line));
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(f.unchecked_ref(), 0);
        }
        let send = Closure::once_into_js(|| {
            let src = STATE.with_borrow_mut(|s| {
                s.change_timer = None;
                s.model.as_ref().map(|m| m.get_value(None, None))
            });
            if let Some(src) = src {
                post(serde_json::json!({ "type": "change", "source": src }));
            }
        });
        let handle = window
            .set_timeout_with_callback_and_timeout_and_arguments_0(send.unchecked_ref(), CHANGE_DEBOUNCE_MS)
            .ok();
        STATE.with_borrow_mut(|s| s.change_timer = handle);
    });
    code_editor.on_did_change_model_content(on_change.as_ref().unchecked_ref());
    on_change.forget();

    let on_save = Closure::<dyn FnMut()>::new(move || {
        let src = STATE.with_borrow_mut(|s| {
            if let (Some(t), Some(w)) = (s.change_timer.take(), web_sys::window()) {
                w.clear_timeout_with_handle(t);
            }
            s.model.as_ref().map(|m| m.get_value(None, None))
        });
        if let Some(src) = src {
            post(serde_json::json!({ "type": "save", "source": src }));
        }
    });
    let keybinding = KeyMod::ctrl_cmd() as u32 | KeyCode::KeyS.to_value();
    editor.add_command(keybinding as f64, on_save.as_ref().unchecked_ref(), None);
    on_save.forget();

    STATE.with_borrow_mut(|s| {
        s.editor = Some(editor);
        s.model = Some(model);
        s.settings = settings;
    });
}

/// If a content change is the user pressing Enter (one inserted line break
/// plus indentation), the line it was pressed on.
fn enter_pressed_on(event: &JsValue) -> Option<u32> {
    let get = |v: &JsValue, k: &str| Reflect::get(v, &k.into()).unwrap_or(JsValue::UNDEFINED);
    if get(event, "isUndoing").is_truthy() || get(event, "isRedoing").is_truthy() {
        return None;
    }
    let changes: Array = get(event, "changes").dyn_into().ok()?;
    if changes.length() != 1 {
        return None;
    }
    let change = changes.get(0);
    let text = get(&change, "text").as_string()?;
    let rest = text.strip_prefix("\r\n").or_else(|| text.strip_prefix('\n'))?;
    if !rest.chars().all(|c| c == ' ' || c == '\t') || get(&change, "rangeLength").as_f64() != Some(0.0) {
        return None;
    }
    get(&get(&change, "range"), "startLineNumber").as_f64().map(|l| l as u32)
}

/// After Enter on `line`: inserts the matching `end` below the new line if
/// the parser says the block is unclosed. The new line may hold closing
/// brackets pushed down by Enter (`Connect(function()|)`); they move after
/// the `end`, giving `end)`.
fn close_block(line: u32) {
    let Some((editor, model)) = STATE.with_borrow(|s| Some((s.editor.as_ref().map(dup)?, s.model.clone()?))) else {
        return;
    };
    let new_line = line + 1;
    if new_line as f64 > model.get_line_count() {
        return;
    }
    let content = model.get_line_content(new_line as f64);
    let ws_len = content.len() - content.trim_start().len();
    let (ws, tail) = content.split_at(ws_len);
    let tail = tail.trim_end();
    if !tail.chars().all(|c| matches!(c, ')' | '}' | ']' | ',')) {
        return;
    }
    let opener = model.get_line_content(line as f64);
    let text = model.get_value(None, None);
    let line_start = byte_offset(&text, new_line, 1);
    let ws_end = line_start + ws.len();
    let stripped = format!("{}{}", &text[..ws_end], &text[line_start + content.len()..]);
    let Some(closer) = features::block_closer(&stripped, ws_end, &opener) else { return };
    let closer = if closer.ends_with(tail) { closer.to_owned() } else { format!("{closer}{tail}") };
    let indent: String = opener.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
    // Monaco outdents a new line that starts with `)`; the body still
    // belongs one level in.
    let (auto_indent, unit) = STATE.with_borrow(|s| {
        let unit = if s.settings.insert_spaces { " ".repeat(s.settings.tab_size as usize) } else { "\t".to_owned() };
        (s.settings.auto_indent, unit)
    });
    let ws = if ws.len() > indent.len() || !auto_indent { ws.to_owned() } else { format!("{indent}{unit}") };
    let ws_len = ws.len();
    // Rewrite the new line's indentation too: Monaco trims auto-inserted
    // indentation when another edit touches the document.
    let max_col = model.get_line_max_column(new_line as f64);
    let cursor_col = ws_len as f64 + 1.0;
    let edit = serde_json::json!({
        "range": { "startLineNumber": new_line, "startColumn": 1, "endLineNumber": new_line, "endColumn": max_col },
        "text": format!("{ws}\n{indent}{closer}"),
    });
    let cursor = json(
        &serde_json::json!({
            "selectionStartLineNumber": new_line, "selectionStartColumn": cursor_col,
            "positionLineNumber": new_line, "positionColumn": cursor_col,
        })
        .to_string(),
    );
    let code_editor: &med::ICodeEditor = editor.as_ref();
    code_editor.execute_edits("phaedrus", &Array::of1(&json(&edit.to_string())), Some(vec![cursor]));
    let pos = json(&serde_json::json!({ "lineNumber": new_line, "column": cursor_col }).to_string());
    code_editor.set_position(pos.unchecked_ref());
}

/// Applies editor settings from the host (JSON of [`EditorSettings`]).
#[wasm_bindgen(js_name = setSettings)]
pub fn set_settings(json_src: String) {
    let settings: EditorSettings = serde_json::from_str(&json_src).unwrap_or_default();
    med::set_theme(&define_theme(&settings));
    if let Some(body) = web_sys::window().and_then(|w| w.document()).and_then(|d| d.body()) {
        let _ = body.style().set_property("background", &settings.colors.background.hex());
    }
    let handles = STATE.with_borrow(|s| Some((s.editor.as_ref().map(dup)?, s.model.clone()?)));
    if let Some((editor, model)) = handles {
        editor.update_options(json(&syntax::editor_options(&settings).to_string()).unchecked_ref());
        model.update_options(json(&syntax::model_options(&settings).to_string()).unchecked_ref());
    }
    STATE.with_borrow_mut(|s| s.settings = settings);
}

fn register_language() {
    let ext: ILanguageExtensionPoint = json(r#"{"id":"luau","extensions":[".lua",".luau"],"aliases":["Luau"]}"#).unchecked_into();
    mlang::register(&ext);

    let config: Object = json(
        r#"{
            "comments": { "lineComment": "--", "blockComment": ["--[[", "]]"] },
            "brackets": [["{", "}"], ["[", "]"], ["(", ")"]],
            "autoClosingPairs": [
                { "open": "{", "close": "}" }, { "open": "[", "close": "]" }, { "open": "(", "close": ")" },
                { "open": "\"", "close": "\"", "notIn": ["string"] }, { "open": "'", "close": "'", "notIn": ["string"] },
                { "open": "`", "close": "`", "notIn": ["string"] }
            ],
            "surroundingPairs": [
                { "open": "{", "close": "}" }, { "open": "[", "close": "]" }, { "open": "(", "close": ")" },
                { "open": "\"", "close": "\"" }, { "open": "'", "close": "'" }
            ]
        }"#,
    )
    .unchecked_into();
    let indent = Object::new();
    set(&indent, "increaseIndentPattern", &RegExp::new(syntax::INCREASE_INDENT, ""));
    set(&indent, "decreaseIndentPattern", &RegExp::new(syntax::DECREASE_INDENT, ""));
    set(&config, "indentationRules", &indent);
    mlang::set_language_configuration(LANGUAGE, config.unchecked_ref::<LanguageConfiguration>());

    let builtins: Vec<String> = {
        let api = crate::api::api();
        let mut b: Vec<String> = api.statics.keys().cloned().collect();
        b.extend(api.globals.iter().map(|(m, _)| m.name.to_string()));
        b.extend(["script", "Enum", "type", "typeof"].map(String::from));
        b.sort();
        b.dedup();
        b
    };
    mlang::set_monarch_tokens_provider(LANGUAGE, &json(&syntax::monarch(&builtins).to_string()));
}

/// Defines a theme for the settings' colours and returns its name. Monaco
/// does not re-apply a redefined current theme, so this alternates between
/// two names.
fn define_theme(settings: &EditorSettings) -> String {
    thread_local! {
        static FLIP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    let name = if FLIP.replace(!FLIP.get()) { "phaedrus-b" } else { "phaedrus-a" }.to_owned();
    let theme: IStandaloneThemeData = json(&syntax::theme(&settings.colors).to_string()).unchecked_into();
    let _ = med::define_theme(&name, &theme);
    name
}

// --- providers -------------------------------------------------------------------

/// Runs `f` with the current text, cursor byte offset and analysis context.
fn with_request<R>(model: &JsValue, position: &JsValue, f: impl FnOnce(&str, usize, &Ctx) -> R) -> R {
    let model: &ITextModel = model.unchecked_ref();
    let pos: &IPosition = position.unchecked_ref();
    let text = model.get_value(None, None);
    let offset = byte_offset(&text, pos.line_number() as u32, pos.column() as u32);
    if types::tables_need_reset() {
        STATE.with_borrow(|s| s.modules.clear());
        types::reset_tables();
    }
    STATE.with_borrow(|s| {
        let script = s.script.as_ref().and_then(|id| s.tree.by_id.get(id).copied());
        let ctx = Ctx { tree: &s.tree, script, modules: &s.modules, depth: 0 };
        f(&text, offset, &ctx)
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsRange {
    start_line_number: u32,
    start_column: u32,
    end_line_number: u32,
    end_column: u32,
}

#[derive(Serialize)]
struct JsCommand {
    id: &'static str,
    title: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsCompletion {
    label: String,
    kind: features::Kind,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    documentation: Option<String>,
    insert_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    insert_text_rules: Option<u8>,
    sort_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    filter_text: Option<String>,
    range: JsRange,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<JsCommand>,
}

fn completion_to_js(c: Completion, text: &str, line: u32, cursor_col: u32) -> JsCompletion {
    JsCompletion {
        kind: c.kind,
        detail: c.detail,
        documentation: c.doc,
        insert_text: c.insert,
        insert_text_rules: c.snippet.then_some(4),
        sort_text: if c.sort.is_empty() { c.label.clone() } else { c.sort },
        filter_text: c.filter,
        range: JsRange {
            start_line_number: line,
            start_column: column_of(text, c.replace_from),
            end_line_number: line,
            end_column: cursor_col,
        },
        command: c.params_hint.then_some(JsCommand { id: "editor.action.triggerParameterHints", title: "" }),
        label: c.label,
    }
}

fn register_providers() {
    // Completions.
    let provide = Closure::<dyn Fn(JsValue, JsValue) -> JsValue>::new(|model: JsValue, position: JsValue| {
        let pos: &IPosition = position.unchecked_ref();
        let line = pos.line_number() as u32;
        let col = pos.column() as u32;
        let suggestions: Vec<JsCompletion> = with_request(&model, &position, |text, offset, ctx| {
            features::complete(text, offset, ctx).into_iter().map(|c| completion_to_js(c, text, line, col)).collect()
        });
        let out = Object::new();
        set(&out, "suggestions", &to_js(&suggestions));
        out.into()
    });
    let provider = Object::new();
    set(&provider, "triggerCharacters", &Array::of4(&".".into(), &":".into(), &"\"".into(), &"'".into()));
    set(&provider, "provideCompletionItems", provide.as_ref());
    provide.forget();
    mlang::register_completion_item_provider(LANGUAGE, provider.unchecked_ref::<CompletionItemProvider>());

    // Hovers.
    let provide = Closure::<dyn Fn(JsValue, JsValue) -> JsValue>::new(|model: JsValue, position: JsValue| {
        let md = with_request(&model, &position, features::hover);
        match md {
            Some(md) => to_js(&serde_json::json!({ "contents": [{ "value": md }] })),
            None => JsValue::NULL,
        }
    });
    let provider = Object::new();
    set(&provider, "provideHover", provide.as_ref());
    provide.forget();
    mlang::register_hover_provider(LANGUAGE, provider.unchecked_ref::<HoverProvider>());

    // Signature help.
    let provide = Closure::<dyn Fn(JsValue, JsValue) -> JsValue>::new(|model: JsValue, position: JsValue| {
        let sig = with_request(&model, &position, features::signature_help);
        let Some(sig) = sig else { return JsValue::NULL };
        let utf16 = |b: usize| sig.label[..b].encode_utf16().count();
        let params: Vec<serde_json::Value> =
            sig.params.iter().map(|&(s, e)| serde_json::json!({ "label": [utf16(s), utf16(e)] })).collect();
        let mut signature = serde_json::json!({ "label": sig.label, "parameters": params });
        if let Some(doc) = &sig.doc {
            signature["documentation"] = doc.clone().into();
        }
        let value = serde_json::json!({ "signatures": [signature], "activeSignature": 0, "activeParameter": sig.active });
        let out = Object::new();
        set(&out, "value", &to_js(&value));
        set(&out, "dispose", &Function::new_no_args(""));
        out.into()
    });
    let provider = Object::new();
    set(&provider, "signatureHelpTriggerCharacters", &Array::of2(&"(".into(), &",".into()));
    set(&provider, "signatureHelpRetriggerCharacters", &Array::of1(&",".into()));
    set(&provider, "provideSignatureHelp", provide.as_ref());
    provide.forget();
    mlang::register_signature_help_provider(LANGUAGE, provider.unchecked_ref::<SignatureHelpProvider>());
}
