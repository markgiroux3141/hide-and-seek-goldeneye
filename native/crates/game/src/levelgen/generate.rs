//! The level **generator**: a seed in, a playable design out, written entirely with the
//! relational builder and judged by the same report an author reads.
//!
//! **Stage 6a — one floor.** Rooms with roles (one hero hall, halls and fight rooms of
//! mixed sizes, a long gallery, closets) are grown outward from the hero hall: each new
//! room is placed beside one already down, across a wall, and a door joins them. That
//! tree is then given loops — doors between rooms that ended up facing each other, and
//! L-corridors between ones that ended up diagonal — but only where the opening would
//! not cut through a third room. Pillars, spawn pads and weapons furnish it. Verticality
//! (stairs, stacked rooms, balconies) is stage 6b.
//!
//! **Best of N.** [`best_of`] builds a run of seeds, analyzes each one headlessly,
//! throws out any with a FAIL, and ranks the rest by [`score`]. Everything is
//! deterministic in the seed: `LEVELGEN_TRIES=1 LEVELGEN_SEED=<winner>` rebuilds the
//! winner exactly.
//!
//! The layout never guesses at geometry. Where a door goes, how far it reaches and which
//! boxes a corridor carves come from [`builder::door_box`] / [`builder::corridor_boxes`],
//! the functions the builder itself carves with.

use std::collections::{HashMap, VecDeque};

use super::analyze::{ReportData, Status};
use super::builder::{self, BuiltLevel, Dir, LevelBuilder, RoomId, DOOR_HEIGHT};

/// Knobs for one generated level.
#[derive(Clone, Copy, Debug)]
pub struct GenParams {
    /// Rooms to aim for, the hero hall included. A room that cannot be placed after
    /// [`PLACE_ATTEMPTS`] tries is skipped, so the result can come in a little under.
    pub rooms: usize,
    /// Extra connections beyond the tree — each one closes a loop.
    pub loops: usize,
}

impl Default for GenParams {
    fn default() -> Self {
        GenParams { rooms: 9, loops: 3 }
    }
}

/// Tries per room before giving up on it.
const PLACE_ATTEMPTS: usize = 80;
/// Rooms that are not joined keep at least this much solid between them (WT): enough for
/// a real wall, and what makes the report's `merged rooms` check pass by construction.
const CLEARANCE: f32 = 2.0;
/// Wall thicknesses a tree door is cut through. Mostly a plain wall; now and then a short
/// hall, which reads as a corridor between the two rooms.
const TREE_WALLS: [f32; 7] = [2.0, 2.0, 2.0, 3.0, 4.0, 6.0, 8.0];
/// A loop door only closes a gap this thin or thinner — past it, it is a tunnel.
const LOOP_MAX_GAP: f32 = 12.0;
/// Pillars stand at least this far inside a room's walls — clear of every door box
/// (which reaches 2 WT in) with a lane to spare, and never a 0.5 m slot at a wall.
const PILLAR_INSET: f32 = 5.0;
/// Weapons from worst to best. Farther rooms get better guns, so the good ones are worth
/// the walk (the pickups design: the player and the hunters start unarmed).
const WEAPON_TIERS: [&str; 5] = ["PP7", "KF7 Soviet", "Shotgun", "AR33", "RC-P90"];

// ─── Randomness ────────────────────────────────────────────────────────────────

/// A tiny xorshift64*, seeded through SplitMix64 so neighbouring seeds diverge at once.
/// The codebase rolls its own RNGs (hunter aim, PD bots) rather than take a dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        Rng((z ^ (z >> 31)) | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform in `[0, 1)`.
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    /// Uniform in `[lo, hi]`, on whole WT.
    fn span(&mut self, lo: f32, hi: f32) -> f32 {
        (lo + (hi - lo + 1.0) * self.unit()).floor().min(hi)
    }
    fn index(&mut self, n: usize) -> usize {
        ((self.unit() * n as f32) as usize).min(n.saturating_sub(1))
    }
    fn pick<T: Copy>(&mut self, v: &[T]) -> T {
        v[self.index(v.len())]
    }
}

