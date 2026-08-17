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

pub mod arsenal;
pub mod attack_anim;
pub mod config;
pub mod enemy_weapons;
pub mod explosives;
pub mod gun_strip;
pub mod hit_anim;
pub mod pd_weapons;
pub mod shooting;
pub mod viewmodel;

pub use arsenal::Arsenal;
pub use config::{
    Explosion, FireKind, MineSpec, MineTrigger, ProjectileSpec, SecondaryFire, WeaponStats,
};
pub use enemy_weapons::{
    enemy_def_for, standoff_for, EnemySecondary, EnemyWeaponClass, EnemyWeaponDef,
};
pub use explosives::{falloff_damage, Mine, Projectile};
pub use shooting::{cast, HitResult};
pub use viewmodel::{load_flash, load_gun, ViewModel};

/// Muzzle-flash visible duration in seconds (JS `WeaponViewmodel.playMuzzleFlash`
/// sets `flashTimer = 0.12`).
const MUZZLE_FLASH_TIME: f32 = 0.12;

/// Pause (s) after the magazine empties from firing before the auto-reload kicks
/// in (JS `reloadDelayTimer = 0.5` in `WeaponSystem.fire`). Also blocks a manual
/// reload during the window, so the empty *click* reads distinctly.
const RELOAD_DELAY: f32 = 0.5;

/// Starting reserve ammo = `magazine_size × this` (JS `Game.ts`:
/// `reserveAmmo: w.magazineSize * 10`).
const RESERVE_MULTIPLIER: u32 = 10;

/// Fixed one-shot volumes (linear amplitude gain), mirroring the JS
/// `WeaponSystem` play-sites: fire `0.6`, reload `0.7`, empty `0.5`.
const FIRE_VOL: f32 = 0.6;
const RELOAD_VOL: f32 = 0.7;
const EMPTY_VOL: f32 = 0.5;

/// A queued sound to play this frame: an asset-relative name + a linear amplitude
/// volume. The [`Weapon`] stays audio-free (headless-testable) and instead queues
/// these; the game layer (`world::combat`) drains them and plays them through
/// `engine::audio`. Mirrors the JS `audio.play(url, volume)` call arguments.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SoundCue {
    pub name: &'static str,
    pub volume: f32,
}

/// The runtime weapon: its config, the view-space [`ViewModel`], fire timing, and
/// the ammo/reload state machine. Orchestrator port of `src/weapons/WeaponSystem.ts`
/// (minus audio + rendering, which the renderer owns). Recoil lands on [`ViewModel`].
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
    /// Rounds in the magazine (JS `WeaponSlot.magazineAmmo`).
    magazine: u32,
    /// Rounds held in reserve to reload from (JS `WeaponSlot.reserveAmmo`).
    reserve: u32,
    /// A reload is in progress; firing is blocked until it finishes (JS `reloading`).
    reloading: bool,
    /// Countdown (s) of the active reload (JS `reloadTimer`).
    reload_timer: f32,
    /// Post-fire delay countdown (s) before the empty auto-reload starts
    /// (JS `reloadDelayTimer`); also gates manual reload while >0.
    reload_delay_timer: f32,
    /// Sound cues queued this frame by [`Self::fire`]/[`Self::start_reload`]/the
    /// empty-click branch, drained by the game layer via [`Self::take_cues`]. Keeps
    /// the fire model audio-free so it stays headless-testable.
    cues: Vec<SoundCue>,
    /// Whether this weapon's **secondary** function is the active one — Perfect
    /// Dark's `gunfuncs[]` bit, remembered per weapon rather than held down.
    ///
    /// Lives on the `Weapon` (and the `World` keeps one `Weapon` per arsenal
    /// entry), so it persists across weapon switches exactly as PD's per-weapon
    /// bit does: put the SuperDragon on grenades, cycle away, come back, and it is
    /// still on grenades. Always `false` for a GoldenEye weapon, which has no
    /// second function to select.
    secondary: bool,
    /// The second ammo pool, for a secondary function with `ammo_index == 1`.
    /// Separate from [`Self::magazine`] because PD gives those functions their own
    /// `ammodef` — the SuperDragon's 6 grenades are not rifle rounds. Unused (and
    /// zero) for every other weapon.
    magazine2: u32,
    reserve2: u32,
}

