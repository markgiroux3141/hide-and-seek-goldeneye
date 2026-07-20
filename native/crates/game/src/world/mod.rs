//! The authored scene — a hand-rolled `World` (no ECS yet; entity counts don't
//! justify one until the Phase 3 enemy roster). Owns the CSG regions, the
//! collision world, and the fly camera, and drives the BUILD-phase authoring
//! loop: crosshair face-pick → push/pull → re-evaluate the region → hand the
//! app a fresh mesh while updating the region's collider in place.
//!
//! Mirrors the reference editor (`src/tools/indoorKeys.js` + `csgActions.js`):
//! `+`/`=` push (carve inward), `-` pull (extend outward), default step 4 WT.

use std::time::Instant;

use glam::{EulerRot, Mat4, Vec3};

use engine::render::camera::FlyCamera;
use crate::character::CharacterController;
// NB: `crate::combat` (the subsystem) vs `world::combat` (the `mod combat;` wiring
// submodule below) share a name — import only the types, and reach the crate
// module fully-qualified (`crate::combat::…`) to avoid the shadow.
use engine::assets::textured_model::TexturedModel;
use engine::audio::AudioManager;
use crate::combat::enemy_weapons::{LEFT_HAND_BONE, RIGHT_HAND_BONE};
use crate::combat::{enemy_def_for, EnemyWeaponClass, EnemyWeaponDef, Weapon};
use engine::geometry::csg_runtime::{
    Axis, Brush, Op, Region, Side, StairDesc, StairDir, WALL_THICKNESS, WORLD_SCALE,
};
use crate::enemy::Enemy;
use rapier3d::prelude::ColliderHandle;
use engine::platform::input::InputState;
use engine::render::mesh::{ColorVertex, ColoredMesh, CpuMesh, TexVertex, TexturedMesh};
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
pub(crate) use hunt::{band_for_speed, fire_clip_index, fire_window_for, is_fire_clip};
pub(crate) use lifecycle::pick_spread_spawns;
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

// ─── USB-N64 gamepad (GoldenEye "solitaire" scheme) ────────────────────────
// Ported verbatim from the 3DS FPS `GamepadManager.ts`. `AIM_MAX_RANGE` /
// `AIM_RETURN_SPRING` above are shared with the mouse free-aim.
/// Radial stick deadzone — below this magnitude the stick reads as centered.
pub(crate) const STICK_DEADZONE: f32 = 0.15;
/// Camera-yaw rate at full stick, in mouse-pixel-equivalents per second (fed to
/// `apply_look_delta`, so the effective rad/s is this × the camera `LOOK_SPEED`).
pub(crate) const PAD_TURN_SPEED: f32 = 1800.0;
/// Aim-mode crosshair spring stiffness toward the stick target (higher = snappier).
pub(crate) const PAD_AIM_SPRING: f32 = 10.0;
/// Stick magnitude at which aim-mode begins rotating the camera (below it, the
/// crosshair just floats).
pub(crate) const PAD_AIM_TURN_THRESHOLD: f32 = 0.85;
/// Camera-rotation rate (pixel-equivalents/s) once past the aim-turn threshold.
pub(crate) const PAD_AIM_TURN_SPEED: f32 = 600.0;
/// C-Up / C-Down look rate (pixel-equivalents/s).
pub(crate) const PAD_C_LOOK_SPEED: f32 = 300.0;
/// Vertical-look sign for the gamepad: `-1.0` = inverted (stick-up looks/aims
/// down, GoldenEye's N64 default), `+1.0` = non-inverted. Applied consistently to
/// the aim reticle, the aim-mode camera pan, and C-Up/C-Down so they never fight.
pub(crate) const PAD_PITCH_SIGN: f32 = -1.0;

/// Skinned-character model scale: GoldenEye units → metres. The 3DS FPS port used
/// 0.00104 (base 0.001 + ~4%); shrunk to ~80% (user call 2026-07-17) so the hunter
/// reads better against the level. The GE-unit weapon bone offsets and the computed
/// `char_feet_offset` both flow through this scale, so they shrink with the model.
pub(crate) const CHAR_SCALE: f32 = 0.000_832; // 0.00104 × 0.8

