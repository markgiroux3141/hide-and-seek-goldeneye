//! Free-standing platforms + connecting stair-runs — the last authoring tool
//! (the JS drag-gizmo `Platform`/`StairRun` system, `src/core/Platform.js`,
//! `src/core/StairRun.js`, `src/geometry/platformGeometry.js`).
//!
//! Unlike CSG stairs (tunnels carved into walls), these are **free-standing**
//! slabs and staircases that connect platforms to each other or to the floor.
//! They are not part of any region's CSG cavity, so they can't fold into a
//! region mesh. Instead every platform/stair-run reduces to a set of **WT AABB
//! boxes** (a platform = its solid slab; a stair-run = its per-step solid
//! blocks, exactly the `navWorld.stairRunStepBoxes` reconstruction). That one
//! box set drives all three consumers, so they can never drift:
//!   - render — combined into the "structures" mesh (checkerboard region shader),
//!   - collision — one trimesh collider (player walks/autosteps it for free),
//!   - nav — the same boxes handed to [`crate::nav::bake`] as extra solids.
//!
//! Railings are the only exception: render-only double-sided sloped quads with
//! no collision (thin planes make poor colliders; JS keeps them cosmetic).
//!
//! Coordinate spaces match the rest of the engine: fields are **world tiles
//! (WT)**; geometry is emitted in **meters** (WT × [`WORLD_SCALE`]).

use crate::csg_runtime::{Brush, Op, WORLD_SCALE};

/// Railing height above the walking surface, in WT (JS `RAILING_HEIGHT`).
const RAILING_HEIGHT: f32 = 3.0;
/// Perpendicular handrail-strip depth, in WT (JS `HANDRAIL_DEPTH`).
const HANDRAIL_DEPTH: f32 = 0.2;
/// Push railings slightly inward to avoid z-fighting (JS `RAILING_INSET`).
const RAILING_INSET: f32 = 0.05;

/// The four horizontal edges of a platform (JS edge keys). The outward normal
/// of `XMin` is −X, `XMax` is +X, etc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    XMin,
    XMax,
    ZMin,
    ZMax,
}

impl Edge {
    pub const ALL: [Edge; 4] = [Edge::XMin, Edge::XMax, Edge::ZMin, Edge::ZMax];

    /// Outward normal in the XZ plane (JS `Platform.edgeNormal`).
    #[inline]
    pub fn normal(self) -> (f32, f32) {
        match self {
            Edge::XMin => (-1.0, 0.0),
            Edge::XMax => (1.0, 0.0),
            Edge::ZMin => (0.0, -1.0),
            Edge::ZMax => (0.0, 1.0),
        }
    }
}

/// One end of a stair-run. A platform end is pinned to an edge at a 0..1
/// `offset` along it; a ground end is a free WT point (JS `anchorFrom`/`anchorTo`
/// — `{edge, offset}` vs `{x, y, z}`).
#[derive(Clone, Copy, Debug)]
pub enum Anchor {
    Edge { edge: Edge, offset: f32 },
    Ground { x: f32, y: f32, z: f32 },
}

/// A rectangular slab at a given height (JS `Platform`). `(x, z)` is the
/// min-corner, `y` the **top** surface, `size_x`/`size_z` the footprint,
/// `thickness` the slab depth. `grounded` extends the underside down to the
/// floor beneath it.
#[derive(Clone, Copy, Debug)]
pub struct Platform {
    pub id: u32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub size_x: f32,
    pub size_z: f32,
    pub thickness: f32,
    pub grounded: bool,
    pub railings: bool,
}

impl Platform {
    #[inline]
    pub fn max_x(&self) -> f32 {
        self.x + self.size_x
    }
    #[inline]
    pub fn max_z(&self) -> f32 {
        self.z + self.size_z
    }
    #[inline]
    pub fn center_x(&self) -> f32 {
        self.x + self.size_x / 2.0
    }
    #[inline]
    pub fn center_z(&self) -> f32 {
        self.z + self.size_z / 2.0
    }

    /// The world-space endpoints of an edge in WT (JS `getEdgeLine`).
    fn edge_line(&self, edge: Edge) -> ((f32, f32), (f32, f32)) {
        match edge {
            Edge::XMin => ((self.x, self.z), (self.x, self.max_z())),
            Edge::XMax => ((self.max_x(), self.z), (self.max_x(), self.max_z())),
            Edge::ZMin => ((self.x, self.z), (self.max_x(), self.z)),
            Edge::ZMax => ((self.x, self.max_z()), (self.max_x(), self.max_z())),
        }
    }

    /// A point at `t` (0..1) along an edge, in WT (JS `getEdgePointAtOffset`).
    pub fn edge_point_at_offset(&self, edge: Edge, t: f32) -> (f32, f32) {
        let (s, e) = self.edge_line(edge);
        (s.0 + (e.0 - s.0) * t, s.1 + (e.1 - s.1) * t)
    }

