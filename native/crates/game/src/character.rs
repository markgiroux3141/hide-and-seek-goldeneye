//! First-person character controller for the HUNT phase. A kinematic capsule
//! driven by manual gravity + jump, with collisions/steps/slopes resolved by
//! Rapier's `KinematicCharacterController` (via [`PhysicsWorld::move_character`]).
//!
//! Feel constants are ported verbatim from `src/game/player.js` (meters). The JS
//! resolved collisions against the nav grid; here Rapier's move-and-slide does
//! it against the CSG trimesh colliders — the plan's transliteration, keeping
//! the same tuning so it feels the same.

use glam::Vec3;
use winit::keyboard::KeyCode;

use engine::render::camera::{apply_look_delta, forward_from, view_proj_from};
use engine::geometry::csg_runtime::WORLD_SCALE;
use engine::platform::input::InputState;
use engine::sim::physics::PhysicsWorld;

const WT: f32 = WORLD_SCALE; // meters per world tile

const RADIUS: f32 = 1.0 * WT; // capsule radius (0.25 m)
const HEIGHT: f32 = 6.0 * WT; // full standing height (1.5 m)
const EYE: f32 = 5.4 * WT; // eye offset above feet (1.35 m)
const WALK_SPEED: f32 = 6.4; // m/s
const GRAVITY: f32 = 20.0; // m/s²
const JUMP_VELOCITY: f32 = 5.5; // m/s

// ─── Crouch ──────────────────────────────────────────────────────────────────

/// Crouched height (0.75 m), sized to fit a vent bore with headroom to spare.
///
/// The bore is 4 WT (1.0 m) and [`DESIGN_VENTS_LADDERS.md`'s Rule 1] caps it at 5 WT, so
/// this leaves 0.25 m of clearance at the design size. It cannot usefully go much lower:
/// a capsule's total height is `2·(half_height + radius)`, so with [`RADIUS`] fixed at
/// 0.25 m the floor on this constant is 0.5 m — a zero-length cylinder between two
/// hemispheres.
const CROUCH_HEIGHT: f32 = 3.0 * WT;

/// Crouched eye height (0.675 m) — the standing 0.9 × height ratio, held.
const CROUCH_EYE: f32 = 2.7 * WT;

/// Speed multiplier while fully crouched (2.24 m/s from [`WALK_SPEED`]).
///
/// **This number is doing a second job**, and it is worth stating so nobody "tidies" it.
/// The hunters' footstep sense ignores the player below `MOVE_NOISE_MIN_SPEED` = 2.5 m/s
/// (`world::alert_enemies_to_movement`), so a crouched player at 2.24 m/s is *silent* —
/// crouch is a sneak, and it costs no new mechanism to be one. Raising this above 0.39
/// would silently make crouching audible again.
const CROUCH_SPEED_SCALE: f32 = 0.35;

/// Seconds for a full stand↔crouch transition. Fast enough to duck into a duct under
/// fire, slow enough that the camera drop reads as a movement rather than a teleport.
const CROUCH_TIME: f32 = 0.18;

// ─── Ladders ─────────────────────────────────────────────────────────────────

/// Climb rate, m/s. Well under [`WALK_SPEED`]: a ladder is a commitment, and being slow
/// and exposed on one is the cost that balances reaching somewhere hunters cannot follow.
const CLIMB_SPEED: f32 = 2.2;

/// Upward kick when jumping off a ladder, m/s — enough to clear the lip at the top
/// rather than re-attaching immediately.
const LADDER_HOP: f32 = 3.0;

/// How close to the top of the climb volume counts as topping out, metres.
const LADDER_TOP_MARGIN: f32 = 0.2;
/// Upward velocity given when topping out, m/s — a step over the lip, not a jump.
const LADDER_TOP_STEP: f32 = 2.0;
/// How long after stepping off before a ladder may grab you again, seconds. Without it,
/// gravity pulls you straight back through the volume you just left.
const LADDER_REATTACH_DELAY: f32 = 0.35;
/// How long the top-out push off the wall lasts, seconds. Short — it is a step onto the
/// ledge, and any longer walks the player away from where they meant to arrive.
const LADDER_EXIT_TIME: f32 = 0.22;

