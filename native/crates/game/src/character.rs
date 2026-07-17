//! First-person character controller for the HUNT phase. A kinematic capsule
//! driven by manual gravity + jump, with collisions/steps/slopes resolved by
//! Rapier's `KinematicCharacterController` (via [`PhysicsWorld::move_character`]).
//!
//! Feel constants are ported verbatim from `src/game/player.js` (meters). The JS
//! resolved collisions against the nav grid; here Rapier's move-and-slide does
//! it against the CSG trimesh colliders — the plan's transliteration, keeping
//! the same tuning so it feels the same.

use glam::Vec3;
use winit::keyboard::KeyCode;

use engine::render::camera::{apply_look_delta, forward_from, view_proj_from};
use engine::geometry::csg_runtime::WORLD_SCALE;
use engine::platform::input::InputState;
use engine::sim::physics::PhysicsWorld;

const WT: f32 = WORLD_SCALE; // meters per world tile

const RADIUS: f32 = 1.0 * WT; // capsule radius (0.25 m)
const HEIGHT: f32 = 6.0 * WT; // full standing height (1.5 m)
const EYE: f32 = 5.4 * WT; // eye offset above feet (1.35 m)
const WALK_SPEED: f32 = 3.2; // m/s
const GRAVITY: f32 = 20.0; // m/s²
const JUMP_VELOCITY: f32 = 5.5; // m/s

/// Capsule cylinder half-height: total = 2·(half + radius) = HEIGHT.
const HALF_HEIGHT: f32 = (HEIGHT - 2.0 * RADIUS) * 0.5; // 0.5 m
/// Capsule midpoint sits this far above the feet.
const CENTER_OFFSET: f32 = HEIGHT * 0.5; // 0.75 m

pub struct CharacterController {
    /// Feet position, meters.
    pub pos: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    vel_y: f32,
    grounded: bool,
}

impl CharacterController {
    /// Spawn with feet at `feet`, inheriting the given look orientation.
    pub fn new(feet: Vec3, yaw: f32, pitch: f32) -> Self {
        CharacterController {
            pos: feet,
            yaw,
            pitch,
            vel_y: 0.0,
            grounded: false,
        }
    }

    /// Mouse-look, once per rendered frame (crisp aim, independent of sim rate).
    pub fn apply_look(&mut self, input: &mut InputState) {
        let (dx, dy) = input.take_mouse_delta();
        if !input.pointer_locked {
            return;
        }
        (self.yaw, self.pitch) = apply_look_delta(self.yaw, self.pitch, dx, dy);
    }

    /// One fixed sim step: horizontal wish-move + gravity/jump, resolved by the
    /// character controller against the static world.
    pub fn apply_move(&mut self, dt: f32, input: &InputState, physics: &mut PhysicsWorld) {
        // Horizontal basis from yaw only (no pitch — feet stay level). `fwd` is the
        // look direction flattened; `right` is its perpendicular (`cross(fwd, up)`).
        let (sy, cy) = self.yaw.sin_cos();
        let fwd = Vec3::new(-sy, 0.0, -cy);
        let right = Vec3::new(cy, 0.0, -sy);
        let mut wish = Vec3::ZERO;
        if input.pointer_locked {
            if input.key_down(KeyCode::KeyW) {
                wish += fwd;
            }
            if input.key_down(KeyCode::KeyS) {
                wish -= fwd;
            }
            if input.key_down(KeyCode::KeyA) {
                wish -= right;
            }
            if input.key_down(KeyCode::KeyD) {
                wish += right;
            }
        }
        // Analog stick (gamepad) wish, added on top of the digital keys. Ported from
        // `PlayerController.update`: analog preserves partial magnitude (clamp to the
        // unit circle, don't normalize) so a half-pushed stick walks at half speed;
        // a purely-digital wish normalizes to full speed as before.
        let (ax, ay) = input.analog_move();
        if input.pointer_locked && (ax != 0.0 || ay != 0.0) {
            wish += right * ax + fwd * ay;
            if wish.length_squared() > 1.0 {
                wish = wish.normalize();
            }
        } else if wish.length_squared() > 0.0 {
            wish = wish.normalize();
        }

        // Gravity + jump (held Space re-jumps on landing, matching player.js).
        self.vel_y -= GRAVITY * dt;
        if self.grounded && input.pointer_locked && input.key_down(KeyCode::Space) {
            self.vel_y = JUMP_VELOCITY;
            self.grounded = false;
        }

        let desired = wish * WALK_SPEED * dt + Vec3::new(0.0, self.vel_y * dt, 0.0);
        let center = self.pos + Vec3::new(0.0, CENTER_OFFSET, 0.0);
        let (corrected, grounded) =
            physics.move_character(dt, RADIUS, HALF_HEIGHT, center, desired);

        self.pos += corrected;
        self.grounded = grounded;
        // Stop accumulating fall speed once the floor is under us.
        if grounded && self.vel_y < 0.0 {
            self.vel_y = 0.0;
        }
    }

    pub fn view_proj(&self, aspect: f32) -> glam::Mat4 {
        view_proj_from(self.eye(), self.forward(), aspect)
    }

    /// Eye (camera) position in world space — feet + eye height. The fire ray
    /// originates here (the crosshair is at the eye centre).
    pub fn eye(&self) -> Vec3 {
        self.pos + Vec3::new(0.0, EYE, 0.0)
    }

    /// Unit look direction (yaw + pitch). The fire ray travels along this, and the
    /// camera looks along it.
    pub fn forward(&self) -> Vec3 {
        forward_from(self.yaw, self.pitch)
    }
}
