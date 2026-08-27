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
/// Max vertical step **inside authored stair geometry** (2 WT = 0.5 m).
///
/// A relaxation, and a deliberately local one. A stair-run's steps are laid out as
/// `step_run = total_run / steps`, so a run steeper than 45° gets treads *shallower than
/// one cell* — and then some tread has no cell centre in it at all, loses its standable
/// cell, and the walkable strip skips it. The gap between the cells that survive is two
/// cells, and with a flat [`MAX_STEP`] the grid severs the flight in the middle. On
/// `slot1` that was 3,488 cells (15% of the level) behind two staircases the *player*
/// walks up at 48°, comfortably inside their 50° slope limit.
///
/// Raising [`MAX_STEP`] globally would fix it by letting every hunter everywhere climb
/// half a metre — twice the player's autostep — to paper over a stair-sampling artifact.
/// This applies only where the level actually contains stairs (see the `stair` grid), so
/// it cannot be used to scale a wall, and hunters take a skipped tread as one 0.5 m step
/// on a staircase, which is what a person does anyway.
///
/// A run steeper than 2:1 would skip *two* treads and need 3 here. Nothing silently
/// breaks if that happens — the NAV tab reports the island exactly as it did before, with
/// a "0.75 m above" gap line.
const STAIR_STEP: i32 = 2;
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
    /// cellIdx → 1 where the cell is inside (or standing on) authored stair geometry.
    /// The only place [`STAIR_STEP`] applies instead of [`MAX_STEP`]. Empty = no stairs,
    /// which reads as all-zero.
    stair: Vec<u8>,
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

// ─── Validation findings (the BUILD NAV tab) ─────────────────────────────────
// Types returned by the "explain this grid" queries below. They exist because the
// authoring question is never "is there a path" — A* answers that — but *why not*,
// and every answer needs its own shape.

/// The closest approach between two walkable components: the cell pair, and the gap
/// split into its **flat** and **vertical** parts.
///
/// That split is the whole diagnosis, and it is why this is not one distance.
/// `flat 0.25, rise 0.50` is a single step half a metre too tall — fix the geometry or
/// the step limit. `flat 4.20, rise 0.00` is a pit the author meant to be there — build
/// a bridge. A 3D distance of 0.56 vs 4.20 cannot tell those two apart, and the fixes
/// have nothing in common.
#[derive(Clone, Copy, Debug)]
pub struct NavGap {
    /// A cell on the first component's side of the gap (the island).
    pub from: Vec3,
    /// The nearest cell on the second's (the level).
    pub to: Vec3,
    /// Horizontal (XZ) distance between them, metres.
    pub flat: f32,
    /// Height of `to` above `from`, metres. Negative = the island is the higher side.
    pub rise: f32,
}

/// A standable cell in a corridor narrower than an agent's body.
///
/// **Not a connectivity fault.** A\* will happily route through it — the grid is a
/// 4-connected line of cell *centres* with no body width — and then the mover's
/// wall-clearance nudge has to fit a real body down it. That is a different bug with a
/// different fix, so it gets a different finding.
#[derive(Clone, Copy, Debug)]
pub struct NavPinch {
    pub pos: Vec3,
    /// Free width (metres) across the corridor at its narrowest axis.
    pub width: f32,
}

/// A step up that the player can take and a hunter cannot: taller than [`MAX_STEP`],
/// inside the height a player clears by walking or jumping.
///
/// Reported as **information, not an error**. Some of these are the level being wrong;
/// some are the *hunter* being less mobile than the player, which is a legitimate design
/// state. Only `joins` distinguishes them.
#[derive(Clone, Copy, Debug)]
pub struct NavClimb {
    /// The lower cell.
    pub from: Vec3,
    /// The cell above it that a hunter cannot reach from there.
    pub to: Vec3,
    /// Height of the step, metres.
    pub rise: f32,
    /// The two component ids this climb would join, when they differ — i.e. this edge
    /// is *why* something is cut off, and closing it reconnects the level. `None` when
    /// both ends are already the same component (a shortcut, not a severed route).
    pub joins: Option<(u32, u32)>,
}

/// Nav cell size in metres — one WT. Callers reporting on the grid have to be able to
/// say "0.5 m" rather than "2 cells".
pub const fn cell_size_m() -> f32 {
    WORLD_SCALE
}

