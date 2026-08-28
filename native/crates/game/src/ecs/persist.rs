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
    /// A door's *authored* settings. Every field past `opening_type` carries a
    /// `#[serde(default)]`, so a level written before doors gained their authoring
    /// options still loads — each missing field falls back to the same catalog default
    /// a freshly-placed door gets. That is why this needed no level-format bump.
    Door {
        opening_type: OpeningType,
        #[serde(default = "default_hinge")]
        hinge: HingeSide,
        #[serde(default)]
        flip: bool,
        #[serde(default)]
        mirrored: bool,
        #[serde(default = "default_open_angle")]
        open_angle: f32,
        #[serde(default)]
        slide_distance: f32,
        #[serde(default = "default_speed")]
        speed: f32,
        #[serde(default = "default_auto_close")]
        auto_close: f32,
        #[serde(default = "default_use_radius")]
        use_radius: f32,
        #[serde(default = "default_access")]
        access: DoorAccess,
    },
    Renderable { mesh: MeshId },
    PointLight { color: [f32; 3], intensity: f32, range: f32 },
    /// A spawn pad. No payload — its position *and facing* are the entity's
    /// [`Transform`] (see [`SpawnPoint`]), so the marker component is a bare tag.
    SpawnPoint,
    /// A climbable ladder. Both fields carry a `#[serde(default)]` for the same reason
    /// the door's authoring options do — a level written before ladders gained a setting
    /// still loads, falling back to the freshly-placed default. No format bump.
    Ladder {
        #[serde(default = "default_ladder_height")]
        height: f32,
        #[serde(default = "default_ladder_width")]
        width: f32,
    },
    /// A weapon / ammo pickup. `weapon` is an owned `String` here (and a
    /// `&'static str` on the live [`Pickup`]) because the file has to record what
    /// the author chose even if that weapon isn't in the arsenal this session —
    /// see [`crate::combat::arsenal::resolve_name`]. The runtime `cooldown` is
    /// absent by design: an authored level opens with every pickup on the floor.
    Pickup { kind: PickupKind, weapon: String, mags: u32, respawn: f32 },
}

impl ComponentData {
    /// A default-configured door of `opening_type` — the data form of [`Door::new`].
    /// Handy for levelgen and tests, which care about the motion and nothing else.
    pub fn door(opening_type: OpeningType) -> Self {
        let d = Door::new(opening_type);
        ComponentData::Door {
            opening_type,
            hinge: d.hinge,
            flip: d.flip,
            mirrored: d.mirrored,
            open_angle: d.open_angle,
            slide_distance: d.slide_distance,
            speed: d.speed,
            auto_close: d.auto_close,
            use_radius: d.use_radius,
            access: d.access,
        }
    }
}

// Serde fallbacks for the door options, so a pre-doors level file loads with exactly
// the defaults a freshly-placed door would get. They mirror `Door::new`.
fn default_hinge() -> HingeSide {
    HingeSide::Left
}
fn default_open_angle() -> f32 {
    DOOR_OPEN_ANGLE
}
fn default_ladder_height() -> f32 {
    Ladder::default().height
}
fn default_ladder_width() -> f32 {
    Ladder::default().width
}

fn default_speed() -> f32 {
    1.0
}
fn default_auto_close() -> f32 {
    DOOR_AUTO_CLOSE
}
fn default_use_radius() -> f32 {
    DOOR_USE_RADIUS
}
fn default_access() -> DoorAccess {
    DoorAccess::Both
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
        ComponentData::Door {
            opening_type,
            hinge,
            flip,
            mirrored,
            open_angle,
            slide_distance,
            speed,
            auto_close,
            use_radius,
            access,
        } => {
            b.add(Door {
                hinge: *hinge,
                flip: *flip,
                mirrored: *mirrored,
                open_angle: *open_angle,
                slide_distance: *slide_distance,
                speed: *speed,
                auto_close: *auto_close,
                use_radius: *use_radius,
                access: *access,
                ..Door::new(*opening_type)
            });
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
        ComponentData::Ladder { height, width } => {
            b.add(Ladder { height: *height, width: *width });
        }
        ComponentData::Pickup { kind, weapon, mags, respawn } => {
            // A name no weapon family knows means the file was written by a build
            // with a weapon this one doesn't have. Keep the entity (it still draws
            // and is still editable) but leave the `Pickup` off, so nothing tries to
            // grant a weapon that cannot exist. Loud, because a silently inert
            // pickup is the kind of thing a playtest blames on the grant radius.
            match crate::combat::arsenal::resolve_name(weapon) {
                Some(name) => {
                    b.add(Pickup {
                        kind: *kind,
                        weapon: name,
                        mags: *mags,
                        respawn: *respawn,
                        cooldown: 0.0,
                    });
                }
                None => log::warn!(
                    "level has a pickup for unknown weapon {weapon:?} — loading it as \
                     scenery (no weapon by that name exists in either arsenal)"
                ),
            }
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
        // Authored settings only — `state`/`open_frac`/`timer` are HUNT-transient, so a
        // door blown open mid-round is saved shut, like every other combat state.
        cs.push(ComponentData::Door {
            opening_type: d.opening_type,
            hinge: d.hinge,
            flip: d.flip,
            mirrored: d.mirrored,
            open_angle: d.open_angle,
            slide_distance: d.slide_distance,
            speed: d.speed,
            auto_close: d.auto_close,
            use_radius: d.use_radius,
            access: d.access,
        });
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
    if let Some(l) = eref.get::<&Ladder>() {
        cs.push(ComponentData::Ladder { height: l.height, width: l.width });
    }
    if let Some(p) = eref.get::<&Pickup>() {
        // Authored settings only — `cooldown` is HUNT-transient, so a pickup taken
        // mid-round is saved sitting on the floor.
        cs.push(ComponentData::Pickup {
            kind: p.kind,
            weapon: p.weapon.to_string(),
            mags: p.mags,
            respawn: p.respawn,
        });
    }
    if let Some(i) = eref.get::<&Interactable>() {
        cs.push(ComponentData::Interactable { radius: i.radius });
    }
    if let Some(h) = eref.get::<&Health>() {
        cs.push(ComponentData::Health { hp: h.hp, max: h.max });
    }
    cs
}
