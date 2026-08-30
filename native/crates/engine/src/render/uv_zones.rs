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
//! Zone layout (matches [`crate::render::textures`]):
//!   0 floor · 1 ceiling · 2 lower wall · 3 upper wall ·
//!   5 stair/doorframe sides+ceiling · 6 doorframe floor · 7 brace.

use crate::geometry::csg_runtime::{face_slot, Axis, FaceTex, Side, WALL_THICKNESS};
use crate::render::mesh::{TexVertex, TexturedMesh, ZoneGroup};

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
    /// The authored brush's stable id — how a face override and the paint tool
    /// name a face, and what the surface probe reports.
    pub id: u32,
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub floor_y: f32,
    /// WT-space **horizontal** UV anchor, the XZ counterpart of [`Self::floor_y`].
    ///
    /// `floor_y` exists because a wall's texture has to start at *its own* floor rather
    /// than at world zero, or a stair pit shifts the whole level's wall texture. Floors
    /// and ceilings had no equivalent — their UVs are raw world `[wx, wz]` — which is
    /// invisible for a room (a room's floor is a big field of tile, and where the grid
    /// starts does not read) and glaring for a **vent duct**, whose texture is a single
    /// bordered panel sized to exactly one face. Placed anywhere that is not a multiple
    /// of the panel size, the seam lands mid-face and the border runs across the middle
    /// of the duct floor.
    ///
    /// So this is the same idea on the other two axes: anchor the panel grid to the
    /// brush that owns the surface. `[0.0, 0.0]` keeps the old world-space behaviour and
    /// is what every brush but a duct uses.
    pub origin_xz: [f32; 2],
    pub scheme: usize,
    pub frame: bool,
    pub door: bool,
    /// Per-face texture overrides, indexed by
    /// [`face_slot`](crate::geometry::csg_runtime::face_slot). An override on the
    /// face a triangle was attributed to replaces the owner's scheme (and, when it
    /// forces one, the derived zone). See [`FaceTex`].
    pub face_tex: [Option<FaceTex>; 6],
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
    fn vertex_uv(v: [f32; 3], axis: u8, rotated: bool, origin: [f32; 3]) -> [f32; 2] {
        let wx = v[0] / WORLD_SCALE - origin[0];
        let wy = v[1] / WORLD_SCALE - origin[1];
        let wz = v[2] / WORLD_SCALE - origin[2];
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
        origin: [f32; 3],
        scheme: usize,
    ) {
        let cross = cross(sub(p_b, p_a), sub(p_c, p_a));
        let dot = cross[0] * n[0] + cross[1] * n[1] + cross[2] * n[2];
        let (vb, vc) = if dot < 0.0 { (p_c, p_b) } else { (p_b, p_c) };
        for v in [p_a, vb, vc] {
            self.verts.push(TexVertex::new(
                v,
                n,
                Self::vertex_uv(v, axis, rotated, origin),
            ));
        }
        self.tri_keys.push((scheme as u16, zone));
    }

    /// Emit a hand-tagged quad (four WT corners, CCW from front) with a fixed
    /// zone and planar (world-position) UVs anchored at `origin`. Dominant-axis
    /// + normal derived from the corners. Single-winding (culling off). Used for
    /// the structures mesh.
    pub fn emit_quad_wt(&mut self, corners: [[f32; 3]; 4], zone: u8, origin: [f32; 3], scheme: usize) {
        let m = |p: [f32; 3]| [p[0] * WORLD_SCALE, p[1] * WORLD_SCALE, p[2] * WORLD_SCALE];
        let (q0, q1, q2, q3) = (m(corners[0]), m(corners[1]), m(corners[2]), m(corners[3]));
        let n = normalize(cross(sub(q1, q0), sub(q2, q0)));
        let axis = dominant_axis(n);
        for (t, uv) in [
            (q0, Self::vertex_uv(q0, axis, false, origin)),
            (q1, Self::vertex_uv(q1, axis, false, origin)),
            (q2, Self::vertex_uv(q2, axis, false, origin)),
        ] {
            self.verts.push(TexVertex::new(t, n, uv));
        }
        self.tri_keys.push((scheme as u16, zone));
        for (t, uv) in [
            (q0, Self::vertex_uv(q0, axis, false, origin)),
            (q2, Self::vertex_uv(q2, axis, false, origin)),
            (q3, Self::vertex_uv(q3, axis, false, origin)),
        ] {
            self.verts.push(TexVertex::new(t, n, uv));
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
            self.verts.push(TexVertex::new(q[i], n, uvs[i]));
        }
        self.tri_keys.push((scheme as u16, zone));
        for &i in &[0usize, 2, 3] {
            self.verts.push(TexVertex::new(q[i], n, uvs[i]));
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
/// Faces bucketed by axis so the classifier can find the handful of brush faces
/// lying on a triangle's plane without scanning every brush.
///
/// The scan it replaces was the single hottest thing in a bake: `O(brushes)` per
/// triangle, for tens of thousands of triangles. That mattered little when each
/// triangle asked once, but the straddle fix below has to ask about *fragments*,
/// so the query had to get cheap first. Sorted per axis, binary-searched to the
/// `±CSG_CENTROID_TOL` window — for a level of a few hundred brushes that's a
/// couple of candidates instead of several hundred.
struct FaceIndex {
    /// Per axis, `(face_pos, brush_index, side)` sorted ascending by `face_pos`.
    axes: [Vec<(f32, u32, Side)>; 3],
}

impl FaceIndex {
    fn build(brushes: &[BrushInfo]) -> FaceIndex {
        let mut axes: [Vec<(f32, u32, Side)>; 3] = Default::default();
        for (i, b) in brushes.iter().enumerate() {
            for axis in 0..3 {
                axes[axis].push((b.min[axis], i as u32, Side::Min));
                axes[axis].push((b.max[axis], i as u32, Side::Max));
            }
        }
        for a in axes.iter_mut() {
            a.sort_by(|x, y| x.0.total_cmp(&y.0));
        }
        FaceIndex { axes }
    }

    /// Every brush face within [`CSG_CENTROID_TOL`] of the plane at `pos_along`,
    /// written into `out`.
    ///
    /// Deliberately **position-independent within the plane**: a fold triangle is
    /// axis-aligned (every brush is an AABB), so all of its points share one
    /// coordinate along the dominant axis and therefore one candidate set. That is
    /// what makes the straddle analysis below exact rather than a sampling guess —
    /// the only thing that can vary across the face of a triangle is which
    /// candidate *contains* it.
    fn candidates(&self, axis: usize, pos_along: f32, out: &mut Vec<(usize, Side)>) {
        out.clear();
        let faces = &self.axes[axis];
        let lo = pos_along - CSG_CENTROID_TOL;
        let hi = pos_along + CSG_CENTROID_TOL;
        let start = faces.partition_point(|f| f.0 < lo);
        for &(pos, bi, side) in &faces[start..] {
            if pos > hi {
                break;
            }
            out.push((bi as usize, side));
        }
    }
}

/// How close two candidate faces have to be for "nearest wins" to give way to
/// "smaller brush wins".
///
/// This used to be an exact `==` on two `f32` distances, which is the same as
/// saying the tie-break never fires unless the two face coordinates are
/// bit-identical. Faces meant to be coincident usually *are* bit-identical (WT
/// coords are grid-aligned), so the bug this hides is narrow — but where it bit,
/// it decided the winner by position in the brush list rather than by geometry,
/// and that order shifts as brushes are added and removed. A tolerance makes the
/// decision a property of the level instead of a property of the array.
const OWNER_TIE_EPS: f32 = 1e-4;

/// One brush face competing to own a triangle, with the numbers the decision was
/// made on. Produced by [`owner_candidates`] for the editor's surface probe —
/// this is the "why did this triangle get that texture" readout.
#[derive(Clone, Copy, Debug)]
pub struct OwnerCandidate {
    pub brush_id: u32,
    pub axis: Axis,
    pub side: Side,
    /// Distance in WT from the triangle's plane to this brush's face plane.
    pub dist: f32,
    /// The brush's volume in WT³ — the tie-break, smallest wins.
    pub volume: f32,
    /// The scheme this candidate would give the triangle (its override, if the
    /// face carries one, else the brush's own theme).
    pub scheme: usize,
    /// Whether this face carries a painted override.
    pub overridden: bool,
    /// Whether the triangle's centroid falls inside this face's claim rect. A
    /// candidate that fails this is on the right plane but the wrong part of it.
    pub contains: bool,
    /// Whether this candidate won.
    pub chosen: bool,
}

/// Resolve the owning face among an already-collected candidate list (the shared
/// decision function — [`face_owner`], the hot path and the probe all route
/// through this, so they cannot drift apart).
///
/// Among faces whose brush contains the centroid on the tangent axes: nearest
/// wins; within [`OWNER_TIE_EPS`] the smaller brush wins; a remaining exact tie
/// keeps the earlier brush (ids are allocated in authoring order, so that is
/// stable across save/load and reclustering).
fn owner_from_candidates(
    brushes: &[BrushInfo],
    cands: &[(usize, Side)],
    centroid_wt: [f32; 3],
    axis: usize,
) -> Option<(usize, Side)> {
    let pos_along = centroid_wt[axis];
    // Two tiers, kept separate and resolved at the end: a brush that actually
    // contains the surface, and one that is merely within tolerance of it.
    let mut best: [Option<(usize, Side)>; 2] = [None, None];
    let mut best_dist = [f32::INFINITY; 2];
    let mut best_vol = [f32::INFINITY; 2];
    for &(i, side) in cands {
        let brush = &brushes[i];
        let face_pos = if side == Side::Min { brush.min[axis] } else { brush.max[axis] };
        let dist = (face_pos - pos_along).abs();
        if dist > CSG_CENTROID_TOL {
            continue;
        }
        let tier = if centroid_in_brush(brush, axis, centroid_wt, 0.0) {
            0
        } else if centroid_in_brush(brush, axis, centroid_wt, claim_tol(brush)) {
            1
        } else {
            continue;
        };
        let vol = brush.volume();
        let better = if dist < best_dist[tier] - OWNER_TIE_EPS {
            true
        } else if dist > best_dist[tier] + OWNER_TIE_EPS {
            false
        } else {
            vol < best_vol[tier] - 1e-6
        };
        if better {
            best_dist[tier] = dist;
            best_vol[tier] = vol;
            best[tier] = Some((i, side));
        }
    }
    best[0].or(best[1])
}

/// How far beyond its own bounds a brush may still claim a surface, in WT.
///
/// This is slop, not geometry: it exists so a triangle whose centroid drifts a
/// little off the face it covers still finds its owner. Frames get none — they
/// would otherwise claim the wall just outside the cutout.
///
/// It is deliberately a *second-tier* allowance (see [`owner_from_candidates`]).
/// Treating it as an ordinary claim meant a small brush could outrank the brush
/// that genuinely contained a surface, anywhere within half a tile of itself —
/// which is a vent duct painting its own theme in a band all the way around its
/// mouth, since a duct shares a plane with the wall it was bored through and wins
/// every distance tie on size.
#[inline]
fn claim_tol(b: &BrushInfo) -> f32 {
    if b.frame {
        0.0
    } else {
        CSG_CENTROID_TOL
    }
}

/// Recover the owning **face** of a triangle (port of `buildFaceMap`, plus the
/// `Side` the winning face sits on so a per-face override can be keyed to it).
///
/// Linear over every brush — used by tests and by the one-shot surface probe. The
/// bake goes through [`FaceIndex`] instead.
fn face_owner(brushes: &[BrushInfo], centroid_wt: [f32; 3], axis: usize) -> Option<(usize, Side)> {
    let cands: Vec<(usize, Side)> = (0..brushes.len())
        .flat_map(|i| [(i, Side::Min), (i, Side::Max)])
        .collect();
    owner_from_candidates(brushes, &cands, centroid_wt, axis)
}

/// Every face competing for a triangle at `centroid_wt`, annotated with why it
/// won or lost. The editor's PAINT tab renders this directly.
pub fn owner_candidates(
    brushes: &[BrushInfo],
    centroid_wt: [f32; 3],
    axis: usize,
) -> Vec<OwnerCandidate> {
    let winner = face_owner(brushes, centroid_wt, axis);
    let ax = axis_from_index(axis);
    let pos_along = centroid_wt[axis];
    let mut out = Vec::new();
    for (i, brush) in brushes.iter().enumerate() {
        for side in [Side::Min, Side::Max] {
            let face_pos = if side == Side::Min { brush.min[axis] } else { brush.max[axis] };
            let dist = (face_pos - pos_along).abs();
            if dist > CSG_CENTROID_TOL {
                continue;
            }
            let ov = brush.face_tex[face_slot(ax, side)];
            out.push(OwnerCandidate {
                brush_id: brush.id,
                axis: ax,
                side,
                dist,
                volume: brush.volume(),
                scheme: ov.map(|o| o.scheme).unwrap_or(brush.scheme),
                overridden: ov.is_some(),
                contains: centroid_in_brush(brush, axis, centroid_wt, 0.0),
                chosen: winner == Some((i, side)),
            });
        }
    }
    out.sort_by(|a, b| a.dist.total_cmp(&b.dist).then(a.volume.total_cmp(&b.volume)));
    out
}

/// Ceiling on how many pieces one straddling triangle may be cut into before we
/// give up and attribute the whole thing by its centroid. Real cases produce
/// three to nine; this only exists so a pathological brush pile can't turn one
/// triangle into an unbounded mesh.
const MAX_STRADDLE_FRAGMENTS: usize = 256;

/// Classify a CSG triangle soup (positions/indices in meters) into the builder.
/// Each triangle is attributed to its owning brush face (face-map) for its
/// `scheme` and `floor_y` anchor, then classified into a zone. `brushes` are the
/// region's brushes (WT); `default_scheme` is used for triangles with no owner
/// (e.g. shell boundary, or the structures mesh which passes an empty brush list).
///
/// # Straddling triangles
///
/// Attribution is by **centroid**, and for most of this function's life a triangle
/// got exactly one owner on that basis. That is wrong whenever a single fold
/// triangle spans two brushes' claims, because then the centroid speaks for
/// geometry it doesn't cover — and the visible symptom is one triangle wearing a
/// neighbouring room's texture, appearing and disappearing as unrelated edits
/// change how the BSP happens to triangulate that surface.
///
/// It is not a rare configuration. Two adjoining rooms with different themes share
/// a *coplanar* floor, and coplanar polygons do not split each other in a BSP fold
/// — so the boundary between them is exactly where nothing forces a cut and a
/// triangle is free to straddle.
///
/// The fix is to cut them. For a planar, axis-aligned triangle the winning
/// candidate can only change across the triangle through the containment test, so
/// the owner boundaries are precisely the candidates' claim-rect edges on the two
/// tangent axes. Splitting there and attributing each fragment separately is exact
/// — not a sampling heuristic — and it costs nothing on the overwhelming majority
/// of triangles, where no such edge crosses the triangle at all.
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
    let index = FaceIndex::build(brushes);

    // Scratch buffers, reused across triangles so the hot path allocates nothing.
    // The split path below does allocate, on purpose: it runs on a small minority of
    // triangles and `split_tris` hands back a fresh vector by design.
    let mut cands: Vec<(usize, Side)> = Vec::new();
    let mut planes: Vec<(usize, f32)> = Vec::new();

    let tri_count = idx.len() / 3;
    for t in 0..tri_count {
        let i0 = idx[t * 3] as usize;
        let i1 = idx[t * 3 + 1] as usize;
        let i2 = idx[t * 3 + 2] as usize;
        let va = [pos[i0 * 3], pos[i0 * 3 + 1], pos[i0 * 3 + 2]];
        let vb = [pos[i1 * 3], pos[i1 * 3 + 1], pos[i1 * 3 + 2]];
        let vc = [pos[i2 * 3], pos[i2 * 3 + 1], pos[i2 * 3 + 2]];

        let n = normalize(cross(sub(vb, va), sub(vc, va)));
        let dom = dominant_axis(n) as usize;
        let tri = [va, vb, vc];
        let c_wt = centroid_wt(tri);

        index.candidates(dom, c_wt[dom], &mut cands);
        let owner = owner_from_candidates(brushes, &cands, c_wt, dom);

        // Where does the *drawn result* change across this triangle? Only at a
        // candidate's claim-rect edge on one of the two tangent axes, only if that
        // edge cuts the triangle, and only if crossing it changes something visible.
        straddle_planes(
            brushes, &cands, dom, c_wt[dom], tri, owner, default_scheme, &mut planes,
        );

        if planes.is_empty() {
            classify_fragment(b, tri, n, dom, owner, brushes, &frames, has_frames, default_scheme);
            continue;
        }

        let mut frags = vec![tri];
        for &(axis, val) in planes.iter() {
            if frags.len() > MAX_STRADDLE_FRAGMENTS {
                break;
            }
            frags = split_tris(frags, axis, val);
        }
        if frags.len() > MAX_STRADDLE_FRAGMENTS {
            log::warn!(
                "uv_zones: triangle straddles {} candidate claims and split into {} \
                 fragments; attributing it whole instead",
                cands.len(),
                frags.len()
            );
            classify_fragment(b, tri, n, dom, owner, brushes, &frames, has_frames, default_scheme);
            continue;
        }
        for &frag in &frags {
            let owner = owner_from_candidates(brushes, &cands, centroid_wt(frag), dom);
            classify_fragment(b, frag, n, dom, owner, brushes, &frames, has_frames, default_scheme);
        }
    }
}

/// Everything a candidate face would give a triangle that is *visible*: the theme,
/// the forced zone slot if it carries one, and the UV anchor. Floats go in by bit
/// pattern so this is comparable.
///
/// Two candidates with equal outcomes are interchangeable. That matters because the
/// boundary between them draws identically either way, and cutting the mesh at an
/// invisible boundary is pure cost — see [`straddle_planes`].
fn face_outcome(
    brushes: &[BrushInfo],
    i: usize,
    side: Side,
    axis: usize,
) -> (usize, Option<u8>, [u32; 2]) {
    let b = &brushes[i];
    let ov = b.face_tex[face_slot(axis_from_index(axis), side)];
    // Only the two *in-plane* components of the anchor reach the UVs (`vertex_uv`
    // projects away the dominant axis), so the third must not count as a
    // difference. It is not a detail: `floor_y` varies room to room and is
    // irrelevant to every floor and ceiling, and comparing it anyway had the
    // classifier cutting floors along boundaries that draw identically.
    let anchor = match axis {
        0 => [b.floor_y, b.origin_xz[1]],
        1 => [b.origin_xz[0], b.origin_xz[1]],
        _ => [b.origin_xz[0], b.floor_y],
    };
    (
        ov.map(|o| o.scheme).unwrap_or(b.scheme),
        ov.and_then(|o| o.zone),
        [anchor[0].to_bits(), anchor[1].to_bits()],
    )
}

/// The outcome for a triangle no brush owns.
fn unowned_outcome(default_scheme: usize) -> (usize, Option<u8>, [u32; 2]) {
    (default_scheme, None, [0f32.to_bits(); 2])
}

/// The claim-rect edges that cut this triangle, as `(axis, world-metre value)`
/// pairs ready for [`split_tris`]. Empty — the overwhelmingly common case — means
/// the whole triangle draws the same way, so it can be emitted whole.
///
/// # Why this is not simply every candidate's edges
///
/// It was, and the cost was ruinous: measured over the levels on disk, splitting at
/// every claim edge multiplied the triangle count by up to six. A doorframe or a
/// pillar sharing a plane with a room's floor cut that floor into strips which were
/// then all re-attributed to the same room — a boundary that costs geometry and
/// changes no pixel.
///
/// The boundaries are the candidates' **real bounds**, matching tier 0 of
/// [`owner_from_candidates`]. The slop allowance on top of them is not a boundary
/// and must not be cut at: it is there to find an owner for a surface that has
/// drifted, and cutting at it would draw the allowance — half a tile of a
/// neighbouring brush's theme, ringing every brush. A triangle that finds its owner
/// only through the slop tier is therefore still attributed whole, by its centroid,
/// exactly as everything was before this function existed.
///
/// So two prunes, both exact with respect to what is drawn:
///
/// 1. **Rank order.** Candidates are walked in the order the owner rule ranks them,
///    and the walk stops at the first whose claim covers the whole triangle: it wins
///    everywhere, so nothing below it is reachable.
/// 2. **Visible difference.** A candidate whose [`face_outcome`] matches the
///    triangle's own owner contributes no edge, because crossing that boundary
///    changes nothing you could see.
#[allow(clippy::too_many_arguments)]
fn straddle_planes(
    brushes: &[BrushInfo],
    cands: &[(usize, Side)],
    axis: usize,
    pos_along: f32,
    tri: [[f32; 3]; 3],
    owner: Option<(usize, Side)>,
    default_scheme: usize,
    out: &mut Vec<(usize, f32)>,
) {
    out.clear();
    if cands.is_empty() {
        return;
    }
    let (tmin, tmax) = tri_bbox(tri[0], tri[1], tri[2]);
    let tangents: [usize; 2] = match axis {
        0 => [2, 1],
        1 => [0, 2],
        _ => [0, 1],
    };
    let here = match owner {
        Some((i, side)) => face_outcome(brushes, i, side, axis),
        None => unowned_outcome(default_scheme),
    };

    // Rank order must match `owner_from_candidates` exactly, or the walk can stop
    // above a candidate that would in fact have won part of the triangle. Stable, so
    // an exact tie keeps list (id) order, the way the winner loop's strict
    // improvement test does.
    let mut ranked: Vec<(usize, Side)> = cands.to_vec();
    ranked.sort_by(|&a, &b| {
        let key = |(i, side): (usize, Side)| {
            let br = &brushes[i];
            let fp = if side == Side::Min { br.min[axis] } else { br.max[axis] };
            ((fp - pos_along).abs(), br.volume())
        };
        let (da, va) = key(a);
        let (db, vb) = key(b);
        if da < db - OWNER_TIE_EPS {
            std::cmp::Ordering::Less
        } else if da > db + OWNER_TIE_EPS {
            std::cmp::Ordering::Greater
        } else {
            va.total_cmp(&vb)
        }
    });

    // The winner's own claim edges count too: where it stops containing the
    // triangle, something else (or nothing) takes over. Held back until the walk
    // finishes so a covering candidate can still short-circuit the whole thing.
    let mut owner_partial: Option<(usize, Side)> = None;

    for &(i, side) in &ranked {
        let b = &brushes[i];
        let fp = if side == Side::Min { b.min[axis] } else { b.max[axis] };
        if (fp - pos_along).abs() > CSG_CENTROID_TOL {
            continue;
        }
        let claim = |t: usize| (b.min[t] * WORLD_SCALE, b.max[t] * WORLD_SCALE);
        let (lo0, hi0) = claim(tangents[0]);
        let (lo1, hi1) = claim(tangents[1]);

        // Misses the triangle entirely — it can never own any of it.
        if hi0 <= tmin[tangents[0]] + 1e-5
            || lo0 >= tmax[tangents[0]] - 1e-5
            || hi1 <= tmin[tangents[1]] + 1e-5
            || lo1 >= tmax[tangents[1]] - 1e-5
        {
            continue;
        }
        let covers = lo0 <= tmin[tangents[0]] + 1e-5
            && hi0 >= tmax[tangents[0]] - 1e-5
            && lo1 <= tmin[tangents[1]] + 1e-5
            && hi1 >= tmax[tangents[1]] - 1e-5;

        if covers {
            // Wins everywhere below this rank; the walk is done.
            return;
        }
        if Some((i, side)) == owner {
            owner_partial = Some((i, side));
            continue;
        }
        if face_outcome(brushes, i, side, axis) == here {
            continue; // an invisible boundary — not worth a cut
        }
        for (t, edges) in [(tangents[0], [lo0, hi0]), (tangents[1], [lo1, hi1])] {
            for edge in edges {
                push_edge(out, t, edge, tmin[t], tmax[t]);
            }
        }
    }

    if let Some((i, _)) = owner_partial {
        let b = &brushes[i];
        for &t in &tangents {
            for edge in [b.min[t] * WORLD_SCALE, b.max[t] * WORLD_SCALE] {
                push_edge(out, t, edge, tmin[t], tmax[t]);
            }
        }
    }
}

/// Record a split plane, if it strictly cuts the triangle's extent on that axis and
/// isn't already recorded.
fn push_edge(out: &mut Vec<(usize, f32)>, axis: usize, edge: f32, lo: f32, hi: f32) {
    if edge <= lo + 1e-5 || edge >= hi - 1e-5 {
        return;
    }
    if !out.iter().any(|&(a, v)| a == axis && (v - edge).abs() < 1e-5) {
        out.push((axis, edge));
    }
}

/// Zone-classify one owner-consistent triangle and emit it. This is the body the
/// classifier always had; `classify_soup` now feeds it fragments rather than raw
/// fold triangles.
#[allow(clippy::too_many_arguments)]
fn classify_fragment(
    b: &mut ZonedBuilder,
    tri: [[f32; 3]; 3],
    n: [f32; 3],
    dom: usize,
    owner: Option<(usize, Side)>,
    brushes: &[BrushInfo],
    frames: &[FrameAabb],
    has_frames: bool,
    default_scheme: usize,
) {
    let [va, vb, vc] = tri;
    let axis = dom as u8;

    // The owner supplies the scheme AND the UV anchor. Anchoring on all three axes
    // (not just the floor) is what keeps a vent duct's single bordered panel centred
    // on each face wherever the author cut it — see `BrushInfo::origin_xz`.
    //
    // A painted face override replaces the scheme the owner would have given, and
    // may additionally pin the zone. The anchor is deliberately *not* overridden:
    // repainting a wall must not slide its texture off the floor it lines up with.
    let (scheme, origin, forced_zone) = match owner {
        Some((i, side)) => {
            let br = &brushes[i];
            let ov = br.face_tex[face_slot(axis_from_index(dom), side)];
            (
                ov.map(|o| o.scheme).unwrap_or(br.scheme),
                [br.origin_xz[0], br.floor_y, br.origin_xz[1]],
                ov.and_then(|o| o.zone),
            )
        }
        None => (default_scheme, [0.0, 0.0, 0.0], None),
    };
    let zone_of = |derived: u8| forced_zone.unwrap_or(derived);
    let split_y = (origin[1] + WALL_SPLIT_V) * WORLD_SCALE;

    let (tmin, tmax) = tri_bbox(va, vb, vc);
    let near = has_frames && tri_overlaps_any_frame(frames, tmin, tmax);

    if dom == 1 {
        // ── Floor / ceiling (Y face) ──
        let plain = if n[1] > 0.0 { 0 } else { 1 };
        if !near {
            b.emit_tri(va, vb, vc, n, 1, zone_of(plain), false, origin, scheme);
            return;
        }
        let mut tris = vec![[va, vb, vc]];
        for f in frames {
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
                    b.emit_tri(tri[0], tri[1], tri[2], n, 1, zone_of(zone), f.w_wt == WALL_THICKNESS, origin, scheme);
                }
                Some(f) => {
                    b.emit_tri(tri[0], tri[1], tri[2], n, 1, zone_of(5), f.w_wt == WALL_THICKNESS, origin, scheme);
                }
                None => b.emit_tri(tri[0], tri[1], tri[2], n, 1, zone_of(plain), false, origin, scheme),
            }
        }
    } else {
        // ── Wall (X or Z face) ──
        if !near {
            emit_wall_split(b, [va, vb, vc], n, axis, split_y, origin, scheme, forced_zone);
            return;
        }
        let mut tris = vec![[va, vb, vc]];
        for f in frames {
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
                    b.emit_tri(tri[0], tri[1], tri[2], n, axis, zone_of(5), rotate, origin, scheme);
                }
                None => emit_wall_split(b, tri, n, axis, split_y, origin, scheme, forced_zone),
            }
        }
    }
}

