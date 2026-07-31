//! The authored scene — a hand-rolled `World` (no ECS yet; entity counts don't
//! justify one until the Phase 3 enemy roster). Owns the CSG regions, the
//! collision world, and the fly camera, and drives the BUILD-phase authoring
//! loop: crosshair face-pick → push/pull → re-evaluate the region → hand the
//! app a fresh mesh while updating the region's collider in place.
//!
//! Mirrors the reference editor (`src/tools/indoorKeys.js` + `csgActions.js`):
//! `+`/`=` push (carve inward), `-` pull (extend outward), default step 4 WT.

use std::time::Instant;

use glam::{EulerRot, Mat4, Quat, Vec3};

use engine::render::camera::FlyCamera;
use crate::character::CharacterController;
// NB: `crate::combat` (the subsystem) vs `world::combat` (the `mod combat;` wiring
// submodule below) share a name — import only the types, and reach the crate
// module fully-qualified (`crate::combat::…`) to avoid the shadow.
use engine::assets::textured_model::TexturedModel;
use engine::audio::AudioManager;
use crate::combat::enemy_weapons::{
    HEAD_BONE, LEFT_FOOT_BONE, LEFT_HAND_BONE, PELVIS_BONE, RIGHT_FOOT_BONE, RIGHT_HAND_BONE,
};
use crate::combat::{enemy_def_for, EnemyWeaponClass, EnemyWeaponDef, Weapon};
use engine::geometry::csg_runtime::{
    Axis, Brush, Op, Region, Side, StairDesc, StairDir, WALL_THICKNESS, WORLD_SCALE,
};
use crate::enemy::{AiState, Enemy};
use rapier3d::prelude::ColliderHandle;
use engine::platform::input::InputState;
use engine::render::mesh::{ColorVertex, ColoredMesh, CpuMesh, TexVertex, TexturedMesh};
use engine::sim::avoidance;
use engine::sim::nav::{self, NavWorld};
use engine::sim::physics::PhysicsWorld;
use engine::sim::ragdoll::Ragdoll;
use engine::skeletal::anim::AnimPlayer;
use engine::skeletal::anim_set;
use engine::skeletal::clip;
use engine::skeletal::layers::{
    AdditiveDecayLayer, AimOffsetLayer, ClipOverlayLayer, LayerCtx, LayeredAnimator,
    LocomotionBlendLayer, Pose, PoseLayer, RootTranslateLayer, TwoBoneIkLayer,
};
use engine::skeletal::gltf_skin::{self, SkinnedModel};
use engine::geometry::structures::{self, Anchor, Edge, Platform, StairRun};
use engine::render::textures::DEFAULT_SCHEME;
use engine::render::uv_zones::ZonedBuilder;

// ─── Submodule tree (the `impl World` methods are spread across these) ──
mod combat;
mod editing;
mod geom;
mod history;
mod hunt;
mod lifecycle;
mod persist;
mod pick;
mod regions;
mod spike_preview;
mod tools;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod ai_testbed;

// Module-internal free helpers, re-exported so every submodule reaches them
// through `use super::*` regardless of which file defines them. (`find_room_brushes`
// / `brushes_touching` are used only within `editing`, so they aren't re-exported.)
pub(crate) use geom::{
    append_textured_collision, boxes_mesh, make_stair_void, make_wall_brush, push_colored_box,
    structure_collider_mesh,
};
pub(crate) use hunt::{band_for_speed, fire_window_for};
pub(crate) use lifecycle::pick_spread_spawns;
pub(crate) use pick::{flip, same_face};

/// Default push/pull increment, in WT (JS `PUSH_PULL_STEP`). Shift → 1 WT.
pub const PUSH_PULL_STEP: f32 = 4.0;

// ─── GoldenEye free-aim (Player Combat; hold RMB) ──────────────────────────
/// The crosshair floats within this circular radius in "aim space" (an isotropic
/// NDC-like space); drawn aspect-corrected so the boundary reads circular on
/// screen. Ported from GamepadManager `AIM_MAX_RANGE`.
pub(crate) const AIM_MAX_RANGE: f32 = 0.6;
/// Mouse pixels → aim-space units. Equal to the camera `LOOK_SPEED` so that when
/// the crosshair is pinned at the rim the leftover motion pans the view seamlessly.
pub(crate) const AIM_SENS: f32 = 0.002;
/// Crosshair snap-back-to-center speed when not aiming (JS `RETURN_SPRING`).
pub(crate) const AIM_RETURN_SPRING: f32 = 15.0;
/// tan(½ · 60°) — the world/viewmodel vertical FOV. Maps an aim-space offset to
/// an angular offset for the gun tilt + the fire ray.
pub(crate) const AIM_FOV_TAN: f32 = 0.577_350_3;

// ─── USB-N64 gamepad (GoldenEye "solitaire" scheme) ────────────────────────
// Ported verbatim from the 3DS FPS `GamepadManager.ts`. `AIM_MAX_RANGE` /
// `AIM_RETURN_SPRING` above are shared with the mouse free-aim.
/// Radial stick deadzone — below this magnitude the stick reads as centered.
pub(crate) const STICK_DEADZONE: f32 = 0.15;
/// Camera-yaw rate at full stick, in mouse-pixel-equivalents per second (fed to
/// `apply_look_delta`, so the effective rad/s is this × the camera `LOOK_SPEED`).
pub(crate) const PAD_TURN_SPEED: f32 = 1800.0;
/// Aim-mode crosshair spring stiffness toward the stick target (higher = snappier).
pub(crate) const PAD_AIM_SPRING: f32 = 10.0;
/// Stick magnitude at which aim-mode begins rotating the camera (below it, the
/// crosshair just floats).
pub(crate) const PAD_AIM_TURN_THRESHOLD: f32 = 0.85;
/// Camera-rotation rate (pixel-equivalents/s) once past the aim-turn threshold.
pub(crate) const PAD_AIM_TURN_SPEED: f32 = 600.0;
/// C-Up / C-Down look rate (pixel-equivalents/s).
pub(crate) const PAD_C_LOOK_SPEED: f32 = 300.0;
/// Vertical-look sign for the gamepad: `-1.0` = inverted (stick-up looks/aims
/// down, GoldenEye's N64 default), `+1.0` = non-inverted. Applied consistently to
/// the aim reticle, the aim-mode camera pan, and C-Up/C-Down so they never fight.
pub(crate) const PAD_PITCH_SIGN: f32 = -1.0;

/// Skinned-character model scale: GoldenEye units → metres. The 3DS FPS port used
/// 0.00104 (base 0.001 + ~4%); shrunk to ~80% (user call 2026-07-17) so the hunter
/// reads better against the level. The GE-unit weapon bone offsets and the computed
/// `char_feet_offset` both flow through this scale, so they shrink with the model.
pub(crate) const CHAR_SCALE: f32 = 0.000_832; // 0.00104 × 0.8

// ─── Procedural aim + recoil (authored upper-body aim clip over locomotion) ─────
// History: 2-bone arm IK → foregrip IK → a single cone-clamped shoulder swing were
// all tried and scrapped. IK fought the authored clips and read robotic; the lone
// shoulder swing could only rotate ONE joint, so it held two-handed rifles
// one-handed and canted the pistols (it never posed the off hand or the wrist).
//
// What's here now is the authentic GoldenEye/Perfect Dark technique: the authored
// fire/aim clip — which already poses BOTH arms into the correct weapon-specific
// hold — is overlaid on the **upper body only** (chest + arms) via a masked
// [`engine::skeletal::layers::ClipOverlayLayer`], while the continuous locomotion
// blend keeps driving the **legs**. So a hunter runs AND holds the gun correctly.
// Recoil kicks on top; the hit is a probability roll, so the barrel needn't point
// exactly at the target (the model's yaw already faces the player).
//
// One catch the overlay can't fix alone: each authored fire clip aims the barrel in
// its own model-space direction (the single-pistol and rifle clips are bladed off to
// a side, the dual clip aims forward), and none of them track the player's height. A
// light `AimOffsetLayer` on the **chest** (`Bone_2`, the parent of both arms) rotates
// the whole authored hold rigidly so the **real gun barrel points at the player** —
// yaw AND pitch — with the hold intact. The model yaw still does the gross turn; the
// chest-aim corrects the clip's bias + tracks the player, cone-clamped so it never
// over-twists the torso.
/// Stack layer indices for a hunter's [`LayeredAnimator`] (build order in
/// [`EnemyArm::build_stack`]): locomotion base, upper-body aim overlay, chest-aim
/// correction, head look-at, recoil. The head look-at sits AFTER the chest-aim so it
/// overrides the overlay's authored head pose and resolves globals against the
/// already-swung chest (its parent); recoil (shoulder-local) stays last.
pub(crate) const ENEMY_LOCO_LAYER: usize = 0;
pub(crate) const ENEMY_AIM_OVERLAY_LAYER: usize = 1;
pub(crate) const ENEMY_CHEST_AIM_LAYER: usize = 2;
pub(crate) const ENEMY_HEAD_LOOK_LAYER: usize = 3;
pub(crate) const ENEMY_RECOIL_LAYER: usize = 4;
/// Aim cone (radians) the chest may swing to point the barrel at the player — wide
/// enough for the clip bias (~45°) plus pitch, capped (~80°) so a target that's
/// swung behind the shoulder pins the torso at the edge instead of contorting it.
pub(crate) const ENEMY_CHEST_AIM_CONE: f32 = 1.4;
/// Gaze cone (radians, ~55°) the head may swing to look at what the hunter is
/// focused on. Deliberately TIGHTER than the chest cone: the 15-bone rig has no
/// separate neck, so a lone head joint over-rotated past ~60° reads as the head
/// detaching from a rigid neck. Anything beyond the cone is left to the body yaw
/// (which turns at [`TURN_RATE`]) to bring back into range.
pub(crate) const ENEMY_HEAD_LOOK_CONE: f32 = 0.95;
/// Ease rate (1/s) the smoothed head look POINT tracks toward the current focus,
/// so switching focus (player ↔ last-known ↔ search point) sweeps the gaze across
/// instead of snapping it. The look WEIGHT eases separately at [`AIM_RAMP`].
pub(crate) const HEAD_LOOK_TRACK: f32 = 8.0;
/// How far (m) ahead a blindly-hunting hunter's head-scan point sits along its scan
/// direction — far enough that the gaze reads as a direction (a level scan), not a
/// look at a spot on the floor. See [`crate::enemy::Enemy::head_scan_dir`].
pub(crate) const HEAD_SCAN_DIST: f32 = 6.0;
/// Foot IK: how fast (1/s) each foot's applied ground offset eases toward the freshly
/// sampled floor delta, so a foot crossing a stair edge glides onto the new step
/// instead of popping. Also smooths the derived pelvis drop.
pub(crate) const FOOT_IK_EASE: f32 = 10.0;
/// Foot IK: the most (m) the pelvis will drop to let a trailing foot reach a lower
/// step without a leg stretching bolt-straight. Caps the crouch on steep geometry.
pub(crate) const FOOT_IK_MAX_DROP: f32 = 0.45;
/// Foot IK: a floor sample this much (m) further below the root than this is treated as
/// "no step here" (a ledge / hole under the foot) and that foot's grounding is skipped,
/// so a hunter at a platform edge doesn't splay a leg into the void.
pub(crate) const FOOT_IK_MAX_REACH: f32 = 0.9;
/// Cadence (stride-warp) clamp: the locomotion phase-rate multiplier from the ratio of
/// actual ground speed to the gait speed is held within this band, so a briefly-stalled
/// or ORCA-shoved hunter neither freezes its feet nor sprints them. See
/// [`engine::skeletal::layers::LocomotionBlendLayer::stride_scale`].
pub(crate) const STRIDE_SCALE_MIN: f32 = 0.35;
pub(crate) const STRIDE_SCALE_MAX: f32 = 1.5;
/// Fallback barrel axis (gun-model space) for weapons whose muzzle offset is
/// degenerate — the real axis is the measured muzzle-flash centroid direction.
pub(crate) const BARREL_MODEL_AXIS: Vec3 = Vec3::NEG_Z;
/// How fast (1/s) a hunter's aim weight eases toward its target (0 ↔ 1), so the
/// upper body raises/lowers into the aim hold smoothly instead of snapping.
pub(crate) const AIM_RAMP: f32 = 9.0;
/// Low-pass rate (1/s) for the locomotion speed that drives band selection. The
/// AI's `speed()` is binary (0 or chase-speed) and toggles frame-to-frame when a
/// hunter micro-steps near a distance boundary (e.g. the attack standoff); band
/// selection off the RAW value thrashes idle↔jog, restarting the crossfade every
/// few frames (visible leg stutter). Easing it kills the flip.
pub(crate) const LOCO_SMOOTH: f32 = 6.0;
/// Max angular speed (rad/s, ~515°/s) the RENDERED hunter body rotates to face its AI
/// heading. The logic facing ([`crate::enemy::Enemy::heading`]) can snap instantly —
/// evade jukes, reposition weave flips, travel↔player facing swaps — which reads as a
/// per-frame spin / flicker; turning the model toward it at this bounded rate smooths
/// those into realistic turns. The chest-aim layer keeps the gun on the player while
/// the body catches up, so aim isn't laggy. See [`EnemyInstance::advance_facing`].
pub(crate) const TURN_RATE: f32 = 9.0;
/// Idle deadzone (m/s): a smoothed locomotion speed below this snaps to 0 so the
/// hunter settles to the IDLE band instead of the WALK band. Without it the
/// exponential decay lingers as a tiny positive value for ~15 s and
/// [`band_for_speed`] (walk when speed > 0) keeps the legs walking in place.
pub(crate) const LOCO_IDLE_EPS: f32 = 0.35;
/// Aim at the player this far (m) above their feet — roughly chest height. The
/// chest-aim points the gun barrel here, so hunters track the player's height.
pub(crate) const PLAYER_AIM_Y: f32 = 1.0;
/// Recoil kick (rad) per shot + its decay rate (1/s) and amplitude ceiling.
/// Applied to PISTOLS only — automatic weapons kick too fast to read well.
pub(crate) const ENEMY_RECOIL_KICK: f32 = 0.32;
pub(crate) const ENEMY_RECOIL_DECAY: f32 = 12.0;
pub(crate) const ENEMY_RECOIL_MAX: f32 = 0.5;
/// Tail (s) added after a fire burst's shot window before the burst ends — the
/// burst length now that firing is a timer, not a full-body animation clip.
pub(crate) const ENEMY_FIRE_TAIL: f32 = 0.25;

