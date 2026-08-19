//! Rapier3D wrapper. Phase 1 scope: the CSG → collision pipeline — per-region
//! static trimesh colliders that are rebuilt in place on every brush edit, plus
//! a ray query for crosshair face-picking. The kinematic character controller
//! lands in Phase 2 on top of this same world.
//!
//! Also retains the Phase 0 [`smoke_test`] as a link/step sanity check.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;

use glam::{Quat, Vec3};
use rapier3d::control::{CharacterAutostep, CharacterLength, KinematicCharacterController};
use rapier3d::prelude::*;

use crate::render::mesh::CpuMesh;

/// A single ray hit: world-space point, the surface normal there, and the
/// collider it landed on (so hitscan can tell an enemy hit from a wall hit).
pub struct RayHit {
    pub point: Vec3,
    pub normal: Vec3,
    pub collider: ColliderHandle,
}

/// The collision world. Holds one static trimesh collider per CSG region, keyed
/// by region id, so a BUILD-phase edit rebuilds exactly one body (per the plan's
/// per-region collision model). Geometry is in meters.
pub struct PhysicsWorld {
    colliders: ColliderSet,
    bodies: RigidBodySet,
    query_pipeline: QueryPipeline,
    /// region id → collider handle, for in-place replacement on re-bake.
    region_colliders: HashMap<u32, ColliderHandle>,
    /// Door panel colliders, indexed to match the nav door overlay. `None` after
    /// a breach removes one. Cleared on return to BUILD.
    door_colliders: Vec<Option<ColliderHandle>>,
    /// Destructible-prop colliders (Milestone 3): a static cuboid per placed
    /// destructible prop, baked at BUILD→HUNT so player shots + player movement hit
    /// it, and removed as each prop is destroyed. Default groups (like the region /
    /// door colliders), so the player's hitscan + move query see them for free.
    /// Excluded from perception line-of-sight (below) so adding cover never shifts
    /// the hunter-perception baseline. Emptied on return to BUILD.
    /// Colliders for **openable** door panels (the ECS door props). Separate from
    /// `door_colliders` above, which is the older fixed breach-panel path: these carry
    /// a rotation and are re-posed every step as the panel swings or slides.
    door_panels: HashSet<ColliderHandle>,
    prop_colliders: HashSet<ColliderHandle>,
    /// The hunters' capsule colliders (Track A) — bare colliders repositioned each
    /// fixed step so hitscan can hit an enemy. One per live hunter, keyed by handle;
    /// emptied outside HUNT and each entry removed as its hunter dies. All are
    /// excluded from the player's move query (the JS enemy doesn't physically block
    /// the player), so the player never jams on a hunter.
    enemy_colliders: HashSet<ColliderHandle>,
    /// Vertical offset (metres) from a hunter's feet to its capsule centre, stored
    /// so `update_enemy_collider` can reposition from a feet position. Uniform — all
    /// hunters share the same capsule size (same character scale).
    enemy_capsule_offset: f32,
    dirty: bool,
    /// Kinematic character controller (stateless config; we own the capsule).
    character: KinematicCharacterController,

    // ── Dynamics substrate (ragdoll) ──────────────────────────────────────────
    // The world above is query-only: static colliders + kinematic capsules moved by
    // hand, no forces. These fields add a real rigid-body solver that is stepped
    // ONLY while ≥1 ragdoll body is live ([`Self::step_dynamics`] early-returns at
    // `ragdoll_count == 0`), so the common HUNT frame pays nothing. Ragdoll dynamic
    // bodies collide against the existing static level colliders for free.
    gravity: Vector<f32>,
    integration_parameters: IntegrationParameters,
    physics_pipeline: PhysicsPipeline,
    islands: IslandManager,
    broad_phase: DefaultBroadPhase,
    narrow_phase: NarrowPhase,
    impulse_joints: ImpulseJointSet,
    multibody_joints: MultibodyJointSet,
    ccd_solver: CCDSolver,
    /// Live ragdoll dynamic bodies. The dynamics step is skipped entirely when this
    /// is empty (the query-only common case).
    ragdoll_bodies: HashSet<RigidBodyHandle>,
    /// Ragdoll colliders, excluded from perception line-of-sight so a corpse on the
    /// floor can't newly block a hunter's sight of the player (keeps the difficulty-0
    /// perception baseline intact). Hitscan still hits them (a shot into a corpse
    /// sparks — harmless + realistic).
    ragdoll_colliders: HashSet<ColliderHandle>,
}

/// Collision group for ragdoll bodies: they collide with the static world but NOT
/// with one another (a corpse's own limbs, and separate corpses, don't fight — the
/// standard stable ragdoll choice; slight limb interpenetration is acceptable).
const GROUP_RAGDOLL: Group = Group::GROUP_2;
/// Collision group for the hunters' hitscan capsules. Ragdoll bodies are filtered to
/// NOT collide with it: a living-hit reaction ragdoll is seeded coincident with the
/// hunter's own capsule, and without this exclusion the solver would explode them
/// apart on frame 0. (A death corpse also then won't shove a live hunter — fine.)
const GROUP_ENEMY: Group = Group::GROUP_3;

