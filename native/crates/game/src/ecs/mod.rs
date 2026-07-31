//! The game's entity-component layer (hecs).
//!
//! `engine` is a domain-free toolkit, so every component and system here lives in
//! the `game` crate and the one-way `engine ← game` dependency is preserved. hecs
//! gives archetypal storage + queries with no scheduler; systems are plain ordered
//! functions ticked from [`Ecs::run_systems`], mirroring the explicit fixed-step
//! pipeline in [`crate::world`].
//!
//! This is the scaffold pass: the component library ([`components`]) + the
//! authored-entity persistence round-trip ([`persist`]) are live, but the system
//! list is an inert no-op ([`systems`]) and nothing is rendered yet — a door's
//! behaviour and a renderer channel land in the follow-up "door" task. The
//! [`components::Door`] type is defined + persisted now so the scaffold has a real
//! multi-component consumer to exercise the seams.

use std::collections::HashMap;

use hecs::{Entity, World as HecsWorld};
use serde::{Deserialize, Serialize};

pub mod components;
pub mod persist;
pub mod systems;

#[cfg(test)]
mod tests;

pub use components::{Door, DoorState, Health, Interactable, MeshId, OpeningType, Renderable, Transform};
pub use persist::{ComponentData, EntityData};
pub use systems::{Command, SystemCtx};

/// A stable, authoring-time id for an entity that persists to the level file.
///
/// hecs [`Entity`] handles are runtime-only — reused after a despawn and never
/// stable across a save/load — so anything the *authored* data must reference (a
/// terminal that unlocks a specific door, a patrol that targets a marker) keys off
/// this id instead, resolved to the live entity at load via [`Ecs::resolve`]. It is
/// stored as a component on its entity, where it also serves as the "this entity is
/// authored, persist it" marker.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct AuthoredId(pub u32);

/// The game's entity store: a hecs world plus the authored-id ↔ live-entity map and
/// the authoring-id allocator. Held as a field on [`crate::world::World`]; present
/// in both BUILD and HUNT, though gameplay systems only tick during HUNT.
pub struct Ecs {
    world: HecsWorld,
    /// authored id → live entity, for cross-entity links + load resolution.
    id_map: HashMap<AuthoredId, Entity>,
    /// Monotonic authoring-id allocator (never reuses an id within a session).
    next_authored: u32,
}

impl Default for Ecs {
    fn default() -> Self {
        Self::new()
    }
}

impl Ecs {
    /// An empty store.
    pub fn new() -> Self {
        Ecs { world: HecsWorld::new(), id_map: HashMap::new(), next_authored: 1 }
    }

    /// Borrow the underlying hecs world for queries + ad-hoc spawns of *transient*
    /// runtime entities (effects, spawned-at-hunt actors) that never persist.
    pub fn world(&self) -> &HecsWorld {
        &self.world
    }

    /// Mutable access to the underlying hecs world (see [`Self::world`]).
    pub fn world_mut(&mut self) -> &mut HecsWorld {
        &mut self.world
    }

    /// Allocate the next stable authoring id.
    pub fn alloc_id(&mut self) -> AuthoredId {
        let id = AuthoredId(self.next_authored);
        self.next_authored += 1;
        id
    }

    /// Resolve a stable authoring id to its live entity, if still alive.
    pub fn resolve(&self, id: AuthoredId) -> Option<Entity> {
        self.id_map.get(&id).copied()
    }

    /// Total live entities (authored + transient).
    pub fn len(&self) -> usize {
        self.world.len() as usize
    }

    /// Whether the store holds no entities.
    pub fn is_empty(&self) -> bool {
        self.world.len() == 0
    }

    /// Tick the ordered system list for one fixed step. Inert this pass (empty
    /// list); door/prop systems slot into [`systems::run_systems`].
    pub fn run_systems(&mut self, ctx: &mut SystemCtx) {
        systems::run_systems(&mut self.world, ctx);
    }

    /// Spawn one authored entity from its data, registering its id. Used both by a
    /// placement tool (editor / levelgen) and by [`Self::load_authored`].
    pub fn spawn_authored(&mut self, data: &EntityData) -> Entity {
        let entity = persist::spawn_from(&mut self.world, data);
        self.id_map.insert(data.id, entity);
        self.next_authored = self.next_authored.max(data.id.0 + 1);
        entity
    }

    /// Duplicate an authored entity: copy **all** its persistable components onto a
    /// fresh entity with a new [`AuthoredId`], returning the new handle. `None` if
    /// `src` is gone. Used by the editor's duplicate (Shift+D); the caller typically
    /// nudges the copy's transform so it doesn't sit exactly on the original.
    pub fn duplicate_authored(&mut self, src: Entity) -> Option<Entity> {
        let components = {
            let eref = self.world.entity(src).ok()?;
            persist::extract(&eref)
        };
        let id = self.alloc_id();
        Some(self.spawn_authored(&EntityData { id, components }))
    }

    /// Serialize every authored entity to its plain-data form for the level file.
    /// Transient runtime entities (no [`AuthoredId`]) are never written.
    pub fn save_authored(&self) -> Vec<EntityData> {
        persist::save_authored(&self.world)
    }

    /// Replace all authored entities with those described by `data`, rebuilding the
    /// id map. A load *replaces* the authored set: existing authored entities are
    /// despawned first; any transient entities are left untouched.
    pub fn load_authored(&mut self, data: &[EntityData]) {
        let existing: Vec<Entity> = self
            .world
            .query::<(Entity, &AuthoredId)>()
            .iter()
            .map(|(e, _)| e)
            .collect();
        for e in existing {
            let _ = self.world.despawn(e);
        }
        self.id_map.clear();
        for ed in data {
            self.spawn_authored(ed);
        }
    }
}
