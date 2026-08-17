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

use crate::skeletal::clip::AnimationClip;
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
    /// The target the `end` joint aims at. Interpretation depends on `reach_frac`:
    /// - `reach_frac <= 0` (default): `target` is the **absolute** point to reach.
    /// - `reach_frac > 0`: `target` is a far **aim point**; the effective hand
    ///   target is `shoulder + dir(target) × reach_frac × arm_length`, so the hand
    ///   reaches a fixed fraction of the *current* arm length toward `target` and
    ///   the elbow extension is exact regardless of where the shoulder currently
    ///   is (anchoring the point to a stale shoulder under-reaches otherwise).
    pub target: Vec3,
    /// See [`Self::target`]. `> 0` selects "aim at a far point, reach this fraction
    /// of arm length" mode (≈1 → nearly straight); `0` → absolute-target mode.
    pub reach_frac: f32,
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
            reach_frac: 0.0,
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
        let arm_len = l_ab + l_bc;
        // Effective hand target. In reach-fraction mode, aim from the CURRENT
        // shoulder toward `target` at a fixed fraction of the CURRENT arm length —
        // this makes the elbow extension exact (anchoring an absolute point to a
        // stale shoulder position under-reaches and leaves the elbow bent).
        let eff_target = if self.reach_frac > 1e-4 {
            let d = (self.target - a).normalize_or_zero();
            if d == Vec3::ZERO {
                return;
            }
            a + d * (self.reach_frac.min(0.999) * arm_len)
        } else {
            self.target
        };
        // Clamp the reach so the target is always achievable (arm never over-
        // extends past straight or folds through itself).
        let reach = (eff_target - a).length().clamp(1e-3, arm_len - 1e-3);
        let dir_at = (eff_target - a).normalize_or_zero();
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
// Look-at layer — aim one joint's axis at a target (single-joint global aim).
// ─────────────────────────────────────────────────────────────────────────────

/// Orients ONE joint so a chosen **joint-local** axis ends up pointing at a
/// model-space `target`. Used to point a hand's weapon barrel exactly at the aim
/// target *after* two-bone IK has placed the arm — IK controls where the hand is,
/// this controls where it points. Minimal-rotation (no roll control); blended
/// with the incoming pose by `weight`.
///
/// Only the joint's rotation changes, so its origin is unmoved — at `weight` 1
/// the local axis points exactly along `target − origin` (the test oracle).
pub struct LookAtLayer {
    pub joint: usize,
    /// The joint-local axis to aim (e.g. the barrel direction in the hand frame).
    pub aim_axis: Vec3,
    /// Model-space point to aim at.
    pub target: Vec3,
    pub weight: f32,
    pub enabled: bool,
}

impl PoseLayer for LookAtLayer {
    fn apply(&mut self, pose: &mut Pose, ctx: &LayerCtx) {
        if !self.enabled || self.weight <= 1e-4 {
            return;
        }
        let sk = ctx.skeleton;
        let globals = sk.global_transforms(&pose.locals());
        let (_, g_rot, origin) = globals[self.joint].to_scale_rotation_translation();
        let to_target = (self.target - origin).normalize_or_zero();
        let axis_world = (g_rot * self.aim_axis).normalize_or_zero();
        if to_target == Vec3::ZERO || axis_world == Vec3::ZERO {
            return;
        }
        // Rotate the current world aim axis onto the direction to the target.
        let delta = Quat::from_rotation_arc(axis_world, to_target);
        let g_new = delta * g_rot;
        let p_rot = match sk.parents[self.joint] {
            Some(p) => globals[p].to_scale_rotation_translation().1,
            None => Quat::IDENTITY,
        };
        let local_new = p_rot.inverse() * g_new;
        let w = self.weight.clamp(0.0, 1.0);
        pose.r[self.joint] = pose.r[self.joint].slerp(local_new, w);
    }