/// Per-body velocity clamps applied every dynamics step so a solver spike can't fling a
/// ragdoll limb "insanely fast" (the twitching). Caps linear (m/s) + angular (rad/s)
/// speed — generous enough for a natural tumble, tight enough to kill the jitter.
const RAGDOLL_MAX_LINVEL: f32 = 7.0;
const RAGDOLL_MAX_ANGVEL: f32 = 10.0;
/// Cone half-angle (rad) each ragdoll joint may swing on every axis (~52°). Turns the
/// free ball joints into LIMITED ones so limbs can't fold through the body into a pile
/// of goo, while still allowing a believably loose tumble.
const RAGDOLL_JOINT_CONE: f32 = 0.9;

impl Default for PhysicsWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl PhysicsWorld {
    pub fn new() -> Self {
        // Character controller tuned to feel-match the JS player (step offset,
        // ground-snap, ~50° climbable slope). Constants come from player.js via
        // the caller; these are the resolver behaviors.
        let mut character = KinematicCharacterController::default();
        character.offset = CharacterLength::Absolute(0.01);
        character.autostep = Some(CharacterAutostep {
            max_height: CharacterLength::Absolute(0.25), // JS STEP_HEIGHT = 1 WT
            min_width: CharacterLength::Absolute(0.1),
            include_dynamic_bodies: false,
        });
        character.snap_to_ground = Some(CharacterLength::Absolute(0.25));
        character.max_slope_climb_angle = 50f32.to_radians();

        // Stiffer joint solving so the ragdoll chain holds together (fewer iterations
        // reads as rubbery/goo); only paid while a ragdoll is actually being stepped.
        let mut integration_parameters = IntegrationParameters::default();
        integration_parameters.num_solver_iterations = NonZeroUsize::new(8).unwrap();

        PhysicsWorld {
            colliders: ColliderSet::new(),
            bodies: RigidBodySet::new(),
            query_pipeline: QueryPipeline::new(),
            region_colliders: HashMap::new(),
            door_colliders: Vec::new(),
            door_panels: HashSet::new(),
            prop_colliders: HashSet::new(),
            enemy_colliders: HashSet::new(),
            enemy_capsule_offset: 0.0,
            dirty: true,
            character,
            gravity: vector![0.0, -9.81, 0.0],
            integration_parameters,
            physics_pipeline: PhysicsPipeline::new(),
            islands: IslandManager::new(),
            broad_phase: DefaultBroadPhase::new(),
            narrow_phase: NarrowPhase::new(),
            impulse_joints: ImpulseJointSet::new(),
            multibody_joints: MultibodyJointSet::new(),
            ccd_solver: CCDSolver::new(),
            ragdoll_bodies: HashSet::new(),
            ragdoll_colliders: HashSet::new(),
        }
    }

    /// Insert a static cuboid collider for a door panel (meters AABB) and return
    /// its door index. The panel blocks the player like a wall until it's
    /// breached; indices stay aligned with the nav door overlay.
    pub fn add_door_collider(&mut self, min: Vec3, max: Vec3) -> usize {
        let center = (min + max) * 0.5;
        let half = (max - min) * 0.5;
        let collider = ColliderBuilder::cuboid(half.x, half.y, half.z)
            .translation(vector![center.x, center.y, center.z])
            .build();
        let handle = self.colliders.insert(collider);
        self.door_colliders.push(Some(handle));
        self.dirty = true;
        self.door_colliders.len() - 1
    }

    /// Remove a door panel collider (the breach). After this the opening is
    /// passable — a Rapier collider gone, with no trimesh/nav rebuild.
    pub fn remove_door_collider(&mut self, idx: usize) {
        if let Some(slot) = self.door_colliders.get_mut(idx) {
            if let Some(handle) = slot.take() {
                self.colliders
                    .remove(handle, &mut IslandManager::new(), &mut self.bodies, false);
                self.dirty = true;
            }
        }
    }

    /// Count of door panel colliders still present (test/inspection helper).
    pub fn door_collider_count(&self) -> usize {
        self.door_colliders.iter().filter(|s| s.is_some()).count()
    }

    /// Remove every door panel collider (on return to BUILD).
    pub fn clear_door_colliders(&mut self) {
        for slot in self.door_colliders.drain(..) {
            if let Some(handle) = slot {
                self.colliders
                    .remove(handle, &mut IslandManager::new(), &mut self.bodies, false);
            }
        }
        self.dirty = true;
    }

    // ── Openable door panels ──────────────────────────────────────────────────
    // (helper `door_iso` lives at the bottom of the file)

    /// Insert the moving collider for an **openable door panel** and return its handle.
    ///
    /// Distinct from [`Self::add_door_collider`] above (the old fixed breach panel) in
    /// the one way that matters: this box carries a **rotation**, because a swinging
    /// panel is not axis-aligned once it starts to move. Half-extents are in the
    /// panel's own frame; `center` and `rot` are world-space and get rewritten every
    /// step by [`Self::set_door_panel_pose`] as the door animates.
    ///
    /// Default collision groups, so the panel is ordinary world geometry: the player's
    /// move-and-slide stops at it, hitscan hits it, and — unlike a destructible prop —
    /// it **blocks line of sight**. That last point is deliberate and is what makes
    /// hiding behind a shut door mean anything. No special-casing is needed for the
    /// open state: the collider physically swings out of the doorway with the panel, so
    /// a ray through the cleared opening simply misses it.
    pub fn add_door_panel(&mut self, center: Vec3, half: Vec3, rot: Quat) -> ColliderHandle {
        let collider = ColliderBuilder::cuboid(half.x.max(1e-3), half.y.max(1e-3), half.z.max(1e-3))
            .position(door_iso(center, rot))
            .build();
        let handle = self.colliders.insert(collider);
        self.door_panels.insert(handle);
        self.dirty = true;
        handle
    }