    /// Length of an edge in WT (JS `getEdgeLength`).
    pub fn edge_length(&self, edge: Edge) -> f32 {
        match edge {
            Edge::XMin | Edge::XMax => self.size_z,
            Edge::ZMin | Edge::ZMax => self.size_x,
        }
    }

    /// Whether a WT XZ point lies within this platform's footprint (with a small
    /// tolerance) — used to identify which platform the crosshair picked.
    pub fn footprint_contains(&self, x: f32, z: f32, eps: f32) -> bool {
        x >= self.x - eps && x <= self.max_x() + eps && z >= self.z - eps && z <= self.max_z() + eps
    }

    /// The solid slab box (WT `[x, y, z, w, h, d]`) or `None` if degenerate.
    /// Direct port of `navWorld.platformSolidBox`, but the underside follows the
    /// render's `findFloorYAt` when grounded (so render/collider/nav agree).
    pub fn solid_box(&self, brushes: &[Brush]) -> Option<[f32; 6]> {
        let y_top = self.y;
        let y_bottom = if self.grounded {
            find_floor_y_at(self.center_x(), self.center_z(), self.y, brushes)
        } else {
            self.y - self.thickness
        };
        let h = y_top - y_bottom;
        if h <= 0.0 {
            return None;
        }
        Some([self.x, y_bottom, self.z, self.size_x, h, self.size_z])
    }
}

/// A flight of stairs connecting two platforms, or a platform to the ground, or
/// two ground points (JS `StairRun`). Anchors are auto-centered on platform
/// edges (offset 0.5) when placed via the connect tool.
#[derive(Clone, Copy, Debug)]
pub struct StairRun {
    pub id: u32,
    pub from_platform: Option<u32>,
    pub to_platform: Option<u32>,
    pub anchor_from: Anchor,
    pub anchor_to: Anchor,
    pub width: f32,
    pub step_height: f32,
    pub rise_over_run: f32,
    pub grounded: bool,
    pub railings: bool,
}

/// A resolved run axis: which horizontal axis (`x`|`z`) the flight advances along.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RunAxis {
    X,
    Z,
}

/// A resolved 3D anchor point in WT.
#[derive(Clone, Copy)]
struct AnchorPt {
    x: f32,
    y: f32,
    z: f32,
}

/// Resolve an anchor to a WT point (JS `resolveStairAnchor`). A platform end
/// uses the edge point at its offset (default 0.5) at the platform's top Y; a
/// ground end is the stored point.
fn resolve_anchor(platform: Option<&Platform>, anchor: &Anchor) -> AnchorPt {
    match (platform, anchor) {
        (Some(p), Anchor::Edge { edge, offset }) => {
            let (x, z) = p.edge_point_at_offset(*edge, *offset);
            AnchorPt { x, y: p.y, z }
        }
        (_, Anchor::Ground { x, y, z }) => AnchorPt {
            x: *x,
            y: *y,
            z: *z,
        },
        // A platform end with a ground anchor (or vice-versa) shouldn't occur;
        // fall back to the ground interpretation so we never panic.
        (None, Anchor::Edge { .. }) => AnchorPt {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        },
    }
}

/// Determine the run axis from the anchoring platform edges, or the dominant
/// horizontal axis between the two points (JS `computeStairRunAxis`).
fn compute_run_axis(
    top_platform: Option<&Platform>,
    top_anchor: &Anchor,
    bottom_platform: Option<&Platform>,
    bottom_anchor: &Anchor,
    top_pt: AnchorPt,
    bottom_pt: AnchorPt,
) -> RunAxis {
    if let (Some(_), Anchor::Edge { edge, .. }) = (top_platform, top_anchor) {
        let (nx, _) = edge.normal();
        return if nx != 0.0 { RunAxis::X } else { RunAxis::Z };
    }
    if let (Some(_), Anchor::Edge { edge, .. }) = (bottom_platform, bottom_anchor) {
        let (nx, _) = edge.normal();
        return if nx != 0.0 { RunAxis::X } else { RunAxis::Z };
    }
    let dx = (bottom_pt.x - top_pt.x).abs();
    let dz = (bottom_pt.z - top_pt.z).abs();
    if dx >= dz {
        RunAxis::X
    } else {
        RunAxis::Z
    }
}

/// Resolved run parameters shared by the box and railing builders.
struct RunGeom {
    run_axis: RunAxis,
    top_run: f32,
    bottom_run: f32,
    step_run: f32,
    steps: u32,
    step_rise: f32,
    stair_base_y: f32,
    floor_y: f32,
    top_y: f32,
    perp_min: f32,
    perp_max: f32,
}

