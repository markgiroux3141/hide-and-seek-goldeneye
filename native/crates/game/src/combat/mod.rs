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
}

impl Weapon {
    pub fn new(config: WeaponStats) -> Self {
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

    /// Rounds currently in the magazine (for the HUD ammo counter).
    pub fn magazine(&self) -> u32 {
        self.magazine
    }

    /// Rounds held in reserve (for the HUD ammo counter).
    pub fn reserve(&self) -> u32 {
        self.reserve
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
            let fire_ready = if self.config().automatic {
                trigger && self.game_time - self.last_fire_time >= self.config().fire_cooldown
            } else {
                edge
            };
            if self.magazine > 0 && fire_ready {
                self.fire();
                fired = true;
            } else if self.magazine == 0 && edge {
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
        if !self.reloading
            && self.reload_delay_timer <= 0.0
            && self.magazine < self.config().magazine_size
            && self.reserve > 0
        {
            self.start_reload();
        }
    }

    /// Consume one round and arm the recoil/flash (JS `WeaponSystem.fire`). Emptying
    /// the magazine (with reserve left) arms the auto-reload delay.
    fn fire(&mut self) {
        self.last_fire_time = self.game_time;
        self.magazine -= 1;
        if self.magazine == 0 && self.reserve > 0 {
            self.reload_delay_timer = RELOAD_DELAY;
        }
        self.flash_timer = MUZZLE_FLASH_TIME;
        self.view.play_recoil();
        self.cues.push(SoundCue {
            name: self.config().fire_sound,
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
        let needed = self.config().magazine_size - self.magazine;
        let to_load = needed.min(self.reserve);
        self.magazine += to_load;
        self.reserve -= to_load;
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
