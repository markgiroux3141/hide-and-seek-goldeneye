//! Headless "perception" for level generation: bake the nav grid and turn the
//! geometry into an **LLM-friendly text report** — per-floor ASCII floorplans, a
//! room-connectivity graph, and multiplayer-flow metrics (reachability, route
//! redundancy, sniper perches, camp nooks). No GPU, no window: everything is
//! computed from the same `NavWorld` the hunt uses plus the region CSG.
//!
//! The report is the feedback the author reads to judge and fix a level: a
//! floorplan you can eyeball, and hard numbers for the things that make a good
//! multiplayer map (many rooms, dense interconnection, verticality, overlooks).

use std::collections::HashSet;
use std::fmt::Write as _;

use engine::geometry::csg_runtime::{Region, WORLD_SCALE};
use engine::sim::nav::NavWorld;
use glam::Vec3;

use super::builder::BuiltLevel;

/// Meters → global WT cell index along a horizontal axis.
fn cell_h(m: f32) -> i32 {
    (m / WORLD_SCALE).floor() as i32
}
/// Meters → global WT cell index along Y (standable feet sit on integer levels).
fn cell_y(m: f32) -> i32 {
    (m / WORLD_SCALE).round() as i32
}

/// A baked, analyzable level: the nav grid plus the source data needed to label
/// and probe it.
pub struct Analysis<'a> {
    nav: &'a NavWorld,
    regions: &'a [Region],
    level: &'a BuiltLevel,
    /// Every standable cell as (ix, iy, iz) global WT indices.
    stand: HashSet<(i32, i32, i32)>,
    /// WT bounds (inclusive-ish) over the whole level.
    x0: i32,
    x1: i32,
    z0: i32,
    z1: i32,
    /// Distinct standable floor levels (WT y), ascending.
    levels: Vec<i32>,
}

impl<'a> Analysis<'a> {
    pub fn new(nav: &'a NavWorld, regions: &'a [Region], level: &'a BuiltLevel) -> Self {
        let standable = nav.all_standable();
        let mut stand = HashSet::new();
        for p in &standable {
            stand.insert((cell_h(p.x), cell_y(p.y), cell_h(p.z)));
        }

        // WT bounds from region shells + platform footprints.
        let mut x0 = i32::MAX;
        let mut x1 = i32::MIN;
        let mut z0 = i32::MAX;
        let mut z1 = i32::MIN;
        let mut fit = |xa: f32, za: f32, xb: f32, zb: f32| {
            x0 = x0.min(xa.floor() as i32);
            z0 = z0.min(za.floor() as i32);
            x1 = x1.max(xb.ceil() as i32);
            z1 = z1.max(zb.ceil() as i32);
        };
        for r in regions {
            let s = r.shell();
            fit(s.x, s.z, s.x + s.w, s.z + s.d);
        }
        for p in &level.platforms {
            fit(p.x, p.z, p.x + p.size_x, p.z + p.size_z);
        }
        if x0 > x1 {
            x0 = 0;
            x1 = 1;
            z0 = 0;
            z1 = 1;
        }

        let mut lv: Vec<i32> = stand.iter().map(|c| c.1).collect();
        lv.sort_unstable();
        lv.dedup();

        Analysis {
            nav,
            regions,
            level,
            stand,
            x0,
            x1,
            z0,
            z1,
            levels: lv,
        }
    }

    /// The full text report.
    pub fn report(&self) -> String {
        let mut s = String::new();
        self.overview(&mut s);
        self.floorplans(&mut s);
        self.connectivity(&mut s);
        self.verticality(&mut s);
        self.perches(&mut s);
        self.headroom(&mut s);
        self.nooks(&mut s);
        s
    }

