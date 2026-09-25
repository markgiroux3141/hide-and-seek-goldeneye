//! Headless "perception" for level generation: turn a baked level into an
//! **LLM-friendly report** — a pass/fail summary first, then the NAV tab's findings,
//! the room graph, one ASCII floorplan per real floor, and the flow metrics (perches,
//! headroom, camp corners). The same data is available as JSON ([`Analysis::data`]) so
//! a scripted caller can assert on it instead of grepping prose.
//!
//! Everything is read off the **one** nav bake the game uses ([`World::bake_level_nav`])
//! via [`NavWorld::walk_graph`], so nothing here re-derives what connects to what.
//!
//! Rewritten 2026-09 after a retro found the first version misleading an author:
//!
//! * **Floors were exact Y values**, so every stair tread was a "floor" with its own
//!   floorplan — 30 plans and 1,850 lines for `grand`. A floor is now a level with a
//!   real amount of flat walkable area; treads and small steps are drawn onto the plan
//!   of the floor they rise from.
//! * **Connectivity was the author's claim.** Degree, loops and dead-ends came from the
//!   edges the design *declared*. The room graph is now **derived** from the walkable
//!   grid and diffed against the declared one, which also catches rooms that merged by
//!   accident.
//! * **Perches sighted from 0.4 m above the deck centre** — a metre below the player's
//!   eye, from the one spot a slab hides the floor below best. They now sight from the
//!   deck's edges at [`crate::character::EYE`].
//! * **Camp nooks were mostly stair-tread edges.** Stair cells are now excluded and the
//!   list is clustered.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;

use engine::geometry::csg_runtime::WORLD_SCALE;
use engine::sim::nav::{NavWorld, WalkGraph};
use glam::Vec3;
use serde::Serialize;

use super::builder::BuiltLevel;
use crate::world::{NavIssues, NavSeverity, World};

/// A level with at least this many flat (non-stair) walkable cells is a **floor** and
/// gets a plan; anything smaller is a step, a landing or a ledge, drawn on the plan of
/// the floor below it. 16 cells = 1 m².
const MIN_FLOOR_CELLS: usize = 16;
/// An island this small is a warning, not a failure: a pillar top, a sliver the bake
/// carved off. Anything larger is floor somebody meant to be usable.
const TINY_ISLAND_CELLS: usize = 16;
/// Where a perch sights *to*: a standing body's chest, not its feet — a player is spotted
/// when their torso is visible, and aiming at the floor under-counts every overlook.
const TARGET_BODY_M: f32 = 0.9;
/// A perch counts as an overlook when it sees at least this share of a lower room.
const PERCH_MIN_SHARE: f32 = 0.10;
/// Head clearance below which a walkable cell reads as cramped (8 WT = 2 m). Nav only
/// needs 6 WT to stand; a ~1.7 m player plus camera bumps below this.
const HEADROOM_COMFORT: i32 = 8;
/// Floorplans wider than this are downsampled (keeping the most important glyph in each
/// block, so thin features survive).
const PLAN_MAX_COLS: i32 = 120;
/// Findings are clustered to one representative per this many metres.
const CLUSTER_M: f32 = 2.0;

/// How a check came out. Ordered: the verdict is the worst status present.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pass,
    Info,
    Warn,
    Fail,
}

impl Status {
    fn tag(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Info => "info",
            Status::Warn => "WARN",
            Status::Fail => "FAIL",
        }
    }
}

/// One line of the summary.
#[derive(Serialize, Clone, Debug)]
pub struct Check {
    pub check: &'static str,
    pub status: Status,
    pub detail: String,
}

/// One room label (a carved room, or a platform deck).
#[derive(Serialize, Clone, Debug)]
pub struct RoomRow {
    pub letter: char,
    pub name: String,
    /// `"room"` or `"platform"`.
    pub kind: &'static str,
    /// Width × depth × height, WT.
    pub size: [i32; 3],
    pub floor_y: i32,
    /// Standable cells labelled as this room.
    pub cells: usize,
    /// Any of its cells in the spawn's walkable component.
    pub reachable: bool,
    /// Rooms it connects to in the **derived** graph.
    pub degree: usize,
}

