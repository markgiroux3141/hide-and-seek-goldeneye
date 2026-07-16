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
    /// the crosshair; the future N64-controller path will drive aim mode instead).
    mouse_right: bool,
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
