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

/// Load the muzzle-flash billboards from a `muzzle.glb`. These GoldenEye "muzzle"
/// GLBs are small additive quads (the flash variants). Most tag those quads with a
/// `CullBoth` material — historically some muzzle assets also packed a gun/hand
/// pose under other materials, so we prefer the `CullBoth`-tagged geometry to avoid
/// flashing a hand into view.
///
/// But four guns (DD44, Phantom, Shotgun, Laser) name their flash quads plain
/// `ClampSClampT` with no `CullBoth`, so that filter drops everything and they'd
/// render no flash. Since every muzzle GLB in the set is tiny (≤24 tris — pure
/// flash, no hand), fall back to loading the whole GLB when nothing is tagged
/// `CullBoth`. Returns an error only if the GLB has no drawable geometry at all.
pub fn load_flash(path: &str) -> Result<TexturedModel, String> {
    textured_model::load(path, |mat| mat.contains("CullBoth"))
        .or_else(|_| textured_model::load(path, |_| true))
}

/// Vertical field of view for the viewmodel projection (radians). Matches the
/// world camera's 60° (`camera::view_proj_from`) so the gun sits consistently
/// against the scene. JS used a separate 75° weapon camera; we keep one FOV.
const VIEWMODEL_FOV: f32 = 60.0;

/// How far the gun dips out of view at the low point of the reload animation
/// (view-space metres downward). JS `WeaponViewmodel` uses `0.6`.
const RELOAD_DIP: f32 = 0.6;

/// The runtime viewmodel state: the weapon config plus the animated overlay state
/// (recoil + the reload dip; bob/sway later). Ported from `WeaponViewmodel.ts`.
pub struct ViewModel {
    pub config: WeaponStats,
    /// Recoil kick-back distance (metres, +Z = toward the viewer) and muzzle-up
    /// tilt (radians). Set on fire, decayed each frame (JS `recoilZ`/`recoilRot`).
    recoil_z: f32,
    recoil_rot: f32,
    /// Reload dip progress in `[0, 1]`, or `-1` when not reloading (JS
    /// `reloadProgress`). Drives the gun lowering out of view + returning.
    reload_progress: f32,
    /// Weapon-switch dip progress in `[0, 1]`, or `-1` when not switching. Driven
    /// by `World`'s switch state machine (NOT ticked here) — `< 0.5` lowers the
    /// outgoing gun, `>= 0.5` raises the incoming one, using the same half-sine
    /// dip curve as reload but at a fixed switch speed. Mirrors the JS
    /// `playLowerAnimation` / `playRaiseAnimation` handoff (which swaps the mesh at
    /// the bottom), so the old gun drops away and the new one pops up.
    switch_t: f32,
}

impl ViewModel {
    pub fn new(config: WeaponStats) -> Self {
        ViewModel {
            config,
            recoil_z: 0.0,
            recoil_rot: 0.0,
            reload_progress: -1.0,
            switch_t: -1.0,
        }
    }

    /// Punch the viewmodel on fire (JS `WeaponViewmodel.playRecoil`): snap the
    /// kick-back + muzzle-up to the weapon's configured amounts.
    pub fn play_recoil(&mut self) {
        self.recoil_z = self.config.recoil_z;
        self.recoil_rot = self.config.recoil_rot;
    }

    /// Start the reload dip (JS `WeaponViewmodel.playReloadAnimation`): the gun
    /// lowers out of view and returns. Called from [`super::Weapon::start_reload`]
    /// so both the manual `R` and the empty auto-reload play it.
    pub fn play_reload(&mut self) {
        self.reload_progress = 0.0;
    }

    /// Whether the reload dip is currently animating.
    pub fn is_reloading(&self) -> bool {
        self.reload_progress >= 0.0
    }

    /// Snap the reload dip back to rest (weapon swap — see [`super::Weapon::cancel_reload`]).
    pub fn cancel_reload(&mut self) {
        self.reload_progress = -1.0;
    }

    /// Set the weapon-switch dip progress (`[0, 1]`), driven by `World`'s switch
    /// state machine each frame. The gun sits lowest at `0.5`.
    pub fn set_switch_t(&mut self, t: f32) {
        self.switch_t = t;
    }

    /// End the switch dip (back to rest). Called on the outgoing gun at the swap
    /// point and on the incoming gun when the raise completes.
    pub fn cancel_switch(&mut self) {
        self.switch_t = -1.0;
    }