/// Clip indices within the character's [`AnimPlayer`], set by the fixed load order
/// in `World::new`: `0–3` locomotion, then one fire clip per weapon class
/// (rifle/pistol/dual), then the hit set, then the death set. The class-specific
/// fire clip + its FIRE_TIMING window are selected via [`hunt::fire_clip_index`] /
/// [`hunt::fire_window_for`]; [`hunt::is_fire_clip`] recognises all three.
pub(crate) const FIRE_RIFLE_IDX: usize = 4; // 01-fire-standing
pub(crate) const FIRE_PISTOL_IDX: usize = 5; // 41-fire-standing-pistol
pub(crate) const FIRE_DUAL_IDX: usize = 6; // 7A-fire-standing-dual-wield
pub(crate) const CHAR_HIT_START: usize = 7;

// ─── Track A — killable hunter ──────────────────────────────────────────────
/// The hunter's capsule collider dimensions in metres — the recon constants
/// (`0.3` / `0.6`) scaled to ~80% to match the shrunk model, so shots still land
/// on the smaller body. Total height ≈ 1.44 m.
pub(crate) const ENEMY_RADIUS: f32 = 0.24; // 0.3 × 0.8
pub(crate) const ENEMY_HALF_HEIGHT: f32 = 0.48; // 0.6 × 0.8
/// Death fade duration (s) — JS `EnemyCharacter.FADE_DURATION`. The body fades
/// its opacity 1→0 over this window after the lethal shot, then vanishes.
pub(crate) const FADE_DURATION: f32 = 2.0;
/// Number of enemy pain vocalisations (`sounds/enemies/pain-1..26.wav`).
pub(crate) const PAIN_COUNT: usize = 26;
/// On-hit SFX volumes (JS `EnemyCharacter.onHit`): the pain vocal + the flesh
/// bullet-hit, linear amplitude.
pub(crate) const PAIN_VOL: f32 = 0.8;
pub(crate) const BULLET_HIT_VOL: f32 = 0.5;

/// Blood/damage painting (JS `EnemyCharacter.paintDamage`): vertices within
/// `BLOOD_RADIUS` (world metres) of a shot's impact get reddened, accumulating so
/// repeated hits build up persistent blood. The JS radius is 300 GE-units in the
/// model's local space; here we compare in world space, so it's scaled by
/// `CHAR_SCALE` (≈0.25 m). `BLOOD_INTENSITY` is the peak per-hit strength at the
/// centre (JS `intensity`), falling off linearly to the rim.
pub(crate) const BLOOD_RADIUS: f32 = 300.0 * CHAR_SCALE;
pub(crate) const BLOOD_INTENSITY: f32 = 0.5;

/// Zone hitscan (damage + hurt animation vary by where the shot lands). Boundaries
/// are impact height above the hunter's feet, in metres, for the ~1.44 m capsule
/// (feet 0 → head ~1.44). Multipliers mirror the JS `ZONE_DAMAGE_MULTIPLIER`
/// (head 4.0, torso 1.0, legs 0.6; arms are folded into torso since a height-only
/// classifier can't separate them).
pub(crate) const ZONE_HEAD_MIN: f32 = 1.1; // ≥ this above the feet → head
pub(crate) const ZONE_LEG_MAX: f32 = 0.55; // < this above the feet → legs
pub(crate) const ZONE_HEAD_MULT: f32 = 4.0;
pub(crate) const ZONE_TORSO_MULT: f32 = 1.0;
pub(crate) const ZONE_LEG_MULT: f32 = 0.6;

// ─── Enemies fire back (A3) — data-driven arsenal + probabilistic hit ────────
// Per-weapon damage / accuracy / range / fire-rate now live on the equipped
// [`EnemyWeaponDef`] (see `combat::enemy_weapons`); only the shared feedback
// timings stay here.
/// The muzzle-flash countdown (s) after each enemy shot; >0 → the enemy muzzle
/// renders.
pub(crate) const ENEMY_MUZZLE_TIME: f32 = 0.1;
/// Enemy gun-report volume (linear amplitude).
pub(crate) const ENEMY_FIRE_VOL: f32 = 0.7;

/// The hunter roster spawned at G→HUNT: `(weapon, dual-wield?)`, one hunter per
/// entry (capped by available standable cells). Covers every animation class —
/// two-handed rifle, one-handed pistol, dual rifle (the canonical akimbo weapon),
/// and dual pistols — so all the fire animations are exercised in one hunt. Any of
/// the 19 arsenal weapons can be listed here; each is classified + attached by
/// [`crate::combat::enemy_def_for`].
pub(crate) const ENEMY_ROSTER: &[(crate::combat::config::WeaponStats, bool)] = &[
    (crate::combat::config::KF7, false),     // two-handed rifle
    (crate::combat::config::PP7, false),     // one-handed pistol
    (crate::combat::config::RCP90, true),    // dual-wield rifle (akimbo)
    (crate::combat::config::PP7, true),      // dual-wield pistols
    (crate::combat::config::AR33, false),    // two-handed rifle
    (crate::combat::config::SHOTGUN, false), // two-handed
];

