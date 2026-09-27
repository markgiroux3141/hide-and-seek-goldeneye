//! Where a bot's guns really are: PD's `chr_get_gun_pos` reads the gun model's
//! `CHRGUNFIRE` node as it was last drawn, and only falls back to "root + 30 cm up,
//! 10 cm to the side" for a gun that wasn't drawn. The two differ a lot for a
//! squatting bot: the fallback sits ~90 cm up, over a crouched target's 90 cm hit
//! cylinder, so level shots skim its head; the drawn gun is in the squat pose's hand.
//!
//! The viewer draws every bot, so it always had drawn gun positions. This module is
//! that computation on its own — body skeleton + the clip bank + PD's joint
//! callback, CPU only — so the headless harnesses can use it too ([`GunPoser`]).

use glam::{Mat4, Vec3};

use engine::skeletal::gltf_skin::{self, SkinnedModel};

use super::arena::UNITS_PER_M;
use super::bot::Hand;
use super::chr::Chr;
use super::model::{self, CallbackJoints, ClipBank};
use super::sim::{Sim, BODIES};
use super::weapons;

/// `CHAR_SCALE` — body GLB units to metres. The PD exports are authored in these
/// units so a body comes out at its true height (`pd_gltf.py`).
pub const CHAR_SCALE: f32 = 0.000_832;

pub fn assets_dir() -> String {
    format!("{}/../../assets", env!("CARGO_MANIFEST_DIR"))
}

/// One body's rig: what posing it needs.
pub struct BodyRig {
    pub model: SkinnedModel,
    pub bank: ClipBank,
    pub joints: CallbackJoints,
}

impl BodyRig {
    pub fn load_all() -> Result<Vec<BodyRig>, String> {
        let dir = assets_dir();
        let anim_dir = format!("{dir}/enemies/pd/bot_anims");
        BODIES
            .iter()
            .map(|name| {
                let path = format!("{dir}/enemies/pd/characters/{name}.glb");
                let model = gltf_skin::load(&path).map_err(|e| format!("body {path}: {e}"))?;
                let bank = ClipBank::load(&anim_dir, &model.skeleton).map_err(|e| format!("clips in {anim_dir}: {e}"))?;
                let joints = CallbackJoints::resolve(&model.skeleton).ok_or("PD body lacks the Bone_1..15 rig")?;
                Ok(BodyRig { model, bank, joints })
            })
            .collect()
    }

    /// The chr's joint world matrices (render space, metres) and its model matrix.
    pub fn pose(&self, c: &Chr) -> (Mat4, Vec<Mat4>) {
        let yaw = c.model_yaw();
        let globals = model::evaluate(&c.model, &self.bank, &self.model.skeleton, &self.joints, &c.joint_fx(), yaw, c.animscale);
        let model_mtx =
            Mat4::from_translation(c.pos / UNITS_PER_M) * Mat4::from_rotation_y(yaw) * Mat4::from_scale(Vec3::splat(CHAR_SCALE));
        (model_mtx, globals)
    }

    /// PD: a held gun's root transform IS the hand joint's matrix (`chr.c:1983`);
    /// the left hand adds rotZ(180°).
    pub fn gun_matrix(&self, model_mtx: Mat4, globals: &[Mat4], hand: Hand) -> Mat4 {
        match hand {
            Hand::Right => model_mtx * globals[self.joints.right_hand],
            Hand::Left => model_mtx * globals[self.joints.left_hand] * Mat4::from_rotation_z(std::f32::consts::PI),
        }
    }
}

/// The muzzle (PD units) of the gun held in `hand`, from its gun matrix.
pub fn muzzle(gun_mtx: Mat4, def: &weapons::WeaponDef) -> Vec3 {
    gun_mtx.transform_point3(Vec3::from(def.tp_muzzle)) * UNITS_PER_M
}

/// Drawn gun positions for headless runs.
pub struct GunPoser {
    bodies: Vec<BodyRig>,
}

impl GunPoser {
    pub fn load() -> Result<Self, String> {
        Ok(GunPoser { bodies: BodyRig::load_all()? })
    }

    /// Set every living chr's `gunpos_rendered` from its current pose, as the viewer
    /// does after each frame it draws (read by the next frame's shots).
    pub fn apply(&self, sim: &mut Sim) {
        for c in &mut sim.chrs {
            c.gunpos_rendered = [None; 2];
            if c.is_dead() {
                continue;
            }
            let Some(body) = self.bodies.get(c.body) else { continue };
            let (model_mtx, globals) = body.pose(c);
            let held: Vec<(Hand, weapons::WeaponId)> = c.held_weapons().collect();
            for (hand, w) in held {
                let Some(def) = weapons::get(w) else { continue };
                let k = if hand == Hand::Right { 0 } else { 1 };
                c.gunpos_rendered[k] = Some(muzzle(body.gun_matrix(model_mtx, &globals, hand), def));
            }
        }
    }
}
