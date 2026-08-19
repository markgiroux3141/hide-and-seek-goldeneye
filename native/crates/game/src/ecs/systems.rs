//! Systems — the behaviour layer over the components.
//!
//! hecs ships no scheduler, which suits this codebase: systems are plain functions
//! run in a fixed, explicit order by [`run_systems`], the same ordered-pipeline
//! style as [`crate::world::World::fixed_step`]. Each system takes the raw hecs
//! [`HecsWorld`] plus a per-tick [`SystemCtx`] carrying the borrows it needs from
//! the game world (dt, player pose, nav, physics) and a deferred [`Command`] buffer
//! so a system can request structural edits (spawn/despawn) without mutating the
//! archetype set while a query is iterating.
//!
//! Scaffold pass: the ordered list is intentionally **empty** (a documented no-op).
//! The door open/close + enemy-slowdown system's home is marked inside `run_systems`.

use glam::Vec3;
use hecs::{Entity, World as HecsWorld};

use engine::sim::nav::NavWorld;
use engine::sim::physics::PhysicsWorld;

use super::components::{Door, DoorGeom, DoorState, OpeningType, Renderable, Transform};

/// Fraction of full travel a door covers per second at `speed: 1.0` — a 90° swing in
/// ~0.6 s. Brisk, close to the reference implementation's `PI rad/s`, and slow enough
/// that the open sound reads as an event rather than a click.
const DOOR_RATE: f32 = 1.6;

/// Whether this door's *closing* sound is the sound of the panel **travelling** rather
/// than of it latching shut.
///
/// A sliding door or a shutter makes its noise for the whole journey, so the cue belongs
/// at the moment it starts to move. A hinged door's close is a latch — a thunk as it
/// meets the frame — so that cue belongs at the moment it arrives. Playing a swing's
/// close on the way in is what made a manually-shut door sound like it had already
/// closed while you were still watching it swing.
fn closes_on_travel(t: OpeningType) -> bool {
    !matches!(t, OpeningType::Swing)
}

/// A deferred structural change, applied after the query passes so a system never
/// mutates storage mid-iteration.
pub enum Command {
    /// Remove an entity at end of tick.
    Despawn(Entity),
    // Spawn variants land as systems need them (e.g. debris when a crate breaks).
}

/// Everything a system may touch this tick beyond the ECS world itself. Built fresh
/// each fixed step in `fixed_step` from disjoint borrows of the game world, so it
/// coexists with the enemy loop's borrows of the same fields.
pub struct SystemCtx<'a> {
    /// Fixed timestep, seconds (the sim rate — ~1/120 s).
    pub dt: f32,
    /// Player feet position this step (proximity / interaction systems).
    pub player_feet: Vec3,
    /// Live nav grid (HUNT only) — systems may flip runtime overlays such as a
    /// door's pathing cost. `None` in BUILD / headless callers that don't bake nav.
    pub nav: Option<&'a mut NavWorld>,
    /// Physics world — systems may move or toggle prop colliders.
    pub physics: &'a mut PhysicsWorld,
    /// Deferred structural edits, drained by [`apply_commands`] after the passes.
    pub commands: Vec<Command>,
    /// World-positioned one-shots a system wants played, as `(asset name, position)`.
    /// Systems don't hold the audio device — the `World` drains this after the tick and
    /// plays each with distance falloff against the listener, so a cue raised out here
    /// is attenuated exactly like one raised by the player's own actions.
    pub sounds: Vec<(&'static str, Vec3)>,
}

impl SystemCtx<'_> {
    /// Queue an entity for removal at the end of this tick.
    pub fn despawn(&mut self, e: Entity) {
        self.commands.push(Command::Despawn(e));
    }
}

/// Run the ordered system list for one fixed step, then apply deferred commands.
pub fn run_systems(world: &mut HecsWorld, ctx: &mut SystemCtx) {
    // ── Ordered system list ──────────────────────────────────────────────────
    // Runs before the hunter FSM (its caller places it there) so any nav overlay a
    // system sets is visible when hunters path this step.
    door_system(world, ctx);

    apply_commands(world, ctx);
}

/// Advance every live door's open/close animation and keep its collider on the panel.
///
/// The whole tick reads from components alone — [`Door`] for the state and the authored
/// rates, [`DoorGeom`] for the baked pivot, half-extents and collider handle — which is
/// what lets it live here rather than as a `World` method: the bake resolved everything
/// that needed the prop catalog and the GLB bounds, so the per-step work needs nothing
/// outside the ECS but the physics borrow.
fn door_system(world: &mut HecsWorld, ctx: &mut SystemCtx) {
    for (door, geom, t, r) in
        world.query_mut::<(&mut Door, &DoorGeom, &Transform, &Renderable)>()
    {
        let close_sound = crate::props::door_def(r.mesh).map(|d| d.close_sound);
        // Publish the panel's blocking state to the nav overlay every step. A* reads
        // this live, so a hunter's route updates as the door swings — the reason doors
        // are kept out of the frozen nav bake. `is_blocking` (open_frac < 0.5) rather
        // than "fully open" so a door only stops walling the doorway once it is
        // meaningfully ajar.
        if let (Some(i), Some(nav)) = (geom.nav_index, ctx.nav.as_deref_mut()) {
            nav.set_door_open(i, !door.is_blocking());
        }
        let step = DOOR_RATE * door.speed.max(0.0) * ctx.dt;
        match door.state {
            DoorState::Opening => {
                door.open_frac += step;
                if door.open_frac >= 1.0 {
                    door.open_frac = 1.0;
                    door.state = DoorState::Open;
                    door.timer = door.auto_close;
                }
            }
            DoorState::Closing => {
                door.open_frac -= step;
                if door.open_frac <= 0.0 {
                    door.open_frac = 0.0;
                    door.state = DoorState::Closed;
                    // A hinged door latches on arrival — this is where its close is
                    // heard, not when it started swinging.
                    if !closes_on_travel(door.opening_type) {
                        if let Some(name) = close_sound {
                            ctx.sounds.push((name, t.pos));
                        }
                    }
                }
            }
            DoorState::Open => {
                // `auto_close == 0` means "stays open" — never start the countdown.
                if door.auto_close > 0.0 {
                    door.timer -= ctx.dt;
                    if door.timer <= 0.0 {
                        door.state = DoorState::Closing;
                        // A door shutting *itself* is a free audio tell, and it was
                        // silent before: nothing on this path ever raised a cue. A
                        // travelling panel is heard from here; a hinged one from its
                        // latch above.
                        if closes_on_travel(door.opening_type) {
                            if let Some(name) = close_sound {
                                ctx.sounds.push((name, t.pos));
                            }
                        }
                    }
                }
            }
            DoorState::Closed => continue, // nothing to move
        }

        // Keep the collider on the panel. Only doors that are actually moving reach
        // here, so a level full of shut doors costs nothing.
        if let Some(handle) = geom.collider {
            let model = crate::world::tools::door::door_world_matrix(t, geom, door);
            let (center, rot) = crate::world::tools::door::panel_pose(door, geom, model, t.rot);
            ctx.physics.set_door_panel_pose(handle, center, rot);
        }
    }
}

/// Drain and apply the deferred command buffer.
fn apply_commands(world: &mut HecsWorld, ctx: &mut SystemCtx) {
    for cmd in ctx.commands.drain(..) {
        match cmd {
            Command::Despawn(e) => {
                let _ = world.despawn(e);
            }
        }
    }
}
