//! A single hunter grunt. Extends the A1 perception FSM (ported from
//! `3DS FPS/src/ai/EnemyAI.ts`, `idle → alert → chase → attack ↔ cooldown`) with a
//! **search layer** so a hunter that floods in through the spawn door and does *not*
//! yet know where the player is will hunt for them rather than stand idle:
//!
//! * **Search** — no known target: walk to an assigned search point (the `World`
//!   hands out spread-out points so the pack fans out and sweeps the base), running
//!   the perception cone the whole time. Seeing the player promotes to `Alert`.
//! * **Alert → Chase → Attack ↔ Cooldown** — the original engagement chain, but
//!   the chase now paths to the player's **last-known position** (updated every step
//!   the player is perceived), so breaking line-of-sight makes the hunter go to where
//!   it last saw you rather than tracking you omnisciently.
//! * **Investigate** — lost the player (LOS broke / a heard gunshot): go to the
//!   last-known / noise position, scan around for a moment, then fall back to Search.
//!
//! Movement/perception constants are ported from `EnemyAI.ts`; the probabilistic
//! shot roll + the fire-animation cadence live in the `World` combat layer (which
//! owns the animation mixer + the player), driven by [`EnemyStep::want_fire`]. Search
//! coordination (which point each hunter gets) lives in `World` too — this file just
//! walks to whatever [`Self::assign_search_target`] set and reports when it needs a
//! fresh one via [`EnemyStep::needs_search_target`].
//!
//! Scope note (2026-07-16): door **breach/blocking is disabled** — doors are open
//! passages during the hunt — so the FSM has no door-blocking branch.

use glam::Vec3;
use rapier3d::prelude::ColliderHandle;

use engine::geometry::csg_runtime::WORLD_SCALE;
use engine::sim::nav::NavWorld;
use engine::sim::physics::PhysicsWorld;

/// The outcome of one enemy step, reported back to [`crate::world::World`].
#[derive(Default)]
pub struct EnemyStep {
    /// The player is within catch range this step (a melee fallback — largely
    /// dormant now that the hunter stops at attack range to shoot).
    pub caught: bool,
    /// The hunter wants to start a fire burst this step (it entered `attack` and
    /// isn't already firing). The `World` plays the fire one-shot on the shared
    /// animation mixer; the shot cadence + damage roll run there.
    pub want_fire: bool,
    /// The hunter is searching and has no (reachable) search point to head for —
    /// the `World` should hand it a fresh one via [`Enemy::assign_search_target`]
    /// (this is where the fan-out coordination lives, since one hunter can't see
    /// where the others are going).
    pub needs_search_target: bool,
}

/// The decision FSM: the A1 engagement chain (`EnemyAI.AIState`) plus the two
/// search-layer states that drive a hunter which doesn't yet know where the player
/// is (see the module docs).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AiState {
    /// Standing still, unaware (the spawn-in state before a search point arrives,
    /// and the fallback if there's nowhere left to search).
    Idle,
    /// Sweeping the base toward an assigned search point, perception cone live.
    Search,
    /// Walking to a last-known / heard position, then scanning it, before giving up
    /// to Search.
    Investigate,
    Alert,
    Chase,
    Attack,
    Cooldown,
}

const WT: f32 = WORLD_SCALE;
/// Per-state movement speeds (m/s) — chosen so the continuous locomotion blend
/// shows the full gait range: search/investigate walk, attack-advance jog, chase
/// run. `speed()` reports whichever the current step used so the legs match.
const SPEED_SEARCH: f32 = 1.6; // ~walk gait — calm sweeping / investigating
const SPEED_ADVANCE: f32 = 3.2; // ~jog gait — closing on the player while firing
/// Chase speed (JS `chaseSpeed`) — the urgent run.
const SPEED_CHASE: f32 = 4.6; // m/s (~run gait)
const REPATH_INTERVAL: f32 = 0.4; // s between path recomputes (CHASE_UPDATE_INTERVAL)
const CATCH_DIST: f32 = 1.2 * WT; // 0.3 m — horizontal catch radius
const WAYPOINT_EPS: f32 = 0.4 * WT; // 0.1 m — advance to next waypoint within this
const CATCH_VERT: f32 = 3.0 * WT; // must be within ~1 floor vertically to catch
/// The straight-line beeline shortcut is only taken when the target is within this
/// height of the hunter (< half a 0.25 m stair step). Any real vertical traversal —
/// stairs, platforms, switchback landings — must go through A*, which walks cardinal
/// cell-to-cell and so can't cut a diagonal corner across an open stairwell (the
/// switchback "turns 180°, cuts through the railing gap, and falls" bug).
const COPLANAR_BEELINE_EPS: f32 = 0.5 * WT; // 0.125 m

