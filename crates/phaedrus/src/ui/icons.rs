//! Vector icons drawn with the egui painter (no bundled artwork).

use bevy_egui::egui::{self, Color32, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};
use phaedrus_dom::reflection;

fn iso_cube(p: &Painter, r: Rect, top: Color32, left: Color32, right: Color32) {
    let c = r.center();
    let s = r.width().min(r.height()) * 0.45;
    let t = pos2(c.x, c.y - s);
    let tl = pos2(c.x - s * 0.87, c.y - s * 0.5);
    let tr = pos2(c.x + s * 0.87, c.y - s * 0.5);
    let m = c;
    let bl = pos2(c.x - s * 0.87, c.y + s * 0.5);
    let br = pos2(c.x + s * 0.87, c.y + s * 0.5);
    let b = pos2(c.x, c.y + s);
    p.add(Shape::convex_polygon(vec![t, tr, m, tl], top, Stroke::NONE));
    p.add(Shape::convex_polygon(vec![tl, m, b, bl], left, Stroke::NONE));
    p.add(Shape::convex_polygon(vec![m, tr, br, b], right, Stroke::NONE));
}

fn doc_icon(p: &Painter, r: Rect, color: Color32, accent: Color32) {
    let r = r.shrink2(vec2(r.width() * 0.18, r.height() * 0.08));
    p.rect_filled(r, 1.5, color);
    for i in 0..3 {
        let y = r.top() + r.height() * (0.3 + i as f32 * 0.2);
        p.line_segment([pos2(r.left() + 2.0, y), pos2(r.right() - 2.0, y)], Stroke::new(1.2, accent));
    }
}

fn folder(p: &Painter, r: Rect, color: Color32) {
    let r = r.shrink(r.width() * 0.08);
    let tab = Rect::from_min_size(r.min + vec2(0.0, r.height() * 0.15), vec2(r.width() * 0.45, r.height() * 0.2));
    p.rect_filled(tab, 1.0, color.gamma_multiply(0.8));
    let body = Rect::from_min_max(pos2(r.left(), r.top() + r.height() * 0.3), r.max - vec2(0.0, r.height() * 0.1));
    p.rect_filled(body, 1.5, color);
}

fn bulb(p: &Painter, r: Rect, color: Color32) {
    let c = r.center() - vec2(0.0, r.height() * 0.08);
    p.circle_filled(c, r.width() * 0.3, color);
    let base = Rect::from_center_size(pos2(c.x, r.bottom() - r.height() * 0.15), vec2(r.width() * 0.3, r.height() * 0.18));
    p.rect_filled(base, 1.0, Color32::from_gray(170));
}

fn people(p: &Painter, r: Rect, color: Color32) {
    let s = r.width();
    for (dx, k) in [(-0.18, 0.8), (0.18, 1.0)] {
        let c = pos2(r.center().x + dx * s, r.top() + s * 0.32);
        p.circle_filled(c, s * 0.14, color.gamma_multiply(k));
        let body = Rect::from_center_size(pos2(c.x, r.top() + s * 0.72), vec2(s * 0.34, s * 0.34));
        p.rect_filled(body, s * 0.15, color.gamma_multiply(k));
    }
}

fn badge(p: &Painter, r: Rect, color: Color32, letter: &str) {
    let rr = r.shrink(r.width() * 0.08);
    p.rect_filled(rr, 3.0, color);
    p.text(rr.center(), egui::Align2::CENTER_CENTER, letter, egui::FontId::proportional(r.height() * 0.62), Color32::WHITE);
}

