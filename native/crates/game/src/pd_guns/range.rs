//! The firing range: a long room with crates and target boards, in PD world
//! units (centimetres, y up, floor at 0). Stand-in for PD's room/portal geometry
//! and prop collision — everything is an axis-aligned box, so the collision and
//! raycast here are exact for this world rather than ports of PD's tile code.

use glam::{Vec2, Vec3};

use engine::geometry::csg_runtime::{Brush, Op, WORLD_SCALE};

pub const UNITS_PER_M: f32 = 100.0;

#[derive(Clone, Copy, Debug)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub fn new(min: Vec3, max: Vec3) -> Self {
        Aabb { min, max }
    }

    /// Slab test: entry distance along `o + d·t` and the entry face normal.
    pub fn ray(&self, o: Vec3, d: Vec3, tmax: f32) -> Option<(f32, Vec3)> {
        let mut t0 = 0.0f32;
        let mut t1 = tmax;
        let mut n = Vec3::ZERO;
        for a in 0..3 {
            let (oa, da, lo, hi) = (o[a], d[a], self.min[a], self.max[a]);
            if da.abs() < 1e-9 {
                if oa < lo || oa > hi {
                    return None;
                }
                continue;
            }
            let mut ta = (lo - oa) / da;
            let mut tb = (hi - oa) / da;
            let mut na = Vec3::ZERO;
            na[a] = -1.0;
            if ta > tb {
                std::mem::swap(&mut ta, &mut tb);
                na = -na;
            }
            if ta > t0 {
                t0 = ta;
                n = na;
            }
            t1 = t1.min(tb);
            if t0 > t1 {
                return None;
            }
        }
        if n == Vec3::ZERO {
            return None; // origin inside
        }
        Some((t0, n))
    }
}

/// What a shot hit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HitKind {
    World,
    Target(usize),
}

#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub dist: f32,
    pub pos: Vec3,
    pub normal: Vec3,
    pub kind: HitKind,
}

/// A target board: a thin box that counts hits and flashes.
#[derive(Clone, Debug)]
pub struct Target {
    pub bbox: Aabb,
    pub hits: u32,
    pub damage: f32,
    pub flash: f32,
    /// Board face centre + half-extents in its plane, for drawing.
    pub face: Vec3,
    pub half: Vec2,
}

pub struct Range {
    /// Inner faces of the walls.
    pub bounds: Aabb,
    pub solids: Vec<Aabb>,
    pub targets: Vec<Target>,
}

impl Range {
    /// A 12 m × 36 m hall, 4 m high. The player starts at the south end looking
    /// north (+z); target boards stand at 5, 10, 20 and 30 m, with crates to
    /// shoot around and pillars to strafe behind.
    pub fn standard() -> Self {
        let bounds = Aabb::new(Vec3::new(-600.0, 0.0, -300.0), Vec3::new(600.0, 400.0, 3300.0));
        let mut solids = Vec::new();
        // Crates
        for (x, z, s, h) in [
            (-250.0, 350.0, 50.0, 100.0),
            (220.0, 450.0, 60.0, 120.0),
            (-120.0, 1300.0, 45.0, 90.0),
            (330.0, 1500.0, 50.0, 100.0),
            (0.0, 2300.0, 70.0, 140.0),
        ] {
            solids.push(Aabb::new(Vec3::new(x - s, 0.0, z - s), Vec3::new(x + s, h, z + s)));
        }
        // Pillars
        for (x, z) in [(-420.0, 900.0), (420.0, 900.0), (-420.0, 2000.0), (420.0, 2000.0)] {
            solids.push(Aabb::new(Vec3::new(x - 60.0, 0.0, z - 60.0), Vec3::new(x + 60.0, 400.0, z + 60.0)));
        }
        let mut targets = Vec::new();
        for (x, z) in [(0.0, 500.0), (-200.0, 1000.0), (200.0, 1000.0), (0.0, 2000.0), (-150.0, 3000.0), (150.0, 3000.0)] {
            let half = Vec2::new(40.0, 60.0);
            let centre_y = 110.0;
            targets.push(Target {
                bbox: Aabb::new(Vec3::new(x - half.x, centre_y - half.y, z - 4.0), Vec3::new(x + half.x, centre_y + half.y, z + 4.0)),
                hits: 0,
                damage: 0.0,
                flash: 0.0,
                face: Vec3::new(x, centre_y, z - 4.0),
                half,
            });
        }
        Range { bounds, solids, targets }
    }