/// Resolve a stair-run's geometry parameters, or `None` for a degenerate run
/// (zero rise). Mirrors `navGrid.stairRunSolids` + `buildBoxStairGeometry`.
fn resolve_run(
    run: &StairRun,
    from_platform: Option<&Platform>,
    to_platform: Option<&Platform>,
    brushes: &[Brush],
) -> Option<RunGeom> {
    let from_pt = resolve_anchor(from_platform, &run.anchor_from);
    let to_pt = resolve_anchor(to_platform, &run.anchor_to);

    let from_top = from_pt.y >= to_pt.y;
    let top_pt = if from_top { from_pt } else { to_pt };
    let bottom_pt = if from_top { to_pt } else { from_pt };
    let top_platform = if from_top { from_platform } else { to_platform };
    let bottom_platform = if from_top { to_platform } else { from_platform };
    let top_anchor = if from_top {
        &run.anchor_from
    } else {
        &run.anchor_to
    };
    let bottom_anchor = if from_top {
        &run.anchor_to
    } else {
        &run.anchor_from
    };

    let rise = top_pt.y - bottom_pt.y;
    if rise == 0.0 {
        return None;
    }

    let run_axis = compute_run_axis(
        top_platform,
        top_anchor,
        bottom_platform,
        bottom_anchor,
        top_pt,
        bottom_pt,
    );

    let (top_run, bottom_run, top_perp) = match run_axis {
        RunAxis::X => (top_pt.x, bottom_pt.x, top_pt.z),
        RunAxis::Z => (top_pt.z, bottom_pt.z, top_pt.x),
    };
    let half_width = run.width / 2.0;
    let steps = (rise / run.step_height).round().max(1.0) as u32;
    let floor_y = if run.grounded {
        find_floor_y_at(bottom_pt.x, bottom_pt.z, bottom_pt.y, brushes)
    } else {
        bottom_pt.y
    };

    Some(RunGeom {
        run_axis,
        top_run,
        bottom_run,
        step_run: (bottom_run - top_run) / steps as f32,
        steps,
        step_rise: rise / steps as f32,
        stair_base_y: bottom_pt.y,
        floor_y,
        top_y: top_pt.y,
        perp_min: top_perp - half_width,
        perp_max: top_perp + half_width,
    })
}

/// The solid step blocks of a stair-run (WT `[x,y,z,w,h,d]`), one per step from
/// the floor up to that step's tread. Direct port of `stairRunStepBoxes`; drives
/// render, collision, and nav alike. Returns `[]` for a degenerate run.
pub fn stair_run_boxes(
    run: &StairRun,
    from_platform: Option<&Platform>,
    to_platform: Option<&Platform>,
    brushes: &[Brush],
) -> Vec<[f32; 6]> {
    let Some(g) = resolve_run(run, from_platform, to_platform, brushes) else {
        return Vec::new();
    };
    let mut boxes = Vec::new();
    for i in 0..g.steps {
        let i = i as f32;
        let r_front = g.top_run + (g.steps as f32 - i) * g.step_run;
        let r_back = g.top_run + (g.steps as f32 - i - 1.0) * g.step_run;
        let run_lo = r_front.min(r_back);
        let run_hi = r_front.max(r_back);
        let step_top = g.stair_base_y + (i + 1.0) * g.step_rise;
        let h = step_top - g.floor_y;
        if h <= 0.0 || run_hi - run_lo <= 0.0 {
            continue;
        }
        match g.run_axis {
            RunAxis::X => boxes.push([
                run_lo,
                g.floor_y,
                g.perp_min,
                run_hi - run_lo,
                h,
                g.perp_max - g.perp_min,
            ]),
            RunAxis::Z => boxes.push([
                g.perp_min,
                g.floor_y,
                run_lo,
                g.perp_max - g.perp_min,
                h,
                run_hi - run_lo,
            ]),
        }
    }
    boxes
}

/// Highest CSG room floor at `(x, z)` strictly below `above_y` (all WT). Used by
/// grounded platforms/stairs to extend their undersides to the visible floor
/// beneath them. Returns 0 when no subtract brush covers that XZ (preserves the
/// world-ground default). Direct port of `platformGeometry.findFloorYAt`.
pub fn find_floor_y_at(x: f32, z: f32, above_y: f32, brushes: &[Brush]) -> f32 {
    let mut best = 0.0f32;
    let mut found = false;
    for b in brushes.iter().filter(|b| b.op == Op::Subtract) {
        if x < b.x || x > b.x + b.w {
            continue;
        }
        if z < b.z || z > b.z + b.d {
            continue;
        }
        if b.y >= above_y {
            continue;
        }
        if !found || b.y > best {
            best = b.y;
            found = true;
        }
    }
    if found {
        best
    } else {
        0.0
    }
}

// ─── Connect-flow edge helpers ───────────────────────────────────────

/// The platform edge closest to a WT XZ point (JS `closestPlatformEdge`). Used
/// to pick the destination platform's anchoring edge from the connect click.
pub fn closest_platform_edge(p: &Platform, x: f32, z: f32) -> Edge {
    let mut best = Edge::XMin;
    let mut best_d = f32::INFINITY;
    for edge in Edge::ALL {
        let (s, e) = p.edge_line(edge);
        let d = dist_to_segment(x, z, s.0, s.1, e.0, e.1);
        if d < best_d {
            best_d = d;
            best = edge;
        }
    }
    best
}

