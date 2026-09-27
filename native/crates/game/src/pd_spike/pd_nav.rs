//! `padhalllv.c` — PD's route finding, ported function by function, over a graph
//! in PD's own format ([`NavGraph`]: pads, waypoints, waygroups).
//!
//! The same code routes on any graph in this format, which is the point of the
//! Complex step: PD's hand-placed graph and our generated one both become a
//! `NavGraph`, so the graph is the only thing that differs between them.
//!
//! How PD routes (`padhalllv.c:15-41`): Dijkstra with a cost of 1 per segment,
//! first between waygroups, then between waypoints inside each group along that
//! group route, writing at most `arrlen - 1` waypoints plus a terminator. Ties
//! between equally short paths are broken at random, or, when the caller has set
//! a nav seed (`chr_go_to_room_pos` always does), by that seed.
//!
//! State PD keeps in the structs (`step`) lives in `Cell`s here, so routing needs
//! only `&NavGraph`.
//!
//! SUBSTITUTIONS: PD finds a pad's room with the BSP (`setup_prepare_pads`) and a
//! room's neighbours through its portals (`bg_room_get_neighbours`); here a pad's
//! room is the room of the floor under it and neighbours come from
//! [`TileLevel::rooms_are_neighbours`]. Candidate waypoints are gathered by room
//! exactly as PD does.

use std::cell::Cell;
use std::collections::BTreeMap;

use glam::Vec3;

use super::pd_tiles::{wpseg_get_id, PdStage, WPSEGFLAG_INWARDSONLY, WPSEGFLAG_OUTWARDSONLY};
use super::pdmath::Rng;
use super::tile_level::{CdResult, TileLevel};

/// `MAX_CHRWAYPOINTS` (`constants.h:20`).
pub const MAX_CHRWAYPOINTS: usize = 6;
const IGNORE_NONE: i32 = 0;
const IGNORE_OUTWARDS: i32 = WPSEGFLAG_OUTWARDSONLY;
const IGNORE_INWARDS: i32 = WPSEGFLAG_INWARDSONLY;

/// The `PADFLAG_AI*` bits the go-to code reads (`chr_tick_gopos`).
#[derive(Clone, Copy, Debug, Default)]
pub struct PadFlags {
    /// `PADFLAG_AIWALKDIRECT`: don't skip past this pad.
    pub walkdirect: bool,
    /// `PADFLAG_AICROUCH` / `PADFLAG_AIDUCK`: heading here, crouch / duck.
    pub crouch: bool,
    pub duck: bool,
}

#[derive(Clone, Debug)]
pub struct NavPad {
    pub pos: Vec3,
    pub room: Option<u16>,
    pub flags: PadFlags,
}

/// `struct waypoint`: `neighbours` in PD's encoding (id | `WPSEGFLAG_*`).
#[derive(Clone, Debug)]
pub struct NavWaypoint {
    pub padnum: usize,
    pub neighbours: Vec<i32>,
    pub groupnum: usize,
}

/// `struct waygroup`.
#[derive(Clone, Debug)]
pub struct NavWaygroup {
    pub neighbours: Vec<i32>,
    pub waypoints: Vec<usize>,
}

/// The nav seed (`g_NavSeed`, `nav_set_seed`): `(0, 0)` means "use `random()`".
#[derive(Clone, Copy, Debug, Default)]
pub struct NavSeed(pub u32, pub u32);

#[derive(Debug)]
pub struct NavGraph {
    pub pads: Vec<NavPad>,
    pub waypoints: Vec<NavWaypoint>,
    pub waygroups: Vec<NavWaygroup>,
    /// `g_Rooms[room].firstwaypoint/numwaypoints`: waypoints by their pad's room.
    room_waypoints: BTreeMap<u16, Vec<usize>>,
    wp_step: Vec<Cell<i32>>,
    group_step: Vec<Cell<i32>>,
}