    fn name(&self) -> &str {
        "look-at"
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Aim-offset layer — a light, cone-clamped single-joint aim (no IK).
// ─────────────────────────────────────────────────────────────────────────────

/// Rotates ONE joint so a joint-local `forward` axis turns toward a model-space
/// `target`, but only as far as [`AimCone`] allows. This is the *light* aim used
/// instead of two-bone IK: point the shoulder (and thus the whole arm + attached
/// weapon) roughly at the player while **keeping the authored elbow bend** from the
/// clip — none of the arm-straightening / robotic extension that full reach-IK
/// produced. Blended with the incoming pose by `weight`.
///
/// Because only the shoulder rotates and the swing is capped, the motion reads as
/// "raise the weapon toward you" layered on the hand-authored locomotion pose,
/// which is how GoldenEye/Perfect Dark guards aimed — the hit itself is a
/// probability roll, so the barrel never needs to point *exactly* at the target.
pub struct AimOffsetLayer {
    pub joint: usize,
    /// The joint-local axis to aim (the arm's rest shoulder→hand direction).
    pub forward: Vec3,
    /// Model-space point to aim toward.
    pub target: Vec3,
    /// How far the joint may swing. Rotations past it are clamped, so a target
    /// behind the shoulder just pins the arm at the limit, never contorts it.
    pub cone: AimCone,
    pub weight: f32,
    pub enabled: bool,
}

/// How far an aim may swing, **separately per direction** (radians, all positive).
///
/// A single cone angle cannot express a real aim limit: a body can look further up
/// than down, and much further up than it can twist sideways. Perfect Dark authors
/// exactly these four numbers per attack animation — `maxup` / `maxdown` /
/// `maxleft` / `maxright` on `struct attackanimconfig`, past which the character
/// leans back or forward instead of swivelling further (`types.h:359`).
///
/// Angles are measured from `forward` in the model's frame, with **model +Y as up**
/// — the character stands upright in model space, and "aiming up" means up in the
/// room, not up relative to a bone that may itself be tilted.
#[derive(Clone, Copy, Debug)]
pub struct AimCone {
    pub up: f32,
    pub down: f32,
    pub left: f32,
    pub right: f32,
}

impl AimCone {
    /// The same limit in every direction — the old single-angle cone.
    pub fn uniform(angle: f32) -> Self {
        AimCone { up: angle, down: angle, left: angle, right: angle }
    }
}

impl PoseLayer for AimOffsetLayer {
    fn apply(&mut self, pose: &mut Pose, ctx: &LayerCtx) {
        if !self.enabled || self.weight <= 1e-4 {
            return;
        }
        let sk = ctx.skeleton;
        let globals = sk.global_transforms(&pose.locals());
        let (_, g_rot, origin) = globals[self.joint].to_scale_rotation_translation();
        let to_target = (self.target - origin).normalize_or_zero();
        let axis_world = (g_rot * self.forward).normalize_or_zero();
        if to_target == Vec3::ZERO || axis_world == Vec3::ZERO {
            return;
        }
        // Clamp the swing per direction rather than as one cone: split the required
        // turn into yaw (toward the character's left/right) and pitch (up/down)
        // about model up, limit each, and rebuild the direction. Falls back to a
        // plain angle clamp when the aim axis is near-vertical and the horizontal
        // frame degenerates.
        let up_ref = Vec3::Y;
        let left_axis = up_ref.cross(axis_world);
        let aimed = if left_axis.length_squared() > 1e-6 {
            let left_axis = left_axis.normalize();
            // Re-derive an exactly orthogonal forward so the decomposition is clean
            // even if `axis_world` is slightly off the horizontal plane.
            let fwd_h = left_axis.cross(up_ref).normalize_or_zero();
            let mut yaw = to_target.dot(left_axis).atan2(to_target.dot(fwd_h));
            let mut pitch = to_target.dot(up_ref).clamp(-1.0, 1.0).asin();
            // Clamp to the ELLIPSE the four limits describe, not to their bounding
            // box: clamping yaw and pitch independently would let a diagonal aim sit
            // outside the limit in both, so `AimCone::uniform(a)` would no longer be
            // the circular cone it claims to be. Scaling both back along the same ray
            // keeps the aim direction and only shortens the swing.
            let lim_y = if yaw >= 0.0 { self.cone.left } else { self.cone.right };
            let lim_p = if pitch >= 0.0 { self.cone.up } else { self.cone.down };
            let ny = if lim_y > 1e-6 { yaw / lim_y } else { 0.0 };
            let np = if lim_p > 1e-6 { pitch / lim_p } else { 0.0 };
            let r = (ny * ny + np * np).sqrt();
            if r > 1.0 {
                yaw /= r;
                pitch /= r;
            }
            let (sy, cy) = yaw.sin_cos();
            let (sp, cp) = pitch.sin_cos();
            (fwd_h * (cp * cy) + left_axis * (cp * sy) + up_ref * sp).normalize_or_zero()
        } else {
            let (axis, angle) = Quat::from_rotation_arc(axis_world, to_target).to_axis_angle();
            let cap = self.cone.up.max(self.cone.down).max(self.cone.left).max(self.cone.right);
            (Quat::from_axis_angle(axis, angle.min(cap)) * axis_world).normalize_or_zero()
        };
        if aimed == Vec3::ZERO {
            return;
        }
        let g_new = Quat::from_rotation_arc(axis_world, aimed) * g_rot;
        let p_rot = match sk.parents[self.joint] {
            Some(p) => globals[p].to_scale_rotation_translation().1,
            None => Quat::IDENTITY,
        };
        let local_new = p_rot.inverse() * g_new;
        let w = self.weight.clamp(0.0, 1.0);
        pose.r[self.joint] = pose.r[self.joint].slerp(local_new, w);
    }

    fn name(&self) -> &str {
        "aim-offset"
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Clip overlay layer — an authored pose over a masked joint subset (upper body).
// ─────────────────────────────────────────────────────────────────────────────

/// Overlays an authored [`AnimationClip`] onto a **masked subset of joints**
/// (e.g. the upper body — chest + arms), blended by `weight`, leaving every other
/// joint whatever an earlier layer produced (the locomotion legs).
///
/// This is the authentic GoldenEye / Perfect Dark "aim while moving" technique.
/// The hand-authored fire/aim clip already poses BOTH arms into the correct
/// weapon-specific hold (two-hand rifle grip, leveled pistol, akimbo) — so
/// overlaying it on the arm joints restores that exact hold *for free*, with none
/// of the single-joint procedural swing that could only rotate the shoulder and
/// left the off hand and wrist un-posed. The legs keep running from the base.
///
/// `time` is a fixed sample point for a static aim hold (advance it for an
/// animated overlay). At `weight` 1 the masked joints exactly match the clip at
/// `time`; at 0 the layer is a no-op (the test oracle).
///
/// # Root reconciliation — why the mask alone is not enough
///
/// A masked overlay copies **local** transforms, so the masked subtree keeps its
/// orientation *relative to the first unmasked parent* — normally the pelvis. That is
/// only correct if the overlay clip and the base pose agree about where that parent is
/// pointing. They frequently do not: a hand-authored fire animation turns the whole
/// body toward its target, so the clip's own root sits at some authored yaw, while the
/// base (locomotion) root sits at its own.
///
/// Grafting the upper body across that difference rotates the entire hold — gun
/// included — by exactly the root delta. Measured on our three fire clips, whose guns
/// all point within 3.3° of forward when the clip is sampled alone:
///
/// | clip | clip root | base root | gun ends up |
/// |---|---|---|---|
/// | rifle | −53.1° | −11.4° | +39° off |
/// | pistol | +68.7° | −11.4° | −78° off |
/// | dual | −15.8° | −11.4° | +4° off |
///
/// The dual clip looked fine for months purely because it happens to be authored at
/// nearly the same root yaw as the locomotion clips.
///
/// So the overlay reconciles the two roots: it rotates the topmost masked joint by
/// `inverse(base parent) · clip parent`, which lands the subtree at the orientation the
/// animation actually posed rather than at one inherited from an unrelated clip. With
/// that, an authored aim pose aims where it was drawn to aim, and a downstream aim
/// solver gets its full swing budget for tracking instead of spending it undoing this.
pub struct ClipOverlayLayer {
    clip: AnimationClip,
    /// The joint indices this clip writes (the upper-body mask).
    joints: Vec<usize>,
    /// The topmost masked joint (the subtree root — e.g. the chest) and its parent, or
    /// `None` when the mask reaches the skeleton root and there is nothing to reconcile
    /// against. Resolved once in [`Self::new`].
    subtree_root: Option<(usize, usize)>,
    /// Sample time (s) into the clip — held fixed for a static aim pose.
    pub time: f32,
    pub weight: f32,
    pub enabled: bool,
}

impl ClipOverlayLayer {
    pub fn new(clip: AnimationClip, joints: Vec<usize>) -> Self {
        ClipOverlayLayer {
            clip,
            joints,
            subtree_root: None,
            time: 0.0,
            weight: 0.0,
            enabled: true,
        }
    }

    /// Swap the posing clip, keeping the mask and the cached seam. Used when the same
    /// masked subtree has to play a *different* authored pose without rebuilding the
    /// layer stack — a hunter switching between Perfect Dark's per-bearing attack
    /// animations mid-fight. The seam is a property of the mask and the skeleton, not
    /// of the clip, so it stays valid; the caller is responsible for re-deriving
    /// anything it measured off the old pose (e.g. a downstream aim axis).
    pub fn set_clip(&mut self, clip: AnimationClip) {
        self.clip = clip;
    }

    /// The clip currently posing the mask, so a caller can size a sample time against
    /// it (or check what it swapped in).
    pub fn clip(&self) -> &AnimationClip {
        &self.clip
    }

    /// The topmost masked joint + its (unmasked) parent — the seam the root
    /// reconciliation acts on. Cached on first use because it needs the skeleton.
    fn seam(&mut self, sk: &Skeleton) -> Option<(usize, usize)> {
        if self.subtree_root.is_none() {
            // The masked joint whose parent is NOT masked: the subtree's root.
            let top = self
                .joints
                .iter()
                .copied()
                .find(|&j| sk.parents.get(j).copied().flatten().is_none_or(|p| !self.joints.contains(&p)))?;
            let parent = sk.parents.get(top).copied().flatten()?;
            self.subtree_root = Some((top, parent));
        }
        self.subtree_root
    }
}

impl PoseLayer for ClipOverlayLayer {
    fn apply(&mut self, pose: &mut Pose, ctx: &LayerCtx) {
        if !self.enabled || self.weight <= 1e-4 || self.joints.is_empty() {
            return;
        }
        let sk = ctx.skeleton;
        let (t, mut r, s) = self.clip.pose_trs(self.time, sk);
        // Root reconciliation (see the type docs): rotate the subtree's top joint so the
        // masked body lands where the CLIP posed it, instead of inheriting the base
        // pose's unrelated root orientation. Computed from the two parents' global
        // rotations — the clip's, from its own full pose, and the base's, from whatever
        // earlier layers produced.
        if let Some((top, parent)) = self.seam(sk) {
            let clip_parent = {
                let locals: Vec<Mat4> = (0..t.len())
                    .map(|i| Mat4::from_scale_rotation_translation(s[i], r[i], t[i]))
                    .collect();
                sk.global_transforms(&locals)[parent].to_scale_rotation_translation().1
            };
            let base_parent = sk.global_transforms(&pose.locals())[parent]
                .to_scale_rotation_translation()
                .1;
            // `base_parent · correction · top_local` == `clip_parent · top_local`.
            let correction = base_parent.inverse() * clip_parent;
            if top < r.len() {
                r[top] = correction * r[top];
            }
        }
        let w = self.weight.clamp(0.0, 1.0);
        for &j in &self.joints {
            if j >= pose.joint_count() {
                continue;
            }
            pose.t[j] = pose.t[j].lerp(t[j], w);
            pose.r[j] = pose.r[j].slerp(r[j], w);
            pose.s[j] = pose.s[j].lerp(s[j], w);
        }
    }

    fn name(&self) -> &str {
        "clip-overlay"
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Root translate layer — offset one joint's local position (pelvis drop for foot IK).
// ─────────────────────────────────────────────────────────────────────────────

/// Adds a fixed offset to ONE joint's **local** translation. Used to lower the pelvis
/// (the root joint, whose local space is model space) so ground-adaptive foot IK can
/// plant both feet on uneven ground — stairs, platform edges — without a leg stretching
/// straight to reach a lower step. Runs after locomotion (which sets the leg pose) and
/// before the foot IK (which then solves each foot to the ground from the lowered hips).
/// A no-op at `offset == 0` or when disabled.
pub struct RootTranslateLayer {
    pub joint: usize,
    /// Local-space translation added to the joint (model space for the root joint).
    pub offset: Vec3,
    pub enabled: bool,
}

impl PoseLayer for RootTranslateLayer {
    fn apply(&mut self, pose: &mut Pose, _ctx: &LayerCtx) {
        if !self.enabled || self.joint >= pose.joint_count() {
            return;
        }
        pose.t[self.joint] += self.offset;
    }

    fn name(&self) -> &str {
        "root-translate"
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

// ─────────────────────────────────────────────────────────────────────────────
// Locomotion blend space — the base-layer kind (writes the whole pose).
// ─────────────────────────────────────────────────────────────────────────────

/// A **1D locomotion blend space**: a continuous function of `speed` over a set
/// of gait clips (idle / walk / jog / run) sorted by their speed anchor. Where
/// the clip mixer *switches* between discrete bands and crossfades over a fixed
/// time, this **blends the two bracketing clips by where `speed` falls between
/// their anchors** — so accelerating morphs the gait smoothly with no band pops.
///
/// **Foot-phase sync:** both clips are sampled at the *same* normalized gait
/// phase `[0,1)`, and the shared phase advances by the *blended* clip period, so
/// the two gaits stay in step through the blend (the classic fix for the
/// foot-skate you'd get sampling each clip on its own clock). Full foot-plant IK
/// — locking a contact foot to the ground — is a deliberately separate, later
/// concern; this removes cross-clip desync but not ground slide.
///
/// This is a **base layer**: it ignores the incoming pose and writes every joint,
/// so it belongs first in the stack (aim/recoil layer on top of it).
pub struct LocomotionBlendLayer {
    /// `(speed_anchor, clip)` sorted ascending by anchor; `[0]` is idle at 0 m/s.
    anchors: Vec<(f32, AnimationClip)>,
    /// Target locomotion speed (m/s); clamped into the anchor range.
    pub speed: f32,
    /// Multiplier on the gait phase advance rate — the cadence knob. `1.0` cycles the
    /// feet at the blended clip's authored cadence; `< 1.0` slows the cadence so the
    /// stride matches a character whose ACTUAL ground speed is below the gait speed
    /// (e.g. an avoidance-yielded hunter), cutting foot-skate. Pose blend is unaffected
    /// (that still comes from [`Self::speed`]); only the foot timing changes.
    pub stride_scale: f32,
    /// Shared normalized gait phase `[0,1)`.
    phase: f32,
    /// When false, the layer is a no-op — so a hit/death one-shot pose fed as the
    /// base can pass through untouched instead of being overwritten by locomotion.
    pub enabled: bool,
}

impl LocomotionBlendLayer {
    /// Build from `(speed, clip)` anchors. They're sorted by speed here, so the
    /// caller needn't pre-order them. Needs at least one anchor.
    pub fn new(mut anchors: Vec<(f32, AnimationClip)>) -> Self {
        anchors.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        LocomotionBlendLayer {
            anchors,
            speed: 0.0,
            stride_scale: 1.0,
            phase: 0.0,
            enabled: true,
        }
    }

    /// The two bracketing anchor indices for the current (clamped) speed, plus the
    /// blend weight `w` from the low anchor (0) to the high one (1).
    fn bracket(&self) -> (usize, usize, f32) {
        let n = self.anchors.len();
        if n == 1 {
            return (0, 0, 0.0);
        }
        let lo = self.anchors[0].0;
        let hi = self.anchors[n - 1].0;
        let s = self.speed.clamp(lo, hi);
        for i in 0..n - 1 {
            let (s0, _) = &self.anchors[i];
            let (s1, _) = &self.anchors[i + 1];
            if s <= *s1 {
                let w = if s1 > s0 { (s - s0) / (s1 - s0) } else { 0.0 };
                return (i, i + 1, w);
            }
        }
        (n - 1, n - 1, 0.0)
    }
}

impl PoseLayer for LocomotionBlendLayer {
    fn apply(&mut self, pose: &mut Pose, ctx: &LayerCtx) {
        if !self.enabled || self.anchors.is_empty() {
            return;
        }
        let (i, j, w) = self.bracket();
        let (_, c0) = &self.anchors[i];
        let (_, c1) = &self.anchors[j];
        let (d0, d1) = (c0.duration.max(1e-3), c1.duration.max(1e-3));

        // Sample both clips at the SAME normalized phase (foot-sync), then blend.
        let (t0, r0, s0) = c0.pose_trs(self.phase * d0, ctx.skeleton);
        if w <= 1e-5 {
            pose.t = t0;
            pose.r = r0;
            pose.s = s0;
        } else {
            let (t1, r1, s1) = c1.pose_trs(self.phase * d1, ctx.skeleton);
            for k in 0..pose.joint_count() {
                pose.t[k] = t0[k].lerp(t1[k], w);
                pose.r[k] = r0[k].slerp(r1[k], w);
                pose.s[k] = s0[k].lerp(s1[k], w);
            }
        }

        // Advance the shared phase by the BLENDED cadence, so a faster gait cycles
        // faster and the two clips stay in step. `stride_scale` warps the cadence to
        // the character's real ground speed (cut foot-skate) without touching the pose.
        let period = d0 + (d1 - d0) * w;
        self.phase =
            (self.phase + self.stride_scale.max(0.0) * ctx.dt / period.max(1e-3)).rem_euclid(1.0);
    }

    fn name(&self) -> &str {
        "locomotion-blend"
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

    /// Aim-offset: with a wide cone the joint's forward axis points at the target
    /// (like a look-at); with a tiny cone the swing is clamped to it.
    #[test]
    fn aim_offset_aims_forward_and_clamps_to_cone() {
        let m = karl();
        let sk = &m.skeleton;
        let joint = sk.index_of("Bone_5").expect("shoulder");
        let base = Pose::bind(sk);
        let g = base.joint_global_transforms(sk);
        let (_, gr, origin) = g[joint].to_scale_rotation_translation();
        let forward_local = Vec3::Z;
        let world_forward = (gr * forward_local).normalize();
        let ctx = LayerCtx { skeleton: sk, dt: 0.0 };

        // (a) Wide cone → axis lands on the target direction.
        let dir = (world_forward + Vec3::new(0.2, 0.1, 0.0)).normalize();
        let mut aim = AimOffsetLayer {
            joint,
            forward: forward_local,
            target: origin + dir * 2.0,
            cone: AimCone::uniform(std::f32::consts::PI),
            weight: 1.0,
            enabled: true,
        };
        let mut pose = base.clone();
        aim.apply(&mut pose, &ctx);
        let nr = pose.joint_global_transforms(sk)[joint].to_scale_rotation_translation().1;
        let err = (nr * forward_local).normalize().angle_between(dir);
        assert!(err < 1e-2, "wide cone should aim at the target (off by {err})");

        // (b) Opposite target + tiny cone → swing clamped to max_angle.
        let max = 0.3;
        let mut aim2 = AimOffsetLayer {
            joint,
            forward: forward_local,
            target: origin - world_forward * 2.0,
            cone: AimCone::uniform(max),
            weight: 1.0,
            enabled: true,
        };
        let mut pose2 = base.clone();
        aim2.apply(&mut pose2, &ctx);
        let nr2 = pose2.joint_global_transforms(sk)[joint].to_scale_rotation_translation().1;
        let swung = (nr2 * forward_local).normalize().angle_between(world_forward);
        assert!(swung <= max + 1e-2, "swing should clamp to the cone (got {swung})");
    }

    /// Clip overlay writes ONLY the masked joints (to the clip's pose) and leaves
    /// every other joint untouched — the upper/lower-body split the run-and-gun aim
    /// relies on. Weight 0 is a full no-op.
    #[test]
    fn clip_overlay_masks_joints_and_respects_weight() {
        let m = karl();
        let sk = &m.skeleton;
        // Upper body = the Bone_2 (chest) subtree — chest + head + both arms.
        let chest = sk.index_of("Bone_2").unwrap();
        let mask: Vec<usize> = (0..sk.joint_count())
            .filter(|&i| {
                let mut j = i;
                loop {
                    if j == chest {
                        break true;
                    }
                    match sk.parents[j] {
                        Some(p) => j = p,
                        None => break false,
                    }
                }
            })
            .collect();
        let fire = clip("01-fire-standing.glb", sk);
        let ctx = LayerCtx { skeleton: sk, dt: 0.0 };

        // Start from the walk pose so masked joints visibly change and the rest don't.
        let walk = clip("28-walking.glb", sk);
        let (t, r, s) = walk.pose_trs(0.4, sk);
        let base = Pose::from_trs(t, r, s);

        // Rotation "equal" via the dot product (handles the q ≡ -q double cover, and
        // avoids `angle_between`'s acos-of-≥1 NaN on near-identical quats).
        let rot_close = |a: Quat, b: Quat| (1.0 - a.dot(b).abs()) < 1e-6;

        // Weight 0 → identical to base everywhere.
        let mut off = ClipOverlayLayer::new(fire.clone(), mask.clone());
        let mut p0 = base.clone();
        off.apply(&mut p0, &ctx);
        for i in 0..sk.joint_count() {
            assert!(rot_close(p0.r[i], base.r[i]), "weight 0 must be a no-op at joint {i}");
        }

        // Weight 1 → masked joints match the fire clip; unmasked match the base.
        let mut on = ClipOverlayLayer::new(fire.clone(), mask.clone());
        on.time = 0.5;
        on.weight = 1.0;
        let mut p1 = base.clone();
        on.apply(&mut p1, &ctx);
        let (ft, fr, fs) = fire.pose_trs(0.5, sk);
        let is_masked = |i: usize| mask.contains(&i);
        for i in 0..sk.joint_count() {
            if is_masked(i) {
                // The subtree ROOT carries the root reconciliation (see the type docs),
                // so its local rotation is deliberately NOT the clip's — it is the
                // clip's pre-rotated to cancel the two poses' differing root yaws. Every
                // other masked joint is still a verbatim copy.
                if i != chest {
                    assert!(rot_close(p1.r[i], fr[i]), "masked joint {i} should equal the fire clip");
                }
                assert!((p1.t[i] - ft[i]).length() < 1e-4 && (p1.s[i] - fs[i]).length() < 1e-4);
            } else {
                assert!(rot_close(p1.r[i], base.r[i]), "unmasked joint {i} must be untouched");
            }
        }

        // The reconciliation's actual contract, and the reason it exists: the masked
        // subtree must end up at the orientation the CLIP posed it at, in world terms —
        // not merely at the clip's orientation *relative to a pelvis from another
        // animation*. Without it, a fire clip authored with the body turned (ours range
        // over 122° of root yaw) drags the whole hold, and the gun, round with it.
        let fire_globals = sk.global_transforms(&Pose::from_trs(ft, fr, fs).locals());
        let out_globals = sk.global_transforms(&p1.locals());
        for &j in &mask {
            let want = fire_globals[j].to_scale_rotation_translation().1;
            let got = out_globals[j].to_scale_rotation_translation().1;
            assert!(
                rot_close(want, got),
                "masked joint {j} must land at the clip's own global orientation"
            );
        }
        // Sanity: the mask excludes the legs (Bone_10..15 hang off the root).
        for leg in ["Bone_10", "Bone_12", "Bone_14", "Bone_11", "Bone_13", "Bone_15"] {
            assert!(!is_masked(sk.index_of(leg).unwrap()), "{leg} must not be in the upper-body mask");
        }
    }

    fn clip(name: &str, sk: &Skeleton) -> AnimationClip {
        let p = format!(
            "{}/../../assets/enemies/animations/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        crate::skeletal::clip::load(&p, sk).expect(name)
    }

    fn loco_layer(sk: &Skeleton) -> LocomotionBlendLayer {
        LocomotionBlendLayer::new(vec![
            (0.0, clip("00-idle.glb", sk)),
            (1.5, clip("28-walking.glb", sk)),
            (3.5, clip("2A-jogging.glb", sk)),
            (5.0, clip("29-running.glb", sk)),
        ])
    }

    fn pose_diff(a: &Pose, b: &Pose) -> f32 {
        a.r.iter()
            .zip(&b.r)
            .map(|(x, y)| x.angle_between(*y))
            .sum::<f32>()
    }

    /// At a speed exactly on an anchor, the blended pose equals that anchor's clip
    /// sampled at the shared phase (weight collapses to the low bracket).
    #[test]
    fn blend_at_anchor_matches_the_clip() {
        let m = karl();
        let sk = &m.skeleton;
        let walk = clip("28-walking.glb", sk);
        let mut loco = loco_layer(sk);
        loco.speed = 1.5; // exactly the walk anchor
        loco.phase = 0.3;

        let mut pose = Pose::bind(sk);
        // apply() advances phase, so capture the phase it samples at first.
        let sampled_phase = loco.phase;
        loco.apply(&mut pose, &LayerCtx { skeleton: sk, dt: 0.0 });

        let (t, r, s) = walk.pose_trs(sampled_phase * walk.duration, sk);
        let want = Pose::from_trs(t, r, s);
        assert!(pose_diff(&pose, &want) < 1e-4, "anchor pose should equal the walk clip");
    }

    /// Continuity: a tiny speed step produces a tiny pose change — no band pop
    /// (the whole point vs. discrete walk/jog/run switching).
    #[test]
    fn blend_is_continuous_across_speed() {
        let m = karl();
        let sk = &m.skeleton;
        let ctx = LayerCtx { skeleton: sk, dt: 0.0 };

        // Two layers at the same phase, speeds a hair apart across the walk→run gap.
        let mut a = loco_layer(sk);
        let mut b = loco_layer(sk);
        a.phase = 0.5;
        b.phase = 0.5;
        a.speed = 3.0;
        b.speed = 3.05;
        let mut pa = Pose::bind(sk);
        let mut pb = Pose::bind(sk);
        a.apply(&mut pa, &ctx);
        b.apply(&mut pb, &ctx);

        // And a large step, to show the small step really is proportionally small.
        let mut c = loco_layer(sk);
        c.phase = 0.5;
        c.speed = 5.0;
        let mut pc = Pose::bind(sk);
        c.apply(&mut pc, &ctx);

        let small = pose_diff(&pa, &pb);
        let large = pose_diff(&pa, &pc);
        assert!(small < large * 0.2, "small speed step ({small}) not small vs large ({large})");
    }

    /// The shared phase advances with dt and wraps in [0,1).
    #[test]
    fn blend_phase_advances_and_wraps() {
        let m = karl();
        let sk = &m.skeleton;
        let mut loco = loco_layer(sk);
        loco.speed = 5.0;
        loco.phase = 0.0;
        let mut pose = Pose::bind(sk);
        loco.apply(&mut pose, &LayerCtx { skeleton: sk, dt: 1.0 / 60.0 });
        assert!(loco.phase > 0.0 && loco.phase < 1.0, "phase advanced into range");
    }

    /// Cadence knob: halving `stride_scale` halves the phase advance (feet cycle at
    /// half rate) — the pose blend is untouched, only the timing.
    #[test]
    fn stride_scale_warps_cadence() {
        let m = karl();
        let sk = &m.skeleton;
        let dt = 1.0 / 60.0;
        let mut full = loco_layer(sk);
        full.speed = 5.0;
        full.phase = 0.0;
        full.stride_scale = 1.0;
        let mut half = loco_layer(sk);
        half.speed = 5.0;
        half.phase = 0.0;
        half.stride_scale = 0.5;
        let mut pf = Pose::bind(sk);
        let mut ph = Pose::bind(sk);
        full.apply(&mut pf, &LayerCtx { skeleton: sk, dt });
        half.apply(&mut ph, &LayerCtx { skeleton: sk, dt });
        assert!(full.phase > 0.0, "full-stride phase advanced");
        assert!(
            (half.phase - full.phase * 0.5).abs() < 1e-6,
            "half stride_scale should advance phase half as far ({} vs {})",
            half.phase,
            full.phase
        );
    }

    /// Root translate offsets the joint's local position, and is a no-op when disabled.
    #[test]
    fn root_translate_offsets_joint_and_respects_enabled() {
        let m = karl();
        let sk = &m.skeleton;
        let root = sk.index_of("Bone_1").expect("pelvis");
        let base = Pose::bind(sk);
        let off = Vec3::new(0.0, -5.0, 0.0);
        let ctx = LayerCtx { skeleton: sk, dt: 0.0 };

        let mut on = RootTranslateLayer { joint: root, offset: off, enabled: true };
        let mut p = base.clone();
        on.apply(&mut p, &ctx);
        assert!((p.t[root] - (base.t[root] + off)).length() < 1e-6, "offset should apply");

        let mut off_layer = RootTranslateLayer { joint: root, offset: off, enabled: false };
        let mut p2 = base.clone();
        off_layer.apply(&mut p2, &ctx);
        assert!((p2.t[root] - base.t[root]).length() < 1e-6, "disabled must be a no-op");
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