/// A connection the design declared.
#[derive(Serialize, Clone, Debug)]
pub struct DeclaredEdge {
    pub a: String,
    pub b: String,
    /// Both rooms share a walkable component.
    pub walkable: bool,
    /// And they touch directly (or through a shared corridor) in the derived graph.
    pub direct: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct FloorRow {
    pub y: i32,
    /// Flat walkable cells on this floor.
    pub cells: usize,
    /// Share of them in the main walkable component, percent.
    pub main_pct: f32,
    pub rooms: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Overlook {
    pub room: String,
    pub seen: usize,
    pub total: usize,
    /// WT above that room's floor.
    pub drop: i32,
}

#[derive(Serialize, Clone, Debug)]
pub struct PerchRow {
    pub name: String,
    pub top_y: i32,
    /// Lower rooms it can see into, best first.
    pub overlooks: Vec<Overlook>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Spot {
    pub wt: [i32; 3],
    pub room: Option<String>,
    /// Head clearance in WT (headroom), or approaches (camp corners).
    pub value: i32,
}

#[derive(Serialize, Clone, Debug)]
pub struct NavLineRow {
    pub severity: &'static str,
    pub text: String,
}

/// Everything the report says, as data. The text report is rendered from this, so the
/// two can never disagree.
#[derive(Serialize, Clone, Debug)]
pub struct ReportData {
    pub design: String,
    pub verdict: Status,
    pub checks: Vec<Check>,
    /// Walkable component sizes, largest first (the main one first).
    pub components: Vec<usize>,
    pub nav: Vec<NavLineRow>,
    pub rooms: Vec<RoomRow>,
    pub declared: Vec<DeclaredEdge>,
    /// Rooms that connect in the derived graph but were never declared to.
    pub undeclared: Vec<[String; 2]>,
    /// Rooms whose carved air touches with no wall between them, undeclared.
    pub merged: Vec<[String; 2]>,
    /// Independent loops in the derived graph, connectors counted as nodes (E - V + C).
    pub loops: i64,
    pub dead_ends: Vec<String>,
    pub floors: Vec<FloorRow>,
    pub perches: Vec<PerchRow>,
    pub cramped_cells: usize,
    pub cramped: Vec<Spot>,
    pub camp_corners: Vec<Spot>,
}

/// A node of the derived room graph: a labelled room, or a connector — a connected run
/// of walkable cells that belongs to no room (a corridor, a stairwell, a pit).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Node {
    Room(usize),
    Connector(u32),
}

/// A baked, analyzable level.
pub struct Analysis<'a> {
    design: &'a str,
    level: &'a BuiltLevel,
    world: &'a World,
    nav: &'a NavWorld,
    issues: &'a NavIssues,
    graph: WalkGraph,
    /// Global WT cell → index into `graph.cells`.
    at: HashMap<(i32, i32, i32), usize>,
    /// Which room label each cell belongs to.
    label: Vec<Option<usize>>,
    /// The main walkable component, and the one the spawn stands in.
    main: Option<u32>,
    spawn_comp: Option<u32>,
    platform: Vec<bool>,
    floors: Vec<i32>,
}

impl<'a> Analysis<'a> {
    pub fn new(
        design: &'a str,
        nav: &'a NavWorld,
        world: &'a World,
        level: &'a BuiltLevel,
        issues: &'a NavIssues,
    ) -> Self {
        let graph = nav.walk_graph();
        let at: HashMap<_, _> = graph.cells.iter().enumerate().map(|(i, c)| (c.wt, i)).collect();

        // Platform labels are the ones the builder mirrored off a platform deck.
        let platform: Vec<bool> = level
            .rooms
            .iter()
            .map(|r| {
                level.platforms.iter().any(|p| {
                    p.x == r.aabb[0] && p.z == r.aabb[2] && p.y == r.aabb[1] && r.aabb[4] == 1.0
                })
            })
            .collect();

        // A cell belongs to the most specific (smallest) label containing it: a deck
        // inside a hall is the deck, not the hall.
        let label = graph
            .cells
            .iter()
            .map(|c| {
                let (x, y, z) = (c.wt.0 as f32 + 0.5, c.wt.1 as f32, c.wt.2 as f32 + 0.5);
                level
                    .rooms
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| {
                        let a = r.aabb;
                        x >= a[0] && x < a[0] + a[3] && z >= a[2] && z < a[2] + a[5] && y >= a[1]
                            && y < a[1] + a[4]
                    })
                    .min_by(|(_, a), (_, b)| {
                        let v = |r: &super::builder::RoomLabel| r.aabb[3] * r.aabb[4] * r.aabb[5];
                        v(a).total_cmp(&v(b))
                    })
                    .map(|(i, _)| i)
            })
            .collect();

        let main = nav.main_component();
        let s = level.spawn;
        let spawn_comp = nav
            .nearest_standable(s.x, s.y + 0.1, s.z, 6)
            .and_then(|p| nav.component_at(p));

        let mut flat: HashMap<i32, usize> = HashMap::new();
        for c in graph.cells.iter().filter(|c| !c.stair) {
            *flat.entry(c.wt.1).or_insert(0) += 1;
        }
        let mut floors: Vec<i32> =
            flat.iter().filter(|(_, n)| **n >= MIN_FLOOR_CELLS).map(|(y, _)| *y).collect();
        if floors.is_empty() {
            floors = flat.keys().copied().collect();
        }
        floors.sort_unstable();

        Analysis {
            design,
            level,
            world,
            nav,
            issues,
            graph,
            at,
            label,
            main,
            spawn_comp,
            platform,
            floors,
        }
    }