/// The resolved gun-arm chain + upper-body joint mask for the shared character
/// skeleton, computed once at load. Each hunter builds its own (stateful) aim +
/// recoil [`LayeredAnimator`] from this via [`Self::build_stack`].
#[derive(Clone)]
pub(crate) struct EnemyArm {
    /// Right (gun) shoulder — the recoil kick anchor + ANIM_DEBUG measurement.
    shoulder: usize,
    /// Right elbow + hand, kept for the ANIM_DEBUG arm measurement only.
    mid: usize,
    end: usize,
    /// Chest joint (`Bone_2`, the two hands' common ancestor + parent of both arms)
    /// — the joint the chest-aim correction rotates to point the barrel at the player.
    chest: usize,
    /// Head joint (`Bone_3`) — the joint the head look-at rotates toward the hunter's
    /// current focus (player / last-known / search point).
    head: usize,
    /// The head's gaze axis expressed in the head joint's LOCAL frame — the fixed
    /// anatomical "forward" the look-at layer swings toward the target. Derived from
    /// the bind pose (`head_global_rot⁻¹ · model_forward`), so it's frame-invariant:
    /// `head_global_rot · head_forward` recovers the world gaze at any runtime pose.
    head_forward: Vec3,
    /// Upper-body joint mask (chest + head + both arms) the authored aim clip
    /// overlays — the two hands' common-ancestor (`Bone_2`) subtree, so the
    /// locomotion legs stay untouched and the hunter can run while aiming.
    upper_body: Vec<usize>,
    /// Pelvis (root) joint — lowered by the foot-IK pelvis drop.
    pelvis: usize,
    /// The two leg IK chains `(hip, knee, foot)` — `[left, right]` — for ground-adaptive
    /// foot IK. Each foot's global origin is solved onto the floor beneath it.
    legs: [(usize, usize, usize); 2],
}

impl EnemyArm {
    /// Resolve the right-arm chain (`Bone_9`) + the upper-body mask. The `idle` clip
    /// is no longer needed to derive rest geometry (the authored aim clip supplies
    /// the pose), so it's ignored. `None` if a required bone is missing.
    fn resolve(model: &SkinnedModel, _idle: &clip::AnimationClip) -> Option<Self> {
        let sk = &model.skeleton;
        let end = sk.index_of(RIGHT_HAND_BONE)?;
        let mid = sk.parents[end]?;
        let shoulder = sk.parents[mid]?;
        // Upper body = the subtree of the two hands' lowest common ancestor (the
        // chest): chest + head + both arms, excluding the pelvis + legs.
        let left_hand = sk.index_of(LEFT_HAND_BONE)?;
        let chest = sk.lowest_common_ancestor(&[end, left_hand])?;
        let upper_body = sk.subtree(chest);
        // Head gaze axis in the head's local frame, from the bind pose. The model
        // faces +Z at rest (see `char_transform_raw`), so world gaze = +Z there;
        // expressing that in the head's local frame gives a pose-invariant forward.
        let head = sk.index_of(HEAD_BONE)?;
        let bind_globals = Pose::bind(sk).joint_global_transforms(sk);
        let head_rot = bind_globals[head].to_scale_rotation_translation().1;
        let head_forward = (head_rot.inverse() * Vec3::Z).normalize_or_zero();
        // Leg IK chains: walk two parents up from each foot (foot←knee←hip).
        let pelvis = sk.index_of(PELVIS_BONE)?;
        let leg_chain = |foot_bone: &str| -> Option<(usize, usize, usize)> {
            let foot = sk.index_of(foot_bone)?;
            let knee = sk.parents[foot]?;
            let hip = sk.parents[knee]?;
            Some((hip, knee, foot))
        };
        let legs = [leg_chain(LEFT_FOOT_BONE)?, leg_chain(RIGHT_FOOT_BONE)?];
        Some(EnemyArm { shoulder, mid, end, chest, head, head_forward, upper_body, pelvis, legs })
    }

    /// Right shoulder joint index (the ANIM_DEBUG arm measurement anchor).
    pub(crate) fn shoulder(&self) -> usize {
        self.shoulder
    }

    /// A fresh per-hunter stack: continuous locomotion base (drives the legs), the
    /// authored **upper-body aim overlay** (`aim_clip` — the weapon class's
    /// fire/aim pose, which holds the gun correctly in both hands), a **chest-aim**
    /// that rotates the whole hold so the real gun barrel points at the player
    /// (`spawn_wave` sets its per-weapon `forward`; `advance_animation` drives its
    /// target + weight), a **head look-at** that turns the head toward whatever the
    /// hunter is focused on, then recoil on the right shoulder. The overlay / chest-aim
    /// / head-look weights are all eased 0↔1 in `advance_animation`. The head-look's
    /// `forward` (the gaze axis) is baked from the rig, so it's live here; `spawn_wave`
    /// just flips its `enabled` on (kept off in headless callers that don't drive it).
    fn build_stack(
        &self,
        loco_clips: Vec<(f32, clip::AnimationClip)>,
        aim_clip: clip::AnimationClip,
    ) -> LayeredAnimator {
        let mut s = LayeredAnimator::new();
        s.push(Box::new(LocomotionBlendLayer::new(loco_clips)));
        s.push(Box::new(ClipOverlayLayer::new(aim_clip, self.upper_body.clone())));
        s.push(Box::new(AimOffsetLayer {
            joint: self.chest,
            forward: Vec3::Z,       // set per-weapon in `spawn_wave` (real barrel)
            target: Vec3::Z,        // set to the player each frame in `advance_animation`
            max_angle: ENEMY_CHEST_AIM_CONE,
            weight: 0.0,
            enabled: false,         // enabled once its `forward` is measured
        }));
        s.push(Box::new(AimOffsetLayer {
            joint: self.head,
            forward: self.head_forward, // baked gaze axis (head-local)
            target: Vec3::Z,            // set to the focus point each frame in `advance_animation`
            max_angle: ENEMY_HEAD_LOOK_CONE,
            weight: 0.0,
            enabled: false,             // `spawn_wave` enables it (off in headless callers)
        }));
        s.push(Box::new(AdditiveDecayLayer::new(
            self.shoulder,
            Vec3::X,
            ENEMY_RECOIL_DECAY,
            ENEMY_RECOIL_MAX,
        )));
        s
    }

    /// The **real gun barrel** direction expressed in the **chest's local frame**,
    /// measured at the overlay aim pose `aim_pose`. The gun rides the right-hand bone
    /// (`Bone_9`) with attach rotation `attach_rot`, and points along `barrel_gun`
    /// (its muzzle axis in gun-model space). Feeding this as the chest-aim layer's
    /// `forward` makes that layer swing the chest until the barrel points at its
    /// target (the player), regardless of the clip's authored bias. `None` if
    /// degenerate.
    fn barrel_forward_in_chest(
        &self,
        aim_pose: &Pose,
        sk: &engine::skeletal::Skeleton,
        attach_rot: Quat,
        barrel_gun: Vec3,
    ) -> Option<Vec3> {
        let g = aim_pose.joint_global_transforms(sk);
        let hand_rot = g[self.end].to_scale_rotation_translation().1;
        let barrel_model = (hand_rot * (attach_rot * barrel_gun)).normalize_or_zero();
        if barrel_model == Vec3::ZERO {
            return None;
        }
        let chest_rot = g[self.chest].to_scale_rotation_translation().1;
        Some((chest_rot.inverse() * barrel_model).normalize_or_zero())
    }
}

/// Clip indices within the character's [`AnimPlayer`], set by the fixed load order
/// in `World::new`: `0–3` locomotion, then one fire clip per weapon class
/// (rifle/pistol/dual), then the hit set, then the death set. The class-specific
/// fire clip + its FIRE_TIMING window are selected via [`hunt::fire_clip_index`] /
/// [`hunt::fire_window_for`]; [`hunt::is_fire_clip`] recognises all three.
pub(crate) const FIRE_RIFLE_IDX: usize = 4; // 01-fire-standing
pub(crate) const FIRE_PISTOL_IDX: usize = 5; // 41-fire-standing-pistol
pub(crate) const FIRE_DUAL_IDX: usize = 6; // 7A-fire-standing-dual-wield
pub(crate) const CHAR_HIT_START: usize = 7;

// ─── Track A — killable hunter ──────────────────────────────────────────────
/// The hunter's capsule collider dimensions in metres — the recon constants
/// (`0.3` / `0.6`) scaled to ~80% to match the shrunk model, so shots still land
/// on the smaller body. Total height ≈ 1.44 m.
pub(crate) const ENEMY_RADIUS: f32 = 0.24; // 0.3 × 0.8
pub(crate) const ENEMY_HALF_HEIGHT: f32 = 0.48; // 0.6 × 0.8
/// Body half-width (m) the movement-time wall-clearance nudge keeps between a hunter's
/// centre and wall geometry, so the wider-than-the-nav-line character model stops
/// clipping into walls. A touch under [`ENEMY_RADIUS`] and, crucially, well under half
/// the narrowest doorway (~0.5 m = 2 WT cells) — and the nudge only ever *pushes*
/// (never blocks), so a doorway narrower than `2×` this still passes (centred). See
/// [`engine::sim::nav::NavWorld::wall_clearance_offset`].
pub(crate) const WALL_CLEARANCE_RADIUS: f32 = 0.2;
/// Death fade duration (s) — JS `EnemyCharacter.FADE_DURATION`. The body fades
/// its opacity 1→0 over this window after the lethal shot, then vanishes.
pub(crate) const FADE_DURATION: f32 = 2.0;
/// Ragdoll knockback impulse magnitude (N·s) applied to the body nearest the impact
/// on death. Bullets shove modestly (a jerk + spin); a blast flings harder. Tuned
/// against the ~flesh-density limb masses in [`engine::sim::ragdoll`]. Eyeball in
/// playtest — bump for a more violent throw, drop for a limper crumple.
pub(crate) const RAGDOLL_BULLET_IMPULSE: f32 = 26.0;
pub(crate) const RAGDOLL_BLAST_IMPULSE: f32 = 60.0;
/// A ragdoll whose fastest body is slower than this (m/s) counts as settled → the
/// corpse begins its fade. Small but non-zero so micro-jitter doesn't hold it open.
pub(crate) const RAGDOLL_SETTLE_SPEED: f32 = 0.35;
/// Hard cap (s) on how long a ragdoll simulates before the fade starts regardless of
/// motion — a backstop so a corpse teetering on an edge can't tumble forever.
pub(crate) const RAGDOLL_MAX_SETTLE: f32 = 6.0;

/// Living-hit stagger (Phase 3, the partial active ragdoll). A non-lethal hit spawns a
/// brief physics ragdoll blended INTO the running animation by a weight that peaks at
/// [`REACTION_PEAK_WEIGHT`] and decays at [`REACTION_DECAY`] (1/τ) back to zero — a real
/// physical reaction that cleanly returns to fighting. Torn down once the weight drops
/// below [`REACTION_MIN_WEIGHT`]. Flat across all difficulties (the `ragdoll` flag is the
/// only kill-switch); a re-hit while staggering re-kicks + restarts the decay.
pub(crate) const REACTION_PEAK_WEIGHT: f32 = 0.6;
pub(crate) const REACTION_DECAY: f32 = 7.0;
pub(crate) const REACTION_MIN_WEIGHT: f32 = 0.03;
/// Brief AI hitch (s) on a non-lethal hit — the hunter staggers, then resumes fighting.
/// Kept short and covers most of the blend window so the (stunned → stationary) hunter's
/// root stays put while the physical reaction plays out.
pub(crate) const REACTION_STUN: f32 = 0.35;
/// Living-hit knockback impulse (N·s) — lighter than a kill (a flinch, not a throw).
pub(crate) const REACTION_IMPULSE: f32 = 14.0;
/// Number of enemy pain vocalisations (`sounds/enemies/pain-1..26.wav`).
pub(crate) const PAIN_COUNT: usize = 26;
/// On-hit SFX volumes (JS `EnemyCharacter.onHit`): the pain vocal + the flesh
/// bullet-hit, linear amplitude.
pub(crate) const PAIN_VOL: f32 = 0.8;
pub(crate) const BULLET_HIT_VOL: f32 = 0.5;

/// Blood/damage painting (JS `EnemyCharacter.paintDamage`): vertices within
/// `BLOOD_RADIUS` (world metres) of a shot's impact get reddened, accumulating so
/// repeated hits build up persistent blood. The JS radius is 300 GE-units in the
/// model's local space; here we compare in world space, so it's scaled by
/// `CHAR_SCALE` (≈0.25 m). `BLOOD_INTENSITY` is the peak per-hit strength at the
/// centre (JS `intensity`), falling off linearly to the rim.
pub(crate) const BLOOD_RADIUS: f32 = 300.0 * CHAR_SCALE;
pub(crate) const BLOOD_INTENSITY: f32 = 0.5;

/// Zone hitscan (damage + hurt animation vary by where the shot lands). Boundaries
/// are impact height above the hunter's feet, in metres, for the ~1.44 m capsule
/// (feet 0 → head ~1.44). Multipliers mirror the JS `ZONE_DAMAGE_MULTIPLIER`
/// (head 4.0, torso 1.0, legs 0.6; arms are folded into torso since a height-only
/// classifier can't separate them).
pub(crate) const ZONE_HEAD_MIN: f32 = 1.1; // ≥ this above the feet → head
pub(crate) const ZONE_LEG_MAX: f32 = 0.55; // < this above the feet → legs
pub(crate) const ZONE_HEAD_MULT: f32 = 4.0;
pub(crate) const ZONE_TORSO_MULT: f32 = 1.0;
pub(crate) const ZONE_LEG_MULT: f32 = 0.6;

// ─── Enemies fire back (A3) — data-driven arsenal + probabilistic hit ────────
// Per-weapon damage / accuracy / range / fire-rate now live on the equipped
// [`EnemyWeaponDef`] (see `combat::enemy_weapons`); only the shared feedback
// timings stay here.
/// The muzzle-flash countdown (s) after each enemy shot; >0 → the enemy muzzle
/// renders.
pub(crate) const ENEMY_MUZZLE_TIME: f32 = 0.1;
/// Enemy gun-report volume (linear amplitude).
pub(crate) const ENEMY_FIRE_VOL: f32 = 0.7;

/// Cap on how often an enemy shot may actually DAMAGE the player (hits/second),
/// independent of the weapon's visual fire rate. Stops a full-auto spray from being a
/// one-hit kill: the gun still muzzle-flashes at its own cadence, but at most this many
/// of those shots per second can land. A pistol (2/s) is already under it; only fast
/// autos get throttled. Max sustained enemy DPS = `MAX_HIT_RATE * ENEMY_DAMAGE`.
/// Difficulty scales accuracy / speed / evasion — NOT this ceiling.
pub(crate) const MAX_HIT_RATE: f32 = 4.0;

