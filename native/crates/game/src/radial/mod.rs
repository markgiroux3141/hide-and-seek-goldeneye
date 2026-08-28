//! The middle-mouse radial menu (BUILD only).
//!
//! Forty-six bindings, and seven letters that mean different things depending on
//! the mode, is more than a person should have to hold. This is the
//! discoverability layer over them: **hold middle mouse, flick, release**. Every
//! key still works exactly as it did — each slot prints its own hotkey, so using
//! the menu teaches the key rather than replacing it.
//!
//! Scope is deliberately narrow: **BUILD, with no panel open.** Not HUNT (a menu
//! in a firefight is a liability), and not while the `O` object panel is up
//! (that panel already *is* the menu for its own contents — the radial's only
//! business there is opening it).
//!
//! This module is the pure half: the state machine, the geometry, and the menu
//! tables. It owns no window, no `World` and no egui — the tables are built from
//! a plain [`RadialCtx`] snapshot, which is what makes the whole thing testable
//! headlessly. [`crate::app`] does the wiring and `radial::paint` draws it.
//!
//! ## The interaction model
//!
//! Two states, never both:
//!
//! * **Held** — MMB is down. Raw mouse motion drives a *virtual* pointer instead
//!   of the camera; angle picks a slot. Push a `▸` slot past [`EXPAND_R`] to
//!   descend into it (the pointer re-centres, Maya marking-menu style); fall
//!   back inside [`INNER_R`] to come back up. Release commits.
//! * **Sticky** — the ring stays up and the real cursor drives it, so the labels
//!   can be read at leisure. Entered by a quick tap, by releasing on a submenu,
//!   or by opening the ring when the cursor was already free.

use crate::app::PanelTab;

pub mod paint;

#[cfg(test)]
mod tests;

// ─── Geometry (egui points) ──────────────────────────────────────────────────

/// Dead zone. Inside this the pointer selects nothing: it is the cancel target at
/// the root, and the "go back up" target inside a submenu.
pub const INNER_R: f32 = 46.0;
/// Where the slot chips are centred.
pub const RING_R: f32 = 136.0;
/// Push a `▸` slot past this while held to descend into it. Comfortably outside
/// the chips, so brushing one on the way to another never descends by accident.
pub const EXPAND_R: f32 = 196.0;
/// The virtual pointer is clamped here so a hard flick can't send it to infinity
/// and make the return trip a long one.
pub const MAX_R: f32 = 232.0;
/// Released inside [`INNER_R`] under this many seconds → sticky, rather than a
/// cancel. The whole "tap for a menu you can read" gesture is this constant.
pub const TAP_SECS: f32 = 0.18;
/// Raw mouse delta → virtual pointer points. Tuned so a comfortable flick from
/// the centre lands past [`RING_R`] without reaching the clamp.
pub const SENSITIVITY: f32 = 0.55;

// ─── What a slot can do ──────────────────────────────────────────────────────

/// The modal authoring tools, in the order they sit on the Tools ring.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Draw,
    Door,
    Hole,
    Pillar,
    Brace,
    Platform,
    BlockStairs,
    Connect,
}

/// Operations on the current face / structure selection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectionOp {
    Push,
    Pull,
    Delete,
    Grounded,
    Railings,
    StairUp,
    StairDown,
    ConfirmStairs,
}

/// One thing the editor can be asked to do, from a key or from a radial slot.
///
/// This enum is the anti-drift device. What `B` *does* used to live only inside
/// `on_key_pressed`'s body; with two front-ends that body would have been copied,
/// and the copies would have diverged. Both now dispatch to `App::apply`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditorAction {
    /// Arm/toggle a modal tool (pressing the armed one again disarms it).
    ArmTool(Tool),
    /// Act on the current selection.
    Selection(SelectionOp),
    /// Open the left authoring panel on a given tab (or switch tabs if it's open).
    OpenPanel(PanelTab),
    /// Retexture the room at the crosshair with this `textures::schemes()` index.
    SetScheme(usize),
    /// BUILD → HUNT.
    EnterHunt,
    /// Load / save a numbered quick-slot.
    LoadSlot(u8),
    SaveSlot(u8),
    /// Save the open level back to its own file (`Ctrl+S`). A level that has never been
    /// written has no file to save to, and the panel says so instead of guessing a name.
    SaveCurrentLevel,
    ToggleGrid,
    ToggleLighting,
    ToggleNavOverlay,
    ToggleProcPreview,
    ToggleInvincible,
    ToggleInvisible,
    ToggleHunters,
    /// Change how many hunters a wave floods in (`[` / `]`).
    WaveSize(i32),
    /// Append the hunter telemetry dump (`F10`).
    DumpTelemetry,
}