impl NavGraph {
    pub fn new(pads: Vec<NavPad>, waypoints: Vec<NavWaypoint>, waygroups: Vec<NavWaygroup>) -> Self {
        let mut room_waypoints: BTreeMap<u16, Vec<usize>> = BTreeMap::new();
        for (i, w) in waypoints.iter().enumerate() {
            if let Some(r) = pads[w.padnum].room {
                room_waypoints.entry(r).or_default().push(i);
            }
        }
        let wp_step = (0..waypoints.len()).map(|_| Cell::new(-1)).collect();
        let group_step = (0..waygroups.len()).map(|_| Cell::new(-1)).collect();
        NavGraph { pads, waypoints, waygroups, room_waypoints, wp_step, group_step }
    }

    /// PD's own graph for a stage. SUBSTITUTION for `setup_prepare_pads`' BSP
    /// lookup: a pad's room is the room of the floor under the pad.
    pub fn from_pd_stage(stage: &PdStage, level: &TileLevel) -> Self {
        let pads = stage
            .pads
            .pads
            .iter()
            .map(|p| NavPad { pos: p.pos, room: level.floor_room(p.pos, 20.0), flags: p.flags })
            .collect();
        let waypoints = stage
            .pads
            .waypoints
            .iter()
            .map(|w| NavWaypoint { padnum: w.padnum, neighbours: w.neighbours.clone(), groupnum: w.groupnum })
            .collect();
        let waygroups = stage
            .pads
            .waygroups
            .iter()
            .map(|g| NavWaygroup { neighbours: g.neighbours.clone(), waypoints: g.waypoints.clone() })
            .collect();
        Self::new(pads, waypoints, waygroups)
    }

    /// A graph with no waypoints at all: every go-to fails.
    pub fn empty() -> Self {
        Self::new(Vec::new(), Vec::new(), Vec::new())
    }

    pub fn waypoint_pos(&self, w: usize) -> Vec3 {
        self.pads[self.waypoints[w].padnum].pos
    }

    pub fn waypoint_room(&self, w: usize) -> Option<u16> {
        self.pads[self.waypoints[w].padnum].room
    }

    // ─── Candidate search ────────────────────────────────────────────────────

    /// `waypoint_find_closest_to_pos` (`padhalllv.c:74`): the ten nearest waypoints
    /// in `rooms` and their neighbouring rooms, nearest first; the first with no
    /// floor in the way (`cd_test_los_oobfail`, floors only) and a clear line to
    /// its pad (`cd_test_cylmove_oobfail_findclosest`, zero height) wins. Failing
    /// that, the first whose blocking edge can be stepped round; failing that, the
    /// nearest.
    pub fn waypoint_find_closest_to_pos(&self, level: &TileLevel, pos: Vec3, rooms: &[u16]) -> Option<usize> {
        let mut allrooms: Vec<u16> = rooms.to_vec();
        for &r in rooms {
            for n in level.room_neighbours(r) {
                if !allrooms.contains(&n) {
                    allrooms.push(n);
                }
            }
        }
        // Candidates sorted by distance, at most 10 (insertion as PD does it).
        let mut cands: Vec<(usize, f32)> = Vec::new();
        for r in &allrooms {
            for &w in self.room_waypoints.get(r).map_or(&[][..], |v| v.as_slice()) {
                let sqdist = pos.distance_squared(self.waypoint_pos(w));
                let index = cands.iter().position(|&(_, d)| sqdist < d).unwrap_or(cands.len());
                if index < 10 {
                    cands.insert(index, (w, sqdist));
                    cands.truncate(10);
                }
            }
        }
        let mut checkmore: Vec<Option<(Vec3, Vec3)>> = vec![None; cands.len()];
        for (i, &(w, _)) in cands.iter().enumerate() {
            let padpos = self.waypoint_pos(w);
            if !level.los_floors(pos, padpos) {
                continue;
            }
            let (r, edge) = level.cd_test_cylmove_oobfail_findclosest(pos, padpos, 20.0, 0.0, 0.0, &[]);
            match r {
                CdResult::Error => {}
                CdResult::Collision => checkmore[i] = edge,
                CdResult::NoCollision => return Some(w),
            }
        }
        // No line of sight to any: step round the first blocking edge that allows it.
        for (i, &(_, _)) in cands.iter().enumerate() {
            let Some((a, b)) = checkmore[i] else { continue };
            if a.x == b.x && a.z == b.z {
                continue;
            }
            let d = Vec3::new(a.x - b.x, 0.0, a.z - b.z);
            let d = d * (10.0 / (d.x * d.x + d.z * d.z).sqrt());
            for tmppos in [Vec3::new(a.x + d.x, pos.y, a.z + d.z), Vec3::new(b.x - d.x, pos.y, b.z - d.z)] {
                if level.cd_test_cylmove_oobok(pos, tmppos, 0.0, 0.0, &[]) != CdResult::Collision {
                    return Some(cands[i].0);
                }
            }
        }
        cands.first().map(|c| c.0)
    }