/// The player's standing height and horizontal half-width (m) — the capsule as the rest
/// of the game needs to measure it.
///
/// Exposed for blast damage, which is measured to the *body* rather than to a point
/// inside it: a grenade at your boots has to read as touching you, and that is a question
/// about the capsule's extent (see `combat::blast_distance_to_body`).
pub(crate) const PLAYER_HEIGHT: f32 = HEIGHT;
pub(crate) const PLAYER_RADIUS: f32 = RADIUS;

/// How high the player's jump actually reaches (`v²/2g` ≈ 0.76 m) — the ceiling on
/// what they can climb that hunters, who cannot jump at all, cannot.
///
/// Exposed because the nav validation report is a statement about *the difference*
/// between the two mobility models, so it has to read this from the same place the
/// jump does rather than restate it (see `world::nav_issues`).
pub(crate) const JUMP_APEX: f32 = JUMP_VELOCITY * JUMP_VELOCITY / (2.0 * GRAVITY);

/// Capsule cylinder half-height for a given total height: total = 2·(half + radius).
/// Clamped at 0 so a height at or below `2·RADIUS` degenerates to a sphere rather than
/// asking Rapier for a negative-length capsule.
#[inline]
fn half_height_for(height: f32) -> f32 {
    ((height - 2.0 * RADIUS) * 0.5).max(0.0)
}

/// Capsule midpoint above the feet for a given total height.
#[inline]
fn center_offset_for(height: f32) -> f32 {
    height * 0.5
}

pub struct CharacterController {
    /// Feet position, meters.
    pub pos: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    vel_y: f32,
    grounded: bool,
    /// Horizontal speed (m/s) actually achieved on the last [`Self::apply_move`] — the
    /// resolved XZ displacement over dt (so wall-slides / blocked moves read slow). The
    /// hunters' movement-noise sense ([`crate::world::World`]) reads this: the faster you
    /// move, the louder your footsteps, the further a searcher hears you.
    speed_xz: f32,
    /// Resolved planar velocity (m/s, XZ; Y always 0) achieved on the last
    /// [`Self::apply_move`]. The hunters' local-avoidance (ORCA) reads this so they
    /// steer around where the player is *heading*, not just its current cell.
    vel_xz: Vec3,
    /// Crouch blend, 0 = fully standing, 1 = fully crouched. Everything the crouch
    /// affects — capsule height, eye height, speed — is a lerp on this one scalar, so
    /// the transition can never desync between the camera and the collider.
    crouch_t: f32,
    /// Whether the player is *asking* to be crouched this step. Kept apart from
    /// [`Self::crouch_t`] because releasing the key does not by itself stand you up:
    /// under a duct there is nowhere to stand, and the blend holds where it is.
    crouch_held: bool,
    /// The level's ladders as `(min, max, outward normal)` — world metres, baked once at
    /// BUILD→HUNT. Held here rather than queried from the ECS each step because the
    /// controller runs on the fixed step and has no world borrow.
    ///
    /// The normal is carried because topping out needs to step you *off the wall* onto
    /// the ledge; an AABB alone cannot say which way that is.
    ladders: Vec<(Vec3, Vec3, Vec3)>,
    /// Whether the player is attached to a ladder right now.
    on_ladder: bool,
    /// Counts down after stepping off, so gravity dropping you back through the volume
    /// you just left cannot immediately re-attach you.
    ladder_cooldown: f32,
    /// Counts down while the top-out nudge pushes you clear of the lip, carrying the
    /// direction to push.
    ladder_exit: f32,
    ladder_exit_dir: Vec3,
}

impl CharacterController {
    /// Spawn with feet at `feet`, inheriting the given look orientation.
    pub fn new(feet: Vec3, yaw: f32, pitch: f32) -> Self {
        CharacterController {
            pos: feet,
            yaw,
            pitch,
            vel_y: 0.0,
            grounded: false,
            speed_xz: 0.0,
            vel_xz: Vec3::ZERO,
            crouch_t: 0.0,
            crouch_held: false,
            ladders: Vec::new(),
            on_ladder: false,
            ladder_cooldown: 0.0,
            ladder_exit: 0.0,
            ladder_exit_dir: Vec3::ZERO,
        }
    }

