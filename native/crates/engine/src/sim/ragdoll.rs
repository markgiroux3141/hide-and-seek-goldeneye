//! **Death ragdoll** — a chain of dynamic capsule/ball bodies driven by
//! [`PhysicsWorld`]'s rigid-body solver, seeded from a character's animated death
//! pose + the killing shot's impulse, and read back into skinning matrices every
//! frame. Replaces the canned death clip: the corpse now tumbles down real stairs,
//! drapes over ledges, and reacts to where it was hit.
//!
//! # How it maps to the rig
//! Every bone gets one dynamic body placed at that bone's world transform at the
//! instant of death. A bone with a child gets a **capsule** spanning to its (farthest)
//! child; a leaf bone (head / hands / feet) gets a **ball**. Each non-root body is
//! pinned to its parent body by a **spherical joint** at the shared bone origin —
//! translations locked, rotations free — so the skeleton holds together while the
//! limbs swing. Ragdoll bodies don't collide with one another (see `GROUP_RAGDOLL`),
//! only the static level, so a corpse's own limbs never fight.
//!
//! # The scale bookkeeping (why read-back is exact)
//! The renderer draws a bone's vertices as `char_transform · skin(b) · v`, where
//! `skin(b) = model_global(b) · inverse_bind(b)` and `char_transform` folds in the
//! `CHAR_SCALE` GLB→metre scale. A bone's WORLD transform is therefore
//! `W(b) = char_transform · model_global(b) = Iso(b) · scale`, an isometry times a
//! uniform scale. A rigid body can only hold the isometry, so we seed each body with
//! `Iso(b)` (the rotation+translation of `W_seed(b)`) and, at read-back, rebuild
//! `W_now(b) = from_rotation_translation(body_rot, body_trans) · scale` and emit
//! `skin(b) = W_now(b) · inverse_bind(b)` with the model transform set to identity.
//! The seed math cancels exactly, so frame 0 of the ragdoll matches the last animated
//! frame before the body starts to move.

use glam::{Mat4, Quat, Vec3};
use rapier3d::prelude::RigidBodyHandle;

use crate::sim::physics::PhysicsWorld;
use crate::skeletal::layers::Pose;
use crate::skeletal::Skeleton;

/// Capsule radius as a fraction of the bone's segment length.
const LIMB_RADIUS_FRAC: f32 = 0.30;
/// Clamp for a derived limb capsule radius (metres).
const MIN_RADIUS: f32 = 0.03;
const MAX_RADIUS: f32 = 0.11;
/// Ball radius for a leaf bone with no child to span a capsule to (metres).
const LEAF_RADIUS: f32 = 0.075;
/// Below this segment length a "capsule" is degenerate → use a ball instead.
const MIN_SEGMENT: f32 = 0.02;

/// A live death ragdoll: one rigid body per bone, the scale to fold back in, and the
/// handle list for the settle check + teardown.
pub struct Ragdoll {
    /// Per bone (joint-indexed): its rigid body, if one was created. Every bone gets
    /// one in the default build; `None` is tolerated (that bone stays at identity).
    bone_body: Vec<Option<RigidBodyHandle>>,
    /// The uniform GLB→metre scale (`CHAR_SCALE`), folded into each bone's world
    /// transform on read-back.
    scale: f32,
    /// All body handles, for [`Self::max_speed`] + [`Self::remove`].
    handles: Vec<RigidBodyHandle>,
}

