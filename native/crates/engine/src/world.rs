//! The authored scene — a hand-rolled `World` (no ECS yet; entity counts don't
//! justify one until the Phase 3 enemy roster). Owns the CSG regions, the
//! collision world, and the fly camera, and drives the BUILD-phase authoring
//! loop: crosshair face-pick → push/pull → re-evaluate the region → hand the
//! app a fresh mesh while updating the region's collider in place.
//!
//! Mirrors the reference editor (`src/tools/indoorKeys.js` + `csgActions.js`):
//! `+`/`=` push (carve inward), `-` pull (extend outward), default step 4 WT.

use std::time::Instant;

use glam::{Mat4, Vec3};

use crate::camera::FlyCamera;
use crate::character::CharacterController;
use crate::csg_runtime::{
    Axis, Brush, Op, Region, Side, StairDesc, StairDir, WALL_THICKNESS, WORLD_SCALE,
};
use crate::enemy::Enemy;
use crate::input::InputState;
use crate::mesh::{ColorVertex, ColoredMesh, CpuMesh};
use crate::nav::{self, NavWorld};
use crate::physics::PhysicsWorld;
use crate::structures::{self, Anchor, Edge, Platform, StairRun};

/// Default push/pull increment, in WT (JS `PUSH_PULL_STEP`). Shift → 1 WT.
pub const PUSH_PULL_STEP: f32 = 4.0;

/// Door opening size in WT (JS `DOOR_WIDTH` / `DOOR_HEIGHT`): 3 × 7 = 0.75 × 1.75 m.
const DOOR_WIDTH: f32 = 3.0;
const DOOR_HEIGHT: f32 = 7.0;

/// Default hole size in WT (JS `HOLE_WIDTH` / `HOLE_HEIGHT`), scroll-adjustable.
const HOLE_WIDTH: f32 = 3.0;
const HOLE_HEIGHT: f32 = 3.0;

/// Pillar/brace sizing bounds in WT (JS `MIN/MAX_PILLAR_SIZE`, `MIN/MAX_BRACE_DIM`).
const PILLAR_SIZE: f32 = 2.0;
const PILLAR_MIN: f32 = 1.0;
const PILLAR_MAX: f32 = 8.0;
const BRACE_DIM: f32 = 2.0;
const BRACE_MIN: f32 = 1.0;
const BRACE_MAX: f32 = 8.0;

/// Burial epsilon in WT: additive decorations (pillars/braces) sink ½ WT into the
/// surrounding solid on their hidden faces, so the CSG doesn't emit stray coplanar
/// triangles at the seam (JS `E = WALL_THICKNESS / 2`).
const BURY_EPS: f32 = WALL_THICKNESS / 2.0;

/// Seconds of sustained breaching to break a door (JS `door.js` `DOOR_HP`).
const DOOR_HP: f32 = 2.5;

/// Reserved renderer/physics id for the combined free-standing structures mesh
/// (all platforms + stair-runs). CSG region ids count up from 0, so `u32::MAX`
/// never collides — the structures live in the same mesh + trimesh-collider
/// slots as regions, reusing the checkerboard shader and the walk-on-it physics
/// path for free (they're free-standing, so they can't fold into a region mesh).
const STRUCT_ID: u32 = u32::MAX;

/// Platform/stair-run defaults in WT (JS `DEFAULT_PLATFORM_*` / `DEFAULT_STAIR_*`).
const PLATFORM_SIZE: f32 = 4.0;
const PLATFORM_THICKNESS: f32 = 1.0;
const PLATFORM_SIZE_MIN: f32 = 1.0;
const PLATFORM_SIZE_MAX: f32 = 20.0;
const STAIR_WIDTH: f32 = 4.0;
const STAIR_STEP_HEIGHT: f32 = 1.0;
const STAIR_RISE_OVER_RUN: f32 = 1.0;

/// Platform gizmo dimensions in WT (JS `GIZMO_*`). Arrows are drawn as thin
/// elongated boxes (no cone tip); scale handles are cubes at the edge midpoints.
const GIZMO_ARROW_LENGTH: f32 = 3.0;
const GIZMO_SHAFT_HALF: f32 = 0.12; // GIZMO_SHAFT_RADIUS
const GIZMO_HANDLE_SIZE: f32 = 0.4;
/// Screen-drag → WT sensitivity, scaled by camera distance (JS `GIZMO_DRAG_SENSITIVITY`).
const GIZMO_DRAG_SENSITIVITY: f32 = 0.008;

/// The two game phases (DESIGN.md): author geometry, then walk it as the player.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Fly-cam authoring (CSG editing enabled).
    Build,
    /// Grounded first-person capsule (geometry frozen).
    Hunt,
}

/// A region's freshly-evaluated mesh, returned to the app for GPU upload.
pub struct RegionMesh {
    pub id: u32,
    pub mesh: CpuMesh,
}

/// The currently-selected brush face (what push/pull acts on, and what the
/// highlight overlay draws). Mirrors JS `state.csg.selectedFace`.
#[derive(Clone, Copy)]
struct Selection {
    region_id: u32,
    brush_id: u32,
    axis: Axis,
    side: Side,
}

/// A breakable door, live only during the HUNT (JS `door.js`). The panel is a
/// standalone cuboid collider that blocks the player; the nav overlay adds a
/// cost the hunter reads live. Breaching drains `hp`, then removes the collider
/// and flips the nav flag — **no re-voxelization, no CSG re-eval** (the thesis).
/// `aabb` is the doorframe carve in WT (min corner + dims), used to draw the panel.
struct Door {
    aabb: Brush,
    hp: f32,
    broken: bool,
    /// The panel collider's index in [`PhysicsWorld`], removed on breach.
    panel: usize,
}

/// Which opening the crosshair tool cuts. A `Door` is a fixed 3×7 wall opening
/// that becomes breakable at HUNT (frame marked `door`); a `Hole` is an
/// arbitrary-size opening in any face (walls, floor, or ceiling), not breakable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OpeningKind {
    Door,
    Hole,
}

/// Which additive-brush placement tool is armed. A `Pillar` is a floor→ceiling
/// square column; a `Brace` is a 3-brush arch (up one wall, across the ceiling,
/// down the opposite wall). Both are plain `Op::Add` brushes (JS marks them
/// `isBrace` for texturing, which we don't have yet).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PlaceKind {
    Pillar,
    Brace,
}

/// The free-standing platform/stair-run tool's phase (JS `state.platformPhase`).
/// `None` on `World` = the tool is off entirely; `Some(_)` = armed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PlatformPhase {
    /// Tool on, nothing selected — a click places a new platform or selects one.
    Idle,
    /// A platform or stair-run is selected (C connects, F grounds, V rails, X deletes).
    Selected,
    /// Connect step 1: aim + click locks the destination (platform/floor) + the
    /// source edge. A marker tracks the crosshair; nothing is built yet.
    ConnectDst,
    /// Connect step 2: destination + source edge are frozen; the crosshair slides
    /// the attach point along the source edge (JS `connecting_src`). A stable stair
    /// ghost follows; click confirms.
    ConnectSrc,
    /// Simple-stair: waiting for the first free endpoint click.
    SimpleFrom,
    /// Simple-stair: waiting for the second free endpoint click.
    SimpleTo,
}

/// The locked connect destination (JS `platformConnectTo`): a platform edge, or a
/// free-standing ground point.
#[derive(Clone, Copy)]
enum ConnectTarget {
    Platform { id: u32, edge: Edge },
    Ground { x: f32, y: f32, z: f32 },
}

/// A resolved crosshair hit for the platform tool: the WT hit point, the dominant
/// surface axis, and which platform/stair-run (if any) that point lies inside.
#[derive(Clone, Copy)]
struct StructureHit {
    hit_wt: Vec3,
    axis: Axis,
    platform: Option<u32>,
    run: Option<u32>,
}

/// One draggable part of the platform gizmo (JS `gizmo.js`): three move arrows
/// (translate the whole platform along an axis) and four edge scale handles
/// (grow/shrink the footprint from that edge).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GizmoHandle {
    MoveX,
    MoveY,
    MoveZ,
    ScaleXMin,
    ScaleXMax,
    ScaleZMin,
    ScaleZMax,
}

/// An in-progress gizmo drag (JS `gizmo.drag`): the handle being dragged, the
/// platform's original transform (for cancel), and the sub-WT accumulator that
/// quantizes screen motion into whole-WT steps.
#[derive(Clone, Copy)]
struct GizmoDrag {
    handle: GizmoHandle,
    platform_id: u32,
    orig: Platform,
    accumulated: f32,
}

/// A resolved opening placement (from the crosshair) — enough to draw the ghost
/// preview and to cut it. `position` is the face-plane WT coord on `axis`;
/// `(u0, v0)` is the opening's min corner on the two in-plane axes; `(w, h)` its
/// size along `(u_axis, v_axis)`. Generalizes the old door placement (JS
/// `computeHolePreview`, which drives both the hole and door tools).
#[derive(Clone, Copy)]
struct OpeningPlacement {
    region_id: u32,
    axis: Axis,
    side: Side,
    position: f32,
    u_axis: Axis,
    v_axis: Axis,
    u0: f32,
    v0: f32,
    w: f32,
    h: f32,
    kind: OpeningKind,
}

/// A pending (unconfirmed) stair op (JS `state.csg.pendingStairOp`): the arrow
/// keys grow/shrink `step_count` on the anchored wall face; Enter confirms it
/// into void brushes + a [`StairDesc`], Esc cancels. No geometry changes until
/// confirm — the counter just accumulates. `anchor_*` pin it to one face so the
/// opposite arrow shrinks the *same* op instead of starting a new one.
#[derive(Clone, Copy)]
struct PendingStair {
    direction: StairDir,
    step_count: u32,
    region_id: u32,
    axis: Axis,
    side: Side,
    face_pos: f32,
    u_axis: Axis,
    u0: f32,
    u1: f32,
    /// Face bottom (vMin) and stairwell ceiling H, in WT Y.
    floor: f32,
    ceil: f32,
}

/// A sub-face carve/extrude in progress (JS `activeBrush`/`activeOp`): a spawned
/// brush grown by repeated push/pull, so holding `+` carves deeper instead of
/// stacking new brushes on every press.
#[derive(Clone, Copy)]
struct ActiveOp {
    brush_id: u32,
    op: SubOp,
    side: Side,
}

#[derive(Clone, Copy, PartialEq)]
enum SubOp {
    Push,
    Pull,
}

/// The selected face's in-plane U/V extent in WT (JS `getFaceUVInfo`), plus the
/// face-plane coord on the normal axis.
struct FaceInfo {
    u_axis: Axis,
    v_axis: Axis,
    u_min: f32,
    u_max: f32,
    v_min: f32,
    v_max: f32,
    u_size: f32,
    v_size: f32,
    position: f32,
}

pub struct World {
    pub camera: FlyCamera,
    pub physics: PhysicsWorld,
    pub mode: Mode,
    /// The player capsule; `Some` only in HUNT mode.
    character: Option<CharacterController>,
    /// Baked nav grid + hunter; `Some` only in HUNT mode.
    nav: Option<NavWorld>,
    enemy: Option<Enemy>,
    caught: bool,
    regions: Vec<Region>,
    selected: Option<Selection>,
    /// Breakable doors; populated at G→HUNT from door-marked brushes, cleared on
    /// return to BUILD. `Some`-active only during the hunt.
    doors: Vec<Door>,
    /// Opening tool state (BUILD): which crosshair opening tool is armed (door or
    /// hole), if any. Armed by `B` (door) / `H` (hole); a ghost preview tracks the
    /// crosshair, a left-click cuts, pressing the same key again disarms.
    opening_tool: Option<OpeningKind>,
    /// The placement the ghost currently previews (recomputed each frame while
    /// arming); what a confirm cuts.
    opening_preview: Option<OpeningPlacement>,
    /// The current hole size in WT (scroll-adjustable while the hole tool is
    /// armed): width along the face U axis, height along V. Doors are fixed size.
    hole_w: f32,
    hole_h: f32,
    /// Additive-brush placement tool (pillar / brace), if armed. Mutually
    /// exclusive with the opening tools.
    place_tool: Option<PlaceKind>,
    /// Pillar cross-section (square) in WT; scroll-adjustable while armed.
    pillar_size: f32,
    /// Brace dimensions in WT: `brace_width` along the wall, `brace_depth` the
    /// inward protrusion + ceiling-strip thickness. Scroll = width, Shift = depth.
    brace_width: f32,
    brace_depth: f32,
    /// Sub-face selection size on the current face in WT; 0 = full face. Grown by
    /// the scroll wheel (JS `state.csg.selSizeU/V`): scroll = U, Shift+scroll = V.
    sel_size_u: f32,
    sel_size_v: f32,
    /// The current sub-rect `[u0, u1, v0, v1]` (WT), tracked to the crosshair by
    /// the per-frame preview and consumed by a sub-face push/pull.
    sel_bounds: Option<[f32; 4]>,
    /// A sub-face carve in progress, grown by repeated push/pull.
    active: Option<ActiveOp>,
    /// A pending stair op (arrow keys), not yet confirmed into geometry.
    pending_stair: Option<PendingStair>,
    /// Allocator for brushes spawned by tools (the door-cut is the first such
    /// tool; extrude / pillar reuse it later). Room brush is id 1.
    next_brush_id: u32,

    // ─── Free-standing platform + stair-run system (JS `Platform`/`StairRun`) ──
    /// The platform tool's phase, or `None` when the tool is off. Mutually
    /// exclusive with the opening/placement tools.
    platform_phase: Option<PlatformPhase>,
    /// Every free-standing platform and stair-run. Combined into the single
    /// `STRUCT_ID` structures mesh + collider; their solid boxes feed nav.
    platforms: Vec<Platform>,
    stair_runs: Vec<StairRun>,
    /// The currently-selected platform / stair-run (at most one is `Some`).
    selected_platform: Option<u32>,
    selected_run: Option<u32>,
    /// Connect source platform id (set from `C` through the connect steps).
    connect_from: Option<u32>,
    /// Locked destination + source edge during [`PlatformPhase::ConnectSrc`].
    connect_to: Option<ConnectTarget>,
    connect_edge: Option<Edge>,
    /// Attach position along the source edge in WT (scroll-adjusted during
    /// `ConnectSrc`); `offset = connect_slide_wt / edge_len`.
    connect_slide_wt: f32,
    /// First endpoint (WT) of a simple-stair while in [`PlatformPhase::SimpleTo`].
    simple_from: Option<Vec3>,
    /// Footprint of the next placed platform in WT (scroll = X, Shift+scroll = Z).
    platform_size_x: f32,
    platform_size_z: f32,
    /// Id allocators for platforms / stair-runs (JS `nextPlatformId`/`nextStairRunId`).
    next_platform_id: u32,
    next_run_id: u32,
    /// An active gizmo drag on the selected platform, if any (JS `gizmo.drag`).
    gizmo_drag: Option<GizmoDrag>,
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    /// One room to start with: a single subtractive brush inside an auto-shell —
    /// the editor's opening move. Camera spawns inside, facing the −Z wall.
    pub fn new() -> Self {
        let mut region = Region::new(0);
        // Room cavity in WT: 24 × 16 × 24 → 6 × 4 × 6 m.
        region
            .brushes
            .push(Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 24.0, 16.0, 24.0));

        // Spawn at the room's horizontal center, ~1.5 m up, looking toward −Z.
        let camera = FlyCamera::new(Vec3::new(3.0, 1.5, 3.0), 0.0, 0.0);

