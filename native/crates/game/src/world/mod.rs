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

use engine::render::camera::FlyCamera;
use crate::character::CharacterController;
// NB: `crate::combat` (the subsystem) vs `world::combat` (the `mod combat;` wiring
// submodule below) share a name — import only the types, and reach the crate
// module fully-qualified (`crate::combat::…`) to avoid the shadow.
use engine::assets::textured_model::TexturedModel;
use engine::audio::AudioManager;
use crate::combat::Weapon;
use engine::geometry::csg_runtime::{
    Axis, Brush, Op, Region, Side, StairDesc, StairDir, WALL_THICKNESS, WORLD_SCALE,
};
use crate::enemy::Enemy;
use engine::platform::input::InputState;
use engine::render::mesh::{ColorVertex, ColoredMesh, CpuMesh, TexturedMesh};
use engine::sim::nav::{self, NavWorld};
use engine::sim::physics::PhysicsWorld;
use engine::skeletal::anim::AnimPlayer;
use engine::skeletal::anim_set;
use engine::skeletal::clip;
use engine::skeletal::gltf_skin::{self, SkinnedModel};
use engine::geometry::structures::{self, Anchor, Edge, Platform, StairRun};
use engine::render::textures::DEFAULT_SCHEME;
use engine::render::uv_zones::ZonedBuilder;

// ─── Submodule tree (the `impl World` methods are spread across these) ──
mod combat;
mod editing;
mod geom;
mod hunt;
mod lifecycle;
mod pick;
mod tools;
#[cfg(test)]
mod tests;

// Module-internal free helpers, re-exported so every submodule reaches them
// through `use super::*` regardless of which file defines them. (`find_room_brushes`
// / `brushes_touching` are used only within `editing`, so they aren't re-exported.)
pub(crate) use geom::{boxes_mesh, make_stair_void, make_wall_brush, push_colored_box};
pub(crate) use pick::{flip, same_face};

/// Default push/pull increment, in WT (JS `PUSH_PULL_STEP`). Shift → 1 WT.
pub const PUSH_PULL_STEP: f32 = 4.0;

// ─── GoldenEye free-aim (Player Combat; hold RMB) ──────────────────────────
/// The crosshair floats within this circular radius in "aim space" (an isotropic
/// NDC-like space); drawn aspect-corrected so the boundary reads circular on
/// screen. Ported from GamepadManager `AIM_MAX_RANGE`.
pub(crate) const AIM_MAX_RANGE: f32 = 0.6;
/// Mouse pixels → aim-space units. Equal to the camera `LOOK_SPEED` so that when
/// the crosshair is pinned at the rim the leftover motion pans the view seamlessly.
pub(crate) const AIM_SENS: f32 = 0.002;
/// Crosshair snap-back-to-center speed when not aiming (JS `RETURN_SPRING`).
pub(crate) const AIM_RETURN_SPRING: f32 = 15.0;
/// tan(½ · 60°) — the world/viewmodel vertical FOV. Maps an aim-space offset to
/// an angular offset for the gun tilt + the fire ray.
pub(crate) const AIM_FOV_TAN: f32 = 0.577_350_3;

/// Skinned-character model scale: GoldenEye units → metres. 0.00104 = base
/// 0.001 + ~4% (matches the 3DS FPS port, sized to level proportions).
pub(crate) const CHAR_SCALE: f32 = 0.00104;

/// B3 locomotion demo: linear speed (m/s) per band, indexed by `demo_band`
/// (0=idle,1=walk,2=jog,3=run). Walk/jog/run match `SPEED_THRESHOLDS` so the
/// JS `_playLocomotion` band selection lands exactly on each clip.
pub(crate) const LOCO_SPEEDS: [f32; 4] = [0.0, 1.5, 3.5, 5.0];
/// Demo circle the character paces (room centre, radius in metres).
pub(crate) const DEMO_CENTER: Vec3 = Vec3::new(3.0, 0.0, 3.0);
pub(crate) const DEMO_RADIUS: f32 = 1.6;

/// Clip indices within the character's [`AnimPlayer`], set by the fixed load
/// order in `World::new`: 0–3 locomotion, 4 fire, then the hit set, then death.
pub(crate) const CHAR_FIRE_IDX: usize = 4;
pub(crate) const CHAR_HIT_START: usize = 5;

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