/// Reactive-evasion aim sense: a hunter counts as "aimed at" when the player's
/// crosshair line is within this cosine of the direction to its chest (≈12° half-
/// angle) AND the line to it is unobstructed. Feeds the FSM's aim-dodge (see
/// [`crate::enemy::Enemy::update`]'s `aimed_at`). The omniscience the user OK'd — the
/// hunter "feels" the bead being drawn on it and jukes off the line.
pub(crate) const AIM_SENSE_COS: f32 = 0.978;
/// Height (m above feet) the aim sense targets — the torso, matching where the player
/// naturally lines a shot up.
pub(crate) const AIM_SENSE_CHEST_Y: f32 = 1.0;

/// The hunter roster spawned at G→HUNT: `(weapon, dual-wield?)`, one hunter per
/// entry (capped by available standable cells). Covers every animation class —
/// two-handed rifle, one-handed pistol, dual rifle (the canonical akimbo weapon),
/// and dual pistols — so all the fire animations are exercised in one hunt. Any of
/// the 19 arsenal weapons can be listed here; each is classified + attached by
/// [`crate::combat::enemy_def_for`].
pub(crate) const ENEMY_ROSTER: &[(crate::combat::config::WeaponStats, bool)] = &[
    (crate::combat::config::KF7, false),     // two-handed rifle
    (crate::combat::config::PP7, false),     // one-handed pistol
    (crate::combat::config::RCP90, true),    // dual-wield rifle (akimbo)
    (crate::combat::config::PP7, true),      // dual-wield pistols
    (crate::combat::config::AR33, false),    // two-handed rifle
    (crate::combat::config::SHOTGUN, false), // two-handed
];

/// How many hunters flood in at the spawn point on G→HUNT. Weapons are drawn from
/// [`ENEMY_ROSTER`] (cycling if this exceeds the roster length), so this is the
/// single knob for "how big is the wave" — bump it and the rest follows.
///
/// **Default set to 1 (2026-07-27) — "duel mode":** while tuning the difficulty dial
/// we spawn a single hunter so its beatability can be judged in isolation. This is the
/// initial value of the runtime [`World`] `wave_size` field; bump the field (or this
/// default) back up once the per-enemy difficulty curve is dialed in.
pub(crate) const ENEMY_COUNT: usize = 1;

/// The wave size the **app** pins at boot (the code default [`ENEMY_COUNT`] stays at
/// 1 so the duel-mode tests are unaffected). A small pack so the coordinated AI —
/// flanking approach angles, squad suppression, cover — actually reads in playtest;
/// bump/lower freely while tuning. Set in `app.rs` right after the difficulty pin.
pub const PLAYTEST_WAVE_SIZE: usize = 4;

/// Top of the difficulty dial ([`World::difficulty`] runs `0..=DIFFICULTY_MAX`). 0 is
/// the original baseline; DIFFICULTY_MAX is brutal. Driven live with the `=` / `-`
/// keys (see `app.rs`), read into [`DiffParams`] each frame.
pub(crate) const DIFFICULTY_MAX: u32 = 10;

/// Difficulty-derived tuning for the current [`World::difficulty`], recomputed on read
/// (cheap). One dial ramps three axes at once: **lethality** (accuracy + fire cadence
/// + reaction), **survivability** (health), and **evasion** (dodge). All multipliers
/// are 1.0 / neutral at level 0 (original behaviour) and ramp linearly to level
/// [`DIFFICULTY_MAX`]. Damage per hit is NOT scaled, and neither is fire rate — the
/// landed-hit rate is capped ([`MAX_HIT_RATE`]) so difficulty comes from being more
/// accurate, faster, and more evasive, not from a one-shot spray.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DiffParams {
    /// Multiplies the weapon's base accuracy (hit chance is still capped at 1.0).
    pub accuracy_mult: f32,
    /// Eases out the distance accuracy falloff: 0 = full falloff (baseline), 1 = the
    /// hunter is as accurate at range as point-blank.
    pub falloff_ease: f32,
    /// Multiplies enemy movement speed (chase / attack-advance / reposition), so a
    /// high-difficulty hunter closes + repositions faster and is harder to escape.
    pub speed_mult: f32,
    /// Scales the between-bursts cooldown DOWN (→ more relentless fire).
    pub cooldown_mult: f32,
    /// Scales the reaction delay DOWN (→ engages faster on sight).
    pub reaction_mult: f32,
    /// Multiplies enemy spawn health (→ harder to put down).
    pub health_mult: f32,
    /// Lateral-evasion intensity 0..1 handed to the FSM (see [`crate::enemy::AiTuning`]).
    pub dodge: f32,
    /// Perception-range multiplier (1.0 baseline → wider): a harder hunter notices +
    /// tracks the player from further out. Threaded to the FSM as [`AiTuning::sense`].
    pub sense_mult: f32,
    /// Suppressing-fire aggression 0..1 (0 baseline): a harder hunter opens fire while
    /// still closing. Threaded to the FSM as [`AiTuning::suppress`].
    pub suppress: f32,
    /// Flanking intensity 0..1 (0 baseline): a harder hunter curves its approach onto
    /// an offset bearing. Threaded to the FSM as [`AiTuning::flank`].
    pub flank: f32,
    /// Cover-usage intensity 0..1 (0 baseline): a harder hunter breaks LOS to cover +
    /// peek-fires between bursts. Threaded to the FSM as [`AiTuning::cover`].
    pub cover: f32,
}

/// The character-body catalog: every skinned body a hunter can wear, as
/// `(display name, GLB filename under assets/enemies/characters)`. The Vec index is
/// the **body id** stored on each [`EnemyInstance`]. All 44 GoldenEye characters ride
/// the same 15-bone rig (verified: `Bone_1..Bone_15`, identical node order), so every
/// clip + the aim overlay retargets onto any body — only the mesh geometry and the
/// skeleton bind pose (bone lengths) differ per body, both handled per-body at load
/// (see [`World::char_models`]). `russian-guard_karl` stays index 0: tests + the BUILD
/// procedural preview default to body 0, and it's the original single-guard asset.
pub(crate) const BODY_CATALOG: &[(&str, &str)] = &[
    ("russian-guard_karl", "russian-guard_karl.glb"),
    ("russian-guard_alan", "russian-guard_alan.glb"),
    ("russian-guard_joe", "russian-guard_joe.glb"),
    ("russian-guard_mark", "russian-guard_mark.glb"),
    ("russian-guard_martin", "russian-guard_martin.glb"),
    ("russian-guard_pete", "russian-guard_pete.glb"),
    // russian-infantry (all 6 head variants) DISABLED 2026-07-26: their source GLBs
    // ship with the waist mesh missing entirely (torso + hips are two disconnected
    // segments — confirmed in Blender). It's an upstream extraction defect baked into
    // the asset (3DS FPS copied these verbatim from the GoldenEye JS assets; no
    // conversion step in any repo drops it), not our loader. Re-enable once the GLBs
    // are patched. Other bodies may have the same flaw — flag + add here as found.
    // ("russian-infantry_alan", "russian-infantry_alan.glb"),
    // ("russian-infantry_joe", "russian-infantry_joe.glb"),
    // ("russian-infantry_karl", "russian-infantry_karl.glb"),
    // ("russian-infantry_mark", "russian-infantry_mark.glb"),
    // ("russian-infantry_martin", "russian-infantry_martin.glb"),
    // ("russian-infantry_pete", "russian-infantry_pete.glb"),
    ("blue-guard_alan", "blue-guard_alan.glb"),
    ("blue-guard_joe", "blue-guard_joe.glb"),
    ("blue-guard_karl", "blue-guard_karl.glb"),
    ("blue-guard_mark", "blue-guard_mark.glb"),
    ("blue-guard_martin", "blue-guard_martin.glb"),
    ("blue-guard_pete", "blue-guard_pete.glb"),
    ("janus-marine_alan", "janus-marine_alan.glb"),
    ("janus-marine_joe", "janus-marine_joe.glb"),
    ("janus-marine_karl", "janus-marine_karl.glb"),
    ("janus-marine_mark", "janus-marine_mark.glb"),
    ("janus-marine_martin", "janus-marine_martin.glb"),
    ("janus-marine_pete", "janus-marine_pete.glb"),
    ("janus-special-forces_alan", "janus-special-forces_alan.glb"),
    ("janus-special-forces_joe", "janus-special-forces_joe.glb"),
    ("janus-special-forces_karl", "janus-special-forces_karl.glb"),
    ("janus-special-forces_mark", "janus-special-forces_mark.glb"),
    ("janus-special-forces_martin", "janus-special-forces_martin.glb"),
    ("janus-special-forces_pete", "janus-special-forces_pete.glb"),
    ("jungle-commando_alan", "jungle-commando_alan.glb"),
    ("jungle-commando_joe", "jungle-commando_joe.glb"),
    ("jungle-commando_karl", "jungle-commando_karl.glb"),
    ("jungle-commando_mark", "jungle-commando_mark.glb"),
    ("jungle-commando_martin", "jungle-commando_martin.glb"),
    ("jungle-commando_pete", "jungle-commando_pete.glb"),
    ("baron-samedi", "baron-samedi.glb"),
    ("boris", "boris.glb"),
    // jaws DISABLED 2026-07-26: the skinned character GLB ships with NO materials/
    // textures (it has UVs but the material+images were dropped on export), so he
    // renders solid white. Upstream export defect — same in 3DS FPS; the ONLY textured
    // Jaws there is a non-skinned Aztec prop (objects/aztec_jaws.glb), so restoring the
    // skin needs asset surgery. Re-enable once the character GLB is re-exported textured.
    // ("jaws", "jaws.glb"),
    ("mayday", "mayday.glb"),
    ("natalya", "natalya.glb"),
    ("ourumov", "ourumov.glb"),
    ("trevelyan", "trevelyan.glb"),
    ("valentin", "valentin.glb"),
    ("xenia", "xenia.glb"),
];

// ─── Enemy spawn point (a FIXED world marker) ────────────────────────────────
/// The hunters always flood in at this fixed world-space point (metres) — a
/// consistent location the builder authors around, **not** derived from where the
/// player happens to be at G. Marked on the floor by a colored square
/// ([`World::spawn_marker_mesh`]) visible in both BUILD and HUNT. Defaults to the
/// centre of the starting room; a placement tool can make it authorable later.
pub(crate) const SPAWN_MARKER_POS: Vec3 = Vec3::new(3.0, 0.0, 3.0);
/// Half-extent (m) of the floor marker square, and its flat colour (a bright
/// red so it clearly reads as the enemy ingress).
const SPAWN_MARKER_HALF: f32 = 0.6;
const SPAWN_MARKER_COLOR: [f32; 3] = [0.95, 0.12, 0.12];
/// Radius (m) of the ring the wave clusters into around the spawn point, so the
/// hunters don't all stack on one cell.
const SPAWN_CLUSTER_RADIUS: f32 = 0.7;

/// Enemies keep this much personal space (m): pairs closer than this are nudged
/// apart each step so they don't merge into one body / march in unison. ~3× the
/// capsule radius, so they ring around a target instead of stacking on it.
pub(crate) const ENEMY_SEPARATION_DIST: f32 = 0.7;

// ─── Local avoidance (ORCA — replaces the position-nudge separation) ─────────
/// The player's disc radius (m) hunters treat as a non-reciprocal ORCA obstacle, so
/// they steer around the player instead of piling onto its exact cell (hunters still
/// don't physically collide with the player — the Attack standoff/back-off owns
/// engagement spacing; this just keeps the crowd from converging *through* you).
pub(crate) const PLAYER_AVOID_RADIUS: f32 = 0.3;
/// Speed (m/s) a hunter with no movement intent of its own may still use to yield
/// ground to a packmate under ORCA — so a holding / attacking hunter shuffles aside
/// to let one pass instead of being interpenetrated, without wandering off station.
pub(crate) const AVOID_YIELD_SPEED: f32 = 1.2;
/// Local-avoidance look-ahead horizons (s): how far ahead a predicted collision must
/// be before a hunter starts steering around it. Tuned for tight indoor quarters +
/// our small (~0.24 m) agent radius — react early enough to weave, not so early a
/// whole room over-avoids.
pub(crate) const AVOID_HORIZONS: avoidance::Horizons =
    avoidance::Horizons { agents: 1.5, obstacles: 1.0 };
/// A non-engaged hunter within this range (m) of an engaged packmate adopts that
/// packmate's player fix — the squad "contact!" call, so the whole pack converges
/// once anyone spots the player instead of some wandering off on their search.
pub(crate) const SQUAD_ALERT_RANGE: f32 = 12.0;

/// Size of the fan-out search-point pool the `World` hands out during the hunt
/// (spread standable cells). More points than hunters keeps the sweep varied.
const SEARCH_POINT_COUNT: usize = 12;
/// How far (m) the player's gunfire carries as a noise ping that pulls nearby
/// searching/investigating hunters toward the sound. Comfortably past the 12 m
/// sight range so shooting while hidden genuinely gives you away.
const GUNSHOT_HEARING_RANGE: f32 = 25.0;

/// Footstep (movement) noise — the difficulty-scaled "sharper hearing" lever (#6).
/// A moving player emits a much quieter ping than gunfire that pulls only *blind*
/// (searching/investigating) hunters toward the sound. Below [`MOVE_NOISE_MIN_SPEED`]
/// m/s you're effectively sneaking and make none; at a full run the audible radius
/// reaches [`MOVE_NOISE_RANGE_MAX`] m — but scaled by the difficulty dial, so at
/// level 0 the range is 0 (a baseline hunter is deaf to footsteps: vision-only).
const MOVE_NOISE_MIN_SPEED: f32 = 2.5;
const MOVE_NOISE_RANGE_MAX: f32 = 10.0;
/// Player run speed (m/s) used to normalise footstep loudness (matches the
/// character controller's `WALK_SPEED`); faster movement carries further.
const PLAYER_RUN_SPEED: f32 = 6.4;

