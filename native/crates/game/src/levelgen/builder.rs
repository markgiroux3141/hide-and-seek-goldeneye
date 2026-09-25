//! The thin "carve air" level builder — an intent-level authoring API over the
//! subtractive CSG so an author (human or program) never hand-manages brush ids,
//! min-corner math, or the shell.
//!
//! Mental model: the world starts solid; you **carve air**. A `room` is an air
//! box. A `passage` is an air box bridging two rooms (an open doorway/corridor).
//! Leftover solid is the walls. Cover (`pillar`) and overlook `platform`s /
//! `stair`s are added on top. The builder also records a **named room graph**
//! (labels + intended connections) purely so the analyzer can label floorplans
//! and verify the intended topology actually bakes into walkable nav.
//!
//! Everything is in **world-tile (WT)** units (1 WT = 0.25 m), min-corner based,
//! matching [`Brush`]. `floor` is the WT y of a room's floor; `height` its
//! interior height (so the ceiling is at `floor + height`).
//!
//! **Two layers.** The coordinate layer (`room`, `passage`, `window`, `csg_stair`, …)
//! takes boxes and wall planes. The **relational** layer (`room_beside`, `door`,
//! `corridor`, `window_between`, `stair_between`, `stair_through_floor`) takes rooms
//! and works the boxes out itself — which wall two rooms share, where the opening
//! centres, how far it must overlap each room, which way a stair runs and how many
//! steps it needs. That arithmetic is exactly what an author (human or LLM) gets wrong,
//! so prefer the relational calls and drop to coordinates only for what they can't say.
//!
//! A relational call that cannot be built (rooms on different floors for a `door`, too
//! little wall for a stair) does not panic and does not guess: it records a **problem**
//! on the [`BuiltLevel`], and the report fails on it.

use engine::geometry::csg_runtime::{Axis, Brush, Op, Side, StairDesc, StairDir, StairShell};
use engine::geometry::structures::{Anchor, Edge, Platform, PlatformStyle, StairRun, StairStyle};
use glam::{Quat, Vec3};

use crate::ecs::{AuthoredId, ComponentData, EntityData, MeshId, PickupKind};

/// How far an opening reaches **into** each room it joins, WT. A passage that only just
/// touches a wall face can fail to connect nav (LEVEL_DESIGN_HEURISTICS: "overlap both
/// rooms by ≥ 2 WT").
const OPENING_OVERLAP: f32 = 2.0;
/// How far an opening stays clear of the room corners it sits between, WT — an opening
/// flush with a perpendicular wall glitches that wall's texture bands.
const OPENING_INSET: f32 = 1.0;
/// Default door / corridor / stairwell height, WT (2.5 m): clear of the 8 WT headroom
/// lint with room to spare.
pub const DOOR_HEIGHT: f32 = 10.0;
/// The shortest shared wall a `width`-wide opening fits in (it keeps
/// `OPENING_INSET` clear of both corners).
pub(crate) fn wall_needed(width: f32) -> f32 {
    width + 2.0 * OPENING_INSET
}

/// A compass direction on the plan. **North is −z**, which is *up* in the report's
/// floorplans (they print z increasing downwards); east is +x.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    North,
    South,
    East,
    West,
}

impl Dir {
    /// The horizontal axis this direction runs along.
    fn axis(self) -> Axis {
        match self {
            Dir::East | Dir::West => Axis::X,
            Dir::North | Dir::South => Axis::Z,
        }
    }
    /// +1 when it runs toward larger coordinates.
    fn sign(self) -> f32 {
        match self {
            Dir::East | Dir::South => 1.0,
            Dir::West | Dir::North => -1.0,
        }
    }
}

/// Min / max of a label box on axis `k` (0 = x, 1 = y, 2 = z).
fn span(a: [f32; 6], k: usize) -> (f32, f32) {
    (a[k], a[k] + a[k + 3])
}

/// How two rooms sit on the plan: separated along `axis` by a wall `gap` WT thick,
/// with `a`'s face toward `b` at `a_face` and `b`'s at `b_face` (`sign` +1 when `b` is
/// on the larger side), overlapping over `[o0, o1)` on the other horizontal axis.
pub(crate) struct Facing {
    axis: Axis,
    sign: f32,
    a_face: f32,
    b_face: f32,
    pub(crate) gap: f32,
    o0: f32,
    o1: f32,
}

pub(crate) fn facing(a: [f32; 6], b: [f32; 6]) -> Result<Facing, String> {
    let mut found = Vec::new();
    for (k, axis) in [(0usize, Axis::X), (2usize, Axis::Z)] {
        let ((a0, a1), (b0, b1)) = (span(a, k), span(b, k));
        if b0 >= a1 {
            found.push((axis, 1.0, a1, b0));
        } else if a0 >= b1 {
            found.push((axis, -1.0, a0, b1));
        }
    }
    match found.as_slice() {
        [] => Err("the rooms already overlap — they share air, there is no wall to open".into()),
        [_, _] => Err("the rooms are diagonal to each other — no shared wall (use `corridor`)".into()),
        [(axis, sign, a_face, b_face)] => {
            let k = if *axis == Axis::X { 2 } else { 0 };
            let ((a0, a1), (b0, b1)) = (span(a, k), span(b, k));
            let (o0, o1) = (a0.max(b0), a1.min(b1));
            if o1 <= o0 {
                return Err("the rooms do not face each other along any length of wall".into());
            }
            Ok(Facing {
                axis: *axis,
                sign: *sign,
                a_face: *a_face,
                b_face: *b_face,
                gap: (b_face - a_face).abs(),
                o0,
                o1,
            })
        }
        _ => unreachable!(),
    }
}