    /// Move an open/closing door panel's collider to match the drawn panel. Called
    /// each fixed step for every door that is mid-animation; a no-op for a handle that
    /// isn't a live panel.
    pub fn set_door_panel_pose(&mut self, handle: ColliderHandle, center: Vec3, rot: Quat) {
        if !self.door_panels.contains(&handle) {
            return;
        }
        if let Some(c) = self.colliders.get_mut(handle) {
            c.set_position(door_iso(center, rot));
            self.dirty = true;
        }
    }

    /// Whether `handle` is a live openable door panel (so a shot can be routed to the
    /// door rather than sparking off it as anonymous world geometry).
    pub fn is_door_panel(&self, handle: ColliderHandle) -> bool {
        self.door_panels.contains(&handle)
    }

    /// Remove every openable door panel (on return to BUILD).
    pub fn clear_door_panels(&mut self) {
        for handle in self.door_panels.drain().collect::<Vec<_>>() {
            self.colliders
                .remove(handle, &mut IslandManager::new(), &mut self.bodies, false);
        }
        self.dirty = true;
    }

    // ── Destructible prop colliders (Milestone 3) ─────────────────────────────

    /// Insert a static cuboid collider (metres AABB) for a destructible prop and
    /// return its handle. Default collision groups — so the player's hitscan (default
    /// filter) and move-and-slide both see it — mirroring the door-panel cuboid path.
    /// The caller maps the handle back to its prop entity. Baked at BUILD→HUNT.
    pub fn add_prop_collider(&mut self, min: Vec3, max: Vec3) -> ColliderHandle {
        let center = (min + max) * 0.5;
        let half = (max - min) * 0.5;
        let collider = ColliderBuilder::cuboid(half.x.max(1e-3), half.y.max(1e-3), half.z.max(1e-3))
            .translation(vector![center.x, center.y, center.z])
            .build();
        let handle = self.colliders.insert(collider);
        self.prop_colliders.insert(handle);
        self.dirty = true;
        handle
    }

    /// Remove one prop collider (the prop was destroyed). After this a shot passes
    /// through where it stood. No-op if the handle isn't a live prop collider.
    pub fn remove_prop_collider(&mut self, handle: ColliderHandle) {
        if self.prop_colliders.remove(&handle) {
            self.colliders
                .remove(handle, &mut IslandManager::new(), &mut self.bodies, false);
            self.dirty = true;
        }
    }

    /// Whether `handle` is a live destructible-prop collider (so hitscan can route a
    /// shot into a prop instead of sparking off it as world geometry).
    pub fn is_prop_collider(&self, handle: ColliderHandle) -> bool {
        self.prop_colliders.contains(&handle)
    }

    /// Remove every prop collider (on return to BUILD).
    pub fn clear_prop_colliders(&mut self) {
        for handle in self.prop_colliders.drain().collect::<Vec<_>>() {
            self.colliders
                .remove(handle, &mut IslandManager::new(), &mut self.bodies, false);
        }
        self.dirty = true;
    }

    /// Spawn one hunter's capsule collider at `feet` (metres), sized `radius` ×
    /// `half_height` (the cylindrical part; the caps add `radius` each end). The
    /// capsule is centred so its bottom cap sits at the feet. Added at G→HUNT (once
    /// per hunter); repositioned each fixed step by [`Self::update_enemy_collider`].
    /// Returns the handle so the caller can move it, remove it on death, and match
    /// it against a hitscan hit. All hunters share the same capsule offset.
    pub fn add_enemy_collider(
        &mut self,
        feet: Vec3,
        radius: f32,
        half_height: f32,
    ) -> ColliderHandle {
        self.enemy_capsule_offset = half_height + radius;
        let c = feet + Vec3::new(0.0, self.enemy_capsule_offset, 0.0);
        let collider = ColliderBuilder::capsule_y(half_height, radius)
            .translation(vector![c.x, c.y, c.z])
            // Tag the group so ragdoll bodies can be filtered off it (see GROUP_ENEMY);
            // player-move / hitscan queries use predicates, not groups, so unaffected.
            .collision_groups(InteractionGroups::new(GROUP_ENEMY, Group::ALL))
            .build();
        let handle = self.colliders.insert(collider);
        self.enemy_colliders.insert(handle);
        self.dirty = true;
        handle
    }

    /// Reposition one hunter's capsule to a new `feet` position (metres), by its
    /// `handle`. Marks the query pipeline dirty so the next raycast/character-move
    /// sees the moved capsule — the per-frame-moving collider the static
    /// dirty-tracking would otherwise miss. No-op if the handle is gone (dead).
    pub fn update_enemy_collider(&mut self, handle: ColliderHandle, feet: Vec3) {
        if !self.enemy_colliders.contains(&handle) {
            return;
        }
        if let Some(collider) = self.colliders.get_mut(handle) {
            let c = feet + Vec3::new(0.0, self.enemy_capsule_offset, 0.0);
            collider.set_translation(vector![c.x, c.y, c.z]);
            self.dirty = true;
        }
    }

