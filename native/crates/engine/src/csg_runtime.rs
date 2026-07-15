//! Runtime CSG subsystem — the thing this engine is fundamentally *for*
//! (ENGINE_PORT_PLAN "Engine ↔ Game boundary"). Brushes are authored at runtime
//! during the BUILD phase; each edit re-evaluates the affected region into a mesh
//! and (upstream) a collider.
//!
//! Ports three JS/spike oracles verbatim in behavior:
//! - the brush model (`src/core/BrushDef.js`) — an AABB in world-tile units,
//! - the region model (`src/core/csg/CSGRegion.js`) — a shell auto-fit to the
//!   subtractive brushes plus a 1-WT pad,
//! - the evaluation fold (`spike/.../csg-wasm/src/lib.rs::evaluate`) — shell
//!   then union/subtract in order, with disjoint-AABB early-reject and a
//!   consecutive-subtract pre-merge. Those two optimizations are what keep
//!   re-bake cheap enough to feel instant.
//!
//! Coordinate spaces: brush fields are in **world tiles (WT)**; geometry is
//! emitted in **meters** (WT × [`WORLD_SCALE`]). Matches the JS convention so
//! behavior diffs 1:1 against the reference build.

use csg::{csg_subtract, csg_union, polygons_to_mesh, Polygon};

use crate::mesh::CpuMesh;

/// Meters per world tile. Mirrors `src/core/constants.js` `WORLD_SCALE`.
pub const WORLD_SCALE: f32 = 0.25;

/// Wall thickness in WT — the fundamental unit. Mirrors `src/core/constants.js`
/// `WALL_THICKNESS`. A doorframe / protoroom carve is one WT deep.
pub const WALL_THICKNESS: f32 = 1.0;

/// A brush is either additive (contributes solid) or subtractive (carves).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Add,
    Subtract,
}

/// The three axes a brush face can face along.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
    Z,
}

/// Which end of an axis a face sits on: `Min` (the `x`/`y`/`z` corner) or `Max`
/// (corner + dimension).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Min,
    Max,
}

/// A single CSG brush: an axis-aligned box in WT units plus its operation.
///
/// Position `(x, y, z)` is the **min corner**; `(w, h, d)` are the dimensions —
/// matching `BrushDef` (`maxX = x + w`, etc.). Taper / scheme flags from the JS
/// `BrushDef` are deliberately omitted until a later phase needs them.
///
/// `door` marks the doorframe carve (JS `BrushDef.isDoorframe`): at the BUILD→HUNT
/// bake, `World::build_doors` scans for these to place a breakable panel + a nav
/// overlay over the opening they cut. It carries no CSG meaning (a doorframe is a
/// plain subtract).
#[derive(Clone, Copy, Debug)]
pub struct Brush {
    pub id: u32,
    pub op: Op,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
    pub h: f32,
    pub d: f32,
    pub door: bool,
}

impl Brush {
    pub fn new(id: u32, op: Op, x: f32, y: f32, z: f32, w: f32, h: f32, d: f32) -> Self {
        Brush { id, op, x, y, z, w, h, d, door: false }
    }

    /// Size along an axis (`w`/`h`/`d`).
    #[inline]
    pub fn dim(&self, axis: Axis) -> f32 {
        match axis {
            Axis::X => self.w,
            Axis::Y => self.h,
            Axis::Z => self.d,
        }
    }

    /// Min-corner coordinate along an axis (`x`/`y`/`z`).
    #[inline]
    pub fn min(&self, axis: Axis) -> f32 {
        match axis {
            Axis::X => self.x,
            Axis::Y => self.y,
            Axis::Z => self.z,
        }
    }

    /// Whether a WT point is inside this brush's AABB (half-open, taper ignored
    /// — coarse nav is fine). Mirrors JS `pointInBrush`.
    #[inline]
    pub fn contains(&self, x: f32, y: f32, z: f32) -> bool {
        x >= self.x
            && x < self.x + self.w
            && y >= self.y
            && y < self.y + self.h
            && z >= self.z
            && z < self.z + self.d
    }

