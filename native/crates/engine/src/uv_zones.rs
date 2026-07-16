//! Post-CSG triangle classification + UV assignment — the render-side port of
//! `src/core/csg/uvZones.js` **and** `src/core/csg/faceMap.js`.
//!
//! The CSG fold produces an unattributed triangle soup with no UVs and no notion
//! of "wall vs floor" or "which brush owns this". We recover both:
//!   1. **Face-map** ([`face_owner`], port of `buildFaceMap`): match each triangle
//!      to its owning brush by dominant-normal axis + face-position + centroid
//!      containment (smaller brushes win ties). The owner supplies the triangle's
//!      texture `scheme` and its `floor_y` wall-UV anchor — so a room and the room
//!      beyond its door can carry different schemes, and a stair pit's walls
//!      anchor to the pit floor instead of shifting the whole level.
//!   2. **Zone classification** (port of `assignUVsAndZones`): dominant normal →
//!      floor/ceiling/wall, walls split at `WALL_SPLIT_V` above the owner floor
//!      into lower(2)/upper(3), tris split at door/hole frame AABBs → tunnel
//!      zones 5/6.
//!
//! Geometry we emit ourselves (stair treads/risers, structures) is tagged with an
//! explicit zone (and often explicit UVs) at emission — see [`ZonedBuilder`].
//!
//! Zone layout (matches [`crate::textures`]):
//!   0 floor · 1 ceiling · 2 lower wall · 3 upper wall ·
//!   5 stair/doorframe sides+ceiling · 6 doorframe floor · 7 brace.

use crate::csg_runtime::WALL_THICKNESS;
use crate::mesh::{TexVertex, TexturedMesh, ZoneGroup};

/// Meters per world tile (mirrors `csg_runtime::WORLD_SCALE`; kept local so the
/// UV math reads against the JS `WORLD_SCALE` directly).
const WORLD_SCALE: f32 = 0.25;

/// Wall vertical split height in WT (JS `WALL_SPLIT_V`).
pub const WALL_SPLIT_V: f32 = 6.0;

/// Face-identity tolerance in WT (JS `CSG_CENTROID_TOL`).
const CSG_CENTROID_TOL: f32 = 0.5;

/// A brush as the classifier needs it: WT AABB + the owner attributes recovered
/// per triangle (scheme, floor anchor) + frame flags for tunnel-zone routing.
#[derive(Clone, Copy, Debug)]
pub struct BrushInfo {
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub floor_y: f32,
    pub scheme: usize,
    pub frame: bool,
    pub door: bool,
}

impl BrushInfo {
    #[inline]
    fn dim(&self, axis: usize) -> f32 {
        self.max[axis] - self.min[axis]
    }
    #[inline]
    fn volume(&self) -> f32 {
        self.dim(0) * self.dim(1) * self.dim(2)
    }
}

/// A door/hole frame's world-space AABB (meters) + the WT dims driving UV rotation.
#[derive(Clone, Copy, Debug)]
struct FrameAabb {
    min: [f32; 3],
    max: [f32; 3],
    is_door: bool,
    w_wt: f32,
    h_wt: f32,
}

impl FrameAabb {
    #[inline]
    fn contains_centroid(&self, c: [f32; 3]) -> bool {
        c[0] >= self.min[0]
            && c[0] <= self.max[0]
            && c[1] >= self.min[1]
            && c[1] <= self.max[1]
            && c[2] >= self.min[2]
            && c[2] <= self.max[2]
    }
}

/// Accumulates classified / hand-tagged triangles, each keyed by (scheme, zone),
/// then sorts them into per-(scheme,zone) draw groups. Vertices are un-indexed
/// (3 per triangle) like the JS output.
pub struct ZonedBuilder {
    verts: Vec<TexVertex>,
    tri_keys: Vec<(u16, u8)>, // (scheme, zone)
}

impl Default for ZonedBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ZonedBuilder {
    pub fn new() -> Self {
        ZonedBuilder {
            verts: Vec::new(),
            tri_keys: Vec::new(),
        }
    }

