//! The component library — plain-data structs stored in the ECS world.
//!
//! Components carry **data only**, no behaviour; systems ([`super::systems`]) act
//! on them. Every *persistable* component has a matching
//! [`super::persist::ComponentData`] variant plus fold/extract arms so it survives
//! save/load. This is the composition-first payoff: a door that is *also*
//! destructible *also* a link target is just three components on one entity, never
//! a new archetype enum.
//!
//! Scaffold pass: the set below is a starter kit. [`Door`] is defined and persisted
//! but **inert** — nothing drives its state yet (that's the follow-up door task).

use glam::{Mat4, Quat, Vec3};
use serde::{Deserialize, Serialize};

/// World placement — the component nearly every entity carries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub pos: Vec3,
    pub rot: Quat,
    pub scale: Vec3,
}

impl Transform {
    /// Identity rotation + unit scale at `pos`.
    pub fn from_pos(pos: Vec3) -> Self {
        Transform { pos, rot: Quat::IDENTITY, scale: Vec3::ONE }
    }

    /// Model matrix (translation·rotation·scale) for rendering / collider placement.
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rot, self.pos)
    }
}

impl Default for Transform {
    fn default() -> Self {
        Transform::from_pos(Vec3::ZERO)
    }
}

/// Render-seam data: which mesh this entity draws with. The actual GPU draw path —
/// a generic instanced static-mesh channel on the renderer — lands with the door
/// task; for now this only *names* the mesh so the component set + persistence are
/// already in place when that channel arrives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Renderable {
    pub mesh: MeshId,
}

/// Handle into the prop-mesh catalog — a small stable id rather than a path, so the
/// render channel can preload + index meshes once. The per-variant metadata (display
/// name, category, GLB path, scale, destructible blast) lives in [`crate::props`];
/// this enum is just the stable key that persists in the level file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MeshId {
    /// The inert door-mechanics scaffold entity (not a catalog prop). The placeable
    /// door *props* below (e.g. [`MeshId::MetalDoor`]) are static scenery for now.
    Door,
    // ── Destructible ──
    WoodenCrate,
    ExplosiveBarrel,
    MetalCrate,
    AmmoCrate,
    CardboardCrate,
    GasCan,
    // ── Furniture ──
    FilingCabinet,
    Bookshelf,
    HeavyWoodenTable,
    WoodenTable,
    BlackTable,
    BlackChair,
    BlueChair,
    Locker,
    MetalSafe,
    MetalCrateStack,
    Divider,
    ChainlinkFence,
    MetalGrate,
    GreyTable,
    // ── Electronics ──
    Console,
    Console1,
    Mainframe,
    Tv,
    Tv1,
    Keyboard,
    Radio,
    RackDevice,
    WallDisplay,
    SecurityCamera,
    SentryGun,
    // ── Clutter ──
    TrashCans,
    Barricade,
    Beaker,
    Book,
    Calculator,
    Alarm,
    Lamp,
    TestTubes,
    BodyArmour,
    // ── Pickups ──
    /// A gun lying on the ground. **Not a catalog prop**: the mesh is chosen by the
    /// [`Pickup`]'s weapon name and drawn from the world-space weapon render
    /// library (the one the hunters' guns already use), so there is one mesh id for
    /// every gun in the arsenal rather than one per weapon.
    WeaponPickup,
    /// An ammo crate — the GoldenEye tan box (`ammo_crate.glb`, shared with the
    /// destructible [`MeshId::AmmoCrate`] prop) as a collectable.
    AmmoPickupTan,
    /// An ammo crate — the green Setup-Editor crate. A purely visual alternative to
    /// [`MeshId::AmmoPickupTan`]; the rounds it holds are authored, not implied.
    AmmoPickupGreen,
    // ── Doors (static props; mechanics later) ──
    MetalDoor,
    MetalDoor2,
    BrownSlidingDoor,
    BlastDoor,
    WoodenDoor,
    ElevatorDoor,
    GreyDoor,
    BigMetalDoor,
    BathroomDoor,
    JailDoor,
    MetalSafeDoor,
    GlassDoor,
}

