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
/// Chase speed (JS `chaseSpeed`).
const SPEED_CHASE: f32 = 4.0; // m/s
const REPATH_INTERVAL: f32 = 0.4; // s between path recomputes (CHASE_UPDATE_INTERVAL)
const CATCH_DIST: f32 = 1.2 * WT; // 0.3 m — horizontal catch radius
const WAYPOINT_EPS: f32 = 0.4 * WT; // 0.1 m — advance to next waypoint within this
const CATCH_VERT: f32 = 3.0 * WT; // must be within ~1 floor vertically to catch

/// Perception + FSM constants (JS `AIConfig` defaults + `EnemyManager` overrides).
const DETECTION_RANGE: f32 = 12.0; // m
const DETECTION_HALF_CONE: f32 = 60.0 * std::f32::consts::PI / 180.0; // 120° cone → ±60°
const ATTACK_RANGE: f32 = 6.0; // m
const ALERT_DURATION: f32 = 0.5; // s reaction delay
const COOLDOWN_DURATION: f32 = 1.5; // s between fire bursts

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
            health: ENEMY_HEALTH,
            dead: false,
            stun_timer: 0.0,
            state: AiState::Search,
            alert_timer: 0.0,
            chase_timer: 0.0,
            cooldown_timer: 0.0,
            is_attacking: false,
            fire_started: false,
            search_target: None,
            last_known: None,
            scan_timer: 0.0,
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

    /// Current speed (m/s): the chase speed while advancing, else 0.
    pub fn speed(&self) -> f32 {
        if self.moving {
            SPEED_CHASE
        } else {
            0.0
        }
    }

    /// The current FSM state (for inspection / tests).
    pub fn state(&self) -> AiState {
        self.state
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
        // position fresh while chasing/attacking.
        let perceived = self.dist_to(player_feet) < DETECTION_RANGE
            && self.in_cone(player_feet)
            && line_of_sight(physics, self.pos, player_feet, self_collider);
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
                            if self.move_toward(dt, t, nav) {
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
                            self.move_toward(dt, t, nav);
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
                    self.path.clear();
                } else if fire_anim {
                    // A fire one-shot is still playing (it began in `attack`, then the
                    // player slipped out of range): stay planted so the feet don't
                    // "float" — just keep facing the target — and resume chasing when
                    // the clip finishes. Movement is gated on the animation, matching
                    // the JS `enemyState === 'action'` decision gate.
                    self.face(player_feet);
                } else {
                    // Path to where we last saw the player (updated to the live
                    // position every perceived step above). Reaching that spot without
                    // seeing them = they got away → investigate it.
                    let target = self.last_known.unwrap_or(player_feet);
                    if self.move_toward(dt, target, nav) && !perceived {
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
                } else {
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
                    // Fire animation finished → cool down.
                    if self.is_attacking && self.fire_started && !fire_anim {
                        self.is_attacking = false;
                        self.state = AiState::Cooldown;
                        self.cooldown_timer = 0.0;
                    }
                }
            }
            AiState::Cooldown => {
                self.face(player_feet);
                self.cooldown_timer += dt;
                if self.cooldown_timer >= COOLDOWN_DURATION {
                    let dist = self.dist_to(player_feet);
                    let los = line_of_sight(physics, self.pos, player_feet, self_collider);
                    if dist <= ATTACK_RANGE && los {
                        self.state = AiState::Attack;
                        self.is_attacking = false;
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
    fn move_toward(&mut self, dt: f32, target: Vec3, nav: &NavWorld) -> bool {
        let flat = Vec3::new(target.x - self.pos.x, 0.0, target.z - self.pos.z);
        if flat.length() < ARRIVE_DIST {
            return true;
        }
        // Sample the walkability line at ~knee height so it clears the floor but
        // catches walls/waist-high obstacles.
        let up = Vec3::new(0.0, 0.5, 0.0);
        if nav.los_clear(self.pos + up, target + up) {
            self.path.clear();
            self.repath_timer = 0.0; // force a fresh A* path the instant LOS breaks
            let dist = flat.length();
            let stepd = (SPEED_CHASE * dt).min(dist);
            self.pos += flat / dist * stepd;
            self.heading = flat / dist; // face the (flat) travel direction
            self.moving = true;
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
                let stepd = (SPEED_CHASE * dt).min(dist);
                self.pos += to / dist * stepd;
                let f = Vec3::new(to.x, 0.0, to.z);
                if f.length_squared() > 1e-6 {
                    self.heading = f.normalize();
                }
                self.moving = true;
            }
            if self.pos.distance(waypoint) < WAYPOINT_EPS && self.path_idx < self.path.len() - 1 {
                self.path_idx += 1;
            }
        }
        false
    }
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
