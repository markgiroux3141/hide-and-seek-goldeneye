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

use engine::geometry::csg_runtime::{Axis, Brush, Op, Side, StairDesc, StairDir};
use engine::geometry::structures::{Anchor, Edge, Platform, StairRun};
use glam::Vec3;

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

/// A named air volume, kept for analysis labeling. Not itself CSG — the CSG is
/// the brush; this is the label + footprint the analyzer samples floor cells in.
#[derive(Clone, Debug)]
pub struct RoomLabel {
    pub name: String,
    /// WT AABB `[x, y, z, w, h, d]`.
    pub aabb: [f32; 6],
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
    pub rooms: Vec<RoomLabel>,
    /// Author-intended room connections (a, b) — the analyzer verifies each is
    /// actually walkable in the baked nav grid.
    pub edges: Vec<(RoomId, RoomId)>,
    pub next_brush_id: u32,
    pub next_platform_id: u32,
    pub next_run_id: u32,
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
    rooms: Vec<RoomLabel>,
    edges: Vec<(RoomId, RoomId)>,
    spawn: Vec3,
    /// The texture scheme applied to subsequent carves/pillars until changed —
    /// mirrors the editor's "pick a texture, then build" flow (`set_scheme`).
    cur_scheme: usize,
    next_brush_id: u32,
    next_platform_id: u32,
    next_run_id: u32,
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
            rooms: Vec::new(),
            edges: Vec::new(),
            spawn: Vec3::new(0.75, 0.1, 0.75),
            cur_scheme: 0,
            next_brush_id: 1,
            next_platform_id: 1,
            next_run_id: 1,
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
            floor_y,
            scheme: self.cur_scheme,
            void_ids: [id1, id2],
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
        });
        // Label the platform top as a "room" so it appears in the graph/floorplan.
        self.rooms.push(RoomLabel {
            name: name.to_string(),
            aabb: [x, top, z, sx, 1.0, sz],
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
            spawn: self.spawn,
            rooms: self.rooms,
            edges: self.edges,
            next_brush_id: self.next_brush_id,
            next_platform_id: self.next_platform_id,
            next_run_id: self.next_run_id,
        }
    }
}