/// A placed omnidirectional point light. Authored in BUILD; drives the real
/// lighting pass. A light entity is just [`Transform`] + `PointLight` — no
/// [`Renderable`] (lights have no mesh; a build-mode billboard marker stands in for
/// selection/picking). Persisted like any other authored component, so lights ride
/// the level file's `entities` collection with props.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointLight {
    /// Linear-RGB colour, 0..1 per channel (white = neutral).
    pub color: Vec3,
    /// Brightness multiplier — scales this light's contribution before attenuation.
    pub intensity: f32,
    /// Falloff radius in metres; the contribution fades to ~zero at this distance.
    pub range: f32,
}

impl Default for PointLight {
    fn default() -> Self {
        PointLight { color: Vec3::ONE, intensity: 1.0, range: 8.0 }
    }
}

/// An authored spawn pad: somewhere a player or a simulant can enter the level.
///
/// Perfect Dark's equivalent is a *pad* listed in the stage setup's `INTROCMD_SPAWN`
/// commands (`playerreset.c:171`), and both sides draw from that one list —
/// `bot_spawn` (`bot.c:288`) and `player_start_new_life` (`player.c:528`) both call
/// `scenario_choose_spawn_location`. So this is one shared pool, not a
/// player-pool/simulant-pool pair.
///
/// Like [`PointLight`], a pad entity is just [`Transform`] + `SpawnPoint` with **no**
/// [`Renderable`] — it has no mesh, so it rides the level file's `entities` collection
/// with no schema change and is naturally skipped by every prop path (draw list,
/// colliders, nav) that queries `Renderable`.
///
/// **Facing lives in the [`Transform`]'s rotation**, not in a field here: PD's pads
/// carry a `look` vector and `player_choose_spawn_location` returns
/// `atan2f(pad.look.x, pad.look.z)` as the spawn angle (`player.c:355`). Storing it as
/// the transform yaw means the existing rotate gizmo authors the facing for free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpawnPoint;

/// Level-wide ambient fill. **Not a component** — a single global carried on the
/// level (and its file), since ambient light has no position. `level` scales
/// `color` into a flat term added to every lit surface so shadowed areas aren't
/// pure black. Serialized directly into the level file (see `world::persist`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AmbientSettings {
    /// Linear-RGB ambient colour, 0..1 per channel.
    pub color: [f32; 3],
    /// Ambient strength, 0 (black) .. 1 (full `color`).
    pub level: f32,
}

impl Default for AmbientSettings {
    fn default() -> Self {
        // A dim neutral fill: lit areas read clearly while unlit corners stay dark
        // enough that placed lights matter. Authorable per level.
        AmbientSettings { color: [1.0, 1.0, 1.0], level: 0.15 }
    }
}

/// Hit points for anything damageable. Props will use this once the damage system
/// exists; enemies keep their own `health` field until they migrate. Data only.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Health {
    pub hp: f32,
    pub max: f32,
}

impl Health {
    /// Full health with capacity `max`.
    pub fn full(max: f32) -> Self {
        Health { hp: max, max }
    }
}

/// Transient "this prop has been blown up" marker (Milestone 3). Added at the moment
/// a destructible prop's [`Health`] reaches zero and stripped when HUNT ends, so the
/// authored prop returns intact in BUILD. **Not persisted** (no [`super::persist::ComponentData`]
/// arm): a destroyed crate is a runtime-only state, never part of the saved level. A
/// destroyed prop is skipped by the render draw list and has no collider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Destroyed;