/// Draws an Explorer icon for a class.
pub fn class_icon(p: &Painter, r: Rect, class: &str) {
    let grey = (Color32::from_gray(200), Color32::from_gray(150), Color32::from_gray(120));
    match class {
        "Workspace" => {
            p.circle_filled(r.center(), r.width() * 0.42, Color32::from_rgb(60, 140, 230));
            p.circle_stroke(r.center(), r.width() * 0.42, Stroke::new(1.0, Color32::from_rgb(140, 210, 255)));
            p.line_segment([r.left_center() + vec2(2.0, 0.0), r.right_center() - vec2(2.0, 0.0)], Stroke::new(1.0, Color32::from_rgb(160, 220, 255)));
        }
        "Model" | "Actor" | "WorldModel" => iso_cube(p, r, Color32::from_rgb(140, 190, 255), Color32::from_rgb(80, 130, 210), Color32::from_rgb(60, 100, 170)),
        "Folder" => folder(p, r, Color32::from_rgb(230, 190, 80)),
        "Script" => doc_icon(p, r, Color32::from_rgb(110, 200, 255), Color32::from_rgb(30, 80, 120)),
        "LocalScript" => doc_icon(p, r, Color32::from_rgb(130, 160, 255), Color32::from_rgb(40, 50, 120)),
        "ModuleScript" => doc_icon(p, r, Color32::from_rgb(190, 140, 255), Color32::from_rgb(70, 40, 120)),
        "Lighting" | "PointLight" | "SpotLight" | "SurfaceLight" => bulb(p, r, Color32::from_rgb(255, 215, 80)),
        "Players" | "Player" | "Teams" | "Team" => people(p, r, Color32::from_rgb(110, 210, 120)),
        "Camera" => {
            let body = Rect::from_center_size(r.center(), vec2(r.width() * 0.62, r.height() * 0.45));
            p.rect_filled(body, 2.0, Color32::from_gray(190));
            p.circle_filled(body.center(), r.width() * 0.13, Color32::from_gray(60));
            p.add(Shape::convex_polygon(
                vec![body.right_center(), body.right_top() + vec2(r.width() * 0.25, -2.0), body.right_bottom() + vec2(r.width() * 0.25, 2.0)],
                Color32::from_gray(160),
                Stroke::NONE,
            ));
        }
        "Terrain" => {
            let pts = vec![
                r.left_bottom() + vec2(1.0, -2.0),
                r.left_bottom() + vec2(r.width() * 0.35, -r.height() * 0.75),
                r.left_bottom() + vec2(r.width() * 0.6, -r.height() * 0.45),
                r.left_bottom() + vec2(r.width() * 0.75, -r.height() * 0.65),
                r.right_bottom() + vec2(-1.0, -2.0),
            ];
            p.add(Shape::convex_polygon(pts, Color32::from_rgb(110, 170, 80), Stroke::NONE));
        }
        "Sky" | "Atmosphere" | "Clouds" => {
            let c = r.center();
            let s = r.width();
            for (dx, dy, rad) in [(-0.18, 0.05, 0.2), (0.12, -0.05, 0.25), (0.25, 0.1, 0.17)] {
                p.circle_filled(pos2(c.x + dx * s, c.y + dy * s), rad * s, Color32::from_rgb(170, 210, 255));
            }
        }
        "Texture" | "Decal" | "ImageLabel" | "ImageButton" => {
            let rr = r.shrink(r.width() * 0.12);
            p.rect_filled(rr, 1.5, Color32::from_rgb(90, 160, 220));
            p.add(Shape::convex_polygon(
                vec![rr.left_bottom(), pos2(rr.center().x - 1.0, rr.center().y), rr.right_bottom()],
                Color32::from_rgb(60, 120, 60),
                Stroke::NONE,
            ));
        }
        "ReplicatedStorage" | "ServerStorage" => iso_cube(p, r, Color32::from_rgb(120, 220, 170), Color32::from_rgb(60, 160, 120), Color32::from_rgb(40, 120, 90)),
        "ReplicatedFirst" => badge(p, r, Color32::from_rgb(70, 140, 220), "1"),
        "ServerScriptService" => doc_icon(p, r, Color32::from_rgb(110, 210, 140), Color32::from_rgb(30, 90, 50)),
        "StarterGui" | "ScreenGui" | "SurfaceGui" | "BillboardGui" => badge(p, r, Color32::from_rgb(90, 170, 90), "G"),
        "StarterPack" | "Backpack" | "Tool" => badge(p, r, Color32::from_rgb(200, 140, 60), "T"),
        "StarterPlayer" | "StarterPlayerScripts" | "StarterCharacterScripts" => people(p, r, Color32::from_rgb(230, 180, 90)),
        "SoundService" | "Sound" => badge(p, r, Color32::from_rgb(90, 140, 210), "♪"),
        "TextChatService" | "Chat" => badge(p, r, Color32::from_rgb(90, 160, 200), "…"),
        "MaterialService" | "MaterialVariant" => {
            p.circle_filled(r.center(), r.width() * 0.4, Color32::from_rgb(200, 120, 90));
            p.circle_filled(r.center() - vec2(r.width() * 0.12, r.height() * 0.12), r.width() * 0.12, Color32::from_rgb(250, 190, 160));
        }
        "SpawnLocation" => iso_cube(p, r, Color32::from_rgb(120, 230, 120), Color32::from_rgb(70, 170, 70), Color32::from_rgb(50, 130, 50)),
        "Humanoid" => people(p, r, Color32::from_rgb(220, 200, 160)),
        _ if reflection::is_a(class, "BasePart") => iso_cube(p, r, grey.0, grey.1, grey.2),
        _ if reflection::is_a(class, "ValueBase") => {
            let c = r.center();
            let s = r.width() * 0.32;
            p.add(Shape::convex_polygon(
                vec![pos2(c.x, c.y - s), pos2(c.x + s, c.y), pos2(c.x, c.y + s), pos2(c.x - s, c.y)],
                Color32::from_rgb(200, 120, 220),
                Stroke::NONE,
            ));
        }
        _ if reflection::is_a(class, "GuiObject") => {
            let rr = r.shrink(r.width() * 0.15);
            p.rect_stroke(rr, 1.0, Stroke::new(1.5, Color32::from_rgb(120, 200, 120)), egui::StrokeKind::Inside);
        }
        _ if reflection::is_a(class, "Constraint") || reflection::is_a(class, "JointInstance") => {
            p.line_segment([r.left_bottom() + vec2(3.0, -3.0), r.right_top() + vec2(-3.0, 3.0)], Stroke::new(2.0, Color32::from_rgb(220, 160, 60)));
            p.circle_filled(r.left_bottom() + vec2(3.0, -3.0), 2.0, Color32::WHITE);
            p.circle_filled(r.right_top() + vec2(-3.0, 3.0), 2.0, Color32::WHITE);
        }
        _ if reflection::is_service(class) => {
            let rr = r.shrink(r.width() * 0.15);
            p.rect_filled(rr, 2.0, Color32::from_rgb(110, 120, 140));
            p.circle_filled(rr.center(), rr.width() * 0.2, Color32::from_rgb(40, 44, 52));
        }
        _ => {
            let rr = r.shrink(r.width() * 0.25);
            p.rect_filled(rr, 2.0, Color32::from_gray(150));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolIcon {
    Select,
    Move,
    Scale,
    Rotate,
    Part,
    Script,
    Material,
    Color,
    Group,
    Ungroup,
    Lock,
    Anchor,
    Explorer,
    Properties,
    Output,
    Play,
    Run,
    Pause,
    Stop,
    Light,
    Model,
    Gear,
    Folder,
    Database,
}

fn arrow(p: &Painter, from: Pos2, to: Pos2, stroke: Stroke) {
    p.line_segment([from, to], stroke);
    let dir = (to - from).normalized();
    let n = vec2(-dir.y, dir.x);
    let head = 3.5;
    p.line_segment([to, to - dir * head + n * head], stroke);
    p.line_segment([to, to - dir * head - n * head], stroke);
}

/// Draws a ribbon/tool icon.
pub fn tool_icon(p: &Painter, r: Rect, icon: ToolIcon, color: Color32) {
    let s = Stroke::new(1.6, color);
    let c = r.center();
    let w = r.width();
    match icon {
        ToolIcon::Select => {
            let pts = vec![
                pos2(c.x - w * 0.22, c.y - w * 0.32),
                pos2(c.x - w * 0.22, c.y + w * 0.22),
                pos2(c.x - w * 0.08, c.y + w * 0.08),
                pos2(c.x + w * 0.04, c.y + w * 0.32),
                pos2(c.x + w * 0.12, c.y + w * 0.28),
                pos2(c.x + w * 0.0, c.y + w * 0.04),
                pos2(c.x + w * 0.2, c.y + w * 0.04),
            ];
            p.add(Shape::closed_line(pts, s));
        }
        ToolIcon::Move => {
            let d = w * 0.38;
            arrow(p, c, c + vec2(d, 0.0), s);
            arrow(p, c, c - vec2(d, 0.0), s);
            arrow(p, c, c + vec2(0.0, d), s);
            arrow(p, c, c - vec2(0.0, d), s);
        }
        ToolIcon::Scale => {
            let rr = Rect::from_center_size(c + vec2(-w * 0.08, w * 0.08), vec2(w * 0.45, w * 0.45));
            p.rect_stroke(rr, 1.0, s, egui::StrokeKind::Middle);
            arrow(p, rr.center(), c + vec2(w * 0.36, -w * 0.36), s);
        }
        ToolIcon::Rotate => {
            let rad = w * 0.3;
            let pts: Vec<Pos2> = (0..=20)
                .map(|i| {
                    let a = -0.3 + i as f32 / 20.0 * 4.9;
                    c + vec2(a.cos(), a.sin()) * rad
                })
                .collect();
            let end = *pts.last().unwrap();
            p.add(Shape::line(pts, s));
            p.line_segment([end, end + vec2(4.0, 0.5)], s);
            p.line_segment([end, end + vec2(0.5, 4.0)], s);
        }
        ToolIcon::Part => iso_cube(p, r.shrink(w * 0.08), Color32::from_gray(225), Color32::from_gray(160), Color32::from_gray(120)),
        ToolIcon::Model => iso_cube(p, r.shrink(w * 0.08), Color32::from_rgb(140, 190, 255), Color32::from_rgb(80, 130, 210), Color32::from_rgb(60, 100, 170)),
        ToolIcon::Script => doc_icon(p, r, Color32::from_rgb(140, 210, 100), Color32::from_rgb(40, 80, 30)),
        ToolIcon::Material => {
            p.circle_filled(c, w * 0.34, Color32::from_rgb(170, 170, 180));
            p.circle_filled(c - vec2(w * 0.1, w * 0.1), w * 0.1, Color32::WHITE);
        }
        ToolIcon::Color => {
            let cols = [Color32::from_rgb(240, 80, 80), Color32::from_rgb(250, 200, 60), Color32::from_rgb(80, 200, 100), Color32::from_rgb(80, 140, 250)];
            for (i, col) in cols.iter().enumerate() {
                let a = i as f32 / 4.0 * std::f32::consts::TAU;
                p.circle_filled(c + vec2(a.cos(), a.sin()) * w * 0.18, w * 0.14, *col);
            }
        }
        ToolIcon::Group | ToolIcon::Ungroup => {
            let rr = Rect::from_center_size(c, vec2(w * 0.7, w * 0.6));
            let dash = Stroke::new(1.2, color);
            p.add(Shape::dashed_line(&[rr.left_top(), rr.right_top(), rr.right_bottom(), rr.left_bottom(), rr.left_top()], dash, 3.0, 2.0));
            iso_cube(p, Rect::from_center_size(c, vec2(w * 0.4, w * 0.4)), Color32::from_gray(225), Color32::from_gray(160), Color32::from_gray(120));
        }
        ToolIcon::Lock => {
            let body = Rect::from_center_size(c + vec2(0.0, w * 0.1), vec2(w * 0.5, w * 0.38));
            p.rect_filled(body, 2.0, color);
            let pts: Vec<Pos2> = (0..=12)
                .map(|i| {
                    let a = std::f32::consts::PI + i as f32 / 12.0 * std::f32::consts::PI;
                    pos2(c.x, body.top()) + vec2(a.cos(), a.sin()) * w * 0.17
                })
                .collect();
            p.add(Shape::line(pts, Stroke::new(2.0, color)));
        }
        ToolIcon::Anchor => {
            p.text(c, egui::Align2::CENTER_CENTER, "⚓", egui::FontId::proportional(w * 0.8), color);
        }
        ToolIcon::Explorer => {
            for i in 0..3 {
                let y = r.top() + w * (0.25 + i as f32 * 0.25);
                let x = r.left() + w * (0.2 + if i == 0 { 0.0 } else { 0.15 });
                p.rect_filled(Rect::from_min_size(pos2(x, y - 2.0), vec2(4.0, 4.0)), 1.0, color);
                p.line_segment([pos2(x + 6.0, y), pos2(r.right() - w * 0.15, y)], s);
            }
        }
        ToolIcon::Properties | ToolIcon::Output => {
            for i in 0..4 {
                let y = r.top() + w * (0.22 + i as f32 * 0.18);
                p.line_segment([pos2(r.left() + w * 0.18, y), pos2(r.right() - w * 0.18, y)], s);
            }
        }
        ToolIcon::Play | ToolIcon::Run => {
            let col = if icon == ToolIcon::Play { Color32::from_rgb(90, 160, 255) } else { Color32::from_rgb(110, 210, 110) };
            p.add(Shape::convex_polygon(
                vec![pos2(c.x - w * 0.22, c.y - w * 0.28), pos2(c.x + w * 0.28, c.y), pos2(c.x - w * 0.22, c.y + w * 0.28)],
                col,
                Stroke::NONE,
            ));
        }
        ToolIcon::Pause => {
            p.rect_filled(Rect::from_center_size(c - vec2(w * 0.12, 0.0), vec2(w * 0.12, w * 0.5)), 1.0, color);
            p.rect_filled(Rect::from_center_size(c + vec2(w * 0.12, 0.0), vec2(w * 0.12, w * 0.5)), 1.0, color);
        }
        ToolIcon::Stop => {
            p.rect_filled(Rect::from_center_size(c, vec2(w * 0.45, w * 0.45)), 1.5, Color32::from_rgb(230, 70, 70));
        }
        ToolIcon::Light => bulb(p, r, Color32::from_rgb(255, 215, 80)),
        ToolIcon::Gear => {
            p.circle_stroke(c, w * 0.25, Stroke::new(2.5, color));
            for i in 0..8 {
                let a = i as f32 / 8.0 * std::f32::consts::TAU;
                let d = vec2(a.cos(), a.sin());
                p.line_segment([c + d * w * 0.25, c + d * w * 0.38], Stroke::new(2.5, color));
            }
        }
        ToolIcon::Folder => folder(p, r, Color32::from_rgb(230, 190, 80)),
        ToolIcon::Database => {
            // A stack of three disks.
            let rx = w * 0.3;
            let ry = w * 0.09;
            let disk = |y: f32| {
                let pts: Vec<Pos2> = (0..24)
                    .map(|i| {
                        let a = i as f32 / 24.0 * std::f32::consts::TAU;
                        pos2(c.x + rx * a.cos(), y + ry * a.sin())
                    })
                    .collect();
                p.add(Shape::closed_line(pts, s));
            };
            let top = c.y - w * 0.28;
            let bottom = c.y + w * 0.28;
            disk(top);
            for y in [c.y - w * 0.04, bottom] {
                let pts: Vec<Pos2> = (0..=12)
                    .map(|i| {
                        let a = i as f32 / 12.0 * std::f32::consts::PI;
                        pos2(c.x + rx * a.cos(), y + ry * a.sin())
                    })
                    .collect();
                p.add(Shape::line(pts, s));
            }
            p.line_segment([pos2(c.x - rx, top), pos2(c.x - rx, bottom)], s);
            p.line_segment([pos2(c.x + rx, top), pos2(c.x + rx, bottom)], s);
        }
    }
}
