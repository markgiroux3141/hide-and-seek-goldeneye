//! The auto-turret: a placed sentry gun that acquires a hunter, tracks it, spins up
//! and hoses it. The rig it poses — which piece is on which node, and where the bore
//! points — lives in [`crate::turret`]; this is the behaviour that drives it.
//!
//! # Where this lives, and why not in an ECS system
//!
//! Every other prop behaviour ticks in [`crate::ecs::systems`], but a turret has to
//! see the hunter roster and shoot it, and `SystemCtx` deliberately carries neither
//! the roster nor the damage path. So the turret ticks as a `World` method called
//! straight from `fixed_step`, alongside `run_systems` — the same shape
//! `hunter_opens_door` uses for the same reason. The component stays plain data.
//!
//! # It fights for the player
//!
//! A placed turret is the player's installation, so its kills credit the player's side
//! ([`Killer::Turret`]) and it never targets the player. That is the whole of its
//! allegiance model: there is no faction system to hook into, and inventing one to
//! support a single prop would be the wrong trade.

use super::super::*;
use crate::ecs::{MeshId, Renderable, Transform, Turret};
use crate::turret as rig;

/// How far out a turret will notice a hunter, in metres. Deliberately short of the
/// RC-P90's own 80 m reach: a turret is an area-denial fixture covering the room it
/// hangs in, not a sniper that rakes the whole level through every open doorway.
const DETECT_RANGE: f32 = 18.0;

/// How far a round actually carries. Longer than [`DETECT_RANGE`] so a hunter that
/// breaks away mid-burst still takes the tail of it.
const SHOT_RANGE: f32 = 40.0;

/// Slew rates, radians/sec. Yaw is the faster axis — the gun swings on a ring, but
/// elevates a heavy housing on a trunnion.
const YAW_RATE: f32 = 3.5;
const PITCH_RATE: f32 = 2.5;

/// Barrel spin-up / spin-down, radians/sec². Half a second to full song, and a longer
/// unpowered coast down, so a turret that has just lost its target keeps whirring —
/// which both reads correctly and means a hunter breaking line of sight for an instant
/// doesn't buy a full re-spin.
const SPIN_UP: f32 = rig::SPIN_RATE / 0.5;
const SPIN_DOWN: f32 = rig::SPIN_RATE / 1.4;

/// The gun will not fire below this fraction of full barrel speed. This is the
/// turret's tell: it whirs before it hurts, which is the window to break line of sight.
const SPIN_ARMED: f32 = 0.85;

/// How closely the bore must be on target before the trigger goes down, in radians.
/// Without this the turret would spray across the room while slewing onto a target.
const FIRE_ARC: f32 = 5.0 * std::f32::consts::PI / 180.0;

/// How often a turret reconsiders which hunter to shoot. Not every tick: a turret that
/// re-picked continuously would jitter between two hunters at equal range and never
/// settle on either long enough to hit one.
const REACQUIRE: f32 = 0.35;

/// How loud the turret's report is at the muzzle, and how far it carries. Louder and
/// further than a door, since it is a gun going off indoors.
const FIRE_VOL: f32 = 0.55;
const FIRE_AUDIBLE_RANGE: f32 = 34.0;

/// The turret's gun. It fires the RC-P90's round at the RC-P90's cadence with the
/// RC-P90's report — the game's bullet hose, which is what a gatling is.
fn gun() -> &'static crate::combat::config::WeaponStats {
    &crate::combat::config::RCP90
}

impl World {
    /// Bring every placed sentry gun to life at BUILD→HUNT by attaching its runtime
    /// [`Turret`] state. Mirrors [`Self::spawn_prop_colliders`]: the authored entity
    /// carries only `Transform` + `Renderable`, and everything the hunt needs is
    /// derived here and stripped again by [`Self::clear_turrets`].
    pub(crate) fn spawn_turrets(&mut self) {
        let mut targets: Vec<hecs::Entity> = Vec::new();
        for (e, r) in self.ecs.world().query::<(hecs::Entity, &Renderable)>().iter() {
            if r.mesh == MeshId::SentryGun {
                targets.push(e);
            }
        }
        let n = targets.len();
        for e in targets {
            let _ = self.ecs.world_mut().insert_one(e, Turret::default());
        }
        if n > 0 {
            log::info!("armed {n} sentry gun(s)");
        }
    }