    fn letter(i: usize) -> char {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"
            .get(i)
            .map(|&c| c as char)
            .unwrap_or('?')
    }

    fn room_name(&self, i: usize) -> String {
        self.level.rooms[i].name.clone()
    }

    // ─── The derived room graph ─────────────────────────────────────────────

    /// Room-to-room connections read off the walkable grid: two rooms connect when a
    /// walkable move joins them directly, or when both touch the same connector run.
    ///
    /// Also returns the independent-loop count, computed on the graph **with connectors
    /// as nodes of their own** (E - V + C). Collapsing connectors into room-to-room edges
    /// gets loops wrong both ways: two parallel halls between the same pair of rooms are
    /// a real second route but dedupe to one edge, and one corridor serving three rooms
    /// becomes a triangle -- a loop that is not there.
    fn derived_graph(&self) -> (BTreeSet<(usize, usize)>, i64) {
        // Connectors: union-find over moves between unlabelled cells.
        let n = self.graph.cells.len();
        let mut parent: Vec<u32> = (0..n as u32).collect();
        fn find(p: &mut [u32], mut i: u32) -> u32 {
            while p[i as usize] != i {
                p[i as usize] = p[p[i as usize] as usize];
                i = p[i as usize];
            }
            i
        }
        for &(a, b) in &self.graph.moves {
            if self.label[a as usize].is_none() && self.label[b as usize].is_none() {
                let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
                if ra != rb {
                    parent[ra as usize] = rb;
                }
            }
        }
        let node = |p: &mut [u32], i: u32| match self.label[i as usize] {
            Some(r) => Node::Room(r),
            None => Node::Connector(find(p, i)),
        };
        let mut edges = BTreeSet::new();
        let mut via: HashMap<u32, BTreeSet<usize>> = HashMap::new();
        for &(a, b) in &self.graph.moves {
            let (na, nb) = (node(&mut parent, a), node(&mut parent, b));
            match (na, nb) {
                (Node::Room(x), Node::Room(y)) if x != y => {
                    edges.insert((x.min(y), x.max(y)));
                }
                (Node::Room(r), Node::Connector(c)) | (Node::Connector(c), Node::Room(r)) => {
                    via.entry(c).or_default().insert(r);
                }
                _ => {}
            }
        }
        for rooms in via.values() {
            let v: Vec<usize> = rooms.iter().copied().collect();
            for i in 0..v.len() {
                for j in i + 1..v.len() {
                    edges.insert((v[i], v[j]));
                }
            }
        }

        // Loops, on rooms + connectors: each directly-touching room pair is one edge,
        // each (connector, room) contact is one edge, and every room with floor is a node
        // even if nothing connects to it.
        let mut all: Vec<(Node, Node)> = Vec::new();
        let mut direct = BTreeSet::new();
        for &(a, b) in &self.graph.moves {
            if let (Some(x), Some(y)) = (self.label[a as usize], self.label[b as usize]) {
                if x != y {
                    direct.insert((x.min(y), x.max(y)));
                }
            }
        }
        all.extend(direct.into_iter().map(|(x, y)| (Node::Room(x), Node::Room(y))));
        for (c, rooms) in &via {
            all.extend(rooms.iter().map(|r| (Node::Connector(*c), Node::Room(*r))));
        }
        let mut nodes: BTreeSet<Node> = self.label.iter().flatten().map(|r| Node::Room(*r)).collect();
        for (a, b) in &all {
            nodes.insert(*a);
            nodes.insert(*b);
        }
        let ids: HashMap<Node, usize> = nodes.iter().enumerate().map(|(i, n)| (*n, i)).collect();
        let mut up: Vec<usize> = (0..nodes.len()).collect();
        fn top(p: &mut [usize], mut i: usize) -> usize {
            while p[i] != i {
                p[i] = p[p[i]];
                i = p[i];
            }
            i
        }
        for (a, b) in &all {
            let (ra, rb) = (top(&mut up, ids[a]), top(&mut up, ids[b]));
            if ra != rb {
                up[ra] = rb;
            }
        }
        let groups = (0..nodes.len()).filter(|&i| top(&mut up, i) == i).count();
        let loops = all.len() as i64 - nodes.len() as i64 + groups as i64;
        (edges, loops)
    }

