//! Behavioural defect detectors for hunter AI — the "is this hunter actually doing
//! anything" half of the AI lab, lifted out of it so the **real level** can be watched
//! with the same eyes as a synthetic arena.
//!
//! [`JankMonitor`] samples a running HUNT sim and flags defect *classes* rather than
//! exact values: a hunter that STALLS (visible player, stationary and silent), THRASHES
//! (state churn / walks in place), ends up in an ILLEGAL position, or stays stacked
//! inside a packmate. It was written for `world::ai_testbed`'s scripted arenas and stayed
//! `#[cfg(test)]` for months — which meant the one place these defects actually get
//! reported (the shipping level, mid-playtest) was the one place they could not be
//! measured. The nav probe (`world::nav_probe`) runs it on real geometry.

use super::*;
use crate::enemy::AiState;

// ─── Detector thresholds ─────────────────────────────────────────────────────
/// A hunter that keeps clear LOS to a player within engage range while NOT firing
/// for this long is "staring / not engaging" (the engage-stall bug).
pub(crate) const STALL_SECS: f32 = 3.0;
/// The range (m) within which a visible player should be getting shot at — beyond it
/// a hunter may legitimately still be closing, so the stall clock only runs inside it.
pub(crate) const STALL_ENGAGE_RANGE: f32 = 12.0;
/// More than this many FSM state changes inside a 1 s window is thrashing.
pub(crate) const THRASH_PER_SEC: usize = 6;
/// Reporting its legs as moving (`speed > 0`) while its feet barely travel for this
/// long is walking-in-place / a strafe dance stuck on a wall.
pub(crate) const WALK_IN_PLACE_SECS: f32 = 1.5;
pub(crate) const WALK_IN_PLACE_DISP: f32 = 0.3; // m of net travel that counts as "actually moving"
/// A single-step vertical jump larger than this means the model fell/teleported
/// through geometry (nav-gate violation).
pub(crate) const ILLEGAL_Y_STEP: f32 = 1.0;
/// Two live hunters whose centres stay closer than this (m) are interpenetrating —
/// well inside the `2·ENEMY_RADIUS` = 0.48 m combined radius. Local avoidance (ORCA)
/// must keep hunters from *remaining* stacked into one body (the crowd defect the old
/// position-nudge separation papered over); a brief transient while they resolve is
/// fine, a sustained one is the defect.
pub(crate) const OVERLAP_DIST: f32 = 0.34;
/// How long (s) two hunters may stay interpenetrated before it's flagged as a stack.
pub(crate) const OVERLAP_SECS: f32 = 1.0;

/// One flagged behavioral defect, with enough context to reproduce it.
#[derive(Clone, Debug)]
pub(crate) struct Violation {
    pub kind: &'static str,
    pub enemy: usize,
    pub t: f32,
    pub detail: String,
}

/// Per-enemy behavioral trace + defect flags across a run.
pub(crate) struct JankMonitor {
    t: f32,
    n: usize,
    pub(crate) prev_state: Vec<Option<AiState>>,
    pub(crate) trans_times: Vec<Vec<f32>>, // FSM-transition timestamps (for the thrash window)
    pub(crate) stare_secs: Vec<f32>,       // continuous "visible + still + silent" time
    pub(crate) still_secs: Vec<f32>,       // continuous "legs moving but not travelling" time
    pub(crate) still_anchor: Vec<Vec3>,    // where the still streak began
    pub(crate) prev_pos: Vec<Option<Vec3>>,
    pub(crate) packmate_blocks: Vec<u32>,  // frames a packmate capsule occluded the player ray
    pub(crate) overlap_secs: Vec<f32>,     // continuous time interpenetrating a packmate
    pub(crate) ever_fired: Vec<bool>,
    pub(crate) violations: Vec<Violation>,
}

impl JankMonitor {
    pub(crate) fn new(n: usize) -> Self {
        Self {
            t: 0.0,
            n,
            prev_state: vec![None; n],
            trans_times: vec![Vec::new(); n],
            stare_secs: vec![0.0; n],
            still_secs: vec![0.0; n],
            still_anchor: vec![Vec3::ZERO; n],
            prev_pos: vec![None; n],
            packmate_blocks: vec![0; n],
            overlap_secs: vec![0.0; n],
            ever_fired: vec![false; n],
            violations: Vec::new(),
        }
    }

