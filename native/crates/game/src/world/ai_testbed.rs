//! Headless **AI test lab** (`#[cfg(test)]`) for ironing out emergent hunter-AI
//! defects — the "stands off staring," "gives up for a bit," and "manic strafing
//! behind a wall" jank that unit tests miss because it's multi-frame and
//! geometry-dependent.
//!
//! Two pieces:
//! * [`TestArena`] — builds a real [`World`] with authored **cover geometry** (a room
//!   cavity + interior solid obstacles), so line-of-sight and pathfinding stay
//!   consistent (both derive from the same region brushes — nav from `regions`,
//!   physics from the region trimesh). It runs the *full* HUNT sim (`fixed_step` +
//!   `enemy_combat_step`), including squad coordination / cover / grenade steps, with
//!   a scriptable player and a fixed timestep → byte-for-byte deterministic replays.
//! * [`JankMonitor`] — samples the sim each step and flags defect *classes* rather
//!   than exact values: a hunter that STALLS (visible player, stationary + silent),
//!   THRASHES (state churn / walks in place), or ends up in an ILLEGAL position, plus
//!   the "packmate capsule occludes the perception ray" metric (the prime suspect for
//!   the staring / give-up bugs). A tripped detector fails the test with a compact
//!   trace so the failure reproduces.
//!
//! Scenarios at the bottom assert the invariants that *should* hold; a failure
//! pinpoints a real defect to fix.

use super::*;
use crate::enemy::AiState;

// ─── Detector thresholds ─────────────────────────────────────────────────────
/// A hunter that keeps clear LOS to a player within engage range while NOT firing
/// for this long is "staring / not engaging" (the engage-stall bug).
const STALL_SECS: f32 = 3.0;
/// The range (m) within which a visible player should be getting shot at — beyond it
/// a hunter may legitimately still be closing, so the stall clock only runs inside it.
const STALL_ENGAGE_RANGE: f32 = 12.0;
/// More than this many FSM state changes inside a 1 s window is thrashing.
const THRASH_PER_SEC: usize = 6;
/// Reporting its legs as moving (`speed > 0`) while its feet barely travel for this
/// long is walking-in-place / a strafe dance stuck on a wall.
const WALK_IN_PLACE_SECS: f32 = 1.5;
const WALK_IN_PLACE_DISP: f32 = 0.3; // m of net travel that counts as "actually moving"
/// A single-step vertical jump larger than this means the model fell/teleported
/// through geometry (nav-gate violation).
const ILLEGAL_Y_STEP: f32 = 1.0;
/// Two live hunters whose centres stay closer than this (m) are interpenetrating —
/// well inside the `2·ENEMY_RADIUS` = 0.48 m combined radius. Local avoidance (ORCA)
/// must keep hunters from *remaining* stacked into one body (the crowd defect the old
/// position-nudge separation papered over); a brief transient while they resolve is
/// fine, a sustained one is the defect.
const OVERLAP_DIST: f32 = 0.34;
/// How long (s) two hunters may stay interpenetrated before it's flagged as a stack.
const OVERLAP_SECS: f32 = 1.0;

/// One flagged behavioral defect, with enough context to reproduce it.
#[derive(Clone, Debug)]
pub(crate) struct Violation {
    pub kind: &'static str,
    pub enemy: usize,
    pub t: f32,
    pub detail: String,
}

/// A headless HUNT sim with authored cover geometry + a placeable/scriptable player.
pub(crate) struct TestArena {
    pub world: World,
    /// The player's floor height (m) for this arena, so scripted moves stay grounded.
    floor_y: f32,
}

impl TestArena {
    /// Build the arena: one region = a `size` (WT `[w,h,d]`) room cavity with interior
    /// solid obstacles `obstacles` (WT AABBs `[x,y,z,w,h,d]`, `Op::Add` = walls/pillars),
    /// difficulty pinned to MAX, `wave` hunters, entered into HUNT with the player
    /// standing at `player_m` (metres, XZ — Y is resolved to the floor). Invulnerable so
    /// long runs don't end early.
    fn build(size: [f32; 3], obstacles: &[[f32; 6]], wave: usize, player_m: Vec3) -> Self {
        let mut world = World::new();
        // Swap the default room for our arena. A Subtract carves the cavity; each Add
        // re-fills a solid box inside it (an interior wall or pillar). Both nav + the
        // physics trimesh come from these brushes, so LOS and pathing agree.
        let mut region = Region::new(0);
        region
            .brushes
            .push(Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, size[0], size[1], size[2]));
        for (k, o) in obstacles.iter().enumerate() {
            region
                .brushes
                .push(Brush::new(2 + k as u32, Op::Add, o[0], o[1], o[2], o[3], o[4], o[5]));
        }
        world.regions = vec![region];
        world.set_difficulty(DIFFICULTY_MAX);
        world.set_wave_size(wave);
        // Seat the fly-cam over the wanted player spot so HUNT drops the capsule there.
        world.camera.pos = Vec3::new(player_m.x, player_m.y.max(1.5), player_m.z);
        world.initial_meshes();
        world.toggle_mode(); // bake nav + physics, spawn the wave
        world.toggle_invulnerable(); // survive long runs
        let floor_y = world.player_pos().map(|p| p.y).unwrap_or(0.0);
        Self { world, floor_y }
    }

    /// Teleport the player to `(x, z)` at the arena floor (a scripted move).
    fn set_player(&mut self, x: f32, z: f32) {
        if let Some(c) = self.world.character.as_mut() {
            c.pos = Vec3::new(x, self.floor_y, z);
        }
    }

    /// Point the player's camera so `forward()` looks at `target` — makes the hunter's
    /// `aimed_at` sense fire (drives the reactive aim-dodge). Convention from
    /// `camera::forward_from`: `pitch = asin(dy)`, `yaw = atan2(-dx, -dz)`.
    fn aim_player_at(&mut self, target: Vec3) {
        if let Some(c) = self.world.character.as_mut() {
            let d = (target - c.eye()).normalize_or_zero();
            if d.length_squared() < 1e-6 {
                return;
            }
            c.pitch = d.y.clamp(-1.0, 1.0).asin();
            c.yaw = (-d.x).atan2(-d.z);
        }
    }

    /// Place hunter `i` at `(x, z)` on the floor and resync its hitscan capsule.
    fn place_hunter(&mut self, i: usize, x: f32, z: f32) {
        let p = Vec3::new(x, self.floor_y, z);
        if let Some(inst) = self.world.enemies.get_mut(i) {
            inst.enemy.pos = p;
            let c = inst.collider;
            self.world.physics.update_enemy_collider(c, p);
        }
    }

    /// Advance the full HUNT sim one fixed step (FSM + combat). No animation pass —
    /// the AI logic doesn't depend on it, and skipping it keeps runs fast.
    fn step(&mut self, dt: f32) {
        let input = InputState::default();
        self.world.fixed_step(dt, &input);
        self.world.enemy_combat_step(dt);
    }

    /// Like [`Self::build`], but every hunter runs the **Perfect Dark simulant
    /// model** at the given tier (see [`super::pd_lab`]). The lab has to be enabled
    /// before `toggle_mode`, because that is what spawns the wave.
    ///
    /// The player is left *vulnerable* here, unlike `build` — measuring how much
    /// damage a tier lands is the whole point of these scenarios — so callers must
    /// keep runs short enough or heal between them.
    fn build_pd(
        size: [f32; 3],
        obstacles: &[[f32; 6]],
        wave: usize,
        player_m: Vec3,
        tier: crate::pdsim::difficulty::BotDifficulty,
    ) -> Self {
        let mut world = World::new();
        let mut region = Region::new(0);
        region
            .brushes
            .push(Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, size[0], size[1], size[2]));
        for (k, o) in obstacles.iter().enumerate() {
            region
                .brushes
                .push(Brush::new(2 + k as u32, Op::Add, o[0], o[1], o[2], o[3], o[4], o[5]));
        }
        world.regions = vec![region];
        world.set_difficulty(DIFFICULTY_MAX);
        world.enable_pd_lab(super::pd_lab::PdLabConfig {
            count: wave,
            difficulty: Some(tier),
            bot_type: crate::pdsim::personality::BotType::General,
            // The jank lab is about hunters versus the player, so teams stay on —
            // free-for-all would have half the pack duelling each other and the
            // player-approach metrics would measure nothing.
            free_for_all: false,
        });
        world.camera.pos = Vec3::new(player_m.x, player_m.y.max(1.5), player_m.z);
        world.initial_meshes();
        world.toggle_mode();
        let floor_y = world.player_pos().map(|p| p.y).unwrap_or(0.0);
        Self { world, floor_y }
    }
}

/// Per-enemy behavioral trace + defect flags across a run.
pub(crate) struct JankMonitor {
    t: f32,
    n: usize,
    prev_state: Vec<Option<AiState>>,
    trans_times: Vec<Vec<f32>>, // FSM-transition timestamps (for the thrash window)
    stare_secs: Vec<f32>,       // continuous "visible + still + silent" time
    still_secs: Vec<f32>,       // continuous "legs moving but not travelling" time
    still_anchor: Vec<Vec3>,    // where the still streak began
    prev_pos: Vec<Option<Vec3>>,
    packmate_blocks: Vec<u32>,  // frames a packmate capsule occluded the player ray
    overlap_secs: Vec<f32>,     // continuous time interpenetrating a packmate
    ever_fired: Vec<bool>,
    violations: Vec<Violation>,
}

