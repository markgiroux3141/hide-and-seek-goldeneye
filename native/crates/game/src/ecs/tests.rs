//! Scaffold-pass tests: prove the storage, the authored persistence round-trip,
//! and the (currently inert) system tick all hold together before any gameplay is
//! built on them.

use glam::{Quat, Vec3};

use super::components::*;
use super::persist::{ComponentData, EntityData};
use super::{AuthoredId, Ecs, SystemCtx};

/// Bare hecs storage: spawn a component, read it back, despawn it.
#[test]
fn spawn_query_despawn() {
    let mut ecs = Ecs::new();
    let e = ecs.world_mut().spawn((Transform::from_pos(Vec3::new(1.0, 2.0, 3.0)),));
    assert_eq!(ecs.len(), 1);
    {
        let eref = ecs.world().entity(e).unwrap();
        let t = eref.get::<&Transform>().unwrap();
        assert_eq!(t.pos, Vec3::new(1.0, 2.0, 3.0));
    }
    ecs.world_mut().despawn(e).unwrap();
    assert!(ecs.is_empty());
}

/// A multi-component authored entity survives spawn → save → JSON → load with its
/// id and every component intact. This is the core scaffold guarantee.
#[test]
fn authored_round_trips_multi_component_entity() {
    let mut ecs = Ecs::new();
    let id = ecs.alloc_id();
    let data = EntityData {
        id,
        components: vec![
            ComponentData::Transform {
                pos: [4.0, 0.0, 6.0],
                rot: Quat::IDENTITY.to_array(),
                scale: [1.0, 1.0, 1.0],
            },
            ComponentData::Door { opening_type: OpeningType::Swing },
            ComponentData::Interactable { radius: 1.5 },
            ComponentData::Health { hp: 100.0, max: 100.0 },
        ],
    };
    ecs.spawn_authored(&data);

    // Round-trip through the exact on-disk path: data → JSON text → data.
    let saved = ecs.save_authored();
    assert_eq!(saved.len(), 1);
    let json = serde_json::to_string(&saved).unwrap();
    let restored: Vec<EntityData> = serde_json::from_str(&json).unwrap();

    let mut ecs2 = Ecs::new();
    ecs2.load_authored(&restored);
    assert_eq!(ecs2.len(), 1);

    let e = ecs2.resolve(id).expect("authored id resolves after load");
    let eref = ecs2.world().entity(e).unwrap();
    assert_eq!(eref.get::<&Transform>().unwrap().pos, Vec3::new(4.0, 0.0, 6.0));
    let d = eref.get::<&Door>().unwrap();
    assert_eq!(d.opening_type, OpeningType::Swing);
    assert_eq!(d.state, DoorState::Closed, "a loaded door starts closed");
    assert_eq!(eref.get::<&Health>().unwrap().max, 100.0);
    assert!(eref.get::<&Interactable>().is_some());
}

/// Loading replaces the authored set rather than appending to it, and the id map
/// is rebuilt so stale ids no longer resolve.
#[test]
fn load_authored_replaces_not_appends() {
    let mut ecs = Ecs::new();
    let old = ecs.alloc_id();
    ecs.spawn_authored(&EntityData {
        id: old,
        components: vec![ComponentData::Transform {
            pos: [0.0; 3],
            rot: Quat::IDENTITY.to_array(),
            scale: [1.0; 3],
        }],
    });

    let fresh = AuthoredId(42);
    ecs.load_authored(&[EntityData {
        id: fresh,
        components: vec![ComponentData::Door { opening_type: OpeningType::Slide }],
    }]);

    assert_eq!(ecs.len(), 1);
    assert!(ecs.resolve(old).is_none(), "old authored entity is gone");
    assert!(ecs.resolve(fresh).is_some(), "new authored entity is present");
    // The allocator never hands back an id that collides with a loaded one.
    assert!(ecs.alloc_id().0 > fresh.0);
}

/// The system tick is a panic-free no-op on an empty world this pass.
#[test]
fn empty_system_tick_is_a_noop() {
    let mut ecs = Ecs::new();
    let mut physics = engine::sim::physics::PhysicsWorld::new();
    let mut ctx = SystemCtx {
        dt: 1.0 / 120.0,
        player_feet: Vec3::ZERO,
        nav: None,
        physics: &mut physics,
        commands: Vec::new(),
    };
    ecs.run_systems(&mut ctx);
    assert!(ecs.is_empty());
}
