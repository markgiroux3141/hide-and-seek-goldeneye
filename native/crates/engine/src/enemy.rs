//! A single hunter grunt — the Phase 3 vertical slice: it pathfinds to the
//! player over the baked nav grid and follows the waypoints. Movement constants
//! are ported from `src/game/enemy.js`.
//!
//! Phase 4 adds **breaching**: when an intact door blocks its next path segment,
//! the hunter stops and reports it (`EnemyStep::breaching`) instead of moving;
//! [`crate::world::World`] drains that door's hp and, on break, removes the panel
//! collider + flips the nav flag. Mirrors the door-blocking branch of `enemy.js`.
//!
//! Scope note: this is still an **omniscient chaser** (always paths to the
//! player's current position). The JS enemy's perception state machine — sight
//! cone, hearing, search/patrol, facing ease — is deferred to the combat/AI phase.

use glam::Vec3;

use crate::csg_runtime::WORLD_SCALE;
use crate::nav::NavWorld;

/// The outcome of one enemy step, reported back to [`crate::world::World`].
pub struct EnemyStep {
    /// The player is within catch range this step.
    pub caught: bool,
    /// An intact door (this index) is blocking the next segment — the hunter is
    /// breaching it and did not move. `World` drains its hp.
    pub breaching: Option<usize>,
}

const WT: f32 = WORLD_SCALE;
const SPEED_CHASE: f32 = 2.8; // m/s
const REPATH_INTERVAL: f32 = 0.4; // s between path recomputes
const CATCH_DIST: f32 = 1.2 * WT; // 0.3 m — horizontal catch radius
const WAYPOINT_EPS: f32 = 0.4 * WT; // 0.1 m — advance to next waypoint within this
const CATCH_VERT: f32 = 3.0 * WT; // must be within ~1 floor vertically to catch

pub struct Enemy {
    /// Feet position, meters.
    pub pos: Vec3,
    /// Current path (meters waypoints), and the index we're heading toward.
    path: Vec<Vec3>,
    path_idx: usize,
    repath_timer: f32,
    /// Horizontal facing (unit vector) — the last direction of travel. Drives
    /// the visual model's yaw (B5) and, later, the perception cone.
    heading: Vec3,
    /// Whether the hunter advanced this step (false while breaching / pathless).
    moving: bool,
}

impl Enemy {
    pub fn new(feet: Vec3) -> Self {
        Enemy {
            pos: feet,
            path: Vec::new(),
            path_idx: 0,
            repath_timer: 0.0, // repath immediately on first step
            heading: Vec3::NEG_Z,
            moving: false,
        }
    }

    /// Horizontal facing (unit vector) — the last direction of travel.
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

    /// Advance one step toward the player, or breach a blocking door.
    pub fn update(&mut self, dt: f32, player_feet: Vec3, nav: &NavWorld) -> EnemyStep {
        self.moving = false; // set true below only if we actually advance
        // Periodically recompute the route to the player's current position.
        self.repath_timer -= dt;
        if self.repath_timer <= 0.0 {
            self.repath_timer = REPATH_INTERVAL;
            if let Some(path) = nav.find_path(self.pos, player_feet) {
                self.path = path;
                self.path_idx = 1.min(self.path.len().saturating_sub(1)); // skip the start cell
            }
        }

        let caught = |pos: Vec3| {
            let horiz = Vec3::new(player_feet.x - pos.x, 0.0, player_feet.z - pos.z).length();
            horiz < CATCH_DIST && (player_feet.y - pos.y).abs() < CATCH_VERT
        };

        // Breach: if an intact door blocks the next segment, stop and break it
        // instead of moving (JS `enemy.js` door-blocking branch). `World` drains
        // the hp and, on break, removes the collider + flips the live nav flag.
        if self.path_idx < self.path.len() {
            let target = self.path[self.path_idx];
            if let Some(door) = nav.door_blocking(self.pos, target) {
                return EnemyStep { caught: caught(self.pos), breaching: Some(door) };
            }
        }

        // Follow the current waypoint.
        if self.path_idx < self.path.len() {
            let target = self.path[self.path_idx];
            let to = target - self.pos;
            let dist = to.length();
            if dist > 1e-4 {
                let step = (SPEED_CHASE * dt).min(dist);
                self.pos += to / dist * step;
                // Face the horizontal direction of travel (ignore vertical).
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

        EnemyStep { caught: caught(self.pos), breaching: None }
    }
}
