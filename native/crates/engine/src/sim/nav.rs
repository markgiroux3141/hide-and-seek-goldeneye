//! Grid navigation — the WT-cell solid/air world plus standability, A*
//! pathfinding, and line-of-sight. Ported from `src/game/navWorld.js` (grid + A*)
//! and `src/game/navGrid.js` (the voxelizer). Baked **once** at the BUILD→HUNT
//! transition from the frozen geometry, per the plan's freeze-then-bake model.
//!
//! The JS proved grid A* holds at wave scale, so this is the primary nav runtime
//! (Recast stays deferred). The door overlay and extra-solids (CSG-stair treads
//! plus free-standing platforms/stair-runs) ride the same grid — solids folded
//! in at bake time, doors as a live post-bake overlay.

use glam::Vec3;
use std::collections::{BinaryHeap, HashMap};

use crate::geometry::csg_runtime::{Brush, Region, WORLD_SCALE};
use crate::geometry::geom;

/// Agent vertical clearance in WT cells (~1.5 m tall = 6 × 0.25 m).
pub const AGENT_HEIGHT_CELLS: i32 = 6;
/// Max vertical step an agent climbs between adjacent cells (stairs rise 1 WT).
const MAX_STEP: i32 = 1;
/// Torso probe height (m above feet) for the wall-clearance nudge — samples wall
/// solidity at body height rather than at the feet (near the floor). See
/// [`NavWorld::wall_clearance_offset`].
const WALL_PROBE_Y: f32 = 0.75;

// ─── A* instrumentation ──────────────────────────────────────────────────────
// Counters, not a profiler: A* cost is dominated by how many cells it *expands*, and a
// path that does not exist expands every reachable cell before returning `None`. That
// asymmetry is invisible in a wall-clock average and obvious in these numbers, so they
// are cheap enough to leave in permanently (three relaxed atomic adds per query).
static PATH_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PATH_FAILS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PATH_EXPANDED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Zero the A\* counters (call before a measured window).
pub fn reset_path_stats() {
    use std::sync::atomic::Ordering::Relaxed;
    PATH_CALLS.store(0, Relaxed);
    PATH_FAILS.store(0, Relaxed);
    PATH_EXPANDED.store(0, Relaxed);
}

/// Raw A\* counters: `(calls, failures, cells expanded)`.
pub fn path_counts() -> (u64, u64, u64) {
    use std::sync::atomic::Ordering::Relaxed;
    (
        PATH_CALLS.load(Relaxed),
        PATH_FAILS.load(Relaxed),
        PATH_EXPANDED.load(Relaxed),
    )
}

/// One line of A\* accounting: calls, how many found nothing, and cells expanded.
///
/// **The failure count is the interesting one.** A successful path expands cells roughly
/// in proportion to its length; a failed one expands the entire connected region it
/// started in. A few hunters walking toward somewhere they cannot reach will therefore
/// dominate a frame while every other number looks healthy.
pub fn path_stats() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let (calls, fails, exp) = (
        PATH_CALLS.load(Relaxed),
        PATH_FAILS.load(Relaxed),
        PATH_EXPANDED.load(Relaxed),
    );
    format!(
        "{calls} A* calls, {fails} failed ({:.0}%), {exp} cells expanded ({} per call)",
        if calls == 0 { 0.0 } else { fails as f64 / calls as f64 * 100.0 },
        if calls == 0 { 0 } else { exp / calls },
    )
}

/// A* penalty for routing through a **shut but openable** door — large enough to
/// prefer an already-open detour, finite so a hunter will still work a door when that
/// is the only way through. JS `navWorld.DOOR_COST` is 25 on a base move cost of 1;
/// here move costs are scaled ×2 (integer), so the penalty scales to 50 to preserve
/// the ratio. This overlay is the whole thesis: a dynamic obstacle rides the static
/// grid, and opening a door = flipping [`NavDoor::open`], read live by A* — **no
/// re-bake**.
const DOOR_COST: i32 = 50;

/// One door's live overlay state, read every query by [`NavWorld::find_path`] and
/// [`NavWorld::door_blocking`] — so a door opening or shutting needs no re-voxelization.
///
/// `open` and `passable` are deliberately **two** flags, not one. `open` is the live
/// state the door system drives; `passable` is authored and says whether a hunter could
/// *ever* get through — false for a door only the player may open, or a locked one. A
/// shut-but-passable door is merely expensive ([`DOOR_COST`]); a shut-and-impassable one
/// is treated as solid, which is what makes "player-only" a real wall rather than a
/// suggestion.
struct NavDoor {
    open: bool,
    passable: bool,
    /// Centre of the doorway in metres, so a hunter can steer to the door it needs to
    /// work without the caller having to map the index back to an entity.
    center: Vec3,
    /// The doorway's width in metres — an upper bound on how far a hinged panel sweeps
    /// out from the wall when it opens. A hunter waiting for a door that swings *toward*
    /// it has to stand back at least this far or the panel passes through it (hunters
    /// move on the nav grid and ignore the panel's collider entirely).
    clearance: f32,
}

#[inline]
fn m_to_wt(m: f32) -> f32 {
    m / WORLD_SCALE
}
#[inline]
fn wt_to_m(wt: f32) -> f32 {
    wt * WORLD_SCALE
}

