//! Hunter navigation probe — "can a hunter actually walk from A to B in *this* level,
//! and if not, which gate stopped it".
//!
//! The NAV tab (`world::nav_issues`) answers a **static** question: is the walkable grid
//! connected. Every hunter-stuck bug so far has lived in the gap between that answer and
//! what a body does at runtime — the grid said a steep flight was walkable and the mover
//! refused every step onto it; the grid said a doorway was passable and the committed
//! move walked through the panel instead of opening it. A connectivity report cannot see
//! either, because neither is a connectivity fault.
//!
//! So this drives a **real hunter** with the **real mover** over the **real level**:
//! `Enemy::move_toward` → `integrate_move` → `try_step`, with the world ticking
//! underneath so doors open and animate as they would in play. It reports where the
//! hunter got to, and — the part that makes it a diagnosis rather than an observation —
//! which gate refused the step it died on ([`crate::enemy::StepBlock`]).
//!
//! Two modes, and the second is the one that finds things:
//!
//! * a single `A → B` walk, for reproducing something you just watched happen;
//! * a **sweep** of every ordered pair drawn from a point set (the spawn pads by
//!   default), which turns "they get stuck sometimes" into a list of failing pairs that
//!   re-runs identically after every change.

use super::*;
use crate::enemy::{AiState, Enemy, StepBlock, SPEED_CHASE};

/// How close (flat metres) the hunter must get to count as arrived. Generous on purpose:
/// the question is "could it get there", not "can it stand on the exact cell".
const PROBE_ARRIVE: f32 = 1.2;
/// Telemetry cadence (s). Fine enough to watch a stall begin, coarse enough that a
/// 30-second probe is a readable log rather than 3,600 lines.
const SAMPLE_EVERY: f32 = 0.25;
/// Probe timestep — the sim rate, so the walk is the one the game runs.
const PROBE_DT: f32 = 1.0 / 120.0;
/// Tail of the run (s) used to decide whether it was still closing when time ran out.
const PROGRESS_WINDOW: f32 = 3.0;
/// Closing slower than this (m/s) over that window is not progress.
const PROGRESS_EPS: f32 = 0.2;

/// One row of probe telemetry.
#[derive(Clone, Copy, Debug)]
pub struct ProbeSample {
    pub t: f32,
    pub pos: Vec3,
    /// Flat distance still to go.
    pub to_go: f32,
    /// Which gate refused the last step (`None` = it moved).
    pub block: StepBlock,
    /// Consecutive refused steps at this moment.
    pub streak: u32,
    /// A* waypoints held, and which one it is walking to.
    pub path_len: usize,
    pub path_idx: usize,
}

/// One row of full-AI chase telemetry.
#[derive(Clone, Debug)]
pub struct ChaseSample {
    pub t: f32,
    pub pos: Vec3,
    pub to_go: f32,
    /// The AI state it was in — the column that answers "did it stop trying".
    pub state: String,
    pub block: StepBlock,
    pub stuck_secs: f32,
    /// Where it was actually walking, and whether it thought it had got there.
    pub target: Option<Vec3>,
    pub target_done: bool,
    /// Seconds left on the anti-grind settle when this row was taken.
    pub holding: f32,
    /// What the AI asked for this step (m/s) versus what the integrator actually
    /// committed. The pair that separates "it never tried" from "something ate the move".
    pub want: f32,
    pub got: f32,
    /// A* waypoints held / which one it walks to, and any door it is waiting on. A path
    /// of 1 is the giveaway for "A* snapped my target onto the cell I am already in".
    pub path_len: usize,
    pub path_idx: usize,
    pub door: Option<usize>,
    /// The worst refusal seen **since the previous row**, not just on this step. A
    /// quarter-second sample of an instantaneous flag misses the two frames that matter.
    pub block_seen: StepBlock,
}

/// What happened when a hunter was left to come and find the player on its own.
pub struct ChaseResult {
    pub from: Vec3,
    pub to: Vec3,
    pub arrived: bool,
    pub secs: f32,
    pub end: Vec3,
    /// The closest it ever got (m) — a pursuit that closes to 3 m and wanders off is a
    /// different failure from one that never left the room.
    pub closest: f32,
    /// Seconds spent in each AI state, most first. If a hunter that could walk the route
    /// never arrives, this is where the reason is.
    pub states: Vec<(String, f32)>,
    pub samples: Vec<ChaseSample>,
}