/// Whether a WT centroid lies within a brush's tangent-axis bounds, `tol` past
/// them (JS `centroidInBrush`). Callers pass `0.0` for real containment and
/// [`claim_tol`] for the slop allowance.
fn centroid_in_brush(b: &BrushInfo, axis: usize, c: [f32; 3], tol: f32) -> bool {
    let within = |i: usize| c[i] >= b.min[i] - tol && c[i] <= b.max[i] + tol;
    match axis {
        0 => within(2) && within(1),
        1 => within(0) && within(2),
        _ => within(0) && within(1),
    }
}

/// Emit a room-wall triangle, split at the zone-2/3 boundary `split_y` (meters).
///
/// `forced_zone` (a painted face override that pins a slot) collapses the band
/// split: the whole wall then draws one texture, which is the point of asking for
/// a specific one.
#[allow(clippy::too_many_arguments)]
fn emit_wall_split(
    b: &mut ZonedBuilder,
    tri: [[f32; 3]; 3],
    n: [f32; 3],
    axis: u8,
    split_y: f32,
    origin: [f32; 3],
    scheme: usize,
    forced_zone: Option<u8>,
) {
    if let Some(z) = forced_zone {
        b.emit_tri(tri[0], tri[1], tri[2], n, axis, z, false, origin, scheme);
        return;
    }
    let (min_y, max_y) = (
        tri[0][1].min(tri[1][1]).min(tri[2][1]),
        tri[0][1].max(tri[1][1]).max(tri[2][1]),
    );
    if max_y <= split_y {
        b.emit_tri(tri[0], tri[1], tri[2], n, axis, 2, false, origin, scheme);
        return;
    }
    if min_y >= split_y {
        b.emit_tri(tri[0], tri[1], tri[2], n, axis, 3, false, origin, scheme);
        return;
    }
    let mut v = tri;
    v.sort_by(|a, b| a[1].total_cmp(&b[1]));
    let (lo, mid, hi) = (v[0], v[1], v[2]);
    let p_lo_hi = lerp_at_y(lo, hi, split_y);
    if mid[1] <= split_y {
        let p_mid_hi = lerp_at_y(mid, hi, split_y);
        b.emit_tri(lo, mid, p_lo_hi, n, axis, 2, false, origin, scheme);
        b.emit_tri(mid, p_mid_hi, p_lo_hi, n, axis, 2, false, origin, scheme);
        b.emit_tri(p_lo_hi, p_mid_hi, hi, n, axis, 3, false, origin, scheme);
    } else {
        let p_lo_mid = lerp_at_y(lo, mid, split_y);
        b.emit_tri(lo, p_lo_mid, p_lo_hi, n, axis, 2, false, origin, scheme);
        b.emit_tri(p_lo_mid, mid, p_lo_hi, n, axis, 3, false, origin, scheme);
        b.emit_tri(mid, hi, p_lo_hi, n, axis, 3, false, origin, scheme);
    }
}

