//! Minimal glTF/GLB loader. Phase 0 pulls static geometry (positions, normals,
//! indices) out of every mesh primitive in the file — skinning/animation come
//! later when the enemy character system lands. Baked node transforms are
//! applied so multi-node models render in the right place.

use glam::Mat4;

use crate::render::mesh::{CpuMesh, Vertex};

/// Load a `.glb`/`.gltf` and merge all primitives into one [`CpuMesh`].
pub fn load(path: &str) -> Result<CpuMesh, String> {
    let (doc, buffers, _images) =
        gltf::import(path).map_err(|e| format!("gltf import {path}: {e}"))?;

    let mut out = CpuMesh::default();

    // Walk the default scene's node hierarchy so we get world-space geometry.
    let scene = doc
        .default_scene()
        .or_else(|| doc.scenes().next())
        .ok_or_else(|| format!("{path}: no scene"))?;

    for node in scene.nodes() {
        visit_node(node, Mat4::IDENTITY, &buffers, &mut out);
    }

    if out.vertices.is_empty() {
        return Err(format!("{path}: no drawable geometry found"));
    }
    Ok(out)
}

fn visit_node(
    node: gltf::Node,
    parent: Mat4,
    buffers: &[gltf::buffer::Data],
    out: &mut CpuMesh,
) {
    let local = Mat4::from_cols_array_2d(&node.transform().matrix());
    let world = parent * local;
    let normal_mat = Mat4::from_mat3(glam::Mat3::from_mat4(world).inverse().transpose());

    if let Some(mesh) = node.mesh() {
        for prim in mesh.primitives() {
            let reader = prim.reader(|b| Some(&buffers[b.index()]));
            let Some(positions) = reader.read_positions() else {
                continue;
            };
            let positions: Vec<[f32; 3]> = positions.collect();

            // Normals may be absent; fall back to +Y so shading is at least defined.
            let normals: Vec<[f32; 3]> = match reader.read_normals() {
                Some(n) => n.collect(),
                None => vec![[0.0, 1.0, 0.0]; positions.len()],
            };

            let base = out.vertices.len() as u32;
            for (i, p) in positions.iter().enumerate() {
                let wp = world.transform_point3(glam::Vec3::from(*p));
                let n = normals.get(i).copied().unwrap_or([0.0, 1.0, 0.0]);
                let wn = normal_mat
                    .transform_vector3(glam::Vec3::from(n))
                    .normalize_or_zero();
                out.vertices.push(Vertex {
                    pos: wp.to_array(),
                    normal: wn.to_array(),
                });
            }

            match reader.read_indices() {
                Some(idx) => out.indices.extend(idx.into_u32().map(|i| base + i)),
                // Non-indexed primitive: emit a trivial 0..n index list.
                None => out
                    .indices
                    .extend((0..positions.len() as u32).map(|i| base + i)),
            }
        }
    }

    for child in node.children() {
        visit_node(child, world, buffers, out);
    }
}