impl Facing {
    /// The `[u0, u1)` span of a `width`-wide opening at fraction `t` (0..1) of the
    /// shared length, kept [`OPENING_INSET`] clear of either end.
    fn opening(&self, t: f32, width: f32) -> Result<(f32, f32), String> {
        let (lo, hi) = (self.o0 + OPENING_INSET, self.o1 - OPENING_INSET);
        if hi - lo < width {
            return Err(format!(
                "a {width} WT opening does not fit the {} WT of wall they share \
                 ({} WT clear of the corners)",
                self.o1 - self.o0,
                hi - lo
            ));
        }
        let c = (self.o0 + (self.o1 - self.o0) * t.clamp(0.0, 1.0))
            .clamp(lo + width * 0.5, hi - width * 0.5);
        Ok((c - width * 0.5, c + width * 0.5))
    }

    /// The `[lo, hi)` span along the separation axis from `into_a` WT inside `a` to
    /// `into_b` WT inside `b`.
    fn through(&self, into_a: f32, into_b: f32) -> (f32, f32) {
        let p = self.a_face - self.sign * into_a;
        let q = self.b_face + self.sign * into_b;
        (p.min(q), p.max(q))
    }
}

// ─── Relational geometry, as free functions over label boxes ──────────────────
//
// The builder's relational calls and the generator (`levelgen::generate`) both need
// these: the builder to carve, the generator to ask *before* carving whether an opening
// would cut through a third room. One definition, so the two can never disagree about
// where a door goes.

/// One box a relational call carves: a span along `axis`, a span along the other
/// horizontal axis, and a vertical span (all WT).
#[derive(Clone, Copy, Debug)]
pub(crate) struct CarveBox {
    pub axis: Axis,
    pub along: (f32, f32),
    pub across: (f32, f32),
    pub y: (f32, f32),
}

impl CarveBox {
    /// Its plan-view footprint as `[x0, x1, z0, z1]`.
    pub fn plan(&self) -> [f32; 4] {
        match self.axis {
            Axis::X => [self.along.0, self.along.1, self.across.0, self.across.1],
            _ => [self.across.0, self.across.1, self.along.0, self.along.1],
        }
    }
}

/// Where a `w` × `d` room placed beside `of` (a label box) across a `wall` on its `dir`
/// side, `along` from `of`'s min corner on the shared side, has its min corner.
pub(crate) fn beside_origin(of: [f32; 6], dir: Dir, wall: f32, along: f32, w: f32, d: f32) -> (f32, f32) {
    let a = of;
    match dir {
        Dir::East => (a[0] + a[3] + wall, a[2] + along),
        Dir::West => (a[0] - wall - w, a[2] + along),
        Dir::South => (a[0] + along, a[2] + a[5] + wall),
        Dir::North => (a[0] + along, a[2] - wall - d),
    }
}

/// The box of a walkable doorway between two label boxes. See [`LevelBuilder::door_at`].
pub(crate) fn door_box(la: [f32; 6], lb: [f32; 6], t: f32, width: f32, height: f32) -> Result<CarveBox, String> {
    let f = facing(la, lb)?;
    let (fa, fb) = (la[1], lb[1]);
    let (ca, cb) = (fa + la[4], fb + lb[4]);
    let floor = if (fa - fb).abs() <= 1.0 {
        fa.max(fb)
    } else {
        let hi = fa.max(fb);
        // The lower room must reach up past the higher floor by the door's height.
        let low_ceiling = if fa < fb { ca } else { cb };
        if low_ceiling < hi + height {
            return Err(format!(
                "floors differ by {} WT ({fa} vs {fb}) — use `stair_between`",
                (fa - fb).abs()
            ));
        }
        hi
    };
    let top = (floor + height).min(ca).min(cb);
    if top - floor < 6.0 {
        return Err(format!(
            "only {} WT of headroom fits under the lower ceiling; a hunter needs 6",
            top - floor
        ));
    }
    Ok(CarveBox {
        axis: f.axis,
        along: f.through(OPENING_OVERLAP, OPENING_OVERLAP),
        across: f.opening(t, width)?,
        y: (floor, top),
    })
}

/// The boxes of a `width`-wide corridor between two label boxes on one floor: the door
/// box if they face each other, else the two legs of an L — out of `a` along x on its
/// centre line past `b`'s centre, then along z down `b`'s centre line into it.
pub(crate) fn corridor_boxes(la: [f32; 6], lb: [f32; 6], width: f32) -> Result<Vec<CarveBox>, String> {
    if facing(la, lb).is_ok() {
        return door_box(la, lb, 0.5, width, DOOR_HEIGHT).map(|b| vec![b]);
    }
    let (fa, fb) = (la[1], lb[1]);
    if (fa - fb).abs() > 1.0 {
        return Err(format!("floors differ ({fa} vs {fb}) — use `stair_between`"));
    }
    let floor = fa.max(fb);
    let top = (floor + DOOR_HEIGHT).min(fa + la[4]).min(fb + lb[4]);
    let (acz, bcx) = (la[2] + la[5] * 0.5, lb[0] + lb[3] * 0.5);
    let hw = width * 0.5;
    let east = lb[0] >= la[0] + la[3];
    let south = lb[2] >= la[2] + la[5];
    let x_from = if east { la[0] + la[3] - OPENING_OVERLAP } else { la[0] + OPENING_OVERLAP };
    let x_to = if east { bcx + hw } else { bcx - hw };
    let z_from = if south { acz - hw } else { acz + hw };
    let z_to = if south { lb[2] + OPENING_OVERLAP } else { lb[2] + lb[5] - OPENING_OVERLAP };
    Ok(vec![
        CarveBox {
            axis: Axis::X,
            along: (x_from.min(x_to), x_from.max(x_to)),
            across: (acz - hw, acz + hw),
            y: (floor, top),
        },
        CarveBox {
            axis: Axis::Z,
            along: (z_from.min(z_to), z_from.max(z_to)),
            across: (bcx - hw, bcx + hw),
            y: (floor, top),
        },
    ])
}

