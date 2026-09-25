//! The level **generator**: a seed in, a playable design out, written entirely with the
//! relational builder and judged by the same report an author reads.
//!
//! **Ground floor (stage 6a).** Rooms with roles (one hero hall, halls and fight rooms
//! of mixed sizes, a long gallery, closets) are grown outward from the hero hall: each
//! new room is placed beside one already down, across a wall, and a door joins them.
//! That tree is given loops — doors between rooms that ended up facing each other and
//! L-corridors between diagonal ones — only where the opening would not cut through a
//! third room. Pillars, spawn pads and weapons furnish it.
//!
//! **Verticality (stage 6b).** An **upper floor** is entered from a balcony along one of
//! the hero hall's walls: a stair up to the deck, a door off it into the first upper room
//! (the `grand` mezzanine pattern), more upper rooms grown from that one. A **second route
//! between floors** — a stair through an upper room's floor down into the ground room
//! beneath it — keeps the upstairs from being one long dead end ("a second route up/down
//! each vertical"). **Basements** are entered by a stair through a big ground room's
//! floor, and grow from there. Rooms on different floors may overlap on the plan: what
//! keeps them apart is [`SLAB`] of solid between one's ceiling and the other's floor.
//!
//! **Best of N.** [`best_of`] builds a run of seeds, analyzes each one headlessly,
//! throws out any with a FAIL, and ranks the rest by [`score`]. Everything is
//! deterministic in the seed: `LEVELGEN_TRIES=1 LEVELGEN_SEED=<winner>` rebuilds the
//! winner exactly.
//!
//! The layout never guesses at geometry: where a door goes and which boxes a corridor
//! carves come from [`builder::door_box`] / [`builder::corridor_boxes`], the functions
//! the builder itself carves with.

use std::collections::{HashMap, VecDeque};

use engine::geometry::structures::Edge;

use super::analyze::{ReportData, Status};
use super::builder::{self, BuiltLevel, Dir, LevelBuilder, RoomId, DOOR_HEIGHT};

/// Knobs for one generated level.
#[derive(Clone, Copy, Debug)]
pub struct GenParams {
    /// Rooms to aim for across every floor, the hero hall included. A room that cannot
    /// be placed after [`PLACE_ATTEMPTS`] tries is skipped, so the result can come in a
    /// little under.
    pub rooms: usize,
    /// Extra connections beyond the tree — each one closes a loop.
    pub loops: usize,
    /// How many of `rooms` go on the upper floor (0 = none, and no balcony).
    pub upper: usize,
    /// How many of `rooms` go in the basement (0 = none).
    pub lower: usize,
}

impl Default for GenParams {
    fn default() -> Self {
        GenParams {
            rooms: 10,
            loops: 3,
            upper: 2,
            lower: 2,
        }
    }
}

/// Tries per room before giving up on it.
const PLACE_ATTEMPTS: usize = 80;
/// Rooms that are not joined keep at least this much solid between them on the plan
/// (WT): enough for a real wall, and what makes the report's `merged rooms` check pass
/// by construction.
const CLEARANCE: f32 = 2.0;
/// …or at least this much solid between them vertically: a floor slab. Rooms on
/// different floors can then overlap on the plan — which is what stacking is.
const SLAB: f32 = 2.0;
/// Wall thicknesses a tree door is cut through. Mostly a plain wall; now and then a short
/// hall, which reads as a corridor between the two rooms.
const TREE_WALLS: [f32; 7] = [2.0, 2.0, 2.0, 3.0, 4.0, 6.0, 8.0];
/// A loop door only closes a gap this thin or thinner — past it, it is a tunnel.
const LOOP_MAX_GAP: f32 = 12.0;
/// Pillars stand at least this far inside a room's walls — clear of every door box
/// (which reaches 2 WT in) with a lane to spare, and never a 0.5 m slot at a wall.
const PILLAR_INSET: f32 = 5.0;
/// …and at least this much open floor from a flight, a hole, a deck or another pillar.
/// A gap of 2 WT (0.5 m) is narrower than a hunter; the first multi-floor build put a
/// pillar exactly 2 WT off a basement stair and walled off a corner with a gun in it.
const PILLAR_GAP: f32 = 4.0;
/// Weapons from worst to best. Farther rooms get better guns, so the good ones are worth
/// the walk (the pickups design: the player and the hunters start unarmed).
const WEAPON_TIERS: [&str; 5] = ["PP7", "KF7 Soviet", "Shotgun", "AR33", "RC-P90"];

/// The upper floor's height, WT. The hero hall's balcony deck is here too, so the hero
/// must be at least this + [`DOOR_HEIGHT`] tall for the door off the deck to fit.
const UPPER_FLOOR: f32 = 16.0;
/// The basement floor, WT: its [`STOREY`]-tall rooms then stop [`SLAB`] under the ground.
const LOWER_FLOOR: f32 = -14.0;
/// Every basement and upper room is this tall or taller (the 12 WT ceiling rule), and a
/// basement exactly this tall, so its ceiling sits one slab under the ground floor.
const STOREY: f32 = 12.0;
/// How deep the hero's balcony is.
const BALCONY_DEPTH: f32 = 8.0;
/// Width of every free-standing flight.
const FLIGHT_W: f32 = 4.0;
/// A flight keeps this far inside the walls of the room it stands in, clear of the door
/// boxes at them.
const FLIGHT_INSET: f32 = 3.0;

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

    /// Footprint + ceiling for a ground-floor room of this role, WT. Sized to the design
    /// rules: the hero clears the 40 WT / 24 WT bar, nothing is under the 12 WT ceiling,
    /// and the gallery is long-skinny.
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
    /// Label box `[x, y, z, w, h, d]`.
    aabb: [f32; 6],
    scheme: usize,
}