// ─── Grenade flush (#5, difficulty-scaled) ──────────────────────────────────
/// The player counts as "camping" while they stay within this radius (m) of the
/// tracked anchor; stepping outside it resets the anchor + dwell timer.
const CAMP_RADIUS: f32 = 2.5;
/// Camp dwell (s) before a hunter lobs a grenade to flush the player, lerped by the
/// difficulty dial: a patient wait at low difficulty, a fast flush at high. The whole
/// behaviour is **off at difficulty 0** (no grenades ever).
const CAMP_DWELL_MAX: f32 = 5.0;
const CAMP_DWELL_MIN: f32 = 2.0;
/// A hunter must be within this range (m) of the camp spot to have a plausible throw.
const GRENADE_THROW_RANGE: f32 = 16.0;
/// No grenade is lobbed if ANY living hunter is within this distance (m) of the camp
/// spot — comfortably outside the ~4 m blast radius, so a lob never catches the
/// thrower or a packmate. This also scopes the flush to its intent: grenades are for a
/// camper the pack is *held away from* (behind cover / at range); when a hunter is
/// close it just shoots instead.
const GRENADE_SAFE_DIST: f32 = 6.5;
/// Height (m above feet) a hunter releases the grenade from (chest/overhand).
const GRENADE_THROW_Y: f32 = 1.2;
/// Squad-wide cooldown (s) between grenade lobs.
const GRENADE_COOLDOWN: f32 = 4.0;
/// Clamp on the ballistic launch speed (m/s) of a lobbed grenade, so a very short or
/// very long throw still arcs sensibly.
const GRENADE_LOB_MIN: f32 = 5.0;
const GRENADE_LOB_MAX: f32 = 16.0;
/// The enemy grenade-throw SFX (reuses the hand-grenade toss sound).
pub(crate) const GRENADE_THROW_SOUND: &str = "sounds/weapons/throw.wav";
const GRENADE_THROW_VOL: f32 = 0.7;

/// Load a weapon's `(gun, muzzle-flash)` CPU meshes from its config, resolving the
/// asset-relative paths under `native/assets/weapons/`. Warn-not-panic: a failed
/// load (or a weapon with no muzzle, like the sniper — `muzzle_path == ""`) yields
/// `None` for that slot, and the renderer simply hides whatever is missing. Used at
/// startup for the initial weapon and on every `Q`/`A` weapon switch.
fn load_weapon_models(cfg: &crate::combat::config::WeaponStats) -> (Option<TexturedModel>, Option<TexturedModel>) {
    let asset = |rel: &str| format!("{}/../../assets/weapons/{}", env!("CARGO_MANIFEST_DIR"), rel);
    let gun = match crate::combat::load_gun(&asset(cfg.gun_path)) {
        Ok(m) => {
            log::info!(
                "loaded weapon {}: {} verts, {} primitives",
                cfg.name,
                m.vertices.len(),
                m.primitives.len()
            );
            Some(m)
        }
        Err(e) => {
            log::warn!("weapon '{}' gun load failed: {e}", cfg.name);
            None
        }
    };
    // `load_flash` keeps only the additive flash billboards — the GoldenEye
    // muzzle.glb is the whole firing pose (gun body + hand + flash), so drawing all
    // of it flashed a hand into view.
    let muzzle = if cfg.muzzle_path.is_empty() {
        None
    } else {
        match crate::combat::load_flash(&asset(cfg.muzzle_path)) {
            Ok(m) => Some(m),
            Err(e) => {
                log::warn!("weapon '{}' muzzle-flash load failed: {e}", cfg.name);
                None
            }
        }
    };
    (gun, muzzle)
}

// ─── Player health + damage feedback (P5) ───────────────────────────────────
pub(crate) const PLAYER_MAX_HEALTH: f32 = 100.0;
pub(crate) const PLAYER_MAX_ARMOR: f32 = 100.0;
/// Red damage-flash decay (JS `HealthHUD`: alpha −= dt·2.5), and the flash's peak
/// alpha per hit = min(0.5, dmg/40).
pub(crate) const DAMAGE_FLASH_DECAY: f32 = 2.5;
/// Health-HUD pop duration on damage + its fade tail (JS `showTimer = 1.5`,
/// `FADE_DURATION = 0.5`).
pub(crate) const HUD_SHOW_TIME: f32 = 1.5;
pub(crate) const HUD_FADE_TAIL: f32 = 0.5;
pub(crate) const PLAYER_HIT_SOUND: &str = "sounds/player/breathe.wav";
pub(crate) const PLAYER_HIT_VOL: f32 = 0.7;

/// Door opening size in WT (JS `DOOR_WIDTH` / `DOOR_HEIGHT`): 3 × 7 = 0.75 × 1.75 m.
const DOOR_WIDTH: f32 = 3.0;
const DOOR_HEIGHT: f32 = 7.0;

/// Default hole size in WT (JS `HOLE_WIDTH` / `HOLE_HEIGHT`), scroll-adjustable.
const HOLE_WIDTH: f32 = 3.0;
const HOLE_HEIGHT: f32 = 3.0;

/// Pillar/brace sizing bounds in WT (JS `MIN/MAX_PILLAR_SIZE`, `MIN/MAX_BRACE_DIM`).
const PILLAR_SIZE: f32 = 2.0;
const PILLAR_MIN: f32 = 1.0;
const PILLAR_MAX: f32 = 8.0;
const BRACE_DIM: f32 = 2.0;
const BRACE_MIN: f32 = 1.0;
const BRACE_MAX: f32 = 8.0;

/// Burial epsilon in WT: additive decorations (pillars/braces) sink ½ WT into the
/// surrounding solid on their hidden faces, so the CSG doesn't emit stray coplanar
/// triangles at the seam (JS `E = WALL_THICKNESS / 2`).
const BURY_EPS: f32 = WALL_THICKNESS / 2.0;

/// Seconds of sustained breaching to break a door (JS `door.js` `DOOR_HP`).
/// Unused while breakable doors stay disabled; kept for re-enable.
#[allow(dead_code)]
const DOOR_HP: f32 = 2.5;

/// Reserved renderer/physics id for the combined free-standing structures mesh
/// (all platforms + stair-runs). CSG region ids count up from 0, so `u32::MAX`
/// never collides — the structures live in the same mesh + trimesh-collider
/// slots as regions, reusing the checkerboard shader and the walk-on-it physics
/// path for free (they're free-standing, so they can't fold into a region mesh).
const STRUCT_ID: u32 = u32::MAX;

/// Platform/stair-run defaults in WT (JS `DEFAULT_PLATFORM_*` / `DEFAULT_STAIR_*`).
const PLATFORM_SIZE: f32 = 4.0;
const PLATFORM_THICKNESS: f32 = 1.0;
const PLATFORM_SIZE_MIN: f32 = 1.0;
const PLATFORM_SIZE_MAX: f32 = 20.0;
const STAIR_WIDTH: f32 = 4.0;
const STAIR_STEP_HEIGHT: f32 = 1.0;
const STAIR_RISE_OVER_RUN: f32 = 1.0;

/// Platform gizmo dimensions in WT (JS `GIZMO_*`). Arrows are drawn as thin
/// elongated boxes (no cone tip); scale handles are cubes at the edge midpoints.
const GIZMO_ARROW_LENGTH: f32 = 3.0;
const GIZMO_SHAFT_HALF: f32 = 0.12; // GIZMO_SHAFT_RADIUS
const GIZMO_HANDLE_SIZE: f32 = 0.4;
/// Screen-drag → WT sensitivity, scaled by camera distance (JS `GIZMO_DRAG_SENSITIVITY`).
const GIZMO_DRAG_SENSITIVITY: f32 = 0.008;

/// The two game phases (DESIGN.md): author geometry, then walk it as the player.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Fly-cam authoring (CSG editing enabled).
    Build,
    /// Grounded first-person capsule (geometry frozen).
    Hunt,
}

/// A region's freshly-evaluated **textured** mesh, classified into per-(scheme,
/// zone) draw groups (scheme is per-triangle via the owning brush), returned to
/// the app for GPU upload. The collider is rebuilt inside
/// [`World::rebuild_region`] from the plain CSG mesh — this carries render data only.
pub struct RegionMesh {
    pub id: u32,
    pub mesh: TexturedMesh,
}

/// The currently-selected brush face (what push/pull acts on, and what the
/// highlight overlay draws). Mirrors JS `state.csg.selectedFace`.
#[derive(Clone, Copy)]
pub(crate) struct Selection {
    region_id: u32,
    brush_id: u32,
    axis: Axis,
    side: Side,
}

/// A breakable door, live only during the HUNT (JS `door.js`). The panel is a
/// standalone cuboid collider that blocks the player; the nav overlay adds a
/// cost the hunter reads live. Breaching drains `hp`, then removes the collider
/// and flips the nav flag — **no re-voxelization, no CSG re-eval** (the thesis).
/// `aabb` is the doorframe carve in WT (min corner + dims), used to draw the panel.
pub(crate) struct Door {
    aabb: Brush,
    hp: f32,
    broken: bool,
    /// The panel collider's index in [`PhysicsWorld`], removed on breach.
    panel: usize,
}

/// A live hit spark (Player Combat P2): a bright marker at a shot's impact point,
/// nudged just off the surface, that fades out after [`SPARK_TTL`] seconds. Purely
/// visual feedback that a shot registered at the right spot.
#[derive(Clone, Copy)]
pub(crate) struct Spark {
    pos: Vec3,
    ttl: f32,
}

/// How long a hit spark lives (s) and its half-extent (metres). Small + brief:
/// enough to see where the shot landed, not a persistent decal.
const SPARK_TTL: f32 = 0.12;
const SPARK_HALF: f32 = 0.02;

/// One puff of a layered explosion. GoldenEye builds its big fireball from several
/// overlapping fireball sprites at slight offsets with staggered start times, so a
/// detonation spawns a cluster of these: a central core plus satellites. Each puff
/// plays the fireball atlas once over its own `life` (after its `delay`), additively
/// — many overlapping puffs read as one big, dense, roiling fireball that blooms and
/// lingers. Purely cosmetic; the blast DAMAGE is applied once at detonation (see
/// `world::combat::detonate`).
#[derive(Clone, Copy)]
pub(crate) struct Blast {
    /// This puff's centre (detonation centre + a small random offset).
    pos: Vec3,
    /// Seconds since the puff was spawned (counts up).
    age: f32,
    /// Seconds before this puff starts animating (staggered starts).
    delay: f32,
    /// This puff's animation duration (s).
    life: f32,
    /// World half-extent at animation scale 1 (already radius-scaled).
    half: f32,
    /// Line-of-sight visibility (0 or 1) from the camera, refreshed each frame: a
    /// puff occluded by a wall is hidden (so explosions don't glow through walls),
    /// while visible puffs still composite on top with no billboard slicing. The
    /// cluster of puffs gives a soft occlusion edge for free (some drop, some stay).
    vis: f32,
}

/// Per-puff fireball animation duration (s). With staggered starts up to
/// [`BLAST_STAGGER`], the whole explosion lasts ~`BLAST_TTL + BLAST_STAGGER` —
/// longer + denser than a single sprite (user call 2026-07-19).
const BLAST_TTL: f32 = 0.6;
/// Max start-delay spread across a blast's puffs (s) — staggered so the fireball
/// blooms and lingers instead of popping all at once.
const BLAST_STAGGER: f32 = 0.28;
/// Puff-centre offset spread, as a fraction of the blast radius.
const BLAST_SPREAD_FRAC: f32 = 0.3;
/// Puff-count bounds for a blast (scaled by radius between them).
const BLAST_PUFFS_MIN: usize = 3;
const BLAST_PUFFS_MAX: usize = 6;
/// Number of frames in the fireball atlas (horizontal strip).
const BLAST_FRAMES: usize = 8;
/// Billboard quad half-extent as a fraction of the blast radius, at animation
/// scale 1. The on-screen fireball peaks a bit under the full damage radius so it
/// reads as a punchy fireball, not a room-filling wall.
const BLAST_QUAD_HALF_FRAC: f32 = 0.42;
/// Half-texel UV inset so linear filtering never samples the neighbouring atlas
/// frame (atlas is `BLAST_FRAMES`×56 wide, 56 tall).
const BLAST_UV_INSET_U: f32 = 0.5 / (BLAST_FRAMES as f32 * 56.0);
const BLAST_UV_INSET_V: f32 = 0.5 / 56.0;
/// In-flight projectile marker: the bright box half-extent (m) drawn at the round's
/// current position, plus its short motion trail length (segments behind it).
const PROJECTILE_HALF: f32 = 0.1;
const PROJECTILE_TRAIL: usize = 4;
/// The detonation sound — the authentic GoldenEye blast SFX used for every
/// explosive (soundpack `blast14`, converted to WAV). Preloaded in `attach_audio`
/// so the first blast never hitches. Plus the shared blast volume.
pub(crate) const EXPLOSION_SOUND: &str = "sounds/weapons/explosion.wav";
pub(crate) const EXPLOSION_VOL: f32 = 0.9;
/// Approx centre-mass height above the feet (m) for the blast distance test — the
/// blast measures to the actor's middle, not its feet, so an overhead burst still
/// bites. One each for the ~1.44 m hunter and the player capsule.
const ENEMY_CENTER_Y: f32 = 0.7;
const PLAYER_CENTER_Y: f32 = 0.9;
/// A projectile that never contacts anything is dropped (no detonation) after this
/// long (s), so a fuseless rocket fired into open sky can't leak forever.
const PROJECTILE_MAX_LIFE: f32 = 6.0;
/// A bouncing projectile whose post-bounce speed drops below this (m/s) settles onto
/// the surface and waits out its fuse (stops the perpetual resting jitter).
const PROJECTILE_REST_SPEED: f32 = 1.5;
/// World scale for an in-flight projectile GLB (e.g. the thrown grenade). The
/// grenade GLB is authored ~3× the gun models, so a third of [`CHAR_SCALE`] lands it
/// at a believable hand-thrown grenade size (user call 2026-07-17).
const PROJECTILE_MODEL_SCALE: f32 = CHAR_SCALE / 3.0;
/// Tumble rates (rad/s) about X and Y for a flying projectile GLB, so it spins as it
/// travels. Frozen once the projectile comes to rest.
const PROJECTILE_SPIN_X: f32 = 9.0;
const PROJECTILE_SPIN_Y: f32 = 6.0;

// ─── Mines (see `world::combat`) ──────────────────────────────────────────────
/// How far off the struck surface the mine sits (m), so it doesn't z-fight or clip.
/// Kept generous so the (now larger) mine's back face clears the surface it's on —
/// too small and the mesh dips into the wall/floor and z-fights or vanishes.
const MINE_SURFACE_OFFSET: f32 = 0.15;
/// Max seconds a thrown mine flies before it's stuck in place where it is (fallback
/// so a toss into open space / the void can't fly forever without attaching).
const MINE_MAX_FLIGHT: f32 = 5.0;
/// World scale for a thrown/stuck mine GLB. The mine meshes are gun-sized in the
/// weapon library; [`CHAR_SCALE`] read too small in world, so bump 2.5× to land them
/// at a believable charge size (retune by eye if they read too big/small).
const MINE_MODEL_SCALE: f32 = CHAR_SCALE * 2.5;
/// The mine's "attach" sound, played when a mine sticks to a surface (soundpack
/// `attach_mine`, converted to WAV). Plus its volume.
pub(crate) const MINE_PLACE_SOUND: &str = "sounds/weapons/mine-place.wav";
const MINE_PLACE_VOL: f32 = 0.7;
/// The timed-mine arm beep, played once when a timed mine goes live (soundpack
/// `bomb_timer`, converted to WAV). Plus its volume.
pub(crate) const MINE_TIMER_SOUND: &str = "sounds/weapons/mine-timer.wav";
const MINE_TIMER_VOL: f32 = 0.6;
/// The remote-detonation "click" (soundpack `trigger_mine`, converted to WAV),
/// played when the player triggers a detonation (pad A+B / keyboard). Plus its
/// volume. No longer a weapon fire_sound (the Detonator slot was removed), so it's
/// preloaded explicitly in `attach_audio`.
pub(crate) const DETONATOR_SOUND: &str = "sounds/weapons/detonator-fire.wav";
const DETONATOR_VOL: f32 = 0.8;