/// Which ring is showing. Depth is capped at 2 by construction: only [`MenuId::Root`]
/// contains submenus.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuId {
    Root,
    Tools,
    Selection,
    Objects,
    Textures,
    Level,
    View,
    Debug,
}

/// What a slot does when committed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Act(EditorAction),
    Menu(MenuId),
}

/// One chip on the ring.
pub struct Slot {
    /// The name, including any live state ("Lighting: real").
    pub label: String,
    /// The keyboard binding, printed small underneath. This is the teaching half.
    pub hint: &'static str,
    pub target: Target,
    /// Dimmed and unclickable when false.
    pub enabled: bool,
    /// Draw the "currently on / armed" accent.
    pub on: bool,
    /// Committing this leaves the ring open (the wave-size steppers), so it can be
    /// clicked repeatedly without a re-open per press.
    pub repeat: bool,
}

impl Slot {
    fn act(label: impl Into<String>, hint: &'static str, action: EditorAction) -> Slot {
        Slot {
            label: label.into(),
            hint,
            target: Target::Act(action),
            enabled: true,
            on: false,
            repeat: false,
        }
    }

    fn menu(label: impl Into<String>, hint: &'static str, id: MenuId) -> Slot {
        Slot {
            label: label.into(),
            hint,
            target: Target::Menu(id),
            enabled: true,
            on: false,
            repeat: false,
        }
    }

    fn enabled(mut self, yes: bool) -> Slot {
        self.enabled = yes;
        self
    }

    fn on(mut self, yes: bool) -> Slot {
        self.on = yes;
        self
    }

    fn repeat(mut self) -> Slot {
        self.repeat = true;
        self
    }

    /// Whether this slot is a submenu (drawn with a `▸`).
    pub fn is_menu(&self) -> bool {
        matches!(self.target, Target::Menu(_))
    }
}

// ─── The snapshot the tables are built from ──────────────────────────────────

/// Everything the menus need to read, gathered by the app before the egui pass.
///
/// A snapshot rather than a `&World` for two reasons: `build_egui_frame` cannot
/// hold a `&mut self` while the egui closure runs (see its doc comment), and a
/// plain value makes every table unit-testable with no window and no world.
#[derive(Clone, Default)]
pub struct RadialCtx {
    /// Ctrl held right now — on the Level ring this flips load into save, the same
    /// convention as `Ctrl+F1..F8`.
    pub ctrl: bool,
    pub grid: bool,
    pub real_lighting: bool,
    pub nav_overlay: bool,
    pub proc_preview: bool,
    pub invincible: bool,
    pub invisible: bool,
    pub hunters: bool,
    pub wave: usize,
    /// A face / structure is selected, so the Selection ring is live.
    pub has_selection: bool,
    /// A stair op is pending confirmation.
    pub pending_stair: bool,
    /// Which modal tool is armed, if any (drawn with the "on" accent).
    pub armed: Option<Tool>,
    /// The digit-bound texture schemes: `(key, label, scheme index)`.
    pub schemes: Vec<(char, String, usize)>,
    /// Whether each quick-slot 1..8 has a file on disk.
    pub slots: [bool; 8],
    /// The open level's display name, or `None` if it has never been saved to a file.
    pub level: Option<String>,
    /// The open level has edits that aren't on disk (drawn as a trailing `*`).
    pub level_dirty: bool,
}

// ─── The tables ──────────────────────────────────────────────────────────────

