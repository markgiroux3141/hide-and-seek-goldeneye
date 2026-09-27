//! The static checks of `SPIKE_PD_COMPLEX.md` (S1–S4), measured the same way on
//! any two graphs in PD's format.

use glam::{Vec2, Vec3};

use super::pd_nav::{NavGraph, NavSeed};
use super::pd_tiles::{wpseg_get_id, PdStage, WPSEGFLAG_INWARDSONLY, WPSEGFLAG_OUTWARDSONLY};
use super::pdmath::Rng;
use super::sim::{drop_to_ground, Sim};
use super::tile_level::TileLevel;
use super::walk::{pd_pad_floor, walk_with_flags, Start};

/// S1: a PD pad (waypoint or spawn) not covered by the graph — no node within
/// 150 cm (XZ) on the same floor (±40 cm) that the pad can see.
#[derive(Clone, Debug)]
pub struct Miss {
    pub pad: usize,
    pub pos: Vec3,
    /// XZ distance to the nearest same-floor node, seen or not.
    pub nearest: f32,
}

/// The floor a PD pad belongs to (see [`pd_pad_floor`]), else the one under it.
fn pad_floor(level: &TileLevel, pad: Vec3) -> f32 {
    pd_pad_floor(level, pad).map_or_else(|| drop_to_ground(level, pad).y, |f| f.0)
}

pub fn s1_coverage(level: &TileLevel, stage: &PdStage, graph: &NavGraph) -> (Vec<Miss>, usize) {
    let mut pads: Vec<usize> = stage.pads.waypoints.iter().map(|w| w.padnum).collect();
    pads.extend(stage.spawn_pads.iter().copied());
    let mut misses = Vec::new();
    for &p in &pads {
        let pos = stage.pads.pads[p].pos;
        let floor = pad_floor(level, pos);
        let mut nearest = f32::INFINITY;
        let mut ok = false;
        for w in 0..graph.waypoints.len() {
            let n = graph.waypoint_pos(w);
            // Each node's own floor, found the same way for either graph.
            let nfloor = pad_floor(level, n);
            if (nfloor - floor).abs() > 40.0 {
                continue;
            }
            let d = Vec2::new(n.x - pos.x, n.z - pos.z).length();
            nearest = nearest.min(d);
            if d <= 150.0 && level.los(Vec3::new(pos.x, floor + 53.0, pos.z), Vec3::new(n.x, nfloor + 53.0, n.z)) {
                ok = true;
                break;
            }
        }
        if !ok {
            misses.push(Miss { pad: p, pos, nearest });
        }
    }
    (misses, pads.len())
}

/// S2: (weakly connected, strongly connected) component counts, links taken in
/// the directions PD's routing allows.
pub fn s2_components(graph: &NavGraph) -> (usize, usize) {
    let (w, comps) = s2_strong_components(graph);
    (w, comps.len())
}

/// S2 in detail: the weak count, and every strongly connected component (largest first).
pub fn s2_strong_components(graph: &NavGraph) -> (usize, Vec<Vec<usize>>) {
    let n = graph.waypoints.len();
    let fwd: Vec<Vec<usize>> = (0..n)
        .map(|a| {
            graph.waypoints[a]
                .neighbours
                .iter()
                .filter(|&&s| s & WPSEGFLAG_INWARDSONLY == 0)
                .map(|&s| wpseg_get_id(s))
                .filter(|&b| {
                    graph.waypoints[b].neighbours.iter().all(|&t| wpseg_get_id(t) != a || t & WPSEGFLAG_OUTWARDSONLY == 0)
                })
                .collect()
        })
        .collect();
    // Weak: union-find.
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut Vec<usize>, x: usize) -> usize {
        let mut r = x;
        while p[r] != r {
            r = p[r];
        }
        let mut c = x;
        while p[c] != r {
            let next = p[c];
            p[c] = r;
            c = next;
        }
        r
    }
    for a in 0..n {
        for &b in &fwd[a] {
            let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
            parent[ra] = rb;
        }
    }
    let weak = (0..n).filter(|&x| find(&mut parent, x) == x).count();
    // Strong: count distinct reach sets by forward+backward BFS (n is small).
    let reach = |start: usize, rev: bool| {
        let mut seen = vec![false; n];
        let mut stack = vec![start];
        seen[start] = true;
        while let Some(u) = stack.pop() {
            let next: Vec<usize> = if rev { (0..n).filter(|&v| fwd[v].contains(&u)).collect() } else { fwd[u].clone() };
            for v in next {
                if !seen[v] {
                    seen[v] = true;
                    stack.push(v);
                }
            }
        }
        seen
    };
    let mut comp = vec![usize::MAX; n];
    let mut comps: Vec<Vec<usize>> = Vec::new();
    for s in 0..n {
        if comp[s] != usize::MAX {
            continue;
        }
        let (f, b) = (reach(s, false), reach(s, true));
        let members: Vec<usize> = (0..n).filter(|&v| f[v] && b[v]).collect();
        for &v in &members {
            comp[v] = comps.len();
        }
        comps.push(members);
    }
    comps.sort_by_key(|c| std::cmp::Reverse(c.len()));
    (weak, comps)
}

