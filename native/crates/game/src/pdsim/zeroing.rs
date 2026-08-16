//! Perfect Dark's **zeroing** aim model — the reason simulants feel human.
//!
//! Ported from `bot_update_zero_angle` (`pd-decomp/src/game/bot.c:1440`) and the
//! aim application in `bot_tick` (`bot.c:955-1050`).
//!
//! # The idea
//!
//! Simulants never roll a hit chance. They aim a real weapon in world space and
//! fire genuine hitscans down the barrel; accuracy is an *emergent* property of
//! how far the body's yaw currently is from the true bearing to the target. That
//! error is [`Zeroing::angle`], and driving it to zero is called zeroing.
//!
//! Three coupled pieces produce it:
//!
//! 1. **A progress timer** (`zero_timer`) that fills while the target is in sight
//!    and drains while it is not. Its fraction-remaining scales how large the aim
//!    error is allowed to get, so a bot that has been staring at you for a while
//!    is accurate and one that just whipped around a corner is not.
//! 2. **A damped random walk** driving the error itself. Each tick an increment
//!    (`inc`) is picked randomly between the scaled min and max convergence
//!    speeds, given a **random sign**, and fed into a leaky accumulator. The sign
//!    is what makes the aim *wander across* the target rather than creep onto it
//!    from one side — bots overshoot and correct like a person does.
//! 3. **Turn feedback.** Turning the body naturally un-zeroes the aim, scaled by
//!    `turn_unzero_mult`. Since correcting the aim error *is* turning, a bot that
//!    is badly off-target fights its own correction. This is the single most
//!    characterful part of the model, and it is why low-tier sims are helpless
//!    against a target that keeps making them swing.
//!
//! # Frame-rate independence
//!
//! PD ran the accumulator at a fixed 240 Hz (`for i in 0..lvupdate240`), decaying
//! by 0.975 each sub-tick. We take variable `dt`, so the loop is replaced with its
//! closed form: after `n` sub-ticks with decay `a`,
//!
//! ```text
//! speed_n = speed_0 * a^n + inc * (1 - a^n) / (1 - a)
//! ```
//!
//! which is exact for integer `n` and well-behaved for fractional ones. At 60 fps
//! (`n = 4`) this reproduces the original arithmetic; the steady state is
//! `inc / (1 - a) = 40 * inc`, and since `angle = speed * 0.025`, a settled aim
//! error is almost exactly `inc` radians. That identity is what makes the table's
//! degree values readable as "how far off this tier aims": a MeatSim wanders up to
//! ~30°, a NormalSim ~8°, a PerfectSim ~2°, a DarkSim not at all.

use super::difficulty::BotTuning;

/// Per-sub-tick decay of the zero-speed accumulator (NTSC value; PAL used 0.97).
const DECAY: f32 = 0.975_000_02;
/// The accumulator ran at 240 Hz.
const SUBTICK_HZ: f32 = 240.0;
/// `zeroangle = zerospeed * this` (NTSC; PAL used 0.03).
const SPEED_TO_ANGLE: f32 = 0.024_999_976;

/// Maximum body turn rate, `tweenangle` in `bot_tick` (`bot.c:988`): 0.06159 rad
/// per 60 Hz frame → rad/s.
pub const MAX_TURN_RATE: f32 = 0.061_590_05 * 60.0;

/// How long a randomly-chosen increment is held before being re-rolled
/// (`random3ttl60`, `bot.c:1458`): 20 ticks plus up to another 20.
const INC_HOLD_MIN: f32 = 20.0 / 60.0;
const INC_HOLD_RAND: f32 = 20.0 / 60.0;