/// The horizontal in-plane axis for a wall whose normal is `axis` (Z for an
/// X-facing wall, X for a Z-facing wall). Mirrors the editor's stair tool.
fn ortho_h(axis: Axis) -> Axis {
    match axis {
        Axis::X => Axis::Z,
        _ => Axis::X,
    }
}

/// Build an axis-aligned subtract brush from wall-relative spans (the port of
/// `world::geom::make_wall_brush`, inlined here so the builder is self-contained):
/// `[lo,hi)` along `axis`, `[y_min,y_max)` vertical, `[u0,u1)` along `u_axis`.
#[allow(clippy::too_many_arguments)]
fn wall_brush(
    id: u32,
    axis: Axis,
    lo: f32,
    hi: f32,
    y_min: f32,
    y_max: f32,
    u_axis: Axis,
    u0: f32,
    u1: f32,
) -> Brush {
    let mut p = [0.0f32; 3];
    let mut s = [0.0f32; 3];
    p[axis.index()] = lo;
    s[axis.index()] = hi - lo;
    p[1] = y_min;
    s[1] = y_max - y_min;
    p[u_axis.index()] = u0;
    s[u_axis.index()] = u1 - u0;
    Brush::new(id, Op::Subtract, p[0], p[1], p[2], s[0], s[1], s[2])
}

/// Handle to a carved room (index into the builder's room-label list).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoomId(pub usize);

/// Handle to an overlook platform (its `Platform::id`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlatId(pub u32);

/// What a label marks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelKind {
    /// A carved room: `aabb` is its air box.
    Room,
    /// A platform deck: `aabb` is its top, one WT thick.
    Platform,
}

/// A named air volume, kept for analysis labeling. Not itself CSG — the CSG is
/// the brush; this is the label + footprint the analyzer samples floor cells in.
#[derive(Clone, Debug)]
pub struct RoomLabel {
    pub name: String,
    /// WT AABB `[x, y, z, w, h, d]`.
    pub aabb: [f32; 6],
    pub kind: LabelKind,
    /// The texture scheme the room was carved with (meaningless for a platform deck,
    /// which always wears the platform style).
    pub scheme: usize,
}

impl RoomLabel {
    pub fn center_floor(&self) -> Vec3 {
        // Feet position at the room center, a hair above the floor (meters).
        let s = engine::geometry::csg_runtime::WORLD_SCALE;
        Vec3::new(
            (self.aabb[0] + self.aabb[3] * 0.5) * s,
            self.aabb[1] * s + 0.1,
            (self.aabb[2] + self.aabb[5] * 0.5) * s,
        )
    }
}

/// The finished, plain-data level ready to drop into a `World` and/or serialize.
pub struct BuiltLevel {
    pub brushes: Vec<Brush>,
    /// CSG stairs (in-wall stairwells cut with the up/down tool). Their tread
    /// solids fold into nav; their void brushes are already in `brushes`.
    pub stairs: Vec<StairDesc>,
    pub platforms: Vec<Platform>,
    pub stair_runs: Vec<StairRun>,
    pub spawn: Vec3, // WT-meters (matches World::spawn_point)
    /// Authored ECS entities (props) placed in the level. See [`crate::ecs`].
    pub entities: Vec<crate::ecs::EntityData>,
    pub rooms: Vec<RoomLabel>,
    /// Author-intended room connections (a, b) — the analyzer verifies each is
    /// actually walkable in the baked nav grid.
    pub edges: Vec<(RoomId, RoomId)>,
    pub next_brush_id: u32,
    pub next_platform_id: u32,
    pub next_run_id: u32,
    /// Relational calls that could not be built, each naming the call and why. The
    /// report fails while any remain; see the module docs.
    pub problems: Vec<String>,
}

/// A deferred solid column (pillar), stored until `finish()` so it's appended
/// **after** every subtract carve — otherwise a later room/passage subtract would
/// eat it (CSG: the last brush covering a cell wins).
struct PendingPillar {
    x: f32,
    z: f32,
    size: f32,
    floor: f32,
    top: f32,
    scheme: usize,
}

/// Accumulates carves + structures, then `finish()`es into a [`BuiltLevel`].
pub struct LevelBuilder {
    brushes: Vec<Brush>,
    stairs: Vec<StairDesc>,
    pillars: Vec<PendingPillar>,
    platforms: Vec<Platform>,
    stair_runs: Vec<StairRun>,
    entities: Vec<crate::ecs::EntityData>,
    rooms: Vec<RoomLabel>,
    edges: Vec<(RoomId, RoomId)>,
    spawn: Vec3,
    /// The texture scheme applied to subsequent carves/pillars until changed —
    /// mirrors the editor's "pick a texture, then build" flow (`set_scheme`).
    cur_scheme: usize,
    next_brush_id: u32,
    next_platform_id: u32,
    next_run_id: u32,
    next_entity_id: u32,
    problems: Vec<String>,
}

impl Default for LevelBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl LevelBuilder {
    pub fn new() -> Self {
        LevelBuilder {
            brushes: Vec::new(),
            stairs: Vec::new(),
            pillars: Vec::new(),
            platforms: Vec::new(),
            stair_runs: Vec::new(),
            entities: Vec::new(),
            rooms: Vec::new(),
            edges: Vec::new(),
            spawn: Vec3::new(0.75, 0.1, 0.75),
            cur_scheme: 0,
            next_brush_id: 1,
            next_platform_id: 1,
            next_run_id: 1,
            next_entity_id: 1,
            problems: Vec::new(),
        }
    }

