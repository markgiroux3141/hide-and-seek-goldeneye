//! **Spike:** a BUILD-phase preview of the enemy pose stack
//! ([`engine::skeletal::layers`]), so the locomotion + light aim + recoil can be
//! eyeballed on the real character (with its gun) in the release binary. Isolated
//! from the HUNT enemy path — it owns its own [`LayeredAnimator`], built from the
//! *same* [`EnemyArm::build_stack`] a live hunter uses, and is driven by an
//! internal clock (auto speed-ramp, auto-circling aim target, periodic recoil), so
//! there's nothing to fiddle with to see it move.
//!
//! Toggle with `Y` in BUILD; `Z` fires a manual recoil kick on top of the auto
//! cadence.

use super::*;

use glam::Quat;

use engine::render::camera::forward_from;
use engine::skeletal::layers::{
    AdditiveDecayLayer, AimOffsetLayer, LayerCtx, LayeredAnimator, LocomotionBlendLayer, Pose,
};

use crate::combat::enemy_def_for;

/// Auto-circle sweep rate (rad/s) of the aim target around the shoulder.
const ORBIT_SPEED: f32 = 1.1;
/// How far forward (in arm-reach fractions) the orbit centre sits, and the orbit
/// radius, so the arm sweeps a visible cone.
const FWD_FRAC: f32 = 0.9;
const ORBIT_FRAC: f32 = 0.4;

/// Seconds between auto-fire recoil kicks, and the manual kick amount.
const FIRE_INTERVAL: f32 = 1.3;
const RECOIL_KICK: f32 = 0.45;

/// Speed auto-ramp: a slow cosine 0 ↔ run so the gait blend is visible hands-free.
const RAMP_RATE: f32 = 0.55; // rad/s of the ramp cosine (~11 s full cycle)

/// A standalone character posed by the enemy layer stack, for BUILD preview.
pub(crate) struct ProceduralPreview {
    /// locomotion base → aim-offset → recoil (the live-hunter stack).
    stack: LayeredAnimator,
    /// Feet position + facing yaw (fed through `World::char_transform`).
    pub(crate) feet: Vec3,
    pub(crate) yaw: f32,
    /// Internal clock driving the auto-ramp + auto-circling target + auto-fire.
    clock: f32,
    fire_cooldown: f32,
    /// Model-space shoulder origin + arm reach, fixed at spawn (orbit geometry).
    shoulder: Vec3,
    reach: f32,
    /// Right-hand (`Bone_9`) joint index, for the attached gun draw.
    bone9: usize,
    /// AR33 preview gun: name + `Bone_9`-local attach transform.
    weapon_name: &'static str,
    attach: Mat4,
    /// Last evaluated skinning matrices + model-space joint globals.
    pub(crate) joints: Vec<Mat4>,
    globals: Vec<Mat4>,
    /// Empty per-vertex blood (clean) — the renderer keeps its white init.
    pub(crate) blood: Vec<f32>,
}

impl ProceduralPreview {
    /// Build the preview over the shared model + anim template. `None` if the
    /// skeleton has no arm chain or the gait clips are missing.
    fn new(
        model: &SkinnedModel,
        arm: EnemyArm,
        loco_clips: Vec<(f32, clip::AnimationClip)>,
        weapon_name: &'static str,
        attach: Mat4,
        aim_forward: Vec3,
        feet: Vec3,
        yaw: f32,
    ) -> Option<Self> {
        let sk = &model.skeleton;
        let bone9 = sk.index_of(RIGHT_HAND_BONE)?;

        // Shoulder origin + arm reach from a standing (idle) pose (bind is a splayed
        // star, so sample the idle clip at t=0 instead).
        let idle = &loco_clips.first()?.1;
        let (t, r, s) = idle.pose_trs(0.0, sk);
        let g = Pose::from_trs(t, r, s).joint_global_transforms(sk);
        let shoulder = g[arm.shoulder()].to_scale_rotation_translation().2;
        let hand = g[bone9].to_scale_rotation_translation().2;
        let reach = (hand - shoulder).length().max(0.1);

        let mut stack = arm.build_stack(loco_clips);
        // Aim the barrel (not the arm) at the orbiting target, like a live hunter.
        if let Some(aim) = stack.layer_as::<AimOffsetLayer>(ENEMY_AIM_LAYER) {
            aim.forward = aim_forward;
        }
        let base = Pose::bind(sk);
        let globals = base.joint_global_transforms(sk);
        let joints = base.skinning_matrices(sk);

        Some(ProceduralPreview {
            stack,
            feet,
            yaw,
            clock: 0.0,
            fire_cooldown: FIRE_INTERVAL,
            shoulder,
            reach,
            bone9,
            weapon_name,
            attach,
            joints,
            globals,
            blood: Vec::new(),
        })
    }

    /// A manual recoil kick (bound to `Z`), on top of the auto cadence.
    pub(crate) fn fire(&mut self) {
        if let Some(r) = self.stack.layer_as::<AdditiveDecayLayer>(ENEMY_RECOIL_LAYER) {
            r.kick(RECOIL_KICK);
        }
    }

