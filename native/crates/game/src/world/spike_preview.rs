//! **Spike:** a BUILD-phase preview of the procedural pose layer stack
//! ([`engine::skeletal::layers`]), so the IK aim + recoil can be eyeballed on the
//! real character in the release binary. Deliberately isolated from the HUNT
//! enemy path — it owns its own [`LayeredAnimator`] over an idle base pose and is
//! driven entirely by an internal clock (auto-circling aim target + periodic
//! recoil), so there's nothing to fiddle with to see it move.
//!
//! Toggle with `Y` in BUILD; `Z` fires a manual recoil kick on top of the auto
//! cadence. Throwaway wiring: when the spike graduates, the same stack moves onto
//! each `EnemyInstance` and this module goes away.

use super::*;

use engine::render::camera::forward_from;
use engine::skeletal::layers::{
    AdditiveDecayLayer, LayerCtx, LayeredAnimator, Pose, TwoBoneIkLayer,
};

/// Stack indices (push order in [`ProceduralPreview::new`]).
const IK_IDX: usize = 0;
const RECOIL_IDX: usize = 1;

/// Auto-circle sweep rate (rad/s) of the aim target around the shoulder.
const ORBIT_SPEED: f32 = 1.1;
/// How far forward (in arm-reach fractions) the orbit center sits, and the orbit
/// radius — kept inside reach so the arm never slams to full extension.
const FWD_FRAC: f32 = 0.45;
const ORBIT_FRAC: f32 = 0.42;

/// Seconds between auto-fire recoil kicks, and the kick/decay shape.
const FIRE_INTERVAL: f32 = 1.3;
const RECOIL_KICK: f32 = 0.45;
const RECOIL_DECAY: f32 = 9.0;
const RECOIL_MAX: f32 = 0.7;

/// A standalone character posed by the procedural layer stack.
pub(crate) struct ProceduralPreview {
    /// Idle base-pose source (a clone of the shared anim template).
    base: AnimPlayer,
    /// base pose → [IK aim] → [recoil] → final pose.
    stack: LayeredAnimator,
    /// Feet position + facing yaw (fed through `World::char_transform`).
    pub(crate) feet: Vec3,
    pub(crate) yaw: f32,
    /// Internal clock driving the auto-circling target + auto-fire cadence.
    clock: f32,
    fire_cooldown: f32,
    /// Model-space orbit center (shoulder origin) + arm reach, fixed at spawn.
    shoulder: Vec3,
    reach: f32,
    /// Last evaluated skinning matrices, consumed by `World::character_instances`.
    pub(crate) joints: Vec<Mat4>,
    /// Empty per-vertex blood (clean) — the renderer leaves its white init when
    /// handed an empty slice, so the preview never needs a real blood buffer.
    pub(crate) blood: Vec<f32>,
}

impl ProceduralPreview {
    /// Build the preview over the shared model + anim template. `None` if the
    /// skeleton has no resolvable `root→mid→end` arm chain from the hand bone.
    pub(crate) fn new(
        model: &SkinnedModel,
        template: &AnimPlayer,
        feet: Vec3,
        yaw: f32,
    ) -> Option<Self> {
        let sk = &model.skeleton;
        let mut ik = TwoBoneIkLayer::from_end_bone(sk, RIGHT_HAND_BONE)?;
        ik.enabled = true;
        ik.weight = 1.0;

        let base = template.clone();
        // Shoulder origin + total arm reach from the idle base pose (model space).
        let g = base.pose(sk).joint_global_transforms(sk);
        let a = g[ik.root].to_scale_rotation_translation().2;
        let b = g[ik.mid].to_scale_rotation_translation().2;
        let c = g[ik.end].to_scale_rotation_translation().2;
        let reach = (b - a).length() + (c - b).length();
        // Elbow-down/forward pole hint (only used if the chain goes straight).
        ik.pole = a + Vec3::new(0.0, -1.0, 0.5);
        let shoulder_joint = ik.root;

        let mut stack = LayeredAnimator::new();
        stack.push(Box::new(ik));
        // Recoil kicks the whole arm at the shoulder: IK re-solves onto the target
        // every frame while the decaying kick rides on top, so a shot reads as an
        // arm jerk that settles back on aim — a clean demo of layer ordering.
        stack.push(Box::new(AdditiveDecayLayer::new(
            shoulder_joint,
            Vec3::X,
            RECOIL_DECAY,
            RECOIL_MAX,
        )));

        Some(ProceduralPreview {
            base,
            stack,
            feet,
            yaw,
            clock: 0.0,
            fire_cooldown: FIRE_INTERVAL,
            shoulder: a,
            reach,
            joints: Vec::new(),
            blood: Vec::new(),
        })
    }

