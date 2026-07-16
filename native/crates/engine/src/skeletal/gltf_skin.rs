//! Load a GoldenEye character GLB into a [`SkinnedModel`]: skinned geometry
//! (`POSITION / TEXCOORD_0 / JOINTS_0 / WEIGHTS_0`), one [`super::Skeleton`], the
//! per-primitive base-color textures, and the bind-pose bounds (for seating the
//! feet on the floor).
//!
//! Node transforms are **not** baked into skinned-mesh positions — a skinned
//! primitive's vertices live in skin space and are placed by the joint matrices,
//! per the glTF spec. Baking the node transform (as the static [`crate::assets::gltf_load`]
//! loader does) would double-transform them.

use std::collections::HashMap;

use glam::{Mat4, Vec3};

use crate::render::mesh::SkinVertex;
use crate::skeletal::Skeleton;

/// One decoded RGBA8 texture image from the GLB.
pub struct TexImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// A contiguous index range sharing one base-color texture (one glTF primitive).
pub struct SkinPrimitive {
    pub index_start: u32,
    pub index_count: u32,
    /// Index into [`SkinnedModel::images`], or `None` if the material has no
    /// base-color texture (drawn with a white fallback).
    pub image: Option<usize>,
}

/// A fully-loaded skinned character: one shared vertex/index buffer split into
/// per-texture [`SkinPrimitive`]s, the [`Skeleton`], and bind-pose bounds
/// (model space, pre-scale) so the caller can seat the feet exactly.
pub struct SkinnedModel {
    pub vertices: Vec<SkinVertex>,
    pub indices: Vec<u32>,
    pub primitives: Vec<SkinPrimitive>,
    pub images: Vec<TexImage>,
    pub skeleton: Skeleton,
    pub bounds_min: Vec3,
    pub bounds_max: Vec3,
}

impl SkinnedModel {
    /// Lowest skinned vertex Y (model space) under a given set of joint
    /// (skinning) matrices — the CPU mirror of the shader's LBS. Used to seat a
    /// posed character's feet on the floor: the bind-pose AABB is useless for
    /// this (the GoldenEye bind pose is a splayed star), so callers sample an
    /// actual animation pose instead.
    pub fn skinned_min_y(&self, joints: &[Mat4]) -> f32 {
        let mut min_y = f32::INFINITY;
        for v in &self.vertices {
            let src = Vec3::from(v.pos);
            let mut y = 0.0;
            for k in 0..4 {
                let w = v.weights[k];
                if w != 0.0 {
                    let j = v.joints[k] as usize;
                    if let Some(m) = joints.get(j) {
                        y += w * m.transform_point3(src).y;
                    }
                }
            }
            min_y = min_y.min(y);
        }
        min_y
    }
}