/// The 0..1 offset along an edge closest to a WT XZ point, snapped to whole WT
/// (JS `closestOffsetOnEdge`). Used to slide the stair's attach point along the
/// source edge and to align the destination anchor to it.
pub fn offset_along_edge(p: &Platform, edge: Edge, x: f32, z: f32) -> f32 {
    let (s, e) = p.edge_line(edge);
    let ex = e.0 - s.0;
    let ez = e.1 - s.1;
    let len_sq = ex * ex + ez * ez;
    if len_sq == 0.0 {
        return 0.5;
    }
    let t = ((x - s.0) * ex + (z - s.1) * ez) / len_sq;
    let edge_len = p.edge_length(edge);
    let wt_pos = (t.clamp(0.0, 1.0) * edge_len).round();
    wt_pos.clamp(0.0, edge_len) / edge_len
}

/// The edge whose outward normal best aligns with a direction in the XZ plane
/// (JS `bestEdgeForDirection`). Picks the source edge that faces the target.
pub fn best_edge_for_direction(dx: f32, dz: f32) -> Edge {
    let mut best = Edge::XMin;
    let mut best_dot = f32::NEG_INFINITY;
    for edge in Edge::ALL {
        let (nx, nz) = edge.normal();
        let dot = nx * dx + nz * dz;
        if dot > best_dot {
            best_dot = dot;
            best = edge;
        }
    }
    best
}

/// Distance from a 2D point to a segment (JS `distToSegment2D`).
fn dist_to_segment(px: f32, pz: f32, ax: f32, az: f32, bx: f32, bz: f32) -> f32 {
    let (dx, dz) = (bx - ax, bz - az);
    let len_sq = dx * dx + dz * dz;
    if len_sq == 0.0 {
        return ((px - ax).powi(2) + (pz - az).powi(2)).sqrt();
    }
    let t = (((px - ax) * dx + (pz - az) * dz) / len_sq).clamp(0.0, 1.0);
    ((px - (ax + t * dx)).powi(2) + (pz - (az + t * dz)).powi(2)).sqrt()
}

// ─── Simple-style render geometry (JS `simplePlatformGeometry.js`) ───
//
// The visible look: a thin floating shell (top plane + skirt), with L-shaped
// 0.5-WT corner-pillar legs down to the floor when grounded, and stairs built
// from treads + short risers + two sloped stringers + a bridge. RENDER ONLY —
// the collider + nav use the solid boxes (`solid_box`/`stair_run_boxes`), which
// match the JS nav semantics (grounded = solid to floor). Everything is emitted
// double-sided so the thin planes read from both faces.

/// Width of each L-pillar leg, in WT (JS `PILLAR_WIDTH`).
const PILLAR_WIDTH: f32 = 0.5;
/// Riser height as a fraction of the step rise — leaves a visible slit between
/// treads, matching the original (JS `RISER_FRACTION`).
const RISER_FRACTION: f32 = 0.55;

/// Append a platform's simple-style shell (top + skirt + grounded legs) in
/// meters. Port of `buildSimplePlatformGeometry`.
pub fn append_platform_mesh(
    p: &Platform,
    brushes: &[Brush],
    pos: &mut Vec<f32>,
    norm: &mut Vec<f32>,
    idx: &mut Vec<u32>,
) {
    let (x_min, x_max) = (p.x, p.max_x());
    let (z_min, z_max) = (p.z, p.max_z());
    let y_top = p.y;
    let y_bot = p.y - p.thickness;
    let mut q = |a, b, c, d| push_quad_double(pos, norm, idx, a, b, c, d);

    // Top plane (+Y).
    q(
        [x_min, y_top, z_min],
        [x_min, y_top, z_max],
        [x_max, y_top, z_max],
        [x_max, y_top, z_min],
    );
    // Skirt — 4 vertical quads yTop→yBot.
    q(
        [x_min, y_bot, z_min],
        [x_min, y_top, z_min],
        [x_max, y_top, z_min],
        [x_max, y_bot, z_min],
    );
    q(
        [x_max, y_bot, z_max],
        [x_max, y_top, z_max],
        [x_min, y_top, z_max],
        [x_min, y_bot, z_max],
    );
    q(
        [x_min, y_bot, z_max],
        [x_min, y_top, z_max],
        [x_min, y_top, z_min],
        [x_min, y_bot, z_min],
    );
    q(
        [x_max, y_bot, z_min],
        [x_max, y_top, z_min],
        [x_max, y_top, z_max],
        [x_max, y_bot, z_max],
    );

    // Corner pillar legs — grounded only. Each corner contributes up to two
    // perpendicular planes (one per adjacent edge) from yBot down to the floor,
    // skipped when the owning edge is against a CSG wall.
    if !p.grounded {
        return;
    }
    let probe = 1.5;
    let y_probe = y_bot * 0.5;
    let x_min_wall = is_edge_against_wall(p, Edge::XMin, brushes, probe, y_probe);
    let x_max_wall = is_edge_against_wall(p, Edge::XMax, brushes, probe, y_probe);
    let z_min_wall = is_edge_against_wall(p, Edge::ZMin, brushes, probe, y_probe);
    let z_max_wall = is_edge_against_wall(p, Edge::ZMax, brushes, probe, y_probe);

    let y_pillar_top = y_bot;
    let y_pillar_bot = find_floor_y_at(p.center_x(), p.center_z(), y_bot, brushes);
    let mut leg = |ax: f32, az: f32, bx: f32, bz: f32| {
        push_quad_double(
            pos,
            norm,
            idx,
            [ax, y_pillar_bot, az],
            [ax, y_pillar_top, az],
            [bx, y_pillar_top, bz],
            [bx, y_pillar_bot, bz],
        );
    };
    let w = PILLAR_WIDTH;
    // Corner (xMin, zMin).
    if !z_min_wall {
        leg(x_min, z_min, x_min + w, z_min);
    }
    if !x_min_wall {
        leg(x_min, z_min, x_min, z_min + w);
    }
    // Corner (xMax, zMin).
    if !z_min_wall {
        leg(x_max - w, z_min, x_max, z_min);
    }
    if !x_max_wall {
        leg(x_max, z_min, x_max, z_min + w);
    }
    // Corner (xMax, zMax).
    if !z_max_wall {
        leg(x_max - w, z_max, x_max, z_max);
    }
    if !x_max_wall {
        leg(x_max, z_max - w, x_max, z_max);
    }
    // Corner (xMin, zMax).
    if !z_max_wall {
        leg(x_min, z_max, x_min + w, z_max);
    }
    if !x_min_wall {
        leg(x_min, z_max - w, x_min, z_max);
    }
}