    /// Advance one frame: ramp speed, sweep the aim target, auto-fire, fold the
    /// stack, and cache the skinning matrices + joint globals.
    fn advance(&mut self, dt: f32, model: &SkinnedModel) {
        self.clock += dt;
        let sk = &model.skeleton;

        // Locomotion speed auto-ramp: a smooth cosine 0 ↔ run.
        let ramp = 0.5 * (1.0 - (self.clock * RAMP_RATE).cos()); // 0..1..0
        if let Some(loco) = self.stack.layer_as::<LocomotionBlendLayer>(ENEMY_LOCO_LAYER) {
            loco.speed = ramp * anim_set::SPEED_RUN;
        }

        // Auto-circling aim target (model space): a circle in front of the shoulder.
        let theta = self.clock * ORBIT_SPEED;
        let center = self.shoulder + Vec3::Z * (self.reach * FWD_FRAC);
        let target =
            center + (Vec3::X * theta.cos() + Vec3::Y * theta.sin()) * (self.reach * ORBIT_FRAC);
        if let Some(aim) = self.stack.layer_as::<AimOffsetLayer>(ENEMY_AIM_LAYER) {
            aim.target = target;
            aim.weight = 1.0;
        }

        // Auto-fire recoil.
        self.fire_cooldown -= dt;
        if self.fire_cooldown <= 0.0 {
            self.fire();
            self.fire_cooldown = FIRE_INTERVAL;
        }

        // Fold the stack over a bind-pose seed (the locomotion base overwrites it).
        let pose = self.stack.evaluate(Pose::bind(sk), &LayerCtx { skeleton: sk, dt });
        self.globals = pose.joint_global_transforms(sk);
        self.joints = pose.skinning_matrices(sk);
    }
}

impl World {
    /// Toggle the BUILD-phase preview (bound to `Y`). Spawns a character ~2 m in
    /// front of the camera facing it; a second press removes it. No-op outside BUILD
    /// or if the model/anim didn't load.
    pub fn toggle_procedural_preview(&mut self) {
        if !self.is_build() {
            return;
        }
        if self.procedural_preview.is_some() {
            self.procedural_preview = None;
            log::info!("procedural preview: off");
            return;
        }
        let (Some(arm), Some(template)) = (self.enemy_arm, self.char_anim_template.clone()) else {
            log::warn!("procedural preview: no arm chain / anim template");
            return;
        };
        let Some(loco_clips) = (|| {
            Some(vec![
                (0.0, template.clip(0)?.clone()),
                (anim_set::SPEED_WALK, template.clip(1)?.clone()),
                (anim_set::SPEED_JOG, template.clip(2)?.clone()),
                (anim_set::SPEED_RUN, template.clip(3)?.clone()),
            ])
        })() else {
            log::warn!("procedural preview: missing gait clips");
            return;
        };

        // AR33 gun attach (right hand), mirroring the hunter setup, so the preview
        // shows the gun following the aimed arm.
        let weapon = enemy_def_for(&crate::combat::config::AR33);
        let attach_rot =
            Quat::from_euler(EulerRot::XYZ, weapon.right_rot.x, weapon.right_rot.y, weapon.right_rot.z);
        let attach = Mat4::from_translation(weapon.right_offset) * Mat4::from_quat(attach_rot);
        let aim_forward = self
            .enemy_weapon_lib
            .iter()
            .find(|a| a.name == weapon.name)
            .map(|asset| arm.right.barrel_aim_forward(attach_rot, asset.barrel_axis()))
            .unwrap_or(Vec3::NEG_Z);

        // Placement: floor ~2 m ahead of the camera, facing back at it.
        let f = forward_from(self.camera.yaw, self.camera.pitch);
        let flat = Vec3::new(f.x, 0.0, f.z).normalize_or_zero();
        let spot = self.camera.pos + flat * (WORLD_SCALE * 2.0);
        let cam_pos = self.camera.pos;
        let feet = self
            .floor_under(spot)
            .or_else(|| self.floor_under(cam_pos))
            .unwrap_or(spot);
        let to_cam = cam_pos - feet;
        let yaw = to_cam.x.atan2(to_cam.z);

        let model = self.char_model.as_ref().unwrap();
        match ProceduralPreview::new(model, arm, loco_clips, weapon.name, attach, aim_forward, feet, yaw) {
            Some(p) => {
                self.procedural_preview = Some(p);
                log::info!("procedural preview: on (Y toggles, Z fires)");
            }
            None => log::warn!("procedural preview: could not build the stack"),
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

    /// The preview's AR33 draw for the BUILD weapon pass: `(name, view_proj · char ·
    /// bone9 · attach)`. `None` unless the preview is live.
    pub(crate) fn preview_weapon_draw(&self, vp: Mat4) -> Option<(&'static str, Mat4)> {
        let p = self.procedural_preview.as_ref()?;
        let g9 = *p.globals.get(p.bone9)?;
        let world = self.char_transform(p.feet, p.yaw) * g9 * p.attach;
        Some((p.weapon_name, vp * world))
    }
}