impl JankMonitor {
    fn new(n: usize) -> Self {
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
    fn sample(&mut self, world: &mut World, dt: f32) {
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

    fn flag(&mut self, kind: &'static str, enemy: usize, detail: String) {
        self.violations.push(Violation { kind, enemy, t: self.t, detail });
    }

    /// Diagnostic dump (printed under `cargo test -- --nocapture`).
    fn report(&self) {
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

    fn violations_of(&self, kind: &str) -> Vec<&Violation> {
        self.violations.iter().filter(|v| v.kind == kind).collect()
    }
}

/// Per-5s-bucket evasive (lateral, perpendicular-to-bearing) movement of hunter 0 while
/// the player holds its aim on it for 60 s (invulnerable). The obstacles let it peek
/// from cover. Used to prove the reactive aim-dodge doesn't go quiet over time.
fn aim_hold_lateral(obstacles: &[[f32; 6]]) -> [f32; 12] {
    let mut arena = TestArena::build([60.0, 16.0, 60.0], obstacles, 1, Vec3::new(7.5, 0.0, 4.0));
    arena.place_hunter(0, 7.5, 11.0); // ~7 m — a rifle's standoff band
    let dt = 1.0 / 60.0;
    let mut lateral = [0.0f32; 12];
    let mut prev = arena.world.enemies[0].enemy.pos;
    for f in 0..(12 * 300) {
        let hp = arena.world.enemies[0].enemy.pos + Vec3::new(0.0, 1.0, 0.0);
        arena.aim_player_at(hp); // hold the crosshair on the hunter
        arena.step(dt);
        let p = arena.world.enemies[0].enemy.pos;
        let ppos = arena.world.player_pos().unwrap();
        let bearing = Vec3::new(p.x - ppos.x, 0.0, p.z - ppos.z).normalize_or_zero();
        let perp = Vec3::new(-bearing.z, 0.0, bearing.x);
        let mv = p - prev;
        lateral[(f as usize / 300).min(11)] += (mv.x * perp.x + mv.z * perp.z).abs();
        prev = p;
    }
    lateral
}

// ═══ Scenarios ══════════════════════════════════════════════════════════════

/// The reactive aim-dodge must keep working as long as the player holds aim — including
/// while the hunter peek-fires from cover. Regression for "they evade at first but
/// ~30 s in they stop trying to get out of the way": the dodge was gated to `Attack`,
/// so once a hunter settled into the cover/peek loop it stood still to be shot. It now
/// also jukes in `Peek`, so its evasive movement never goes quiet for a sustained
/// stretch while being aimed at.
#[test]
fn aim_dodge_persists_even_when_peeking_from_cover() {
    let lateral = aim_hold_lateral(&[[28.0, 0.0, 20.0, 4.0, 16.0, 4.0]]); // a pillar to peek from
    // No 3+ consecutive 5 s windows where it barely juked (<6 m) while aimed at.
    let (mut run, mut worst) = (0, 0);
    for &l in &lateral {
        if l < 6.0 {
            run += 1;
            worst = i32::max(worst, run);
        } else {
            run = 0;
        }
    }
    assert!(worst <= 2, "aim-dodge went quiet for {worst} consecutive 5 s windows: {lateral:?}");
}

/// A hunter with a clear, long sightline to a stationary, visible player must ENGAGE
/// (fire), not stand off staring. Targets the "stands off in the distance looking at
/// me but not engaging" report. Single hunter → no packmate occlusion in play.
#[test]
fn long_hall_hunter_engages_a_visible_player() {
    // 60×16×80 WT = 15×4×20 m hall, empty. Player at one end, hunter at the other,
    // clear line between them.
    let mut arena = TestArena::build([60.0, 16.0, 80.0], &[], 1, Vec3::new(7.5, 0.0, 3.0));
    arena.place_hunter(0, 7.5, 17.0); // ~14 m straight down the hall
    let mut mon = JankMonitor::new(1);
    let dt = 1.0 / 60.0;
    for _ in 0..900 {
        // 15 s
        arena.step(dt);
        mon.sample(&mut arena.world, dt);
    }
    mon.report();
    assert!(mon.ever_fired[0], "a hunter with a clear sightline must engage a visible player");
    assert!(mon.violations_of("stall").is_empty(), "hunter stalled/stared instead of engaging");
    assert!(mon.violations_of("illegal_y").is_empty(), "hunter clipped/fell through geometry");
}

/// A player behind a pillar, with a hunter forced to deal with cover, must not
/// oscillate manically. Targets "manic strafing behind a wall."
#[test]
fn hunter_at_cover_does_not_thrash() {
    // 60×16×60 WT room with a 3×3 WT pillar near the middle.
    let pillar = [28.0, 0.0, 28.0, 4.0, 16.0, 4.0];
    let mut arena = TestArena::build([60.0, 16.0, 60.0], &[pillar], 1, Vec3::new(4.0, 0.0, 7.5));
    arena.place_hunter(0, 11.0, 7.5); // ~7 m from the player, same side as the pillar
    let mut mon = JankMonitor::new(1);
    let dt = 1.0 / 60.0;
    for _ in 0..900 {
        arena.step(dt);
        mon.sample(&mut arena.world, dt);
    }
    mon.report();
    assert!(mon.violations_of("thrash").is_empty(), "hunter thrashed states at cover");
    assert!(mon.violations_of("walk_in_place").is_empty(), "hunter danced in place at cover");
    assert!(mon.violations_of("illegal_y").is_empty(), "hunter clipped/fell through geometry");
}

/// A 4-hunter pack on a stationary, fully-visible player: at least one must engage
/// (fire), and no hunter should stall — even though packmate capsules cross each
/// other's perception rays. Targets the self-occlusion "give up / stare" bug. The
/// `packmate_los_blocks` metric localizes it if it trips.
#[test]
fn a_pack_still_engages_despite_self_occlusion() {
    let mut arena = TestArena::build([60.0, 16.0, 60.0], &[], 4, Vec3::new(7.5, 0.0, 4.0));
    // Line the pack up behind one another down the sightline, so they occlude.
    for i in 0..arena.world.enemies.len() {
        arena.place_hunter(i, 7.5, 9.0 + i as f32 * 1.2);
    }
    let mut mon = JankMonitor::new(arena.world.enemies.len());
    let dt = 1.0 / 60.0;
    for _ in 0..900 {
        arena.step(dt);
        mon.sample(&mut arena.world, dt);
    }
    mon.report();
    assert!(mon.ever_fired.iter().any(|&f| f), "at least one hunter in the pack must engage");
    assert!(mon.violations_of("stall").is_empty(), "a hunter stalled/stared (self-occlusion suspect)");
}

/// When the player breaks line-of-sight behind a wall, the hunter should transition
/// cleanly to Investigate/Search and settle — not flicker engage↔disengage. Targets
/// "gives up for a bit." Scripts the player from open into cover mid-run.
#[test]
fn losing_sight_settles_into_search_not_flicker() {
    // A wall at x∈[20,22] WT partly dividing the room, so the player can duck behind it.
    let wall = [20.0, 0.0, 0.0, 2.0, 16.0, 36.0]; // spans z 0..9 m, leaves a gap past it
    let mut arena = TestArena::build([60.0, 16.0, 60.0], &[wall], 1, Vec3::new(7.5, 0.0, 4.0));
    arena.place_hunter(0, 3.0, 4.0);
    let mut mon = JankMonitor::new(1);
    let dt = 1.0 / 60.0;
    // 3 s in the open (hunter acquires), then duck behind the wall for 7 s.
    for f in 0..600 {
        if f == 180 {
            arena.set_player(3.0, 12.0); // step behind the wall (x<5, z past the wall's reach)
        }
        arena.step(dt);
        mon.sample(&mut arena.world, dt);
    }
    mon.report();
    assert!(mon.violations_of("thrash").is_empty(), "hunter flickered states after losing sight");
    assert!(mon.violations_of("illegal_y").is_empty(), "hunter clipped/fell through geometry");
}

/// Peek-a-boo: the player circles a pillar while a hunter tries to fight it. LOS makes
/// and breaks every half-orbit — the classic case that induces the cover/peek/flank
/// interplay to thrash or the hunter to be slow re-engaging on each peek. Targets both
/// "manic strafing behind a wall" and "gives up for a bit."
#[test]
fn a_player_circling_a_pillar_is_handled_cleanly() {
    let pillar = [28.0, 0.0, 28.0, 4.0, 16.0, 4.0]; // centre (7.5,7.5) m, ~1 m square
    let mut arena = TestArena::build([60.0, 16.0, 60.0], &[pillar], 1, Vec3::new(7.5, 0.0, 4.5));
    arena.place_hunter(0, 7.5, 1.0); // south of the pillar; the player orbits it
    let mut mon = JankMonitor::new(1);
    let dt = 1.0 / 60.0;
    let (cx, cz, r) = (7.5f32, 7.5f32, 3.0f32);
    for f in 0..1800 {
        // 30 s ≈ 2.4 orbits
        let ang = 0.5 * (f as f32 * dt); // 0.5 rad/s
        arena.set_player(cx + r * ang.cos(), cz + r * ang.sin());
        arena.step(dt);
        mon.sample(&mut arena.world, dt);
    }
    mon.report();
    assert!(mon.violations_of("thrash").is_empty(), "AI thrashed while the player circled cover");
    assert!(mon.violations_of("walk_in_place").is_empty(), "hunter danced in place");
    assert!(mon.violations_of("stall").is_empty(), "hunter kept losing/failing to re-acquire the peeking player");
    assert!(mon.violations_of("illegal_y").is_empty(), "hunter clipped/fell through geometry");
}

/// The shared "extended run" soak: a 4-hunter pack chases a player that wanders a
/// deterministic pseudo-random path around a wall + a pillar for 40 s — the long
/// session that surfaced the jank live. Returns the monitor for the caller to assert.
fn extended_wander_run() -> JankMonitor {
    extended_wander_run_n(4)
}

fn extended_wander_run_n(wave: usize) -> JankMonitor {
    let obstacles = [
        [24.0, 0.0, 16.0, 4.0, 16.0, 16.0], // a wall
        [40.0, 0.0, 40.0, 4.0, 16.0, 4.0],  // a pillar
    ];
    let mut arena = TestArena::build([64.0, 16.0, 64.0], &obstacles, wave, Vec3::new(4.0, 0.0, 4.0));
    let mut mon = JankMonitor::new(arena.world.enemies.len());
    let dt = 1.0 / 60.0;
    // Deterministic wander: a small LCG picks new targets; the player eases toward them.
    let mut rng: u64 = 0x1234_5678_9abc_def0;
    let next = |lo: f32, hi: f32, rng: &mut u64| {
        *rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        lo + ((*rng >> 33) as f32 / (1u64 << 31) as f32) * (hi - lo)
    };
    let (mut tx, mut tz) = (12.0f32, 12.0f32);
    let (mut px, mut pz) = (4.0f32, 4.0f32);
    for _ in 0..2400 {
        // 40 s — ease toward the target; pick a fresh one on arrival.
        let (dx, dz) = (tx - px, tz - pz);
        let d = (dx * dx + dz * dz).sqrt();
        if d < 0.5 {
            tx = next(2.0, 14.0, &mut rng);
            tz = next(2.0, 14.0, &mut rng);
        } else {
            let step = (3.0f32 * dt).min(d); // ~3 m/s walk
            px += dx / d * step;
            pz += dz / d * step;
        }
        arena.set_player(px, pz);
        arena.step(dt);
        mon.sample(&mut arena.world, dt);
    }
    mon.report();
    mon
}

/// The invariants the extended run must ALWAYS hold: no hunter ever ends up in an
/// illegal (clipped/fallen) position, and none thrashes its FSM. (The walk-in-place
/// defect the run also exposes is tracked separately — see `repro_chase_walk_in_place`.)
#[test]
fn extended_run_holds_the_hard_invariants() {
    let mon = extended_wander_run();
    assert!(mon.violations_of("illegal_y").is_empty(), "a hunter clipped/fell through geometry");
    assert!(mon.violations_of("thrash").is_empty(), "a hunter thrashed states on the long run");
    // ORCA keeps the pack from stacking into one body over the whole soak.
    assert!(mon.violations_of("overlap").is_empty(), "hunters interpenetrated (local avoidance failed)");
}

// ═══ Local avoidance (ORCA) scenarios ═══════════════════════════════════════
// These assert the NEW capability the RVO/ORCA layer adds — hunters steer smoothly
// around one another (and the player) instead of interpenetrating + being shoved
// apart after the fact. They exercise exactly the crowd cases the old position-nudge
// `separate_enemies` handled badly: a tight cluster, and a pack funnelling through a
// narrow gap. The `overlap` detector (added above) is the invariant.

/// A pack spawned almost on top of one another must fan out under ORCA to a
/// non-overlapping ring within a beat — never staying stacked into one body. The ORCA
/// analog of the legacy-path `stacked_hunters_are_pushed_apart` (which the nudge did in
/// a single teleport-y step); ORCA separates smoothly over a few frames, so we assert
/// the settled state + that they never *remained* interpenetrated.
#[test]
fn orca_a_stacked_pack_fans_out_without_interpenetrating() {
    let mut arena = TestArena::build([48.0, 16.0, 48.0], &[], 5, Vec3::new(6.0, 0.0, 6.0));
    // Cram all five into a 0.2 m knot far from the player (isolate the separation).
    for i in 0..arena.world.enemies.len() {
        let a = i as f32 * 1.3;
        arena.place_hunter(i, 9.0 + a.cos() * 0.1, 9.0 + a.sin() * 0.1);
    }
    let mut mon = JankMonitor::new(arena.world.enemies.len());
    let dt = 1.0 / 60.0;
    for _ in 0..300 {
        // 5 s
        arena.step(dt);
        mon.sample(&mut arena.world, dt);
    }
    mon.report();
    // They separated (smallest pairwise gap cleared the interpenetration band)…
    let n = arena.world.enemies.len();
    let mut min_gap = f32::INFINITY;
    for i in 0..n {
        for j in (i + 1)..n {
            let (a, b) = (arena.world.enemies[i].enemy.pos, arena.world.enemies[j].enemy.pos);
            min_gap = min_gap.min(Vec3::new(a.x - b.x, 0.0, a.z - b.z).length());
        }
    }
    assert!(min_gap > OVERLAP_DIST, "the pack fanned out (closest pair {min_gap:.2} m)");
    // …and none stayed interpenetrated for a sustained stretch getting there.
    assert!(mon.violations_of("overlap").is_empty(), "a hunter stayed stacked on a packmate");
    assert!(mon.violations_of("walk_in_place").is_empty(), "a hunter ground in place while separating");
}

/// A whole pack must funnel through a one-doorway gap toward the player instead of
/// jamming shoulder-to-shoulder at the pinch. Under the old nudge this was the worst
/// case (hunters converging on the gap shoved each other sideways into the walls and
/// ground in place); ORCA should queue them through. Assert they cross to the far side
/// with no stacking / grinding / thrash and stay on legal ground.
///
/// **The player is deliberately off-axis from the gap**, so the wall blocks the sightline
/// from the pack's starting corner and crossing is the only way to engage. It used to sit
/// dead in front of the gap, and the pack crossed for the wrong reason: a burst in flight
/// suppressed the Attack score, so a firing hunter stayed in `Chase` and ran the player
/// down through the doorway. That hole is fixed (see `Enemy::util_score`) and a hunter
/// that can see its target now stops at its standoff — which, from the near side of a
/// 7 m-deep room, is short of the wall. Sightline, not distance, is what has to force the
/// funnel, and now it does.
#[test]
fn orca_a_pack_funnels_through_a_doorway() {
    // Room 60×60 WT (15×15 m) split at z∈[28,32] WT by two wall segments leaving a
    // ~1.5 m central gap (x∈[27,33] WT). Player on the far side, pack on the near side.
    let walls = [
        [0.0, 0.0, 28.0, 27.0, 16.0, 4.0],  // left wall  (x 0..6.75 m)
        [33.0, 0.0, 28.0, 27.0, 16.0, 4.0], // right wall (x 8.25..15 m)
    ];
    // Player far side AND off to the left, so the left wall is between it and the pack.
    let mut arena = TestArena::build([60.0, 16.0, 60.0], &walls, 5, Vec3::new(2.0, 0.0, 11.0));
    // Cluster the pack tight on the near side, dead in front of the gap.
    for i in 0..arena.world.enemies.len() {
        let col = (i % 3) as f32 - 1.0;
        let row = (i / 3) as f32;
        arena.place_hunter(i, 7.5 + col * 0.4, 3.0 - row * 0.4);
    }
    let mut mon = JankMonitor::new(arena.world.enemies.len());
    let dt = 1.0 / 60.0;
    for _ in 0..1500 {
        // 25 s
        arena.step(dt);
        mon.sample(&mut arena.world, dt);
    }
    mon.report();
    // At least one hunter made it through the gap to the player's side (z past the wall).
    // Through the gap = past the wall's FAR face. The walls occupy z 28..32 WT = 7..8 m,
    // so 8.2 is "clear of it" with a hand's margin — derived from the geometry rather than
    // picked. It used to be a flat 9.0, which quietly became 5 cm too strict when the
    // engagement distance went 3D and the standoff started holding hunters closer to the
    // pinch: three of five were at z 8.25–8.95, i.e. through, and the assertion said no.
    let crossed = arena.world.enemies.iter().filter(|e| e.enemy.pos.z > 8.2).count();
    let closest = arena
        .world
        .enemies
        .iter()
        .map(|e| e.enemy.pos.distance(arena.world.player_pos().unwrap()))
        .fold(f32::MAX, f32::min);
    assert!(
        crossed >= 1,
        "at least one hunter funnelled through the gap (crossed={crossed}, closest {closest:.2} m)",
    );
    // The property the crossing is a proxy for: the pack got to the player. Stated
    // directly so a geometry change cannot make the test vacuous.
    assert!(closest < 6.0, "the pack pressured the player through the pinch ({closest:.2} m)");
    assert!(mon.violations_of("overlap").is_empty(), "hunters jammed/stacked at the pinch");
    assert!(mon.violations_of("walk_in_place").is_empty(), "a hunter ground in place at the pinch");
    assert!(mon.violations_of("thrash").is_empty(), "a hunter thrashed states at the pinch");
    assert!(mon.violations_of("illegal_y").is_empty(), "a hunter clipped/fell through geometry");
}

/// The player-as-obstacle half of ORCA: a pack converging on a stationary player must
/// ring up around it rather than piling onto its exact cell (they still don't collide
/// with the player — the standoff owns spacing — but avoidance keeps them from
/// converging *through* it). Assert engagement holds with no stacking.
#[test]
fn orca_a_converging_pack_rings_the_player_without_stacking() {
    let mut arena = TestArena::build([48.0, 16.0, 48.0], &[], 5, Vec3::new(6.0, 0.0, 6.0));
    // Line the pack up behind one another charging straight at the player.
    for i in 0..arena.world.enemies.len() {
        arena.place_hunter(i, 6.0, 10.0 + i as f32 * 0.6);
    }
    let mut mon = JankMonitor::new(arena.world.enemies.len());
    let dt = 1.0 / 60.0;
    for _ in 0..900 {
        // 15 s
        arena.step(dt);
        mon.sample(&mut arena.world, dt);
    }
    mon.report();
    assert!(mon.ever_fired.iter().any(|&f| f), "the converging pack engages the player");
    assert!(mon.violations_of("overlap").is_empty(), "hunters stacked converging on the player");
}

// ─── Utility-AI decision layer (roadmap #4) ──────────────────────────────────

/// The utility layer (default ON) engages a visible player, fires, and HOLDS a standoff
/// — it never runs the player down to point-blank. (Regression for the scoring tie where
/// `Chase`+inertia matched `Attack` and the hunter closed to point-blank, firing, forever
/// instead of planting at its standoff.) Open room, so there's no cover cycle to pull it
/// in — pure standoff behaviour.
#[test]
fn utility_engages_holds_standoff_and_fires() {
    let mut arena = TestArena::build([60.0, 16.0, 60.0], &[], 1, Vec3::new(7.5, 0.0, 4.0));
    assert!(arena.world.utility_ai(), "utility AI is on by default");
    arena.place_hunter(0, 7.5, 11.0); // ~7 m — a rifle's standoff band
    let mut mon = JankMonitor::new(1);
    let dt = 1.0 / 60.0;
    let mut min_dist = f32::MAX;
    for _ in 0..1200 {
        arena.step(dt);
        mon.sample(&mut arena.world, dt);
        let d = arena.world.enemies[0]
            .enemy
            .pos
            .distance(arena.world.player_pos().unwrap());
        min_dist = min_dist.min(d);
    }
    mon.report();
    assert!(mon.ever_fired[0], "a utility hunter engages + fires");
    assert!(mon.violations_of("stall").is_empty(), "utility hunter stalled");
    assert!(mon.violations_of("illegal_y").is_empty(), "utility hunter clipped geometry");
    assert!(
        min_dist > 2.0,
        "utility hunter holds a standoff, not point-blank (min {min_dist:.2} m)"
    );
}

/// The `utility_ai` kill-switch drops back to the legacy FSM, which still engages + fires
/// — so the pre-utility baseline is one setter away (A/B + regression safety).
#[test]
fn utility_kill_switch_runs_the_fsm() {
    let mut arena = TestArena::build([60.0, 16.0, 60.0], &[], 1, Vec3::new(7.5, 0.0, 4.0));
    arena.world.set_utility_ai(false);
    assert!(!arena.world.utility_ai(), "kill-switch disables the utility layer");
    arena.place_hunter(0, 7.5, 11.0);
    let mut mon = JankMonitor::new(1);
    let dt = 1.0 / 60.0;
    for _ in 0..900 {
        arena.step(dt);
        mon.sample(&mut arena.world, dt);
    }
    mon.report();
    assert!(mon.ever_fired[0], "the FSM kill-switch still engages + fires");
    assert!(mon.violations_of("stall").is_empty(), "FSM hunter stalled");
}

// ─── Tracked defect repros (the lab caught these; un-ignore each when fixing) ────

/// FIXED 2026-07-27: a hunter with a clear line-of-sight to the player at engage
/// range, in `Search`, now spots them — searching hunters use a wide (200°) perception
/// arc ([`SEARCH_HALF_CONE`]) instead of the unaware guard's 120° cone, so a plainly-
/// visible player no longer goes ignored ("stands off looking at me but not engaging").
#[test]
fn search_spots_a_plainly_visible_player() {
    let pillar = [28.0, 0.0, 28.0, 4.0, 16.0, 4.0];
    let mut arena = TestArena::build([60.0, 16.0, 60.0], &[pillar], 1, Vec3::new(4.0, 0.0, 7.5));
    arena.place_hunter(0, 11.0, 7.5);
    let mut mon = JankMonitor::new(1);
    let dt = 1.0 / 60.0;
    for _ in 0..900 {
        arena.step(dt);
        mon.sample(&mut arena.world, dt);
    }
    mon.report();
    assert!(mon.violations_of("stall").is_empty(), "searcher stalled with the player in plain sight");
}

/// KNOWN DEFECT (un-ignore when fixed): during the extended run a chasing hunter
/// reports its legs moving while barely travelling — "manic strafing / stuck in
/// place," likely a hunter pinned against geometry or a flank target that oscillates.
/// FIXED 2026-07-27: a crowded hunter no longer grinds a walk cycle in place. The
/// pack's separation nudge used to fight `move_toward` when hunters converged on one
/// spot, so they jockeyed at full walk-speed travelling ~nothing ("manic strafing").
/// Now each hunter reports its ACTUAL post-separation travel (legs idle when settled)
/// and briefly HOLDS when it's been held in place — so the crowd settles into a ring.
#[test]
fn a_crowded_pack_settles_without_grinding_in_place() {
    let mon = extended_wander_run();
    assert!(
        mon.violations_of("walk_in_place").is_empty(),
        "a hunter's legs moved but it barely travelled (walk-in-place)"
    );
}

// ─── PD simulant lab scenarios ───────────────────────────────────────────────
//
// These are the ones that matter for judging the port, because they measure the
// property the whole model exists to produce: **accuracy that emerges from where
// the barrel is pointing**, with no hit roll anywhere in the chain.
//
// If the zeroing model were broken — the aim error stuck at zero, or the tier
// table read wrong — `emit_pd_shot` would still fire, still raycast, and still
// deal damage. Nothing would fail except the *shape* of the outcome. So these
// scenarios assert the shape.

/// A dead-simple duel: player and one simulant in an empty box at a fixed range,
/// with the player standing still in plain sight. Returns how long the simulant
/// took to kill the player, or `None` if it failed to inside `secs`.
///
/// Time-to-kill rather than damage-in-a-window, because the player only has 100
/// HP: any tier competent enough to empty that pool inside the window looks
/// identical to any other, and the measurement silently stops discriminating.
fn pd_time_to_kill(tier: crate::pdsim::difficulty::BotDifficulty, secs: f32) -> Option<f32> {
    // 40 WT (10 m) apart in a 60 WT box: far enough that a couple of degrees of
    // aim error is the difference between a hit and a miss, close enough to be
    // well inside every weapon's range.
    let mut arena = TestArena::build_pd([60.0, 16.0, 60.0], &[], 1, Vec3::new(7.5, 0.0, 2.5), tier);
    arena.set_player(7.5, 2.5);
    arena.place_hunter(0, 7.5, 12.5);
    let dt = 1.0 / 60.0;
    for i in 0..(secs / dt) as usize {
        // Hold the player still and in the open — no dodging, no cover. Any
        // difference between tiers is then purely the aim model.
        arena.set_player(7.5, 2.5);
        arena.step(dt);
        if arena.world.is_player_dead() {
            return Some(i as f32 * dt);
        }
    }
    None
}

/// **The headline claim.** PD simulants never roll a hit chance, yet difficulty
/// still has to mean something. If the zeroing model works, a DarkSim (zero aim
/// error, no reaction delay) must kill a stationary target in the open far faster
/// than a MeatSim (up to ~30° of wander, a 1.5 s reaction) — purely because its
/// gun is actually pointed at the player.
#[test]
fn pd_accuracy_emerges_from_aim_rather_than_a_dice_roll() {
    use crate::pdsim::difficulty::BotDifficulty;
    const WINDOW: f32 = 30.0;
    let dark = pd_time_to_kill(BotDifficulty::Dark, WINDOW);
    let meat = pd_time_to_kill(BotDifficulty::Meat, WINDOW);
    println!("PD time-to-kill: dark {dark:?}, meat {meat:?}");

    let dark = dark.expect("a DarkSim must kill a stationary target in the open");
    match meat {
        // A MeatSim failing to land a kill at all inside 30 s is the strongest
        // possible version of this result.
        None => {}
        Some(meat) => assert!(
            meat > dark * 2.0,
            "difficulty must separate on aim alone: dark killed in {dark:.1}s, meat in {meat:.1}s"
        ),
    }
}

/// The reaction delay has to be real, not cosmetic. A MeatSim waits 1.5 s between
/// seeing you and shooting; a DarkSim waits none. Measure when the first shot
/// leaves the barrel.
#[test]
fn pd_reaction_delay_gates_the_first_shot() {
    use crate::pdsim::difficulty::BotDifficulty;

    let first_shot = |tier| {
        let mut arena =
            TestArena::build_pd([60.0, 16.0, 60.0], &[], 1, Vec3::new(7.5, 0.0, 2.5), tier);
        arena.set_player(7.5, 2.5);
        arena.place_hunter(0, 7.5, 9.0);
        let dt = 1.0 / 60.0;
        for i in 0..(5.0 / dt) as usize {
            arena.set_player(7.5, 2.5);
            arena.step(dt);
            if arena.world.enemies.first().is_some_and(|e| e.fire_elapsed.is_some()) {
                return Some(i as f32 * dt);
            }
        }
        None
    };

    let dark = first_shot(BotDifficulty::Dark).expect("a DarkSim must open fire within 5 s");
    let meat = first_shot(BotDifficulty::Meat).expect("a MeatSim must open fire within 5 s");
    println!("first shot: dark at {dark:.2}s, meat at {meat:.2}s");
    // A MeatSim owes a 1.5 s reaction; a DarkSim owes none, so it fires as soon as
    // it has swung round to face. Note the two costs *overlap* — the shoot-delay
    // clock runs while the body is still turning — so the gap between them is less
    // than the full 1.5 s. Assert each against what it actually owes.
    let owed = BotDifficulty::Meat.tuning().shoot_delay;
    assert!(
        meat >= owed - 1.0 / 60.0,
        "a MeatSim fired at {meat:.2}s, before its {owed:.2}s reaction was served"
    );
    assert!(dark < owed * 0.75, "a DarkSim should not wait a MeatSim's reaction (fired at {dark:.2}s)");
}

/// A simulant's body yaw must actually carry the aim error — that is what makes
/// the model legible on screen and what `emit_pd_shot` fires along. A MeatSim
/// tracking a stationary target should be visibly off it much of the time; a
/// DarkSim should be locked on.
#[test]
fn pd_body_yaw_carries_the_aim_error() {
    use crate::pdsim::difficulty::BotDifficulty;

    let worst_error = |tier| {
        let mut arena =
            TestArena::build_pd([60.0, 16.0, 60.0], &[], 1, Vec3::new(7.5, 0.0, 2.5), tier);
        arena.set_player(7.5, 2.5);
        arena.place_hunter(0, 7.5, 9.0);
        let dt = 1.0 / 60.0;
        let mut worst = 0.0f32;
        for i in 0..(8.0 / dt) as usize {
            arena.set_player(7.5, 2.5);
            arena.step(dt);
            // Ignore the opening second, while it is still swinging round to face.
            if i as f32 * dt > 1.0 {
                if let Some(d) = arena.world.pd_debug().first() {
                    worst = worst.max(d.aim_error_deg.abs());
                }
            }
        }
        worst
    };

    let meat = worst_error(BotDifficulty::Meat);
    let dark = worst_error(BotDifficulty::Dark);
    println!("worst aim error: meat {meat:.1}deg, dark {dark:.2}deg");
    assert!(meat > 5.0, "a MeatSim should wander well off target, got {meat:.1}deg");
    assert!(dark < 0.01, "a DarkSim should never leave the target, got {dark:.3}deg");
}

// ─── PD omniscience (the knowledge rule) ─────────────────────────────────────
//
// Perfect Dark's simulants come and find you. `bot_choose_general_target`
// (`bot.c:1589`) hands a bot the closest out-of-sight opponent when nobody is in
// sight, and `botcmd_tick_dist_mode` (`botcmd.c:39`) then re-issues
// `chr_go_to_prop(chr, targetprop, GOPOSFLAG_RUN)` against that target's LIVE
// position — there is no last-known-position anywhere in the simulant path. These
// scenarios assert we reproduce that (and that the kill-switch restores our own
// perceive-then-remember behaviour).

/// The arena for the omniscience scenarios: a 15×15 m room split by a wall with one
/// ~1.5 m gap, the player on the far side, and hunter 0 tucked into a near-side corner
/// with **no line of sight** — the straight line to the player crosses the wall. To
/// reach the player it has to know where the player is, walk to the gap, and come
/// through. Returns `(arena, player_xz)`; the player is invulnerable so a long run
/// can't end early with a kill.
fn omniscience_arena(pd: bool) -> (TestArena, Vec3) {
    let walls = [
        [0.0, 0.0, 28.0, 27.0, 16.0, 4.0],  // left wall  (x 0..6.75 m)
        [33.0, 0.0, 28.0, 27.0, 16.0, 4.0], // right wall (x 8.25..15 m)
    ];
    let player = Vec3::new(11.0, 0.0, 12.0);
    let mut arena = if pd {
        TestArena::build_pd(
            [60.0, 16.0, 60.0],
            &walls,
            1,
            player,
            crate::pdsim::difficulty::BotDifficulty::Normal,
        )
    } else {
        TestArena::build([60.0, 16.0, 60.0], &walls, 1, player)
    };
    if pd {
        arena.world.toggle_invulnerable(); // `build_pd` leaves the player killable
    }
    arena.place_hunter(0, 2.0, 3.0); // near-side corner, wall between it and the player
    (arena, player)
}

/// Run the omniscience arena for `secs` and report `(closest approach, whether it ever
/// went blind-hunting)`. "Blind-hunting" = the hunter reached `Search` or `Investigate`,
/// which is exactly what an omniscient one must never do — it has lost nothing to look
/// for.
fn omniscience_run(arena: &mut TestArena, secs: f32, player: Vec3) -> (f32, bool) {
    let dt = 1.0 / 60.0;
    let mut closest = f32::MAX;
    let mut searched = false;
    for _ in 0..(secs / dt) as usize {
        arena.set_player(player.x, player.z); // hold them put; only the hunter moves
        arena.step(dt);
        let Some(e) = arena.world.enemies.first() else { break };
        let p = e.enemy.pos;
        closest = closest.min(Vec3::new(p.x - player.x, 0.0, p.z - player.z).length());
        if matches!(e.enemy.state(), AiState::Search | AiState::Investigate) {
            searched = true;
        }
    }
    (closest, searched)
}

/// **The headline claim.** A PD hunter that cannot see the player still walks to them:
/// it knows the live position, paths through the gap in the wall, and closes to its
/// weapon standoff — never once dropping into the fan-out search.
#[test]
fn pd_omniscient_hunter_finds_a_player_it_cannot_see() {
    let (mut arena, player) = omniscience_arena(true);
    assert!(arena.world.pd_omniscience(), "PD omniscience is on by default");
    // No sightline at the start — otherwise this proves nothing.
    let start = arena.world.enemies[0].enemy.pos;
    assert!(
        !crate::enemy::perception_los(&mut arena.world.physics, start, player),
        "the arena must start with the wall between hunter and player"
    );
    let (closest, searched) = omniscience_run(&mut arena, 20.0, player);
    println!("PD omniscient: closest approach {closest:.2} m, ever searched: {searched}");
    assert!(
        closest < 6.0,
        "an omniscient hunter must come and find the player it cannot see (got no closer than {closest:.2} m)"
    );
    assert!(!searched, "an omniscient hunter has nothing to search for, yet it entered Search/Investigate");
}

/// The kill-switch restores our own knowledge rule: the same PD hunter in the same
/// arena, with omniscience off, never perceives the player and falls back to the
/// fan-out sweep. This is the A/B that proves the behaviour above is the new policy
/// and not the arena handing it a free sightline.
#[test]
fn pd_omniscience_kill_switch_restores_the_search() {
    let (mut arena, player) = omniscience_arena(true);
    arena.world.set_pd_omniscience(false);
    assert!(!arena.world.pd_omniscience(), "the kill-switch disables omniscience");
    let (_, searched) = omniscience_run(&mut arena, 20.0, player);
    assert!(searched, "without omniscience a blind hunter must fall back to Search/Investigate");
}

/// **Omniscience is family-agnostic**, and the only thing gating it is the kill-switch.
///
/// This test used to assert the opposite — that a GoldenEye hunter was *never*
/// omniscient, because the policy was gated per hunter on `pdsim.is_some()` and only lab
/// hunters carried a simulant. Every hunter carries one now (§17), so that gate is
/// vestigial and the knowledge rule reaches the whole roster. Asserted against a
/// **forced-GoldenEye** wave, because that is the case the old gate would have excluded:
/// the body it wears has nothing to do with what it knows.
#[test]
fn omniscience_reaches_a_goldeneye_bodied_hunter_too() {
    let walls = [
        [0.0, 0.0, 28.0, 27.0, 16.0, 4.0],
        [33.0, 0.0, 28.0, 27.0, 16.0, 4.0],
    ];
    let player = Vec3::new(11.0, 0.0, 12.0);
    let mut world = World::new();
    world.set_body_set(super::BodySet::GoldenEye);
    let mut region = Region::new(0);
    region.brushes.push(Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 60.0, 16.0, 60.0));
    for (k, o) in walls.iter().enumerate() {
        region
            .brushes
            .push(Brush::new(2 + k as u32, Op::Add, o[0], o[1], o[2], o[3], o[4], o[5]));
    }
    world.regions = vec![region];
    world.set_difficulty(DIFFICULTY_MAX);
    world.set_wave_size(1);
    world.camera.pos = Vec3::new(player.x, 1.5, player.z);
    world.initial_meshes();
    world.toggle_mode();
    world.toggle_invulnerable();
    let floor_y = world.player_pos().map(|p| p.y).unwrap_or(0.0);
    let mut arena = TestArena { world, floor_y };
    assert!(
        arena.world.enemies[0].pdsim.is_some(),
        "every hunter carries a simulant now, whatever body it wears"
    );
    assert!(
        arena.world.ge_bodies().contains(&arena.world.enemies[0].body),
        "…and this one wears a GoldenEye body (it is on PD's clips like everything else)",
    );
    arena.place_hunter(0, 2.0, 3.0); // near-side corner, wall between it and the player

    assert!(arena.world.pd_omniscience(), "the world flag is on");
    let (closest, searched) = omniscience_run(&mut arena, 20.0, player);
    assert!(
        arena.world.enemies[0].enemy.is_omniscient(),
        "the knowledge policy reaches it regardless of family"
    );
    assert!(closest < 6.0, "and it comes to find the player (got no closer than {closest:.2} m)");
    assert!(!searched, "an omniscient hunter has nothing to search for");
}

// ─── Per-shot spread + burst cadence (why an automatic isn't an instant kill) ───
//
// Reported from playtest: "when they have an automatic weapon, every single bullet
// hits me in a row and I die almost instantly." Both halves of that are real defects
// against Perfect Dark, and they have different causes:
//
// * **Every bullet in a row.** The zeroing model is a damped random walk whose
//   increment is held for a third to two thirds of a second, so it barely moves across
//   a burst — a burst was one ray fired repeatedly. PD offsets every individual round
//   by `bgun_calculate_bot_shot_spread`, so a burst is a pattern.
// * **Almost instantly.** PD's automatics are `FUNCFLAG_BURST3` rows: three rounds
//   `nextbullettimer60 = 5` ticks apart, then a `6 + 18 = 24` tick pause. We fired a
//   flat, gapless stream.

/// Fire a simulant at a stationary player at its standoff and return the intervals (s)
/// between successive rounds leaving the barrel. Rising edges of `muzzle_timer` count
/// shots — it is set on every shot, hit or miss, so this measures the trigger and not
/// the damage.
fn pd_shot_intervals(tier: crate::pdsim::difficulty::BotDifficulty, secs: f32) -> Vec<f32> {
    let mut arena = TestArena::build_pd([60.0, 16.0, 60.0], &[], 1, Vec3::new(7.5, 0.0, 2.5), tier);
    arena.world.toggle_invulnerable(); // measure the whole window, don't die partway
    arena.set_player(7.5, 2.5);
    arena.place_hunter(0, 7.5, 9.5); // 7 m — a rifle's standoff band
    let dt = 1.0 / 60.0;
    let (mut intervals, mut since, mut prev_muzzle, mut first) = (Vec::new(), 0.0f32, 0.0f32, true);
    for _ in 0..(secs / dt) as usize {
        arena.set_player(7.5, 2.5);
        arena.step(dt);
        since += dt;
        let m = arena.world.enemies[0].muzzle_timer;
        if m > prev_muzzle {
            if !first {
                intervals.push(since);
            }
            first = false;
            since = 0.0;
        }
        prev_muzzle = m;
    }
    intervals
}

/// A PD simulant with an automatic must fire in **bursts with gaps**, not a continuous
/// stream — PD's `FUNCFLAG_BURST3`. Asserts the rhythm directly: the intervals between
/// rounds fall into two populations (tight, then a pause), and no more than
/// [`PD_BURST_ROUNDS`] rounds ever leave back-to-back without one.
///
/// This is the regression for "I die almost instantly": the gap is the player's window
/// to break line of sight, and it roughly halves sustained incoming DPS.
#[test]
fn pd_an_automatic_fires_in_bursts_not_a_continuous_stream() {
    use crate::pdsim::difficulty::BotDifficulty;
    let intervals = pd_shot_intervals(BotDifficulty::Dark, 12.0);
    assert!(intervals.len() >= 12, "not enough shots to judge the rhythm: {intervals:?}");
    // A "pause" is anything near the authored inter-burst gap; anything much shorter is
    // inside a burst. The two populations must both exist.
    let pause = PD_BURST_GAP * 0.9;
    let pauses = intervals.iter().filter(|&&d| d >= pause).count();
    let tight = intervals.iter().filter(|&&d| d < pause).count();
    println!("shot intervals ({} shots): {intervals:?}", intervals.len() + 1);
    assert!(pauses > 0, "an automatic never paused — it hosed continuously");
    assert!(tight > 0, "no rounds came close together — the burst itself is missing");
    // The longest back-to-back run must respect the burst length.
    let mut run = 1usize;
    let mut worst = 1usize;
    for &d in &intervals {
        if d < pause {
            run += 1;
            worst = worst.max(run);
        } else {
            run = 1;
        }
    }
    assert!(
        worst <= PD_BURST_ROUNDS as usize,
        "{worst} rounds left back-to-back with no pause; PD's burst is {PD_BURST_ROUNDS}"
    );
}

/// **Where the misses actually come from.** Measured, because the intuition is wrong.
///
/// PD's per-shot spread is *narrow*: a rifle's `spread` of 8 is ±2° worst case and
/// ~0.67° RMS, which at a 7 m standoff is a few centimetres against a 0.35 m torso. So
/// a DarkSim — zero zeroing error by definition — really does land nearly every round,
/// in Perfect Dark as much as here. Spread is what stops a burst being *one ray fired
/// repeatedly*; it is not an accuracy tax, and it is not what keeps you alive.
///
/// What keeps you alive is the **zeroing error**, and it is enormous by comparison: a
/// NormalSim wanders 4–8°, an order of magnitude past the weapon cone. This asserts that
/// separation of responsibilities — the tier dominates the hit fraction, the weapon does
/// not — which is the property the whole zeroing port exists to produce.
#[test]
fn pd_the_hit_fraction_is_set_by_tier_not_by_the_weapon() {
    use crate::pdsim::difficulty::BotDifficulty;

    // Rounds fired / rounds landed for one simulant on a stationary player at 7 m.
    let engage = |tier| {
        let mut arena =
            TestArena::build_pd([60.0, 16.0, 60.0], &[], 1, Vec3::new(7.5, 0.0, 2.5), tier);
        arena.set_player(7.5, 2.5);
        arena.place_hunter(0, 7.5, 9.5);
        let weapon = arena.world.enemies[0].weapon;
        let dt = 1.0 / 60.0;
        let (mut fired, mut landed, mut prev_muzzle) = (0u32, 0u32, 0.0f32);
        let mut hp = arena.world.player_health();
        for _ in 0..(30.0 / dt) as usize {
            arena.set_player(7.5, 2.5);
            arena.step(dt);
            let m = arena.world.enemies[0].muzzle_timer;
            if m > prev_muzzle {
                fired += 1;
            }
            prev_muzzle = m;
            let now = arena.world.player_health();
            if now < hp {
                landed += ((hp - now) / weapon.damage).round().max(1.0) as u32;
                hp = now;
            }
            if arena.world.is_player_dead() {
                break;
            }
        }
        (fired, landed)
    };

    let (df, dl) = engage(BotDifficulty::Dark);
    let (nf, nl) = engage(BotDifficulty::Normal);
    let dark = dl as f32 / df.max(1) as f32;
    let normal = nl as f32 / nf.max(1) as f32;
    println!("hit fraction at 7 m — dark {dl}/{df} = {dark:.2}, normal {nl}/{nf} = {normal:.2}");
    assert!(dark > 0.9, "a DarkSim's gun is genuinely on target; it should hit ~everything ({dark:.2})");
    assert!(
        normal < dark * 0.8,
        "the TIER must dominate the hit fraction: normal {normal:.2} vs dark {dark:.2}"
    );
}

/// A PeaceSim must never pull the trigger, however lethal its tier. This is the
/// personality axis proving it is genuinely orthogonal to difficulty: a Dark
/// PeaceSim is a perfect shot that refuses to shoot.
#[test]
fn pd_a_dark_pacifist_never_fires() {
    let mut world = World::new();
    let mut region = Region::new(0);
    region.brushes.push(Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 60.0, 16.0, 60.0));
    world.regions = vec![region];
    world.set_difficulty(DIFFICULTY_MAX);
    world.enable_pd_lab(super::pd_lab::PdLabConfig {
        count: 1,
        difficulty: Some(crate::pdsim::difficulty::BotDifficulty::Dark),
        bot_type: crate::pdsim::personality::BotType::Peace,
        free_for_all: false,
    });
    world.camera.pos = Vec3::new(7.5, 1.5, 2.5);
    world.initial_meshes();
    world.toggle_mode();
    let floor_y = world.player_pos().map(|p| p.y).unwrap_or(0.0);
    if let Some(inst) = world.enemies.get_mut(0) {
        inst.enemy.pos = Vec3::new(7.5, floor_y, 9.0);
        let c = inst.collider;
        world.physics.update_enemy_collider(c, inst.enemy.pos);
    }
    let start = world.player_health();
    let dt = 1.0 / 60.0;
    let input = InputState::default();
    for _ in 0..(8.0 / dt) as usize {
        world.fixed_step(dt, &input);
        world.enemy_combat_step(dt);
    }
    assert_eq!(
        world.player_health(),
        start,
        "a PeaceSim landed damage — the personality veto is not reaching the trigger"
    );
}

// ═══ `AI=pd` vs `AI=ours` — the engagement-model A/B ════════════════════════
//
// The switch is a full alternative hunter, not a knob, so what it needs is not pass/fail
// invariants but **comparative measurements**: the two models are both correct, and the
// question a playtest is being set up to answer is which one feels better. These
// scenarios produce the numbers that make that argument concrete, and assert only the
// properties that would mean the port is *wrong* rather than merely different.
//
// The headline claim to check is the one that changes how a firefight reads: a Perfect
// Dark bot in `BOTDISTMODE_OK` **stands still and shoots** (`chr_try_stop`), where ours
// weaves between bursts and never stops moving. If PD mode still orbits, the distance
// mode is not driving the feet and everything downstream of it is theatre.
//
// CPU-side green says nothing about feel — every AI defect this project has had passed
// the full suite — so these are a floor, not a verdict.

/// One run of the A/B, from hunter 0's point of view.
#[derive(Debug, Clone, Copy)]
struct AbMetrics {
    /// Seconds of the run the hunter had line of sight to the player. Every fraction
    /// below is measured over these frames only: how a hunter behaves *while fighting*
    /// is the question, and the walk-in from spawn is common to both models.
    engaged_s: f32,
    /// Fraction of engaged time the feet were stationary. PD's `OK` mode is the only
    /// thing in either model that plants them.
    still_frac: f32,
    /// Fraction of engaged time the hunter held a distance inside its weapon's
    /// `g_BotDistConfigs` band.
    in_band_frac: f32,
    /// Fraction of engaged time in `BOTDISTMODE_OK` — the mode that issues
    /// `chr_try_stop`. Reads the ported state directly rather than inferring it from
    /// the feet. Always 0 under `AI=ours`, which never ticks the distance mode.
    ok_frac: f32,
    /// Mean |distance − band centre| (m) while engaged.
    band_err_m: f32,
    /// Total lateral (perpendicular-to-bearing) travel, m — the orbiting metric.
    /// Circling the player racks this up; closing head-on and stopping does not.
    lateral_m: f32,
    /// Seconds until the player died, or `None` if they survived the run.
    time_to_kill: Option<f32>,
    /// Damage dealt to the player over the run (HP).
    damage: f32,
    /// Whether the hunter was ever caught mid-reload (PD's rule; `AI=ours` never
    /// reloads at all).
    reloaded: bool,
}

/// Run one hunter against a stationary player for `secs` under `mode`, and measure it.
///
/// The arena is a 15 m room with a single pillar — enough geometry for line of sight to
/// be a real question, little enough that neither model is fighting the level. The
/// hunter starts 9 m out, past every weapon band, so both models have to close first.
/// `invulnerable` picks what the run is for: movement metrics want a fight that lasts
/// the whole window, time-to-kill wants one that ends.
fn ab_run(mode: crate::enemy::AiMode, secs: f32, invulnerable: bool) -> AbMetrics {
    use crate::pdsim::difficulty::BotDifficulty;
    let pillar = [28.0, 0.0, 28.0, 4.0, 16.0, 4.0]; // centre of the room
    let (px, pz) = (7.5f32, 4.0f32);
    let mut arena = TestArena::build_pd(
        [60.0, 16.0, 60.0],
        &[pillar],
        1,
        Vec3::new(px, 0.0, pz),
        BotDifficulty::Normal,
    );
    arena.world.set_ai_mode(mode);
    if invulnerable {
        arena.world.toggle_invulnerable(); // `build_pd` leaves the player killable
    }
    arena.place_hunter(0, px, pz + 9.0);
    let band = crate::combat::enemy_weapons::dist_band_for(
        &arena.world.enemies[0].weapon,
        arena.world.enemies[0].use_secondary,
    );

    let dt = 1.0 / 60.0;
    let (mut engaged_s, mut still_s, mut in_band_s, mut err_sum, mut lateral_m) =
        (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let mut ok_s = 0.0f32;
    let (mut ttk, mut reloaded) = (None, false);
    let start_hp = arena.world.player_health();
    let mut prev = arena.world.enemies[0].enemy.pos;
    let mut t = 0.0f32;
    for _ in 0..(secs / dt) as usize {
        arena.set_player(px, pz); // hold them put; only the hunter moves
        arena.step(dt);
        t += dt;
        let Some(ppos) = arena.world.player_pos() else { break };
        let inst = &arena.world.enemies[0];
        let (epos, speed) = (inst.enemy.pos, inst.enemy.speed());
        if inst.reload_timer > 0.0 {
            reloaded = true;
        }
        if arena.world.is_player_dead() {
            // Stop here: hunters keep manoeuvring against a corpse but never fire, so
            // every frame past this point would drag the movement metrics toward
            // "always moving" and say nothing about how the fight was fought.
            ttk = Some(t);
            break;
        }
        let los = crate::enemy::perception_los(&mut arena.world.physics, epos, ppos);
        if los {
            let dist = (ppos - epos).length();
            engaged_s += dt;
            if speed < 0.15 {
                still_s += dt;
            }
            if band.contains(dist) {
                in_band_s += dt;
            }
            if mode.is_pd() && inst.enemy.dist_mode() == crate::pdsim::distmode::DistMode::Ok {
                ok_s += dt;
            }
            err_sum += (dist - band.centre_m()).abs() * dt;
            // Lateral travel: the component of this step perpendicular to the bearing
            // from the player — what orbiting is made of.
            let bearing = Vec3::new(epos.x - ppos.x, 0.0, epos.z - ppos.z).normalize_or_zero();
            let perp = Vec3::new(-bearing.z, 0.0, bearing.x);
            let mv = epos - prev;
            lateral_m += (mv.x * perp.x + mv.z * perp.z).abs();
        }
        prev = epos;
    }
    let over = engaged_s.max(1e-6);
    AbMetrics {
        engaged_s,
        still_frac: still_s / over,
        in_band_frac: in_band_s / over,
        ok_frac: ok_s / over,
        band_err_m: err_sum / over,
        lateral_m,
        time_to_kill: ttk,
        damage: start_hp - arena.world.player_health(),
        reloaded,
    }
}

/// Print both models' numbers side by side. The A/B is the deliverable here; the
/// assertions below only fence off the ways the port could be broken.
fn ab_report(label: &str, ours: &AbMetrics, pd: &AbMetrics) {
    eprintln!("── AI A/B: {label} ──");
    for (name, m) in [("ours", ours), ("pd", pd)] {
        eprintln!(
            "  {name:>4}: engaged {:.1}s  still {:.0}%  in-band {:.0}%  OK-mode {:.0}%  \
             band-err {:.2}m  lateral {:.1}m  ttk {}  dmg {:.0}  reloaded {}",
            m.engaged_s,
            m.still_frac * 100.0,
            m.in_band_frac * 100.0,
            m.ok_frac * 100.0,
            m.band_err_m,
            m.lateral_m,
            m.time_to_kill.map_or("—".to_string(), |t| format!("{t:.1}s")),
            m.damage,
            m.reloaded,
        );
    }
}

/// **The headline: `AI=pd` plants its feet and never weaves.**
///
/// `botcmd_tick_dist_mode`'s `BOTDISTMODE_OK` issues `chr_try_stop` — a bot inside its
/// weapon's band with a sightline stops moving and shoots. This asserts it against the
/// ported state (`ok_frac`) as well as against the feet, so a pass means the distance
/// mode is genuinely driving movement rather than the hunter happening to settle.
///
/// **A measured surprise, and the reason the assertions are shaped the way they are.**
/// Against a *stationary* player in the open our own hunter is nearly as stationary as
/// PD's — around 94% of engaged time, because it reaches its standoff and its
/// burst-and-reposition jukes are short. Standing still is therefore *not* what separates
/// the two models in this scenario. **Lateral travel is**: ours weaves several metres
/// around the bearing, PD's does not move sideways at all. So the discriminating
/// assertion is the orbit metric, not the stillness one — the stillness figure is
/// asserted absolutely (PD must be still) rather than comparatively.
#[test]
fn pd_mode_stands_still_in_band_where_ours_weaves() {
    use crate::enemy::AiMode;
    let ours = ab_run(AiMode::Ours, 20.0, true);
    let pd = ab_run(AiMode::Pd, 20.0, true);
    ab_report("stationary player, 20 s, invulnerable", &ours, &pd);
    assert!(pd.engaged_s > 5.0, "the PD run barely engaged ({:.1}s) — arena problem", pd.engaged_s);
    assert!(ours.engaged_s > 5.0, "the ours run barely engaged ({:.1}s)", ours.engaged_s);
    assert!(
        pd.ok_frac > 0.8,
        "a PD hunter spent only {:.0}% of the fight in BOTDISTMODE_OK — it should close \
         once and then hold",
        pd.ok_frac * 100.0
    );
    assert!(
        pd.still_frac > 0.9,
        "…and OK means chr_try_stop, so its feet must be still: got {:.0}%",
        pd.still_frac * 100.0
    );
    assert!(
        pd.lateral_m < ours.lateral_m.max(1.0) * 0.5,
        "PD mode weaved {:.1}m laterally against ours' {:.1}m — PD bots do not strafe \
         (speedmultsideways is zero in every branch that writes it)",
        pd.lateral_m,
        ours.lateral_m
    );
}

/// **PD mode holds PD's distance, not ours.** `standoff_for` is a fraction of an invented
/// range; `g_BotDistConfigs` is PD's own number. A PD-mode hunter should be found inside
/// its band essentially whenever it can see you.
///
/// The *comparison* is reported and deliberately not asserted. Measured: against a
/// stationary player our standoff lands close to PD's band for most weapons anyway
/// (band error 0.61 m ours vs 0.64 m PD on the default roster), so "PD sits nearer the
/// band centre" is not a property that holds — the two rules agree here and diverge on
/// guns whose band and standoff disagree, which is a playtest observation and not a unit
/// test. What must hold is that PD mode obeys its own band.
#[test]
fn pd_mode_holds_the_weapon_band() {
    use crate::enemy::AiMode;
    let ours = ab_run(AiMode::Ours, 20.0, true);
    let pd = ab_run(AiMode::Pd, 20.0, true);
    ab_report("band adherence", &ours, &pd);
    assert!(
        pd.in_band_frac > 0.9,
        "a PD hunter spent only {:.0}% of the fight inside its own engagement band",
        pd.in_band_frac * 100.0
    );
    assert!(
        pd.band_err_m < 1.5,
        "a PD hunter held {:.2}m off its band centre — it is not fighting to the band",
        pd.band_err_m
    );
}

/// Lethality, side by side, with the player killable — the number a playtest will
/// actually argue about. **Deliberately not asserted as a threshold**: which model kills
/// faster is the question being asked, not a property to lock in. All this fences off is
/// that PD mode can still finish a fight at all.
#[test]
fn ai_ab_time_to_kill_is_reported_for_both_models() {
    use crate::enemy::AiMode;
    let ours = ab_run(AiMode::Ours, 25.0, false);
    let pd = ab_run(AiMode::Pd, 25.0, false);
    ab_report("time to kill, player killable", &ours, &pd);
    assert!(pd.damage > 0.0, "a PD-mode hunter never landed a shot in 25 s");
    assert!(ours.damage > 0.0, "an ours-mode hunter never landed a shot in 25 s");
}

/// **In PD mode, everything PD does not have is switched off** — and switched back on
/// by returning to `ours`. Measured on the dial at maximum, where all four knobs are at
/// their loudest, so a zero here cannot be the difficulty being low.
///
/// `aibot->speedmultsideways` is written to zero in every branch that writes it
/// (`bot.c:206, 1063…`), `chr_try_sidestep`'s only caller is a hand-authored guard
/// script, and there is no cover selection in the bot code at all
/// (`DESIGN_AI_PD_VS_OURS.md` §4b).
#[test]
fn pd_mode_zeroes_the_behaviours_perfect_dark_does_not_have() {
    use crate::enemy::AiMode;
    let mut world = World::new();
    world.set_difficulty(DIFFICULTY_MAX);

    world.set_ai_mode(AiMode::Ours);
    let ours = world.ai_tuning();
    assert!(
        ours.dodge > 0.0 && ours.flank > 0.0 && ours.cover > 0.0 && ours.suppress > 0.0,
        "the dial at max should have all four knobs live: dodge {} flank {} cover {} suppress {}",
        ours.dodge,
        ours.flank,
        ours.cover,
        ours.suppress
    );

    world.set_ai_mode(AiMode::Pd);
    let pd = world.ai_tuning();
    assert_eq!(pd.dodge, 0.0, "PD bots do not dodge your crosshair");
    assert_eq!(pd.flank, 0.0, "PD bots do not flank");
    assert_eq!(pd.cover, 0.0, "PD bots do not take cover");
    assert_eq!(pd.suppress, 0.0, "PD bots have no suppressing-fire behaviour");
    // The knobs that are NOT PD-specific stay put — this is a flag, not a lobotomy.
    assert_eq!(pd.speed_mult, ours.speed_mult, "movement speed still follows the dial");
    assert_eq!(pd.sense, ours.sense, "perception reach still follows the dial");

    world.set_ai_mode(AiMode::Ours);
    let back = world.ai_tuning();
    assert!(back.dodge > 0.0, "switching back must restore our behaviours — nothing was deleted");
}

/// A PD-mode hunter never searches, whatever the `pd_omniscience` flag says: PD's target
/// selection has no visibility gate, so the model has no blind state to fall into. The
/// kill-switch governs `AI=ours`, where omniscience is an experiment rather than the
/// foundation.
#[test]
fn pd_mode_is_omniscient_even_with_the_kill_switch_off() {
    use crate::enemy::AiMode;
    let (mut arena, player) = omniscience_arena(true);
    arena.world.set_ai_mode(AiMode::Pd);
    arena.world.set_pd_omniscience(false);
    let (closest, searched) = omniscience_run(&mut arena, 20.0, player);
    println!("PD mode, omniscience kill-switch off: closest {closest:.2} m, searched {searched}");
    assert!(
        arena.world.enemies[0].enemy.is_omniscient(),
        "AI=pd requires omniscience — bot_choose_general_target has no visibility gate"
    );
    assert!(!searched, "a PD-mode hunter has no Search/Investigate to reach");
    assert!(closest < 6.0, "and it comes through the gap to find you ({closest:.2} m)");
}

/// **PD's reload rule, out of ammo.** A hunter in a sustained firefight empties its
/// magazine and is briefly out of the fight — the one opening the distance-band model
/// leaves you, and something `AI=ours` hunters (who carry infinite ammunition) never do.
#[test]
fn pd_mode_hunters_run_dry_and_reload() {
    use crate::enemy::AiMode;
    let pd = ab_run(AiMode::Pd, 20.0, true);
    let ours = ab_run(AiMode::Ours, 20.0, true);
    ab_report("reload rule", &ours, &pd);
    assert!(pd.reloaded, "a PD-mode hunter fired for 20 s without ever reloading");
    assert!(!ours.reloaded, "an ours-mode hunter reloaded — that rule is PD-mode only");
}

/// **PD's reload rule, the half-clip clause.** `bot.c:2470`: below half a clip *and* the
/// target unseen for 2 s, a bot tops up rather than waiting to run dry. Held here by
/// making the player unperceivable (the `N` observe toggle) after a burst — the hunter
/// still knows where you are (omniscience is knowledge, not perception), so this isolates
/// the "haven't seen you lately" clause from losing the target altogether.
#[test]
fn pd_mode_tops_up_a_partial_clip_once_you_are_out_of_sight() {
    use crate::enemy::AiMode;
    use crate::pdsim::difficulty::BotDifficulty;
    let (px, pz) = (7.5f32, 4.0f32);
    let mut arena =
        TestArena::build_pd([60.0, 16.0, 60.0], &[], 1, Vec3::new(px, 0.0, pz), BotDifficulty::Normal);
    arena.world.set_ai_mode(AiMode::Pd);
    arena.world.toggle_invulnerable();
    arena.place_hunter(0, px, pz + 4.0); // already inside every band → it opens fire at once
    let dt = 1.0 / 60.0;
    let clip = arena.world.enemies[0].weapon.clip;
    if clip < 2 {
        eprintln!("skipping: this hunter's weapon has no magazine to be half-empty");
        return;
    }
    // Fire until the magazine is down but NOT empty, so only the half-clip clause can
    // explain a reload.
    let mut fired_frames = 0;
    while arena.world.enemies[0].loaded > clip / 2 && fired_frames < 60 * 20 {
        arena.set_player(px, pz);
        arena.step(dt);
        fired_frames += 1;
    }
    let loaded = arena.world.enemies[0].loaded;
    if loaded == 0 || arena.world.enemies[0].reload_timer > 0.0 {
        eprintln!("skipping: this weapon empties its clip faster than the clause can trigger");
        return;
    }
    assert!(loaded < clip, "the hunter never fired a shot in 20 s");
    // Now vanish. The 2 s clock starts, and the reload should be scheduled just after it.
    arena.world.toggle_invisible();
    let mut scheduled_at = None;
    for f in 0..(60.0 * 4.0) as usize {
        arena.set_player(px, pz);
        arena.step(dt);
        if scheduled_at.is_none() && arena.world.enemies[0].reload_timer > 0.0 {
            scheduled_at = Some(f as f32 * dt);
        }
    }
    let at = scheduled_at.expect("a partial magazine was never topped up after 4 s unseen");
    println!("half-clip reload scheduled {at:.1}s after losing sight ({loaded}/{clip} loaded)");
    assert!(
        (super::PD_RELOAD_UNSEEN..3.5).contains(&at),
        "the reload came {at:.1}s after losing sight; PD's clause is {:.0}s",
        super::PD_RELOAD_UNSEEN
    );
}

/// **An explicit `AI=` outranks any mode default**, whatever order a caller uses — the
/// trap that ate `BODIES=ge` for a whole playtest when `enable_pd_lab` pinned a body set
/// and the environment was applied first. Nothing here pins an AI mode today; this pins
/// the property so nothing may start.
#[test]
fn an_explicit_ai_mode_outranks_the_lab() {
    use crate::enemy::AiMode;
    let mut world = World::new();
    world.enable_pd_lab(super::pd_lab::PdLabConfig::default());
    world.set_ai_mode(AiMode::Ours);
    assert_eq!(world.ai_mode(), AiMode::Ours, "the lab must not pin the engagement model");
    world.set_ai_mode(AiMode::Pd);
    assert_eq!(world.ai_mode(), AiMode::Pd);
}