/// How many hunters flood in at the spawn point on G→HUNT. Weapons are drawn from
/// [`ENEMY_ROSTER`] (cycling if this exceeds the roster length), so this is the
/// single knob for "how big is the wave" — bump it and the rest follows.
pub(crate) const ENEMY_COUNT: usize = 6;

// ─── Enemy spawn point (a FIXED world marker) ────────────────────────────────
/// The hunters always flood in at this fixed world-space point (metres) — a
/// consistent location the builder authors around, **not** derived from where the
/// player happens to be at G. Marked on the floor by a colored square
/// ([`World::spawn_marker_mesh`]) visible in both BUILD and HUNT. Defaults to the
/// centre of the starting room; a placement tool can make it authorable later.
pub(crate) const SPAWN_MARKER_POS: Vec3 = Vec3::new(3.0, 0.0, 3.0);
/// Half-extent (m) of the floor marker square, and its flat colour (a bright
/// red so it clearly reads as the enemy ingress).
const SPAWN_MARKER_HALF: f32 = 0.6;
const SPAWN_MARKER_COLOR: [f32; 3] = [0.95, 0.12, 0.12];
/// Radius (m) of the ring the wave clusters into around the spawn point, so the
/// hunters don't all stack on one cell.
const SPAWN_CLUSTER_RADIUS: f32 = 0.7;

/// Size of the fan-out search-point pool the `World` hands out during the hunt
/// (spread standable cells). More points than hunters keeps the sweep varied.
const SEARCH_POINT_COUNT: usize = 12;
/// How far (m) the player's gunfire carries as a noise ping that pulls nearby
/// searching/investigating hunters toward the sound. Comfortably past the 12 m
/// sight range so shooting while hidden genuinely gives you away.
const GUNSHOT_HEARING_RANGE: f32 = 25.0;

/// Load a weapon's `(gun, muzzle-flash)` CPU meshes from its config, resolving the
/// asset-relative paths under `native/assets/weapons/`. Warn-not-panic: a failed
/// load (or a weapon with no muzzle, like the sniper — `muzzle_path == ""`) yields
/// `None` for that slot, and the renderer simply hides whatever is missing. Used at
/// startup for the initial weapon and on every `Q`/`A` weapon switch.
fn load_weapon_models(cfg: &crate::combat::config::WeaponStats) -> (Option<TexturedModel>, Option<TexturedModel>) {
    let asset = |rel: &str| format!("{}/../../assets/weapons/{}", env!("CARGO_MANIFEST_DIR"), rel);
    let gun = match crate::combat::load_gun(&asset(cfg.gun_path)) {
        Ok(m) => {
            log::info!(
                "loaded weapon {}: {} verts, {} primitives",
                cfg.name,
                m.vertices.len(),
                m.primitives.len()
            );
            Some(m)
        }
        Err(e) => {
            log::warn!("weapon '{}' gun load failed: {e}", cfg.name);
            None
        }
    };
    // `load_flash` keeps only the additive flash billboards — the GoldenEye
    // muzzle.glb is the whole firing pose (gun body + hand + flash), so drawing all
    // of it flashed a hand into view.
    let muzzle = if cfg.muzzle_path.is_empty() {
        None
    } else {
        match crate::combat::load_flash(&asset(cfg.muzzle_path)) {
            Ok(m) => Some(m),
            Err(e) => {
                log::warn!("weapon '{}' muzzle-flash load failed: {e}", cfg.name);
                None
            }
        }
    };
    (gun, muzzle)
}

// ─── Player health + damage feedback (P5) ───────────────────────────────────
pub(crate) const PLAYER_MAX_HEALTH: f32 = 100.0;
pub(crate) const PLAYER_MAX_ARMOR: f32 = 100.0;
/// Red damage-flash decay (JS `HealthHUD`: alpha −= dt·2.5), and the flash's peak
/// alpha per hit = min(0.5, dmg/40).
pub(crate) const DAMAGE_FLASH_DECAY: f32 = 2.5;
/// Health-HUD pop duration on damage + its fade tail (JS `showTimer = 1.5`,
/// `FADE_DURATION = 0.5`).
pub(crate) const HUD_SHOW_TIME: f32 = 1.5;
pub(crate) const HUD_FADE_TAIL: f32 = 0.5;
pub(crate) const PLAYER_HIT_SOUND: &str = "sounds/player/breathe.wav";
pub(crate) const PLAYER_HIT_VOL: f32 = 0.7;

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
/// Unused while breakable doors stay disabled; kept for re-enable.
#[allow(dead_code)]
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