    /// Set the texture scheme (0..=8) applied to subsequent carves + pillars.
    /// Call before each room/wing to give rooms distinct looks (like the editor's
    /// number-key retexture). Scheme 9 is reserved for platform/stair styling.
    pub fn set_scheme(&mut self, scheme: usize) {
        self.cur_scheme = scheme;
    }

    fn carve(&mut self, x: f32, y: f32, z: f32, w: f32, h: f32, d: f32, floor_y: f32) -> u32 {
        let id = self.next_brush_id;
        self.next_brush_id += 1;
        let mut b = Brush::new(id, Op::Subtract, x, y, z, w, h, d);
        b.floor_y = floor_y;
        b.scheme = self.cur_scheme;
        self.brushes.push(b);
        id
    }

    /// Carve a rectangular room. `(x, z)` is the min corner, `w`×`d` the
    /// footprint, `floor` the WT floor height, `height` the interior height.
    /// Returns a handle used to declare connections + place stairs/spawn.
    pub fn room(
        &mut self,
        name: &str,
        x: f32,
        z: f32,
        w: f32,
        d: f32,
        floor: f32,
        height: f32,
    ) -> RoomId {
        self.carve(x, floor, z, w, height, d, floor);
        self.rooms.push(RoomLabel {
            name: name.to_string(),
            aabb: [x, floor, z, w, height, d],
            kind: LabelKind::Room,
            scheme: self.cur_scheme,
        });
        RoomId(self.rooms.len() - 1)
    }

    /// Carve an open passage (doorway / corridor) as a connecting air box, and
    /// record the intended edge `a`↔`b` for the analyzer to verify. The box
    /// should straddle the wall between the two rooms so their air spaces merge.
    pub fn passage(
        &mut self,
        a: RoomId,
        b: RoomId,
        x: f32,
        z: f32,
        w: f32,
        d: f32,
        floor: f32,
        height: f32,
    ) {
        self.carve(x, floor, z, w, height, d, floor);
        self.edges.push((a, b));
    }

    /// A standalone air box with no recorded edge (an atrium void, light-well, or
    /// a vertical shaft joining floors). Use when the connection isn't a simple
    /// room-to-room doorway.
    pub fn void(&mut self, x: f32, z: f32, w: f32, d: f32, floor: f32, height: f32) {
        self.carve(x, floor, z, w, height, d, floor);
    }

    /// A window / opening cut through a wall at an explicit vertical band — a
    /// **frame** carve at `[x, sill, z]` sized `w × height × d`. Placed *above* the
    /// floor (sill > room floor) it becomes a see/shoot-through window that nav
    /// won't route through; at floor level with `d` shallow it's a floor/ceiling
    /// light-hole between stacked rooms. Cross-room + cross-floor sightlines.
    pub fn window(&mut self, x: f32, sill: f32, z: f32, w: f32, height: f32, d: f32) {
        let id = self.next_brush_id;
        self.next_brush_id += 1;
        let mut b = Brush::new(id, Op::Subtract, x, sill, z, w, height, d);
        b.frame = true;
        b.floor_y = sill;
        b.scheme = self.cur_scheme;
        self.brushes.push(b);
    }

    /// Sink a section of a room's floor into a **pit** — a split-level within one
    /// room. Carves the solid from `room_floor − depth` up to `room_floor` over the
    /// `[x,z]`×`w×d` footprint, so that area's floor drops by `depth` WT. The pit
    /// walls are a `depth`-WT cliff, so pair it with a `stair_ground` from the main
    /// floor down to the pit floor (`room_floor − depth`) so it's walkable both
    /// ways — otherwise you fall in and can't climb out.
    pub fn pit(&mut self, x: f32, z: f32, w: f32, d: f32, room_floor: f32, depth: f32) {
        self.carve(x, room_floor - depth, z, w, depth, d, room_floor - depth);
    }

    /// Solid cover column (Op::Add) spanning `floor`..`top` — a sightline blocker /
    /// camp corner. **Deferred** to `finish()` so it's carved-proof, and it should
    /// run full floor-to-ceiling (use [`pillar_in`](Self::pillar_in) to get that
    /// automatically). Keep pillars clear of stairs/platforms.
    pub fn pillar(&mut self, x: f32, z: f32, size: f32, floor: f32, top: f32) {
        self.pillars.push(PendingPillar {
            x,
            z,
            size,
            floor,
            top,
            scheme: self.cur_scheme,
        });
    }

    /// A full-height cover pillar inside a room (floor→ceiling of that room), so it
    /// always reaches the ceiling. `(x, z)` is the min corner, `size` the square
    /// footprint.
    pub fn pillar_in(&mut self, room: RoomId, x: f32, z: f32, size: f32) {
        let r = &self.rooms[room.0];
        let (floor, top) = (r.aabb[1], r.aabb[1] + r.aabb[4]);
        self.pillar(x, z, size, floor, top);
    }