/// Build the slots for one ring. Slot 0 is north, then clockwise.
///
/// Layout is fixed and entries grey out rather than disappearing: muscle memory is
/// the entire point of a radial, and a ring that reshuffles by context has none.
pub fn menu(id: MenuId, ctx: &RadialCtx) -> Vec<Slot> {
    match id {
        MenuId::Root => vec![
            Slot::menu("Tools", "", MenuId::Tools).on(ctx.armed.is_some()),
            Slot::menu("Selection", "", MenuId::Selection).enabled(ctx.has_selection || ctx.pending_stair),
            Slot::menu("Objects", "O", MenuId::Objects),
            Slot::menu("Textures", "1-9", MenuId::Textures).enabled(!ctx.schemes.is_empty()),
            Slot::act("▶ Enter HUNT", "G", EditorAction::EnterHunt),
            Slot::menu("Level", "F1-F8", MenuId::Level),
            Slot::menu("View", "", MenuId::View),
            Slot::menu("Debug", "", MenuId::Debug),
        ],
        MenuId::Tools => {
            let t = |tool: Tool, label: &str, hint: &'static str| {
                Slot::act(label, hint, EditorAction::ArmTool(tool)).on(ctx.armed == Some(tool))
            };
            vec![
                t(Tool::Draw, "Draw", "Q"),
                t(Tool::Door, "Door", "B"),
                t(Tool::Hole, "Hole", "H"),
                t(Tool::Pillar, "Pillar", "P"),
                t(Tool::Brace, "Brace", "R"),
                t(Tool::Platform, "Platform", "T"),
                t(Tool::BlockStairs, "Block stairs", "K"),
                t(Tool::Connect, "Connect", "C"),
            ]
        }
        MenuId::Selection => {
            let sel = ctx.has_selection;
            let s = |op: SelectionOp, label: &str, hint: &'static str, live: bool| {
                Slot::act(label, hint, EditorAction::Selection(op)).enabled(live)
            };
            vec![
                s(SelectionOp::Push, "Push", "=", sel),
                s(SelectionOp::Pull, "Pull", "-", sel),
                s(SelectionOp::Delete, "Delete", "X", sel),
                s(SelectionOp::Grounded, "Grounded", "F", sel),
                s(SelectionOp::Railings, "Railings", "V", sel),
                s(SelectionOp::StairUp, "Stairs up", "\u{2191}", sel),
                s(SelectionOp::StairDown, "Stairs down", "\u{2193}", sel),
                s(
                    SelectionOp::ConfirmStairs,
                    "Confirm stairs",
                    "\u{23ce}",
                    ctx.pending_stair,
                ),
            ]
        }
        MenuId::Objects => PanelTab::ALL
            .iter()
            .map(|&tab| {
                Slot::act(
                    tab.title(),
                    if tab == PanelTab::Objects { "O" } else { "" },
                    EditorAction::OpenPanel(tab),
                )
            })
            .collect(),
        MenuId::Textures => {
            // Only the digits this level actually resolves, labelled with what they
            // resolve *to* — the whole point is not having to remember that 4 is the
            // Facility tile set on this map and something else on the next one.
            let mut v: Vec<Slot> = ctx
                .schemes
                .iter()
                .map(|(key, label, idx)| {
                    let hint: &'static str = digit_hint(*key);
                    Slot::act(label.clone(), hint, EditorAction::SetScheme(*idx))
                })
                .collect();
            // The library is ~390 themes; a ring is the wrong shape for that, so the
            // rest live behind the panel.
            v.push(Slot::act(
                "More\u{2026}",
                "O",
                EditorAction::OpenPanel(PanelTab::Textures),
            ));
            v
        }
        MenuId::Level => {
            // The named level comes first: it is the primary way to save now, and the
            // eight files below it are just the ones the F-keys still reach.
            let mut v = vec![
                Slot::act(
                    match (&ctx.level, ctx.level_dirty) {
                        (Some(n), true) => format!("Save \"{n}\" *"),
                        (Some(n), false) => format!("Save \"{n}\""),
                        (None, _) => "Save (unnamed)".to_string(),
                    },
                    "Ctrl+S",
                    EditorAction::SaveCurrentLevel,
                )
                .enabled(ctx.level.is_some())
                .on(ctx.level_dirty),
                Slot::act(
                    "All levels\u{2026}",
                    "O",
                    EditorAction::OpenPanel(PanelTab::Levels),
                ),
            ];
            v.extend((1u8..=8).map(|n| {
                let used = ctx.slots[(n - 1) as usize];
                let label = if ctx.ctrl {
                    format!("SAVE \u{2192} {n}")
                } else if used {
                    format!("Load {n}")
                } else {
                    format!("Slot {n}")
                };
                let action = if ctx.ctrl {
                    EditorAction::SaveSlot(n)
                } else {
                    EditorAction::LoadSlot(n)
                };
                Slot::act(label, fkey_hint(n), action)
                    // Loading an empty slot is a no-op that logs a warning; saving to
                    // one is the normal case.
                    .enabled(ctx.ctrl || used)
                    .on(used)
            }));
            v
        }
        MenuId::View => vec![
            Slot::act(
                if ctx.grid { "View: grid" } else { "View: textured" },
                "\\",
                EditorAction::ToggleGrid,
            )
            .on(ctx.grid),
            Slot::act(
                if ctx.real_lighting {
                    "Lighting: real"
                } else {
                    "Lighting: flat"
                },
                "L",
                EditorAction::ToggleLighting,
            )
            .on(ctx.real_lighting),
            Slot::act(
                if ctx.nav_overlay {
                    "Nav overlay: ON"
                } else {
                    "Nav overlay: off"
                },
                "",
                EditorAction::ToggleNavOverlay,
            )
            .on(ctx.nav_overlay),
            Slot::act("Nav validation", "", EditorAction::OpenPanel(PanelTab::Nav)),
            Slot::act(
                if ctx.proc_preview {
                    "Anim preview: ON"
                } else {
                    "Anim preview: off"
                },
                "Y",
                EditorAction::ToggleProcPreview,
            )
            .on(ctx.proc_preview),
        ],
        MenuId::Debug => vec![
            Slot::act(
                if ctx.hunters {
                    "Hunters: ON"
                } else {
                    "Hunters: off"
                },
                "J",
                EditorAction::ToggleHunters,
            )
            .on(ctx.hunters),
            Slot::act(format!("Wave: {} +", ctx.wave), "]", EditorAction::WaveSize(1)).repeat(),
            Slot::act(
                if ctx.invincible {
                    "Invincible: ON"
                } else {
                    "Invincible: off"
                },
                "I",
                EditorAction::ToggleInvincible,
            )
            .on(ctx.invincible),
            Slot::act("Telemetry dump", "F10", EditorAction::DumpTelemetry),
            Slot::act(
                if ctx.invisible {
                    "Invisible: ON"
                } else {
                    "Invisible: off"
                },
                "N",
                EditorAction::ToggleInvisible,
            )
            .on(ctx.invisible),
            Slot::act(format!("Wave: {} \u{2212}", ctx.wave), "[", EditorAction::WaveSize(-1)).repeat(),
        ],
    }
}