    fn overview(&self, s: &mut String) {
        let _ = writeln!(s, "==================== LEVEL REPORT ====================");
        let _ = writeln!(
            s,
            "bounds (WT): x[{}..{}] z[{}..{}]  span {}x{} WT  ({:.1}x{:.1} m)",
            self.x0,
            self.x1,
            self.z0,
            self.z1,
            self.x1 - self.x0,
            self.z1 - self.z0,
            (self.x1 - self.x0) as f32 * WORLD_SCALE,
            (self.z1 - self.z0) as f32 * WORLD_SCALE,
        );
        let _ = writeln!(
            s,
            "rooms/labels: {}   platforms: {}   stair-runs: {}   brushes: {}",
            self.level.rooms.len(),
            self.level.platforms.len(),
            self.level.stair_runs.len(),
            self.level.brushes.len(),
        );
        let _ = writeln!(
            s,
            "standable cells: {}   floor levels (WT y): {:?}",
            self.stand.len(),
            self.levels,
        );
        let _ = writeln!(s);
    }

    /// One ASCII top-down plan per floor level. `.`=floor `#`=solid ` `=air/void
    /// `S`=spawn, letters = room-label centers (see legend).
    fn floorplans(&self, s: &mut String) {
        // Legend: assign each room a char.
        let label_char = |i: usize| -> char {
            let alph = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
            alph.get(i).map(|&c| c as char).unwrap_or('?')
        };
        let _ = writeln!(s, "-------------------- FLOORPLANS ---------------------");
        let _ = writeln!(s, "legend:");
        for (i, r) in self.level.rooms.iter().enumerate() {
            let _ = writeln!(
                s,
                "  {} = {:<10} {}x{}x{} WT (w×d×h), floor y={}",
                label_char(i),
                r.name,
                r.aabb[3] as i32,
                r.aabb[5] as i32,
                r.aabb[4] as i32,
                r.aabb[1] as i32
            );
        }
        let _ = writeln!(s);

        let spawn_cell = (
            cell_h(self.level.spawn.x),
            cell_y(self.level.spawn.y),
            cell_h(self.level.spawn.z),
        );

        // Sample step: keep any plan under ~110 columns wide.
        let width = (self.x1 - self.x0).max(1);
        let step = ((width + 109) / 110).max(1);

        for &ly in &self.levels {
            let _ = writeln!(s, "### floor y={} WT (z↓ down, x→ right, step={}):", ly, step);
            // Room labels that live on this level.
            let labels_here: Vec<(i32, i32, char)> = self
                .level
                .rooms
                .iter()
                .enumerate()
                .filter(|(_, r)| (r.aabb[1].round() as i32) == ly)
                .map(|(i, r)| {
                    let cx = (r.aabb[0] + r.aabb[3] * 0.5).floor() as i32;
                    let cz = (r.aabb[2] + r.aabb[5] * 0.5).floor() as i32;
                    (cx, cz, label_char(i))
                })
                .collect();

            let mut iz = self.z0;
            while iz < self.z1 {
                let mut line = String::new();
                let mut ix = self.x0;
                while ix < self.x1 {
                    line.push(self.cell_char(ix, ly, iz, spawn_cell, &labels_here));
                    ix += step;
                }
                let _ = writeln!(s, "{}", line);
                iz += step;
            }
            let _ = writeln!(s);
        }
    }

    fn cell_char(
        &self,
        ix: i32,
        ly: i32,
        iz: i32,
        spawn_cell: (i32, i32, i32),
        labels: &[(i32, i32, char)],
    ) -> char {
        if self.stand.contains(&(ix, ly, iz)) {
            if (ix, ly, iz) == spawn_cell {
                return 'S';
            }
            for &(lx, lz, ch) in labels {
                if lx == ix && lz == iz {
                    return ch;
                }
            }
            return '.';
        }
        // Solid probe a bit above the floor (body height) → wall.
        let wx = ix as f32 + 0.5;
        let wz = iz as f32 + 0.5;
        let wy = ly as f32 + 2.0;
        if self.regions.iter().any(|r| r.solid_at(wx, wy, wz)) {
            '#'
        } else {
            ' '
        }
    }