impl ChaseResult {
    pub fn verdict(&self) -> String {
        let states = self
            .states
            .iter()
            .map(|(s, secs)| format!("{s} {secs:.1}s"))
            .collect::<Vec<_>>()
            .join(", ");
        if self.arrived {
            format!(
                "CAME AND FOUND YOU in {:.1}s, closing to {:.1} m  [{states}]",
                self.secs, self.closest
            )
        } else {
            format!(
                "NEVER ARRIVED in {:.1}s — got within {:.1} m, ended at {:?}  [{states}]",
                self.secs,
                self.closest,
                round1(self.end)
            )
        }
    }

    pub fn report(&self) -> String {
        use std::fmt::Write;
        let mut s = self.verdict();
        s.push('\n');
        let _ = writeln!(
            s,
            "      t   pos                          to_go  state         block   stuck   hold   want    got   path  door  walking to                  done"
        );
        for r in &self.samples {
            let _ = writeln!(
                s,
                "  {:5.2}   ({:7.2},{:7.2},{:7.2})  {:6.2}  {:12}  {:6}  {:5.1}  {:5.2}  {:5.2}  {:5.2}  {:>3}/{:<3} {:>4}  {:26}  {}",
                r.t,
                r.pos.x,
                r.pos.y,
                r.pos.z,
                r.to_go,
                r.state,
                r.block_seen.label(),
                r.stuck_secs,
                r.holding,
                r.want,
                r.got,
                r.path_idx,
                r.path_len,
                r.door.map(|d| d.to_string()).unwrap_or_else(|| "-".into()),
                r.target
                    .map(|v| format!("({:6.2},{:6.2},{:6.2})", v.x, v.y, v.z))
                    .unwrap_or_else(|| "-".into()),
                if r.target_done { "ARRIVED/UNREACHABLE" } else { "" }
            );
        }
        s
    }
}

/// What happened on one A → B walk.
pub struct ProbeResult {
    pub from: Vec3,
    pub to: Vec3,
    pub arrived: bool,
    /// Seconds of simulated time before arriving or giving up.
    pub secs: f32,
    pub end: Vec3,
    /// Total ground actually covered (m) — a hunter that shuffles 40 m to travel 3 is a
    /// different failure from one that never moved at all.
    pub travelled: f32,
    /// Flat distance from the end position to the goal.
    pub to_go: f32,
    /// The gate that refused the most steps during the run, and how many.
    pub worst_block: StepBlock,
    pub blocked_steps: u32,
    /// Walkable components of the two endpoints — different ids mean no route exists and
    /// nothing the mover does could have helped.
    pub comp_from: Option<u32>,
    pub comp_to: Option<u32>,
    /// Whether A* ever produced a route at all.
    pub path_found: bool,
    /// How fast it was still closing on the goal over the last few seconds (m/s). The
    /// number that separates "it stopped" from "the clock ran out on a long walk" — and
    /// without it a sweep cries wolf over every cross-level route.
    pub closing_rate: f32,
    /// Seconds it had spent asking to move without travelling, when time ran out.
    pub end_stuck_secs: f32,
    pub samples: Vec<ProbeSample>,
}