/// A live auto-turret's articulation and firing state.
///
/// **Not persisted** (no [`super::persist::ComponentData`] arm), for the same reason
/// [`DoorGeom`] isn't: every field is runtime. A sentry gun in the level file is a
/// plain `Transform` + `Renderable`; this is attached to each one at BUILD→HUNT and
/// stripped on the way back, so the editor always shows the turret parked at rest and
/// no level file has to change to gain a working turret.
///
/// Angles are in **rig space** — the prop's own frame, before its authored rotation —
/// so [`crate::turret`] can build the draw matrices from them directly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Turret {
    /// Rotation about the mount's vertical axis, radians. Unlimited: it hangs on a ring.
    pub yaw: f32,
    /// Elevation about the trunnion, radians, clamped to
    /// [`crate::turret::PITCH_MIN`]..[`crate::turret::PITCH_MAX`].
    pub pitch: f32,
    /// Barrel-bundle angle about the bore, radians, wrapped to one turn.
    pub spin: f32,
    /// Current barrel speed, radians/sec — ramps up while a target is held and coasts
    /// down when it is lost. The gun will not fire until this is near full, which is
    /// the turret's spin-up tell and the player's (and the hunters') warning.
    pub spin_speed: f32,
    /// Roster index of the hunter being tracked, if any.
    pub target: Option<usize>,
    /// Seconds until the next round may leave the barrel.
    pub cooldown: f32,
    /// Seconds until the turret looks for a better target.
    pub reacquire: f32,
}

impl Default for Turret {
    fn default() -> Self {
        Turret {
            yaw: 0.0,
            pitch: 0.0,
            spin: 0.0,
            spin_speed: 0.0,
            target: None,
            cooldown: 0.0,
            reacquire: 0.0,
        }
    }
}

/// What a [`Pickup`] hands over when the player walks onto it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PickupKind {
    /// The gun itself: grants ownership, a loaded magazine and some reserve.
    Weapon,
    /// A crate of rounds for one weapon: reserve only.
    Ammo,
}

/// A weapon or ammo crate lying on the ground for the player to collect.
///
/// The deathmatch pivot in one component (`DESIGN_PICKUPS.md`): you spawn holding
/// nothing, and the level is stocked with these. Placed as an ordinary authored
/// entity, so it inherits the whole object pipeline — palette, ghost, gizmos,
/// duplicate, undo, persistence — with no new tooling.
///
/// # The weapon is NAMED, not indexed
///
/// `weapon` is an arsenal **display name**, matching what [`crate::shop`] prices and
/// what [`crate::combat::enemy_def_for`] resolves — both name-keyed on purpose, so
/// that reordering a weapon table cannot silently repoint authored data. It stays a
/// `&'static str` (keeping this `Copy`) because
/// [`crate::combat::arsenal::resolve_name`] hands back the table's own string, and it
/// resolves across **both** weapon families so a level authored under one arsenal
/// still loads under the other.
///
/// # What persists
///
/// Everything except `cooldown`, which is HUNT-only runtime — an authored level
/// always opens with every pickup sitting on the floor, the same rule
/// [`Turret`] and [`DoorGeom`] follow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pickup {
    pub kind: PickupKind,
    /// Which weapon this is, or which weapon's rounds it holds.
    pub weapon: &'static str,
    /// Magazines granted. For a [`PickupKind::Weapon`] this is the reserve handed
    /// over *on top of* the full magazine it arrives loaded with.
    pub mags: u32,
    /// Seconds until it returns after being taken. `0.0` = gone for the round.
    pub respawn: f32,
    /// Runtime: seconds left until it comes back. `> 0.0` means taken — the draw
    /// list skips it and it cannot be collected again. Never persisted.
    pub cooldown: f32,
}

/// Default reserve a weapon pickup hands over, in magazines. Two spare mags is
/// enough to fight with and few enough that ammo crates still matter.
pub const PICKUP_WEAPON_MAGS: u32 = 2;
/// Default rounds in an ammo crate, in magazines of the weapon it feeds.
pub const PICKUP_AMMO_MAGS: u32 = 3;
/// Default respawn for a weapon pickup, in seconds — long enough that taking the
/// good gun is worth something, short enough that the level keeps flowing.
pub const PICKUP_WEAPON_RESPAWN: f32 = 20.0;
/// Default respawn for an ammo crate. Shorter than a weapon's: rounds are the
/// consumable, so they should come back faster than the gun that eats them.
pub const PICKUP_AMMO_RESPAWN: f32 = 10.0;

