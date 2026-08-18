//! Perfect Dark's **combat movement**: `botcmd_tick_dist_mode` (`botcmd.c:39`),
//! ported verbatim.
//!
//! This is the whole of how a PD simulant moves in a fight, and it is far simpler
//! than what we grew: a distance to the target, measured against the weapon's band
//! from `g_BotDistConfigs`, picks one of four modes, and each mode issues **one**
//! movement command.
//!
//! | mode | condition | PD's action | ours |
//! |---|---|---|---|
//! | [`DistMode::Backup`] | closer than the band's min | `chr_run_from_pos` | run straight away |
//! | [`DistMode::Ok`] | inside the band | `chr_try_stop` | **stand still and shoot** |
//! | [`DistMode::Advance`] | past the band's max | `chr_go_to_prop` | path to the live target |
//! | [`DistMode::Goto`] | past the third limit | `chr_go_to_prop` | identical |
//!
//! `Advance` and `Goto` issue the same command in PD too — the third limit exists
//! only to name the case, which is why the table's own comment says the third value
//! "doesn't appear to have any purpose". It is carried here so the mode a hunter
//! reports is PD's, not a two-mode simplification of it.
//!
//! Three details carry the behaviour, and all three are ported:
//!
//! * **`Ok` requires line of sight.** `!insight` demotes it to `Advance`, so a bot
//!   never stands still behind a wall (`botcmd.c:135`).
//! * **The anti-oscillation override.** If the target leaves sight *during a backup*,
//!   the bot advances and then holds `Ok` for a random 0.33–2.33 s. The source
//!   explains it as stopping a backup/advance loop around a corner (`botcmd.c:146`).
//! * **The 1 s rate limit** (`distmodettl60`): a mode's movement command is only
//!   re-issued on a mode *change*, or once a second, or when the bot is standing.
//!
//! What is deliberately **not** here: PD's `MA_AIBOTFOLLOW` branch (we have no
//! follow action — see the action ladder in `enemy.rs`), and the `bot_has_ground`
//! early-out (our player is nav-grounded by construction, so there is no airborne
//! target to bail on).
//!
//! This module is pure: it decides a mode and whether to re-issue the command. The
//! execution — run away / stand / path in — is [`crate::enemy::Enemy`]'s, because
//! that is where the nav grid lives.

use super::difficulty::BotDifficulty;

/// PD world units per metre. `g_BotDistConfigs` is authored in these (the pistol
/// band's `300` is 3 m), and so is the ±25 hysteresis below.
pub const PD_UNITS_PER_M: f32 = 100.0;

/// Widen the band by this much (m) *against the direction the bot is currently
/// moving*, so it does not flip modes on the exact boundary — `botcmd.c:110`'s
/// `minattackdistance += 25.0f` / `maxattackdistance -= 25.0f`.
const HYSTERESIS_M: f32 = 25.0 / PD_UNITS_PER_M;

/// The anti-oscillation `Ok` hold, in seconds: `TICKS(20) + random() % TICKS(120)`
/// (`botcmd.c:152`), where `TICKS(60)` is one second.
const OVERRIDE_MIN_S: f32 = 20.0 / 60.0;
const OVERRIDE_SPAN_S: f32 = 120.0 / 60.0;

/// How long a movement command stands before it may be re-issued —
/// `aibot->distmodettl60 = TICKS(60)` (`botcmd.c:189`).
const MODE_TTL_S: f32 = 1.0;

/// `BOTDISTMODE_*` (`bot.h`). The four states of PD's combat movement.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DistMode {
    /// Too close — run directly away from the target.
    Backup,
    /// In the band with a sightline — **stop and shoot**.
    Ok,
    /// Past the band's max — run at the target.
    Advance,
    /// Past the third limit — run at the target, from further out.
    Goto,
}

impl DistMode {
    /// Whether this mode wants the feet to move at all. Only [`Self::Ok`] does not,
    /// and standing still in-band is the single most visible thing about a PD bot.
    pub fn is_moving(self) -> bool {
        !matches!(self, DistMode::Ok)
    }