/// Which opening the crosshair tool cuts. A `Door` is a fixed 3×7 wall opening
/// that becomes breakable at HUNT (frame marked `door`); a `Hole` is an
/// arbitrary-size opening in any face (walls, floor, or ceiling), not breakable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OpeningKind {
    Door,
    Hole,
}

/// Which additive-brush placement tool is armed. A `Pillar` is a floor→ceiling
/// square column; a `Brace` is a 3-brush arch (up one wall, across the ceiling,
/// down the opposite wall). Both are plain `Op::Add` brushes (JS marks them
/// `isBrace` for texturing, which we don't have yet).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PlaceKind {
    Pillar,
    Brace,
}

/// The free-standing platform/stair-run tool's phase (JS `state.platformPhase`).
/// `None` on `World` = the tool is off entirely; `Some(_)` = armed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PlatformPhase {
    /// Tool on, nothing selected — a click places a new platform or selects one.
    Idle,
    /// A platform or stair-run is selected (C connects, F grounds, V rails, X deletes).
    Selected,
    /// Connect step 1: aim + click locks the destination (platform/floor) + the
    /// source edge. A marker tracks the crosshair; nothing is built yet.
    ConnectDst,
    /// Connect step 2: destination + source edge are frozen; the crosshair slides
    /// the attach point along the source edge (JS `connecting_src`). A stable stair
    /// ghost follows; click confirms.
    ConnectSrc,
    /// Simple-stair: waiting for the first free endpoint click.
    SimpleFrom,
    /// Simple-stair: waiting for the second free endpoint click.
    SimpleTo,
}

/// The locked connect destination (JS `platformConnectTo`): a platform edge, or a
/// free-standing ground point.
#[derive(Clone, Copy)]
pub(crate) enum ConnectTarget {
    Platform { id: u32, edge: Edge },
    Ground { x: f32, y: f32, z: f32 },
}

/// A resolved crosshair hit for the platform tool: the WT hit point, the dominant
/// surface axis, and which platform/stair-run (if any) that point lies inside.
#[derive(Clone, Copy)]
pub(crate) struct StructureHit {
    hit_wt: Vec3,
    axis: Axis,
    platform: Option<u32>,
    run: Option<u32>,
}

/// One draggable part of the platform gizmo (JS `gizmo.js`): three move arrows
/// (translate the whole platform along an axis) and four edge scale handles
/// (grow/shrink the footprint from that edge).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum GizmoHandle {
    MoveX,
    MoveY,
    MoveZ,
    ScaleXMin,
    ScaleXMax,
    ScaleZMin,
    ScaleZMax,
}

/// An in-progress gizmo drag (JS `gizmo.drag`): the handle being dragged, the
/// platform's original transform (for cancel), and the sub-WT accumulator that
/// quantizes screen motion into whole-WT steps.
#[derive(Clone, Copy)]
pub(crate) struct GizmoDrag {
    handle: GizmoHandle,
    platform_id: u32,
    orig: Platform,
    accumulated: f32,
}

/// Which prop gizmo is active (object mode). One shown at a time, cycled with Tab.
/// Scale is a later addition; props are uniform-scaled for now.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PropGizmoMode {
    Translate,
    Rotate,
}

/// A translate axis for the prop move gizmo.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PropAxis {
    X,
    Y,
    Z,
}

/// An in-progress prop gizmo drag. Unlike the platform gizmo (look-relative mouse
/// deltas), this is driven by the absolute mouse ray each frame: a **fixed** axis
/// line / rotation plane through `pivot` (captured at drag start so the object
/// doesn't chase its own moving gizmo), plus the object's start transform and the
/// grab reference (axis param for translate, plane angle for rotate).
#[derive(Clone, Copy)]
pub(crate) struct PropGizmoDrag {
    mode: PropGizmoMode,
    axis: PropAxis,
    entity: hecs::Entity,
    pivot: Vec3,
    start_pos: Vec3,
    start_yaw: f32,
    grab_ref: f32,
}

/// A resolved opening placement (from the crosshair) — enough to draw the ghost
/// preview and to cut it. `position` is the face-plane WT coord on `axis`;
/// `(u0, v0)` is the opening's min corner on the two in-plane axes; `(w, h)` its
/// size along `(u_axis, v_axis)`. Generalizes the old door placement (JS
/// `computeHolePreview`, which drives both the hole and door tools).
#[derive(Clone, Copy)]
pub(crate) struct OpeningPlacement {
    region_id: u32,
    axis: Axis,
    side: Side,
    position: f32,
    u_axis: Axis,
    v_axis: Axis,
    u0: f32,
    v0: f32,
    w: f32,
    h: f32,
    kind: OpeningKind,
}

/// A pending (unconfirmed) stair op (JS `state.csg.pendingStairOp`): the arrow
/// keys grow/shrink `step_count` on the anchored wall face; Enter confirms it
/// into void brushes + a [`StairDesc`], Esc cancels. No geometry changes until
/// confirm — the counter just accumulates. `anchor_*` pin it to one face so the
/// opposite arrow shrinks the *same* op instead of starting a new one.
#[derive(Clone, Copy)]
pub(crate) struct PendingStair {
    direction: StairDir,
    step_count: u32,
    region_id: u32,
    axis: Axis,
    side: Side,
    face_pos: f32,
    u_axis: Axis,
    u0: f32,
    u1: f32,
    /// Face bottom (vMin) and stairwell ceiling H, in WT Y.
    floor: f32,
    ceil: f32,
    /// Texture scheme inherited from the wall the stair anchors to.
    scheme: usize,
}