    /// Cut a CSG staircase into a wall (the up/down-arrow tool). The stairwell is
    /// carved into the solid on `side` of the wall plane `face_pos` (along `axis`,
    /// X or Z), spanning `[u0,u1)` horizontally; it climbs (`Up`) or descends
    /// (`Down`) `steps` WT, opening a 1-WT destination corridor at the new level
    /// beyond the well. Carve a room/hallway at that level next to the destination
    /// to make the stair lead somewhere. `floor`/`ceil` are the source wall's
    /// vertical extent (its room floor + ceiling). Replicates `confirm_stairs`.
    #[allow(clippy::too_many_arguments)]
    pub fn csg_stair(
        &mut self,
        axis: Axis,
        side: Side,
        face_pos: f32,
        u0: f32,
        u1: f32,
        floor: f32,
        ceil: f32,
        dir: StairDir,
        steps: u32,
    ) {
        let u_axis = ortho_h(axis);
        let sc = steps as f32;
        let d = if side == Side::Max { 1.0 } else { -1.0 };
        let floor_y = match dir {
            StairDir::Up => floor + sc,
            StairDir::Down => floor - sc,
        };

        // Brush 1: the stairwell, flush with the wall face.
        let (b1_lo, b1_hi) = if d > 0.0 {
            (face_pos, face_pos + sc)
        } else {
            (face_pos - sc, face_pos)
        };
        let (b1_ymin, b1_ymax) = match dir {
            StairDir::Down => (floor - sc, ceil),
            StairDir::Up => (floor, ceil + sc),
        };
        let id1 = self.next_brush_id;
        self.next_brush_id += 1;
        let mut b1 = wall_brush(id1, axis, b1_lo, b1_hi, b1_ymin, b1_ymax, u_axis, u0, u1);
        b1.floor_y = floor_y;
        b1.scheme = self.cur_scheme;

        // Brush 2: the destination corridor, 1 WT past the stairwell.
        let (b2_lo, b2_hi) = if d > 0.0 {
            (face_pos + sc, face_pos + sc + 1.0)
        } else {
            (face_pos - sc - 1.0, face_pos - sc)
        };
        let (b2_ymin, b2_ymax) = match dir {
            StairDir::Down => (floor - sc, ceil - sc),
            StairDir::Up => (floor + sc, ceil + sc),
        };
        let id2 = self.next_brush_id;
        self.next_brush_id += 1;
        let mut b2 = wall_brush(id2, axis, b2_lo, b2_hi, b2_ymin, b2_ymax, u_axis, u0, u1);
        b2.floor_y = floor_y;
        b2.scheme = self.cur_scheme;

        self.brushes.push(b1);
        self.brushes.push(b2);
        self.stairs.push(StairDesc {
            direction: dir,
            step_count: steps,
            axis,
            side,
            face_pos,
            u_axis,
            u0,
            u1,
            floor,
            ceil,
            // The harness cuts full-height stairwells — `ceil` *is* the wall's top — so
            // there is no wall above the doorway for the carve to take and no lintel to
            // close. `None` says exactly that.
            face_top: None,
            floor_y,
            scheme: self.cur_scheme,
            void_ids: [id1, id2],
            // The headless harness carves the original 45° staircase; the ramp shell and
            // the shallower slopes are hand-authoring choices, made in the O panel.
            shell: StairShell::Steps,
            run_per_step: 1.0,
        });
    }

    /// An overlook slab. `(x, z)` min corner, `sx`×`sz` footprint, `top` the WT y
    /// of its walking surface. `railings` adds cosmetic+collidable rails; a perch
    /// to snipe from usually wants them off on the side facing the drop is not an
    /// option here (rails are all-or-nothing), so pass `false` for open perches.
    pub fn platform(
        &mut self,
        name: &str,
        x: f32,
        z: f32,
        sx: f32,
        sz: f32,
        top: f32,
        railings: bool,
    ) -> PlatId {
        let id = self.next_platform_id;
        self.next_platform_id += 1;
        self.platforms.push(Platform {
            id,
            x,
            y: top,
            z,
            size_x: sx,
            size_z: sz,
            thickness: 1.0,
            grounded: false,
            railings,
            // The headless harness authors the original look; plane platforms are a
            // hand-authoring choice, made in BUILD with Shift+T.
            style: PlatformStyle::Solid,
        });
        // Label the platform top as a "room" so it appears in the graph/floorplan.
        self.rooms.push(RoomLabel {
            name: name.to_string(),
            aabb: [x, top, z, sx, 1.0, sz],
            kind: LabelKind::Platform,
            scheme: self.cur_scheme,
        });
        PlatId(id)
    }

    /// The [`RoomId`] label that mirrors a platform (so it can be an edge endpoint
    /// in the room graph). Call right after [`platform`](Self::platform).
    pub fn last_room(&self) -> RoomId {
        RoomId(self.rooms.len() - 1)
    }

    /// A free-standing straight flight of stairs between two WT ground points.
    /// Steps rise 1 WT each (nav-walkable). Good for floor→floor and the legs of
    /// a spiral (chain several, turning 90° around a core, landing on platforms).
    pub fn stair_ground(
        &mut self,
        from: (f32, f32, f32),
        to: (f32, f32, f32),
        width: f32,
        railings: bool,
    ) {
        let id = self.next_run_id;
        self.next_run_id += 1;
        self.stair_runs.push(StairRun {
            id,
            from_platform: None,
            to_platform: None,
            anchor_from: Anchor::Ground {
                x: from.0,
                y: from.1,
                z: from.2,
            },
            anchor_to: Anchor::Ground {
                x: to.0,
                y: to.1,
                z: to.2,
            },
            width,
            step_height: 1.0,
            rise_over_run: 1.0,
            grounded: true,
            railings,
            style: StairStyle::Platform,
        });
    }

    /// A flight from a WT ground point up to a platform edge (offset 0..1 along
    /// that edge). Lets a stair land cleanly on an overlook.
    pub fn stair_to_platform(
        &mut self,
        from: (f32, f32, f32),
        plat: PlatId,
        edge: Edge,
        offset: f32,
        width: f32,
        railings: bool,
    ) {
        let id = self.next_run_id;
        self.next_run_id += 1;
        self.stair_runs.push(StairRun {
            id,
            from_platform: None,
            to_platform: Some(plat.0),
            anchor_from: Anchor::Ground {
                x: from.0,
                y: from.1,
                z: from.2,
            },
            anchor_to: Anchor::Edge { edge, offset },
            width,
            step_height: 1.0,
            rise_over_run: 1.0,
            grounded: true,
            railings,
            style: StairStyle::Platform,
        });
    }