/// Append a stair-run's simple-style shell (treads + short risers + two sloped
/// stringers + bridge) in meters. Port of `buildSimpleStairGeometry`.
pub fn append_stair_mesh(
    run: &StairRun,
    from_platform: Option<&Platform>,
    to_platform: Option<&Platform>,
    brushes: &[Brush],
    pos: &mut Vec<f32>,
    norm: &mut Vec<f32>,
    idx: &mut Vec<u32>,
) {
    let Some(g) = resolve_run(run, from_platform, to_platform, brushes) else {
        return;
    };
    let steps = g.steps as f32;
    let tw = |r: f32, y: f32, perp: f32| -> [f32; 3] {
        match g.run_axis {
            RunAxis::X => [r, y, perp],
            RunAxis::Z => [perp, y, r],
        }
    };
    let mut q = |a, b, c, d| push_quad_double(pos, norm, idx, a, b, c, d);

    // Per step: tread + a short front riser (slit visible between treads).
    let riser_height = g.step_rise * RISER_FRACTION;
    for i in 0..g.steps {
        let i = i as f32;
        let r_front = g.top_run + (steps - i) * g.step_run;
        let r_back = g.top_run + (steps - i - 1.0) * g.step_run;
        let step_top = g.stair_base_y + (i + 1.0) * g.step_rise;
        // Tread.
        q(
            tw(r_back, step_top, g.perp_min),
            tw(r_front, step_top, g.perp_min),
            tw(r_front, step_top, g.perp_max),
            tw(r_back, step_top, g.perp_max),
        );
        // Riser (front, shorter than the full rise).
        let riser_bot = step_top - riser_height;
        q(
            tw(r_front, riser_bot, g.perp_min),
            tw(r_front, riser_bot, g.perp_max),
            tw(r_front, step_top, g.perp_max),
            tw(r_front, step_top, g.perp_min),
        );
    }

    // Two sloped stringer boards (perpMin + perpMax), starting one stepRun in
    // from the upper platform's edge.
    let front_run = g.top_run + steps * g.step_run;
    let stringer_back_run = g.top_run + g.step_run;
    let stringer_front_run = front_run;
    let stringer_back_top = g.top_y;
    let stringer_front_top = g.stair_base_y + g.step_rise;
    let board_depth = g.step_rise;
    let stringer_back_bot = stringer_back_top - board_depth;
    let stringer_front_bot = stringer_front_top - board_depth;

    q(
        tw(stringer_front_run, stringer_front_bot, g.perp_min),
        tw(stringer_back_run, stringer_back_bot, g.perp_min),
        tw(stringer_back_run, stringer_back_top, g.perp_min),
        tw(stringer_front_run, stringer_front_top, g.perp_min),
    );
    q(
        tw(stringer_back_run, stringer_back_bot, g.perp_max),
        tw(stringer_front_run, stringer_front_bot, g.perp_max),
        tw(stringer_front_run, stringer_front_top, g.perp_max),
        tw(stringer_back_run, stringer_back_top, g.perp_max),
    );

    // Bridge — fill the small gap under the topmost tread between the upper
    // platform edge and the stringer's start.
    let bridge_run = g.top_run;
    let bridge_front = g.top_run + g.step_run;
    let bridge_top = g.top_y;
    let bridge_bot = g.top_y - board_depth;
    q(
        tw(bridge_front, bridge_bot, g.perp_min),
        tw(bridge_run, bridge_bot, g.perp_min),
        tw(bridge_run, bridge_top, g.perp_min),
        tw(bridge_front, bridge_top, g.perp_min),
    );
    q(
        tw(bridge_run, bridge_bot, g.perp_max),
        tw(bridge_front, bridge_bot, g.perp_max),
        tw(bridge_front, bridge_top, g.perp_max),
        tw(bridge_run, bridge_top, g.perp_max),
    );
}