    // ─── Tie-breaks ──────────────────────────────────────────────────────────

    /// The 50% "keep looking" coin both `*_choose_neighbour` functions and
    /// `waypoint_find_segment_into_group` flip. With a nav seed set, PD rotates a
    /// *copy* of the seed each time, so every flip in one routing call lands the
    /// same way: all take the first match, or all the last.
    fn stop_here(seed: NavSeed, rng: &mut Rng) -> bool {
        if seed.0 == 0 && seed.1 == 0 {
            rng.random() % 2 == 0
        } else {
            let mut s = ((seed.0 as u64) << 32) | seed.1 as u64;
            Rng::rotate_seed(&mut s) % 2 == 0
        }
    }

    // ─── Group level ─────────────────────────────────────────────────────────

    /// `waygroup_choose_neighbour` (`padhalllv.c:251`).
    fn waygroup_choose_neighbour(&self, groupnums: &[i32], step: i32, ignoremask: i32, seed: NavSeed, rng: &mut Rng) -> Option<usize> {
        let mut best = None;
        for &g in groupnums {
            if g & ignoremask == 0 {
                let group = wpseg_get_id(g);
                if self.group_step[group].get() == step {
                    best = Some(group);
                    if Self::stop_here(seed, rng) {
                        break;
                    }
                }
            }
        }
        best
    }

    /// `waygroup_set_step_if_undiscovered` (`padhalllv.c:286`).
    fn waygroup_set_step_if_undiscovered(&self, groupnums: &[i32], step: i32, ignoremask: i32) {
        for &g in groupnums {
            if g & ignoremask == 0 {
                let s = &self.group_step[wpseg_get_id(g)];
                if s.get() < 0 {
                    s.set(step);
                }
            }
        }
    }

    /// `waygroup_discover_one_step` (`padhalllv.c:306`): one pass over every group.
    fn waygroup_discover_one_step(&self, step: i32, ignoremask: i32) -> bool {
        let mut discovered = false;
        for (g, group) in self.waygroups.iter().enumerate() {
            if self.group_step[g].get() == step {
                discovered = true;
                self.waygroup_set_step_if_undiscovered(&group.neighbours, step + 1, ignoremask);
            }
        }
        discovered
    }

    /// `waygroup_discover_steps` (`padhalllv.c:333`).
    fn waygroup_discover_steps(&self, from: usize, to: usize, discoverall: bool, ignoremask: i32) -> bool {
        for s in &self.group_step {
            s.set(-1);
        }
        self.group_step[from].set(0);
        let mut result = true;
        let mut step = 0;
        while (discoverall || self.group_step[to].get() < 0) && result {
            result = self.waygroup_discover_one_step(step, ignoremask);
            step += 1;
        }
        result
    }