    /// A flight connecting two platform edges (for spiral landings).
    pub fn stair_platform_to_platform(
        &mut self,
        from: PlatId,
        from_edge: Edge,
        from_offset: f32,
        to: PlatId,
        to_edge: Edge,
        to_offset: f32,
        width: f32,
        railings: bool,
    ) {
        let id = self.next_run_id;
        self.next_run_id += 1;
        self.stair_runs.push(StairRun {
            id,
            from_platform: Some(from.0),
            to_platform: Some(to.0),
            anchor_from: Anchor::Edge {
                edge: from_edge,
                offset: from_offset,
            },
            anchor_to: Anchor::Edge {
                edge: to_edge,
                offset: to_offset,
            },
            width,
            step_height: 1.0,
            rise_over_run: 1.0,
            grounded: false,
            railings,
            style: StairStyle::Platform,
        });
    }

    /// Record a logical connection between two labels without carving geometry —
    /// for links a `passage` box doesn't express (a stair up to a perch, a
    /// vertical shaft). Keeps the connectivity graph honest so a stair-reached
    /// perch isn't flagged a dead-end.
    pub fn link(&mut self, a: RoomId, b: RoomId) {
        self.edges.push((a, b));
    }

    /// Set the player/enemy ingress point (WT coords; converted to meters).
    pub fn spawn_wt(&mut self, x: f32, y: f32, z: f32) {
        let s = engine::geometry::csg_runtime::WORLD_SCALE;
        self.spawn = Vec3::new(x * s, y * s, z * s);
    }

    /// Author a placed ECS entity (prop). Scaffold seam — mirrors how `window` /
    /// `pillar` push a plain record. The entity's [`crate::ecs::AuthoredId`] must be
    /// unique within the level; the caller assigns it (typically monotonically). A
    /// typed helper per prop kind (e.g. `door(...)`) lands with the door task.
    pub fn entity(&mut self, data: EntityData) {
        self.next_entity_id = self.next_entity_id.max(data.id.0 + 1);
        self.entities.push(data);
    }

    pub fn finish(mut self) -> BuiltLevel {
        // Append pillars LAST (after every subtract carve) so nothing eats them.
        for p in &self.pillars {
            let id = self.next_brush_id;
            self.next_brush_id += 1;
            let mut brush = Brush::new(id, Op::Add, p.x, p.floor, p.z, p.size, p.top - p.floor, p.size);
            brush.scheme = p.scheme;
            self.brushes.push(brush);
        }
        BuiltLevel {
            brushes: self.brushes,
            stairs: self.stairs,
            platforms: self.platforms,
            stair_runs: self.stair_runs,
            entities: self.entities,
            spawn: self.spawn,
            rooms: self.rooms,
            edges: self.edges,
            next_brush_id: self.next_brush_id,
            next_platform_id: self.next_platform_id,
            next_run_id: self.next_run_id,
            problems: self.problems,
        }
    }
}

// ─── The relational layer ─────────────────────────────────────────────────────

impl LevelBuilder {
    fn label(&self, r: RoomId) -> [f32; 6] {
        self.rooms[r.0].aabb
    }

    fn name(&self, r: RoomId) -> &str {
        &self.rooms[r.0].name
    }

    fn problem(&mut self, call: String, why: String) {
        self.problems.push(format!("{call}: {why}"));
    }

    /// Carve an axis-aligned box given as a span along `axis`, a span along the other
    /// horizontal axis, and a vertical span.
    fn carve_spans(&mut self, axis: Axis, along: (f32, f32), across: (f32, f32), y: (f32, f32)) -> u32 {
        let (x, w, z, d) = match axis {
            Axis::X => (along.0, along.1 - along.0, across.0, across.1 - across.0),
            _ => (across.0, across.1 - across.0, along.0, along.1 - along.0),
        };
        self.carve(x, y.0, z, w, y.1 - y.0, d, y.0)
    }

    /// Carve a room **beside** `of`, across a `wall` WT thick on its `dir` side, centred
    /// on that side. The wall is what keeps the two separate — connect them with
    /// [`door`](Self::door) or a stair.
    #[allow(clippy::too_many_arguments)]
    pub fn room_beside(
        &mut self,
        name: &str,
        of: RoomId,
        dir: Dir,
        wall: f32,
        w: f32,
        d: f32,
        floor: f32,
        height: f32,
    ) -> RoomId {
        let a = self.label(of);
        let along = match dir.axis() {
            Axis::X => a[2] + (a[5] - d) * 0.5,
            _ => a[0] + (a[3] - w) * 0.5,
        } - match dir.axis() {
            Axis::X => a[2],
            _ => a[0],
        };
        self.room_beside_at(name, of, dir, wall, along, w, d, floor, height)
    }

    /// [`room_beside`](Self::room_beside) with an explicit offset: `along` is where the
    /// new room's min corner sits along the shared side, measured from `of`'s min corner
    /// (0 = flush with its west / north end).
    #[allow(clippy::too_many_arguments)]
    pub fn room_beside_at(
        &mut self,
        name: &str,
        of: RoomId,
        dir: Dir,
        wall: f32,
        along: f32,
        w: f32,
        d: f32,
        floor: f32,
        height: f32,
    ) -> RoomId {
        let (x, z) = beside_origin(self.label(of), dir, wall, along, w, d);
        if wall < 1.0 {
            let n = self.name(of).to_string();
            self.problem(
                format!("room_beside({name}, {n})"),
                format!("a {wall} WT wall does not keep them apart — use at least 1"),
            );
        }
        self.room(name, x, z, w, d, floor, height)
    }