impl ProbeResult {
    /// One line, and it has to name the cause rather than the symptom.
    pub fn verdict(&self) -> String {
        if self.arrived {
            return format!(
                "ARRIVED in {:.1}s ({:.1} m travelled)  {:?} -> {:?}",
                self.secs,
                self.travelled,
                round1(self.from),
                round1(self.to)
            );
        }
        if self.comp_from != self.comp_to {
            return format!(
                "NO ROUTE — endpoints are on different walkable components ({:?} vs {:?}); \
                 the level is cut in two here and no mover could cross it  {:?} -> {:?}",
                self.comp_from,
                self.comp_to,
                round1(self.from),
                round1(self.to)
            );
        }
        if !self.path_found {
            return format!(
                "NO PATH — same component, but A* never returned a route  {:?} -> {:?}",
                round1(self.from),
                round1(self.to)
            );
        }
        if self.closing_rate > PROGRESS_EPS {
            return format!(
                "TIMED OUT at {:?} after {:.1}s, {:.1} m short — but still closing at {:.1} m/s and nothing refused it; the route is longer than the time budget, so raise --secs  {:?} -> {:?}",
                round1(self.end),
                self.secs,
                self.to_go,
                self.closing_rate,
                round1(self.from),
                round1(self.to)
            );
        }
        format!(
            "STALLED at {:?} after {:.1}s, {:.1} m short — stopped moving for {:.1}s; {} step(s) refused by `{}`  {:?} -> {:?}",
            round1(self.end),
            self.secs,
            self.to_go,
            self.end_stuck_secs,
            self.blocked_steps,
            self.worst_block.label(),
            round1(self.from),
            round1(self.to)
        )
    }

    /// Whether this run found a real defect, as against merely running out of clock.
    /// The sweep's exit status keys off this: a timeout is a budget problem and must not
    /// train anyone to ignore the report.
    pub fn is_defect(&self) -> bool {
        !self.arrived && self.closing_rate <= PROGRESS_EPS
    }

    /// The verdict plus the telemetry rows — what goes in the log file.
    pub fn report(&self) -> String {
        use std::fmt::Write;
        let mut s = self.verdict();
        s.push('\n');
        let _ = writeln!(s, "      t   pos                          to_go  block   streak  path");
        for r in &self.samples {
            let _ = writeln!(
                s,
                "  {:5.2}   ({:7.2},{:7.2},{:7.2})  {:6.2}  {:6}  {:6}  {}/{}",
                r.t,
                r.pos.x,
                r.pos.y,
                r.pos.z,
                r.to_go,
                r.block.label(),
                r.streak,
                r.path_idx,
                r.path_len
            );
        }
        s
    }
}

fn round1(v: Vec3) -> (f32, f32, f32) {
    let r = |a: f32| (a * 10.0).round() / 10.0;
    (r(v.x), r(v.y), r(v.z))
}

fn flat_dist(a: Vec3, b: Vec3) -> f32 {
    Vec3::new(b.x - a.x, 0.0, b.z - a.z).length()
}

