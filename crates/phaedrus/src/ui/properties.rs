//! Properties panel.

use bevy_egui::egui::{self, Color32, RichText};
use phaedrus_dom::geometry::{all_brick_colors, color3uint8_to_f32};
use phaedrus_dom::reflection::{self, VisibleProperty};
use phaedrus_dom::types::{
    Color3, Color3uint8, Enum, Ref, UDim, UDim2, Variant, Vector2, Vector3,
};

use super::{UiState, theme};
use crate::studio::Studio;

const LABEL_FRAC: f32 = 0.45;

pub fn properties_panel(ui: &mut egui::Ui, studio: &mut Studio, state: &mut UiState) {
    egui::Frame::new().inner_margin(egui::Margin::symmetric(4, 3)).show(ui, |ui| {
        let resp = ui.add(
            egui::TextEdit::singleline(&mut state.properties_filter)
                .hint_text("Filter Properties (Ctrl+Shift+P)")
                .desired_width(f32::INFINITY),
        );
        if state.focus_properties_filter {
            resp.request_focus();
            state.focus_properties_filter = false;
        }
    });

    let selection: Vec<Ref> = studio.selection.clone();
    let Some(&first) = selection.first() else {
        return;
    };
    let class = match studio.doc().get(first) {
        Some(i) => i.class.to_string(),
        None => return,
    };
    // Only show properties shared by every selected instance's class.
    let same_class = {
        let doc = studio.doc();
        selection.iter().all(|r| doc.get(*r).is_some_and(|i| i.class == class))
    };
    let mut props: Vec<VirtualProp> = Vec::new();
    props.push(VirtualProp::Named("Name", "Data"));
    props.push(VirtualProp::Named("ClassName", "Data"));
    props.push(VirtualProp::Named("Parent", "Data"));
    if reflection::is_a(&class, "BasePart") {
        props.push(VirtualProp::Named("BrickColor", "Appearance"));
        props.push(VirtualProp::Named("Position", "Transform"));
        props.push(VirtualProp::Named("Orientation", "Transform"));
    }
    if class == "Model" {
        props.push(VirtualProp::Named("Position", "Transform"));
        props.push(VirtualProp::Named("Orientation", "Transform"));
    }
    for p in reflection::visible_properties(&class) {
        if matches!(p.name, "Name" | "Parent" | "ClassName" | "Position" | "Orientation" | "BrickColor" | "Rotation") {
            continue;
        }
        if !same_class && reflection::find_property("Instance", p.name).is_none() {
            continue;
        }
        props.push(VirtualProp::Real(p));
    }
    props.sort_by_key(|p| reflection::category_rank(p.category()));
    let filter = state.properties_filter.to_lowercase();
    props.retain(|p| filter.is_empty() || p.name().to_lowercase().contains(&filter));

    let header = if selection.len() > 1 {
        format!("{} ({} selected)", class, selection.len())
    } else {
        format!("{} \"{}\"", class, studio.doc().get(first).map(|i| i.name.clone()).unwrap_or_default())
    };
    ui.add_space(2.0);
    ui.label(RichText::new(header).small().color(theme::TEXT_DIM));

    let mut commit: Option<(String, Variant)> = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let mut current_cat = "";
        let mut open = true;
        for p in &props {
            let cat = p.category();
            if cat != current_cat {
                current_cat = cat;
                let id = ui.id().with(("cat", cat));
                let mut cat_open = ui.data_mut(|d| *d.get_persisted_mut_or(id, true));
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), egui::Sense::click());
                ui.painter().rect_filled(rect, 0.0, theme::BG_TOP);
                let c = rect.left_center() + egui::vec2(9.0, 0.0);
                let tri = if cat_open {
                    vec![c + egui::vec2(-3.5, -1.5), c + egui::vec2(3.5, -1.5), c + egui::vec2(0.0, 2.5)]
                } else {
                    vec![c + egui::vec2(-1.5, -3.5), c + egui::vec2(2.5, 0.0), c + egui::vec2(-1.5, 3.5)]
                };
                ui.painter().add(egui::Shape::convex_polygon(tri, theme::TEXT_DIM, egui::Stroke::NONE));
                ui.painter().text(
                    rect.left_center() + egui::vec2(18.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    cat,
                    egui::FontId::proportional(12.0),
                    theme::TEXT,
                );
                if resp.clicked() {
                    cat_open = !cat_open;
                    ui.data_mut(|d| d.insert_persisted(id, cat_open));
                }
                open = cat_open;
            }
            if !open {
                continue;
            }
            let value = studio.doc().get_property(first, p.name());
            let Some(value) = value else { continue };
            let read_only = p.read_only();
            let enum_name = reflection::property_enum(&class, p.name());
            ui.horizontal(|ui| {
                let w = ui.available_width();
                let label_w = (w * LABEL_FRAC).max(80.0);
                let (lrect, _) = ui.allocate_exact_size(egui::vec2(label_w, 20.0), egui::Sense::hover());
                ui.painter().text(
                    lrect.left_center() + egui::vec2(14.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    p.name(),
                    egui::FontId::proportional(12.0),
                    if read_only { theme::TEXT_DIM } else { theme::TEXT },
                );
                ui.painter().line_segment(
                    [lrect.right_top(), lrect.right_bottom()],
                    egui::Stroke::new(1.0, theme::BORDER),
                );
                ui.add_enabled_ui(!read_only, |ui| {
                    ui.set_width(ui.available_width());
                    if let Some(v) = value_editor(ui, studio, state, p.name(), &value, enum_name) {
                        commit = Some((p.name().to_owned(), v));
                    }
                });
            });
        }
    });

    if let Some((name, value)) = commit {
        if name == "Parent" {
            return;
        }
        studio.set_on_selection(&name, value);
    }
}