    /// Open a doorway through the wall `a` and `b` share, centred on it. See
    /// [`door_at`](Self::door_at).
    pub fn door(&mut self, a: RoomId, b: RoomId, width: f32) {
        self.door_at(a, b, 0.5, width, DOOR_HEIGHT);
    }

    /// Open a `width` × `height` doorway through the wall `a` and `b` share, at fraction
    /// `t` (0..1) along it, reaching [`OPENING_OVERLAP`] into each room, and record the
    /// connection. Works for any wall thickness, so it is also a straight corridor.
    ///
    /// Floors within 1 WT of each other meet at the higher one. A room whose ceiling
    /// clears the *other* room's floor by the door's height can be entered at that
    /// floor — a door off a mezzanine into an upper room. Anything else is a stair.
    pub fn door_at(&mut self, a: RoomId, b: RoomId, t: f32, width: f32, height: f32) {
        let call = format!("door({}, {})", self.name(a), self.name(b));
        match door_box(self.label(a), self.label(b), t, width, height) {
            Ok(bx) => {
                self.carve_box(bx);
                self.edges.push((a, b));
            }
            Err(why) => self.problem(call, why),
        }
    }

    fn carve_box(&mut self, b: CarveBox) -> u32 {
        self.carve_spans(b.axis, b.along, b.across, b.y)
    }

    /// Connect two rooms on the same floor with a `width`-wide corridor: straight if
    /// they face each other, an L (out along x from `a`, then along z into `b`) if they
    /// are diagonal. Records the connection. The corridor is not checked against rooms it
    /// might cross — the report lists any connection it made that was never declared.
    pub fn corridor(&mut self, a: RoomId, b: RoomId, width: f32) {
        let call = format!("corridor({}, {})", self.name(a), self.name(b));
        match corridor_boxes(self.label(a), self.label(b), width) {
            Ok(boxes) => {
                for bx in boxes {
                    self.carve_box(bx);
                }
                self.edges.push((a, b));
            }
            Err(why) => self.problem(call, why),
        }
    }

    /// Cut a see- and shoot-through window in the wall `a` and `b` share, at fraction `t`
    /// (0..1) along it: `sill` WT above the higher floor, `width` × `height`. Not a
    /// walkable connection, so nothing is recorded. Keep `t` off a door's (`0.5` is where
    /// [`door`](Self::door) puts one).
    #[allow(clippy::too_many_arguments)]
    pub fn window_between(&mut self, a: RoomId, b: RoomId, t: f32, sill: f32, width: f32, height: f32) {
        let call = format!("window_between({}, {})", self.name(a), self.name(b));
        let (la, lb) = (self.label(a), self.label(b));
        let f = match facing(la, lb) {
            Ok(f) => f,
            Err(why) => return self.problem(call, why),
        };
        let bottom = la[1].max(lb[1]) + sill;
        let ceiling = (la[1] + la[4]).min(lb[1] + lb[4]);
        if bottom + height > ceiling {
            return self.problem(
                call,
                format!("a {height} WT window at sill {bottom} runs past the lower ceiling ({ceiling})"),
            );
        }
        let across = match f.opening(t, width) {
            Ok(u) => u,
            Err(why) => return self.problem(call, why),
        };
        let along = f.through(1.0, 1.0);
        let id = self.carve_spans(f.axis, along, across, (bottom, bottom + height));
        if let Some(b) = self.brushes.iter_mut().find(|b| b.id == id) {
            b.frame = true;
        }
    }

    /// Join two rooms on **different floors** with a stairwell cut through the wall they
    /// share — up or down, as many 1 WT steps as the floors differ. The wall must be at
    /// least `steps + 1` WT thick (the stairwell, then a 1 WT landing); a thicker one gets
    /// a corridor at the far level to close the rest of the gap. `width` wide, headroom
    /// [`DOOR_HEIGHT`]. Records the connection.
    pub fn stair_between(&mut self, a: RoomId, b: RoomId, width: f32) {
        let call = format!("stair_between({}, {})", self.name(a), self.name(b));
        let (la, lb) = (self.label(a), self.label(b));
        let f = match facing(la, lb) {
            Ok(f) => f,
            Err(why) => return self.problem(call, why),
        };
        let (fa, fb) = (la[1], lb[1]);
        let rise = fb - fa;
        if rise.abs() < 1.0 || rise.fract() != 0.0 {
            return self.problem(
                call,
                format!("the floors must differ by a whole number of WT ({fa} vs {fb}) — use `door`"),
            );
        }
        let steps = rise.abs() as u32;
        let need = steps as f32 + 1.0;
        if f.gap < need {
            return self.problem(
                call,
                format!(
                    "a {steps}-step stair needs {need} WT of wall between them; they have {} — \
                     move them {} WT further apart",
                    f.gap,
                    need - f.gap
                ),
            );
        }
        for (label, floor) in [(la, fa), (lb, fb)] {
            if floor + DOOR_HEIGHT > label[1] + label[4] {
                return self.problem(
                    call,
                    format!("a room is under {DOOR_HEIGHT} WT tall; the stairwell needs that headroom"),
                );
            }
        }
        let (u0, u1) = match f.opening(0.5, width) {
            Ok(u) => u,
            Err(why) => return self.problem(call, why),
        };
        let side = if f.sign > 0.0 { Side::Max } else { Side::Min };
        let dir = if rise > 0.0 { StairDir::Up } else { StairDir::Down };
        self.csg_stair(f.axis, side, f.a_face, u0, u1, fa, fa + DOOR_HEIGHT, dir, steps);
        // Close whatever wall is left past the landing, at the far room's level.
        if f.gap > need {
            let start = f.a_face + f.sign * need;
            let end = f.b_face + f.sign * OPENING_OVERLAP;
            self.carve_spans(f.axis, (start.min(end), start.max(end)), (u0, u1), (fb, fb + DOOR_HEIGHT));
        }
        self.edges.push((a, b));
    }