    /// Remove one hunter's capsule collider (on its death), by `handle`. After this
    /// a shot passes through where that corpse was.
    pub fn remove_enemy_collider(&mut self, handle: ColliderHandle) {
        if self.enemy_colliders.remove(&handle) {
            self.colliders
                .remove(handle, &mut IslandManager::new(), &mut self.bodies, false);
            self.dirty = true;
        }
    }

    /// Remove every hunter's capsule collider (on return to BUILD).
    pub fn clear_enemy_colliders(&mut self) {
        for handle in self.enemy_colliders.drain().collect::<Vec<_>>() {
            self.colliders
                .remove(handle, &mut IslandManager::new(), &mut self.bodies, false);
        }
        self.dirty = true;
    }

    /// Whether `handle` is a live hunter capsule (for hitscan to tell an enemy hit
    /// from a wall hit, and to find which hunter a shot landed on).
    pub fn is_enemy_collider(&self, handle: ColliderHandle) -> bool {
        self.enemy_colliders.contains(&handle)
    }

    /// Insert or replace the static trimesh collider for a region. Called on
    /// every brush edit; the old collider (if any) is removed first so the
    /// region always has exactly one up-to-date body. An empty mesh just clears
    /// the region's collider.
    pub fn set_region_collider(&mut self, region_id: u32, mesh: &CpuMesh) {
        if let Some(old) = self.region_colliders.remove(&region_id) {
            self.colliders
                .remove(old, &mut IslandManager::new(), &mut self.bodies, false);
        }
        if mesh.indices.is_empty() {
            self.dirty = true;
            return;
        }

        let verts: Vec<Point<f32>> = mesh
            .vertices
            .iter()
            .map(|v| point![v.pos[0], v.pos[1], v.pos[2]])
            .collect();
        let tris: Vec<[u32; 3]> = mesh
            .indices
            .chunks_exact(3)
            .map(|c| [c[0], c[1], c[2]])
            .collect();

        let collider = ColliderBuilder::trimesh(verts, tris).build();
        let handle = self.colliders.insert(collider);
        self.region_colliders.insert(region_id, handle);
        self.dirty = true;
    }

    /// Refresh the acceleration structure if any collider changed since the last
    /// query. Cheap when nothing is dirty.
    fn ensure_current(&mut self) {
        if self.dirty {
            self.query_pipeline.update(&self.colliders);
            self.dirty = false;
        }
    }

    /// Cast a ray and return the first hit point + normal, if any. `dir` need
    /// not be normalized. Used for crosshair face-picking.
    pub fn raycast(&mut self, origin: Vec3, dir: Vec3, max_toi: f32) -> Option<RayHit> {
        self.raycast_excluding(origin, dir, max_toi, None)
    }

    /// As [`PhysicsWorld::raycast`], but excluding one collider from the query.
    /// Player hitscan uses this to exclude the player's own capsule (JS
    /// `castRayAndGetNormal(..., playerCollider)`).
    ///
    /// NB: today the native player is a *transient shape-cast* (see
    /// [`PhysicsWorld::move_character`]), not a registered collider — so there's
    /// no player handle to pass and this is effectively `raycast`. The exclude
    /// path is threaded now for when Track A adds enemy/player colliders.
    pub fn raycast_excluding(
        &mut self,
        origin: Vec3,
        dir: Vec3,
        max_toi: f32,
        exclude: Option<ColliderHandle>,
    ) -> Option<RayHit> {
        self.ensure_current();
        let ray = Ray::new(
            point![origin.x, origin.y, origin.z],
            vector![dir.x, dir.y, dir.z],
        );
        let mut filter = QueryFilter::default();
        if let Some(h) = exclude {
            filter = filter.exclude_collider(h);
        }
        let (handle, intersection) = self.query_pipeline.cast_ray_and_get_normal(
            &self.bodies,
            &self.colliders,
            &ray,
            max_toi,
            true,
            filter,
        )?;
        let p = ray.point_at(intersection.time_of_impact);
        let n = intersection.normal;
        Some(RayHit {
            point: Vec3::new(p.x, p.y, p.z),
            normal: Vec3::new(n.x, n.y, n.z),
            collider: handle,
        })
    }