impl Weapon {
    pub fn new(config: WeaponStats) -> Self {
        let mag2 = config
            .secondary
            .filter(|s| s.ammo_index == 1)
            .map(|s| s.magazine_size)
            .unwrap_or(0);
        Weapon {
            view: ViewModel::new(config),
            game_time: 0.0,
            last_fire_time: f32::NEG_INFINITY,
            prev_trigger: false,
            flash_timer: 0.0,
            magazine: config.magazine_size,
            reserve: config.magazine_size * RESERVE_MULTIPLIER,
            reloading: false,
            reload_timer: 0.0,
            reload_delay_timer: 0.0,
            cues: Vec::new(),
            secondary: false,
            magazine2: mag2,
            reserve2: mag2 * RESERVE_MULTIPLIER,
        }
    }

    // ── Firing function (Perfect Dark's `functions[2]`) ─────────────────────

    /// Whether this weapon has a second firing function to switch to.
    pub fn has_secondary(&self) -> bool {
        self.config().secondary.is_some()
    }

    /// Whether the secondary function is currently selected.
    pub fn is_secondary(&self) -> bool {
        self.secondary && self.has_secondary()
    }

    /// The active function's authored label, or `None` on the primary. Drives the
    /// HUD, so the player can tell which mode a gun is in without firing it.
    pub fn function_label(&self) -> Option<&'static str> {
        if self.is_secondary() {
            self.config().secondary.map(|s| s.label)
        } else {
            None
        }
    }

    /// Toggle between the primary and secondary function, PD-style.
    ///
    /// A no-op (returning `false`) on a weapon with one function, so the key does
    /// nothing rather than something surprising. Cancels any in-progress reload —
    /// PD plays a `pritosec_animation` here, and letting a reload complete *through*
    /// a mode switch would top up the wrong magazine.
    pub fn toggle_function(&mut self) -> bool {
        if !self.has_secondary() {
            return false;
        }
        self.secondary = !self.secondary;
        self.cancel_reload();
        // Re-arm the cadence so switching modes is not a way to skip a cooldown.
        self.last_fire_time = self.game_time;
        true
    }

    /// Seconds between shots for the active function.
    fn active_cooldown(&self) -> f32 {
        match self.config().secondary {
            Some(s) if self.secondary => s.fire_cooldown,
            _ => self.config().fire_cooldown,
        }
    }

    /// Whether the active function is full-auto.
    fn active_automatic(&self) -> bool {
        match self.config().secondary {
            Some(s) if self.secondary => s.automatic,
            _ => self.config().automatic,
        }
    }

    /// Damage per hit for the active function.
    pub fn active_damage(&self) -> f32 {
        match self.config().secondary {
            Some(s) if self.secondary => s.damage,
            _ => self.config().damage,
        }
    }

    /// Effective range for the active function.
    pub fn active_range(&self) -> f32 {
        match self.config().secondary {
            Some(s) if self.secondary => s.range,
            _ => self.config().range,
        }
    }

    /// How the active function delivers its damage.
    pub fn active_fire_kind(&self) -> FireKind {
        match self.config().secondary {
            Some(s) if self.secondary => s.fire_kind,
            _ => self.config().fire_kind,
        }
    }

    /// The fire sound for the active function (a silenced secondary sounds
    /// silenced).
    fn active_fire_sound(&self) -> &'static str {
        match self.config().secondary {
            Some(s) if self.secondary => s.fire_sound,
            _ => self.config().fire_sound,
        }
    }

    /// `funcdef.ammoindex` for the active function: `0` the shared magazine, `1`
    /// the second pool, `-1` free.
    fn active_ammo_index(&self) -> i8 {
        match self.config().secondary {
            Some(s) if self.secondary => s.ammo_index,
            _ => 0,
        }
    }

    /// Rounds available to the active function. A function that consumes no ammo
    /// (`ammo_index == -1`, the melee ones) always reports one, so the shared fire
    /// gate treats it as loaded without a special case.
    fn active_magazine(&self) -> u32 {
        match self.active_ammo_index() {
            -1 => 1,
            1 => self.magazine2,
            _ => self.magazine,
        }
    }

    /// Drain the sound cues queued since the last call (fire/reload/empty). The
    /// game layer plays these through `engine::audio` each frame; the weapon itself
    /// never touches audio hardware.
    pub fn take_cues(&mut self) -> Vec<SoundCue> {
        std::mem::take(&mut self.cues)
    }

    pub fn config(&self) -> &WeaponStats {
        &self.view.config
    }

    /// Rounds currently in the magazine (for the HUD ammo counter) — of the pool
    /// the **active** function draws from, so the HUD reads the ammo you are about
    /// to spend. A melee function has no magazine and reports 0.
    pub fn magazine(&self) -> u32 {
        match self.active_ammo_index() {
            -1 => 0,
            1 => self.magazine2,
            _ => self.magazine,
        }
    }

    /// Rounds held in reserve (for the HUD ammo counter), of the active pool.
    pub fn reserve(&self) -> u32 {
        match self.active_ammo_index() {
            -1 => 0,
            1 => self.reserve2,
            _ => self.reserve,
        }
    }

    /// Add `rounds` to the reserve pool (a shop ammo purchase). Saturates.
    pub fn add_reserve(&mut self, rounds: u32) {
        self.reserve = self.reserve.saturating_add(rounds);
    }

    /// Whether a reload is currently in progress (drives the HUD "RELOADING" text).
    pub fn is_reloading(&self) -> bool {
        self.reloading
    }

    /// Advance the weapon one frame and decide whether it fires. `trigger` = left
    /// mouse held this frame. Returns `true` on the frame a shot leaves the barrel.
    /// Also runs the reload timers, decays the viewmodel recoil, and fires its kick
    /// on a shot. Port of the fire/reload block of `WeaponSystem.update`.
    ///
    /// Fire model (matching GoldenEye feel):
    /// - **Automatic** weapons fire while the trigger is held, gated by
    ///   `fire_cooldown` (the sustained rate).
    /// - **Semi-auto** weapons fire on every fresh trigger **edge** with *no*
    ///   cooldown — one shot per pull, so you fire as fast as you can click (real
    ///   GoldenEye pistols are trigger-pull limited, not rate-capped). `fire_cooldown`
    ///   is the auto rate only; it does NOT throttle deliberate clicks.
    ///
    /// Ammo model: firing needs a round in the magazine and no reload in progress.
    /// Emptying the magazine (from firing) arms a [`RELOAD_DELAY`] pause, then
    /// auto-reloads if reserve remains. Pulling the trigger on an already-empty gun
    /// also auto-reloads. Manual reload is [`Self::request_reload`].
    pub fn update(&mut self, dt: f32, trigger: bool) -> bool {
        self.game_time += dt;
        if self.flash_timer > 0.0 {
            self.flash_timer = (self.flash_timer - dt).max(0.0);
        }
        self.view.tick(dt);

        // Active reload finishing.
        if self.reloading {
            self.reload_timer -= dt;
            if self.reload_timer <= 0.0 {
                self.finish_reload();
            }
        }

        // Post-fire delay elapsing → auto-reload the emptied magazine.
        if self.reload_delay_timer > 0.0 {
            self.reload_delay_timer -= dt;
            if self.reload_delay_timer <= 0.0
                && self.magazine == 0
                && self.reserve > 0
                && !self.reloading
            {
                self.start_reload();
            }
        }

        let edge = trigger && !self.prev_trigger;
        self.prev_trigger = trigger;

        let mut fired = false;
        if !self.reloading {
            // Fire readiness: semi = a fresh edge (no cooldown, our deliberate
            // GoldenEye-trigger-pull deviation); auto = held + cooldown elapsed.
            let fire_ready = if self.active_automatic() {
                trigger && self.game_time - self.last_fire_time >= self.active_cooldown()
            } else {
                edge
            };
            if self.active_magazine() > 0 && fire_ready {
                self.fire();
                fired = true;
            } else if self.active_magazine() == 0 && edge {
                // Empty click: a fresh trigger pull on an empty magazine clicks.
                //
                // DEVIATION from the JS oracle (flagged): JS queued `empty` then
                // `startReload` in a branch gated on `reloadDelayTimer <= 0 &&
                // reserve > 0`, but the auto-reload in the `reload_delay_timer`
                // block above *always* wins that race the moment the delay elapses
                // (it sets `reloading` first), so the JS empty sound was effectively
                // dead code. We instead click on each fresh pull of an empty mag —
                // audible feedback whether or not a reload is pending. The reserve
                // auto-reload still runs from the delay block above, so reload
                // timing is unchanged; this only adds the click.
                self.cues.push(SoundCue {
                    name: self.config().empty_sound,
                    volume: EMPTY_VOL,
                });
            }
        }
        fired
    }

    /// Whether the muzzle flash should render this frame.
    pub fn flash_active(&self) -> bool {
        self.flash_timer > 0.0
    }

    /// Abort an in-progress reload without refilling the magazine (weapon swap —
    /// JS `cycleWeapon` sets `reloading = false`). Also clears the post-fire
    /// auto-reload delay so a holstered weapon doesn't silently top up while it's
    /// away, and resets the viewmodel dip. The ammo state (mag/reserve) is
    /// preserved, so switching back resumes exactly where you left off.
    pub fn cancel_reload(&mut self) {
        self.reloading = false;
        self.reload_timer = 0.0;
        self.reload_delay_timer = 0.0;
        self.view.cancel_reload();
    }

    /// Manual reload request (the `R` key). Starts a reload only when one isn't
    /// already running, the post-fire delay isn't active, the magazine isn't full,
    /// and there's reserve to draw from — JS `WeaponSystem.update`'s `KeyR` branch.
    pub fn request_reload(&mut self) {
        if self.reloading || self.reload_delay_timer > 0.0 {
            return;
        }
        // Whichever pool the active function uses. A no-ammo (melee) function has
        // nothing to reload.
        match self.active_ammo_index() {
            -1 => {}
            1 => {
                let cap = self.config().secondary.map(|s| s.magazine_size).unwrap_or(0);
                if self.magazine2 < cap && self.reserve2 > 0 {
                    self.start_reload();
                }
            }
            _ => {
                if self.magazine < self.config().magazine_size && self.reserve > 0 {
                    self.start_reload();
                }
            }
        }
    }

    /// Consume one round and arm the recoil/flash (JS `WeaponSystem.fire`). Emptying
    /// the magazine (with reserve left) arms the auto-reload delay.
    fn fire(&mut self) {
        self.last_fire_time = self.game_time;
        // Spend from whichever pool the ACTIVE function draws on. A melee
        // secondary (`ammo_index == -1`) spends nothing — the pistol whip does not
        // consume a bullet.
        match self.active_ammo_index() {
            -1 => {}
            1 => {
                self.magazine2 = self.magazine2.saturating_sub(1);
                if self.magazine2 == 0 && self.reserve2 > 0 {
                    self.reload_delay_timer = RELOAD_DELAY;
                }
            }
            _ => {
                self.magazine -= 1;
                if self.magazine == 0 && self.reserve > 0 {
                    self.reload_delay_timer = RELOAD_DELAY;
                }
            }
        }
        self.flash_timer = MUZZLE_FLASH_TIME;
        self.view.play_recoil();
        self.cues.push(SoundCue {
            name: self.active_fire_sound(),
            volume: FIRE_VOL,
        });
    }

    /// Begin a reload (JS `startReload`): sets the timer + plays the viewmodel dip;
    /// the refill happens in [`Self::finish_reload`] when it elapses.
    fn start_reload(&mut self) {
        self.reloading = true;
        self.reload_timer = self.config().reload_time;
        self.view.play_reload();
        self.cues.push(SoundCue {
            name: self.config().reload_sound,
            volume: RELOAD_VOL,
        });
    }

    /// Refill the magazine from reserve, capped at the magazine size and available
    /// reserve (JS `finishReload`).
    fn finish_reload(&mut self) {
        // Reload the pool the ACTIVE function feeds from, so switching the
        // SuperDragon to grenades and reloading tops up grenades, not rifle rounds.
        if self.active_ammo_index() == 1 {
            let cap = self
                .config()
                .secondary
                .map(|s| s.magazine_size)
                .unwrap_or(0);
            let needed = cap.saturating_sub(self.magazine2);
            let to_load = needed.min(self.reserve2);
            self.magazine2 += to_load;
            self.reserve2 -= to_load;
        } else {
            let needed = self.config().magazine_size - self.magazine;
            let to_load = needed.min(self.reserve);
            self.magazine += to_load;
            self.reserve -= to_load;
        }
        self.reloading = false;
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

    /// Firing queues exactly one fire cue (the right sound + JS's 0.6 volume), and
    /// draining clears it so the next frame starts empty.
    #[test]
    fn firing_queues_a_fire_cue() {
        let mut w = Weapon::new(config::PP7);
        w.update(0.016, true);
        let cues = w.take_cues();
        assert_eq!(
            cues,
            vec![SoundCue {
                name: config::PP7.fire_sound,
                volume: FIRE_VOL
            }],
            "one fire cue at the fire volume"
        );
        assert!(w.take_cues().is_empty(), "cues drained");
    }

    /// A manual reload queues a reload cue (the shared reload sound + 0.7 volume).
    #[test]
    fn reload_queues_a_reload_cue() {
        let mut w = Weapon::new(config::PP7);
        // Spend a round so a reload is allowed, and clear the fire cue it queued.
        w.update(0.016, true);
        w.take_cues();
        w.request_reload();
        assert_eq!(
            w.take_cues(),
            vec![SoundCue {
                name: config::PP7.reload_sound,
                volume: RELOAD_VOL
            }],
            "manual reload queues the reload cue"
        );
    }

    /// A fresh trigger pull on an empty magazine queues the empty-click sound (and
    /// only that — no fire, no accompanying reload cue on the click itself).
    #[test]
    fn empty_click_queues_the_empty_sound() {
        let mut w = Weapon::new(config::PP7); // mag 7
        // Drain the magazine (release between pulls for a fresh edge each shot).
        for _ in 0..7 {
            w.update(0.016, true);
            w.update(0.016, false);
        }
        assert_eq!(w.magazine(), 0, "magazine emptied");
        w.take_cues(); // discard the 7 fire cues
        // The post-fire delay is still counting (well under 0.5 s elapsed), so no
        // auto-reload has started — a fresh pull is a clean empty click.
        w.update(0.016, true);
        assert_eq!(
            w.take_cues(),
            vec![SoundCue {
                name: config::PP7.empty_sound,
                volume: EMPTY_VOL
            }],
            "a dry pull queues exactly the empty-click sound"
        );
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
    /// trigger-pull limited). Rapid release/pull each land a shot, up to the
    /// magazine capacity.
    #[test]
    fn semi_auto_fires_as_fast_as_you_click() {
        let mut w = Weapon::new(config::PP7); // fire_cooldown 0.4s, mag 7
        let mut shots = 0;
        // 7 rapid click cycles (down,up), each ~2 frames ≈ 0.03s ≪ 0.4s cooldown.
        for _ in 0..7 {
            if w.update(0.016, true) {
                shots += 1;
            }
            w.update(0.016, false);
        }
        assert_eq!(shots, 7, "every deliberate click fires (a full mag), cooldown notwithstanding");
    }

    /// Automatic weapons DO auto-fire while held, spaced by `fire_cooldown`.
    #[test]
    fn automatic_is_cooldown_spaced() {
        let mut w = Weapon::new(auto()); // fire_cooldown 0.4s, mag 7
        let mut shots = 0;
        // Hold for ~2.4s (150 × 0.016). Cooldown allows ~6 shots (0,0.4,…,2.0) but
        // the 7-round mag is the real cap; then it empties + auto-reloads.
        for _ in 0..150 {
            if w.update(0.016, true) {
                shots += 1;
            }
        }
        assert!((5..=7).contains(&shots), "auto fire spaced by cooldown, capped by mag: {shots} shots");
    }

    /// Firing decrements the magazine one round at a time.
    #[test]
    fn firing_decrements_the_magazine() {
        let mut w = Weapon::new(config::PP7);
        assert_eq!(w.magazine(), 7);
        assert_eq!(w.reserve(), 70);
        for expect in (0..7).rev() {
            assert!(w.update(0.016, true), "shot fires while ammo remains");
            w.update(0.016, false); // release for the next edge
            assert_eq!(w.magazine(), expect, "one round spent per shot");
        }
    }

    /// An empty magazine blocks firing; the shot count never exceeds capacity even
    /// under sustained clicking.
    #[test]
    fn empty_magazine_blocks_firing() {
        let mut w = Weapon::new(config::PP7); // mag 7
        let mut shots = 0;
        for _ in 0..20 {
            if w.update(0.016, true) {
                shots += 1;
            }
            w.update(0.016, false);
        }
        assert_eq!(shots, 7, "an empty magazine stops firing");
        assert_eq!(w.magazine(), 0, "magazine emptied");
    }

    /// A manual reload refills the magazine from reserve over `reload_time`, and
    /// firing is blocked while it runs.
    #[test]
    fn manual_reload_refills_after_reload_time() {
        let mut w = Weapon::new(config::PP7); // mag 7, reload 0.75s
        // Spend 3 rounds.
        for _ in 0..3 {
            w.update(0.016, true);
            w.update(0.016, false);
        }
        assert_eq!(w.magazine(), 4);
        w.request_reload();
        assert!(w.is_reloading(), "reload starts");
        // Firing is blocked mid-reload.
        assert!(!w.update(0.016, true), "cannot fire while reloading");
        w.update(0.016, false);
        // Advance past reload_time (1.5s).
        for _ in 0..100 {
            w.update(0.016, false);
        }
        assert!(!w.is_reloading(), "reload finished");
        assert_eq!(w.magazine(), 7, "magazine topped up");
        assert_eq!(w.reserve(), 70 - 3, "reserve drew the 3 rounds loaded");
    }

    // ── Perfect Dark's two firing functions ─────────────────────────────────

    /// A GoldenEye weapon has one function, so the toggle is inert rather than
    /// surprising.
    #[test]
    fn a_single_function_weapon_cannot_switch() {
        let mut w = Weapon::new(config::PP7);
        assert!(!w.has_secondary());
        assert!(!w.toggle_function(), "nothing to switch to");
        assert!(!w.is_secondary());
        assert_eq!(w.function_label(), None);
    }

    /// A PD weapon switches, reports its authored label, and switches back.
    #[test]
    fn a_pd_weapon_toggles_between_its_two_functions() {
        let falcon = *arsenal::Arsenal::PerfectDark
            .weapons()
            .iter()
            .find(|w| w.name == "Falcon 2")
            .unwrap();
        let mut w = Weapon::new(falcon);
        assert!(w.has_secondary());
        assert_eq!(w.function_label(), None, "starts on the primary");

        assert!(w.toggle_function());
        assert!(w.is_secondary());
        // PD's own label for the Falcon 2's second function.
        assert_eq!(w.function_label(), Some("Pistol Whip"));

        assert!(w.toggle_function());
        assert!(!w.is_secondary(), "toggles back");
    }

    /// The active function drives damage, reach and delivery — not just a label.
    #[test]
    fn the_active_function_drives_the_shot() {
        let falcon = *arsenal::Arsenal::PerfectDark
            .weapons()
            .iter()
            .find(|w| w.name == "Falcon 2")
            .unwrap();
        let mut w = Weapon::new(falcon);
        let (pri_dmg, pri_range) = (w.active_damage(), w.active_range());
        w.toggle_function();
        // The pistol whip is a melee function: it reaches barely any distance
        // compared to the gun, which is the point of it being a separate function.
        assert!(
            w.active_range() < pri_range * 0.5,
            "the whip ({}) should be far shorter than the shot ({pri_range})",
            w.active_range()
        );
        assert!(w.active_damage() > 0.0 && pri_dmg > 0.0);
    }

    /// A melee secondary consumes no ammo (`funcdef.ammoindex == -1`) — you can
    /// pistol-whip with an empty gun, which is exactly when you would want to.
    #[test]
    fn a_melee_secondary_costs_no_ammo() {
        let falcon = *arsenal::Arsenal::PerfectDark
            .weapons()
            .iter()
            .find(|w| w.name == "Falcon 2")
            .unwrap();
        let mut w = Weapon::new(falcon);
        // Empty the magazine on the primary.
        for _ in 0..falcon.magazine_size {
            w.update(0.016, true);
            w.update(0.016, false);
        }
        assert_eq!(w.magazine(), 0, "gun is dry");
        w.toggle_function();
        w.take_cues();
        let mut hits = 0;
        for _ in 0..5 {
            if w.update(0.016, true) {
                hits += 1;
            }
            w.update(0.016, false);
        }
        assert_eq!(hits, 5, "the whip works on an empty gun");
    }

    /// The SuperDragon is the MP set's one weapon whose secondary has its **own**
    /// ammo pool (`ammoindex == 1`, a 6-round grenade `ammodef`). Firing grenades
    /// must not eat rifle rounds, and the HUD must read the pool in use.
    #[test]
    fn a_secondary_with_its_own_pool_spends_that_pool() {
        let dragon = *arsenal::Arsenal::PerfectDark
            .weapons()
            .iter()
            .find(|w| w.name == "SuperDragon")
            .unwrap();
        let sec = dragon.secondary.expect("a grenade launcher");
        assert_eq!(sec.ammo_index, 1, "PD gives it its own ammodef");
        assert_eq!(sec.magazine_size, 6, "6 grenades");

        let mut w = Weapon::new(dragon);
        let rifle_rounds = w.magazine();
        w.toggle_function();
        assert_eq!(w.magazine(), 6, "the HUD shows grenades once switched");

        w.update(0.016, true);
        w.update(0.016, false);
        assert_eq!(w.magazine(), 5, "a grenade was spent");

        w.toggle_function();
        assert_eq!(w.magazine(), rifle_rounds, "the rifle magazine is untouched");
    }

    /// Reloading tops up the pool the active function feeds from.
    #[test]
    fn reloading_fills_the_active_pool() {
        let dragon = *arsenal::Arsenal::PerfectDark
            .weapons()
            .iter()
            .find(|w| w.name == "SuperDragon")
            .unwrap();
        let mut w = Weapon::new(dragon);
        w.toggle_function(); // grenades
        for _ in 0..3 {
            w.update(0.016, true);
            w.update(0.016, false);
        }
        assert_eq!(w.magazine(), 3, "three grenades spent");
        let rifle_before = {
            w.toggle_function();
            let m = w.magazine();
            w.toggle_function();
            m
        };
        w.request_reload();
        assert!(w.is_reloading());
        for _ in 0..400 {
            w.update(0.016, false);
        }
        assert_eq!(w.magazine(), 6, "grenades refilled");
        w.toggle_function();
        assert_eq!(w.magazine(), rifle_before, "the rifle pool never moved");
    }

    /// Switching function cancels a running reload rather than letting it complete
    /// into the other magazine, and does not hand out a free shot by resetting the
    /// cadence backwards.
    #[test]
    fn switching_function_cancels_a_reload_and_does_not_skip_the_cooldown() {
        let dragon = *arsenal::Arsenal::PerfectDark
            .weapons()
            .iter()
            .find(|w| w.name == "SuperDragon")
            .unwrap();
        let mut w = Weapon::new(dragon);
        // Spend a round so a reload is legal, then start one.
        w.update(0.016, true);
        w.update(0.016, false);
        // Clear the post-fire delay so the manual reload is allowed.
        for _ in 0..40 {
            w.update(0.016, false);
        }
        w.request_reload();
        assert!(w.is_reloading(), "a reload is running");
        w.toggle_function();
        assert!(!w.is_reloading(), "the switch cancelled it");

        // The primary is automatic, so an immediate held trigger must still wait
        // out the cadence rather than firing on the switch frame.
        w.toggle_function(); // back to the automatic primary
        assert!(!w.update(0.001, true), "no free shot on the switch frame");
    }

    /// Emptying the magazine auto-reloads after the post-fire delay elapses,
    /// without pressing R.
    #[test]
    fn empty_magazine_auto_reloads_after_delay() {
        let mut w = Weapon::new(config::PP7); // mag 7
        // Fire the mag dry.
        for _ in 0..7 {
            w.update(0.016, true);
            w.update(0.016, false);
        }
        assert_eq!(w.magazine(), 0);
        assert!(!w.is_reloading(), "delay not elapsed yet");
        // Idle past the 0.5s delay → auto-reload starts, then past 1.5s → finishes.
        for _ in 0..200 {
            w.update(0.016, false);
        }
        assert!(!w.is_reloading(), "auto-reload completed");
        assert_eq!(w.magazine(), 7, "magazine refilled on empty auto-reload");
    }
}