    /// The WT coordinate of the plane of the given face.
    #[inline]
    pub fn face_pos(&self, axis: Axis, side: Side) -> f32 {
        match side {
            Side::Min => self.min(axis),
            Side::Max => self.min(axis) + self.dim(axis),
        }
    }

    /// Grow this brush's face outward by `step` WT (JS `applyFullFacePush`): a
    /// `Max` face extends the dimension; a `Min` face moves the corner back and
    /// extends the dimension so the opposite face stays put.
    pub fn push_face(&mut self, axis: Axis, side: Side, step: f32) {
        match side {
            Side::Max => self.set_dim(axis, self.dim(axis) + step),
            Side::Min => {
                self.set_min(axis, self.min(axis) - step);
                self.set_dim(axis, self.dim(axis) + step);
            }
        }
    }

    /// Shrink this brush's face inward by `step` WT (JS `applyFullFacePull`).
    /// Returns `false` (no-op) if the brush is too thin along `axis` to absorb it.
    pub fn pull_face(&mut self, axis: Axis, side: Side, step: f32) -> bool {
        if self.dim(axis) <= step {
            return false;
        }
        match side {
            Side::Max => self.set_dim(axis, self.dim(axis) - step),
            Side::Min => {
                self.set_min(axis, self.min(axis) + step);
                self.set_dim(axis, self.dim(axis) - step);
            }
        }
        true
    }

    #[inline]
    fn set_min(&mut self, axis: Axis, v: f32) {
        match axis {
            Axis::X => self.x = v,
            Axis::Y => self.y = v,
            Axis::Z => self.z = v,
        }
    }

    #[inline]
    fn set_dim(&mut self, axis: Axis, v: f32) {
        match axis {
            Axis::X => self.w = v,
            Axis::Y => self.h = v,
            Axis::Z => self.d = v,
        }
    }
}

// ─── Stairs ──────────────────────────────────────────────────────────
//
// A confirmed CSG stair, split three ways (JS `csgActions.confirmStairOp` +
// `csgStairGeometry` + `navWorld.stairSolidBoxes`):
//   1. Two `subtract` void brushes carve the stairwell tunnel + far corridor
//      into the region (they live in `Region::brushes`, like any subtract).
//   2. This descriptor drives the visible tread/riser/side mesh, which
//      [`Region::evaluate`] appends straight into the region mesh — so treads
//      render with the wall shader AND land in the region's trimesh collider
//      (the player walks/autosteps them for free; no separate physics path).
//   3. [`StairDesc::solid_boxes`] reconstructs the solid step blocks for the
//      nav voxelizer (the mesh isn't visible to grid nav, which reads CSG
//      membership) — the `collectExtraSolids` port.

/// Which way a staircase runs from the selected wall face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StairDir {
    /// Steps descend below the floor into a lower corridor (JS `'down'`).
    Down,
    /// Steps rise above the floor into a raised corridor (JS `'up'`).
    Up,
}

/// A confirmed stair's parameters, in WT. Mirrors the JS `state.csg.csgStairs[]`
/// descriptor: `axis`/`side`/`face_pos` fix the anchoring wall face, `(u0, u1)`
/// the horizontal span along the in-plane `u_axis`, `floor`/`ceil` the vertical
/// extent, and `direction`/`step_count` the run. Enough to rebuild both the tread
/// mesh and the nav solid boxes deterministically (matches the JS oracles).
#[derive(Clone, Copy, Debug)]
pub struct StairDesc {
    pub direction: StairDir,
    pub step_count: u32,
    /// Wall-normal axis the stair steps along (X or Z; never Y).
    pub axis: Axis,
    pub side: Side,
    /// Wall-plane WT coord on `axis` (the face the stair starts flush with).
    pub face_pos: f32,
    /// The horizontal in-plane axis (Z for an X-wall, X for a Z-wall).
    pub u_axis: Axis,
    /// Horizontal span [u0, u1) along `u_axis`.
    pub u0: f32,
    pub u1: f32,
    /// Face bottom (vMin) and the stairwell ceiling H, in WT Y.
    pub floor: f32,
    pub ceil: f32,
}