impl World {
    /// Walk one hunter from `start` to `goal` with the real mover, for at most
    /// `max_secs` of simulated time.
    ///
    /// Must be called in HUNT (it needs the baked grid, the live doors and the props).
    /// The world is ticked each step so a door the hunter asks for actually swings, and
    /// its requests are serviced exactly as `fixed_step` services a live hunter's.
    ///
    /// The probe hunter is a **local** — never added to `self.enemies` — so it does not
    /// fight, is not shot at, and cannot perturb the level it is measuring.
    pub fn probe_walk(&mut self, start: Vec3, goal: Vec3, max_secs: f32) -> ProbeResult {
        let mut out = ProbeResult {
            from: start,
            to: goal,
            arrived: false,
            secs: 0.0,
            end: start,
            travelled: 0.0,
            to_go: flat_dist(start, goal),
            worst_block: StepBlock::None,
            blocked_steps: 0,
            comp_from: None,
            comp_to: None,
            path_found: false,
            closing_rate: 0.0,
            end_stuck_secs: 0.0,
            samples: Vec::new(),
        };
        let Some(nav) = self.nav.as_ref() else {
            return out;
        };
        // Snap both ends onto standable ground, so a point read off a marker or a
        // screenshot does not fail as "no path" for being 4 cm inside the floor.
        let from = nav
            .nearest_standable(start.x, start.y + 0.1, start.z, 16)
            .unwrap_or(start);
        let goal = nav
            .nearest_standable(goal.x, goal.y + 0.1, goal.z, 16)
            .unwrap_or(goal);
        out.from = from;
        out.to = goal;
        out.comp_from = nav.component_at(from);
        out.comp_to = nav.component_at(goal);
        out.end = from;

        let mut e = Enemy::new(from, goal);
        // Nothing may shoot the probe or be shot by it; it is a measuring instrument.
        let was_invuln = self.player_invulnerable;
        self.player_invulnerable = true;

        let (mut door_blocks, mut wall_blocks, mut ground_blocks) = (0u32, 0u32, 0u32);
        let mut t = 0.0f32;
        let mut next_sample = 0.0f32;
        let mut prev = from;
        let input = InputState::default();
        while t < max_secs {
            // Tick the level itself so doors swing and props settle; the probe hunter is
            // driven separately below. With no live hunters this is the world's own
            // clockwork and little else.
            self.fixed_step(PROBE_DT, &input);
            let Some(nav) = self.nav.as_ref() else { break };
            e.desired_vel = Vec3::ZERO;
            e.move_toward(PROBE_DT, goal, nav, SPEED_CHASE);
            let v = e.desired_vel;
            e.integrate_move(v, PROBE_DT, nav);
            let door = e.pending_door();
            if e.path_len() > 0 {
                out.path_found = true;
            }

            t += PROBE_DT;
            out.travelled += flat_dist(prev, e.pos);
            prev = e.pos;
            let block = e.step_block();
            match block {
                StepBlock::Door => door_blocks += 1,
                StepBlock::Sightline => wall_blocks += 1,
                StepBlock::Ground => ground_blocks += 1,
                StepBlock::None => {}
            }
            if block != StepBlock::None {
                out.blocked_steps += 1;
            }
            let to_go = flat_dist(e.pos, goal);
            if t >= next_sample {
                next_sample += SAMPLE_EVERY;
                out.samples.push(ProbeSample {
                    t,
                    pos: e.pos,
                    to_go,
                    block,
                    streak: e.block_streak(),
                    path_len: e.path_len(),
                    path_idx: e.path_index(),
                });
            }
            // Service the door it pulled up at, exactly as the live loop does.
            if let Some(di) = door {
                self.hunter_opens_door(di);
            }
            if to_go <= PROBE_ARRIVE {
                out.arrived = true;
                break;
            }
        }
        self.player_invulnerable = was_invuln;
        out.end_stuck_secs = e.stuck_secs();
        // Progress over the tail of the run: still shrinking means it was walking, not
        // stuck, and the only thing wrong is the time budget.
        let tail: Vec<&ProbeSample> = out
            .samples
            .iter()
            .filter(|s| s.t >= t - PROGRESS_WINDOW)
            .collect();
        if let (Some(a), Some(b)) = (tail.first(), tail.last()) {
            let span = b.t - a.t;
            if span > 0.1 {
                out.closing_rate = (a.to_go - b.to_go) / span;
            }
        }
        out.secs = t;
        out.end = e.pos;
        out.to_go = flat_dist(e.pos, goal);
        // Whichever gate did the most refusing is the one to go and look at.
        out.worst_block = if door_blocks > 0 && door_blocks >= wall_blocks && door_blocks >= ground_blocks
        {
            StepBlock::Door
        } else if ground_blocks > 0 && ground_blocks >= wall_blocks {
            StepBlock::Ground
        } else if wall_blocks > 0 {
            StepBlock::Sightline
        } else {
            StepBlock::None
        };
        out
    }