/// The aim-error state for one simulant.
///
/// Everything is in SI units. [`Self::angle`] is the live output: a signed yaw
/// error in radians to add to the true bearing before the body turns toward it.
#[derive(Clone, Debug)]
pub struct Zeroing {
    /// Zero progress (s), clamped into `[0, zero_time]`. Full = aim is on target.
    pub zero_timer: f32,
    /// Time the target has been continuously in sight (s), `shootdelaytimer60`.
    /// Reset to 0 when the target *changes*, decays (not resets) when sight breaks.
    pub shoot_delay_timer: f32,
    /// The leaky accumulator (`zerospeed`).
    pub speed: f32,
    /// The currently-held signed increment (`zeroinc`), re-rolled on `inc_ttl`.
    pub inc: f32,
    /// Seconds until `inc` is re-rolled (`random3ttl60`).
    pub inc_ttl: f32,
    /// The live signed aim error in radians (`zeroangle`) — what the caller adds
    /// to the true bearing.
    pub angle: f32,
}

impl Default for Zeroing {
    fn default() -> Self {
        Self {
            zero_timer: 0.0,
            shoot_delay_timer: 0.0,
            speed: 0.0,
            inc: 0.0,
            inc_ttl: 0.0,
            angle: 0.0,
        }
    }
}

impl Zeroing {
    /// Called when the bot switches to a different target (`bot_set_target`,
    /// `bot.c:1366`). The shoot delay must be served again from scratch.
    pub fn on_target_changed(&mut self) {
        self.shoot_delay_timer = 0.0;
    }

    /// Fully reset — respawn / disengage (`botmgr.c:229`).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Advance the shoot-delay clock (`bot_set_target`, `bot.c:1376-1391`).
    ///
    /// Note it *decays* rather than resets when sight is lost, which is what the
    /// original field note means by "a brief break in sight will have little
    /// effect" — ducking behind a pillar for a moment does not buy you a fresh
    /// reaction time.
    pub fn tick_shoot_delay(&mut self, dt: f32, target_in_sight: bool) {
        if target_in_sight {
            self.shoot_delay_timer += dt;
        } else {
            self.shoot_delay_timer = (self.shoot_delay_timer - dt).max(0.0);
        }
    }

    /// Whether the reaction delay has been served and the bot may pull the
    /// trigger (`bot.c:3605`). Note this is *not* a "fully zeroed" test — PD lets
    /// bots fire while still converging, which is exactly why they spray wide when
    /// they first spot you.
    pub fn may_shoot(&self, tuning: &BotTuning) -> bool {
        self.shoot_delay_timer >= tuning.shoot_delay
    }

    /// How zeroed the aim is, 0 (just acquired) to 1 (fully converged). Purely for
    /// display and for behaviours that want to know — the aim error itself is
    /// [`Self::angle`].
    pub fn progress(&self, tuning: &BotTuning) -> f32 {
        if tuning.zero_time <= 0.0 {
            1.0
        } else {
            (self.zero_timer / tuning.zero_time).clamp(0.0, 1.0)
        }
    }