impl PlanRoom {
    /// Plan footprint `[x0, x1, z0, z1]`.
    fn rect(&self) -> [f32; 4] {
        let a = self.aabb;
        [a[0], a[0] + a[3], a[2], a[2] + a[5]]
    }
    fn floor(&self) -> f32 {
        self.aabb[1]
    }
    /// Vertical air span.
    fn y(&self) -> (f32, f32) {
        (self.aabb[1], self.aabb[1] + self.aabb[4])
    }
}

/// The hero hall's balcony: a deck along its `side` wall at [`UPPER_FLOOR`], a stair up
/// to it, and a door off it into upper room `upper`.
#[derive(Clone, Copy, Debug)]
struct Balcony {
    upper: usize,
    side: Dir,
    /// Where the stair lands along the deck's inner edge, 0..1.
    offset: f32,
}

#[derive(Clone, Copy, Debug)]
enum Link {
    Door { a: usize, b: usize, width: f32 },
    Corridor { a: usize, b: usize, width: f32 },
    /// The hero (room 0) to its first upper room, by way of the balcony.
    Balcony(Balcony),
    /// A free-standing flight from `upper`'s floor down into `lower`, through a hole.
    Through { upper: usize, lower: usize, x: f32, z: f32, dir: Dir },
}

impl Link {
    fn ends(self) -> (usize, usize) {
        match self {
            Link::Door { a, b, .. } | Link::Corridor { a, b, .. } => (a, b),
            Link::Balcony(bal) => (0, bal.upper),
            Link::Through { upper, lower, .. } => (upper, lower),
        }
    }
}

/// Everything decided before a brush is carved.
#[derive(Default)]
struct Plan {
    rooms: Vec<PlanRoom>,
    links: Vec<Link>,
    degree: Vec<usize>,
    /// Per-room plan rectangles nothing may stand in (a flight, a hole in the floor).
    no_drop: Vec<(usize, [f32; 4])>,
    /// Per-room rectangles a full-height pillar must not rise through (the above, plus
    /// the balcony deck).
    no_pillar: Vec<(usize, [f32; 4])>,
    counts: HashMap<Role, usize>,
}

impl Plan {
    fn name(&mut self, role: Role, floor: f32) -> String {
        let n = self.counts.entry(role).or_insert(0);
        *n += 1;
        let level = if floor > 0.0 {
            "upper_"
        } else if floor < 0.0 {
            "lower_"
        } else {
            ""
        };
        if role == Role::Hero {
            "hero".to_string()
        } else {
            format!("{level}{}_{}", role.label(), n)
        }
    }

    fn push(&mut self, room: PlanRoom) -> usize {
        self.rooms.push(room);
        self.degree.push(0);
        self.rooms.len() - 1
    }

    fn join(&mut self, link: Link) {
        let (a, b) = link.ends();
        self.degree[a] += 1;
        self.degree[b] += 1;
        self.links.push(link);
    }

    /// Would `cand` come too close to any room but `except`?
    fn clashes(&self, cand: &PlanRoom, except: Option<usize>) -> bool {
        self.rooms
            .iter()
            .enumerate()
            .any(|(i, r)| Some(i) != except && clash(cand, r))
    }

    /// For an **upper-floor** candidate: the ground rooms whose ceilings would have to
    /// come down to leave a slab under it, or `None` if something else is in the way.
    ///
    /// Ground halls run up to 18 WT tall, which is more than the 14 WT a room under the
    /// upper floor can have — so without this, upper rooms could only ever stand over low
    /// rooms or empty rock, and almost never over a ground room big enough to take a
    /// second stair down. Lowering is safe here because nothing yet depends on a ground
    /// room's height: doors (10 WT), loops and pillars are all decided later. The hero is
    /// never lowered — its height is what makes the balcony door fit.
    fn lowerable_under(&self, cand: &PlanRoom, except: Option<usize>) -> Option<Vec<usize>> {
        let ceiling = cand.floor() - SLAB;
        let mut lower = Vec::new();
        for (i, r) in self.rooms.iter().enumerate() {
            if Some(i) == except || !clash(cand, r) {
                continue;
            }
            let can = i != 0 && r.floor() == 0.0 && ceiling - r.floor() >= STOREY;
            if !can {
                return None;
            }
            lower.push(i);
        }
        Some(lower)
    }

    /// Accept an upper-floor candidate if it fits, lowering whatever ground ceilings it
    /// needs to (see [`Self::lowerable_under`]).
    fn fit_upper(&mut self, cand: &PlanRoom, except: Option<usize>) -> bool {
        match self.lowerable_under(cand, except) {
            Some(rooms) => {
                let ceiling = cand.floor() - SLAB;
                for i in rooms {
                    let r = &mut self.rooms[i];
                    r.aabb[4] = ceiling - r.aabb[1];
                }
                true
            }
            None => false,
        }
    }

    /// Would a rectangle in room `room` come within `margin` of something already
    /// reserved there?
    fn reserved(&self, room: usize, rect: [f32; 4], margin: f32) -> bool {
        self.no_pillar.iter().any(|(r, b)| *r == room && near(rect, *b, margin))
    }
}

/// Whether two plan rectangles come within `margin` of each other.
fn near(a: [f32; 4], b: [f32; 4], margin: f32) -> bool {
    a[0] < b[1] + margin && b[0] < a[1] + margin && a[2] < b[3] + margin && b[2] < a[3] + margin
}