    /// Walk one hunter to the player under the **full AI** — target selection, belief,
    /// combat and all — rather than by commanding it to a point.
    ///
    /// [`Self::probe_walk`] deliberately bypasses the AI so it measures nav and the mover
    /// in isolation. That leaves one question it cannot answer, and it is the one a long
    /// route raises: a hunter may be perfectly able to walk somewhere and still never
    /// *choose* to, because its belief about where you are lapses part-way
    /// (`ENGAGE_MEMORY` is 5 s, and a 20 m climb takes fifteen). "It can get there" and
    /// "it will get there" are different claims and this makes the second one.
    ///
    /// The player is pinned at `goal` and made invulnerable — the probe is measuring
    /// pursuit, not a gunfight it might lose.
    pub fn probe_chase(&mut self, start: Vec3, goal: Vec3, max_secs: f32) -> ChaseResult {
        let mut out = ChaseResult {
            from: start,
            to: goal,
            arrived: false,
            secs: 0.0,
            end: start,
            closest: f32::INFINITY,
            states: Vec::new(),
            samples: Vec::new(),
        };
        if self.mode != Mode::Hunt || self.nav.is_none() {
            return out;
        }
        // A single hunter, put where we want it, with the player standing at the goal.
        self.player_invulnerable = true;
        self.set_wave_size(1);
        if let Some(c) = self.character.as_mut() {
            c.pos = goal;
        }
        self.restart_hunt();
        if let Some(c) = self.character.as_mut() {
            c.pos = goal;
        }
        let Some(inst) = self.enemies.first_mut() else { return out };
        inst.enemy.pos = start;
        let collider = inst.collider;
        self.physics.update_enemy_collider(collider, start);

        let mut tally: std::collections::BTreeMap<String, f32> = std::collections::BTreeMap::new();
        let mut t = 0.0f32;
        let mut next_sample = 0.0f32;
        let mut block_seen = StepBlock::None;
        let input = InputState::default();
        while t < max_secs {
            // Re-pin the player each step: a respawn or a shove would otherwise move the
            // destination out from under the measurement.
            if let Some(c) = self.character.as_mut() {
                c.pos = goal;
            }
            self.fixed_step(PROBE_DT, &input);
            self.enemy_combat_step(PROBE_DT);
            t += PROBE_DT;
            let Some(inst) = self.enemies.first() else { break };
            let e = &inst.enemy;
            let d = flat_dist(e.pos, goal);
            out.closest = out.closest.min(d);
            out.end = e.pos;
            *tally.entry(format!("{:?}", e.state())).or_insert(0.0) += PROBE_DT;
            if e.step_block() != StepBlock::None {
                block_seen = e.step_block();
            }
            if t >= next_sample {
                next_sample += SAMPLE_EVERY;
                out.samples.push(ChaseSample {
                    t,
                    pos: e.pos,
                    to_go: d,
                    state: format!("{:?}", e.state()),
                    block: e.step_block(),
                    stuck_secs: e.stuck_secs(),
                    target: e.last_target(),
                    target_done: e.last_target_done(),
                    holding: e.stuck_hold(),
                    want: e.desired_velocity().length(),
                    got: e.velocity().length(),
                    path_len: e.path_len(),
                    path_idx: e.path_index(),
                    door: e.pending_door(),
                    block_seen,
                });
                block_seen = StepBlock::None;
            }
            // "Came and found you" means it got into the fight, not that it touched you.
            // A hunter that closes and then plants itself at its standoff to shoot has
            // succeeded; holding it to a 1.2 m arrival would score correct behaviour as a
            // failure and hide the real ones.
            if d <= PROBE_ARRIVE || matches!(e.state(), AiState::Attack) {
                out.arrived = true;
                break;
            }
        }
        out.secs = t;
        out.states = tally.into_iter().collect();
        out.states.sort_by(|a, b| b.1.total_cmp(&a.1));
        out
    }

    /// The default probe point set: every spawn pad the level authored, which is where
    /// bodies actually enter and so where "can they get out of here" matters most.
    pub fn probe_points(&self) -> Vec<Vec3> {
        self.spawn_pads.iter().map(|p| p.pos).collect()
    }

    /// Walk every ordered pair of `points`. `O(n^2)` probes, each bounded by `max_secs`,
    /// and most arrive in a second or two.
    pub fn probe_sweep(&mut self, points: &[Vec3], max_secs: f32) -> Vec<ProbeResult> {
        let mut out = Vec::new();
        for (i, a) in points.iter().enumerate() {
            for (j, b) in points.iter().enumerate() {
                if i != j {
                    out.push(self.probe_walk(*a, *b, max_secs));
                }
            }
        }
        out
    }