/// A baked navigation grid: solid/air cells in WT space plus queries over them.
/// Doors ride the frozen grid as a live overlay (`doors` + `door_grid`), attached
/// after the bake via [`set_doors`](NavWorld::set_doors).
pub struct NavWorld {
    x0: i32,
    y0: i32,
    z0: i32,
    nx: i32,
    ny: i32,
    nz: i32,
    solid: Vec<u8>, // 1 = solid
    /// Door records; `doors[i].open` is read live by A* and door_blocking.
    doors: Vec<NavDoor>,
    /// cellIdx → (doorIndex + 1); 0 = no door. Empty until `set_doors`.
    door_grid: Vec<u16>,
    /// cellIdx → connected-component id (0 = not standable), from [`Self::label_components`].
    ///
    /// **This is what makes "you cannot get there" cheap.** A\* answers *reachable* fast
    /// and *unreachable* catastrophically: with no route it expands every cell in the
    /// region before returning `None`. A hunter walking toward somewhere cut off therefore
    /// pays a full-grid search every `REPATH_INTERVAL`, for as long as it keeps wanting to
    /// go there — which on a real level is the rest of the round. Labelling once at bake
    /// turns that into an integer comparison.
    comp: Vec<u32>,
}

impl NavWorld {
    /// Total cell count (for logging).
    pub fn cell_count(&self) -> usize {
        self.solid.len()
    }

    #[inline]
    fn idx(&self, ix: i32, iy: i32, iz: i32) -> usize {
        ((iy * self.nz + iz) * self.nx + ix) as usize
    }

    #[inline]
    fn in_bounds(&self, ix: i32, iy: i32, iz: i32) -> bool {
        ix >= 0 && iy >= 0 && iz >= 0 && ix < self.nx && iy < self.ny && iz < self.nz
    }

    // ─── Door overlay (JS `navWorld` doors/doorGrid) ─────────────────────

    /// Attach the dynamic door overlay: one record per door brush, plus a grid
    /// marking each door's cells (JS `door.js` `buildDoors` + `nav.setDoors`).
    /// Cells whose center lies inside a door brush's AABB get that door's marker.
    /// Doors start intact. Ordering matches the input slice, so caller-side door
    /// state (panel colliders, hp) stays index-aligned with the nav overlay.
    pub fn set_doors(&mut self, door_brushes: &[Brush]) {
        self.doors = door_brushes
            .iter()
            .map(|b| NavDoor {
                open: false,
                passable: true,
                center: Vec3::new(
                    wt_to_m(b.x + b.w * 0.5),
                    wt_to_m(b.y + b.h * 0.5),
                    wt_to_m(b.z + b.d * 0.5),
                ),
                // The wider horizontal extent is the opening's span; the thin one is
                // the wall. A leaf can never sweep further than the span it fills.
                clearance: wt_to_m(b.w.max(b.d)),
            })
            .collect();
        let mut grid = vec![0u16; self.solid.len()];
        for (i, b) in door_brushes.iter().enumerate() {
            let marker = (i + 1) as u16;
            let ix_lo = ((b.x - self.x0 as f32).floor() as i32).max(0);
            let ix_hi = (((b.x + b.w - self.x0 as f32).ceil() as i32) - 1).min(self.nx - 1);
            let iy_lo = ((b.y - self.y0 as f32).floor() as i32).max(0);
            let iy_hi = (((b.y + b.h - self.y0 as f32).ceil() as i32) - 1).min(self.ny - 1);
            let iz_lo = ((b.z - self.z0 as f32).floor() as i32).max(0);
            let iz_hi = (((b.z + b.d - self.z0 as f32).ceil() as i32) - 1).min(self.nz - 1);
            for iy in iy_lo..=iy_hi {
                let cy = self.y0 as f32 + iy as f32 + 0.5;
                for iz in iz_lo..=iz_hi {
                    let cz = self.z0 as f32 + iz as f32 + 0.5;
                    for ix in ix_lo..=ix_hi {
                        let cx = self.x0 as f32 + ix as f32 + 0.5;
                        if cx >= b.x && cx < b.x + b.w
                            && cy >= b.y && cy < b.y + b.h
                            && cz >= b.z && cz < b.z + b.d
                        {
                            let k = self.idx(ix, iy, iz);
                            grid[k] = marker;
                        }
                    }
                }
            }
        }
        self.door_grid = grid;
    }

    /// Number of attached doors (for logging / tests).
    pub fn door_count(&self) -> usize {
        self.doors.len()
    }

    /// Whether door `i` currently stands open. An out-of-range index reads as open,
    /// so a stale index can never wall a hunter in.
    pub fn door_is_open(&self, i: usize) -> bool {
        self.doors.get(i).map(|d| d.open).unwrap_or(true)
    }

    /// Set door `i`'s live open state. A* and `door_blocking` read it on the next
    /// query, so a door swinging open reroutes paths with no re-bake — the thesis.
    pub fn set_door_open(&mut self, i: usize, open: bool) {
        if let Some(d) = self.doors.get_mut(i) {
            d.open = open;
        }
    }

    /// Whether a hunter could ever get through door `i` (authored, not live state).
    /// Setting this false makes the shut door count as **solid** to pathing rather than
    /// merely expensive — a door only the player can open really does wall hunters out.
    pub fn set_door_passable(&mut self, i: usize, passable: bool) {
        if let Some(d) = self.doors.get_mut(i) {
            d.passable = passable;
        }
    }

    /// Centre of door `i` in metres, for a hunter steering to the door it must work.
    pub fn door_center(&self, i: usize) -> Option<Vec3> {
        self.doors.get(i).map(|d| d.center)
    }

    /// How far back a hunter must stand to be clear of door `i`'s swing, in metres.
    pub fn door_clearance(&self, i: usize) -> Option<f32> {
        self.doors.get(i).map(|d| d.clearance)
    }

    /// Door index at a cell index, or `None` (JS `_doorAtCellIdx`).
    #[inline]
    fn door_at_cell_idx(&self, nk: usize) -> Option<usize> {
        if self.door_grid.is_empty() {
            return None;
        }
        let di = self.door_grid[nk];
        if di == 0 {
            None
        } else {
            Some((di - 1) as usize)
        }
    }