    /// Raycast that ignores ALL enemy capsules (friendly actors) — only WORLD
    /// geometry (walls / floors / doors) can block it. Used for perception
    /// line-of-sight: a packmate standing on the ray must not hide the player from a
    /// hunter's sight (which caused hunters to flip-flop engage/disengage and "give
    /// up"). Shooting keeps the normal [`Self::raycast_excluding`], so a friendly in
    /// the line still blocks a shot.
    pub fn raycast_world_only(&mut self, origin: Vec3, dir: Vec3, max_toi: f32) -> Option<RayHit> {
        self.ensure_current();
        let ray = Ray::new(
            point![origin.x, origin.y, origin.z],
            vector![dir.x, dir.y, dir.z],
        );
        let enemy_colliders = &self.enemy_colliders;
        let ragdoll_colliders = &self.ragdoll_colliders;
        let prop_colliders = &self.prop_colliders;
        // Skip live hunter capsules (a packmate mustn't hide the player), ragdoll
        // corpses (a body on the floor mustn't newly block sight), AND destructible
        // props (a placed crate is physical for shots/movement but is deliberately NOT
        // a new sight-blocker) — all to keep the difficulty-0 perception baseline
        // unchanged. World geometry (walls/floors) still blocks sight.
        let predicate = |handle: ColliderHandle, _: &Collider| {
            !enemy_colliders.contains(&handle)
                && !ragdoll_colliders.contains(&handle)
                && !prop_colliders.contains(&handle)
        };
        let filter = QueryFilter::default().predicate(&predicate);
        let (handle, intersection) = self.query_pipeline.cast_ray_and_get_normal(
            &self.bodies,
            &self.colliders,
            &ray,
            max_toi,
            true,
            filter,
        )?;
        let p = ray.point_at(intersection.time_of_impact);
        let n = intersection.normal;
        Some(RayHit {
            point: Vec3::new(p.x, p.y, p.z),
            normal: Vec3::new(n.x, n.y, n.z),
            collider: handle,
        })
    }

    /// Move a character capsule against the static world with move-and-slide,
    /// autostep, and ground-snap. `capsule_center` is the world position of the
    /// capsule's midpoint; `desired` is the attempted translation this step.
    /// Returns the collision-corrected translation and whether it ended grounded.
    pub fn move_character(
        &mut self,
        dt: f32,
        radius: f32,
        half_height: f32,
        capsule_center: Vec3,
        desired: Vec3,
    ) -> (Vec3, bool) {
        self.ensure_current();
        let shape = Capsule::new_y(half_height, radius);
        let pos = Isometry::translation(capsule_center.x, capsule_center.y, capsule_center.z);
        // Exclude every hunter's capsule: the JS enemy walks its own path and does
        // not physically block the player, and (crucially) with both capsule radii
        // summing to ~0.55 m the collision would stop the hunter well short of the
        // 0.3 m catch radius — it could never catch the player. Hitscan still hits
        // the enemies (that query keeps the default filter). A predicate filter
        // rejects all enemy colliders at once (there can be several hunters).
        let enemy_colliders = &self.enemy_colliders;
        let predicate = |handle: ColliderHandle, _: &Collider| !enemy_colliders.contains(&handle);
        let filter = QueryFilter::default().predicate(&predicate);
        let movement = self.character.move_shape(
            dt,
            &self.bodies,
            &self.colliders,
            &self.query_pipeline,
            &shape,
            &pos,
            vector![desired.x, desired.y, desired.z],
            filter,
            |_collision| {},
        );
        let t = movement.translation;
        (Vec3::new(t.x, t.y, t.z), movement.grounded)
    }

    // ── Dynamics substrate (ragdoll) ──────────────────────────────────────────

    /// Advance the rigid-body solver one step (`dt` seconds). A **no-op** while no
    /// ragdoll body is live, so a normal HUNT frame pays nothing. When ragdolls are
    /// active it integrates gravity + joints + contacts against the static level
    /// colliders, moving the dynamic bodies in place. Call once per fixed step. Marks
    /// the query pipeline dirty so a subsequent raycast sees the moved bodies.
    pub fn step_dynamics(&mut self, dt: f32) {
        if self.ragdoll_bodies.is_empty() {
            return;
        }
        // Clamp BEFORE (caps the spawn impulse — a hit on a light bone would otherwise
        // launch it "insanely fast") and AFTER (caps any in-step penetration spike before
        // the pose read-back sees it) the integration.
        self.clamp_ragdoll_velocities();
        self.integration_parameters.dt = dt;
        self.physics_pipeline.step(
            &self.gravity,
            &self.integration_parameters,
            &mut self.islands,
            &mut self.broad_phase,
            &mut self.narrow_phase,
            &mut self.bodies,
            &mut self.colliders,
            &mut self.impulse_joints,
            &mut self.multibody_joints,
            &mut self.ccd_solver,
            None,
            &(),
            &(),
        );
        self.clamp_ragdoll_velocities();
        self.dirty = true; // bodies moved → the acceleration structure is stale
    }

    /// Cap every ragdoll body's linear + angular speed to [`RAGDOLL_MAX_LINVEL`] /
    /// [`RAGDOLL_MAX_ANGVEL`] — the anti-twitch clamp, applied each side of the step.
    fn clamp_ragdoll_velocities(&mut self) {
        for &h in &self.ragdoll_bodies {
            if let Some(b) = self.bodies.get_mut(h) {
                let lv = *b.linvel();
                let l = lv.norm();
                if l > RAGDOLL_MAX_LINVEL {
                    b.set_linvel(lv * (RAGDOLL_MAX_LINVEL / l), false);
                }
                let av = *b.angvel();
                let a = av.norm();
                if a > RAGDOLL_MAX_ANGVEL {
                    b.set_angvel(av * (RAGDOLL_MAX_ANGVEL / a), false);
                }
            }
        }
    }