/// A sub-face carve/extrude in progress (JS `activeBrush`/`activeOp`): a spawned
/// brush grown by repeated push/pull, so holding `+` carves deeper instead of
/// stacking new brushes on every press.
#[derive(Clone, Copy)]
pub(crate) struct ActiveOp {
    brush_id: u32,
    op: SubOp,
    side: Side,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum SubOp {
    Push,
    Pull,
}

/// The selected face's in-plane U/V extent in WT (JS `getFaceUVInfo`), plus the
/// face-plane coord on the normal axis.
pub(crate) struct FaceInfo {
    u_axis: Axis,
    v_axis: Axis,
    u_min: f32,
    u_max: f32,
    v_min: f32,
    v_max: f32,
    u_size: f32,
    v_size: f32,
    position: f32,
}

/// One live hunter during the HUNT: its AI/movement [`Enemy`], its own animation
/// mixer (cloned from the shared clip template so each hunter animates
/// independently), the weapon it wields + whether it's dual-wielding, its hitscan
/// capsule handle, and its per-hunter combat/feedback timers. Each hunter wears one
/// body from [`World::char_models`] (its [`Self::body`] id); the pose differs per
/// hunter and the geometry differs per body.
pub(crate) struct EnemyInstance {
    pub enemy: Enemy,
    /// Which body this hunter wears — an index into [`World::char_models`] /
    /// [`BODY_CATALOG`]. All bodies share the clip set + aim rig, so this only
    /// selects the mesh + that body's skeleton (bind pose) for skinning, feet
    /// seating, weapon attach, and blood-buffer sizing. Assigned at spawn.
    pub body: usize,
    /// This hunter's crossfade mixer (own clock/pose). Clip layout matches the
    /// shared template: locomotion, per-class fire, hit set, death set.
    pub anim: AnimPlayer,
    /// The equipped weapon (asset paths + AI stats + bone-local attach offsets).
    pub weapon: EnemyWeaponDef,
    /// Dual-wielding — a second copy of `weapon` is held in the left hand and both
    /// muzzles flash on a shot (JS `weaponOptions.dual`).
    pub dual: bool,
    /// This hunter's hitscan capsule (moved each fixed step, removed on death).
    pub collider: ColliderHandle,
    /// Death fade: seconds since the death animation finished, or `None` while alive
    /// / mid death-animation. Drives opacity 1→0 over [`FADE_DURATION`].
    pub fade: Option<f32>,
    /// Enemy-fire cadence: seconds until the next shot may leave during the fire
    /// window (spaced by `1/weapon.fire_rate`).
    pub shot_timer: f32,
    /// Active fire burst: `Some(elapsed_seconds)` while a burst is running, `None`
    /// otherwise. Firing is now a **timer** (not a full-body clip), so the hunter
    /// keeps running its locomotion + procedural aim while shooting.
    pub fire_elapsed: Option<f32>,
    /// Muzzle-flash countdown (s); >0 → this hunter's muzzle(s) render.
    pub muzzle_timer: f32,
    /// Seconds until this hunter may DAMAGE the player again — the hit-rate cap
    /// ([`MAX_HIT_RATE`]) that stops a full-auto spray from being a one-hit kill. The
    /// gun still visually fires at its own cadence; only how often a shot can *land*
    /// is capped, so difficulty comes from accuracy / speed / evasion, not raw ROF.
    pub damage_cooldown: f32,
    /// Per-vertex RGB blood color (flat, len = 3×model vertex count), white =
    /// clean. Each shot reddens the vertices near the impact (accumulating, so it
    /// builds up as persistent blood); uploaded to this hunter's instance color
    /// buffer each frame. JS `EnemyCharacter` per-instance vertex colors.
    pub blood: Vec<f32>,
    /// Procedural post-pass over `anim`'s blended pose: [aim-offset, recoil]. Nudges
    /// the gun arm toward the player while engaged; recoil kicks on each shot.
    pub stack: LayeredAnimator,
    /// Eased aim weight (0 = arm follows the clip, 1 = full aim swing at player).
    pub aim_weight: f32,
    /// Eased head look-at weight (0 = head follows the clip/loco pose, 1 = full gaze
    /// swing toward the focus). Ramped like [`Self::aim_weight`] so the head raises /
    /// lowers its gaze smoothly as the hunter acquires / loses a focus.
    pub head_look_weight: f32,
    /// Smoothed WORLD-space point the head is looking at — eased toward the current
    /// focus (player / last-known / search point) at [`HEAD_LOOK_TRACK`] so a focus
    /// switch sweeps the gaze rather than snapping. `None` until the first focus.
    pub head_look_point: Option<Vec3>,
    /// Foot IK: the eased per-foot vertical ground offset (world m, `[left, right]`) —
    /// how far each foot is lifted/dropped from its animated height onto the floor
    /// beneath it. Eased toward the freshly sampled floor delta at [`FOOT_IK_EASE`] so
    /// stair edges glide rather than pop; also the source of the pelvis drop.
    pub foot_delta: [f32; 2],
    /// Low-passed locomotion speed (m/s) for band selection — smooths the AI's
    /// binary `speed()` so the walk/jog/run band doesn't flip on frame-to-frame
    /// jitter. See [`LOCO_SMOOTH`].
    pub anim_speed: f32,
    /// The **rendered** facing yaw, eased toward the AI heading at [`TURN_RATE`] so the
    /// body turns at a believable rate instead of snapping when the FSM flips heading
    /// (evade jukes, reposition, travel↔player facing). `None` until the first frame
    /// (then it snaps to the current heading, and smooths thereafter). Drives every
    /// model/weapon/muzzle transform via [`EnemyInstance::yaw`]. See [`Self::yaw`].
    pub render_yaw: Option<f32>,
    /// Cached final pose after the stack this frame — the source for both the
    /// skinning matrices and the hand-bone weapon transform (so the gun follows
    /// the aimed arm). `None` until the first `advance_animation`.
    pub final_pose: Option<Pose>,
    /// Active death ragdoll (`Some` once killed while the [`World::ragdoll`] flag is
    /// on) — a chain of dynamic bodies that replaces the canned death clip. While set,
    /// this hunter's pose + model transform come from the physics bodies (WORLD-space
    /// skinning, identity model) instead of the animation stack; removed once the
    /// corpse has faded out. `None` for a living hunter or the canned-death path.
    pub ragdoll: Option<Ragdoll>,
    /// Seconds the ragdoll has simulated — feeds the [`RAGDOLL_MAX_SETTLE`] backstop.
    pub ragdoll_time: f32,
    /// Active living-hit reaction (`Some` briefly after a non-lethal hit while the
    /// [`World::ragdoll`] flag is on) — a transient physics ragdoll whose model-local
    /// pose is blended into the animation by a decaying weight (the partial active
    /// ragdoll). `None` for a hunter that isn't currently staggering.
    pub reaction: Option<Reaction>,
}

/// A living-hit stagger: the transient physics ragdoll for a non-lethal reaction plus
/// how long it's been decaying. The blend weight is a function of [`Self::elapsed`], so
/// re-seeding `elapsed = 0` on a re-hit re-peaks the stagger.
pub(crate) struct Reaction {
    /// The physics ragdoll seeded from the hit pose (blended in, not a full takeover).
    pub rag: Ragdoll,
    /// Seconds since this reaction (re-)started — drives the decaying blend weight.
    pub elapsed: f32,
}

impl Reaction {
    /// Current blend weight (0..[`REACTION_PEAK_WEIGHT`]): peak at the hit instant,
    /// decaying exponentially back to zero so the hunter returns to animation.
    pub fn weight(&self) -> f32 {
        REACTION_PEAK_WEIGHT * (-self.elapsed * REACTION_DECAY).exp()
    }
}

/// A loaded enemy weapon's render assets: the gun mesh + optional muzzle-flash
/// mesh, keyed by the weapon name. Loaded once for the whole arsenal in
/// [`World::new`] and handed to the renderer's weapon library so any hunter can
/// draw any weapon (and the BUILD demo can preview every gun).
pub(crate) struct EnemyWeaponAsset {
    pub name: &'static str,
    pub gun: TexturedModel,
    pub muzzle: Option<TexturedModel>,
    /// Muzzle offset in the gun model's local space (the muzzle-flash mesh centroid,
    /// which sits down the barrel). Its direction is the barrel axis — used by the
    /// chest-aim to point the real gun barrel at the player. Falls back to the gun
    /// mesh centroid.
    pub muzzle_offset: Vec3,
}

impl EnemyWeaponAsset {
    /// Barrel-forward axis (gun-model space) — the muzzle offset, normalized.
    pub(crate) fn barrel_axis(&self) -> Vec3 {
        let a = self.muzzle_offset.normalize_or_zero();
        if a == Vec3::ZERO {
            BARREL_MODEL_AXIS
        } else {
            a
        }
    }
}

/// Muzzle/barrel offset (gun-model space) from a mesh centroid: the flash/gun
/// extends away from the grip origin toward the muzzle, so the mean vertex position
/// lies down the barrel.
fn mesh_muzzle_offset(gun: &TexturedModel, muzzle: &Option<TexturedModel>) -> Vec3 {
    let model = muzzle.as_ref().unwrap_or(gun);
    let n = model.vertices.len().max(1) as f32;
    model
        .vertices
        .iter()
        .fold(Vec3::ZERO, |a, v| a + Vec3::from(v.pos))
        / n
}

pub struct World {
    pub camera: FlyCamera,
    pub physics: PhysicsWorld,
    pub mode: Mode,
    /// The entity-component layer (hecs): authored props today, and a home for
    /// runtime actors as they migrate off the god-struct. Present in both modes;
    /// gameplay systems only tick during HUNT (see [`Self::fixed_step`]). Authored
    /// entities persist via the level file (see `world::persist`). See [`crate::ecs`].
    ecs: crate::ecs::Ecs,
    /// The player capsule; `Some` only in HUNT mode.
    character: Option<CharacterController>,
    /// Baked nav grid; `Some` only in HUNT mode.
    nav: Option<NavWorld>,
    /// The live hunters (HUNT only) — one per [`ENEMY_ROSTER`] entry that found a
    /// spawn cell. Each carries its own mixer/weapon/collider and wears a body from
    /// [`Self::char_models`] (its `body` id). Empty in BUILD.
    enemies: Vec<EnemyInstance>,
    /// Whether a G→HUNT transition spawns the [`ENEMY_ROSTER`]. Defaults to `true`
    /// (so tests and the normal game get hunters); the app flips it off as a dev
    /// convenience while iterating on explosives (see `set_spawn_enemies`), so a
    /// hunt starts empty and you aren't gunned down before you can test.
    spawn_enemies: bool,
    /// Every loaded character body, indexed by body id ([`BODY_CATALOG`] order,
    /// minus any that failed to load). Index 0 is `russian-guard_karl`. Each hunter
    /// renders the body its `EnemyInstance::body` selects; the BUILD preview uses
    /// body 0. Empty if no asset loaded.
    char_models: Vec<SkinnedModel>,
    /// Pristine animation mixer over the full clip set (locomotion + per-class fire
    /// + hit + death), cloned once per spawned hunter so each animates on its own
    /// clock. Built once against body 0's skeleton and shared by every body (the rig
    /// is identical across all bodies; skinning uses each body's own skeleton at the
    /// call site). `None` if any clip failed to load.
    char_anim_template: Option<AnimPlayer>,
    /// xorshift state for the hit/death/pain random picks (no `rand` dep).
    char_rng: u64,
    /// Difficulty dial, `0..=DIFFICULTY_MAX` (0 = original baseline). Cranked live with
    /// the `=` / `-` keys; drives [`DiffParams`] for enemy lethality/health/evasion.
    difficulty: u32,
    /// How many hunters the next HUNT floods in (default [`ENEMY_COUNT`] = 1, "duel
    /// mode"). A runtime field so tests can spawn a pack for multi-hunter behaviours.
    wave_size: usize,
    /// The player's pose (feet, yaw, pitch) captured when this HUNT began, so a
    /// difficulty-change duel-reset ([`Self::restart_hunt`]) can drop the player back
    /// at the start rather than wherever they'd wandered. `None` outside HUNT.
    hunt_spawn: Option<(Vec3, f32, f32)>,
    /// Whether a shot hunter plays a flinch/hurt animation (+ brief stun). **Off by
    /// default** — the Perfect-Dark "sim" behaviour: take the damage (pain SFX + blood
    /// accumulate) and keep fighting, so a hurt hunter never stops shooting or looks
    /// like it's mid-flinch while firing. Kept as a flag so a future GoldenEye-faithful
    /// mode can turn the authored hit reactions back on. Death animations are
    /// unaffected (a kill always plays its death clip).
    hit_reactions: bool,
    /// Whether hunters use **ORCA local avoidance** to steer smoothly around one
    /// another + the player (the modern crowd-movement layer), replacing the old
    /// position-nudge separation. **On by default.** A kill-switch for A/B + a
    /// regression baseline: when off, each hunter applies its preferred velocity
    /// directly and the legacy [`separate_enemies`] nudge runs instead. Movement
    /// quality only — it never changes who/when a hunter shoots, so the difficulty-0
    /// engagement baseline is unaffected either way.
    local_avoidance: bool,
    /// Whether hunters turn their **head toward what they're focused on** (the player
    /// while engaged, the last-known position while investigating, the search point
    /// while sweeping) — the procedural look-at "aliveness" cue. **On by default.**
    /// A kill-switch / regression baseline: when off, the head just follows the
    /// authored clip + locomotion pose. Purely visual — it never touches perception
    /// or engagement, so the difficulty-0 combat baseline is unaffected either way.
    head_look: bool,
    /// Whether hunters use **ground-adaptive foot IK** (feet plant on stairs/platforms
    /// instead of floating/clipping) + stride-warped cadence (feet cycle at the real
    /// ground speed instead of skating). **On by default.** A kill-switch / regression
    /// baseline: when off, the model uses the raw locomotion pose seated at its root.
    /// Purely visual (enemy model only) — no perception/engagement effect.
    foot_ik: bool,
    /// Whether a killed hunter becomes a **physics ragdoll** (a chain of dynamic
    /// bodies seeded from its death pose + the killing shot's impulse, tumbling on the
    /// real level geometry) instead of playing a canned death clip. **On by default.**
    /// A kill-switch / regression baseline: when off, deaths play the authored death
    /// animation + fade exactly as before. Visual only — it fires on death (the AI is
    /// already gone), collides on its own [`GROUP_RAGDOLL`], and corpses are excluded
    /// from perception LOS, so the difficulty-0 lethality/perception baseline is
    /// unchanged either way.
    ragdoll: bool,
    /// Whether hunters get a **wall-clearance** nudge each step so the wide character
    /// model stops clipping into walls (grid nav only keeps the CENTRE on walkable
    /// ground — no body width). **On by default.** A kill-switch / regression baseline.
    /// Push-not-block, so it never stops a hunter fitting through a doorway; it slightly
    /// adjusts positions (off walls) but never changes who/when a hunter shoots.
    wall_clearance: bool,
    /// Whether hunters use the **utility-AI decision layer** (roadmap #4) — a scored
    /// behaviour selector that replaces the hand-coded FSM transitions, making the six
    /// behaviours emergent + composable. **On by default.** A kill-switch / regression
    /// baseline: when off, each hunter runs the legacy FSM (`Enemy::update`'s match).
    /// Reuses every tuned movement/perception primitive — only the *decision* changes.
    utility_ai: bool,
    /// Per-body world-space Y offset that seats that body's feet on the floor
    /// (parallel to [`Self::char_models`]). Computed from the **lowest skinned point
    /// of the actual idle pose** for each body (the bind-pose AABB can't be used —
    /// the bind pose is a splayed star with the feet spread high, so seating by it
    /// leaves the standing pose sunk). Bodies differ in height (e.g. Jaws), so this
    /// is per-body.
    char_feet_offset: Vec<f32>,
    /// The resolved gun-arm chain + rest geometry per body (parallel to
    /// [`Self::char_models`]), used to build each hunter's procedural aim/recoil
    /// stack. Per-body because bind poses (bone lengths) differ. An entry is `None`
    /// if that body's skeleton has no resolvable arm chain.
    enemy_arm: Vec<Option<EnemyArm>>,
    /// Spike: the optional BUILD-phase procedural-anim preview character (`Y`).
    /// `None` unless the preview is toggled on. See [`world::spike_preview`].
    procedural_preview: Option<spike_preview::ProceduralPreview>,
    /// `ANIM_DEBUG=1` in the env → log the nearest engaged hunter's per-frame
    /// locomotion/aim/fire state, to diagnose run-and-gun jank without eyes on it.
    anim_debug: bool,
    anim_dbg_frame: u64,
    // (Per-hunter death fade + fire cadence + muzzle timers now live on each
    // [`EnemyInstance`]; see `enemies` above.)

    // ─── Player health + damage feedback (P5; see `world/combat.rs`) ──
    /// Player health / armor (JS `Actor`; armor-first damage). Death at health 0.
    player_health: f32,
    player_armor: f32,
    /// Dead — the YOU DIED screen is up; the sim freezes until a restart.
    player_dead: bool,
    /// Dev/observe toggle (`I`): when set, the player takes no damage — enemies
    /// still aim + fire so their behaviour can be watched safely. Default off.
    player_invulnerable: bool,
    /// Dev/observe toggle (`N`, "iNvisible"): when set, no hunter can perceive the
    /// player (see [`crate::enemy::Enemy::set_detectable`]), so they never engage and
    /// revert to searching — you can walk around and watch the search + head-scan
    /// behaviour. Applied to every hunter each step in `fixed_step`. Default off (all
    /// hunters detectable).
    player_invisible: bool,
    /// Red full-screen damage-flash alpha (decays each frame).
    damage_flash: f32,
    /// Health-HUD pop timer (s); the radial HUD is shown while >0, fading over its
    /// last [`HUD_FADE_TAIL`].
    hud_show_timer: f32,
    /// The processed GoldenEye radial health graphic (angle/side maps), used to
    /// bake the HUD RGBA for the current health/armor. `None` if the JPEG failed.
    health_hud: Option<crate::hud::health::HealthHud>,

    // ─── Player Combat (HUNT-phase weapon; see `world/combat.rs`) ──
    /// P1: the first-person weapon's static gun mesh (CPU side), uploaded once to
    /// the renderer at startup. `None` if the asset failed to load.
    gun_model: Option<TexturedModel>,
    /// P2: the muzzle-flash mesh (separate GLB), uploaded once; drawn additively
    /// on top of the gun while a shot's flash is active. `None` if load failed.
    muzzle_model: Option<TexturedModel>,
    /// A3: the enemy weapon render library — the gun + muzzle meshes for the whole
    /// arsenal, loaded once and handed to the renderer so any hunter can draw any
    /// weapon (and the BUILD demo can preview each). Keyed by weapon name.
    enemy_weapon_lib: Vec<EnemyWeaponAsset>,
    /// The player's weapon inventory (JS `WeaponSystem.slots`) — one [`Weapon`]
    /// per `config::WEAPONS` entry, each keeping its own ammo/reload state so a
    /// swap resumes where you left off. `Q` / N64 `A` cycles [`weapon_index`].
    weapons: Vec<Weapon>,
    /// Ownership flag per [`weapons`] entry (same length/order as `config::WEAPONS`).
    /// The player starts owning only the PP7; the BUILD-phase shop flips these true
    /// as weapons are bought. Cycling (`Q` / N64 `A`) steps only through owned
    /// weapons — see [`combat::World::begin_weapon_switch`] / `next_owned`. The
    /// `weapons` Vec always holds all entries so per-weapon ammo state persists
    /// whether or not the weapon is currently owned.
    owned: Vec<bool>,
    /// The player's unified credit wallet (earned from kills, spent in the shop).
    /// Session-only for now — see [`crate::economy`].
    economy: crate::economy::Economy,
    /// Index of the active weapon in [`weapons`] (JS `currentSlotIndex`).
    weapon_index: usize,
    /// Weapon-switch animation state (JS `WeaponSystem.cycleWeapon`). `switching`
    /// gates firing + re-entry; `switch_timer` runs `0..SWITCH_TIME` across the
    /// lower→raise dip; at the halfway point the mesh swaps to `switch_target`,
    /// `switch_swapped` latches, and `models_dirty` tells the app to re-upload the
    /// new gun/muzzle. See `world::combat::combat_step`.
    switching: bool,
    switch_target: usize,
    switch_timer: f32,
    switch_swapped: bool,
    /// Set when a switch swaps the active weapon's meshes mid-animation; the app
    /// drains it via `take_models_dirty` and re-uploads the viewmodel + muzzle.
    models_dirty: bool,
    /// P2: live hit sparks — a short-lived bright marker at each impact point, so
    /// wall hits read at the right spot. Decayed each frame in HUNT.
    sparks: Vec<Spark>,
    /// Explosives: live projectiles in flight (rocket / launched grenade / thrown
    /// grenade). Advanced + collision-swept each frame in `explosives_step`; a
    /// contact or fuse expiry detonates them. Empty in BUILD.
    projectiles: Vec<crate::combat::Projectile>,
    /// Explosives: live placed mines (proximity / timed / remote). Armed + trip-
    /// checked each frame in `mines_step`; a trip, timeout, or the Detonator sets
    /// them off. Empty in BUILD.
    mines: Vec<crate::combat::Mine>,
    /// Explosives: live explosion VFX bursts, decayed each frame.
    blasts: Vec<Blast>,
    // ─── Grenade flush (#5) ──
    /// Where the player has been holding position — the camp anchor. Reset to the
    /// player's spot whenever they leave [`CAMP_RADIUS`] of it; `camp_timer` counts how
    /// long they've stayed. `None` until the first HUNT step / after a mode switch.
    camp_anchor: Option<Vec3>,
    /// Seconds the player has camped within [`CAMP_RADIUS`] of [`Self::camp_anchor`].
    camp_timer: f32,
    /// Squad-wide cooldown (s) after a grenade is lobbed, so the whole pack doesn't
    /// bury a camper under a simultaneous volley.
    grenade_cooldown: f32,
    /// GoldenEye free-aim crosshair offset in aim space (see `AIM_MAX_RANGE`).
    /// Moves while RMB is held (HUNT), springs back to center on release. Drives
    /// the crosshair position, the gun tilt, and the fire-ray direction. 0 = center.
    aim_x: f32,
    aim_y: f32,
    /// Whether free-aim is currently engaged (RMB held in HUNT). The crosshair is
    /// shown only while aiming (HUNT) — matching GoldenEye's aim-mode reticle.
    aiming: bool,
    /// The audio subsystem (one-shot weapon SFX + looping background music).
    /// `None` until the app attaches it post-construction (see `attach_audio`) —
    /// so headless tests, which never attach it, run silently. Cue draining and
    /// music are no-ops while `None`.
    audio: Option<AudioManager>,

