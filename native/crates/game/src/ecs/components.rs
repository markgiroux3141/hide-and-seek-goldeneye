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
    Door,
    WoodenCrate,
    ExplosiveBarrel,
    FilingCabinet,
    Bookshelf,
    HeavyWoodenTable,
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