/// One puff of a layered explosion. GoldenEye builds its big fireball from several
/// overlapping fireball sprites at slight offsets with staggered start times, so a
/// detonation spawns a cluster of these: a central core plus satellites. Each puff
/// plays the fireball atlas once over its own `life` (after its `delay`), additively
/// — many overlapping puffs read as one big, dense, roiling fireball that blooms and
/// lingers. Purely cosmetic; the blast DAMAGE is applied once at detonation (see
/// `world::combat::detonate`).
#[derive(Clone, Copy)]
pub(crate) struct Blast {
    /// This puff's centre (detonation centre + a small random offset).
    pos: Vec3,
    /// Seconds since the puff was spawned (counts up).
    age: f32,
    /// Seconds before this puff starts animating (staggered starts).
    delay: f32,
    /// This puff's animation duration (s).
    life: f32,
    /// World half-extent at animation scale 1 (already radius-scaled).
    half: f32,
    /// Line-of-sight visibility (0 or 1) from the camera, refreshed each frame: a
    /// puff occluded by a wall is hidden (so explosions don't glow through walls),
    /// while visible puffs still composite on top with no billboard slicing. The
    /// cluster of puffs gives a soft occlusion edge for free (some drop, some stay).
    vis: f32,
}

/// Per-puff fireball animation duration (s). With staggered starts up to
/// [`BLAST_STAGGER`], the whole explosion lasts ~`BLAST_TTL + BLAST_STAGGER` —
/// longer + denser than a single sprite (user call 2026-07-19).
const BLAST_TTL: f32 = 0.6;
/// Max start-delay spread across a blast's puffs (s) — staggered so the fireball
/// blooms and lingers instead of popping all at once.
const BLAST_STAGGER: f32 = 0.28;
/// Puff-centre offset spread, as a fraction of the blast radius.
const BLAST_SPREAD_FRAC: f32 = 0.3;
/// Puff-count bounds for a blast (scaled by radius between them).
const BLAST_PUFFS_MIN: usize = 3;
const BLAST_PUFFS_MAX: usize = 6;
/// Number of frames in the fireball atlas (horizontal strip).
const BLAST_FRAMES: usize = 8;
/// Billboard quad half-extent as a fraction of the blast radius, at animation
/// scale 1. The on-screen fireball peaks a bit under the full damage radius so it
/// reads as a punchy fireball, not a room-filling wall.
const BLAST_QUAD_HALF_FRAC: f32 = 0.42;
/// Half-texel UV inset so linear filtering never samples the neighbouring atlas
/// frame (atlas is `BLAST_FRAMES`×56 wide, 56 tall).
const BLAST_UV_INSET_U: f32 = 0.5 / (BLAST_FRAMES as f32 * 56.0);
const BLAST_UV_INSET_V: f32 = 0.5 / 56.0;
/// In-flight projectile marker: the bright box half-extent (m) drawn at the round's
/// current position, plus its short motion trail length (segments behind it).
const PROJECTILE_HALF: f32 = 0.1;
const PROJECTILE_TRAIL: usize = 4;
/// The detonation sound — the authentic GoldenEye blast SFX used for every
/// explosive (soundpack `blast14`, converted to WAV). Preloaded in `attach_audio`
/// so the first blast never hitches. Plus the shared blast volume.
pub(crate) const EXPLOSION_SOUND: &str = "sounds/weapons/explosion.wav";
pub(crate) const EXPLOSION_VOL: f32 = 0.9;
/// Approx centre-mass height above the feet (m) for the blast distance test — the
/// blast measures to the actor's middle, not its feet, so an overhead burst still
/// bites. One each for the ~1.44 m hunter and the player capsule.
const ENEMY_CENTER_Y: f32 = 0.7;
const PLAYER_CENTER_Y: f32 = 0.9;
/// A projectile that never contacts anything is dropped (no detonation) after this
/// long (s), so a fuseless rocket fired into open sky can't leak forever.
const PROJECTILE_MAX_LIFE: f32 = 6.0;
/// A bouncing projectile whose post-bounce speed drops below this (m/s) settles onto
/// the surface and waits out its fuse (stops the perpetual resting jitter).
const PROJECTILE_REST_SPEED: f32 = 1.5;
/// World scale for an in-flight projectile GLB (e.g. the thrown grenade). The
/// grenade GLB is authored ~3× the gun models, so a third of [`CHAR_SCALE`] lands it
/// at a believable hand-thrown grenade size (user call 2026-07-17).
const PROJECTILE_MODEL_SCALE: f32 = CHAR_SCALE / 3.0;
/// Tumble rates (rad/s) about X and Y for a flying projectile GLB, so it spins as it
/// travels. Frozen once the projectile comes to rest.
const PROJECTILE_SPIN_X: f32 = 9.0;
const PROJECTILE_SPIN_Y: f32 = 6.0;