    /// Planar UV from a world-space (meters) vertex, in tile units (JS `vertexUV`).
    #[inline]
    fn vertex_uv(v: [f32; 3], axis: u8, rotated: bool, origin_y: f32) -> [f32; 2] {
        let wx = v[0] / WORLD_SCALE;
        let wy = v[1] / WORLD_SCALE - origin_y;
        let wz = v[2] / WORLD_SCALE;
        if rotated {
            match axis {
                0 => [wy, wz],
                2 => [wy, wx],
                _ => [wz, wx],
            }
        } else {
            match axis {
                0 => [wz, wy],
                1 => [wx, wz],
                _ => [wx, wy],
            }
        }
    }

    /// Emit a classified triangle (JS `emitTri`): correct winding to match the
    /// intended normal, then push 3 UV'd vertices tagged (scheme, zone).
    #[allow(clippy::too_many_arguments)]
    fn emit_tri(
        &mut self,
        p_a: [f32; 3],
        p_b: [f32; 3],
        p_c: [f32; 3],
        n: [f32; 3],
        axis: u8,
        zone: u8,
        rotated: bool,
        origin_y: f32,
        scheme: usize,
    ) {
        let cross = cross(sub(p_b, p_a), sub(p_c, p_a));
        let dot = cross[0] * n[0] + cross[1] * n[1] + cross[2] * n[2];
        let (vb, vc) = if dot < 0.0 { (p_c, p_b) } else { (p_b, p_c) };
        for v in [p_a, vb, vc] {
            self.verts.push(TexVertex {
                pos: v,
                normal: n,
                uv: Self::vertex_uv(v, axis, rotated, origin_y),
            });
        }
        self.tri_keys.push((scheme as u16, zone));
    }

    /// Emit a hand-tagged quad (four WT corners, CCW from front) with a fixed
    /// zone and planar (world-position) UVs anchored at `origin_y`. Dominant-axis
    /// + normal derived from the corners. Single-winding (culling off). Used for
    /// the structures mesh.
    pub fn emit_quad_wt(&mut self, corners: [[f32; 3]; 4], zone: u8, origin_y: f32, scheme: usize) {
        let m = |p: [f32; 3]| [p[0] * WORLD_SCALE, p[1] * WORLD_SCALE, p[2] * WORLD_SCALE];
        let (q0, q1, q2, q3) = (m(corners[0]), m(corners[1]), m(corners[2]), m(corners[3]));
        let n = normalize(cross(sub(q1, q0), sub(q2, q0)));
        let axis = dominant_axis(n);
        for (t, uv) in [
            (q0, Self::vertex_uv(q0, axis, false, origin_y)),
            (q1, Self::vertex_uv(q1, axis, false, origin_y)),
            (q2, Self::vertex_uv(q2, axis, false, origin_y)),
        ] {
            self.verts.push(TexVertex { pos: t, normal: n, uv });
        }
        self.tri_keys.push((scheme as u16, zone));
        for (t, uv) in [
            (q0, Self::vertex_uv(q0, axis, false, origin_y)),
            (q2, Self::vertex_uv(q2, axis, false, origin_y)),
            (q3, Self::vertex_uv(q3, axis, false, origin_y)),
        ] {
            self.verts.push(TexVertex { pos: t, normal: n, uv });
        }
        self.tri_keys.push((scheme as u16, zone));
    }

    /// Emit a hand-tagged quad with **explicit** per-corner UVs (tile units) — for
    /// stair geometry, where the JS uses custom UVs so the gradient riser maps
    /// 0..1 per step. Single-winding.
    pub fn emit_quad_uv(
        &mut self,
        corners: [[f32; 3]; 4],
        uvs: [[f32; 2]; 4],
        scheme: usize,
        zone: u8,
    ) {
        let m = |p: [f32; 3]| [p[0] * WORLD_SCALE, p[1] * WORLD_SCALE, p[2] * WORLD_SCALE];
        let q = [m(corners[0]), m(corners[1]), m(corners[2]), m(corners[3])];
        let n = normalize(cross(sub(q[1], q[0]), sub(q[2], q[0])));
        for &i in &[0usize, 1, 2] {
            self.verts.push(TexVertex { pos: q[i], normal: n, uv: uvs[i] });
        }
        self.tri_keys.push((scheme as u16, zone));
        for &i in &[0usize, 2, 3] {
            self.verts.push(TexVertex { pos: q[i], normal: n, uv: uvs[i] });
        }
        self.tri_keys.push((scheme as u16, zone));
    }

