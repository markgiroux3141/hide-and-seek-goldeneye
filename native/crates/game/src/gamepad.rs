//! USB-N64 controller driver — the GoldenEye "solitaire" control scheme, ported
//! from the 3DS FPS `GamepadManager.ts`. Wraps the engine's neutral
//! [`Gamepads`] reader, maps the N64 buttons/stick onto the game's controls, and
//! injects them into [`InputState`] + drives [`World`] look/aim/move each frame.
//!
//! ## Button mapping — VERIFY ON HARDWARE
//! The 3DS FPS build ran in a browser, which read the adapter as a raw HID device
//! (C-Left = index 0, B = 1, A = 2, …). `gilrs` on Windows is XInput-based and
//! instead reports *semantic* buttons, so those raw indices don't carry over. The
//! table below is a **best guess**; the N64 C-cluster in particular is unknowable
//! without the physical pad. Run with `GAMEPAD_DEBUG=1` and press each N64 button
//! — the engine logs the real gilrs `Button`/`Axis` — then correct the constants
//! here. The core scheme (move, turn, aim, fire) is likely right; the C-buttons
//! (strafe + look-up/down) are the most likely to need remapping.

use engine::platform::gamepad::{Gamepads, PadAxis, PadButton};
use engine::platform::input::InputState;
use winit::keyboard::KeyCode;

use crate::world::{World, STICK_DEADZONE};

// ── N64 → raw-code binding table ────────────────────────────────────────────
// This adapter passes through raw HID button codes that match the browser's
// Gamepad-API indices (the exact table the 3DS FPS `GamepadManager` used), while
// gilrs's *semantic* layer mis-maps them (e.g. C-Up code 9 → `Button::Start`,
// which would fire the pause/cursor-release). So bind everything by raw code via
// `Gamepads::pressed_raw` — confirmed against GAMEPAD_DEBUG=1 on the user's pad.
const CODE_C_LEFT: u32 = 0; // C-Left  → strafe left
const CODE_B: u32 = 1; // B      → reload
#[allow(dead_code)] // A → weapon cycle: inert until a 2nd weapon lands (single PP7 today)
const CODE_A: u32 = 2;
const CODE_C_DOWN: u32 = 3; // C-Down → look down
const CODE_L: u32 = 4; // L shoulder → aim
const CODE_R: u32 = 5; // R shoulder → aim
const CODE_Z: u32 = 6; // Z under-trigger → fire
const CODE_C_RIGHT: u32 = 8; // C-Right → strafe right
const CODE_C_UP: u32 = 9; // C-Up   → look up
const CODE_START: u32 = 12; // Start → pause / release cursor

// Semantic-button + right-stick fallbacks for OTHER adapters (the user's pad works
// purely off the raw codes above; these cost nothing when absent).
const BTN_Z: PadButton = PadButton::LeftTrigger2;
const BTN_L: PadButton = PadButton::LeftTrigger;
const BTN_R: PadButton = PadButton::RightTrigger;
const BTN_B: PadButton = PadButton::East;
const BTN_C_UP: PadButton = PadButton::DPadUp;
const BTN_C_DOWN: PadButton = PadButton::DPadDown;
const BTN_C_LEFT: PadButton = PadButton::DPadLeft;
const BTN_C_RIGHT: PadButton = PadButton::DPadRight;

/// Right-stick deflection past which a C-direction counts as pressed (for adapters
/// that expose the yellow C-cluster as the right analog stick).
const C_STICK_THRESHOLD: f32 = 0.5;

/// One frame's edge-triggered actions the app must handle (held/analog inputs are
/// injected straight into [`InputState`] / [`World`] and aren't reported here).
#[derive(Default)]
pub struct PadActions {
    /// A pad became connected this frame — grab pointer-lock / enter gameplay.
    pub just_connected: bool,
    /// B pressed this frame — reload (or restart, when dead).
    pub reload: bool,
    /// Start pressed this frame — toggle pause (release/grab the cursor).
    pub pause: bool,
}

pub struct N64Pad {
    pads: Gamepads,
    prev_start: bool,
    prev_reload: bool,
}

impl N64Pad {
    /// Initialize the gamepad backend. `None` if no input subsystem is available
    /// (the app then runs keyboard/mouse only).
    pub fn new() -> Option<Self> {
        Gamepads::new().map(|pads| N64Pad {
            pads,
            prev_start: false,
            prev_reload: false,
        })
    }

    /// Whether a pad is currently connected (the app uses this to decide whether
    /// the pad or the mouse owns HUNT look this frame).
    pub fn connected(&self) -> bool {
        self.pads.connected()
    }

