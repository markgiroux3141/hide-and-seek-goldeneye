//! The edge walker: drive one chr along a graph edge with the real movement code
//! and report how it went. Stage 2 walks every link of PD's own graph with it;
//! stage 4's check S4 walks our generated graph the same way.
//!
//! A walk is a [`Sim::walk_straight_to`] (a spike harness, not PD): the bot's
//! GoPos steering, `bot_update_lateral`, `chr_calculate_push_pos` and the ground
//! code do the rest.

use glam::{Vec2, Vec3};

use super::chr::Act;
use super::level_geom::FloorKind;
use super::pd_nav::PadFlags;
use super::pd_tiles::{wpseg_get_id, PdStage, WPSEGFLAG_INWARDSONLY, WPSEGFLAG_OUTWARDSONLY};
use super::sim::Sim;
use super::tile_level::TileLevel;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Stopped within 40 cm of the goal, on the goal's floor (or, for a goal at a
    /// ladder, while still climbing it: a route would carry on up).
    Arrived,
    /// Reached the goal's XZ, but on another floor.
    WrongFloor,
    /// Stood still for a second and gave up (`chr_tick_gopos` re-routes, finds no
    /// graph, stops).
    Stuck,
    /// Still walking at twice the nominal time plus a second.
    Timeout,
}

#[derive(Clone, Copy, Debug)]
pub struct Walk {
    pub outcome: Outcome,
    pub ticks: i32,
    /// XZ distance ÷ the bot's max speed, in ticks.
    pub nominal: i32,
    /// Largest gap between `manground` and the floor under the chr during the walk
    /// (cm): above 69 it went off a ledge rather than down a step.
    pub max_drop: f32,
    /// Frames on which the chr slid along or was refused by an obstacle.
    pub blocked_frames: i32,
    pub end: Vec3,
    /// On a ladder when the walk ended.
    pub on_ladder: bool,
    /// Where the chr was placed.
    pub start: Vec3,
}

/// The floor a PD pad belongs to. PD places pads 52–121 cm above their floor
/// (measured over Complex's 144 waypoint and 19 spawn pads), and some pads sit on
/// a railing line or a walkway edge, just off their tile. So: the highest floor
/// polygon within 60 cm (XZ) of the pad whose height there is 40–130 cm below it.
/// Returns `(height, polygon)`.
pub fn pd_pad_floor(level: &TileLevel, pad: Vec3) -> Option<(f32, usize)> {
    let mut best: Option<(f32, usize)> = None;
    for (i, p) in level.geom.polys.iter().enumerate() {
        if !matches!(p.floor_kind(), Some(FloorKind::Flat | FloorKind::Ramp)) {
            continue;
        }
        let q = nearest_point_xz(p, Vec2::new(pad.x, pad.z));
        if q.distance(Vec2::new(pad.x, pad.z)) > 60.0 {
            continue;
        }
        let y = p.find_y(q.x, q.y);
        let above = pad.y - y;
        if (40.0..=130.0).contains(&above) && best.map_or(true, |(by, _)| y > by) {
            best = Some((y, i));
        }
    }
    best
}