/// Perception + FSM constants (JS `AIConfig` defaults + `EnemyManager` overrides).
const DETECTION_RANGE: f32 = 12.0; // m
const DETECTION_HALF_CONE: f32 = 60.0 * std::f32::consts::PI / 180.0; // 120° cone → ±60°
/// A player closer than this is noticed **regardless of facing** (you can't sneak
/// past a guard you're standing next to — footsteps/presence). Still LOS-gated, so a
/// wall between hides you. Kills the "walks off ignoring me while I'm right here" bug.
const PROXIMITY_RANGE: f32 = 3.5; // m
const ATTACK_RANGE: f32 = 6.0; // m
/// While attacking, the hunter advances on the player down to this standoff (m)
/// then holds — so it fires *while moving* (run-and-gun) instead of freezing at
/// first sight of the player. Firing is a timer now, so movement + shooting mix.
const ATTACK_STANDOFF: f32 = 3.0; // m
/// Hysteresis around the standoff: once holding, the hunter only resumes closing
/// if the player pulls beyond `standoff + this`. Prevents the micro-step-in-and-out
/// at the exact boundary that kept the legs walking in place.
const STANDOFF_HYST: f32 = 1.2; // m
const ALERT_DURATION: f32 = 0.5; // s reaction delay
const COOLDOWN_DURATION: f32 = 1.5; // s between fire bursts

// ─── Burst-and-reposition (Perfect Dark "sim" repositioning) ─────────────────
/// Between fire bursts, instead of standing at the standoff waiting out the
/// cooldown (a turret), the hunter jukes to a fresh firing position: it swings
/// this far around the player and re-engages from the new angle. No strafe clip is
/// needed — it faces its *travel* direction on the move (clean legs, like Chase),
/// then snaps back to face + fire when it plants. The direction flips each burst so
/// it weaves back and forth rather than orbiting one way.
const REPOSITION_ARC: f32 = 0.7; // rad (~40°) swung around the player per juke
/// Keep the new spot within this band of the player (m): far enough to have moved,
/// close enough (inside `ATTACK_RANGE`) that it re-engages the instant it arrives.
const REPOSITION_MIN_R: f32 = 3.0;
const REPOSITION_MAX_R: f32 = 5.5;
/// Jog to the reposition spot (reads as a deliberate tactical relocate, not a panic
/// sprint or a stroll).
const REPOSITION_SPEED: f32 = SPEED_ADVANCE;

// ─── Search layer ────────────────────────────────────────────────────────────
/// Within this XZ distance (m) of a search / investigate target, the hunter counts
/// as "arrived" (and Search asks for the next point).
const ARRIVE_DIST: f32 = 0.6;
/// How long (s) a hunter scans a spot in `Investigate` before giving up to `Search`.
const INVESTIGATE_SCAN_DURATION: f32 = 2.5;
/// How fast (rad/s) the hunter's facing sweeps while scanning in `Investigate`, so
/// its perception cone actually pans across the room to re-acquire the player.
const SCAN_TURN_RATE: f32 = 1.6;

/// Starting health (JS `EnemyCharacter` default + facility karl/joe). With PP7
/// damage 25 → 4 shots to kill.
pub const ENEMY_HEALTH: f32 = 100.0;

pub struct Enemy {
    /// Feet position, meters.
    pub pos: Vec3,
    /// Current path (meters waypoints), and the index we're heading toward.
    path: Vec<Vec3>,
    path_idx: usize,
    repath_timer: f32,
    /// Horizontal facing (unit vector): the direction the model faces + the
    /// perception cone axis. Set to the travel direction while chasing and toward
    /// the player while alert/attack/cooldown (JS `faceTarget`).
    heading: Vec3,
    /// Whether the hunter advanced this step (false while idle/attacking/pathless).
    moving: bool,
    /// Speed (m/s) of the step actually taken this update — 0 when stationary. Set
    /// by [`Self::move_toward`] from the per-state speed; drives the locomotion gait.
    move_speed: f32,
    /// Remaining health; at ≤0 the hunter is [`Self::dead`] (Track A).
    health: f32,
    /// Killed — [`Self::update`] is a full no-op (the body holds its death pose
    /// while it fades). Set by [`Self::take_damage`] on the lethal shot.
    dead: bool,
    /// Hit-reaction "spaz-out" timer (s); while >0 the hunter stops moving so the
    /// hit one-shot reads (JS clears `moveTarget` during a hit).
    stun_timer: f32,

    // ─── A1 perception FSM ──
    state: AiState,
    alert_timer: f32,
    chase_timer: f32,
    cooldown_timer: f32,
    /// A fire burst has been requested this attack entry (JS `isAttacking`).
    is_attacking: bool,
    /// In `Attack`, whether the hunter has reached its standoff and is holding
    /// position (feet planted). Hysteresis flag — see [`STANDOFF_HYST`].
    holding: bool,
    /// The fire animation has actually started playing (JS `fireAnimStarted`) —
    /// so we detect its *completion* (not just its not-yet-started frames).
    fire_started: bool,

