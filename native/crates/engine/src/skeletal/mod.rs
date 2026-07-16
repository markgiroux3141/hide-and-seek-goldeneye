//! Skinned-character animation subsystem — the rendering foundation for the
//! GoldenEye enemy roster.
//!
//! GoldenEye characters are skinned glTF on a **shared 15-bone skeleton**
//! (`Bone_1..Bone_15` under `Armature`, identical node order across all 45
//! characters). Meshes carry `POSITION / TEXCOORD_0 / JOINTS_0 / WEIGHTS_0` and
//! **no `NORMAL`** → they render **unlit + textured** (N64 look), never PBR.
//!
//! B1 (this file + [`gltf_skin`]) loads one character and its skeleton and draws
//! it in **bind pose**: with the joints held at their bind transforms, every
//! joint matrix `global · inverse_bind` reduces to the identity, so a *correct*
//! skinning pipeline reproduces the undistorted bind pose. That makes the bind
//! pose the first, cheap correctness oracle for the whole skinning path before
//! any animation drives the joints (B2+).

pub mod anim;
pub mod anim_set;
pub mod clip;
pub mod gltf_skin;

use glam::{Mat4, Quat, Vec3};

/// A shared character skeleton: joint hierarchy + bind + inverse-bind data.
///
/// Indices throughout are **joint indices** (positions in the skin's `joints`
/// list, i.e. the order the vertex `JOINTS_0` attribute refers to), *not* glTF
/// node indices. `parents[i]` is the joint index of `i`'s parent, or `None` when
/// `i`'s parent is not itself a joint (e.g. the `Armature` root).
pub struct Skeleton {
    /// Joint node names (`Bone_1`..), indexed by joint index.
    pub names: Vec<String>,
    /// Parent joint index for each joint, or `None` if the parent isn't a joint.
    pub parents: Vec<Option<usize>>,
    /// Each joint's **local** bind transform (its glTF node TRS).
    pub local_bind: Vec<Mat4>,
    /// Bind-pose local translation / rotation / scale (the decomposed TRS of
    /// `local_bind`). Kept so an animation channel that overrides only *one*
    /// property (e.g. rotation-only locomotion clips) can recompose against the
    /// other two bind components instead of losing them.
    pub bind_t: Vec<Vec3>,
    pub bind_r: Vec<Quat>,
    pub bind_s: Vec<Vec3>,
    /// Each joint's inverse bind matrix (from the skin's `inverseBindMatrices`).
    pub inverse_bind: Vec<Mat4>,
}

impl Skeleton {
    pub fn joint_count(&self) -> usize {
        self.names.len()
    }

    /// Joint index for a bone name (e.g. `"Bone_9"`), if present.
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name)
    }

    /// Global (model-space) transform of every joint, given each joint's local
    /// transform. `locals` must be joint-indexed and the same length as the
    /// skeleton. Parents are resolved by walking up `parents`, so the joint list
    /// need not be in hierarchy order.
    pub fn global_transforms(&self, locals: &[Mat4]) -> Vec<Mat4> {
        let n = self.joint_count();
        let mut global = vec![None; n];
        // Memoized walk up the parent chain (only 15 joints — cheap; robust to
        // any joint ordering).
        fn resolve(
            i: usize,
            parents: &[Option<usize>],
            locals: &[Mat4],
            global: &mut Vec<Option<Mat4>>,
        ) -> Mat4 {
            if let Some(g) = global[i] {
                return g;
            }
            let g = match parents[i] {
                Some(p) => resolve(p, parents, locals, global) * locals[i],
                None => locals[i],
            };
            global[i] = Some(g);
            g
        }
        (0..n)
            .map(|i| resolve(i, &self.parents, locals, &mut global))
            .collect()
    }

    /// The skinning (joint) matrices for a pose: `global(i) · inverse_bind(i)`.
    /// These are what the vertex shader multiplies weighted per vertex (LBS).
    /// At bind pose (`locals == local_bind`) each result is the identity.
    pub fn skinning_matrices(&self, locals: &[Mat4]) -> Vec<Mat4> {
        let global = self.global_transforms(locals);
        (0..self.joint_count())
            .map(|i| global[i] * self.inverse_bind[i])
            .collect()
    }

    /// Convenience: the skinning matrices for the **bind pose** (all identity).
    pub fn bind_pose_matrices(&self) -> Vec<Mat4> {
        self.skinning_matrices(&self.local_bind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Mat4, Quat, Vec3};

    /// A 2-joint skeleton: root at origin, child offset +1 on Y. Verifies the
    /// parent-chain accumulation and that `global · inverse_bind` is identity at
    /// bind pose (the B1 correctness oracle), then that a child rotation composes
    /// through the parent.
    fn two_joint() -> Skeleton {
        let root_local = Mat4::IDENTITY;
        let child_local = Mat4::from_translation(Vec3::new(0.0, 1.0, 0.0));
        // Bind globals: root = I, child = translate(0,1,0). Inverse binds are the
        // inverses of the bind globals.
        let root_global = root_local;
        let child_global = root_global * child_local;
        Skeleton {
            names: vec!["root".into(), "child".into()],
            parents: vec![None, Some(0)],
            local_bind: vec![root_local, child_local],
            bind_t: vec![Vec3::ZERO, Vec3::new(0.0, 1.0, 0.0)],
            bind_r: vec![Quat::IDENTITY, Quat::IDENTITY],
            bind_s: vec![Vec3::ONE, Vec3::ONE],
            inverse_bind: vec![root_global.inverse(), child_global.inverse()],
        }
    }

    #[test]
    fn bind_pose_is_identity() {
        let sk = two_joint();
        for m in sk.bind_pose_matrices() {
            let d = (m - Mat4::IDENTITY).to_cols_array();
            assert!(d.iter().all(|v| v.abs() < 1e-5), "bind matrix not identity: {m:?}");
        }
    }

    #[test]
    fn child_rotation_composes_through_parent() {
        let sk = two_joint();
        // Rotate the root 90° about Z; child keeps its bind local.
        let rot = Mat4::from_quat(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2));
        let locals = vec![rot, sk.local_bind[1]];
        let mats = sk.skinning_matrices(&locals);
        // A point at the child's bind position (0,1,0) → skin by the child matrix.
        // Child global = rot * translate(0,1,0); times inverse_bind(child) which
        // maps bind→local. The net effect on the bind-position point is the root
        // rotation: (0,1,0) rotated 90° about Z → (-1,0,0).
        let p = mats[1].transform_point3(Vec3::new(0.0, 1.0, 0.0));
        assert!((p - Vec3::new(-1.0, 0.0, 0.0)).length() < 1e-5, "got {p:?}");
    }
}

