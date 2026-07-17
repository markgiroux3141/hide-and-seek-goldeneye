//! Minimal input state: held keys + accumulated mouse delta + pointer-lock flag.
//! The app feeds winit events in; the camera and authoring loop read out. Mirrors
//! `src/input/input.js` (keys `Set`, `consumeMouseDelta`, `isPointerLocked`).

use std::collections::HashSet;

use winit::keyboard::KeyCode;

#[derive(Default)]
pub struct InputState {
    keys: HashSet<KeyCode>,
    mouse_dx: f32,
    mouse_dy: f32,
    pub pointer_locked: bool,
    /// Left mouse button held (JS `InputManager.mouseButtons`). Combat reads this
    /// each frame for firing; edge detection (semi-auto) is done in the weapon.
    mouse_left: bool,
    /// Right mouse button held — the GoldenEye free-aim modifier (hold to float
    /// the crosshair; the N64-controller path drives this via the L/R triggers).
    mouse_right: bool,
    /// Analog wish-move from a gamepad stick this frame: `(strafe, forward)`, each
    /// roughly −1..1 (magnitude preserved for proportional speed). Written by the
    /// gamepad driver, consumed + cleared each fixed step by the character
    /// controller. Zero when no pad drives movement (keyboard uses the digital keys).
    analog_move: (f32, f32),
}

impl InputState {
    pub fn press(&mut self, key: KeyCode) {
        self.keys.insert(key);
    }

    pub fn release(&mut self, key: KeyCode) {
        self.keys.remove(&key);
    }

    pub fn key_down(&self, key: KeyCode) -> bool {
        self.keys.contains(&key)
    }

    /// Set the left-mouse-button held state (from winit press/release events).
    pub fn set_mouse_left(&mut self, down: bool) {
        self.mouse_left = down;
    }

    /// Whether the left mouse button is currently held.
    pub fn mouse_left_down(&self) -> bool {
        self.mouse_left
    }

    /// Set the right-mouse-button held state (from winit press/release events).
    pub fn set_mouse_right(&mut self, down: bool) {
        self.mouse_right = down;
    }

    /// Whether the right mouse button is currently held (free-aim modifier).
    pub fn mouse_right_down(&self) -> bool {
        self.mouse_right
    }

    /// Set this frame's analog wish-move `(strafe, forward)` (from a gamepad stick).
    /// The gamepad driver refreshes this every frame — including back to `(0, 0)`
    /// when no pad drives movement — so it's read (not consumed) each fixed substep
    /// like the held keys, and a removed pad can't strand a stale value.
    pub fn set_analog_move(&mut self, strafe: f32, forward: f32) {
        self.analog_move = (strafe, forward);
    }

    /// This frame's analog wish-move `(strafe, forward)`; `(0, 0)` if none.
    pub fn analog_move(&self) -> (f32, f32) {
        self.analog_move
    }

    pub fn add_mouse(&mut self, dx: f32, dy: f32) {
        self.mouse_dx += dx;
        self.mouse_dy += dy;
    }

    /// Read and reset the accumulated mouse delta (JS `consumeMouseDelta`).
    pub fn take_mouse_delta(&mut self) -> (f32, f32) {
        let d = (self.mouse_dx, self.mouse_dy);
        self.mouse_dx = 0.0;
        self.mouse_dy = 0.0;
        d
    }
}