    /// Grid cell index for a meters point, or `None` if out of bounds.
    fn cell_index_meters(&self, m: Vec3) -> Option<usize> {
        let ix = (m_to_wt(m.x) - self.x0 as f32).floor() as i32;
        let iy = (m_to_wt(m.y) - self.y0 as f32).floor() as i32;
        let iz = (m_to_wt(m.z) - self.z0 as f32).floor() as i32;
        if !self.in_bounds(ix, iy, iz) {
            return None;
        }
        Some(self.idx(ix, iy, iz))
    }

    /// The first *intact* door whose cells the segment `from`→`to` passes through,
    /// or `None` (JS `doorBlocking`). The hunter calls this to decide whether to
    /// breach instead of walk. Reads `broken` live.
    pub fn door_blocking(&self, from: Vec3, to: Vec3) -> Option<usize> {
        if self.door_grid.is_empty() {
            return None;
        }
        let d = to - from;
        let dist = d.length();
        let n = (dist / 0.15).ceil().max(1.0) as i32;
        for i in 0..=n {
            let t = i as f32 / n as f32;
            if let Some(ci) = self.cell_index_meters(from + d * t) {
                if let Some(di) = self.door_at_cell_idx(ci) {
                    if !self.doors[di].open {
                        return Some(di);
                    }
                }
            }
        }
        None
    }

    /// Solid at a cell. Out-of-bounds below the world counts as solid ground so
    /// agents on the lowest floor still register a floor beneath them.
    fn is_solid_cell(&self, ix: i32, iy: i32, iz: i32) -> bool {
        if !self.in_bounds(ix, iy, iz) {
            return iy < 0; // below world = solid; sides/top = open
        }
        self.solid[self.idx(ix, iy, iz)] == 1
    }

    /// Solid query in meters (player/collision helpers).
    pub fn is_solid_meters(&self, mx: f32, my: f32, mz: f32) -> bool {
        let ix = (m_to_wt(mx) - self.x0 as f32).floor() as i32;
        let iy = (m_to_wt(my) - self.y0 as f32).floor() as i32;
        let iz = (m_to_wt(mz) - self.z0 as f32).floor() as i32;
        self.is_solid_cell(ix, iy, iz)
    }

