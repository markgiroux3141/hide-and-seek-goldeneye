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

use crate::combat::config::WeaponStats;
use crate::mesh::TexVertex;
use crate::skeletal::gltf_skin::{to_rgba8, TexImage};

/// A contiguous index range sharing one base-color texture (one glTF primitive).
pub struct GunPrimitive {
    pub index_start: u32,
    pub index_count: u32,
    /// Index into [`GunModel::images`], or `None` for an untextured material
    /// (drawn with a white fallback).
    pub image: Option<usize>,
}

/// A loaded static weapon mesh: one shared vertex/index buffer split into
/// per-texture [`GunPrimitive`]s, plus the decoded images. Positions are baked
/// through the node hierarchy (the guns are multi-node static models), matching
/// the [`crate::gltf_load`] static loader — but with UVs + per-primitive textures
/// so the N64 weapon skins render.
pub struct GunModel {
    pub vertices: Vec<TexVertex>,
    pub indices: Vec<u32>,
    pub primitives: Vec<GunPrimitive>,
    pub images: Vec<TexImage>,
}

/// Load a weapon `gun.glb` into a [`GunModel`] — the whole model. Static (no
/// skin/anim): the GoldenEye weapon GLBs carry `POSITION / NORMAL / TEXCOORD_0`
/// with embedded textures. Node transforms are baked into positions (multi-node).
pub fn load_gun(path: &str) -> Result<GunModel, String> {
    load_filtered(path, |_| true)
}

/// Load ONLY the muzzle-flash billboards from a `muzzle.glb`. These GoldenEye
/// "muzzle" GLBs are actually the full *firing pose* — they re-contain the gun
/// body AND a hand/arm AND the flash. Drawing all of it flashes a hand into view,
/// so we keep only the additive flash quads, identified by their `CullBoth`
/// material (the gun/hand use plain or `ClampSClampT` materials). Returns an error
/// if no flash geometry is found (→ the caller renders no flash).
pub fn load_flash(path: &str) -> Result<GunModel, String> {
    load_filtered(path, |mat| mat.contains("CullBoth"))
}

/// Shared loader: walk the scene, keeping only primitives whose material name
/// passes `keep` (matched on the glTF material name; a nameless material is kept
/// only if `keep("")` is true).
fn load_filtered(path: &str, keep: impl Fn(&str) -> bool + Copy) -> Result<GunModel, String> {
    let (doc, buffers, images) =
        gltf::import(path).map_err(|e| format!("gltf import {path}: {e}"))?;

    let scene = doc
        .default_scene()
        .or_else(|| doc.scenes().next())
        .ok_or_else(|| format!("{path}: no scene"))?;

    let mut model = GunModel {
        vertices: Vec::new(),
        indices: Vec::new(),
        primitives: Vec::new(),
        images: Vec::new(),
    };
    for node in scene.nodes() {
        visit_node(node, Mat4::IDENTITY, &buffers, &mut model, keep);
    }
    if model.vertices.is_empty() {
        return Err(format!("{path}: no drawable geometry found (after material filter)"));
    }
    model.images = images.iter().map(to_rgba8).collect();
    Ok(model)
}

fn visit_node(
    node: gltf::Node,
    parent: Mat4,
    buffers: &[gltf::buffer::Data],
    out: &mut GunModel,
    keep: impl Fn(&str) -> bool + Copy,
) {
    let local = Mat4::from_cols_array_2d(&node.transform().matrix());
    let world = parent * local;
    let normal_mat = Mat4::from_mat3(glam::Mat3::from_mat4(world).inverse().transpose());

    if let Some(mesh) = node.mesh() {
        for prim in mesh.primitives() {
            // Material filter (e.g. keep only the flash billboards for a muzzle).
            let mat_name = prim.material().name().unwrap_or("");
            if !keep(mat_name) {
                continue;
            }
            let reader = prim.reader(|b| Some(&buffers[b.index()]));
            let Some(positions) = reader.read_positions() else {
                continue;
            };
            let positions: Vec<[f32; 3]> = positions.collect();
            let normals: Vec<[f32; 3]> = match reader.read_normals() {
                Some(n) => n.collect(),
                None => vec![[0.0, 1.0, 0.0]; positions.len()],
            };
            let uvs: Vec<[f32; 2]> = match reader.read_tex_coords(0) {
                Some(t) => t.into_f32().collect(),
                None => vec![[0.0, 0.0]; positions.len()],
            };

            let base = out.vertices.len() as u32;
            for i in 0..positions.len() {
                let wp = world.transform_point3(Vec3::from(positions[i]));
                let n = normals.get(i).copied().unwrap_or([0.0, 1.0, 0.0]);
                let wn = normal_mat.transform_vector3(Vec3::from(n)).normalize_or_zero();
                out.vertices.push(TexVertex {
                    pos: wp.to_array(),
                    normal: wn.to_array(),
                    uv: uvs.get(i).copied().unwrap_or([0.0, 0.0]),
                });
            }

            let index_start = out.indices.len() as u32;
            match reader.read_indices() {
                Some(idx) => out.indices.extend(idx.into_u32().map(|i| base + i)),
                None => out
                    .indices
                    .extend((0..positions.len() as u32).map(|i| base + i)),
            }
            let index_count = out.indices.len() as u32 - index_start;

            let image = prim
                .material()
                .pbr_metallic_roughness()
                .base_color_texture()
                .map(|info| info.texture().source().index());

            out.primitives.push(GunPrimitive {
                index_start,
                index_count,
                image,
            });
        }
    }

    for child in node.children() {
        visit_node(child, world, buffers, out, keep);
    }
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