        World {
            camera,
            physics: PhysicsWorld::new(),
            mode: Mode::Build,
            character: None,
            nav: None,
            enemy: None,
            caught: false,
            regions: vec![region],
            selected: None,
            doors: Vec::new(),
            opening_tool: None,
            opening_preview: None,
            hole_w: HOLE_WIDTH,
            hole_h: HOLE_HEIGHT,
            place_tool: None,
            pillar_size: PILLAR_SIZE,
            brace_width: BRACE_DIM,
            brace_depth: BRACE_DIM,
            sel_size_u: 0.0,
            sel_size_v: 0.0,
            sel_bounds: None,
            active: None,
            pending_stair: None,
            next_brush_id: 2,
            platform_phase: None,
            platforms: Vec::new(),
            stair_runs: Vec::new(),
            selected_platform: None,
            selected_run: None,
            connect_from: None,
            connect_to: None,
            connect_edge: None,
            connect_slide_wt: 0.0,
            simple_from: None,
            platform_size_x: PLATFORM_SIZE,
            platform_size_z: PLATFORM_SIZE,
            next_platform_id: 1,
            next_run_id: 1,
            gizmo_drag: None,
        }
    }

    /// Evaluate every region once, set colliders, and return the meshes so the
    /// app can upload them. Call at startup.
    pub fn initial_meshes(&mut self) -> Vec<RegionMesh> {
        let ids: Vec<u32> = self.regions.iter().map(|r| r.id).collect();
        ids.into_iter()
            .filter_map(|id| self.rebuild_region(id))
            .collect()
    }

    /// Apply mouse-look — once per rendered frame, so aim is decoupled from the
    /// fixed sim rate.
    pub fn look(&mut self, input: &mut InputState) {
        match self.mode {
            Mode::Build => self.camera.apply_look(input),
            Mode::Hunt => {
                if let Some(c) = self.character.as_mut() {
                    c.apply_look(input);
                }
            }
        }
    }

    /// Advance movement/physics by one fixed timestep.
    pub fn fixed_step(&mut self, dt: f32, input: &InputState) {
        match self.mode {
            Mode::Build => self.camera.apply_move(dt, input),
            Mode::Hunt => {
                let Some(c) = self.character.as_mut() else { return };
                c.apply_move(dt, input, &mut self.physics);
                let feet = c.pos;
                // Advance the hunter toward the player over the baked grid.
                let step = match (self.nav.as_ref(), self.enemy.as_mut()) {
                    (Some(nav), Some(enemy)) => Some(enemy.update(dt, feet, nav)),
                    _ => None,
                };
                if let Some(step) = step {
                    // A blocking door: drain its hp; the breach itself flips the
                    // nav flag + drops the collider with no re-bake.
                    if let Some(di) = step.breaching {
                        self.breach_tick(di, dt);
                    }
                    if step.caught && !self.caught {
                        self.caught = true;
                        log::info!("CAUGHT by the hunter!");
                    }
                }
            }
        }
    }

    /// View-projection for whichever controller is active.
    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        match (self.mode, self.character.as_ref()) {
            (Mode::Hunt, Some(c)) => c.view_proj(aspect),
            _ => self.camera.view_proj(aspect),
        }
    }

    /// Toggle BUILD↔HUNT (bound to `G`). Entering HUNT freezes the geometry and
    /// drops a capsule onto the floor beneath the fly-cam; leaving HUNT restores
    /// the fly-cam at the player's eye so editing can continue.
    pub fn toggle_mode(&mut self) {
        // The authoring tools are BUILD-only; a mode switch always disarms them
        // and clears any sub-face selection state.
        self.opening_tool = None;
        self.opening_preview = None;
        self.place_tool = None;
        self.clear_platform_state();
        self.reset_subface();
        match self.mode {
            Mode::Build => {
                let Some(feet) = self.floor_under(self.camera.pos) else {
                    log::warn!("HUNT: no floor beneath the camera to spawn on — staying in BUILD");
                    return;
                };
                self.character = Some(CharacterController::new(
                    feet,
                    self.camera.yaw,
                    self.camera.pitch,
                ));
                self.selected = None; // clear any authoring selection
                self.caught = false;

                // Bake the nav grid from the frozen geometry (once) and drop a
                // hunter on the standable cell farthest from the player.
                let t0 = Instant::now();
                let structure_solids = self.structure_solid_boxes();
                match nav::bake(&mut self.regions, &structure_solids) {
                    Some(mut nav) => {
                        let bake_ms = t0.elapsed().as_secs_f32() * 1000.0;
                        log::info!(
                            "nav baked in {bake_ms:.2} ms ({} cells)",
                            nav.cell_count()
                        );
                        if let Some(spawn) = nav
                            .all_standable()
                            .into_iter()
                            .max_by(|a, b| {
                                a.distance_squared(feet)
                                    .total_cmp(&b.distance_squared(feet))
                            })
                        {
                            self.enemy = Some(Enemy::new(spawn));
                            log::info!("hunter spawned at {spawn:?}");
                        } else {
                            log::warn!("no standable cell for the hunter");
                        }
                        // Arm breakable doors as a live overlay on the frozen grid
                        // (panel colliders + nav cost). This is the only per-hunt
                        // dynamic layer; the grid itself never re-bakes.
                        self.build_doors(&mut nav);
                        self.nav = Some(nav);
                    }
                    None => log::warn!("nav bake produced no grid"),
                }

                self.mode = Mode::Hunt;
                log::info!("→ HUNT (spawned at {feet:?})");
            }
            Mode::Hunt => {
                if let Some(c) = self.character.take() {
                    self.camera.pos = c.pos + Vec3::new(0.0, WORLD_SCALE * 5.4, 0.0);
                    self.camera.yaw = c.yaw;
                    self.camera.pitch = c.pitch;
                }
                self.nav = None;
                self.enemy = None;
                self.caught = false;
                self.physics.clear_door_colliders();
                self.doors.clear();
                self.mode = Mode::Build;
                log::info!("→ BUILD");
            }
        }
    }

    /// Whether the selection-highlight should be shown (BUILD only).
    pub fn is_build(&self) -> bool {
        self.mode == Mode::Build
    }

    /// The player's feet position (meters), if in HUNT mode.
    pub fn player_pos(&self) -> Option<Vec3> {
        self.character.as_ref().map(|c| c.pos)
    }

    /// Whether the hunter has caught the player.
    pub fn is_caught(&self) -> bool {
        self.caught
    }

    /// A box mesh for the hunter at its current position (meters), for the
    /// renderer's entity pass. `None` when no hunter is active.
    pub fn enemy_mesh(&self) -> Option<CpuMesh> {
        let e = self.enemy.as_ref()?;
        // Capsule-sized box: 0.5 × 1.5 × 0.5 m, centered above the feet.
        let c = e.pos + Vec3::new(0.0, 0.75, 0.0);
        let polys = csg::box_polygons([c.x, c.y, c.z], [0.25, 0.75, 0.25]);
        let (p, n, i) = csg::polygons_to_mesh(&polys);
        Some(CpuMesh::from_csg(&p, &n, &i))
    }

    /// Arm breakable doors for the hunt (JS `door.js` `buildDoors`): scan every
    /// region for `door`-marked brushes, and for each add a panel collider (blocks
    /// the player) + a `World`-side [`Door`] record, then hand the doorframe AABBs
    /// to the nav overlay (index-aligned). Called once at G→HUNT.
    fn build_doors(&mut self, nav: &mut NavWorld) {
        let door_brushes: Vec<Brush> = self
            .regions
            .iter()
            .flat_map(|r| r.brushes.iter().copied().filter(|b| b.door))
            .collect();

        self.doors.clear();
        for b in &door_brushes {
            let min = Vec3::new(b.x, b.y, b.z) * WORLD_SCALE;
            let max = Vec3::new(b.x + b.w, b.y + b.h, b.z + b.d) * WORLD_SCALE;
            let panel = self.physics.add_door_collider(min, max);
            self.doors.push(Door {
                aabb: *b,
                hp: DOOR_HP,
                broken: false,
                panel,
            });
        }

        nav.set_doors(&door_brushes);
        if !door_brushes.is_empty() {
            log::info!("{} door(s) armed for the hunt", door_brushes.len());
        }
    }

    /// Drain a breaching door's hp; on break, remove its panel collider and flip
    /// the live nav flag. **This is the thesis in code:** a built element is
    /// destroyed and both collision and nav react instantly — one collider gone,
    /// one bool flipped — with **no re-voxelization and no CSG re-eval**.
    fn breach_tick(&mut self, di: usize, dt: f32) {
        let broke = {
            let Some(door) = self.doors.get_mut(di) else { return };
            if door.broken {
                return;
            }
            door.hp -= dt;
            if door.hp <= 0.0 {
                door.broken = true;
                Some(door.panel)
            } else {
                None
            }
        };
        if let Some(panel) = broke {
            self.physics.remove_door_collider(panel);
            if let Some(nav) = self.nav.as_mut() {
                nav.break_door(di);
            }
            log::info!(
                "DOOR {di} BREACHED — panel collider removed + nav flag flipped, no re-bake"
            );
        }
    }

    /// A combined mesh of every intact door panel (meters), for the renderer's
    /// door pass. `None` when no intact doors remain — so a breached door simply
    /// vanishes. Cheap to regenerate (a handful of boxes).
    pub fn door_mesh(&self) -> Option<CpuMesh> {
        let mut positions: Vec<f32> = Vec::new();
        let mut normals: Vec<f32> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        for door in self.doors.iter().filter(|d| !d.broken) {
            let b = &door.aabb;
            let c = [
                (b.x + b.w * 0.5) * WORLD_SCALE,
                (b.y + b.h * 0.5) * WORLD_SCALE,
                (b.z + b.d * 0.5) * WORLD_SCALE,
            ];
            let half = [
                b.w * 0.5 * WORLD_SCALE,
                b.h * 0.5 * WORLD_SCALE,
                b.d * 0.5 * WORLD_SCALE,
            ];
            let polys = csg::box_polygons(c, half);
            let (p, n, i) = csg::polygons_to_mesh(&polys);
            let base = (positions.len() / 3) as u32;
            positions.extend_from_slice(&p);
            normals.extend_from_slice(&n);
            indices.extend(i.iter().map(|idx| idx + base));
        }
        if indices.is_empty() {
            return None;
        }
        Some(CpuMesh::from_csg(&positions, &normals, &indices))
    }

    /// Raycast straight down from `from` to find the floor; returns feet position.
    fn floor_under(&mut self, from: Vec3) -> Option<Vec3> {
        // Start a little above the camera so we don't begin inside geometry.
        let origin = from + Vec3::new(0.0, 0.1, 0.0);
        let hit = self.physics.raycast(origin, Vec3::NEG_Y, 100.0)?;
        Some(hit.point)
    }

    /// Select the face under the crosshair (left-click). Returns `true` if a
    /// face was hit. The selection persists and drives push/pull + the highlight.
    /// Picking a *different* face resets sub-face sizing and any active carve
    /// (JS `selectFaceAtCrosshair`).
    pub fn select_at_crosshair(&mut self) -> bool {
        if self.mode != Mode::Build {
            return false;
        }
        let picked = self.pick_face();
        let changed = !same_face(self.selected, picked);
        self.selected = picked;
        if changed {
            self.reset_subface();
        }
        self.selected.is_some()
    }

    /// Clear sub-face selection sizing + any in-progress carve, and drop any
    /// pending stair op (it was anchored to the old face). Mirrors the resets in
    /// JS `selectFaceAtCrosshair`.
    fn reset_subface(&mut self) {
        self.sel_size_u = 0.0;
        self.sel_size_v = 0.0;
        self.sel_bounds = None;
        self.active = None;
        self.pending_stair = None;
    }

    /// The selected face resolved from state, or a fresh crosshair pick if
    /// nothing is selected yet (so `+`/`-` work without an explicit click).
    fn resolve_selection(&mut self) -> Option<Selection> {
        if self.mode != Mode::Build {
            return None; // no authoring during the hunt
        }
        if self.selected.is_none() {
            self.selected = self.pick_face();
        }
        self.selected
    }

    /// The selected face's U/V extent (JS `getFaceUVInfo`). `None` if nothing is
    /// selected (or the brush is gone).
    fn selected_face_info(&self) -> Option<FaceInfo> {
        let sel = self.selected?;
        let region = self.regions.iter().find(|r| r.id == sel.region_id)?;
        let brush = region.brushes.iter().find(|b| b.id == sel.brush_id)?;
        let (u_axis, v_axis) = others(sel.axis);
        let u_min = brush.min(u_axis);
        let v_min = brush.min(v_axis);
        let u_max = u_min + brush.dim(u_axis);
        let v_max = v_min + brush.dim(v_axis);
        Some(FaceInfo {
            u_axis,
            v_axis,
            u_min,
            u_max,
            v_min,
            v_max,
            u_size: u_max - u_min,
            v_size: v_max - v_min,
            position: brush.face_pos(sel.axis, sel.side),
        })
    }

    /// Whether the current selection covers the whole face (JS `isFullFace`) —
    /// i.e. no sub-rect has been scrolled in. Push/pull then resize the brush
    /// directly instead of spawning a sub-face brush.
    fn is_full_face(&self) -> bool {
        match self.selected_face_info() {
            None => true,
            Some(info) => {
                (self.sel_size_u <= 0.0 || self.sel_size_u >= info.u_size)
                    && (self.sel_size_v <= 0.0 || self.sel_size_v >= info.v_size)
            }
        }
    }

    /// Scroll-wheel handler (JS `adjustSelectionSize`): shrink/grow the sub-rect
    /// on the selected face. `du`/`dv` are ±1 WT steps (scroll = U, Shift = V).
    /// Starts from full size, clamps to `[1, faceSize]`, and cancels any active
    /// carve so the next push spawns fresh.
    pub fn adjust_selection_size(&mut self, du: f32, dv: f32) {
        let Some(info) = self.selected_face_info() else { return };
        if du != 0.0 {
            if self.sel_size_u <= 0.0 {
                self.sel_size_u = info.u_size;
            }
            self.sel_size_u = (self.sel_size_u + du).clamp(1.0, info.u_size);
        }
        if dv != 0.0 {
            if self.sel_size_v <= 0.0 {
                self.sel_size_v = info.v_size;
            }
            self.sel_size_v = (self.sel_size_v + dv).clamp(1.0, info.v_size);
        }
        self.active = None;
    }

    /// The sub-rect `[u0, u1, v0, v1]` to carve. Uses the crosshair-tracked
    /// bounds if the preview has run this frame, else a face-centered fallback
    /// (JS `ensureSelectionBounds`).
    fn ensure_selection_bounds(&mut self) -> Option<[f32; 4]> {
        if let Some(b) = self.sel_bounds {
            return Some(b);
        }
        let info = self.selected_face_info()?;
        let s_u = if self.sel_size_u <= 0.0 { info.u_size } else { self.sel_size_u.min(info.u_size) };
        let s_v = if self.sel_size_v <= 0.0 { info.v_size } else { self.sel_size_v.min(info.v_size) };
        let u0 = info.u_min + ((info.u_size - s_u) / 2.0).round();
        let v0 = info.v_min + ((info.v_size - s_v) / 2.0).round();
        let b = [u0, u0 + s_u, v0, v0 + s_v];
        self.sel_bounds = Some(b);
        Some(b)
    }

    /// Push the selected face inward (JS `pushSelectedFace`). Full-face → resize
    /// the brush directly (whole wall moves). Sub-face (a sub-rect scrolled in) →
    /// carve a subtract brush over the sub-rect, growing deeper on repeat.
    /// Returns the changed region's mesh, or `None`.
    pub fn push(&mut self, step: f32) -> Option<RegionMesh> {
        let sel = self.resolve_selection()?;

        if self.is_full_face() {
            let region = self.regions.iter_mut().find(|r| r.id == sel.region_id)?;
            let brush = region.brushes.iter_mut().find(|b| b.id == sel.brush_id)?;
            brush.push_face(sel.axis, sel.side, step);
            self.active = None;
            return self.rebuild_region(sel.region_id);
        }

        // Sub-face carve: grow the active push brush, or spawn one over the rect.
        if matches!(self.active, Some(a) if a.op == SubOp::Push) {
            self.grow_active_brush(step);
        } else {
            let id = self.create_sub_face_brush(Op::Subtract, step)?;
            self.active = Some(ActiveOp { brush_id: id, op: SubOp::Push, side: sel.side });
        }
        self.selected = self.active_outward_face();
        self.sel_size_u = 0.0;
        self.sel_size_v = 0.0;
        self.sel_bounds = None;
        self.rebuild_region(sel.region_id)
    }

    /// Pull the selected face outward (JS `pullSelectedFace`). Full-face → shrink
    /// the brush directly (no-op if too thin). Sub-face → extend an additive brush
    /// (a protrusion) over the sub-rect, growing on repeat.
    pub fn pull(&mut self, step: f32) -> Option<RegionMesh> {
        let sel = self.resolve_selection()?;

        // Continue an active pull first (JS ordering).
        if matches!(self.active, Some(a) if a.op == SubOp::Pull) {
            self.grow_active_brush(step);
            self.selected = self.active_inward_face();
            return self.rebuild_region(sel.region_id);
        }

        if self.is_full_face() {
            let region = self.regions.iter_mut().find(|r| r.id == sel.region_id)?;
            let brush = region.brushes.iter_mut().find(|b| b.id == sel.brush_id)?;
            if !brush.pull_face(sel.axis, sel.side, step) {
                log::info!("pull: brush {} too thin along {:?} — no-op", sel.brush_id, sel.axis);
                return None;
            }
            self.active = None;
            return self.rebuild_region(sel.region_id);
        }

        // Sub-face extend.
        let id = self.create_sub_face_brush(Op::Add, step)?;
        self.active = Some(ActiveOp { brush_id: id, op: SubOp::Pull, side: sel.side });
        self.selected = self.active_inward_face();
        self.sel_size_u = 0.0;
        self.sel_size_v = 0.0;
        self.sel_bounds = None;
        self.rebuild_region(sel.region_id)
    }

    /// Spawn a sub-face brush over the current sub-rect (JS `createSubFaceBrush`):
    /// `depth` deep along the face normal, anchored at the face plane. A subtract
    /// carves inward from the face; an add protrudes outward. Returns its id.
    fn create_sub_face_brush(&mut self, op: Op, depth: f32) -> Option<u32> {
        let bounds = self.ensure_selection_bounds()?;
        let sel = self.selected?;
        let info = self.selected_face_info()?;
        let position = info.position;
        let [u0, u1, v0, v1] = bounds;
        let a = match op {
            Op::Subtract => if sel.side == Side::Max { position } else { position - depth },
            Op::Add => if sel.side == Side::Max { position - depth } else { position },
        };
        let id = self.next_brush_id;
        let mut brush = make_wall_brush(
            id, sel.axis, a, depth, info.u_axis, u0, u1 - u0, info.v_axis, v0, v1 - v0,
        );
        brush.op = op;
        let region = self.regions.iter_mut().find(|r| r.id == sel.region_id)?;
        region.brushes.push(brush);
        self.next_brush_id += 1;
        Some(id)
    }

    /// Grow the active sub-face brush by `amount` (JS `growActiveBrush`, push/pull
    /// cases). A push grows on the face side; a pull grows on the opposite side
    /// (deeper into the room). Reuses `Brush::push_face`, which encodes exactly
    /// that min/dim math.
    fn grow_active_brush(&mut self, amount: f32) {
        let Some(active) = self.active else { return };
        let Some(sel) = self.selected else { return };
        let grow_side = match active.op {
            SubOp::Push => active.side,
            SubOp::Pull => flip(active.side),
        };
        if let Some(brush) = self
            .regions
            .iter_mut()
            .flat_map(|r| r.brushes.iter_mut())
            .find(|b| b.id == active.brush_id)
        {
            brush.push_face(sel.axis, grow_side, amount);
        }
    }

    /// The active brush's outward face (JS `getActiveBrushOutwardFace`) — where
    /// the selection follows to after a sub-face push.
    fn active_outward_face(&self) -> Option<Selection> {
        let active = self.active?;
        let sel = self.selected?;
        Some(Selection {
            region_id: sel.region_id,
            brush_id: active.brush_id,
            axis: sel.axis,
            side: active.side,
        })
    }

    /// The active brush's inward face (JS `getActiveBrushInwardFace`).
    fn active_inward_face(&self) -> Option<Selection> {
        let active = self.active?;
        let sel = self.selected?;
        Some(Selection {
            region_id: sel.region_id,
            brush_id: active.brush_id,
            axis: sel.axis,
            side: flip(active.side),
        })
    }

    /// Build the highlight quad (in meters) for the current selection — the
    /// scrolled-in sub-rect if one exists, else the full face. Used for immediate
    /// post-edit feedback; the crosshair-tracked version is
    /// [`update_selection_preview`](Self::update_selection_preview). `None` when
    /// nothing is selected.
    pub fn selection_face_mesh(&self) -> Option<CpuMesh> {
        let sel = self.selected?;
        let info = self.selected_face_info()?;
        let [u0, u1, v0, v1] = self
            .sel_bounds
            .unwrap_or([info.u_min, info.u_max, info.v_min, info.v_max]);
        Some(self.face_quad_mesh(sel.axis, sel.side, info.position, info.u_axis, info.v_axis, u0, u1, v0, v1))
    }

    /// Recompute the selection sub-rect from the crosshair (JS csgPreviews
    /// `updateSelectionPreview`): while looking at the selected face, center a
    /// `sel_size_u × sel_size_v` rect on the crosshair (full face when unscrolled),
    /// clamp it, store the bounds, and return the ghost quad. `None` when not
    /// looking at the selected face — so the highlight hides (matching JS).
    pub fn update_selection_preview(&mut self) -> Option<CpuMesh> {
        if self.mode != Mode::Build {
            return None;
        }
        let sel = self.selected?;
        let (hit_sel, hit_wt) = self.pick_face_hit()?;
        if !same_face(Some(sel), Some(hit_sel)) {
            return None;
        }
        let info = self.selected_face_info()?;
        let s_u = if self.sel_size_u <= 0.0 { info.u_size } else { self.sel_size_u.min(info.u_size) };
        let s_v = if self.sel_size_v <= 0.0 { info.v_size } else { self.sel_size_v.min(info.v_size) };
        let u0 = (axis_val(hit_wt, info.u_axis) - s_u / 2.0)
            .round()
            .clamp(info.u_min, info.u_max - s_u);
        let v0 = (axis_val(hit_wt, info.v_axis) - s_v / 2.0)
            .round()
            .clamp(info.v_min, info.v_max - s_v);
        self.sel_bounds = Some([u0, u0 + s_u, v0, v0 + s_v]);
        Some(self.face_quad_mesh(sel.axis, sel.side, info.position, info.u_axis, info.v_axis, u0, u0 + s_u, v0, v0 + s_v))
    }

    /// A translucent quad over a face rectangle (meters), nudged slightly toward
    /// the room interior so it sits in front of the wall. Shared by the selection
    /// highlight and the door ghost.
    fn face_quad_mesh(
        &self,
        axis: Axis,
        side: Side,
        position: f32,
        u_axis: Axis,
        v_axis: Axis,
        u0: f32,
        u1: f32,
        v0: f32,
        v1: f32,
    ) -> CpuMesh {
        // Interior is +axis for a Min face, −axis for a Max face.
        let a = position + if side == Side::Max { -0.06 } else { 0.06 };
        let corner = |u: f32, v: f32| -> [f32; 3] {
            let mut p = [0.0f32; 3];
            p[axis_index(axis)] = a;
            p[axis_index(u_axis)] = u;
            p[axis_index(v_axis)] = v;
            [p[0] * WORLD_SCALE, p[1] * WORLD_SCALE, p[2] * WORLD_SCALE]
        };
        let quad = [corner(u0, v0), corner(u1, v0), corner(u1, v1), corner(u0, v1)];
        let n = axis_normal(axis);
        let mut positions = Vec::with_capacity(12);
        let mut normals = Vec::with_capacity(12);
        for c in &quad {
            positions.extend_from_slice(c);
            normals.extend_from_slice(&n);
        }
        // Two tris; cull is disabled in the highlight pipeline so winding is moot.
        let indices = vec![0u32, 1, 2, 0, 2, 3];
        CpuMesh::from_csg(&positions, &normals, &indices)
    }

    /// Re-evaluate a region: rebuild its collider in place and return its mesh.
    /// Logs the bake time — the Phase 1 "does authoring feel instant?" signal.
    fn rebuild_region(&mut self, region_id: u32) -> Option<RegionMesh> {
        let region = self.regions.iter_mut().find(|r| r.id == region_id)?;
        let t0 = Instant::now();
        let mesh = region.evaluate();
        let bake_ms = t0.elapsed().as_secs_f32() * 1000.0;
        self.physics.set_region_collider(region_id, &mesh);
        log::info!(
            "region {region_id} re-baked in {bake_ms:.2} ms ({} tris)",
            mesh.indices.len() / 3
        );
        Some(RegionMesh { id: region_id, mesh })
    }

    /// Raycast the crosshair against the collision world and resolve which brush
    /// face was hit (dropping the hit point). See [`pick_face_hit`](Self::pick_face_hit).
    fn pick_face(&mut self) -> Option<Selection> {
        self.pick_face_hit().map(|(sel, _)| sel)
    }

    /// Raycast the crosshair against the collision world and resolve which brush
    /// face was hit, plus the hit point in WT. Uses geometric matching (like JS
    /// `buildFaceMap`): find the brush face plane the hit point lies on, ignoring
    /// op-dependent normal sign. The WT hit point is what the door-cut tool
    /// centers its opening on.
    fn pick_face_hit(&mut self) -> Option<(Selection, Vec3)> {
        let origin = self.camera.pos;
        let dir = self.camera.forward();
        let hit = self.physics.raycast(origin, dir, 100.0)?;

        // Dominant axis of the surface normal.
        let n = hit.normal.abs();
        let axis = if n.x >= n.y && n.x >= n.z {
            Axis::X
        } else if n.y >= n.z {
            Axis::Y
        } else {
            Axis::Z
        };

        // Hit point in WT space.
        let hit_wt = hit.point / WORLD_SCALE;
        let hit_a = axis_val(hit_wt, axis);
        let (u_axis, v_axis) = others(axis);
        let hit_u = axis_val(hit_wt, u_axis);
        let hit_v = axis_val(hit_wt, v_axis);

        // WT tolerances: on-plane match tight, in-rect containment lenient.
        const PLANE_EPS: f32 = 0.15;
        const RECT_EPS: f32 = 0.15;

        let region = &self.regions[0]; // Phase 1: single region.
        let mut best: Option<(u32, Side, f32)> = None;
        for b in &region.brushes {
            for side in [Side::Min, Side::Max] {
                let plane = b.face_pos(axis, side);
                let d = (plane - hit_a).abs();
                if d > PLANE_EPS {
                    continue;
                }
                // Hit point must lie within the face's other-axes extent.
                let (u0, u1) = (b.min(u_axis), b.min(u_axis) + b.dim(u_axis));
                let (v0, v1) = (b.min(v_axis), b.min(v_axis) + b.dim(v_axis));
                if hit_u < u0 - RECT_EPS
                    || hit_u > u1 + RECT_EPS
                    || hit_v < v0 - RECT_EPS
                    || hit_v > v1 + RECT_EPS
                {
                    continue;
                }
                if best.map(|(_, _, bd)| d < bd).unwrap_or(true) {
                    best = Some((b.id, side, d));
                }
            }
        }

        best.map(|(brush_id, side, _)| {
            (
                Selection {
                    region_id: region.id,
                    brush_id,
                    axis,
                    side,
                },
                hit_wt,
            )
        })
    }

    // ─── Opening tools: door (fixed, breakable) + hole (arbitrary, any face) ──

    /// Whether a crosshair opening tool is armed (door or hole). The app draws
    /// the ghost and routes a left-click confirm while this is true.
    pub fn is_opening_arming(&self) -> bool {
        self.opening_tool.is_some()
    }

    /// Whether the *hole* tool specifically is armed (so the app routes scroll to
    /// hole sizing instead of sub-face selection).
    pub fn is_hole_arming(&self) -> bool {
        self.opening_tool == Some(OpeningKind::Hole)
    }

    /// Arm/toggle a crosshair opening tool, BUILD only (JS `setHoleMode`). Pressing
    /// the same tool's key again disarms; a different key switches tools. Never
    /// cuts (the cut is a left-click), so it returns `None`.
    fn arm_opening(&mut self, kind: OpeningKind) -> Option<RegionMesh> {
        if self.mode != Mode::Build {
            return None;
        }
        if self.opening_tool == Some(kind) {
            self.cancel_opening(); // same key again = deselect
        } else {
            // The ghost preview owns the highlight, so drop any face pick and any
            // other armed tool.
            self.place_tool = None;
            self.clear_platform_state();
            self.opening_tool = Some(kind);
            self.selected = None;
            if kind == OpeningKind::Hole {
                self.hole_w = HOLE_WIDTH;
                self.hole_h = HOLE_HEIGHT;
            }
            self.opening_preview = self.resolve_opening_placement();
        }
        None
    }

    /// Door tool key (`B`): arm/toggle the fixed breakable door.
    pub fn door_tool_key(&mut self) -> Option<RegionMesh> {
        self.arm_opening(OpeningKind::Door)
    }

    /// Hole tool key (`H`): arm/toggle the arbitrary-size opening (any face).
    pub fn hole_tool_key(&mut self) -> Option<RegionMesh> {
        self.arm_opening(OpeningKind::Hole)
    }

    /// Confirm the armed opening (left-click). Cuts at the previewed placement,
    /// falling back to a fresh crosshair resolve.
    pub fn confirm_opening(&mut self) -> Option<RegionMesh> {
        self.opening_tool?;
        self.opening_tool = None;
        let placement = self.opening_preview.take().or_else(|| self.resolve_opening_placement());
        placement.and_then(|p| self.cut_opening(p))
    }

    /// Cancel an armed opening without cutting (Esc / pointer release / mode switch).
    pub fn cancel_opening(&mut self) {
        self.opening_tool = None;
        self.opening_preview = None;
    }

    /// Recompute the ghost from the crosshair (each frame while arming) and return
    /// the ghost quad, or `None` if the crosshair isn't on a suitable face.
    pub fn update_opening_preview(&mut self) -> Option<CpuMesh> {
        self.opening_tool?;
        self.opening_preview = self.resolve_opening_placement();
        self.opening_preview.map(|p| self.opening_preview_mesh(&p))
    }

    /// Scroll-size the hole (only while the hole tool is armed): `du` widens (U),
    /// `dv` heightens (V), in ±1 WT steps, clamped to ≥1. The upper clamp to the
    /// face happens in [`resolve_opening_placement`](Self::resolve_opening_placement).
    pub fn adjust_opening_size(&mut self, du: f32, dv: f32) {
        if self.opening_tool != Some(OpeningKind::Hole) {
            return;
        }
        if du != 0.0 {
            self.hole_w = (self.hole_w + du).max(1.0);
        }
        if dv != 0.0 {
            self.hole_h = (self.hole_h + dv).max(1.0);
        }
    }

    /// Resolve an opening placement from the crosshair (JS `computeHolePreview`):
    /// the face hit → a `w × h` opening centered on the hit, clamped to the face
    /// and WT-snapped. Door: fixed 3×7, walls only. Hole: `hole_w × hole_h`
    /// (clamped to the face), any face incl. floor/ceiling. `None` if the face is
    /// unsuitable or too small.
    fn resolve_opening_placement(&mut self) -> Option<OpeningPlacement> {
        let kind = self.opening_tool?;
        if self.mode != Mode::Build {
            return None;
        }
        let (sel, hit_wt) = self.pick_face_hit()?;
        if kind == OpeningKind::Door && sel.axis == Axis::Y {
            return None; // doors go in walls only (JS rejects axis 'y')
        }
        let region = self.regions.iter().find(|r| r.id == sel.region_id)?;
        let brush = *region.brushes.iter().find(|b| b.id == sel.brush_id)?;
        let position = brush.face_pos(sel.axis, sel.side);

        // Face UV bounds (JS `getFaceUVInfo`): the two axes orthogonal to the face
        // normal. The opening must fit within them.
        let (u_axis, v_axis) = others(sel.axis);
        let (u_min, u_max) = (brush.min(u_axis), brush.min(u_axis) + brush.dim(u_axis));
        let (v_min, v_max) = (brush.min(v_axis), brush.min(v_axis) + brush.dim(v_axis));
        let (face_w, face_h) = (u_max - u_min, v_max - v_min);

        let (w, h) = match kind {
            OpeningKind::Door => (DOOR_WIDTH, DOOR_HEIGHT),
            OpeningKind::Hole => (self.hole_w.min(face_w), self.hole_h.min(face_h)),
        };
        if face_w < w || face_h < h || w < 1.0 || h < 1.0 {
            return None;
        }

        let u0 = ((axis_val(hit_wt, u_axis) - w / 2.0).round()).clamp(u_min, u_max - w);
        let v0 = ((axis_val(hit_wt, v_axis) - h / 2.0).round()).clamp(v_min, v_max - h);

        Some(OpeningPlacement {
            region_id: sel.region_id,
            axis: sel.axis,
            side: sel.side,
            position,
            u_axis,
            v_axis,
            u0,
            v0,
            w,
            h,
            kind,
        })
    }

    /// Cut the opening at a resolved placement (JS `confirmHolePlacement`): a frame
    /// subtract through the face + a 1-WT protoroom subtract just beyond, so it
    /// opens into navigable space, not solid. A door's frame is `door`-marked
    /// (breakable at HUNT); a hole's isn't.
    fn cut_opening(&mut self, p: OpeningPlacement) -> Option<RegionMesh> {
        let t = WALL_THICKNESS;
        // Frame carve: 1 WT deep along the face normal, at the face plane.
        let frame_a = if p.side == Side::Max { p.position } else { p.position - t };
        let mut frame = make_wall_brush(
            self.next_brush_id, p.axis, frame_a, t, p.u_axis, p.u0, p.w, p.v_axis, p.v0, p.h,
        );
        frame.door = p.kind == OpeningKind::Door;
        self.next_brush_id += 1;

        // Protoroom carve: 1 WT deep just beyond the frame.
        let proto_a = if p.side == Side::Max { p.position + t } else { p.position - 2.0 * t };
        let proto = make_wall_brush(
            self.next_brush_id, p.axis, proto_a, t, p.u_axis, p.u0, p.w, p.v_axis, p.v0, p.h,
        );
        self.next_brush_id += 1;

        let region = self.regions.iter_mut().find(|r| r.id == p.region_id)?;
        region.brushes.push(frame);
        region.brushes.push(proto);
        log::info!("{:?} cut in region {} at {:?} {:?}", p.kind, p.region_id, p.axis, p.side);
        self.rebuild_region(p.region_id)
    }

    /// The ghost preview quad (meters) for an opening placement — the opening rect
    /// on the face. Drawn via the translucent highlight pipeline.
    fn opening_preview_mesh(&self, p: &OpeningPlacement) -> CpuMesh {
        self.face_quad_mesh(
            p.axis, p.side, p.position, p.u_axis, p.v_axis, p.u0, p.u0 + p.w, p.v0, p.v0 + p.h,
        )
    }

    // ── Door-named wrappers, kept so the door tests/callers stay stable. ──

    /// Whether the *door* tool specifically is armed.
    pub fn is_door_arming(&self) -> bool {
        self.opening_tool == Some(OpeningKind::Door)
    }

    /// Confirm the armed door (delegates to the generic opening confirm).
    pub fn confirm_door(&mut self) -> Option<RegionMesh> {
        self.confirm_opening()
    }

    /// Cancel an armed door (delegates to the generic opening cancel).
    pub fn cancel_door(&mut self) {
        self.cancel_opening()
    }

    /// Recompute the door ghost (delegates to the generic opening preview).
    pub fn update_door_preview(&mut self) -> Option<CpuMesh> {
        self.update_opening_preview()
    }

    // ─── Placement tools: pillar (column) + brace (arch) ─────────────────────

    /// Whether a placement tool (pillar/brace) is armed. The app draws its ghost
    /// and routes a left-click confirm + scroll sizing while this is true.
    pub fn is_placing(&self) -> bool {
        self.place_tool.is_some()
    }

    /// Arm/toggle a placement tool, BUILD only. Same key again disarms; a
    /// different tool switches. Cancels any armed opening tool.
    fn arm_place(&mut self, kind: PlaceKind) {
        if self.mode != Mode::Build {
            return;
        }
        if self.place_tool == Some(kind) {
            self.place_tool = None;
        } else {
            self.opening_tool = None;
            self.opening_preview = None;
            self.clear_platform_state();
            self.selected = None;
            self.place_tool = Some(kind);
        }
    }

    /// Pillar tool key (`P`): arm/toggle the floor→ceiling column.
    pub fn pillar_tool_key(&mut self) {
        self.arm_place(PlaceKind::Pillar);
    }

    /// Brace tool key (`R`): arm/toggle the 3-brush wall arch.
    pub fn brace_tool_key(&mut self) {
        self.arm_place(PlaceKind::Brace);
    }

    /// Cancel an armed placement tool (Esc / pointer release).
    pub fn cancel_place(&mut self) {
        self.place_tool = None;
    }

    /// Scroll-size the armed placement tool: pillars use `da` (square size);
    /// braces use `da` (width along the wall) and `db` (depth into the room).
    /// Clamped to the tool's bounds.
    pub fn adjust_place_size(&mut self, da: f32, db: f32) {
        match self.place_tool {
            Some(PlaceKind::Pillar) => {
                self.pillar_size = (self.pillar_size + da).clamp(PILLAR_MIN, PILLAR_MAX);
            }
            Some(PlaceKind::Brace) => {
                if da != 0.0 {
                    self.brace_width = (self.brace_width + da).clamp(BRACE_MIN, BRACE_MAX);
                }
                if db != 0.0 {
                    self.brace_depth = (self.brace_depth + db).clamp(BRACE_MIN, BRACE_MAX);
                }
            }
            None => {}
        }
    }

    /// The ghost mesh for the armed placement tool (each frame while arming), or
    /// `None` if the crosshair isn't on a valid face. Drawn via the highlight
    /// pipeline (translucent boxes).
    pub fn update_place_preview(&mut self) -> Option<CpuMesh> {
        match self.place_tool? {
            PlaceKind::Pillar => {
                let boxes = self.resolve_pillar()?;
                Some(boxes_mesh(&[boxes]))
            }
            PlaceKind::Brace => {
                let boxes = self.resolve_brace()?;
                Some(boxes_mesh(&boxes))
            }
        }
    }

    /// Confirm the armed placement (left-click): add the pillar's single brush or
    /// the brace's three brushes to the region and re-evaluate. Returns the
    /// changed region's mesh, or `None`.
    pub fn confirm_place(&mut self) -> Option<RegionMesh> {
        match self.place_tool? {
            PlaceKind::Pillar => {
                let (region_id, b) = self.resolve_pillar_placed()?;
                self.place_tool = None;
                let brush = self.push_add_brush(region_id, b)?;
                log::info!("pillar placed in region {region_id} (brush {brush})");
                self.rebuild_region(region_id)
            }
            PlaceKind::Brace => {
                let (region_id, boxes) = self.resolve_brace_placed()?;
                self.place_tool = None;
                for b in boxes {
                    self.push_add_brush(region_id, b);
                }
                log::info!("brace placed in region {region_id}");
                self.rebuild_region(region_id)
            }
        }
    }

    /// Push an `Op::Add` brush (WT AABB `[x,y,z,w,h,d]`) into a region; returns its id.
    fn push_add_brush(&mut self, region_id: u32, b: [f32; 6]) -> Option<u32> {
        let id = self.next_brush_id;
        let brush = Brush::new(id, Op::Add, b[0], b[1], b[2], b[3], b[4], b[5]);
        let region = self.regions.iter_mut().find(|r| r.id == region_id)?;
        region.brushes.push(brush);
        self.next_brush_id += 1;
        Some(id)
    }

    /// Resolve the pillar box (WT `[x,y,z,w,h,d]`) under the crosshair, or `None`
    /// if not aimed at a floor (JS `computePillarPreview`: axis Y, side Min).
    fn resolve_pillar(&mut self) -> Option<[f32; 6]> {
        self.resolve_pillar_placed().map(|(_, b)| b)
    }

    /// Like [`resolve_pillar`](Self::resolve_pillar) but also returns the region id.
    fn resolve_pillar_placed(&mut self) -> Option<(u32, [f32; 6])> {
        if self.mode != Mode::Build {
            return None;
        }
        let (sel, hit_wt) = self.pick_face_hit()?;
        if sel.axis != Axis::Y || sel.side != Side::Min {
            return None; // pillars stand on floors only
        }
        let region = self.regions.iter().find(|r| r.id == sel.region_id)?;
        let brush = *region.brushes.iter().find(|b| b.id == sel.brush_id)?;
        if brush.op != Op::Subtract {
            return None;
        }
        let ps = self.pillar_size;
        let e = BURY_EPS;
        let (min_x, max_x) = (brush.x, brush.x + brush.w);
        let (min_y, max_y) = (brush.y, brush.y + brush.h);
        let (min_z, max_z) = (brush.z, brush.z + brush.d);
        // Snap the cursor to WT and center the (integer) footprint on it.
        let x0 = (hit_wt.x.round() - (ps / 2.0).floor()).clamp(min_x, max_x - ps);
        let z0 = (hit_wt.z.round() - (ps / 2.0).floor()).clamp(min_z, max_z - ps);
        Some((
            sel.region_id,
            [x0, min_y - e, z0, ps, (max_y - min_y) + 2.0 * e, ps],
        ))
    }

    /// Resolve the three brace boxes under the crosshair, or `None` if not aimed
    /// at a wall (JS `computeBracePreview`: axis X or Z, on a subtract brush).
    fn resolve_brace(&mut self) -> Option<[[f32; 6]; 3]> {
        self.resolve_brace_placed().map(|(_, boxes)| boxes)
    }

    /// Like [`resolve_brace`](Self::resolve_brace) but also returns the region id.
    fn resolve_brace_placed(&mut self) -> Option<(u32, [[f32; 6]; 3])> {
        if self.mode != Mode::Build {
            return None;
        }
        let (sel, hit_wt) = self.pick_face_hit()?;
        if sel.axis == Axis::Y {
            return None; // braces are wall→ceiling→wall arches
        }
        let region = self.regions.iter().find(|r| r.id == sel.region_id)?;
        let brush = *region.brushes.iter().find(|b| b.id == sel.brush_id)?;
        if brush.op != Op::Subtract {
            return None;
        }
        let (bw, bd, e) = (self.brace_width, self.brace_depth, BURY_EPS);
        let (ix0, ix1) = (brush.x, brush.x + brush.w);
        let (iy0, iy1) = (brush.y, brush.y + brush.h);
        let (iz0, iz1) = (brush.z, brush.z + brush.d);
        let ih = iy1 - iy0;

        let boxes = if sel.axis == Axis::X {
            // Arch spans across X; U runs along Z (position from the cursor Z).
            let z0 = (hit_wt.z.round() - (bw / 2.0).floor()).clamp(iz0, iz1 - bw);
            [
                [ix0 - e, iy0 - e, z0, bd + e, ih + 2.0 * e, bw], // wall on min-X
                [ix0 - e, iy1 - bd, z0, (ix1 - ix0) + 2.0 * e, bd + e, bw], // ceiling strip
                [ix1 - bd, iy0 - e, z0, bd + e, ih + 2.0 * e, bw], // wall on max-X
            ]
        } else {
            // Arch spans across Z; U runs along X.
            let x0 = (hit_wt.x.round() - (bw / 2.0).floor()).clamp(ix0, ix1 - bw);
            [
                [x0, iy0 - e, iz0 - e, bw, ih + 2.0 * e, bd + e], // wall on min-Z
                [x0, iy1 - bd, iz0 - e, bw, bd + e, (iz1 - iz0) + 2.0 * e], // ceiling strip
                [x0, iy0 - e, iz1 - bd, bw, ih + 2.0 * e, bd + e], // wall on max-Z
            ]
        };
        Some((sel.region_id, boxes))
    }

    // ─── Free-standing platform + stair-run tool ────────────────────────

    /// Whether the platform tool is armed (the app routes clicks/scroll/ghost to
    /// it, and Esc backs out of its sub-phases).
    pub fn is_platform_tool(&self) -> bool {
        self.platform_phase.is_some()
    }

    /// Whether the platform tool is in its idle/placement phase (so the app shows
    /// the placement ghost and routes scroll to footprint sizing).
    pub fn is_platform_placing(&self) -> bool {
        self.platform_phase == Some(PlatformPhase::Idle)
    }

    /// Disarm the platform tool entirely (Esc / pointer release), mirroring
    /// `cancel_opening`/`cancel_place`.
    pub fn cancel_platform_tool(&mut self) {
        self.clear_platform_state();
    }

    /// Clear all platform-tool state (turning the tool off).
    fn clear_platform_state(&mut self) {
        self.platform_phase = None;
        self.selected_platform = None;
        self.selected_run = None;
        self.connect_from = None;
        self.connect_to = None;
        self.connect_edge = None;
        self.simple_from = None;
        self.gizmo_drag = None;
    }

    /// Platform tool key (`T`): arm/toggle. Arming disarms the opening/placement
    /// tools + drops any face selection (mutually exclusive modal tools).
    pub fn platform_tool_key(&mut self) {
        if self.mode != Mode::Build {
            return;
        }
        if self.platform_phase.is_some() {
            self.clear_platform_state();
        } else {
            self.opening_tool = None;
            self.opening_preview = None;
            self.place_tool = None;
            self.selected = None;
            self.platform_size_x = PLATFORM_SIZE;
            self.platform_size_z = PLATFORM_SIZE;
            self.platform_phase = Some(PlatformPhase::Idle);
            log::info!("platform tool armed — click a surface to place, click a platform to select");
        }
    }

    /// Esc while the platform tool is active: cancel an active gizmo drag
    /// (restoring the platform), else back out of a sub-phase (connect /
    /// simple-stair). Returns `(consumed, changed_mesh)` — `consumed` tells the
    /// app not to also release the pointer; `changed_mesh` is `Some` when the
    /// cancel restored geometry that must be re-uploaded.
    pub fn platform_escape(&mut self) -> (bool, Option<RegionMesh>) {
        if let Some(drag) = self.gizmo_drag.take() {
            // Restore the platform to its pre-drag transform.
            if let Some(p) = self.platforms.iter_mut().find(|p| p.id == drag.platform_id) {
                *p = drag.orig;
            }
            return (true, Some(self.rebuild_structures()));
        }
        match self.platform_phase {
            // Back-out ladder: ConnectSrc → re-pick destination; ConnectDst → done.
            Some(PlatformPhase::ConnectSrc) => {
                self.connect_to = None;
                self.connect_edge = None;
                self.platform_phase = Some(PlatformPhase::ConnectDst);
                (true, None)
            }
            Some(PlatformPhase::ConnectDst) => {
                self.connect_from = None;
                self.platform_phase = Some(PlatformPhase::Selected);
                (true, None)
            }
            Some(PlatformPhase::SimpleFrom) | Some(PlatformPhase::SimpleTo) => {
                self.simple_from = None;
                self.platform_phase = Some(PlatformPhase::Idle);
                (true, None)
            }
            _ => (false, None),
        }
    }

    /// Handle a left-click while the platform tool is armed. The gizmo takes
    /// precedence when a platform is selected: a click confirms an active drag,
    /// or starts one if a handle is under the crosshair (JS `gizmo` click flow).
    /// Otherwise dispatch on the phase: place/select, connect, or simple-stair.
    /// Returns the rebuilt structures mesh when geometry changed, else `None`.
    pub fn platform_click(&mut self) -> Option<RegionMesh> {
        if self.gizmo_drag.is_some() {
            self.gizmo_drag = None; // click confirms the drag (geometry already applied)
            return None;
        }
        if self.platform_phase == Some(PlatformPhase::Selected) {
            if let Some(handle) = self.gizmo_pick() {
                self.gizmo_start(handle);
                return None;
            }
        }
        match self.platform_phase? {
            PlatformPhase::Idle | PlatformPhase::Selected => self.place_or_select_click(),
            PlatformPhase::ConnectDst => {
                self.connect_lock_target();
                None
            }
            PlatformPhase::ConnectSrc => self.connect_confirm(),
            PlatformPhase::SimpleFrom => {
                self.simple_stair_first_click();
                None
            }
            PlatformPhase::SimpleTo => self.simple_stair_second_click(),
        }
    }

    /// Idle/Selected click: select the platform/stair-run under the crosshair, or
    /// (nothing hit while selected) deselect, or (nothing hit while idle) place a
    /// new platform. Ports `indoorClick.js` platform idle/selected branch.
    fn place_or_select_click(&mut self) -> Option<RegionMesh> {
        let hit = self.pick_structure_hit();
        if let Some(h) = hit {
            if let Some(pid) = h.platform {
                self.selected_platform = Some(pid);
                self.selected_run = None;
                self.platform_phase = Some(PlatformPhase::Selected);
                log::info!("selected platform {pid}");
                return None;
            }
            if let Some(rid) = h.run {
                self.selected_run = Some(rid);
                self.selected_platform = None;
                self.platform_phase = Some(PlatformPhase::Selected);
                log::info!("selected stair-run {rid}");
                return None;
            }
        }
        // Empty surface (or miss): a selected platform deselects; an idle click
        // places a new platform at the hit.
        if self.platform_phase == Some(PlatformPhase::Selected) {
            self.selected_platform = None;
            self.selected_run = None;
            self.platform_phase = Some(PlatformPhase::Idle);
            log::info!("deselected");
            return None;
        }
        let mut p = self.resolve_platform_placement(hit?)?;
        p.id = self.next_platform_id;
        self.next_platform_id += 1;
        let id = p.id;
        self.platforms.push(p);
        self.selected_platform = Some(id);
        self.selected_run = None;
        self.platform_phase = Some(PlatformPhase::Selected);
        log::info!("placed platform {id}");
        Some(self.rebuild_structures())
    }

    /// Resolve the platform the crosshair would place (id 0 — the caller assigns
    /// the real id). "Aim-point sets top surface": the platform's top Y is the
    /// (WT-snapped) hit Y, centered on the hit in XZ. Aiming at a vertical wall
    /// butts the near edge against the wall (JS `indoorClick.js` wall-offset).
    fn resolve_platform_placement(&self, h: StructureHit) -> Option<Platform> {
        let sx = h.hit_wt.x.round();
        let sy = h.hit_wt.y.round();
        let sz = h.hit_wt.z.round();
        let size_x = self.platform_size_x;
        let size_z = self.platform_size_z;
        let mut px = sx - (size_x / 2.0).floor();
        let mut pz = sz - (size_z / 2.0).floor();
        if h.axis == Axis::X {
            let cam = self.camera.pos.x / WORLD_SCALE;
            px = if cam > sx { sx } else { sx - size_x };
        } else if h.axis == Axis::Z {
            let cam = self.camera.pos.z / WORLD_SCALE;
            pz = if cam > sz { sz } else { sz - size_z };
        }
        Some(Platform {
            id: 0,
            x: px,
            y: sy,
            z: pz,
            size_x,
            size_z,
            thickness: PLATFORM_THICKNESS,
            grounded: false,
            railings: false,
        })
    }

    /// The platform-tool ghost, drawn via the translucent highlight pipeline:
    /// - `Idle` — the to-be-placed platform slab.
    /// - `ConnectDst` — a small marker cube at the destination the crosshair would
    ///   lock (no swinging staircase — the target isn't chosen yet).
    /// - `ConnectSrc` — the stable stair-run ghost; only the attach offset slides
    ///   along the frozen source edge as you aim (JS connect-preview).
    pub fn update_platform_preview(&mut self) -> Option<CpuMesh> {
        match self.platform_phase? {
            PlatformPhase::Idle => {
                let hit = self.pick_structure_hit()?;
                if hit.platform.is_some() || hit.run.is_some() {
                    return None;
                }
                let p = self.resolve_platform_placement(hit)?;
                let brushes = self.all_region_brushes();
                let b = p.solid_box(&brushes)?;
                Some(boxes_mesh(&[b]))
            }
            PlatformPhase::ConnectDst => {
                // A 1-WT marker where the destination will lock (platform edge
                // midpoint, or the snapped floor point) — stable, no staircase yet.
                let hit = self.pick_structure_hit()?;
                let c = match hit.platform.filter(|&id| Some(id) != self.connect_from) {
                    Some(tid) => {
                        let tp = self.platform_by_id(tid)?;
                        let edge = structures::closest_platform_edge(&tp, hit.hit_wt.x, hit.hit_wt.z);
                        let (mx, mz) = tp.edge_point_at_offset(edge, 0.5);
                        [mx, tp.y, mz]
                    }
                    None => [hit.hit_wt.x.round(), hit.hit_wt.y.round(), hit.hit_wt.z.round()],
                };
                Some(boxes_mesh(&[[c[0] - 0.5, c[1] - 0.5, c[2] - 0.5, 1.0, 1.0, 1.0]]))
            }
            PlatformPhase::ConnectSrc => {
                let run = self.resolve_connect_run()?;
                let (fp, tp) = self.run_platforms(&run);
                let brushes = self.all_region_brushes();
                let boxes = structures::stair_run_boxes(&run, fp.as_ref(), tp.as_ref(), &brushes);
                Some(boxes_mesh(&boxes))
            }
            _ => None,
        }
    }

    /// Scroll-size the next platform's footprint (idle phase): `du` = X, `dv` = Z.
    pub fn adjust_platform_size(&mut self, du: f32, dv: f32) {
        if self.platform_phase != Some(PlatformPhase::Idle) {
            return;
        }
        if du != 0.0 {
            self.platform_size_x = (self.platform_size_x + du).clamp(PLATFORM_SIZE_MIN, PLATFORM_SIZE_MAX);
        }
        if dv != 0.0 {
            self.platform_size_z = (self.platform_size_z + dv).clamp(PLATFORM_SIZE_MIN, PLATFORM_SIZE_MAX);
        }
    }

    /// Connect key (`C`): arm the stair-connect from the selected platform. The
    /// next click picks the destination (JS `connect_stairs`, phase `connecting_dst`).
    pub fn connect_key(&mut self) {
        if self.platform_phase == Some(PlatformPhase::Selected) {
            if let Some(pid) = self.selected_platform {
                self.connect_from = Some(pid);
                self.platform_phase = Some(PlatformPhase::ConnectDst);
                log::info!("connect: click a destination platform or the floor (Esc cancels)");
            }
        }
    }

    /// Connect step 1 (JS `connecting_dst`): lock the destination the crosshair is
    /// on (a platform's nearest edge, or a floor point) and freeze the source edge
    /// that faces it, then advance to the slide step. No build, no phase change if
    /// nothing valid is under the crosshair.
    fn connect_lock_target(&mut self) {
        let Some(from_id) = self.connect_from else {
            return;
        };
        let Some(from_plat) = self.platform_by_id(from_id) else {
            return;
        };
        let Some(hit) = self.pick_structure_hit() else {
            return;
        };
        let (target, approx) = match hit.platform.filter(|&id| id != from_id) {
            Some(tid) => {
                let Some(to_plat) = self.platform_by_id(tid) else {
                    return;
                };
                let edge = structures::closest_platform_edge(&to_plat, hit.hit_wt.x, hit.hit_wt.z);
                let (tx, tz) = to_plat.edge_point_at_offset(edge, 0.5);
                (
                    ConnectTarget::Platform { id: tid, edge },
                    Vec3::new(tx, to_plat.y, tz),
                )
            }
            None => {
                let (gx, gy, gz) = (hit.hit_wt.x.round(), hit.hit_wt.y.round(), hit.hit_wt.z.round());
                (
                    ConnectTarget::Ground { x: gx, y: gy, z: gz },
                    Vec3::new(gx, gy, gz),
                )
            }
        };
        // Source edge = the one whose outward normal best faces the target — chosen
        // ONCE here and frozen, so the ghost can't swing sides while sliding.
        let from_edge = structures::best_edge_for_direction(
            approx.x - from_plat.center_x(),
            approx.z - from_plat.center_z(),
        );
        self.connect_to = Some(target);
        self.connect_edge = Some(from_edge);
        // Start the attach point at the edge midpoint; the wheel slides it in 1-WT
        // steps from there.
        self.connect_slide_wt = (from_plat.edge_length(from_edge) / 2.0).round();
        self.platform_phase = Some(PlatformPhase::ConnectSrc);
        log::info!("connect: destination locked — scroll to slide along the edge, click to place (Esc re-picks)");
    }

    /// Connect step 2 commit (JS `connecting_src` click): build the resolved run
    /// and return to `Selected`. Clears the connect state.
    fn connect_confirm(&mut self) -> Option<RegionMesh> {
        let run = self.resolve_connect_run();
        self.platform_phase = Some(PlatformPhase::Selected);
        self.connect_from = None;
        self.connect_to = None;
        self.connect_edge = None;
        let mut run = match run {
            Some(r) => r,
            None => {
                log::info!("connect: endpoints too close or level — nothing built");
                return None;
            }
        };
        run.id = self.next_run_id;
        self.next_run_id += 1;
        let id = run.id;
        self.stair_runs.push(run);
        log::info!("stair-run {id} created");
        Some(self.rebuild_structures())
    }

    /// Resolve the stair-run for the current slide (id 0), from the locked
    /// destination + source edge and the crosshair-projected attach offset. `None`
    /// if the endpoints are too close horizontally or level. Shared by the stable
    /// ConnectSrc ghost and the commit, so they always agree.
    fn resolve_connect_run(&mut self) -> Option<StairRun> {
        let from_id = self.connect_from?;
        let from_plat = self.platform_by_id(from_id)?;
        let from_edge = self.connect_edge?;
        let target = self.connect_to?;

        // Attach point slides along the (frozen) source edge — driven by the wheel
        // (`connect_slide_wt`), not the aim, so it never twitches with the camera.
        let edge_len = from_plat.edge_length(from_edge);
        let offset = if edge_len > 0.0 {
            (self.connect_slide_wt / edge_len).clamp(0.0, 1.0)
        } else {
            0.5
        };
        let (fx, fz) = from_plat.edge_point_at_offset(from_edge, offset);
        let from_pt = Vec3::new(fx, from_plat.y, fz);

        let (to_platform_id, anchor_to, to_pt): (Option<u32>, Anchor, Vec3) = match target {
            ConnectTarget::Platform { id, edge } => {
                let to_plat = self.platform_by_id(id)?;
                // Align the destination anchor to the slid source point.
                let toff = structures::offset_along_edge(&to_plat, edge, from_pt.x, from_pt.z);
                let (tx, tz) = to_plat.edge_point_at_offset(edge, toff);
                (
                    Some(id),
                    Anchor::Edge { edge, offset: toff },
                    Vec3::new(tx, to_plat.y, tz),
                )
            }
            ConnectTarget::Ground { x, y, z } => {
                (None, Anchor::Ground { x, y, z }, Vec3::new(x, y, z))
            }
        };

        if (to_pt.x - from_pt.x).abs() < 1.0 && (to_pt.z - from_pt.z).abs() < 1.0 {
            return None;
        }
        if (to_pt.y - from_pt.y).abs() == 0.0 {
            return None;
        }

        Some(StairRun {
            id: 0,
            from_platform: Some(from_id),
            to_platform: to_platform_id,
            anchor_from: Anchor::Edge { edge: from_edge, offset },
            anchor_to,
            width: STAIR_WIDTH,
            step_height: STAIR_STEP_HEIGHT,
            rise_over_run: STAIR_RISE_OVER_RUN,
            grounded: false,
            railings: false,
        })
    }

    /// Whether the connect tool is in its slide step (so the app routes the scroll
    /// wheel to the attach-point slide instead of platform sizing).
    pub fn is_connect_sliding(&self) -> bool {
        self.platform_phase == Some(PlatformPhase::ConnectSrc)
    }

    /// Slide the attach point along the frozen source edge by `steps` WT (scroll
    /// wheel during `ConnectSrc`), clamped to the edge length.
    pub fn adjust_connect_slide(&mut self, steps: f32) {
        if self.platform_phase != Some(PlatformPhase::ConnectSrc) {
            return;
        }
        let edge_len = self
            .connect_from
            .and_then(|id| self.platform_by_id(id))
            .zip(self.connect_edge)
            .map(|(p, e)| p.edge_length(e))
            .unwrap_or(0.0);
        self.connect_slide_wt = (self.connect_slide_wt + steps).clamp(0.0, edge_len);
    }

    /// Simple-stair key (`K`): arm a two-click free stair-run between any two
    /// surface points (JS `simple_stairs`). Available from Idle or Selected.
    pub fn simple_stair_key(&mut self) {
        if matches!(
            self.platform_phase,
            Some(PlatformPhase::Idle) | Some(PlatformPhase::Selected)
        ) {
            self.simple_from = None;
            self.selected_platform = None;
            self.selected_run = None;
            self.platform_phase = Some(PlatformPhase::SimpleFrom);
            log::info!("simple stair: click the first endpoint");
        }
    }

    fn simple_stair_first_click(&mut self) {
        if let Some(hit) = self.pick_structure_hit() {
            self.simple_from = Some(Vec3::new(
                hit.hit_wt.x.round(),
                hit.hit_wt.y.round(),
                hit.hit_wt.z.round(),
            ));
            self.platform_phase = Some(PlatformPhase::SimpleTo);
            log::info!("simple stair: click the second endpoint (Esc cancels)");
        }
    }

    fn simple_stair_second_click(&mut self) -> Option<RegionMesh> {
        let from = self.simple_from?;
        let hit = self.pick_structure_hit()?;
        let to = Vec3::new(hit.hit_wt.x.round(), hit.hit_wt.y.round(), hit.hit_wt.z.round());
        self.simple_from = None;
        self.platform_phase = Some(PlatformPhase::Idle);

        if (to.y - from.y).abs() == 0.0 {
            log::info!("simple stair: endpoints at the same height");
            return None;
        }
        if (to.x - from.x).abs() < 1.0 && (to.z - from.z).abs() < 1.0 {
            log::info!("simple stair: need horizontal distance");
            return None;
        }
        let id = self.next_run_id;
        self.next_run_id += 1;
        self.stair_runs.push(StairRun {
            id,
            from_platform: None,
            to_platform: None,
            anchor_from: Anchor::Ground {
                x: from.x,
                y: from.y,
                z: from.z,
            },
            anchor_to: Anchor::Ground {
                x: to.x,
                y: to.y,
                z: to.z,
            },
            width: STAIR_WIDTH,
            step_height: STAIR_STEP_HEIGHT,
            rise_over_run: STAIR_RISE_OVER_RUN,
            grounded: false,
            railings: false,
        });
        log::info!("simple stair-run {id} created");
        Some(self.rebuild_structures())
    }

    /// Grounded key (`F`): toggle `grounded` on the selected platform (and its
    /// connected stair-runs) or the selected stair-run (JS `toggle_grounded`).
    pub fn toggle_grounded_key(&mut self) -> Option<RegionMesh> {
        if self.platform_phase != Some(PlatformPhase::Selected) {
            return None;
        }
        if let Some(pid) = self.selected_platform {
            let g = {
                let p = self.platforms.iter_mut().find(|p| p.id == pid)?;
                p.grounded = !p.grounded;
                p.grounded
            };
            for r in self
                .stair_runs
                .iter_mut()
                .filter(|r| r.from_platform == Some(pid) || r.to_platform == Some(pid))
            {
                r.grounded = g;
            }
            log::info!("platform {pid} grounded={g}");
            return Some(self.rebuild_structures());
        }
        if let Some(rid) = self.selected_run {
            let r = self.stair_runs.iter_mut().find(|r| r.id == rid)?;
            r.grounded = !r.grounded;
            log::info!("stair-run {rid} grounded={}", r.grounded);
            return Some(self.rebuild_structures());
        }
        None
    }

    /// Railings key (`V`): toggle `railings` on the selected platform (and its
    /// connected stair-runs) or the selected stair-run (JS `toggle_railings`).
    pub fn toggle_railings_key(&mut self) -> Option<RegionMesh> {
        if self.platform_phase != Some(PlatformPhase::Selected) {
            return None;
        }
        if let Some(pid) = self.selected_platform {
            let on = {
                let p = self.platforms.iter_mut().find(|p| p.id == pid)?;
                p.railings = !p.railings;
                p.railings
            };
            for r in self
                .stair_runs
                .iter_mut()
                .filter(|r| r.from_platform == Some(pid) || r.to_platform == Some(pid))
            {
                r.railings = on;
            }
            log::info!("platform {pid} railings={on}");
            return Some(self.rebuild_structures());
        }
        if let Some(rid) = self.selected_run {
            let r = self.stair_runs.iter_mut().find(|r| r.id == rid)?;
            r.railings = !r.railings;
            log::info!("stair-run {rid} railings={}", r.railings);
            return Some(self.rebuild_structures());
        }
        None
    }

    /// Delete key (`X`/Delete): remove the selected platform (and every stair-run
    /// attached to it) or the selected stair-run (JS `delete`).
    pub fn delete_selected(&mut self) -> Option<RegionMesh> {
        if self.platform_phase != Some(PlatformPhase::Selected) {
            return None;
        }
        if let Some(pid) = self.selected_platform.take() {
            self.stair_runs
                .retain(|r| r.from_platform != Some(pid) && r.to_platform != Some(pid));
            self.platforms.retain(|p| p.id != pid);
            log::info!("platform {pid} deleted");
        } else if let Some(rid) = self.selected_run.take() {
            self.stair_runs.retain(|r| r.id != rid);
            log::info!("stair-run {rid} deleted");
        } else {
            return None;
        }
        self.platform_phase = Some(PlatformPhase::Idle);
        Some(self.rebuild_structures())
    }

    // ─── Structures geometry / nav (shared by the tool + the bake) ───────

    /// Every region brush (all ops), for grounded floor-lookup + railing wall
    /// probes (the helpers filter to subtracts themselves).
    fn all_region_brushes(&self) -> Vec<Brush> {
        self.regions
            .iter()
            .flat_map(|r| r.brushes.iter().copied())
            .collect()
    }

    fn platform_by_id(&self, id: u32) -> Option<Platform> {
        self.platforms.iter().find(|p| p.id == id).copied()
    }

    /// The two platforms a stair-run connects (each `None` for a ground end).
    fn run_platforms(&self, run: &StairRun) -> (Option<Platform>, Option<Platform>) {
        (
            run.from_platform.and_then(|id| self.platform_by_id(id)),
            run.to_platform.and_then(|id| self.platform_by_id(id)),
        )
    }

    /// The solid WT boxes of every platform + stair-run — the single source that
    /// drives render, collision, and nav (so they can't drift). Grounded elements
    /// resolve their underside via `findFloorYAt` over the region brushes.
    fn structure_solid_boxes(&self) -> Vec<[f32; 6]> {
        let brushes = self.all_region_brushes();
        let mut boxes = Vec::new();
        for p in &self.platforms {
            if let Some(b) = p.solid_box(&brushes) {
                boxes.push(b);
            }
        }
        for r in &self.stair_runs {
            let (fp, tp) = self.run_platforms(r);
            boxes.extend(structures::stair_run_boxes(r, fp.as_ref(), tp.as_ref(), &brushes));
        }
        boxes
    }

    /// Re-derive the structures mesh + collider from the current platforms /
    /// stair-runs and return it for GPU upload (under [`STRUCT_ID`]). **Collider +
    /// nav use the solid boxes** (`structure_solid_boxes`, matching JS nav
    /// semantics: grounded = solid to floor). **Render uses the simple floating
    /// shell** (top + skirt + grounded pillar legs; stair treads + stringers) plus
    /// the cosmetic railings — thin planes that never enter the collider.
    fn rebuild_structures(&mut self) -> RegionMesh {
        let boxes = self.structure_solid_boxes();
        self.physics
            .set_region_collider(STRUCT_ID, &boxes_mesh(&boxes));

        let brushes = self.all_region_brushes();
        let mut pos: Vec<f32> = Vec::new();
        let mut norm: Vec<f32> = Vec::new();
        let mut idx: Vec<u32> = Vec::new();
        for p in &self.platforms {
            structures::append_platform_mesh(p, &brushes, &mut pos, &mut norm, &mut idx);
            if p.railings {
                structures::append_platform_railings(
                    p,
                    &self.stair_runs,
                    &self.platforms,
                    &brushes,
                    &mut pos,
                    &mut norm,
                    &mut idx,
                );
            }
        }
        for r in &self.stair_runs {
            let (fp, tp) = self.run_platforms(r);
            structures::append_stair_mesh(r, fp.as_ref(), tp.as_ref(), &brushes, &mut pos, &mut norm, &mut idx);
            if r.railings {
                structures::append_stair_railings(
                    r,
                    fp.as_ref(),
                    tp.as_ref(),
                    &brushes,
                    &mut pos,
                    &mut norm,
                    &mut idx,
                );
            }
        }
        RegionMesh {
            id: STRUCT_ID,
            mesh: CpuMesh::from_csg(&pos, &norm, &idx),
        }
    }

    /// Raycast the crosshair against the collision world (regions + structures)
    /// and classify: WT hit point, dominant surface axis, and which platform /
    /// stair-run (if any) that point lies inside.
    fn pick_structure_hit(&mut self) -> Option<StructureHit> {
        let origin = self.camera.pos;
        let dir = self.camera.forward();
        let hit = self.physics.raycast(origin, dir, 100.0)?;
        let n = hit.normal.abs();
        let axis = if n.x >= n.y && n.x >= n.z {
            Axis::X
        } else if n.y >= n.z {
            Axis::Y
        } else {
            Axis::Z
        };
        let hit_wt = hit.point / WORLD_SCALE;

        let brushes = self.all_region_brushes();
        const EPS: f32 = 0.25;
        let platform = self
            .platforms
            .iter()
            .find(|p| {
                p.solid_box(&brushes)
                    .map(|b| in_box_eps(&b, hit_wt, EPS))
                    .unwrap_or(false)
            })
            .map(|p| p.id);
        let run = if platform.is_some() {
            None
        } else {
            self.stair_runs
                .iter()
                .find(|r| {
                    let (fp, tp) = self.run_platforms(r);
                    structures::stair_run_boxes(r, fp.as_ref(), tp.as_ref(), &brushes)
                        .iter()
                        .any(|b| in_box_eps(b, hit_wt, EPS))
                })
                .map(|r| r.id)
        };
        Some(StructureHit {
            hit_wt,
            axis,
            platform,
            run,
        })
    }

    // ─── Platform gizmo: move arrows + scale handles (JS `gizmo.js`) ─────

    /// Whether a gizmo drag is in progress (the app routes mouse motion to the
    /// drag instead of the camera while this is true).
    pub fn is_gizmo_dragging(&self) -> bool {
        self.gizmo_drag.is_some()
    }

    /// The gizmo's parts for the selected platform as `(handle, min_m, max_m, rgb)`
    /// AABBs in **meters** — shared by picking and the mesh build. Empty unless a
    /// platform is selected under the platform tool.
    fn gizmo_parts(&self) -> Vec<(GizmoHandle, Vec3, Vec3, [f32; 3])> {
        if self.platform_phase.is_none() {
            return Vec::new();
        }
        let Some(pid) = self.selected_platform else {
            return Vec::new();
        };
        let Some(p) = self.platform_by_id(pid) else {
            return Vec::new();
        };
        const RED: [f32; 3] = [0.93, 0.20, 0.20];
        const GREEN: [f32; 3] = [0.20, 0.93, 0.20];
        const BLUE: [f32; 3] = [0.20, 0.20, 0.93];
        let s = WORLD_SCALE;
        // WT AABB → meters AABB.
        let m = |x0: f32, y0: f32, z0: f32, x1: f32, y1: f32, z1: f32| {
            (Vec3::new(x0 * s, y0 * s, z0 * s), Vec3::new(x1 * s, y1 * s, z1 * s))
        };
        let (cx, cy, cz) = (p.center_x(), p.y, p.center_z());
        let sh = GIZMO_SHAFT_HALF;
        let al = GIZMO_ARROW_LENGTH;
        let hh = GIZMO_HANDLE_SIZE * 0.5;
        let hy = cy + hh; // scale cubes sit just above the top surface

        let mut parts = Vec::new();
        // Move arrows: thin boxes from centre outward along +axis.
        let (a, b) = m(cx, cy - sh, cz - sh, cx + al, cy + sh, cz + sh);
        parts.push((GizmoHandle::MoveX, a, b, RED));
        let (a, b) = m(cx - sh, cy, cz - sh, cx + sh, cy + al, cz + sh);
        parts.push((GizmoHandle::MoveY, a, b, GREEN));
        let (a, b) = m(cx - sh, cy - sh, cz, cx + sh, cy + sh, cz + al);
        parts.push((GizmoHandle::MoveZ, a, b, BLUE));
        // Scale handle cubes at edge midpoints.
        let mut cube = |gx: f32, gz: f32, handle: GizmoHandle, rgb: [f32; 3]| {
            let (a, b) = m(gx - hh, hy - hh, gz - hh, gx + hh, hy + hh, gz + hh);
            parts.push((handle, a, b, rgb));
        };
        cube(p.max_x(), cz, GizmoHandle::ScaleXMax, RED);
        cube(p.x, cz, GizmoHandle::ScaleXMin, RED);
        cube(cx, p.max_z(), GizmoHandle::ScaleZMax, BLUE);
        cube(cx, p.z, GizmoHandle::ScaleZMin, BLUE);
        parts
    }

    /// The gizmo handle under the crosshair, if any (ray vs each part's AABB).
    fn gizmo_pick(&self) -> Option<GizmoHandle> {
        let origin = self.camera.pos;
        let dir = self.camera.forward();
        let pad = Vec3::splat(0.02); // easier aim on the thin arrows
        let mut best: Option<(f32, GizmoHandle)> = None;
        for (h, min, max, _c) in self.gizmo_parts() {
            if let Some(t) = ray_aabb(origin, dir, min - pad, max + pad) {
                if best.map(|(bt, _)| t < bt).unwrap_or(true) {
                    best = Some((t, h));
                }
            }
        }
        best.map(|(_, h)| h)
    }

    /// Begin a gizmo drag on the given handle (records the platform's pre-drag
    /// transform for cancel).
    fn gizmo_start(&mut self, handle: GizmoHandle) {
        if let Some(pid) = self.selected_platform {
            if let Some(orig) = self.platform_by_id(pid) {
                self.gizmo_drag = Some(GizmoDrag {
                    handle,
                    platform_id: pid,
                    orig,
                    accumulated: 0.0,
                });
                log::info!("gizmo drag started ({handle:?})");
            }
        }
    }

    /// Feed a mouse delta into the active gizmo drag: project the handle's world
    /// axis onto the screen, accumulate distance-scaled motion, and apply whole-WT
    /// steps (JS `gizmo.processDrag`). Returns the rebuilt mesh when it changed.
    pub fn gizmo_drag_delta(&mut self, dx: f32, dy: f32) -> Option<RegionMesh> {
        let mut drag = self.gizmo_drag?;
        let p = self.platform_by_id(drag.platform_id)?;

        let world_axis = match drag.handle {
            GizmoHandle::MoveX | GizmoHandle::ScaleXMax => Vec3::X,
            GizmoHandle::ScaleXMin => -Vec3::X,
            GizmoHandle::MoveY => Vec3::Y,
            GizmoHandle::MoveZ | GizmoHandle::ScaleZMax => Vec3::Z,
            GizmoHandle::ScaleZMin => -Vec3::Z,
        };
        let fwd = self.camera.forward();
        let right = fwd.cross(Vec3::Y).normalize_or_zero();
        let up = right.cross(fwd).normalize_or_zero();
        let center_m = Vec3::new(p.center_x(), p.y, p.center_z()) * WORLD_SCALE;
        let dist = self.camera.pos.distance(center_m).max(0.5);
        let sens = dist * GIZMO_DRAG_SENSITIVITY;

        drag.accumulated += (dx * world_axis.dot(right) - dy * world_axis.dot(up)) * sens;
        let wt = drag.accumulated.round();
        drag.accumulated -= wt;
        self.gizmo_drag = Some(drag);
        if wt == 0.0 {
            return None;
        }

        let plat = self.platforms.iter_mut().find(|p| p.id == drag.platform_id)?;
        let mut changed = false;
        match drag.handle {
            GizmoHandle::MoveX => {
                plat.x += wt;
                changed = true;
            }
            GizmoHandle::MoveY => {
                plat.y += wt;
                changed = true;
            }
            GizmoHandle::MoveZ => {
                plat.z += wt;
                changed = true;
            }
            GizmoHandle::ScaleXMax => {
                let ns = (plat.size_x + wt).max(1.0);
                changed = ns != plat.size_x;
                plat.size_x = ns;
            }
            GizmoHandle::ScaleXMin => {
                let ns = (plat.size_x + wt).max(1.0);
                if ns != plat.size_x {
                    plat.x -= ns - plat.size_x;
                    plat.size_x = ns;
                    changed = true;
                }
            }
            GizmoHandle::ScaleZMax => {
                let ns = (plat.size_z + wt).max(1.0);
                changed = ns != plat.size_z;
                plat.size_z = ns;
            }
            GizmoHandle::ScaleZMin => {
                let ns = (plat.size_z + wt).max(1.0);
                if ns != plat.size_z {
                    plat.z -= ns - plat.size_z;
                    plat.size_z = ns;
                    changed = true;
                }
            }
        }
        if changed {
            Some(self.rebuild_structures())
        } else {
            None
        }
    }

    /// The gizmo overlay mesh (colored handles) for the selected platform, or
    /// `None`. The hovered handle (or the one being dragged) is brightened.
    pub fn gizmo_mesh(&self) -> Option<ColoredMesh> {
        if self.mode != Mode::Build {
            return None;
        }
        let parts = self.gizmo_parts();
        if parts.is_empty() {
            return None;
        }
        let active = self.gizmo_drag.map(|d| d.handle).or_else(|| self.gizmo_pick());
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        for (h, min, max, rgb) in parts {
            let col = if Some(h) == active {
                [(rgb[0] * 1.5).min(1.0), (rgb[1] * 1.5).min(1.0), (rgb[2] * 1.5).min(1.0)]
            } else {
                rgb
            };
            push_colored_box(&mut vertices, &mut indices, min, max, col);
        }
        Some(ColoredMesh { vertices, indices })
    }

    // ─── Stair tool (arrow keys) ─────────────────────────────────────────

    /// Whether a stair op is pending (the arrow-key counter is non-empty). The
    /// app reads this to route Esc (cancel the stair vs. release the cursor) and
    /// to show the step-count message.
    pub fn has_pending_stair(&self) -> bool {
        self.pending_stair.is_some()
    }

    /// The pending stair's step count + direction, for the app's status message.
    pub fn pending_stair(&self) -> Option<(u32, StairDir)> {
        self.pending_stair.map(|p| (p.step_count, p.direction))
    }

    /// Whether the bottom of the current selection sits on the face's floor
    /// (JS `wallSelectionTouchesFloor`). Stairs must start from the floor. Only
    /// meaningful for walls (axis ≠ Y). A full-V selection implicitly touches it.
    fn wall_selection_touches_floor(&mut self) -> bool {
        let Some(sel) = self.selected else { return false };
        if sel.axis == Axis::Y {
            return false;
        }
        let Some(info) = self.selected_face_info() else { return false };
        if self.sel_size_v <= 0.0 {
            return true; // full-V selection reaches the floor
        }
        match self.ensure_selection_bounds() {
            Some([_, _, v0, _]) => (v0 - info.v_min).abs() < 1e-3,
            None => false,
        }
    }

    /// Arrow-key handler (JS `pushSelectedFaceAsStairs`), BUILD only. `direction`
    /// grows the pending step counter; the opposite direction shrinks it (to zero
    /// = cancel). No geometry changes until [`confirm_stairs`](Self::confirm_stairs).
    /// Requires a selected wall face whose selection touches the floor. Returns
    /// whether a pending op is now active (for the app's message).
    pub fn push_stairs(&mut self, direction: StairDir) -> bool {
        if self.mode != Mode::Build {
            log::info!("push_stairs: not in BUILD mode");
            return false;
        }
        // Auto-pick the crosshair face if nothing's selected yet (matches how
        // push/pull behave — no separate click required).
        let Some(sel) = self.resolve_selection() else {
            log::info!("push_stairs: no face under the crosshair to anchor to");
            return false;
        };
        if sel.axis == Axis::Y {
            log::info!("push_stairs: selected a floor/ceiling (axis Y) — stairs need a wall");
            return false; // floors/ceilings aren't stair anchors
        }
        let Some(info) = self.selected_face_info() else {
            log::info!("push_stairs: selected_face_info returned None");
            return false;
        };
        if !self.wall_selection_touches_floor() {
            log::info!("push_stairs: selection does not touch the floor");
            return false;
        }
        let bounds = match self.ensure_selection_bounds() {
            Some(b) => b,
            None => return false,
        };
        let [u0, u1, _v0, v1] = bounds;
        let floor = info.v_min;
        let ceil = if self.sel_size_v <= 0.0 { info.v_max } else { v1 };
        let face_pos = info.position;

        // Same-anchor test (JS): same face + same sub-rect → adjust the existing
        // counter; otherwise start a fresh 1-step op.
        let same_anchor = self.pending_stair.map(|op| {
            op.region_id == sel.region_id
                && op.axis == sel.axis
                && op.side == sel.side
                && (op.face_pos - face_pos).abs() < 1e-3
                && (op.u0 - u0).abs() < 1e-3
                && (op.u1 - u1).abs() < 1e-3
        }).unwrap_or(false);

        let new_count = match self.pending_stair {
            Some(op) if same_anchor && op.direction == direction => op.step_count + 1,
            Some(op) if same_anchor => {
                // Opposite arrow shrinks the same op; hitting zero cancels it.
                if op.step_count <= 1 {
                    self.pending_stair = None;
                    return false;
                }
                op.step_count - 1
            }
            _ => 1,
        };

        self.pending_stair = Some(PendingStair {
            direction,
            step_count: new_count,
            region_id: sel.region_id,
            axis: sel.axis,
            side: sel.side,
            face_pos,
            u_axis: info.u_axis,
            u0,
            u1,
            floor,
            ceil,
        });
        true
    }

    /// Cancel a pending stair op (Esc), discarding the counter. No geometry was
    /// created yet, so nothing to undo.
    pub fn cancel_stairs(&mut self) {
        self.pending_stair = None;
    }

    /// The [`StairDesc`] a pending op would confirm into (also used for the ghost).
    fn pending_desc(&self) -> Option<StairDesc> {
        let op = self.pending_stair?;
        Some(StairDesc {
            direction: op.direction,
            step_count: op.step_count,
            axis: op.axis,
            side: op.side,
            face_pos: op.face_pos,
            u_axis: op.u_axis,
            u0: op.u0,
            u1: op.u1,
            floor: op.floor,
            ceil: op.ceil,
        })
    }

    /// A translucent ghost of the pending stair's steps (meters), for the highlight
    /// pipeline — immediate feedback as the arrow keys grow the op. `None` when no
    /// stair is pending.
    pub fn stair_preview_mesh(&self) -> Option<CpuMesh> {
        self.pending_desc().map(|d| d.mesh())
    }

    /// Confirm the pending stair (Enter, JS `confirmStairOp`): create the two
    /// `subtract` void brushes (stairwell + far corridor), register a [`StairDesc`]
    /// on the region (whose treads [`Region::evaluate`] folds into the mesh), and
    /// re-evaluate. Returns the changed region's mesh, or `None` if nothing pending.
    pub fn confirm_stairs(&mut self) -> Option<RegionMesh> {
        if self.pending_stair.is_none() {
            log::info!("confirm_stairs: nothing pending (press ↑/↓ on a floor-touching wall first)");
            return None;
        }
        let desc = self.pending_desc()?;
        let op = self.pending_stair.take()?;
        let sc = op.step_count as f32;
        let dir = if op.side == Side::Max { 1.0 } else { -1.0 };

        // Brush 1: the main stairwell, flush with the wall face.
        let (b1_lo, b1_hi) = if dir > 0.0 {
            (op.face_pos, op.face_pos + sc)
        } else {
            (op.face_pos - sc, op.face_pos)
        };
        let (b1_ymin, b1_ymax) = match op.direction {
            StairDir::Down => (op.floor - sc, op.ceil),
            StairDir::Up => (op.floor, op.ceil + sc),
        };
        let brush1 = make_stair_void(
            self.next_brush_id, op.axis, b1_lo, b1_hi, b1_ymin, b1_ymax, op.u_axis, op.u0, op.u1,
        );
        self.next_brush_id += 1;

        // Brush 2: the destination corridor, 1 WT deep past the stairwell.
        let (b2_lo, b2_hi) = if dir > 0.0 {
            (op.face_pos + sc, op.face_pos + sc + 1.0)
        } else {
            (op.face_pos - sc - 1.0, op.face_pos - sc)
        };
        let (b2_ymin, b2_ymax) = match op.direction {
            StairDir::Down => (op.floor - sc, op.ceil - sc),
            StairDir::Up => (op.floor + sc, op.ceil + sc),
        };
        let brush2 = make_stair_void(
            self.next_brush_id, op.axis, b2_lo, b2_hi, b2_ymin, b2_ymax, op.u_axis, op.u0, op.u1,
        );
        self.next_brush_id += 1;

        let region = self.regions.iter_mut().find(|r| r.id == op.region_id)?;
        region.brushes.push(brush1);
        region.brushes.push(brush2);
        region.stairs.push(desc);
        log::info!(
            "stairs confirmed: {} step(s) {:?} in region {}",
            op.step_count, op.direction, op.region_id
        );
        self.rebuild_region(op.region_id)
    }
}