    /// Strip turret runtime state at HUNT→BUILD, so the authored turret returns to the
    /// editor parked at rest rather than frozen mid-track.
    pub(crate) fn clear_turrets(&mut self) {
        let live: Vec<hecs::Entity> = self
            .ecs
            .world()
            .query::<(hecs::Entity, &Turret)>()
            .iter()
            .map(|(e, _)| e)
            .collect();
        for e in live {
            let _ = self.ecs.world_mut().remove_one::<Turret>(e);
        }
    }

    /// One turret tick: acquire, slew, spin, fire. Called per fixed step in HUNT.
    pub(crate) fn turret_step(&mut self, dt: f32) {
        // Snapshot each turret's placement so the ECS borrow is released before we
        // touch the roster, the physics world or the damage path.
        let mut turrets: Vec<(hecs::Entity, Mat4, Turret)> = Vec::new();
        for (e, t, r, g) in self
            .ecs
            .world()
            .query::<(hecs::Entity, &Transform, &Renderable, &Turret)>()
            .iter()
        {
            if r.mesh == MeshId::SentryGun {
                turrets.push((e, self.prop_model_matrix(r.mesh, t.pos, t.rot, t.scale), *g));
            }
        }
        if turrets.is_empty() {
            return;
        }

        let listener = self.player_pos().unwrap_or(Vec3::ZERO);
        // (roster index, impact point) per round that connected, applied after the loop.
        let mut hits: Vec<(usize, Vec3)> = Vec::new();
        let mut reports: Vec<Vec3> = Vec::new();

        for (e, place, mut g) in turrets {
            let pivot = place.transform_point3(rig::PITCH_PIVOT);

            // ── Acquire ────────────────────────────────────────────────────────────
            g.reacquire -= dt;
            // Re-pick on the timer, or the instant a held target stops being valid.
            // The "held target" test is gated on actually holding one: an empty-handed
            // turret must wait for the timer like everyone else, or it would sweep the
            // whole roster with line-of-sight raycasts every single tick while the
            // room is empty — which is most of the time.
            let lost = g
                .target
                .is_some_and(|i| !self.turret_can_engage(i, pivot));
            if lost || g.reacquire <= 0.0 {
                g.target = self.turret_pick_target(pivot);
                g.reacquire = REACQUIRE;
            }

            // ── Slew ───────────────────────────────────────────────────────────────
            // Rig-space aim, so the turret's own authored rotation is accounted for
            // rather than assumed to be identity.
            if let Some(aim) = g.target.and_then(|i| self.turret_aim_point(i)) {
                let dir_rig = place.inverse().transform_vector3(aim - pivot);
                let (want_yaw, want_pitch) = rig::aim_at(dir_rig);
                let dy = rig::angle_delta(g.yaw, want_yaw);
                g.yaw += dy.clamp(-YAW_RATE * dt, YAW_RATE * dt);
                let dp = want_pitch - g.pitch;
                g.pitch = (g.pitch + dp.clamp(-PITCH_RATE * dt, PITCH_RATE * dt))
                    .clamp(rig::PITCH_MIN, rig::PITCH_MAX);
            }

            // ── Spin ───────────────────────────────────────────────────────────────
            let want_spin = if g.target.is_some() { rig::SPIN_RATE } else { 0.0 };
            let rate = if g.target.is_some() { SPIN_UP } else { -SPIN_DOWN };
            g.spin_speed = if rate > 0.0 {
                (g.spin_speed + rate * dt).min(want_spin)
            } else {
                (g.spin_speed + rate * dt).max(0.0)
            };
            g.spin = (g.spin + g.spin_speed * dt) % std::f32::consts::TAU;

            // ── Fire ───────────────────────────────────────────────────────────────
            g.cooldown -= dt;
            let armed = g.spin_speed >= rig::SPIN_RATE * SPIN_ARMED;
            let on_target = g
                .target
                .and_then(|i| self.turret_aim_point(i))
                .is_some_and(|aim| {
                    let bore = place.transform_vector3(rig::bore_dir(g.yaw, g.pitch));
                    let to = (aim - place.transform_point3(rig::muzzle(g.yaw, g.pitch)))
                        .normalize_or_zero();
                    bore.normalize_or_zero().dot(to) >= FIRE_ARC.cos()
                });
            if armed && on_target && g.cooldown <= 0.0 {
                g.cooldown = gun().fire_cooldown;
                let muzzle = place.transform_point3(rig::muzzle(g.yaw, g.pitch));
                let bore = place
                    .transform_vector3(rig::bore_dir(g.yaw, g.pitch))
                    .normalize_or_zero();
                reports.push(muzzle);
                // The gatling's flash: a spark at the barrel tip, on the same channel
                // as an impact, refreshed every round — at 14 rounds a second it reads
                // as a continuous flicker at the muzzle.
                self.sparks.push(Spark { pos: muzzle, ttl: SPARK_TTL });
                match crate::combat::shooting::cast(
                    &mut self.physics,
                    muzzle,
                    bore,
                    SHOT_RANGE,
                    None,
                ) {
                    Some(hit) if self.physics.is_enemy_collider(hit.collider) => {
                        if let Some(i) = self
                            .enemies
                            .iter()
                            .position(|x| x.collider == hit.collider && !x.enemy.is_dead())
                        {
                            hits.push((i, hit.point));
                        }
                    }
                    // Anything else the round meets — wall, prop, the turret's own
                    // mount — just sparks. A turret does not blow up the scenery.
                    Some(hit) => self.sparks.push(Spark {
                        pos: hit.point + hit.normal * 0.01,
                        ttl: SPARK_TTL,
                    }),
                    None => {}
                }
            }

            let _ = self.ecs.world_mut().insert_one(e, g);
        }

        for at in reports {
            if let Some(audio) = self.audio.as_mut() {
                let vol = super::door::falloff_volume(
                    FIRE_VOL,
                    at,
                    listener,
                    FIRE_AUDIBLE_RANGE,
                );
                if vol > 0.0 {
                    audio.play(gun().fire_sound, vol);
                }
            }
        }
        for (i, at) in hits {
            self.hit_enemy_with(i, at, gun().damage, Killer::Turret);
        }
    }