/// Whether two vertical spans come within `margin` of each other.
fn spans_near(a: (f32, f32), b: (f32, f32), margin: f32) -> bool {
    a.0 < b.1 + margin && b.0 < a.1 + margin
}

/// Two rooms too close to stay separate: near on the plan *and* without a slab between.
fn clash(a: &PlanRoom, b: &PlanRoom) -> bool {
    near(a.rect(), b.rect(), CLEARANCE) && spans_near(a.y(), b.y(), SLAB)
}

/// Whether `inner` lies inside `outer`, at least `inset` from its edges.
fn inside(inner: [f32; 4], outer: [f32; 4], inset: f32) -> bool {
    inner[0] >= outer[0] + inset
        && inner[1] <= outer[1] - inset
        && inner[2] >= outer[2] + inset
        && inner[3] <= outer[3] - inset
}

/// The plan rectangle of a `run`-long flight starting at `(x, z)` heading `dir`.
fn flight_rect(x: f32, z: f32, dir: Dir, run: f32) -> [f32; 4] {
    let hw = FLIGHT_W * 0.5;
    match dir {
        Dir::East => [x, x + run, z - hw, z + hw],
        Dir::West => [x - run, x, z - hw, z + hw],
        Dir::South => [x - hw, x + hw, z, z + run],
        Dir::North => [x - hw, x + hw, z - run, z],
    }
}

/// A start point for a `run`-long flight heading `dir` that lies wholly inside `area`
/// (a plan rectangle, already inset from whatever walls it must clear), or `None` if it
/// cannot fit that way round.
///
/// Sampled from the **feasible range**, not the whole rectangle: a 17 WT flight in a
/// 23 WT room has a sliver of legal starts, and picking points anywhere in the room and
/// testing them missed it nearly every time — which is why the second route between
/// floors was first built in 0 levels out of 24.
fn place_flight(area: [f32; 4], dir: Dir, run: f32, rng: &mut Rng) -> Option<(f32, f32)> {
    let hw = FLIGHT_W * 0.5;
    let (run_lo, run_hi, across_lo, across_hi) = match dir {
        Dir::East => (area[0], area[1] - run, area[2] + hw, area[3] - hw),
        Dir::West => (area[0] + run, area[1], area[2] + hw, area[3] - hw),
        Dir::South => (area[2], area[3] - run, area[0] + hw, area[1] - hw),
        Dir::North => (area[2] + run, area[3], area[0] + hw, area[1] - hw),
    };
    if run_hi < run_lo || across_hi < across_lo {
        return None;
    }
    let (along, across) = (
        run_lo + (run_hi - run_lo) * rng.unit(),
        across_lo + (across_hi - across_lo) * rng.unit(),
    );
    Some(match dir {
        Dir::East | Dir::West => (along.floor(), across.floor()),
        Dir::North | Dir::South => (across.floor(), along.floor()),
    })
}

/// `r` shrunk by `by` on every side.
fn inset(r: [f32; 4], by: f32) -> [f32; 4] {
    [r[0] + by, r[1] - by, r[2] + by, r[3] - by]
}

/// The role list for `n` ground-floor rooms: the hero first, one gallery, a closet per
/// four rooms, the rest halls and fight rooms. With `hall_first`, the room after the hero
/// — which always grows off the hero, being the only room there — is a hall: the big
/// neighbour the upper floor stacks over (see [`stack_over_neighbour`]).
fn ground_roles(n: usize, hall_first: bool, rng: &mut Rng) -> Vec<Role> {
    let mut v = vec![Role::Gallery];
    for _ in 0..(n / 4) {
        v.push(Role::Closet);
    }
    while v.len() + 1 < n {
        v.push(if rng.unit() < 0.5 { Role::Hall } else { Role::Fight });
    }
    v.truncate(n.saturating_sub(1));
    // Shuffle (Fisher-Yates) so the gallery isn't always the first thing off the hero.
    for i in (1..v.len()).rev() {
        let j = rng.index(i + 1);
        v.swap(i, j);
    }
    if hall_first && n >= 2 {
        match v.iter().position(|&r| r == Role::Hall) {
            Some(i) => v.swap(0, i),
            None => v[0] = Role::Hall,
        }
    }
    v.insert(0, Role::Hero);
    v
}

/// A room of `role` for a floor other than the ground: ground-floor footprint, storey
/// height (a basement exactly [`STOREY`], so its ceiling stops a slab under the ground).
fn storey_size(role: Role, floor: f32, rng: &mut Rng) -> (f32, f32, f32) {
    let (w, d, _) = role.size(rng);
    let h = if floor < 0.0 { STOREY } else { rng.span(STOREY, STOREY + 2.0) };
    (w, d, h)
}