    /// Spawn one dynamic ragdoll capsule at world isometry (`rot`, `pos`), with the
    /// capsule running between local endpoints `a`→`b` (body-frame metres) of `radius`.
    /// The body has CCD on (so a hard impulse can't tunnel the floor) and mild damping
    /// (so it settles). Ragdoll bodies never collide with each other ([`GROUP_RAGDOLL`]),
    /// only the static world. Returns the body handle.
    pub fn add_ragdoll_capsule(
        &mut self,
        rot: Quat,
        pos: Vec3,
        a: Vec3,
        b: Vec3,
        radius: f32,
    ) -> RigidBodyHandle {
        self.add_ragdoll_body(rot, pos, SharedShape::capsule(point![a.x, a.y, a.z], point![b.x, b.y, b.z], radius))
    }

    /// As [`Self::add_ragdoll_capsule`] but a ball (for leaf bones — head / hands /
    /// feet — that have no child bone to span a capsule to).
    pub fn add_ragdoll_ball(&mut self, rot: Quat, pos: Vec3, radius: f32) -> RigidBodyHandle {
        self.add_ragdoll_body(rot, pos, SharedShape::ball(radius))
    }

    fn add_ragdoll_body(&mut self, rot: Quat, pos: Vec3, shape: SharedShape) -> RigidBodyHandle {
        let (axis, angle) = rot.to_axis_angle();
        let axisangle = axis * angle;
        let body = RigidBodyBuilder::dynamic()
            .translation(vector![pos.x, pos.y, pos.z])
            .rotation(vector![axisangle.x, axisangle.y, axisangle.z])
            .ccd_enabled(true)
            .linear_damping(0.4)
            .angular_damping(1.5) // heavy angular damping kills the residual limb twitch
            .build();
        let handle = self.bodies.insert(body);
        let collider = ColliderBuilder::new(shape)
            .density(1000.0) // ~flesh density → kg-scale limb masses, so impulses read naturally
            .friction(0.8)
            .restitution(0.0)
            .collision_groups(InteractionGroups::new(
                GROUP_RAGDOLL,
                Group::ALL & !GROUP_RAGDOLL & !GROUP_ENEMY,
            ))
            .build();
        let ch = self.colliders.insert_with_parent(collider, handle, &mut self.bodies);
        self.ragdoll_bodies.insert(handle);
        self.ragdoll_colliders.insert(ch);
        self.dirty = true;
        handle
    }

    /// Join two ragdoll bodies at the shared bone point with a **cone-limited** ball
    /// joint: the three translations are locked and each rotation axis is clamped to
    /// ±[`RAGDOLL_JOINT_CONE`], so limbs can't fold through the body into goo while still
    /// tumbling loosely. `anchor1`/`anchor2` are that point in each body's local frame
    /// (metres). Adjacent bodies don't collide ([`GROUP_RAGDOLL`]), so the joint alone
    /// shapes the chain.
    pub fn add_ragdoll_joint(
        &mut self,
        parent: RigidBodyHandle,
        child: RigidBodyHandle,
        anchor1: Vec3,
        anchor2: Vec3,
    ) {
        let cone = RAGDOLL_JOINT_CONE;
        let joint = GenericJointBuilder::new(JointAxesMask::LOCKED_SPHERICAL_AXES)
            .local_anchor1(point![anchor1.x, anchor1.y, anchor1.z])
            .local_anchor2(point![anchor2.x, anchor2.y, anchor2.z])
            .limits(JointAxis::AngX, [-cone, cone])
            .limits(JointAxis::AngY, [-cone, cone])
            .limits(JointAxis::AngZ, [-cone, cone])
            .build();
        self.impulse_joints.insert(parent, child, joint, true);
    }

    /// Apply a one-shot impulse to a ragdoll body at a world point (the killing shot's
    /// knockback / a blast's radial shove). Wakes the body.
    pub fn apply_ragdoll_impulse(&mut self, handle: RigidBodyHandle, impulse: Vec3, at: Vec3) {
        if let Some(b) = self.bodies.get_mut(handle) {
            b.apply_impulse_at_point(vector![impulse.x, impulse.y, impulse.z], point![at.x, at.y, at.z], true);
        }
    }

    /// The world isometry (rotation, translation in metres) of a ragdoll body — read
    /// back each frame to drive the skinned pose. `None` if the handle is gone.
    pub fn ragdoll_body_iso(&self, handle: RigidBodyHandle) -> Option<(Quat, Vec3)> {
        let b = self.bodies.get(handle)?;
        let t = b.translation();
        let q = b.rotation().coords; // [i, j, k, w] → glam xyzw
        Some((Quat::from_xyzw(q.x, q.y, q.z, q.w), Vec3::new(t.x, t.y, t.z)))
    }

    /// The largest linear speed (m/s) across `handles` — the settle test (a ragdoll
    /// whose fastest body is near-still has come to rest and may start to fade). 0 if
    /// none are live.
    pub fn ragdoll_max_speed(&self, handles: &[RigidBodyHandle]) -> f32 {
        handles
            .iter()
            .filter_map(|h| self.bodies.get(*h))
            .map(|b| b.linvel().norm())
            .fold(0.0, f32::max)
    }

    /// Remove one ragdoll body (its colliders + attached joints go with it). After
    /// the last is removed [`Self::step_dynamics`] goes back to a no-op.
    pub fn remove_ragdoll_body(&mut self, handle: RigidBodyHandle) {
        if !self.ragdoll_bodies.remove(&handle) {
            return;
        }
        if let Some(body) = self.bodies.get(handle) {
            for ch in body.colliders().to_vec() {
                self.ragdoll_colliders.remove(&ch);
            }
        }
        self.bodies.remove(
            handle,
            &mut self.islands,
            &mut self.colliders,
            &mut self.impulse_joints,
            &mut self.multibody_joints,
            true,
        );
        self.dirty = true;
    }