pub fn load(path: &str) -> Result<SkinnedModel, String> {
    let (doc, buffers, images) =
        gltf::import(path).map_err(|e| format!("gltf import {path}: {e}"))?;

    // ── Build a node → parent-node map by walking every scene node. glTF nodes
    // only list children, so parents are recovered here.
    let mut node_parent: HashMap<usize, usize> = HashMap::new();
    for node in doc.nodes() {
        for child in node.children() {
            node_parent.insert(child.index(), node.index());
        }
    }

    // ── Skeleton from the (single) skin.
    let skin = doc
        .skins()
        .next()
        .ok_or_else(|| format!("{path}: no skin"))?;

    let joint_nodes: Vec<usize> = skin.joints().map(|n| n.index()).collect();
    let joint_pos: HashMap<usize, usize> = joint_nodes
        .iter()
        .enumerate()
        .map(|(i, &n)| (n, i))
        .collect();

    let names: Vec<String> = skin
        .joints()
        .enumerate()
        .map(|(i, n)| n.name().map(String::from).unwrap_or_else(|| format!("joint{i}")))
        .collect();

    let local_bind: Vec<Mat4> = skin
        .joints()
        .map(|n| Mat4::from_cols_array_2d(&n.transform().matrix()))
        .collect();

    // Decompose each joint's bind TRS so rotation-only animation channels can
    // recompose against the bind translation + scale.
    let mut bind_t: Vec<glam::Vec3> = Vec::new();
    let mut bind_r: Vec<glam::Quat> = Vec::new();
    let mut bind_s: Vec<glam::Vec3> = Vec::new();
    for n in skin.joints() {
        let (t, r, s) = n.transform().decomposed();
        bind_t.push(glam::Vec3::from(t));
        bind_r.push(glam::Quat::from_array(r));
        bind_s.push(glam::Vec3::from(s));
    }

    let parents: Vec<Option<usize>> = joint_nodes
        .iter()
        .map(|&jn| node_parent.get(&jn).and_then(|p| joint_pos.get(p).copied()))
        .collect();

    let ibm_reader = skin.reader(|b| Some(&buffers[b.index()]));
    let inverse_bind: Vec<Mat4> = match ibm_reader.read_inverse_bind_matrices() {
        Some(iter) => iter.map(|m| Mat4::from_cols_array_2d(&m)).collect(),
        // Spec default when omitted: identity per joint.
        None => vec![Mat4::IDENTITY; joint_nodes.len()],
    };
    if inverse_bind.len() != joint_nodes.len() {
        return Err(format!(
            "{path}: {} inverse-bind matrices for {} joints",
            inverse_bind.len(),
            joint_nodes.len()
        ));
    }

    let skeleton = Skeleton {
        names,
        parents,
        local_bind,
        bind_t,
        bind_r,
        bind_s,
        inverse_bind,
    };

    // ── Geometry. Every mesh primitive in the document is loaded into one shared
    // buffer (the character is a single skinned mesh split by material). Positions
    // are taken raw (skin space) — no node-transform baking.
    let mut vertices: Vec<SkinVertex> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut primitives: Vec<SkinPrimitive> = Vec::new();
    let mut bmin = Vec3::splat(f32::INFINITY);
    let mut bmax = Vec3::splat(f32::NEG_INFINITY);

    for mesh in doc.meshes() {
        for prim in mesh.primitives() {
            let reader = prim.reader(|b| Some(&buffers[b.index()]));
            let Some(positions) = reader.read_positions() else {
                continue;
            };
            let positions: Vec<[f32; 3]> = positions.collect();

            let uvs: Vec<[f32; 2]> = match reader.read_tex_coords(0) {
                Some(t) => t.into_f32().collect(),
                None => vec![[0.0, 0.0]; positions.len()],
            };
            let joints: Vec<[u16; 4]> = match reader.read_joints(0) {
                Some(j) => j.into_u16().collect(),
                None => vec![[0, 0, 0, 0]; positions.len()],
            };
            let weights: Vec<[f32; 4]> = match reader.read_weights(0) {
                Some(w) => w.into_f32().collect(),
                None => vec![[1.0, 0.0, 0.0, 0.0]; positions.len()],
            };

            let base = vertices.len() as u32;
            for i in 0..positions.len() {
                let p = positions[i];
                bmin = bmin.min(Vec3::from(p));
                bmax = bmax.max(Vec3::from(p));
                let j = joints.get(i).copied().unwrap_or([0; 4]);
                let mut w = weights.get(i).copied().unwrap_or([1.0, 0.0, 0.0, 0.0]);
                // Defensive normalize — LBS assumes weights sum to 1.
                let sum = w[0] + w[1] + w[2] + w[3];
                if sum > 1e-6 {
                    for k in 0..4 {
                        w[k] /= sum;
                    }
                }
                vertices.push(SkinVertex {
                    pos: p,
                    uv: uvs.get(i).copied().unwrap_or([0.0, 0.0]),
                    joints: [j[0] as u32, j[1] as u32, j[2] as u32, j[3] as u32],
                    weights: w,
                });
            }

            let index_start = indices.len() as u32;
            match reader.read_indices() {
                Some(idx) => indices.extend(idx.into_u32().map(|i| base + i)),
                None => indices.extend((0..positions.len() as u32).map(|i| base + i)),
            }
            let index_count = indices.len() as u32 - index_start;

            let image = prim
                .material()
                .pbr_metallic_roughness()
                .base_color_texture()
                .map(|info| info.texture().source().index());

            primitives.push(SkinPrimitive {
                index_start,
                index_count,
                image,
            });
        }
    }

    if vertices.is_empty() {
        return Err(format!("{path}: no skinned geometry"));
    }

    // ── Decode every image to RGBA8.
    let images: Vec<TexImage> = images.iter().map(to_rgba8).collect();

    Ok(SkinnedModel {
        vertices,
        indices,
        primitives,
        images,
        skeleton,
        bounds_min: bmin,
        bounds_max: bmax,
    })
}

