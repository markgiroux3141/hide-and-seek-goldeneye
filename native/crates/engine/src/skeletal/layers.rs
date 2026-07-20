//! Procedural **pose layer stack** — the composable seam for driving characters
//! by *functions* instead of only by canned clips.
//!
//! # Why this exists
//! The clip mixer ([`crate::skeletal::anim::AnimPlayer`]) produces one blended
//! **base pose** each frame (walk/jog/run + fire/hit/death one-shots). That's the
//! authored *style*. On top of it we want continuous, parameterised behaviour: an
//! arm that reaches toward an arbitrary target (aim), a weapon that recoils along
//! a decay curve, a spine that leans, a head that tracks — none of which any
//! single clip can express, because they depend on *runtime* values (where the
//! player is, when the trigger was pulled).
//!
//! # The abstraction
//! Everything is expressed as an operation on a [`Pose`] — the per-joint local
//! `T/R/S` arrays the clip mixer already speaks. A [`PoseLayer`] takes the pose
//! accumulated so far and mutates it; [`LayeredAnimator`] folds a stack of them:
//!
//! ```text
//!   base pose (AnimPlayer) ─▶ [layer] ─▶ [layer] ─▶ … ─▶ final pose ─▶ matrices
//! ```
//!
//! This one contract covers every layer *kind*:
//! - **base**    — ignore the input, write the whole pose (a clip / blend space);
//! - **override** — replace some joints (an upper-body clip over a walking base);
//! - **additive** — compose a delta onto some joints ([`AdditiveDecayLayer`]);
//! - **IK**      — resolve the pose to *global* space, solve, write locals back
//!   ([`TwoBoneIkLayer`]) — the case that most constrains the trait, since it
//!   needs the whole hierarchy, not just one joint in isolation.
//!
//! Because the final [`Pose`] flows back through the same [`Skeleton`] matrix
//! path, a weapon parented to a hand bone follows the IK'd hand automatically.

use std::any::Any;

use glam::{Mat4, Quat, Vec3};

use crate::skeletal::Skeleton;

/// A whole-skeleton pose as per-joint **local** `T/R/S` (joint-indexed, same
/// length as the skeleton). This is the currency every layer reads and writes;
/// it matches what [`crate::skeletal::clip::AnimationClip::pose_trs`] emits, so
/// the clip mixer's output drops straight in as the base.
#[derive(Clone, Debug)]
pub struct Pose {
    pub t: Vec<Vec3>,
    pub r: Vec<Quat>,
    pub s: Vec<Vec3>,
}

impl Pose {
    /// A pose from raw TRS arrays (e.g. the mixer's blended output).
    pub fn from_trs(t: Vec<Vec3>, r: Vec<Quat>, s: Vec<Vec3>) -> Self {
        Pose { t, r, s }
    }

    /// The skeleton's bind pose — the identity starting point for a stack whose
    /// first layer is additive rather than a full base write.
    pub fn bind(skeleton: &Skeleton) -> Self {
        Pose {
            t: skeleton.bind_t.clone(),
            r: skeleton.bind_r.clone(),
            s: skeleton.bind_s.clone(),
        }
    }

    pub fn joint_count(&self) -> usize {
        self.r.len()
    }

    /// Per-joint local matrices (`T·R·S`) — the input to the skeleton hierarchy.
    pub fn locals(&self) -> Vec<Mat4> {
        (0..self.joint_count())
            .map(|i| Mat4::from_scale_rotation_translation(self.s[i], self.r[i], self.t[i]))
            .collect()
    }

    /// Skinning matrices (`global · inverse_bind`) for this pose — what the
    /// renderer uploads. Mirrors [`AnimPlayer::skinning_matrices`].
    ///
    /// [`AnimPlayer::skinning_matrices`]: crate::skeletal::anim::AnimPlayer::skinning_matrices
    pub fn skinning_matrices(&self, skeleton: &Skeleton) -> Vec<Mat4> {
        skeleton.skinning_matrices(&self.locals())
    }