    /// Reachability from spawn + intended-edge verification + graph density.
    fn connectivity(&self, s: &mut String) {
        let _ = writeln!(s, "------------------ CONNECTIVITY ---------------------");
        let spawn = self.level.spawn;

        // Representative standable target for each room: sample a 3×3 grid over the
        // footprint and prefer a point actually reachable from spawn (so a room
        // isn't mis-flagged when its center sits in a pit/hole). Falls back to the
        // nearest-standable to the center if none reach.
        let targets: Vec<Option<Vec3>> = self
            .level
            .rooms
            .iter()
            .map(|r| {
                let s = WORLD_SCALE;
                let mut fallback = None;
                for fz in [0.25, 0.5, 0.75] {
                    for fx in [0.25, 0.5, 0.75] {
                        let p = Vec3::new(
                            (r.aabb[0] + r.aabb[3] * fx) * s,
                            r.aabb[1] * s + 0.1,
                            (r.aabb[2] + r.aabb[5] * fz) * s,
                        );
                        if let Some(t) = self.nav.nearest_standable(p.x, p.y, p.z, 6) {
                            if fallback.is_none() {
                                fallback = Some(t);
                            }
                            if self.nav.find_path(spawn, t).is_some() {
                                return Some(t); // a reachable representative
                            }
                        }
                    }
                }
                fallback
            })
            .collect();

        let _ = writeln!(s, "reachability from spawn {:?} (WT):", to_wt(spawn));
        let mut unreachable = 0;
        for (i, r) in self.level.rooms.iter().enumerate() {
            match targets[i] {
                None => {
                    let _ = writeln!(s, "  [!] {:<16} NO standable cell found", r.name);
                    unreachable += 1;
                }
                Some(t) => match self.nav.find_path(spawn, t) {
                    Some(path) => {
                        let _ = writeln!(
                            s,
                            "      {:<16} reachable  ({} waypoints, ~{:.1} m)",
                            r.name,
                            path.len(),
                            path_len(&path)
                        );
                    }
                    None => {
                        let _ = writeln!(s, "  [!] {:<16} UNREACHABLE from spawn", r.name);
                        unreachable += 1;
                    }
                },
            }
        }
        if unreachable == 0 {
            let _ = writeln!(s, "  => all {} labels reachable from spawn.", self.level.rooms.len());
        } else {
            let _ = writeln!(
                s,
                "  => {} label(s) not reachable by ENEMY grid-nav — either a sealed bug OR an",
                unreachable
            );
            let _ = writeln!(
                s,
                "     intentional player-only area (free-standing stairs down bake this way).",
            );
        }
        let _ = writeln!(s);

        // Intended edges: verify walkable + compute degrees.
        let n = self.level.rooms.len();
        let mut degree = vec![0i32; n];
        let _ = writeln!(s, "intended connections ({} edges):", self.level.edges.len());
        for &(a, b) in &self.level.edges {
            degree[a.0] += 1;
            degree[b.0] += 1;
            let ok = match (targets[a.0], targets[b.0]) {
                (Some(ta), Some(tb)) => self.nav.find_path(ta, tb).is_some(),
                _ => false,
            };
            let (na, nb) = (&self.level.rooms[a.0].name, &self.level.rooms[b.0].name);
            let _ = writeln!(
                s,
                "  {} {:<14} <-> {:<14}",
                if ok { "  " } else { "[!]" },
                na,
                nb
            );
        }
        let _ = writeln!(s);
        let _ = writeln!(s, "room degrees (connections each):");
        let mut deadends = 0;
        for (i, r) in self.level.rooms.iter().enumerate() {
            let flag = if degree[i] < 2 { "  <- dead-end/low" } else { "" };
            if degree[i] < 2 {
                deadends += 1;
            }
            let _ = writeln!(s, "  {:<16} {}{}", r.name, degree[i], flag);
        }
        // Cyclomatic number: edges - nodes + 1 (assuming connected) = independent loops.
        let cyclo = self.level.edges.len() as i32 - n as i32 + 1;
        let _ = writeln!(
            s,
            "  density: {} edges / {} rooms   independent loops ~= {}   dead-ends: {}",
            self.level.edges.len(),
            n,
            cyclo.max(0),
            deadends
        );
        let _ = writeln!(
            s,
            "  (multiplayer wants loops>0 and few/zero dead-ends: multiple routes everywhere)"
        );
        let _ = writeln!(s);
    }