/// A region's freshly-evaluated **textured** mesh, classified into per-(scheme,
/// zone) draw groups (scheme is per-triangle via the owning brush), returned to
/// the app for GPU upload. The collider is rebuilt inside
/// [`World::rebuild_region`] from the plain CSG mesh — this carries render data only.
pub struct RegionMesh {
    pub id: u32,
    pub mesh: TexturedMesh,
}

/// The currently-selected brush face (what push/pull acts on, and what the
/// highlight overlay draws). Mirrors JS `state.csg.selectedFace`.
#[derive(Clone, Copy)]
pub(crate) struct Selection {
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
pub(crate) struct Door {
    aabb: Brush,
    hp: f32,
    broken: bool,
    /// The panel collider's index in [`PhysicsWorld`], removed on breach.
    panel: usize,
}

/// A live hit spark (Player Combat P2): a bright marker at a shot's impact point,
/// nudged just off the surface, that fades out after [`SPARK_TTL`] seconds. Purely
/// visual feedback that a shot registered at the right spot.
#[derive(Clone, Copy)]
pub(crate) struct Spark {
    pos: Vec3,
    ttl: f32,
}

/// How long a hit spark lives (s) and its half-extent (metres). Small + brief:
/// enough to see where the shot landed, not a persistent decal.
const SPARK_TTL: f32 = 0.12;
const SPARK_HALF: f32 = 0.02;

/// Which opening the crosshair tool cuts. A `Door` is a fixed 3×7 wall opening
/// that becomes breakable at HUNT (frame marked `door`); a `Hole` is an
/// arbitrary-size opening in any face (walls, floor, or ceiling), not breakable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OpeningKind {
    Door,
    Hole,
}

/// Which additive-brush placement tool is armed. A `Pillar` is a floor→ceiling
/// square column; a `Brace` is a 3-brush arch (up one wall, across the ceiling,
/// down the opposite wall). Both are plain `Op::Add` brushes (JS marks them
/// `isBrace` for texturing, which we don't have yet).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PlaceKind {
    Pillar,
    Brace,
}

/// The free-standing platform/stair-run tool's phase (JS `state.platformPhase`).
/// `None` on `World` = the tool is off entirely; `Some(_)` = armed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PlatformPhase {
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
pub(crate) enum ConnectTarget {
    Platform { id: u32, edge: Edge },
    Ground { x: f32, y: f32, z: f32 },
}

/// A resolved crosshair hit for the platform tool: the WT hit point, the dominant
/// surface axis, and which platform/stair-run (if any) that point lies inside.
#[derive(Clone, Copy)]
pub(crate) struct StructureHit {
    hit_wt: Vec3,
    axis: Axis,
    platform: Option<u32>,
    run: Option<u32>,
}