    pub fn name(self) -> &'static str {
        match self {
            DistMode::Backup => "BACKUP",
            DistMode::Ok => "OK",
            DistMode::Advance => "ADVANCE",
            DistMode::Goto => "GOTO",
        }
    }
}

/// One row of `g_BotDistConfigs` (`botcmd.c:29`) in metres: the distance a bot wants
/// to fight at with a given weapon function.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DistBand {
    pub min_m: f32,
    pub max_m: f32,
    /// The third limit — `Advance` below it, `Goto` above. Behaviourally inert in PD
    /// (both issue `chr_go_to_prop`); kept so the reported mode is faithful.
    pub limit3_m: f32,
}

impl DistBand {
    /// `BOTDISTCFG_DEFAULT` — the band a weapon with nothing more specific gets.
    pub const DEFAULT: DistBand = DistBand { min_m: 3.0, max_m: 6.0, limit3_m: 45.0 };

    /// The middle of the band — where a hunter running this model should be found
    /// standing. Used by the AI lab's "distance held vs the weapon band" metric.
    pub fn centre_m(&self) -> f32 {
        (self.min_m + self.max_m) * 0.5
    }

    /// Whether `dist_m` is inside the band (the raw test, without the mode-dependent
    /// hysteresis that [`DistModeState::tick`] applies).
    pub fn contains(&self, dist_m: f32) -> bool {
        dist_m >= self.min_m && dist_m < self.max_m
    }
}

/// What one [`DistModeState::tick`] decided.
#[derive(Clone, Copy, Debug)]
pub struct DistDecision {
    /// The mode now in force.
    pub mode: DistMode,
    /// Whether the movement command should be **re-issued** this tick (a fresh path).
    /// False means "keep walking the one you already have" — PD only re-issues on a
    /// mode change, when standing, or once the 1 s TTL lapses.
    pub reissue: bool,
}

/// The per-bot state `botcmd_tick_dist_mode` keeps on `aibot`.
///
/// `distoverrideprop` is a `bool` here rather than a target pointer: a hunter engages
/// exactly one target at a time in this game, so "the override is armed against the
/// current target" is all the pointer ever told PD.
#[derive(Clone, Copy, Debug)]
pub struct DistModeState {
    mode: DistMode,
    /// `aibot->distoverrideprop != NULL` — the anti-oscillation override is armed.
    override_armed: bool,
    /// `aibot->distoverridetimer60`, in seconds.
    override_timer: f32,
    /// `aibot->distmodettl60`, in seconds.
    ttl: f32,
}

impl Default for DistModeState {
    fn default() -> Self {
        // PD zero-initialises the aibot, and `BOTDISTMODE_BACKUP` is 0 — but a bot
        // that has never seen a target is not backing away from one, and the first
        // tick overwrites this before anything reads it.
        DistModeState { mode: DistMode::Advance, override_armed: false, override_timer: 0.0, ttl: 0.0 }
    }
}

impl DistModeState {
    /// The mode currently in force.
    pub fn mode(&self) -> DistMode {
        self.mode
    }