    /// Pairs of carved rooms whose air boxes share a face (or overlap) — no wall survives
    /// between them, so they are one space whether or not anyone meant it.
    fn merged_pairs(&self) -> Vec<(usize, usize)> {
        let rooms = &self.level.rooms;
        let mut out = Vec::new();
        for i in 0..rooms.len() {
            for j in i + 1..rooms.len() {
                if self.platform[i] || self.platform[j] {
                    continue;
                }
                let (a, b) = (rooms[i].aabb, rooms[j].aabb);
                let span = |r: [f32; 6], k: usize| (r[k], r[k] + r[k + 3]);
                // Positive overlap on two axes and at least contact on the third.
                let mut strict = 0;
                let mut touching = 0;
                for k in 0..3 {
                    let ((a0, a1), (b0, b1)) = (span(a, k), span(b, k));
                    if a0 < b1 && b0 < a1 {
                        strict += 1;
                    } else if a0 <= b1 && b0 <= a1 {
                        touching += 1;
                    }
                }
                if strict == 3 || (strict == 2 && touching == 1) {
                    out.push((i, j));
                }
            }
        }
        out
    }

    // ─── Perches ────────────────────────────────────────────────────────────

    fn perches(&self) -> Vec<PerchRow> {
        let s = WORLD_SCALE;
        let mut out = Vec::new();
        for (pi, lab) in self.level.rooms.iter().enumerate() {
            if !self.platform[pi] {
                continue;
            }
            let [x, top, z, w, _, d] = lab.aabb;
            // Eye points around the deck's edge, 0.5 WT in, every 2 WT: where a player
            // stands to look down, not the middle of the slab that hides the floor below.
            let mut eyes = Vec::new();
            let (x0, x1, z0, z1) = (x + 0.5, x + w - 0.5, z + 0.5, z + d - 0.5);
            let mut t = x0;
            while t <= x1 {
                eyes.push((t, z0));
                eyes.push((t, z1));
                t += 2.0;
            }
            let mut t = z0;
            while t <= z1 {
                eyes.push((x0, t));
                eyes.push((x1, t));
                t += 2.0;
            }
            let eyes: Vec<Vec3> = eyes
                .into_iter()
                .map(|(ex, ez)| Vec3::new(ex * s, top * s + crate::character::EYE, ez * s))
                .collect();

            let mut overlooks = Vec::new();
            for (ri, room) in self.level.rooms.iter().enumerate() {
                if self.platform[ri] {
                    continue;
                }
                let drop = top - room.aabb[1];
                if drop < 3.0 {
                    continue;
                }
                let ry = room.aabb[1] as i32;
                let targets: Vec<Vec3> = self
                    .graph
                    .cells
                    .iter()
                    .enumerate()
                    .filter(|(i, c)| self.label[*i] == Some(ri) && c.wt.1 == ry)
                    .filter(|(_, c)| (c.wt.0 + c.wt.2) % 2 == 0)
                    .map(|(_, c)| c.pos + Vec3::Y * TARGET_BODY_M)
                    .collect();
                if targets.is_empty() {
                    continue;
                }
                let seen = targets
                    .iter()
                    .filter(|t| eyes.iter().any(|e| self.nav.los_clear(*e, **t)))
                    .count();
                if seen > 0 {
                    overlooks.push(Overlook {
                        room: room.name.clone(),
                        seen,
                        total: targets.len(),
                        drop: drop as i32,
                    });
                }
            }
            overlooks.sort_by(|a, b| {
                (b.seen as f32 / b.total as f32).total_cmp(&(a.seen as f32 / a.total as f32))
            });
            out.push(PerchRow {
                name: lab.name.clone(),
                top_y: top as i32,
                overlooks,
            });
        }
        out
    }

    // ─── Headroom + camp corners ────────────────────────────────────────────

    fn room_at(&self, i: usize) -> Option<String> {
        self.label[i].map(|r| self.room_name(r))
    }