    /// Floor levels reached from spawn (verticality).
    fn verticality(&self, s: &mut String) {
        let _ = writeln!(s, "------------------- VERTICALITY ---------------------");
        let _ = writeln!(s, "distinct floor levels: {} -> {:?} WT", self.levels.len(), self.levels);
        let spawn = self.level.spawn;
        for &ly in &self.levels {
            // Sample up to N standable cells on this level and count how many are
            // reachable — robust against a single stray/disconnected cell.
            let cells: Vec<(i32, i32, i32)> =
                self.stand.iter().copied().filter(|c| c.1 == ly).collect();
            let total = cells.len();
            let sample = total.min(16);
            let mut reachable = 0;
            for &(ix, iy, iz) in cells.iter().take(sample) {
                let t = Vec3::new(
                    (ix as f32 + 0.5) * WORLD_SCALE,
                    iy as f32 * WORLD_SCALE + 0.1,
                    (iz as f32 + 0.5) * WORLD_SCALE,
                );
                if self.nav.find_path(spawn, t).is_some() {
                    reachable += 1;
                }
            }
            let tag = if reachable == 0 {
                "[!] NOT reachable"
            } else if reachable < sample {
                "partly reachable"
            } else {
                "reachable from spawn"
            };
            let _ = writeln!(
                s,
                "  y={:<3} WT ({:.2} m): {} ({}/{} sampled, {} cells)",
                ly,
                ly as f32 * WORLD_SCALE,
                tag,
                reachable,
                sample,
                total
            );
        }
        let _ = writeln!(s);
    }

    /// Sniper perches: platform tops with line-of-sight down into a lower room.
    fn perches(&self, s: &mut String) {
        let _ = writeln!(s, "------------------ SNIPER PERCHES -------------------");
        if self.level.platforms.is_empty() {
            let _ = writeln!(s, "  (no platforms)");
            let _ = writeln!(s);
            return;
        }
        for p in &self.level.platforms {
            let eye = Vec3::new(
                (p.x + p.size_x * 0.5) * WORLD_SCALE,
                p.y * WORLD_SCALE + 0.4, // ~eye height above the deck
                (p.z + p.size_z * 0.5) * WORLD_SCALE,
            );
            // For each room whose floor is meaningfully below this perch, count
            // how many of its floor cells the perch can see.
            let mut seen_rooms: Vec<(String, usize, usize, i32)> = Vec::new();
            for r in &self.level.rooms {
                let drop = p.y - r.aabb[1];
                if drop < 3.0 {
                    continue; // not below the perch
                }
                let (mut seen, mut total) = (0usize, 0usize);
                let (rx, rz) = (r.aabb[0], r.aabb[2]);
                let (rw, rd) = (r.aabb[3], r.aabb[5]);
                let mut zz = rz + 0.5;
                while zz < rz + rd {
                    let mut xx = rx + 0.5;
                    while xx < rx + rw {
                        let target = Vec3::new(
                            xx * WORLD_SCALE,
                            r.aabb[1] * WORLD_SCALE + 0.2,
                            zz * WORLD_SCALE,
                        );
                        total += 1;
                        if self.nav.los_clear(eye, target) {
                            seen += 1;
                        }
                        xx += 2.0;
                    }
                    zz += 2.0;
                }
                if seen > 0 {
                    seen_rooms.push((r.name.clone(), seen, total, drop as i32));
                }
            }
            seen_rooms.sort_by(|a, b| b.1.cmp(&a.1));
            if seen_rooms.is_empty() {
                let _ = writeln!(
                    s,
                    "  perch id{} (top y={} WT): no clear overlook onto a lower room",
                    p.id, p.y as i32
                );
            } else {
                let _ = writeln!(s, "  perch id{} (top y={} WT) overlooks:", p.id, p.y as i32);
                for (name, seen, total, drop) in &seen_rooms {
                    let _ = writeln!(
                        s,
                        "      {:<14} sees {}/{} floor cells (+{} WT above)",
                        name, seen, total, drop
                    );
                }
            }
        }
        let _ = writeln!(s);
    }