/// The breadcrumb shown in the middle of the ring.
pub fn menu_title(id: MenuId) -> &'static str {
    match id {
        MenuId::Root => "BUILD",
        MenuId::Tools => "TOOLS",
        MenuId::Selection => "SELECTION",
        MenuId::Objects => "OBJECTS",
        MenuId::Textures => "TEXTURES",
        MenuId::Level => "LEVEL",
        MenuId::View => "VIEW",
        MenuId::Debug => "DEBUG",
    }
}

/// `'1'..'9'` as a `&'static str` for the hint line (the slots are built per frame,
/// so this avoids a `String` per digit).
fn digit_hint(key: char) -> &'static str {
    const D: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];
    D.get(key as usize - '1' as usize).copied().unwrap_or("")
}

fn fkey_hint(n: u8) -> &'static str {
    const F: [&str; 8] = ["F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8"];
    F.get((n - 1) as usize).copied().unwrap_or("")
}

// ─── Geometry helper ─────────────────────────────────────────────────────────

/// Which of `n` evenly-spaced slots the pointer is over, or `None` inside the dead
/// zone. Slot 0 is north and they run clockwise, matching the drawn ring.
pub fn slot_at(ptr: (f32, f32), n: usize) -> Option<usize> {
    if n == 0 {
        return None;
    }
    let (x, y) = ptr;
    if (x * x + y * y).sqrt() < INNER_R {
        return None;
    }
    // Screen space has +y downward, so north is -y. `atan2(x, -y)` puts 0 at north
    // and grows clockwise, which is the drawn order.
    let tau = std::f32::consts::TAU;
    let a = x.atan2(-y);
    let a = if a < 0.0 { a + tau } else { a };
    let step = tau / n as f32;
    Some(((a / step).round() as usize) % n)
}

