//! Studio-style editor camera.
//!
//! - Right mouse drag: look around
//! - Middle mouse drag: pan
//! - Wheel: dolly
//! - WASD / QE: fly (Shift for slow); speeds up the longer keys are held
//! - F: focus selection

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit};
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

use super::ViewportState;
use crate::studio::Studio;

/// User-adjustable fly speed, edited from the Camera settings window.
#[derive(Resource, Debug, Clone, PartialEq)]
pub struct CameraSettings {
    /// Base fly speed in studs per second.
    pub speed: f32,
    /// Ramp up speed while movement keys stay held.
    pub accelerate: bool,
    /// Top speed as a multiple of `speed`, reached after `ramp_time`.
    pub max_boost: f32,
    /// Seconds of holding before reaching `max_boost`.
    pub ramp_time: f32,
    /// Multiplier currently applied (written by the camera system).
    pub boost: f32,
}

impl Default for CameraSettings {
    fn default() -> Self {
        Self { speed: 40.0, accelerate: true, max_boost: 4.0, ramp_time: 2.0, boost: 1.0 }
    }
}

impl CameraSettings {
    /// Boost multiplier after holding movement keys for `held` seconds.
    fn boost_after(&self, held: f32) -> f32 {
        if !self.accelerate || self.max_boost <= 1.0 {
            return 1.0;
        }
        let x = (held / self.ramp_time.max(0.01)).clamp(0.0, 1.0);
        // Smoothstep: gentle start so short taps stay precise.
        1.0 + (self.max_boost - 1.0) * x * x * (3.0 - 2.0 * x)
    }
}

#[derive(Component)]
pub struct EditorCamera {
    pub yaw: f32,
    pub pitch: f32,
    /// Distance used for orbit/focus.
    pub focus_distance: f32,
}

impl EditorCamera {
    pub fn from_transform(t: &Transform) -> Self {
        let (yaw, pitch, _) = t.rotation.to_euler(EulerRot::YXZ);
        Self { yaw, pitch, focus_distance: 20.0 }
    }

    pub fn rotation(&self) -> Quat {
        Quat::from_euler(EulerRot::YXZ, self.yaw, self.pitch, 0.0)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn camera_controls(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    viewport: Res<ViewportState>,
    mut settings: ResMut<CameraSettings>,
    mut held: Local<f32>,
    mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>,
    mut cam: Query<(&mut Transform, &mut EditorCamera)>,
) {
    let Ok((mut t, mut cam)) = cam.single_mut() else { return };
    let dt = time.delta_secs();

    let looking = buttons.pressed(MouseButton::Right) && viewport.drag_started_inside;
    if let Ok(mut c) = cursor.single_mut() {
        // Windows cannot lock the cursor; confining it still yields raw
        // mouse motion for looking around.
        let want = if looking { CursorGrabMode::Confined } else { CursorGrabMode::None };
        if c.grab_mode != want {
            c.grab_mode = want;
            c.visible = !looking;
        }
    }
    if looking {
        let d = motion.delta * 0.004;
        cam.yaw -= d.x;
        cam.pitch = (cam.pitch - d.y).clamp(-1.55, 1.55);
        t.rotation = cam.rotation();
    }

    if buttons.pressed(MouseButton::Middle) && viewport.drag_started_inside {
        let d = motion.delta * (cam.focus_distance.max(4.0) * 0.0015);
        let right = t.right();
        let up = t.up();
        t.translation += -right * d.x + up * d.y;
    }

    if viewport.hovered && scroll.delta.y != 0.0 {
        let lines = match scroll.unit {
            MouseScrollUnit::Line => scroll.delta.y,
            MouseScrollUnit::Pixel => scroll.delta.y / 40.0,
        };
        let step = (cam.focus_distance * 0.15).clamp(1.0, 50.0);
        let fwd = t.forward();
        t.translation += fwd * lines * step;
        cam.focus_distance = (cam.focus_distance - lines * step).max(1.0);
    }

    // Keyboard flight only when the viewport has focus (no text field).
    let mut flying = false;
    if viewport.keyboard_focus && !keys.pressed(KeyCode::ControlLeft) && !keys.pressed(KeyCode::ControlRight) {
        let mut dir = Vec3::ZERO;
        if keys.pressed(KeyCode::KeyW) || keys.pressed(KeyCode::ArrowUp) {
            dir += *t.forward();
        }
        if keys.pressed(KeyCode::KeyS) || keys.pressed(KeyCode::ArrowDown) {
            dir -= *t.forward();
        }
        if keys.pressed(KeyCode::KeyD) || keys.pressed(KeyCode::ArrowRight) {
            dir += *t.right();
        }
        if keys.pressed(KeyCode::KeyA) || keys.pressed(KeyCode::ArrowLeft) {
            dir -= *t.right();
        }
        if keys.pressed(KeyCode::KeyE) {
            dir += Vec3::Y;
        }
        if keys.pressed(KeyCode::KeyQ) {
            dir -= Vec3::Y;
        }
        if dir != Vec3::ZERO {
            flying = true;
            *held += dt;
            let boost = settings.boost_after(*held);
            let slow = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
            let base = if slow { settings.speed * 0.25 } else { settings.speed };
            t.translation += dir.normalize() * base * boost * dt;
            if settings.boost != boost {
                settings.boost = boost;
            }
        }
    }
    if !flying {
        *held = 0.0;
        if settings.boost != 1.0 {
            settings.boost = 1.0;
        }
    }
}

/// Handles `Studio::focus_request` (F key / Zoom To).
pub fn focus_selection(mut studio: ResMut<Studio>, mut cam: Query<(&mut Transform, &mut EditorCamera)>) {
    if !studio.focus_request {
        return;
    }
    studio.focus_request = false;
    let Ok((mut t, mut cam)) = cam.single_mut() else { return };
    let bounds = {
        let doc = studio.doc();
        let mut min = Vec3::splat(f32::MAX);
        let mut max = Vec3::splat(f32::MIN);
        let mut any = false;
        for r in &studio.selection {
            if let Some((a, b)) = doc.bounds(*r) {
                min = min.min(Vec3::new(a.x, a.y, a.z));
                max = max.max(Vec3::new(b.x, b.y, b.z));
                any = true;
            }
        }
        any.then_some((min, max))
    };
    let Some((min, max)) = bounds else { return };
    let center = (min + max) * 0.5;
    let radius = ((max - min).length() * 0.5).max(1.0);
    let dist = radius * 2.2;
    let back = -*t.forward();
    t.translation = center + back * dist;
    cam.focus_distance = dist;
}
