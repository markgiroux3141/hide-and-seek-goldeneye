//! Stand-in for PD's hand-placed waypoints in the box arena: a regular grid over
//! the floor, turned into a PD-format graph ([`Waypoints::to_nav`]) that PD's own
//! routing ([`super::pd_nav`]) runs on. PD's MP maps space pads a few metres apart;
//! 250 cm is the default here. (Complex uses PD's real graph instead.)
//!
//! [`Waypoints::find_route`] is the old shortest-by-distance search, kept only
//! for the grid's own test; bots route with `nav_find_route`, which counts hops.

use std::collections::BinaryHeap;

use glam::{Vec2, Vec3};

use super::arena::Arena;
use super::pd_nav::{NavGraph, NavPad, NavWaygroup, NavWaypoint, PadFlags};
use super::tile_level::TileLevel;

pub struct Waypoints {
    pub pos: Vec<Vec3>,
    pub edges: Vec<Vec<usize>>,
    pub spacing: f32,
}

impl Waypoints {
    /// A grid at `spacing` cm, dropping points a chr can't stand on and linking
    /// 8-neighbours whose connecting corridor is clear.
    pub fn grid(arena: &Arena, spacing: f32) -> Self {
        let min = arena.bounds.min;
        let max = arena.bounds.max;
        let nx = ((max.x - min.x) / spacing).floor() as i32;
        let nz = ((max.y - min.y) / spacing).floor() as i32;
        let ox = min.x + ((max.x - min.x) - (nx - 1) as f32 * spacing) * 0.5;
        let oz = min.y + ((max.y - min.y) - (nz - 1) as f32 * spacing) * 0.5;
        let mut pos = Vec::new();
        let mut index = vec![vec![None; nz as usize]; nx as usize];
        for ix in 0..nx {
            for iz in 0..nz {
                let p = Vec2::new(ox + ix as f32 * spacing, oz + iz as f32 * spacing);
                if arena.is_clear(p, 30.0) {
                    index[ix as usize][iz as usize] = Some(pos.len());
                    pos.push(Vec3::new(p.x, 0.0, p.y));
                }
            }
        }
        let mut edges = vec![Vec::new(); pos.len()];
        for ix in 0..nx {
            for iz in 0..nz {
                let Some(a) = index[ix as usize][iz as usize] else { continue };
                for (dx, dz) in [(1, 0), (0, 1), (1, 1), (1, -1)] {
                    let (jx, jz) = (ix + dx, iz + dz);
                    if jx < 0 || jz < 0 || jx >= nx || jz >= nz {
                        continue;
                    }
                    let Some(b) = index[jx as usize][jz as usize] else { continue };
                    if arena.cylinder_path_clear(pos[a], pos[b], 20.0) {
                        edges[a].push(b);
                        edges[b].push(a);
                    }
                }
            }
        }
        Waypoints { pos, edges, spacing }
    }

    /// The grid as a PD-format graph: one pad per point, raised to PD's usual
    /// 53 cm above the floor, every point in one waygroup (the arena is one room).
    pub fn to_nav(&self, level: &TileLevel) -> NavGraph {
        let pads = self
            .pos
            .iter()
            .map(|p| {
                let pos = Vec3::new(p.x, p.y + 53.0, p.z);
                NavPad { pos, room: level.floor_room(pos, 20.0), flags: PadFlags::default() }
            })
            .collect();
        let waypoints = self
            .edges
            .iter()
            .enumerate()
            .map(|(k, e)| NavWaypoint { padnum: k, neighbours: e.iter().map(|&b| b as i32).collect(), groupnum: 0 })
            .collect();
        let waygroups = vec![NavWaygroup { neighbours: Vec::new(), waypoints: (0..self.pos.len()).collect() }];
        NavGraph::new(pads, waypoints, waygroups)
    }

    /// `waypoint_find_closest_to_pos`: the nearest waypoint with a clear line.
    pub fn closest_to(&self, level: &TileLevel, p: Vec3) -> Option<usize> {
        let mut best: Option<(f32, usize)> = None;
        for (i, w) in self.pos.iter().enumerate() {
            let d = (Vec2::new(w.x, w.z) - Vec2::new(p.x, p.z)).length_squared();
            if best.map_or(true, |(bd, _)| d < bd) && level.los(p + Vec3::Y * 50.0, *w + Vec3::Y * 50.0) {
                best = Some((d, i));
            }
        }
        best.map(|(_, i)| i)
    }

    /// `nav_find_route(from, to)`: the waypoint list including both ends, or empty.
    pub fn find_route(&self, from: usize, to: usize) -> Vec<usize> {
        if from == to {
            return vec![from];
        }
        #[derive(PartialEq)]
        struct Node(f32, usize);
        impl Eq for Node {}
        impl PartialOrd for Node {
            fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(o))
            }
        }
        impl Ord for Node {
            fn cmp(&self, o: &Self) -> std::cmp::Ordering {
                o.0.total_cmp(&self.0).then(o.1.cmp(&self.1))
            }
        }
        let n = self.pos.len();
        let mut dist = vec![f32::INFINITY; n];
        let mut prev = vec![usize::MAX; n];
        let mut heap = BinaryHeap::new();
        dist[from] = 0.0;
        heap.push(Node(0.0, from));
        while let Some(Node(d, u)) = heap.pop() {
            if u == to {
                break;
            }
            if d > dist[u] {
                continue;
            }
            for &v in &self.edges[u] {
                let nd = d + self.pos[u].distance(self.pos[v]);
                if nd < dist[v] {
                    dist[v] = nd;
                    prev[v] = u;
                    heap.push(Node(nd, v));
                }
            }
        }
        if prev[to] == usize::MAX {
            return Vec::new();
        }
        let mut route = vec![to];
        let mut c = to;
        while c != from {
            c = prev[c];
            route.push(c);
        }
        route.reverse();
        route
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_routes_around_pillars() {
        let arena = Arena::standard();
        let w = Waypoints::grid(&arena, 250.0);
        assert!(w.pos.len() > 20);
        let level = TileLevel::new(arena.geom());
        let a = w.closest_to(&level, Vec3::new(-700.0, 0.0, -700.0)).unwrap();
        let b = w.closest_to(&level, Vec3::new(700.0, 0.0, 700.0)).unwrap();
        let r = w.find_route(a, b);
        assert!(r.len() > 2, "{r:?}");
        for pair in r.windows(2) {
            assert!(arena.cylinder_path_clear(w.pos[pair[0]], w.pos[pair[1]], 20.0));
        }
    }
}