/// Whether two selections point at the same brush face.
fn same_face(a: Option<Selection>, b: Option<Selection>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            a.region_id == b.region_id && a.brush_id == b.brush_id && a.axis == b.axis && a.side == b.side
        }
        _ => false,
    }
}

/// The opposite side of an axis.
fn flip(side: Side) -> Side {
    match side {
        Side::Min => Side::Max,
        Side::Max => Side::Min,
    }
}

/// Build a subtract brush for a wall carve from face-relative parameters: `a`/`da`
/// are the min corner + size along the face-normal `axis`; `(u0,du)` and `(v0,dv)`
/// are the extents along the two in-plane axes. Mirrors the axis dispatch in JS
/// `confirmHolePlacement`.
#[allow(clippy::too_many_arguments)]
fn make_wall_brush(
    id: u32,
    axis: Axis,
    a: f32,
    da: f32,
    u_axis: Axis,
    u0: f32,
    du: f32,
    v_axis: Axis,
    v0: f32,
    dv: f32,
) -> Brush {
    let mut p = [0.0f32; 3];
    let mut s = [0.0f32; 3];
    p[axis_index(axis)] = a;
    s[axis_index(axis)] = da;
    p[axis_index(u_axis)] = u0;
    s[axis_index(u_axis)] = du;
    p[axis_index(v_axis)] = v0;
    s[axis_index(v_axis)] = dv;
    Brush::new(id, Op::Subtract, p[0], p[1], p[2], s[0], s[1], s[2])
}