    /// Count of live ragdoll bodies (test/inspection helper).
    pub fn ragdoll_body_count(&self) -> usize {
        self.ragdoll_bodies.len()
    }
}

/// Drop a ball onto a ground plane, step the sim, and return the ball's final
/// height. A correct link makes it fall from y=10 toward the ground (~0.5).
pub fn smoke_test() -> f32 {
    let mut bodies = RigidBodySet::new();
    let mut colliders = ColliderSet::new();

    // Static ground.
    let ground = ColliderBuilder::cuboid(50.0, 0.1, 50.0).build();
    colliders.insert(ground);

    // Dynamic ball starting at y = 10.
    let ball_body = RigidBodyBuilder::dynamic()
        .translation(vector![0.0, 10.0, 0.0])
        .build();
    let ball_handle = bodies.insert(ball_body);
    let ball_collider = ColliderBuilder::ball(0.5).restitution(0.0).build();
    colliders.insert_with_parent(ball_collider, ball_handle, &mut bodies);

    let gravity = vector![0.0, -9.81, 0.0];
    let integration_parameters = IntegrationParameters::default();
    let mut physics_pipeline = PhysicsPipeline::new();
    let mut island_manager = IslandManager::new();
    let mut broad_phase = DefaultBroadPhase::new();
    let mut narrow_phase = NarrowPhase::new();
    let mut impulse_joints = ImpulseJointSet::new();
    let mut multibody_joints = MultibodyJointSet::new();
    let mut ccd_solver = CCDSolver::new();
    let mut query_pipeline = QueryPipeline::new();

    for _ in 0..180 {
        physics_pipeline.step(
            &gravity,
            &integration_parameters,
            &mut island_manager,
            &mut broad_phase,
            &mut narrow_phase,
            &mut bodies,
            &mut colliders,
            &mut impulse_joints,
            &mut multibody_joints,
            &mut ccd_solver,
            Some(&mut query_pipeline),
            &(),
            &(),
        );
    }

    bodies[ball_handle].translation().y
}