impl StairDesc {
    /// A WT point mapped into world space by the wall axis (JS `csgStairGeometry`
    /// `tw`): `n` runs along the wall normal, `y` is world-up, `u` runs along the
    /// horizontal in-plane axis.
    #[inline]
    fn tw(&self, n: f32, y: f32, u: f32) -> [f32; 3] {
        let mut p = [0.0f32; 3];
        p[axis_index(self.axis)] = n;
        p[1] = y;
        p[axis_index(self.u_axis)] = u;
        p
    }

    /// Append this stair's tread/riser/side/fill geometry (in meters) to a mesh
    /// buffer. Port of `buildCsgStairGeometry`, but every quad is emitted
    /// **double-sided** — so backface culling is a non-issue and the JS `flip`
    /// bookkeeping is unnecessary (the visible winding always renders, with its
    /// normal toward the viewer). The extra reversed triangles are harmless in
    /// the region's trimesh collider.
    fn append_geometry(&self, pos: &mut Vec<f32>, norm: &mut Vec<f32>, idx: &mut Vec<u32>, ws: f32) {
        let dir = if self.side == Side::Max { 1.0 } else { -1.0 };
        let (u0, u1) = (self.u0, self.u1);
        let floor = self.floor;
        let h_ceil = self.ceil;
        let sc = self.step_count as i32;

        let mut quad = |a: [f32; 3], b: [f32; 3], c: [f32; 3], d: [f32; 3]| {
            push_quad_double(pos, norm, idx, a, b, c, d, ws);
        };

        for k in 0..sc {
            let kf = k as f32;
            // Normal-axis span for this step (1 WT deep).
            let (n_lo, n_hi) = if dir > 0.0 {
                (self.face_pos + kf, self.face_pos + kf + 1.0)
            } else {
                (self.face_pos - (kf + 1.0), self.face_pos - kf)
            };
            // Vertical span for this step.
            let (step_floor, step_top) = match self.direction {
                StairDir::Down => (floor - (kf + 1.0), floor - kf),
                StairDir::Up => (floor + kf, floor + kf + 1.0),
            };

            // Tread (top surface).
            quad(
                self.tw(n_lo, step_top, u0),
                self.tw(n_hi, step_top, u0),
                self.tw(n_hi, step_top, u1),
                self.tw(n_lo, step_top, u1),
            );

            // Riser (vertical front face of the step).
            let riser_pos = match self.direction {
                StairDir::Down => if dir > 0.0 { n_hi } else { n_lo },
                StairDir::Up => if dir > 0.0 { n_lo } else { n_hi },
            };
            quad(
                self.tw(riser_pos, step_floor, u0),
                self.tw(riser_pos, step_floor, u1),
                self.tw(riser_pos, step_top, u1),
                self.tw(riser_pos, step_top, u0),
            );

            // Left/right side walls.
            quad(
                self.tw(n_lo, step_floor, u0),
                self.tw(n_hi, step_floor, u0),
                self.tw(n_hi, step_top, u0),
                self.tw(n_lo, step_top, u0),
            );
            quad(
                self.tw(n_hi, step_floor, u1),
                self.tw(n_lo, step_floor, u1),
                self.tw(n_lo, step_top, u1),
                self.tw(n_hi, step_top, u1),
            );
        }

        // Far-end fill (JS `csgStairGeometry` closing panels).
        match self.direction {
            StairDir::Down if sc > 0 => {
                let scf = sc as f32;
                let (last_lo, last_hi) = if dir > 0.0 {
                    (self.face_pos + (scf - 1.0), self.face_pos + scf)
                } else {
                    (self.face_pos - scf, self.face_pos - (scf - 1.0))
                };
                let ceil_drop = h_ceil - scf;
                // Ceiling panel at the far column.
                quad(
                    self.tw(last_lo, ceil_drop, u0),
                    self.tw(last_hi, ceil_drop, u0),
                    self.tw(last_hi, ceil_drop, u1),
                    self.tw(last_lo, ceil_drop, u1),
                );
                // Vertical wall dropping from H to H-stepCount.
                let ceil_wall = if dir > 0.0 { last_lo } else { last_hi };
                quad(
                    self.tw(ceil_wall, ceil_drop, u0),
                    self.tw(ceil_wall, ceil_drop, u1),
                    self.tw(ceil_wall, h_ceil, u1),
                    self.tw(ceil_wall, h_ceil, u0),
                );
            }
            StairDir::Up if sc > 0 => {
                // Fill the stepped floor underneath the stairs.
                for k in 0..(sc - 1) {
                    let kf = k as f32;
                    let (fill_lo, fill_hi) = if dir > 0.0 {
                        (self.face_pos + kf, self.face_pos + kf + 1.0)
                    } else {
                        (self.face_pos - (kf + 1.0), self.face_pos - kf)
                    };
                    let fill_y = floor + (kf + 1.0);
                    quad(
                        self.tw(fill_lo, fill_y, u0),
                        self.tw(fill_hi, fill_y, u0),
                        self.tw(fill_hi, fill_y, u1),
                        self.tw(fill_lo, fill_y, u1),
                    );
                }
            }
            _ => {}
        }
    }

