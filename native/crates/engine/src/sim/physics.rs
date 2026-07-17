//! Rapier3D wrapper. Phase 1 scope: the CSG → collision pipeline — per-region
//! static trimesh colliders that are rebuilt in place on every brush edit, plus
//! a ray query for crosshair face-picking. The kinematic character controller
//! lands in Phase 2 on top of this same world.
//!
//! Also retains the Phase 0 [`smoke_test`] as a link/step sanity check.

use std::collections::{HashMap, HashSet};

use glam::Vec3;
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
}

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

        PhysicsWorld {
            colliders: ColliderSet::new(),
            bodies: RigidBodySet::new(),
            query_pipeline: QueryPipeline::new(),
            region_colliders: HashMap::new(),
            door_colliders: Vec::new(),
            enemy_colliders: HashSet::new(),
            enemy_capsule_offset: 0.0,
            dirty: true,
            character,
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
}