    /// Poll the pad and apply the solitaire scheme for this frame: inject held
    /// buttons + analog move into `input`, drive `world` look/aim (HUNT), and
    /// return the edge actions for the app to handle.
    pub fn update(&mut self, dt: f32, input: &mut InputState, world: &mut World) -> PadActions {
        self.pads.poll();
        let mut actions = PadActions {
            just_connected: self.pads.just_connected(),
            ..Default::default()
        };
        if !self.pads.connected() {
            // Clear anything a now-removed pad might have latched.
            input.set_analog_move(0.0, 0.0);
            input.set_mouse_left(false);
            input.release(KeyCode::KeyA);
            input.release(KeyCode::KeyD);
            self.prev_start = false;
            self.prev_reload = false;
            return actions;
        }

        // Left stick with a radial deadzone (prevents diagonal snapping), rescaled
        // so the live range starts at the deadzone edge. Screen convention: +y down.
        let mut sx = self.pads.axis(PadAxis::LeftStickX);
        let mut sy = -self.pads.axis(PadAxis::LeftStickY); // gillrs: +y = up → flip to +y = down
        let mag = (sx * sx + sy * sy).sqrt();
        if mag < STICK_DEADZONE {
            sx = 0.0;
            sy = 0.0;
        } else {
            let scale = (mag - STICK_DEADZONE) / (1.0 - STICK_DEADZONE) / mag;
            sx *= scale;
            sy *= scale;
        }

        // Read by raw code first (the user's adapter), with the semantic/right-stick
        // fallbacks for other pads. NOTE: pause reads ONLY the raw Start code, never
        // semantic `Button::Start` — gilrs mis-maps C-Up (code 9) to Start, which
        // would otherwise fire the pause/cursor-release on every C-Up press.
        let (rx, ry) = (
            self.pads.axis(PadAxis::RightStickX),
            self.pads.axis(PadAxis::RightStickY),
        );
        let aim_mode = self.pads.pressed_raw(CODE_L)
            || self.pads.pressed_raw(CODE_R)
            || self.pads.pressed(BTN_L)
            || self.pads.pressed(BTN_R);
        let fire = self.pads.pressed_raw(CODE_Z) || self.pads.pressed(BTN_Z);
        let c_left =
            self.pads.pressed_raw(CODE_C_LEFT) || self.pads.pressed(BTN_C_LEFT) || rx < -C_STICK_THRESHOLD;
        let c_right =
            self.pads.pressed_raw(CODE_C_RIGHT) || self.pads.pressed(BTN_C_RIGHT) || rx > C_STICK_THRESHOLD;
        let c_up =
            self.pads.pressed_raw(CODE_C_UP) || self.pads.pressed(BTN_C_UP) || ry > C_STICK_THRESHOLD;
        let c_down =
            self.pads.pressed_raw(CODE_C_DOWN) || self.pads.pressed(BTN_C_DOWN) || ry < -C_STICK_THRESHOLD;
        let start = self.pads.pressed_raw(CODE_START);
        let reload = self.pads.pressed_raw(CODE_B) || self.pads.pressed(BTN_B);

        // Held inputs → InputState. C-Left/Right map to the strafe keys (the
        // character controller reads A/D); Z is the trigger (combat reads mouse-left).
        set_key(input, KeyCode::KeyA, c_left);
        set_key(input, KeyCode::KeyD, c_right);
        input.set_mouse_left(fire);

        // Look / aim / analog-move. HUNT runs the full solitaire path; BUILD gets a
        // simple stick-as-WASD fly so you can move while still looking with the mouse.
        if world.is_build() {
            input.set_analog_move(0.0, 0.0);
            set_key(input, KeyCode::KeyW, sy < -0.5);
            set_key(input, KeyCode::KeyS, sy > 0.5);
            // In BUILD the stick also strafes (no C-button strafe needed there).
            set_key(input, KeyCode::KeyA, c_left || sx < -0.5);
            set_key(input, KeyCode::KeyD, c_right || sx > 0.5);
        } else {
            let pitch_axis = (c_down as i32 - c_up as i32) as f32;
            world.gamepad_look(dt, sx, sy, aim_mode, pitch_axis, input);
        }

        // Edges.
        actions.reload = reload && !self.prev_reload;
        actions.pause = start && !self.prev_start;
        self.prev_reload = reload;
        self.prev_start = start;
        actions
    }
}

/// Press/release a synthetic key to mirror a held button.
fn set_key(input: &mut InputState, key: KeyCode, down: bool) {
    if down {
        input.press(key);
    } else {
        input.release(key);
    }
}