    /// The tread/riser/side geometry as a standalone mesh (meters), for the ghost
    /// preview drawn while a stair op is pending.
    pub fn mesh(&self) -> CpuMesh {
        let mut pos = Vec::new();
        let mut norm = Vec::new();
        let mut idx = Vec::new();
        self.append_geometry(&mut pos, &mut norm, &mut idx, WORLD_SCALE);
        CpuMesh::from_csg(&pos, &norm, &idx)
    }

    /// Reconstruct the solid step blocks (WT AABBs `[x, y, z, w, h, d]`) — one per
    /// step, from the void floor up to that step's tread. Direct port of
    /// `navWorld.stairSolidBoxes`; fed to the nav voxelizer so grid nav sees the
    /// treads as walkable ground (the mesh isn't visible to grid nav).
    pub fn solid_boxes(&self) -> Vec<[f32; 6]> {
        let dir = if self.side == Side::Max { 1.0 } else { -1.0 };
        let sc = self.step_count as f32;
        let void_floor = match self.direction {
            StairDir::Down => self.floor - sc,
            StairDir::Up => self.floor,
        };
        let (u0, u1) = (self.u0, self.u1);
        let mut boxes = Vec::new();
        for k in 0..self.step_count as i32 {
            let kf = k as f32;
            let n_lo = if dir > 0.0 {
                self.face_pos + kf
            } else {
                self.face_pos - (kf + 1.0)
            };
            let step_top = match self.direction {
                StairDir::Down => self.floor - kf,
                StairDir::Up => self.floor + (kf + 1.0),
            };
            let h = step_top - void_floor;
            if h <= 0.0 {
                continue;
            }
            match self.axis {
                Axis::X => boxes.push([n_lo, void_floor, u0, 1.0, h, u1 - u0]),
                _ => boxes.push([u0, void_floor, n_lo, u1 - u0, h, 1.0]),
            }
        }
        boxes
    }
}