/// The tallest step a hunter climbs, in metres ([`MAX_STEP`] cells). The player's
/// autostep and jump apex are the numbers this wants comparing against.
pub const fn max_step_m() -> f32 {
    MAX_STEP as f32 * WORLD_SCALE
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

    /// How many grid cells door `i` actually marks.
    ///
    /// **Zero means the door does not exist as far as pathing is concerned** — hunters
    /// walk through it and nothing in the door system ever fires. That is a live hazard
    /// rather than a hypothetical: [`Self::set_doors`] marks a cell when the cell's
    /// *centre* falls inside the panel's box, and a door panel is thin — a few
    /// centimetres against a 0.25 m cell. Whether a given door catches a centre or slips
    /// between two is decided by where the author happened to place it, which is why the
    /// symptom is "they walk through doors *sometimes*".
    pub fn door_cells(&self, i: usize) -> usize {
        if self.door_grid.is_empty() {
            return 0;
        }
        let marker = (i + 1) as u16;
        self.door_grid.iter().filter(|&&m| m == marker).count()
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

    /// The door whose cells contain `m`, if any.
    ///
    /// The question "am I standing *in* the doorway" — which is different from "is a door
    /// in my way", and has to be asked separately: an agent refusing to enter a shut
    /// door's cells is right, and an agent refusing to *leave* them is entombed the moment
    /// a door auto-closes on it.
    pub fn door_at(&self, m: Vec3) -> Option<usize> {
        self.cell_index_meters(m).and_then(|ci| self.door_at_cell_idx(ci))
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
    /// adjacency A\* walks** ([`Self::can_step`], shared with `find_path`) but **ignoring
    /// doors**.
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
                            for dy in -STAIR_STEP..=STAIR_STEP {
                                let n = (cur.0 + dx, cur.1 + dy, cur.2 + dz);
                                if !self.in_bounds(n.0, n.1, n.2) || !self.can_step(cur, n) {
                                    continue;
                                }
                                let nk = self.idx(n.0, n.1, n.2);
                                if self.comp[nk] == 0 {
                                    self.comp[nk] = id;
                                    stack.push(n);
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

    /// Whether this cell is inside (or standing on) authored stair geometry.
    #[inline]
    fn is_stair_cell(&self, ix: i32, iy: i32, iz: i32) -> bool {
        if self.stair.is_empty() || !self.in_bounds(ix, iy, iz) {
            return false;
        }
        self.stair[self.idx(ix, iy, iz)] == 1
    }

    /// The tallest step allowed between two cells: [`STAIR_STEP`] when **both** ends are
    /// stair cells, otherwise [`MAX_STEP`].
    ///
    /// Both ends, not either: a hunter beside a staircase must not get half a metre of
    /// climb onto unrelated floor just for standing next to it.
    #[inline]
    fn step_limit(&self, a: (i32, i32, i32), b: (i32, i32, i32)) -> i32 {
        if self.is_stair_cell(a.0, a.1, a.2) && self.is_stair_cell(b.0, b.1, b.2) {
            STAIR_STEP
        } else {
            MAX_STEP
        }
    }

    /// **The single definition of nav adjacency**: can an agent standing at `a` move to
    /// the neighbouring cell `b`?
    ///
    /// One function on purpose. [`Self::label_components`] and [`Self::find_path`] have to
    /// agree *exactly*, because the O(1) unreachable refusal in `find_path` trusts the
    /// component labels: if labelling were ever stricter than the search, the refusal
    /// would deny a route that exists. They were two copies of this rule until the stair
    /// step gave them a third thing to disagree about.
    fn can_step(&self, a: (i32, i32, i32), b: (i32, i32, i32)) -> bool {
        if !self.is_standable(b.0, b.1, b.2) {
            return false;
        }
        let dy = b.1 - a.1;
        if dy.abs() > self.step_limit(a, b) {
            return false;
        }
        // Climbing: every cell of the agent's own column between here and there must be
        // clear, or it would clip up through a wall corner. (Descending needs no check —
        // the cell it leaves is already air.)
        for k in 1..=dy.max(0) {
            if self.is_solid_cell(a.0, a.1 + k, a.2) {
                return false;
            }
        }
        true
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

    /// Raw (unclamped, unchecked) cell coordinates for a metres point.
    #[inline]
    fn cell_coords(&self, m: Vec3) -> (i32, i32, i32) {
        (
            (m_to_wt(m.x) - self.x0 as f32).floor() as i32,
            (m_to_wt(m.y) - self.y0 as f32).floor() as i32,
            (m_to_wt(m.z) - self.z0 as f32).floor() as i32,
        )
    }

    /// The walking surface an agent whose feet are at `feet` should be standing on — the
    /// floor it can actually **step onto** from there, looking as far *up* as the grid's
    /// own step limit allows before scanning down.
    ///
    /// This exists because [`Self::floor_height_at`] only ever looks **downward** from the
    /// height it is given, and every caller was giving it one step's worth of headroom.
    /// Inside stair geometry the grid allows [`STAIR_STEP`], so a tread half a metre up is
    /// a legal move that the surface probe could not see: the agent stepped into the riser,
    /// its feet stayed at the old height, and — since a path waypoint is matched on 3D
    /// distance — the waypoint 0.5 m above could never be reached. It ground to a halt
    /// mid-flight on exactly the staircases the relaxed step limit had just opened up.
    ///
    /// So the reach has to be read from the same place the step limit is, or the mover and
    /// the grid disagree about what "walkable" means. Outside stairs this is identical to
    /// probing one step up.
    pub fn walk_surface_at(&self, feet: Vec3) -> Option<f32> {
        self.floor_height_at(feet.x, feet.z, feet.y + self.step_reach_at(feet) + 1e-3)
    }

    /// How far up an agent standing here may step, in metres: one cell normally,
    /// [`STAIR_STEP`] inside stair geometry.
    ///
    /// **The mover's single source for this number.** It was written out by hand in two
    /// places — the floor snap and [`Self::ground_path_clear`] — and a hunter needs *both*
    /// to agree with the grid or it stalls: the snap decides whether its feet find the next
    /// tread, and the ground check decides whether the step is permitted at all. Fixing
    /// only one of them moves the stall rather than curing it.
    ///
    /// The tag is read off the raw cell, which may well be *solid* — that is the case that
    /// matters. Mid-step into a riser the agent is inside the stair's own volume, and stair
    /// volumes are tagged through their solid interior precisely so this stays answerable
    /// there.
    fn step_reach_at(&self, m: Vec3) -> f32 {
        let (ix, iy, iz) = self.cell_coords(m);
        let cells = if self.is_stair_cell(ix, iy, iz) {
            STAIR_STEP
        } else {
            MAX_STEP
        };
        wt_to_m(cells as f32)
    }

    /// Whether the straight XZ path `from`→`to` stays on continuous, climbable
    /// ground: every sampled column has a standable floor and no two adjacent
    /// samples differ in floor height by more than a step ([`Self::step_reach_at`], so
    /// [`STAIR_STEP`] on a staircase). This gates the hunter's beeline so it only
    /// shortcuts across ground it could actually walk — never diagonally across an open
    /// stairwell or off a platform edge (where it would clip the cosmetic railing and
    /// drop) — and, via `try_step`, gates **every** committed move.
    ///
    /// That second job is why the tolerance has to be the grid's own. A\* will route up a
    /// flight whose treads came out shallower than a cell, taking one riser two cells at a
    /// time; a flat one-cell tolerance here vetoes that step, so the hunter arrives at the
    /// riser and freezes against it with a valid path in hand.
    pub fn ground_path_clear(&self, from: Vec3, to: Vec3) -> bool {
        let flat = Vec3::new(to.x - from.x, 0.0, to.z - from.z);
        let dist = flat.length();
        if dist < 1e-4 {
            return true;
        }
        // ~2 samples per WT cell so a one-cell gap can't be stepped over.
        let n = (m_to_wt(dist) * 2.0).ceil().max(1.0) as i32;
        // Each column is probed from a step above the straight-line height, so the local
        // tread is the first standable cell found (cell_at scans down).
        let mut prev = self.walk_surface_at(from);
        let mut prev_reach = self.step_reach_at(from);
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let p = from + flat * t;
            let here = Vec3::new(p.x, from.y + (to.y - from.y) * t, p.z);
            let cur = self.walk_surface_at(here);
            let reach = self.step_reach_at(here);
            match (prev, cur) {
                // The looser of the two ends: stepping *off* a staircase is as much a
                // stair move as stepping onto one.
                (Some(a), Some(b)) if (a - b).abs() <= prev_reach.max(reach) + 1e-3 => {}
                _ => return false,
            }
            prev = cur;
            prev_reach = reach;
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

    // ─── Validation queries (the BUILD NAV tab) ──────────────────────────
    // One-off queries answering "why can't the hunters get there", run on a button
    // press rather than in a frame. They therefore favour being exactly right over
    // being fast — see [`Self::nearest_gap`], which is a brute-force closest pair.

    /// Every standable cell as `(floor position, component id)` — the overlay's raw
    /// material. Reads the baked labels rather than re-testing standability, so it
    /// shows exactly what A\* is walking.
    pub fn standable_with_components(&self) -> Vec<(Vec3, u32)> {
        let mut out = Vec::new();
        for iy in 0..self.ny {
            for iz in 0..self.nz {
                for ix in 0..self.nx {
                    let c = self.comp[self.idx(ix, iy, iz)];
                    if c != 0 {
                        out.push((self.cell_floor_meters(ix, iy, iz), c));
                    }
                }
            }
        }
        out
    }

    /// The largest walkable component — "the level", as against the islands. Every
    /// other finding is phrased relative to this one, because a report of "4 components"
    /// with no main one named is a fact rather than a diagnosis.
    pub fn main_component(&self) -> Option<u32> {
        self.component_sizes().first().map(|(id, _)| *id)
    }

    /// Component id at a cell, `0` outside the grid or off the walkable set.
    ///
    /// The bounds check is not decoration: [`Self::is_standable`] answers *true* for a
    /// cell just outside the baked volume (out-of-bounds below the world reads as solid
    /// ground), so a neighbour probe can legitimately land off-grid, and `idx` would
    /// silently alias it onto an unrelated cell.
    #[inline]
    fn comp_at_cell(&self, ix: i32, iy: i32, iz: i32) -> u32 {
        if !self.in_bounds(ix, iy, iz) {
            return 0;
        }
        self.comp[self.idx(ix, iy, iz)]
    }

    /// Every cell belonging to one component.
    fn cells_of(&self, id: u32) -> Vec<(i32, i32, i32)> {
        let mut out = Vec::new();
        for iy in 0..self.ny {
            for iz in 0..self.nz {
                for ix in 0..self.nx {
                    if self.comp[self.idx(ix, iy, iz)] == id {
                        out.push((ix, iy, iz));
                    }
                }
            }
        }
        out
    }

    /// The narrowest place between two components, as a [`NavGap`].
    ///
    /// A brute-force closest pair — every cell of `from` against every cell of `to`.
    /// On the shipping level that is a few thousand against twenty thousand, so tens of
    /// millions of integer distances: ~0.1 s, against a bake that already costs 0.5 s,
    /// and it runs on a button. Pruning to boundary cells would be faster and **wrong**
    /// in the one case that matters most — a platform directly above a floor, where the
    /// closest cell on the lower side is an interior one — so it stays exhaustive.
    pub fn nearest_gap(&self, from: u32, to: u32) -> Option<NavGap> {
        let a = self.cells_of(from);
        let b = self.cells_of(to);
        if a.is_empty() || b.is_empty() {
            return None;
        }
        let mut best: Option<((i32, i32, i32), (i32, i32, i32), i64)> = None;
        for &p in &a {
            for &q in &b {
                let (dx, dy, dz) = (
                    (q.0 - p.0) as i64,
                    (q.1 - p.1) as i64,
                    (q.2 - p.2) as i64,
                );
                let d = dx * dx + dy * dy + dz * dz;
                if best.is_none_or(|(_, _, bd)| d < bd) {
                    best = Some((p, q, d));
                }
            }
        }
        let (p, q, _) = best?;
        let pm = self.cell_floor_meters(p.0, p.1, p.2);
        let qm = self.cell_floor_meters(q.0, q.1, q.2);
        Some(NavGap {
            from: pm,
            to: qm,
            flat: Vec3::new(qm.x - pm.x, 0.0, qm.z - pm.z).length(),
            rise: qm.y - pm.y,
        })
    }

    /// The component this one comes **closest to** — any component, not necessarily the
    /// main one — and the gap between them.
    ///
    /// Nearest-to-*main* is the intuitive query and it is misleading, measurably so. On
    /// the shipping level a 1,380-cell area sits 3.5 m below the main component, which
    /// reads as a drop nobody can fix; its actual nearest neighbour is a 32-cell sliver
    /// half a metre away, and that sliver is half a metre from the level. Two 0.5 m steps,
    /// not a 3.5 m cliff — and only this query can say so.
    ///
    /// Same brute force as [`Self::nearest_gap`], against every other component at once
    /// (one pass to bucket them, so the cost is `|this| × |everything else|` rather than
    /// a grid scan per pair).
    pub fn nearest_neighbour_gap(&self, id: u32) -> Option<(u32, NavGap)> {
        let mine = self.cells_of(id);
        if mine.is_empty() {
            return None;
        }
        let mut others: Vec<(i32, i32, i32, u32)> = Vec::new();
        for iy in 0..self.ny {
            for iz in 0..self.nz {
                for ix in 0..self.nx {
                    let c = self.comp[self.idx(ix, iy, iz)];
                    if c != 0 && c != id {
                        others.push((ix, iy, iz, c));
                    }
                }
            }
        }
        let mut best: Option<((i32, i32, i32), (i32, i32, i32), u32, i64)> = None;
        for &p in &mine {
            for &(qx, qy, qz, c) in &others {
                let (dx, dy, dz) = ((qx - p.0) as i64, (qy - p.1) as i64, (qz - p.2) as i64);
                let d = dx * dx + dy * dy + dz * dz;
                if best.is_none_or(|(_, _, _, bd)| d < bd) {
                    best = Some((p, (qx, qy, qz), c, d));
                }
            }
        }
        let (p, q, c, _) = best?;
        let pm = self.cell_floor_meters(p.0, p.1, p.2);
        let qm = self.cell_floor_meters(q.0, q.1, q.2);
        Some((
            c,
            NavGap {
                from: pm,
                to: qm,
                flat: Vec3::new(qm.x - pm.x, 0.0, qm.z - pm.z).length(),
                rise: qm.y - pm.y,
            },
        ))
    }

    /// How many cells you can **walk** from `(ix, iy, iz)` in one XZ direction before the
    /// floor runs out, stopping at `cap`. Excludes the starting cell.
    ///
    /// It follows the surface up and down within [`MAX_STEP`], using A\*'s own adjacency,
    /// and that is not a refinement — it is the difference between the query meaning
    /// anything and not. Measured at a *fixed* height, every step of every staircase is a
    /// one-cell-deep strip (a tread is exactly 1 WT deep and the next one is 1 WT up), so
    /// a fixed-height version reports the entire stairwell as a 0.25 m corridor: on the
    /// shipping level that was 1,423 cells of pure false positive, swamping the handful of
    /// real ones.
    fn free_run(&self, ix: i32, iy: i32, iz: i32, dx: i32, dz: i32, cap: i32) -> i32 {
        let mut n = 0;
        let mut y = iy;
        while n < cap {
            let (nx_, nz_) = (ix + dx * (n + 1), iz + dz * (n + 1));
            // Prefer level ground, then the smallest step either way — same order of
            // preference a walking agent has.
            let step = (0..=STAIR_STEP)
                .flat_map(|d| if d == 0 { vec![0] } else { vec![d, -d] })
                .find(|&dy| self.can_step((ix + dx * n, y, iz + dz * n), (nx_, y + dy, nz_)));
            match step {
                Some(dy) => {
                    y += dy;
                    n += 1;
                }
                None => break,
            }
        }
        n
    }

    /// Every standable cell whose corridor is narrower than `min_width_m` across.
    ///
    /// Width is the **walkable span** through that cell (floor running out on both
    /// sides), taken along whichever of X/Z is narrower — not the distance to the nearest
    /// wall. That distinction is what keeps an open room quiet: a cell pressed against
    /// one wall of a big room still has the whole room's span in both axes, and only a
    /// cell with walls close on *both* sides is a pinch. And the span follows the surface
    /// up and down, so a staircase is a corridor as long as the flight, not a file of
    /// one-tread slivers — see [`Self::free_run`].
    ///
    /// Runs are capped at the width being tested, so the cost is per-cell constant.
    pub fn pinch_points(&self, min_width_m: f32) -> Vec<NavPinch> {
        let need = (min_width_m / WORLD_SCALE).ceil().max(1.0) as i32;
        let mut out = Vec::new();
        for iy in 0..self.ny {
            for iz in 0..self.nz {
                for ix in 0..self.nx {
                    if self.comp[self.idx(ix, iy, iz)] == 0 {
                        continue;
                    }
                    let wx = 1
                        + self.free_run(ix, iy, iz, 1, 0, need)
                        + self.free_run(ix, iy, iz, -1, 0, need);
                    let wz = 1
                        + self.free_run(ix, iy, iz, 0, 1, need)
                        + self.free_run(ix, iy, iz, 0, -1, need);
                    let width = wx.min(wz) as f32 * WORLD_SCALE;
                    if width < min_width_m {
                        out.push(NavPinch {
                            pos: self.cell_floor_meters(ix, iy, iz),
                            width,
                        });
                    }
                }
            }
        }
        out
    }

    /// Every adjacent step taller than a hunter can climb but no taller than
    /// `max_rise_m` — the band where the player walks or jumps up and the hunter is
    /// looking at a wall.
    ///
    /// Only reported where the neighbouring column has **no** cell a hunter could reach
    /// normally: if there is already a legal step there, the taller ledge beside it is a
    /// shortcut, not a severed route, and listing it would bury the real findings.
    pub fn overclimb_edges(&self, max_rise_m: f32) -> Vec<NavClimb> {
        let hi = (max_rise_m / WORLD_SCALE).floor() as i32;
        if hi <= MAX_STEP {
            return Vec::new();
        }
        let mut out = Vec::new();
        for iy in 0..self.ny {
            for iz in 0..self.nz {
                for ix in 0..self.nx {
                    let ca = self.comp[self.idx(ix, iy, iz)];
                    if ca == 0 {
                        continue;
                    }
                    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                        let (nx_, nz_) = (ix + dx, iz + dz);
                        // Already walkable in this direction → nothing to report. Uses
                        // `can_step`, so a legal stair step is not listed as a climb the
                        // hunter cannot make.
                        if (-STAIR_STEP..=STAIR_STEP)
                            .any(|dy| self.can_step((ix, iy, iz), (nx_, iy + dy, nz_)))
                        {
                            continue;
                        }
                        // The first floor above the step limit, if any.
                        for dy in (MAX_STEP + 1)..=hi {
                            if !self.is_standable(nx_, iy + dy, nz_) {
                                continue;
                            }
                            let from = self.cell_floor_meters(ix, iy, iz);
                            let to = self.cell_floor_meters(nx_, iy + dy, nz_);
                            let cb = self.comp_at_cell(nx_, iy + dy, nz_);
                            out.push(NavClimb {
                                from,
                                to,
                                rise: to.y - from.y,
                                joins: (cb != 0 && cb != ca).then_some((ca, cb)),
                            });
                            break;
                        }
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
                for dy in -STAIR_STEP..=STAIR_STEP {
                    let (nx, ny, nz) = (cur.0 + dx, cur.1 + dy, cur.2 + dz);
                    // Same adjacency the component labels were built from — see
                    // `can_step`. If these two ever disagree, the O(1) refusal above
                    // starts denying routes that exist.
                    if !self.in_bounds(nx, ny, nz) || !self.can_step(cur, (nx, ny, nz)) {
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
///
/// `stair_volumes` are the boxes the caller knows to be **stairs** (the free-standing
/// stair-run steps; each region's own [`StairDesc`] treads are found here and tagged
/// automatically). They are treated as solid like any other extra *and* tagged, so
/// passing a box in both lists is harmless — the tag is what matters. Inside them the
/// vertical step limit relaxes to [`STAIR_STEP`]; see that constant for why.
pub fn bake(
    regions: &mut [Region],
    structure_solids: &[[f32; 6]],
    stair_volumes: &[[f32; 6]],
) -> Option<NavWorld> {
    if regions.is_empty() {
        return None;
    }
    for r in regions.iter_mut() {
        r.refresh_shell();
    }

    // Stair treads + free-standing platform/stair-run boxes — solid volumes that
    // live outside the CSG brush set but that agents must stand on / be blocked by.
    let stairs: Vec<[f32; 6]> = regions
        .iter()
        .flat_map(|r| r.stairs.iter().flat_map(|s| s.solid_boxes()))
        .chain(stair_volumes.iter().copied())
        .collect();
    let mut extras: Vec<[f32; 6]> = stairs.clone();
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

    // Tag the stair cells. Each box is grounded (solid from the floor to its tread), so
    // the cells that actually matter — the standable ones *on* the flight — sit above it:
    // hence the upward extension by an agent's height. Widened by one cell in XZ too, so
    // the landing at either end of a flight is inside the relaxation and a skipped tread
    // right at the top can still be stepped onto.
    let mut stair = vec![0u8; (nx * ny * nz) as usize];
    for b in &stairs {
        let (bx0, by0, bz0) = (b[0] - 1.0, b[1], b[2] - 1.0);
        let (bx1, by1, bz1) = (
            b[0] + b[3] + 1.0,
            b[1] + b[4] + AGENT_HEIGHT_CELLS as f32,
            b[2] + b[5] + 1.0,
        );
        let lo = |v: f32, o: i32| ((v - o as f32).floor() as i32).max(0);
        for iy in lo(by0, y0)..=(((by1 - y0 as f32).ceil() as i32) - 1).min(ny - 1) {
            let cy = y0 as f32 + iy as f32 + 0.5;
            for iz in lo(bz0, z0)..=(((bz1 - z0 as f32).ceil() as i32) - 1).min(nz - 1) {
                let cz = z0 as f32 + iz as f32 + 0.5;
                for ix in lo(bx0, x0)..=(((bx1 - x0 as f32).ceil() as i32) - 1).min(nx - 1) {
                    let cx = x0 as f32 + ix as f32 + 0.5;
                    if cx >= bx0 && cx < bx1 && cy >= by0 && cy < by1 && cz >= bz0 && cz < bz1 {
                        stair[((iy * nz + iz) * nx + ix) as usize] = 1;
                    }
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
        stair,
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
        let nav = bake(&mut regions, &[], &[]).expect("room bakes");
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
        let nav = bake(&mut regions, &[island], &[]).expect("bake");
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
        let nav = bake(&mut regions, &[], &[]).expect("bake");
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
        let mut nav = bake(&mut regions, &[], &[]).expect("bake");

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
        let mut nav = bake(&mut regions, &[], &[]).expect("bake");
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
        let nav = bake(&mut regions, &[slab], &[]).expect("bake");

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
        let nav = bake(&mut regions, &[], &[]).expect("bake");
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

    // ─── Validation queries ──────────────────────────────────────────────

    /// **The gap report has to name the height, not just the distance.**
    ///
    /// A ledge two cells up is the shipping level's actual defect (every island there
    /// sits behind one 0.5 m climb), and the fix depends entirely on which number is
    /// large: `flat 0.25, rise 0.50` says raise the step or add a tread, `flat 4.0`
    /// would say build a bridge. Asserted on both halves for that reason.
    #[test]
    fn a_severed_ledge_reports_its_height_and_the_edge_that_would_join_it() {
        // Room floor at y=0, plus a slab whose top is TWO cells up (0.5 m) — one more
        // than MAX_STEP, so the top is walkable and unreachable at the same time.
        let mut regions = room();
        let slab: [f32; 6] = [8.0, 0.0, 8.0, 6.0, 2.0, 6.0]; // WT
        let nav = bake(&mut regions, &[slab], &[]).expect("bake");

        let sizes = nav.component_sizes();
        assert_eq!(sizes.len(), 2, "floor + ledge, got {sizes:?}");
        let main = nav.main_component().expect("a main component");
        let island = sizes.iter().map(|(id, _)| *id).find(|id| *id != main).unwrap();

        let gap = nav.nearest_gap(island, main).expect("the two are close by");
        assert!(gap.flat <= 0.26, "the ledge is one cell across, got {:.2} m", gap.flat);
        assert!(
            (gap.rise + 0.5).abs() < 1e-3,
            "the floor is 0.5 m BELOW the ledge, got rise {:+.2} m",
            gap.rise
        );

        // And the same defect from the other side: an over-climb edge that would
        // reconnect the level, which is what tells the author it is a step problem
        // rather than a distance problem.
        let climbs = nav.overclimb_edges(0.8);
        let joiners: Vec<&NavClimb> = climbs.iter().filter(|c| c.joins.is_some()).collect();
        assert!(
            !joiners.is_empty(),
            "the step onto the ledge should be reported as joining two components"
        );
        assert!(
            joiners.iter().all(|c| (c.rise - 0.5).abs() < 1e-3),
            "every joining climb here is exactly the 0.5 m step"
        );
    }

    /// A corridor two cells (0.5 m) wide is a **legal path** the grid is happy with —
    /// so the pinch report is what makes it visible at all. Its threshold is exclusive:
    /// a corridor exactly as wide as the body asked for is not a pinch.
    #[test]
    fn a_narrow_corridor_is_reported_as_a_pinch_and_an_open_room_is_not() {
        let mut regions = room();
        // Fill the room except a 2-cell strip along X at z = 10..12.
        let solids = [
            [0.0, 0.0, 0.0, 24.0, 16.0, 10.0],
            [0.0, 0.0, 12.0, 24.0, 16.0, 12.0],
        ];
        let nav = bake(&mut regions, &solids, &[]).expect("bake");

        let pinches = nav.pinch_points(0.6);
        assert!(!pinches.is_empty(), "a 0.5 m corridor should be flagged at 0.6 m");
        assert!(
            pinches.iter().all(|p| (p.width - 0.5).abs() < 1e-3),
            "each flagged cell reports the corridor's own width"
        );
        assert!(
            nav.pinch_points(0.5).is_empty(),
            "a corridor exactly 0.5 m wide is not narrower than 0.5 m"
        );

        // The control: the same query over open floor finds nothing, which is the
        // property that keeps the report readable on a real level.
        let mut open = room();
        let plain = bake(&mut open, &[], &[]).expect("bake");
        assert!(plain.pinch_points(0.6).is_empty(), "open floor is not a pinch");
    }

    /// **A staircase is not a tight corridor**, and a width query that says otherwise is
    /// useless on a real level.
    ///
    /// Treads are one cell deep by construction (`StairDesc::solid_boxes` emits 1 WT
    /// boxes, and a stair-run's are about that), so measuring the free span at a *fixed
    /// height* makes every step of every flight a 0.25 m corridor. On the shipping level
    /// that was 1,423 false positives — more findings than the level has real problems,
    /// which is the same as having no report at all.
    #[test]
    fn a_staircase_is_not_reported_as_a_pinch() {
        let mut regions = room();
        // Ten steps across the full width of the room: 1 WT tread, 1 WT riser — exactly
        // what the CSG stair tool emits. Descending toward +X so the bottom tread meets
        // the room floor (a flight whose base is walled off is its own component and
        // would prove nothing).
        let steps: Vec<[f32; 6]> = (0..10)
            .map(|k| [k as f32, 0.0, 0.0, 1.0, 10.0 - k as f32, 24.0])
            .collect();
        let nav = bake(&mut regions, &steps, &[]).expect("bake");
        // Sanity: the flight really is there and really is walkable as one component.
        let top = nav.cell_floor_meters(0, 10, 12);
        let floor = Vec3::new(20.0 * WORLD_SCALE, 0.1, 12.0 * WORLD_SCALE);
        assert_eq!(
            nav.component_at(top),
            nav.component_at(floor),
            "a 1-cell-per-step flight must be walkable, or this tests nothing"
        );
        assert!(
            nav.pinch_points(0.73).is_empty(),
            "a wide staircase reported {} pinch cell(s) — the width query must follow \
             the surface, not a fixed height",
            nav.pinch_points(0.73).len()
        );
    }

    /// **A staircase steeper than 45° stays walkable — and only the staircase does.**
    ///
    /// The shipping level's real defect. A stair run lays its steps out as
    /// `step_run = total_run / steps`, so a run steeper than 45° gets treads shallower
    /// than a nav cell; some tread then has no cell centre in it, loses its standable
    /// cell, and the strip skips it. That leaves a two-cell gap mid-flight, and a flat
    /// `MAX_STEP` severs the level there (3,488 cells, 15%, on `slot1`).
    ///
    /// The arena is that geometry exactly: 9 treads over 8 columns, so one column serves
    /// two treads. The control matters as much as the claim — the identical 0.5 m step
    /// built out of a *platform* must stay a wall, or the fix is just a global
    /// `MAX_STEP = 2` wearing a disguise.
    #[test]
    fn a_stair_with_sub_cell_treads_stays_walkable_but_a_ledge_does_not() {
        // 9 steps of 1 WT rise spread over 8 WT of run: tread depth 8/9 = 0.889 WT,
        // the same as the shipping level's stair run 10. Descending toward +X so the
        // bottom tread meets the room floor.
        let steps: Vec<[f32; 6]> = (0..9)
            .map(|k| {
                let lo = 8.0 - (k as f32 + 1.0) * 8.0 / 9.0;
                [lo, 0.0, 0.0, 8.0 / 9.0, k as f32 + 1.0, 24.0]
            })
            .collect();

        // Untagged: the flight is severed, which is the bug this fixes.
        let mut regions = room();
        let blind = bake(&mut regions, &steps, &[]).expect("bake");
        assert!(
            blind.component_sizes().len() > 1,
            "sub-cell treads must sever the flight when nav doesn't know it's a stair — \
             if this passes, the arena isn't reproducing the defect"
        );

        // Tagged as stairs: one component, the whole flight walkable.
        let mut regions = room();
        let nav = bake(&mut regions, &steps, &steps).expect("bake");
        assert_eq!(
            nav.component_sizes().len(),
            1,
            "the tagged flight should join the floor, got {:?}",
            nav.component_sizes()
        );
        let top = nav.cell_floor_meters(0, 9, 12);
        let floor = Vec3::new(20.0 * WORLD_SCALE, 0.1, 12.0 * WORLD_SCALE);
        assert!(
            nav.find_path(floor, top).is_some(),
            "and A* should route up it — labels and search must agree"
        );

        // The control: the same 0.5 m rise as a plain ledge is still a wall.
        let mut regions = room();
        let ledge: [f32; 6] = [8.0, 0.0, 8.0, 6.0, 2.0, 6.0];
        let with_ledge = bake(&mut regions, &[ledge], &[]).expect("bake");
        assert_eq!(
            with_ledge.component_sizes().len(),
            2,
            "a 0.5 m ledge that is NOT stairs must stay unreachable — the relaxation is \
             local to stair geometry, not a global step increase"
        );
    }

    /// A level with nothing wrong must be *provably* clean, not merely quiet: one
    /// component covering every standable cell, no islands, no climbs.
    #[test]
    fn an_open_room_reports_no_issues_at_all() {
        let mut regions = room();
        let nav = bake(&mut regions, &[], &[]).expect("bake");
        let sizes = nav.component_sizes();
        assert_eq!(sizes.len(), 1, "one room, one component");

        let cells = nav.standable_with_components();
        assert_eq!(
            cells.len(),
            nav.all_standable().len(),
            "the overlay must cover every standable cell"
        );
        let main = nav.main_component().unwrap();
        assert!(cells.iter().all(|(_, c)| *c == main), "all of it is the main component");
        assert!(nav.overclimb_edges(0.8).is_empty(), "flat floor has no steps");
    }

    #[test]
    fn los_blocked_by_the_wall() {
        let mut regions = room();
        let nav = bake(&mut regions, &[], &[]).expect("bake");
        // A point inside vs. a point well outside the room (through the wall).
        let inside = Vec3::new(3.0, 1.0, 3.0);
        let outside = Vec3::new(3.0, 1.0, -5.0);
        assert!(!nav.los_clear(inside, outside), "wall should block LOS");
        // Two interior points see each other.
        assert!(nav.los_clear(Vec3::new(1.0, 1.0, 1.0), Vec3::new(5.0, 1.0, 5.0)));
    }
}