enum VirtualProp {
    Named(&'static str, &'static str),
    Real(&'static VisibleProperty),
}

impl VirtualProp {
    fn name(&self) -> &'static str {
        match self {
            VirtualProp::Named(n, _) => n,
            VirtualProp::Real(p) => p.name,
        }
    }
    fn category(&self) -> &'static str {
        match self {
            VirtualProp::Named(_, c) => c,
            VirtualProp::Real(p) => p.category,
        }
    }
    fn read_only(&self) -> bool {
        match self {
            VirtualProp::Named(n, _) => *n == "ClassName",
            VirtualProp::Real(p) => p.read_only,
        }
    }
}

fn fmt_f(v: f32) -> String {
    let r = (v * 1000.0).round() / 1000.0;
    if r == r.trunc() { format!("{}", r as i64) } else { format!("{r}") }
}

fn parse_floats(s: &str) -> Vec<f32> {
    s.split(',').filter_map(|p| p.trim().parse::<f32>().ok()).collect()
}

/// Text field that only commits on Enter / focus loss.
fn text_commit(ui: &mut egui::Ui, state: &mut UiState, key: &str, display: String) -> Option<String> {
    let editing = state.prop_edit.as_ref().is_some_and(|(k, _)| k == key);
    let mut buf = if editing { state.prop_edit.as_ref().unwrap().1.clone() } else { display };
    let resp = ui.add(egui::TextEdit::singleline(&mut buf).desired_width(f32::INFINITY));
    if resp.gained_focus() || resp.has_focus() {
        state.prop_edit = Some((key.to_owned(), buf.clone()));
    }
    if resp.lost_focus() {
        let cancelled = ui.input(|i| i.key_pressed(egui::Key::Escape));
        state.prop_edit = None;
        if !cancelled {
            return Some(buf);
        }
    }
    None
}