/// Convert a `gltf`-decoded image (whatever channel layout) to tightly-packed
/// RGBA8. Covers the formats the character PNGs actually use (RGB / RGBA) plus
/// grayscale, with a magenta fallback for anything unexpected so a decode gap is
/// obvious on screen rather than silent. Shared with the static textured-model
/// loader ([`crate::assets::textured_model`]), which decodes the same GLB
/// asset family (weapons/props).
pub(crate) fn to_rgba8(img: &gltf::image::Data) -> TexImage {
    use gltf::image::Format;
    let (w, h) = (img.width, img.height);
    let px = &img.pixels;
    let n = (w * h) as usize;
    let mut rgba = Vec::with_capacity(n * 4);
    match img.format {
        Format::R8G8B8A8 => rgba.extend_from_slice(px),
        Format::R8G8B8 => {
            for c in px.chunks_exact(3) {
                rgba.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        Format::R8 => {
            for &g in px {
                rgba.extend_from_slice(&[g, g, g, 255]);
            }
        }
        Format::R8G8 => {
            for c in px.chunks_exact(2) {
                rgba.extend_from_slice(&[c[0], c[0], c[0], c[1]]);
            }
        }
        other => {
            log::warn!("unsupported glTF image format {other:?}; using magenta placeholder");
            for _ in 0..n {
                rgba.extend_from_slice(&[255, 0, 255, 255]);
            }
        }
    }
    TexImage {
        width: w,
        height: h,
        rgba,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn karl_path() -> String {
        format!(
            "{}/../../assets/enemies/characters/russian-guard_karl.glb",
            env!("CARGO_MANIFEST_DIR")
        )
    }

    /// The load path works end-to-end on the real asset: skinned attributes,
    /// the shared 15-bone skeleton, decoded PNG textures, and bind-pose bounds.
    /// This is the headless half of the B1 oracle (the live launch is the other).
    #[test]
    fn loads_russian_guard_karl() {
        let m = load(&karl_path()).expect("load karl");
        assert_eq!(m.skeleton.joint_count(), 15, "shared 15-bone skeleton");
        assert_eq!(m.skeleton.names[0], "Bone_1");
        assert_eq!(m.skeleton.names[8], "Bone_9", "right-hand weapon bone");
        assert!(!m.vertices.is_empty(), "has skinned vertices");
        assert!(!m.primitives.is_empty(), "has per-texture primitives");
        assert!(!m.images.is_empty(), "decoded embedded textures");
        // Every referenced image decoded to a non-empty RGBA8 buffer.
        for img in &m.images {
            assert_eq!(img.rgba.len(), (img.width * img.height * 4) as usize);
        }
        // Bind pose reduces to identity joint matrices — the correctness oracle.
        for mat in m.skeleton.bind_pose_matrices() {
            let d = (mat - Mat4::IDENTITY).to_cols_array();
            assert!(d.iter().all(|v| v.abs() < 1e-3), "bind matrix not ~identity");
        }
        // Joint indices in the mesh stay within the skeleton.
        for v in &m.vertices {
            for &j in &v.joints {
                assert!((j as usize) < 15, "joint index {j} out of range");
            }
        }
    }
}
