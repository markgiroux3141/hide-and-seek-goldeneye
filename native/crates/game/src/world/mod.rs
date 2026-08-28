//! The authored scene — a hand-rolled `World` (no ECS yet; entity counts don't
//! justify one until the Phase 3 enemy roster). Owns the CSG regions, the
//! collision world, and the fly camera, and drives the BUILD-phase authoring
//! loop: crosshair face-pick → push/pull → re-evaluate the region → hand the
//! app a fresh mesh while updating the region's collider in place.
//!
//! Mirrors the reference editor (`src/tools/indoorKeys.js` + `csgActions.js`):
//! `+`/`=` push (carve inward), `-` pull (extend outward), default step 4 WT.

use std::time::Instant;

use glam::{EulerRot, Mat4, Quat, Vec2, Vec3};

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
    AdditiveDecayLayer, AimCone, AimOffsetLayer, ClipOverlayLayer, LayerCtx, LayeredAnimator,
    LocomotionBlendLayer, Pose, PoseLayer, RootTranslateLayer, TwoBoneIkLayer,
};
use engine::skeletal::gltf_skin::{self, SkinnedModel};
use engine::geometry::structures::{self, Anchor, Edge, Platform, StairRun, StairStyle};
use engine::render::textures::default_scheme;
use engine::render::uv_zones::ZonedBuilder;

// ─── Submodule tree (the `impl World` methods are spread across these) ──
mod combat;
mod editing;
mod geom;
mod history;
mod hunt;
mod lifecycle;
mod jank;
mod nav_probe;
pub use nav_probe::{ProbeResult, ProbeSample};
mod nav_issues;
pub use nav_issues::{NavIssues, NavLine, NavSeverity};
pub mod pd_lab;
/// `pub(crate)` so the app can ask whether a quick-slot has a file — the radial's
/// Level ring says "Load 3" vs "Slot 3", which needs the path.
pub(crate) mod persist;
mod pick;
mod regions;
mod respawn;
mod scoreboard;
pub use scoreboard::{RoundOutcome, Score};
/// The model bounds for a gun lying on the ground — the app registers these for
/// [`crate::ecs::MeshId::WeaponPickup`] at startup, since a weapon pickup has no
/// catalog GLB to measure.
pub use tools::pickup::weapon_pickup_bounds;
pub(crate) use scoreboard::Killer;
mod spawn;
pub(crate) use spawn::Spawning;
mod spike_preview;
pub(crate) mod tools;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod ai_testbed;