/// Build a combined mesh (meters) of one or more WT AABB boxes `[x,y,z,w,h,d]`,
/// for the pillar/brace ghost preview (drawn via the translucent highlight
/// pipeline). Uses the CSG box helper so winding matches the region meshes.
fn boxes_mesh(boxes: &[[f32; 6]]) -> CpuMesh {
    let mut positions: Vec<f32> = Vec::new();
    let mut normals: Vec<f32> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    for b in boxes {
        let c = [
            (b[0] + b[3] * 0.5) * WORLD_SCALE,
            (b[1] + b[4] * 0.5) * WORLD_SCALE,
            (b[2] + b[5] * 0.5) * WORLD_SCALE,
        ];
        let half = [
            b[3] * 0.5 * WORLD_SCALE,
            b[4] * 0.5 * WORLD_SCALE,
            b[5] * 0.5 * WORLD_SCALE,
        ];
        let polys = csg::box_polygons(c, half);
        let (p, n, i) = csg::polygons_to_mesh(&polys);
        let base = (positions.len() / 3) as u32;
        positions.extend_from_slice(&p);
        normals.extend_from_slice(&n);
        indices.extend(i.iter().map(|idx| idx + base));
    }
    CpuMesh::from_csg(&positions, &normals, &indices)
}

/// Append a solid colored box (meters AABB `min`..`max`, flat `rgb`) to a gizmo
/// mesh buffer. Uses the CSG box helper for winding-consistent faces.
fn push_colored_box(verts: &mut Vec<ColorVertex>, idx: &mut Vec<u32>, min: Vec3, max: Vec3, rgb: [f32; 3]) {
    let center = ((min + max) * 0.5).to_array();
    let half = ((max - min) * 0.5).to_array();
    let polys = csg::box_polygons(center, half);
    let (p, _n, i) = csg::polygons_to_mesh(&polys);
    let base = verts.len() as u32;
    for c in p.chunks_exact(3) {
        verts.push(ColorVertex {
            pos: [c[0], c[1], c[2]],
            color: rgb,
        });
    }
    idx.extend(i.iter().map(|k| k + base));
}

