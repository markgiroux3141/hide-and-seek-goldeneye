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

// ─── Orthographic editor views ───────────────────────────────────────────────

/// How far behind the focus point an orthographic eye sits, in metres. An ortho
/// projection has no perspective divide, so this only has to clear the level — it
/// costs nothing in precision (the depth range is linear).
const ORTHO_PULLBACK: f32 = 400.0;

/// Vertical half-extent limits for [`OrthoCamera::zoom`], in metres. The lower bound
/// is roughly one WT cell filling the screen; the upper is far past any level.
const ORTHO_MIN_HALF_H: f32 = 0.25;
const ORTHO_MAX_HALF_H: f32 = 400.0;

/// One of the six axis-aligned orthographic views the editor can snap to.
///
/// Y-up, and `forward` follows the same convention as [`forward_from`] — the
/// direction the camera *looks along*, not the direction it sits in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewAxis {
    Top,
    Bottom,
    Front,
    Back,
    Left,
    Right,
}

impl ViewAxis {
    /// Unit direction the view looks along.
    pub fn forward(self) -> Vec3 {
        match self {
            ViewAxis::Top => Vec3::NEG_Y,
            ViewAxis::Bottom => Vec3::Y,
            ViewAxis::Front => Vec3::NEG_Z,
            ViewAxis::Back => Vec3::Z,
            ViewAxis::Left => Vec3::X,
            ViewAxis::Right => Vec3::NEG_X,
        }
    }

    /// Screen-up for this view. World Y for the four side views; for top/bottom
    /// (where Y is the view normal) −Z points up the screen, so a top-down plan is
    /// laid out with +X right and +Z down — the same handedness as reading a map.
    pub fn up(self) -> Vec3 {
        match self {
            ViewAxis::Top => Vec3::NEG_Z,
            ViewAxis::Bottom => Vec3::Z,
            _ => Vec3::Y,
        }
    }

    /// Screen-right for this view (`forward × up`, right-handed).
    pub fn right(self) -> Vec3 {
        self.forward().cross(self.up())
    }

    pub fn label(self) -> &'static str {
        match self {
            ViewAxis::Top => "top",
            ViewAxis::Bottom => "bottom",
            ViewAxis::Front => "front",
            ViewAxis::Back => "back",
            ViewAxis::Left => "left",
            ViewAxis::Right => "right",
        }
    }
}

/// An axis-aligned orthographic view of the level: which way it looks, what it is
/// centred on, and how much of the world fits vertically.
///
/// Deliberately *not* a variant of [`FlyCamera`] — it shares no state with one (no
/// yaw/pitch, and its "position" is a focus point rather than an eye). The editor
/// holds one of these only while a tool that needs a drafting view is armed, and
/// hands its matrix to the same `view_proj` seam the fly camera feeds, so mouse
/// unprojection, picking and every draw call work unchanged.
#[derive(Clone, Copy, Debug)]
pub struct OrthoCamera {
    pub axis: ViewAxis,
    /// Focus point in world metres — what sits at the centre of the screen.
    pub center: Vec3,
    /// Half the view's vertical extent, in metres. This is the zoom.
    pub half_h: f32,
}

impl OrthoCamera {
    pub fn new(axis: ViewAxis, center: Vec3, half_h: f32) -> Self {
        OrthoCamera {
            axis,
            center,
            half_h: half_h.clamp(ORTHO_MIN_HALF_H, ORTHO_MAX_HALF_H),
        }
    }

    /// View-projection matrix, matching the perspective path's clip conventions
    /// (right-handed, Y-up, 0..1 depth) so the renderer needs no branch of its own.
    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let f = self.axis.forward();
        let eye = self.center - f * ORTHO_PULLBACK;
        let half_w = self.half_h * aspect.max(0.01);
        let proj = Mat4::orthographic_rh(
            -half_w,
            half_w,
            -self.half_h,
            self.half_h,
            0.05,
            ORTHO_PULLBACK * 2.0,
        );
        proj * Mat4::look_at_rh(eye, self.center, self.axis.up())
    }

    /// Drag the view by a cursor delta in physical pixels. The content follows the
    /// cursor, so the focus moves the opposite way.
    pub fn pan(&mut self, dx_px: f32, dy_px: f32, viewport_h_px: f32) {
        if viewport_h_px <= 0.0 {
            return;
        }
        let per_px = 2.0 * self.half_h / viewport_h_px;
        // Screen Y grows downward, world `up` grows upward — hence the sign flip.
        self.center += self.axis.up() * dy_px * per_px - self.axis.right() * dx_px * per_px;
    }

    /// Zoom by wheel notches (positive = in). Geometric, so each notch feels the
    /// same at every scale.
    pub fn zoom(&mut self, steps: f32) {
        self.half_h =
            (self.half_h * 1.15f32.powf(-steps)).clamp(ORTHO_MIN_HALF_H, ORTHO_MAX_HALF_H);
    }

    /// Re-centre and zoom so a world-space AABB (metres) fits with a little margin.
    /// A degenerate or empty box falls back to a sane default framing.
    pub fn frame(&mut self, min: Vec3, max: Vec3) {
        self.center = (min + max) * 0.5;
        let ext = max - min;
        // The vertical extent on screen is whichever world axis maps to screen-up.
        let up = self.axis.up().abs();
        let right = self.axis.right().abs();
        let h = ext.dot(up).abs();
        let w = ext.dot(right).abs();
        let span = h.max(w).max(1.0);
        self.half_h = (span * 0.65).clamp(ORTHO_MIN_HALF_H, ORTHO_MAX_HALF_H);
    }
}