    /// Install the level's ladder climb volumes as `(min, max, outward normal)` in world
    /// metres. Called at BUILD→HUNT and on respawn; an empty list disables climbing.
    pub fn set_ladders(&mut self, volumes: Vec<(Vec3, Vec3, Vec3)>) {
        self.ladders = volumes;
    }

    /// Whether the player is on a ladder — for the HUD and for the animation state.
    pub fn is_climbing(&self) -> bool {
        self.on_ladder
    }

    /// The ladder whose climb volume contains a point, if any.
    fn ladder_at(&self, p: Vec3) -> Option<(Vec3, Vec3, Vec3)> {
        self.ladders
            .iter()
            .find(|(min, max, _)| {
                p.x >= min.x && p.x <= max.x
                    && p.y >= min.y && p.y <= max.y
                    && p.z >= min.z && p.z <= max.z
            })
            .copied()
    }

    /// Current capsule height (m), blended between standing and crouched.
    #[inline]
    pub fn height(&self) -> f32 {
        HEIGHT + (CROUCH_HEIGHT - HEIGHT) * self.crouch_t
    }

    /// How crouched the player is: 0 standing, 1 fully crouched.
    #[inline]
    pub fn crouch_amount(&self) -> f32 {
        self.crouch_t
    }

    /// Whether the player is crouched far enough to fit somewhere a standing body
    /// cannot — the test any vent-entry rule should ask, rather than reading
    /// [`Self::crouch_amount`] against a magic number of its own.
    #[inline]
    pub fn is_crouched(&self) -> bool {
        self.crouch_t > 0.99
    }

    /// Request the crouch state for this step (held = down). Driven from the keyboard
    /// bind and the pad's hold-split; the controller decides what it can actually do
    /// about it in [`Self::apply_move`].
    pub fn set_crouch(&mut self, held: bool) {
        self.crouch_held = held;
    }

    /// Mouse-look, once per rendered frame (crisp aim, independent of sim rate).
    pub fn apply_look(&mut self, input: &mut InputState) {
        let (dx, dy) = input.take_mouse_delta();
        if !input.pointer_locked {
            return;
        }
        (self.yaw, self.pitch) = apply_look_delta(self.yaw, self.pitch, dx, dy);
    }