    /// One step of `bot_update_zero_angle` (`bot.c:1440`).
    ///
    /// * `target_in_sight` — fills the zero timer when true, drains it when false.
    /// * `turn_rate` — the body's actual angular speed this step (rad/s, signed or
    ///   not; magnitude is what matters). Feed it the turn the body *actually*
    ///   made, so correcting a large error costs zero progress.
    /// * `target_cloaked` — raises the convergence floor. We have no cloak; the
    ///   parameter exists so the port stays faithful and a future invisibility
    ///   pickup can drive it.
    /// * `rand01` — a fresh uniform in `[0, 1)`, used only when the held increment
    ///   expires.
    ///
    /// Returns the new [`Self::angle`].
    pub fn update(
        &mut self,
        dt: f32,
        tuning: &BotTuning,
        target_in_sight: bool,
        turn_rate: f32,
        target_cloaked: bool,
        rand01: impl FnMut() -> f32,
    ) -> f32 {
        let mut rand01 = rand01;

        // ── Re-roll the held increment when its lifetime expires ──
        // PD holds one random value for a third to two thirds of a second rather
        // than re-rolling per tick. That hold is load-bearing: it makes the aim
        // drift steadily in one direction and then change its mind, instead of
        // dithering into a smooth average.
        self.inc_ttl -= dt;
        let reroll = self.inc_ttl <= 0.0;
        let (r_mag, r_sign) = if reroll {
            self.inc_ttl = INC_HOLD_MIN + rand01() * INC_HOLD_RAND;
            (rand01(), rand01())
        } else {
            (0.0, 0.0)
        };

        // ── Zero progress: fills in sight, drains out of sight ──
        if target_in_sight {
            self.zero_timer += dt;
        } else {
            self.zero_timer -= dt;
        }

        // Turning un-zeroes. PD works in ticks-per-frame with `speedtheta` scaled
        // so that a full-rate turn is 1.0; here that is `turn_rate / MAX_TURN_RATE`.
        let turn_frac = (turn_rate.abs() / MAX_TURN_RATE).min(1.0);
        self.zero_timer -= tuning.turn_unzero_mult * turn_frac * dt;

        // The bot must not zero faster than its reaction time allows, or it would
        // finish converging and then stand there waiting for permission to shoot
        // (`bot.c:1481`).
        self.zero_timer = self.zero_timer.min(self.shoot_delay_timer).max(0.0);

        // ── Convergence rate for this tick, scaled by how much zeroing is left ──
        let (mut min_speed, mut max_speed) = if self.zero_timer >= tuning.zero_time {
            self.zero_timer = tuning.zero_time;
            (0.0, 0.0)
        } else if tuning.zero_time <= 0.0 {
            (0.0, 0.0)
        } else {
            let frac = (tuning.zero_time - self.zero_timer) / tuning.zero_time;
            (tuning.min_zero_speed * frac, tuning.max_zero_speed * frac)
        };

        if target_cloaked {
            max_speed = max_speed.max(tuning.zero_cloak_speed);
        }
        // The floor applies unconditionally, including to a fully-zeroed bot —
        // which is why every tier below Perfect keeps a small permanent wobble.
        max_speed = max_speed.max(tuning.force_zero_min_speed);
        min_speed = min_speed.min(max_speed);

        if reroll {
            let mag = min_speed + (max_speed - min_speed) * r_mag;
            // PD negates on a coin flip (`bot.c:1521`). The comment in the
            // decompilation calls the negation "a weird choice", but it is the
            // whole reason the aim crosses the target instead of approaching it
            // from one side: without it a bot would only ever be wrong in one
            // direction and players would learn to strafe the safe way.
            self.inc = if r_sign < 0.5 { -mag } else { mag };
        } else {
            // Between re-rolls the magnitude still tracks the current bounds, so a
            // bot that finishes zeroing settles immediately rather than coasting
            // on a stale increment.
            let mag = self.inc.abs().clamp(min_speed, max_speed.max(min_speed));
            self.inc = mag.copysign(self.inc);
        }

        // ── Leaky accumulator, closed form of PD's 240 Hz sub-tick loop ──
        let n = dt * SUBTICK_HZ;
        let decay = DECAY.powf(n);
        self.speed = self.speed * decay + self.inc * (1.0 - decay) / (1.0 - DECAY);
        self.angle = self.speed * SPEED_TO_ANGLE;
        self.angle
    }
}

/// Turn `current` yaw toward `target` yaw at no more than [`MAX_TURN_RATE`],
/// taking the short way round. Returns the new yaw and the angular rate actually
/// used (rad/s) — feed that rate straight back into [`Zeroing::update`] as
/// `turn_rate` to close the turn-unzero feedback loop.
///
/// This is `bot_tick`'s tween (`bot.c:988-1040`) with the wrap handling folded
/// into a single `rem_euclid`.
pub fn turn_toward(current: f32, target: f32, dt: f32) -> (f32, f32) {
    let max_step = MAX_TURN_RATE * dt;
    let mut diff = (target - current).rem_euclid(std::f32::consts::TAU);
    if diff > std::f32::consts::PI {
        diff -= std::f32::consts::TAU;
    }
    if diff.abs() <= max_step {
        let rate = if dt > 0.0 { diff / dt } else { 0.0 };
        (target.rem_euclid(std::f32::consts::TAU), rate)
    } else {
        let step = max_step.copysign(diff);
        (
            (current + step).rem_euclid(std::f32::consts::TAU),
            if dt > 0.0 { step / dt } else { 0.0 },
        )
    }
}
