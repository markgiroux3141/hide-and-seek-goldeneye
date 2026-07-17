//! A single hunter grunt with the A1 perception FSM ported from
//! `3DS FPS/src/ai/EnemyAI.ts`: `idle → alert → chase → attack ↔ cooldown`.
//! The hunter stands idle until it **sees** the player (detection cone + range +
//! a line-of-sight ray), reacts after a short delay, chases over the baked nav
//! grid, and once in attack range with LOS it stops and fires. Losing the target
//! (too far) drops it back to idle.
//!
//! Movement constants + the FSM are ported from `EnemyAI.ts`; the probabilistic
//! shot roll + the fire-animation cadence live in the `World` combat layer (which
//! owns the animation mixer + the player), driven by [`EnemyStep::want_fire`].
//!
//! Scope note (2026-07-16): door **breach/blocking is disabled** — doors are open
//! passages during the hunt — so the FSM has no door-blocking branch. Varied
//! behavior types (patrol/search/etc.) are a future addition.

use glam::Vec3;

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
}

/// The A1 decision FSM (`EnemyAI.AIState`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AiState {
    Idle,
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
}

impl Enemy {
    /// Spawn at `feet`, initially watching toward `watch` (the player's start), so
    /// the encounter can trigger — a guard on watch rather than facing a wall.
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
            state: AiState::Idle,
            alert_timer: 0.0,
            chase_timer: 0.0,
            cooldown_timer: 0.0,
            is_attacking: false,
            fire_started: false,
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
    /// to start a fire burst this step.
    pub fn update(
        &mut self,
        dt: f32,
        player_feet: Vec3,
        nav: &NavWorld,
        physics: &mut PhysicsWorld,
        fire_anim: bool,
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

        let mut step = EnemyStep::default();
        match self.state {
            AiState::Idle => {
                let dist = self.dist_to(player_feet);
                if dist < DETECTION_RANGE
                    && self.in_cone(player_feet)
                    && line_of_sight(physics, self.pos, player_feet)
                {
                    self.state = AiState::Alert;
                    self.alert_timer = 0.0;
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
                let los = line_of_sight(physics, self.pos, player_feet);
                if dist <= ATTACK_RANGE && !fire_anim && los {
                    self.face(player_feet);
                    self.state = AiState::Attack;
                    self.is_attacking = false;
                    self.path.clear();
                } else if dist > DETECTION_RANGE * 1.5 {
                    self.state = AiState::Idle;
                    self.path.clear();
                } else {
                    self.chase_step(dt, player_feet, nav);
                }
            }
            AiState::Attack => {
                let dist = self.dist_to(player_feet);
                let los = line_of_sight(physics, self.pos, player_feet);
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
                    let los = line_of_sight(physics, self.pos, player_feet);
                    if dist <= ATTACK_RANGE && los {
                        self.state = AiState::Attack;
                        self.is_attacking = false;
                    } else if dist <= DETECTION_RANGE {
                        self.state = AiState::Chase;
                        self.chase_timer = 0.0;
                    } else {
                        self.state = AiState::Idle;
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

    /// Chase movement: periodically repath to the player, then follow the current
    /// waypoints (ported from the original omniscient chaser's body).
    fn chase_step(&mut self, dt: f32, player_feet: Vec3, nav: &NavWorld) {
        self.repath_timer -= dt;
        if self.repath_timer <= 0.0 {
            self.repath_timer = REPATH_INTERVAL;
            if let Some(path) = nav.find_path(self.pos, player_feet) {
                self.path = path;
                self.path_idx = 1.min(self.path.len().saturating_sub(1)); // skip the start cell
            }
        }

        if self.path_idx < self.path.len() {
            let target = self.path[self.path_idx];
            let to = target - self.pos;
            let dist = to.length();
            if dist > 1e-4 {
                let stepd = (SPEED_CHASE * dt).min(dist);
                self.pos += to / dist * stepd;
                let flat = Vec3::new(to.x, 0.0, to.z);
                if flat.length_squared() > 1e-6 {
                    self.heading = flat.normalize();
                }
                self.moving = true;
            }
            if self.pos.distance(target) < WAYPOINT_EPS && self.path_idx < self.path.len() - 1 {
                self.path_idx += 1;
            }
        }
    }
}

/// Rapier line-of-sight from `from_feet` to `to_feet`, cast between chest heights
/// (JS `EnemyAI.hasLineOfSight`). The hunter's own capsule is excluded. Clear when
/// nothing is hit (the native player has no collider), or when the only hit is at
/// essentially the target distance. A wall in between blocks the shot.
pub(crate) fn line_of_sight(physics: &mut PhysicsWorld, from_feet: Vec3, to_feet: Vec3) -> bool {
    let from = from_feet + Vec3::new(0.0, 1.0, 0.0);
    let to = to_feet + Vec3::new(0.0, 0.8, 0.0);
    let d = to - from;
    let dist = d.length();
    if dist < 1e-4 {
        return true;
    }
    let dir = d / dist;
    let exclude = physics.enemy_collider_handle();
    match physics.raycast_excluding(from, dir, dist, exclude) {
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
}
