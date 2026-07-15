//! Pure-Rust BSP CSG core, vendored verbatim from the GoldenEye spike
//! (`spike/csg-wasm-bench/csg-wasm/src/csg.rs`). The original crate's wasm
//! wrapper (`lib.rs`: JSON parse + js_sys array wrapping) is intentionally
//! NOT ported — this engine consumes `csg.rs` directly.
//!
//! Public surface: [`Plane`], [`Polygon`], [`Node`], [`csg_union`],
//! [`csg_subtract`], [`polygons_to_mesh`]. Works in `[f32; 3]`; mesh output is
//! `(positions, normals, indices)` ready for a wgpu vertex buffer or a Rapier
//! trimesh collider.

mod csg;

pub use csg::{csg_subtract, csg_union, polygons_to_mesh, Node, Plane, Polygon};

/// Build the 6 faces of an axis-aligned box as CSG polygons, centered at
/// `center` with half-extents `half`. Winding is CCW-outward so normals face
/// out (matching the JS editor's brush convention).
pub fn box_polygons(center: [f32; 3], half: [f32; 3]) -> Vec<Polygon> {
    let [cx, cy, cz] = center;
    let [hx, hy, hz] = half;
    // 8 corners.
    let c = |sx: f32, sy: f32, sz: f32| [cx + sx * hx, cy + sy * hy, cz + sz * hz];
    // Each face as 4 CCW verts seen from outside.
    let faces = [
        // +X
        [c(1.0, -1.0, -1.0), c(1.0, -1.0, 1.0), c(1.0, 1.0, 1.0), c(1.0, 1.0, -1.0)],
        // -X
        [c(-1.0, -1.0, 1.0), c(-1.0, -1.0, -1.0), c(-1.0, 1.0, -1.0), c(-1.0, 1.0, 1.0)],
        // +Y
        [c(-1.0, 1.0, -1.0), c(1.0, 1.0, -1.0), c(1.0, 1.0, 1.0), c(-1.0, 1.0, 1.0)],
        // -Y
        [c(-1.0, -1.0, 1.0), c(1.0, -1.0, 1.0), c(1.0, -1.0, -1.0), c(-1.0, -1.0, -1.0)],
        // +Z
        [c(-1.0, -1.0, 1.0), c(-1.0, 1.0, 1.0), c(1.0, 1.0, 1.0), c(1.0, -1.0, 1.0)],
        // -Z
        [c(1.0, -1.0, -1.0), c(1.0, 1.0, -1.0), c(-1.0, 1.0, -1.0), c(-1.0, -1.0, -1.0)],
    ];
    faces
        .into_iter()
        .filter_map(|verts| {
            // Reverse to CCW-from-outside so Plane::from_points (right-hand
            // rule on the first 3 verts) yields an outward normal. CSG
            // subtract/union depend on correct facing; inward normals produce
            // empty or inverted results.
            let mut v = verts.to_vec();
            v.reverse();
            Polygon::new(v)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_has_six_quads_twelve_tris() {
        let polys = box_polygons([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        assert_eq!(polys.len(), 6, "a box should yield 6 faces");
        let (pos, norm, idx) = polygons_to_mesh(&polys);
        assert_eq!(pos.len(), norm.len());
        // 6 quads * 2 tris * 3 indices = 36 indices.
        assert_eq!(idx.len(), 36);
    }

    #[test]
    fn subtract_carves_a_room() {
        // A solid 16-wide block with a 12-wide cavity subtracted = a room shell.
        // This is the editor's opening move (main.js: first 'subtract' brush).
        let solid = box_polygons([0.0, 0.0, 0.0], [8.0, 8.0, 8.0]);
        let cavity = box_polygons([0.0, 0.0, 0.0], [6.0, 6.0, 6.0]);
        let result = csg_subtract(solid, cavity);
        assert!(!result.is_empty(), "subtract should leave the outer shell");
        let (pos, _n, idx) = polygons_to_mesh(&result);
        assert!(pos.len() >= 3 * 3);
        assert!(idx.len() % 3 == 0 && !idx.is_empty());
    }

    #[test]
    fn union_of_two_boxes_is_nonempty() {
        let a = box_polygons([0.0, 0.0, 0.0], [4.0, 4.0, 4.0]);
        let b = box_polygons([4.0, 0.0, 0.0], [4.0, 4.0, 4.0]);
        let result = csg_union(a, b);
        let (_p, _n, idx) = polygons_to_mesh(&result);
        assert!(!idx.is_empty());
    }
}
