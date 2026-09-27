//! The firing range: a long room with crates and target boards, in PD world
//! units (centimetres, y up, floor at 0). Stand-in for PD's room/portal geometry
//! and prop collision — everything is an axis-aligned box, so the collision and
//! raycast here are exact for this world rather than ports of PD's tile code.
//!
//! The same type also carries a **PD stage** (`Range::for_stage`): then the
//! world is the stage's collision tiles (the simulant spike's port of
//! `lib/collision.c`), there is no box shell, and chrs are hit through
//! [`Target`]s whose `chr` is set (their perimeter's bounding box).

use std::sync::Arc;

use glam::{Vec2, Vec3};

use engine::geometry::csg_runtime::{Brush, Op, WORLD_SCALE};

use crate::pd_spike::level_geom::{GeomPoly, LevelGeom};
use crate::pd_spike::tile_level::{CdResult, TileLevel};

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
    /// For a chr: the `HITPART_*` of the box that was hit ([`HITPART_GENERAL`]
    /// when the chr has no boxes).
    pub hitpart: i32,
}

/// `HITPART_GENERAL` (`constants.h:1412`): no body part.
pub const HITPART_GENERAL: i32 = 200;

/// One of a chr model's `BBOX` nodes, posed for this frame: world (cm) to the
/// box's bone space, the box there, its `HITPART_*`, and the box it sits under
/// (skipped when that one is missed, as `model_test_for_hit` skips a missed
/// box's children).
#[derive(Clone, Debug)]
pub struct PartBox {
    pub to_local: glam::Mat4,
    pub min: Vec3,
    pub max: Vec3,
    pub hitpart: i32,
    pub parent: Option<usize>,
}

/// `model_test_for_hit` (`model.c:3785`) + `model_test_bbox_node_for_hit`
/// (`:3593`), the multiplayer ("cheap") path of `chr_test_hit` (`chr.c:4502`):
/// the FIRST box in the model's tree order the ray passes through wins, not the
/// nearest. Returns the distance along the ray and the box's hit part.
/// SUBSTITUTION: PD's single-player path then refines the position on the
/// model's triangles (`projectile_0f06bea0`).
pub fn test_part_boxes(parts: &[PartBox], o: Vec3, d: Vec3, tmax: f32) -> Option<(f32, i32)> {
    let mut missed = vec![false; parts.len()];
    for (i, b) in parts.iter().enumerate() {
        if b.parent.is_some_and(|p| p < i && missed[p]) {
            missed[i] = true;
            continue;
        }
        let lo = b.to_local.transform_point3(o);
        let ld = b.to_local.transform_vector3(d);
        // Distances along `ld` are world distances along `d`: the bone matrices
        // are rigid up to one uniform scale, which `ld`'s length carries.
        let scale = ld.length();
        if scale < 1e-9 {
            missed[i] = true;
            continue;
        }
        match Aabb::new(b.min, b.max).ray(lo, ld / scale, tmax * scale) {
            Some((t, _)) => return Some((t / scale, b.hitpart)),
            None => missed[i] = true,
        }
    }
    None
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
    /// `Some(chr)`: this is a chr's hit volume, not a board — hits on it go to
    /// the chr (`chr_hit`), not the board counters, and it isn't drawn.
    pub chr: Option<usize>,
    /// A chr's posed body-part boxes; empty = the whole bbox is the target.
    pub parts: Vec<PartBox>,
}

impl Target {
    /// A chr's hit volume: the bounding box of its perimeter cylinder.
    /// SUBSTITUTION: PD tests the shot against each of the chr model's BBOX
    /// nodes (`bg_test_hit_on_chr`), which also gives the body part.
    pub fn chr(chr: usize, x: f32, z: f32, radius: f32, ymin: f32, ymax: f32) -> Target {
        let bbox = Aabb::new(Vec3::new(x - radius, ymin, z - radius), Vec3::new(x + radius, ymax, z + radius));
        Target {
            bbox,
            hits: 0,
            damage: 0.0,
            flash: 0.0,
            face: Vec3::new(x, (ymin + ymax) * 0.5, z),
            half: Vec2::new(radius, (ymax - ymin) * 0.5),
            chr: Some(chr),
            parts: Vec::new(),
        }
    }
}