// ─── Railings (render-only) ──────────────────────────────────────────

/// Append a double-sided quad (WT corners → meters) as two triangles per side,
/// each with its own winding normal, so the render is view-independent.
fn push_quad_double(
    pos: &mut Vec<f32>,
    norm: &mut Vec<f32>,
    idx: &mut Vec<u32>,
    p0: [f32; 3],
    p1: [f32; 3],
    p2: [f32; 3],
    p3: [f32; 3],
) {
    let s = |p: [f32; 3]| [p[0] * WORLD_SCALE, p[1] * WORLD_SCALE, p[2] * WORLD_SCALE];
    let (q0, q1, q2, q3) = (s(p0), s(p1), s(p2), s(p3));
    let n = tri_normal(q0, q1, q2);
    let nb = [-n[0], -n[1], -n[2]];

    let base = (pos.len() / 3) as u32;
    for (p, nn) in [(q0, n), (q1, n), (q2, n), (q3, n)] {
        pos.extend_from_slice(&p);
        norm.extend_from_slice(&nn);
    }
    idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);

    let base = (pos.len() / 3) as u32;
    for (p, nn) in [(q0, nb), (q1, nb), (q2, nb), (q3, nb)] {
        pos.extend_from_slice(&p);
        norm.extend_from_slice(&nn);
    }
    idx.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
}