    // ─── Search layer ──
    /// The point this hunter is sweeping toward while in `Search`. Assigned by the
    /// `World` (fan-out coordination); cleared on arrival, when the hunter reports
    /// [`EnemyStep::needs_search_target`].
    search_target: Option<Vec3>,
    /// Where the player was last perceived (or a heard gunshot) — the chase paths
    /// here, and `Investigate` walks here then scans it.
    last_known: Option<Vec3>,
    /// Seconds spent scanning the current spot in `Investigate`.
    scan_timer: f32,

    // ─── Burst-and-reposition ──
    /// Which way the hunter arcs when it repositions between bursts (+1 / −1),
    /// flipped each juke so it weaves back and forth rather than orbiting the player.
    strafe_dir: f32,
    /// The spot the hunter is juking to during `Cooldown` (burst-and-reposition), or
    /// `None` to hold + face like a plain cooldown (no standable/LOS spot to juke to).
    reposition_target: Option<Vec3>,
}

impl Enemy {
    /// Spawn at `feet`, initially watching toward `watch` (into the room, so the
    /// perception cone faces where the player is likely to be), and starting in
    /// [`AiState::Search`] — a hunter that just flooded in through the door and is
    /// hunting for the player. The `World` hands it a search point on the first step.
    pub fn new(feet: Vec3, watch: Vec3) -> Self {
        let heading = {
            let flat = Vec3::new(watch.x - feet.x, 0.0, watch.z - feet.z);
            if flat.length_squared() > 1e-6 {
                flat.normalize()
            } else {
                Vec3::NEG_Z
            }
        };
        Enemy {
            pos: feet,
            path: Vec::new(),
            path_idx: 0,
            repath_timer: 0.0,
            heading,
            moving: false,
            move_speed: 0.0,
            health: ENEMY_HEALTH,
            dead: false,
            stun_timer: 0.0,
            state: AiState::Search,
            alert_timer: 0.0,
            chase_timer: 0.0,
            cooldown_timer: 0.0,
            is_attacking: false,
            holding: false,
            fire_started: false,
            search_target: None,
            last_known: None,
            scan_timer: 0.0,
            strafe_dir: 1.0,
            reposition_target: None,
        }
    }

    /// The point this hunter is currently sweeping toward in `Search` (so the
    /// `World` can fan the pack out — avoid handing two hunters the same point).
    pub fn search_target(&self) -> Option<Vec3> {
        self.search_target
    }

    /// Hand this hunter a fresh search point (the `World`'s fan-out coordinator).
    /// A no-op once dead. Keeps the hunter in / returns it to `Search`.
    pub fn assign_search_target(&mut self, target: Vec3) {
        if self.dead {
            return;
        }
        self.search_target = Some(target);
        if matches!(self.state, AiState::Idle) {
            self.state = AiState::Search;
        }
    }

    /// React to a heard noise (e.g. the player's gunfire) at `pos`: if the hunter is
    /// still hunting blind (searching / investigating / idle), converge on the sound
    /// to investigate it. A hunter already engaged (alerted / chasing / attacking)
    /// keeps its better information. No-op once dead.
    pub fn hear_noise(&mut self, pos: Vec3) {
        if self.dead {
            return;
        }
        if matches!(self.state, AiState::Search | AiState::Investigate | AiState::Idle) {
            self.last_known = Some(pos);
            self.search_target = None;
            self.scan_timer = 0.0;
            self.state = AiState::Investigate;
        }
    }

    /// Horizontal facing (unit vector) — the direction the model faces.
    pub fn heading(&self) -> Vec3 {
        self.heading
    }

    /// Current speed (m/s): the speed of the step taken this update (per-state),
    /// or 0 when stationary. Drives the continuous locomotion gait.
    pub fn speed(&self) -> f32 {
        self.move_speed
    }

    /// The current FSM state (for inspection / tests).
    pub fn state(&self) -> AiState {
        self.state
    }

    /// Whether the hunter is actively engaged with the player (has eyes on it or is
    /// running the engagement chain) — the squad-alert broadcaster.
    pub fn is_engaged(&self) -> bool {
        matches!(
            self.state,
            AiState::Alert | AiState::Chase | AiState::Attack | AiState::Cooldown
        )
    }

    /// Where this hunter last perceived the player (or a heard noise) — the position
    /// an engaged hunter calls its packmates to converge on.
    pub fn last_known(&self) -> Option<Vec3> {
        self.last_known
    }

    /// Apply `dmg` to the hunter; returns `true` if this shot killed it (health
    /// crossed to ≤0). A dead hunter takes no further damage. Mirrors JS
    /// `Actor.takeDamage` (armor omitted — the grunt has none).
    pub fn take_damage(&mut self, dmg: f32) -> bool {
        if self.dead {
            return false;
        }
        self.health -= dmg;
        if self.health <= 0.0 {
            self.health = 0.0;
            self.dead = true;
            self.moving = false;
        }
        self.dead
    }

    /// Whether the hunter has been killed.
    pub fn is_dead(&self) -> bool {
        self.dead
    }

    /// Remaining health (for inspection / tests).
    pub fn health(&self) -> f32 {
        self.health
    }