/// Grow `roles` onto `floor`, each beside an existing room that `parent_ok` accepts,
/// joined by a door. Returns how many were placed.
fn grow_on(
    plan: &mut Plan,
    roles: &[Role],
    floor: f32,
    parent_ok: impl Fn(&PlanRoom) -> bool,
    rng: &mut Rng,
) -> usize {
    let mut placed = 0;
    for &role in roles {
        let (w, d, h) = if floor == 0.0 { role.size(rng) } else { storey_size(role, floor, rng) };
        for _ in 0..PLACE_ATTEMPTS {
            // A parent with room left on its walls; on the ground the hero hub gets more
            // than its share.
            let open: Vec<usize> = (0..plan.rooms.len())
                .filter(|&i| plan.degree[i] < 4 && parent_ok(&plan.rooms[i]))
                .collect();
            if open.is_empty() {
                break;
            }
            let parent = if floor == 0.0 && rng.unit() < 0.35 { 0 } else { rng.pick(&open) };
            let dir = rng.pick(&[Dir::North, Dir::South, Dir::East, Dir::West]);
            let wall = rng.pick(&TREE_WALLS);
            let width = role.door().min(plan.rooms[parent].role.door());
            let need = builder::wall_needed(width) + 1.0;
            let pa = plan.rooms[parent].aabb;
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
                aabb: [x, floor, z, w, h, d],
                scheme: 0,
            };
            let fits = if floor == UPPER_FLOOR {
                plan.fit_upper(&cand, Some(parent))
            } else {
                !plan.clashes(&cand, Some(parent))
            };
            if !fits {
                continue;
            }
            let name = plan.name(role, floor);
            let scheme = rng.index(9);
            let i = plan.push(PlanRoom { name, scheme, ..cand });
            plan.join(Link::Door { a: parent, b: i, width });
            placed += 1;
            break;
        }
    }
    placed
}

/// The ground floor: the hero hall, then everything grown from it.
fn grow_ground(plan: &mut Plan, n: usize, tall_hero: bool, rng: &mut Rng) {
    let roles = ground_roles(n.max(1), tall_hero, rng);
    let (w, d, mut h) = Role::Hero.size(rng);
    if tall_hero {
        // The door off the balcony needs the hero's ceiling a door's height above it.
        h = h.max(UPPER_FLOOR + DOOR_HEIGHT + rng.span(0.0, 4.0));
    }
    let name = plan.name(Role::Hero, 0.0);
    let scheme = rng.index(9);
    plan.push(PlanRoom {
        role: Role::Hero,
        name,
        aabb: [0.0, 0.0, 0.0, w, h, d],
        scheme,
    });
    grow_on(plan, &roles[1..], 0.0, |r| r.floor() == 0.0, rng);
}

/// The deck and the stair footprint of the hero's balcony.
fn balcony_rects(hero: [f32; 6], bal: Balcony) -> ([f32; 4], [f32; 4]) {
    let (x0, x1, z0, z1) = (hero[0], hero[0] + hero[3], hero[2], hero[2] + hero[5]);
    let t = UPPER_FLOOR;
    let hw = FLIGHT_W * 0.5;
    match bal.side {
        Dir::North => {
            let (e, x) = (z0 + BALCONY_DEPTH, x0 + (x1 - x0) * bal.offset);
            ([x0, x1, z0, e], [x - hw, x + hw, e, e + t])
        }
        Dir::South => {
            let (e, x) = (z1 - BALCONY_DEPTH, x0 + (x1 - x0) * bal.offset);
            ([x0, x1, e, z1], [x - hw, x + hw, e - t, e])
        }
        Dir::West => {
            let (e, z) = (x0 + BALCONY_DEPTH, z0 + (z1 - z0) * bal.offset);
            ([x0, e, z0, z1], [e, e + t, z - hw, z + hw])
        }
        Dir::East => {
            let (e, z) = (x1 - BALCONY_DEPTH, z0 + (z1 - z0) * bal.offset);
            ([e, x1, z0, z1], [e - t, e, z - hw, z + hw])
        }
    }
}

/// Whether the hero is deep enough across `side` for a balcony there: the stair runs
/// from the deck's inner edge into the hall and needs its whole rise as run, with the
/// far wall's door lane to spare.
fn balcony_fits(hero: [f32; 6], side: Dir) -> bool {
    let depth_across = match side {
        Dir::North | Dir::South => hero[5],
        Dir::East | Dir::West => hero[3],
    };
    depth_across >= BALCONY_DEPTH + UPPER_FLOOR + FLIGHT_INSET + 2.0
}

/// Put the first upper room **directly over a big ground room beside the hero**, with the
/// balcony on that side — so the second route down (a flight from it into the room it
/// stands over) is guaranteed room, and the upper floor is a loop from the start:
/// hero → balcony → upper room → flight → ground room → door → hero.
///
/// Left to chance, this almost never happened: an upper room can only stand over a
/// ground room that leaves a slab under it, and a random upper placement over one big
/// enough to take a 17 WT flight was rare enough that 24 seeds out of 24 had no second
/// route. Returns whether it placed one.
fn stack_over_neighbour(plan: &mut Plan, rng: &mut Rng) -> bool {
    let hero = plan.rooms[0].aabb;
    let need = UPPER_FLOOR + 1.0 + 2.0 * FLIGHT_INSET;
    let mut hosts: Vec<(usize, Dir)> = Vec::new();
    for l in &plan.links {
        let Link::Door { a: 0, b: g, .. } = *l else { continue };
        let r = &plan.rooms[g];
        if r.floor() != 0.0 || r.aabb[3] * r.aabb[5] < 256.0 || r.aabb[3].max(r.aabb[5]) < need {
            continue;
        }
        let Ok(f) = builder::facing(hero, r.aabb) else { continue };
        let side = match (f.axis, f.sign > 0.0) {
            (engine::geometry::csg_runtime::Axis::X, true) => Dir::East,
            (engine::geometry::csg_runtime::Axis::X, false) => Dir::West,
            (_, true) => Dir::South,
            (_, false) => Dir::North,
        };
        if balcony_fits(hero, side) {
            hosts.push((g, side));
        }
    }
    if hosts.is_empty() {
        return false;
    }
    let (g, side) = rng.pick(&hosts);
    let role = plan.rooms[g].role;
    let under = plan.rooms[g].aabb;
    let cand = PlanRoom {
        role,
        name: String::new(),
        aabb: [under[0], UPPER_FLOOR, under[2], under[3], rng.span(STOREY, STOREY + 2.0), under[5]],
        scheme: 0,
    };
    let width = role.door();
    if builder::door_box(hero, cand.aabb, 0.5, width, DOOR_HEIGHT).is_err()
        || plan.lowerable_under(&cand, Some(0)).is_none()
    {
        return false;
    }
    plan.fit_upper(&cand, Some(0));
    let name = plan.name(role, UPPER_FLOOR);
    let scheme = rng.index(9);
    let i = plan.push(PlanRoom { name, scheme, ..cand });
    let bal = Balcony {
        upper: i,
        side,
        offset: 0.25 + 0.5 * rng.unit(),
    };
    let (deck, stair) = balcony_rects(hero, bal);
    plan.join(Link::Balcony(bal));
    plan.no_pillar.push((0, deck));
    plan.no_pillar.push((0, stair));
    plan.no_drop.push((0, stair));
    true
}