pub struct Range {
    /// Inner faces of the walls (the range), or the stage's bounding box.
    pub bounds: Aabb,
    pub solids: Vec<Aabb>,
    pub targets: Vec<Target>,
    /// A PD stage's collision tiles: shots stop on `GEOFLAG_BLOCK_SHOOT`
    /// polygons, objects collide with its walls and land on its floors.
    pub tiles: Option<Arc<TileLevel>>,
    /// The bounds are walls (the range's shell). A PD stage has none.
    pub shell: bool,
    /// A PD stage's BG triangles (world cm), which the x-ray draws
    /// (`bg_render_scene_in_xray`) in place of the range's tessellated boxes.
    pub xray_tris: Option<Arc<Vec<[Vec3; 3]>>>,
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
                chr: None,
                parts: Vec::new(),
            });
        }
        Range { bounds, solids, targets, tiles: None, shell: true, xray_tris: None }
    }

    /// A PD stage: its collision tiles and nothing else.
    pub fn for_stage(tiles: Arc<TileLevel>) -> Self {
        let (lo, hi) = tiles.geom.bounds();
        let bounds = Aabb::new(lo - Vec3::splat(100.0), hi + Vec3::splat(100.0));
        Range { bounds, solids: Vec::new(), targets: Vec::new(), tiles: Some(tiles), shell: false, xray_tris: None }
    }

    /// The boards (targets that aren't chrs).
    pub fn boards(&self) -> impl Iterator<Item = &Target> {
        self.targets.iter().filter(|t| t.chr.is_none())
    }

    /// The bounding box of the room `pos` stands in (its floor's room), else
    /// the whole world — what an explosion is confined to (`g_Rooms[].bbmin/bbmax`).
    pub fn room_bbox_at(&self, pos: Vec3) -> (Vec3, Vec3) {
        if let Some(t) = &self.tiles {
            if let Some(room) = t.floor_room(pos, 1.0) {
                let mut lo = Vec3::splat(f32::INFINITY);
                let mut hi = Vec3::splat(f32::NEG_INFINITY);
                for p in t.geom.polys.iter().filter(|p| p.room == Some(room)) {
                    for v in &p.verts {
                        lo = lo.min(*v);
                        hi = hi.max(*v);
                    }
                }
                if lo.x <= hi.x {
                    return (lo, hi);
                }
            }
        }
        (self.bounds.min, self.bounds.max)
    }

    /// The floor under `pos`: the highest box top at or below it, else the
    /// room floor (`cd_find_room_at_pos_ycnp`'s answer for the range). `None`
    /// outside the room.
    pub fn floor_below(&self, pos: Vec3) -> Option<(f32, Vec3, bool)> {
        if let Some(t) = &self.tiles {
            // cd_find_room_at_pos_ycnp over the stage's floor tiles.
            let (y, poly) = t.cd_find_ground_at_cyl(pos, 0.1);
            let poly = poly?;
            let n = t.geom.polys[poly].normal;
            return Some((y, if n.y < 0.0 { -n } else { n }, false));
        }
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
        let mut take = |t: f32, n: Vec3, kind: HitKind, hitpart: i32| {
            if t >= 0.0 && t <= tmax && best.is_none_or(|b| t < b.dist) {
                best = Some(Hit { dist: t, pos: o + d * t, normal: n, kind, hitpart });
            }
        };
        for s in &self.solids {
            if let Some((t, n)) = s.ray(o, d, tmax) {
                take(t, n, HitKind::World, HITPART_GENERAL);
            }
        }
        for (i, tg) in self.targets.iter().enumerate() {
            if let Some((t, n)) = tg.bbox.ray(o, d, tmax) {
                if tg.parts.is_empty() {
                    take(t, n, HitKind::Target(i), HITPART_GENERAL);
                } else if let Some((tp, hp)) = test_part_boxes(&tg.parts, o, d, tmax) {
                    take(tp, -d, HitKind::Target(i), hp);
                }
            }
        }
        // A PD stage: the first shot-blocking tile.
        if let Some(t) = &self.tiles {
            if let Some(h) = t.raycast_shoot(o, d, tmax) {
                let n = t.geom.polys[h.poly].normal;
                take(h.dist, if n.dot(d) > 0.0 { -n } else { n }, HitKind::World, HITPART_GENERAL);
            }
        }
        // Room shell: the ray leaves through exactly one face per axis.
        for a in (0..3).filter(|_| self.shell) {
            if d[a] > 1e-9 {
                let mut n = Vec3::ZERO;
                n[a] = -1.0;
                take((self.bounds.max[a] - o[a]) / d[a], n, HitKind::World, HITPART_GENERAL);
            } else if d[a] < -1e-9 {
                let mut n = Vec3::ZERO;
                n[a] = 1.0;
                take((self.bounds.min[a] - o[a]) / d[a], n, HitKind::World, HITPART_GENERAL);
            }
        }
        best
    }

    /// [`Range::raycast`] against the world only (boxes and the shell).
    pub fn raycast_world(&self, o: Vec3, d: Vec3, tmax: f32) -> Option<Hit> {
        let no_boards = Range { bounds: self.bounds, solids: self.solids.clone(), targets: Vec::new(), tiles: self.tiles.clone(), shell: self.shell, xray_tris: None };
        no_boards.raycast(o, d, tmax)
    }

    /// [`Range::raycast`] against the target boards only.
    pub fn raycast_targets(&self, o: Vec3, d: Vec3, tmax: f32) -> Option<Hit> {
        let d = d.normalize_or_zero();
        let mut best: Option<Hit> = None;
        for (i, tg) in self.targets.iter().enumerate() {
            if let Some((t, n)) = tg.bbox.ray(o, d, tmax) {
                let (t, n, hitpart) = if tg.parts.is_empty() {
                    (t, n, HITPART_GENERAL)
                } else {
                    match test_part_boxes(&tg.parts, o, d, tmax) {
                        Some((tp, hp)) => (tp, -d, hp),
                        None => continue,
                    }
                };
                if t >= 0.0 && best.is_none_or(|b| t < b.dist) {
                    best = Some(Hit { dist: t, pos: o + d * t, normal: n, kind: HitKind::Target(i), hitpart });
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

    /// The range as PD collision polygons (the simulant spike's generic
    /// [`LevelGeom`]), so the player walks it with the ported `bondwalk.c`: the
    /// floor, the shell's four inner walls, and each box's four sides (walls) and
    /// top (floor).
    pub fn geom(&self) -> LevelGeom {
        let b = self.bounds;
        let floor = |y: f32, mn: Vec3, mx: Vec3| {
            GeomPoly::new(
                vec![Vec3::new(mn.x, y, mn.z), Vec3::new(mx.x, y, mn.z), Vec3::new(mx.x, y, mx.z), Vec3::new(mn.x, y, mx.z)],
                true,
                false,
                true,
                true,
                Some(0),
            )
        };
        let sides = |mn: Vec3, mx: Vec3, out: &mut Vec<GeomPoly>| {
            let c = [Vec3::new(mn.x, 0.0, mn.z), Vec3::new(mx.x, 0.0, mn.z), Vec3::new(mx.x, 0.0, mx.z), Vec3::new(mn.x, 0.0, mx.z)];
            for i in 0..4 {
                let (p, q) = (c[i], c[(i + 1) % 4]);
                let v = vec![Vec3::new(p.x, mn.y, p.z), Vec3::new(q.x, mn.y, q.z), Vec3::new(q.x, mx.y, q.z), Vec3::new(p.x, mx.y, p.z)];
                out.push(GeomPoly::new(v, false, true, true, true, Some(0)));
            }
        };
        let mut polys = vec![floor(b.min.y, b.min, b.max)];
        sides(b.min, b.max, &mut polys);
        for s in &self.solids {
            sides(s.min, s.max, &mut polys);
            polys.push(floor(s.max.y, s.min, s.max));
        }
        LevelGeom { polys, room_names: vec![(0, "range".to_string())] }
    }

    /// A PD stage's half of `func0f06d37c` (`propobj.c:3401`): move an object's
    /// cylinder (`radius`, ±`radius` about its centre) from `from` to `to`
    /// against the wall tiles. `Err(normal)` when blocked: the hit edge's
    /// horizontal normal, facing back towards `from`.
    pub fn stage_move_obj(&self, from: Vec3, to: Vec3, radius: f32) -> Option<Result<(), Vec3>> {
        let t = self.tiles.as_ref()?;
        let edge_normal = |e: (Vec3, Vec3)| {
            let d = e.1 - e.0;
            let mut n = Vec3::new(d.z, 0.0, -d.x).normalize_or_zero();
            if n == Vec3::ZERO {
                n = (from - to).normalize_or_zero();
            }
            if n.dot(from - e.0) < 0.0 {
                n = -n;
            }
            n
        };
        let (r, edge) = t.cd_test_cylmove_oobfail_findclosest(from, to, radius, radius, -radius, &[]);
        match r {
            CdResult::Collision => return Some(Err(edge.map_or(Vec3::Y, edge_normal))),
            CdResult::Error => return Some(Err((from - to).normalize_or_zero())),
            CdResult::NoCollision => {}
        }
        let (r, edge) = t.cd_test_volume_closestedge(from, to, radius, radius, -radius, &[]);
        if r == CdResult::Collision {
            return Some(Err(edge.map_or(Vec3::Y, edge_normal)));
        }
        Some(Ok(()))
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
    fn room_bbox(&self, pos: Vec3) -> (Vec3, Vec3) {
        self.room_bbox_at(pos)
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
