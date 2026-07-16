//! `World` geometry builders: face-quad + box meshes and the wall/stair-void
//! subtract-brush constructors. Pure construction, no `World` state beyond
//! `face_quad_mesh`'s read of the camera-independent selection.

use super::*;

impl World {
    /// A translucent quad over a face rectangle (meters), nudged slightly toward
    /// the room interior so it sits in front of the wall. Shared by the selection
    /// highlight and the door ghost.
    pub(crate) fn face_quad_mesh(
        &self,
        axis: Axis,
        side: Side,
        position: f32,
        u_axis: Axis,
        v_axis: Axis,
        u0: f32,
        u1: f32,
        v0: f32,
        v1: f32,
    ) -> CpuMesh {
        // Interior is +axis for a Min face, −axis for a Max face.
        let a = position + if side == Side::Max { -0.06 } else { 0.06 };
        let corner = |u: f32, v: f32| -> [f32; 3] {
            let mut p = [0.0f32; 3];
            p[axis.index()] = a;
            p[u_axis.index()] = u;
            p[v_axis.index()] = v;
            [p[0] * WORLD_SCALE, p[1] * WORLD_SCALE, p[2] * WORLD_SCALE]
        };
        let quad = [corner(u0, v0), corner(u1, v0), corner(u1, v1), corner(u0, v1)];
        let n = axis.normal();
        let mut positions = Vec::with_capacity(12);
        let mut normals = Vec::with_capacity(12);
        for c in &quad {
            positions.extend_from_slice(c);
            normals.extend_from_slice(&n);
        }
        // Two tris; cull is disabled in the highlight pipeline so winding is moot.
        let indices = vec![0u32, 1, 2, 0, 2, 3];
        CpuMesh::from_csg(&positions, &normals, &indices)
    }
}

/// Build a subtract brush for a wall carve from face-relative parameters: `a`/`da`
/// are the min corner + size along the face-normal `axis`; `(u0,du)` and `(v0,dv)`
/// are the extents along the two in-plane axes. Mirrors the axis dispatch in JS
/// `confirmHolePlacement`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn make_wall_brush(
    id: u32,
    axis: Axis,
    a: f32,
    da: f32,
    u_axis: Axis,
    u0: f32,
    du: f32,
    v_axis: Axis,
    v0: f32,
    dv: f32,
) -> Brush {
    let mut p = [0.0f32; 3];
    let mut s = [0.0f32; 3];
    p[axis.index()] = a;
    s[axis.index()] = da;
    p[u_axis.index()] = u0;
    s[u_axis.index()] = du;
    p[v_axis.index()] = v0;
    s[v_axis.index()] = dv;
    Brush::new(id, Op::Subtract, p[0], p[1], p[2], s[0], s[1], s[2])
}

/// Build a combined mesh (meters) of one or more WT AABB boxes `[x,y,z,w,h,d]`,
/// for the pillar/brace ghost preview (drawn via the translucent highlight
/// pipeline). Uses the CSG box helper so winding matches the region meshes.
pub(crate) fn boxes_mesh(boxes: &[[f32; 6]]) -> CpuMesh {
    let mut positions: Vec<f32> = Vec::new();
    let mut normals: Vec<f32> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    for b in boxes {
        let c = [
            (b[0] + b[3] * 0.5) * WORLD_SCALE,
            (b[1] + b[4] * 0.5) * WORLD_SCALE,
            (b[2] + b[5] * 0.5) * WORLD_SCALE,
        ];
        let half = [
            b[3] * 0.5 * WORLD_SCALE,
            b[4] * 0.5 * WORLD_SCALE,
            b[5] * 0.5 * WORLD_SCALE,
        ];
        let polys = csg::box_polygons(c, half);
        let (p, n, i) = csg::polygons_to_mesh(&polys);
        let base = (positions.len() / 3) as u32;
        positions.extend_from_slice(&p);
        normals.extend_from_slice(&n);
        indices.extend(i.iter().map(|idx| idx + base));
    }
    CpuMesh::from_csg(&positions, &normals, &indices)
}

/// Append a solid colored box (meters AABB `min`..`max`, flat `rgb`) to a gizmo
/// mesh buffer. Uses the CSG box helper for winding-consistent faces.
pub(crate) fn push_colored_box(verts: &mut Vec<ColorVertex>, idx: &mut Vec<u32>, min: Vec3, max: Vec3, rgb: [f32; 3]) {
    let center = ((min + max) * 0.5).to_array();
    let half = ((max - min) * 0.5).to_array();
    let polys = csg::box_polygons(center, half);
    let (p, _n, i) = csg::polygons_to_mesh(&polys);
    let base = verts.len() as u32;
    for c in p.chunks_exact(3) {
        verts.push(ColorVertex {
            pos: [c[0], c[1], c[2]],
            color: rgb,
        });
    }
    idx.extend(i.iter().map(|k| k + base));
}

/// Build a stair void `subtract` brush (JS `csgActions.makeBrush`): `lo`/`hi` are
/// the span along the wall-normal `axis`, `y_min`/`y_max` the vertical extent, and
/// `(u0, u1)` the horizontal span along the in-plane `u_axis`. The vertical axis
/// is always world-up Y.
#[allow(clippy::too_many_arguments)]
pub(crate) fn make_stair_void(
    id: u32,
    axis: Axis,
    lo: f32,
    hi: f32,
    y_min: f32,
    y_max: f32,
    u_axis: Axis,
    u0: f32,
    u1: f32,
) -> Brush {
    make_wall_brush(
        id, axis, lo, hi - lo, u_axis, u0, u1 - u0, Axis::Y, y_min, y_max - y_min,
    )
}