/// The upper floor: a balcony along one of the hero's walls, a door off it into the
/// first upper room, more rooms grown from that — and, if one fits, a second stair
/// down from an upper room into a ground room beneath it.
fn grow_upper(plan: &mut Plan, n: usize, rng: &mut Rng) {
    if n == 0 {
        return;
    }
    let hero = plan.rooms[0].aabb;
    let mut placed = stack_over_neighbour(plan, rng);
    for _ in 0..PLACE_ATTEMPTS {
        if placed {
            break;
        }
        let side = rng.pick(&[Dir::North, Dir::South, Dir::East, Dir::West]);
        if !balcony_fits(hero, side) {
            continue;
        }
        let role = if rng.unit() < 0.6 { Role::Hall } else { Role::Fight };
        let (w, d, h) = storey_size(role, UPPER_FLOOR, rng);
        let width = role.door();
        let need = builder::wall_needed(width) + 1.0;
        let (p_len, n_len) = match side {
            Dir::East | Dir::West => (hero[5], d),
            Dir::North | Dir::South => (hero[3], w),
        };
        if p_len < need || n_len < need {
            continue;
        }
        let along = rng.span(need - n_len, p_len - need);
        let (x, z) = builder::beside_origin(hero, side, 2.0, along, w, d);
        let cand = PlanRoom {
            role,
            name: String::new(),
            aabb: [x, UPPER_FLOOR, z, w, h, d],
            scheme: 0,
        };
        if builder::door_box(hero, cand.aabb, 0.5, width, DOOR_HEIGHT).is_err()
            || plan.lowerable_under(&cand, Some(0)).is_none()
        {
            continue;
        }
        plan.fit_upper(&cand, Some(0));
        let offset = 0.25 + 0.5 * rng.unit();
        let name = plan.name(role, UPPER_FLOOR);
        let scheme = rng.index(9);
        let i = plan.push(PlanRoom { name, scheme, ..cand });
        let bal = Balcony { upper: i, side, offset };
        let (deck, stair) = balcony_rects(hero, bal);
        plan.join(Link::Balcony(bal));
        plan.no_pillar.push((0, deck));
        plan.no_pillar.push((0, stair));
        plan.no_drop.push((0, stair));
        placed = true;
        break;
    }
    if !placed {
        return;
    }
    let roles: Vec<Role> = (1..n)
        .map(|_| {
            if rng.unit() < 0.4 {
                Role::Closet
            } else if rng.unit() < 0.5 {
                Role::Hall
            } else {
                Role::Fight
            }
        })
        .collect();
    grow_on(plan, &roles, UPPER_FLOOR, |r| r.floor() == UPPER_FLOOR, rng);

    // A second way down: a flight through an upper room's floor into the ground room
    // beneath it, if one is big enough to take it.
    second_route(plan, UPPER_FLOOR, 0.0, rng);
}

/// A **second route between two floors**: a flight through a room on floor `top` down
/// into a room on floor `bottom` directly beneath it, for rooms that are not already
/// joined that way — "a second route up/down each vertical, for flanking". Tries each
/// overlapping pair; the first flight that fits inside both rooms, clear of everything
/// reserved in them, is built. The room the flight stands in must be a big one ("stairs
/// never in a small room").
fn second_route(plan: &mut Plan, top: f32, bottom: f32, rng: &mut Rng) {
    let run = top - bottom + 1.0;
    let tops: Vec<usize> = (0..plan.rooms.len()).filter(|&i| plan.rooms[i].floor() == top).collect();
    let bottoms: Vec<usize> = (0..plan.rooms.len()).filter(|&i| plan.rooms[i].floor() == bottom).collect();
    for &u in &tops {
        for &g in &bottoms {
            let already = plan.links.iter().any(|l| matches!(l, Link::Through { .. }) && {
                let (a, b) = l.ends();
                (a, b) == (u, g) || (a, b) == (g, u)
            });
            let lower = &plan.rooms[g];
            if already || lower.aabb[3] * lower.aabb[5] < 256.0 {
                continue;
            }
            let (ru, rg) = (plan.rooms[u].rect(), lower.rect());
            // Only the overlap can hold the flight.
            let ov = [ru[0].max(rg[0]), ru[1].min(rg[1]), ru[2].max(rg[2]), ru[3].min(rg[3])];
            if ov[1] - ov[0] < FLIGHT_W + 2.0 * FLIGHT_INSET || ov[3] - ov[2] < FLIGHT_W + 2.0 * FLIGHT_INSET {
                continue;
            }
            let area = inset(ov, FLIGHT_INSET);
            for _ in 0..24 {
                let dir = rng.pick(&[Dir::North, Dir::South, Dir::East, Dir::West]);
                let Some((x, z)) = place_flight(area, dir, run, rng) else { continue };
                let fr = flight_rect(x, z, dir, run);
                if inside(fr, ru, FLIGHT_INSET)
                    && inside(fr, rg, FLIGHT_INSET)
                    && !plan.reserved(g, fr, 2.0)
                    && !plan.reserved(u, fr, 2.0)
                {
                    plan.join(Link::Through { upper: u, lower: g, x, z, dir });
                    for room in [u, g] {
                        plan.no_drop.push((room, fr));
                        plan.no_pillar.push((room, fr));
                    }
                    return;
                }
            }
        }
    }
}