    /// A free-standing staircase from `upper` down into `lower` through a hole in the
    /// floor — the stairwell for rooms stacked one over the other. The flight starts at
    /// `(x, z)` on the upper floor and descends toward `dir` at 1:1, `width` wide; the
    /// hole is cut over its whole footprint, so every tread has the upper room's headroom.
    /// The footprint must lie inside both rooms. Records the connection.
    #[allow(clippy::too_many_arguments)]
    pub fn stair_through_floor(&mut self, upper: RoomId, lower: RoomId, x: f32, z: f32, dir: Dir, width: f32) {
        let call = format!("stair_through_floor({}, {})", self.name(upper), self.name(lower));
        let (lu, ll) = (self.label(upper), self.label(lower));
        let (fu, fl) = (lu[1], ll[1]);
        let ceiling_below = fl + ll[4];
        if ceiling_below > fu - 1.0 {
            let (l, u) = (self.name(lower).to_string(), self.name(upper).to_string());
            return self.problem(
                call,
                format!(
                    "{l}'s ceiling ({ceiling_below}) must sit at least 1 WT below {u}'s floor ({fu}) — with no slab between them they are one space"
                ),
            );
        }
        let rise = fu - fl;
        // A stair-run's lowest tread sits one step above its ground anchor, so anchor one
        // below the lower floor and run one further to land flush on it.
        let run = rise + 1.0;
        let (dx, dz) = match dir.axis() {
            Axis::X => (dir.sign() * run, 0.0),
            _ => (0.0, dir.sign() * run),
        };
        let hw = width * 0.5;
        let (along, across) = match dir.axis() {
            Axis::X => ((x.min(x + dx), x.max(x + dx)), (z - hw, z + hw)),
            _ => ((z.min(z + dz), z.max(z + dz)), (x - hw, x + hw)),
        };
        let inside = |l: [f32; 6]| {
            let (ka, kc) = match dir.axis() {
                Axis::X => (0, 2),
                _ => (2, 0),
            };
            along.0 >= l[ka] && along.1 <= l[ka] + l[ka + 3] && across.0 >= l[kc] && across.1 <= l[kc] + l[kc + 3]
        };
        if !inside(ll) || !inside(lu) {
            return self.problem(call, "the flight's footprint must lie inside both rooms".to_string());
        }
        let id = self.carve_spans(dir.axis(), along, across, (ceiling_below, fu));
        if let Some(b) = self.brushes.iter_mut().find(|b| b.id == id) {
            b.frame = true;
        }
        self.stair_ground((x, fu, z), (x + dx, fl - 1.0, z + dz), width, false);
        self.edges.push((upper, lower));
    }

    // ─── Entities ───────────────────────────────────────────────────────────

    fn push_entity(&mut self, components: Vec<ComponentData>) {
        let id = AuthoredId(self.next_entity_id);
        self.next_entity_id += 1;
        self.entities.push(EntityData { id, components });
    }

    fn transform(x: f32, y: f32, z: f32, yaw_deg: f32) -> ComponentData {
        let s = engine::geometry::csg_runtime::WORLD_SCALE;
        ComponentData::Transform {
            pos: [x * s, y * s, z * s],
            rot: Quat::from_rotation_y(yaw_deg.to_radians()).to_array(),
            scale: [1.0, 1.0, 1.0],
        }
    }

    /// A spawn pad at WT `(x, y, z)` (feet on the floor), facing `yaw_deg`. The hunt
    /// draws both the player's and the hunters' spawns from these (Perfect Dark's rule);
    /// a level with none falls back to the spawn marker.
    pub fn spawn_pad(&mut self, x: f32, y: f32, z: f32, yaw_deg: f32) {
        self.push_entity(vec![Self::transform(x, y, z, yaw_deg), ComponentData::SpawnPoint]);
    }

    /// A weapon lying on the floor at WT `(x, y, z)`, with the editor's defaults. The
    /// player and the hunters start unarmed, so a level needs these to be a match.
    pub fn weapon(&mut self, name: &str, x: f32, y: f32, z: f32) {
        self.pickup(PickupKind::Weapon, MeshId::WeaponPickup, name, x, y, z);
    }

    /// An ammo crate for weapon `name` at WT `(x, y, z)`. The tan *pickup* crate —
    /// walk-through, like the editor's — not the `AmmoCrate` scenery prop, which is solid
    /// to nav (the first version used that one and every crate became a 1 WT block).
    pub fn ammo(&mut self, name: &str, x: f32, y: f32, z: f32) {
        self.pickup(PickupKind::Ammo, MeshId::AmmoPickupTan, name, x, y, z);
    }

    fn pickup(&mut self, kind: PickupKind, mesh: MeshId, name: &str, x: f32, y: f32, z: f32) {
        let Some(weapon) = crate::combat::arsenal::resolve_name(name) else {
            return self.problem(format!("{kind:?} pickup \"{name}\""), "no weapon by that name".into());
        };
        let p = match kind {
            PickupKind::Weapon => crate::ecs::Pickup::weapon(weapon),
            PickupKind::Ammo => crate::ecs::Pickup::ammo(weapon),
        };
        self.push_entity(vec![
            Self::transform(x, y, z, 0.0),
            ComponentData::Renderable { mesh },
            ComponentData::Pickup {
                kind,
                weapon: weapon.to_string(),
                mags: p.mags,
                respawn: p.respawn,
            },
        ]);
    }
}