// ─── The plan ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Role {
    Hero,
    Hall,
    Fight,
    Gallery,
    Closet,
}

impl Role {
    fn label(self) -> &'static str {
        match self {
            Role::Hero => "hero",
            Role::Hall => "hall",
            Role::Fight => "fight",
            Role::Gallery => "gallery",
            Role::Closet => "closet",
        }
    }

    /// Footprint + ceiling for a room of this role, WT. Sized to the design rules: the
    /// hero clears the 40 WT / 24 WT bar, nothing is under the 12 WT ceiling, and the
    /// gallery is long-skinny.
    fn size(self, rng: &mut Rng) -> (f32, f32, f32) {
        let (w, d, h) = match self {
            Role::Hero => (rng.span(40.0, 52.0), rng.span(32.0, 44.0), rng.span(24.0, 30.0)),
            Role::Hall => (rng.span(20.0, 28.0), rng.span(18.0, 26.0), rng.span(14.0, 18.0)),
            Role::Fight => (rng.span(14.0, 20.0), rng.span(12.0, 18.0), rng.span(12.0, 16.0)),
            Role::Gallery => (rng.span(6.0, 8.0), rng.span(28.0, 36.0), rng.span(12.0, 14.0)),
            Role::Closet => (rng.span(10.0, 12.0), rng.span(10.0, 12.0), 12.0),
        };
        // Half the galleries run east-west.
        if self == Role::Gallery && rng.unit() < 0.5 {
            (d, w, h)
        } else {
            (w, d, h)
        }
    }

    /// Doorway width into a room of this role.
    fn door(self) -> f32 {
        match self {
            Role::Hero | Role::Hall => 6.0,
            Role::Fight | Role::Gallery => 5.0,
            Role::Closet => 4.0,
        }
    }
}

#[derive(Clone, Debug)]
struct PlanRoom {
    role: Role,
    name: String,
    /// Label box `[x, y, z, w, h, d]`, floor at 0.
    aabb: [f32; 6],
    scheme: usize,
}

impl PlanRoom {
    /// Plan footprint `[x0, x1, z0, z1]`.
    fn rect(&self) -> [f32; 4] {
        let a = self.aabb;
        [a[0], a[0] + a[3], a[2], a[2] + a[5]]
    }
}

#[derive(Clone, Copy, Debug)]
enum Link {
    Door { a: usize, b: usize, width: f32 },
    Corridor { a: usize, b: usize, width: f32 },
}

impl Link {
    fn ends(self) -> (usize, usize) {
        match self {
            Link::Door { a, b, .. } | Link::Corridor { a, b, .. } => (a, b),
        }
    }
}

/// Whether two plan rectangles come within `margin` of each other.
fn near(a: [f32; 4], b: [f32; 4], margin: f32) -> bool {
    a[0] < b[1] + margin && b[0] < a[1] + margin && a[2] < b[3] + margin && b[2] < a[3] + margin
}

/// The role list for `n` rooms: the hero first, one gallery, a closet per four rooms,
/// the rest halls and fight rooms.
fn roles(n: usize, rng: &mut Rng) -> Vec<Role> {
    let mut v = vec![Role::Gallery];
    for _ in 0..(n / 4) {
        v.push(Role::Closet);
    }
    while v.len() + 1 < n {
        v.push(if rng.unit() < 0.5 { Role::Hall } else { Role::Fight });
    }
    // Shuffle (Fisher-Yates) so the gallery isn't always the first thing off the hero.
    for i in (1..v.len()).rev() {
        let j = rng.index(i + 1);
        v.swap(i, j);
    }
    v.insert(0, Role::Hero);
    v
}

