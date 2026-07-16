//! First-person weapon viewmodel: the static gun GLB loaded into a textured mesh,
//! plus the view-space transform that places it on screen. Transliterated from
//! `src/weapons/WeaponViewmodel.ts`.
//!
//! **The gun is a pure overlay.** It's positioned in *camera/view space* (a fixed
//! offset in front of the eye) and projected on its own — it never uses the world
//! view matrix, so it stays locked to the screen wherever the player looks. The
//! renderer draws it in a second pass with the **depth buffer cleared**, so it's
//! always on top and never clips into walls (exactly like a real FPS view weapon).
//! The fire ray comes from the camera centre (crosshair), NOT the muzzle — the gun
//! only visually tracks (JS comment: "gun just visually tracks").
//!
//! Bob / sway / recoil / muzzle-flash come in later milestones (P2/P4); P1 is the
//! static placed gun.

use glam::{EulerRot, Mat4, Quat, Vec3};

use engine::assets::textured_model::{self, TexturedModel};
use crate::combat::config::WeaponStats;

/// Load a weapon `gun.glb` into a [`TexturedModel`] — the whole model. Static
/// (no skin/anim): the GoldenEye weapon GLBs carry `POSITION / NORMAL /
/// TEXCOORD_0` with embedded textures.
pub fn load_gun(path: &str) -> Result<TexturedModel, String> {
    textured_model::load(path, |_| true)
}

/// Load ONLY the muzzle-flash billboards from a `muzzle.glb`. These GoldenEye
/// "muzzle" GLBs are actually the full *firing pose* — they re-contain the gun
/// body AND a hand/arm AND the flash. Drawing all of it flashes a hand into view,
/// so we keep only the additive flash quads, identified by their `CullBoth`
/// material (the gun/hand use plain or `ClampSClampT` materials). Returns an error
/// if no flash geometry is found (→ the caller renders no flash).
pub fn load_flash(path: &str) -> Result<TexturedModel, String> {
    textured_model::load(path, |mat| mat.contains("CullBoth"))
}

/// Vertical field of view for the viewmodel projection (radians). Matches the
/// world camera's 60° (`camera::view_proj_from`) so the gun sits consistently
/// against the scene. JS used a separate 75° weapon camera; we keep one FOV.
const VIEWMODEL_FOV: f32 = 60.0;

/// The runtime viewmodel state: the weapon config plus the animated overlay state
/// (recoil now; bob/sway later). Ported from `WeaponViewmodel.ts`.
pub struct ViewModel {
    pub config: WeaponStats,
    /// Recoil kick-back distance (metres, +Z = toward the viewer) and muzzle-up
    /// tilt (radians). Set on fire, decayed each frame (JS `recoilZ`/`recoilRot`).
    recoil_z: f32,
    recoil_rot: f32,
}

impl ViewModel {
    pub fn new(config: WeaponStats) -> Self {
        ViewModel {
            config,
            recoil_z: 0.0,
            recoil_rot: 0.0,
        }
    }

    /// Punch the viewmodel on fire (JS `WeaponViewmodel.playRecoil`): snap the
    /// kick-back + muzzle-up to the weapon's configured amounts.
    pub fn play_recoil(&mut self) {
        self.recoil_z = self.config.recoil_z;
        self.recoil_rot = self.config.recoil_rot;
    }

    /// Decay the recoil toward rest each frame (JS: `recoilZ *= 1 - dt*15`,
    /// `recoilRot *= 1 - dt*10` — the kick-back snaps back faster than the tilt).
    pub fn tick_recoil(&mut self, dt: f32) {
        self.recoil_z *= (1.0 - dt * 15.0).max(0.0);
        self.recoil_rot *= (1.0 - dt * 10.0).max(0.0);
    }

    /// The clip transform for the gun this frame: `projection · viewmodel`, where
    /// `viewmodel` places the mesh in view space per `WeaponConfig`
    /// (offset · pivot · rotation · scale). No world/view matrix — the gun is an
    /// overlay locked to the screen. `aspect` = framebuffer width / height.
    ///
    /// Mirrors the JS node graph: `weaponCamera → model(position=modelOffset +
    /// recoilZ, rotation=aim + recoilRot) → gunGltf(position=pivotOffset,
    /// rotation=modelRotation, scale=modelScale)`. The recoil kick-back (+Z) and
    /// the aim/recoil tilt live on the outer `model` group.
    ///
    /// `(aim_x, aim_y)` is the free-aim crosshair offset in aim space; the gun
    /// rotates to point toward the crosshair (JS `WeaponViewmodel.setAimOffset`:
    /// `yaw = atan(aim·tan(fov/2))`), so it visually tracks where the bullet goes.
    pub fn clip_transform(&self, aspect: f32, aim_x: f32, aim_y: f32) -> Mat4 {
        let c = &self.config;
        let proj = Mat4::perspective_rh(VIEWMODEL_FOV.to_radians(), aspect, 0.01, 10.0);
        // Aim tilt toward the crosshair (yaw about Y, pitch about X), plus recoil.
        let tan = (VIEWMODEL_FOV * 0.5).to_radians().tan();
        let yaw = (aim_x * tan).atan();
        let pitch = (aim_y * tan).atan();
        let aim_rot = Mat4::from_rotation_y(-yaw) * Mat4::from_rotation_x(pitch + self.recoil_rot);
        let model = Mat4::from_translation(c.model_offset + Vec3::new(0.0, 0.0, self.recoil_z))
            * aim_rot
            * Mat4::from_translation(c.pivot_offset)
            * Mat4::from_quat(euler_xyz(c.model_rotation))
            * Mat4::from_scale(Vec3::splat(c.model_scale));
        proj * model
    }
}

/// Build a quaternion from an XYZ Euler triple (three.js `modelRotation`
/// convention). All curated weapons use a single-axis yaw, so order is moot, but
/// this keeps the general case faithful.
fn euler_xyz(e: Vec3) -> Quat {
    Quat::from_euler(EulerRot::XYZ, e.x, e.y, e.z)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pp7_path() -> String {
        format!("{}/../../assets/weapons/pp7/gun.glb", env!("CARGO_MANIFEST_DIR"))
    }

    /// The static gun loader works end-to-end on the real PP7 asset: textured
    /// vertices, per-primitive material images, decoded textures. Headless half of
    /// the P1 oracle (the live launch confirms placement/pixels).
    #[test]
    fn loads_pp7_gun() {
        let m = load_gun(&pp7_path()).expect("load pp7 gun");
        assert!(!m.vertices.is_empty(), "has textured vertices");
        assert!(!m.primitives.is_empty(), "has per-texture primitives");
        assert!(!m.images.is_empty(), "decoded embedded textures");
        for img in &m.images {
            assert_eq!(img.rgba.len(), (img.width * img.height * 4) as usize);
        }
        // Indices stay within the shared vertex buffer.
        let vn = m.vertices.len() as u32;
        assert!(m.indices.iter().all(|&i| i < vn), "index out of range");
    }

    /// The clip transform is finite and scales the gun down into view space (the
    /// PP7 mesh is GoldenEye-sized; `model_scale` 0.0007 shrinks it to metres).
    #[test]
    fn clip_transform_is_finite() {
        let vm = ViewModel::new(crate::combat::config::PP7);
        // Centered and at a free-aim offset — both finite.
        assert!(vm.clip_transform(16.0 / 9.0, 0.0, 0.0).to_cols_array().iter().all(|v| v.is_finite()));
        assert!(vm.clip_transform(16.0 / 9.0, 0.5, -0.3).to_cols_array().iter().all(|v| v.is_finite()));
    }
}