// ─── Mines (see `world::combat`) ──────────────────────────────────────────────
/// How far off the struck surface the mine sits (m), so it doesn't z-fight or clip.
const MINE_SURFACE_OFFSET: f32 = 0.05;
/// Max seconds a thrown mine flies before it's stuck in place where it is (fallback
/// so a toss into open space / the void can't fly forever without attaching).
const MINE_MAX_FLIGHT: f32 = 5.0;
/// World scale for a thrown/stuck mine GLB. The mine meshes are gun-sized in the
/// weapon library, so [`CHAR_SCALE`] lands them at a believable charge size in world
/// space (retune by eye if they read too big/small).
const MINE_MODEL_SCALE: f32 = CHAR_SCALE;
/// The mine's "attach" sound, played when a mine sticks to a surface (soundpack
/// `attach_mine`, converted to WAV). Plus its volume.
pub(crate) const MINE_PLACE_SOUND: &str = "sounds/weapons/mine-place.wav";
const MINE_PLACE_VOL: f32 = 0.7;
/// The timed-mine arm beep, played once when a timed mine goes live (soundpack
/// `bomb_timer`, converted to WAV). Plus its volume.
pub(crate) const MINE_TIMER_SOUND: &str = "sounds/weapons/mine-timer.wav";
const MINE_TIMER_VOL: f32 = 0.6;
/// The remote-detonation "click" (soundpack `trigger_mine`, converted to WAV),
/// played when the player triggers a detonation (pad A+B / keyboard). Plus its
/// volume. No longer a weapon fire_sound (the Detonator slot was removed), so it's
/// preloaded explicitly in `attach_audio`.
pub(crate) const DETONATOR_SOUND: &str = "sounds/weapons/detonator-fire.wav";
const DETONATOR_VOL: f32 = 0.8;

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

/// One live hunter during the HUNT: its AI/movement [`Enemy`], its own animation
/// mixer (cloned from the shared clip template so each hunter animates
/// independently), the weapon it wields + whether it's dual-wielding, its hitscan
/// capsule handle, and its per-hunter combat/feedback timers. All hunters share the
/// single [`SkinnedModel`] geometry ([`World::char_model`]); only the pose differs.
pub(crate) struct EnemyInstance {
    pub enemy: Enemy,
    /// This hunter's crossfade mixer (own clock/pose). Clip layout matches the
    /// shared template: locomotion, per-class fire, hit set, death set.
    pub anim: AnimPlayer,
    /// The equipped weapon (asset paths + AI stats + bone-local attach offsets).
    pub weapon: EnemyWeaponDef,
    /// Dual-wielding — a second copy of `weapon` is held in the left hand and both
    /// muzzles flash on a shot (JS `weaponOptions.dual`).
    pub dual: bool,
    /// This hunter's hitscan capsule (moved each fixed step, removed on death).
    pub collider: ColliderHandle,
    /// Death fade: seconds since the death animation finished, or `None` while alive
    /// / mid death-animation. Drives opacity 1→0 over [`FADE_DURATION`].
    pub fade: Option<f32>,
    /// Enemy-fire cadence: seconds until the next shot may leave during the fire
    /// window (spaced by `1/weapon.fire_rate`).
    pub shot_timer: f32,
    /// Muzzle-flash countdown (s); >0 → this hunter's muzzle(s) render.
    pub muzzle_timer: f32,
    /// Per-vertex RGB blood color (flat, len = 3×model vertex count), white =
    /// clean. Each shot reddens the vertices near the impact (accumulating, so it
    /// builds up as persistent blood); uploaded to this hunter's instance color
    /// buffer each frame. JS `EnemyCharacter` per-instance vertex colors.
    pub blood: Vec<f32>,
}