    /// Stun the hunter for `dur` seconds — it stops moving while a hit reaction
    /// plays. Refreshes (does not stack) so a fresh hit restarts the window.
    pub fn stun(&mut self, dur: f32) {
        self.stun_timer = self.stun_timer.max(dur);
        self.moving = false;
    }

    /// Face the player instantly (JS `faceTarget`).
    fn face(&mut self, player_feet: Vec3) {
        let flat = Vec3::new(player_feet.x - self.pos.x, 0.0, player_feet.z - self.pos.z);
        if flat.length_squared() > 1e-6 {
            self.heading = flat.normalize();
        }
    }

    /// Horizontal (XZ) distance to the player.
    fn dist_to(&self, player_feet: Vec3) -> f32 {
        Vec3::new(player_feet.x - self.pos.x, 0.0, player_feet.z - self.pos.z).length()
    }

    /// Whether the hunter perceives the player this step, given whether it has clear
    /// line-of-sight. Two LOS-gated ways in: the 120° detection cone out to
    /// [`DETECTION_RANGE`], or bare proximity within [`PROXIMITY_RANGE`] regardless of
    /// facing (can't sneak past a guard you're standing next to).
    fn perceives(&self, player_feet: Vec3, has_los: bool) -> bool {
        if !has_los {
            return false;
        }
        let dist = self.dist_to(player_feet);
        dist < PROXIMITY_RANGE || (dist < DETECTION_RANGE && self.in_cone(player_feet))
    }

    /// Whether the player is inside the detection cone (JS `isTargetInCone`).
    fn in_cone(&self, player_feet: Vec3) -> bool {
        let to = Vec3::new(player_feet.x - self.pos.x, 0.0, player_feet.z - self.pos.z);
        if to.length_squared() < 1e-6 {
            return true;
        }
        self.heading.angle_between(to.normalize()) < DETECTION_HALF_CONE
    }