    /// The floor under `pos`: the highest box top at or below it, else the
    /// room floor (`cd_find_room_at_pos_ycnp`'s answer for the range). `None`
    /// outside the room.
    pub fn floor_below(&self, pos: Vec3) -> Option<(f32, Vec3, bool)> {
        let b = self.bounds;
        if pos.x < b.min.x || pos.x > b.max.x || pos.z < b.min.z || pos.z > b.max.z {
            return None;
        }
        let mut y = b.min.y;
        for s in &self.solids {
            if pos.x >= s.min.x && pos.x <= s.max.x && pos.z >= s.min.z && pos.z <= s.max.z && s.max.y <= pos.y + 0.01 && s.max.y > y {
                y = s.max.y;
            }
        }
        Some((y, Vec3::Y, false))
    }

    pub fn spawn(&self) -> (Vec3, f32) {
        (Vec3::new(0.0, 0.0, -150.0), 0.0)
    }

    /// First thing along `o + d·t`, `t ≤ tmax`. `d` is normalised here.
    pub fn raycast(&self, o: Vec3, d: Vec3, tmax: f32) -> Option<Hit> {
        let d = d.normalize_or_zero();
        if d == Vec3::ZERO {
            return None;
        }
        let mut best: Option<Hit> = None;
        let mut take = |t: f32, n: Vec3, kind: HitKind| {
            if t >= 0.0 && t <= tmax && best.is_none_or(|b| t < b.dist) {
                best = Some(Hit { dist: t, pos: o + d * t, normal: n, kind });
            }
        };
        for s in &self.solids {
            if let Some((t, n)) = s.ray(o, d, tmax) {
                take(t, n, HitKind::World);
            }
        }
        for (i, tg) in self.targets.iter().enumerate() {
            if let Some((t, n)) = tg.bbox.ray(o, d, tmax) {
                take(t, n, HitKind::Target(i));
            }
        }
        // Room shell: the ray leaves through exactly one face per axis.
        for a in 0..3 {
            if d[a] > 1e-9 {
                let mut n = Vec3::ZERO;
                n[a] = -1.0;
                take((self.bounds.max[a] - o[a]) / d[a], n, HitKind::World);
            } else if d[a] < -1e-9 {
                let mut n = Vec3::ZERO;
                n[a] = 1.0;
                take((self.bounds.min[a] - o[a]) / d[a], n, HitKind::World);
            }
        }
        best
    }

    /// [`Range::raycast`] against the world only (boxes and the shell).
    pub fn raycast_world(&self, o: Vec3, d: Vec3, tmax: f32) -> Option<Hit> {
        let no_boards = Range { bounds: self.bounds, solids: self.solids.clone(), targets: Vec::new() };
        no_boards.raycast(o, d, tmax)
    }

    /// [`Range::raycast`] against the target boards only.
    pub fn raycast_targets(&self, o: Vec3, d: Vec3, tmax: f32) -> Option<Hit> {
        let d = d.normalize_or_zero();
        let mut best: Option<Hit> = None;
        for (i, tg) in self.targets.iter().enumerate() {
            if let Some((t, n)) = tg.bbox.ray(o, d, tmax) {
                if t >= 0.0 && best.is_none_or(|b| t < b.dist) {
                    best = Some(Hit { dist: t, pos: o + d * t, normal: n, kind: HitKind::Target(i) });
                }
            }
        }
        best
    }