    /// Global (model-space) joint transforms — for parenting a prop/weapon to a
    /// bone. Mirrors [`AnimPlayer::joint_global_transforms`], so a weapon on the
    /// hand bone tracks whatever an IK layer did to the arm.
    ///
    /// [`AnimPlayer::joint_global_transforms`]: crate::skeletal::anim::AnimPlayer::joint_global_transforms
    pub fn joint_global_transforms(&self, skeleton: &Skeleton) -> Vec<Mat4> {
        skeleton.global_transforms(&self.locals())
    }
}

/// Per-evaluation context handed to every layer: the skeleton (for resolving
/// globals) and the frame delta (for time-evolving layers like recoil decay).
pub struct LayerCtx<'a> {
    pub skeleton: &'a Skeleton,
    pub dt: f32,
}

/// One stage in the pose stack. Mutates the accumulating [`Pose`] in place.
///
/// `&mut self` so a layer can carry state that evolves over time (a decaying
/// recoil amplitude, a smoothed aim target). Reading global transforms is done
/// by resolving `pose` against `ctx.skeleton` inside the layer (see
/// [`TwoBoneIkLayer`]).
pub trait PoseLayer: Any + Send {
    fn apply(&mut self, pose: &mut Pose, ctx: &LayerCtx);

    /// Short label for debugging / a live layer inspector.
    fn name(&self) -> &str {
        "layer"
    }

    /// Upcast so callers can [`downcast_mut`](std::any::Any::downcast_mut) a
    /// stacked layer back to its concrete type to set runtime params (aim
    /// target, recoil kick). One trivial line per impl.
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Owns an ordered stack of [`PoseLayer`]s and evaluates them over a base pose.
///
/// The base pose comes from the caller (today: the clip mixer's blend). A future
/// parametric locomotion **blend space** is just a base layer that ignores the
/// incoming pose and writes a speed-blended clip pose — no change to this type.
#[derive(Default)]
pub struct LayeredAnimator {
    layers: Vec<Box<dyn PoseLayer>>,
}

impl LayeredAnimator {
    pub fn new() -> Self {
        LayeredAnimator { layers: Vec::new() }
    }

    /// Append a layer; later layers see the effect of earlier ones.
    pub fn push(&mut self, layer: Box<dyn PoseLayer>) -> &mut Self {
        self.layers.push(layer);
        self
    }

    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    /// Borrow the layer at stack index `i` as its concrete type `T` to set
    /// runtime parameters (aim target, recoil kick) between frames. `None` if
    /// the index is out of range or the layer isn't a `T`.
    pub fn layer_as<T: PoseLayer>(&mut self, i: usize) -> Option<&mut T> {
        self.layers.get_mut(i)?.as_any_mut().downcast_mut::<T>()
    }