    /// `waygroup_find_route` (`padhalllv.c:358`): marks the chosen group route with
    /// steps ≥ 10000.
    fn waygroup_find_route(&self, from: usize, to: usize, seed: NavSeed, rng: &mut Rng) -> bool {
        let result = self.waygroup_discover_steps(from, to, false, IGNORE_INWARDS);
        if result {
            let mut curto = to;
            let mut step = self.group_step[curto].get() - 1;
            while step >= 0 {
                self.group_step[curto].set(self.group_step[curto].get() + 10000);
                match self.waygroup_choose_neighbour(&self.waygroups[curto].neighbours, step, IGNORE_OUTWARDS, seed, rng) {
                    Some(g) => curto = g,
                    None => return result,
                }
                step -= 1;
            }
            self.group_step[curto].set(self.group_step[curto].get() + 10000);
        }
        result
    }

    // ─── Waypoint level ──────────────────────────────────────────────────────

    /// `waypoint_choose_neighbour` (`padhalllv.c:397`).
    fn waypoint_choose_neighbour(
        &self,
        pointnums: &[i32],
        step: i32,
        groupnum: usize,
        ignoremask: i32,
        seed: NavSeed,
        rng: &mut Rng,
    ) -> Option<usize> {
        let mut best = None;
        for &p in pointnums {
            if p & ignoremask == 0 {
                let point = wpseg_get_id(p);
                if self.waypoints[point].groupnum == groupnum && self.wp_step[point].get() == step {
                    best = Some(point);
                    if Self::stop_here(seed, rng) {
                        break;
                    }
                }
            }
        }
        best
    }

    /// `waypoint_set_step_if_undiscovered` (`padhalllv.c:434`).
    fn waypoint_set_step_if_undiscovered(&self, pointnums: &[i32], value: i32, groupnum: usize, ignoremask: i32) {
        for &p in pointnums {
            if p & ignoremask == 0 {
                let point = wpseg_get_id(p);
                if self.waypoints[point].groupnum == groupnum && self.wp_step[point].get() < 0 {
                    self.wp_step[point].set(value);
                }
            }
        }
    }

    /// `waypoint_discover_one_step` (`padhalllv.c:455`): one pass over the group.
    fn waypoint_discover_one_step(&self, groupnum: usize, step: i32, ignoremask: i32) -> bool {
        let mut result = false;
        for &p in &self.waygroups[groupnum].waypoints {
            if self.wp_step[p].get() == step {
                result = true;
                self.waypoint_set_step_if_undiscovered(&self.waypoints[p].neighbours, step + 1, groupnum, ignoremask);
            }
        }
        result
    }

    /// `waypoint_discover_steps` (`padhalllv.c:486`): `from` and `to` must share a group.
    fn waypoint_discover_steps(&self, from: usize, to: usize, discoverall: bool, ignoremask: i32) {
        let groupnum = self.waypoints[from].groupnum;
        for &p in &self.waygroups[groupnum].waypoints {
            self.wp_step[p].set(-1);
        }
        self.wp_step[from].set(0);
        let mut more = true;
        let mut i = 0;
        while (discoverall || self.wp_step[to].get() < 0) && more {
            more = self.waypoint_discover_one_step(groupnum, i, ignoremask);
            i += 1;
        }
    }

    /// `waypoint_find_route` (`padhalllv.c:514`): marks the route with steps ≥ 10000.
    fn waypoint_find_route(&self, from: usize, to: usize, seed: NavSeed, rng: &mut Rng) {
        self.waypoint_discover_steps(from, to, false, IGNORE_INWARDS);
        let groupnum = self.waypoints[from].groupnum;
        let mut value = self.wp_step[to].get() - 1;
        let mut curto = to;
        while value >= 0 {
            self.wp_step[curto].set(self.wp_step[curto].get() + 10000);
            match self.waypoint_choose_neighbour(&self.waypoints[curto].neighbours, value, groupnum, IGNORE_OUTWARDS, seed, rng) {
                Some(p) => curto = p,
                None => return,
            }
            value -= 1;
        }
        self.wp_step[curto].set(self.wp_step[curto].get() + 10000);
    }