    /// Keep one spot per [`CLUSTER_M`] cube, in the order given.
    fn cluster(&self, idx: impl Iterator<Item = (usize, i32)>, limit: usize) -> Vec<Spot> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for (i, value) in idx {
            let p = self.graph.cells[i].pos;
            let key = (
                (p.x / CLUSTER_M).floor() as i32,
                (p.y / CLUSTER_M).floor() as i32,
                (p.z / CLUSTER_M).floor() as i32,
            );
            if seen.insert(key) {
                let wt = self.graph.cells[i].wt;
                out.push(Spot {
                    wt: [wt.0, wt.1, wt.2],
                    room: self.room_at(i),
                    value,
                });
                if out.len() >= limit {
                    break;
                }
            }
        }
        out
    }

    fn headroom(&self) -> (usize, Vec<Spot>) {
        const CAP: i32 = 16;
        let mut low: Vec<(usize, i32)> = Vec::new();
        for (i, c) in self.graph.cells.iter().enumerate() {
            let clear = (1..=CAP)
                .find(|k| {
                    let m = c.pos + Vec3::Y * ((*k as f32 + 0.5) * WORLD_SCALE);
                    self.nav.is_solid_meters(m.x, m.y, m.z)
                })
                .unwrap_or(CAP);
            if clear < HEADROOM_COMFORT {
                low.push((i, clear));
            }
        }
        low.sort_by_key(|&(_, c)| c);
        (low.len(), self.cluster(low.into_iter(), 8))
    }

    /// Flat cells backed into a corner or alcove (1–2 same-level approaches), away from
    /// any stair — a stair tread's edge has the same shape and is not a hiding place.
    fn camp_corners(&self) -> Vec<Spot> {
        let mut spots: Vec<(usize, i32)> = Vec::new();
        for (i, c) in self.graph.cells.iter().enumerate() {
            if c.stair || Some(c.comp) != self.main {
                continue;
            }
            let (x, y, z) = c.wt;
            let mut open = 0;
            let mut near_stair = false;
            for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                if let Some(&n) = self.at.get(&(x + dx, y, z + dz)) {
                    open += 1;
                    near_stair |= self.graph.cells[n].stair;
                }
            }
            if (1..=2).contains(&open) && !near_stair {
                spots.push((i, open));
            }
        }
        spots.sort_by_key(|&(_, o)| o);
        self.cluster(spots.into_iter(), 8)
    }

    // ─── The data ───────────────────────────────────────────────────────────

    /// The whole report as data.
    pub fn data(&self) -> ReportData {
        let comps: Vec<usize> = self.nav.component_sizes().iter().map(|(_, n)| *n).collect();
        let rooms = &self.level.rooms;
        let mut cells_of = vec![0usize; rooms.len()];
        let mut comps_of: Vec<BTreeSet<u32>> = vec![BTreeSet::new(); rooms.len()];
        for (i, c) in self.graph.cells.iter().enumerate() {
            if let Some(r) = self.label[i] {
                cells_of[r] += 1;
                comps_of[r].insert(c.comp);
            }
        }
        let reachable = |r: usize| self.spawn_comp.is_some_and(|s| comps_of[r].contains(&s));

        let (derived, loops) = self.derived_graph();
        let degree = |r: usize| derived.iter().filter(|(a, b)| *a == r || *b == r).count();
        let room_rows: Vec<RoomRow> = rooms
            .iter()
            .enumerate()
            .map(|(i, r)| RoomRow {
                letter: Self::letter(i),
                name: r.name.clone(),
                kind: if self.platform[i] { "platform" } else { "room" },
                size: [r.aabb[3] as i32, r.aabb[5] as i32, r.aabb[4] as i32],
                floor_y: r.aabb[1] as i32,
                cells: cells_of[i],
                reachable: reachable(i),
                degree: degree(i),
            })
            .collect();

        let declared_set: BTreeSet<(usize, usize)> =
            self.level.edges.iter().map(|(a, b)| (a.0.min(b.0), a.0.max(b.0))).collect();
        let declared: Vec<DeclaredEdge> = self
            .level
            .edges
            .iter()
            .map(|(a, b)| {
                let key = (a.0.min(b.0), a.0.max(b.0));
                DeclaredEdge {
                    a: self.room_name(a.0),
                    b: self.room_name(b.0),
                    walkable: !comps_of[a.0].is_disjoint(&comps_of[b.0]),
                    direct: derived.contains(&key),
                }
            })
            .collect();
        let undeclared: Vec<[String; 2]> = derived
            .iter()
            .filter(|k| !declared_set.contains(k))
            .map(|(a, b)| [self.room_name(*a), self.room_name(*b)])
            .collect();
        let merged: Vec<[String; 2]> = self
            .merged_pairs()
            .into_iter()
            .filter(|k| !declared_set.contains(k))
            .map(|(a, b)| [self.room_name(a), self.room_name(b)])
            .collect();

        let live = cells_of.iter().filter(|&&n| n > 0).count();
        let dead_ends: Vec<String> = room_rows
            .iter()
            .filter(|r| live > 1 && r.cells > 0 && r.degree <= 1)
            .map(|r| r.name.clone())
            .collect();

        let floors: Vec<FloorRow> = self
            .floors
            .iter()
            .map(|&y| {
                let on: Vec<usize> = (0..self.graph.cells.len())
                    .filter(|&i| self.graph.cells[i].wt.1 == y && !self.graph.cells[i].stair)
                    .collect();
                let main = on.iter().filter(|&&i| Some(self.graph.cells[i].comp) == self.main).count();
                let names: BTreeSet<usize> = on.iter().filter_map(|&i| self.label[i]).collect();
                FloorRow {
                    y,
                    cells: on.len(),
                    main_pct: main as f32 / on.len().max(1) as f32 * 100.0,
                    rooms: names.into_iter().map(|r| self.room_name(r)).collect(),
                }
            })
            .collect();

        let perches = self.perches();
        let (cramped_cells, cramped) = self.headroom();
        let camp_corners = self.camp_corners();

        let nav: Vec<NavLineRow> = self
            .issues
            .lines()
            .into_iter()
            .map(|l| NavLineRow {
                severity: match l.sev {
                    NavSeverity::Ok => "ok",
                    NavSeverity::Info => "info",
                    NavSeverity::Warn => "warn",
                    NavSeverity::Error => "error",
                },
                text: l.text,
            })
            .collect();

        // ── The summary ──
        let mut checks = Vec::new();
        let mut check = |check, status, detail: String| checks.push(Check { check, status, detail });

        let islands = &comps[comps.len().min(1)..];
        check(
            "walkable",
            if comps.is_empty() || islands.iter().any(|&n| n > TINY_ISLAND_CELLS) {
                Status::Fail
            } else if islands.is_empty() {
                Status::Pass
            } else {
                Status::Warn
            },
            match comps.len() {
                0 => "no walkable floor at all".into(),
                1 => format!("1 walkable component ({} cells)", comps[0]),
                n => format!(
                    "{n} walkable components {:?} cells — islands > {TINY_ISLAND_CELLS} cells fail; see NAV",
                    comps
                ),
            },
        );
        let floorless: Vec<&str> = room_rows
            .iter()
            .filter(|r| r.cells == 0)
            .map(|r| r.name.as_str())
            .collect();
        let unreachable: Vec<&str> = room_rows
            .iter()
            .filter(|r| r.cells > 0 && !r.reachable)
            .map(|r| r.name.as_str())
            .collect();
        check(
            "reachable",
            if self.spawn_comp.is_none() || !unreachable.is_empty() || !floorless.is_empty() {
                Status::Fail
            } else {
                Status::Pass
            },
            if self.spawn_comp.is_none() {
                "the spawn marker is not on walkable floor".into()
            } else if unreachable.is_empty() && floorless.is_empty() {
                format!("all {} rooms/decks reachable from spawn", room_rows.len())
            } else {
                let mut parts = Vec::new();
                if !unreachable.is_empty() {
                    parts.push(format!("cut off: {}", unreachable.join(", ")));
                }
                if !floorless.is_empty() {
                    parts.push(format!(
                        "no standable floor (under 6 WT of headroom, or buried): {}",
                        floorless.join(", ")
                    ));
                }
                format!(
                    "{}/{} reachable — {}",
                    room_rows.len() - unreachable.len() - floorless.len(),
                    room_rows.len(),
                    parts.join("; ")
                )
            },
        );
        let broken: Vec<String> = declared
            .iter()
            .filter(|e| !e.walkable)
            .map(|e| format!("{}↔{}", e.a, e.b))
            .collect();
        check(
            "declared links",
            if !broken.is_empty() {
                Status::Fail
            } else if declared.is_empty() {
                Status::Info
            } else {
                Status::Pass
            },
            if declared.is_empty() {
                "no connections declared".into()
            } else if broken.is_empty() {
                format!("all {} declared connections are walkable", declared.len())
            } else {
                format!("not walkable: {}", broken.join(", "))
            },
        );
        check(
            "merged rooms",
            if merged.is_empty() { Status::Pass } else { Status::Warn },
            if merged.is_empty() {
                "every pair of undeclared rooms keeps a wall between them".into()
            } else {
                format!(
                    "no wall between (undeclared): {}",
                    merged.iter().map(|p| format!("{}+{}", p[0], p[1])).collect::<Vec<_>>().join(", ")
                )
            },
        );
        check(
            "loops",
            if live <= 1 {
                Status::Info
            } else if loops > 0 {
                Status::Pass
            } else {
                Status::Warn
            },
            format!(
                "{loops} independent loop(s) in the walkable graph; {} dead-end(s){}",
                dead_ends.len(),
                if dead_ends.is_empty() {
                    String::new()
                } else {
                    format!(": {}", dead_ends.join(", "))
                }
            ),
        );
        let best = perches
            .iter()
            .filter_map(|p| {
                p.overlooks
                    .first()
                    .map(|o| (p, o, o.seen as f32 / o.total as f32))
            })
            .max_by(|a, b| a.2.total_cmp(&b.2));
        check(
            "perches",
            match (&best, perches.is_empty()) {
                (_, true) => Status::Info,
                (Some((_, _, share)), _) if *share >= PERCH_MIN_SHARE => Status::Pass,
                _ => Status::Warn,
            },
            match (&best, perches.is_empty()) {
                (_, true) => "no platforms".into(),
                (Some((p, o, share)), _) => format!(
                    "best: {} sees {:.0}% of {} ({} deck(s))",
                    p.name,
                    share * 100.0,
                    o.room,
                    perches.len()
                ),
                _ => format!("none of {} deck(s) overlooks a lower room", perches.len()),
            },
        );
        check(
            "headroom",
            if cramped_cells == 0 { Status::Pass } else { Status::Warn },
            if cramped_cells == 0 {
                format!("every walkable cell has ≥ {HEADROOM_COMFORT} WT (2 m) clearance")
            } else {
                format!("{cramped_cells} walkable cell(s) under {HEADROOM_COMFORT} WT clearance")
            },
        );
        check(
            "floors",
            Status::Info,
            format!(
                "{} floor(s) at y = {:?} WT",
                floors.len(),
                floors.iter().map(|f| f.y).collect::<Vec<_>>()
            ),
        );

        let verdict = checks.iter().map(|c| c.status).max().unwrap_or(Status::Pass);
        let verdict = if verdict == Status::Info { Status::Pass } else { verdict };

        ReportData {
            design: self.design.to_string(),
            verdict,
            checks,
            components: comps,
            nav,
            rooms: room_rows,
            declared,
            undeclared,
            merged,
            loops,
            dead_ends,
            floors,
            perches,
            cramped_cells,
            cramped,
            camp_corners,
        }
    }

    // ─── The text ───────────────────────────────────────────────────────────

    /// The full text report: summary first, detail after.
    pub fn report(&self) -> String {
        let d = self.data();
        let mut s = String::new();
        let rule = |s: &mut String, title: &str| {
            let _ = writeln!(s, "\n---------------- {title} ----------------");
        };

        let _ = writeln!(s, "==================== LEVEL REPORT: {} ====================", d.design);
        let (fails, warns) = (
            d.checks.iter().filter(|c| c.status == Status::Fail).count(),
            d.checks.iter().filter(|c| c.status == Status::Warn).count(),
        );
        let _ = writeln!(s, "VERDICT: {}   ({fails} fail, {warns} warn)", d.verdict.tag());
        for c in &d.checks {
            let _ = writeln!(s, "  [{}] {:<15} {}", c.status.tag(), c.check, c.detail);
        }

        rule(&mut s, "NAV (same as O → NAV → Calculate)");
        for l in &d.nav {
            let tag = match l.severity {
                "error" => "!! ",
                "warn" => " ! ",
                _ => "   ",
            };
            let _ = writeln!(s, "{tag}{}", l.text);
        }

        rule(&mut s, "ROOMS");
        let _ = writeln!(s, "  id name             kind      w×d×h      floor  cells  reach  links");
        for r in &d.rooms {
            let _ = writeln!(
                s,
                "  {}  {:<16} {:<9} {:>3}×{:<3}×{:<3} {:>5}  {:>5}  {:<5}  {}{}",
                r.letter,
                r.name,
                r.kind,
                r.size[0],
                r.size[1],
                r.size[2],
                r.floor_y,
                r.cells,
                if r.reachable { "yes" } else { "NO" },
                r.degree,
                if r.cells > 0 && r.degree <= 1 { "  (dead-end)" } else { "" }
            );
        }
        let _ = writeln!(s, "\n  declared connections ({}):", d.declared.len());
        for e in &d.declared {
            let _ = writeln!(
                s,
                "    {} {} ↔ {}{}",
                if e.walkable { "  " } else { "!!" },
                e.a,
                e.b,
                match (e.walkable, e.direct) {
                    (false, _) => "   — NOT walkable",
                    (true, false) => "   — walkable only by way of other rooms",
                    _ => "",
                }
            );
        }
        if !d.undeclared.is_empty() {
            let _ = writeln!(s, "  walkable but never declared ({}):", d.undeclared.len());
            for p in &d.undeclared {
                let _ = writeln!(s, "       {} ↔ {}", p[0], p[1]);
            }
        }
        for p in &d.merged {
            let _ = writeln!(s, "   !  {} and {} share air with no wall — one space, undeclared", p[0], p[1]);
        }

        rule(&mut s, "FLOORS");
        let _ = writeln!(
            s,
            "  `.` floor  `/` stairs & steps  `!` cut off from the main area  `#` wall  `S` spawn  letters = rooms (see ROOMS)"
        );
        for f in &d.floors {
            let _ = writeln!(
                s,
                "\n### floor y={} WT ({:.2} m): {} cells, {:.0}% in the main area — {}",
                f.y,
                f.y as f32 * WORLD_SCALE,
                f.cells,
                f.main_pct,
                if f.rooms.is_empty() { "no labelled room".into() } else { f.rooms.join(", ") }
            );
            self.floorplan(&mut s, f.y);
        }

        rule(&mut s, "PERCHES (sighted from the deck edge at eye height)");
        if d.perches.is_empty() {
            let _ = writeln!(s, "  (no platforms)");
        }
        for p in &d.perches {
            if p.overlooks.is_empty() {
                let _ = writeln!(s, "  {} (y={}): overlooks no lower room", p.name, p.top_y);
                continue;
            }
            let _ = writeln!(s, "  {} (y={}):", p.name, p.top_y);
            for o in &p.overlooks {
                let _ = writeln!(
                    s,
                    "      {:<16} {:>3.0}% ({}/{} sampled cells, {} WT below)",
                    o.room,
                    o.seen as f32 / o.total as f32 * 100.0,
                    o.seen,
                    o.total,
                    o.drop
                );
            }
        }

        rule(&mut s, "HEADROOM");
        if d.cramped_cells == 0 {
            let _ = writeln!(s, "  OK — every walkable cell has ≥ {HEADROOM_COMFORT} WT of head clearance.");
        } else {
            let _ = writeln!(s, "  {} cramped cell(s); tightest spots:", d.cramped_cells);
            for p in &d.cramped {
                let _ = writeln!(
                    s,
                    "      WT {:?}  clearance {} WT  {}",
                    p.wt,
                    p.value,
                    p.room.as_deref().unwrap_or("(corridor/stair)")
                );
            }
        }

        rule(&mut s, "CAMP CORNERS (flat, 1–2 approaches, clear of stairs)");
        for p in &d.camp_corners {
            let _ = writeln!(
                s,
                "      WT {:?}  approaches {}  {}",
                p.wt,
                p.value,
                p.room.as_deref().unwrap_or("(corridor)")
            );
        }
        s
    }

    /// One floor's ASCII plan, cropped to what is on it and downsampled if wide — keeping
    /// the most important glyph in each block, so a 1-WT stair or island never vanishes.
    fn floorplan(&self, s: &mut String, y: i32) {
        // Which floor each cell is drawn on: the highest floor at or below it.
        let floor_of = |cy: i32| {
            self.floors
                .iter()
                .rev()
                .find(|&&f| f <= cy)
                .or(self.floors.first())
                .copied()
                .unwrap_or(cy)
        };
        // Glyph priority: higher wins a downsampled block.
        let rank = |c: char| match c {
            ' ' => 0,
            '#' => 1,
            '.' => 2,
            '/' => 3,
            '!' => 4,
            'S' => 5,
            _ => 6, // a room letter
        };
        let mut glyph: HashMap<(i32, i32), char> = HashMap::new();
        let put = |g: &mut HashMap<(i32, i32), char>, k, c: char| {
            let e = g.entry(k).or_insert(c);
            if rank(c) > rank(*e) {
                *e = c;
            }
        };
        for c in &self.graph.cells {
            if floor_of(c.wt.1) != y {
                continue;
            }
            let off_main = Some(c.comp) != self.main;
            let ch = if off_main {
                '!'
            } else if c.stair || c.wt.1 != y {
                '/'
            } else {
                '.'
            };
            put(&mut glyph, (c.wt.0, c.wt.2), ch);
        }
        if glyph.is_empty() {
            return;
        }
        let spawn = (
            (self.level.spawn.x / WORLD_SCALE).floor() as i32,
            (self.level.spawn.y / WORLD_SCALE).round() as i32,
            (self.level.spawn.z / WORLD_SCALE).floor() as i32,
        );
        if self.at.contains_key(&spawn) && floor_of(spawn.1) == y {
            put(&mut glyph, (spawn.0, spawn.2), 'S');
        }
        for (i, r) in self.level.rooms.iter().enumerate() {
            if r.aabb[1] as i32 == y {
                let k = (
                    (r.aabb[0] + r.aabb[3] * 0.5).floor() as i32,
                    (r.aabb[2] + r.aabb[5] * 0.5).floor() as i32,
                );
                put(&mut glyph, k, Self::letter(i));
            }
        }

        let (mut x0, mut x1, mut z0, mut z1) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
        for &(x, z) in glyph.keys() {
            x0 = x0.min(x);
            x1 = x1.max(x);
            z0 = z0.min(z);
            z1 = z1.max(z);
        }
        let (x0, x1, z0, z1) = (x0 - 1, x1 + 1, z0 - 1, z1 + 1);
        let step = ((x1 - x0 + 1 + PLAN_MAX_COLS - 1) / PLAN_MAX_COLS).max(1);
        let _ = writeln!(
            s,
            "    x {x0}..{x1} →, z {z0}..{z1} ↓{}",
            if step > 1 { format!(", 1 char = {step}×{step} WT") } else { String::new() }
        );
        let wall = |x: i32, z: i32| {
            let (wx, wy, wz) = (x as f32 + 0.5, y as f32 + 2.0, z as f32 + 0.5);
            self.world.regions().iter().any(|r| r.solid_at(wx, wy, wz))
        };
        let mut z = z0;
        while z <= z1 {
            let mut line = String::from("    ");
            let mut x = x0;
            while x <= x1 {
                let mut best = ' ';
                for bz in z..(z + step).min(z1 + 1) {
                    for bx in x..(x + step).min(x1 + 1) {
                        let c = glyph.get(&(bx, bz)).copied().unwrap_or(' ');
                        if rank(c) > rank(best) {
                            best = c;
                        }
                    }
                }
                if best == ' ' && wall(x, z) {
                    best = '#';
                }
                line.push(best);
                x += step;
            }
            let _ = writeln!(s, "{}", line.trim_end());
            z += step;
        }
    }
}