impl Ragdoll {
    /// Build a ragdoll for `skeleton` from each bone's WORLD transform at death,
    /// `world_bone[b] = char_transform · model_global_death(b)` (metres, uniform
    /// `scale` embedded). Creates a body per bone, joins each to its parent, and
    /// applies `impulse` (world N·s) at `impulse_point` (world m) to whichever body is
    /// nearest that point — the killing shot's knockback. Returns the live ragdoll.
    pub fn build(
        physics: &mut PhysicsWorld,
        skeleton: &Skeleton,
        world_bone: &[Mat4],
        scale: f32,
        impulse: Vec3,
        impulse_point: Vec3,
    ) -> Ragdoll {
        let n = skeleton.joint_count();
        // Per-bone isometry (rotation + world origin) of the seed transform.
        let iso: Vec<(Quat, Vec3)> = (0..n)
            .map(|b| {
                let (_, r, t) = world_bone[b].to_scale_rotation_translation();
                (r, t)
            })
            .collect();
        // Children of each bone (joint-indexed).
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
        for b in 0..n {
            if let Some(p) = skeleton.parents[b] {
                children[p].push(b);
            }
        }

        let mut bone_body: Vec<Option<RigidBodyHandle>> = vec![None; n];
        let mut handles: Vec<RigidBodyHandle> = Vec::with_capacity(n);

        // One body per bone: capsule to the farthest child, else a ball.
        for b in 0..n {
            let (rot_b, org_b) = iso[b];
            // Farthest child origin (the main limb direction), if any.
            let tip = children[b]
                .iter()
                .map(|&c| (c, iso[c].1.distance(org_b)))
                .max_by(|a, z| a.1.partial_cmp(&z.1).unwrap_or(std::cmp::Ordering::Equal));
            let handle = match tip {
                Some((c, len)) if len >= MIN_SEGMENT => {
                    // Capsule from this bone's origin (local 0) to the child origin,
                    // expressed in the body's own (unscaled) local frame.
                    let b_local = rot_b.inverse() * (iso[c].1 - org_b);
                    let radius = (len * LIMB_RADIUS_FRAC).clamp(MIN_RADIUS, MAX_RADIUS);
                    physics.add_ragdoll_capsule(rot_b, org_b, Vec3::ZERO, b_local, radius)
                }
                _ => physics.add_ragdoll_ball(rot_b, org_b, LEAF_RADIUS),
            };
            bone_body[b] = Some(handle);
            handles.push(handle);
        }

        // Spherical joint each non-root bone to its parent at the shared bone origin.
        for b in 0..n {
            let (Some(p), Some(cb)) = (skeleton.parents[b], bone_body[b]) else {
                continue;
            };
            let Some(pb) = bone_body[p] else { continue };
            let (rot_p, org_p) = iso[p];
            // Shared point = bone b's origin. On the child body that's its own origin
            // (local 0); on the parent it's org_b in the parent's local frame.
            let anchor_parent = rot_p.inverse() * (iso[b].1 - org_p);
            physics.add_ragdoll_joint(pb, cb, anchor_parent, Vec3::ZERO);
        }

        let rag = Ragdoll { bone_body, scale, handles };
        // Knockback: shove the body nearest the impact point (the killing / hit shove).
        rag.kick(physics, impulse, impulse_point);
        rag
    }

    /// Apply an `impulse` (world N·s) to the body nearest world point `at` — the hit
    /// knockback. Used at build time and again on a re-hit while a living reaction is
    /// still staggering. No-op for a negligible impulse.
    pub fn kick(&self, physics: &mut PhysicsWorld, impulse: Vec3, at: Vec3) {
        if impulse.length_squared() <= 1e-6 {
            return;
        }
        let nearest = self
            .handles
            .iter()
            .copied()
            .filter_map(|h| physics.ragdoll_body_iso(h).map(|(_, t)| (h, t.distance(at))))
            .min_by(|a, z| a.1.partial_cmp(&z.1).unwrap_or(std::cmp::Ordering::Equal));
        if let Some((h, _)) = nearest {
            physics.apply_ragdoll_impulse(h, impulse, at);
        }
    }

    /// The skinning matrices for the current ragdoll state — one per bone, in WORLD
    /// space (the caller draws these with an **identity** model transform).
    /// `skin(b) = W_now(b) · inverse_bind(b)`, `W_now(b) = iso(b) · scale`.
    pub fn skinning_matrices(&self, physics: &PhysicsWorld, skeleton: &Skeleton) -> Vec<Mat4> {
        let scale_m = Mat4::from_scale(Vec3::splat(self.scale));
        (0..skeleton.joint_count())
            .map(|b| match self.bone_body[b] {
                Some(h) => match physics.ragdoll_body_iso(h) {
                    Some((rot, trans)) => {
                        Mat4::from_rotation_translation(rot, trans) * scale_m * skeleton.inverse_bind[b]
                    }
                    None => Mat4::IDENTITY,
                },
                None => Mat4::IDENTITY,
            })
            .collect()
    }

    /// The ragdoll's current state as a **model-local** [`Pose`] (per-joint local
    /// T/R/S), for the living-hit reaction that BLENDS the physical reaction back into
    /// the running animation. `char_inv` is the inverse of the character's world
    /// transform this frame (`char_transform⁻¹`); each bone's model-space global is
    /// `char_inv · W_now(b)`, and locals are recovered against the parent chain. At the
    /// seed instant (before the sim steps) this reproduces the pose the ragdoll was
    /// built from — so blending it in at weight → 0 returns exactly to animation.
    pub fn model_local_pose(&self, physics: &PhysicsWorld, skeleton: &Skeleton, char_inv: Mat4) -> Pose {
        let n = skeleton.joint_count();
        let scale_m = Mat4::from_scale(Vec3::splat(self.scale));
        // Model-space globals from the bodies.
        let model_global: Vec<Mat4> = (0..n)
            .map(|b| match self.bone_body[b].and_then(|h| physics.ragdoll_body_iso(h)) {
                Some((rot, trans)) => char_inv * Mat4::from_rotation_translation(rot, trans) * scale_m,
                None => Mat4::IDENTITY,
            })
            .collect();
        // Globals → locals against the parent chain, then decompose to T/R/S.
        let (mut t, mut r, mut s) = (
            Vec::with_capacity(n),
            Vec::with_capacity(n),
            Vec::with_capacity(n),
        );
        for b in 0..n {
            let local = match skeleton.parents[b] {
                Some(p) => model_global[p].inverse() * model_global[b],
                None => model_global[b],
            };
            let (bs, br, bt) = local.to_scale_rotation_translation();
            t.push(bt);
            r.push(br);
            s.push(bs);
        }
        Pose::from_trs(t, r, s)
    }