/// Ray vs AABB (slab method), meters. Returns the near hit distance (≥0) or
/// `None`. `dir` need not be normalized; near-zero components are nudged so the
/// reciprocal stays finite.
fn ray_aabb(origin: Vec3, dir: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let safe = |d: f32| if d.abs() < 1e-6 { 1e-6 } else { d };
    let inv = Vec3::new(1.0 / safe(dir.x), 1.0 / safe(dir.y), 1.0 / safe(dir.z));
    let t0 = (min - origin) * inv;
    let t1 = (max - origin) * inv;
    let tmin = t0.min(t1).max_element();
    let tmax = t0.max(t1).min_element();
    if tmax >= tmin.max(0.0) {
        Some(tmin.max(0.0))
    } else {
        None
    }
}

/// Whether a WT point lies within a WT AABB `[x,y,z,w,h,d]`, with tolerance
/// `eps` — used to classify which platform / stair-run the crosshair hit.
fn in_box_eps(b: &[f32; 6], p: Vec3, eps: f32) -> bool {
    p.x >= b[0] - eps
        && p.x <= b[0] + b[3] + eps
        && p.y >= b[1] - eps
        && p.y <= b[1] + b[4] + eps
        && p.z >= b[2] - eps
        && p.z <= b[2] + b[5] + eps
}