/// Grow the rooms and the tree of doors that joins them.
fn grow(params: &GenParams, rng: &mut Rng) -> (Vec<PlanRoom>, Vec<Link>) {
    let mut rooms: Vec<PlanRoom> = Vec::new();
    let mut links: Vec<Link> = Vec::new();
    let mut degree: Vec<usize> = Vec::new();
    let mut counts: HashMap<Role, usize> = HashMap::new();
    let mut name = |role: Role| {
        let n = counts.entry(role).or_insert(0);
        *n += 1;
        if role == Role::Hero {
            "hero".to_string()
        } else {
            format!("{}_{}", role.label(), n)
        }
    };
    for role in roles(params.rooms.max(1), rng) {
        let (w, d, h) = role.size(rng);
        if rooms.is_empty() {
            rooms.push(PlanRoom {
                role,
                name: name(role),
                aabb: [0.0, 0.0, 0.0, w, h, d],
                scheme: rng.index(9),
            });
            degree.push(0);
            continue;
        }
        for _ in 0..PLACE_ATTEMPTS {
            // A parent with room left on its walls; the hero hub gets more than its share.
            let open: Vec<usize> = (0..rooms.len()).filter(|&i| degree[i] < 4).collect();
            if open.is_empty() {
                break;
            }
            let parent = if rng.unit() < 0.35 { 0 } else { rng.pick(&open) };
            let dir = rng.pick(&[Dir::North, Dir::South, Dir::East, Dir::West]);
            let wall = rng.pick(&TREE_WALLS);
            let width = role.door().min(rooms[parent].role.door());
            let need = builder::wall_needed(width) + 1.0;
            let pa = rooms[parent].aabb;
            // The side lengths that must share `need` of wall.
            let (p_len, n_len) = match dir {
                Dir::East | Dir::West => (pa[5], d),
                Dir::North | Dir::South => (pa[3], w),
            };
            if p_len < need || n_len < need {
                continue;
            }
            let along = rng.span(need - n_len, p_len - need);
            let (x, z) = builder::beside_origin(pa, dir, wall, along, w, d);
            let cand = PlanRoom {
                role,
                name: String::new(),
                aabb: [x, 0.0, z, w, h, d],
                scheme: 0,
            };
            let clear = rooms
                .iter()
                .enumerate()
                .all(|(i, r)| i == parent || !near(cand.rect(), r.rect(), CLEARANCE));
            if !clear {
                continue;
            }
            let i = rooms.len();
            rooms.push(PlanRoom {
                name: name(role),
                scheme: rng.index(9),
                ..cand
            });
            degree.push(1);
            degree[parent] += 1;
            links.push(Link::Door { a: parent, b: i, width });
            break;
        }
    }
    (rooms, links)
}

/// Rooms joined by `links`, as adjacency lists.
fn adjacency(n: usize, links: &[Link]) -> Vec<Vec<usize>> {
    let mut adj = vec![Vec::new(); n];
    for l in links {
        let (a, b) = l.ends();
        adj[a].push(b);
        adj[b].push(a);
    }
    adj
}

/// Hops from `from` to every room over `links` (`usize::MAX` = unreachable).
fn hops(n: usize, links: &[Link], from: usize) -> Vec<usize> {
    let adj = adjacency(n, links);
    let mut dist = vec![usize::MAX; n];
    let mut q = VecDeque::from([from]);
    dist[from] = 0;
    while let Some(i) = q.pop_front() {
        for &j in &adj[i] {
            if dist[j] == usize::MAX {
                dist[j] = dist[i] + 1;
                q.push_back(j);
            }
        }
    }
    dist
}

