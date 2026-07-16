//! Player Combat track: first-person weapons, firing/hitscan, ammo/reload,
//! recoil, and the HUD-facing weapon state. Transliterated from the 3DS FPS
//! `src/weapons/*` + `src/player/*` (the read-only spec/oracle), keeping its
//! tuning so the feel ports 1:1.
//!
//! Combat is a HUNT-phase feature — BUILD stays the fly-cam editor. The `World`
//! owns a [`Weapon`] and drives it (`world::combat`) only while in HUNT.
//!
//! Milestone map (see the memory log's "Player Combat" section):
//! - P1 — [`viewmodel`]: the view-space gun, rendered depth-cleared on top.
//! - P2 — [`shooting`] hitscan + muzzle flash + hit spark.
//! - P3 — ammo/reload state machine.
//! - P4 — recoil + bob/sway.
//! - P5 — player health + the GoldenEye radial-arc HUD.

pub mod config;
pub mod shooting;
pub mod viewmodel;

pub use config::WeaponStats;
pub use shooting::{cast, HitResult};
pub use viewmodel::{load_flash, load_gun, GunModel, GunPrimitive, ViewModel};

/// Muzzle-flash visible duration in seconds (JS `WeaponViewmodel.playMuzzleFlash`
/// sets `flashTimer = 0.12`).
const MUZZLE_FLASH_TIME: f32 = 0.12;

/// The runtime weapon: its config, the view-space [`ViewModel`], and fire timing.
/// Orchestrator port of `src/weapons/WeaponSystem.ts` (minus audio + rendering,
/// which the renderer owns). Ammo/reload land on this struct at P3, recoil at P4.
pub struct Weapon {
    pub view: ViewModel,
    /// Accumulated game time (s) — the clock `fire_cooldown` is measured against
    /// (JS `gameTime`). Advancing on real per-frame dt makes the fire rate
    /// frame-rate independent (the cooldown is wall-clock elapsed time).
    game_time: f32,
    /// `game_time` of the last shot (JS `lastFireTime`); −∞ so the first shot
    /// always fires.
    last_fire_time: f32,
    /// Left-trigger held state last frame, for semi-auto edge detection.
    prev_trigger: bool,
    /// Muzzle-flash countdown (s); >0 → the flash renders (JS `flashTimer`).
    flash_timer: f32,
}

impl Weapon {
    pub fn new(config: WeaponStats) -> Self {
        Weapon {
            view: ViewModel::new(config),
            game_time: 0.0,
            last_fire_time: f32::NEG_INFINITY,
            prev_trigger: false,
            flash_timer: 0.0,
        }
    }

    pub fn config(&self) -> &WeaponStats {
        &self.view.config
    }

    /// Advance the weapon one frame and decide whether it fires. `trigger` = left
    /// mouse held this frame. Returns `true` on the frame a shot leaves the barrel.
    /// Also decays the viewmodel recoil and fires its kick on a shot.
    ///
    /// Fire model (matching GoldenEye feel):
    /// - **Automatic** weapons fire while the trigger is held, gated by
    ///   `fire_cooldown` (the sustained rate).
    /// - **Semi-auto** weapons fire on every fresh trigger **edge** with *no*
    ///   cooldown — one shot per pull, so you fire as fast as you can click (real
    ///   GoldenEye pistols are trigger-pull limited, not rate-capped). `fire_cooldown`
    ///   is the auto rate only; it does NOT throttle deliberate clicks.
    ///
    /// (Ammo gating is added at P3; today the weapon has unlimited ammo.)
    pub fn update(&mut self, dt: f32, trigger: bool) -> bool {
        self.game_time += dt;
        if self.flash_timer > 0.0 {
            self.flash_timer = (self.flash_timer - dt).max(0.0);
        }
        self.view.tick_recoil(dt);

        let fired = if self.config().automatic {
            trigger && self.game_time - self.last_fire_time >= self.config().fire_cooldown
        } else {
            trigger && !self.prev_trigger
        };
        if fired {
            self.last_fire_time = self.game_time;
            self.flash_timer = MUZZLE_FLASH_TIME;
            self.view.play_recoil();
        }
        self.prev_trigger = trigger;
        fired
    }

    /// Whether the muzzle flash should render this frame.
    pub fn flash_active(&self) -> bool {
        self.flash_timer > 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test-only automatic variant of the PP7 (WeaponStats is Copy).
    fn auto() -> WeaponStats {
        let mut w = config::PP7;
        w.automatic = true;
        w
    }

    /// A fresh semi-auto weapon fires on the first triggered frame + arms recoil.
    #[test]
    fn first_pull_fires() {
        let mut w = Weapon::new(config::PP7);
        assert!(w.update(0.016, true), "first trigger pull fires");
        assert!(w.flash_active(), "flash armed on fire");
    }

    /// Semi-auto is edge-triggered: holding the trigger fires exactly once — you
    /// must release + re-pull to fire again.
    #[test]
    fn semi_auto_is_edge_triggered() {
        let mut w = Weapon::new(config::PP7);
        assert!(w.update(0.016, true), "shot 1 on the edge");
        let mut shots = 0;
        for _ in 0..100 {
            if w.update(0.016, true) {
                shots += 1;
            }
        }
        assert_eq!(shots, 0, "held trigger does not auto-fire a semi weapon");
        w.update(0.016, false); // release
        assert!(w.update(0.016, true), "re-pull fires");
    }

    /// Semi-auto fires as fast as you click — NO cooldown between deliberate
    /// pulls, even ones far tighter than `fire_cooldown` (GoldenEye pistols are
    /// trigger-pull limited). Rapid release/pull each land a shot.
    #[test]
    fn semi_auto_fires_as_fast_as_you_click() {
        let mut w = Weapon::new(config::PP7); // fire_cooldown 0.4s
        let mut shots = 0;
        // 10 rapid click cycles (down,up), each ~2 frames ≈ 0.03s ≪ 0.4s cooldown.
        for _ in 0..10 {
            if w.update(0.016, true) {
                shots += 1;
            }
            w.update(0.016, false);
        }
        assert_eq!(shots, 10, "every deliberate click fires, cooldown notwithstanding");
    }

    /// Automatic weapons DO auto-fire while held, spaced by `fire_cooldown`.
    #[test]
    fn automatic_is_cooldown_spaced() {
        let mut w = Weapon::new(auto()); // fire_cooldown 0.4s
        let mut shots = 0;
        // Hold for ~1.6s (100 × 0.016). Expect shots at ~0, 0.4, 0.8, 1.2, 1.6.
        for _ in 0..100 {
            if w.update(0.016, true) {
                shots += 1;
            }
        }
        assert!((3..=6).contains(&shots), "auto fire spaced by cooldown: {shots} shots");
    }
}