    /// Sample the sim after a step. Reads private world state (in-crate) + casts a
    /// perception ray per hunter to classify LOS occlusion.
    pub(crate) fn sample(&mut self, world: &mut World, dt: f32) {
        self.t += dt;
        let Some(ppos) = world.player_pos() else { return };
        for i in 0..self.n.min(world.enemies.len()) {
            let (epos, collider, state, speed, dead, firing) = {
                let inst = &world.enemies[i];
                (
                    inst.enemy.pos,
                    inst.collider,
                    inst.enemy.state(),
                    inst.enemy.speed(),
                    inst.enemy.is_dead(),
                    inst.fire_elapsed.is_some(),
                )
            };
            if dead {
                continue;
            }
            if firing {
                self.ever_fired[i] = true;
            }

            // ── Overlap: two live hunters interpenetrating (stacked into one body) for
            //    a sustained stretch → local avoidance failed to keep them apart. ──
            let nearest = world
                .enemies
                .iter()
                .enumerate()
                .filter(|(j, e)| *j != i && !e.enemy.is_dead())
                .map(|(_, e)| Vec3::new(e.enemy.pos.x - epos.x, 0.0, e.enemy.pos.z - epos.z).length())
                .fold(f32::INFINITY, f32::min);
            if nearest < OVERLAP_DIST {
                self.overlap_secs[i] += dt;
                if self.overlap_secs[i] >= OVERLAP_SECS {
                    self.flag("overlap", i, format!(
                        "a packmate stayed {nearest:.2}m away (interpenetrating) for {:.1}s in {state:?}",
                        self.overlap_secs[i]
                    ));
                    self.overlap_secs[i] = 0.0; // one flag per streak
                }
            } else {
                self.overlap_secs[i] = 0.0;
            }

            // ── LOS: the AI's world-only perception (friendlies don't occlude). Plus a
            //    diagnostic count of frames a PACKMATE capsule sits on the self-excluded
            //    ray — now harmless to perception, but the metric shows how often it
            //    WOULD have blocked before the fix. ──
            let los = crate::enemy::perception_los(&mut world.physics, epos, ppos);
            let from = epos + Vec3::new(0.0, 1.0, 0.0);
            let to = ppos + Vec3::new(0.0, 0.8, 0.0);
            let d = to - from;
            let dist = d.length();
            let packmate_block = if dist > 1e-3 {
                let dir = d / dist;
                match world.physics.raycast_excluding(from, dir, dist - 0.1, Some(collider)) {
                    Some(hit) => world.physics.is_enemy_collider(hit.collider),
                    None => false,
                }
            } else {
                false
            };
            if packmate_block {
                self.packmate_blocks[i] += 1;
            }

            // ── Stall: a visible, in-range player that isn't being shot at → the
            //    "stands off looking at me but won't engage" bug. (Movement doesn't
            //    excuse it — a hunter circling a visible target should still fire.)
            let horiz = Vec3::new(epos.x - ppos.x, 0.0, epos.z - ppos.z).length();
            if los && horiz <= STALL_ENGAGE_RANGE && !firing {
                self.stare_secs[i] += dt;
                if self.stare_secs[i] >= STALL_SECS {
                    self.flag("stall", i, format!(
                        "visible player {:.1}m away un-engaged for {:.1}s in {:?}",
                        horiz, self.stare_secs[i], state
                    ));
                    self.stare_secs[i] = 0.0; // one flag per streak
                }
            } else {
                self.stare_secs[i] = 0.0;
            }
            let _ = speed; // (speed feeds the walk-in-place detector below)

            // ── Thrash: too many FSM transitions inside a 1 s window. ──
            if self.prev_state[i] != Some(state) {
                if self.prev_state[i].is_some() {
                    self.trans_times[i].push(self.t);
                    let recent = self.trans_times[i].iter().filter(|&&tt| tt >= self.t - 1.0).count();
                    if recent > THRASH_PER_SEC {
                        self.flag("thrash", i, format!(
                            "{recent} state changes in 1s (…→{:?}) at {:.1},{:.1}",
                            state, epos.x, epos.z
                        ));
                        self.trans_times[i].clear(); // one flag per burst
                    }
                }
                self.prev_state[i] = Some(state);
            }

            // ── Walk-in-place: legs moving but feet barely travelling (strafe dance). ──
            if speed > 0.1 {
                if self.still_secs[i] == 0.0 {
                    self.still_anchor[i] = epos;
                }
                self.still_secs[i] += dt;
                let travelled = Vec3::new(epos.x - self.still_anchor[i].x, 0.0, epos.z - self.still_anchor[i].z).length();
                if self.still_secs[i] >= WALK_IN_PLACE_SECS && travelled < WALK_IN_PLACE_DISP {
                    // Diagnostic: how many packmates are crowding it (separation-fight suspect)?
                    let crowd = world
                        .enemies
                        .iter()
                        .enumerate()
                        .filter(|(j, e)| *j != i && !e.enemy.is_dead() && e.enemy.pos.distance(epos) < 1.0)
                        .count();
                    self.flag("walk_in_place", i, format!(
                        "legs {:.1}s / travelled {:.2}m in {:?}, speed {:.1}, {crowd} packmates <1m",
                        self.still_secs[i], travelled, state, speed
                    ));
                    self.still_secs[i] = 0.0;
                } else if travelled >= WALK_IN_PLACE_DISP {
                    self.still_secs[i] = 0.0; // genuinely moving → reset
                }
            } else {
                self.still_secs[i] = 0.0;
            }

            // ── Illegal position: a big single-step vertical jump = fell/clipped. ──
            if let Some(prev) = self.prev_pos[i] {
                if (epos.y - prev.y).abs() > ILLEGAL_Y_STEP {
                    self.flag("illegal_y", i, format!(
                        "Y jumped {:.2}m in one step ({:.2}→{:.2})", (epos.y - prev.y).abs(), prev.y, epos.y
                    ));
                }
            }
            self.prev_pos[i] = Some(epos);
        }
    }

    pub(crate) fn flag(&mut self, kind: &'static str, enemy: usize, detail: String) {
        self.violations.push(Violation { kind, enemy, t: self.t, detail });
    }

    /// Diagnostic dump (printed under `cargo test -- --nocapture`).
    pub(crate) fn report(&self) {
        eprintln!("── JankMonitor: {} enemies, {:.1}s ──", self.n, self.t);
        for i in 0..self.n {
            eprintln!(
                "  e{i}: ever_fired={} packmate_los_blocks={}",
                self.ever_fired[i], self.packmate_blocks[i]
            );
        }
        for v in &self.violations {
            eprintln!("  ! [{}] e{} @ {:.1}s — {}", v.kind, v.enemy, v.t, v.detail);
        }
    }

    pub(crate) fn violations_of(&self, kind: &str) -> Vec<&Violation> {
        self.violations.iter().filter(|v| v.kind == kind).collect()
    }
}