/// Close loops: add doors between rooms that face each other across a thin wall, and
/// L-corridors between diagonal ones, wherever the carve would not pass through (or
/// come within a wall of) any third room. Longer loops first — a door between two rooms
/// already three hops apart makes a real alternative route; one between neighbours of
/// the same room is barely a loop at all.
fn close_loops(rooms: &[PlanRoom], links: &mut Vec<Link>, loops: usize, rng: &mut Rng) {
    let n = rooms.len();
    let mut cands: Vec<(usize, f32, Link)> = Vec::new();
    for a in 0..n {
        let dist = hops(n, links, a);
        for b in a + 1..n {
            if dist[b] < 2 {
                continue; // already joined, or joined through one room
            }
            let width = rooms[a].role.door().min(rooms[b].role.door());
            let (la, lb) = (rooms[a].aabb, rooms[b].aabb);
            let (link, boxes) = match builder::facing(la, lb) {
                Ok(f) if f.gap <= LOOP_MAX_GAP => match builder::door_box(la, lb, 0.5, width, DOOR_HEIGHT) {
                    Ok(bx) => (Link::Door { a, b, width }, vec![bx]),
                    Err(_) => continue,
                },
                Ok(_) => continue,
                Err(_) => match builder::corridor_boxes(la, lb, width) {
                    Ok(bxs) if bxs.len() == 2 => (Link::Corridor { a, b, width }, bxs),
                    _ => continue,
                },
            };
            // The carve may touch `a` and `b` (it must) but nothing else.
            let clean = boxes.iter().all(|bx| {
                rooms
                    .iter()
                    .enumerate()
                    .all(|(i, r)| i == a || i == b || !near(bx.plan(), r.rect(), 1.0))
            });
            // …and an L's legs stay a sensible length.
            let length: f32 = boxes
                .iter()
                .map(|bx| bx.along.1 - bx.along.0)
                .sum();
            if clean && length <= 60.0 {
                cands.push((dist[b], rng.unit(), link));
            }
        }
    }
    cands.sort_by(|x, y| y.0.cmp(&x.0).then(x.1.total_cmp(&y.1)));
    let mut added = 0;
    for (_, _, link) in cands {
        if added >= loops {
            break;
        }
        // Re-check: an earlier loop may have joined these two already.
        let (a, b) = link.ends();
        if hops(n, links, a)[b] < 2 {
            continue;
        }
        links.push(link);
        added += 1;
    }
}

// ─── Building it ───────────────────────────────────────────────────────────────

/// Build the level for `seed`. Deterministic: the same seed and params always give the
/// same level.
pub fn build(seed: u64, params: &GenParams) -> BuiltLevel {
    let mut rng = Rng::new(seed);
    let (rooms, mut links) = grow(params, &mut rng);
    close_loops(&rooms, &mut links, params.loops, &mut rng);

    let mut b = LevelBuilder::new();
    let ids: Vec<RoomId> = rooms
        .iter()
        .map(|r| {
            b.set_scheme(r.scheme);
            let a = r.aabb;
            b.room(&r.name, a[0], a[2], a[3], a[5], a[1], a[4])
        })
        .collect();
    for l in &links {
        match *l {
            Link::Door { a, b: c, width } => b.door(ids[a], ids[c], width),
            Link::Corridor { a, b: c, width } => b.corridor(ids[a], ids[c], width),
        }
    }

    // Things standing on the floor, so pillars keep off them.
    let mut taken: Vec<(f32, f32)> = Vec::new();
    let centre = |r: &PlanRoom| (r.aabb[0] + r.aabb[3] * 0.5, r.aabb[2] + r.aabb[5] * 0.5);
    let jitter = |rng: &mut Rng, r: &PlanRoom| {
        let (cx, cz) = centre(r);
        let (hx, hz) = ((r.aabb[3] * 0.5 - 3.0).max(0.0), (r.aabb[5] * 0.5 - 3.0).max(0.0));
        (cx - hx + 2.0 * hx * rng.unit(), cz - hz + 2.0 * hz * rng.unit())
    };

    // The spawn marker in the hero hall, and a pad in every room.
    let (hx, hz) = centre(&rooms[0]);
    b.spawn_wt(hx, 0.0, hz);
    for r in &rooms {
        let (x, z) = jitter(&mut rng, r);
        b.spawn_pad(x, 0.0, z, rng.index(8) as f32 * 45.0);
        taken.push((x, z));
    }

    // Weapons, better the farther from the hero hall; two rooms in three get one.
    let dist = hops(rooms.len(), &links, 0);
    let far = dist.iter().copied().filter(|&d| d != usize::MAX).max().unwrap_or(0).max(1);
    for (i, r) in rooms.iter().enumerate() {
        if i != 0 && rng.unit() > 0.67 {
            continue;
        }
        let tier = (dist[i].min(far) * (WEAPON_TIERS.len() - 1) + far / 2) / far;
        let gun = WEAPON_TIERS[tier.min(WEAPON_TIERS.len() - 1)];
        let (x, z) = jitter(&mut rng, r);
        b.weapon(gun, x, 0.0, z);
        b.ammo(gun, x + 1.5, 0.0, z);
        taken.push((x, z));
        taken.push((x + 1.5, z));
    }

    // Cover pillars in the big rooms.
    for (i, r) in rooms.iter().enumerate() {
        let (w, d) = (r.aabb[3], r.aabb[5]);
        if w.min(d) < 18.0 {
            continue;
        }
        let want = if w * d >= 1200.0 { 4 } else { 2 };
        let mut placed: Vec<(f32, f32)> = Vec::new();
        for _ in 0..want * 12 {
            if placed.len() >= want {
                break;
            }
            let size = rng.pick(&[2.0, 3.0]);
            let x = rng.span(r.aabb[0] + PILLAR_INSET, r.aabb[0] + w - PILLAR_INSET - size);
            let z = rng.span(r.aabb[2] + PILLAR_INSET, r.aabb[2] + d - PILLAR_INSET - size);
            let (px, pz) = (x + size * 0.5, z + size * 0.5);
            let crowded = placed
                .iter()
                .chain(&taken)
                .any(|&(ox, oz)| (ox - px).abs() < 5.0 && (oz - pz).abs() < 5.0);
            if !crowded {
                b.pillar_in(ids[i], x, z, size);
                placed.push((px, pz));
            }
        }
    }
    b.finish()
}