/// Append one double-sided quad (corners in WT, scaled to meters) as four
/// triangles: front + back, each with its own winding normal. Shared by the
/// stair geometry so treads render correctly regardless of view side.
fn push_quad_double(
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
    let n = quad_normal(q0, q1, q2);
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

/// Unit normal of the triangle `a→b→c` (right-hand rule); zero if degenerate.
fn quad_normal(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> [f32; 3] {
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

#[inline]
fn axis_index(axis: Axis) -> usize {
    match axis {
        Axis::X => 0,
        Axis::Y => 1,
        Axis::Z => 2,
    }
}

// ─── Brush → polygons ───────────────────────────────────────────────
//
// Port of `brush_to_polygons` (spike lib.rs): convert a WT-space box to 6 quad
// polygons in meters, CCW-from-outside so `Plane::from_points` yields outward
// normals. Taper is omitted (Phase 1 boxes have none).

fn brush_to_polygons(b: &Brush, ws: f32) -> Vec<Polygon> {
    let ws64 = ws as f64;
    let x0 = (b.x as f64 * ws64) as f32;
    let x1 = ((b.x + b.w) as f64 * ws64) as f32;
    let y0 = (b.y as f64 * ws64) as f32;
    let y1 = ((b.y + b.h) as f64 * ws64) as f32;
    let z0 = (b.z as f64 * ws64) as f32;
    let z1 = ((b.z + b.d) as f64 * ws64) as f32;

    // 8 corners: index bits are (x1?, y1?, z1?).
    let c: [[f32; 3]; 8] = [
        [x0, y0, z0], // 0: ---
        [x1, y0, z0], // 1: +--
        [x0, y1, z0], // 2: -+-
        [x1, y1, z0], // 3: ++-
        [x0, y0, z1], // 4: --+
        [x1, y0, z1], // 5: +-+
        [x0, y1, z1], // 6: -++
        [x1, y1, z1], // 7: +++
    ];

    // 6 faces, CCW winding seen from outside (identical vertex order to spike).
    const FACES: [[usize; 4]; 6] = [
        [0, 4, 6, 2], // x-min
        [1, 3, 7, 5], // x-max
        [0, 1, 5, 4], // y-min
        [2, 6, 7, 3], // y-max
        [0, 2, 3, 1], // z-min
        [4, 5, 7, 6], // z-max
    ];

    FACES
        .iter()
        .filter_map(|vi| Polygon::new(vec![c[vi[0]], c[vi[1]], c[vi[2]], c[vi[3]]]))
        .collect()
}

// ─── AABB (meters) for the evaluate() early-reject ──────────────────

#[derive(Clone, Copy)]
struct Aabb {
    min: [f32; 3],
    max: [f32; 3],
}

impl Aabb {
    fn from_brush(b: &Brush, ws: f32) -> Self {
        let ws64 = ws as f64;
        Aabb {
            min: [
                (b.x as f64 * ws64) as f32,
                (b.y as f64 * ws64) as f32,
                (b.z as f64 * ws64) as f32,
            ],
            max: [
                ((b.x + b.w) as f64 * ws64) as f32,
                ((b.y + b.h) as f64 * ws64) as f32,
                ((b.z + b.d) as f64 * ws64) as f32,
            ],
        }
    }

    fn intersects(&self, o: &Aabb) -> bool {
        self.min[0] <= o.max[0]
            && self.max[0] >= o.min[0]
            && self.min[1] <= o.max[1]
            && self.max[1] >= o.min[1]
            && self.min[2] <= o.max[2]
            && self.max[2] >= o.min[2]
    }

    fn union(&self, o: &Aabb) -> Aabb {
        Aabb {
            min: [
                self.min[0].min(o.min[0]),
                self.min[1].min(o.min[1]),
                self.min[2].min(o.min[2]),
            ],
            max: [
                self.max[0].max(o.max[0]),
                self.max[1].max(o.max[1]),
                self.max[2].max(o.max[2]),
            ],
        }
    }
}

// ─── The fold ───────────────────────────────────────────────────────

/// Evaluate `shell ± brushes` into a polygon soup, in meters. Direct port of
/// the spike `evaluate()`: start from the shell, then apply each brush in order,
/// with a disjoint-AABB early-reject and a consecutive-subtract pre-merge.
fn evaluate(shell: &Brush, brushes: &[Brush], ws: f32) -> Vec<Polygon> {
    let mut result = brush_to_polygons(shell, ws);
    // Grows with unions; subtracts never grow it, so it stays a correct upper
    // bound for early-rejecting non-overlapping brushes.
    let mut acc_aabb = Aabb::from_brush(shell, ws);

    let mut i = 0;
    while i < brushes.len() {
        let is_subtract = brushes[i].op == Op::Subtract;
        let brush_aabb = Aabb::from_brush(&brushes[i], ws);

        if is_subtract {
            // Disjoint subtract is a no-op — skip the BSP build entirely.
            if !brush_aabb.intersects(&acc_aabb) {
                i += 1;
                continue;
            }

            // Consecutive-subtract run: union the overlapping ones, subtract once.
            let mut run_end = i + 1;
            while run_end < brushes.len() && brushes[run_end].op == Op::Subtract {
                run_end += 1;
            }
            if run_end - i >= 3 {
                let mut merged: Vec<Polygon> = Vec::new();
                let mut started = false;
                for j in i..run_end {
                    if !Aabb::from_brush(&brushes[j], ws).intersects(&acc_aabb) {
                        continue;
                    }
                    let polys = brush_to_polygons(&brushes[j], ws);
                    if !started {
                        merged = polys;
                        started = true;
                    } else {
                        merged = csg_union(merged, polys);
                    }
                }
                if started {
                    result = csg_subtract(result, merged);
                }
                i = run_end;
                continue;
            }
        }

        let polys = brush_to_polygons(&brushes[i], ws);
        if is_subtract {
            result = csg_subtract(result, polys);
        } else if !brush_aabb.intersects(&acc_aabb) {
            // Disjoint union — concatenate; no BSP needed.
            result.extend(polys);
            acc_aabb = acc_aabb.union(&brush_aabb);
        } else {
            result = csg_union(result, polys);
            acc_aabb = acc_aabb.union(&brush_aabb);
        }
        i += 1;
    }

    result
}

// ─── Region ─────────────────────────────────────────────────────────

/// One cluster of brushes plus its auto-resized shell — the unit of re-bake and
/// (upstream) the unit of collision (per-region colliders, per the plan). Ports
/// `CSGRegion`: the shell is an additive box fit to the subtractive brushes plus
/// a 1-WT pad so the carved cavities always sit inside solid.
pub struct Region {
    pub id: u32,
    pub brushes: Vec<Brush>,
    /// Confirmed stairs in this region (JS `state.csg.csgStairs`, scoped per
    /// region). Their void brushes live in `brushes`; these descriptors drive the
    /// tread mesh (folded into [`evaluate`](Self::evaluate)) and the nav solids.
    pub stairs: Vec<StairDesc>,
    shell: Brush,
}

/// Shell padding around the subtractive brushes, in WT (JS `WALL_THICKNESS`-ish
/// 1-tile margin so walls have thickness).
const SHELL_PAD: f32 = 1.0;

impl Region {
    pub fn new(id: u32) -> Self {
        // Placeholder shell; update_shell() resizes it before every evaluate.
        Region {
            id,
            brushes: Vec::new(),
            stairs: Vec::new(),
            shell: Brush::new(u32::MAX, Op::Add, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0),
        }
    }

    /// Resize the shell to enclose every subtractive brush, padded by [`SHELL_PAD`]
    /// on all sides. No subtractive brushes → shell left as-is (nothing to house).
    fn update_shell(&mut self) {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        let mut any = false;
        for b in self.brushes.iter().filter(|b| b.op == Op::Subtract) {
            any = true;
            min[0] = min[0].min(b.x);
            min[1] = min[1].min(b.y);
            min[2] = min[2].min(b.z);
            max[0] = max[0].max(b.x + b.w);
            max[1] = max[1].max(b.y + b.h);
            max[2] = max[2].max(b.z + b.d);
        }
        if !any {
            return;
        }
        self.shell.x = min[0] - SHELL_PAD;
        self.shell.y = min[1] - SHELL_PAD;
        self.shell.z = min[2] - SHELL_PAD;
        self.shell.w = (max[0] - min[0]) + SHELL_PAD * 2.0;
        self.shell.h = (max[1] - min[1]) + SHELL_PAD * 2.0;
        self.shell.d = (max[2] - min[2]) + SHELL_PAD * 2.0;
    }

    /// Re-run CSG for this region and return the resulting mesh in meters. Any
    /// confirmed stairs then get their tread/riser/side geometry appended, so a
    /// single mesh (and thus a single trimesh collider) carries both the carved
    /// walls and the walkable steps.
    pub fn evaluate(&mut self) -> CpuMesh {
        self.update_shell();
        let polys = evaluate(&self.shell, &self.brushes, WORLD_SCALE);
        let (mut pos, mut norm, mut idx) = polygons_to_mesh(&polys);
        for s in &self.stairs {
            s.append_geometry(&mut pos, &mut norm, &mut idx, WORLD_SCALE);
        }
        CpuMesh::from_csg(&pos, &norm, &idx)
    }

    /// Recompute the shell to fit the current brushes (call before querying
    /// [`shell`](Self::shell) or [`solid_at`](Self::solid_at) after edits).
    pub fn refresh_shell(&mut self) {
        self.update_shell();
    }

    /// The current shell box (WT). Only valid after [`refresh_shell`](Self::refresh_shell)
    /// or [`evaluate`](Self::evaluate).
    pub fn shell(&self) -> Brush {
        self.shell
    }

    /// Solidity at a WT point: replay CSG membership — inside the shell (solid),
    /// then each brush in order flips it (`add` → solid, `subtract` → air).
    /// Mirrors JS `regionSolidAt`; used by the nav voxelizer.
    pub fn solid_at(&self, x: f32, y: f32, z: f32) -> bool {
        if !self.shell.contains(x, y, z) {
            return false; // outside the shell — this region doesn't cover the point
        }
        let mut solid = true;
        for b in &self.brushes {
            if b.contains(x, y, z) {
                solid = b.op == Op::Add;
            }
        }
        solid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_shell_is_nonempty_and_watertight_count() {
        // Editor's opening move: one subtract brush inside an auto-shell = a room.
        let mut region = Region::new(0);
        region
            .brushes
            .push(Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 12.0, 8.0, 12.0));
        let mesh = region.evaluate();
        assert!(!mesh.vertices.is_empty(), "room should produce geometry");
        assert!(mesh.indices.len() % 3 == 0 && !mesh.indices.is_empty());
    }

    #[test]
    fn disjoint_subtract_is_a_noop() {
        // Test the fold directly with a fixed shell (Region::update_shell would
        // otherwise grow the shell to enclose the far brush). A subtract whose
        // AABB misses the accumulator must leave the result byte-identical to
        // the shell alone.
        let shell = Brush::new(u32::MAX, Op::Add, 0.0, 0.0, 0.0, 12.0, 8.0, 12.0);
        let (_p, _n, base) = polygons_to_mesh(&evaluate(&shell, &[], WORLD_SCALE));

        let far = Brush::new(2, Op::Subtract, 500.0, 500.0, 500.0, 4.0, 4.0, 4.0);
        let (_p2, _n2, with_far) = polygons_to_mesh(&evaluate(&shell, &[far], WORLD_SCALE));
        assert_eq!(base.len(), with_far.len(), "disjoint subtract changed geometry");
    }

    #[test]
    fn push_pull_are_inverse_on_a_max_face() {
        let mut brush = Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 10.0, 8.0, 10.0);
        brush.push_face(Axis::X, Side::Max, 4.0);
        assert_eq!(brush.w, 14.0);
        assert!(brush.pull_face(Axis::X, Side::Max, 4.0));
        assert_eq!(brush.w, 10.0);
    }

    #[test]
    fn pull_refuses_to_collapse_a_thin_brush() {
        let mut brush = Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 3.0, 8.0, 10.0);
        assert!(!brush.pull_face(Axis::X, Side::Max, 4.0), "3 <= 4, must no-op");
        assert_eq!(brush.w, 3.0);
    }

    #[test]
    fn min_face_push_holds_the_opposite_face() {
        let mut brush = Brush::new(1, Op::Subtract, 5.0, 0.0, 0.0, 10.0, 8.0, 10.0);
        let max_before = brush.face_pos(Axis::X, Side::Max);
        brush.push_face(Axis::X, Side::Min, 4.0);
        assert_eq!(brush.x, 1.0);
        assert_eq!(brush.face_pos(Axis::X, Side::Max), max_before, "max face fixed");
    }
}