    /// This frame's switch-dip offset (view-space metres, ≤ 0 = down): the same
    /// half-sine as the reload dip, 0 at the ends and `-RELOAD_DIP` at `t = 0.5`.
    /// Zero when not switching.
    fn switch_offset_y(&self) -> f32 {
        if self.switch_t >= 0.0 {
            -(self.switch_t * std::f32::consts::PI).sin() * RELOAD_DIP
        } else {
            0.0
        }
    }

    /// Advance the per-frame viewmodel animation: decay the recoil toward rest
    /// (JS: `recoilZ *= 1 - dt*15`, `recoilRot *= 1 - dt*10` — the kick-back snaps
    /// back faster than the tilt) and advance the reload dip.
    ///
    /// **Deviation from JS (deliberate):** the JS advances `reloadProgress` at
    /// `dt / (reloadTime * 0.5)` (a "2× speed" dip that finishes at half the reload,
    /// leaving the gun up-but-unfireable for the rest). We advance over the *full*
    /// `reload_time`, so the gun stays lowered for the whole reload and returns
    /// exactly as firing re-enables — better feel. Revert by dividing dt by
    /// `reload_time * 0.5` to match the oracle.
    pub fn tick(&mut self, dt: f32) {
        self.recoil_z *= (1.0 - dt * 15.0).max(0.0);
        self.recoil_rot *= (1.0 - dt * 10.0).max(0.0);
        if self.reload_progress >= 0.0 {
            self.reload_progress += dt / self.config.reload_time.max(1e-3);
            if self.reload_progress >= 1.0 {
                self.reload_progress = -1.0;
            }
        }
    }

    /// This frame's reload dip offset (view-space metres, ≤ 0 = down). A half-sine
    /// over the reload: 0 at the ends, `-RELOAD_DIP` at the midpoint (JS
    /// `-sin(progress·π) · 0.6`). Zero when not reloading.
    fn reload_offset_y(&self) -> f32 {
        if self.reload_progress >= 0.0 {
            -(self.reload_progress * std::f32::consts::PI).sin() * RELOAD_DIP
        } else {
            0.0
        }
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
        // The reload dip lowers the whole gun group in view space (Y down), like
        // recoil kick-back (Z) — both on the outer `model` translation.
        let model = Mat4::from_translation(
            c.model_offset
                + Vec3::new(0.0, self.reload_offset_y() + self.switch_offset_y(), self.recoil_z),
        )
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

    fn muzzle_path(gun: &str) -> String {
        format!("{}/../../assets/weapons/{}/muzzle.glb", env!("CARGO_MANIFEST_DIR"), gun)
    }

    /// The muzzle-flash loads for both the `CullBoth`-tagged guns (pp7) AND the
    /// guns whose flash quads are plain `ClampSClampT` (dd44, phantom, shotgun,
    /// laser) — the latter via the whole-GLB fallback. Regression for the four guns
    /// that had no flash when the loader kept only `CullBoth` materials.
    #[test]
    fn muzzle_flash_loads_for_cullboth_and_plain_guns() {
        for gun in ["pp7", "dd44", "phantom", "shotgun", "laser"] {
            let m = load_flash(&muzzle_path(gun)).unwrap_or_else(|e| panic!("{gun} flash: {e}"));
            assert!(!m.vertices.is_empty(), "{gun} flash has geometry");
            assert!(!m.primitives.is_empty(), "{gun} flash has primitives");
        }
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

    /// The reload dip lowers the gun (negative Y offset) at mid-reload and returns
    /// it to rest by the end of `reload_time` (whatever that is configured to).
    #[test]
    fn reload_dips_the_gun_then_returns() {
        let vm0 = ViewModel::new(crate::combat::config::PP7);
        let reload_time = vm0.config.reload_time;
        let dt = 0.001; // fine steps so the frame count tracks reload_time exactly
        let mid = (reload_time * 0.5 / dt) as u32; // frames to the deepest dip
        let end = (reload_time * 1.2 / dt) as u32; // comfortably past the end

        let mut vm = vm0;
        assert_eq!(vm.reload_offset_y(), 0.0, "at rest before reload");
        vm.play_reload();
        assert!(vm.is_reloading());
        for _ in 0..mid {
            vm.tick(dt);
        }
        assert!(vm.reload_offset_y() < -0.4, "gun dipped out of view mid-reload");
        for _ in 0..(end - mid) {
            vm.tick(dt);
        }
        assert!(!vm.is_reloading(), "reload dip finished");
        assert_eq!(vm.reload_offset_y(), 0.0, "gun returned to rest");
    }
}