    /// The point on hunter `idx` a turret aims at: the torso, the same height the
    /// hunters shoot each other at. `None` once it is dead.
    fn turret_aim_point(&self, idx: usize) -> Option<Vec3> {
        let inst = self.enemies.get(idx)?;
        (!inst.enemy.is_dead()).then(|| inst.enemy.pos + Vec3::Y * PD_TORSO_AIM)
    }

    /// Whether hunter `idx` is still a valid engagement from `pivot`: alive, in range,
    /// and not behind a wall.
    fn turret_can_engage(&mut self, idx: usize, pivot: Vec3) -> bool {
        let Some(aim) = self.turret_aim_point(idx) else {
            return false;
        };
        if aim.distance(pivot) > DETECT_RANGE {
            return false;
        }
        let feet = self.enemies[idx].enemy.pos;
        crate::enemy::perception_los(&mut self.physics, pivot, feet)
    }

    /// Pick the nearest engageable hunter, or `None` if the room is clear. Nearest
    /// rather than most-wounded or most-dangerous: a fixed gun's job is whatever has
    /// got closest to it.
    fn turret_pick_target(&mut self, pivot: Vec3) -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for i in 0..self.enemies.len() {
            let Some(aim) = self.turret_aim_point(i) else {
                continue;
            };
            let d = aim.distance(pivot);
            if d > DETECT_RANGE || best.is_some_and(|(_, bd)| d >= bd) {
                continue;
            }
            if self.turret_can_engage(i, pivot) {
                best = Some((i, d));
            }
        }
        best.map(|(i, _)| i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ecs::{ComponentData, EntityData};

    /// The turret's assembled model bounds, as the app registers them at startup —
    /// the placement ghost, the pick box and the gizmo all read these.
    fn register_bounds(world: &mut World) {
        let path = format!(
            "{}/../../assets/props/sentry_gun/sentry_gun.obj",
            env!("CARGO_MANIFEST_DIR")
        );
        let parts =
            engine::assets::obj_model::load_obj_components(&path).expect("sentry gun loads");
        let (min, max) = rig::assembled_bounds(&parts);
        world.register_prop_bounds(MeshId::SentryGun, min, max);
    }

    /// Author a sentry gun hanging at `pos` in a world already in BUILD.
    fn place_turret(world: &mut World, pos: Vec3) {
        place_turret_scaled(world, pos, rig::RIG_SCALE);
    }

    fn place_turret_scaled(world: &mut World, pos: Vec3, s: f32) {
        let id = world.ecs.alloc_id();
        world.ecs.spawn_authored(&EntityData {
            id,
            components: vec![
                ComponentData::Transform {
                    pos: pos.to_array(),
                    rot: Quat::IDENTITY.to_array(),
                    scale: [s, s, s],
                },
                ComponentData::Renderable { mesh: MeshId::SentryGun },
            ],
        });
    }

    fn count_turret_state(world: &World) -> usize {
        world.ecs.world().query::<&Turret>().iter().count()
    }

    /// The highest and lowest world Y actually drawn, by running the real asset's
    /// vertices through the draw matrices the world just produced.
    fn drawn_vertical_extent(draws: &[(&'static str, Mat4, [f32; 4])]) -> (f32, f32) {
        let path = format!(
            "{}/../../assets/props/sentry_gun/sentry_gun.obj",
            env!("CARGO_MANIFEST_DIR")
        );
        let parts =
            engine::assets::obj_model::load_obj_components(&path).expect("sentry gun loads");
        let (mut top, mut bottom) = (f32::MIN, f32::MAX);
        for (part, model) in rig::PARTS.iter().zip(&parts) {
            let m = draws.iter().find(|(k, _, _)| *k == part.key).unwrap().1;
            for v in &model.vertices {
                let y = m.transform_point3(Vec3::from_array(v.pos)).y;
                top = top.max(y);
                bottom = bottom.min(y);
            }
        }
        (top, bottom)
    }

    /// Turret runtime state is HUNT-only: attached when the hunt starts, gone when it
    /// ends. A turret left mid-track in the authored level would draw crooked in the
    /// editor and persist a pose the level file has no field for.
    #[test]
    fn turret_state_is_attached_for_the_hunt_and_stripped_after() {
        let mut world = World::new();
        place_turret(&mut world, Vec3::new(2.0, 2.0, 2.0));
        assert_eq!(count_turret_state(&world), 0, "no runtime state while authoring");
        world.spawn_turrets();
        assert_eq!(count_turret_state(&world), 1);
        world.clear_turrets();
        assert_eq!(count_turret_state(&world), 0, "state survived the hunt");
    }

    /// A turret hangs from the point it was authored at, and draws as one entry per
    /// rig piece with only the articulated pieces moving.
    ///
    /// Both halves are things only the draw path can get wrong. A turret anchored like
    /// an ordinary prop would hang from the centre of its own bounding box — half a
    /// metre in front of the ceiling bolt, since the barrel cantilevers forward — and
    /// a rig wired to the wrong nodes would swing the ceiling plate around with the gun.
    #[test]
    fn a_turret_draws_as_six_pieces_hung_from_its_mount_point() {
        let mut world = World::new();
        let at = Vec3::new(3.0, 2.0, -1.5);
        place_turret(&mut world, at);

        let keys: Vec<&str> = crate::turret::PARTS.iter().map(|p| p.key).collect();
        let rest: Vec<_> = world.prop_draws(1.0);
        assert_eq!(rest.len(), keys.len(), "one draw per rig piece");
        for k in &keys {
            assert!(rest.iter().any(|(dk, _, _)| dk == k), "no draw for piece {k}");
        }
        // It hangs from the authored point: the assembled turret's ceiling plate is
        // flush with it and the whole gun is below. Measured off the real asset
        // through the real draw matrices, so this covers the split order, the rig, the
        // ceiling anchor and the authoring scale in one go.
        let (top, bottom) = drawn_vertical_extent(&rest);
        assert!(
            (top - at.y).abs() < 1e-3,
            "turret's top is {top:.4}, should be flush with the ceiling at {:.4}",
            at.y
        );
        let drop = at.y - bottom;
        assert!(
            (drop - 0.472).abs() < 5e-3,
            "turret hangs {drop:.3} m; expected ~0.47 m (0.45x the rig's 1.05 m)"
        );

        // Articulate it, and check what moved. The plate is bolted on; the gun is not.
        let e = world
            .ecs
            .world()
            .query::<(hecs::Entity, &Renderable)>()
            .iter()
            .map(|(e, _)| e)
            .next()
            .unwrap();
        world.spawn_turrets();
        let mut g = *world.ecs.world().get::<&Turret>(e).unwrap();
        g.yaw = 0.9;
        g.pitch = -0.4;
        g.spin = 1.7;
        let _ = world.ecs.world_mut().insert_one(e, g);

        let posed = world.prop_draws(1.0);
        let sample = |draws: &[(&'static str, Mat4, [f32; 4])], key: &str| {
            draws
                .iter()
                .find(|(k, _, _)| *k == key)
                .unwrap()
                .1
                .transform_point3(Vec3::new(0.3, -0.7, 0.0))
        };
        for still in ["sentry_gun_cowl", "sentry_gun_panel"] {
            assert!(
                sample(&rest, still).distance(sample(&posed, still)) < 1e-5,
                "{still} is on the static mount but moved with the gun"
            );
        }
        for moved in ["sentry_gun_fin", "sentry_gun_housing", "sentry_gun_barrel"] {
            assert!(
                sample(&rest, moved).distance(sample(&posed, moved)) > 1e-3,
                "{moved} should articulate but did not move"
            );
        }
    }

    /// You can click the gun you can see.
    ///
    /// The click-pick box is built from the prop's bounds and its **anchor**, and the
    /// gizmo module used to inline its own copy of the anchor formula — the base-centre
    /// one every floor prop uses. For a turret, which anchors at its mount point, that
    /// copy put the pick box in the ceiling *above* the gun: the turret drew correctly,
    /// hung correctly and could not be selected or deleted.
    #[test]
    fn a_turrets_pick_box_covers_the_gun_you_can_see() {
        let mut world = World::new();
        register_bounds(&mut world);
        let at = Vec3::new(3.0, 2.0, -1.5);
        place_turret(&mut world, at);
        let e = world
            .ecs
            .world()
            .query::<(hecs::Entity, &Renderable)>()
            .iter()
            .map(|(e, _)| e)
            .next()
            .unwrap();

        let (lo, hi) = world.prop_world_aabb(e).expect("turret has a pick box");
        let (top, bottom) = drawn_vertical_extent(&world.prop_draws(1.0));
        assert!(
            lo.y <= bottom + 1e-3 && hi.y >= top - 1e-3,
            "pick box spans y {:.3}..{:.3}, gun is drawn over {bottom:.3}..{top:.3}",
            lo.y,
            hi.y
        );
        // And specifically: it is under the ceiling, not above it.
        assert!(
            hi.y <= at.y + 1e-3,
            "pick box reaches {:.3}, above the ceiling it hangs from ({at:?})",
            hi.y
        );
    }

    /// A sentry gun authored before the rig is lifted out of the floor on load.
    ///
    /// Those were floor props at `PROP_SCALE`, anchored by the centre of their base.
    /// Re-read as ceiling fixtures they hang *downward* from that floor point — the
    /// whole gun below the floor, invisible and unclickable. The migration re-scales
    /// them and raises the mount by the turret's own drop, so the gun occupies the
    /// space it used to and can be grabbed.
    #[test]
    fn a_pre_rig_sentry_gun_is_lifted_out_of_the_floor_on_load() {
        let mut world = World::new();
        register_bounds(&mut world);
        let floor = Vec3::new(4.0, 0.0, 2.0);
        place_turret_scaled(&mut world, floor, crate::props::PROP_SCALE);

        world.migrate_legacy_turrets();

        let e = world
            .ecs
            .world()
            .query::<(hecs::Entity, &Renderable)>()
            .iter()
            .map(|(e, _)| e)
            .next()
            .unwrap();
        let t = *world.ecs.world().get::<&Transform>(e).unwrap();
        assert!(
            (t.scale.x - rig::RIG_SCALE).abs() < 1e-5,
            "legacy turret kept its old scale {}",
            t.scale.x
        );
        let (top, bottom) = drawn_vertical_extent(&world.prop_draws(1.0));
        assert!(
            bottom >= floor.y - 1e-3,
            "turret still hangs {:.3} m below the floor it was authored on",
            floor.y - bottom
        );
        assert!(top > floor.y, "turret has no height above the floor at all");

        // Idempotent: loading the same level twice must not walk it up the wall.
        world.migrate_legacy_turrets();
        let again = *world.ecs.world().get::<&Transform>(e).unwrap();
        assert_eq!(again.pos, t.pos, "migration ran twice and moved it twice");
    }

    /// A turret with nothing to shoot winds its barrels down to a stop and stays
    /// parked, rather than idling at speed or drifting off its rest aim.
    #[test]
    fn an_idle_turret_spins_down_and_holds_still() {
        let mut world = World::new();
        place_turret(&mut world, Vec3::new(2.0, 2.0, 2.0));
        world.spawn_turrets();
        // Prime it as though it had just lost a target mid-burst.
        let e = world
            .ecs
            .world()
            .query::<(hecs::Entity, &Turret)>()
            .iter()
            .map(|(e, _)| e)
            .next()
            .unwrap();
        let mut g = *world.ecs.world().get::<&Turret>(e).unwrap();
        g.spin_speed = rig::SPIN_RATE;
        g.yaw = 0.7;
        let _ = world.ecs.world_mut().insert_one(e, g);

        for _ in 0..400 {
            world.turret_step(1.0 / 120.0);
        }
        let g = *world.ecs.world().get::<&Turret>(e).unwrap();
        assert_eq!(g.spin_speed, 0.0, "idle turret never stopped spinning");
        assert!(g.target.is_none(), "idle turret acquired something");
        assert!((g.yaw - 0.7).abs() < 1e-5, "idle turret slewed off its rest aim");
    }
}