/// The basement: a flight through a big ground room's floor into the first basement
/// room (placed around the flight), then more basement rooms grown from it.
fn grow_lower(plan: &mut Plan, n: usize, rng: &mut Rng) {
    if n == 0 {
        return;
    }
    let run = -LOWER_FLOOR + 1.0;
    let hosts: Vec<usize> = (0..plan.rooms.len())
        .filter(|&i| {
            let r = &plan.rooms[i];
            r.floor() == 0.0
                && r.aabb[3] * r.aabb[5] >= 256.0
                && r.aabb[3].max(r.aabb[5]) >= run + 2.0 * FLIGHT_INSET
        })
        .collect();
    if hosts.is_empty() {
        return;
    }
    let mut placed = false;
    for _ in 0..PLACE_ATTEMPTS {
        let g = rng.pick(&hosts);
        let rg = plan.rooms[g].rect();
        let dir = rng.pick(&[Dir::North, Dir::South, Dir::East, Dir::West]);
        let Some((x, z)) = place_flight(inset(rg, FLIGHT_INSET), dir, run, rng) else { continue };
        let fr = flight_rect(x, z, dir, run);
        if !inside(fr, rg, FLIGHT_INSET) || plan.reserved(g, fr, 2.0) {
            continue;
        }
        // A basement room around the flight, big enough to hold it with a lane each side.
        let role = if rng.unit() < 0.5 { Role::Hall } else { Role::Fight };
        let (mut w, mut d, h) = storey_size(role, LOWER_FLOOR, rng);
        w = w.max(fr[1] - fr[0] + 2.0 * FLIGHT_INSET + 2.0);
        d = d.max(fr[3] - fr[2] + 2.0 * FLIGHT_INSET + 2.0);
        let bx = rng.span(fr[1] + FLIGHT_INSET - w, fr[0] - FLIGHT_INSET);
        let bz = rng.span(fr[3] + FLIGHT_INSET - d, fr[2] - FLIGHT_INSET);
        let cand = PlanRoom {
            role,
            name: String::new(),
            aabb: [bx, LOWER_FLOOR, bz, w, h, d],
            scheme: 0,
        };
        if !inside(fr, cand.rect(), FLIGHT_INSET) || plan.clashes(&cand, None) {
            continue;
        }
        let name = plan.name(role, LOWER_FLOOR);
        let scheme = rng.index(9);
        let i = plan.push(PlanRoom { name, scheme, ..cand });
        plan.join(Link::Through { upper: g, lower: i, x, z, dir });
        for room in [g, i] {
            plan.no_drop.push((room, fr));
            plan.no_pillar.push((room, fr));
        }
        placed = true;
        break;
    }
    if !placed {
        return;
    }
    // The second basement room under another big ground room, joined to the first — so
    // the second flight up has somewhere to land. Then the rest, grown as usual.
    let stacked = usize::from(n >= 2 && stack_under_neighbour(plan, rng));
    // Halls and fight rooms, not closets: a basement room has to be big enough to take
    // the second flight up (`second_route` needs 16×16 WT to stand a stair in).
    let roles: Vec<Role> = (1 + stacked..n)
        .map(|_| if rng.unit() < 0.6 { Role::Hall } else { Role::Fight })
        .collect();
    grow_on(plan, &roles, LOWER_FLOOR, |r| r.floor() == LOWER_FLOOR, rng);
    // A second way up out of the basement, from another room above it.
    second_route(plan, 0.0, LOWER_FLOOR, rng);
}

