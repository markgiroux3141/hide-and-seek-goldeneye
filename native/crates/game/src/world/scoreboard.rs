//! Kills, deaths, and the round's win condition.
//!
//! With respawning, neither `caught` nor `player_dead` ends anything any more, so the
//! round needs a shape of its own: **first side to [`SCORE_LIMIT`] kills wins**. That
//! is Perfect Dark's default multiplayer shape, and PD already carries the same two
//! counters per competitor — `numkills` / `numdeaths` (`types.h:5699`).
//!
//! Two design notes worth stating, because both are load-bearing:
//!
//! * **The hunters score as one side, not individually.** A pack of simulants against
//!   one player is not a free-for-all leaderboard; the interesting number is "how many
//!   times have they got me". Per-slot tallies are still kept (see
//!   [`World::hunter_scores`]) because they cost nothing and make a playtest legible,
//!   but the win condition reads the side total.
//! * **The tallies live on the `World`, not on the `EnemyInstance`.** A hunter respawns
//!   *in place* — its instance is rebuilt in its own roster slot — so a score field on
//!   the instance would be wiped by every respawn. Keying off the slot index on the
//!   outside keeps a hunter's record across its own deaths.

use super::*;

/// One competitor's tally. PD's `numkills` / `numdeaths` (`types.h:5699`).
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Score {
    pub kills: u32,
    pub deaths: u32,
}

/// How a round ended. `None` on [`World::round_outcome`] means it is still live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoundOutcome {
    /// The player reached the score limit first.
    PlayerWins,
    /// The hunters, as a side, reached it first.
    HuntersWin,
}

/// Who dealt a killing blow, so a death can be credited. PD tracks the attacker on the
/// chr itself; here it is threaded through the damage call, which is honest about the
/// one case it cannot resolve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Killer {
    Player,
    /// The hunter in this roster slot.
    Hunter(usize),
    /// A turret the player placed. Credited to the player's side: the round is one
    /// player against the hunter pack, and a fixture the player built and sited is
    /// that player's doing. Kept distinct from [`Killer::Player`] rather than folded
    /// into it so "shot it myself" and "my turret got it" stay separable — which is
    /// the number that says whether turrets are pulling their weight.
    Turret,
    /// Nobody identifiable — a splash kill from an explosive, which carries no owner
    /// through the projectile/mine sim. The victim still takes the death; no kill is
    /// credited. (Threading an owner through `combat::explosives` would close this;
    /// it is deliberately out of scope for the scoreboard pass.)
    Unattributed,
}

impl World {
    /// Clear both sides' tallies and un-end the round. Called at G→HUNT and by the
    /// round restart, so a fresh round always starts 0–0.
    pub(crate) fn reset_scores(&mut self) {
        self.player_score = Score::default();
        for s in &mut self.hunter_scores {
            *s = Score::default();
        }
        self.round_over = None;
    }

    /// The player's tally.
    pub fn player_score(&self) -> Score {
        self.player_score
    }

    /// The hunters' **side** tally: kills and deaths summed over the roster. This is
    /// what the HUD shows and what the win condition reads.
    pub fn hunter_side_score(&self) -> Score {
        self.hunter_scores.iter().fold(Score::default(), |mut acc, s| {
            acc.kills += s.kills;
            acc.deaths += s.deaths;
            acc
        })
    }

    /// Per-slot hunter tallies, for logs and the AI lab. Indexed by roster slot, which is
    /// stable across a hunter's own respawns.
    pub fn hunter_scores(&self) -> &[Score] {
        &self.hunter_scores
    }

    /// How this round ended, or `None` while it is still live.
    pub fn round_outcome(&self) -> Option<RoundOutcome> {
        self.round_over
    }

    /// The kills needed to win (0 = endless). Set from `SCORE_LIMIT` at boot.
    pub fn score_limit(&self) -> u32 {
        self.score_limit
    }