    /// `waypoint_collect_local` (`padhalllv.c:540`): append the in-group route
    /// `from → to` (at most `arrlen - 1` waypoints) to `arr`. Returns the count PD
    /// returns, which includes the NULL terminator.
    fn waypoint_collect_local(&self, from: usize, to: usize, arr: &mut Vec<usize>, arrlen: i32, seed: NavSeed, rng: &mut Rng) -> i32 {
        let before = arr.len();
        if arrlen >= 2 {
            self.waypoint_find_route(from, to, seed, rng);
            arr.push(from);
            let groupnum = self.waypoints[from].groupnum;
            let mut curfrom = from;
            let limit = arrlen + 9999;
            let mut step = 10001;
            while step <= self.wp_step[to].get() && step < limit {
                match self.waypoint_choose_neighbour(&self.waypoints[curfrom].neighbours, step, groupnum, IGNORE_INWARDS, seed, rng) {
                    Some(p) => {
                        curfrom = p;
                        arr.push(p);
                    }
                    None => break,
                }
                step += 1;
            }
        }
        (arr.len() - before) as i32 + 1
    }

    /// `waypoint_find_segment_into_group` (`padhalllv.c:574`): a link from a
    /// waypoint of `fromgroup` into `togroup`, the last one found unless the coin
    /// stops the search earlier.
    fn waypoint_find_segment_into_group(&self, fromgroup: usize, togroup: usize, seed: NavSeed, rng: &mut Rng) -> Option<(usize, usize)> {
        let mut found = None;
        'outer: for &fromwp in &self.waygroups[fromgroup].waypoints {
            for &n in &self.waypoints[fromwp].neighbours {
                if n & IGNORE_INWARDS == 0 {
                    let neighbour = wpseg_get_id(n);
                    if self.waypoints[neighbour].groupnum == togroup {
                        found = Some((fromwp, neighbour));
                        if Self::stop_here(seed, rng) {
                            // PD's `break` leaves only the inner loop.
                            continue 'outer;
                        }
                    }
                }
            }
        }
        found
    }

    /// `nav_find_route` (`padhalllv.c:630`): the route `frompoint → topoint`, at
    /// most `arrlen - 1` waypoints of it. Returns the waypoints and PD's count,
    /// which **includes the NULL terminator** — so PD's `numwaypoints > 1` means
    /// "at least one waypoint".
    pub fn nav_find_route(&self, frompoint: usize, topoint: usize, arrlen: usize, seed: NavSeed, rng: &mut Rng) -> (Vec<usize>, i32) {
        let mut arr = Vec::new();
        if self.waygroups.is_empty() {
            return (arr, 1);
        }
        let mut arrlen = arrlen as i32;
        let fromgroup = self.waypoints[frompoint].groupnum;
        let togroup = self.waypoints[topoint].groupnum;
        if self.waygroup_find_route(fromgroup, togroup, seed, rng) {
            let mut curfrompoint = frompoint;
            let mut curfromgroup = fromgroup;
            let mut step = self.group_step[fromgroup].get() + 1;
            while step <= self.group_step[togroup].get() && arrlen >= 2 {
                let Some(nextfromgroup) =
                    self.waygroup_choose_neighbour(&self.waygroups[curfromgroup].neighbours, step, IGNORE_INWARDS, seed, rng)
                else {
                    break;
                };
                let Some((lastwp, nextfirstwp)) = self.waypoint_find_segment_into_group(curfromgroup, nextfromgroup, seed, rng) else {
                    break;
                };
                let numwritten = self.waypoint_collect_local(curfrompoint, lastwp, &mut arr, arrlen, seed, rng) - 1;
                arrlen -= numwritten;
                curfrompoint = nextfirstwp;
                curfromgroup = nextfromgroup;
                step += 1;
            }
            self.waypoint_collect_local(curfrompoint, topoint, &mut arr, arrlen, seed, rng);
        }
        let count = arr.len() as i32 + 1;
        (arr, count)
    }

    /// Waypoint-level hop distances between every pair, ignoring groups (for the
    /// route-length comparison in the static checks). `None` = unreachable.
    pub fn hop_counts_from(&self, from: usize) -> Vec<Option<u32>> {
        let mut dist = vec![None; self.waypoints.len()];
        dist[from] = Some(0);
        let mut queue = std::collections::VecDeque::from([from]);
        while let Some(a) = queue.pop_front() {
            for &s in &self.waypoints[a].neighbours {
                let b = wpseg_get_id(s);
                if s & IGNORE_INWARDS == 0 && dist[b].is_none() {
                    dist[b] = Some(dist[a].unwrap() + 1);
                    queue.push_back(b);
                }
            }
        }
        let _ = IGNORE_NONE;
        dist
    }
}

