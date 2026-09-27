//! The test arena: a closed square room with four full-height pillars.
//!
//! Everything here is in **Perfect Dark world units (centimetres)** with the floor at
//! `y = 0`, because the bot code it serves is a literal port and its constants
//! (`g_BotDistConfigs`, the 200-unit arrival slow-down, `chr->radius`) are all in
//! those units. Conversion to metres happens once, at the render boundary.
//!
//! The room is centred on the world origin on purpose. `chr_run_from_pos` carries a
//! bug PD itself flags (`chraction.c:15690`): it forgets to add the bot's position
//! to its flee vector, so a backing-up bot heads for `away_dir * 10000` in *world*
//! coordinates. With the origin at the room's centre that still reads as "back away
//! from the target until a wall stops you", which is what it looks like in PD's own
//! arenas.
//!
//! All obstacles are full-height, so every query is a 2D problem in XZ. The floor
//! and ceiling only matter to [`Arena::raycast`], for pitched shots.

use glam::{Vec2, Vec3};

use engine::geometry::csg_runtime::{Brush, Op, WORLD_SCALE};

use super::level_geom::{GeomPoly, LevelGeom};

/// PD units per metre. PD world units are centimetres.
pub const UNITS_PER_M: f32 = 100.0;

/// An axis-aligned box in the XZ plane (PD units).
#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub min: Vec2,
    pub max: Vec2,
}

impl Rect {
    pub fn centred(cx: f32, cz: f32, half: f32) -> Self {
        Rect { min: Vec2::new(cx - half, cz - half), max: Vec2::new(cx + half, cz + half) }
    }

    /// Entry distance of the segment `a + t·d` (t in 0..=1) into this box, if any.
    /// Slab test; returns `t` and the outward normal of the face it entered through.
    fn segment_entry(&self, a: Vec2, d: Vec2) -> Option<(f32, Vec2)> {
        let mut t0 = 0.0f32;
        let mut t1 = 1.0f32;
        let mut normal = Vec2::ZERO;
        for axis in 0..2 {
            let (o, v, lo, hi) = if axis == 0 {
                (a.x, d.x, self.min.x, self.max.x)
            } else {
                (a.y, d.y, self.min.y, self.max.y)
            };
            if v.abs() < 1e-9 {
                if o < lo || o > hi {
                    return None;
                }
                continue;
            }
            let (mut ta, mut tb) = ((lo - o) / v, (hi - o) / v);
            let mut n = if axis == 0 { Vec2::new(-1.0, 0.0) } else { Vec2::new(0.0, -1.0) };
            if ta > tb {
                std::mem::swap(&mut ta, &mut tb);
                n = -n;
            }
            if ta > t0 {
                t0 = ta;
                normal = n;
            }
            t1 = t1.min(tb);
            if t0 > t1 {
                return None;
            }
        }
        // Starting inside counts as no entry: a chr is never inside a pillar, and a
        // ray that starts inside one would otherwise report a hit at its own origin.
        (t0 > 0.0).then_some((t0, normal))
    }
}

/// What a [`Arena::raycast`] struck.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Surface {
    Wall,
    Pillar(usize),
    Floor,
    Ceiling,
}

#[derive(Clone, Copy, Debug)]
pub struct RayHit {
    pub dist: f32,
    pub point: Vec3,
    pub surface: Surface,
}

/// The test room.
#[derive(Clone, Debug)]
pub struct Arena {
    /// Inner face of the four walls (PD units, XZ).
    pub bounds: Rect,
    /// Ceiling height (PD units).
    pub height: f32,
    pub pillars: Vec<Rect>,
}

impl Default for Arena {
    fn default() -> Self {
        Self::standard()
    }
}

impl Arena {
    /// The standard room: 16 m square, 3 m ceiling, four 1.5 m pillars in a pinwheel.
    ///
    /// Same footprint as the game's `pd_lab` level (so experience carries over), but
    /// the pillars are arranged so no two sit on one line through the centre: there
    /// is always an open diagonal to watch an engagement at range, and always a
    /// pillar within a few steps to break line of sight — the two things that make
    /// PD's zeroing and dist-mode logic visible at all.
    pub fn standard() -> Self {
        const HALF: f32 = 800.0;
        const PILLAR_HALF: f32 = 75.0;
        let pillars = [(-350.0, -150.0), (150.0, -350.0), (350.0, 150.0), (-150.0, 350.0)]
            .into_iter()
            .map(|(x, z)| Rect::centred(x, z, PILLAR_HALF))
            .collect();
        Arena {
            bounds: Rect { min: Vec2::splat(-HALF), max: Vec2::splat(HALF) },
            height: 300.0,
            pillars,
        }
    }