/// A loaded enemy weapon's render assets: the gun mesh + optional muzzle-flash
/// mesh, keyed by the weapon name. Loaded once for the whole arsenal in
/// [`World::new`] and handed to the renderer's weapon library so any hunter can
/// draw any weapon (and the BUILD demo can preview every gun).
pub(crate) struct EnemyWeaponAsset {
    pub name: &'static str,
    pub gun: TexturedModel,
    pub muzzle: Option<TexturedModel>,
}

pub struct World {
    pub camera: FlyCamera,
    pub physics: PhysicsWorld,
    pub mode: Mode,
    /// The player capsule; `Some` only in HUNT mode.
    character: Option<CharacterController>,
    /// Baked nav grid; `Some` only in HUNT mode.
    nav: Option<NavWorld>,
    /// The live hunters (HUNT only) — one per [`ENEMY_ROSTER`] entry that found a
    /// spawn cell. Each carries its own mixer/weapon/collider; all share
    /// [`Self::char_model`] geometry. Empty in BUILD.
    enemies: Vec<EnemyInstance>,
    /// Whether a G→HUNT transition spawns the [`ENEMY_ROSTER`]. Defaults to `true`
    /// (so tests and the normal game get hunters); the app flips it off as a dev
    /// convenience while iterating on explosives (see `set_spawn_enemies`), so a
    /// hunt starts empty and you aren't gunned down before you can test.
    spawn_enemies: bool,
    /// The shared skinned-character geometry (one GLB) rendered for every hunter.
    /// `None` if the asset failed to load.
    char_model: Option<SkinnedModel>,
    /// Pristine animation mixer over the full clip set (locomotion + per-class fire
    /// + hit + death), cloned once per spawned hunter so each animates on its own
    /// clock. `None` if any clip failed to load.
    char_anim_template: Option<AnimPlayer>,
    /// xorshift state for the hit/death/pain random picks (no `rand` dep).
    char_rng: u64,
    /// World-space Y offset that seats the character's feet on the floor.
    /// Computed from the **lowest skinned point of the actual idle pose** (the
    /// bind-pose AABB can't be used — the bind pose is a splayed star with the
    /// feet spread high, so seating by it leaves the standing pose sunk).
    char_feet_offset: f32,
    // (Per-hunter death fade + fire cadence + muzzle timers now live on each
    // [`EnemyInstance`]; see `enemies` above.)

    // ─── Player health + damage feedback (P5; see `world/combat.rs`) ──
    /// Player health / armor (JS `Actor`; armor-first damage). Death at health 0.
    player_health: f32,
    player_armor: f32,
    /// Dead — the YOU DIED screen is up; the sim freezes until a restart.
    player_dead: bool,
    /// Red full-screen damage-flash alpha (decays each frame).
    damage_flash: f32,
    /// Health-HUD pop timer (s); the radial HUD is shown while >0, fading over its
    /// last [`HUD_FADE_TAIL`].
    hud_show_timer: f32,
    /// The processed GoldenEye radial health graphic (angle/side maps), used to
    /// bake the HUD RGBA for the current health/armor. `None` if the JPEG failed.
    health_hud: Option<crate::hud::health::HealthHud>,

