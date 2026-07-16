//! A loaded static textured model — the generic render asset behind any
//! multi-node, multi-material glTF/GLB that carries `POSITION / NORMAL /
//! TEXCOORD_0` with embedded base-color textures (the N64-style weapon/prop
//! GLBs). Positions are baked through the node hierarchy; the model is split
//! into per-texture [`TexturedPrimitive`]s so each material's skin renders.
//!
//! This is domain-free: the engine renderer uploads it verbatim
//! ([`crate::render::renderer::Renderer::upload_viewmodel`]). Game-specific
//! loading policy (which GLB, which material filter) lives with the caller —
//! e.g. the game's weapon viewmodel.

use glam::{Mat4, Vec3};

use crate::render::mesh::TexVertex;
use crate::skeletal::gltf_skin::{to_rgba8, TexImage};

/// A contiguous index range sharing one base-color texture (one glTF primitive).
pub struct TexturedPrimitive {
    pub index_start: u32,
    pub index_count: u32,
    /// Index into [`TexturedModel::images`], or `None` for an untextured
    /// material (drawn with a white fallback).
    pub image: Option<usize>,
}

/// A loaded static textured model: one shared vertex/index buffer split into
/// per-texture [`TexturedPrimitive`]s, plus the decoded images.
pub struct TexturedModel {
    pub vertices: Vec<TexVertex>,
    pub indices: Vec<u32>,
    pub primitives: Vec<TexturedPrimitive>,
    pub images: Vec<TexImage>,
}

/// Load a GLB into a [`TexturedModel`], keeping only primitives whose glTF
/// material name passes `keep` (a nameless material is offered as `""`). Pass
/// `|_| true` to keep everything. Node transforms are baked into positions
/// (these models are multi-node). Errors if nothing drawable survives the
/// filter.
pub fn load(path: &str, keep: impl Fn(&str) -> bool + Copy) -> Result<TexturedModel, String> {
    let (doc, buffers, images) =
        gltf::import(path).map_err(|e| format!("gltf import {path}: {e}"))?;

    let scene = doc
        .default_scene()
        .or_else(|| doc.scenes().next())
        .ok_or_else(|| format!("{path}: no scene"))?;

    let mut model = TexturedModel {
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
    out: &mut TexturedModel,
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

            out.primitives.push(TexturedPrimitive {
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