/// The direction unit vector for slot `i` of `n`, in screen space.
pub fn slot_dir(i: usize, n: usize) -> (f32, f32) {
    let a = std::f32::consts::TAU * i as f32 / n as f32;
    (a.sin(), -a.cos())
}

// ─── The state machine ───────────────────────────────────────────────────────

/// What the app must do to the pointer lock after handing an event to the radial.
/// The radial has no window, so it says what it needs and the app performs it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LockRequest {
    /// Nothing to do.
    None,
    /// Free the cursor — the ring went sticky and wants a real pointer. The app
    /// remembers the previous lock state to restore on close.
    Free,
    /// Put the lock back exactly as it was before the ring opened.
    Restore,
}

/// A frame's worth of ring, ready to paint (see [`Radial::view`]).
pub struct RadialView {
    pub origin: (f32, f32),
    pub ptr: (f32, f32),
    pub slots: Vec<Slot>,
    /// The slot under the pointer, already filtered for `enabled`.
    pub hovered: Option<usize>,
    pub title: &'static str,
    pub sticky: bool,
    /// Inside a submenu, so the hub reads "back" rather than "cancel".
    pub can_back: bool,
}

/// The live radial menu.
#[derive(Default)]
pub struct Radial {
    open: bool,
    held: bool,
    sticky: bool,
    held_secs: f32,
    /// Screen position (egui points) the ring is drawn around.
    origin: (f32, f32),
    /// Pointer offset from [`Self::origin`]. Integrated from raw motion while held,
    /// or read straight off the cursor while sticky.
    ptr: (f32, f32),
    stack: Vec<MenuId>,
    /// Set once the pointer has left the dead zone, so the pointer re-centring that
    /// follows a descend doesn't instantly read as "go back up".
    back_armed: bool,
}