    caught: bool,
    /// Where the hunters materialise at G→HUNT — the fixed [`SPAWN_MARKER_POS`]
    /// snapped to a standable cell. Set by [`World::prepare_spawn`]; the wave clusters
    /// around it. HUNT only.
    spawn_point: Vec3,
    /// The fan-out search-point pool for the hunt (spread standable cells). The
    /// `World` hands these out to searching hunters so the pack sweeps the base
    /// instead of clumping. Rebuilt each G→HUNT, cleared on return to BUILD.
    search_points: Vec<Vec3>,
    regions: Vec<Region>,
    /// brush id → the id of the region that owns it. Maintained incrementally as
    /// brushes are added ([`assign_brush_to_region`](Self::assign_brush_to_region))
    /// and rebuilt wholesale by [`recluster_all`](Self::recluster_all) on load/undo.
    /// Lets an edit re-bake only the affected region(s) instead of the whole level.
    brush_to_region: std::collections::HashMap<u32, u32>,
    /// Stable region-id allocator. Region ids must be unique over the session so a
    /// reclustered region never reuses an id still held by a renderer/physics entry
    /// mid-swap; [`recluster_all`](Self::recluster_all) hands out fresh ids from here.
    next_region_id: u32,
    /// Memoizes CSG results by a hash of a region's authored brushes+stairs, so a
    /// full recluster (undo/load) re-folds only the region that actually changed;
    /// the rest hit the cache. (JS `wasmResultCache`.)
    csg_cache: regions::CsgCache,
    selected: Option<Selection>,
    /// Doors, populated at G→HUNT: the fixed **spawn-door seal** (a black
    /// non-breakable panel) plus (when re-enabled) breakable doors. Cleared on
    /// return to BUILD. `Some`-active only during the hunt.
    doors: Vec<Door>,
    /// Opening tool state (BUILD): which crosshair opening tool is armed (door or
    /// hole), if any. Armed by `B` (door) / `H` (hole); a ghost preview tracks the
    /// crosshair, a left-click cuts, pressing the same key again disarms.
    opening_tool: Option<OpeningKind>,
    /// The placement the ghost currently previews (recomputed each frame while
    /// arming); what a confirm cuts.
    opening_preview: Option<OpeningPlacement>,
    /// The current hole size in WT (scroll-adjustable while the hole tool is
    /// armed): width along the face U axis, height along V. Doors are fixed size.
    hole_w: f32,
    hole_h: f32,
    /// Additive-brush placement tool (pillar / brace), if armed. Mutually
    /// exclusive with the opening tools.
    place_tool: Option<PlaceKind>,
    /// Prop-placement tool (the object palette): the [`crate::ecs::MeshId`] armed for
    /// placement, if any. While set, a floor ghost tracks the crosshair and a
    /// left-click drops the prop as an authored ECS entity. See `world::tools::prop`.
    prop_tool: Option<crate::ecs::MeshId>,
    /// The grounded metric floor point the prop ghost currently previews (recomputed
    /// each frame while `prop_tool` is armed); what a confirm places the prop at.
    prop_preview_pos: Option<Vec3>,
    /// Model-space AABB `(min, max)` per prop mesh, registered by the app once the
    /// GLBs load. Drives the render/ghost anchor (base-to-floor, horizontally
    /// centred) and, later, prop pick/collision. Empty in headless callers.
    prop_bounds: std::collections::HashMap<crate::ecs::MeshId, (Vec3, Vec3)>,
    /// The prop currently selected for editing (object mode), or `None`. Its gizmo
    /// is drawn and drag-editable. A runtime handle — cleared on load/undo (the ECS
    /// respawns entities). See `world::tools::prop_gizmo`.
    selected_prop: Option<hecs::Entity>,
    /// Live destructible-prop colliders → their prop entity (Milestone 3). Baked at
    /// BUILD→HUNT from every authored destructible prop; a hitscan hit on one of these
    /// handles routes damage to the mapped entity. An entry is removed as its prop is
    /// destroyed, and the whole map is cleared on return to BUILD. HUNT-only.
    prop_colliders: std::collections::HashMap<ColliderHandle, hecs::Entity>,
    /// Which prop gizmo is active (Translate / Rotate); cycled with Tab.
    prop_gizmo_mode: PropGizmoMode,
    /// The in-progress prop gizmo drag, if any.
    prop_gizmo_drag: Option<PropGizmoDrag>,
    /// This frame's mouse world ray `(origin, dir)`, pushed by the app from the free
    /// cursor's unprojection; the prop gizmo pick/hover/drag read it.
    mouse_ray: Option<(Vec3, Vec3)>,
    /// Whether gizmo drags snap to a grid/angle increment this frame (Ctrl held);
    /// pushed by the app. `false` = continuous. See `world::tools::prop_gizmo`.
    gizmo_snap: bool,
    /// Pillar cross-section (square) in WT; scroll-adjustable while armed.
    pillar_size: f32,
    /// Brace dimensions in WT: `brace_width` along the wall, `brace_depth` the
    /// inward protrusion + ceiling-strip thickness. Scroll = width, Shift = depth.
    brace_width: f32,
    brace_depth: f32,
    /// Sub-face selection size on the current face in WT; 0 = full face. Grown by
    /// the scroll wheel (JS `state.csg.selSizeU/V`): scroll = U, Shift+scroll = V.
    sel_size_u: f32,
    sel_size_v: f32,
    /// The current sub-rect `[u0, u1, v0, v1]` (WT), tracked to the crosshair by
    /// the per-frame preview and consumed by a sub-face push/pull.
    sel_bounds: Option<[f32; 4]>,
    /// A sub-face carve in progress, grown by repeated push/pull.
    active: Option<ActiveOp>,
    /// A pending stair op (arrow keys), not yet confirmed into geometry.
    pending_stair: Option<PendingStair>,
    /// Allocator for brushes spawned by tools (the door-cut is the first such
    /// tool; extrude / pillar reuse it later). Room brush is id 1.
    next_brush_id: u32,

    // ─── Free-standing platform + stair-run system (JS `Platform`/`StairRun`) ──
    /// The platform tool's phase, or `None` when the tool is off. Mutually
    /// exclusive with the opening/placement tools.
    platform_phase: Option<PlatformPhase>,
    /// Every free-standing platform and stair-run. Combined into the single
    /// `STRUCT_ID` structures mesh + collider; their solid boxes feed nav.
    platforms: Vec<Platform>,
    stair_runs: Vec<StairRun>,
    /// The currently-selected platform / stair-run (at most one is `Some`).
    selected_platform: Option<u32>,
    selected_run: Option<u32>,
    /// Connect source platform id (set from `C` through the connect steps).
    connect_from: Option<u32>,
    /// Locked destination + source edge during [`PlatformPhase::ConnectSrc`].
    connect_to: Option<ConnectTarget>,
    connect_edge: Option<Edge>,
    /// Attach position along the source edge in WT (scroll-adjusted during
    /// `ConnectSrc`); `offset = connect_slide_wt / edge_len`.
    connect_slide_wt: f32,
    /// First endpoint (WT) of a simple-stair while in [`PlatformPhase::SimpleTo`].
    simple_from: Option<Vec3>,
    /// Footprint of the next placed platform in WT (scroll = X, Shift+scroll = Z).
    platform_size_x: f32,
    platform_size_z: f32,
    /// Id allocators for platforms / stair-runs (JS `nextPlatformId`/`nextStairRunId`).
    next_platform_id: u32,
    next_run_id: u32,
    /// An active gizmo drag on the selected platform, if any (JS `gizmo.drag`).
    gizmo_drag: Option<GizmoDrag>,

    // ─── Undo / redo (BUILD authoring; see `world::history`) ──────────────────
    /// Snapshots of the authored level state (the same source of truth
    /// [`save_level`](Self::save_level) serializes), one per committed edit. Undo
    /// pops here and re-bakes; a fresh edit clears [`redo_stack`](Self::redo_stack).
    /// Capped at [`history::MAX_HISTORY`].
    undo_stack: Vec<history::LevelSnapshot>,
    /// Snapshots popped by undo, replayed by redo. Cleared whenever a new edit is
    /// recorded (you can't redo past a divergent edit).
    redo_stack: Vec<history::LevelSnapshot>,
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    /// One room to start with: a single subtractive brush inside an auto-shell —
    /// the editor's opening move. Camera spawns inside, facing the −Z wall.
    pub fn new() -> Self {
        let mut region = Region::new(0);
        // Room cavity in WT: 24 × 16 × 24 → 6 × 4 × 6 m.
        region
            .brushes
            .push(Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 24.0, 16.0, 24.0));

        // Spawn at the room's horizontal center, ~1.5 m up, looking toward −Z.
        let camera = FlyCamera::new(Vec3::new(3.0, 1.5, 3.0), 0.0, 0.0);

        // Load every character body in the catalog (a warning, not a panic, per body
        // if an asset is missing — the editor still runs without them). Index 0 is
        // `russian-guard_karl`; any body that fails to load is skipped, so body ids
        // stay contiguous over what actually loaded. All bodies ride the same rig, so
        // one clip template + one aim rig-topology serves them all (below).
        let mut char_models: Vec<SkinnedModel> = Vec::new();
        for (name, file) in BODY_CATALOG {
            let path = format!(
                "{}/../../assets/enemies/characters/{file}",
                env!("CARGO_MANIFEST_DIR")
            );
            match gltf_skin::load(&path) {
                Ok(m) => {
                    log::info!(
                        "loaded character {name}: {} verts, {} primitives, {} joints",
                        m.vertices.len(),
                        m.primitives.len(),
                        m.skeleton.joint_count()
                    );
                    char_models.push(m);
                }
                Err(e) => log::warn!("character {name} load failed: {e}"),
            }
        }
        log::info!("loaded {}/{} character bodies", char_models.len(), BODY_CATALOG.len());

        // Load the clip set bound to body 0's skeleton in a FIXED index order —
        // locomotion 0–3, then one fire clip per weapon CLASS (rifle/pistol/dual,
        // indices FIRE_*_IDX), then the hit set, then the death set (see CHAR_*_IDX) —
        // into a template mixer. The rig is identical across all bodies, so this one
        // template drives every body; each spawned hunter clones it so it animates on
        // its own clock, and the BUILD demo clones it too. Skinning always uses the
        // hunter's OWN body skeleton, so per-body bone lengths are respected.
        let char_anim_template = char_models.first().and_then(|m| {
            let mut files: Vec<&str> =
                vec!["00-idle.glb", "28-walking.glb", "2A-jogging.glb", "29-running.glb"];
            files.push("01-fire-standing.glb"); // FIRE_RIFLE_IDX
            files.push("41-fire-standing-pistol.glb"); // FIRE_PISTOL_IDX
            files.push("7A-fire-standing-dual-wield.glb"); // FIRE_DUAL_IDX
            files.extend_from_slice(anim_set::HIT_CLIPS);
            files.extend_from_slice(anim_set::DEATH_CLIPS);
            let mut clips = Vec::new();
            for f in &files {
                let path =
                    format!("{}/../../assets/enemies/animations/{f}", env!("CARGO_MANIFEST_DIR"));
                match clip::load(&path, &m.skeleton) {
                    Ok(c) => clips.push(c),
                    Err(e) => log::warn!("clip {f} load failed: {e}"),
                }
            }
            if clips.len() == files.len() {
                log::info!(
                    "loaded {} character clips (idle/walk/jog/run + rifle/pistol/dual fire + 12 hit + 17 death)",
                    clips.len()
                );
                Some(AnimPlayer::new(clips, 0))
            } else {
                log::warn!("only {}/{} clips loaded; character animation disabled", clips.len(), files.len());
                None
            }
        });
        // Per-body feet seating + gun-arm resolution: bind poses (bone lengths) differ
        // between bodies, so both are computed for EACH body against its own skeleton.
        // Feet: sample the idle across its loop, skin each pose on the CPU, and take
        // the global lowest Y (the most-planted foot); seating that at the floor keeps
        // the feet grounded while the animation's own vertical motion still reads.
        // Falls back to the bind-pose AABB with no idle clip. Arm: resolve the gun-arm
        // chain once per body; each hunter clones a fresh aim/recoil stack from its
        // body's arm at spawn.
        let idle_clip = char_anim_template.as_ref().and_then(|a| a.clip(0));
        let mut char_feet_offset: Vec<f32> = Vec::with_capacity(char_models.len());
        let mut enemy_arm: Vec<Option<EnemyArm>> = Vec::with_capacity(char_models.len());
        for m in &char_models {
            let feet = match idle_clip {
                Some(idle) => {
                    let samples = 24;
                    let mut min_y = f32::INFINITY;
                    for i in 0..samples {
                        let t = idle.duration * i as f32 / samples as f32;
                        let mats = idle.skinning_matrices(t, &m.skeleton);
                        min_y = min_y.min(m.skinned_min_y(&mats));
                    }
                    -min_y * CHAR_SCALE
                }
                None => -m.bounds_min.y * CHAR_SCALE,
            };
            char_feet_offset.push(feet);
            enemy_arm.push(idle_clip.and_then(|idle| EnemyArm::resolve(m, idle)));
        }

        // Player Combat: build the full weapon inventory (JS `ALL_WEAPONS`) and
        // load the *active* weapon's gun + muzzle-flash meshes. The rest of the
        // guns load their meshes lazily on the first switch (see `cycle_weapon`) —
        // startup only pays for PP7 (index 0). Warn-not-panic if an asset is
        // missing. All GLBs live under `native/assets/weapons/`.
        let weapons: Vec<Weapon> = crate::combat::config::WEAPONS
            .iter()
            .map(|&cfg| Weapon::new(cfg))
            .collect();
        // Start on the PP7 (the default sidearm) — and, now that there's an economy,
        // start *owning only* the PP7. The rest of the arsenal is bought from the
        // BUILD-phase shop; cycling (Q / N64 A) reaches only what you own.
        let weapon_index = crate::combat::config::WEAPONS
            .iter()
            .position(|w| w.name == "PP7")
            .unwrap_or(0);
        let mut owned = vec![false; weapons.len()];
        owned[weapon_index] = true;
        let (gun_model, muzzle_model) = load_weapon_models(weapons[weapon_index].config());

        // P5: the GoldenEye radial health HUD graphic (processed once into angle/
        // side maps). Warn-not-panic if the JPEG is missing.
        let health_hud = {
            let p = format!("{}/../../assets/hud/goldeneye-health.jpg", env!("CARGO_MANIFEST_DIR"));
            match crate::hud::health::HealthHud::load(&p) {
                Some(h) => {
                    log::info!("loaded health HUD graphic {}×{}", h.w, h.h);
                    Some(h)
                }
                None => {
                    log::warn!("health HUD graphic load failed");
                    None
                }
            }
        };

