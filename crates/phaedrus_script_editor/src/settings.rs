//! Script editor settings, shared by the host (which edits and saves them)
//! and the editor (which applies them to Monaco).

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An sRGB colour, stored as `"#RRGGBB"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub [u8; 3]);

impl Rgb {
    pub fn hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.0[0], self.0[1], self.0[2])
    }

    pub fn parse(s: &str) -> Option<Rgb> {
        let h = s.trim().strip_prefix('#')?;
        if h.len() != 6 {
            return None;
        }
        let v = u32::from_str_radix(h, 16).ok()?;
        Some(Rgb([(v >> 16) as u8, (v >> 8) as u8, v as u8]))
    }
}

impl Serialize for Rgb {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.hex())
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Rgb::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("invalid colour {s:?}")))
    }
}

macro_rules! colors {
    ($($field:ident: $label:literal = ($r:literal, $g:literal, $b:literal),)*) => {
        /// Syntax and editor colours, named as in Roblox Studio's settings.
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(default)]
        pub struct EditorColors {
            $(pub $field: Rgb,)*
        }

        impl Default for EditorColors {
            fn default() -> Self {
                Self { $($field: Rgb([$r, $g, $b]),)* }
            }
        }

        impl EditorColors {
            /// Every colour with its display name, in settings order.
            pub fn entries_mut(&mut self) -> Vec<(&'static str, &mut Rgb)> {
                vec![$(($label, &mut self.$field),)*]
            }
        }
    };
}

colors! {
    text: "Text Color" = (188, 190, 200),
    background: "Background Color" = (37, 37, 37),
    selection: "Selection Color" = (255, 255, 255),
    selection_background: "Selection Background Color" = (11, 90, 175),
    operator: "Operator Color" = (204, 204, 204),
    number: "Number Color" = (255, 198, 0),
    string: "String Color" = (173, 241, 149),
    comment: "Comment Color" = (102, 102, 102),
    keyword: "Keyword Color" = (248, 109, 124),
    error: "Error Color" = (255, 0, 0),
    warning: "Warning Color" = (255, 115, 21),
    information: "Information Color" = (79, 79, 79),
    hint: "Hint Color" = (0, 139, 219),
    find_selection_background: "Find Selection Background Color" = (141, 118, 0),
    matching_word_background: "Matching Word Background Color" = (85, 85, 85),
    builtin_function: "Built-in Function Color" = (132, 214, 247),
    whitespace: "Whitespace Color" = (85, 85, 85),
    current_line: "Current Line Highlight Color" = (45, 50, 65),
    debugger_current_line: "Debugger Current Line Color" = (42, 60, 76),
    debugger_error_line: "Debugger Error Line Color" = (76, 42, 42),
    local_method: "Local Method Color" = (253, 251, 172),
    local_property: "Local Property Color" = (97, 161, 241),
    nil: "\"nil\" Color" = (255, 198, 0),
    boolean: "Bool Color" = (255, 198, 0),
    function_keyword: "\"function\" Color" = (248, 109, 124),
    local_keyword: "\"local\" Color" = (248, 109, 124),
    self_keyword: "\"self\" Color" = (248, 109, 124),
    luau_keyword: "Luau Keyword Color" = (248, 109, 124),
    function_name: "Function Name Color" = (253, 251, 172),
    todo: "TODO Color" = (102, 102, 102),
    ruler: "Script Ruler Color" = (102, 102, 102),
    bracket: "Script Bracket Color" = (204, 204, 204),
    type_name: "Type Color" = (56, 99, 192),
    doc_code_block_background: "Documentation Code Block Background Color" = (66, 66, 66),
    menu_primary_text: "Menu Primary Text Color" = (255, 255, 255),
    menu_secondary_text: "Menu Secondary Text Color" = (102, 102, 102),
    menu_selected_text: "Menu Selected Text Color" = (255, 255, 255),
    menu_item_background: "Menu Item Background" = (46, 46, 46),
    menu_selected_background: "Selected Menu Item Background" = (38, 90, 169),
    scrollbar_background: "Script Editor Scrollbar Background" = (46, 46, 46),
    scrollbar_handle: "Script Editor Scrollbar Handle" = (179, 179, 179),
    indentation_ruler: "Indentation Ruler Color" = (102, 102, 102),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    pub font_family: String,
    pub font_size: u32,
    pub tab_size: u32,
    /// Indent with spaces instead of tabs.
    pub insert_spaces: bool,
    /// Indent after lines that open a scope and outdent `end`/`else`/`until`.
    pub auto_indent: bool,
    /// Insert the matching `end` when pressing Enter after a scope opener.
    pub auto_close_blocks: bool,
    pub auto_close_brackets: bool,
    /// Colour bracket pairs by depth instead of using the bracket colour.
    pub rainbow_brackets: bool,
    pub minimap: bool,
    pub show_whitespace: bool,
    /// Column of the vertical ruler; 0 hides it.
    pub ruler_column: u32,
    pub colors: EditorColors,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            font_family: "Consolas, 'Cascadia Code', 'Courier New', monospace".into(),
            font_size: 14,
            tab_size: 4,
            insert_spaces: false,
            auto_indent: true,
            auto_close_blocks: true,
            auto_close_brackets: true,
            rainbow_brackets: false,
            minimap: true,
            show_whitespace: false,
            ruler_column: 0,
            colors: EditorColors::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_round_trip_and_fill_defaults() {
        let s: EditorSettings = serde_json::from_str(r##"{"font_size": 16, "colors": {"keyword": "#010203"}}"##).unwrap();
        assert_eq!(s.font_size, 16);
        assert_eq!(s.colors.keyword, Rgb([1, 2, 3]));
        assert_eq!(s.colors.string, EditorColors::default().string);
        assert!(s.auto_indent);
        let back: EditorSettings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        assert_eq!(EditorColors::default().entries_mut().len(), 42);
    }
}