    /// Fold every layer over `base` and return the final pose.
    pub fn evaluate(&mut self, base: Pose, ctx: &LayerCtx) -> Pose {
        let mut pose = base;
        for layer in &mut self.layers {
            layer.apply(&mut pose, ctx);
        }
        pose
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Two-bone IK layer — the case that defines the trait.
// ─────────────────────────────────────────────────────────────────────────────

/// Analytic two-bone IK over a `root → mid → end` chain (shoulder → elbow →
/// hand), reaching the `end` joint toward a model-space `target`.
///
/// Closed-form (law of cosines), not iterative: bend the elbow to the target
/// *distance*, then swing the root so the end points at the target. Only the
/// root and mid **rotations** are written; segment lengths are preserved, so a
/// reachable target is hit exactly (the headless correctness oracle). Blended
/// with the incoming pose by `weight`, so aim can fade in/out.
pub struct TwoBoneIkLayer {
    pub root: usize,
    pub mid: usize,
    pub end: usize,
    /// Model-space point the `end` joint should reach.
    pub target: Vec3,
    /// Model-space hint for which way the elbow bends when the chain is straight
    /// (only consulted when the current bend plane is degenerate).
    pub pole: Vec3,
    /// 0 → leave the pose untouched, 1 → full IK. Slerped per affected joint.
    pub weight: f32,
    pub enabled: bool,
}

impl TwoBoneIkLayer {
    /// Derive the `root → mid → end` chain by walking two parents up from a hand
    /// bone (e.g. `"Bone_9"`), so we never hardcode joint indices.
    pub fn from_end_bone(skeleton: &Skeleton, end_bone: &str) -> Option<Self> {
        let end = skeleton.index_of(end_bone)?;
        let mid = skeleton.parents[end]?;
        let root = skeleton.parents[mid]?;
        Some(TwoBoneIkLayer {
            root,
            mid,
            end,
            target: Vec3::ZERO,
            pole: Vec3::ZERO,
            weight: 0.0,
            enabled: false,
        })
    }
}

impl PoseLayer for TwoBoneIkLayer {
    fn apply(&mut self, pose: &mut Pose, ctx: &LayerCtx) {
        if !self.enabled || self.weight <= 1e-4 {
            return;
        }
        let sk = ctx.skeleton;
        let globals = sk.global_transforms(&pose.locals());

        // Joint origins (model space) and current global rotations.
        let (_, gr_a, a) = globals[self.root].to_scale_rotation_translation();
        let (_, gr_b, b) = globals[self.mid].to_scale_rotation_translation();
        let c = globals[self.end].to_scale_rotation_translation().2;
        // Parent-of-root global rotation (for converting the root's new global
        // rotation back to a local one). Identity if the root has no joint parent.
        let p_rot = match sk.parents[self.root] {
            Some(p) => globals[p].to_scale_rotation_translation().1,
            None => Quat::IDENTITY,
        };

        let l_ab = (b - a).length();
        let l_bc = (c - b).length();
        if l_ab < 1e-5 || l_bc < 1e-5 {
            return; // degenerate chain
        }
        // Clamp the reach so the target is always achievable (arm never over-
        // extends past straight or folds through itself).
        let reach = (self.target - a).length().clamp(1e-3, l_ab + l_bc - 1e-3);
        let dir_at = (self.target - a).normalize_or_zero();
        if dir_at == Vec3::ZERO {
            return;
        }

        // Current interior angles.
        let ac = c - a;
        let angle_b_cur = clamp_acos((a - b).normalize_or_zero().dot((c - b).normalize_or_zero()));
        // Desired interior angles from the law of cosines at the clamped reach.
        let angle_b_des = clamp_acos((l_ab * l_ab + l_bc * l_bc - reach * reach) / (2.0 * l_ab * l_bc));

        // Bend plane normal: prefer the current (a,b,c) plane so the arm keeps its
        // natural bend; fall back to the pole hint when the chain is straight.
        let mut axis = ac.cross(b - a);
        if axis.length_squared() < 1e-8 {
            axis = ac.cross(self.pole - a);
        }
        if axis.length_squared() < 1e-8 {
            axis = ac.cross(Vec3::Y); // last-ditch perpendicular
        }
        let axis = axis.normalize_or_zero();
        if axis == Vec3::ZERO {
            return;
        }

        // 1. Elbow bend: rotate about the plane normal by the interior-angle delta.
        let q_b = Quat::from_axis_angle(axis, angle_b_des - angle_b_cur);
        // End position after the elbow bends (root not yet moved).
        let c1 = b + q_b * (c - b);

        // 2. Root swing: rotate so the (post-bend) end direction aligns to target.
        let to_c = (c1 - a).normalize_or_zero();
        let swing_axis = to_c.cross(dir_at);
        let q_a = if swing_axis.length_squared() > 1e-8 {
            Quat::from_axis_angle(swing_axis.normalize(), clamp_acos(to_c.dot(dir_at)))
        } else {
            Quat::IDENTITY
        };

        // World deltas → new local rotations (see module math notes):
        //   root local  = inv(parent_global) · (q_a · gr_a)
        //   mid  local  = inv(gr_a)          · (q_b · gr_b)
        let new_root_local = p_rot.inverse() * (q_a * gr_a);
        let new_mid_local = gr_a.inverse() * (q_b * gr_b);

        let w = self.weight.clamp(0.0, 1.0);
        pose.r[self.root] = pose.r[self.root].slerp(new_root_local, w);
        pose.r[self.mid] = pose.r[self.mid].slerp(new_mid_local, w);
    }

    fn name(&self) -> &str {
        "two-bone-ik"
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Additive decay layer — recoil (stateful additive).
// ─────────────────────────────────────────────────────────────────────────────

/// An additive rotational impulse on one joint that decays exponentially — the
/// weapon "recoil" kick. [`kick`](Self::kick) adds amplitude (on the trigger
/// pull); [`apply`](PoseLayer::apply) advances the decay by `dt` and composes the
/// current angle onto the joint's local rotation, on top of whatever aim/clip
/// left there.
pub struct AdditiveDecayLayer {
    pub joint: usize,
    /// Local rotation axis of the kick (e.g. pitch the wrist up).
    pub axis: Vec3,
    /// Current kick angle (radians); evolves toward 0.
    pub amplitude: f32,
    /// Decay rate `1/tau` (larger = snappier settle).
    pub decay: f32,
    /// Amplitude ceiling so rapid fire can't wind up unboundedly.
    pub max_amplitude: f32,
}

impl AdditiveDecayLayer {
    pub fn new(joint: usize, axis: Vec3, decay: f32, max_amplitude: f32) -> Self {
        AdditiveDecayLayer {
            joint,
            axis: axis.normalize_or_zero(),
            amplitude: 0.0,
            decay,
            max_amplitude,
        }
    }

    /// Add a recoil impulse (radians), clamped to `max_amplitude`.
    pub fn kick(&mut self, amount: f32) {
        self.amplitude = (self.amplitude + amount).min(self.max_amplitude);
    }
}

impl PoseLayer for AdditiveDecayLayer {
    fn apply(&mut self, pose: &mut Pose, ctx: &LayerCtx) {
        // Exponential decay toward rest.
        self.amplitude *= (-self.decay * ctx.dt).exp();
        if self.amplitude.abs() < 1e-5 {
            self.amplitude = 0.0;
            return;
        }
        if self.joint >= pose.joint_count() || self.axis == Vec3::ZERO {
            return;
        }
        let kick = Quat::from_axis_angle(self.axis, self.amplitude);
        // Post-multiply: additive in the joint's own (post-aim) local frame.
        pose.r[self.joint] = pose.r[self.joint] * kick;
    }

    fn name(&self) -> &str {
        "recoil"
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// `acos` guarded against out-of-domain inputs from float error.
fn clamp_acos(x: f32) -> f32 {
    x.clamp(-1.0, 1.0).acos()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn karl() -> crate::skeletal::gltf_skin::SkinnedModel {
        let p = format!(
            "{}/../../assets/enemies/characters/russian-guard_karl.glb",
            env!("CARGO_MANIFEST_DIR")
        );
        crate::skeletal::gltf_skin::load(&p).expect("load karl")
    }

    /// The IK correctness oracle: after solving toward a reachable target, the
    /// `end` joint's global origin lands on the target. No eyeballing needed.
    #[test]
    fn ik_end_effector_reaches_reachable_target() {
        let m = karl();
        let sk = &m.skeleton;
        let mut ik = TwoBoneIkLayer::from_end_bone(sk, "Bone_9").expect("arm chain");

        // Start from the bind pose; pick a target near the shoulder within reach.
        let base = Pose::bind(sk);
        let g = base.joint_global_transforms(sk);
        let a = g[ik.root].to_scale_rotation_translation().2;
        let b = g[ik.mid].to_scale_rotation_translation().2;
        let c = g[ik.end].to_scale_rotation_translation().2;
        let l = (b - a).length() + (c - b).length();

        // A target at 60% of full reach, offset off-axis so the elbow must bend.
        ik.target = a + Vec3::new(0.4, -0.3, 0.5).normalize() * (0.6 * l);
        ik.pole = a + Vec3::new(0.0, -1.0, 1.0);
        ik.weight = 1.0;
        ik.enabled = true;

        let mut pose = base;
        let ctx = LayerCtx { skeleton: sk, dt: 1.0 / 60.0 };
        ik.apply(&mut pose, &ctx);

        let solved = pose.joint_global_transforms(sk);
        let reached = solved[ik.end].to_scale_rotation_translation().2;
        let err = (reached - ik.target).length();
        assert!(err < 1e-2, "end effector off target by {err} (reached {reached:?}, want {:?})", ik.target);
    }

    /// Weight 0 (or disabled) leaves the incoming pose untouched.
    #[test]
    fn ik_weight_zero_is_a_noop() {
        let m = karl();
        let sk = &m.skeleton;
        let mut ik = TwoBoneIkLayer::from_end_bone(sk, "Bone_9").unwrap();
        ik.target = Vec3::new(0.3, 0.3, 0.3);
        ik.enabled = true;
        ik.weight = 0.0;

        let mut pose = Pose::bind(sk);
        let before = pose.r.clone();
        ik.apply(&mut pose, &LayerCtx { skeleton: sk, dt: 0.016 });
        for (a, b) in before.iter().zip(&pose.r) {
            assert!((a.to_array().iter().zip(b.to_array()).map(|(x, y)| (x - y).abs()).sum::<f32>()) < 1e-6);
        }
    }

    /// Recoil: a kick perturbs the joint, then exponential decay returns it to
    /// (near) its pre-kick rotation.
    #[test]
    fn recoil_kicks_then_decays_back() {
        let m = karl();
        let sk = &m.skeleton;
        let joint = sk.index_of("Bone_9").unwrap();
        let mut recoil = AdditiveDecayLayer::new(joint, Vec3::X, 12.0, 0.6);

        let base = Pose::bind(sk);
        let rest = base.r[joint];

        // Kick, then evaluate one frame — the joint should have moved.
        recoil.kick(0.5);
        let mut pose = base.clone();
        recoil.apply(&mut pose, &LayerCtx { skeleton: sk, dt: 1.0 / 60.0 });
        let kicked_delta = rest.angle_between(pose.r[joint]);
        assert!(kicked_delta > 1e-2, "kick should rotate the joint (got {kicked_delta})");

        // Let ~1.5s pass; the amplitude should have decayed to rest.
        for _ in 0..90 {
            let mut p = base.clone();
            recoil.apply(&mut p, &LayerCtx { skeleton: sk, dt: 1.0 / 60.0 });
            pose = p;
        }
        let settled_delta = rest.angle_between(pose.r[joint]);
        assert!(settled_delta < 1e-2, "recoil should decay back to rest (got {settled_delta})");
    }

    /// The stack composes: IK aims the arm, recoil rides on top, and the final
    /// pose is finite and reflects both layers in order.
    #[test]
    fn stack_composes_ik_then_recoil() {
        let m = karl();
        let sk = &m.skeleton;
        let mut ik = TwoBoneIkLayer::from_end_bone(sk, "Bone_9").unwrap();
        let a = Pose::bind(sk).joint_global_transforms(sk)[ik.root]
            .to_scale_rotation_translation()
            .2;
        ik.target = a + Vec3::new(0.3, 0.0, 0.4);
        ik.weight = 1.0;
        ik.enabled = true;
        let hand = ik.end;

        let mut anim = LayeredAnimator::new();
        anim.push(Box::new(ik));
        anim.push(Box::new(AdditiveDecayLayer::new(hand, Vec3::X, 10.0, 0.6)));

        let ctx = LayerCtx { skeleton: sk, dt: 1.0 / 60.0 };
        // No recoil yet: pose is the pure IK result.
        let no_recoil = anim.evaluate(Pose::bind(sk), &ctx);

        // Fire: kick the recoil layer (index 1) via a typed downcast, evaluate again.
        anim.layer_as::<AdditiveDecayLayer>(1).expect("recoil at index 1").kick(0.4);
        let with_recoil = anim.evaluate(Pose::bind(sk), &ctx);

        // Recoil moved the hand relative to the pure-IK pose, and everything's finite.
        let d = no_recoil.r[hand].angle_between(with_recoil.r[hand]);
        assert!(d > 1e-2, "recoil should perturb the IK'd hand (got {d})");
        for q in &with_recoil.r {
            assert!(q.is_finite(), "non-finite rotation in composed pose");
        }
        assert_eq!(anim.layer_count(), 2);
    }
}