/// One draggable part of the platform gizmo (JS `gizmo.js`): three move arrows
/// (translate the whole platform along an axis) and four edge scale handles
/// (grow/shrink the footprint from that edge).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum GizmoHandle {
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
pub(crate) struct GizmoDrag {
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
pub(crate) struct OpeningPlacement {
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
pub(crate) struct PendingStair {
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
    /// Texture scheme inherited from the wall the stair anchors to.
    scheme: usize,
}

/// A sub-face carve/extrude in progress (JS `activeBrush`/`activeOp`): a spawned
/// brush grown by repeated push/pull, so holding `+` carves deeper instead of
/// stacking new brushes on every press.
#[derive(Clone, Copy)]
pub(crate) struct ActiveOp {
    brush_id: u32,
    op: SubOp,
    side: Side,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum SubOp {
    Push,
    Pull,
}

/// The selected face's in-plane U/V extent in WT (JS `getFaceUVInfo`), plus the
/// face-plane coord on the normal axis.
pub(crate) struct FaceInfo {
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
    /// B1 skinned-character viewer: one loaded character rendered in bind pose so
    /// the skinning pipeline can be verified live. Later milestones drive its pose
    /// from clips and hand it to the enemy. `None` if the asset failed to load.
    char_model: Option<SkinnedModel>,
    /// B2/B3: the crossfade mixer driving the character (idle + walk/jog/run),
    /// or `None` → static bind pose. Clip indices: 0=idle,1=walk,2=jog,3=run.
    char_anim: Option<AnimPlayer>,
    /// B3 demo: locomotion band (0=idle,1=walk,2=jog,3=run) cycled by `L`, and
    /// the character's angle around the demo circle. Replaced by enemy/nav-driven
    /// movement in B5.
    demo_band: usize,
    demo_angle: f32,
    /// Character feet position (metres) + facing yaw, updated by the demo mover.
    char_pos: Vec3,
    char_yaw: f32,
    /// B4: `true` while a death one-shot is clamped on its last frame (press `L`
    /// to revive). Fire/hit are suppressed while dead.
    char_dead: bool,
    /// Last fire-window state, to log OPEN/closed transitions once.
    char_fire_open: bool,
    /// xorshift state for the hit/death random pick (no `rand` dep — a demo pick,
    /// not a statistical roll; combat can bring `rand` when it needs one).
    char_rng: u64,
    /// World-space Y offset that seats the character's feet on the floor.
    /// Computed from the **lowest skinned point of the actual idle pose** (the
    /// bind-pose AABB can't be used — the bind pose is a splayed star with the
    /// feet spread high, so seating by it leaves the standing pose sunk).
    char_feet_offset: f32,

    // ─── Player Combat (HUNT-phase weapon; see `world/combat.rs`) ──
    /// P1: the first-person weapon's static gun mesh (CPU side), uploaded once to
    /// the renderer at startup. `None` if the asset failed to load.
    gun_model: Option<TexturedModel>,
    /// P2: the muzzle-flash mesh (separate GLB), uploaded once; drawn additively
    /// on top of the gun while a shot's flash is active. `None` if load failed.
    muzzle_model: Option<TexturedModel>,
    /// The active weapon: config + viewmodel + fire timing (+ ammo P3, recoil P4).
    weapon: Weapon,
    /// P2: live hit sparks — a short-lived bright marker at each impact point, so
    /// wall hits read at the right spot. Decayed each frame in HUNT.
    sparks: Vec<Spark>,
    /// GoldenEye free-aim crosshair offset in aim space (see `AIM_MAX_RANGE`).
    /// Moves while RMB is held (HUNT), springs back to center on release. Drives
    /// the crosshair position, the gun tilt, and the fire-ray direction. 0 = center.
    aim_x: f32,
    aim_y: f32,
    /// Whether free-aim is currently engaged (RMB held in HUNT). The crosshair is
    /// shown only while aiming (HUNT) — matching GoldenEye's aim-mode reticle.
    aiming: bool,
    /// The audio subsystem (one-shot weapon SFX + looping background music).
    /// `None` until the app attaches it post-construction (see `attach_audio`) —
    /// so headless tests, which never attach it, run silently. Cue draining and
    /// music are no-ops while `None`.
    audio: Option<AudioManager>,

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

        // B1: load the first skinned character (a warning, not a panic, if the
        // asset is missing — the editor still runs without it).
        let char_path = format!(
            "{}/../../assets/enemies/characters/russian-guard_karl.glb",
            env!("CARGO_MANIFEST_DIR")
        );
        let char_model = match gltf_skin::load(&char_path) {
            Ok(m) => {
                log::info!(
                    "loaded character russian-guard_karl: {} verts, {} primitives, {} joints",
                    m.vertices.len(),
                    m.primitives.len(),
                    m.skeleton.joint_count()
                );
                Some(m)
            }
            Err(e) => {
                log::warn!("character load failed: {e}");
                None
            }
        };