/// S3: for every ordered pair of spawn pads, the length (cm, along the floor
/// points) of the route `nav_find_route` gives, from spawn to spawn. `None` when
/// there is no route.
pub fn s3_route_lengths(level: &TileLevel, stage: &PdStage, graph: &NavGraph) -> Vec<((usize, usize), Option<f32>)> {
    let spawns: Vec<Vec3> = stage.spawn_pads.iter().map(|&p| drop_to_ground(level, stage.pads.pads[p].pos)).collect();
    let mut rng = Rng::new(1);
    let mut out = Vec::new();
    for (i, &a) in spawns.iter().enumerate() {
        for (j, &b) in spawns.iter().enumerate() {
            if i == j {
                continue;
            }
            let rooms = |p: Vec3| level.floor_room(p, 20.0).into_iter().collect::<Vec<_>>();
            let from = graph.waypoint_find_closest_to_pos(level, a + Vec3::Y * 50.0, &rooms(a));
            let to = graph.waypoint_find_closest_to_pos(level, b + Vec3::Y * 50.0, &rooms(b));
            let len = match (from, to) {
                (Some(f), Some(t)) => {
                    let (route, _) = graph.nav_find_route(f, t, 100_000, NavSeed(1, 1), &mut rng);
                    if std::env::var("S3_DETAIL").ok().as_deref() == Some(&format!("{i},{j}")) {
                        let pts: Vec<String> = route.iter().map(|&w| { let p = graph.waypoint_pos(w); format!("{w:#x}({:.0},{:.0},{:.0})", p.x, p.y, p.z) }).collect();
                        println!("  route {i}->{j}: {}", pts.join(" "));
                    }
                    if route.last() != Some(&t) {
                        None
                    } else {
                        let mut pts = vec![a];
                        pts.extend(route.iter().map(|&w| graph.waypoint_pos(w) - Vec3::Y * 53.0));
                        pts.push(b);
                        Some(pts.windows(2).map(|w| w[0].distance(w[1])).sum())
                    }
                }
                _ => None,
            };
            out.push(((i, j), len));
        }
    }
    out
}

/// S4: walk every allowed directed link of `graph` with the real movement code.
/// Returns `(from, to, one_way, walk)`.
pub fn s4_walk(sim: &mut Sim, graph: &NavGraph) -> Vec<(usize, usize, bool, super::walk::Walk)> {
    let mut out = Vec::new();
    for a in 0..graph.waypoints.len() {
        for &s in &graph.waypoints[a].neighbours {
            let b = wpseg_get_id(s);
            if s & WPSEGFLAG_INWARDSONLY != 0 {
                continue;
            }
            let back = graph.waypoints[b].neighbours.iter().copied().find(|&t| wpseg_get_id(t) == a);
            if back.map_or(false, |t| t & WPSEGFLAG_OUTWARDSONLY != 0) {
                continue;
            }
            let one_way = s & WPSEGFLAG_OUTWARDSONLY != 0;
            let (pa, pb) = (graph.waypoint_pos(a), graph.waypoint_pos(b));
            // Our nodes stand 53 cm above verified floor points: start exactly there.
            let flags = graph.pads[graph.waypoints[b].padnum].flags;
            let w = walk_with_flags(sim, 0, Start::Floor(pa - Vec3::Y * 53.0), pb, Some(pb.y - 53.0), flags);
            out.push((a, b, one_way, w));
        }
    }
    out
}