impl Pickup {
    /// A weapon lying on the ground, with the authoring defaults.
    pub fn weapon(name: &'static str) -> Self {
        Pickup {
            kind: PickupKind::Weapon,
            weapon: name,
            mags: PICKUP_WEAPON_MAGS,
            respawn: PICKUP_WEAPON_RESPAWN,
            cooldown: 0.0,
        }
    }

    /// An ammo crate feeding `name`, with the authoring defaults.
    pub fn ammo(name: &'static str) -> Self {
        Pickup {
            kind: PickupKind::Ammo,
            weapon: name,
            mags: PICKUP_AMMO_MAGS,
            respawn: PICKUP_AMMO_RESPAWN,
            cooldown: 0.0,
        }
    }

    /// Whether it has been taken and is waiting to come back (or gone for good).
    pub fn taken(&self) -> bool {
        self.cooldown > 0.0
    }
}

/// Marker + parameters for something the player can "use" (a door, a terminal).
/// The interaction input + resolution are future work; this reserves the shape so
/// interactable props persist their reach from the start.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Interactable {
    /// Interaction reach in metres (how close the player must be to use it).
    pub radius: f32,
}

impl Default for Interactable {
    fn default() -> Self {
        Interactable { radius: 1.5 }
    }
}

/// How a door animates open. Mirrors [`crate::props::DoorMotion`], which is what the
/// catalog fixes per model; this is the *persisted* copy on the placed entity, so a
/// level file records the motion it was authored with even if a catalog row changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpeningType {
    /// Hinged swing about one vertical edge.
    Swing,
    /// Slides sideways into a wall pocket.
    Slide,
    /// Rises vertically — a shutter / vehicle blast door.
    Shutter,
}

/// Which vertical edge of the panel a swing door pivots about. Resolved against the
/// panel's own width axis (the wider of local X/Z — measured to differ per model), so
/// it means the same thing whichever way the door was rotated when placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HingeSide {
    Left,
    Right,
}

/// Who is allowed to open a door. Not a lock in the key-and-keyhole sense — an
/// authoring lever specific to this game: a door only the player can open is a hiding
/// advantage, one only hunters can open is a trap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DoorAccess {
    Both,
    PlayerOnly,
    HuntersOnly,
    /// Sealed — opens for nobody. Rattles when used.
    Locked,
}

/// Runtime open/close state of a door. Not persisted — an authored door always
/// begins [`DoorState::Closed`]; this is derived at runtime by the door system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoorState {
    Closed,
    Opening,
    Open,
    Closing,
}

/// A door that opens. Placed as an ordinary prop (so it inherits the whole object
/// pipeline — palette, ghost, gizmos, snap, duplicate, undo, persistence) and given
/// this component at bake from [`crate::props::door_def`].
///
/// The fields split three ways: **authored** (persisted, edited in the object panel),
/// **catalog-derived** (`opening_type`, copied at bake), and **runtime**
/// (`state`/`open_frac`/`timer`, never persisted — an authored door always loads shut).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Door {
    // ── runtime ──
    pub state: DoorState,
    /// Open fraction, 0 (shut) → 1 (fully open). Drives both the draw matrix and the
    /// collider pose.
    pub open_frac: f32,
    /// Counts down the auto-close dwell while [`DoorState::Open`].
    pub timer: f32,
    // ── catalog-derived ──
    pub opening_type: OpeningType,
    // ── authored ──
    pub hinge: HingeSide,
    /// Reverse the motion: swings the other way / slides to the other side. Together
    /// with `hinge` this covers all four swing configurations.
    pub flip: bool,
    /// Mirror the panel's artwork across its width. The mesh is reflected about the
    /// panel's own centre, so the hinge and collider are untouched — only which side
    /// the handle and any decals appear on changes. This is what makes the two leaves
    /// of a double door read as a matched pair meeting in the middle, rather than two
    /// copies of the same door.
    pub mirrored: bool,
    /// How far a swing door opens, in radians.
    pub open_angle: f32,
    /// How far a sliding door travels, in metres. `0.0` = auto (the panel's own width
    /// for a slide, its height for a shutter), which is what a door in a wall pocket
    /// wants and what every catalog default uses.
    pub slide_distance: f32,
    /// Animation rate multiplier.
    pub speed: f32,
    /// Seconds to dwell fully open before closing itself. `0.0` = stays open.
    pub auto_close: f32,
    /// How close an actor must be to work the door, in metres.
    pub use_radius: f32,
    pub access: DoorAccess,
}