// Module-internal free helpers, re-exported so every submodule reaches them
// through `use super::*` regardless of which file defines them. (`find_room_brushes`
// / `brushes_touching` are used only within `editing`, so they aren't re-exported.)
pub(crate) use geom::{
    append_textured_collision, boxes_mesh, make_stair_void, make_wall_brush, push_colored_box,
    push_colored_quad_y, structure_collider_mesh,
};
pub(crate) use hunt::band_for_speed;
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
///
/// This is the **fallback** for a hunter whose fire animation has no authored
/// limits. A hunter with a [`crate::combat::attack_anim::AttackAnimConfig`] uses
/// Perfect Dark's own per-animation `maxup`/`maxdown`/`maxleft`/`maxright` instead,
/// which are tighter sideways (±40°) and asymmetric vertically (+50°/−40°) — so the
/// body turns rather than the chest twisting most of the way round.
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
/// **The arsenal's barrel-forward convention** in gun-model space, for the five weapons
/// that ship no muzzle-flash mesh to measure it from (sniper, rocket launcher, hand
/// grenade, the three mines).
///
/// `+Z`, because that is what all eighteen flash-bearing weapons measure to, every one
/// of them within 4°. It was `-Z` — the exact opposite — which is how the rocket
/// launcher ended up aiming backwards. Nothing caught it because the old fallback never
/// reached this constant: it produced a gun-mesh centroid instead, which is a different
/// wrong answer per weapon. See [`resolve_barrel_axis`].
pub(crate) const BARREL_MODEL_AXIS: Vec3 = Vec3::Z;
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
            cone: AimCone::uniform(ENEMY_CHEST_AIM_CONE),
            weight: 0.0,
            enabled: false,         // enabled once its `forward` is measured
        }));
        s.push(Box::new(AimOffsetLayer {
            joint: self.head,
            forward: self.head_forward, // baked gaze axis (head-local)
            target: Vec3::Z,            // set to the focus point each frame in `advance_animation`
            cone: AimCone::uniform(ENEMY_HEAD_LOOK_CONE),
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
/// The capsule's full standing height (≈1.44 m) — for anything that has to measure to a
/// hunter's *body* rather than to a point on it, blast damage being the one that cares
/// (see `combat::blast_distance_to_body`).
pub(crate) const ENEMY_BODY_HEIGHT: f32 = 2.0 * (ENEMY_HALF_HEIGHT + ENEMY_RADIUS);
/// The body height those constants were tuned against — a GoldenEye body renders
/// 1.50 m. A **Perfect Dark** body is 1.73 m, so a fixed capsule would leave its head
/// 23 cm in the open air: unhittable, and never a headshot. [`World::body_capsule`]
/// and [`World::body_hit_zones`] therefore scale with the body's own measured
/// height, and these ratios are what make a GoldenEye body come out at exactly the
/// numbers above (`1.44 / 1.50 = 0.96` of its height, radius a sixth of that,
/// half-height a third).
pub(crate) const CAPSULE_REF_HEIGHT: f32 = 1.50;
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
/// Number of body-fall impacts (`sounds/enemies/fallover1..3.wav`, converted from
/// the GoldenEye sound pack) and their volume. Played on a death animation's
/// authored `thudframe`, so the impact lands with the body rather than with the
/// shot — see `combat::hit_anim`.
pub(crate) const FALLOVER_COUNT: usize = 3;
pub(crate) const FALLOVER_VOL: f32 = 0.75;
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
/// Boundaries for a [`CAPSULE_REF_HEIGHT`] body; [`World::body_hit_zones`] scales
/// them by the hunter's own height so "head" means the head on a body of any size.
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

// `MAX_HIT_RATE` lived here: a global ceiling (4/s) on how often an enemy shot could
// actually DAMAGE the player, independent of the weapon's visual fire rate.
//
// **Retired with the hit roll it existed to contain** (§17). It was an artificial
// lethality limiter bolted on top of an artificial accuracy model: because a hunter
// *rolled* for hits, a full-auto would otherwise have deleted the player in one burst.
// The zeroing model replaces both, and keeping the cap on top of it would have clipped
// the top of exactly the range the difficulty table expresses — Hard and Dark both
// saturate 4/s and become indistinguishable. The honest ceiling is Perfect Dark's burst
// gap, which `enemy_combat_step` applies to the *cadence* rather than to the damage.

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

/// The Perfect Dark counterpart of [`ENEMY_ROSTER`], covering the same animation
/// classes so one hunt still exercises rifle / pistol / dual-rifle / dual-pistol.
///
/// Needed because [`ENEMY_ROSTER`] names GoldenEye weapons as `const`s, and the
/// enemy weapon library is built from the **live** arsenal — so under `ARSENAL=pd`
/// none of those names resolved and every hunter spawned holding nothing at all.
/// Weapons are looked up by name rather than listed as consts, because the PD table
/// is generated and cannot be referenced from a `const` here.
fn pd_enemy_roster() -> Vec<(crate::combat::config::WeaponStats, bool)> {
    // (PD name, dual-wield?) — chosen to mirror the GoldenEye roster's classes.
    const PICKS: &[(&str, bool)] = &[
        ("AR34", false),        // two-handed rifle
        ("Falcon 2", false),    // one-handed pistol
        ("RC-P120", true),      // dual-wield rifle (akimbo)
        ("Falcon 2", true),     // dual-wield pistols
        ("K7 Avenger", false),  // two-handed rifle
        ("Shotgun (PD)", false), // two-handed
    ];
    let arsenal = crate::combat::arsenal::pd_arsenal();
    PICKS
        .iter()
        .filter_map(|(name, dual)| {
            arsenal.iter().find(|w| w.name == *name).map(|w| (*w, *dual))
        })
        .collect()
}

/// The hunter roster for the live arsenal. Falls back to [`ENEMY_ROSTER`] whenever
/// the GoldenEye weapons are present (`ge` and `both`), so the tuned squad and every
/// test that depends on it are untouched.
pub(crate) fn enemy_roster_for(
    arsenal: crate::combat::Arsenal,
) -> Vec<(crate::combat::config::WeaponStats, bool)> {
    match arsenal {
        crate::combat::Arsenal::PerfectDark => pd_enemy_roster(),
        _ => ENEMY_ROSTER.to_vec(),
    }
}

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

/// Ceiling on the live wave-size dial (`[` / `]`). Not a design limit — spawn pads and
/// the frame budget bite long before this — just a guard so a held key cannot ask for a
/// thousand hunters.
pub const WAVE_SIZE_MAX: usize = 16;

/// Top of the difficulty dial ([`World::difficulty`] runs `0..=DIFFICULTY_MAX`). 0 is
/// the original baseline; DIFFICULTY_MAX is brutal. Driven live with the `=` / `-`
/// keys (see `app.rs`), read into [`DiffParams`] each frame.
pub const DIFFICULTY_MAX: u32 = 10;

/// Difficulty-derived tuning for the current [`World::difficulty`], recomputed on read
/// (cheap). One dial ramps **survivability** (health), **pressure** (speed, cooldown,
/// reaction, perception, cover, flanking, suppression) and **evasion** (dodge). All
/// multipliers are 1.0 / neutral at level 0 and ramp linearly to [`DIFFICULTY_MAX`].
///
/// **Aim is not on this dial.** It used to be — `accuracy_mult` and `falloff_ease` scaled
/// the hit roll — and both are retired with it (§17). How well a hunter shoots is now the
/// Perfect Dark tier its [`crate::pdsim::Simulant`] carries, which the same dial position
/// selects through [`pd_lab::tier_for_dial_frac`]. One dial still, but it picks a
/// zeroing tier instead of multiplying a probability.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DiffParams {
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

/// The **Perfect Dark** body family, loaded after [`BODY_CATALOG`] so PD bodies get
/// their own body ids (and their own GPU meshes) with no special-casing in the
/// renderer. Converted from the ROM by `tools/pd-assets/pd_gltf.py`; see
/// `tools/pd-assets/pd_roster.json` for the body/head pairings and
/// `HANDOFF_PD_ASSETS.md` for the format work behind them.
///
/// They are addressed by the *same* `Bone_1..Bone_15` names as the GoldenEye rig
/// (the exporter renames PD's parts onto the GE roles), so every bone-driven
/// system — weapon attach, head look-at, foot IK, the aim overlay — works on them
/// unchanged.
///
/// **Hunters in the `PD_LAB` wear these**, driven by [`PD_TEMPLATE_CLIPS`] — the
/// same 36-slot layout as the GoldenEye template, filled with Perfect Dark's own
/// animations. Outside the lab a wave draws from [`BODY_CATALOG`] as before;
/// [`World::ge_body_count`] is the boundary and `lifecycle::spawn_wave` picks the
/// side.
/// All six carry Perfect Dark's real textures, from both storage paths: four
/// bodies keep them inline in the model file, everything else indexes PD's
/// compressed global pool (decoded by `tools/pd-assets/pd_tex.py`). Any of the 65
/// shared-rig bodies can be exported this way — this six is a lineup, not a limit.
pub(crate) const PD_BODY_CATALOG: &[(&str, &str)] = &[
    ("pd_joanna", "pd_joanna.glb"),
    ("pd_a51guard", "pd_a51guard.glb"),
    ("pd_elvis", "pd_elvis.glb"),
    ("pd_ddshock", "pd_ddshock.glb"),
    ("pd_cassandra", "pd_cassandra.glb"),
    ("pd_mrblonde", "pd_mrblonde.glb"),
];

/// Which body family a wave draws its hunters from.
///
/// `All` is the game: 44 GoldenEye bodies plus 6 Perfect Dark ones, every one of them on
/// Perfect Dark's animations. The narrower sets exist because the two families still look
/// completely different, and because a checkout without the PD export has to degrade to
/// GoldenEye rather than to an empty hunt.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BodySet {
    #[default]
    All,
    GoldenEye,
    PerfectDark,
}

/// The **Perfect Dark hunter clip set** — the same 36 slots as the GoldenEye
/// template, in the same order, so [`FIRE_RIFLE_IDX`], [`CHAR_HIT_START`] and the
/// death block at `CHAR_HIT_START + HIT_CLIPS.len()` address a PD hunter with the
/// arithmetic they already use. The filenames are numbered because that order is
/// load-bearing: slot `n` is file `n`.
///
/// Which PD animation fills each slot is decided in
/// `tools/pd-assets/pd_roster.json` and re-derivable with
/// `python tools/pd-assets/pd_gltf.py batch tools/pd-assets/pd_roster.json native/assets/enemies/pd`.
///
/// **Perfect Dark and GoldenEye share one animation bank** — Rare carried the GE
/// character animations into PD at the same numbers — so most of these are literally
/// the same animation the GE template loads, decoded from PD's ROM instead.
/// `tools/pd-assets/pd_animmap.py` measures that rather than assuming it: 30 of the
/// 36 match frame-for-frame. The six deaths PD has no counterpart for are filled
/// from `g_DeathAnimsHuman*` (`game/chraction.c:228`), so a PD hunter dies in ways a
/// GoldenEye guard cannot.
///
/// # Which bodies these can drive
///
/// **A GoldenEye body plays this whole set correctly** — measured, in
/// `tests::a_goldeneye_body_can_play_the_perfect_dark_clips`. The two rigs are the same
/// 15 joints under the same names with **identical bind orientations** (0.0 deg apart on
/// every bone); only the rest lengths differ (1.00-1.27x, i.e. body proportions), which a
/// rotation-driven clip is indifferent to -- 38 differently-proportioned GoldenEye bodies
/// already share one clip set. Every slot here, including the appended directional block,
/// binds all 15 bones on all 38 GoldenEye bodies and poses a person.
///
/// (This doc used to claim the opposite -- "PD's bind pose is not GoldenEye's, and driving
/// a PD body with a GE clip produces a confidently-posed wrong figure". The conclusion
/// holds in **one direction only**, and the stated reason was wrong.)
///
/// The direction that really does break is a **GoldenEye clip on a Perfect Dark body**
/// (9 deg mean, 62 deg worst joint-axis error): the PD rig carries 15 extra `Blend_*`
/// joints -- its seam-hiding half-rotation frames -- that a GoldenEye clip has no channels
/// for, so they stay at bind while their owning bones rotate. That asymmetry is what the
/// all-one-family rule in [`World::spawn_family`] is really protecting, and it is why the
/// rule is "one family per wave" rather than "PD clips only".
pub(crate) const PD_TEMPLATE_CLIPS: &[&str] = &[
    // 0–3 locomotion, read by the gait blend.
    "00-idle.glb",
    "01-walk.glb",
    "02-jog.glb",
    "03-run.glb",
    // 4–6 fire, one per weapon class (FIRE_RIFLE_IDX / _PISTOL_ / _DUAL_).
    "04-fire-rifle.glb",
    "05-fire-pistol.glb",
    "06-fire-dual.glb",
    // 7–18 the hit set, positionally parallel to `anim_set::HIT_CLIPS` so
    // `hit_clip_pos` resolves a zone's reaction on either family.
    "07-hit-left-shoulder.glb",
    "08-hit-right-shoulder.glb",
    "09-hit-left-arm.glb",
    "10-hit-right-arm.glb",
    "11-hit-left-hand.glb",
    "12-hit-right-hand.glb",
    "13-hit-left-leg.glb",
    "14-hit-right-leg.glb",
    "15-hit-neck.glb",
    "16-hit-butt-long.glb",
    "17-hit-butt-short.glb",
    "18-hit-taser.glb",
    // 19–35 the death set, parallel to `anim_set::DEATH_CLIPS` (random-picked).
    // `19`, `20`, `23`, `25`, `26`, `27` are Perfect Dark's own — GoldenEye has no
    // equivalent at those ids.
    "19-death-bicep.glb",
    "20-death-thigh.glb",
    "21-death-stagger-back-to-wall.glb",
    "22-death-forward-face-down.glb",
    "23-death-torso-fall.glb",
    "24-death-backward-fall-face-up.glb",
    "25-death-pelvis.glb",
    "26-death-torso-quick.glb",
    "27-death-head-fall.glb",
    "28-death-backward-spin-face-up-left.glb",
    "29-death-forward-face-down-hard.glb",
    "30-death-forward-face-down-soft.glb",
    "31-death-fetal-position-right.glb",
    "32-death-fetal-position-left.glb",
    "33-death-backward-fall-face-up-2.glb",
    "34-death-head.glb",
    "35-death-stomach-long.glb",
    // 36–50 the **directional fire set**: the rest of Perfect Dark's 32-slot
    // per-bearing attack tables, transcribed in [`crate::combat::attack_anim`]. Slots
    // 4–6 above are the forward-facing member of each stance; these are the flanking,
    // sideways and turn-around animations a burst switches to when the hunter starts
    // it facing away from its target. `AttackAnimConfig::slot` is what pins each row
    // to its index here, and `world::tests` asserts the two agree.
    //
    // Appended rather than inserted: the three blocks above are addressed by
    // arithmetic on `FIRE_RIFLE_IDX` / `CHAR_HIT_START` / `+ HIT_CLIPS.len()`, so
    // renumbering in place would silently repoint every reaction table.
    "36-fire-heavy-front-0002.glb",
    "37-fire-heavy-flank-0003.glb",
    "38-fire-heavy-left-0004.glb",
    "39-fire-heavy-behind-0006.glb",
    "40-fire-light-front-0044.glb",
    "41-fire-light-front-0045.glb",
    "42-fire-light-front-0046.glb",
    "43-fire-light-left-0049.glb",
    "44-fire-light-left-004A.glb",
    "45-fire-light-right-0047.glb",
    "46-fire-light-right-0048.glb",
    "47-fire-dual-left-007B.glb",
    "48-fire-dual-left-007D.glb",
    "49-fire-dual-right-007C.glb",
    "50-fire-dual-right-007E.glb",
];

// ─── Spawn points ────────────────────────────────────────────────────────────
/// The **fallback** ingress point (metres), used only when a level has authored no
/// spawn pads at all: the pack floods in here and the player still enters under the
/// fly-cam, which is what this game did before pads existed.
///
/// Perfect Dark guards its spawn selection exactly this way —
/// `if (g_NumSpawnPoints > 0) { ...choose... }` (`playerreset.c:398`) — so an empty
/// pool falling back to a default entry is the ported behaviour, not a workaround. It
/// also keeps every headless caller that predates pads (the AI lab's arenas, the
/// levelgen harness) spawning exactly as before: they author brushes only, so their
/// pool is empty and they take this path.
///
/// Authored pads are the normal case; see `world::tools::spawn_point`.
pub(crate) const SPAWN_MARKER_POS: Vec3 = Vec3::new(3.0, 0.0, 3.0);
/// Radius (m) of the ring bodies cluster into when several draw the **same** pad, so
/// they don't all stack on one cell. (With one pad in the pool this reproduces the old
/// fixed-marker ring exactly.)
const SPAWN_CLUSTER_RADIUS: f32 = 0.7;
/// How far (m) a body entering the level must be from anyone already in it before the
/// spawn point is accepted as-is. Perfect Dark's `chr_adjust_pos_for_spawn` steps aside
/// in 60 cm increments (`chraction.c:15064`), which is that number; a hunter capsule is
/// 0.24 m in radius, so this is comfortably more than two bodies touching. See
/// [`World::spawn_body_clearance`] for why this only ever fires on a padless level.
const SPAWN_BODY_CLEAR: f32 = 0.6;

/// Seconds between a death and the respawn that follows it, both sides.
///
/// Perfect Dark's bots respawn immediately and fade in over `TICKS(120)` — 2 seconds
/// (`aibot->fadeintimer60`, `bot.c:258`) — and in normal multiplayer the player
/// respawns automatically once the death fade completes rather than on a keypress
/// (`dostartnewlife`, `player.c:4596`). 2 s auto for both is that behaviour.
pub(crate) const RESPAWN_DELAY: f32 = 2.0;

/// Kills needed to win a round. First side to this many ends it (see
/// [`World::round_outcome`]); `R` starts the next round. Overridable at launch with
/// `SCORE_LIMIT=n` (`0` = endless, for open-ended playtesting).
pub const SCORE_LIMIT: u32 = 10;

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

// ─── Radar (PD-style lab minimap) ────────────────────────────────────────────
/// Decimation of the nav grid for the radar backdrop (m). The grid is quarter-metre
/// cells; one point per metre reads as a floor plan rather than a smear.
const RADAR_CELL: f32 = 1.0;
/// Half-height (m) of the storey slice the radar draws — floor points and the "same
/// level" blip test. Roughly one storey, so a room above doesn't print through.
const RADAR_FLOOR_BAND: f32 = 2.0;

/// One character on the radar, already projected into the radar's unit disc.
#[derive(Clone, Copy, Debug)]
pub struct RadarBlip {
    /// Index in the hunter roster — matches the `#n` in the simulant overlay.
    pub id: usize,
    /// Position in the radar's frame: player at the origin, `+y` = the way the player
    /// is facing, unit length at the radar's range.
    pub at: Vec2,
    /// Height (m) above (+) or below (−) the player, so a blip on another storey can
    /// be drawn differently instead of lying about where it is.
    pub dy: f32,
    pub dead: bool,
    pub engaged: bool,
    pub firing: bool,
}

/// A frame's radar state — see [`World::radar`].
#[derive(Clone, Debug)]
pub struct RadarView {
    /// The range (m) the disc's edge corresponds to.
    pub range: f32,
    /// Walkable floor on the player's storey, in the radar frame.
    pub floor: Vec<Vec2>,
    pub blips: Vec<RadarBlip>,
}

/// Torso half-width (m) a **PD simulant**'s hitscan must pass within to hit the
/// player (see `World::emit_pd_shot`). Slightly wider than the avoidance radius
/// because it stands in for a whole body silhouette, not a navigation footprint:
/// at a 10 m engagement it makes ~2° of aim error the difference between a hit
/// and a miss, which is the band the difficulty table's degree values live in.
pub(crate) const PD_TORSO_RADIUS: f32 = 0.35;
/// Height (m) a **PD simulant**'s round leaves its barrel at, and the point on a
/// target it is aimed at — PD's `chr_calculate_aimend` drops the aim from the target's
/// origin to its chest (`relaimy -= eyeheight * 0.4`), which is what makes a simulant
/// centre-mass rather than head-hunt. Both sit at roughly chest height on the 1.73 m
/// Perfect Dark body, so a level shot between two standing characters is near-flat and
/// the elevation only becomes interesting across stairs and platforms.
pub(crate) const PD_MUZZLE_HEIGHT: f32 = 1.2;
pub(crate) const PD_TORSO_AIM: f32 = 1.1;
/// The vertical extent (m above the feet) of the torso segment a PD round is tested
/// against — hips to shoulders. Together with [`PD_TORSO_RADIUS`] this is the capsule
/// that decides a hit, so a round that clears the shoulders or passes under the ribs
/// misses. Deliberately excludes the head and legs: PD aims centre-mass, and giving a
/// simulant credit for a clipped ankle would make near-misses land.
pub(crate) const PD_TORSO_LO: f32 = 0.75;
pub(crate) const PD_TORSO_HI: f32 = 1.55;

// ─── PD burst cadence (`FUNCFLAG_BURST3`) ────────────────────────────────────
// Perfect Dark's automatics do **not** hose continuously. Their `funcdef` rows carry
// `FUNCFLAG_BURST3`, and `bot_tick` (`bot.c:3644`) counts rounds against it: three go
// out spaced by `nextbullettimer60 = 5` ticks, then the bot waits
// `botact_get_shoot_interval60` — `unk24 + unk25`, which is `6 + 18 = 24` ticks for
// the AR34, K7 Avenger, CMP150, Callisto and Laptop Gun alike — before the next burst.
//
// This is the mechanism that makes an automatic survivable without making it
// inaccurate. Inside a burst the rounds come *faster* than our old flat cadence; it is
// the gap between bursts that gives the player a window to break line of sight, and
// that makes incoming fire read as a rhythm rather than a wall.
/// Rounds per burst (`FUNCFLAG_BURST3`).
pub(crate) const PD_BURST_ROUNDS: u32 = 3;
/// Seconds between rounds *inside* a burst (`nextbullettimer60 = 5` at 60 Hz).
pub(crate) const PD_BURST_SPACING: f32 = 5.0 / 60.0;
/// Seconds between bursts (`unk24 + unk25 = 24` ticks at 60 Hz).
pub(crate) const PD_BURST_GAP: f32 = 24.0 / 60.0;
/// How long a bot's target must have been out of sight before a **partial** magazine
/// is worth topping up — `lastseenanytarget60 < lvframe60 - TICKS(120)` (`bot.c:2470`).
/// Out of ammo it reloads regardless. See `World::enemy_reload_step`.
pub(crate) const PD_RELOAD_UNSEEN: f32 = 2.0;
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
/// How long a lobbed grenade spends in the air, for the predictive safety guard in
/// `grenade_flush_step`. Measured from the throw tuning rather than guessed: the
/// enemy grenade rides the same `GRENADE` spec the player throws (fuse 3.5 s, but
/// it detonates on impact well before that), and a flat ~1 s covers the arc across
/// the throw range. Erring long is the safe direction — it refuses more throws.
const GRENADE_FLIGHT_SECS: f32 = 1.0;
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

/// Load one character family's clip set into a template mixer, in the caller's
/// **fixed slot order** — locomotion 0–3, then a fire clip per weapon class
/// ([`FIRE_RIFLE_IDX`]…), then the hit set, then the death set. The combat code
/// indexes those slots arithmetically, so the template is **all-or-nothing**: a
/// partial load returns `None` rather than a mixer whose slot 20 is a different
/// animation than the code believes. Every hunter clones the template so it animates
/// on its own clock, while skinning always uses that hunter's OWN body skeleton, so
/// per-body bone lengths are respected.
///
/// `dir` is relative to `native/assets/enemies/`, and `reference` is the body whose
/// skeleton the clips bind against — one per family, because a clip means nothing
/// against the other family's bind pose (see [`PD_TEMPLATE_CLIPS`]).
fn load_anim_template(
    family: &str,
    dir: &str,
    files: &[&str],
    reference: &SkinnedModel,
) -> Option<AnimPlayer> {
    let mut clips = Vec::with_capacity(files.len());
    for f in files {
        let path = format!("{}/../../assets/enemies/{dir}/{f}", env!("CARGO_MANIFEST_DIR"));
        match clip::load(&path, &reference.skeleton) {
            Ok(c) => clips.push(c),
            Err(e) => log::warn!("{family} clip {f} load failed: {e}"),
        }
    }
    if clips.len() != files.len() {
        log::warn!(
            "{family}: only {}/{} clips loaded — that family's hunters are disabled \
             (the fixed slot layout can't be honoured with a gap in it)",
            clips.len(),
            files.len()
        );
        return None;
    }
    log::info!(
        "loaded {} {family} character clips (idle/walk/jog/run + rifle/pistol/dual fire \
         + {} hit + {} death)",
        clips.len(),
        anim_set::HIT_CLIPS.len(),
        anim_set::DEATH_CLIPS.len()
    );
    Some(AnimPlayer::new(clips, 0))
}

/// Load a weapon's `(gun, muzzle-flash)` CPU meshes from its config, resolving the
/// asset-relative paths under `native/assets/weapons/`. Warn-not-panic: a failed
/// load (or a weapon with no muzzle, like the sniper — `muzzle_path == ""`) yields
/// `None` for that slot, and the renderer simply hides whatever is missing. Used at
/// startup for the initial weapon and on every `Q`/`A` weapon switch.
fn load_weapon_models(cfg: &crate::combat::config::WeaponStats) -> (Option<TexturedModel>, Option<TexturedModel>) {
    let asset = |rel: &str| format!("{}/../../assets/weapons/{}", env!("CARGO_MANIFEST_DIR"), rel);
    // The unarmed slot has no mesh by design (`config::UNARMED`), so it takes the
    // no-model path rather than the failed-load one — otherwise equipping empty hands
    // (at startup, and on every death) would warn about an asset nobody authored.
    if cfg.gun_path.is_empty() {
        return (None, None);
    }
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
///
/// **Kept at 3 WT deliberately.** Measured against the thirteen door GLBs, a 3×7 carve
/// has aspect ratio 0.429 and the person-sized door models span 0.346–0.528 with a
/// median of **0.440** — so the existing opening is already the best single-leaf match
/// available. Widening it by one wall (4 WT → 0.571) would make it wider than *every*
/// door model, so every door would leave a gap. What the eye reads as "the door is too
/// wide for the hole" is the widest model (`wooden_door`, 0.528) not being fitted to
/// the carve, which `door_fit_scale` now does.
const DOOR_WIDTH: f32 = 3.0;
const DOOR_HEIGHT: f32 = 7.0;

/// Double-door opening width in WT — exactly twice [`DOOR_WIDTH`], so each of the two
/// leaves fills a 3×7 half and keeps the same 0.429 ratio a single door has. (The
/// `glass_door` asset is itself ratio 0.195, about half the others: it is a double-door
/// leaf, which is independent evidence that this is the right pairing.)
const DOOR_WIDTH_DOUBLE: f32 = DOOR_WIDTH * 2.0;

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
/// Width range (WT) the simple-stair tool's Shift+scroll spans. The lower bound is
/// 3 WT because the player capsule's radius is 1 WT (`character::RADIUS`): a 2 WT
/// flight is exactly capsule-width, and the block's own side walls are solid, so
/// anything narrower is a staircase nobody can walk up.
const SIMPLE_STAIR_WIDTH_MIN: f32 = 3.0;
const SIMPLE_STAIR_WIDTH_MAX: f32 = 12.0;

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

/// The freeform draw tool's phase (see `world::tools::draw`). `None` on [`World`] =
/// the tool is off entirely; `Some(_)` = armed. Esc walks back down this ladder one
/// rung (and one vertex) at a time, following [`World::platform_escape`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DrawPhase {
    /// Armed, nothing drawn — the crosshair previews the first vertex and which face
    /// the polygon would be drawn on.
    Idle,
    /// At least one vertex down; the crosshair previews the next axis-locked segment.
    /// Clicking the first vertex again closes the loop.
    Drawing,
    /// The loop is closed and decomposed into rectangles; the scroll wheel sets the
    /// signed depth (out of the face / into it) and a click commits.
    Depth,
}

/// The **surface** a freeform polygon is being drawn on, resolved once at the first
/// click and then **frozen**.
///
/// Frozen rather than re-picked per frame because subsequent vertices project onto
/// this stored plane by ray/plane intersection: re-picking would let the polygon jump
/// to a different face mid-draw the moment the crosshair crossed an edge, and would
/// lose the plane entirely whenever the ray pointed out of an opening.
///
/// A surface, not *a brush's face*. One continuous floor or wall is routinely made of
/// several brushes — pushing a wall out to enlarge a room, or carving an adjoining
/// area, leaves a seam the player can't see but which a per-brush face rect would stop
/// the tool dead at. So this holds the whole coplanar, co-facing, contiguous group
/// (see `draw::coplanar_face_group`): `rects` are its members and the `u/v_min/max`
/// bounds are their union.
#[derive(Clone, Debug)]
pub(crate) struct DrawFace {
    /// The region the picked brush belongs to; the emitted brushes join it. Every group
    /// member is necessarily in the same region — coplanar faces that overlap or touch
    /// in-plane also overlap in 3D, so `brushes_overlap_or_touch` always clusters them
    /// together.
    pub(crate) region_id: u32,
    pub(crate) axis: Axis,
    pub(crate) side: Side,
    /// The face plane's coordinate along `axis`, in WT.
    pub(crate) position: f32,
    /// The face's two in-plane axes. Per [`Axis::orthogonals`], `v_axis` is world-up
    /// Y for both wall orientations and `u_axis` is never Y.
    pub(crate) u_axis: Axis,
    pub(crate) v_axis: Axis,
    /// Union bounding box of the whole group, in integer WT — what the crosshair is
    /// clamped to, so drawing ranges over every brush in the surface.
    pub(crate) u_min: i32,
    pub(crate) u_max: i32,
    pub(crate) v_min: i32,
    pub(crate) v_max: i32,
    /// Each group member's in-plane rect `[u0, u1, v0, v1]` in integer WT. The union of
    /// these is the *real* surface, which for an L-shaped group is smaller than the
    /// bounding box — so this, not the bbox, is what the built shape gets masked to.
    pub(crate) rects: Vec<[i32; 4]>,
    /// The picked brush's texture scheme, inherited by everything the tool emits so
    /// drawn geometry wears the room's texture rather than the default. Taken from the
    /// brush the crosshair actually hit and applied uniformly, so a shape spanning a
    /// seam between two differently-schemed brushes reads as one thing.
    pub(crate) scheme: usize,
}

impl DrawFace {
    /// Whether the unit WT cell at `(u, v)` lies on the real surface — i.e. inside some
    /// group member, not merely inside the union bounding box. Members are integer-
    /// aligned, so a cell can never straddle two of them and this needs no tolerance.
    pub(crate) fn covers_cell(&self, u: i32, v: i32) -> bool {
        self.rects
            .iter()
            .any(|&[u0, u1, v0, v1]| u >= u0 && u < u1 && v >= v0 && v < v1)
    }
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
    /// The pierced wall brush's texture theme, worn by both the frame and the
    /// protoroom beyond it (see [`World::cut_opening`]).
    scheme: usize,
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
/// face-plane coord on the normal axis, plus the owning brush's texture theme.
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
    /// The owning brush's texture theme, so anything spawned off this face can
    /// inherit it (see [`World::create_sub_face_brush`]).
    scheme: usize,
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
    /// Counts down from [`RESPAWN_DELAY`] once this hunter is killed; at 0 it respawns
    /// **into this same slot** (see [`World::respawn_hunter`]). `None` while alive.
    ///
    /// The slot has to be reused rather than a fresh instance pushed: the roster index is
    /// load-bearing well past the roster — `pd_lab::PdTarget::Hunter(i)`,
    /// [`Self::pd_target`], the ORCA agent tags, squad alert, and every AI-lab metric key
    /// off it — so renumbering mid-fight would silently repoint all of them.
    pub respawn_timer: Option<f32>,
    /// Enemy-fire cadence: seconds until the next shot may leave during the fire
    /// window (spaced by `1/weapon.fire_rate`, or by the PD burst cadence).
    pub shot_timer: f32,
    /// **Rounds left in the magazine** — `aibot->loadedammo[HAND_RIGHT]`. Only PD's
    /// reload rule (`AI=pd`, see [`World::enemy_reload_step`]) reads or writes it; under
    /// `AI=ours` a hunter has never had a magazine and still does not, so it sits at the
    /// weapon's clip size and nothing consults it.
    pub loaded: u32,
    /// **Spare rounds** this hunter can reload from — its equivalent of the player's
    /// reserve.
    ///
    /// New with pickups, and the reason hunters care about ammo crates at all: a
    /// hunter used to have unlimited magazines (like a Perfect Dark bot, whose
    /// `botact_reload` just refills), so an ammo crate would have been worthless to it
    /// and "prioritise picking up ammo" would have had nothing to prioritise. A hunter
    /// with an empty magazine AND an empty reserve is **dry** — it stops fighting and
    /// goes looking, exactly like an unarmed one.
    pub reserve: u32,
    /// Seconds left of a scheduled reload (`aibot->timeuntilreload60`), or 0. While it
    /// runs the hunter holds fire, and it refills [`Self::loaded`] when it lapses.
    pub reload_timer: f32,
    /// Rounds already sent in the current PD burst (`aibot->burstsdone`). Counts up to
    /// [`PD_BURST_ROUNDS`], then the next shot waits [`PD_BURST_GAP`] and it resets.
    /// PD simulants with an automatic weapon only; zero everywhere else.
    pub burst_shot: u32,
    /// Whether this hunter is firing its weapon's **secondary** function.
    ///
    /// Decided once when a burst starts ([`World::start_enemy_fire`]) rather than
    /// per frame, from PD's own engagement bands and per-function scores — so a
    /// hunter commits to a choice for the burst instead of flickering between two
    /// functions while the range wobbles. Always `false` on the GoldenEye arsenal.
    pub use_secondary: bool,
    /// Active fire burst: `Some(elapsed_seconds)` while a burst is running, `None`
    /// otherwise. Firing is now a **timer** (not a full-body clip), so the hunter
    /// keeps running its locomotion + procedural aim while shooting.
    pub fire_elapsed: Option<f32>,
    /// When this hunter may aim, shoot and recoil within that burst, and how far its
    /// chest may swing. **Perfect Dark's authored `attackanimconfig` row** for a PD
    /// hunter, the legacy `FIRE_TIMING` guess for a GoldenEye one. See
    /// [`crate::combat::attack_anim`].
    ///
    /// A GoldenEye hunter resolves this once at spawn and keeps it. A Perfect Dark one
    /// **re-resolves it at the start of every burst**, because PD picks the attack
    /// animation from the bearing to its target (`chr_attack`) — see
    /// [`World::start_enemy_fire`].
    pub fire: crate::combat::attack_anim::FireTiming,
    /// Chest-aim calibration per fire clip: `(template slot, barrel-forward in the
    /// chest's local frame)`, measured against **this hunter's own body** at spawn.
    ///
    /// The chest-aim layer needs to know which way the barrel points in the pose it is
    /// swinging, and every fire animation holds the gun differently — that measurement
    /// is what stops an authored aim bias from becoming a permanent one (see
    /// `EnemyArm::barrel_forward_in_chest`). One clip needed one number; the
    /// directional set needs one per clip, and they are all taken up front so a burst
    /// starting mid-fight costs no pose evaluation. Empty for a GoldenEye hunter, whose
    /// clip never changes.
    pub fire_axes: Vec<(usize, Vec3)>,
    /// Whether this hunter's clips are the **Perfect Dark** set, and so whether its
    /// hit/death reactions come from PD's per-hit-part tables
    /// ([`crate::combat::hit_anim`]) rather than our height-zone random pick. Set at
    /// spawn from the wave's family — a body and its animation data travel together.
    pub pd_anims: bool,
    /// Which body part the last shot landed on, for the reaction tables. `None`
    /// before the first hit, or when the impact resolved to no anatomical bone.
    pub hit_part: Option<crate::combat::hit_anim::HitPart>,
    /// Frames of the playing death animation at which the body strikes the floor
    /// (`thudframe1`/`thudframe2`, `-1` for none), and whether each has been played
    /// yet this death. Set when an authored death starts.
    pub thud: Option<(f32, f32)>,
    pub thud_played: [bool; 2],
    /// Muzzle-flash countdown (s); >0 → this hunter's muzzle(s) render.
    pub muzzle_timer: f32,
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
    /// **PD simulant model** (`PD_LAB=1` only, see [`pd_lab`]). When present this
    /// hunter aims and shoots the Perfect Dark way — body yaw carries a live aim
    /// error and shots are real hitscans rather than probability rolls. `None` on
    /// every normal hunter, which keeps the shipping game byte-identical.
    pub pdsim: Option<crate::pdsim::Simulant>,
    /// Last frame's debug snapshot for the lab overlay, or `None` when not a
    /// simulant. Diagnostic only — nothing reads it back into the sim.
    pub pd_debug: Option<pd_lab::PdDebug>,
    /// Who this simulant currently intends to shoot — the player, or another hunter.
    /// Diagnostic (the debug overlay) and the seam a full free-for-all would grow
    /// from: the FSM still only knows how to *move* toward the player, so today a
    /// simulant that picks a packmate shoots it where it stands rather than hunting it.
    pub pd_target: Option<pd_lab::PdTarget>,
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
    /// The **muzzle-flash mesh centroid** in the gun model's local space — a real point
    /// down the barrel — or `None` for the five weapons that ship no flash mesh
    /// (sniper, rocket launcher, hand grenade, the three mines).
    ///
    /// Provenance for [`Self::barrel`]: `Some` means this weapon measured its own axis,
    /// `None` means it inherited [`BARREL_MODEL_AXIS`]. Nothing at runtime needs it —
    /// the axis is resolved once at load — but the regression test that keeps the
    /// rocket launcher pointing forwards asserts on which branch each weapon took, and
    /// dropping the field would leave that untestable.
    #[cfg_attr(not(test), allow(dead_code))]
    pub muzzle_offset: Option<Vec3>,
    /// Barrel-forward axis in gun-model space. Resolved once at load; see
    /// [`resolve_barrel_axis`] for why it is not a mesh statistic.
    barrel: Vec3,
}

impl EnemyWeaponAsset {
    /// Barrel-forward axis (gun-model space) — what the chest-aim layer swings so this
    /// gun's barrel points at its target.
    pub(crate) fn barrel_axis(&self) -> Vec3 {
        self.barrel
    }
}

/// The barrel-forward axis for a weapon, in the gun model's own space.
///
/// # This cannot be derived from the gun mesh, and used to be
///
/// The old rule was "normalise the muzzle-flash centroid, or the **gun** centroid when
/// there is no flash mesh". The first half is sound; the second was wrong, and wrong in
/// a way that only showed on the five weapons with no flash. Measured across the
/// arsenal it put the sniper 22° high and the rocket launcher **backwards**
/// (`(-0.06, -0.03, -0.998)`).
///
/// The reason is that a gun mesh's geometry says nothing about which end the muzzle is.
/// The eighteen weapons that *do* carry a flash mesh give the ground truth — all of
/// them resolve to `+Z` within 4° — and against that ground truth every mesh-derived
/// estimator fails on a good third of them:
///
/// | estimator | worst error on a known-answer gun |
/// |---|---|
/// | gun-mesh centroid (the old fallback) | 176° (Moonraker Laser) |
/// | furthest vertex from the origin | 178° (Phantom) |
/// | longest bounding-box axis, signed by the centroid | 180° (six guns) |
/// | furthest vertex along `+Z` | 91° (Moonraker Laser) |
///
/// Because the models are not consistently placed relative to their origin: the DD44
/// occupies `z ∈ [−269, 0.2]` while its flash sits at `z = +64`, so it is modelled
/// entirely *behind* a muzzle-at-the-origin, and the AR33 is modelled entirely in front
/// of a grip-at-the-origin. Both point `+Z`. Nothing about the vertices distinguishes
/// those two cases.
///
/// # What Perfect Dark does
///
/// It does not derive a direction from the gun either. `chr_calculate_aimend`
/// (`chraction.c:9200`) reads a **named node** for the muzzle *position* —
/// `MODELPART_CHRGUN_GUNFIRE`, falling back to `MODELPART_CHRGUN_0001` — and takes the
/// firing *direction* from the character's own aim angle (`sinf(aimangle)`,
/// `chraction.c:9254`), using the muzzle point only to offset the ray's origin. The gun
/// model is authored to point along the aim; it is never asked which way it faces.
///
/// So the axis here is a **convention of the asset set**, evidenced by the eighteen
/// weapons that declare it, and this measures it where it is declared and applies the
/// convention where it is not — rather than inventing a statistic that happens to look
/// right on the gun someone last checked.
fn resolve_barrel_axis(name: &str, muzzle_offset: Option<Vec3>) -> Vec3 {
    // A Perfect Dark weapon does not need this measured at all: its third-person
    // model carries the authored `CHRGUNFIRE` node, which is the same point
    // `chr_get_gun_pos` uses for the shot ray, so flash and bullet agree by
    // construction. Checked FIRST, because an authored answer outranks a
    // convention inferred from eighteen meshes agreeing.
    if let Some(pd) = crate::combat::arsenal::pd_weapon_for(name) {
        if pd.muzzle_is_authored {
            let a = Vec3::from(pd.tp_muzzle).normalize_or_zero();
            if a != Vec3::ZERO {
                return a;
            }
        }
        // No CHRGUNFIRE authored (17 of the 33) — PD fires from the grip, so
        // there is no barrel offset to recover and the model convention is the
        // honest fallback. See `pd_gltf.py`'s `gun_metadata`.
        log::debug!("{name}: PD weapon with no authored muzzle — firing from the grip");
        return crate::combat::arsenal::PD_BARREL_AXIS;
    }
    match muzzle_offset.map(|o| o.normalize_or_zero()) {
        Some(a) if a != Vec3::ZERO => a,
        _ => {
            log::debug!("{name} has no muzzle-flash mesh — barrel axis from the asset-set convention");
            BARREL_MODEL_AXIS
        }
    }
}

/// The muzzle-flash mesh centroid in gun-model space (a point down the barrel), or
/// `None` when the weapon ships no flash mesh. Deliberately **not** falling back to the
/// gun mesh — see [`resolve_barrel_axis`].
fn mesh_muzzle_offset(muzzle: &Option<TexturedModel>) -> Option<Vec3> {
    let model = muzzle.as_ref()?;
    if model.vertices.is_empty() {
        return None;
    }
    let n = model.vertices.len() as f32;
    Some(model.vertices.iter().fold(Vec3::ZERO, |a, v| a + Vec3::from(v.pos)) / n)
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
    /// Level-wide ambient fill light (colour + strength). A global, not an entity —
    /// ambient has no position. Persisted in the level file; edited in the OBJECTS
    /// panel's LEVEL LIGHTING section. See [`crate::ecs::AmbientSettings`].
    ambient: crate::ecs::AmbientSettings,
    /// This level's number-key → theme-name bindings, for quick per-room retexturing.
    ///
    /// Per **level** rather than global because the useful nine differ completely
    /// between a bunker and a jungle, and the library now runs to hundreds of themes.
    /// Stored by theme *name* like everything else that references a theme (see
    /// `Brush::scheme`), and consulted ahead of the manifest's own `key` — an unbound
    /// digit falls through to whatever `themes.json` says.
    theme_hotkeys: std::collections::BTreeMap<char, String>,
    /// The player capsule; `Some` only in HUNT mode.
    character: Option<CharacterController>,
    /// Baked nav grid; `Some` only in HUNT mode.
    nav: Option<NavWorld>,
    /// Last result of the NAV tab's **Calculate** (`world::nav_issues`), or `None` until
    /// it is pressed. Cached rather than live because the grid bake it needs costs ~0.5 s
    /// on a real level — far too slow to re-run per edit.
    nav_issues: Option<NavIssues>,
    /// The walkable-component overlay mesh built alongside those findings.
    nav_overlay: Option<ColoredMesh>,
    /// Whether that overlay is drawn. Separate from the findings on purpose: you leave it
    /// on, edit the geometry, and re-Calculate to see whether the island closed.
    nav_overlay_on: bool,
    /// Bumped on every change to the overlay. It runs to ~90k vertices, so the app
    /// uploads on a revision change rather than rebuilding a GPU buffer every frame.
    nav_overlay_rev: u32,
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
    /// The **Perfect Dark** counterpart of [`Self::char_anim_template`], filling the
    /// same 36 slots in the same order with PD's own animations (see
    /// [`PD_TEMPLATE_CLIPS`]). A hunter wearing a PD body clones this one instead.
    ///
    /// It has to be a separate template rather than a retarget: a clip stores each
    /// joint's *absolute local* rotation, so it only means what it should against the
    /// bind orientation it was authored for, and PD's bind pose is not GoldenEye's.
    /// Driving a PD body with a GE clip produces a confidently-posed wrong figure.
    /// `None` if any PD clip failed to load, which falls the lab back to GE bodies.
    pd_anim_template: Option<AnimPlayer>,
    /// The **Perfect Dark clip set bound to a GoldenEye skeleton** — the same
    /// [`PD_TEMPLATE_CLIPS`] files as [`Self::pd_anim_template`], re-bound against body 0.
    ///
    /// A clip's channels bind to one skeleton, so the same animation data needs one
    /// template per rig. Both are the PD set, which is what puts every hunter on Perfect
    /// Dark's animations regardless of which body it wears. See [`Self::body_clips`].
    pd_anim_template_ge: Option<AnimPlayer>,
    /// xorshift state for the hit/death/pain random picks (no `rand` dep).
    char_rng: u64,
    /// A **separate** xorshift state for spawn-pad selection (`world::spawn`).
    ///
    /// Deliberately not `char_rng`: spawn choice is a roll, and sharing the combat
    /// stream would mean every spawn shifted every subsequent hit/pain/reaction draw.
    /// That is not a hypothetical — folding the spawn roll into `char_rng` moved the
    /// PD half-clip-reload lab scenario off its boundary and failed it, without any
    /// change to the behaviour it measures. Two streams keep combat outcomes
    /// reproducible independently of how many bodies have entered the level.
    spawn_rng: u64,
    /// Difficulty dial, `0..=DIFFICULTY_MAX` (0 = original baseline). Cranked live with
    /// the `=` / `-` keys; drives [`DiffParams`] for enemy lethality/health/evasion.
    difficulty: u32,
    /// How many hunters the next HUNT floods in (default [`ENEMY_COUNT`] = 1, "duel
    /// mode"). A runtime field so tests can spawn a pack for multi-hunter behaviours.
    wave_size: usize,
    /// Whether a **Perfect Dark** hunter reacts to hits and death with PD's authored
    /// per-hit-part animation tables ([`crate::combat::hit_anim`]) rather than with
    /// the physics ragdoll. Default ON — the tables are the reason the animations
    /// were ported, and a ragdoll discards them. Turning it off is the A/B: PD
    /// hunters fall back to the ragdoll like GoldenEye ones.
    authored_reactions: bool,
    /// **How hunters fight** — Perfect Dark's bot model, on every hunter, always. Every
    /// hunter spawns carrying a [`crate::pdsim::Simulant`], wears a Perfect Dark body
    /// driven by [`PD_TEMPLATE_CLIPS`], and aims / shoots the Perfect Dark way; see
    /// [`pd_lab`] for exactly what that replaces.
    ///
    /// This used to be `Option<PdLabConfig>` — `None` in the normal game, `Some` under
    /// `PD_LAB=1` — which is what made the whole track a spike. It is the game's AI now.
    pd: pd_lab::PdHunters,
    /// Whether the **lab** is on (`PD_LAB=1`): the bare test room and the per-simulant
    /// debug overlay. Not a gate on the AI any more — only on the instrumentation.
    pd_lab: bool,
    /// Which bodies a wave draws from. `All` — both families — is the default; the
    /// narrower sets are for A/B and for the assets-missing fallback. See
    /// [`Self::wave_bodies`].
    body_set: BodySet,
    /// Drive GoldenEye-bodied hunters with the **legacy GoldenEye clip set** instead of
    /// the Perfect Dark one.
    ///
    /// The only way to reach the pre-promotion animation paths — the hand-set
    /// `FIRE_TIMING` windows, the height-zone random hit pick, the canned flinch — now
    /// that Perfect Dark's clips drive every body. Those paths have tests that A/B them
    /// against the Perfect Dark ones, so the other side of the comparison has to stay
    /// reachable. Ignored for a Perfect Dark body, where a GoldenEye clip is invalid.
    /// See [`Self::body_clips`].
    goldeneye_clips: bool,
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
    /// Whether **PD-lab hunters are omniscient** — Perfect Dark's knowledge rule: they
    /// always know where the player is and navigate to the live position instead of a
    /// last-known one, so breaking line-of-sight no longer sends them fan-out searching.
    /// **On by default**, and PD-lab-only (a GoldenEye hunter is never affected, so the
    /// normal game is unchanged). A kill-switch / A-B baseline; see
    /// [`crate::enemy::Enemy::known_player_pos`] for what it does and does *not* change.
    pd_omniscience: bool,
    /// **Which engagement model the hunters run** (`AI=pd|ours`, default ours). Unlike
    /// the flags above this is not a kill-switch but a full A/B: `ours` is everything
    /// this file has grown, `pd` is Perfect Dark's deathmatch simulant. Resolved from
    /// the environment in [`Self::new`] and re-applied last at boot (`app.rs`) so an
    /// explicit choice outranks any mode default. See [`crate::enemy::AiMode`].
    ai_mode: crate::enemy::AiMode,
    /// Whether hunters may lob a **grenade to flush a camping player** (`#5`).
    ///
    /// **OFF by default (2026-08-17).** Turned off from playtest: hunters were killing
    /// themselves and each other with it. The self-kill is structural, not a tuning
    /// miss — [`Self::grenade_flush_step`] checks that no hunter is within
    /// [`GRENADE_SAFE_DIST`] of the camp spot *at the moment of the throw*, but the
    /// round then spends about a second in the air while the pack keeps closing on the
    /// player at up to [`crate::enemy::SPEED_CHASE`]. 6.5 m of clearance is under 1.5 s
    /// of running, and the blast is 4 m across, so a hunter that was clear on release
    /// is regularly inside the blast on impact. PD omniscience made this worse by
    /// guaranteeing the whole pack converges instead of some of it wandering off.
    ///
    /// Re-enable with [`Self::set_grenades`] once the throw predicts where the pack
    /// will *be* (or simply refuses while anyone is inbound to the camp spot).
    grenades: bool,
    /// Decimated walkable floor, for the radar's level backdrop. Baked once on entering
    /// HUNT (see [`Self::bake_radar_cells`]) rather than re-derived per frame, because
    /// `NavWorld::all_standable` walks the whole grid and allocates.
    radar_cells: Vec<Vec3>,
    /// Per-body world-space Y offset that seats that body's feet on the floor
    /// (parallel to [`Self::char_models`]). Computed from the **lowest skinned point
    /// of the actual idle pose** for each body (the bind-pose AABB can't be used —
    /// the bind pose is a splayed star with the feet spread high, so seating by it
    /// leaves the standing pose sunk). Bodies differ in height (e.g. Jaws), so this
    /// is per-body.
    /// Per body, the feet-seating offset for `[Perfect Dark clips, GoldenEye clips]` —
    /// see the sweep in [`Self::new`] for why one number per body is not enough.
    char_feet_offset: Vec<[f32; 2]>,
    /// Per-body standing height in metres, measured over the same idle sweep
    /// (parallel to [`Self::char_models`]). A GoldenEye body is ~1.50 m and a Perfect
    /// Dark one ~1.73 m, so hit capsules and hit-zone boundaries are derived from
    /// this rather than assumed — see [`Self::body_capsule`].
    char_height: Vec<f32>,
    /// The resolved gun-arm chain + rest geometry per body (parallel to
    /// [`Self::char_models`]), used to build each hunter's procedural aim/recoil
    /// stack. Per-body because bind poses (bone lengths) differ. An entry is `None`
    /// if that body's skeleton has no resolvable arm chain.
    enemy_arm: Vec<Option<EnemyArm>>,
    /// Spike: the optional BUILD-phase procedural-anim preview character (`Y`).
    /// `None` unless the preview is toggled on. See [`world::spike_preview`].
    procedural_preview: Option<spike_preview::ProceduralPreview>,
    /// How many of [`Self::char_models`] are GoldenEye bodies. Body ids below this
    /// are GoldenEye bodies ([`BODY_CATALOG`]); ids at or above it are the Perfect
    /// Dark family ([`PD_BODY_CATALOG`]). Which side a wave draws from is decided by
    /// [`Self::pd_lab`] — see `lifecycle::spawn_wave`.
    ge_body_count: usize,
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
    /// Which arsenal is live — GoldenEye's 23, Perfect Dark's 33, or both.
    /// Resolved once in [`Self::new`] from `ARSENAL=` and then the single source of
    /// truth for the weapon list, so the shop, the cycle order and the enemy
    /// weapon library cannot disagree about what index means what.
    arsenal: crate::combat::Arsenal,
    /// The player's weapon inventory (JS `WeaponSystem.slots`) — one [`Weapon`]
    /// per [`Self::arsenal`] entry, each keeping its own ammo/reload state so a
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
    /// The player's kill/death tally for the current round. See `world::scoreboard`.
    player_score: Score,
    /// Per-roster-slot hunter tallies, sized to the wave at spawn. Held here rather than
    /// on the `EnemyInstance` precisely because a respawn rebuilds the instance in place
    /// — a score on the instance would be wiped by every death.
    hunter_scores: Vec<Score>,
    /// Kills needed to win the round; 0 = endless. Defaults to [`SCORE_LIMIT`], and the
    /// launcher honours a `SCORE_LIMIT=n` env override.
    score_limit: u32,
    /// Latched once a side reaches [`Self::score_limit`]; `None` while the round is live.
    /// Freezes the sim behind the round-over screen (`R` starts the next round).
    round_over: Option<RoundOutcome>,
    /// Counts down from [`RESPAWN_DELAY`] while the player is dead; at 0 the player
    /// respawns from the pool. The death beat, not a game-over.
    player_respawn: f32,
    /// The live spawn pool for this hunt: every authored pad resolved to a standable
    /// nav cell (PD's `chr_adjust_pos_for_spawn` step, done once at pool-build time).
    /// **One shared pool** — the player and the simulants both draw from it, as in
    /// Perfect Dark. Never empty during HUNT: with no pads authored it holds one
    /// synthetic pad at [`SPAWN_MARKER_POS`], which reproduces the old fixed-ingress
    /// behaviour exactly. Built by [`World::prepare_spawn`], cleared on return to BUILD.
    spawn_pads: Vec<spawn::SpawnPad>,
    /// The wave's reference point: the first resolved pad (or the fallback marker).
    /// Only two things still read it — the fan-out search-pool seed and
    /// [`World::pick_search_point`]'s empty-pool fallback — since *where a body enters*
    /// is now per-body ([`World::spawn_pads`]). HUNT only.
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
    /// The settings the **next** placed pickup gets — the panel's draft (which
    /// weapon, how many magazines, how long until it returns). Authoring state, not
    /// persisted; each placed pickup carries its own copy. See
    /// `world::tools::pickup`.
    pickup_draft: crate::ecs::Pickup,
    /// Shared hover/spin clock for the weapon pickups, advanced per render frame.
    /// One clock rather than per-entity phase state: the phase is derived from each
    /// pickup's position, so this stays a single float no matter how many are placed.
    pickup_clock: f32,
    /// What the player last collected + how long the HUD still shows it.
    pickup_message: String,
    pickup_message_timer: f32,
    /// Whether `OWN_ALL=1` unlocked the whole arsenal. Remembered because it is also
    /// the exemption from the on-death loadout reset (see `pickup::reset_loadout`).
    own_all: bool,
    /// Whether hunters spawn **empty-handed** and have to find a gun (the default,
    /// and the deathmatch rule the player plays by). The kill-switch, off, restores
    /// hunters that spawn holding their roster weapon with spare magazines — which is
    /// what the AI-lab arenas and the combat tests are calibrated against, since a
    /// hunter that has to go shopping first never reaches the behaviour they measure.
    /// `ARMED_HUNTERS=1` turns it off for a playtest.
    unarmed_hunters: bool,
    /// Live destructible-prop colliders → their prop entity (Milestone 3). Baked at
    /// BUILD→HUNT from every authored destructible prop; a hitscan hit on one of these
    /// handles routes damage to the mapped entity. An entry is removed as its prop is
    /// destroyed, and the whole map is cleared on return to BUILD. HUNT-only.
    prop_colliders: std::collections::HashMap<ColliderHandle, hecs::Entity>,
    /// Live openable-door panel colliders → their door entity. Baked at BUILD→HUNT
    /// alongside `prop_colliders` and cleared on return to BUILD; unlike a prop
    /// collider, a door's is re-posed every step as the panel swings.
    door_entities: std::collections::HashMap<ColliderHandle, hecs::Entity>,
    /// Whether the door tool cuts a **double**-width opening (scroll toggles it while
    /// the tool is armed). Authoring state, not persisted.
    door_double: bool,
    /// Whether hunters spawn at all (dev toggle, `J`). Off lets you author and test the
    /// level — doors especially — without being hunted while you do it.
    hunters_enabled: bool,
    /// Which prop gizmo is active (Translate / Rotate); cycled with Tab.
    prop_gizmo_mode: PropGizmoMode,
    /// The in-progress prop gizmo drag, if any.
    prop_gizmo_drag: Option<PropGizmoDrag>,
    /// Point-light placement tool armed (the OBJECTS panel's Place Light), BUILD only.
    /// While set, a marker ghost tracks the cursor floor-pick and a left-click authors
    /// a light entity. Mutually exclusive with prop placement. See `world::tools::light`.
    light_tool: bool,
    /// The metric point the light-placement ghost currently previews (floor pick +
    /// fixed height); what a confirm places the light at.
    light_preview_pos: Option<Vec3>,
    /// Spawn-pad placement tool armed (the SPAWNS panel's Place Spawn Point), BUILD
    /// only. While set, a floor marker ghost tracks the cursor and a left-click authors
    /// a pad entity facing the fly-cam. Mutually exclusive with the other placeables.
    /// See `world::tools::spawn_point`.
    spawn_tool: bool,
    /// The metric floor point the pad-placement ghost currently previews; what a
    /// confirm places the pad at.
    spawn_preview: Option<Vec3>,
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

    // ─── Freeform draw tool (see `world::tools::draw`) ────────────────────────
    /// The draw tool's phase, or `None` when the tool is off. Mutually exclusive
    /// with the opening / placement / platform tools.
    draw_phase: Option<DrawPhase>,
    /// The frozen face the in-progress polygon is being drawn on.
    draw_face: Option<DrawFace>,
    /// The committed polygon vertices, in **integer** face-UV WT.
    ///
    /// Integer rather than float on purpose: every vertex is grid-snapped anyway, and
    /// exact arithmetic is what lets the self-intersection test and the rasterizing
    /// decomposition be epsilon-free.
    draw_verts: Vec<(i32, i32)>,
    /// The axis-locked vertex the crosshair is currently over — what the next click
    /// would commit. Recomputed by the per-frame preview.
    draw_cursor: Option<(i32, i32)>,
    /// The rectangles the closed polygon decomposed into, in integer face-UV WT
    /// `(u0, v0, w, h)`. Computed once at close, then reused by both the ghost and the
    /// commit so the two can't disagree.
    draw_rects: Vec<(i32, i32, i32, i32)>,
    /// Which of the crosshair's candidate surfaces the first corner will land on — the
    /// author's cycle offset, advanced by the scroll wheel while the tool is idle.
    ///
    /// On a flat face there is one candidate and this is inert. On an **edge** two faces
    /// meet and on a **corner** three do; `Axis::dominant` resolves that from whichever
    /// normal the physics engine happened to report, which is arbitrary. Taken modulo the
    /// live candidate count, so index 0 is always physics' own answer and the tool behaves
    /// exactly as before unless the author scrolls.
    draw_candidate: usize,
    /// Signed extrusion depth in WT during [`DrawPhase::Depth`]: positive protrudes
    /// out of the face into the room (`Op::Add`), negative sinks into it
    /// (`Op::Subtract`). Scroll-adjusted in ±1 WT steps.
    draw_depth: f32,

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
    /// Sideways slide of the simple-stair being placed, in WT perpendicular to its
    /// run axis (scroll wheel). Lets a flight be nudged off the line through the two
    /// clicked points without re-picking either endpoint.
    simple_offset_wt: f32,
    /// Width in WT of the simple-stair being placed (Shift+scroll).
    simple_width_wt: f32,
    /// The simple-stair tool's yellow x-ray preview — endpoint marker cubes plus a
    /// ghost of the flight itself — rebuilt each frame by
    /// [`World::update_platform_preview`] and drawn through the stair-ghost channel
    /// (see [`World::stair_preview_mesh`]). Cached on `World` because the render pass
    /// only holds `&World`, while picking the crosshair surface needs `&mut`.
    simple_ghost: Option<CpuMesh>,
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

        // Perfect Dark bodies, appended so they carry ordinary body ids. Everything
        // below body `ge_body_count` is a GoldenEye body a hunter may wear; everything
        // at or above it is PD, driven only by the lab showcase (see `PD_BODY_CATALOG`).
        let ge_body_count = char_models.len();
        for (name, file) in PD_BODY_CATALOG {
            let path = format!(
                "{}/../../assets/enemies/pd/characters/{file}",
                env!("CARGO_MANIFEST_DIR")
            );
            match gltf_skin::load(&path) {
                Ok(m) => {
                    log::info!(
                        "loaded PD character {name}: {} verts, {} joints",
                        m.vertices.len(),
                        m.skeleton.joint_count()
                    );
                    char_models.push(m);
                }
                Err(e) => log::warn!("PD character {name} load failed: {e}"),
            }
        }

        // Load the clip set bound to body 0's skeleton in a FIXED index order —
        // locomotion 0–3, then one fire clip per weapon CLASS (rifle/pistol/dual,
        // indices FIRE_*_IDX), then the hit set, then the death set (see CHAR_*_IDX) —
        // into a template mixer. The rig is identical across all bodies, so this one
        // template drives every body; each spawned hunter clones it so it animates on
        // its own clock, and the BUILD demo clones it too. Skinning always uses the
        // hunter's OWN body skeleton, so per-body bone lengths are respected.
        let ge_clip_files: Vec<&str> = {
            let mut files: Vec<&str> =
                vec!["00-idle.glb", "28-walking.glb", "2A-jogging.glb", "29-running.glb"];
            files.push("01-fire-standing.glb"); // FIRE_RIFLE_IDX
            files.push("41-fire-standing-pistol.glb"); // FIRE_PISTOL_IDX
            files.push("7A-fire-standing-dual-wield.glb"); // FIRE_DUAL_IDX
            files.extend_from_slice(anim_set::HIT_CLIPS);
            files.extend_from_slice(anim_set::DEATH_CLIPS);
            files
        };
        let char_anim_template = char_models
            .first()
            .and_then(|m| load_anim_template("GoldenEye", "animations", &ge_clip_files, m));
        // The Perfect Dark clip set, **bound twice** — once per body family.
        //
        // A clip binds its channels to one skeleton, so a template is per-rig even when
        // the animation data is identical. And the PD set drives *both* rigs correctly:
        // the two are the same 15 joints with the same bind orientations, differing only
        // in bone length, which a rotation clip ignores (measured in
        // `tests::a_goldeneye_body_can_play_the_perfect_dark_clips`).
        //
        // That is what lets the wave draw from all 44 GoldenEye bodies *and* the 6 Perfect
        // Dark ones while every hunter plays Perfect Dark's animations — the directional
        // fire table and the authored per-hit-part reactions included. Before this there
        // was one PD template, bound to a PD rig, so PD animations cost 38 bodies.
        let pd_anim_template = char_models
            .get(ge_body_count)
            .and_then(|m| load_anim_template("Perfect Dark", "pd/animations", PD_TEMPLATE_CLIPS, m));
        let pd_anim_template_ge = char_models
            .first()
            .and_then(|m| load_anim_template("PD-on-GoldenEye", "pd/animations", PD_TEMPLATE_CLIPS, m));

        // Per-body feet seating + gun-arm resolution: bind poses (bone lengths) differ
        // between bodies, so both are computed for EACH body against its own skeleton.
        // Feet: sample the idle across its loop, skin each pose on the CPU, and take
        // the global lowest Y (the most-planted foot); seating that at the floor keeps
        // the feet grounded while the animation's own vertical motion still reads.
        // Falls back to the bind-pose AABB with no idle clip. Arm: resolve the gun-arm
        // chain once per body; each hunter clones a fresh aim/recoil stack from its
        // body's arm at spawn.
        //
        // A PD body is seated by the *PD* idle, not the GE one — its rest pose is a
        // splayed star whose AABB floor is nowhere near its feet, and the GE idle
        // means nothing on it, so seating either way round would bury or float it.
        // **Seated by the clips it will actually play, not by the family it belongs to.**
        //
        // Which matters because the two clip sets do not agree about where the root is.
        // A Perfect Dark clip carries an absolute root translation of 1301.7 units — PD's
        // rest hip height — on top of the animation's own vertical motion; a GoldenEye clip
        // carries 0 and relies on the bind. That pedestal is load-bearing for a PD body
        // (its vertices are stored bone-local, so the root lift is what puts the geometry
        // in place) and pure double-counting on a GoldenEye body (whose vertices are
        // model-space with real inverse-binds). See `tools/pd-assets/pd_gltf.py` on the two
        // conventions.
        //
        // So a GoldenEye body on the Perfect Dark clips floats by exactly that pedestal —
        // 1301.7 units, 1.09 m — which is what the roster widening shipped and a playtest
        // immediately found. There is no single correct offset per body; it depends on the
        // clip set, and both are measured here.
        let ge_idle_clip = char_anim_template.as_ref().and_then(|a| a.clip(0));
        let pd_idle_clip = pd_anim_template.as_ref().and_then(|a| a.clip(0));
        let pd_on_ge_idle_clip = pd_anim_template_ge.as_ref().and_then(|a| a.clip(0));
        // The same sweep also measures each body's **standing height**, because the
        // two families are not the same size — a GoldenEye body renders 1.50 m (they
        // were deliberately shrunk 20%, `CHAR_SCALE = 0.00104 × 0.8`), a Perfect Dark
        // body 1.73 m. Anything sized to a character is derived from this, so a PD
        // hunter's head is inside its own hit capsule instead of 23 cm above a
        // GoldenEye-sized one (see `Self::body_capsule` / `Self::body_hit_zones`).
        let mut char_feet_offset: Vec<[f32; 2]> = Vec::with_capacity(char_models.len());
        let mut char_height: Vec<f32> = Vec::with_capacity(char_models.len());
        let mut enemy_arm: Vec<Option<EnemyArm>> = Vec::with_capacity(char_models.len());
        for (body, m) in char_models.iter().enumerate() {
            let is_pd_body = body >= ge_body_count;
            // The Perfect Dark clips as this body will see them, and the GoldenEye ones —
            // a PD body can only take the PD set, so it has no second entry to measure.
            let pd_idle = if is_pd_body { pd_idle_clip } else { pd_on_ge_idle_clip };
            let ge_idle = if is_pd_body { pd_idle_clip } else { ge_idle_clip };
            // Sample the idle across its loop, skin each pose on the CPU, and take the
            // global lowest Y (the most-planted foot); seating that at the floor keeps the
            // feet grounded while the animation's own vertical motion still reads. Falls
            // back to the bind-pose AABB with no idle clip.
            let seat = |idle: Option<&clip::AnimationClip>| -> (f32, f32) {
                match idle {
                    Some(idle) => {
                        let samples = 24;
                        let (mut min_y, mut max_y) = (f32::INFINITY, f32::NEG_INFINITY);
                        for i in 0..samples {
                            let t = idle.duration * i as f32 / samples as f32;
                            let mats = idle.skinning_matrices(t, &m.skeleton);
                            let (lo, hi) = m.skinned_y_extent(&mats);
                            min_y = min_y.min(lo);
                            max_y = max_y.max(hi);
                        }
                        (-min_y * CHAR_SCALE, (max_y - min_y) * CHAR_SCALE)
                    }
                    None => (
                        -m.bounds_min.y * CHAR_SCALE,
                        (m.bounds_max.y - m.bounds_min.y) * CHAR_SCALE,
                    ),
                }
            };
            let (pd_feet, pd_height) = seat(pd_idle);
            let (ge_feet, _) = seat(ge_idle);
            char_feet_offset.push([pd_feet, ge_feet]);
            // Height is measured off the PD idle (what a hunter normally plays); the two
            // agree to ~0.01% because the pedestal shifts the whole figure without
            // stretching it, so nothing sized to a character needs the second entry.
            char_height.push(pd_height);
            // The arm chain is resolved from rest *directions*, which a uniform root
            // translation cannot change — so either idle gives the same answer.
            enemy_arm.push(pd_idle.and_then(|idle| EnemyArm::resolve(m, idle)));
        }
        if let (Some(ge), Some(pd)) = (char_height.first(), char_height.get(ge_body_count)) {
            log::info!("body heights: GoldenEye {ge:.2} m, Perfect Dark {pd:.2} m");
        }
        if let Some([pd_seat, ge_seat]) = char_feet_offset.first().copied() {
            log::info!(
                "GoldenEye body 0 seating: {pd_seat:.3} m on the PD clips, {ge_seat:.3} m on \
                 the GoldenEye ones (they differ by the PD root pedestal)"
            );
        }

        // Player Combat: build the full weapon inventory (JS `ALL_WEAPONS`) and
        // load the *active* weapon's gun + muzzle-flash meshes. The rest of the
        // guns load their meshes lazily on the first switch (see `cycle_weapon`) —
        // startup only pays for PP7 (index 0). Warn-not-panic if an asset is
        // missing. All GLBs live under `native/assets/weapons/`.
        // Which arsenal: the tuned GoldenEye 23, Perfect Dark's 33, or both.
        // Resolved from the environment and logged, so the answer is visible in a
        // playtest log rather than deduced from what the guns look like.
        let arsenal = crate::combat::Arsenal::from_env();
        log::info!("{}", arsenal.summary());
        let arsenal_weapons = arsenal.weapons();
        // Every weapon starts **empty**: ammo is something you find now, so a gun you
        // don't own must not be quietly holding ten magazines for the moment you pick
        // it up. `OWN_ALL=1` hands the arsenal over stocked, since a dev judging 33
        // guns wants to fire them, not forage for them.
        let own_all = matches!(
            std::env::var("OWN_ALL").unwrap_or_default().trim(),
            "1" | "on" | "yes" | "true"
        );
        let weapons: Vec<Weapon> = arsenal_weapons
            .iter()
            .map(|&cfg| if own_all { Weapon::new(cfg) } else { Weapon::empty(cfg) })
            .collect();
        // You start **empty-handed** and find your guns on the floor
        // (`DESIGN_PICKUPS.md`) — so the starting slot is `config::UNARMED`, which
        // every arsenal leads with, and it is the only thing owned. The shop still
        // works for the credits economy; a deathmatch level simply never opens it.
        //
        // Resolved by predicate rather than by index 0, for the same reason the old
        // sidearm was resolved by name: nothing in this file should depend on where a
        // weapon sits in a table.
        let weapon_index = arsenal_weapons
            .iter()
            .position(|w| w.is_unarmed())
            .unwrap_or(0);
        // `OWN_ALL=1` grants the whole arsenal so the full cycle (`Q` / N64 `A`) is
        // reachable immediately — for judging 33 guns in one session, hunting each one
        // down first is pure friction. It is also the one exemption from the on-death
        // loadout wipe, for the same reason (see `pickup::reset_loadout`).
        let mut owned = vec![own_all; weapons.len()];
        owned[weapon_index] = true;
        if own_all {
            log::info!("OWN_ALL=1 — the whole arsenal is unlocked ({} weapons)", weapons.len());
        }
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
        for cfg in arsenal_weapons {
            // The unarmed slot has no mesh by definition — skipped explicitly rather
            // than left to fail the load, so booting doesn't warn about an asset that
            // was never meant to exist.
            if cfg.is_unarmed() {
                continue;
            }
            // Enemies wield the HANDLESS variant so Bond's ripped first-person hand
            // doesn't float on the hunter's gun (see `combat::gun_strip`). Only the
            // pistols + detonator ship a `gun_handless.glb`; everything else has no
            // hand, so this falls back to `gun.glb`. The player viewmodel keeps the
            // hand (it loads `gun.glb` directly — see `combat::viewmodel::load_gun`).
            // The mesh a HUNTER holds, which is not always the player's. For a
            // Perfect Dark weapon that is the third-person `chr*` model (see
            // `enemy_def_for`); for GoldenEye it is the hand-stripped variant.
            let held = crate::combat::enemy_def_for(cfg).gun_path;
            let gun_path = crate::combat::gun_strip::enemy_gun_path(held, |rel| {
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
            let muzzle_offset = mesh_muzzle_offset(&muzzle);
            let barrel = resolve_barrel_axis(cfg.name, muzzle_offset);
            enemy_weapon_lib.push(EnemyWeaponAsset {
                name: cfg.name,
                gun,
                muzzle,
                muzzle_offset,
                barrel,
            });
        }
        log::info!("loaded {} enemy weapon meshes", enemy_weapon_lib.len());

        World {
            camera,
            arsenal,
            physics: PhysicsWorld::new(),
            mode: Mode::Build,
            ecs: crate::ecs::Ecs::new(),
            ambient: crate::ecs::AmbientSettings::default(),
            theme_hotkeys: std::collections::BTreeMap::new(),
            character: None,
            nav: None,
            nav_issues: None,
            nav_overlay: None,
            nav_overlay_on: false,
            nav_overlay_rev: 0,
            enemies: Vec::new(),
            spawn_enemies: true,
            char_models,
            char_anim_template,
            pd_anim_template,
            pd_anim_template_ge,
            char_rng: 0x9E37_79B9_7F4A_7C15,
            spawn_rng: 0x5DEE_CE66_D5C4_1B3B,
            difficulty: 0,
            wave_size: ENEMY_COUNT,
            pd: pd_lab::PdHunters::default(),
            pd_lab: false,
            body_set: BodySet::All,
            goldeneye_clips: false,
            hunt_spawn: None,
            hit_reactions: false, // GoldenEye-style flinches; PD hunters use their own tables
            authored_reactions: true, // PD hunters react on PD's tables, not the ragdoll
            local_avoidance: true, // ORCA crowd steering on by default (kill-switch below)
            head_look: true, // procedural head look-at on by default (kill-switch below)
            foot_ik: true, // ground-adaptive foot IK + cadence on by default (kill-switch below)
            ragdoll: true, // physics ragdoll death on by default (kill-switch below)
            wall_clearance: true, // wall-clearance nudge on by default (kill-switch below)
            utility_ai: true, // utility-AI decision layer on by default (kill-switch below)
            pd_omniscience: true, // PD hunters always know where you are (kill-switch below)
            ai_mode: crate::enemy::AiMode::from_env(),
            grenades: false, // OFF — hunters were blowing themselves up; see the field doc
            radar_cells: Vec::new(),
            char_feet_offset,
            char_height,
            enemy_arm,
            procedural_preview: None,
            ge_body_count,
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
            player_score: Score::default(),
            hunter_scores: Vec::new(),
            score_limit: SCORE_LIMIT,
            round_over: None,
            player_respawn: 0.0,
            spawn_pads: Vec::new(),
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
            // The draft starts on the first real gun of the live arsenal (index 0 is
            // the unarmed slot, which cannot be picked up), so the pickup panel opens
            // on something placeable rather than on "Unarmed".
            pickup_draft: crate::ecs::Pickup::weapon(
                arsenal_weapons
                    .iter()
                    .find(|w| !w.is_unarmed())
                    .map(|w| w.name)
                    .unwrap_or("PP7"),
            ),
            pickup_clock: 0.0,
            pickup_message: String::new(),
            pickup_message_timer: 0.0,
            own_all,
            // On by default — the player and the hunters play by the same rule. Read
            // as a negative flag (`ARMED_HUNTERS=1` opts out) so the default needs no
            // environment at all.
            unarmed_hunters: !matches!(
                std::env::var("ARMED_HUNTERS").unwrap_or_default().trim(),
                "1" | "on" | "yes" | "true"
            ),
            prop_colliders: std::collections::HashMap::new(),
            door_entities: std::collections::HashMap::new(),
            door_double: false,
            hunters_enabled: true,
            prop_gizmo_mode: PropGizmoMode::Translate,
            light_tool: false,
            light_preview_pos: None,
            spawn_tool: false,
            spawn_preview: None,
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
            draw_phase: None,
            draw_face: None,
            draw_verts: Vec::new(),
            draw_cursor: None,
            draw_rects: Vec::new(),
            draw_candidate: 0,
            draw_depth: 0.0,
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
            simple_offset_wt: 0.0,
            simple_width_wt: STAIR_WIDTH,
            simple_ghost: None,
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
        self.wave_size = n.clamp(1, WAVE_SIZE_MAX);
    }

    /// How many hunters the next HUNT will flood in.
    pub fn wave_size(&self) -> usize {
        self.wave_size
    }

    /// Nudge the wave size and, mid-hunt, re-flood immediately — the `[` / `]` keys.
    ///
    /// Live rather than next-round because its first use is bisecting a defect: a hunter
    /// that stalls in a corridor with four in the level and walks it cleanly with one is
    /// a **crowding** problem (ORCA, separation, doorway queueing), and one that stalls
    /// either way is not. Being able to drop to a single hunter without leaving HUNT,
    /// reloading and re-walking there is the difference between testing that hypothesis
    /// in seconds and in minutes. Mirrors [`Self::change_difficulty`], which restarts the
    /// duel for the same reason.
    pub fn change_wave_size(&mut self, delta: i32) {
        let new = (self.wave_size as i32 + delta).clamp(1, WAVE_SIZE_MAX as i32) as usize;
        if new == self.wave_size {
            return; // already at the floor/ceiling
        }
        self.wave_size = new;
        log::info!(
            "WAVE SIZE -> {} hunter(s){}",
            self.wave_size,
            if self.mode == Mode::Hunt { " — re-flooding the wave" } else { " (takes effect at G)" }
        );
        self.restart_hunt(); // no-op outside HUNT
    }

    /// The difficulty dial as a 0..=1 fraction — what the PD tier interpolation
    /// and the oblivious-targeting cutover read.
    pub(crate) fn difficulty_frac(&self) -> f32 {
        self.difficulty as f32 / DIFFICULTY_MAX as f32
    }

    /// A body's measured standing height in metres (from its own idle), or the
    /// GoldenEye reference if that body never loaded.
    pub(crate) fn body_height(&self, body: usize) -> f32 {
        self.char_height.get(body).copied().filter(|h| *h > 0.1).unwrap_or(CAPSULE_REF_HEIGHT)
    }

    /// A body's hit-capsule `(radius, half_height)` in metres, scaled to its own
    /// height so the collider actually covers the figure that is drawn. A body of
    /// [`CAPSULE_REF_HEIGHT`] reproduces [`ENEMY_RADIUS`] / [`ENEMY_HALF_HEIGHT`]
    /// exactly, so GoldenEye hunters are numerically unchanged.
    pub(crate) fn body_capsule(&self, body: usize) -> (f32, f32) {
        let k = self.body_height(body) / CAPSULE_REF_HEIGHT;
        (ENEMY_RADIUS * k, ENEMY_HALF_HEIGHT * k)
    }

    /// A body's `(head_min, leg_max)` hit-zone boundaries — impact height above the
    /// feet, in metres — scaled the same way, so a head shot means the head on a
    /// 1.73 m Perfect Dark body as much as on a 1.50 m GoldenEye one.
    pub(crate) fn body_hit_zones(&self, body: usize) -> (f32, f32) {
        let k = self.body_height(body) / CAPSULE_REF_HEIGHT;
        (ZONE_HEAD_MIN * k, ZONE_LEG_MAX * k)
    }

    /// Body ids belonging to the GoldenEye family ([`BODY_CATALOG`]).
    pub(crate) fn ge_bodies(&self) -> std::ops::Range<usize> {
        0..self.ge_body_count
    }

    /// Body ids belonging to the Perfect Dark family ([`PD_BODY_CATALOG`]) — they
    /// load after the GoldenEye ones, so they occupy the tail of `char_models`.
    pub(crate) fn pd_bodies(&self) -> std::ops::Range<usize> {
        self.ge_body_count..self.char_models.len()
    }

    /// The body ids a wave draws from — **every loaded body by default**, both families.
    ///
    /// This is the whole point of binding the Perfect Dark clip set twice: a GoldenEye
    /// body plays those clips correctly, so the animation fidelity no longer costs body
    /// variety and the roster is all 44 GoldenEye bodies plus the 6 Perfect Dark ones.
    /// [`Self::body_set`] narrows it for A/B and for the assets-missing fallback.
    fn wave_bodies(&self) -> std::ops::Range<usize> {
        let (ge, pd) = (self.ge_bodies(), self.pd_bodies());
        let want = match self.body_set {
            BodySet::All => 0..self.char_models.len(),
            BodySet::GoldenEye => ge.clone(),
            BodySet::PerfectDark => pd.clone(),
        };
        // Never hand back an empty range if *something* loaded: a wave with no bodies
        // spawns no hunters, and "the PD export is missing" should degrade to GoldenEye
        // rather than to an empty hunt.
        if want.is_empty() {
            if !ge.is_empty() {
                return ge;
            }
            if !pd.is_empty() {
                return pd;
            }
        }
        want
    }

    /// The clip template that drives `body`, and whether it is the **Perfect Dark** set
    /// (which is what `EnemyInstance::pd_anims` records, and so what gates the directional
    /// fire table and the authored per-hit-part reactions).
    ///
    /// A clip binds its channels to one skeleton, so this picks the template built for
    /// *that body's rig*. Both rigs get the Perfect Dark set; `goldeneye_clips` swaps a
    /// GoldenEye body back onto the legacy GoldenEye set, which is the only way to reach
    /// the pre-promotion animation paths (hand-set `FIRE_TIMING`, height-zone hit picks)
    /// now that Perfect Dark drives everything.
    ///
    /// **A GoldenEye clip on a Perfect Dark body is the one combination that is wrong** —
    /// the PD rig's 15 extra `Blend_*` joints have no channels in a GoldenEye clip and
    /// would stay at bind while their owners rotate — so a PD body always takes the PD
    /// set regardless of the flag.
    fn body_clips(&self, body: usize) -> Option<(&AnimPlayer, bool)> {
        let is_pd_body = self.pd_bodies().contains(&body);
        if is_pd_body {
            // PD body: only the PD set is valid on it.
            return self.pd_anim_template.as_ref().map(|t| (t, true));
        }
        if self.goldeneye_clips {
            if let Some(t) = self.char_anim_template.as_ref() {
                return Some((t, false));
            }
        }
        // GoldenEye body on the Perfect Dark clip set — the roster-widening case.
        if let Some(t) = self.pd_anim_template_ge.as_ref() {
            return Some((t, true));
        }
        self.char_anim_template.as_ref().map(|t| (t, false))
    }

    /// Turn the **PD simulant lab** on (`PD_LAB=1`). Every hunter spawned from now
    /// on carries a [`crate::pdsim::Simulant`], aims/shoots the Perfect Dark way (see
    /// [`pd_lab`] for the exact list of what that replaces) **and wears a Perfect
    /// Dark body**, animated by [`PD_TEMPLATE_CLIPS`]. Also sets the wave size, since
    /// the lab is about watching one bot at a time.
    pub fn enable_pd_lab(&mut self, cfg: pd_lab::PdLabConfig) {
        self.wave_size = cfg.count.max(1);
        self.pd = cfg.into();
        self.pd_lab = true;
        // The lab pins **Perfect Dark bodies**. The shipped roster mixes both families now
        // (they all play PD's animations either way), but the lab exists to look at
        // Perfect Dark specifically — and pinning it here is also what gives the headless
        // scenarios a one-call way to get a PD-bodied hunter.
        self.body_set = BodySet::PerfectDark;
        if self.pd_bodies().is_empty() || self.pd_anim_template.is_none() {
            log::warn!(
                "PD lab: no PD bodies ({}) or no PD clip template ({}) — hunters will \
                 wear GoldenEye bodies",
                self.pd_bodies().len(),
                self.pd_anim_template.is_some()
            );
        }
        log::info!(
            "PD SIMULANT LAB: wave of {} x {} ({}) — set PD_LAB_COUNT to change",
            self.wave_size,
            cfg.bot_type.name(),
            match cfg.difficulty {
                Some(d) => d.name(),
                None => "following the difficulty dial",
            }
        );
    }

    /// One line describing what the next wave will actually wear — the resolved roster
    /// after every flag has had its say.
    ///
    /// Exists because a boot flag losing a fight with a mode default is invisible
    /// otherwise: `BODIES=ge` was silently overridden by `PD_LAB` pinning Perfect Dark
    /// bodies, and nothing said so until the hunters were already on screen.
    pub fn roster_summary(&self) -> String {
        let bodies = self.wave_bodies();
        let ge = bodies.clone().filter(|b| self.ge_bodies().contains(b)).count();
        let pd = bodies.clone().filter(|b| self.pd_bodies().contains(b)).count();
        let clips = match self.body_clips(bodies.start) {
            Some((_, true)) => "Perfect Dark clips",
            Some((_, false)) => "GoldenEye clips (legacy)",
            None => "NO CLIPS LOADED",
        };
        format!(
            "{ge} GoldenEye + {pd} Perfect Dark bodies, {clips}{}{}",
            if self.goldeneye_clips { " [GE_CLIPS]" } else { "" },
            if self.pd_lab { " [PD_LAB]" } else { "" },
        )
    }

    /// Narrow which bodies a wave draws from (default [`BodySet::All`]). Takes effect on
    /// the next wave, so call it before `toggle_mode`. Set from `BODIES=ge|pd` at boot.
    pub fn set_body_set(&mut self, s: BodySet) {
        self.body_set = s;
    }

    /// Put GoldenEye-bodied hunters back on the **legacy GoldenEye clip set** — the
    /// pre-promotion A/B. See [`Self::goldeneye_clips`]. Implies nothing about the body
    /// set, but only affects GoldenEye bodies, so pair it with [`BodySet::GoldenEye`] to
    /// get a wave that is entirely on the legacy paths. Set from `GE_CLIPS=1` at boot.
    pub fn set_goldeneye_clips(&mut self, on: bool) {
        self.goldeneye_clips = on;
    }

    /// Whether the **lab** is active — the bare room and the debug overlay. The Perfect
    /// Dark hunter model itself is always on; see [`Self::pd`].
    pub fn pd_lab_active(&self) -> bool {
        self.pd_lab
    }

    /// Per-simulant debug snapshots for the lab overlay, in hunter order. Empty
    /// outside the lab.
    pub fn pd_debug(&self) -> Vec<pd_lab::PdDebug> {
        self.enemies.iter().filter_map(|e| e.pd_debug).collect()
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
    /// hunter plays the canned death clip + fade (pre-ragdoll baseline). Note a
    /// Perfect Dark hunter prefers its authored death table over the ragdoll either
    /// way — see [`Self::set_authored_reactions`].
    pub fn set_ragdoll(&mut self, on: bool) {
        self.ragdoll = on;
    }

    /// Toggle Perfect Dark's authored hit/death animation tables for PD hunters
    /// (default ON — see [`Self::authored_reactions`]). Off, they fall back to the
    /// physics ragdoll like GoldenEye hunters, which is the A/B.
    pub fn set_authored_reactions(&mut self, on: bool) {
        self.authored_reactions = on;
    }

    /// Whether PD hunters use their authored reaction tables (inspection / tests).
    pub fn authored_reactions(&self) -> bool {
        self.authored_reactions
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

    /// Toggle PD-lab hunter omniscience (default ON — see [`Self::pd_omniscience`]).
    /// Off, PD hunters fall back to our perceive-then-remember knowledge and will lose
    /// you + fan-out search, exactly like a GoldenEye hunter (the A/B baseline).
    pub fn set_pd_omniscience(&mut self, on: bool) {
        self.pd_omniscience = on;
    }

    /// Whether PD-lab hunters are omniscient (inspection / tests).
    pub fn pd_omniscience(&self) -> bool {
        self.pd_omniscience
    }

    /// Toggle the grenade flush (`#5`). **Default OFF** — see [`Self::grenades`] for
    /// why, and what has to change before it should come back on.
    pub fn set_grenades(&mut self, on: bool) {
        self.grenades = on;
    }

    /// Whether hunters may lob flush grenades.
    pub fn grenades(&self) -> bool {
        self.grenades
    }

    /// Decimate the baked nav grid into the radar's floor backdrop: one point per
    /// [`RADAR_CELL`] cube. The nav grid is quarter-metre cells, so drawing it raw
    /// would be an illegible smear of thousands of dots — and the radar only needs
    /// enough to read the *shape* of the rooms and corridors. Keeps Y so a multi-storey
    /// level can be sliced to the player's floor at draw time.
    fn bake_radar_cells(&mut self, nav: &NavWorld) {
        let mut seen: std::collections::HashSet<(i32, i32, i32)> = std::collections::HashSet::new();
        self.radar_cells = nav
            .all_standable()
            .into_iter()
            .filter(|c| {
                seen.insert((
                    (c.x / RADAR_CELL).floor() as i32,
                    (c.y / RADAR_FLOOR_BAND).floor() as i32,
                    (c.z / RADAR_CELL).floor() as i32,
                ))
            })
            .collect();
        log::info!("radar: {} floor points baked", self.radar_cells.len());
    }

    /// A frame's worth of radar state, or `None` outside HUNT / with no player.
    ///
    /// Everything is resolved into the **radar's own frame** here rather than in the
    /// UI: metres → a unit disc with the player at the origin and its facing pointing
    /// up. That keeps the yaw convention (`forward_from`: yaw 0 looks down −Z) in one
    /// place next to the camera code that defines it, and leaves the drawing side to
    /// deal only with pixels.
    pub fn radar(&self, range: f32) -> Option<RadarView> {
        let c = self.character.as_ref()?;
        let origin = c.pos;
        // The player's flat basis. `forward_from(yaw, 0) == (−sin, 0, −cos)`, and right
        // is `forward × up`.
        let (sy, cy) = c.yaw.sin_cos();
        let fwd = Vec3::new(-sy, 0.0, -cy);
        let right = Vec3::new(cy, 0.0, -sy);
        let project = |p: Vec3| {
            let d = p - origin;
            // Unit disc: +x right of the player, +y ahead of it.
            (Vec2::new(d.dot(right), d.dot(fwd)) / range, d.y - origin.y)
        };
        let in_disc = |v: Vec2| v.length_squared() <= 1.0;

        let floor = self
            .radar_cells
            .iter()
            .filter_map(|&p| {
                let (v, dy) = project(p);
                // Only this storey: a floor above or below would otherwise print
                // straight through the room you are standing in.
                (in_disc(v) && dy.abs() <= RADAR_FLOOR_BAND).then_some(v)
            })
            .collect();

        let blips = self
            .enemies
            .iter()
            .enumerate()
            .filter_map(|(id, e)| {
                let (v, dy) = project(e.enemy.pos);
                in_disc(v).then(|| RadarBlip {
                    id,
                    at: v,
                    // Kept as a signed height so the UI can mark someone a storey up.
                    dy,
                    dead: e.enemy.is_dead(),
                    engaged: e.enemy.is_engaged(),
                    firing: e.fire_elapsed.is_some(),
                })
            })
            .collect();
        Some(RadarView { range, floor, blips })
    }

    /// The difficulty-derived tuning for the current level (see [`DiffParams`]). Linear
    /// ramp from all-neutral at level 0 to brutal at [`DIFFICULTY_MAX`].
    pub(crate) fn difficulty_params(&self) -> DiffParams {
        let t = self.difficulty as f32 / DIFFICULTY_MAX as f32;
        DiffParams {
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
        // Under `AI=pd`, zero every knob Perfect Dark has no equivalent for. Measured,
        // not assumed (`DESIGN_AI_PD_VS_OURS.md` §4b): `aibot->speedmultsideways` is
        // written to zero in every branch that writes it, `chr_try_sidestep`'s only
        // caller is a hand-authored single-player guard script, and there is no cover
        // selection in the bot code at all. So: no aim-dodge, no flanking, no
        // cover/peek, no suppressing fire. A flag, not a deletion — `AI=ours` hands the
        // same dial back unchanged.
        let pd = self.ai_mode.is_pd();
        let off = |v: f32| if pd { 0.0 } else { v };
        crate::enemy::AiTuning {
            alert: crate::enemy::ALERT_DURATION * dp.reaction_mult,
            cooldown: crate::enemy::COOLDOWN_DURATION * dp.cooldown_mult,
            dodge: off(dp.dodge),
            speed_mult: dp.speed_mult,
            sense: dp.sense_mult,
            suppress: off(dp.suppress),
            flank: off(dp.flank),
            cover: off(dp.cover),
            mode: self.ai_mode,
            // The same tier the hunters' simulants carry, so the distance-band rule and
            // the aim model agree about how good this bot is.
            tier: self
                .pd
                .difficulty
                .unwrap_or_else(|| pd_lab::tier_for_dial_frac(self.difficulty_frac())),
        }
    }

    /// Select the engagement model (`AI=pd|ours`). Applied last at boot so an explicit
    /// environment choice outranks any mode default, and settable live from the AI lab
    /// for an A/B on one arena. See [`crate::enemy::AiMode`].
    pub fn set_ai_mode(&mut self, mode: crate::enemy::AiMode) {
        self.ai_mode = mode;
    }

    /// Which engagement model the hunters are running.
    pub fn ai_mode(&self) -> crate::enemy::AiMode {
        self.ai_mode
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

}