fn tri_normal(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> [f32; 3] {
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

/// Whether a WT point lies inside any subtract brush's interior (open bounds, to
/// match the JS probe). Doorframe carves count, so railings stay across doorways.
fn point_in_any_subtract(brushes: &[Brush], x: f32, y: f32, z: f32) -> bool {
    brushes.iter().any(|b| {
        b.op == Op::Subtract
            && x > b.x
            && x < b.x + b.w
            && y > b.y
            && y < b.y + b.h
            && z > b.z
            && z < b.z + b.d
    })
}

/// Whether a platform edge is flush against a CSG wall (every probe point past
/// the edge sits in solid shell). Port of `isEdgeAgainstWall`; `probe_dist` and
/// `y_probe` are the JS `opts.probeDist`/`opts.yProbe` (railings use 0.5 / y−0.5;
/// the grounded pillar legs probe wider and lower — 1.5 / mid-leg height).
fn is_edge_against_wall(p: &Platform, edge: Edge, brushes: &[Brush], probe_dist: f32, y_probe: f32) -> bool {
    if brushes.is_empty() {
        return false;
    }
    const SAMPLES: i32 = 5;

    let (edge_pos, edge_min, edge_max, probe_is_x, probe_sign) = match edge {
        Edge::XMin => (p.x, p.z, p.max_z(), true, -1.0),
        Edge::XMax => (p.max_x(), p.z, p.max_z(), true, 1.0),
        Edge::ZMin => (p.z, p.x, p.max_x(), false, -1.0),
        Edge::ZMax => (p.max_z(), p.x, p.max_x(), false, 1.0),
    };
    for i in 0..SAMPLES {
        let t = (i as f32 + 0.5) / SAMPLES as f32;
        let along = edge_min + t * (edge_max - edge_min);
        let (px, pz) = if probe_is_x {
            (edge_pos + probe_sign * probe_dist, along)
        } else {
            (along, edge_pos + probe_sign * probe_dist)
        };
        if point_in_any_subtract(brushes, px, y_probe, pz) {
            return false; // open air past the edge → not fully walled
        }
    }
    true
}

/// Merge a set of [lo, hi] ranges (sorted, overlapping unioned).
fn merge_ranges(mut ranges: Vec<[f32; 2]>) -> Vec<[f32; 2]> {
    ranges.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let mut merged: Vec<[f32; 2]> = Vec::new();
    for r in ranges {
        if let Some(last) = merged.last_mut() {
            if r[0] <= last[1] + 0.001 {
                last[1] = last[1].max(r[1]);
                continue;
            }
        }
        merged.push(r);
    }
    merged
}

/// The 0..1 edge ranges occupied by connecting stair-runs (JS
/// `getStairOccupiedRanges`).
fn stair_occupied_ranges(p: &Platform, edge: Edge, runs: &[StairRun]) -> Vec<[f32; 2]> {
    let edge_len = p.edge_length(edge);
    if edge_len < 0.001 {
        return Vec::new();
    }
    let mut ranges = Vec::new();
    for run in runs {
        let anchor = if run.from_platform == Some(p.id) {
            match run.anchor_from {
                Anchor::Edge { edge: e, offset } if e == edge => Some(offset),
                _ => None,
            }
        } else if run.to_platform == Some(p.id) {
            match run.anchor_to {
                Anchor::Edge { edge: e, offset } if e == edge => Some(offset),
                _ => None,
            }
        } else {
            None
        };
        if let Some(offset) = anchor {
            let half = (run.width / 2.0) / edge_len;
            ranges.push([(offset - half).max(0.0), (offset + half).min(1.0)]);
        }
    }
    ranges
}

/// The 0..1 edge ranges occupied by adjacent platforms at the same height (JS
/// `getAdjacentPlatformOccupiedRanges`).
fn adjacent_platform_ranges(p: &Platform, edge: Edge, all: &[Platform]) -> Vec<[f32; 2]> {
    let edge_len = p.edge_length(edge);
    if edge_len < 0.001 {
        return Vec::new();
    }
    let mut ranges = Vec::new();
    for other in all {
        if other.id == p.id || (other.y - p.y).abs() > 0.01 {
            continue;
        }
        let overlap = match edge {
            Edge::XMin if (other.max_x() - p.x).abs() < 0.01 => {
                span(p.z, p.max_z(), other.z, other.max_z(), p.z, edge_len)
            }
            Edge::XMax if (other.x - p.max_x()).abs() < 0.01 => {
                span(p.z, p.max_z(), other.z, other.max_z(), p.z, edge_len)
            }
            Edge::ZMin if (other.max_z() - p.z).abs() < 0.01 => {
                span(p.x, p.max_x(), other.x, other.max_x(), p.x, edge_len)
            }
            Edge::ZMax if (other.z - p.max_z()).abs() < 0.01 => {
                span(p.x, p.max_x(), other.x, other.max_x(), p.x, edge_len)
            }
            _ => None,
        };
        if let Some(r) = overlap {
            ranges.push(r);
        }
    }
    ranges
}

/// Overlap of [a0,a1] and [b0,b1] expressed as a 0..1 range along the edge.
fn span(a0: f32, a1: f32, b0: f32, b1: f32, base: f32, edge_len: f32) -> Option<[f32; 2]> {
    let lo = a0.max(b0);
    let hi = a1.min(b1);
    if hi > lo + 0.001 {
        Some([(lo - base) / edge_len, (hi - base) / edge_len])
    } else {
        None
    }
}

/// The free (unoccupied) 0..1 segments of an edge (JS `getFreeEdgeSegments`).
fn free_edge_segments(
    p: &Platform,
    edge: Edge,
    runs: &[StairRun],
    all: &[Platform],
) -> Vec<[f32; 2]> {
    let mut occupied = stair_occupied_ranges(p, edge, runs);
    occupied.extend(adjacent_platform_ranges(p, edge, all));
    let merged = merge_ranges(occupied);
    let mut free = Vec::new();
    let mut cursor = 0.0;
    for r in merged {
        if r[0] > cursor + 0.001 {
            free.push([cursor, r[0]]);
        }
        cursor = r[1];
    }
    if cursor < 1.0 - 0.001 {
        free.push([cursor, 1.0]);
    }
    free
}

/// Append a platform's railing quads (render-only). Railings rise on the free
/// segments of each exposed edge (not against a wall, not occupied by a stair or
/// adjacent platform). Port of `buildPlatformRailingGeometry`.
pub fn append_platform_railings(
    p: &Platform,
    runs: &[StairRun],
    all: &[Platform],
    brushes: &[Brush],
    pos: &mut Vec<f32>,
    norm: &mut Vec<f32>,
    idx: &mut Vec<u32>,
) {
    let y_top = p.y;
    let rail_top = y_top + RAILING_HEIGHT;
    for edge in Edge::ALL {
        if is_edge_against_wall(p, edge, brushes, 0.5, p.y - 0.5) {
            continue;
        }
        let (nx, nz) = edge.normal();
        let (start, end) = p.edge_line(edge);
        let edge_len = p.edge_length(edge);
        for seg in free_edge_segments(p, edge, runs, all) {
            let seg_len = (seg[1] - seg[0]) * edge_len;
            if seg_len < 0.1 {
                continue;
            }
            let x0 = start.0 + (end.0 - start.0) * seg[0];
            let z0 = start.1 + (end.1 - start.1) * seg[0];
            let x1 = start.0 + (end.0 - start.0) * seg[1];
            let z1 = start.1 + (end.1 - start.1) * seg[1];
            // Vertical rail plane.
            push_quad_double(
                pos,
                norm,
                idx,
                [x0, y_top, z0],
                [x1, y_top, z1],
                [x1, rail_top, z1],
                [x0, rail_top, z0],
            );
            // Handrail cap (thin horizontal strip).
            let (dx, dz) = (nx * HANDRAIL_DEPTH, nz * HANDRAIL_DEPTH);
            push_quad_double(
                pos,
                norm,
                idx,
                [x0, rail_top, z0],
                [x1, rail_top, z1],
                [x1 + dx, rail_top, z1 + dz],
                [x0 + dx, rail_top, z0 + dz],
            );
        }
    }
}

/// Append a stair-run's side railing quads (render-only): a sloped plane +
/// handrail cap on each side not blocked by a wall. Port of
/// `buildStairRunRailingGeometry`.
pub fn append_stair_railings(
    run: &StairRun,
    from_platform: Option<&Platform>,
    to_platform: Option<&Platform>,
    brushes: &[Brush],
    pos: &mut Vec<f32>,
    norm: &mut Vec<f32>,
    idx: &mut Vec<u32>,
) {
    let Some(g) = resolve_run(run, from_platform, to_platform, brushes) else {
        return;
    };
    let bot_y = g.stair_base_y;
    let top_y = g.top_y;
    let bot_run = g.bottom_run;
    let top_run = g.top_run;

    // to_world maps (run, y, perp) → WT [x,y,z] for the run axis.
    let tw = |r: f32, y: f32, perp: f32| -> [f32; 3] {
        match g.run_axis {
            RunAxis::X => [r, y, perp],
            RunAxis::Z => [perp, y, r],
        }
    };

    for (perp, normal_sign) in [(g.perp_min, -1.0f32), (g.perp_max, 1.0f32)] {
        // Wall probe: if every sample past this side sits in solid, omit it.
        let mut blocked = !brushes.is_empty();
        if blocked {
            const PROBE_DIST: f32 = 0.5;
            const SAMPLES: i32 = 5;
            for i in 0..SAMPLES {
                let t = (i as f32 + 0.5) / SAMPLES as f32;
                let run_pos = bot_run + t * (top_run - bot_run);
                let y_probe = bot_y + t * (top_y - bot_y) + 0.5;
                let perp_probe = perp + normal_sign * PROBE_DIST;
                let (px, pz) = match g.run_axis {
                    RunAxis::X => (run_pos, perp_probe),
                    RunAxis::Z => (perp_probe, run_pos),
                };
                if point_in_any_subtract(brushes, px, y_probe, pz) {
                    blocked = false;
                    break;
                }
            }
        }
        if blocked {
            continue;
        }
        let inset = perp - normal_sign * RAILING_INSET;
        // Sloped side plane.
        push_quad_double(
            pos,
            norm,
            idx,
            tw(bot_run, bot_y, inset),
            tw(top_run, top_y, inset),
            tw(top_run, top_y + RAILING_HEIGHT, inset),
            tw(bot_run, bot_y + RAILING_HEIGHT, inset),
        );
        // Handrail cap following the slope.
        let (nx, nz) = match g.run_axis {
            RunAxis::X => (0.0, normal_sign * HANDRAIL_DEPTH),
            RunAxis::Z => (normal_sign * HANDRAIL_DEPTH, 0.0),
        };
        let p4 = tw(bot_run, bot_y + RAILING_HEIGHT, inset);
        let p5 = tw(top_run, top_y + RAILING_HEIGHT, inset);
        push_quad_double(
            pos,
            norm,
            idx,
            p4,
            p5,
            [p5[0] + nx, p5[1], p5[2] + nz],
            [p4[0] + nx, p4[1], p4[2] + nz],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plat(id: u32, x: f32, y: f32, z: f32) -> Platform {
        Platform {
            id,
            x,
            y,
            z,
            size_x: 4.0,
            size_z: 4.0,
            thickness: 1.0,
            grounded: false,
            railings: false,
        }
    }

    #[test]
    fn platform_solid_box_is_the_slab() {
        let p = plat(1, 2.0, 8.0, 3.0);
        let b = p.solid_box(&[]).unwrap();
        assert_eq!(b, [2.0, 7.0, 3.0, 4.0, 1.0, 4.0]);
    }

    #[test]
    fn grounded_platform_extends_to_zero_without_brushes() {
        let mut p = plat(1, 0.0, 8.0, 0.0);
        p.grounded = true;
        let b = p.solid_box(&[]).unwrap();
        assert_eq!(b[1], 0.0, "grounded underside at world ground");
        assert_eq!(b[4], 8.0, "full height to the top surface");
    }

    #[test]
    fn stair_run_from_platform_edge_to_ground_descends_in_steps() {
        // Platform top at y=8, its xMax edge faces +X; a ground point 8 WT away
        // at y=0. Expect 8 steps (rise 8 / stepHeight 1), each 1 WT taller.
        let p = plat(1, 0.0, 8.0, 0.0);
        let run = StairRun {
            id: 1,
            from_platform: Some(1),
            to_platform: None,
            anchor_from: Anchor::Edge {
                edge: Edge::XMax,
                offset: 0.5,
            },
            anchor_to: Anchor::Ground {
                x: 12.0,
                y: 0.0,
                z: 2.0,
            },
            width: 4.0,
            step_height: 1.0,
            rise_over_run: 1.0,
            grounded: false,
            railings: false,
        };
        let boxes = stair_run_boxes(&run, Some(&p), None, &[]);
        assert_eq!(boxes.len(), 8, "8 steps for a rise of 8");
        // Steps should form an increasing-height staircase from the floor.
        let heights: Vec<f32> = boxes.iter().map(|b| b[4]).collect();
        for w in heights.windows(2) {
            assert!(w[1] > w[0], "each successive step is taller: {heights:?}");
        }
        // Run advances along X (xMax edge normal is +X).
        assert!(
            boxes.iter().all(|b| (b[3] - 1.0).abs() < 1e-3),
            "each step is 1 WT deep along the run axis"
        );
    }
}
