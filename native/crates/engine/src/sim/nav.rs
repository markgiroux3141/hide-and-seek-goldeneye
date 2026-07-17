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

/// A* penalty for routing through an intact door — large enough to prefer an open
/// detour, finite so a walled-in player is still reachable via breaching. JS
/// `navWorld.DOOR_COST` is 25 on a base move cost of 1; here move costs are
/// scaled ×2 (integer), so the penalty scales to 50 to preserve the ratio.
/// This overlay is the whole thesis: a dynamic obstacle rides the static grid,
/// and breaching = flipping [`NavDoor::broken`], read live by A* — **no re-bake**.
const DOOR_COST: i32 = 50;

/// One door's live overlay state. `broken` is read live by [`NavWorld::find_path`]
/// and [`NavWorld::door_blocking`], so breaching needs no re-voxelization.
struct NavDoor {
    broken: bool,
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
    /// Door records; `doors[i].broken` is read live by A* and door_blocking.
    doors: Vec<NavDoor>,
    /// cellIdx → (doorIndex + 1); 0 = no door. Empty until `set_doors`.
    door_grid: Vec<u16>,
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
        self.doors = door_brushes.iter().map(|_| NavDoor { broken: false }).collect();
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

    /// Whether door `i` is broken (test/inspection helper).
    pub fn door_broken(&self, i: usize) -> bool {
        self.doors.get(i).map(|d| d.broken).unwrap_or(true)
    }

    /// Flip a door to broken — the breach. A* and door_blocking read this live,
    /// so the path reroutes with no re-bake (the thesis).
    pub fn break_door(&mut self, i: usize) {
        if let Some(d) = self.doors.get_mut(i) {
            d.broken = true;
        }
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
                    if !self.doors[di].broken {
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

        while let Some(std::cmp::Reverse((_f, _, cur))) = open.pop() {
            let ck = self.idx(cur.0, cur.1, cur.2);
            if ck == goal_key {
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
                    let door_penalty = match self.door_at_cell_idx(nk) {
                        Some(di) if !self.doors[di].broken => DOOR_COST,
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

    Some(NavWorld {
        x0,
        y0,
        z0,
        nx,
        ny,
        nz,
        solid,
        doors: Vec::new(),
        door_grid: Vec::new(),
    })
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

        // Intact: the door blocks the segment and A* still finds the (only) route
        // through it — the door slab spans the room, so breaching is the only way.
        assert_eq!(nav.door_blocking(from, to), Some(0), "intact door blocks segment");
        assert!(nav.find_path(from, to).is_some(), "path exists through the door");
        assert!(!nav.door_broken(0));

        // Breach = flip the flag; the same overlay, no re-bake.
        nav.break_door(0);
        assert!(nav.door_broken(0));
        assert_eq!(nav.door_blocking(from, to), None, "broken door no longer blocks");
        assert!(nav.find_path(from, to).is_some(), "path still exists after breach");
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