    // ─── Player Combat (HUNT-phase weapon; see `world/combat.rs`) ──
    /// P1: the first-person weapon's static gun mesh (CPU side), uploaded once to
    /// the renderer at startup. `None` if the asset failed to load.
    gun_model: Option<TexturedModel>,
    /// P2: the muzzle-flash mesh (separate GLB), uploaded once; drawn additively
    /// on top of the gun while a shot's flash is active. `None` if load failed.
    muzzle_model: Option<TexturedModel>,
    /// A3: the enemy weapon render library — the gun + muzzle meshes for the whole
    /// arsenal, loaded once and handed to the renderer so any hunter can draw any
    /// weapon (and the BUILD demo can preview each). Keyed by weapon name.
    enemy_weapon_lib: Vec<EnemyWeaponAsset>,
    /// The player's weapon inventory (JS `WeaponSystem.slots`) — one [`Weapon`]
    /// per `config::WEAPONS` entry, each keeping its own ammo/reload state so a
    /// swap resumes where you left off. `Q` / N64 `A` cycles [`weapon_index`].
    weapons: Vec<Weapon>,
    /// Index of the active weapon in [`weapons`] (JS `currentSlotIndex`).
    weapon_index: usize,
    /// Weapon-switch animation state (JS `WeaponSystem.cycleWeapon`). `switching`
    /// gates firing + re-entry; `switch_timer` runs `0..SWITCH_TIME` across the
    /// lower→raise dip; at the halfway point the mesh swaps to `switch_target`,
    /// `switch_swapped` latches, and `models_dirty` tells the app to re-upload the
    /// new gun/muzzle. See `world::combat::combat_step`.
    switching: bool,
    switch_target: usize,
    switch_timer: f32,
    switch_swapped: bool,
    /// Set when a switch swaps the active weapon's meshes mid-animation; the app
    /// drains it via `take_models_dirty` and re-uploads the viewmodel + muzzle.
    models_dirty: bool,
    /// P2: live hit sparks — a short-lived bright marker at each impact point, so
    /// wall hits read at the right spot. Decayed each frame in HUNT.
    sparks: Vec<Spark>,
    /// Explosives: live projectiles in flight (rocket / launched grenade / thrown
    /// grenade). Advanced + collision-swept each frame in `explosives_step`; a
    /// contact or fuse expiry detonates them. Empty in BUILD.
    projectiles: Vec<crate::combat::Projectile>,
    /// Explosives: live placed mines (proximity / timed / remote). Armed + trip-
    /// checked each frame in `mines_step`; a trip, timeout, or the Detonator sets
    /// them off. Empty in BUILD.
    mines: Vec<crate::combat::Mine>,
    /// Explosives: live explosion VFX bursts, decayed each frame.
    blasts: Vec<Blast>,
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
    /// Where the hunters materialise at G→HUNT — the fixed [`SPAWN_MARKER_POS`]
    /// snapped to a standable cell. Set by [`World::prepare_spawn`]; the wave clusters
    /// around it. HUNT only.
    spawn_point: Vec3,
    /// The fan-out search-point pool for the hunt (spread standable cells). The
    /// `World` hands these out to searching hunters so the pack sweeps the base
    /// instead of clumping. Rebuilt each G→HUNT, cleared on return to BUILD.
    search_points: Vec<Vec3>,
    regions: Vec<Region>,
    selected: Option<Selection>,
    /// Doors, populated at G→HUNT: the fixed **spawn-door seal** (a black
    /// non-breakable panel) plus (when re-enabled) breakable doors. Cleared on
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