impl Radial {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Whether MMB is currently down and driving the virtual pointer (so the app
    /// diverts raw mouse motion away from the camera).
    pub fn is_held(&self) -> bool {
        self.held
    }

    pub fn is_sticky(&self) -> bool {
        self.sticky
    }

    /// Where the ring is drawn (egui points).
    pub fn origin(&self) -> (f32, f32) {
        self.origin
    }

    /// The ring currently showing.
    pub fn menu_id(&self) -> MenuId {
        self.stack.last().copied().unwrap_or(MenuId::Root)
    }

    /// Open at `origin` (egui points). `locked` is the current pointer-lock state:
    /// with the cursor already free there is no raw motion to integrate, so the ring
    /// starts sticky and the real cursor drives it.
    pub fn press(&mut self, origin: (f32, f32), locked: bool) -> LockRequest {
        self.open = true;
        self.held = locked;
        self.sticky = !locked;
        self.held_secs = 0.0;
        self.origin = origin;
        self.ptr = (0.0, 0.0);
        self.stack = vec![MenuId::Root];
        self.back_armed = false;
        LockRequest::None
    }

    /// Raw mouse motion while held (device units).
    pub fn motion(&mut self, dx: f32, dy: f32) {
        if !self.held {
            return;
        }
        let (mut x, mut y) = self.ptr;
        x += dx * SENSITIVITY;
        y += dy * SENSITIVITY;
        let r = (x * x + y * y).sqrt();
        if r > MAX_R {
            let k = MAX_R / r;
            x *= k;
            y *= k;
        }
        self.ptr = (x, y);
    }

    /// Absolute cursor position (egui points) while sticky.
    pub fn cursor(&mut self, x: f32, y: f32) {
        if !self.sticky {
            return;
        }
        self.ptr = (x - self.origin.0, y - self.origin.1);
    }

    /// Per-frame tick: ages the hold clock and runs the radius-driven descend /
    /// ascend. Held-only — sticky navigates by click.
    pub fn update(&mut self, dt: f32, ctx: &RadialCtx) {
        if !self.open {
            return;
        }
        if self.held {
            self.held_secs += dt;
        } else {
            return;
        }
        let r = (self.ptr.0 * self.ptr.0 + self.ptr.1 * self.ptr.1).sqrt();
        if r > INNER_R {
            self.back_armed = true;
        }
        if self.stack.len() > 1 && self.back_armed && r < INNER_R {
            self.stack.pop();
            self.ptr = (0.0, 0.0);
            self.back_armed = false;
            return;
        }
        if r >= EXPAND_R {
            let slots = menu(self.menu_id(), ctx);
            if let Some(i) = slot_at(self.ptr, slots.len()) {
                if let Target::Menu(m) = slots[i].target {
                    if slots[i].enabled {
                        self.stack.push(m);
                        self.ptr = (0.0, 0.0);
                        self.back_armed = false;
                    }
                }
            }
        }
    }

    /// MMB released. Commits the hovered slot, or — on a quick tap in the dead zone
    /// — leaves the ring up as a sticky menu.
    pub fn release(&mut self, ctx: &RadialCtx) -> (Option<EditorAction>, LockRequest) {
        if !self.held {
            return (None, LockRequest::None);
        }
        self.held = false;
        let r = (self.ptr.0 * self.ptr.0 + self.ptr.1 * self.ptr.1).sqrt();
        if self.held_secs < TAP_SECS && r < INNER_R {
            self.sticky = true;
            self.ptr = (0.0, 0.0);
            return (None, LockRequest::Free);
        }
        let action = self.commit(ctx);
        if self.open {
            // Still up — the flick landed on a submenu, or on a repeating slot. Hand
            // it the real cursor so it can be read and clicked.
            self.sticky = true;
            (action, LockRequest::Free)
        } else {
            (action, LockRequest::Restore)
        }
    }

    /// A click while sticky.
    pub fn click(&mut self, ctx: &RadialCtx) -> (Option<EditorAction>, LockRequest) {
        if !self.sticky {
            return (None, LockRequest::None);
        }
        let action = self.commit(ctx);
        let lock = if self.open {
            LockRequest::None
        } else {
            LockRequest::Restore
        };
        (action, lock)
    }

    /// Commit whatever the pointer is over. Inside the dead zone this goes back up a
    /// level, or closes at the root.
    fn commit(&mut self, ctx: &RadialCtx) -> Option<EditorAction> {
        let slots = menu(self.menu_id(), ctx);
        let Some(i) = slot_at(self.ptr, slots.len()) else {
            if self.stack.len() > 1 {
                self.stack.pop();
                if !self.sticky {
                    self.ptr = (0.0, 0.0);
                }
                self.back_armed = false;
            } else {
                self.close();
            }
            return None;
        };
        let slot = &slots[i];
        if !slot.enabled {
            return None;
        }
        match slot.target {
            Target::Menu(m) => {
                self.stack.push(m);
                if !self.sticky {
                    self.ptr = (0.0, 0.0);
                }
                self.back_armed = false;
                None
            }
            Target::Act(a) => {
                if !slot.repeat {
                    self.close();
                }
                Some(a)
            }
        }
    }

    /// A frame's worth of ring for the painter, or `None` when closed.
    ///
    /// Snapshotted rather than handing the painter a `&Radial` + `&RadialCtx`,
    /// because the egui closure in `build_egui_frame` runs under a live borrow of
    /// `self.egui_state` — everything it reads is gathered before it opens.
    pub fn view(&self, ctx: &RadialCtx) -> Option<RadialView> {
        if !self.open {
            return None;
        }
        let slots = menu(self.menu_id(), ctx);
        let hovered = slot_at(self.ptr, slots.len()).filter(|&i| slots[i].enabled);
        Some(RadialView {
            origin: self.origin,
            ptr: self.ptr,
            hovered,
            title: menu_title(self.menu_id()),
            sticky: self.sticky,
            can_back: self.stack.len() > 1,
            slots,
        })
    }

    /// Close without acting (Esc, right-click, or a cancel commit).
    pub fn close(&mut self) {
        self.open = false;
        self.held = false;
        self.sticky = false;
        self.held_secs = 0.0;
        self.ptr = (0.0, 0.0);
        self.stack.clear();
        self.back_armed = false;
    }
}
