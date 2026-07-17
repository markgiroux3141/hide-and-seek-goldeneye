//! Gamepad input — a thin, domain-agnostic wrapper over [`gilrs`]. Enumerates
//! connected pads, tracks one "active" pad, and exposes its stick axes + button
//! states after each [`Gamepads::poll`]. The game layer maps these neutral reads
//! onto its own control scheme (the USB-N64 GoldenEye "solitaire" bindings live in
//! `game/src/gamepad.rs`); this module knows nothing about that.
//!
//! ## Windows / adapter caveat
//! `gilrs` on Windows is **XInput**-backed. A USB N64 adapter that presents as a
//! raw DirectInput/HID device may either (a) not enumerate here at all, or (b)
//! expose its buttons under gilrs's *semantic* [`Button`] names (South/East/…)
//! rather than the raw HID indices a browser's Gamepad API reports. Because the
//! true mapping can't be known without the physical pad, set the env var
//! `GAMEPAD_DEBUG=1` and the wrapper logs every button/axis event with its gilrs
//! name + native code — press each N64 button once to read off the real layout,
//! then encode it in the game-side binding table.

use std::collections::HashMap;

use gilrs::{Axis, Button, EventType, Gilrs, GamepadId};

/// Re-export the neutral button/axis vocabulary so the game layer can build its
/// binding table without depending on `gilrs` directly.
pub use gilrs::{Axis as PadAxis, Button as PadButton};

pub struct Gamepads {
    gilrs: Gilrs,
    /// The pad we currently read from (first one connected). `None` until one
    /// connects or if enumeration finds none.
    active: Option<GamepadId>,
    /// Set for one `poll` when a pad transitions disconnected → connected, so the
    /// app can grab pointer-lock / drop into gameplay without a mouse click.
    just_connected: bool,
    /// Held state of every button keyed by its raw native code (`Code::into_u32`).
    /// Needed for buttons `gilrs` can't map semantically (reported as
    /// `Button::Unknown`) — e.g. this N64 adapter's C-cluster — which
    /// [`Gamepads::pressed`] would miss. Read via [`Gamepads::pressed_raw`].
    raw_buttons: HashMap<u32, bool>,
    debug: bool,
}

impl Gamepads {
    /// Initialize the gamepad backend. Returns `None` if `gilrs` fails to start
    /// (no input subsystem) — the caller then simply runs keyboard/mouse-only.
    pub fn new() -> Option<Self> {
        let gilrs = match Gilrs::new() {
            Ok(g) => g,
            Err(e) => {
                log::warn!("[gamepad] gilrs init failed ({e:?}); pad input disabled");
                return None;
            }
        };
        let debug = std::env::var("GAMEPAD_DEBUG").is_ok();
        // Adopt the first already-connected pad (gilrs enumerates on construction).
        let mut active = None;
        for (id, gp) in gilrs.gamepads() {
            log::info!("[gamepad] found \"{}\" (id {id})", gp.name());
            if active.is_none() {
                active = Some(id);
            }
        }
        if active.is_none() {
            log::info!("[gamepad] no pad connected yet (hot-plug is supported)");
        }
        Some(Gamepads {
            gilrs,
            active,
            just_connected: false,
            raw_buttons: HashMap::new(),
            debug,
        })
    }

    /// Drain and process the event queue: update connection state, adopt a pad on
    /// hot-plug, and (in debug mode) log raw button/axis activity so an unknown
    /// adapter's layout can be read off. Call once per frame before reading state.
    pub fn poll(&mut self) {
        self.just_connected = false;
        while let Some(ev) = self.gilrs.next_event() {
            match ev.event {
                EventType::Connected => {
                    let name = self.gilrs.gamepad(ev.id).name().to_string();
                    log::info!("[gamepad] connected: \"{name}\" (id {})", ev.id);
                    if self.active.is_none() {
                        self.active = Some(ev.id);
                        self.just_connected = true;
                    }
                }
                EventType::Disconnected => {
                    log::info!("[gamepad] disconnected (id {})", ev.id);
                    if self.active == Some(ev.id) {
                        self.raw_buttons.clear();
                        // Fall back to any other still-connected pad.
                        self.active = self
                            .gilrs
                            .gamepads()
                            .find(|(_, gp)| gp.is_connected())
                            .map(|(id, _)| id);
                    }
                }
                EventType::ButtonPressed(btn, code) => {
                    self.raw_buttons.insert(code.into_u32(), true);
                    if self.debug {
                        log::info!("[gamepad] BUTTON pressed: {btn:?} (code {})", code.into_u32());
                    }
                }
                EventType::ButtonReleased(_, code) => {
                    self.raw_buttons.insert(code.into_u32(), false);
                }
                // Analog-ish buttons (triggers) report their held state here.
                EventType::ButtonChanged(_, val, code) => {
                    self.raw_buttons.insert(code.into_u32(), val > 0.5);
                }
                EventType::AxisChanged(axis, val, code) if self.debug && val.abs() > 0.5 => {
                    log::info!("[gamepad] AXIS {axis:?} = {val:+.2} (code {})", code.into_u32());
                }
                _ => {}
            }
        }
    }

    /// Whether an active pad is currently connected.
    pub fn connected(&self) -> bool {
        self.active
            .map(|id| self.gilrs.gamepad(id).is_connected())
            .unwrap_or(false)
    }

    /// True for exactly the one `poll` in which a pad became connected (edge), so
    /// the app can auto-acquire pointer-lock and enter gameplay.
    pub fn just_connected(&self) -> bool {
        self.just_connected
    }

    /// Current value of `axis` on the active pad (roughly −1..1), or 0 if none.
    /// gilrs applies its own small deadzone; the game adds a radial deadzone on top.
    pub fn axis(&self, axis: Axis) -> f32 {
        self.active
            .map(|id| self.gilrs.gamepad(id).value(axis))
            .unwrap_or(0.0)
    }

    /// Whether `button` is held on the active pad.
    pub fn pressed(&self, button: Button) -> bool {
        self.active
            .map(|id| self.gilrs.gamepad(id).is_pressed(button))
            .unwrap_or(false)
    }

    /// Whether the button with this raw native code is held — for buttons `gilrs`
    /// can't map to a semantic [`Button`] (see [`Self::pressed`]). Codes are read
    /// off the `GAMEPAD_DEBUG=1` log.
    pub fn pressed_raw(&self, code: u32) -> bool {
        self.connected() && *self.raw_buttons.get(&code).unwrap_or(&false)
    }
}
