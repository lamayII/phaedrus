//! Luau highlighting and the editor's look: the Monarch grammar, the theme
//! built from [`EditorColors`], and Monaco options from [`EditorSettings`].
//! Plain JSON so it can be checked natively; `web.rs` hands it to Monaco.

use serde_json::{Value, json};

use crate::settings::{EditorColors, EditorSettings, Rgb};

/// Lines after which the next line is indented: a `then`, `do`, `else`,
/// `repeat` or function header not closed on the same line, or an unclosed
/// `{` / `(`. Comments are ignored.
pub const INCREASE_INDENT: &str =
    r"^((?!--).)*((\b(then|do|else|repeat)\b((?!\b(end|until)\b).)*)|(\bfunction\b[^)]*\)?((?!\bend\b).)*)|(\{[^}]*)|(\([^)]*))$";
/// Lines that are moved out a level as they are typed.
pub const DECREASE_INDENT: &str = r"^\s*((\b(elseif|else|end|until)\b)|(\})|(\)))";

const KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "for", "if", "in", "not", "or", "repeat", "return", "then",
    "until", "while",
];
const LUAU_KEYWORDS: &[&str] = &["continue", "export"];
const TODO: &str = "TODO|FIXME|HACK|XXX|NOTE";

/// The Monarch tokenizer. Token names map to colours in [`theme`].
pub fn monarch(builtins: &[String]) -> Value {
    let ident = "[a-zA-Z_]\\w*";
    let call_follows = "(?=\\s*[(\"'{`])";
    json!({
        // Monarch otherwise starts in the first state, and json! sorts keys.
        "start": "root",
        "keywords": KEYWORDS,
        "luauKeywords": LUAU_KEYWORDS,
        "nils": ["nil"],
        "booleans": ["true", "false"],
        "selfs": ["self"],
        "functions": ["function"],
        "locals": ["local"],
        "builtins": builtins,
        "tokenizer": {
            "root": [
                ["--\\[(=*)\\[", { "token": "comment", "next": "@longcomment.$1" }],
                ["--$", "comment"],
                ["--", { "token": "comment", "next": "@linecomment" }],
                ["\\[(=*)\\[", { "token": "string", "next": "@longstring.$1" }],
                // `function name`, `function a.b:c`
                [format!("(function)(\\s+)({ident}(?:[.:]{ident})*)"), ["keyword.function", "white", "function.name"]],
                // `type Name =`, `export type Name<T> =`
                [format!("(type)(\\s+)({ident})(?=\\s*[=<])"), ["luaukeyword", "white", "type"]],
                ["\\.\\.\\.|\\.\\.=?", "operator"],
                // Annotations and casts: `-> T`, `:: T`.
                [format!("(::|->)(\\s*)({ident})"), ["operator", "white", "type"]],
                // `obj:Method(` before `name: Type`.
                [format!("(:)({ident}){call_follows}"), ["operator", "method"]],
                [format!("(:)(\\s*)({ident})"), ["operator", "white", "type"]],
                [format!("(\\.)({ident}){call_follows}"), ["operator", "method"]],
                [format!("(\\.)({ident})"), ["operator", "property"]],
                [ident, { "cases": {
                    "@nils": "nil",
                    "@booleans": "boolean",
                    "@selfs": "self",
                    "@functions": "keyword.function",
                    "@locals": "keyword.local",
                    "@luauKeywords": "luaukeyword",
                    "@keywords": "keyword",
                    "@builtins": "builtin",
                    "@default": "identifier"
                } }],
                ["\\d*\\.\\d+([eE][\\-+]?\\d+)?", "number"],
                ["0[xX][0-9a-fA-F_]+", "number"],
                ["0[bB][01_]+", "number"],
                ["\\d[\\d_]*(\\.\\d*)?([eE][\\-+]?\\d+)?", "number"],
                ["\"([^\"\\\\]|\\\\.)*$", "string"],
                ["'([^'\\\\]|\\\\.)*$", "string"],
                ["\"", "string", "@dstring"],
                ["'", "string", "@sstring"],
                ["`", "string", "@istring"],
                ["[{}()\\[\\]]", "bracket"],
                ["(\\+|-|\\*|\\/\\/?|%|\\^)=?", "operator"],
                ["==|~=|<=|>=|<|>|=|#|\\?|[;,.:]", "operator"]
            ],
            "linecomment": [
                [format!("({TODO})$"), { "token": "comment.todo", "next": "@pop" }],
                [format!("\\b({TODO})\\b"), "comment.todo"],
                ["[^TFHXN]+$", { "token": "comment", "next": "@pop" }],
                ["[^TFHXN]+", "comment"],
                [".$", { "token": "comment", "next": "@pop" }],
                [".", "comment"]
            ],
            "longcomment": [
                ["\\](=*)\\]", { "cases": { "$1==$S2": { "token": "comment", "next": "@pop" }, "@default": "comment" } }],
                [format!("\\b({TODO})\\b"), "comment.todo"],
                [".", "comment"]
            ],
            "longstring": [["\\](=*)\\]", { "cases": { "$1==$S2": { "token": "string", "next": "@pop" }, "@default": "string" } }], [".", "string"]],
            "dstring": [["[^\\\\\"]+", "string"], ["\\\\.", "string"], ["\"", "string", "@pop"]],
            "sstring": [["[^\\\\']+", "string"], ["\\\\.", "string"], ["'", "string", "@pop"]],
            "istring": [["\\{", { "token": "bracket", "next": "@interp" }], ["[^\\\\`{]+", "string"], ["\\\\.", "string"], ["`", "string", "@pop"]],
            "interp": [["\\}", { "token": "bracket", "next": "@pop" }], { "include": "root" }]
        }
    })
}