/// A door's resolved panel geometry plus its live collider handle — everything the
/// door system needs to animate it, baked once at BUILD→HUNT so the per-step tick
/// touches nothing outside the ECS.
///
/// **Transient, never persisted.** It is derived from the prop's model-space AABB
/// (registered from the GLB at startup), the authored [`Transform`], and the [`Door`]
/// settings, so it is always reconstructable and would only go stale in a save file.
///
/// The derivation is the part the *assets* cannot supply: every door GLB is a single
/// unnamed mesh with no frame and no authored pivot, and — measured — the panel's thin
/// axis is **X** on some models (`metal_door`, `grey_door`, `wooden_door`) and **Z** on
/// the rest, with four models not even centred on their width axis. So the width axis
/// is chosen per model and the hinge is taken from the AABB edge, never the origin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DoorGeom {
    /// Model-space pivot: on the hinge edge of the width axis, at the panel's base.
    pub hinge: Vec3,
    /// Model-space unit vector along the panel's width (either `X` or `Z`).
    pub width_axis: Vec3,
    /// Panel width in world metres (after the prop's scale).
    pub width: f32,
    /// Panel height in world metres.
    pub height: f32,
    /// Model-space centre of the panel — the collider's anchor point.
    pub center: Vec3,
    /// The prop placement anchor (horizontal centre + vertical base) this mesh draws
    /// about. Cached here so the per-step tick can rebuild the world matrix without
    /// reaching back into the world's mesh-bounds table.
    pub anchor: Vec3,
    /// World half-extents of the panel box, in its own frame.
    pub half: Vec3,
    /// The moving collider, if one was baked (absent headless, where no GLB bounds are
    /// registered and so no panel geometry exists).
    pub collider: Option<rapier3d::prelude::ColliderHandle>,
    /// This door's index in the nav door overlay, if it was registered there. The door
    /// system flips the overlay's open flag through it, which is what lets a hunter's
    /// A\* reroute the instant a door swings — with no nav re-bake.
    pub nav_index: Option<usize>,
}

/// Default swing arc — a quarter turn, as in the reference implementation.
pub const DOOR_OPEN_ANGLE: f32 = std::f32::consts::FRAC_PI_2;
/// Default dwell before a door shuts itself, in seconds.
pub const DOOR_AUTO_CLOSE: f32 = 4.0;
/// Default reach to work a door, in metres. Generous enough that you don't have to be
/// touching the panel, tight enough that you can't open one through a wall.
pub const DOOR_USE_RADIUS: f32 = 2.0;

impl Door {
    /// A closed door of the given motion, with catalog defaults for everything the
    /// author hasn't touched.
    pub fn new(opening_type: OpeningType) -> Self {
        Door {
            state: DoorState::Closed,
            open_frac: 0.0,
            timer: 0.0,
            opening_type,
            hinge: HingeSide::Left,
            flip: false,
            mirrored: false,
            open_angle: DOOR_OPEN_ANGLE,
            slide_distance: 0.0,
            speed: 1.0,
            auto_close: DOOR_AUTO_CLOSE,
            use_radius: DOOR_USE_RADIUS,
            access: DoorAccess::Both,
        }
    }

    /// Whether the panel is currently shut enough to block a doorway — the test both
    /// the LOS collider and the nav overlay key off. A door reads as blocking until it
    /// is meaningfully ajar, so cracking one open doesn't instantly expose you.
    pub fn is_blocking(&self) -> bool {
        self.open_frac < 0.5
    }

    /// Whether `is_hunter` may work this door.
    pub fn opens_for(&self, is_hunter: bool) -> bool {
        match self.access {
            DoorAccess::Both => true,
            DoorAccess::PlayerOnly => !is_hunter,
            DoorAccess::HuntersOnly => is_hunter,
            DoorAccess::Locked => false,
        }
    }
}