    /// Unobstructed line between two points (PD units)? Pillars only — the walls
    /// bound the room and nothing inside it can see past them anyway.
    pub fn los(&self, from: Vec3, to: Vec3) -> bool {
        let a = Vec2::new(from.x, from.z);
        let d = Vec2::new(to.x, to.z) - a;
        !self.pillars.iter().any(|p| p.segment_entry(a, d).is_some())
    }

    /// First surface along `origin + dir·t` up to `max_dist` (PD units). `dir` need
    /// not be normalised; distances are along its normalised direction.
    pub fn raycast(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> Option<RayHit> {
        let dir = dir.normalize_or_zero();
        if dir == Vec3::ZERO {
            return None;
        }
        let end = origin + dir * max_dist;
        let a = Vec2::new(origin.x, origin.z);
        let d = Vec2::new(end.x, end.z) - a;
        let mut best: Option<(f32, Surface)> = None;
        let mut consider = |t: f32, s: Surface| {
            if t >= 0.0 && t <= 1.0 && best.map_or(true, |(bt, _)| t < bt) {
                best = Some((t, s));
            }
        };
        for (i, p) in self.pillars.iter().enumerate() {
            if let Some((t, _)) = p.segment_entry(a, d) {
                consider(t, Surface::Pillar(i));
            }
        }
        // Walls: the ray is inside the bounds, so it leaves through exactly one face.
        for (o, v, lo, hi) in [
            (a.x, d.x, self.bounds.min.x, self.bounds.max.x),
            (a.y, d.y, self.bounds.min.y, self.bounds.max.y),
        ] {
            if v > 1e-9 {
                consider((hi - o) / v, Surface::Wall);
            } else if v < -1e-9 {
                consider((lo - o) / v, Surface::Wall);
            }
        }
        let dy = end.y - origin.y;
        if dy < -1e-9 {
            consider(-origin.y / dy, Surface::Floor);
        } else if dy > 1e-9 {
            consider((self.height - origin.y) / dy, Surface::Ceiling);
        }
        best.map(|(t, surface)| RayHit {
            dist: t * max_dist,
            point: origin + (end - origin) * t,
            surface,
        })
    }

    /// Resolve a chr cylinder of `radius` moving from `old` to `new` against the
    /// walls and pillars: slide along whatever it touches.
    ///
    /// Stand-in for `chr_calculate_push_pos` (PD's cylinder-vs-geometry push, which
    /// works on the room's collision tiles). In a room made only of axis-aligned
    /// boxes, "push the circle out of every box it overlaps, then clamp to the walls"
    /// is the same answer.
    pub fn push_pos(&self, old: Vec2, new: Vec2, radius: f32) -> Vec2 {
        let _ = old;
        let mut p = new;
        for _ in 0..2 {
            for r in &self.pillars {
                let q = p.clamp(r.min, r.max);
                let off = p - q;
                let d2 = off.length_squared();
                if d2 < radius * radius {
                    if d2 > 1e-6 {
                        p = q + off / d2.sqrt() * radius;
                    } else {
                        // Centre inside the box: leave along the shallowest axis.
                        let exits = [
                            (p.x - r.min.x, Vec2::new(r.min.x - radius, p.y)),
                            (r.max.x - p.x, Vec2::new(r.max.x + radius, p.y)),
                            (p.y - r.min.y, Vec2::new(p.x, r.min.y - radius)),
                            (r.max.y - p.y, Vec2::new(p.x, r.max.y + radius)),
                        ];
                        p = exits.iter().min_by(|a, b| a.0.total_cmp(&b.0)).unwrap().1;
                    }
                }
            }
            p = p.clamp(self.bounds.min + Vec2::splat(radius), self.bounds.max - Vec2::splat(radius));
        }
        p
    }

    /// Is a chr cylinder of `radius` at `p` clear of every obstacle?
    pub fn is_clear(&self, p: Vec2, radius: f32) -> bool {
        let inside = p.x >= self.bounds.min.x + radius
            && p.x <= self.bounds.max.x - radius
            && p.y >= self.bounds.min.y + radius
            && p.y <= self.bounds.max.y - radius;
        inside
            && self.pillars.iter().all(|r| (p - p.clamp(r.min, r.max)).length_squared() >= radius * radius)
    }

    /// Can a cylinder of `radius` travel in a straight line from `a` to `b`?
    /// Stand-in for `chr_prop_can_move_to_pos_without_nav` against background
    /// geometry: each pillar is grown by the radius and the centre line tested.
    pub fn cylinder_path_clear(&self, a: Vec3, b: Vec3, radius: f32) -> bool {
        let a2 = Vec2::new(a.x, a.z);
        let b2 = Vec2::new(b.x, b.z);
        if !self.is_clear(a2, radius * 0.99) || !self.is_clear(b2, radius) {
            return false;
        }
        let d = b2 - a2;
        !self.pillars.iter().any(|p| {
            let grown = Rect { min: p.min - Vec2::splat(radius), max: p.max + Vec2::splat(radius) };
            grown.segment_entry(a2, d).is_some()
        })
    }

    /// The room as generic level geometry: a floor, a ceiling (blocks sight and
    /// shots only), the four walls and each pillar's four sides. This is the second
    /// adapter onto [`LevelGeom`] after PD's tiles, so the arena runs through the
    /// same ported collision code as Complex.
    pub fn geom(&self) -> LevelGeom {
        let (b, h) = (self.bounds, self.height);
        let quad_y = |y: f32| {
            vec![
                Vec3::new(b.min.x, y, b.min.y),
                Vec3::new(b.max.x, y, b.min.y),
                Vec3::new(b.max.x, y, b.max.y),
                Vec3::new(b.min.x, y, b.max.y),
            ]
        };
        let wall = |a: Vec2, c: Vec2| {
            vec![Vec3::new(a.x, 0.0, a.y), Vec3::new(c.x, 0.0, c.y), Vec3::new(c.x, h, c.y), Vec3::new(a.x, h, a.y)]
        };
        let sides = |r: &Rect| {
            let (p0, p1, p2, p3) =
                (r.min, Vec2::new(r.max.x, r.min.y), r.max, Vec2::new(r.min.x, r.max.y));
            [wall(p0, p1), wall(p1, p2), wall(p2, p3), wall(p3, p0)]
        };
        let mut polys = vec![
            GeomPoly::new(quad_y(0.0), true, false, true, true, Some(0)),
            GeomPoly::new(quad_y(h), false, false, true, true, Some(0)),
        ];
        for r in std::iter::once(&self.bounds).chain(&self.pillars) {
            for v in sides(r) {
                polys.push(GeomPoly::new(v, false, true, true, true, Some(0)));
            }
        }
        LevelGeom { polys, room_names: vec![(0, "arena".to_string())] }
    }

    /// The CSG brushes that draw this room: one subtract for the cavity, one add per
    /// pillar. Brushes are min-corner boxes in world tiles (0.25 m).
    pub fn brushes(&self) -> Vec<Brush> {
        let wt = |units: f32| units / UNITS_PER_M / WORLD_SCALE;
        let b = self.bounds;
        let mut out = vec![Brush::new(
            1,
            Op::Subtract,
            wt(b.min.x),
            0.0,
            wt(b.min.y),
            wt(b.max.x - b.min.x),
            wt(self.height),
            wt(b.max.y - b.min.y),
        )];
        for (i, p) in self.pillars.iter().enumerate() {
            out.push(Brush::new(
                2 + i as u32,
                Op::Add,
                wt(p.min.x),
                0.0,
                wt(p.min.y),
                wt(p.max.x - p.min.x),
                wt(self.height),
                wt(p.max.y - p.min.y),
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pillar_blocks_sight_and_open_floor_does_not() {
        let a = Arena::standard();
        let p = a.pillars[0];
        let c = (p.min + p.max) * 0.5;
        let left = Vec3::new(c.x - 300.0, 150.0, c.y);
        let right = Vec3::new(c.x + 300.0, 150.0, c.y);
        assert!(!a.los(left, right));
        let above = Vec3::new(c.x - 300.0, 150.0, c.y + 200.0);
        let above_r = Vec3::new(c.x + 300.0, 150.0, c.y + 200.0);
        assert!(a.los(above, above_r));
    }

    #[test]
    fn raycasts_stop_at_the_nearest_surface() {
        let a = Arena::standard();
        let hit = a.raycast(Vec3::new(0.0, 100.0, 0.0), Vec3::X, 5000.0).unwrap();
        assert_eq!(hit.surface, Surface::Wall);
        assert!((hit.dist - 800.0).abs() < 0.5, "{}", hit.dist);
        let down = a.raycast(Vec3::new(0.0, 100.0, 0.0), Vec3::new(0.0, -1.0, 0.0), 5000.0).unwrap();
        assert_eq!(down.surface, Surface::Floor);
        assert!((down.dist - 100.0).abs() < 0.5);
    }

    #[test]
    fn pushing_into_a_pillar_slides_out_to_its_face() {
        let a = Arena::standard();
        let p = a.pillars[0];
        let c = (p.min + p.max) * 0.5;
        let out = a.push_pos(c, Vec2::new(p.min.x + 5.0, c.y), 20.0);
        assert!(a.is_clear(out, 19.9), "{out:?}");
    }
}