/// Token name → colour, for every token the grammar produces.
fn token_colors(c: &EditorColors) -> Vec<(&'static str, Rgb)> {
    vec![
        ("", c.text),
        ("identifier", c.text),
        ("white", c.text),
        ("operator", c.operator),
        ("number", c.number),
        ("string", c.string),
        ("comment", c.comment),
        ("comment.todo", c.todo),
        ("keyword", c.keyword),
        ("luaukeyword", c.luau_keyword),
        ("keyword.function", c.function_keyword),
        ("keyword.local", c.local_keyword),
        ("nil", c.nil),
        ("boolean", c.boolean),
        ("self", c.self_keyword),
        ("builtin", c.builtin_function),
        ("method", c.local_method),
        ("property", c.local_property),
        ("function.name", c.function_name),
        ("type", c.type_name),
        ("bracket", c.bracket),
    ]
}

fn hex(c: Rgb) -> String {
    c.hex()
}

/// `#RRGGBBAA` with the given alpha.
fn alpha(c: Rgb, a: u8) -> String {
    format!("{}{a:02X}", c.hex())
}

/// The Monaco theme for a set of colours.
pub fn theme(c: &EditorColors) -> Value {
    let rules: Vec<Value> = token_colors(c)
        .into_iter()
        .map(|(token, color)| json!({ "token": token, "foreground": color.hex().trim_start_matches('#') }))
        .collect();
    let colors: serde_json::Map<String, Value> = [
        ("editor.background", hex(c.background)),
        ("editor.foreground", hex(c.text)),
        ("editorCursor.foreground", hex(c.text)),
        ("editorGutter.background", hex(c.background)),
        ("minimap.background", hex(c.background)),
        ("editorLineNumber.foreground", alpha(c.text, 0x70)),
        ("editorLineNumber.activeForeground", hex(c.text)),
        ("editor.selectionBackground", hex(c.selection_background)),
        ("editor.selectionForeground", hex(c.selection)),
        ("editor.inactiveSelectionBackground", alpha(c.selection_background, 0x80)),
        ("editor.selectionHighlightBackground", alpha(c.matching_word_background, 0x99)),
        ("editor.wordHighlightBackground", hex(c.matching_word_background)),
        ("editor.wordHighlightStrongBackground", hex(c.matching_word_background)),
        ("editor.findMatchBackground", hex(c.find_selection_background)),
        ("editor.findMatchHighlightBackground", alpha(c.find_selection_background, 0x80)),
        ("editor.lineHighlightBackground", hex(c.current_line)),
        ("editor.lineHighlightBorder", hex(c.current_line)),
        ("editorError.foreground", hex(c.error)),
        ("editorWarning.foreground", hex(c.warning)),
        ("editorInfo.foreground", hex(c.information)),
        ("editorHint.foreground", hex(c.hint)),
        ("editorWhitespace.foreground", hex(c.whitespace)),
        ("editorRuler.foreground", hex(c.ruler)),
        ("editorIndentGuide.background", hex(c.indentation_ruler)),
        ("editorIndentGuide.activeBackground", hex(c.indentation_ruler)),
        ("editorBracketMatch.border", hex(c.bracket)),
        ("editorBracketMatch.background", alpha(c.bracket, 0x30)),
        ("textCodeBlock.background", hex(c.doc_code_block_background)),
        ("descriptionForeground", hex(c.menu_secondary_text)),
        ("editorWidget.background", hex(c.menu_item_background)),
        ("editorWidget.foreground", hex(c.menu_primary_text)),
        ("editorWidget.border", hex(c.menu_item_background)),
        ("editorSuggestWidget.background", hex(c.menu_item_background)),
        ("editorSuggestWidget.border", hex(c.menu_item_background)),
        ("editorSuggestWidget.foreground", hex(c.menu_primary_text)),
        ("editorSuggestWidget.selectedBackground", hex(c.menu_selected_background)),
        ("editorSuggestWidget.selectedForeground", hex(c.menu_selected_text)),
        ("editorHoverWidget.background", hex(c.menu_item_background)),
        ("editorHoverWidget.foreground", hex(c.menu_primary_text)),
        ("editorHoverWidget.border", hex(c.menu_item_background)),
        ("list.hoverBackground", alpha(c.menu_selected_background, 0x66)),
        ("list.activeSelectionBackground", hex(c.menu_selected_background)),
        ("list.activeSelectionForeground", hex(c.menu_selected_text)),
        ("list.focusBackground", hex(c.menu_selected_background)),
        ("list.focusForeground", hex(c.menu_selected_text)),
        ("menu.background", hex(c.menu_item_background)),
        ("menu.foreground", hex(c.menu_primary_text)),
        ("menu.selectionBackground", hex(c.menu_selected_background)),
        ("menu.selectionForeground", hex(c.menu_selected_text)),
        ("menu.separatorBackground", hex(c.menu_secondary_text)),
        ("editorOverviewRuler.background", hex(c.scrollbar_background)),
        ("editorOverviewRuler.border", hex(c.scrollbar_background)),
        ("scrollbar.shadow", "#00000000".to_owned()),
        ("scrollbarSlider.background", alpha(c.scrollbar_handle, 0x55)),
        ("scrollbarSlider.hoverBackground", alpha(c.scrollbar_handle, 0x88)),
        ("scrollbarSlider.activeBackground", alpha(c.scrollbar_handle, 0xBB)),
    ]
    .into_iter()
    .map(|(k, v): (&str, String)| (k.to_owned(), Value::String(v)))
    .collect();
    json!({ "base": "vs-dark", "inherit": true, "rules": rules, "colors": colors })
}