    /// A cell is standable if it's air, the cell below is solid, and there is
    /// AGENT_HEIGHT_CELLS of air above for head clearance.
    /// Flood-fill every standable cell into connected components, using the **same
    /// adjacency A\* walks** (4 cardinal, ±[`MAX_STEP`] vertical, no corner-clipping on a
    /// step) but **ignoring doors**.
    ///
    /// Ignoring doors is deliberate and is what keeps the early-out sound. A door can only
    /// ever *remove* connectivity (a shut impassable one is a wall to A\*), never add it —
    /// so two cells in different door-free components can never be joined by any door
    /// state. "Different component" therefore means *definitely* unreachable, and it is
    /// safe to refuse without searching. "Same component" says nothing and still runs A\*.
    fn label_components(&mut self) {
        self.comp = vec![0u32; self.solid.len()];
        let mut next = 1u32;
        let mut stack: Vec<(i32, i32, i32)> = Vec::new();
        for iy in 0..self.ny {
            for iz in 0..self.nz {
                for ix in 0..self.nx {
                    let k = self.idx(ix, iy, iz);
                    if self.comp[k] != 0 || !self.is_standable(ix, iy, iz) {
                        continue;
                    }
                    let id = next;
                    next += 1;
                    self.comp[k] = id;
                    stack.push((ix, iy, iz));
                    while let Some(cur) = stack.pop() {
                        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                            for dy in -MAX_STEP..=MAX_STEP {
                                let (nx_, ny_, nz_) = (cur.0 + dx, cur.1 + dy, cur.2 + dz);
                                if !self.is_standable(nx_, ny_, nz_) {
                                    continue;
                                }
                                if dy != 0 && self.is_solid_cell(cur.0, cur.1 + dy.max(0), cur.2) {
                                    continue;
                                }
                                let nk = self.idx(nx_, ny_, nz_);
                                if self.comp[nk] == 0 {
                                    self.comp[nk] = id;
                                    stack.push((nx_, ny_, nz_));
                                }
                            }
                        }
                    }
                }
            }
        }
        log::info!("nav: {} connected walkable component(s)", next - 1);
    }

    /// Every walkable component as `(id, cell count)`, largest first — the raw material
    /// for a "why can't the hunters get there" report: one big component and a scatter of
    /// tiny ones is a **quantization** story (slivers the bake carved off), while two large
    /// ones is a genuine severed route.
    pub fn component_sizes(&self) -> Vec<(u32, usize)> {
        let mut counts: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
        for &c in &self.comp {
            if c != 0 {
                *counts.entry(c).or_insert(0) += 1;
            }
        }
        let mut out: Vec<(u32, usize)> = counts.into_iter().collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        out
    }

    /// The connected-component id of the standable cell at (or nearest to) `m`, or `None`
    /// where there is no standable cell at all. Two positions with different ids cannot
    /// walk to each other whatever the doors do — see [`Self::label_components`].
    pub fn component_at(&self, m: Vec3) -> Option<u32> {
        let c = self
            .cell_at(m.x, m.y, m.z)
            .or_else(|| self.nearest_cell(m))?;
        self.comp.get(self.idx(c.0, c.1, c.2)).copied().filter(|&id| id != 0)
    }

    fn is_standable(&self, ix: i32, iy: i32, iz: i32) -> bool {
        if self.is_solid_cell(ix, iy, iz) {
            return false;
        }
        if !self.is_solid_cell(ix, iy - 1, iz) {
            return false;
        }
        for h in 1..AGENT_HEIGHT_CELLS {
            if self.is_solid_cell(ix, iy + h, iz) {
                return false;
            }
        }
        true
    }

    /// Line-of-sight: true if no solid cell lies between two meters points.
    pub fn los_clear(&self, from: Vec3, to: Vec3) -> bool {
        let d = to - from;
        let dist = d.length();
        if dist == 0.0 {
            return true;
        }
        let n = (dist / 0.2).ceil().max(1.0) as i32;
        for i in 1..n {
            let t = i as f32 / n as f32;
            let p = from + d * t;
            if self.is_solid_meters(p.x, p.y, p.z) {
                return false;
            }
        }
        true
    }

    /// Feet-height (meters) of the standable floor beneath a horizontal
    /// position, searching downward from `near_y` like [`Self::cell_at`].
    /// `None` if no standable ground sits in that column. Lets a hunter stay
    /// glued to the walking surface instead of leaving its Y frozen mid-air
    /// after a flat (XZ-only) beeline step.
    pub fn floor_height_at(&self, mx: f32, mz: f32, near_y: f32) -> Option<f32> {
        self.cell_at(mx, near_y, mz)
            .map(|(ix, iy, iz)| self.cell_floor_meters(ix, iy, iz).y)
    }

    /// Whether the straight XZ path `from`→`to` stays on continuous, climbable
    /// ground: every sampled column has a standable floor and no two adjacent
    /// samples differ in floor height by more than one step ([`MAX_STEP`]). This
    /// gates the hunter's beeline so it only shortcuts across ground it could
    /// actually walk — never diagonally across an open stairwell or off a
    /// platform edge (where it would clip the cosmetic railing and drop). When
    /// this is false the caller falls back to A*, which steps up tread-by-tread.
    pub fn ground_path_clear(&self, from: Vec3, to: Vec3) -> bool {
        let flat = Vec3::new(to.x - from.x, 0.0, to.z - from.z);
        let dist = flat.length();
        if dist < 1e-4 {
            return true;
        }
        // ~2 samples per WT cell so a one-cell gap can't be stepped over.
        let n = (m_to_wt(dist) * 2.0).ceil().max(1.0) as i32;
        let tol = wt_to_m(MAX_STEP as f32) + 1e-3;
        let margin = wt_to_m(1.0);
        // Search each column from a bit above the straight-line height so the
        // local tread is the first standable cell found (cell_at scans down).
        let mut prev = self.floor_height_at(from.x, from.z, from.y + margin);
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let p = from + flat * t;
            let lerp_y = from.y + (to.y - from.y) * t;
            let cur = self.floor_height_at(p.x, p.z, lerp_y + margin);
            match (prev, cur) {
                (Some(a), Some(b)) if (a - b).abs() <= tol => {}
                _ => return false,
            }
            prev = cur;
        }
        true
    }

    /// World meters at the center of a cell's floor (feet position).
    fn cell_floor_meters(&self, ix: i32, iy: i32, iz: i32) -> Vec3 {
        Vec3::new(
            wt_to_m(self.x0 as f32 + ix as f32 + 0.5),
            wt_to_m(self.y0 as f32 + iy as f32),
            wt_to_m(self.z0 as f32 + iz as f32 + 0.5),
        )
    }

    /// Meters position → the standable cell at/under it (searches a few cells
    /// down so a point slightly above the floor still snaps).
    fn cell_at(&self, mx: f32, my: f32, mz: f32) -> Option<(i32, i32, i32)> {
        let ix = (m_to_wt(mx) - self.x0 as f32).floor() as i32;
        let iz = (m_to_wt(mz) - self.z0 as f32).floor() as i32;
        let iy = (m_to_wt(my) - self.y0 as f32).floor() as i32;
        for dy in 0..=40 {
            if self.is_standable(ix, iy - dy, iz) {
                return Some((ix, iy - dy, iz));
            }
        }
        None
    }

    /// The standable cell nearest a meters position (bounded search). Used to
    /// place the player/enemies on valid ground.
    pub fn nearest_standable(&self, mx: f32, my: f32, mz: f32, max_r: i32) -> Option<Vec3> {
        let cx = (m_to_wt(mx) - self.x0 as f32).floor() as i32;
        let cy = (m_to_wt(my) - self.y0 as f32).floor() as i32;
        let cz = (m_to_wt(mz) - self.z0 as f32).floor() as i32;
        let mut best = None;
        let mut best_d = i32::MAX;
        for iy in (cy - max_r).max(0)..(cy + max_r).min(self.ny) {
            for iz in (cz - max_r).max(0)..(cz + max_r).min(self.nz) {
                for ix in (cx - max_r).max(0)..(cx + max_r).min(self.nx) {
                    if !self.is_standable(ix, iy, iz) {
                        continue;
                    }
                    let d = (ix - cx).pow(2) + (iy - cy).pow(2) + (iz - cz).pow(2);
                    if d < best_d {
                        best_d = d;
                        best = Some((ix, iy, iz));
                    }
                }
            }
        }
        best.map(|(ix, iy, iz)| self.cell_floor_meters(ix, iy, iz))
    }

    /// Every standable cell's floor position (to place enemies far from the player).
    pub fn all_standable(&self) -> Vec<Vec3> {
        let mut out = Vec::new();
        for iy in 0..self.ny {
            for iz in 0..self.nz {
                for ix in 0..self.nx {
                    if self.is_standable(ix, iy, iz) {
                        out.push(self.cell_floor_meters(ix, iy, iz));
                    }
                }
            }
        }
        out
    }

    /// Horizontal clearance around a standable meters position, in WT cells: the
    /// largest ring radius `r` (capped at `cap`) such that every cell within
    /// Chebyshev distance `r` at the same floor level is standable. `0` = the cell
    /// touches a wall/edge. Used to spawn enemies away from walls so the (wider than
    /// one cell) character model doesn't clip into them.
    pub fn wall_clearance_cells(&self, m: Vec3, cap: i32) -> i32 {
        let Some((ix, iy, iz)) = self.cell_at(m.x, m.y, m.z) else {
            return 0;
        };
        let mut r = 0;
        while r < cap {
            let nr = r + 1;
            let mut ring_ok = true;
            'ring: for dz in -nr..=nr {
                for dx in -nr..=nr {
                    // Only the new outer ring (Chebyshev distance == nr).
                    if dx.abs() != nr && dz.abs() != nr {
                        continue;
                    }
                    if !self.is_standable(ix + dx, iy, iz + dz) {
                        ring_ok = false;
                        break 'ring;
                    }
                }
            }
            if !ring_ok {
                break;
            }
            r = nr;
        }
        r
    }

    /// A movement-time **wall-clearance** nudge: given a feet position `m` and the
    /// agent's horizontal body `radius` (m), return an XZ offset that pushes the agent's
    /// centre off any wall geometry its (wider-than-the-nav-line) body would otherwise
    /// clip. Grid nav keeps only the *centre* on walkable ground — the step check is a
    /// thin centre-line ray with no body width — so the model pokes through walls the
    /// centre walks alongside; this restores the body's half-width of clearance.
    ///
    /// **Push, never block:** it only ever nudges *away* from solids, so two opposing
    /// walls (a corridor / doorway narrower than `2·radius`) cancel to a centred pass —
    /// an agent is never stopped from fitting through a door. The nudge is rejected if it
    /// would land the centre inside a wall or off its floor, so it can't shove an agent
    /// into geometry or off a ledge. Probes at torso height ([`WALL_PROBE_Y`]) in the 8
    /// compass directions; the total nudge is capped at one `radius`.
    pub fn wall_clearance_offset(&self, m: Vec3, radius: f32) -> Vec3 {
        if radius <= 0.0 {
            return Vec3::ZERO;
        }
        let y = m.y + WALL_PROBE_Y;
        let steps = 3;
        let mut push = Vec3::ZERO;
        for (dx, dz) in [
            (1, 0), (-1, 0), (0, 1), (0, -1),
            (1, 1), (1, -1), (-1, 1), (-1, -1),
        ] {
            let inv_len = 1.0 / ((dx * dx + dz * dz) as f32).sqrt();
            let dir = Vec3::new(dx as f32 * inv_len, 0.0, dz as f32 * inv_len);
            // Nearest solid along this direction within `radius` → push back by the
            // penetration depth (radius − hit distance).
            for s in 1..=steps {
                let dist = radius * s as f32 / steps as f32;
                let p = m + dir * dist;
                if self.is_solid_meters(p.x, y, p.z) {
                    push -= dir * (radius - dist);
                    break;
                }
            }
        }
        let plen = push.length();
        if plen < 1e-4 {
            return Vec3::ZERO;
        }
        // Cap the total nudge at one radius so a corner can't fling the agent.
        let push = push * (plen.min(radius) / plen);
        let cand = m + push;
        // Reject a nudge that lands in a wall or off the current floor (never shove into
        // geometry or over a ledge — a wall on one side + a drop on the other holds).
        let tol = wt_to_m(1.0);
        let grounded = self
            .floor_height_at(cand.x, cand.z, m.y + tol)
            .map_or(false, |fy| (fy - m.y).abs() <= tol);
        if !grounded || self.is_solid_meters(cand.x, y, cand.z) {
            return Vec3::ZERO;
        }
        push
    }

    /// A* over standable cells (4-connected in x/z, ±MAX_STEP in y for stairs).
    /// Returns meters waypoints (feet positions) from start to goal, or `None`.
    /// Costs are scaled ×2 to stay integer (the only fractional term is the
    /// +0.5 vertical-step penalty).
    pub fn find_path(&self, start_m: Vec3, goal_m: Vec3) -> Option<Vec<Vec3>> {
        let start = self
            .cell_at(start_m.x, start_m.y, start_m.z)
            .or_else(|| self.nearest_cell(start_m))?;
        let goal = self
            .cell_at(goal_m.x, goal_m.y, goal_m.z)
            .or_else(|| self.nearest_cell(goal_m))?;

        // ── The O(1) refusal ──
        // Different walkable components → no route exists, so do not spend a full-region
        // expansion discovering that. This is the fix for a 10 fps playtest: the player
        // spawned on a nav island, four hunters each ran a failing full-grid A* every
        // 0.4 s trying to reach them, and the sim went from 0.7 ms to 140 ms a frame.
        let (ca, cb) = (
            self.comp.get(self.idx(start.0, start.1, start.2)).copied(),
            self.comp.get(self.idx(goal.0, goal.1, goal.2)).copied(),
        );
        if let (Some(a), Some(b)) = (ca, cb) {
            if a != 0 && b != 0 && a != b {
                use std::sync::atomic::Ordering::Relaxed;
                PATH_CALLS.fetch_add(1, Relaxed);
                PATH_FAILS.fetch_add(1, Relaxed);
                return None;
            }
        }

        let goal_key = self.idx(goal.0, goal.1, goal.2);
        let h = |c: (i32, i32, i32)| {
            2 * ((c.0 - goal.0).abs() + (c.1 - goal.1).abs() + (c.2 - goal.2).abs())
        };

        // Min-heap on f; Reverse for min-first. Tiebreak by insertion counter.
        let mut open: BinaryHeap<std::cmp::Reverse<(i32, u32, (i32, i32, i32))>> =
            BinaryHeap::new();
        let mut g_score: HashMap<usize, i32> = HashMap::new();
        let mut came: HashMap<usize, (i32, i32, i32)> = HashMap::new();
        let mut counter: u32 = 0;

        let start_key = self.idx(start.0, start.1, start.2);
        g_score.insert(start_key, 0);
        open.push(std::cmp::Reverse((h(start), counter, start)));

        use std::sync::atomic::Ordering::Relaxed;
        PATH_CALLS.fetch_add(1, Relaxed);
        let mut expanded = 0u64;

        while let Some(std::cmp::Reverse((_f, _, cur))) = open.pop() {
            expanded += 1;
            let ck = self.idx(cur.0, cur.1, cur.2);
            if ck == goal_key {
                PATH_EXPANDED.fetch_add(expanded, Relaxed);
                return Some(self.reconstruct(&came, cur));
            }
            let cur_g = *g_score.get(&ck).unwrap();

            for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                for dy in -MAX_STEP..=MAX_STEP {
                    let (nx, ny, nz) = (cur.0 + dx, cur.1 + dy, cur.2 + dz);
                    if !self.is_standable(nx, ny, nz) {
                        continue;
                    }
                    // Don't clip through a wall corner when stepping up/down.
                    if dy != 0 && self.is_solid_cell(cur.0, cur.1 + dy.max(0), cur.2) {
                        continue;
                    }
                    let nk = self.idx(nx, ny, nz);
                    // Intact-door penalty (read live): prefer an open detour, but
                    // keep the door route finite so a walled-in target stays
                    // reachable by breaching.
                    // A shut door is expensive if a hunter could work it, and simply
                    // impassable if it isn't theirs to open (locked / player-only).
                    let door_penalty = match self.door_at_cell_idx(nk) {
                        Some(di) if !self.doors[di].open => {
                            if !self.doors[di].passable {
                                continue;
                            }
                            DOOR_COST
                        }
                        _ => 0,
                    };
                    let tentative = cur_g + 2 + if dy != 0 { 1 } else { 0 } + door_penalty;
                    if tentative < *g_score.get(&nk).unwrap_or(&i32::MAX) {
                        g_score.insert(nk, tentative);
                        came.insert(nk, cur);
                        counter += 1;
                        let node = (nx, ny, nz);
                        open.push(std::cmp::Reverse((tentative + h(node), counter, node)));
                    }
                }
            }
        }
        // Exhausted the whole connected region without reaching the goal.
        PATH_EXPANDED.fetch_add(expanded, Relaxed);
        PATH_FAILS.fetch_add(1, Relaxed);
        None
    }

    fn nearest_cell(&self, m: Vec3) -> Option<(i32, i32, i32)> {
        // Reuse nearest_standable's search but return the cell indices.
        let cx = (m_to_wt(m.x) - self.x0 as f32).floor() as i32;
        let cy = (m_to_wt(m.y) - self.y0 as f32).floor() as i32;
        let cz = (m_to_wt(m.z) - self.z0 as f32).floor() as i32;
        let mut best = None;
        let mut best_d = i32::MAX;
        let r = 24;
        for iy in (cy - r).max(0)..(cy + r).min(self.ny) {
            for iz in (cz - r).max(0)..(cz + r).min(self.nz) {
                for ix in (cx - r).max(0)..(cx + r).min(self.nx) {
                    if !self.is_standable(ix, iy, iz) {
                        continue;
                    }
                    let d = (ix - cx).pow(2) + (iy - cy).pow(2) + (iz - cz).pow(2);
                    if d < best_d {
                        best_d = d;
                        best = Some((ix, iy, iz));
                    }
                }
            }
        }
        best
    }

    fn reconstruct(
        &self,
        came: &HashMap<usize, (i32, i32, i32)>,
        mut cur: (i32, i32, i32),
    ) -> Vec<Vec3> {
        let mut cells = vec![cur];
        let mut k = self.idx(cur.0, cur.1, cur.2);
        while let Some(&prev) = came.get(&k) {
            cur = prev;
            cells.push(cur);
            k = self.idx(cur.0, cur.1, cur.2);
        }
        cells.reverse();
        cells
            .into_iter()
            .map(|(ix, iy, iz)| self.cell_floor_meters(ix, iy, iz))
            .collect()
    }
}