    /// Every live hunter's state as one block of text — the in-game capture.
    ///
    /// Written for the moment you are staring at a hunter that will not move: what it
    /// thinks it is doing, what it is walking to, and what refused its last step, without
    /// a debugger. It names the walkable component of both it and you, so a genuinely
    /// cut-off hunter is distinguishable from a stuck one.
    pub fn hunter_telemetry(&self) -> String {
        use std::fmt::Write;
        let mut s = String::new();
        let ppos = self.player_pos();
        let comp = |p: Vec3| {
            self.nav
                .as_ref()
                .and_then(|n| n.component_at(p))
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".into())
        };
        let _ = writeln!(
            s,
            "== hunter telemetry == {} hunter(s); player at {:?}, walkable comp {}",
            self.enemies.len(),
            ppos.map(round1),
            ppos.map(&comp).unwrap_or_else(|| "-".into())
        );
        if let Some(nav) = self.nav.as_ref() {
            for i in 0..nav.door_count() {
                let cells = nav.door_cells(i);
                let _ = writeln!(
                    s,
                    "  door {i}: marks {cells} nav cell(s){}",
                    if cells == 0 {
                        "  ** INVISIBLE TO PATHING — hunters walk through this **"
                    } else {
                        ""
                    }
                );
            }
        }
        for (i, inst) in self.enemies.iter().enumerate() {
            let e = &inst.enemy;
            let _ = writeln!(
                s,
                "  h{i}: {:?} at ({:6.2},{:6.2},{:6.2}) comp {:>3} {}  last step refused by \
                 `{}` (streak {}, stuck {:.1}s)  path {}/{}  door {:?}  holding {} {}/{}",
                e.state(),
                e.pos.x,
                e.pos.y,
                e.pos.z,
                comp(e.pos),
                if e.is_dead() { "DEAD " } else { "alive" },
                e.step_block().label(),
                e.block_streak(),
                e.stuck_secs(),
                e.path_index(),
                e.path_len(),
                e.pending_door(),
                inst.weapon.name,
                inst.loaded,
                inst.reserve,
            );
            if let Some(p) = ppos {
                let _ = writeln!(
                    s,
                    "       {:.1} m from you, {:+.1} m of rise; A* can route to you: {}",
                    flat_dist(e.pos, p),
                    p.y - e.pos.y,
                    self.nav
                        .as_ref()
                        .map(|n| if n.find_path(e.pos, p).is_some() { "yes" } else { "NO" })
                        .unwrap_or("?")
                );
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::tools::spawn_point::tests::big_room;

    fn hunt(mut world: World) -> World {
        world.set_spawn_enemies(false);
        world.camera.pos = Vec3::new(3.0, 2.0, 3.0);
        world.toggle_mode();
        world
    }

    /// The probe walks. Sounds trivial; it is the guard that the whole instrument still
    /// drives a real hunter — a probe that silently reports 0 s and no movement would
    /// make every sweep pass and mean nothing.
    #[test]
    fn a_probe_walks_across_an_open_room() {
        let mut world = hunt(big_room(20.0));
        let r = world.probe_walk(Vec3::new(3.0, 0.0, 3.0), Vec3::new(17.0, 0.0, 17.0), 30.0);
        assert!(r.arrived, "{}", r.verdict());
        assert!(r.travelled > 15.0, "it should have covered ground: {:.1} m", r.travelled);
        assert!(!r.samples.is_empty(), "telemetry must be recorded");
        assert!(r.verdict().starts_with("ARRIVED"), "{}", r.verdict());
    }

    /// A severed destination is reported as **no route**, naming the two components —
    /// not as a stall. They are different bugs with different fixes, and the whole point
    /// of the probe is that it says which.
    #[test]
    fn a_probe_names_a_severed_destination_rather_than_calling_it_a_stall() {
        let mut world = big_room(20.0);
        // A grounded platform 0.5 m up: walkable on top, no step onto it, and NOT stairs
        // (so the stair-local step limit does not apply).
        world.platforms.push(Platform {
            id: 1,
            x: 32.0,
            y: 2.0,
            z: 32.0,
            size_x: 24.0,
            size_z: 24.0,
            thickness: 2.0,
            grounded: true,
            railings: false,
        });
        let mut world = hunt(world);
        let r = world.probe_walk(Vec3::new(3.0, 0.0, 3.0), Vec3::new(11.0, 0.5, 11.0), 10.0);
        assert!(!r.arrived, "the ledge is unreachable: {}", r.verdict());
        assert!(r.is_defect(), "and it is a defect, not a slow walk");
        assert_ne!(r.comp_from, r.comp_to, "the endpoints are on different components");
        assert!(r.verdict().starts_with("NO ROUTE"), "{}", r.verdict());
    }

    /// **A hunter comes and finds you on another floor.**
    ///
    /// The defect this whole probe was built to catch, reduced to an arena: a hunter that
    /// could demonstrably *walk* the route (`probe_walk` did it in 16.7 s on the shipping
    /// level) would not *choose* to, and stood in Chase for 82 seconds instead. The cause
    /// was its aim point — a flank offset computed flat on its own floor, which with the
    /// player upstairs lands somewhere unwalkable; nav snapped that goal onto the cell
    /// under the hunter's own feet, A\* returned a one-cell path, and the mover asked for
    /// nothing while reporting it was still going.
    ///
    /// Asserted on the outcome a player cares about — did it turn up — because every
    /// internal signal in that failure looked healthy.
    #[test]
    fn a_hunter_climbs_to_a_player_on_another_floor() {
        let mut world = big_room(24.0);
        // A landing 3 m up with a ramp of platforms leading to it, so reaching the player
        // needs a real climb rather than a walk across the floor.
        for (i, y) in [4.0f32, 8.0, 12.0].into_iter().enumerate() {
            world.platforms.push(Platform {
                id: i as u32 + 1,
                x: 20.0 + i as f32 * 12.0,
                y,
                z: 40.0,
                size_x: 12.0,
                size_z: 24.0,
                thickness: 2.0,
                grounded: true,
                railings: false,
            });
        }
        world.set_spawn_enemies(true);
        world.camera.pos = Vec3::new(12.0, 2.0, 12.0);
        world.toggle_mode();

        // Player on the top landing (y = 12 WT = 3 m), hunter on the floor across the room.
        let goal = Vec3::new(11.5, 3.0, 13.0);
        let start = Vec3::new(3.0, 0.0, 3.0);
        let r = world.probe_chase(start, goal, 45.0);
        assert!(
            r.arrived,
            "the hunter never came: {}
{}",
            r.verdict(),
            r.samples
                .iter()
                .rev()
                .take(3)
                .map(|s| format!("  {:?} at {:?} walking to {:?}", s.state, s.pos, s.target))
                .collect::<Vec<_>>()
                .join("
")
        );
    }

    /// The live wave dial clamps, and re-floods the wave when it changes mid-hunt — the
    /// property the whole crowding bisection rests on. A dial that only took effect at
    /// the next `G` would make "try it with one" a level reload rather than a keypress.
    #[test]
    fn the_wave_dial_resizes_the_live_wave() {
        let mut world = big_room(20.0);
        world.set_wave_size(4);
        world.camera.pos = Vec3::new(10.0, 2.0, 10.0);
        world.toggle_mode();
        assert_eq!(world.enemies.len(), 4, "the wave floods in at the dialled size");

        world.change_wave_size(-3);
        assert_eq!(world.wave_size(), 1);
        assert_eq!(world.enemies.len(), 1, "and the live wave is re-flooded, not left as it was");

        // Clamped at the floor: no zero-hunter hunt by holding the key down.
        world.change_wave_size(-5);
        assert_eq!(world.wave_size(), 1, "the dial floors at one hunter");
        assert_eq!(world.enemies.len(), 1);

        world.change_wave_size(2);
        assert_eq!(world.enemies.len(), 3, "and it goes back up");
    }

    /// The in-game capture names every hunter and, crucially, whether A* can reach the
    /// player at all — the one question that separates "cut off" from "stuck".
    #[test]
    fn the_telemetry_capture_reports_each_hunter() {
        let mut world = big_room(20.0);
        world.set_wave_size(2);
        world.camera.pos = Vec3::new(10.0, 2.0, 10.0);
        world.toggle_mode();
        let dump = world.hunter_telemetry();
        assert!(dump.contains("hunter telemetry"), "{dump}");
        assert!(dump.contains("h0:"), "every hunter gets a line: {dump}");
        assert!(dump.contains("A* can route to you"), "the reachability verdict: {dump}");
    }
}