// ─── Choosing ──────────────────────────────────────────────────────────────────

/// How good a level is, or `None` if it has a FAIL.
///
/// In order of weight: loops and a low share of dead-ends (the multiplayer rules —
/// "loops, not trees"), clean checks and design rules, room-size variety, room count, and
/// a hair of walkable area to break ties. Every term is read off the report's data, never
/// its prose, and each is bounded so no one of them can buy its way past a FAIL-free
/// level that does everything else better.
pub fn score(d: &ReportData) -> Option<f32> {
    if d.verdict == Status::Fail {
        return None;
    }
    let rooms = d.rooms.len().max(1) as f32;
    let warns = d.checks.iter().chain(&d.lints).filter(|c| c.status == Status::Warn).count() as f32;
    let rules = d.lints.iter().filter(|c| c.status == Status::Pass).count() as f32;
    let cells: usize = d.components.iter().sum();
    Some(
        10.0 * d.loops.clamp(0, 4) as f32 - 15.0 * d.dead_ends.len() as f32 / rooms - 4.0 * warns
            + 2.0 * rules
            + 6.0 * d.area_spread.min(1.5)
            + rooms
            + (cells as f32 / 1000.0).min(5.0),
    )
}

/// One scored try.
pub struct Candidate {
    pub seed: u64,
    pub built: BuiltLevel,
    /// `None` when the level has no walkable floor at all.
    pub report: Option<ReportData>,
    pub score: Option<f32>,
}

/// Build `tries` seeds from `base`, analyze each, and return them best first (anything
/// with a FAIL last, unscored).
pub fn best_of(base: u64, tries: usize, params: &GenParams) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = (0..tries.max(1) as u64)
        .map(|i| {
            let seed = base + i;
            let built = build(seed, params);
            let report = super::analyze_built(&format!("gen-{seed}"), &built);
            let score = report.as_ref().and_then(score);
            Candidate { seed, built, report, score }
        })
        .collect();
    out.sort_by(|a, b| match (a.score, b.score) {
        (Some(x), Some(y)) => y.total_cmp(&x),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.seed.cmp(&b.seed),
    });
    out
}