    /// Advance the FSM one step. `fire_anim` = a fire one-shot is currently playing
    /// on the shared mixer (the JS `enemyState === 'action'` proxy, disambiguated
    /// from hit/death by the caller). Returns `want_fire` when it wants the caller
    /// to start a fire burst this step, and `needs_search_target` when it's searching
    /// and needs the `World` to hand it a fresh point.
    pub fn update(
        &mut self,
        dt: f32,
        player_feet: Vec3,
        nav: &NavWorld,
        physics: &mut PhysicsWorld,
        fire_anim: bool,
        self_collider: ColliderHandle,
    ) -> EnemyStep {
        self.moving = false;
        self.move_speed = 0.0;
        if self.dead {
            return EnemyStep::default();
        }
        // Stunned (mid hit-reaction): drain the timer, don't move or think.
        if self.stun_timer > 0.0 {
            self.stun_timer = (self.stun_timer - dt).max(0.0);
            return EnemyStep::default();
        }

        // Perception is checked every step, in every state: seeing the player is what
        // promotes a searcher to the engagement chain, and keeps the last-known
        // position fresh while chasing/attacking. Two ways in, both LOS-gated: the
        // 120° detection cone out to `DETECTION_RANGE`, OR close proximity regardless
        // of facing (`PROXIMITY_RANGE`).
        let has_los = line_of_sight(physics, self.pos, player_feet, self_collider);
        let perceived = self.perceives(player_feet, has_los);
        if perceived {
            self.last_known = Some(player_feet);
        }

        let mut step = EnemyStep::default();
        match self.state {
            AiState::Idle => {
                // Unaware and with nowhere assigned to search — the `World` will give
                // it a point (spawn-in / stuck fallback). Acquire on sight meanwhile.
                if perceived {
                    self.enter_alert();
                } else {
                    step.needs_search_target = true;
                }
            }
            AiState::Search => {
                if perceived {
                    self.enter_alert();
                } else {
                    match self.search_target {
                        Some(t) => {
                            if self.move_toward(dt, t, nav, SPEED_SEARCH) {
                                // Reached it (or it's unreachable) — ask for the next.
                                self.search_target = None;
                                step.needs_search_target = true;
                            }
                        }
                        None => step.needs_search_target = true,
                    }
                }
            }
            AiState::Investigate => {
                if perceived {
                    self.enter_alert();
                } else {
                    match self.last_known {
                        // Still walking to the spot we're curious about.
                        Some(t) if self.dist_to(t) > ARRIVE_DIST => {
                            self.move_toward(dt, t, nav, SPEED_SEARCH);
                        }
                        // Arrived (or nothing to walk to): scan around, sweeping the
                        // cone, then give up to a fresh search.
                        _ => {
                            self.scan_timer += dt;
                            self.sweep_heading(dt);
                            if self.scan_timer >= INVESTIGATE_SCAN_DURATION {
                                self.state = AiState::Search;
                                self.last_known = None;
                                self.search_target = None;
                                step.needs_search_target = true;
                            }
                        }
                    }
                }
            }
            AiState::Alert => {
                self.face(player_feet);
                self.alert_timer += dt;
                if self.alert_timer >= ALERT_DURATION {
                    self.state = AiState::Chase;
                    self.chase_timer = 0.0;
                }
            }
            AiState::Chase => {
                let dist = self.dist_to(player_feet);
                let los = line_of_sight(physics, self.pos, player_feet, self_collider);
                if dist <= ATTACK_RANGE && !fire_anim && los {
                    self.face(player_feet);
                    self.state = AiState::Attack;
                    self.is_attacking = false;
                    self.holding = false;
                    self.path.clear();
                } else {
                    // Path to where we last saw the player (updated to the live
                    // position every perceived step above). Reaching that spot without
                    // seeing them = they got away → investigate it. A burst in flight
                    // no longer freezes movement — firing is a timer, and the legs run
                    // on locomotion while the arm keeps its procedural aim.
                    let target = self.last_known.unwrap_or(player_feet);
                    if self.move_toward(dt, target, nav, SPEED_CHASE) && !perceived {
                        self.state = AiState::Investigate;
                        self.scan_timer = 0.0;
                    }
                }
            }
            AiState::Attack => {
                let dist = self.dist_to(player_feet);
                let los = line_of_sight(physics, self.pos, player_feet, self_collider);
                if dist > ATTACK_RANGE * 1.3 || !los {
                    self.state = AiState::Chase;
                    self.chase_timer = 0.0;
                    self.is_attacking = false;
                    self.holding = false;
                } else {
                    // Advance-fire with a hold dead-band: close to the standoff, then
                    // plant the feet and HOLD until the player pulls beyond
                    // standoff+hyst. This stops the micro-step-in-and-out at the exact
                    // boundary that left the legs walking in place. Always face the
                    // player so the procedural aim points the gun at them.
                    if self.holding {
                        if dist > ATTACK_STANDOFF + STANDOFF_HYST {
                            self.holding = false;
                        }
                    } else if dist > ATTACK_STANDOFF {
                        self.move_toward(dt, player_feet, nav, SPEED_ADVANCE); // jog in
                    } else {
                        self.holding = true;
                    }
                    self.face(player_feet);
                    // Request a fire burst once per attack entry.
                    if !fire_anim && !self.is_attacking {
                        self.is_attacking = true;
                        self.fire_started = false;
                        step.want_fire = true;
                    }
                    if self.is_attacking && !self.fire_started && fire_anim {
                        self.fire_started = true;
                    }
                    // Fire animation finished → cool down, and pick a fresh firing
                    // angle to juke to (burst-and-reposition). Flip the weave
                    // direction first so successive bursts alternate sides.
                    if self.is_attacking && self.fire_started && !fire_anim {
                        self.is_attacking = false;
                        self.state = AiState::Cooldown;
                        self.cooldown_timer = 0.0;
                        self.strafe_dir = -self.strafe_dir;
                        self.reposition_target =
                            self.pick_reposition(player_feet, nav, physics, self_collider);
                    }
                }
            }
            AiState::Cooldown => {
                self.cooldown_timer += dt;
                // Burst-and-reposition: juke to the new firing angle while the
                // cooldown runs, facing the travel direction so the legs read
                // cleanly (no strafe clip). Fall back to holding + facing the player
                // if there was no spot to juke to.
                let arrived = match self.reposition_target {
                    Some(t) => self.move_toward(dt, t, nav, REPOSITION_SPEED),
                    None => {
                        self.face(player_feet);
                        true
                    }
                };
                // Arrived at the new spot (or the cooldown elapsed) → plant, face the
                // player, and re-evaluate the engagement exactly as before.
                if arrived || self.cooldown_timer >= COOLDOWN_DURATION {
                    self.reposition_target = None;
                    self.face(player_feet);
                    let dist = self.dist_to(player_feet);
                    let los = line_of_sight(physics, self.pos, player_feet, self_collider);
                    if dist <= ATTACK_RANGE && los {
                        self.state = AiState::Attack;
                        self.is_attacking = false;
                        self.holding = false;
                    } else if dist <= DETECTION_RANGE {
                        self.state = AiState::Chase;
                        self.chase_timer = 0.0;
                    } else {
                        // Lost them — go poke at where they last were.
                        self.state = AiState::Investigate;
                        self.scan_timer = 0.0;
                    }
                }
            }
        }

        step.caught = {
            let horiz = self.dist_to(player_feet);
            horiz < CATCH_DIST && (player_feet.y - self.pos.y).abs() < CATCH_VERT
        };
        step
    }

    /// Begin the reaction delay after acquiring the player.
    fn enter_alert(&mut self) {
        self.state = AiState::Alert;
        self.alert_timer = 0.0;
        self.path.clear();
    }

