//! Authored-entity persistence: the plain-data serde form of an entity and the
//! translation both ways between it and the live hecs world.
//!
//! An authored entity is a **list of components**, not a fixed archetype — so
//! adding a persistable component is a strictly-local change: one
//! [`ComponentData`] variant + one arm in [`fold_one`] (data → world) + one arm in
//! [`extract`] (world → data). Nothing else in the file format shifts.
//!
//! The stable [`AuthoredId`] rides along as a component on the entity (it doubles
//! as the "this entity is authored, persist it" marker), so [`save_authored`] can
//! recover each entity's id when writing it back out.

use glam::{Quat, Vec3};
use hecs::{Entity, EntityBuilder, EntityRef, World as HecsWorld};
use serde::{Deserialize, Serialize};

use super::components::*;
use super::AuthoredId;

/// One authored entity in its on-disk form: a stable id + its components.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EntityData {
    pub id: AuthoredId,
    pub components: Vec<ComponentData>,
}

/// The serde form of one persistable component. `#[serde(tag = "type")]` yields
/// readable, hand-editable JSON, e.g. `{"type":"Door","opening_type":"Swing"}`.
/// Exactly one variant per persistable component type.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ComponentData {
    Transform { pos: [f32; 3], rot: [f32; 4], scale: [f32; 3] },
    Health { hp: f32, max: f32 },
    Interactable { radius: f32 },
    Door { opening_type: OpeningType },
    Renderable { mesh: MeshId },
    PointLight { color: [f32; 3], intensity: f32, range: f32 },
    /// A spawn pad. No payload — its position *and facing* are the entity's
    /// [`Transform`] (see [`SpawnPoint`]), so the marker component is a bare tag.
    SpawnPoint,
}

/// Spawn one authored entity into `world` from its data, returning the live handle.
/// Attaches the [`AuthoredId`] component (the authored marker) then folds each
/// [`ComponentData`]. The caller ([`super::Ecs`]) owns the id-map bookkeeping.
pub fn spawn_from(world: &mut HecsWorld, data: &EntityData) -> Entity {
    let mut b = EntityBuilder::new();
    b.add(data.id); // AuthoredId doubles as the "authored — persist me" marker.
    for c in &data.components {
        fold_one(&mut b, c);
    }
    world.spawn(b.build())
}

/// Fold one component's data onto the builder. The data → world half of the map.
fn fold_one(b: &mut EntityBuilder, c: &ComponentData) {
    match c {
        ComponentData::Transform { pos, rot, scale } => {
            b.add(Transform {
                pos: Vec3::from(*pos),
                rot: Quat::from_array(*rot),
                scale: Vec3::from(*scale),
            });
        }
        ComponentData::Health { hp, max } => {
            b.add(Health { hp: *hp, max: *max });
        }
        ComponentData::Interactable { radius } => {
            b.add(Interactable { radius: *radius });
        }
        ComponentData::Door { opening_type } => {
            b.add(Door::new(*opening_type));
        }
        ComponentData::Renderable { mesh } => {
            b.add(Renderable { mesh: *mesh });
        }
        ComponentData::PointLight { color, intensity, range } => {
            b.add(PointLight { color: Vec3::from(*color), intensity: *intensity, range: *range });
        }
        ComponentData::SpawnPoint => {
            b.add(SpawnPoint);
        }
    }
}

/// Serialize every authored entity in `world` (those carrying an [`AuthoredId`]),
/// in ascending-id order for a stable, diff-friendly file.
pub fn save_authored(world: &HecsWorld) -> Vec<EntityData> {
    // Snapshot the authored handles first so the query borrow is released before
    // we re-borrow each entity to extract its components.
    let authored: Vec<(Entity, AuthoredId)> = world
        .query::<(Entity, &AuthoredId)>()
        .iter()
        .map(|(e, id)| (e, *id))
        .collect();
    let mut out: Vec<EntityData> = authored
        .into_iter()
        .map(|(e, id)| {
            let eref = world.entity(e).expect("entity just enumerated is alive");
            EntityData { id, components: extract(&eref) }
        })
        .collect();
    out.sort_by_key(|ed| ed.id.0);
    out
}

/// Extract one entity's persistable components into data. The world → data half of
/// the map; the inverse of [`fold_one`]. Order here sets the field order in the
/// JSON, so keep [`ComponentData::Transform`] first for readability.
pub(crate) fn extract(eref: &EntityRef<'_>) -> Vec<ComponentData> {
    let mut cs = Vec::new();
    if let Some(t) = eref.get::<&Transform>() {
        cs.push(ComponentData::Transform {
            pos: t.pos.to_array(),
            rot: t.rot.to_array(),
            scale: t.scale.to_array(),
        });
    }
    if let Some(d) = eref.get::<&Door>() {
        cs.push(ComponentData::Door { opening_type: d.opening_type });
    }
    if let Some(r) = eref.get::<&Renderable>() {
        cs.push(ComponentData::Renderable { mesh: r.mesh });
    }
    if let Some(l) = eref.get::<&PointLight>() {
        cs.push(ComponentData::PointLight {
            color: l.color.to_array(),
            intensity: l.intensity,
            range: l.range,
        });
    }
    if eref.has::<SpawnPoint>() {
        cs.push(ComponentData::SpawnPoint);
    }
    if let Some(i) = eref.get::<&Interactable>() {
        cs.push(ComponentData::Interactable { radius: i.radius });
    }
    if let Some(h) = eref.get::<&Health>() {
        cs.push(ComponentData::Health { hp: h.hp, max: h.max });
    }
    cs
}