fn nearest_point_xz(p: &super::level_geom::GeomPoly, pt: Vec2) -> Vec2 {
    if p.xz_in_convex(pt.x, pt.y) {
        return pt;
    }
    let v = &p.verts;
    let mut best = Vec2::new(v[0].x, v[0].z);
    for i in 0..v.len() {
        let (a, b) = (Vec2::new(v[i].x, v[i].z), Vec2::new(v[(i + 1) % v.len()].x, v[(i + 1) % v.len()].z));
        let ab = b - a;
        let t = if ab.length_squared() > 0.0 { ((pt - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
        let c = a + ab * t;
        if c.distance(pt) < best.distance(pt) {
            best = c;
        }
    }
    best
}

/// Walk chr `i` from pad `from` to pad `to`. The chr starts where a spawn at
/// `from` would put it ([`Sim::place`]: PD's spawn adjustment, then the floor) and
/// steers at `to` itself, as PD steers at a waypoint's pad. It should end on
/// either floor that can claim the goal: where a spawn at `to` would stand, or
/// `to_floor` (e.g. [`pd_pad_floor`]) — the two disagree for pads on a railing
/// line or a walkway edge. A walk that ends mid-fall is given up to 1.5 s to land.
pub fn walk(sim: &mut Sim, i: usize, from: Vec3, to: Vec3, to_floor: Option<f32>) -> Walk {
    walk_from(sim, i, Start::Pad(from), to, to_floor)
}

/// Where a walk starts.
#[derive(Clone, Copy, Debug)]
pub enum Start {
    /// A PD pad: placed the way a spawn would be ([`Sim::place`]).
    Pad(Vec3),
    /// A floor point known to be standable (a generated node): placed exactly.
    Floor(Vec3),
}

/// [`walk`] with an explicit kind of start.
pub fn walk_from(sim: &mut Sim, i: usize, from: Start, to: Vec3, to_floor: Option<f32>) -> Walk {
    walk_with_flags(sim, i, from, to, to_floor, PadFlags::default())
}

/// [`walk_from`] towards a pad with PD's `PADFLAG_AI*` bits: heading to a crouch
/// or duck pad sets `GOPOSFLAG_CROUCH`/`DUCK` for the go-to, as `chr_tick_gopos`
/// does on a real route.
pub fn walk_with_flags(sim: &mut Sim, i: usize, from: Start, to: Vec3, to_floor: Option<f32>, flags: PadFlags) -> Walk {
    // The harness drives the bot: no brain second-guessing it, and no graph for a
    // stuck go-to to re-route on (it should stop and be reported instead).
    sim.brains = false;
    let nav = std::mem::replace(&mut sim.nav, super::pd_nav::NavGraph::empty());
    let w = walk_inner(sim, i, from, to, to_floor, flags);
    sim.nav = nav;
    w
}

fn walk_inner(sim: &mut Sim, i: usize, from: Start, to: Vec3, to_floor: Option<f32>, flags: PadFlags) -> Walk {
    while sim.g.lvframe60 < 145 {
        // PD bots don't move for the first 145 ticks of a match.
        sim.frame();
    }
    match from {
        Start::Pad(p) => sim.place(i, p),
        Start::Floor(f) => sim.place_exact(i, f),
    }
    let start_pos = sim.chrs[i].pos;
    let spawn_floor = sim.spawn_spot(i, to, 0.0).y;
    let on_goal_floor = move |y: f32| (y - spawn_floor).abs() <= 40.0 || to_floor.map_or(false, |f| (y - f).abs() <= 40.0);
    // Steer at the pad itself, as PD steers at a waypoint's pad (arrival is tested
    // from `prop->pos`, within 150 cm of the pad's height).
    let goal = to;
    sim.walk_straight_to(i, goal);
    sim.chrs[i].act_gopos.crouch = flags.crouch;
    sim.chrs[i].act_gopos.duck = flags.duck;
    // Nominal time at standing speed. The limit counts each tick in proportion to
    // the max speed the chr had that tick, so a slow stretch (0.35x squatting, 0.5x
    // ducking and on a go-to's last 2 m, as in PD) doesn't read as a stall.
    let standing = {
        let mut c = sim.chrs[i].clone();
        c.height = 185.0;
        super::bot::bot_calculate_max_speed(&c).max(1.0)
    };
    let dist = Vec2::new(goal.x - sim.chrs[i].pos.x, goal.z - sim.chrs[i].pos.z).length();
    let nominal = (dist / standing).ceil() as i32;
    let limit = (nominal * 2 + 60) as f32;
    let mut effective = 0.0f32;
    let start = sim.g.lvframe60;
    let mut max_drop = 0.0f32;
    let mut blocked_frames = 0;
    loop {
        sim.frame();
        let c = &sim.chrs[i];
        max_drop = max_drop.max(c.pos.y - c.ground);
        if c.invalidmove != 0 {
            blocked_frames += 1;
        }
        let ticks = sim.g.lvframe60 - start;
        // The max speed the chr really has this tick (crouch, and the go-to's
        // last-leg slow-down) relative to standing.
        effective += super::bot::bot_calculate_max_speed(c) / standing * sim.g.lvupdate60 as f32;
        let done = c.actiontype != Act::GoPos;
        if done || effective > limit {
            // Let a fall finish before judging the floor.
            for _ in 0..90 {
                let c = &sim.chrs[i];
                if c.fallspeed.y == 0.0 && (c.pos.y - c.ground).abs() < 1.0 {
                    break;
                }
                sim.frame();
                let c = &sim.chrs[i];
                max_drop = max_drop.max(c.pos.y - c.ground);
            }
            let c = &sim.chrs[i];
            let lateral = Vec2::new(goal.x - c.pos.x, goal.z - c.pos.z).length();
            let outcome = if !done {
                Outcome::Timeout
            } else if lateral > 40.0 {
                Outcome::Stuck
            } else if c.onladder || sim.level.cd_find_ladder(c.prop_pos(), c.radius * 2.5, c.height, 1.0 - c.root_height()).is_some() {
                Outcome::Arrived
            } else if !on_goal_floor(c.pos.y) {
                Outcome::WrongFloor
            } else {
                Outcome::Arrived
            };
            let on_ladder = sim.level.cd_find_ladder(c.prop_pos(), c.radius * 2.5, c.height, 1.0 - c.root_height()).is_some();
            return Walk { outcome, ticks, nominal, max_drop, blocked_frames, end: c.pos, on_ladder, start: start_pos };
        }
    }
}

/// Every directed link of PD's graph a bot may travel (`padhalllv.c:365-595`): not
/// if `a`'s entry for `b` is `WPSEGFLAG_INWARDSONLY` ("only arrive at `a` this
/// way"), nor if `b`'s entry for `a` is `WPSEGFLAG_OUTWARDSONLY`. Returns
/// `(a, b, one_way)`.
pub fn pd_links(stage: &PdStage) -> Vec<(usize, usize, bool)> {
    let wps = &stage.pads.waypoints;
    let seg = |a: usize, b: usize| wps[a].neighbours.iter().copied().find(|&s| wpseg_get_id(s) == b);
    let mut out = Vec::new();
    for (a, w) in wps.iter().enumerate() {
        for &s in &w.neighbours {
            let b = wpseg_get_id(s);
            let back = seg(b, a);
            let forward_ok = s & WPSEGFLAG_INWARDSONLY == 0;
            let back_ok = back.map_or(true, |t| t & WPSEGFLAG_OUTWARDSONLY == 0);
            if forward_ok && back_ok {
                let one_way = s & WPSEGFLAG_OUTWARDSONLY != 0 || back.map_or(true, |t| t & WPSEGFLAG_INWARDSONLY != 0);
                out.push((a, b, one_way));
            }
        }
    }
    out
}