    /// The fastest body's linear speed (m/s) — the settle test.
    pub fn max_speed(&self, physics: &PhysicsWorld) -> f32 {
        physics.ragdoll_max_speed(&self.handles)
    }

    /// Tear the ragdoll down (remove every body + its colliders/joints from the sim).
    pub fn remove(self, physics: &mut PhysicsWorld) {
        for h in self.handles {
            physics.remove_ragdoll_body(h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skeletal::gltf_skin;

    fn karl() -> gltf_skin::SkinnedModel {
        let p = format!(
            "{}/../../assets/enemies/characters/russian-guard_karl.glb",
            env!("CARGO_MANIFEST_DIR")
        );
        gltf_skin::load(&p).expect("load karl")
    }

    /// Build a ragdoll from a standing pose, drop it on a floor, and verify: one body
    /// per bone, the skinning matrices are finite from frame 0, the corpse falls +
    /// settles without tunnelling the floor, and teardown empties the sim.
    #[test]
    fn ragdoll_seeds_from_pose_falls_and_settles() {
        let m = karl();
        let sk = &m.skeleton;
        let mut physics = PhysicsWorld::new();
        physics.add_door_collider(Vec3::new(-50.0, -1.0, -50.0), Vec3::new(50.0, 0.0, 50.0));

        // Stand the (bind) skeleton in the world at a test scale, feet a bit up.
        let scale = 0.01;
        let char_mat = Mat4::from_translation(Vec3::new(0.0, 1.2, 0.0)) * Mat4::from_scale(Vec3::splat(scale));
        let model_globals = Pose::bind(sk).joint_global_transforms(sk);
        let world_bone: Vec<Mat4> = model_globals.iter().map(|g| char_mat * *g).collect();

        let rag = Ragdoll::build(
            &mut physics,
            sk,
            &world_bone,
            scale,
            Vec3::new(3.0, 0.5, 0.0), // a sideways+up knockback
            world_bone[0].to_scale_rotation_translation().2,
        );
        assert_eq!(physics.ragdoll_body_count(), sk.joint_count(), "one body per bone");

        let finite = |mats: &[Mat4]| mats.iter().all(|mm| mm.to_cols_array().iter().all(|v| v.is_finite()));
        assert!(finite(&rag.skinning_matrices(&physics, sk)), "seed skinning is finite");

        for _ in 0..360 {
            physics.step_dynamics(1.0 / 60.0);
        }
        let mats = rag.skinning_matrices(&physics, sk);
        assert!(finite(&mats), "settled skinning is finite");
        assert!(rag.max_speed(&physics) < 0.6, "the corpse should have settled to near-rest");

        rag.remove(&mut physics);
        assert_eq!(physics.ragdoll_body_count(), 0, "teardown empties the sim");
    }

    /// The living-reaction oracle: at the seed instant (before the sim steps), the
    /// model-local pose read back from the bodies reproduces the exact pose the ragdoll
    /// was built from — so blending it into animation at weight → 0 returns cleanly.
    /// Checked under a translated + ROTATED + scaled character transform (the general
    /// case the space-conversion must handle).
    #[test]
    fn model_local_pose_round_trips_at_seed() {
        let m = karl();
        let sk = &m.skeleton;
        let mut physics = PhysicsWorld::new();
        let scale = 0.01;
        let char_mat = Mat4::from_translation(Vec3::new(1.0, 1.2, -0.5))
            * Mat4::from_rotation_y(0.7)
            * Mat4::from_scale(Vec3::splat(scale));
        let model_globals = Pose::bind(sk).joint_global_transforms(sk);
        let world_bone: Vec<Mat4> = model_globals.iter().map(|g| char_mat * *g).collect();

        // No impulse → bodies stay at the seed isometry.
        let rag = Ragdoll::build(&mut physics, sk, &world_bone, scale, Vec3::ZERO, Vec3::ZERO);
        let pose = rag.model_local_pose(&physics, sk, char_mat.inverse());
        let recovered = pose.joint_global_transforms(sk);
        for b in 0..sk.joint_count() {
            let got = recovered[b].to_scale_rotation_translation().2;
            let want = model_globals[b].to_scale_rotation_translation().2;
            assert!(
                (got - want).length() < 2e-3,
                "bone {b} origin off by {} (got {got:?}, want {want:?})",
                (got - want).length()
            );
        }
        rag.remove(&mut physics);
    }
}