        // Load the clip set bound to the character's skeleton in a FIXED index
        // order — locomotion 0–3, then one fire clip per weapon CLASS
        // (rifle/pistol/dual, indices FIRE_*_IDX), then the hit set, then the death
        // set (see CHAR_*_IDX) — into a template mixer. Each spawned hunter clones
        // this template so it animates on its own clock; the BUILD demo clones it too.
        let char_anim_template = char_model.as_ref().and_then(|m| {
            let mut files: Vec<&str> =
                vec!["00-idle.glb", "28-walking.glb", "2A-jogging.glb", "29-running.glb"];
            files.push("01-fire-standing.glb"); // FIRE_RIFLE_IDX
            files.push("41-fire-standing-pistol.glb"); // FIRE_PISTOL_IDX
            files.push("7A-fire-standing-dual-wield.glb"); // FIRE_DUAL_IDX
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
                log::info!(
                    "loaded {} character clips (idle/walk/jog/run + rifle/pistol/dual fire + 12 hit + 17 death)",
                    clips.len()
                );
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
        let char_feet_offset = match (&char_model, char_anim_template.as_ref().and_then(|a| a.clip(0))) {
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

        // Player Combat: build the full weapon inventory (JS `ALL_WEAPONS`) and
        // load the *active* weapon's gun + muzzle-flash meshes. The rest of the
        // guns load their meshes lazily on the first switch (see `cycle_weapon`) —
        // startup only pays for PP7 (index 0). Warn-not-panic if an asset is
        // missing. All GLBs live under `native/assets/weapons/`.
        let weapons: Vec<Weapon> = crate::combat::config::WEAPONS
            .iter()
            .map(|&cfg| Weapon::new(cfg))
            .collect();
        // Start on the PP7 (the default sidearm). Cycle (Q / N64 A) reaches the rest
        // of the arsenal — rifles, the grenades, the mines, the Detonator.
        let weapon_index = crate::combat::config::WEAPONS
            .iter()
            .position(|w| w.name == "PP7")
            .unwrap_or(0);
        let (gun_model, muzzle_model) = load_weapon_models(weapons[weapon_index].config());

        // P5: the GoldenEye radial health HUD graphic (processed once into angle/
        // side maps). Warn-not-panic if the JPEG is missing.
        let health_hud = {
            let p = format!("{}/../../assets/hud/goldeneye-health.jpg", env!("CARGO_MANIFEST_DIR"));
            match crate::hud::health::HealthHud::load(&p) {
                Some(h) => {
                    log::info!("loaded health HUD graphic {}×{}", h.w, h.h);
                    Some(h)
                }
                None => {
                    log::warn!("health HUD graphic load failed");
                    None
                }
            }
        };

        // A3: the enemy weapon render library — load the gun + muzzle-flash meshes
        // for the WHOLE arsenal once, so any hunter can wield any weapon (attached
        // to a hand bone in world space) and the BUILD demo can preview each. Same
        // static-textured loaders as the player gun (the flash keeps only the
        // additive `CullBoth` billboards). Warn-not-panic per weapon.
        let asset = |rel: &str| format!("{}/../../assets/weapons/{}", env!("CARGO_MANIFEST_DIR"), rel);
        let mut enemy_weapon_lib: Vec<EnemyWeaponAsset> = Vec::new();
        for cfg in crate::combat::config::WEAPONS {
            let gun = match crate::combat::load_gun(&asset(cfg.gun_path)) {
                Ok(m) => m,
                Err(e) => {
                    log::warn!("enemy weapon '{}' gun load failed: {e}", cfg.name);
                    continue; // no gun mesh → this weapon can't be drawn on a hunter
                }
            };
            let muzzle = if cfg.muzzle_path.is_empty() {
                None
            } else {
                match crate::combat::load_flash(&asset(cfg.muzzle_path)) {
                    Ok(m) => Some(m),
                    Err(e) => {
                        log::warn!("enemy weapon '{}' muzzle load failed: {e}", cfg.name);
                        None
                    }
                }
            };
            enemy_weapon_lib.push(EnemyWeaponAsset { name: cfg.name, gun, muzzle });
        }
        log::info!("loaded {} enemy weapon meshes", enemy_weapon_lib.len());

        World {
            camera,
            physics: PhysicsWorld::new(),
            mode: Mode::Build,
            character: None,
            nav: None,
            enemies: Vec::new(),
            spawn_enemies: true,
            char_model,
            char_anim_template,
            char_rng: 0x9E37_79B9_7F4A_7C15,
            char_feet_offset,
            player_health: PLAYER_MAX_HEALTH,
            player_armor: 0.0,
            player_dead: false,
            damage_flash: 0.0,
            hud_show_timer: 0.0,
            health_hud,
            gun_model,
            muzzle_model,
            enemy_weapon_lib,
            weapons,
            weapon_index,
            switching: false,
            switch_target: 0,
            switch_timer: 0.0,
            switch_swapped: false,
            models_dirty: false,
            sparks: Vec::new(),
            projectiles: Vec::new(),
            mines: Vec::new(),
            blasts: Vec::new(),
            aim_x: 0.0,
            aim_y: 0.0,
            aiming: false,
            audio: None,
            caught: false,
            spawn_point: SPAWN_MARKER_POS,
            search_points: Vec::new(),
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

    /// Enable/disable spawning the [`ENEMY_ROSTER`] on G→HUNT (dev convenience). Off
    /// = hunts start with no hunters, so you can test explosives without being shot.
    /// On by default; the app turns it off while iterating on explosives.
    pub fn set_spawn_enemies(&mut self, on: bool) {
        self.spawn_enemies = on;
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

    /// The enemy spawn-point marker: a flat colored square laid on the floor at the
    /// fixed [`SPAWN_MARKER_POS`], drawn in **both** BUILD and HUNT so the level can
    /// be authored around a consistent, visible enemy-ingress point. A thin raised
    /// tile (via [`push_colored_box`]) through the depth-tested spark pipeline.
    pub fn spawn_marker_mesh(&self) -> Option<ColoredMesh> {
        let c = SPAWN_MARKER_POS;
        let min = Vec3::new(c.x - SPAWN_MARKER_HALF, c.y + 0.01, c.z - SPAWN_MARKER_HALF);
        let max = Vec3::new(c.x + SPAWN_MARKER_HALF, c.y + 0.05, c.z + SPAWN_MARKER_HALF);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        push_colored_box(&mut vertices, &mut indices, min, max, SPAWN_MARKER_COLOR);
        Some(ColoredMesh { vertices, indices })
    }
}