fn value_editor(
    ui: &mut egui::Ui,
    studio: &Studio,
    state: &mut UiState,
    name: &str,
    value: &Variant,
    enum_name: Option<&'static str>,
) -> Option<Variant> {
    match value {
        Variant::Bool(b) => {
            let mut v = *b;
            ui.checkbox(&mut v, "").changed().then_some(Variant::Bool(v))
        }
        Variant::String(s) => {
            if name == "Source" {
                ui.label(RichText::new(format!("{} lines", s.lines().count())).color(theme::TEXT_DIM));
                return None;
            }
            if name == "ClassName" {
                ui.label(s);
                return None;
            }
            text_commit(ui, state, name, s.clone()).map(Variant::String)
        }
        Variant::Float32(f) => num_edit(ui, state, name, *f as f64).map(|v| Variant::Float32(v as f32)),
        Variant::Float64(f) => num_edit(ui, state, name, *f).map(Variant::Float64),
        Variant::Int32(i) => num_edit(ui, state, name, *i as f64).map(|v| Variant::Int32(v as i32)),
        Variant::Int64(i) => num_edit(ui, state, name, *i as f64).map(|v| Variant::Int64(v as i64)),
        Variant::Vector3(v) => {
            let s = format!("{}, {}, {}", fmt_f(v.x), fmt_f(v.y), fmt_f(v.z));
            text_commit(ui, state, name, s).and_then(|t| {
                let f = parse_floats(&t);
                (f.len() == 3).then(|| Variant::Vector3(Vector3::new(f[0], f[1], f[2])))
            })
        }
        Variant::Vector2(v) => {
            let s = format!("{}, {}", fmt_f(v.x), fmt_f(v.y));
            text_commit(ui, state, name, s).and_then(|t| {
                let f = parse_floats(&t);
                (f.len() == 2).then(|| Variant::Vector2(Vector2::new(f[0], f[1])))
            })
        }
        Variant::Color3(c) => color_edit(ui, state, name, [c.r, c.g, c.b]).map(|c| Variant::Color3(Color3::new(c[0], c[1], c[2]))),
        Variant::Color3uint8(c) => color_edit(ui, state, name, color3uint8_to_f32(*c)).map(|c| {
            let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            Variant::Color3uint8(Color3uint8::new(q(c[0]), q(c[1]), q(c[2])))
        }),
        Variant::BrickColor(b) => {
            let mut out = None;
            let c = b.to_color3uint8();
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                ui.painter().rect_filled(r, 2.0, Color32::from_rgb(c.r, c.g, c.b));
                egui::ComboBox::from_id_salt(("brick", name))
                    .selected_text(b.to_string())
                    .width(ui.available_width())
                    .height(300.0)
                    .show_ui(ui, |ui| {
                        for bc in all_brick_colors() {
                            let cc = bc.to_color3uint8();
                            ui.horizontal(|ui| {
                                let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                                ui.painter().rect_filled(r, 2.0, Color32::from_rgb(cc.r, cc.g, cc.b));
                                if ui.selectable_label(bc == b, bc.to_string()).clicked() {
                                    out = Some(Variant::BrickColor(*bc));
                                }
                            });
                        }
                    });
            });
            out
        }
        Variant::Enum(e) => {
            let Some(en) = enum_name else {
                ui.label(e.to_u32().to_string());
                return None;
            };
            let current = reflection::enum_item_name(en, e.to_u32()).unwrap_or("?");
            let mut out = None;
            egui::ComboBox::from_id_salt(("enum", name))
                .selected_text(current)
                .width(ui.available_width())
                .height(320.0)
                .show_ui(ui, |ui| {
                    for (item, v) in reflection::enum_items(en) {
                        if ui.selectable_label(v == e.to_u32(), item).clicked() {
                            out = Some(Variant::Enum(Enum::from_u32(v)));
                        }
                    }
                });
            out
        }
        Variant::Ref(r) => {
            let label = if r.is_none() {
                "nil".to_owned()
            } else {
                studio.doc().get(*r).map(|i| i.name.clone()).unwrap_or_else(|| "nil".into())
            };
            ui.label(label);
            None
        }
        Variant::CFrame(c) => {
            ui.label(RichText::new(format!("{}, {}, {}", fmt_f(c.position.x), fmt_f(c.position.y), fmt_f(c.position.z))).color(theme::TEXT_DIM));
            None
        }
        Variant::UDim(u) => {
            let s = format!("{}, {}", fmt_f(u.scale), u.offset);
            text_commit(ui, state, name, s).and_then(|t| {
                let f = parse_floats(&t);
                (f.len() == 2).then(|| Variant::UDim(UDim::new(f[0], f[1] as i32)))
            })
        }
        Variant::UDim2(u) => {
            let s = format!("{{{}, {}}}, {{{}, {}}}", fmt_f(u.x.scale), u.x.offset, fmt_f(u.y.scale), u.y.offset);
            text_commit(ui, state, name, s).and_then(|t| {
                let cleaned: String = t.chars().filter(|c| !matches!(c, '{' | '}')).collect();
                let f = parse_floats(&cleaned);
                (f.len() == 4).then(|| Variant::UDim2(UDim2::new(UDim::new(f[0], f[1] as i32), UDim::new(f[2], f[3] as i32))))
            })
        }
        Variant::Content(c) => {
            let s = match c.value() {
                phaedrus_dom::types::ContentType::Uri(u) => u.clone(),
                _ => String::new(),
            };
            text_commit(ui, state, name, s).map(|t| Variant::Content(phaedrus_dom::types::Content::from_uri(t)))
        }
        Variant::ContentId(c) => {
            text_commit(ui, state, name, c.as_str().to_owned()).map(|t| Variant::ContentId(t.into()))
        }
        other => {
            ui.label(RichText::new(format!("{:?}", other.ty())).color(theme::TEXT_DIM));
            None
        }
    }
}

fn num_edit(ui: &mut egui::Ui, state: &mut UiState, name: &str, v: f64) -> Option<f64> {
    let display = {
        let r = (v * 1000.0).round() / 1000.0;
        if r == r.trunc() { format!("{}", r as i64) } else { format!("{r}") }
    };
    text_commit(ui, state, name, display).and_then(|t| eval_number(&t))
}

/// Accepts plain numbers and simple arithmetic like Studio ("2*4", "10/3").
fn eval_number(s: &str) -> Option<f64> {
    let s = s.trim();
    if let Ok(v) = s.parse() {
        return Some(v);
    }
    for op in ['+', '-', '*', '/'] {
        if let Some(i) = s.rfind(op).filter(|i| *i > 0) {
            let (a, b) = (eval_number(&s[..i])?, eval_number(&s[i + 1..])?);
            return Some(match op {
                '+' => a + b,
                '-' => a - b,
                '*' => a * b,
                _ => a / b,
            });
        }
    }
    None
}

fn color_edit(ui: &mut egui::Ui, state: &mut UiState, name: &str, c: [f32; 3]) -> Option<[f32; 3]> {
    let mut out = None;
    ui.horizontal(|ui| {
        let mut rgb = c;
        let resp = ui.color_edit_button_rgb(&mut rgb);
        if resp.changed() {
            out = Some(rgb);
        }
        let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        let s = format!("[{}, {}, {}]", to8(c[0]), to8(c[1]), to8(c[2]));
        if let Some(t) = text_commit(ui, state, name, s) {
            let cleaned: String = t.chars().filter(|ch| !matches!(ch, '[' | ']')).collect();
            let f = parse_floats(&cleaned);
            if f.len() == 3 {
                out = Some([f[0] / 255.0, f[1] / 255.0, f[2] / 255.0]);
            }
        }
    });
    out
}