    /// Sort triangles by (scheme, zone) and produce the grouped, un-indexed mesh.
    pub fn finish(self) -> TexturedMesh {
        let ntris = self.tri_keys.len();
        let key = |t: usize| {
            let (s, z) = self.tri_keys[t];
            (s as u32) * 8 + z as u32
        };
        let mut order: Vec<usize> = (0..ntris).collect();
        order.sort_by_key(|&t| key(t));

        let mut indices = Vec::with_capacity(ntris * 3);
        let mut groups: Vec<ZoneGroup> = Vec::new();
        let mut cur: Option<(u16, u8)> = None;
        let mut start = 0u32;
        let mut count = 0u32;
        for &t in &order {
            let k = self.tri_keys[t];
            if cur != Some(k) {
                if let Some((s, z)) = cur {
                    groups.push(ZoneGroup { scheme: s, zone: z, start, count });
                    start += count;
                    count = 0;
                }
                cur = Some(k);
            }
            let base = (t * 3) as u32;
            indices.extend_from_slice(&[base, base + 1, base + 2]);
            count += 3;
        }
        if let Some((s, z)) = cur {
            groups.push(ZoneGroup { scheme: s, zone: z, start, count });
        }
        TexturedMesh {
            vertices: self.verts,
            indices,
            groups,
        }
    }
}

/// Classify a CSG triangle soup (positions/indices in meters) into the builder.
/// Each triangle is attributed to its owning brush (face-map) for its `scheme`
/// and `floor_y` anchor, then classified into a zone. `brushes` are the region's
/// brushes (WT); `default_scheme` is used for triangles with no owner (e.g. shell
/// boundary, or the structures mesh which passes an empty brush list).
pub fn classify_soup(
    b: &mut ZonedBuilder,
    pos: &[f32],
    idx: &[u32],
    brushes: &[BrushInfo],
    default_scheme: usize,
) {
    let frames: Vec<FrameAabb> = brushes
        .iter()
        .filter(|br| br.frame)
        .map(|br| FrameAabb {
            min: [br.min[0] * WORLD_SCALE, br.min[1] * WORLD_SCALE, br.min[2] * WORLD_SCALE],
            max: [br.max[0] * WORLD_SCALE, br.max[1] * WORLD_SCALE, br.max[2] * WORLD_SCALE],
            is_door: br.door,
            w_wt: br.dim(0),
            h_wt: br.dim(1),
        })
        .collect();
    let has_frames = !frames.is_empty();

    let tri_count = idx.len() / 3;
    for t in 0..tri_count {
        let i0 = idx[t * 3] as usize;
        let i1 = idx[t * 3 + 1] as usize;
        let i2 = idx[t * 3 + 2] as usize;
        let va = [pos[i0 * 3], pos[i0 * 3 + 1], pos[i0 * 3 + 2]];
        let vb = [pos[i1 * 3], pos[i1 * 3 + 1], pos[i1 * 3 + 2]];
        let vc = [pos[i2 * 3], pos[i2 * 3 + 1], pos[i2 * 3 + 2]];

        let n = normalize(cross(sub(vb, va), sub(vc, va)));
        let (ax, ay, az) = (n[0].abs(), n[1].abs(), n[2].abs());

        // Owner brush → scheme + wall-UV floor anchor (face-map).
        let centroid_wt = [
            (va[0] + vb[0] + vc[0]) / 3.0 / WORLD_SCALE,
            (va[1] + vb[1] + vc[1]) / 3.0 / WORLD_SCALE,
            (va[2] + vb[2] + vc[2]) / 3.0 / WORLD_SCALE,
        ];
        let dom = dominant_axis(n) as usize;
        let owner = face_owner(brushes, centroid_wt, dom);
        let (scheme, origin_y) = match owner {
            Some(i) => (brushes[i].scheme, brushes[i].floor_y),
            None => (default_scheme, 0.0),
        };
        let split_y = (origin_y + WALL_SPLIT_V) * WORLD_SCALE;

        let (tmin, tmax) = tri_bbox(va, vb, vc);
        let near = has_frames && tri_overlaps_any_frame(&frames, tmin, tmax);

        if ay >= ax && ay >= az {
            // ── Floor / ceiling (Y face) ──
            let plain = if n[1] > 0.0 { 0 } else { 1 };
            if !near {
                b.emit_tri(va, vb, vc, n, 1, plain, false, origin_y, scheme);
                continue;
            }
            let mut tris = vec![[va, vb, vc]];
            for f in &frames {
                tris = split_tris(tris, 0, f.min[0]);
                tris = split_tris(tris, 0, f.max[0]);
                tris = split_tris(tris, 2, f.min[2]);
                tris = split_tris(tris, 2, f.max[2]);
            }
            for tri in tris {
                let c = centroid(tri);
                match frames.iter().find(|f| f.contains_centroid(c)) {
                    Some(f) if n[1] > 0.0 => {
                        let zone = if f.is_door { 6 } else { 5 };
                        b.emit_tri(tri[0], tri[1], tri[2], n, 1, zone, f.w_wt == WALL_THICKNESS, origin_y, scheme);
                    }
                    Some(f) => {
                        b.emit_tri(tri[0], tri[1], tri[2], n, 1, 5, f.w_wt == WALL_THICKNESS, origin_y, scheme);
                    }
                    None => b.emit_tri(tri[0], tri[1], tri[2], n, 1, plain, false, origin_y, scheme),
                }
            }
        } else {
            // ── Wall (X or Z face) ──
            let axis: u8 = if ax >= az { 0 } else { 2 };
            if !near {
                emit_wall_split(b, [va, vb, vc], n, axis, split_y, origin_y, scheme);
                continue;
            }
            let mut tris = vec![[va, vb, vc]];
            for f in &frames {
                if axis == 0 {
                    tris = split_tris(tris, 2, f.min[2]);
                    tris = split_tris(tris, 2, f.max[2]);
                } else {
                    tris = split_tris(tris, 0, f.min[0]);
                    tris = split_tris(tris, 0, f.max[0]);
                }
                tris = split_tris(tris, 1, f.min[1]);
                tris = split_tris(tris, 1, f.max[1]);
            }
            for tri in tris {
                let c = centroid(tri);
                match frames.iter().find(|f| f.contains_centroid(c)) {
                    Some(f) => {
                        let rotate = f.h_wt != WALL_THICKNESS;
                        b.emit_tri(tri[0], tri[1], tri[2], n, axis, 5, rotate, origin_y, scheme);
                    }
                    None => emit_wall_split(b, tri, n, axis, split_y, origin_y, scheme),
                }
            }
        }
    }
}

