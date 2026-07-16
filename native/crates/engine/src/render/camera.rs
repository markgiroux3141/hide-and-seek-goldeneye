//! First-person fly camera — no gravity. Direct port of `src/scene/camera.js`
//! (the BUILD-phase editor camera): pointer-lock mouse look + WASD, Space rises.
//! Tuning constants match the original exactly.

use glam::{Mat4, Vec3};
use winit::keyboard::KeyCode;

use crate::platform::input::InputState;

const MOVE_SPEED: f32 = 8.0; // m/s
const LOOK_SPEED: f32 = 0.002; // radians per pixel of mouse motion

pub struct FlyCamera {
    pub pos: Vec3,
    pub yaw: f32,
    pub pitch: f32,
}

impl FlyCamera {
    pub fn new(pos: Vec3, yaw: f32, pitch: f32) -> Self {
        FlyCamera { pos, yaw, pitch }
    }

    /// Unit look direction. yaw=0,pitch=0 → −Z (Three.js `getWorldDirection`
    /// convention), so movement/picking match the reference build.
    pub fn forward(&self) -> Vec3 {
        forward_from(self.yaw, self.pitch)
    }

    /// Apply mouse-look. Called once per rendered frame (not per sim step) so
    /// aiming stays crisp regardless of the fixed sim rate.
    pub fn apply_look(&mut self, input: &mut InputState) {
        let (dx, dy) = input.take_mouse_delta();
        if !input.pointer_locked {
            return; // delta already drained so a re-lock doesn't jump
        }
        (self.yaw, self.pitch) = apply_look_delta(self.yaw, self.pitch, dx, dy);
    }

    /// Apply fly movement for a fixed timestep. Forward includes pitch, so W
    /// flies where you look; Space rises (no descend key — matches the original).
    pub fn apply_move(&mut self, dt: f32, input: &InputState) {
        if !input.pointer_locked {
            return;
        }
        let forward = self.forward();
        let right = forward.cross(Vec3::Y).normalize_or_zero();
        let step = MOVE_SPEED * dt;
        if input.key_down(KeyCode::KeyW) {
            self.pos += forward * step;
        }
        if input.key_down(KeyCode::KeyS) {
            self.pos -= forward * step;
        }
        if input.key_down(KeyCode::KeyA) {
            self.pos -= right * step;
        }
        if input.key_down(KeyCode::KeyD) {
            self.pos += right * step;
        }
        if input.key_down(KeyCode::Space) {
            self.pos.y += step;
        }
    }

    /// View-projection matrix for the given aspect ratio (right-handed, Y-up).
    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        view_proj_from(self.pos, self.forward(), aspect)
    }
}

/// yaw=0,pitch=0 → −Z look direction (shared by fly-cam and character).
pub fn forward_from(yaw: f32, pitch: f32) -> Vec3 {
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    Vec3::new(-sy * cp, sp, -cy * cp)
}

/// Apply a mouse delta to a (yaw, pitch), clamping pitch to ±90°. Shared so the
/// fly-cam and the character look identical.
pub fn apply_look_delta(yaw: f32, pitch: f32, dx: f32, dy: f32) -> (f32, f32) {
    let limit = std::f32::consts::FRAC_PI_2;
    (yaw - dx * LOOK_SPEED, (pitch - dy * LOOK_SPEED).clamp(-limit, limit))
}

/// Standard perspective view-projection (right-handed, Y-up, 60° vertical FOV).
pub fn view_proj_from(eye: Vec3, forward: Vec3, aspect: f32) -> Mat4 {
    let proj = Mat4::perspective_rh(60f32.to_radians(), aspect, 0.05, 500.0);
    let view = Mat4::look_at_rh(eye, eye + forward, Vec3::Y);
    proj * view
}