    /// A manual recoil kick (bound to `Z`), on top of the auto cadence.
    pub(crate) fn fire(&mut self) {
        if let Some(r) = self.stack.layer_as::<AdditiveDecayLayer>(RECOIL_IDX) {
            r.kick(RECOIL_KICK);
        }
    }

    /// Advance one frame: move the aim target along its orbit, auto-fire on the
    /// cadence, tick the idle base, then fold the stack and cache the skinning
    /// matrices.
    pub(crate) fn advance(&mut self, dt: f32, model: &SkinnedModel) {
        self.clock += dt;

        // Auto-circling aim target (model space): a circle in front of the shoulder.
        let theta = self.clock * ORBIT_SPEED;
        let center = self.shoulder + Vec3::Z * (self.reach * FWD_FRAC);
        let target =
            center + (Vec3::X * theta.cos() + Vec3::Y * theta.sin()) * (self.reach * ORBIT_FRAC);
        if let Some(ik) = self.stack.layer_as::<TwoBoneIkLayer>(IK_IDX) {
            ik.target = target;
        }

        // Auto-fire recoil.
        self.fire_cooldown -= dt;
        if self.fire_cooldown <= 0.0 {
            self.fire();
            self.fire_cooldown = FIRE_INTERVAL;
        }

        // Idle base clock, then evaluate the full stack.
        self.base.update(dt);
        let base_pose: Pose = self.base.pose(&model.skeleton);
        let final_pose = self.stack.evaluate(
            base_pose,
            &LayerCtx {
                skeleton: &model.skeleton,
                dt,
            },
        );
        self.joints = final_pose.skinning_matrices(&model.skeleton);
    }
}

impl World {
    /// Toggle the BUILD-phase procedural-anim preview (bound to `Y`). Spawns a
    /// character ~2 m in front of the camera on the floor, facing back at it; a
    /// second press removes it. No-op outside BUILD or if the model didn't load.
    pub fn toggle_procedural_preview(&mut self) {
        if !self.is_build() {
            return;
        }
        if self.procedural_preview.is_some() {
            self.procedural_preview = None;
            log::info!("procedural preview: off");
            return;
        }
        if self.char_model.is_none() || self.char_anim_template.is_none() {
            log::warn!("procedural preview: no character model/anim loaded");
            return;
        }
        // Resolve placement first (`floor_under` needs `&mut self`), *then* borrow
        // the model + template to build the preview.
        let f = forward_from(self.camera.yaw, self.camera.pitch);
        let flat = Vec3::new(f.x, 0.0, f.z).normalize_or_zero();
        let spot = self.camera.pos + flat * (WORLD_SCALE * 2.0);
        let cam_pos = self.camera.pos;
        let feet = self
            .floor_under(spot)
            .or_else(|| self.floor_under(cam_pos))
            .unwrap_or(spot);
        // Face the camera (model faces +Z at yaw 0).
        let to_cam = cam_pos - feet;
        let yaw = to_cam.x.atan2(to_cam.z);

        let model = self.char_model.as_ref().unwrap();
        let template = self.char_anim_template.as_ref().unwrap();
        match ProceduralPreview::new(model, template, feet, yaw) {
            Some(p) => {
                self.procedural_preview = Some(p);
                log::info!("procedural preview: on (Y toggles, Z fires)");
            }
            None => log::warn!("procedural preview: no arm chain in skeleton"),
        }
    }

    /// A manual recoil kick on the preview (bound to `Z`); no-op if it's off.
    pub fn fire_procedural_preview(&mut self) {
        if let Some(p) = self.procedural_preview.as_mut() {
            p.fire();
        }
    }

    /// Advance the BUILD preview one frame (disjoint borrows of the two fields).
    pub(crate) fn advance_procedural_preview(&mut self, dt: f32) {
        let (Some(preview), Some(model)) =
            (self.procedural_preview.as_mut(), self.char_model.as_ref())
        else {
            return;
        };
        preview.advance(dt, model);
    }
}