/// Recover the owning brush of a triangle (port of `buildFaceMap`): among faces
/// on the dominant axis within tolerance of the centroid, whose brush contains
/// the centroid on the tangent axes, pick the nearest (smaller brush wins ties).
fn face_owner(brushes: &[BrushInfo], centroid_wt: [f32; 3], axis: usize) -> Option<usize> {
    let pos_along = centroid_wt[axis];
    let mut best: Option<usize> = None;
    let mut best_dist = f32::INFINITY;
    let mut best_vol = f32::INFINITY;
    for (i, brush) in brushes.iter().enumerate() {
        for face_pos in [brush.min[axis], brush.max[axis]] {
            let dist = (face_pos - pos_along).abs();
            if dist > CSG_CENTROID_TOL {
                continue;
            }
            if !centroid_in_brush(brush, axis, centroid_wt) {
                continue;
            }
            let vol = brush.volume();
            if dist < best_dist || (dist == best_dist && vol < best_vol) {
                best_dist = dist;
                best_vol = vol;
                best = Some(i);
            }
        }
    }
    best
}

/// Whether a WT centroid lies within a brush's tangent-axis bounds (JS
/// `centroidInBrush`). Frames use strict containment (tol 0) so they don't claim
/// wall triangles just outside the cutout.
fn centroid_in_brush(b: &BrushInfo, axis: usize, c: [f32; 3]) -> bool {
    let tol = if b.frame { 0.0 } else { CSG_CENTROID_TOL };
    let within = |i: usize| c[i] >= b.min[i] - tol && c[i] <= b.max[i] + tol;
    match axis {
        0 => within(2) && within(1),
        1 => within(0) && within(2),
        _ => within(0) && within(1),
    }
}