/// Put a basement room **directly under a big ground room** that does not already have a
/// flight down, and join it to an existing basement room by a door or an L-corridor —
/// the basement's half of a second route (the flight itself is laid by
/// [`second_route`]). Returns whether it placed one.
fn stack_under_neighbour(plan: &mut Plan, rng: &mut Rng) -> bool {
    let need = -LOWER_FLOOR + 1.0 + 2.0 * FLIGHT_INSET;
    let has_flight = |plan: &Plan, i: usize| {
        plan.links
            .iter()
            .any(|l| matches!(l, Link::Through { .. }) && (l.ends().0 == i || l.ends().1 == i))
    };
    let mut hosts: Vec<usize> = (0..plan.rooms.len())
        .filter(|&i| {
            let r = &plan.rooms[i];
            r.floor() == 0.0
                && r.aabb[3] * r.aabb[5] >= 256.0
                && r.aabb[3].max(r.aabb[5]) >= need
                && !has_flight(plan, i)
        })
        .collect();
    // Try them in a seeded order.
    for i in (1..hosts.len()).rev() {
        let j = rng.index(i + 1);
        hosts.swap(i, j);
    }
    let basements: Vec<usize> = (0..plan.rooms.len()).filter(|&i| plan.rooms[i].floor() == LOWER_FLOOR).collect();
    for g in hosts {
        let over = plan.rooms[g].aabb;
        let role = if rng.unit() < 0.6 { Role::Hall } else { Role::Fight };
        // The whole footprint of the room above, or — if that clashes with a basement room
        // already there — the same footprint pulled 2 WT in, provided it still holds the
        // flight.
        let fit = [0.0f32, 2.0].into_iter().find_map(|pull| {
            let (w, d) = (over[3] - 2.0 * pull, over[5] - 2.0 * pull);
            if w.max(d) < need || w.min(d) < FLIGHT_W + 2.0 * FLIGHT_INSET {
                return None;
            }
            let cand = PlanRoom {
                role,
                name: String::new(),
                aabb: [over[0] + pull, LOWER_FLOOR, over[2] + pull, w, STOREY, d],
                scheme: 0,
            };
            (!plan.clashes(&cand, None)).then_some(cand)
        });
        let Some(cand) = fit else { continue };
        for &b in &basements {
            let width = role.door().min(plan.rooms[b].role.door());
            let (la, lb) = (plan.rooms[b].aabb, cand.aabb);
            let (link, boxes) = match builder::facing(la, lb) {
                Ok(f) if f.gap <= LOOP_MAX_GAP => match builder::door_box(la, lb, 0.5, width, DOOR_HEIGHT) {
                    Ok(bx) => (true, vec![bx]),
                    Err(_) => continue,
                },
                Ok(_) => continue,
                Err(_) => match builder::corridor_boxes(la, lb, width) {
                    Ok(bxs) if bxs.len() == 2 => (false, bxs),
                    _ => continue,
                },
            };
            let clean = boxes.iter().all(|bx| {
                plan.rooms.iter().enumerate().all(|(i, r)| {
                    i == b || !(near(bx.plan(), r.rect(), 1.0) && spans_near(bx.y, r.y(), 1.0))
                })
            });
            let length: f32 = boxes.iter().map(|bx| bx.along.1 - bx.along.0).sum();
            if !clean || length > 60.0 {
                continue;
            }
            let name = plan.name(role, LOWER_FLOOR);
            let scheme = rng.index(9);
            let i = plan.push(PlanRoom { name, scheme, ..cand });
            plan.join(if link {
                Link::Door { a: b, b: i, width }
            } else {
                Link::Corridor { a: b, b: i, width }
            });
            return true;
        }
    }
    false
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

/// Close loops on each floor: doors between rooms that face each other across a thin
/// wall and L-corridors between diagonal ones, wherever the carve would not pass through
/// (or come within a wall of) any third room. Longer loops first — a door between two
/// rooms already three hops apart makes a real alternative route; one between neighbours
/// of the same room is barely a loop at all.
fn close_loops(plan: &mut Plan, loops: usize, rng: &mut Rng) {
    let n = plan.rooms.len();
    let mut cands: Vec<(usize, f32, Link)> = Vec::new();
    for a in 0..n {
        let dist = hops(n, &plan.links, a);
        for b in a + 1..n {
            let rooms = &plan.rooms;
            if dist[b] < 2 || rooms[a].floor() != rooms[b].floor() {
                continue; // already joined (or nearly), or on different floors
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
            // The carve may touch `a` and `b` (it must) but nothing else, on any floor.
            let clean = boxes.iter().all(|bx| {
                rooms.iter().enumerate().all(|(i, r)| {
                    i == a || i == b || !(near(bx.plan(), r.rect(), 1.0) && spans_near(bx.y, r.y(), 1.0))
                })
            });
            // …and an L's legs stay a sensible length.
            let length: f32 = boxes.iter().map(|bx| bx.along.1 - bx.along.0).sum();
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
        if hops(n, &plan.links, a)[b] < 2 {
            continue;
        }
        plan.join(link);
        added += 1;
    }
}

// ─── Building it ───────────────────────────────────────────────────────────────

/// Build the level for `seed`. Deterministic: the same seed and params always give the
/// same level.
pub fn build(seed: u64, params: &GenParams) -> BuiltLevel {
    let mut rng = Rng::new(seed);
    let mut plan = Plan::default();
    let ground = params.rooms.saturating_sub(params.upper + params.lower).max(1);
    grow_ground(&mut plan, ground, params.upper > 0, &mut rng);
    grow_upper(&mut plan, params.upper, &mut rng);
    grow_lower(&mut plan, params.lower, &mut rng);
    close_loops(&mut plan, params.loops, &mut rng);
    let rooms = &plan.rooms;

    let mut b = LevelBuilder::new();
    let ids: Vec<RoomId> = rooms
        .iter()
        .map(|r| {
            b.set_scheme(r.scheme);
            let a = r.aabb;
            b.room(&r.name, a[0], a[2], a[3], a[5], a[1], a[4])
        })
        .collect();
    for l in &plan.links {
        match *l {
            Link::Door { a, b: c, width } => b.door(ids[a], ids[c], width),
            Link::Corridor { a, b: c, width } => b.corridor(ids[a], ids[c], width),
            Link::Through { upper, lower, x, z, dir } => {
                b.stair_through_floor(ids[upper], ids[lower], x, z, dir, FLIGHT_W)
            }
            Link::Balcony(bal) => {
                let hero = rooms[0].aabb;
                let (deck, stair) = balcony_rects(hero, bal);
                let plat = b.platform(
                    "balcony",
                    deck[0],
                    deck[2],
                    deck[1] - deck[0],
                    deck[3] - deck[2],
                    UPPER_FLOOR,
                    true,
                );
                let deck_room = b.last_room();
                // The flight's foot is the far end of its footprint from the deck.
                let (edge, foot) = match bal.side {
                    Dir::North => (Edge::ZMax, ((stair[0] + stair[1]) * 0.5, stair[3])),
                    Dir::South => (Edge::ZMin, ((stair[0] + stair[1]) * 0.5, stair[2])),
                    Dir::West => (Edge::XMax, (stair[1], (stair[2] + stair[3]) * 0.5)),
                    Dir::East => (Edge::XMin, (stair[0], (stair[2] + stair[3]) * 0.5)),
                };
                b.stair_to_platform((foot.0, 0.0, foot.1), plat, edge, bal.offset, FLIGHT_W, true);
                b.link(ids[0], deck_room);
                b.door(ids[0], ids[bal.upper], rooms[bal.upper].role.door());
                b.link(deck_room, ids[bal.upper]);
            }
        }
    }

    // Things standing on the floor, so pillars keep off them.
    let mut taken: Vec<(usize, f32, f32)> = Vec::new();
    let centre = |r: &PlanRoom| (r.aabb[0] + r.aabb[3] * 0.5, r.aabb[2] + r.aabb[5] * 0.5);
    // A spot on room `i`'s floor, clear of its holes and flights.
    let spot = |rng: &mut Rng, i: usize| {
        let r = &rooms[i];
        let (cx, cz) = centre(r);
        let (hx, hz) = ((r.aabb[3] * 0.5 - 3.0).max(0.0), (r.aabb[5] * 0.5 - 3.0).max(0.0));
        for _ in 0..24 {
            let p = (cx - hx + 2.0 * hx * rng.unit(), cz - hz + 2.0 * hz * rng.unit());
            let clear = plan
                .no_drop
                .iter()
                .all(|(room, rect)| *room != i || !near([p.0 - 1.5, p.0 + 1.5, p.1, p.1], *rect, 1.0));
            if clear {
                return Some(p);
            }
        }
        None
    };

    // The spawn marker in the hero hall, and a pad in every room.
    let (hx, hz) = centre(&rooms[0]);
    b.spawn_wt(hx, 0.0, hz);
    for (i, r) in rooms.iter().enumerate() {
        if let Some((x, z)) = spot(&mut rng, i) {
            b.spawn_pad(x, r.floor(), z, rng.index(8) as f32 * 45.0);
            taken.push((i, x, z));
        }
    }

    // Weapons, better the farther from the hero hall; two rooms in three get one.
    let dist = hops(rooms.len(), &plan.links, 0);
    let far = dist.iter().copied().filter(|&d| d != usize::MAX).max().unwrap_or(0).max(1);
    for (i, r) in rooms.iter().enumerate() {
        if i != 0 && rng.unit() > 0.67 {
            continue;
        }
        let tier = (dist[i].min(far) * (WEAPON_TIERS.len() - 1) + far / 2) / far;
        let gun = WEAPON_TIERS[tier.min(WEAPON_TIERS.len() - 1)];
        if let Some((x, z)) = spot(&mut rng, i) {
            b.weapon(gun, x, r.floor(), z);
            b.ammo(gun, x + 1.5, r.floor(), z);
            taken.push((i, x, z));
            taken.push((i, x + 1.5, z));
        }
    }

    // Cover pillars in the big rooms, clear of stairs, holes, the balcony and anything
    // standing on the floor.
    for (i, r) in rooms.iter().enumerate() {
        let (w, d) = (r.aabb[3], r.aabb[5]);
        if w.min(d) < 18.0 {
            continue;
        }
        let want = if w * d >= 1200.0 { 4 } else { 2 };
        let mut placed: Vec<[f32; 4]> = Vec::new();
        for _ in 0..want * 12 {
            if placed.len() >= want {
                break;
            }
            let size = rng.pick(&[2.0, 3.0]);
            let x = rng.span(r.aabb[0] + PILLAR_INSET, r.aabb[0] + w - PILLAR_INSET - size);
            let z = rng.span(r.aabb[2] + PILLAR_INSET, r.aabb[2] + d - PILLAR_INSET - size);
            let rect = [x, x + size, z, z + size];
            let (px, pz) = (x + size * 0.5, z + size * 0.5);
            let crowded = placed.iter().any(|o| near(rect, *o, PILLAR_GAP))
                || taken
                    .iter()
                    .any(|&(room, ox, oz)| room == i && (ox - px).abs() < 5.0 && (oz - pz).abs() < 5.0)
                || plan.reserved(i, rect, PILLAR_GAP);
            if !crowded {
                b.pillar_in(ids[i], x, z, size);
                placed.push(rect);
            }
        }
    }
    b.finish()
}

// ─── Choosing ──────────────────────────────────────────────────────────────────

/// How good a level is, or `None` if it has a FAIL.
///
/// In order of weight: loops and a low share of dead-ends (the multiplayer rules —
/// "loops, not trees"), clean checks and design rules, floors (verticality), room-size
/// variety, room count, and a hair of walkable area to break ties. Every term is read off
/// the report's data, never its prose, and each is bounded so no one of them can buy its
/// way past a level that does everything else better.
pub fn score(d: &ReportData) -> Option<f32> {
    if d.verdict == Status::Fail {
        return None;
    }
    let rooms = d.rooms.len().max(1) as f32;
    let warns = d.checks.iter().chain(&d.lints).filter(|c| c.status == Status::Warn).count() as f32;
    let rules = d.lints.iter().filter(|c| c.status == Status::Pass).count() as f32;
    let cells: usize = d.components.iter().sum();
    let floors = d.floors.len().clamp(1, 3) as f32 - 1.0;
    Some(
        10.0 * d.loops.clamp(0, 4) as f32 - 15.0 * d.dead_ends.len() as f32 / rooms - 4.0 * warns
            + 2.0 * rules
            + 4.0 * floors
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
