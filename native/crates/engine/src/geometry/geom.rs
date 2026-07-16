//! Shared geometry primitives — pure math with no domain knowledge.
//!
//! These were previously duplicated across `csg_runtime` (stair geometry),
//! `structures` (platform/stair-run meshes), `nav` (grid solidity), and `world`
//! (picking / gizmo). Pulling them here keeps one authoritative copy each and
//! lets every module share them without a dependency cycle (this module imports
//! nothing from its callers — only `glam` and plain arrays).
//!
//! Coordinate note: the quad emitters take positions in **world tiles (WT)** and
//! a `ws` scale (meters-per-WT, [`crate::geometry::csg_runtime::WORLD_SCALE`]); everything
//! else is unit-agnostic (works in WT or meters as long as inputs agree).

use glam::Vec3;

/// Unit normal of the triangle `a→b→c` (right-hand rule); zero if degenerate.
/// (Was `quad_normal` in `csg_runtime` and `tri_normal` in `structures` —
/// byte-identical bodies.)
pub fn tri_normal(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> [f32; 3] {
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len > 1e-8 {
        [n[0] / len, n[1] / len, n[2] / len]
    } else {
        [0.0, 0.0, 0.0]
    }
}

/// Emit a **single-winding** quad (two triangles, CCW `p0→p1→p2→p3`) with one
/// face normal. WT positions scaled to meters by `ws`. Used by the structures
/// render meshes, which draw with culling off so one winding is visible from
/// both sides — emitting the back winding too would z-fight duplicate tris.
pub fn push_quad(
    pos: &mut Vec<f32>,
    norm: &mut Vec<f32>,
    idx: &mut Vec<u32>,
    p0: [f32; 3],
    p1: [f32; 3],
    p2: [f32; 3],
    p3: [f32; 3],
    ws: f32,
) {
    let s = |p: [f32; 3]| [p[0] * ws, p[1] * ws, p[2] * ws];
    let (q0, q1, q2, q3) = (s(p0), s(p1), s(p2), s(p3));
    let n = tri_normal(q0, q1, q2);

    let base = (pos.len() / 3) as u32;
    for p in [q0, q1, q2, q3] {
        pos.extend_from_slice(&p);
        norm.extend_from_slice(&n);
    }
    idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// Emit a **double-sided** quad: front (CCW `p0→p1→p2→p3`, normal +n) and back
/// (reversed winding, normal −n). WT positions scaled to meters by `ws`. Used by
/// the CSG stair collider geometry, where being visible from both sides matters
/// and the reversed tris are harmless in a trimesh collider.
pub fn push_quad_double(
    pos: &mut Vec<f32>,
    norm: &mut Vec<f32>,
    idx: &mut Vec<u32>,
    p0: [f32; 3],
    p1: [f32; 3],
    p2: [f32; 3],
    p3: [f32; 3],
    ws: f32,
) {
    let s = |p: [f32; 3]| [p[0] * ws, p[1] * ws, p[2] * ws];
    let (q0, q1, q2, q3) = (s(p0), s(p1), s(p2), s(p3));
    let n = tri_normal(q0, q1, q2);
    let nb = [-n[0], -n[1], -n[2]];

    // Front side (CCW p0→p1→p2→p3), normal +n.
    let base = (pos.len() / 3) as u32;
    for (p, nn) in [(q0, n), (q1, n), (q2, n), (q3, n)] {
        pos.extend_from_slice(&p);
        norm.extend_from_slice(&nn);
    }
    idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);

    // Back side (reversed winding), normal -n.
    let base = (pos.len() / 3) as u32;
    for (p, nn) in [(q0, nb), (q1, nb), (q2, nb), (q3, nb)] {
        pos.extend_from_slice(&p);
        norm.extend_from_slice(&nn);
    }
    idx.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
}

/// Whether a point is inside a `[x, y, z, w, h, d]` AABB — **half-open**
/// (`>= min`, `< max`), matching the JS `pointInBrush` probe. Backs both
/// [`crate::geometry::csg_runtime::Brush::contains`] and `nav`'s extra-solid test.
#[inline]
pub fn point_in_box(b: &[f32; 6], x: f32, y: f32, z: f32) -> bool {
    x >= b[0]
        && x < b[0] + b[3]
        && y >= b[1]
        && y < b[1] + b[4]
        && z >= b[2]
        && z < b[2] + b[5]
}

/// Whether a point lies within a `[x, y, z, w, h, d]` AABB with tolerance `eps`
/// — **closed** bounds grown by `eps` on all sides. Used to classify which
/// platform / stair-run the crosshair hit.
#[inline]
pub fn point_in_box_eps(b: &[f32; 6], p: Vec3, eps: f32) -> bool {
    p.x >= b[0] - eps
        && p.x <= b[0] + b[3] + eps
        && p.y >= b[1] - eps
        && p.y <= b[1] + b[4] + eps
        && p.z >= b[2] - eps
        && p.z <= b[2] + b[5] + eps
}

/// Ray vs AABB (slab method), meters. Returns the near hit distance (≥0) or
/// `None`. `dir` need not be normalized; near-zero components are nudged so the
/// reciprocal stays finite.
pub fn ray_aabb(origin: Vec3, dir: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let safe = |d: f32| if d.abs() < 1e-6 { 1e-6 } else { d };
    let inv = Vec3::new(1.0 / safe(dir.x), 1.0 / safe(dir.y), 1.0 / safe(dir.z));
    let t0 = (min - origin) * inv;
    let t1 = (max - origin) * inv;
    let tmin = t0.min(t1).max_element();
    let tmax = t0.max(t1).min_element();
    if tmax >= tmin.max(0.0) {
        Some(tmin.max(0.0))
    } else {
        None
    }
}