        // B2/B3/B4: load the clip set bound to the character's skeleton, in a
        // FIXED index order (locomotion 0–3, fire 4, then the hit set, then the
        // death set — see CHAR_*_IDX), into a crossfade mixer.
        let char_anim = char_model.as_ref().and_then(|m| {
            let mut files: Vec<&str> =
                vec!["00-idle.glb", "28-walking.glb", "2A-jogging.glb", "29-running.glb"];
            files.push(anim_set::FIRE_CLIP);
            files.extend_from_slice(anim_set::HIT_CLIPS);
            files.extend_from_slice(anim_set::DEATH_CLIPS);
            let mut clips = Vec::new();
            for f in &files {
                let path =
                    format!("{}/../../assets/enemies/animations/{f}", env!("CARGO_MANIFEST_DIR"));
                match clip::load(&path, &m.skeleton) {
                    Ok(c) => clips.push(c),
                    Err(e) => log::warn!("clip {f} load failed: {e}"),
                }
            }
            if clips.len() == files.len() {
                log::info!("loaded {} character clips (idle/walk/jog/run + fire + 12 hit + 17 death)", clips.len());
                Some(AnimPlayer::new(clips, 0))
            } else {
                log::warn!("only {}/{} clips loaded; character animation disabled", clips.len(), files.len());
                None
            }
        });

        // Seat the feet: sample the idle across its loop, skin each pose on the
        // CPU, and take the global lowest Y (the most-planted foot). Seating that
        // at the floor keeps the feet grounded while the animation's own vertical
        // motion still reads. Falls back to the bind-pose AABB with no clip.
        let char_feet_offset = match (&char_model, char_anim.as_ref().and_then(|a| a.clip(0))) {
            (Some(m), Some(idle)) => {
                let samples = 24;
                let mut min_y = f32::INFINITY;
                for i in 0..samples {
                    let t = idle.duration * i as f32 / samples as f32;
                    let mats = idle.skinning_matrices(t, &m.skeleton);
                    min_y = min_y.min(m.skinned_min_y(&mats));
                }
                -min_y * CHAR_SCALE
            }
            (Some(m), _) => -m.bounds_min.y * CHAR_SCALE,
            _ => 0.0,
        };

        // Player Combat: build the weapon (PP7, the P0 pick) and load its gun +
        // muzzle-flash meshes. Warn-not-panic if an asset is missing — the rest
        // still runs. Both GLBs live under `native/assets/weapons/`.
        let weapon = Weapon::new(crate::combat::config::PP7);
        let asset = |rel: &str| {
            format!("{}/../../assets/weapons/{}", env!("CARGO_MANIFEST_DIR"), rel)
        };
        let gun_model = match crate::combat::load_gun(&asset(weapon.config().gun_path)) {
            Ok(m) => {
                log::info!(
                    "loaded weapon {}: {} verts, {} primitives",
                    weapon.config().name,
                    m.vertices.len(),
                    m.primitives.len()
                );
                Some(m)
            }
            Err(e) => {
                log::warn!("weapon load failed: {e}");
                None
            }
        };
        let muzzle_model = if weapon.config().muzzle_path.is_empty() {
            None
        } else {
            // `load_flash` keeps only the additive flash billboards — the GoldenEye
            // muzzle.glb is the whole firing pose (gun body + hand + flash), so
            // drawing all of it flashed a hand into view.
            match crate::combat::load_flash(&asset(weapon.config().muzzle_path)) {
                Ok(m) => {
                    log::info!("loaded muzzle flash: {} verts", m.vertices.len());
                    Some(m)
                }
                Err(e) => {
                    log::warn!("muzzle-flash load failed: {e}");
                    None
                }
            }
        };

        // Demo character starts on the circle at 270° — the nice centre-front
        // spot in front of the spawn camera — facing +Z (toward the camera).
        let demo_angle = std::f32::consts::FRAC_PI_2 * 3.0;
        let char_pos = DEMO_CENTER
            + Vec3::new(DEMO_RADIUS * demo_angle.cos(), 0.0, DEMO_RADIUS * demo_angle.sin());

        World {
            camera,
            physics: PhysicsWorld::new(),
            mode: Mode::Build,
            character: None,
            nav: None,
            enemy: None,
            char_model,
            char_anim,
            demo_band: 0,
            demo_angle,
            char_pos,
            char_yaw: 0.0,
            char_dead: false,
            char_fire_open: false,
            char_rng: 0x9E37_79B9_7F4A_7C15,
            char_feet_offset,
            gun_model,
            muzzle_model,
            weapon,
            sparks: Vec::new(),
            aim_x: 0.0,
            aim_y: 0.0,
            aiming: false,
            audio: None,
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
}
