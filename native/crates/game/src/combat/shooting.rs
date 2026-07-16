//! Hitscan — a single rapier ray from the camera centre. Transliterated from
//! `src/weapons/ShootingSystem.ts` (`fire()` → `castRayAndGetNormal`).
//!
//! **The ray comes from the camera centre (crosshair), not the gun muzzle** — the
//! viewmodel only visually tracks (JS comment: "ray fires from camera center,
//! gun just visually tracks"). The player's own collider is excluded (moot today
//! — the native player has no registered collider — but threaded for Track A).

use glam::Vec3;
use rapier3d::prelude::ColliderHandle;

use engine::sim::physics::PhysicsWorld;

/// A resolved hitscan hit: world-space impact point, surface normal, and the
/// distance from the ray origin (JS `HitResult` minus the collider handle, which
/// Track A will re-add for entity lookup).
#[derive(Clone, Copy, Debug)]
pub struct HitResult {
    pub point: Vec3,
    pub normal: Vec3,
    pub distance: f32,
}

/// Cast a shot ray from `origin` along `dir` (need not be normalized) up to
/// `range` metres, excluding `exclude` (the player collider, if any). Returns the
/// first hit, or `None` if the shot hit nothing within range.
pub fn cast(
    physics: &mut PhysicsWorld,
    origin: Vec3,
    dir: Vec3,
    range: f32,
    exclude: Option<ColliderHandle>,
) -> Option<HitResult> {
    let hit = physics.raycast_excluding(origin, dir, range, exclude)?;
    Some(HitResult {
        point: hit.point,
        normal: hit.normal,
        distance: (hit.point - origin).length(),
    })
}