        // A3: the enemy weapon render library — load the gun + muzzle-flash meshes
        // for the WHOLE arsenal once, so any hunter can wield any weapon (attached
        // to a hand bone in world space) and the BUILD demo can preview each. Same
        // static-textured loaders as the player gun (the flash keeps only the
        // additive `CullBoth` billboards). Warn-not-panic per weapon.
        let asset = |rel: &str| format!("{}/../../assets/weapons/{}", env!("CARGO_MANIFEST_DIR"), rel);
        let mut enemy_weapon_lib: Vec<EnemyWeaponAsset> = Vec::new();
        for cfg in crate::combat::config::WEAPONS {
            // Enemies wield the HANDLESS variant so Bond's ripped first-person hand
            // doesn't float on the hunter's gun (see `combat::gun_strip`). Only the
            // pistols + detonator ship a `gun_handless.glb`; everything else has no
            // hand, so this falls back to `gun.glb`. The player viewmodel keeps the
            // hand (it loads `gun.glb` directly — see `combat::viewmodel::load_gun`).
            let gun_path = crate::combat::gun_strip::enemy_gun_path(cfg.gun_path, |rel| {
                std::path::Path::new(&asset(rel)).exists()
            });
            let gun = match crate::combat::load_gun(&asset(&gun_path)) {
                Ok(m) => m,
                Err(e) => {
                    log::warn!("enemy weapon '{}' gun load failed: {e}", cfg.name);
                    continue; // no gun mesh → this weapon can't be drawn on a hunter
                }
            };
            let muzzle = if cfg.muzzle_path.is_empty() {
                None
            } else {
                match crate::combat::load_flash(&asset(cfg.muzzle_path)) {
                    Ok(m) => Some(m),
                    Err(e) => {
                        log::warn!("enemy weapon '{}' muzzle load failed: {e}", cfg.name);
                        None
                    }
                }
            };
            let muzzle_offset = mesh_muzzle_offset(&gun, &muzzle);
            enemy_weapon_lib.push(EnemyWeaponAsset { name: cfg.name, gun, muzzle, muzzle_offset });
        }
        log::info!("loaded {} enemy weapon meshes", enemy_weapon_lib.len());

        World {
            camera,
            physics: PhysicsWorld::new(),
            mode: Mode::Build,
            ecs: crate::ecs::Ecs::new(),
            character: None,
            nav: None,
            enemies: Vec::new(),
            spawn_enemies: true,
            char_models,
            char_anim_template,
            char_rng: 0x9E37_79B9_7F4A_7C15,
            difficulty: 0,
            wave_size: ENEMY_COUNT,
            hunt_spawn: None,
            hit_reactions: false, // Perfect-Dark sim style; flag for a future GE mode
            local_avoidance: true, // ORCA crowd steering on by default (kill-switch below)
            head_look: true, // procedural head look-at on by default (kill-switch below)
            foot_ik: true, // ground-adaptive foot IK + cadence on by default (kill-switch below)
            ragdoll: true, // physics ragdoll death on by default (kill-switch below)
            wall_clearance: true, // wall-clearance nudge on by default (kill-switch below)
            utility_ai: true, // utility-AI decision layer on by default (kill-switch below)
            char_feet_offset,
            enemy_arm,
            procedural_preview: None,
            anim_debug: std::env::var("ANIM_DEBUG").is_ok(),
            anim_dbg_frame: 0,
            player_health: PLAYER_MAX_HEALTH,
            player_armor: 0.0,
            player_dead: false,
            player_invulnerable: false,
            player_invisible: false,
            damage_flash: 0.0,
            hud_show_timer: 0.0,
            health_hud,
            gun_model,
            muzzle_model,
            enemy_weapon_lib,
            weapons,
            owned,
            economy: crate::economy::Economy::new(0),
            weapon_index,
            switching: false,
            switch_target: 0,
            switch_timer: 0.0,
            switch_swapped: false,
            models_dirty: false,
            sparks: Vec::new(),
            projectiles: Vec::new(),
            mines: Vec::new(),
            blasts: Vec::new(),
            camp_anchor: None,
            camp_timer: 0.0,
            grenade_cooldown: 0.0,
            aim_x: 0.0,
            aim_y: 0.0,
            aiming: false,
            audio: None,
            caught: false,
            spawn_point: SPAWN_MARKER_POS,
            search_points: Vec::new(),
            regions: vec![region],
            // The opening room is region 0, owning brush 1.
            brush_to_region: std::collections::HashMap::from([(1u32, 0u32)]),
            next_region_id: 1,
            csg_cache: regions::CsgCache::new(),
            selected: None,
            doors: Vec::new(),
            opening_tool: None,
            opening_preview: None,
            hole_w: HOLE_WIDTH,
            hole_h: HOLE_HEIGHT,
            place_tool: None,
            prop_tool: None,
            prop_preview_pos: None,
            prop_bounds: std::collections::HashMap::new(),
            selected_prop: None,
            prop_colliders: std::collections::HashMap::new(),
            prop_gizmo_mode: PropGizmoMode::Translate,
            prop_gizmo_drag: None,
            mouse_ray: None,
            gizmo_snap: false,
            pillar_size: PILLAR_SIZE,
            brace_width: BRACE_DIM,
            brace_depth: BRACE_DIM,
            sel_size_u: 0.0,
            sel_size_v: 0.0,
            sel_bounds: None,
            active: None,
            pending_stair: None,
            next_brush_id: 2,
            platform_phase: None,
            platforms: Vec::new(),
            stair_runs: Vec::new(),
            selected_platform: None,
            selected_run: None,
            connect_from: None,
            connect_to: None,
            connect_edge: None,
            connect_slide_wt: 0.0,
            simple_from: None,
            platform_size_x: PLATFORM_SIZE,
            platform_size_z: PLATFORM_SIZE,
            next_platform_id: 1,
            next_run_id: 1,
            gizmo_drag: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    /// The current difficulty level (`0..=DIFFICULTY_MAX`), for the HUD readout + tests.
    pub fn difficulty(&self) -> u32 {
        self.difficulty
    }

    /// Set the difficulty dial to an absolute `level` (clamped to `0..=DIFFICULTY_MAX`),
    /// restarting the duel if mid-HUNT. Used to pin a fixed level at startup so the dial
    /// doesn't have to be managed by hand; the `=`/`-` keys still nudge it after.
    pub fn set_difficulty(&mut self, level: u32) {
        let new = level.min(DIFFICULTY_MAX);
        if new == self.difficulty {
            return;
        }
        self.difficulty = new;
        log::info!("DIFFICULTY set to {}/{}", self.difficulty, DIFFICULTY_MAX);
        self.restart_hunt();
    }

    /// Set how many hunters the next HUNT spawns (default 1 — "duel mode"). Dev/test
    /// knob; tests bump it to exercise multi-hunter separation/squad behaviour.
    pub fn set_wave_size(&mut self, n: usize) {
        self.wave_size = n;
    }

    /// Enable/disable the authored flinch/hurt animations on a shot hunter (default
    /// OFF — see [`Self::hit_reactions`]). A future GoldenEye-faithful mode turns this
    /// on; the current sim-style hunt leaves it off so hurt hunters keep fighting.
    pub fn set_hit_reactions(&mut self, on: bool) {
        self.hit_reactions = on;
    }

    /// Toggle ORCA local avoidance (default ON — see [`Self::local_avoidance`]). Off
    /// falls back to applying each hunter's preferred velocity directly + the legacy
    /// position-nudge separation, the pre-ORCA baseline.
    pub fn set_local_avoidance(&mut self, on: bool) {
        self.local_avoidance = on;
    }

    /// Whether ORCA local avoidance is active (inspection / tests).
    pub fn local_avoidance(&self) -> bool {
        self.local_avoidance
    }

    /// Toggle procedural head look-at (default ON — see [`Self::head_look`]). Off, the
    /// head follows only the authored clip + locomotion pose (the pre-look-at baseline).
    pub fn set_head_look(&mut self, on: bool) {
        self.head_look = on;
    }

    /// Whether procedural head look-at is active (inspection / tests).
    pub fn head_look(&self) -> bool {
        self.head_look
    }

    /// Toggle ground-adaptive foot IK + cadence (default ON — see [`Self::foot_ik`]).
    /// Off, the model uses the raw locomotion pose seated at its root (pre-foot-IK
    /// baseline).
    pub fn set_foot_ik(&mut self, on: bool) {
        self.foot_ik = on;
    }

    /// Whether foot IK is active (inspection / tests).
    pub fn foot_ik(&self) -> bool {
        self.foot_ik
    }

    /// Toggle physics-ragdoll death (default ON — see [`Self::ragdoll`]). Off, a killed
    /// hunter plays the canned death clip + fade (pre-ragdoll baseline).
    pub fn set_ragdoll(&mut self, on: bool) {
        self.ragdoll = on;
    }

    /// Whether physics-ragdoll death is active (inspection / tests).
    pub fn ragdoll(&self) -> bool {
        self.ragdoll
    }

    /// Toggle the utility-AI decision layer (default ON — see [`Self::utility_ai`]). Off,
    /// hunters run the legacy hand-coded FSM (pre-utility baseline / kill-switch).
    pub fn set_utility_ai(&mut self, on: bool) {
        self.utility_ai = on;
    }

    /// Whether the utility-AI decision layer is active (inspection / tests).
    pub fn utility_ai(&self) -> bool {
        self.utility_ai
    }

    /// Toggle the wall-clearance nudge (default ON — see [`Self::wall_clearance`]). Off,
    /// hunters keep their raw nav positions (may clip walls with the wide model).
    pub fn set_wall_clearance(&mut self, on: bool) {
        self.wall_clearance = on;
    }

    /// Whether the wall-clearance nudge is active (inspection / tests).
    pub fn wall_clearance(&self) -> bool {
        self.wall_clearance
    }

    /// The difficulty-derived tuning for the current level (see [`DiffParams`]). Linear
    /// ramp from all-neutral at level 0 to brutal at [`DIFFICULTY_MAX`].
    pub(crate) fn difficulty_params(&self) -> DiffParams {
        let t = self.difficulty as f32 / DIFFICULTY_MAX as f32;
        DiffParams {
            accuracy_mult: 1.0 + 0.6 * t,  // 1.0 → 1.6 (capped at 1.0 hit chance in use)
            falloff_ease: t,               // 0 → 1 (removes the distance penalty)
            speed_mult: 1.0 + 0.5 * t,     // 1.0 → 1.5× (closes/repositions faster)
            cooldown_mult: 1.0 - 0.85 * t, // 1.0 → 0.15× (near-continuous fire)
            reaction_mult: 1.0 - 0.8 * t,  // 1.0 → 0.2× (near-instant engage)
            health_mult: 1.0 + 3.0 * t,    // 1.0 → 4× (100 → 400 hp)
            dodge: t,                      // 0 → 1 (reactive aim-dodge frequency)
            sense_mult: 1.0 + 0.4 * t,     // 1.0 → 1.4× perception reach (sharper senses)
            suppress: t,                   // 0 → 1 (fire while closing; band widens with t)
            flank: t,                      // 0 → 1 (curve the approach off the direct line)
            cover: t,                      // 0 → 1 (break LOS to cover + peek-fire)
        }
    }

    /// The FSM-side difficulty knobs for the current level (reaction/cooldown/dodge),
    /// built from [`Self::difficulty_params`] + the enemy baseline constants.
    pub(crate) fn ai_tuning(&self) -> crate::enemy::AiTuning {
        let dp = self.difficulty_params();
        crate::enemy::AiTuning {
            alert: crate::enemy::ALERT_DURATION * dp.reaction_mult,
            cooldown: crate::enemy::COOLDOWN_DURATION * dp.cooldown_mult,
            dodge: dp.dodge,
            speed_mult: dp.speed_mult,
            sense: dp.sense_mult,
            suppress: dp.suppress,
            flank: dp.flank,
            cover: dp.cover,
        }
    }

    /// Nudge the difficulty dial by `delta`, clamped to `0..=DIFFICULTY_MAX`, then
    /// **restart the duel** ([`Self::restart_hunt`]) so the encounter begins fresh at
    /// the new level — full player health, the player back at the hunt start, and the
    /// enemy respawned with the new lethality/health/evasion. In BUILD there's no live
    /// sim, so it just updates the dial for the next hunt. Logged for the console too.
    pub fn change_difficulty(&mut self, delta: i32) {
        let new = (self.difficulty as i32 + delta).clamp(0, DIFFICULTY_MAX as i32) as u32;
        if new == self.difficulty {
            return; // already at the floor/ceiling — nothing to change or reset
        }
        self.difficulty = new;
        log::info!("DIFFICULTY → {}/{} — restarting the duel", self.difficulty, DIFFICULTY_MAX);
        self.restart_hunt();
    }

    /// Enable/disable spawning the [`ENEMY_ROSTER`] on G→HUNT (dev convenience). Off
    /// = hunts start with no hunters, so you can test explosives without being shot.
    /// On by default; the app turns it off while iterating on explosives.
    pub fn set_spawn_enemies(&mut self, on: bool) {
        self.spawn_enemies = on;
    }

    /// Evaluate every region once, set colliders, and return the meshes so the
    /// app can upload them. Call at startup.
    pub fn initial_meshes(&mut self) -> Vec<RegionMesh> {
        let ids: Vec<u32> = self.regions.iter().map(|r| r.id).collect();
        ids.into_iter()
            .filter_map(|id| self.rebuild_region(id))
            .collect()
    }

    /// Whether the selection-highlight should be shown (BUILD only).
    pub fn is_build(&self) -> bool {
        self.mode == Mode::Build
    }

    /// The player's feet position (meters), if in HUNT mode.
    pub fn player_pos(&self) -> Option<Vec3> {
        self.character.as_ref().map(|c| c.pos)
    }

    /// Whether the hunter has caught the player.
    pub fn is_caught(&self) -> bool {
        self.caught
    }

    /// The enemy spawn-point marker: a flat colored square laid on the floor at the
    /// fixed [`SPAWN_MARKER_POS`], drawn in **both** BUILD and HUNT so the level can
    /// be authored around a consistent, visible enemy-ingress point. A thin raised
    /// tile (via [`push_colored_box`]) through the depth-tested spark pipeline.
    pub fn spawn_marker_mesh(&self) -> Option<ColoredMesh> {
        let c = SPAWN_MARKER_POS;
        let min = Vec3::new(c.x - SPAWN_MARKER_HALF, c.y + 0.01, c.z - SPAWN_MARKER_HALF);
        let max = Vec3::new(c.x + SPAWN_MARKER_HALF, c.y + 0.05, c.z + SPAWN_MARKER_HALF);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        push_colored_box(&mut vertices, &mut indices, min, max, SPAWN_MARKER_COLOR);
        Some(ColoredMesh { vertices, indices })
    }
}