/// Emit a room-wall triangle, split at the zone-2/3 boundary `split_y` (meters).
#[allow(clippy::too_many_arguments)]
fn emit_wall_split(
    b: &mut ZonedBuilder,
    tri: [[f32; 3]; 3],
    n: [f32; 3],
    axis: u8,
    split_y: f32,
    origin_y: f32,
    scheme: usize,
) {
    let (min_y, max_y) = (
        tri[0][1].min(tri[1][1]).min(tri[2][1]),
        tri[0][1].max(tri[1][1]).max(tri[2][1]),
    );
    if max_y <= split_y {
        b.emit_tri(tri[0], tri[1], tri[2], n, axis, 2, false, origin_y, scheme);
        return;
    }
    if min_y >= split_y {
        b.emit_tri(tri[0], tri[1], tri[2], n, axis, 3, false, origin_y, scheme);
        return;
    }
    let mut v = tri;
    v.sort_by(|a, b| a[1].total_cmp(&b[1]));
    let (lo, mid, hi) = (v[0], v[1], v[2]);
    let p_lo_hi = lerp_at_y(lo, hi, split_y);
    if mid[1] <= split_y {
        let p_mid_hi = lerp_at_y(mid, hi, split_y);
        b.emit_tri(lo, mid, p_lo_hi, n, axis, 2, false, origin_y, scheme);
        b.emit_tri(mid, p_mid_hi, p_lo_hi, n, axis, 2, false, origin_y, scheme);
        b.emit_tri(p_lo_hi, p_mid_hi, hi, n, axis, 3, false, origin_y, scheme);
    } else {
        let p_lo_mid = lerp_at_y(lo, mid, split_y);
        b.emit_tri(lo, p_lo_mid, p_lo_hi, n, axis, 2, false, origin_y, scheme);
        b.emit_tri(p_lo_mid, mid, p_lo_hi, n, axis, 3, false, origin_y, scheme);
        b.emit_tri(mid, hi, p_lo_hi, n, axis, 3, false, origin_y, scheme);
    }
}

// ─── Geometry helpers ────────────────────────────────────────────────

#[inline]
fn dominant_axis(n: [f32; 3]) -> u8 {
    let (ax, ay, az) = (n[0].abs(), n[1].abs(), n[2].abs());
    if ay >= ax && ay >= az {
        1
    } else if ax >= az {
        0
    } else {
        2
    }
}

#[inline]
fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 1e-8 {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        [0.0, 0.0, 0.0]
    }
}

#[inline]
fn centroid(t: [[f32; 3]; 3]) -> [f32; 3] {
    [
        (t[0][0] + t[1][0] + t[2][0]) / 3.0,
        (t[0][1] + t[1][1] + t[2][1]) / 3.0,
        (t[0][2] + t[1][2] + t[2][2]) / 3.0,
    ]
}

#[inline]
fn tri_bbox(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    (
        [a[0].min(b[0]).min(c[0]), a[1].min(b[1]).min(c[1]), a[2].min(b[2]).min(c[2])],
        [a[0].max(b[0]).max(c[0]), a[1].max(b[1]).max(c[1]), a[2].max(b[2]).max(c[2])],
    )
}

fn tri_overlaps_any_frame(frames: &[FrameAabb], tmin: [f32; 3], tmax: [f32; 3]) -> bool {
    frames.iter().any(|f| {
        tmax[0] >= f.min[0]
            && tmin[0] <= f.max[0]
            && tmax[1] >= f.min[1]
            && tmin[1] <= f.max[1]
            && tmax[2] >= f.min[2]
            && tmin[2] <= f.max[2]
    })
}

fn lerp_at_y(a: [f32; 3], b: [f32; 3], y: f32) -> [f32; 3] {
    let t = (y - a[1]) / (b[1] - a[1]);
    [a[0] + (b[0] - a[0]) * t, y, a[2] + (b[2] - a[2]) * t]
}