    /// Rotate the facing in place (used to scan a spot in `Investigate`), so the
    /// perception cone sweeps and can re-acquire the player.
    fn sweep_heading(&mut self, dt: f32) {
        let ang = SCAN_TURN_RATE * dt;
        let (s, c) = ang.sin_cos();
        let (x, z) = (self.heading.x, self.heading.z);
        let h = Vec3::new(x * c - z * s, 0.0, x * s + z * c);
        if h.length_squared() > 1e-6 {
            self.heading = h.normalize();
        }
    }

    /// Move toward a flat `target` this step; returns `true` when the hunter has
    /// arrived (within [`ARRIVE_DIST`]) or the target is **unreachable** (no A* path
    /// and no clear line) so the caller can pick a new one instead of getting stuck.
    ///
    /// When the straight line to the target is walkable (an open room), **beeline** —
    /// move directly at any angle — so the hunter doesn't zig-zag along the grid's
    /// cardinal-only A* waypoints. Only when the line is blocked (a wall/corner) does
    /// it fall back to A* (the JS "LOS → beeline" shortcut). Shared by Chase, Search,
    /// and Investigate.
    /// Drop the hunter's feet to the standable floor beneath it (bounded, so a
    /// transient bad step can't teleport it across a full drop). Enemies have no
    /// gravity or collide-and-slide of their own — their Y comes only from the
    /// path they follow — so the flat beeline branch needs this to stay on the
    /// surface rather than floating over a rise.
    fn snap_to_floor(&mut self, nav: &NavWorld) {
        if let Some(fy) = nav.floor_height_at(self.pos.x, self.pos.z, self.pos.y + 0.25) {
            if (fy - self.pos.y).abs() <= 1.0 {
                self.pos.y = fy;
            }
        }
    }

    fn move_toward(&mut self, dt: f32, target: Vec3, nav: &NavWorld, speed: f32) -> bool {
        let flat = Vec3::new(target.x - self.pos.x, 0.0, target.z - self.pos.z);
        if flat.length() < ARRIVE_DIST {
            return true;
        }
        // Sample the walkability line at ~knee height so it clears the floor but
        // catches walls/waist-high obstacles.
        let up = Vec3::new(0.0, 0.5, 0.0);
        // Beeline (straight-line shortcut) ONLY on the same level, clear of walls,
        // and over continuous ground. The height gate is the key one: a switchback
        // landing gives clear line-of-sight up to the next flight (railings are
        // cosmetic and don't block LOS), and the flat beeline would cut a diagonal
        // straight across the open well — through the railing gap — instead of
        // walking the landing to the offset second flight. Forcing every vertical
        // traversal through A* fixes that: A* is 4-connected, so it steps cardinal
        // cell-to-cell along the landing and physically cannot cut the corner.
        let coplanar = (target.y - self.pos.y).abs() <= COPLANAR_BEELINE_EPS;
        if coplanar
            && nav.los_clear(self.pos + up, target + up)
            && nav.ground_path_clear(self.pos, target)
        {
            self.path.clear();
            self.repath_timer = 0.0; // force a fresh A* path the instant LOS breaks
            let dist = flat.length();
            let stepd = (speed * dt).min(dist);
            self.pos += flat / dist * stepd;
            self.heading = flat / dist; // face the (flat) travel direction
            // The beeline moves in XZ only; glue the feet back to the surface so
            // the hunter rides gentle rises instead of leaving its Y frozen.
            self.snap_to_floor(nav);
            self.moving = true;
            self.move_speed = speed;
            return false;
        }

        self.repath_timer -= dt;
        if self.repath_timer <= 0.0 {
            self.repath_timer = REPATH_INTERVAL;
            match nav.find_path(self.pos, target) {
                Some(path) => {
                    let last = path.len().saturating_sub(1);
                    self.path = path;
                    self.path_idx = 1.min(last); // skip the start cell
                }
                None => {
                    // Nowhere to walk and no clear line → treat as arrived so the
                    // caller reassigns rather than freezing here forever.
                    self.path.clear();
                    return true;
                }
            }
        }

        if self.path_idx < self.path.len() {
            let waypoint = self.path[self.path_idx];
            let to = waypoint - self.pos;
            let dist = to.length();
            if dist > 1e-4 {
                let stepd = (speed * dt).min(dist);
                self.pos += to / dist * stepd;
                let f = Vec3::new(to.x, 0.0, to.z);
                if f.length_squared() > 1e-6 {
                    self.heading = f.normalize();
                }
                self.moving = true;
                self.move_speed = speed;
            }
            if self.pos.distance(waypoint) < WAYPOINT_EPS && self.path_idx < self.path.len() - 1 {
                self.path_idx += 1;
            }
        }
        // Keep the feet on the real tread/slab surface while following the path —
        // A* waypoints are quantized to integer-WT cell floors, so raw
        // interpolation can leave the hunter slightly above/below the surface it's
        // crossing. Snapping smooths that and guarantees it never floats over a step.
        self.snap_to_floor(nav);
        false
    }