    /// Override the score limit (0 = endless). Used by the launcher env override and by
    /// tests that want a round to end in a couple of kills.
    pub fn set_score_limit(&mut self, limit: u32) {
        // An explicit override outranks the level's authored config (see `PlayPins`).
        self.pins.score_limit = true;
        self.score_limit = limit;
    }

    /// Credit a hunter's death: a death for the hunter side (slot `victim`) and a kill
    /// for whoever landed it. Called from the one place every hunter death funnels
    /// through (`start_death`).
    pub(crate) fn record_hunter_death(&mut self, victim: usize, killer: Killer) {
        if let Some(s) = self.hunter_scores.get_mut(victim) {
            s.deaths += 1;
        }
        match killer {
            // A turret kill is the player's side's kill — see [`Killer::Turret`].
            Killer::Player | Killer::Turret => self.player_score.kills += 1,
            // Hunter-on-hunter: the pack's side total gains nothing from shooting its
            // own, so only the individual slot is credited. Otherwise a free-for-all
            // pack would win the round against itself without ever touching the player.
            Killer::Hunter(k) => {
                if let Some(s) = self.hunter_scores.get_mut(k) {
                    s.kills += 1;
                }
            }
            Killer::Unattributed => {}
        }
        self.check_round_over();
        log::info!(
            "SCORE — you {}-{}, hunters {}-{}",
            self.player_score.kills,
            self.player_score.deaths,
            self.hunter_side_score().kills,
            self.hunter_side_score().deaths,
        );
    }

    /// Credit the player's death: a death for the player and a kill for the hunter that
    /// landed it (the pack's side total, since the hunters compete as one side).
    pub(crate) fn record_player_death(&mut self, killer: Killer) {
        self.player_score.deaths += 1;
        if let Killer::Hunter(k) = killer {
            if let Some(s) = self.hunter_scores.get_mut(k) {
                s.kills += 1;
            }
        }
        self.check_round_over();
        log::info!(
            "SCORE — you {}-{}, hunters {}-{}",
            self.player_score.kills,
            self.player_score.deaths,
            self.hunter_side_score().kills,
            self.hunter_side_score().deaths,
        );
    }