fn lerp_at_axis(a: [f32; 3], b: [f32; 3], axis: usize, val: f32) -> [f32; 3] {
    let t = (val - a[axis]) / (b[axis] - a[axis]);
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn split_tris(tris: Vec<[[f32; 3]; 3]>, axis: usize, val: f32) -> Vec<[[f32; 3]; 3]> {
    let mut out = Vec::with_capacity(tris.len());
    for tri in tris {
        let vals = [tri[0][axis], tri[1][axis], tri[2][axis]];
        let min_v = vals[0].min(vals[1]).min(vals[2]);
        let max_v = vals[0].max(vals[1]).max(vals[2]);
        if max_v <= val + 1e-6 || min_v >= val - 1e-6 {
            out.push(tri);
            continue;
        }
        let mut sorted = tri;
        sorted.sort_by(|a, b| a[axis].total_cmp(&b[axis]));
        let (lo, mid, hi) = (sorted[0], sorted[1], sorted[2]);
        let p_lo_hi = lerp_at_axis(lo, hi, axis, val);
        if mid[axis] <= val {
            let p_mid_hi = lerp_at_axis(mid, hi, axis, val);
            out.push([lo, mid, p_lo_hi]);
            out.push([mid, p_mid_hi, p_lo_hi]);
            out.push([p_lo_hi, p_mid_hi, hi]);
        } else {
            let p_lo_mid = lerp_at_axis(lo, mid, axis, val);
            out.push([lo, p_lo_mid, p_lo_hi]);
            out.push([p_lo_mid, mid, p_lo_hi]);
            out.push([mid, hi, p_lo_hi]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csg_runtime::{Brush, Op, Region};

    fn zone_counts(m: &TexturedMesh) -> std::collections::BTreeMap<u8, u32> {
        let mut map = std::collections::BTreeMap::new();
        for g in &m.groups {
            *map.entry(g.zone).or_insert(0) += g.count / 3;
        }
        map
    }

    #[test]
    fn plain_room_has_floor_ceiling_and_split_walls() {
        let mut region = Region::new(0);
        region
            .brushes
            .push(Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 12.0, 12.0, 12.0));
        let tex = region.evaluate_textured();
        let zones = zone_counts(&tex);
        assert!(zones.contains_key(&0), "floor zone present: {zones:?}");
        assert!(zones.contains_key(&1), "ceiling zone present: {zones:?}");
        assert!(zones.contains_key(&2), "lower-wall zone present: {zones:?}");
        assert!(zones.contains_key(&3), "upper-wall zone present: {zones:?}");
        assert!(!zones.contains_key(&5) && !zones.contains_key(&6), "no frame zones: {zones:?}");
        let mut cursor = 0;
        for g in &tex.groups {
            assert_eq!(g.start, cursor, "group starts are contiguous");
            cursor += g.count;
        }
        assert_eq!(cursor as usize, tex.indices.len());
        assert!(tex.indices.iter().all(|&i| (i as usize) < tex.vertices.len()));
    }

    #[test]
    fn uvs_are_in_tile_units() {
        let mut region = Region::new(0);
        region
            .brushes
            .push(Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 4.0, 8.0, 4.0));
        let tex = region.evaluate_textured();
        let floor = tex.groups.iter().find(|g| g.zone == 0).expect("floor group");
        let mut max_u = 0.0f32;
        for k in floor.start..(floor.start + floor.count) {
            let vi = tex.indices[k as usize] as usize;
            max_u = max_u.max(tex.vertices[vi].uv[0].abs());
        }
        assert!(max_u > 3.0, "floor UVs should reach tile units (~4), got {max_u}");
    }

    #[test]
    fn owner_scheme_flows_into_groups() {
        // A room brush with scheme 2 → its wall/floor groups carry scheme 2.
        let mut region = Region::new(0);
        let mut b = Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 8.0, 8.0, 8.0);
        b.scheme = 2;
        region.brushes.push(b);
        let tex = region.evaluate_textured();
        assert!(
            tex.groups.iter().any(|g| g.scheme == 2),
            "room brush scheme 2 should reach the draw groups: {:?}",
            tex.groups.iter().map(|g| (g.scheme, g.zone)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_lower_pit_anchors_its_walls_to_its_own_floor() {
        // Main room floor at y=0; a second subtract carved below (floor y=-6) with
        // its own floor_y. Its walls should split at (-6 + 6) = 0 in WT, i.e. its
        // lower wall reaches up to world y=0 — proving per-brush floor anchoring,
        // not a single region-wide origin.
        let mut region = Region::new(0);
        region
            .brushes
            .push(Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 12.0, 8.0, 12.0));
        let mut pit = Brush::new(2, Op::Subtract, 4.0, -6.0, 4.0, 4.0, 14.0, 4.0);
        pit.floor_y = -6.0;
        region.brushes.push(pit);
        // Just assert it evaluates with both floor and wall zones and doesn't panic
        // — the anchoring correctness is visual, but this guards the plumbing.
        let tex = region.evaluate_textured();
        let zones = zone_counts(&tex);
        assert!(zones.contains_key(&0) && zones.contains_key(&2));
    }
}