    /// One fixed sim step: horizontal wish-move + gravity/jump, resolved by the
    /// character controller against the static world.
    pub fn apply_move(&mut self, dt: f32, input: &InputState, physics: &mut PhysicsWorld) {
        // Horizontal basis from yaw only (no pitch — feet stay level). `fwd` is the
        // look direction flattened; `right` is its perpendicular (`cross(fwd, up)`).
        let (sy, cy) = self.yaw.sin_cos();
        let fwd = Vec3::new(-sy, 0.0, -cy);
        let right = Vec3::new(cy, 0.0, -sy);
        let mut wish = Vec3::ZERO;
        if input.pointer_locked {
            if input.key_down(KeyCode::KeyW) {
                wish += fwd;
            }
            if input.key_down(KeyCode::KeyS) {
                wish -= fwd;
            }
            if input.key_down(KeyCode::KeyA) {
                wish -= right;
            }
            if input.key_down(KeyCode::KeyD) {
                wish += right;
            }
        }
        // Analog stick (gamepad) wish, added on top of the digital keys. Ported from
        // `PlayerController.update`: analog preserves partial magnitude (clamp to the
        // unit circle, don't normalize) so a half-pushed stick walks at half speed;
        // a purely-digital wish normalizes to full speed as before.
        let (ax, ay) = input.analog_move();
        if input.pointer_locked && (ax != 0.0 || ay != 0.0) {
            wish += right * ax + fwd * ay;
            if wish.length_squared() > 1.0 {
                wish = wish.normalize();
            }
        } else if wish.length_squared() > 0.0 {
            wish = wish.normalize();
        }

        // Crouch is read as a key like the others, so the pad drives it through the same
        // `drive_key` channel W/A/S/D already use rather than needing its own seam. A
        // caller may also set it directly via [`Self::set_crouch`].
        if input.pointer_locked {
            self.crouch_held = input.key_down(KeyCode::ControlLeft);
        }

        // ── Crouch blend ──
        // Ducking is always allowed; standing up is not. Releasing crouch inside a duct
        // has to be refused, or the player grows through the slab and Rapier ejects them
        // somewhere unpredictable — so the stand-up is *gated on a shape query at the
        // height it would reach*, not merely started and hoped for.
        //
        // The query runs against the height one step of blend away rather than full
        // standing height, so clearing a low ceiling raises you as far as it allows
        // instead of holding you flat until the whole 0.75 m is free.
        let target = if self.crouch_held { 1.0 } else { 0.0 };
        if target > self.crouch_t {
            self.crouch_t = (self.crouch_t + dt / CROUCH_TIME).min(1.0);
        } else if target < self.crouch_t {
            let next = (self.crouch_t - dt / CROUCH_TIME).max(0.0);
            let next_h = HEIGHT + (CROUCH_HEIGHT - HEIGHT) * next;
            let probe = self.pos + Vec3::new(0.0, center_offset_for(next_h), 0.0);
            if physics.capsule_free(RADIUS, half_height_for(next_h), probe) {
                self.crouch_t = next;
            }
        }
        let height = self.height();

        // ── Ladders ──
        //
        // Attaching is positional — no grab key, because a ladder you have to *ask* to
        // use is a ladder you fumble while being shot at — but it is **not** merely
        // "inside the volume". That was the first version and it left you stuck twice
        // over: standing at the foot of a ladder you had no wish to climb, gravity was
        // off and forward/back were spent on the climb axis, so you could only shuffle
        // sideways out; and at the top there was no way off at all, because the volume
        // reaches the ledge and you never leave it.
        //
        // So attaching takes *intent* (a climb input) and detaching has three exits:
        // topping out onto the ledge, reaching the floor, or jumping off.
        self.ladder_cooldown = (self.ladder_cooldown - dt).max(0.0);
        self.ladder_exit = (self.ladder_exit - dt).max(0.0);
        let jump = input.pointer_locked && input.key_down(KeyCode::Space);
        let (ax, ay) = input.analog_move();
        let keyed = input.key_down(KeyCode::KeyW) as i32 - input.key_down(KeyCode::KeyS) as i32;
        let climb = if keyed != 0 { keyed as f32 } else if input.pointer_locked { ay } else { 0.0 };

        let center_now = self.pos + Vec3::new(0.0, center_offset_for(height), 0.0);
        let vol = self.ladder_at(center_now).or_else(|| self.ladder_at(self.pos));
        let mut engaged = false;
        if let Some((_, lmax, out)) = vol {
            if self.ladder_cooldown <= 0.0 && !jump {
                // Already climbing stays climbing; otherwise it takes a deliberate
                // up/down input to grab on.
                engaged = self.on_ladder || climb != 0.0;
                if engaged && climb > 0.0 && self.pos.y >= lmax.y - LADDER_TOP_MARGIN {
                    // Topped out: hand the player to the ledge with a step up and a
                    // push off the wall, and refuse to re-attach for a beat so falling
                    // back through the volume cannot grab them again mid-step.
                    engaged = false;
                    self.vel_y = LADDER_TOP_STEP;
                    self.ladder_cooldown = LADDER_REATTACH_DELAY;
                    self.ladder_exit = LADDER_EXIT_TIME;
                    self.ladder_exit_dir = out;
                } else if engaged && self.grounded && climb <= 0.0 {
                    // At the foot with no wish to go up: this is standing next to a
                    // ladder, not hanging on one.
                    engaged = false;
                }
            }
        }
        self.on_ladder = engaged;

        if self.on_ladder {
            // W climbs, S descends, A/D still strafe so you can step off sideways at a
            // landing. Forward/back are spent on the vertical here rather than projected
            // through the look direction: pitching to climb means you cannot look around
            // while you do it, and a ladder is exactly where you want to be watching.
            self.vel_y = climb * CLIMB_SPEED;
            // Rebuild the horizontal wish from the strafe axes ALONE, rather than
            // subtracting the forward term back out of the one computed above. Same
            // result for the keyboard, but the analog stick also feeds `fwd`, and
            // cancelling it by hand would have quietly left half a stick's worth of
            // forward drive on while climbing.
            let strafe = (input.key_down(KeyCode::KeyD) as i32
                - input.key_down(KeyCode::KeyA) as i32) as f32;
            wish = right * if strafe != 0.0 { strafe } else { ax };
            if jump {
                // Push off: hop up and away so you clear the lip instead of instantly
                // re-attaching to the volume you are still standing in.
                self.on_ladder = false;
                self.vel_y = LADDER_HOP;
                self.grounded = false;
                self.ladder_cooldown = LADDER_REATTACH_DELAY;
            }
        } else {
            // Gravity + jump (held Space re-jumps on landing, matching player.js).
            // Crouched, there is no jump: a duck that can launch you is a way to skip
            // geometry, and the nav report's player-only-climb accounting is calibrated
            // against a *standing* JUMP_APEX.
            self.vel_y -= GRAVITY * dt;
            if self.grounded && jump && self.crouch_t <= 0.0 {
                self.vel_y = JUMP_VELOCITY;
                self.grounded = false;
            }
            // Carry the top-out step forward, away from the wall, so the player ends up
            // standing on the ledge rather than dropping straight back down the shaft.
            if self.ladder_exit > 0.0 {
                wish += self.ladder_exit_dir;
                if wish.length_squared() > 1.0 {
                    wish = wish.normalize();
                }
            }
        }

        // Crouched movement is slowed — and thereby silenced, see CROUCH_SPEED_SCALE.
        let speed = WALK_SPEED * (1.0 + (CROUCH_SPEED_SCALE - 1.0) * self.crouch_t);
        let desired = wish * speed * dt + Vec3::new(0.0, self.vel_y * dt, 0.0);
        let center = self.pos + Vec3::new(0.0, center_offset_for(height), 0.0);
        let (corrected, grounded) =
            physics.move_character(dt, RADIUS, half_height_for(height), center, desired);

        self.pos += corrected;
        self.grounded = grounded;
        // Horizontal speed actually travelled this step (XZ only), for the enemy
        // movement-noise sense. Measured off the resolved displacement so hugging a
        // wall (little real motion) is quiet.
        let horiz_v = Vec3::new(corrected.x, 0.0, corrected.z);
        let horiz = horiz_v.length();
        self.speed_xz = if dt > 1e-6 { horiz / dt } else { 0.0 };
        self.vel_xz = if dt > 1e-6 { horiz_v / dt } else { Vec3::ZERO };
        // Stop accumulating fall speed once the floor is under us — but never while
        // climbing, where `vel_y` is a *commanded* rate rather than accumulated gravity.
        // A ladder that starts at floor level is grounded on its first rungs, and
        // zeroing there would pin the player to the bottom of it.
        if grounded && self.vel_y < 0.0 && !self.on_ladder {
            self.vel_y = 0.0;
        }
    }

