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
