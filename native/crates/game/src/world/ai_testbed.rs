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
#[test]
fn orca_a_pack_funnels_through_a_doorway() {
    // Room 60×60 WT (15×15 m) split at z∈[28,32] WT by two wall segments leaving a
    // ~1.5 m central gap (x∈[27,33] WT). Player on the far side, pack on the near side.
    let walls = [
        [0.0, 0.0, 28.0, 27.0, 16.0, 4.0],  // left wall  (x 0..6.75 m)
        [33.0, 0.0, 28.0, 27.0, 16.0, 4.0], // right wall (x 8.25..15 m)
    ];
    let mut arena = TestArena::build([60.0, 16.0, 60.0], &walls, 5, Vec3::new(7.5, 0.0, 11.0));
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
    let crossed = arena.world.enemies.iter().filter(|e| e.enemy.pos.z > 9.0).count();
    assert!(crossed >= 1, "at least one hunter funnelled through the gap (crossed={crossed})");
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