    /// Horizontal speed (m/s) achieved on the last [`Self::apply_move`]. ~[`WALK_SPEED`]
    /// at a full run, 0 when standing still or pinned against a wall.
    pub fn speed(&self) -> f32 {
        self.speed_xz
    }

    /// Resolved planar velocity (m/s, XZ; Y = 0) from the last [`Self::apply_move`] —
    /// the hunters' local-avoidance treats the player as a moving disc obstacle.
    pub fn velocity(&self) -> Vec3 {
        self.vel_xz
    }

    pub fn view_proj(&self, aspect: f32) -> glam::Mat4 {
        view_proj_from(self.eye(), self.forward(), aspect)
    }

    /// Eye (camera) position in world space — feet + eye height, lowered as the player
    /// crouches. The fire ray originates here (the crosshair is at the eye centre), so
    /// a crouched player genuinely shoots from a crouched eyeline.
    pub fn eye(&self) -> Vec3 {
        self.pos + Vec3::new(0.0, EYE + (CROUCH_EYE - EYE) * self.crouch_t, 0.0)
    }

    /// Unit look direction (yaw + pitch). The fire ray travels along this, and the
    /// camera looks along it.
    pub fn forward(&self) -> Vec3 {
        forward_from(self.yaw, self.pitch)
    }
}