/// Bake a [`NavWorld`] from the frozen regions. Bounds = union of region shells
/// and every extra solid; each cell is solid if any region's CSG membership says
/// so at its center **or** it falls inside an extra-solid box. The extra solids
/// are (1) the stair treads reconstructed from each region's [`StairDesc`]s and
/// (2) the caller-supplied free-standing structures (platform slabs +
/// stair-run step blocks) — the `collectExtraSolids` port, so grid nav walks
/// geometry the CSG mesh alone doesn't describe. Returns `None` if nothing built.
pub fn bake(regions: &mut [Region], structure_solids: &[[f32; 6]]) -> Option<NavWorld> {
    if regions.is_empty() {
        return None;
    }
    for r in regions.iter_mut() {
        r.refresh_shell();
    }

    // Stair treads + free-standing platform/stair-run boxes — solid volumes that
    // live outside the CSG brush set but that agents must stand on / be blocked by.
    let mut extras: Vec<[f32; 6]> = regions
        .iter()
        .flat_map(|r| r.stairs.iter().flat_map(|s| s.solid_boxes()))
        .collect();
    extras.extend_from_slice(structure_solids);

    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for r in regions.iter() {
        let s = r.shell();
        min = min.min(Vec3::new(s.x, s.y, s.z));
        max = max.max(Vec3::new(s.x + s.w, s.y + s.h, s.z + s.d));
    }
    for b in &extras {
        min = min.min(Vec3::new(b[0], b[1], b[2]));
        max = max.max(Vec3::new(b[0] + b[3], b[1] + b[4], b[2] + b[5]));
    }

    let x0 = min.x.floor() as i32;
    let y0 = min.y.floor() as i32;
    let z0 = min.z.floor() as i32;
    let nx = max.x.ceil() as i32 - x0;
    let ny = max.y.ceil() as i32 - y0;
    let nz = max.z.ceil() as i32 - z0;
    if nx <= 0 || ny <= 0 || nz <= 0 {
        return None;
    }

    let mut solid = vec![0u8; (nx * ny * nz) as usize];
    for iy in 0..ny {
        let wy = y0 as f32 + iy as f32 + 0.5;
        for iz in 0..nz {
            let wz = z0 as f32 + iz as f32 + 0.5;
            for ix in 0..nx {
                let wx = x0 as f32 + ix as f32 + 0.5;
                if regions.iter().any(|r| r.solid_at(wx, wy, wz))
                    || extras.iter().any(|b| geom::point_in_box(b, wx, wy, wz))
                {
                    solid[((iy * nz + iz) * nx + ix) as usize] = 1;
                }
            }
        }
    }

    let mut nav = NavWorld {
        x0,
        y0,
        z0,
        nx,
        ny,
        nz,
        solid,
        doors: Vec::new(),
        door_grid: Vec::new(),
        comp: Vec::new(),
    };
    nav.label_components();
    Some(nav)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::csg_runtime::{Brush, Op, Region};

    fn room() -> Vec<Region> {
        let mut region = Region::new(0);
        region
            .brushes
            .push(Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 24.0, 16.0, 24.0));
        vec![region]
    }

    #[test]
    fn bake_produces_a_walkable_floor() {
        let mut regions = room();
        let nav = bake(&mut regions, &[]).expect("room bakes");
        let stand = nav.all_standable();
        assert!(!stand.is_empty(), "room should have standable floor cells");
        // Floor cells sit at the cavity bottom (y≈0 m).
        assert!(stand.iter().all(|c| c.y.abs() < 0.3));
    }

    /// **An unreachable destination must be refused for free.**
    ///
    /// A\* is fast at finding a route and pathological at proving there isn't one: with no
    /// path it pops every cell in the region before returning `None`. That asymmetry cost a
    /// playtest 10 fps — the player spawned on a walkable island, and four hunters each ran
    /// a doomed full-region search every 0.4 s trying to reach them, taking the sim from
    /// 0.7 ms to 140 ms a frame while the renderer sat idle at 0.7 ms.
    ///
    /// Asserted as **cells expanded**, not wall-clock: the guarantee is algorithmic (an
    /// integer comparison against the baked component labels) and a timing assertion would
    /// be flaky for no extra information.
    #[test]
    fn an_unreachable_goal_is_refused_without_searching() {
        // A room, plus a platform floating two cells above the floor: standable on top,
        // and with no step up to it (MAX_STEP is one cell), so it is its own component.
        let mut regions = room();
        let island: [f32; 6] = [8.0, 3.0, 8.0, 6.0, 1.0, 6.0];
        let nav = bake(&mut regions, &[island]).expect("bake");
        let floor = Vec3::new(0.5, 0.1, 0.5);
        let top = Vec3::new(2.75, 1.05, 2.75); // on top of the island (m; WT ×0.25)
        assert_ne!(
            nav.component_at(floor),
            nav.component_at(top),
            "the arena must actually be two components, or this proves nothing"
        );

        // The control: a reachable goal still searches and still finds a route.
        reset_path_stats();
        assert!(nav.find_path(floor, Vec3::new(5.5, 0.1, 5.5)).is_some());
        let (_, _, reachable_cells) = path_counts();
        assert!(
            reachable_cells > 0,
            "a real path should have expanded cells; got {reachable_cells}"
        );

        // The claim: the unreachable goal is refused having expanded nothing at all.
        reset_path_stats();
        assert!(nav.find_path(floor, top).is_none(), "there is no way up");
        let (calls, fails, expanded) = path_counts();
        assert_eq!((calls, fails), (1, 1), "the refusal is still counted as a call");
        assert_eq!(
            expanded, 0,
            "refusing an unreachable goal expanded {expanded} cells — it should search none"
        );
    }

    #[test]
    fn path_crosses_the_room() {
        let mut regions = room();
        let nav = bake(&mut regions, &[]).expect("bake");
        // Opposite corners of the room interior, in meters.
        let a = Vec3::new(0.5, 0.1, 0.5);
        let b = Vec3::new(5.5, 0.1, 5.5);
        let path = nav.find_path(a, b).expect("a path should exist across open room");
        assert!(path.len() >= 2);
        // Endpoints land near the requested corners.
        assert!((path.first().unwrap().distance(a)) < 1.0);
        assert!((path.last().unwrap().distance(b)) < 1.0);
    }

    #[test]
    fn door_overlay_is_read_live() {
        // Bake a plain room, then overlay a full-width door slab at x≈12 WT. The
        // overlay is attached AFTER the bake and mutated in place — the thesis:
        // no re-voxelization when the door's state changes.
        let mut regions = room();
        let mut nav = bake(&mut regions, &[]).expect("bake");

        let mut door = Brush::new(1, Op::Subtract, 12.0, 0.0, 0.0, 1.0, 7.0, 24.0);
        door.door = true;
        nav.set_doors(&[door]);
        assert_eq!(nav.door_count(), 1);

        // A segment crossing the door plane (left cell → right cell), at feet height.
        let from = Vec3::new(11.5 * WORLD_SCALE, 0.1, 5.0 * WORLD_SCALE);
        let to = Vec3::new(13.5 * WORLD_SCALE, 0.1, 5.0 * WORLD_SCALE);

        // Shut: the door blocks the segment and A* still finds the (only) route
        // through it — the door slab spans the room, so working it is the only way.
        assert_eq!(nav.door_blocking(from, to), Some(0), "shut door blocks segment");
        assert!(nav.find_path(from, to).is_some(), "path exists through the door");
        assert!(!nav.door_is_open(0));
        // Its centre is reported in metres, so a hunter can steer to it.
        let c = nav.door_center(0).expect("door has a centre");
        assert!((c.x - 12.5 * WORLD_SCALE).abs() < 1e-4, "centre of the slab in metres");

        // Opening = flip the flag; the same overlay, no re-bake.
        nav.set_door_open(0, true);
        assert!(nav.door_is_open(0));
        assert_eq!(nav.door_blocking(from, to), None, "open door no longer blocks");
        assert!(nav.find_path(from, to).is_some(), "path still exists once open");
    }

    /// A shut door a hunter may never open is **solid** to pathing, not merely
    /// expensive — that is what makes an authored "player only" door a real wall
    /// rather than a suggestion. Re-opening it restores the route.
    #[test]
    fn an_impassable_shut_door_walls_hunters_out() {
        let mut regions = room();
        let mut nav = bake(&mut regions, &[]).expect("bake");
        let mut door = Brush::new(1, Op::Subtract, 12.0, 0.0, 0.0, 1.0, 7.0, 24.0);
        door.door = true;
        nav.set_doors(&[door]);

        let from = Vec3::new(11.5 * WORLD_SCALE, 0.1, 5.0 * WORLD_SCALE);
        let to = Vec3::new(13.5 * WORLD_SCALE, 0.1, 5.0 * WORLD_SCALE);
        assert!(nav.find_path(from, to).is_some(), "passable while shut: costly route");

        nav.set_door_passable(0, false);
        assert!(
            nav.find_path(from, to).is_none(),
            "a door that is not theirs to open blocks the only route entirely"
        );

        // Live state still wins: if it is opened (by the player), the route returns.
        nav.set_door_open(0, true);
        assert!(nav.find_path(from, to).is_some(), "an open door is passable regardless");
    }

    #[test]
    fn ground_path_clear_gates_the_beeline() {
        // Room floor at y≈0, plus a raised platform slab (top at 8 WT) in the
        // middle, solid down to the floor.
        let mut regions = room();
        let slab = [8.0, 0.0, 8.0, 8.0, 8.0, 8.0]; // WT [x,y,z,w,h,d]
        let nav = bake(&mut regions, &[slab]).expect("bake");

        // Flat floor → continuous, climbable ground (beeline allowed).
        let a = Vec3::new(2.0 * WORLD_SCALE, 0.1, 2.0 * WORLD_SCALE);
        let b = Vec3::new(5.0 * WORLD_SCALE, 0.1, 2.0 * WORLD_SCALE);
        assert!(nav.ground_path_clear(a, b), "flat floor is continuous ground");

        // Floor → top of the ledge: the floor height jumps a full storey, far
        // more than one step, so the straight line is NOT walkable — the hunter
        // must take A* (stairs) instead of beelining off/into the ledge.
        let low = Vec3::new(2.0 * WORLD_SCALE, 0.1, 12.0 * WORLD_SCALE);
        let high = Vec3::new(12.0 * WORLD_SCALE, 8.0 * WORLD_SCALE + 0.1, 12.0 * WORLD_SCALE);
        assert!(
            !nav.ground_path_clear(low, high),
            "a line across a cliff edge is not continuous ground"
        );
    }

    #[test]
    fn wall_clearance_pushes_off_walls_but_not_in_the_open() {
        let mut regions = room(); // 24×16×24 WT cavity → interior walls at x,z ≈ 0 and 6 m
        let nav = bake(&mut regions, &[]).expect("bake");
        let radius = 0.22;

        // Deep in the open middle of the room → no wall within radius → no nudge.
        let mid = Vec3::new(3.0, 0.1, 3.0);
        assert!(
            nav.wall_clearance_offset(mid, radius).length() < 1e-4,
            "open floor should not be nudged"
        );

        // Hard against the −X wall (interior face at x≈0): the nudge must push in +X
        // (away from the wall) and not move much in Z.
        let by_wall = Vec3::new(0.05, 0.1, 3.0);
        let off = nav.wall_clearance_offset(by_wall, radius);
        assert!(off.x > 0.0, "should push away from the −X wall (+X), got {off:?}");
        assert!(off.z.abs() < off.x + 1e-3, "push should be mostly along X");
        // And the nudged point is off the wall (not shoved into geometry).
        let moved = by_wall + off;
        assert!(!nav.is_solid_meters(moved.x, moved.y + WALL_PROBE_Y, moved.z));
    }

    #[test]
    fn los_blocked_by_the_wall() {
        let mut regions = room();
        let nav = bake(&mut regions, &[]).expect("bake");
        // A point inside vs. a point well outside the room (through the wall).
        let inside = Vec3::new(3.0, 1.0, 3.0);
        let outside = Vec3::new(3.0, 1.0, -5.0);
        assert!(!nav.los_clear(inside, outside), "wall should block LOS");
        // Two interior points see each other.
        assert!(nav.los_clear(Vec3::new(1.0, 1.0, 1.0), Vec3::new(5.0, 1.0, 5.0)));
    }
}