/// `CHRNAVSEED(chr)` (`constants.h:59`): `(lvframe60 >> 9) * 128 + chrnum * 8`,
/// used for both halves of the seed — so a chr's routes are stable for ~8.5 s.
/// SUBSTITUTION: `chrnum` is the spike's chr index.
pub fn chrnavseed(lvframe60: i32, chrnum: usize) -> NavSeed {
    let v = ((lvframe60 >> 9) as u32).wrapping_mul(128).wrapping_add(chrnum as u32 * 8);
    NavSeed(v, v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ring of 6 waypoints in two groups (0-2 | 3-5), linked 0-1-2-3-4-5-0.
    fn ring() -> NavGraph {
        let pads = (0..6)
            .map(|k| {
                let a = k as f32 * std::f32::consts::TAU / 6.0;
                NavPad { pos: Vec3::new(a.sin() * 500.0, 50.0, a.cos() * 500.0), room: Some(0), flags: PadFlags::default() }
            })
            .collect();
        let waypoints = (0..6)
            .map(|k: usize| NavWaypoint {
                padnum: k,
                neighbours: vec![((k + 5) % 6) as i32, ((k + 1) % 6) as i32],
                groupnum: k / 3,
            })
            .collect();
        let waygroups =
            vec![NavWaygroup { neighbours: vec![1], waypoints: vec![0, 1, 2] }, NavWaygroup { neighbours: vec![0], waypoints: vec![3, 4, 5] }];
        NavGraph::new(pads, waypoints, waygroups)
    }

    #[test]
    fn routes_go_group_by_group_and_count_the_terminator() {
        let g = ring();
        let mut rng = Rng::new(1);
        let (r, n) = g.nav_find_route(1, 4, MAX_CHRWAYPOINTS, NavSeed(7, 7), &mut rng);
        assert_eq!(n as usize, r.len() + 1);
        assert_eq!(r.first(), Some(&1));
        assert_eq!(r.last(), Some(&4));
        for w in r.windows(2) {
            assert!(g.waypoints[w[0]].neighbours.contains(&(w[1] as i32)), "{r:?}");
        }
        // Same waypoint: just itself.
        assert_eq!(g.nav_find_route(2, 2, MAX_CHRWAYPOINTS, NavSeed(7, 7), &mut rng).0, vec![2]);
        // A short array keeps only the first `arrlen - 1` waypoints.
        let (short, n) = g.nav_find_route(0, 4, 3, NavSeed(7, 7), &mut rng);
        assert_eq!((short.len(), n), (2, 3));
    }

    #[test]
    fn a_one_way_link_is_only_used_in_its_direction() {
        let mut g = ring();
        // 2 → 3 outwards only (a ledge): 3's entry for 2 is inwards only.
        g.waypoints[2].neighbours = vec![1, 3 | WPSEGFLAG_OUTWARDSONLY];
        g.waypoints[3].neighbours = vec![2 | WPSEGFLAG_INWARDSONLY, 4];
        let g = NavGraph::new(g.pads, g.waypoints, g.waygroups);
        let mut rng = Rng::new(1);
        let (r, _) = g.nav_find_route(3, 2, 8, NavSeed(3, 3), &mut rng);
        assert!(!r.windows(2).any(|w| w == [3, 2]), "{r:?}");
    }
}