    /// One tick of `botcmd_tick_dist_mode` (`botcmd.c:39`).
    ///
    /// * `dist_m` — 3D distance to the target (`chr_get_distance_to_coord`, `botcmd.c:98`).
    /// * `band` — the weapon function's `g_BotDistConfigs` row.
    /// * `insight` — `aibot->targetinsight`: a raw line of sight, **not** a view cone.
    /// * `tier` — the bot's difficulty; MEAT and EASY are allowed much closer in.
    /// * `standing` — `chr->actiontype == ACT_STAND`, i.e. the feet are not travelling.
    /// * `rand01` — one uniform draw in `[0,1)`, consumed only when the override arms.
    pub fn tick(
        &mut self,
        dist_m: f32,
        band: DistBand,
        insight: bool,
        tier: BotDifficulty,
        standing: bool,
        dt: f32,
        rand01: f32,
    ) -> DistDecision {
        let prev = self.mode;

        // A weak bot is allowed much closer before it decides it is crowded
        // (`botcmd.c:103`) — the only place difficulty touches this decision.
        let mut min = band.min_m
            * match tier {
                BotDifficulty::Meat => 0.35,
                BotDifficulty::Easy => 0.5,
                _ => 1.0,
            };
        let mut max = band.max_m;
        // Boundary hysteresis, applied against the CURRENT mode so a bot that is
        // already backing up has to get further out before it stops, and one that is
        // already advancing has to get further in.
        match self.mode {
            DistMode::Backup => min += HYSTERESIS_M,
            DistMode::Advance | DistMode::Goto => max -= HYSTERESIS_M,
            DistMode::Ok => {}
        }

        let mut newmode = if dist_m < min {
            DistMode::Backup
        } else if dist_m < max {
            DistMode::Ok
        } else if dist_m < band.limit3_m {
            DistMode::Advance
        } else {
            DistMode::Goto
        };

        // Clear the override unless we are still in the exact case that armed it
        // (`botcmd.c:129`): a backup, against the same target, with sight regained.
        if !(newmode == DistMode::Backup && insight && self.override_armed) {
            self.override_armed = false;
            self.override_timer = 0.0;
        }

        if newmode == DistMode::Ok {
            // Never stand still without a sightline — close instead.
            if !insight {
                newmode = DistMode::Advance;
            }
        } else if newmode == DistMode::Backup {
            // The corner case the source calls out: losing sight mid-backup would
            // otherwise loop backup↔advance around a corner. Advance now, and when
            // the target comes back into view hold OK for a random beat.
            if !insight {
                newmode = DistMode::Advance;
                self.override_armed = true;
                self.override_timer = OVERRIDE_MIN_S + rand01.clamp(0.0, 1.0) * OVERRIDE_SPAN_S;
            } else if self.override_armed {
                if dt < self.override_timer {
                    self.override_timer -= dt;
                    newmode = DistMode::Ok;
                } else {
                    self.override_armed = false;
                    self.override_timer = 0.0;
                }
            }
        }

        self.mode = newmode;

        if self.ttl >= 0.0 {
            self.ttl -= dt;
        }

        // Re-issue the movement command on a mode change, or — for the three moving
        // modes — when the bot is standing still or the 1 s TTL has lapsed.
        let reissue = newmode != prev || (newmode != DistMode::Ok && (standing || self.ttl <= 0.0));
        if reissue {
            self.ttl = MODE_TTL_S;
        }
        DistDecision { mode: newmode, reissue }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IN_SIGHT: bool = true;
    const BLIND: bool = false;
    const DT: f32 = 1.0 / 60.0;
    const BAND: DistBand = DistBand::DEFAULT; // 3–6 m

    fn tick(s: &mut DistModeState, dist: f32, insight: bool) -> DistMode {
        s.tick(dist, BAND, insight, BotDifficulty::Normal, false, DT, 0.5).mode
    }

    /// The four modes come off the band exactly where PD puts them.
    #[test]
    fn the_band_picks_the_mode() {
        let mut s = DistModeState::default();
        assert_eq!(tick(&mut s, 1.0, IN_SIGHT), DistMode::Backup, "inside the min → back off");
        s = DistModeState::default();
        assert_eq!(tick(&mut s, 4.5, IN_SIGHT), DistMode::Ok, "in the band → stand and shoot");
        s = DistModeState::default();
        assert_eq!(tick(&mut s, 20.0, IN_SIGHT), DistMode::Advance, "past the max → close");
        s = DistModeState::default();
        assert_eq!(tick(&mut s, 60.0, IN_SIGHT), DistMode::Goto, "past the third limit → GOTO");
    }

    /// `!insight` demotes OK to ADVANCE — a bot never holds a distance it has no
    /// shot along (`botcmd.c:135`).
    #[test]
    fn no_sightline_never_stands_still() {
        let mut s = DistModeState::default();
        assert_eq!(tick(&mut s, 4.5, BLIND), DistMode::Advance);
    }

    /// Losing sight during a backup arms the override: the bot advances, and once the
    /// target is visible again it holds OK for a beat instead of resuming the backup
    /// (`botcmd.c:146`) — which is what stops the loop around a corner.
    #[test]
    fn the_backup_override_holds_ok_when_sight_returns() {
        let mut s = DistModeState::default();
        assert_eq!(tick(&mut s, 1.0, IN_SIGHT), DistMode::Backup, "too close, in sight");
        assert_eq!(tick(&mut s, 1.0, BLIND), DistMode::Advance, "sight lost mid-backup → advance");
        // Sight returns, still too close: PD holds OK rather than backing up again.
        for _ in 0..10 {
            assert_eq!(tick(&mut s, 1.0, IN_SIGHT), DistMode::Ok, "override holds OK");
        }
        // …until the random window (0.33–2.33 s; 0.5 draw → ~1.33 s) elapses.
        for _ in 0..200 {
            tick(&mut s, 1.0, IN_SIGHT);
        }
        assert_eq!(tick(&mut s, 1.0, IN_SIGHT), DistMode::Backup, "window over → normal behaviour");
    }

    /// The movement command is re-issued on a change, then rate-limited to once a
    /// second while the mode holds (`distmodettl60`).
    #[test]
    fn the_command_is_rate_limited_to_once_a_second() {
        let mut s = DistModeState::default();
        // First ADVANCE tick is a change → issue.
        assert!(s.tick(20.0, BAND, IN_SIGHT, BotDifficulty::Normal, false, DT, 0.5).reissue);
        let mut issues = 0;
        for _ in 0..180 {
            // 3 s of holding the same mode → the TTL lapses about three times
            if s.tick(20.0, BAND, IN_SIGHT, BotDifficulty::Normal, false, DT, 0.5).reissue {
                issues += 1;
            }
        }
        assert!((2..=3).contains(&issues), "expected ~1 re-issue per second over 3 s, got {issues}");
    }

    /// OK is never re-issued on the TTL — `chr_try_stop` is idempotent, and PD's
    /// condition excludes it explicitly (`botcmd.c:181`).
    #[test]
    fn standing_still_is_issued_once() {
        let mut s = DistModeState::default();
        assert!(s.tick(4.5, BAND, IN_SIGHT, BotDifficulty::Normal, false, DT, 0.5).reissue);
        for _ in 0..300 {
            assert!(
                !s.tick(4.5, BAND, IN_SIGHT, BotDifficulty::Normal, false, DT, 0.5).reissue,
                "OK re-issued a stop command"
            );
        }
    }

    /// The weakest tiers are allowed much closer before they feel crowded.
    #[test]
    fn meat_and_easy_tolerate_being_crowded() {
        let close = 1.2; // inside the 3 m min for a normal bot…
        for (tier, want) in [
            (BotDifficulty::Normal, DistMode::Backup),
            (BotDifficulty::Easy, DistMode::Backup),   // min 1.5 → still too close
            (BotDifficulty::Meat, DistMode::Ok),       // min 1.05 → happy here
        ] {
            let mut s = DistModeState::default();
            let got = s.tick(close, BAND, IN_SIGHT, tier, false, DT, 0.5).mode;
            assert_eq!(got, want, "{tier:?} at {close} m");
        }
    }

    /// The ±25-unit hysteresis keeps a bot sitting exactly on a boundary from
    /// flip-flopping between two modes every tick.
    #[test]
    fn the_boundary_does_not_oscillate() {
        let mut s = DistModeState::default();
        let mut flips = 0;
        let mut prev = None;
        for i in 0..600 {
            // Hover within a centimetre of the 6 m max.
            let d = 6.0 + if i % 2 == 0 { 0.005 } else { -0.005 };
            let m = tick(&mut s, d, IN_SIGHT);
            if prev.is_some_and(|p| p != m) {
                flips += 1;
            }
            prev = Some(m);
        }
        assert!(flips <= 1, "mode flipped {flips} times on the band boundary");
    }
}
