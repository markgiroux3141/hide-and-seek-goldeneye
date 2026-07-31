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
    // Empty this pass. The door open/close + enemy-slowdown system slots here as
    //     door_system(world, ctx);
    // running before the hunter FSM (its caller places it there) so any nav-cost
    // overlay it sets is visible when hunters path this step.
    //
    // Reference the ctx fields so the seam compiles cleanly with no systems yet
    // (and so adding the first system needs no signature churn).
    let _ = (ctx.dt, ctx.player_feet, ctx.nav.is_some(), &ctx.physics, world.len());

    apply_commands(world, ctx);
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