    /// Headroom lint: flag walkable cells (especially stair treads) whose ceiling
    /// is uncomfortably low — the "head-bumps-going-down-the-stairs" defect. Nav
    /// only needs 6 WT (1.5 m) of clearance to stand, but a ~1.7 m player + camera
    /// bumps below ~8 WT, so anything under COMFORT reads as cramped.
    fn headroom(&self, s: &mut String) {
        const COMFORT: i32 = 8; // WT; below this the player's head clips
        const CAP: i32 = 16;
        let _ = writeln!(s, "-------------------- HEADROOM -----------------------");
        let mut low: Vec<(i32, i32, i32, i32)> = Vec::new(); // ix,iy,iz,clearance
        for &(ix, iy, iz) in &self.stand {
            let mut clear = CAP;
            for k in 1..=CAP {
                let m = Vec3::new(
                    (ix as f32 + 0.5) * WORLD_SCALE,
                    (iy as f32 + k as f32 + 0.5) * WORLD_SCALE,
                    (iz as f32 + 0.5) * WORLD_SCALE,
                );
                if self.nav.is_solid_meters(m.x, m.y, m.z) {
                    clear = k; // k air cells above feet (feet..feet+k-1), solid at +k
                    break;
                }
            }
            if clear < COMFORT {
                low.push((ix, iy, iz, clear));
            }
        }
        if low.is_empty() {
            let _ = writeln!(
                s,
                "  OK — every walkable cell has >= {} WT ({:.1} m) of head clearance.",
                COMFORT,
                COMFORT as f32 * WORLD_SCALE
            );
        } else {
            low.sort_by_key(|n| n.3); // tightest first
            let _ = writeln!(
                s,
                "  [!] {} walkable cell(s) are cramped (< {} WT clearance) — raise the",
                low.len(),
                COMFORT
            );
            let _ = writeln!(s, "      ceiling/stairwell there so the player doesn't bump:");
            for n in low.iter().take(8) {
                let _ = writeln!(
                    s,
                    "      WT ({}, {}, {})  clearance={} WT ({:.2} m)",
                    n.0,
                    n.1,
                    n.2,
                    n.3,
                    n.3 as f32 * WORLD_SCALE
                );
            }
        }
        let _ = writeln!(s);
    }

    /// Camp nooks: standable cells backed into a corner/alcove (<=2 open
    /// horizontal neighbours) — defensible holds.
    fn nooks(&self, s: &mut String) {
        let _ = writeln!(s, "-------------------- CAMP NOOKS ---------------------");
        let mut nooks: Vec<(i32, i32, i32, i32)> = Vec::new(); // ix,iy,iz,openings
        for &(ix, iy, iz) in &self.stand {
            let mut open = 0;
            for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                if self.stand.contains(&(ix + dx, iy, iz + dz)) {
                    open += 1;
                }
            }
            // Isolated single cells (0 openings) are usually stair tops/noise;
            // a good nook has exactly 1-2 approaches.
            if open == 1 || open == 2 {
                nooks.push((ix, iy, iz, open));
            }
        }
        let _ = writeln!(
            s,
            "  {} standable cells are alcove-like (1-2 approaches).",
            nooks.len()
        );
        // Show a spread-out handful so the author can place objectives/camps.
        nooks.sort_by_key(|n| (n.3, n.1));
        for n in nooks.iter().take(8) {
            let _ = writeln!(
                s,
                "  nook WT ({}, {}, {})  approaches={}",
                n.0, n.1, n.2, n.3
            );
        }
        let _ = writeln!(s);
        let _ = writeln!(s, "=====================================================");
    }
}

fn to_wt(m: Vec3) -> (i32, i32, i32) {
    (
        (m.x / WORLD_SCALE).round() as i32,
        (m.y / WORLD_SCALE).round() as i32,
        (m.z / WORLD_SCALE).round() as i32,
    )
}

fn path_len(path: &[Vec3]) -> f32 {
    path.windows(2).map(|w| w[0].distance(w[1])).sum()
}