    /// Stand-in for `bwalk_resolve_posdelta` (`bondwalk.c:1257`): move a player
    /// cylinder of `radius` by a horizontal `delta`, sliding along what it hits.
    /// Obstacles lower than 30 cm are ignored the way PD steps up onto them.
    pub fn resolve(&self, pos: Vec3, delta: Vec3, radius: f32) -> Vec3 {
        let feet = 0.0f32;
        let mut p = Vec2::new(pos.x + delta.x, pos.z + delta.z);
        for _ in 0..3 {
            for s in &self.solids {
                if s.max.y <= feet + 30.0 {
                    continue;
                }
                let mn = Vec2::new(s.min.x, s.min.z);
                let mx = Vec2::new(s.max.x, s.max.z);
                let q = p.clamp(mn, mx);
                let off = p - q;
                let d2 = off.length_squared();
                if d2 < radius * radius {
                    if d2 > 1e-6 {
                        p = q + off / d2.sqrt() * radius;
                    } else {
                        let exits = [
                            (p.x - mn.x, Vec2::new(mn.x - radius, p.y)),
                            (mx.x - p.x, Vec2::new(mx.x + radius, p.y)),
                            (p.y - mn.y, Vec2::new(p.x, mn.y - radius)),
                            (mx.y - p.y, Vec2::new(p.x, mx.y + radius)),
                        ];
                        p = exits.iter().min_by(|a, b| a.0.total_cmp(&b.0)).unwrap().1;
                    }
                }
            }
            p = p.clamp(
                Vec2::new(self.bounds.min.x + radius, self.bounds.min.z + radius),
                Vec2::new(self.bounds.max.x - radius, self.bounds.max.z - radius),
            );
        }
        Vec3::new(p.x, pos.y, p.y)
    }

    /// CSG brushes for the engine's world renderer (boxes in 0.25 m world tiles).
    pub fn brushes(&self) -> Vec<Brush> {
        let wt = |u: f32| u / UNITS_PER_M / WORLD_SCALE;
        let b = self.bounds;
        let mut out = vec![Brush::new(1, Op::Subtract, wt(b.min.x), wt(b.min.y), wt(b.min.z), wt(b.max.x - b.min.x), wt(b.max.y - b.min.y), wt(b.max.z - b.min.z))];
        for (i, s) in self.solids.iter().enumerate() {
            out.push(Brush::new(2 + i as u32, Op::Add, wt(s.min.x), wt(s.min.y), wt(s.min.z), wt(s.max.x - s.min.x), wt(s.max.y - s.min.y), wt(s.max.z - s.min.z)));
        }
        out
    }
}

impl super::explosions::ExpWorld for Range {
    fn room_bbox(&self) -> (Vec3, Vec3) {
        (self.bounds.min, self.bounds.max)
    }
    fn floor_below(&self, pos: Vec3) -> Option<(f32, Vec3, bool)> {
        Range::floor_below(self, pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shot_down_the_range_hits_the_first_board() {
        let r = Range::standard();
        let h = r.raycast(Vec3::new(0.0, 110.0, 0.0), Vec3::Z, 10000.0).unwrap();
        assert_eq!(h.kind, HitKind::Target(0));
        assert!((h.pos.z - 496.0).abs() < 0.5, "{:?}", h.pos);
        assert_eq!(h.normal, Vec3::new(0.0, 0.0, -1.0));
    }

    #[test]
    fn walking_into_a_crate_slides_along_it() {
        let r = Range::standard();
        let c = r.solids[0];
        let start = Vec3::new(c.min.x - 40.0, 159.0, (c.min.z + c.max.z) * 0.5);
        let p = r.resolve(start, Vec3::new(30.0, 0.0, 5.0), 30.0);
        assert!(p.x <= c.min.x - 29.9, "{p:?}");
        assert!(p.z > start.z, "still slides along z: {p:?}");
    }
}