/// Editor options for a set of settings (font, indentation, features).
pub fn editor_options(s: &EditorSettings) -> Value {
    let closing = if s.auto_close_brackets { "languageDefined" } else { "never" };
    json!({
        "fontFamily": s.font_family,
        "fontSize": s.font_size.clamp(6, 48),
        "minimap": { "enabled": s.minimap },
        "renderWhitespace": if s.show_whitespace { "all" } else { "none" },
        "autoIndent": if s.auto_indent { "full" } else { "keep" },
        "autoClosingBrackets": closing,
        "autoClosingQuotes": closing,
        "bracketPairColorization": { "enabled": s.rainbow_brackets },
        "rulers": if s.ruler_column > 0 { vec![s.ruler_column] } else { Vec::new() },
    })
}

/// Text model options (indentation).
pub fn model_options(s: &EditorSettings) -> Value {
    json!({ "tabSize": s.tab_size.clamp(1, 16), "indentSize": s.tab_size.clamp(1, 16), "insertSpaces": s.insert_spaces })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every token the grammar can emit has a theme rule.
    #[test]
    fn grammar_tokens_are_themed() {
        let grammar = monarch(&[]).to_string();
        let themed: Vec<&str> = token_colors(&EditorColors::default()).into_iter().map(|(t, _)| t).collect();
        for t in [
            "comment", "comment.todo", "string", "keyword.function", "function.name", "luaukeyword", "type",
            "operator", "method", "property", "nil", "boolean", "self", "keyword.local", "keyword", "builtin",
            "identifier", "number", "bracket", "white",
        ] {
            assert!(grammar.contains(&format!("\"{t}\"")), "grammar does not emit {t}");
            assert!(themed.contains(&t), "no colour for {t}");
        }
    }

    /// Monarch appends `.luau` to every token, so a theme rule named
    /// `x.luau` would also match token `x` and override its colour.
    #[test]
    fn token_names_do_not_collide_with_the_language_suffix() {
        for (t, _) in token_colors(&EditorColors::default()) {
            assert!(!t.ends_with(".luau"), "{t}");
        }
    }

    #[test]
    fn theme_uses_settings() {
        let mut c = EditorColors::default();
        c.keyword = Rgb([1, 2, 3]);
        c.background = Rgb([4, 5, 6]);
        let t = theme(&c);
        assert!(t["rules"].as_array().unwrap().iter().any(|r| r["token"] == "keyword" && r["foreground"] == "010203"));
        assert_eq!(t["colors"]["editor.background"], "#040506");
        let o = editor_options(&EditorSettings { ruler_column: 80, auto_indent: false, ..Default::default() });
        assert_eq!(o["rulers"], json!([80]));
        assert_eq!(o["autoIndent"], "keep");
    }
}