    /// Choose the spot to juke to for a burst-and-reposition (called on the
    /// Attack→Cooldown transition). Prefers a standable spot that keeps
    /// line-of-sight to the player — so the hunter re-engages from the new angle
    /// rather than juking behind a wall — trying the current weave side first, then
    /// the far side, then shallower arcs; falls back to any standable spot if none
    /// keeps LOS, or `None` if there's nowhere standable to go (→ plain hold-and-face).
    fn pick_reposition(
        &self,
        player_feet: Vec3,
        nav: &NavWorld,
        physics: &mut PhysicsWorld,
        self_collider: ColliderHandle,
    ) -> Option<Vec3> {
        let mut fallback = None;
        for (dir, arc) in [
            (self.strafe_dir, REPOSITION_ARC),
            (-self.strafe_dir, REPOSITION_ARC),
            (self.strafe_dir, REPOSITION_ARC * 0.5),
            (-self.strafe_dir, REPOSITION_ARC * 0.5),
        ] {
            let ideal = reposition_point(player_feet, self.pos, dir, arc);
            if let Some(spot) = nav.nearest_standable(ideal.x, ideal.y.max(0.1), ideal.z, 3) {
                if fallback.is_none() {
                    fallback = Some(spot);
                }
                if line_of_sight(physics, spot, player_feet, self_collider) {
                    return Some(spot);
                }
            }
        }
        fallback
    }
}

/// The ideal burst-and-reposition spot (before snapping to a standable cell): swing
/// `dir · arc` radians around the player from the hunter's current bearing, at the
/// current player-distance clamped to `[REPOSITION_MIN_R, REPOSITION_MAX_R]`. Pure
/// (the nav snap + LOS preference live in [`Enemy::pick_reposition`]).
fn reposition_point(player_feet: Vec3, pos: Vec3, dir: f32, arc: f32) -> Vec3 {
    let to = Vec3::new(pos.x - player_feet.x, 0.0, pos.z - player_feet.z);
    let r = to.length();
    if r < 1e-3 {
        return pos; // sitting on the player — no meaningful bearing to arc from
    }
    let ang = to.z.atan2(to.x) + dir * arc;
    let nr = r.clamp(REPOSITION_MIN_R, REPOSITION_MAX_R);
    Vec3::new(
        player_feet.x + ang.cos() * nr,
        pos.y,
        player_feet.z + ang.sin() * nr,
    )
}

