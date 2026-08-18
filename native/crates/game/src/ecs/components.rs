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

/// How a door animates open.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpeningType {
    /// Hinged swing about one edge.
    Swing,
    /// Slides sideways into a wall pocket.
    Slide,
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

/// The exemplar prop: a door that opens. **Inert this pass** — defined as data and
/// persisted so the scaffold has a real multi-component consumer, but nothing
/// drives `state`/`open_frac` yet. The door task adds the system that animates it
/// and makes the act of opening slow a passing enemy (via the nav `DOOR_COST`
/// overlay in [`engine::sim::nav`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Door {
    pub state: DoorState,
    pub opening_type: OpeningType,
    /// Open fraction, 0 (shut) → 1 (fully open); driven by the future door system.
    pub open_frac: f32,
}

impl Door {
    /// A closed door of the given opening style.
    pub fn new(opening_type: OpeningType) -> Self {
        Door { state: DoorState::Closed, opening_type, open_frac: 0.0 }
    }
}