/// Build a stair void `subtract` brush (JS `csgActions.makeBrush`): `lo`/`hi` are
/// the span along the wall-normal `axis`, `y_min`/`y_max` the vertical extent, and
/// `(u0, u1)` the horizontal span along the in-plane `u_axis`. The vertical axis
/// is always world-up Y.
#[allow(clippy::too_many_arguments)]
fn make_stair_void(
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
    make_wall_brush(
        id, axis, lo, hi - lo, u_axis, u0, u1 - u0, Axis::Y, y_min, y_max - y_min,
    )
}

#[inline]
fn axis_index(axis: Axis) -> usize {
    match axis {
        Axis::X => 0,
        Axis::Y => 1,
        Axis::Z => 2,
    }
}

#[inline]
fn axis_normal(axis: Axis) -> [f32; 3] {
    match axis {
        Axis::X => [1.0, 0.0, 0.0],
        Axis::Y => [0.0, 1.0, 0.0],
        Axis::Z => [0.0, 0.0, 1.0],
    }
}

#[inline]
fn axis_val(v: Vec3, axis: Axis) -> f32 {
    match axis {
        Axis::X => v.x,
        Axis::Y => v.y,
        Axis::Z => v.z,
    }
}

/// The two axes orthogonal to `axis` as (U, V), matching the JS oracle's
/// `getFaceUVInfo` convention. Crucially, for both vertical walls (X- and
/// Z-facing) **V is the world-up axis Y**, so a door/opening keeps its width
/// horizontal and height vertical regardless of which wall it's cut into (fixes
/// the 90°-rotated door). Y-facing faces (floor/ceiling) use (X, Z).
#[inline]
fn others(axis: Axis) -> (Axis, Axis) {
    match axis {
        Axis::X => (Axis::Z, Axis::Y),
        Axis::Y => (Axis::X, Axis::Z),
        Axis::Z => (Axis::X, Axis::Y),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end authoring loop with no GPU: build the room + collider, aim the
    /// crosshair at the −Z wall, push it, and confirm the whole pipeline fires
    /// (raycast pick → brush resize → re-evaluate → collider rebuilt → new mesh).
    /// This is the Phase 1 risk-burndown proof.
    #[test]
    fn push_carves_the_wall_the_crosshair_hits() {
        let mut world = World::new();
        let initial = world.initial_meshes();
        assert_eq!(initial.len(), 1, "one room region");
        let tris_before = initial[0].mesh.indices.len();
        assert!(tris_before > 0, "room built geometry");

        // Camera spawns at (3,1.5,3) m looking −Z → crosshair hits the z=0 wall.
        let rm = world.push(PUSH_PULL_STEP).expect("crosshair should hit a wall");
        assert_eq!(rm.id, 0);
        assert!(!rm.mesh.indices.is_empty(), "carved room still has geometry");

        // Pulling the same wall back should also resolve a hit (loop is stable).
        assert!(world.pull(PUSH_PULL_STEP).is_some(), "pull resolves a face too");
    }

    /// Aiming at empty space (no collider along the ray) picks nothing — push is
    /// a safe no-op rather than a panic.
    #[test]
    fn looking_at_nothing_is_a_safe_noop() {
        let mut world = World::new();
        world.initial_meshes();
        // Fly far outside the room and look away from it.
        world.camera.pos = Vec3::new(1000.0, 1000.0, 1000.0);
        world.camera.yaw = 0.0;
        assert!(world.push(PUSH_PULL_STEP).is_none());
    }

    /// Entering HUNT drops a capsule that gravity settles onto the room floor
    /// (y≈0, the cavity's bottom) — it neither sinks through nor floats.
    #[test]
    fn character_settles_on_the_floor_under_gravity() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode(); // BUILD → HUNT, spawns on the floor beneath the cam
        assert_eq!(world.mode, Mode::Hunt);
        let input = InputState::default(); // no keys, not pointer-locked

        for _ in 0..240 {
            // 2 s at 120 Hz
            world.fixed_step(1.0 / 120.0, &input);
        }
        let feet = world.player_pos().expect("player exists in HUNT");
        assert!(
            feet.y.abs() < 0.05,
            "feet should rest on the y≈0 floor, got {}",
            feet.y
        );
    }

    /// Phase 3 milestone: on HUNT the nav grid bakes, a hunter spawns across the
    /// room, and it pathfinds to a stationary player and catches them.
    #[test]
    fn hunter_pathfinds_to_and_catches_the_player() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode(); // bakes nav + spawns hunter far from the player
        assert!(world.player_pos().is_some(), "player spawned");
        assert!(world.enemy.is_some(), "hunter spawned");

        let input = InputState::default(); // player stands still
        let mut caught = false;
        for _ in 0..1800 {
            // up to 15 s at 120 Hz
            world.fixed_step(1.0 / 120.0, &input);
            if world.is_caught() {
                caught = true;
                break;
            }
        }
        assert!(caught, "hunter should reach and catch the stationary player");
    }

    /// The door tool: `B` arms a preview on the wall, a left-click cuts a
    /// `door`-marked opening. No cut happens just from arming.
    #[test]
    fn door_tool_arms_with_b_and_cuts_on_click() {
        let mut world = World::new(); // camera at (3,1.5,3) m looking −Z at the z=0 wall
        world.initial_meshes();
        assert!(!world.is_door_arming());

        // B arms (no geometry change).
        assert!(world.door_tool_key().is_none(), "B does not cut");
        assert!(world.is_door_arming(), "B arms the preview");
        assert!(world.update_door_preview().is_some(), "ghost previews on the wall");
        assert!(!world.regions[0].brushes.iter().any(|b| b.door), "no door yet");

        // Left-click (confirm_door) cuts.
        assert!(world.confirm_door().is_some(), "click cuts the door");
        assert!(!world.is_door_arming(), "cutting disarms");
        assert!(
            world.regions[0].brushes.iter().any(|b| b.door),
            "a door-marked doorframe brush was created"
        );
    }

    /// Pressing `B` while the tool is armed toggles it back off, cutting nothing.
    #[test]
    fn pressing_b_again_deselects_the_door_tool() {
        let mut world = World::new();
        world.initial_meshes();
        world.door_tool_key(); // arm
        assert!(world.is_door_arming());
        world.door_tool_key(); // B again → deselect
        assert!(!world.is_door_arming(), "second B turns the tool off");
        assert!(!world.regions[0].brushes.iter().any(|b| b.door), "toggling off cuts nothing");
    }

    /// A door cut into an X-facing wall stays upright (height along Y, width
    /// along Z) — the regression for the 90°-rotated door.
    #[test]
    fn door_on_an_x_wall_stays_upright() {
        let mut world = World::new();
        world.initial_meshes();
        world.camera.yaw = std::f32::consts::FRAC_PI_2; // face the −X wall
        world.door_tool_key(); // arm
        assert!(world.update_door_preview().is_some(), "previews on the −X wall");
        assert!(world.confirm_door().is_some(), "cuts the door");

        let frame = world
            .regions[0]
            .brushes
            .iter()
            .find(|b| b.door)
            .expect("doorframe exists");
        assert_eq!(frame.h, DOOR_HEIGHT, "height runs vertically (Y)");
        assert_eq!(frame.d, DOOR_WIDTH, "width runs horizontally (Z)");
        assert_eq!(frame.w, WALL_THICKNESS, "1 WT thick through the wall (X)");
    }

    /// Cancelling an armed door leaves the geometry untouched.
    #[test]
    fn cancel_door_leaves_no_opening() {
        let mut world = World::new();
        world.initial_meshes();
        world.door_tool_key(); // arm
        assert!(world.is_door_arming());
        world.cancel_door();
        assert!(!world.is_door_arming());
        assert!(!world.regions[0].brushes.iter().any(|b| b.door), "cancel cuts nothing");
    }

    /// Scroll sizing clamps to [1, faceSize] and flips full ↔ sub-face.
    #[test]
    fn scroll_sizes_and_clamps_the_selection() {
        let mut world = World::new();
        world.initial_meshes();
        assert!(world.select_at_crosshair(), "picks the −Z wall");
        assert!(world.is_full_face(), "a fresh selection is full-face");

        // One scroll-down shrinks below full → sub-face.
        world.adjust_selection_size(-1.0, 0.0);
        assert!(!world.is_full_face(), "scrolling in makes it a sub-face");

        // Shrinking hard clamps at 1 (never full).
        for _ in 0..40 {
            world.adjust_selection_size(-1.0, 0.0);
        }
        assert!(!world.is_full_face());

        // Growing hard clamps back to the full face size.
        for _ in 0..40 {
            world.adjust_selection_size(1.0, 0.0);
        }
        assert!(world.is_full_face(), "grown back to full clamps at faceSize");
    }

    /// A sub-face push spawns a subtract brush sized to the sub-rect (carves a
    /// niche) rather than moving the whole wall.
    #[test]
    fn sub_face_push_carves_a_sized_brush() {
        let mut world = World::new();
        world.initial_meshes();
        world.select_at_crosshair(); // −Z wall: axis Z, side Min; u=X(24), v=Y(16)
        world.adjust_selection_size(-20.0, 0.0); // sel_size_u: 24 → 4
        world.adjust_selection_size(0.0, -10.0); // sel_size_v: 16 → 6
        assert!(!world.is_full_face());

        let before = world.regions[0].brushes.len();
        assert!(world.push(4.0).is_some(), "sub-face push rebuilds the region");
        assert_eq!(world.regions[0].brushes.len(), before + 1, "spawned one brush");

        let sub = world.regions[0].brushes.last().unwrap();
        assert_eq!(sub.op, Op::Subtract);
        assert_eq!(sub.w, 4.0, "width = sub-rect U");
        assert_eq!(sub.h, 6.0, "height = sub-rect V");
        assert_eq!(sub.d, 4.0, "depth = push step along the normal");
        // The original room brush is untouched (whole wall didn't move).
        let room = world.regions[0].brushes.iter().find(|b| b.id == 1).unwrap();
        assert_eq!(room.d, 24.0, "room brush unchanged by a sub-face carve");
    }

    /// A full-face push (no scroll) still resizes the wall brush in place — the
    /// Phase 1 behavior, unregressed.
    #[test]
    fn full_face_push_still_moves_the_whole_wall() {
        let mut world = World::new();
        world.initial_meshes();
        world.select_at_crosshair();
        assert!(world.is_full_face());
        let before = world.regions[0].brushes.len();
        world.push(4.0);
        assert_eq!(world.regions[0].brushes.len(), before, "no new brush");
        let room = world.regions[0].brushes.iter().find(|b| b.id == 1).unwrap();
        assert_eq!(room.d, 28.0, "whole −Z wall pushed out by the step");
    }

    /// Repeated sub-face pushes deepen the same carve rather than stacking brushes.
    #[test]
    fn repeat_sub_face_push_grows_the_same_brush() {
        let mut world = World::new();
        world.initial_meshes();
        world.select_at_crosshair();
        world.adjust_selection_size(-20.0, 0.0);
        world.adjust_selection_size(0.0, -10.0);

        world.push(4.0); // spawn the sub-face carve
        let n1 = world.regions[0].brushes.len();
        let d1 = world.regions[0].brushes.last().unwrap().d;

        world.push(4.0); // deepen it
        let n2 = world.regions[0].brushes.len();
        let d2 = world.regions[0].brushes.last().unwrap().d;

        assert_eq!(n2, n1, "repeat push grows the same brush, no new one");
        assert!(d2 > d1, "the carve deepened: {d1} → {d2}");
    }

    /// Two rooms in one region, joined ONLY through a door-marked opening in the
    /// dividing wall. Room A: x∈[0,10); Room B: x∈[11,21); the wall at x∈[10,11)
    /// is solid except where the door carves a floor-level opening. The player
    /// (camera) is placed in Room B, aligned with the door.
    fn two_rooms_joined_by_a_door() -> World {
        let mut world = World::new();
        let region = &mut world.regions[0];
        region.brushes.clear();
        region
            .brushes
            .push(Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 10.0, 16.0, 10.0));
        region
            .brushes
            .push(Brush::new(2, Op::Subtract, 11.0, 0.0, 0.0, 10.0, 16.0, 10.0));
        // Door through the dividing wall (x∈[10,11)), floor-level, z∈[3,6).
        let mut door = Brush::new(3, Op::Subtract, 10.0, 0.0, 3.0, 1.0, 7.0, 3.0);
        door.door = true;
        region.brushes.push(door);
        world.next_brush_id = 4;
        // Player camera in Room B (meters), aligned with the door opening in z.
        world.camera.pos = Vec3::new(4.0, 1.6, 1.125);
        world
    }

    /// The intact door panel blocks the player like a wall; removing it (the
    /// breach) makes the opening passable — collision reacts with no re-bake.
    #[test]
    fn intact_door_panel_blocks_the_player_until_breached() {
        let mut world = two_rooms_joined_by_a_door();
        world.initial_meshes();
        world.toggle_mode(); // spawn player in B, hunter in A, arm the door
        assert_eq!(world.doors.len(), 1, "one door armed");
        assert_eq!(world.physics.door_collider_count(), 1, "panel collider present");

        // Face −X (yaw π/2) and walk toward Room A through the door opening.
        world.character.as_mut().unwrap().yaw = std::f32::consts::FRAC_PI_2;
        let mut input = InputState::default();
        input.pointer_locked = true;
        input.press(winit::keyboard::KeyCode::KeyW);

        // Short window: the door stays intact (the hunter can't breach this fast).
        for _ in 0..180 {
            world.fixed_step(1.0 / 120.0, &input);
        }
        let feet = world.player_pos().unwrap();
        // Door plane is x∈[2.5,2.75] m; capsule radius 0.25 m → blocked above ~3.0.
        assert!(
            feet.x > 2.9,
            "panel should block the player at the door, got x={}",
            feet.x
        );

        // Breach the panel directly (isolate collision from the AI): the opening
        // becomes passable and the player crosses into Room A.
        let panel = world.doors[0].panel;
        world.physics.remove_door_collider(panel);
        assert_eq!(world.physics.door_collider_count(), 0, "panel removed");
        for _ in 0..300 {
            world.fixed_step(1.0 / 120.0, &input);
        }
        let feet = world.player_pos().unwrap();
        assert!(
            feet.x < 2.5,
            "player should cross the breached opening into Room A, got x={}",
            feet.x
        );
    }

    /// Phase 4 thesis: a hunter walled off from the player breaches the only door
    /// on its route, then reaches the player over the SAME baked grid. The breach
    /// flips a live nav flag + drops one collider — no re-voxelization (nothing in
    /// `fixed_step` re-bakes; `nav::bake` runs only at the BUILD→HUNT toggle).
    #[test]
    fn hunter_breaches_the_only_door_to_reach_a_walled_off_player() {
        let mut world = two_rooms_joined_by_a_door();
        world.initial_meshes();
        world.toggle_mode();
        assert!(world.enemy.is_some(), "hunter spawned");
        assert_eq!(world.nav.as_ref().unwrap().door_count(), 1);
        assert!(!world.nav.as_ref().unwrap().door_broken(0), "door starts intact");
        assert_eq!(world.physics.door_collider_count(), 1);

        let input = InputState::default(); // player stands still in Room B
        let mut caught = false;
        for _ in 0..2400 {
            // up to 20 s at 120 Hz (travel + 2.5 s breach + travel)
            world.fixed_step(1.0 / 120.0, &input);
            if world.is_caught() {
                caught = true;
                break;
            }
        }
        assert!(caught, "hunter should breach the door and catch the player");
        assert!(world.nav.as_ref().unwrap().door_broken(0), "nav flag flipped by breach");
        assert!(world.doors[0].broken, "world door marked broken");
        assert_eq!(world.physics.door_collider_count(), 0, "panel collider removed by breach");
    }

    // ─── Hole tool ─────────────────────────────────────────────────────────

    /// The hole tool arms with `H`, scroll sizes it, and a click cuts an opening
    /// that is NOT door-marked (holes aren't breakable). Distinct from the door.
    #[test]
    fn hole_tool_cuts_a_non_door_opening() {
        let mut world = World::new(); // camera looks −Z at the z=0 wall
        world.initial_meshes();
        assert!(!world.is_opening_arming());

        world.hole_tool_key(); // arm
        assert!(world.is_opening_arming() && world.is_hole_arming(), "hole tool armed");
        assert!(!world.is_door_arming(), "not the door tool");
        assert!(world.update_opening_preview().is_some(), "ghost previews on the wall");

        let before = world.regions[0].brushes.len();
        assert!(world.confirm_opening().is_some(), "click cuts the hole");
        assert!(!world.is_opening_arming(), "cutting disarms");
        // Frame + protoroom subtracts added, and NO brush is door-marked.
        assert_eq!(world.regions[0].brushes.len(), before + 2, "frame + protoroom");
        assert!(
            !world.regions[0].brushes.iter().any(|b| b.door),
            "a hole is not breakable (no door-marked brush)"
        );
    }

    /// A hole can be cut into a floor (axis Y) — doors can't. Scroll grows the
    /// opening, and the cut carves the floor face.
    #[test]
    fn hole_can_be_cut_into_the_floor() {
        let mut world = World::new();
        world.initial_meshes();
        world.camera.pitch = -1.4; // look almost straight down at the floor
        world.hole_tool_key(); // arm hole
        world.adjust_opening_size(2.0, 2.0); // grow to 5×5
        let p = world.resolve_opening_placement().expect("floor is a valid hole face");
        assert_eq!(p.axis, Axis::Y, "the crosshair resolved the floor");
        let before = world.regions[0].brushes.len();
        assert!(world.confirm_opening().is_some(), "cuts a floor hole");
        assert_eq!(world.regions[0].brushes.len(), before + 2);
    }

    /// The door tool still rejects the floor (walls only) — the generalization
    /// didn't loosen the door's constraint.
    #[test]
    fn door_tool_still_rejects_the_floor() {
        let mut world = World::new();
        world.initial_meshes();
        world.camera.pitch = -1.4; // look down at the floor
        world.door_tool_key(); // arm door
        assert!(world.update_opening_preview().is_none(), "no door ghost on the floor");
        assert!(world.confirm_opening().is_none(), "no door cut into the floor");
        assert!(!world.regions[0].brushes.iter().any(|b| b.door));
    }

    // ─── Pillars & braces ───────────────────────────────────────────────────

    /// The pillar tool places one additive floor→ceiling column when aimed at the
    /// floor, and rejects a wall.
    #[test]
    fn pillar_places_a_column_on_the_floor() {
        let mut world = World::new();
        world.initial_meshes();

        // Aimed at the −Z wall (default view) → pillar rejects it.
        world.pillar_tool_key();
        assert!(world.is_placing());
        assert!(world.update_place_preview().is_none(), "no pillar ghost on a wall");
        assert!(world.confirm_place().is_none(), "no pillar placed on a wall");

        // Look down at the floor → a ghost appears and a click adds one Add brush.
        world.camera.pitch = -1.4;
        assert!(world.update_place_preview().is_some(), "pillar ghost on the floor");
        let before = world.regions[0].brushes.len();
        assert!(world.confirm_place().is_some(), "pillar placed");
        assert_eq!(world.regions[0].brushes.len(), before + 1, "one additive column");
        let col = world.regions[0].brushes.last().unwrap();
        assert_eq!(col.op, Op::Add);
        assert_eq!(col.w, PILLAR_SIZE);
        assert_eq!(col.d, PILLAR_SIZE);
        assert!(!world.is_placing(), "placing disarms after a click");
    }

    /// Scroll resizes the pillar footprint before placement.
    #[test]
    fn scroll_resizes_the_pillar() {
        let mut world = World::new();
        world.initial_meshes();
        world.camera.pitch = -1.4;
        world.pillar_tool_key();
        world.adjust_place_size(2.0, 0.0); // 2 → 4
        world.confirm_place().unwrap();
        let col = world.regions[0].brushes.last().unwrap();
        assert_eq!(col.w, 4.0, "pillar grew to the scrolled size");
    }

    /// The brace tool places three additive brushes (arch) when aimed at a wall,
    /// and rejects the floor.
    #[test]
    fn brace_places_a_three_brush_arch_on_a_wall() {
        let mut world = World::new();
        world.initial_meshes();

        // Floor → brace rejects it.
        world.camera.pitch = -1.4;
        world.brace_tool_key();
        assert!(world.update_place_preview().is_none(), "no brace ghost on the floor");
        assert!(world.confirm_place().is_none(), "no brace on the floor");

        // −Z wall → three additive brushes.
        world.camera.pitch = 0.0;
        assert!(world.update_place_preview().is_some(), "brace ghost on the wall");
        let before = world.regions[0].brushes.len();
        assert!(world.confirm_place().is_some(), "brace placed");
        assert_eq!(world.regions[0].brushes.len(), before + 3, "wall + ceiling + wall");
        assert!(
            world.regions[0].brushes.iter().rev().take(3).all(|b| b.op == Op::Add),
            "all three brace brushes are additive"
        );
    }

    /// Arming a placement tool cancels an armed opening tool (mutually exclusive).
    #[test]
    fn tools_are_mutually_exclusive() {
        let mut world = World::new();
        world.initial_meshes();
        world.hole_tool_key();
        assert!(world.is_opening_arming());
        world.pillar_tool_key();
        assert!(world.is_placing(), "pillar armed");
        assert!(!world.is_opening_arming(), "arming the pillar cancelled the hole");
    }

    // ─── Stairs ──────────────────────────────────────────────────────────

    /// Stairs require the selection to touch the floor: a sub-face selection
    /// scrolled up off the floor rejects the arrow key (no pending op forms).
    #[test]
    fn stairs_require_the_selection_to_touch_the_floor() {
        let mut world = World::new();
        world.initial_meshes();
        world.select_at_crosshair(); // −Z wall
        // Shrink V to a small band and slide it up off the floor via the preview
        // (which centers the rect on the crosshair). Aim high so it clears vMin.
        world.adjust_selection_size(0.0, -12.0); // sel_size_v: 16 → 4
        world.camera.pitch = 0.5; // look up so the centered rect sits above the floor
        world.update_selection_preview();
        assert!(!world.wall_selection_touches_floor(), "raised band is off the floor");
        assert!(!world.push_stairs(StairDir::Down), "off-floor selection rejects stairs");
        assert!(!world.has_pending_stair());
    }

    /// Arrow keys accumulate a pending step counter; the opposite arrow shrinks
    /// the same op, and confirming creates two void brushes + one descriptor with
    /// the tread mesh folded into the region (more triangles than before).
    #[test]
    fn confirm_stairs_creates_voids_treads_and_descriptor() {
        let mut world = World::new();
        let initial = world.initial_meshes();
        let tris_before = initial[0].mesh.indices.len();

        world.select_at_crosshair(); // full-face −Z wall, touches floor
        assert!(world.push_stairs(StairDir::Down), "first down grows to 1 step");
        world.push_stairs(StairDir::Down); // 2
        world.push_stairs(StairDir::Down); // 3
        world.push_stairs(StairDir::Up); // opposite shrinks → 2
        assert_eq!(world.pending_stair().unwrap().0, 2, "opposite arrow shrank the op");

        let brushes_before = world.regions[0].brushes.len();
        let rm = world.confirm_stairs().expect("confirm rebuilds the region");
        assert!(!world.has_pending_stair(), "confirm clears the pending op");
        assert_eq!(
            world.regions[0].brushes.len(),
            brushes_before + 2,
            "two void brushes (stairwell + corridor)"
        );
        assert_eq!(world.regions[0].stairs.len(), 1, "one stair descriptor");
        assert!(
            rm.mesh.indices.len() > tris_before,
            "tread geometry folded into the region mesh ({} → {})",
            tris_before,
            rm.mesh.indices.len()
        );
    }

    /// Reproduce the exact live-app ordering: click to select, then the per-frame
    /// selection preview runs (as it does every RedrawRequested), THEN the arrow
    /// keys + Enter. This guards against the preview loop clobbering the selection
    /// or pending-stair state (which the other tests don't exercise).
    #[test]
    fn preview_loop_between_select_and_confirm_does_not_break_stairs() {
        let mut world = World::new();
        world.initial_meshes();
        assert!(world.select_at_crosshair(), "click selects the −Z wall");

        // Simulate several render frames: preview updates before the user acts.
        for _ in 0..5 {
            world.update_selection_preview();
        }
        assert!(
            world.push_stairs(StairDir::Down),
            "arrow-down must still form a pending op after the preview ran"
        );
        // More frames between key presses.
        world.update_selection_preview();
        world.push_stairs(StairDir::Down);
        world.update_selection_preview();

        assert_eq!(world.pending_stair().unwrap().0, 2, "two steps pending");
        assert!(world.confirm_stairs().is_some(), "Enter confirms after previews");
        assert_eq!(world.regions[0].stairs.len(), 1);
    }

    /// Down-stairs are walkable by the hunter: the nav bake sees the treads (via
    /// the solid-box extras) and finds a path from the room floor down into the
    /// lower corridor. Also proves standable cells exist below the original floor.
    #[test]
    fn down_stairs_are_walkable_by_nav() {
        let mut world = World::new();
        world.initial_meshes();
        world.select_at_crosshair(); // −Z wall
        for _ in 0..4 {
            world.push_stairs(StairDir::Down);
        }
        world.confirm_stairs();

        let mut regions = std::mem::take(&mut world.regions);
        let nav = nav::bake(&mut regions, &[]).expect("bake with stairs");
        world.regions = regions;

        // A cell below the room floor exists (the descended corridor), and a path
        // runs from the room floor down to it.
        let stand = nav.all_standable();
        assert!(
            stand.iter().any(|c| c.y < -0.1),
            "some standable cell sits below the original floor (descended steps)"
        );
        let top = Vec3::new(3.0, 0.1, 3.0); // room floor
        let bottom = *stand
            .iter()
            .min_by(|a, b| a.y.total_cmp(&b.y))
            .expect("a lowest cell");
        let path = nav
            .find_path(top, bottom)
            .expect("a path should run from the room floor down the stairs");
        assert!(path.len() >= 2);
        assert!(path.last().unwrap().y < -0.1, "the route reaches the lower corridor");
    }

    /// Up-stairs are walkable by the hunter: treads rise above the floor and a
    /// path runs up onto the raised corridor.
    #[test]
    fn up_stairs_are_walkable_by_nav() {
        let mut world = World::new();
        world.initial_meshes();
        world.select_at_crosshair(); // −Z wall
        for _ in 0..3 {
            world.push_stairs(StairDir::Up);
        }
        world.confirm_stairs();

        let mut regions = std::mem::take(&mut world.regions);
        let nav = nav::bake(&mut regions, &[]).expect("bake with up-stairs");
        world.regions = regions;

        let stand = nav.all_standable();
        assert!(
            stand.iter().any(|c| c.y > 0.1),
            "some standable cell sits above the original floor (ascended steps)"
        );
        let bottom = Vec3::new(3.0, 0.1, 3.0);
        let top = *stand.iter().max_by(|a, b| a.y.total_cmp(&b.y)).expect("a highest cell");
        let path = nav
            .find_path(bottom, top)
            .expect("a path should run up the stairs to the raised corridor");
        assert!(path.last().unwrap().y > 0.1, "the route reaches the raised corridor");
    }

    /// Down-stairs are walkable by the player: entering HUNT and walking into the
    /// stairwell, the capsule descends the treads (feet drop below the floor) and
    /// is caught by them (never falls through to the void floor). This exercises
    /// the folded tread geometry as a Rapier trimesh collider.
    #[test]
    fn player_descends_the_stairs() {
        let mut world = World::new();
        world.initial_meshes();
        world.select_at_crosshair(); // −Z wall, full face
        for _ in 0..4 {
            world.push_stairs(StairDir::Down); // 4 steps down (−1 m at the bottom)
        }
        let rm = world.confirm_stairs().expect("confirm");
        // Sanity: the tread mesh made it into the region collider.
        assert!(!rm.mesh.indices.is_empty());

        world.toggle_mode(); // BUILD → HUNT; player spawns on the room floor
        assert_eq!(world.mode, Mode::Hunt);
        world.character.as_mut().unwrap().yaw = 0.0; // face −Z, toward the stairs
        let mut input = InputState::default();
        input.pointer_locked = true;
        input.press(winit::keyboard::KeyCode::KeyW);

        for _ in 0..600 {
            // 5 s at 120 Hz — walk into and down the stairwell
            world.fixed_step(1.0 / 120.0, &input);
        }
        let feet = world.player_pos().unwrap();
        assert!(
            feet.y < -0.1,
            "player should walk down the treads (feet below the floor), got y={}",
            feet.y
        );
        // Void floor is at −4 WT = −1.0 m; treads must catch the capsule above it.
        assert!(
            feet.y > -1.05,
            "player should rest on a tread, not fall through to the void floor, got y={}",
            feet.y
        );
    }

    /// Walking straight into a wall is blocked — the capsule can't tunnel
    /// through the CSG collider.
    #[test]
    fn character_cannot_walk_through_a_wall() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode();
        // Face −Z (yaw 0) toward the z=0 wall; hold W, pointer locked.
        let mut input = InputState::default();
        input.pointer_locked = true;
        input.press(winit::keyboard::KeyCode::KeyW);

        for _ in 0..600 {
            world.fixed_step(1.0 / 120.0, &input);
        }
        let feet = world.player_pos().unwrap();
        // Capsule radius is 0.25 m, so it should stop before z=0, never negative.
        assert!(feet.z > 0.1, "capsule tunneled through the wall: z={}", feet.z);
    }

    // ─── Free-standing platforms + stair-runs ───────────────────────────────

    /// The default room plus a raised platform (top at y=6 WT) and a stair-run
    /// descending from its −X edge down to the floor. Structures are built into
    /// the `STRUCT_ID` mesh + collider. The platform sits at x∈[10,14], z∈[8,12];
    /// the stair-run runs along −X from x=10 down to x=4 over z∈[8,12].
    fn room_with_platform_and_stair() -> World {
        let mut world = World::new(); // 24×16×24 cavity, floor at y=0
        world.initial_meshes();
        world.platforms.push(Platform {
            id: 1,
            x: 10.0,
            y: 6.0,
            z: 8.0,
            size_x: 4.0,
            size_z: 4.0,
            thickness: 1.0,
            grounded: false,
            railings: false,
        });
        world.next_platform_id = 2;
        world.stair_runs.push(StairRun {
            id: 1,
            from_platform: Some(1),
            to_platform: None,
            anchor_from: Anchor::Edge {
                edge: structures::Edge::XMin,
                offset: 0.5,
            },
            anchor_to: Anchor::Ground {
                x: 4.0,
                y: 0.0,
                z: 10.0,
            },
            width: 4.0,
            step_height: 1.0,
            rise_over_run: 1.0,
            grounded: false,
            railings: false,
        });
        world.next_run_id = 2;
        world.rebuild_structures();
        world
    }

    /// A platform + connecting stair-run are walkable by the hunter's grid nav:
    /// the platform top and stair treads become standable, and A* finds a route
    /// from the room floor up onto the platform. Proves `structure_solid_boxes`
    /// reaches the voxelizer (the `collectExtraSolids`/platform-box port).
    #[test]
    fn platform_and_stair_are_walkable_by_nav() {
        let mut world = room_with_platform_and_stair();

        let solids = world.structure_solid_boxes();
        assert!(!solids.is_empty(), "platform + stair produced solid boxes");
        let mut regions = std::mem::take(&mut world.regions);
        let nav = nav::bake(&mut regions, &solids).expect("bake with structures");
        world.regions = regions;

        // The platform top (y=6 WT = 1.5 m) yields a standable cell up there.
        let stand = nav.all_standable();
        assert!(
            stand.iter().any(|c| c.y > 1.4),
            "a standable cell sits on the raised platform (top at 1.5 m)"
        );

        // A route runs from the room floor up the stairs onto the platform top.
        let floor = Vec3::new(0.75, 0.1, 2.5); // near the bottom of the stairs
        let top = *stand
            .iter()
            .max_by(|a, b| a.y.total_cmp(&b.y))
            .expect("a highest standable cell");
        let path = nav
            .find_path(floor, top)
            .expect("A* should route up the stair-run onto the platform");
        assert!(
            path.last().unwrap().y > 1.4,
            "the route climbs onto the platform, got {:?}",
            path.last()
        );
    }

    /// The player capsule rests on a platform's top surface (its trimesh collider
    /// holds it): spawning the player above the platform, gravity settles it onto
    /// the slab (y≈1.5 m), not through it to the floor.
    #[test]
    fn player_capsule_rests_on_a_platform() {
        let mut world = room_with_platform_and_stair();
        // Camera above the platform centre (x=12,z=10 WT → 3.0, 2.5 m).
        world.camera.pos = Vec3::new(3.0, 2.5, 2.5);
        world.toggle_mode(); // spawns the capsule via a downward ray onto the slab
        assert_eq!(world.mode, Mode::Hunt);

        let input = InputState::default(); // stand still
        for _ in 0..360 {
            world.fixed_step(1.0 / 120.0, &input);
        }
        let feet = world.player_pos().expect("player in HUNT");
        assert!(
            feet.y > 1.4,
            "capsule should rest on the platform top (~1.5 m), got y={}",
            feet.y
        );
    }

    /// The platform tool state machine: `T` arms it, aiming at a wall places a
    /// platform on click, and it becomes selected. A second placement, connect,
    /// grounded, and delete all round-trip through the public API.
    #[test]
    fn platform_tool_places_and_edits() {
        let mut world = World::new(); // camera looks −Z at the z=0 wall
        world.initial_meshes();

        assert!(!world.is_platform_tool());
        world.platform_tool_key();
        assert!(world.is_platform_tool() && world.is_platform_placing());

        // Click while aimed at the wall → a platform is placed and selected.
        assert!(
            world.platform_click().is_some(),
            "placing a platform rebuilds the structures mesh"
        );
        assert_eq!(world.platforms.len(), 1, "one platform placed");
        assert_eq!(world.platform_phase, Some(PlatformPhase::Selected));

        // Toggle grounded on the selection.
        assert!(world.toggle_grounded_key().is_some());
        assert!(world.platforms[0].grounded, "F grounded the platform");

        // Delete it (and it returns to the idle placement phase).
        assert!(world.delete_selected().is_some());
        assert!(world.platforms.is_empty(), "platform deleted");
        assert_eq!(world.platform_phase, Some(PlatformPhase::Idle));
    }

    /// Arming another modal tool (door) disarms the platform tool, and vice
    /// versa — the tools stay mutually exclusive.
    #[test]
    fn platform_tool_is_mutually_exclusive() {
        let mut world = World::new();
        world.initial_meshes();
        world.platform_tool_key();
        assert!(world.is_platform_tool());
        world.door_tool_key(); // arming the door disarms the platform tool
        assert!(!world.is_platform_tool(), "door tool disarmed the platform tool");
        assert!(world.is_opening_arming());
        world.platform_tool_key(); // arming the platform disarms the door
        assert!(!world.is_opening_arming(), "platform tool disarmed the door tool");
        assert!(world.is_platform_tool());
    }

    /// The two-step connect flow: `C` arms ConnectDst; locking a destination +
    /// source edge advances to ConnectSrc; a confirm builds one run and returns to
    /// Selected; and the Esc ladder walks ConnectSrc → ConnectDst → Selected.
    #[test]
    fn connect_two_step_locks_slides_and_builds() {
        let mut world = room_with_platform_and_stair(); // platform 1 at (10,6,8)
        world.platform_phase = Some(PlatformPhase::Selected);
        world.selected_platform = Some(1);

        world.connect_key();
        assert_eq!(world.platform_phase, Some(PlatformPhase::ConnectDst));

        // Lock a ground destination + the −X source edge (what connect_lock_target
        // does from a crosshair hit), then confirm. Camera looks level (pitch 0)
        // so the slide offset resolves to the edge midpoint (0.5).
        world.connect_to = Some(ConnectTarget::Ground { x: 4.0, y: 0.0, z: 10.0 });
        world.connect_edge = Some(Edge::XMin);
        world.connect_slide_wt = 2.0;
        world.platform_phase = Some(PlatformPhase::ConnectSrc);

        // The wheel slides the attach point in 1-WT steps, clamped to the edge
        // length (platform 1 is 4 WT deep, so the XMin edge is 4 WT long).
        assert!(world.is_connect_sliding());
        world.adjust_connect_slide(1.0);
        assert_eq!(world.connect_slide_wt, 3.0, "wheel slid +1 WT");
        world.adjust_connect_slide(10.0);
        assert_eq!(world.connect_slide_wt, 4.0, "clamped to the edge length");

        let before = world.stair_runs.len();
        assert!(world.connect_confirm().is_some(), "confirm builds + rebuilds");
        assert_eq!(world.stair_runs.len(), before + 1, "one run added");
        assert_eq!(world.platform_phase, Some(PlatformPhase::Selected));
        assert!(world.connect_to.is_none() && world.connect_edge.is_none());

        // Esc ladder from a fresh ConnectSrc.
        world.connect_key();
        world.connect_to = Some(ConnectTarget::Ground { x: 4.0, y: 0.0, z: 10.0 });
        world.connect_edge = Some(Edge::XMin);
        world.platform_phase = Some(PlatformPhase::ConnectSrc);
        assert!(world.platform_escape().0, "esc consumed");
        assert_eq!(world.platform_phase, Some(PlatformPhase::ConnectDst), "src → dst");
        assert!(world.platform_escape().0);
        assert_eq!(world.platform_phase, Some(PlatformPhase::Selected), "dst → selected");
    }

    /// The gizmo shows for a selected platform, a scale-handle drag grows the
    /// footprint, a move-arrow drag repositions it, and Esc cancels a drag
    /// (restoring the transform).
    #[test]
    fn gizmo_scales_moves_and_cancels() {
        let mut world = room_with_platform_and_stair();
        world.platform_phase = Some(PlatformPhase::Selected);
        world.selected_platform = Some(1);
        assert!(world.gizmo_mesh().is_some(), "gizmo shows for a selected platform");

        // Scale +X: a large rightward drag grows the footprint.
        let size_before = world.platforms[0].size_x;
        world.gizmo_start(GizmoHandle::ScaleXMax);
        assert!(world.is_gizmo_dragging());
        world.gizmo_drag_delta(400.0, 0.0);
        assert!(
            world.platforms[0].size_x > size_before,
            "scale handle grew size_x: {} → {}",
            size_before,
            world.platforms[0].size_x
        );
        world.gizmo_drag = None; // a click would confirm the drag

        // Move +X: drag shifts the platform; Esc cancels and restores it.
        let x_before = world.platforms[0].x;
        world.gizmo_start(GizmoHandle::MoveX);
        world.gizmo_drag_delta(400.0, 0.0);
        assert!(world.platforms[0].x > x_before, "move arrow shifted +X");
        let (consumed, mesh) = world.platform_escape();
        assert!(consumed && mesh.is_some(), "Esc cancels the drag + rebuilds");
        assert_eq!(world.platforms[0].x, x_before, "cancel restored the position");
        assert!(!world.is_gizmo_dragging());
    }
}