// ─── Geometry helpers ────────────────────────────────────────────────

/// The [`Axis`] for an `[x, y, z]` array index — the inverse of [`Axis::index`],
/// which the classifier needs because it works in raw indices but a face override
/// is keyed by the enum.
#[inline]
fn axis_from_index(i: usize) -> Axis {
    match i {
        0 => Axis::X,
        1 => Axis::Y,
        _ => Axis::Z,
    }
}

/// A triangle's centroid in WT (tile) units — the point every attribution
/// decision is made at.
#[inline]
fn centroid_wt(t: [[f32; 3]; 3]) -> [f32; 3] {
    let c = centroid(t);
    [c[0] / WORLD_SCALE, c[1] / WORLD_SCALE, c[2] / WORLD_SCALE]
}

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
    use crate::geometry::csg_runtime::{Axis, Brush, FaceTex, Op, Region, Side};

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

    /// A `BrushInfo` for the classifier tests: an AABB in WT with a scheme, no
    /// frame flags and no overrides.
    fn info(id: u32, min: [f32; 3], max: [f32; 3], scheme: usize) -> BrushInfo {
        BrushInfo {
            id,
            min,
            max,
            floor_y: min[1],
            origin_xz: [0.0, 0.0],
            scheme,
            frame: false,
            door: false,
            face_tex: [None; 6],
        }
    }

    /// One floor triangle (Y-up, at world height `y`) spanning the given X/Z
    /// corners, as the `(positions, indices)` pair `classify_soup` takes.
    fn floor_tri(x0: f32, x1: f32, z0: f32, z1: f32, y: f32) -> (Vec<f32>, Vec<u32>) {
        let m = WORLD_SCALE;
        (
            vec![
                x0 * m, y * m, z0 * m,
                x1 * m, y * m, z0 * m,
                x1 * m, y * m, z1 * m,
            ],
            vec![0, 1, 2],
        )
    }

    impl BrushInfo {
        /// Give a test brush the duct treatment: its own UV anchor, as
        /// `Region::brush_infos` does for a `vent` brush.
        fn vent_like(&mut self) {
            self.origin_xz = [self.min[0], self.min[2]];
        }
    }

    fn schemes_present(m: &TexturedMesh) -> std::collections::BTreeSet<u16> {
        m.groups.iter().map(|g| g.scheme).collect()
    }

    #[test]
    fn a_triangle_spanning_two_rooms_is_split_and_each_half_keeps_its_own_theme() {
        // The defect this guards: two adjoining rooms share a *coplanar* floor, and
        // coplanar polygons don't split each other in a BSP fold — so one triangle can
        // cover both rooms. Attributing it by its centroid gave the whole thing one
        // room's theme, which is the "random wrong-textured triangle" symptom.
        let brushes = [
            info(1, [0.0, 0.0, 0.0], [8.0, 8.0, 8.0], 1),
            info(2, [8.0, 0.0, 0.0], [16.0, 8.0, 8.0], 2),
        ];
        let (pos, idx) = floor_tri(0.0, 16.0, 0.0, 8.0, 0.0);
        let mut b = ZonedBuilder::new();
        classify_soup(&mut b, &pos, &idx, &brushes, 0);
        let mesh = b.finish();
        assert_eq!(
            schemes_present(&mesh),
            [1u16, 2].into_iter().collect(),
            "each room's half of the shared triangle keeps its own theme: {:?}",
            mesh.groups.iter().map(|g| (g.scheme, g.zone, g.count / 3)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_triangle_inside_one_room_is_not_split() {
        // The fast path has to stay fast: a triangle wholly inside one brush's claim
        // must come out as exactly one triangle, not a fan of fragments.
        let brushes = [info(1, [0.0, 0.0, 0.0], [16.0, 8.0, 16.0], 1)];
        let (pos, idx) = floor_tri(2.0, 6.0, 2.0, 6.0, 0.0);
        let mut b = ZonedBuilder::new();
        classify_soup(&mut b, &pos, &idx, &brushes, 0);
        let mesh = b.finish();
        assert_eq!(mesh.indices.len(), 3, "one triangle in, one triangle out");
        assert_eq!(schemes_present(&mesh), [1u16].into_iter().collect());
    }

    #[test]
    fn near_coincident_faces_break_the_tie_by_size_not_by_list_order() {
        // `dist == best_dist` on two f32s is a tie-break that almost never fires. Two
        // faces a whisker apart used to be decided by whichever brush the list happened
        // to hold first — which moves as brushes are added and removed.
        let big = info(1, [0.0, 0.0, 0.0], [16.0, 8.0, 16.0], 1);
        let small = info(2, [4.0, 1e-6, 4.0], [8.0, 8.0, 8.0], 2);
        let c = [6.0, 0.0, 6.0];
        let a = face_owner(&[big, small], c, 1).expect("an owner");
        let b = face_owner(&[small, big], c, 1).expect("an owner");
        assert_eq!(
            (a.0, b.0),
            (1, 0),
            "the smaller brush wins from either ordering (indices differ, brush does not)"
        );
    }

    #[test]
    fn a_face_override_repaints_only_that_face() {
        // A painted override on the room's floor (Y-min) must reach the floor and
        // nothing else.
        let mut region = Region::new(0);
        let mut b = Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 8.0, 8.0, 8.0);
        b.scheme = 1;
        b.set_face_tex(Axis::Y, Side::Min, Some(FaceTex { scheme: 3, zone: None }));
        region.brushes.push(b);
        let tex = region.evaluate_textured();
        let painted: Vec<u8> = tex.groups.iter().filter(|g| g.scheme == 3).map(|g| g.zone).collect();
        assert_eq!(painted, vec![0], "only the floor zone repaints: {painted:?}");
        assert!(
            tex.groups.iter().any(|g| g.scheme == 1 && g.zone == 2),
            "the walls keep the room's own theme"
        );
    }

    #[test]
    fn a_face_override_can_force_a_zone_slot() {
        // Forcing a slot is how you put one specific texture on a wall; it collapses
        // the lower/upper band split, which is the documented cost.
        let mut region = Region::new(0);
        let mut b = Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 8.0, 12.0, 8.0);
        b.scheme = 1;
        b.set_face_tex(Axis::X, Side::Min, Some(FaceTex { scheme: 3, zone: Some(1) }));
        region.brushes.push(b);
        let tex = region.evaluate_textured();
        let painted: Vec<u8> = tex.groups.iter().filter(|g| g.scheme == 3).map(|g| g.zone).collect();
        assert_eq!(painted, vec![1], "the whole face lands in the forced slot");
        assert!(
            tex.groups.iter().any(|g| g.scheme == 1 && g.zone == 2),
            "the other three walls still split into bands"
        );
    }

    /// Every triangle the classifier emits, checked corner by corner against the
    /// owner rule. Returns how many disagree with themselves — i.e. how many are
    /// drawing one theme over geometry that belongs to two.
    fn triangles_spanning_a_boundary(brushes: &[BrushInfo], tex: &TexturedMesh) -> usize {
        let mut spanning = 0;
        for g in &tex.groups {
            for chunk in tex.indices[g.start as usize..(g.start + g.count) as usize].chunks(3) {
                let corners: Vec<[f32; 3]> =
                    chunk.iter().map(|&i| tex.vertices[i as usize].pos).collect();
                let tri = [corners[0], corners[1], corners[2]];
                let n = normalize(cross(sub(tri[1], tri[0]), sub(tri[2], tri[0])));
                if n == [0.0, 0.0, 0.0] {
                    continue; // a degenerate sliver from a split; nothing is drawn
                }
                let dom = dominant_axis(n) as usize;
                let mid = centroid(tri);
                let outcome_at = |p: [f32; 3]| {
                    // Well inside, so a corner sitting exactly on a boundary doesn't
                    // decide the answer.
                    let q = [
                        p[0] + (mid[0] - p[0]) * 0.25,
                        p[1] + (mid[1] - p[1]) * 0.25,
                        p[2] + (mid[2] - p[2]) * 0.25,
                    ];
                    let wt = [q[0] / WORLD_SCALE, q[1] / WORLD_SCALE, q[2] / WORLD_SCALE];
                    face_owner(brushes, wt, dom)
                        .map(|(i, side)| face_outcome(brushes, i, side, dom))
                        .unwrap_or_else(|| unowned_outcome(0))
                };
                let here = outcome_at(mid);
                if tri.iter().any(|&p| outcome_at(p) != here) {
                    spanning += 1;
                }
            }
        }
        spanning
    }

    #[test]
    fn no_emitted_triangle_spans_a_visible_theme_boundary() {
        // The invariant the straddle fix exists to hold, checked without reference to
        // how it is held: resolve the owner independently at each corner of every
        // emitted triangle. A triangle that disagrees with itself is drawing the
        // wrong theme over part of its area.
        //
        // The soup is hand-fed rather than folded. A BSP *often* splits coplanar
        // surfaces at a brush boundary and then there is nothing to catch — which is
        // exactly why the defect is rare and looks random, and why a folded fixture
        // passed this assertion even with the fix switched off. Handing the classifier
        // the straddling triangle directly is what makes the test able to fail.
        let brushes = [
            info(1, [0.0, 0.0, 0.0], [12.0, 10.0, 12.0], 1),
            info(2, [12.0, 0.0, 0.0], [22.0, 10.0, 12.0], 2),
        ];
        // One floor quad and one ceiling quad, each spanning both rooms whole.
        let m = WORLD_SCALE;
        let mut pos: Vec<f32> = Vec::new();
        let mut idx: Vec<u32> = Vec::new();
        for (y, flip) in [(0.0f32, false), (10.0f32, true)] {
            let quad = [
                [0.0, y, 0.0],
                [22.0, y, 0.0],
                [22.0, y, 12.0],
                [0.0, y, 12.0],
            ];
            let base = (pos.len() / 3) as u32;
            for c in quad {
                pos.extend_from_slice(&[c[0] * m, c[1] * m, c[2] * m]);
            }
            let tris: [[u32; 3]; 2] = if flip {
                [[0, 2, 1], [0, 3, 2]]
            } else {
                [[0, 1, 2], [0, 2, 3]]
            };
            for t in tris {
                idx.extend_from_slice(&[base + t[0], base + t[1], base + t[2]]);
            }
        }

        let mut b = ZonedBuilder::new();
        classify_soup(&mut b, &pos, &idx, &brushes, 0);
        let tex = b.finish();
        assert_eq!(
            triangles_spanning_a_boundary(&brushes, &tex),
            0,
            "an emitted triangle spans the boundary between the two rooms"
        );
        assert_eq!(
            schemes_present(&tex),
            [1u16, 2].into_iter().collect(),
            "and both rooms are represented in the result"
        );
    }

    #[test]
    fn a_duct_does_not_halo_its_theme_onto_the_wall_around_its_mouth() {
        // A vent duct carries its own theme, and its mouth sits in the *same plane* as
        // the wall it was bored through — so on that plane the duct and the room are
        // tied on distance and the duct, being far the smaller brush, wins the tie
        // anywhere it is judged to contain the surface.
        //
        // "Contains" is the whole question. `CSG_CENTROID_TOL` lets a brush claim
        // half a tile beyond its own bounds, which was a fudge for triangles whose
        // centroid drifted off the geometry they covered. Applied here it hands the
        // duct a band of wall all the way around its mouth.
        let room = info(1, [0.0, 0.0, 0.0], [24.0, 12.0, 24.0], 1);
        let mut duct = info(2, [8.0, 0.0, -1.0], [12.0, 4.0, 0.0], 5);
        duct.vent_like();
        let brushes = [room, duct];

        // The room's −Z wall, as the fold leaves it: the full face minus the bore.
        // Handed over whole so the classifier, not the BSP, decides where it is cut.
        let m = WORLD_SCALE;
        let mut pos: Vec<f32> = Vec::new();
        let mut idx: Vec<u32> = Vec::new();
        let mut quad = |x0: f32, x1: f32, y0: f32, y1: f32| {
            let base = (pos.len() / 3) as u32;
            for c in [[x0, y0], [x1, y0], [x1, y1], [x0, y1]] {
                pos.extend_from_slice(&[c[0] * m, c[1] * m, 0.0]);
            }
            idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        };
        quad(0.0, 8.0, 0.0, 12.0); // left of the bore
        quad(12.0, 24.0, 0.0, 12.0); // right of it
        quad(8.0, 12.0, 4.0, 12.0); // above it

        let mut b = ZonedBuilder::new();
        classify_soup(&mut b, &pos, &idx, &brushes, 0);
        let tex = b.finish();

        let ducted: u32 = tex.groups.iter().filter(|g| g.scheme == 5).map(|g| g.count / 3).sum();
        assert_eq!(
            ducted, 0,
            "{ducted} wall triangle(s) around the mouth took the duct's theme: {:?}",
            tex.groups.iter().map(|g| (g.scheme, g.zone, g.count / 3)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_probe_reports_the_winner_and_the_losers() {
        let brushes = [
            info(1, [0.0, 0.0, 0.0], [16.0, 8.0, 16.0], 1),
            info(2, [4.0, 0.0, 4.0], [8.0, 8.0, 8.0], 2),
        ];
        let cands = owner_candidates(&brushes, [6.0, 0.0, 6.0], 1);
        let chosen: Vec<u32> = cands.iter().filter(|c| c.chosen).map(|c| c.brush_id).collect();
        assert_eq!(chosen, vec![2], "the smaller brush owns the overlap");
        assert!(
            cands.iter().any(|c| c.brush_id == 1 && !c.chosen && c.contains),
            "the big room is reported as a losing candidate that did contain the point"
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