/// Rapier line-of-sight from `from_feet` to `to_feet`, cast between chest heights
/// (JS `EnemyAI.hasLineOfSight`). This hunter's own capsule (`self_collider`) is
/// excluded so it doesn't block its own view; another hunter's capsule in the way
/// legitimately does. Clear when nothing is hit (the native player has no
/// collider), or when the only hit is at essentially the target distance. A wall
/// in between blocks the shot.
pub(crate) fn line_of_sight(
    physics: &mut PhysicsWorld,
    from_feet: Vec3,
    to_feet: Vec3,
    self_collider: ColliderHandle,
) -> bool {
    let from = from_feet + Vec3::new(0.0, 1.0, 0.0);
    let to = to_feet + Vec3::new(0.0, 0.8, 0.0);
    let d = to - from;
    let dist = d.length();
    if dist < 1e-4 {
        return true;
    }
    let dir = d / dist;
    match physics.raycast_excluding(from, dir, dist, Some(self_collider)) {
        None => true,
        Some(hit) => (hit.point - from).length() >= dist - 0.1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Damage is subtractive off the starting health.
    #[test]
    fn damage_is_subtractive() {
        let mut e = Enemy::new(Vec3::ZERO, Vec3::NEG_Z);
        assert_eq!(e.health(), ENEMY_HEALTH);
        assert!(!e.take_damage(30.0), "not dead after 30 dmg");
        assert_eq!(e.health(), 70.0);
    }

    /// Four PP7 shots (25 dmg) kill the 100-hp hunter; only the lethal shot
    /// returns `true`, and a corpse takes no further damage.
    #[test]
    fn four_25_damage_shots_kill() {
        let mut e = Enemy::new(Vec3::ZERO, Vec3::NEG_Z);
        assert!(!e.take_damage(25.0), "75 hp");
        assert!(!e.take_damage(25.0), "50 hp");
        assert!(!e.take_damage(25.0), "25 hp");
        assert!(e.take_damage(25.0), "lethal shot returns died");
        assert!(e.is_dead());
        assert_eq!(e.health(), 0.0);
        assert!(!e.take_damage(25.0), "a dead hunter takes no more damage");
    }

    /// A hunter facing away from the player does not detect it (cone gate); one
    /// facing toward it (LOS clear, no physics obstacles) alerts.
    #[test]
    fn cone_gates_detection() {
        // Facing +Z, player at −Z (behind) → outside the cone.
        let mut e = Enemy::new(Vec3::ZERO, Vec3::Z);
        assert!(!e.in_cone(Vec3::new(0.0, 0.0, -5.0)), "player behind is out of cone");
        assert!(e.in_cone(Vec3::new(0.0, 0.0, 5.0)), "player ahead is in cone");
        // Watching toward the player seeds the heading toward it.
        e = Enemy::new(Vec3::ZERO, Vec3::new(0.0, 0.0, 5.0));
        assert!(e.in_cone(Vec3::new(0.0, 0.0, 5.0)));
    }

    /// Proximity sense: a player standing right next to the hunter is noticed even
    /// when behind it (out of cone), but only with line-of-sight, and not once beyond
    /// proximity range while still behind.
    #[test]
    fn close_player_noticed_even_from_behind() {
        let e = Enemy::new(Vec3::ZERO, Vec3::Z); // facing +Z
        let behind_close = Vec3::new(0.0, 0.0, -2.0); // behind, within PROXIMITY_RANGE
        assert!(!e.in_cone(behind_close), "the close player is behind the cone");
        assert!(e.perceives(behind_close, true), "…but proximity notices it");
        assert!(!e.perceives(behind_close, false), "…unless a wall blocks LOS");
        let behind_far = Vec3::new(0.0, 0.0, -8.0); // behind AND beyond proximity
        assert!(!e.perceives(behind_far, true), "far + behind stays unnoticed");
    }

    /// A freshly-spawned hunter starts hunting (Search), not standing idle — it
    /// flooded in through the door without knowing where the player is.
    #[test]
    fn new_hunter_starts_searching() {
        let e = Enemy::new(Vec3::ZERO, Vec3::NEG_Z);
        assert_eq!(e.state(), AiState::Search);
        assert!(e.search_target().is_none(), "no point assigned yet");
    }

    /// Assigning a search point stores it (and the `World` reads it back to fan the
    /// pack out); a dead hunter ignores the assignment.
    #[test]
    fn assign_search_target_stores_and_reads_back() {
        let mut e = Enemy::new(Vec3::ZERO, Vec3::NEG_Z);
        let t = Vec3::new(4.0, 0.0, 2.0);
        e.assign_search_target(t);
        assert_eq!(e.search_target(), Some(t));
        e.take_damage(ENEMY_HEALTH); // kill
        e.assign_search_target(Vec3::new(9.0, 0.0, 9.0));
        assert_eq!(e.search_target(), Some(t), "a corpse ignores new orders");
    }

    /// The burst-and-reposition point swings the hunter around the player by the arc
    /// (sign = weave direction), lands at the player-distance clamped into the band,
    /// and flips sides with the direction. A hunter sitting on the player keeps its
    /// spot (no bearing to arc from).
    #[test]
    fn reposition_point_arcs_around_the_player() {
        let player = Vec3::ZERO;
        // Hunter 4 m due +X of the player (bearing 0). Arc +0.7 rad should rotate the
        // bearing that far while staying at ~4 m (inside the [3, 5.5] band).
        let pos = Vec3::new(4.0, 0.0, 0.0);
        let p = reposition_point(player, pos, 1.0, REPOSITION_ARC);
        let r = (p.x * p.x + p.z * p.z).sqrt();
        assert!((r - 4.0).abs() < 1e-3, "stays at the clamped radius, got {r}");
        let ang = p.z.atan2(p.x);
        assert!((ang - REPOSITION_ARC).abs() < 1e-3, "arced +{REPOSITION_ARC} rad, got {ang}");
        // Flipping the direction arcs to the mirror bearing.
        let q = reposition_point(player, pos, -1.0, REPOSITION_ARC);
        assert!((q.z.atan2(q.x) + REPOSITION_ARC).abs() < 1e-3, "the other side mirrors");
        // Too close → the radius is pushed out to the min band, not left inside it.
        let close = Vec3::new(1.0, 0.0, 0.0);
        let c = reposition_point(player, close, 1.0, REPOSITION_ARC);
        let cr = (c.x * c.x + c.z * c.z).sqrt();
        assert!((cr - REPOSITION_MIN_R).abs() < 1e-3, "clamped up to the min band, got {cr}");
        // Sitting on the player → unchanged (no bearing).
        assert_eq!(reposition_point(player, player, 1.0, REPOSITION_ARC), player);
    }

    /// A gunshot pulls a *searching* hunter to investigate the sound (last-known set
    /// to the noise, state → Investigate), but a hunter already *engaged* keeps its
    /// own better information.
    #[test]
    fn hear_noise_diverts_only_a_seeker() {
        let noise = Vec3::new(3.0, 0.0, 5.0);

        let mut seeker = Enemy::new(Vec3::ZERO, Vec3::NEG_Z); // starts in Search
        seeker.hear_noise(noise);
        assert_eq!(seeker.state(), AiState::Investigate);
        assert_eq!(seeker.last_known, Some(noise));

        let mut engaged = Enemy::new(Vec3::ZERO, Vec3::NEG_Z);
        engaged.state = AiState::Attack; // mid-fight — has eyes on the player
        engaged.hear_noise(noise);
        assert_eq!(engaged.state(), AiState::Attack, "an engaged hunter isn't distracted");
    }
}