    /// Latch the round outcome once either side reaches the limit. A limit of 0 means
    /// endless (the round never ends), which is what a long observation run wants.
    fn check_round_over(&mut self) {
        if self.score_limit == 0 || self.round_over.is_some() {
            return;
        }
        // The player is checked first so a simultaneous double-kill resolves in its
        // favour — an arbitrary but stated tie-break.
        if self.player_score.kills >= self.score_limit {
            self.round_over = Some(RoundOutcome::PlayerWins);
        } else if self.hunter_side_score().kills >= self.score_limit {
            self.round_over = Some(RoundOutcome::HuntersWin);
        }
        if let Some(o) = self.round_over {
            log::info!("ROUND OVER — {o:?} (limit {})", self.score_limit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::tools::spawn_point::tests::{big_room, place_pad};

    /// A HUNT with pads, no hunters spawned, and a short score limit — so a round can be
    /// driven to its end in a couple of scripted kills.
    fn arena(limit: u32) -> World {
        let mut world = big_room(40.0);
        world.set_spawn_enemies(false);
        world.set_score_limit(limit);
        place_pad(&mut world, Vec3::new(6.0, 0.0, 6.0), 0.0);
        place_pad(&mut world, Vec3::new(34.0, 0.0, 34.0), 0.0);
        world.camera.pos = Vec3::new(20.0, 2.0, 20.0);
        world.toggle_mode();
        world
    }

    /// The player's kills reach the limit → the player wins, and the round latches so it
    /// can't be re-decided by a later kill.
    #[test]
    fn the_player_wins_on_reaching_the_score_limit() {
        let mut world = arena(3);
        assert!(world.round_outcome().is_none(), "a fresh round is live");
        for n in 1..=3 {
            world.player_score.kills = n;
            world.check_round_over();
        }
        assert_eq!(world.round_outcome(), Some(RoundOutcome::PlayerWins));
        // Latched: the hunters overtaking afterwards does not flip the result.
        world.hunter_scores = vec![Score { kills: 99, deaths: 0 }];
        world.check_round_over();
        assert_eq!(
            world.round_outcome(),
            Some(RoundOutcome::PlayerWins),
            "the outcome latches once decided"
        );
    }

    /// The hunters score as one **side**: their per-slot kills sum toward the limit, so
    /// three simulants with one kill each end a limit-3 round.
    #[test]
    fn the_hunters_win_as_a_side_not_individually() {
        let mut world = arena(3);
        world.hunter_scores = vec![Score::default(); 3];
        for i in 0..3 {
            world.hunter_scores[i].kills = 1;
            world.check_round_over();
        }
        assert_eq!(world.hunter_side_score().kills, 3, "kills sum across the side");
        assert_eq!(world.round_outcome(), Some(RoundOutcome::HuntersWin));
    }

    /// A hunter shooting a packmate credits that hunter's own slot but **not** the side
    /// total — otherwise a free-for-all pack would win the round against itself without
    /// ever touching the player.
    #[test]
    fn hunter_on_hunter_kills_do_not_advance_the_side_total() {
        let mut world = arena(2);
        world.hunter_scores = vec![Score::default(); 2];
        world.record_hunter_death(1, Killer::Hunter(0));
        assert_eq!(world.hunter_scores()[0].kills, 1, "the shooter's slot is credited");
        assert_eq!(world.hunter_scores()[1].deaths, 1, "the victim takes the death");
        assert_eq!(
            world.hunter_side_score().kills, 1,
            "…but only as that slot's own tally"
        );
        assert!(
            world.round_outcome().is_none(),
            "the pack cannot win the round by shooting itself"
        );
        // Whereas killing the player does advance the side.
        world.record_player_death(Killer::Hunter(0));
        world.record_player_death(Killer::Hunter(1));
        assert_eq!(world.round_outcome(), Some(RoundOutcome::HuntersWin));
    }

    /// A splash kill has no attacker to credit (neither `Projectile` nor `Mine` carries an
    /// owner), so the victim takes the death and nobody gains a kill. This is the one
    /// attribution gap, asserted so it stays a known shape rather than a surprise.
    #[test]
    fn an_unattributed_kill_scores_a_death_but_no_kill() {
        let mut world = arena(5);
        world.hunter_scores = vec![Score::default(); 1];
        world.record_hunter_death(0, Killer::Unattributed);
        assert_eq!(world.hunter_scores()[0].deaths, 1, "the death still counts");
        assert_eq!(world.player_score().kills, 0, "nobody is credited");
    }

    /// `SCORE_LIMIT=0` is endless: no number of kills ends the round. What a long
    /// observation run wants.
    #[test]
    fn a_zero_limit_round_never_ends() {
        let mut world = arena(0);
        world.player_score.kills = 500;
        world.check_round_over();
        assert!(world.round_outcome().is_none(), "endless rounds do not end");
    }

    /// Entering HUNT starts 0–0, and the round restart clears the board again.
    #[test]
    fn scores_reset_at_hunt_entry_and_on_a_round_restart() {
        let mut world = arena(3);
        assert_eq!(world.player_score(), Score::default(), "G starts 0-0");

        world.player_score = Score { kills: 3, deaths: 2 };
        world.hunter_scores = vec![Score { kills: 2, deaths: 3 }];
        world.check_round_over();
        assert!(world.round_outcome().is_some());

        world.restart_round();
        assert_eq!(world.player_score(), Score::default(), "a new round starts 0-0");
        assert_eq!(world.hunter_side_score(), Score::default());
        assert!(world.round_outcome().is_none(), "the new round is live");
        assert!(!world.is_build(), "a round restart stays in HUNT");
    }
}