/// glam pose → rapier isometry, for the door-panel collider. Rapier takes rotation as
/// a scaled-axis vector, so this matches the axis-angle conversion the ragdoll bodies
/// already use rather than inventing a second convention.
fn door_iso(center: Vec3, rot: Quat) -> Isometry<f32> {
    let (axis, angle) = rot.to_axis_angle();
    let axisangle = axis * angle;
    Isometry::new(
        vector![center.x, center.y, center.z],
        vector![axisangle.x, axisangle.y, axisangle.z],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Track A: the hunter's capsule is hittable, reports its own handle (so
    /// hitscan can tell it from a wall), follows [`PhysicsWorld::update_enemy_collider`]
    /// (the query pipeline sees the per-frame move — the collider-move gotcha),
    /// and vanishes from queries once removed.
    #[test]
    fn enemy_capsule_is_hittable_moves_and_is_removable() {
        let mut p = PhysicsWorld::new();
        let h = p.add_enemy_collider(Vec3::ZERO, 0.3, 0.6);
        assert!(p.is_enemy_collider(h));

        // A ray at the capsule's centre height (feet 0 → centre 0.9) hits it, and
        // the hit reports the enemy's handle.
        let origin = Vec3::new(0.0, 0.9, -3.0);
        let hit = p
            .raycast(origin, Vec3::Z, 100.0)
            .expect("ray should hit the capsule");
        assert_eq!(hit.collider, h, "hit reports the enemy collider");

        // Move it aside: the SAME ray now misses (the query pipeline saw the move).
        p.update_enemy_collider(h, Vec3::new(10.0, 0.0, 0.0));
        assert!(
            p.raycast(origin, Vec3::Z, 100.0).is_none(),
            "the moved capsule is no longer where it was"
        );
        // A ray at the new position hits it.
        let hit2 = p
            .raycast(Vec3::new(10.0, 0.9, -3.0), Vec3::Z, 100.0)
            .expect("ray should hit at the new position");
        assert_eq!(hit2.collider, h);

        // Remove: gone from the query set entirely.
        p.remove_enemy_collider(h);
        assert!(!p.is_enemy_collider(h));
        assert!(
            p.raycast(Vec3::new(10.0, 0.9, -3.0), Vec3::Z, 100.0).is_none(),
            "the removed capsule is unhittable"
        );
    }

    /// Multiple hunters coexist: each capsule is independently hittable and
    /// removable, and removing one leaves the other live.
    #[test]
    fn multiple_enemy_capsules_are_independent() {
        let mut p = PhysicsWorld::new();
        let a = p.add_enemy_collider(Vec3::ZERO, 0.3, 0.6);
        let b = p.add_enemy_collider(Vec3::new(5.0, 0.0, 0.0), 0.3, 0.6);
        assert!(p.is_enemy_collider(a) && p.is_enemy_collider(b));

        let hit_b = p
            .raycast(Vec3::new(5.0, 0.9, -3.0), Vec3::Z, 100.0)
            .expect("ray should hit capsule b");
        assert_eq!(hit_b.collider, b);

        p.remove_enemy_collider(a);
        assert!(!p.is_enemy_collider(a), "a removed");
        assert!(p.is_enemy_collider(b), "b still live");
        // b is still hittable after a's removal.
        assert!(p.raycast(Vec3::new(5.0, 0.9, -3.0), Vec3::Z, 100.0).is_some());

        p.clear_enemy_colliders();
        assert!(!p.is_enemy_collider(b), "cleared");
    }

    /// A destructible-prop collider is hittable by a default-filter ray (so player
    /// hitscan lands on it), reports its own handle, and vanishes from queries once
    /// removed — and `clear` empties the lot.
    #[test]
    fn prop_collider_is_hittable_and_removable() {
        let mut p = PhysicsWorld::new();
        // A unit cube centred at the origin.
        let h = p.add_prop_collider(Vec3::new(-0.5, -0.5, -0.5), Vec3::new(0.5, 0.5, 0.5));
        assert!(p.is_prop_collider(h));

        let hit = p
            .raycast(Vec3::new(0.0, 0.0, -3.0), Vec3::Z, 100.0)
            .expect("ray should hit the prop cube");
        assert_eq!(hit.collider, h, "hit reports the prop collider");

        p.remove_prop_collider(h);
        assert!(!p.is_prop_collider(h));
        assert!(
            p.raycast(Vec3::new(0.0, 0.0, -3.0), Vec3::Z, 100.0).is_none(),
            "the removed prop cube is unhittable"
        );

        // Clear drops every remaining prop collider.
        let a = p.add_prop_collider(Vec3::new(-0.5, -0.5, -0.5), Vec3::new(0.5, 0.5, 0.5));
        let b = p.add_prop_collider(Vec3::new(9.5, -0.5, -0.5), Vec3::new(10.5, 0.5, 0.5));
        assert!(p.is_prop_collider(a) && p.is_prop_collider(b));
        p.clear_prop_colliders();
        assert!(!p.is_prop_collider(a) && !p.is_prop_collider(b), "cleared");
    }

    // ── Dynamics substrate (ragdoll) ──────────────────────────────────────────

    /// With no ragdoll body live, [`PhysicsWorld::step_dynamics`] is a no-op — the
    /// query-only common case pays nothing and never panics on the empty solver.
    #[test]
    fn step_dynamics_is_a_noop_without_ragdolls() {
        let mut p = PhysicsWorld::new();
        for _ in 0..10 {
            p.step_dynamics(1.0 / 60.0);
        }
        assert_eq!(p.ragdoll_body_count(), 0);
    }

    /// A ragdoll body falls under gravity and comes to rest on a static level
    /// collider — proving the dynamics step runs AND that ragdoll bodies collide with
    /// the existing static world for free.
    #[test]
    fn ragdoll_body_falls_and_settles_on_static_ground() {
        let mut p = PhysicsWorld::new();
        // A static ground slab (top at y = 0), reusing the door-panel cuboid path.
        p.add_door_collider(Vec3::new(-50.0, -1.0, -50.0), Vec3::new(50.0, 0.0, 50.0));
        let r = 0.3;
        let h = p.add_ragdoll_ball(Quat::IDENTITY, Vec3::new(0.0, 5.0, 0.0), r);
        assert_eq!(p.ragdoll_body_count(), 1);
        for _ in 0..300 {
            p.step_dynamics(1.0 / 60.0);
        }
        let (_, pos) = p.ragdoll_body_iso(h).expect("body still live");
        assert!(
            (pos.y - r).abs() < 0.12,
            "ball should rest on the ground (~{r}), got y={}",
            pos.y
        );
        assert!(p.ragdoll_max_speed(&[h]) < 0.1, "ball should have settled to rest");
    }

    /// A spherical joint holds two ragdoll bodies at a fixed separation as they fall
    /// together, and removing them empties the solver (the step returns to a no-op).
    #[test]
    fn ragdoll_joint_links_bodies_and_removal_clears_the_sim() {
        let mut p = PhysicsWorld::new();
        let parent = p.add_ragdoll_ball(Quat::IDENTITY, Vec3::new(0.0, 5.0, 0.0), 0.2);
        let child = p.add_ragdoll_ball(Quat::IDENTITY, Vec3::new(0.0, 4.0, 0.0), 0.2);
        // Shared point midway between them, expressed in each body's local frame.
        p.add_ragdoll_joint(parent, child, Vec3::new(0.0, -0.5, 0.0), Vec3::new(0.0, 0.5, 0.0));
        assert_eq!(p.ragdoll_body_count(), 2);
        for _ in 0..120 {
            p.step_dynamics(1.0 / 60.0);
        }
        let (_, pp) = p.ragdoll_body_iso(parent).unwrap();
        let (_, cp) = p.ragdoll_body_iso(child).unwrap();
        let sep = pp.distance(cp);
        assert!(
            (sep - 1.0).abs() < 0.25,
            "the ball joint should hold the bodies ~1 m apart, got {sep}"
        );
        p.remove_ragdoll_body(parent);
        p.remove_ragdoll_body(child);
        assert_eq!(p.ragdoll_body_count(), 0);
        // Empty again → stepping is a no-op.
        p.step_dynamics(1.0 / 60.0);
    }
}
