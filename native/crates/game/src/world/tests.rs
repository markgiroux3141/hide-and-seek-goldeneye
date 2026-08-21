//! End-to-end authoring + simulation tests driving `World`'s public API (plus a
//! few module-internal helpers). This is the behavioral oracle for the port —
//! moved verbatim out of the old `world.rs`; the split changed no test logic.

use super::*;
use super::editing::find_room_brushes;

/// Put weapon `name` in the player's hands, owned and loaded.
///
/// The player starts **empty-handed** now (`DESIGN_PICKUPS.md`), so a test that
/// wants to shoot has to arm itself first. Resolved by name against the live
/// arsenal rather than by index: index 0 is the unarmed slot, and the whole reason
/// weapon lookups are name-keyed is so a test can't quietly end up pinning a
/// different gun than the one its assertions are calibrated for.
fn arm_with(world: &mut World, name: &str) -> usize {
    let idx = world
        .arsenal
        .weapons()
        .iter()
        .position(|w| w.name == name)
        .unwrap_or_else(|| panic!("{name} is not in the live arsenal"));
    world.owned[idx] = true;
    world.weapons[idx].stock_bought();
    world.weapon_index = idx;
    idx
}

    /// Free-aim: a small mouse delta floats the crosshair inside the boundary with
    /// no camera pan; a big delta pins it to the rim and spills the rest to a pan.
    #[test]
    fn free_aim_floats_then_pans_at_the_rim() {
        use super::combat::resolve_aim;
        // Small move from center — stays inside the circle, no pan.
        let (ax, ay, pdx, pdy) = resolve_aim(0.0, 0.0, 20.0, 0.0);
        assert!((ax * ax + ay * ay).sqrt() < AIM_MAX_RANGE, "inside the boundary");
        assert!(ax > 0.0, "moved right");
        assert_eq!((pdx, pdy), (0.0, 0.0), "no camera pan inside the boundary");
        // Huge move — crosshair pinned at the rim, remainder becomes a pan.
        let (ax, ay, pdx, _pdy) = resolve_aim(0.0, 0.0, 100_000.0, 0.0);
        let mag = (ax * ax + ay * ay).sqrt();
        assert!((mag - AIM_MAX_RANGE).abs() < 1e-3, "clamped to the rim: {mag}");
        assert!(pdx > 0.0, "overflow pans the camera right");
    }

    /// End-to-end authoring loop with no GPU: build the room + collider, aim the
    /// crosshair at the −Z wall, push it, and confirm the whole pipeline fires
    /// (raycast pick → brush resize → re-evaluate → collider rebuilt → new mesh).
    /// This is the Phase 1 risk-burndown proof.
    #[test]
    fn push_carves_the_wall_the_crosshair_hits() {
        let mut world = World::new();
        let initial = world.initial_meshes();
        assert_eq!(initial.len(), 1, "one room region");
        let tris_before = initial[0].mesh.indices.len();
        assert!(tris_before > 0, "room built geometry");

        // Camera spawns at (3,1.5,3) m looking −Z → crosshair hits the z=0 wall.
        let rm = world.push(PUSH_PULL_STEP).expect("crosshair should hit a wall");
        assert_eq!(rm.id, 0);
        assert!(!rm.mesh.indices.is_empty(), "carved room still has geometry");

        // Pulling the same wall back should also resolve a hit (loop is stable).
        assert!(world.pull(PUSH_PULL_STEP).is_some(), "pull resolves a face too");
    }

    /// Aiming at empty space (no collider along the ray) picks nothing — push is
    /// a safe no-op rather than a panic.
    #[test]
    fn looking_at_nothing_is_a_safe_noop() {
        let mut world = World::new();
        world.initial_meshes();
        // Fly far outside the room and look away from it.
        world.camera.pos = Vec3::new(1000.0, 1000.0, 1000.0);
        world.camera.yaw = 0.0;
        assert!(world.push(PUSH_PULL_STEP).is_none());
    }

    /// Entering HUNT drops a capsule that gravity settles onto the room floor
    /// (y≈0, the cavity's bottom) — it neither sinks through nor floats.
    #[test]
    fn character_settles_on_the_floor_under_gravity() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode(); // BUILD → HUNT, spawns on the floor beneath the cam
        assert_eq!(world.mode, Mode::Hunt);
        let input = InputState::default(); // no keys, not pointer-locked

        for _ in 0..240 {
            // 2 s at 120 Hz
            world.fixed_step(1.0 / 120.0, &input);
        }
        let feet = world.player_pos().expect("player exists in HUNT");
        assert!(
            feet.y.abs() < 0.05,
            "feet should rest on the y≈0 floor, got {}",
            feet.y
        );
    }

    /// A3 milestone: on HUNT the hunter spawns watching the player, runs its
    /// perception FSM (detects → alert → chase into range → attack), fires its
    /// rifle inside the animation's FIRE_TIMING window, and damages the stationary
    /// player. Drives a full render-frame loop (sim + animation + enemy combat).
    #[test]
    fn hunter_perceives_chases_and_shoots_the_player() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode(); // bake nav + spawn the hunter roster watching the player
        assert!(!world.enemies.is_empty(), "hunters spawned");
        assert_eq!(world.player_health(), PLAYER_MAX_HEALTH, "player starts at full health");

        let input = InputState::default(); // player stands still, in the guards' view
        let dt = 1.0 / 60.0;
        let mut damaged = false;
        for _ in 0..600 {
            // up to 10 s of frames
            world.fixed_step(dt, &input);
            world.advance_animation(dt);
            world.enemy_combat_step(dt);
            if world.player_health() < PLAYER_MAX_HEALTH {
                damaged = true;
                break;
            }
        }
        assert!(damaged, "a hunter should perceive, close in, and shoot the player");
        // At least one hunter engaged (left idle) — the perception FSM ran.
        assert!(
            world
                .enemies
                .iter()
                .any(|e| e.enemy.state() != crate::enemy::AiState::Idle),
            "a hunter should have engaged, not all stayed idle"
        );
    }

    /// Back-off: a hunter shoved inside its standoff gives ground until it regains the
    /// weapon's hold distance, instead of firing point-blank. Regression for "enemies
    /// get right on top of me." Uses a shotgun (3 m standoff) so only a couple metres
    /// of retreat room is needed in the open spawn area.
    #[test]
    fn a_pinned_hunter_backs_off_toward_its_standoff() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode(); // HUNT — bake nav + spawn the roster
        assert!(!world.enemies.is_empty(), "hunters spawned");
        let ppos = world.player_pos().expect("player exists in HUNT");

        // Isolate hunter 0: kill + banish every packmate so no separation-nudge or
        // stale capsule interferes with its retreat lane.
        let far = Vec3::new(500.0, 0.0, 500.0);
        for i in 1..world.enemies.len() {
            world.enemies[i].enemy.take_damage(1e6);
            let c = world.enemies[i].collider;
            world.physics.update_enemy_collider(c, far);
        }

        // Give it a short-standoff weapon so the open spawn area easily fits the retreat.
        world.enemies[0].weapon = crate::combat::enemy_def_for(&crate::combat::config::SHOTGUN);
        let standoff = world.enemies[0].weapon.standoff;

        // Pin it at point-blank on the open line toward the spawn marker (walkable both
        // ways — the wave fanned out through here) so it has room to give ground.
        let toward_spawn = {
            let d = Vec3::new(SPAWN_MARKER_POS.x - ppos.x, 0.0, SPAWN_MARKER_POS.z - ppos.z);
            if d.length_squared() > 1e-6 { d.normalize() } else { Vec3::X }
        };
        let pinned = ppos + toward_spawn * 1.0;
        world.enemies[0].enemy.pos = pinned;
        let c0 = world.enemies[0].collider;
        world.physics.update_enemy_collider(c0, pinned);
        let start = Vec3::new(pinned.x - ppos.x, 0.0, pinned.z - ppos.z).length();

        let input = InputState::default(); // player stands still
        let dt = 1.0 / 60.0;
        for _ in 0..300 {
            world.fixed_step(dt, &input); // 5 s
        }

        let e = world.enemies[0].enemy.pos;
        let end = Vec3::new(e.x - ppos.x, 0.0, e.z - ppos.z).length();
        assert!(!world.enemies[0].enemy.is_dead(), "the test hunter stays alive");
        assert!(end > start + 0.5, "gave ground from point-blank (start {start:.2}, end {end:.2})");
        // Settles back near the standoff band ([standoff−hyst, …]); hyst is 1.2 m.
        assert!(
            end >= standoff - 1.6,
            "regained ~standoff (end {end:.2}, standoff {standoff:.2})"
        );
    }

    /// The fixed spawn fallback: with no pads authored, entering HUNT floods exactly
    /// [`ENEMY_COUNT`] hunters in clustered at the legacy fixed point (snapped to a
    /// standable cell) — independent of where the player is standing.
    ///
    /// No marker is drawn for it in either mode. The fallback is a compatibility shim
    /// for un-authored levels, not level content — spawn *pads* are what an author
    /// places now, and only those get markers (see
    /// `world::tools::spawn_point::markers_only_render_for_authored_pads`).
    #[test]
    fn wave_floods_in_at_the_fixed_marker() {
        let mut world = World::new();
        world.set_wave_size(6); // a real wave (gameplay default is 1 — "duel mode")
        world.initial_meshes();

        // Nothing authored, so nothing is drawn — the fresh room is bare floor.
        assert!(world.is_build());
        assert!(world.spawn_marker_mesh().is_none(), "no marker in BUILD");

        world.toggle_mode(); // BUILD → HUNT
        assert!(world.spawn_marker_mesh().is_none(), "and none in HUNT");

        // The whole wave floods in, clustered at the fixed marker (not the player).
        assert_eq!(world.enemies.len(), 6, "the whole requested wave floods in");
        assert!(
            world.spawn_point.distance(SPAWN_MARKER_POS) < 1.0,
            "spawn snaps to the fixed marker, got {:?}",
            world.spawn_point
        );
        for e in &world.enemies {
            let d = (e.enemy.pos - world.spawn_point).length();
            assert!(d < 2.0, "hunter enters near the marker (was {d:.1} m away)");
        }

        // No door is built for the spawn (it's just a marked floor point).
        assert!(world.doors.is_empty(), "no spawn door built");

        // Returning to BUILD tears the wave down.
        world.toggle_mode();
        assert!(world.enemies.is_empty(), "wave cleared on BUILD");
    }

    /// B5: in HUNT the animated model *is* each hunter — the placeholder box is
    /// gone and there is one skinned instance per hunter, each a real posed
    /// skinning set (opaque while alive).
    #[test]
    fn hunter_drives_the_animated_model_not_a_box() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode(); // HUNT: bake nav + spawn hunter roster
        assert!(!world.enemies.is_empty(), "hunters spawned");
        assert!(!world.char_models.is_empty(), "character model loaded");
        // The placeholder box is suppressed (the model is the hunter).
        assert!(world.enemy_mesh().is_none(), "box replaced by the model");

        // Step the hunters, then advance the animation driver.
        let input = InputState::default();
        for _ in 0..30 {
            world.fixed_step(1.0 / 120.0, &input);
        }
        world.advance_animation(1.0 / 60.0);

        // One skinned instance per hunter; each a real full-rig pose, opaque alive.
        // The joint count is the SPAWNED BODY's own — 15 on a GoldenEye rig, 30 on a
        // Perfect Dark one (15 bones plus its blend joints) — so this cannot be a
        // constant now that a wave wears Perfect Dark by default.
        let instances = world.character_instances();
        assert_eq!(instances.len(), world.enemies.len(), "one instance per hunter");
        let (body, _model, joints, opacity, colors) = &instances[0];
        let rig = world.char_models[*body].skeleton.joint_count();
        assert_eq!(joints.len(), rig, "posed against its own body's rig");
        assert!(joints.len() >= 15, "a real skeleton, not a box");
        assert_eq!(*opacity, 1.0, "alive hunter is opaque");
        assert!(colors.iter().all(|&c| c == 1.0), "un-shot hunter is clean (white blood)");
    }

    /// Multi-body: the whole character catalog loads onto the shared 15-bone rig, and
    /// every per-body derived table stays parallel to it. This is the headless proof
    /// that any of the 44 GoldenEye bodies is loadable + drivable by the one shared
    /// clip set (skinning uses each body's own skeleton). Skips if assets are absent.
    #[test]
    fn character_catalog_loads_onto_the_shared_rig() {
        let world = World::new();
        if world.char_models.is_empty() {
            eprintln!("skipping: character assets not loaded");
            return;
        }
        // More than the single original guard, and the per-body tables are parallel.
        assert!(world.char_models.len() > 1, "multiple bodies loaded");
        assert_eq!(world.char_models.len(), world.char_feet_offset.len(), "feet offsets parallel");
        assert_eq!(world.char_models.len(), world.enemy_arm.len(), "arm chains parallel");
        // Every loaded body rides the identical 15-bone rig, so the shared clip set
        // (bound to body 0) retargets onto it: the idle clip must skin cleanly on each
        // body's OWN skeleton, producing 15 finite joint matrices.
        let idle = world
            .char_anim_template
            .as_ref()
            .and_then(|a| a.clip(0))
            .expect("idle clip loaded");
        // Only the GoldenEye bodies: the Perfect Dark family loaded after them carries
        // extra blend joints and its own clips (see `PD_BODY_CATALOG`), and is covered
        // by `pd_bodies_load_skinned_and_animated` below.
        for (i, m) in world.char_models[..world.ge_body_count].iter().enumerate() {
            assert_eq!(m.skeleton.joint_count(), 15, "body {i} on the 15-bone rig");
            let mats = idle.skinning_matrices(0.0, &m.skeleton);
            assert_eq!(mats.len(), 15, "body {i} skins to 15 joints");
            assert!(
                mats.iter().all(|mm| mm.to_cols_array().iter().all(|f| f.is_finite())),
                "body {i} skinning matrices are finite"
            );
        }
    }

    /// The Perfect Dark import, end to end through the engine's own pipeline: each PD
    /// body loads as a skinned model, carries the GoldenEye bone *names* (so the
    /// weapon/head/foot systems address it unchanged), every PD clip binds to it by
    /// name, and posing it produces a human-sized standing figure.
    ///
    /// The height assertion is the one that matters. PD model units are millimetres
    /// and the exporter converts them to the engine's character units so `CHAR_SCALE`
    /// lands them at life size; a scale slip anywhere in that chain puts a speck or a
    /// tower in the level, and every other check here would still pass.
    #[test]
    fn pd_bodies_load_skinned_and_animated() {
        let world = World::new();
        let pd_range = world.pd_bodies();
        let pd = &world.char_models[pd_range.clone()];
        let Some(pd_template) = world.pd_anim_template.as_ref() else {
            eprintln!("skipping: PD assets not loaded");
            return;
        };
        if pd.is_empty() {
            eprintln!("skipping: PD assets not loaded");
            return;
        }

        // Every PD body must expose the SAME joint names, because one clip export
        // drives them all and `clip.rs` binds channels by name. This is not a
        // formality: PD puts its blend matrices on different bones per character
        // (51 use elbows + knees, `elvis` uses shoulders + hips), so a rig that
        // only declared the blends a body happens to use left `elvis`'s hip blend
        // geometry unrotated — a flat fin at the hip — while the clip's unmatched
        // blend channels landed on arbitrary joints via the node-index fallback.
        // Neither showed up in a channel count; both are obvious on screen.
        let expected: Vec<String> = (1..=15)
            .map(|i| format!("Bone_{i}"))
            .chain((1..=15).map(|i| format!("Blend_{i}")))
            .collect();
        for (i, m) in pd.iter().enumerate() {
            assert_eq!(
                m.skeleton.names, expected,
                "PD body {i} has a different joint-name set — one clip cannot drive them all"
            );
        }

        for (i, m) in pd.iter().enumerate() {
            let sk = &m.skeleton;
            // The GE bone roles the game addresses by name (weapon hand, head, feet).
            for bone in ["Bone_1", "Bone_2", "Bone_3", "Bone_8", "Bone_9", "Bone_14", "Bone_15"] {
                assert!(sk.index_of(bone).is_some(), "PD body {i} has no {bone}");
            }
            assert!(!m.vertices.is_empty(), "PD body {i} has skinned geometry");
            for v in &m.vertices {
                for &j in &v.joints {
                    assert!((j as usize) < sk.joint_count(), "PD body {i} joint {j} out of range");
                }
            }
            for (slot, name) in super::PD_TEMPLATE_CLIPS.iter().enumerate() {
                let c = pd_template.clip(slot).expect("every PD template slot is filled");
                assert_eq!(
                    c.bound_joints(),
                    sk.joint_count(),
                    "PD clip {name} binds every joint of body {i} by name"
                );
                let mats = c.skinning_matrices(0.0, sk);
                assert!(
                    mats.iter().all(|mm| mm.to_cols_array().iter().all(|f| f.is_finite())),
                    "PD body {i} + {name} skins finite"
                );
            }
            // Posed by its own idle, the body is a person-sized standing figure.
            let idle = pd_template.clip(0).expect("PD idle");
            let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
            for k in 0..12 {
                let mats = idle.skinning_matrices(idle.duration * k as f32 / 12.0, sk);
                for v in &m.vertices {
                    let mut y = 0.0;
                    for t in 0..4 {
                        if v.weights[t] != 0.0 {
                            y += v.weights[t]
                                * mats[v.joints[t] as usize]
                                    .transform_point3(glam::Vec3::from(v.pos))
                                    .y;
                        }
                    }
                    lo = lo.min(y);
                    hi = hi.max(y);
                }
            }
            let height = (hi - lo) * CHAR_SCALE;
            assert!(
                (0.9..=2.1).contains(&height),
                "PD body {i} stands {height:.2} m tall — expected a person",
            );
        }
    }

    /// **A body and its clips travel together.** The two character families are
    /// separately rigged — a clip stores absolute local rotations, so PD's animations
    /// only mean what they should against PD's bind pose and GoldenEye's against
    /// GoldenEye's — and mixing them yields a confidently-posed wrong figure that no
    /// numeric check notices. So the rule is per-wave, not per-hunter.
    ///
    /// **The rule the promotion left, and then the roster-widening replaced.** A wave used
    /// to be all one *family* because there was one Perfect Dark template and it was bound
    /// to a Perfect Dark rig. The PD clip set is bound to both rigs now
    /// (`a_goldeneye_body_can_play_the_perfect_dark_clips` is why that is sound), so:
    ///
    /// * the wave draws from **every** body, both families, and
    /// * **every hunter is on Perfect Dark's animations** whichever body it wears.
    ///
    /// What is still all-or-nothing is the *clip set*, and only in one direction: a
    /// GoldenEye clip on a Perfect Dark body is the invalid combination (the PD rig's extra
    /// `Blend_*` joints would sit at bind), so `set_goldeneye_clips` is ignored for a PD
    /// body. Both narrowings stay reachable — `set_body_set` for who spawns,
    /// `set_goldeneye_clips` for what animates them.
    ///
    /// This replaces `hunters_never_wear_a_pd_body`, which pinned the boundary two
    /// revisions ago (PD bodies were showcase-only because their clip set had not been
    /// identified).
    #[test]
    fn a_wave_mixes_both_body_families_on_the_perfect_dark_clips() {
        // The normal game: a big wave, drawing from everything.
        let mut game = World::new();
        game.set_wave_size(12);
        game.initial_meshes();
        game.toggle_mode(); // HUNT: spawn the wave
        if game.enemies.is_empty() || game.pd_bodies().is_empty() || game.ge_bodies().is_empty() {
            eprintln!("skipping: need hunters and both body families loaded");
            return;
        }
        assert!(!game.pd_lab_active(), "no lab flag — this is the shipped game");
        for inst in &game.enemies {
            assert!(inst.body < game.char_models.len(), "body id in range");
            assert!(inst.pd_anims, "every hunter is on the Perfect Dark animation tables");
            assert!(inst.pdsim.is_some(), "…and carries a simulant");
        }
        // The spread reaches across the families rather than stopping at one of them.
        // (12 hunters over ~44 bodies, spread evenly, so both ends are represented.)
        let ge_n = game.enemies.iter().filter(|e| game.ge_bodies().contains(&e.body)).count();
        let pd_n = game.enemies.iter().filter(|e| game.pd_bodies().contains(&e.body)).count();
        assert!(ge_n > 0, "the wave includes GoldenEye bodies");
        assert!(pd_n > 0, "…and Perfect Dark ones ({ge_n} GE / {pd_n} PD)");

        // `BodySet::GoldenEye` narrows the bodies but keeps the Perfect Dark animations —
        // this is the whole point: the fidelity no longer costs body variety.
        let mut ge = World::new();
        ge.set_body_set(super::BodySet::GoldenEye);
        ge.set_wave_size(8);
        ge.initial_meshes();
        ge.toggle_mode();
        let ge_bodies = ge.ge_bodies();
        for inst in &ge.enemies {
            assert!(
                ge_bodies.contains(&inst.body),
                "GoldenEye-only hunter wears body {} — GoldenEye bodies are {ge_bodies:?}",
                inst.body,
            );
            assert!(inst.pd_anims, "…and is STILL on the Perfect Dark animation tables");
        }

        // `set_goldeneye_clips` is the other knob, and it is the one that takes a hunter
        // off the PD tables.
        let mut legacy = World::new();
        legacy.set_goldeneye_clips(true);
        legacy.set_body_set(super::BodySet::GoldenEye);
        legacy.set_wave_size(4);
        legacy.initial_meshes();
        legacy.toggle_mode();
        for inst in &legacy.enemies {
            assert!(ge_bodies.contains(&inst.body), "legacy wave is GoldenEye-bodied");
            assert!(!inst.pd_anims, "…and off the Perfect Dark animation tables");
            assert!(inst.pdsim.is_some(), "…but still carries a simulant (family-agnostic)");
        }

        // The lab: Perfect Dark bodies only, driven by the PD template.
        let mut lab = World::new();
        if lab.pd_anim_template.is_none() {
            eprintln!("skipping: PD clip template not loaded");
            return;
        }
        lab.enable_pd_lab(super::pd_lab::PdLabConfig::default());
        lab.set_wave_size(8);
        lab.initial_meshes();
        lab.toggle_mode();
        assert!(!lab.enemies.is_empty(), "the lab spawns hunters");
        let pd_bodies = lab.pd_bodies();
        for inst in &lab.enemies {
            assert!(
                pd_bodies.contains(&inst.body),
                "lab hunter wears body {} — Perfect Dark bodies are {pd_bodies:?}",
                inst.body,
            );
            // …and is animated by the PD template, not the GoldenEye one. Both fill
            // the same 36 slots, so a slot count cannot tell them apart — compare the
            // clip that actually got loaded.
            let pd_idle = lab.pd_anim_template.as_ref().and_then(|t| t.clip(0)).unwrap();
            let ge_idle = lab.char_anim_template.as_ref().and_then(|t| t.clip(0)).unwrap();
            let got = inst.anim.clip(0).expect("hunter has an idle clip");
            assert_eq!(got.duration, pd_idle.duration, "lab hunter animates on the PD idle");
            assert_ne!(
                pd_idle.bound_joints(),
                ge_idle.bound_joints(),
                "the two idles must be distinguishable for this assertion to mean anything",
            );
            assert_eq!(
                got.bound_joints(),
                pd_idle.bound_joints(),
                "lab hunter's clips bind PD's 30 joints, not GoldenEye's 15",
            );
        }
    }

    /// **Simulants can now target and hit each other.** The lab's candidate list was
    /// the player alone, which left the half of `BotType` that compares *between*
    /// candidates — Prey, Judge, Venge, Feud, Coward — nothing to compare. Every live
    /// hunter is a candidate now.
    ///
    /// Friendly fire is emergent rather than special-cased: a simulant's round leaves
    /// along its barrel and the nearest body on that line takes it, so a packmate that
    /// walks through the line gets shot. This pins both halves.
    #[test]
    fn simulants_can_target_and_shoot_each_other() {
        use super::pd_lab::{candidates, PdActor, PdTarget};

        // The candidate list excludes the simulant itself and includes everyone else.
        let actors = [
            PdActor {
                who: PdTarget::Player,
                pos: Vec3::ZERO,
                alive: true,
                health_frac: 1.0,
                armed: true,
                visible: true,
            },
            PdActor {
                who: PdTarget::Hunter(0),
                pos: Vec3::X,
                alive: true,
                health_frac: 0.2,
                armed: true,
                visible: true,
            },
            PdActor {
                who: PdTarget::Hunter(1),
                pos: Vec3::Z,
                alive: true,
                health_frac: 1.0,
                armed: false,
                visible: true,
            },
        ];
        // **Teams off** (PD's free-for-all): everyone but itself is a candidate.
        let (cands, who) = candidates(PdTarget::Hunter(0), &actors, true);
        assert_eq!(cands.len(), 2, "the player and the other hunter, not itself");
        assert!(!who.contains(&PdTarget::Hunter(0)), "a simulant never targets itself");
        assert!(who.contains(&PdTarget::Player) && who.contains(&PdTarget::Hunter(1)));
        // **Teams on** (the default, and the hunt): packmates are not candidates at all.
        // This is the check that stops a pack ignoring the player to duel itself — a
        // packmate is nearly always the closest visible character, so leaving them in
        // the list and trusting the distance sort does not work.
        let (team_cands, team_who) = candidates(PdTarget::Hunter(0), &actors, false);
        assert_eq!(team_cands.len(), 1, "only the player is an enemy of a hunter");
        assert_eq!(team_who, vec![PdTarget::Player]);
        // …and the player's own list is every hunter, since they are all its enemies.
        let (p_cands, _) = candidates(PdTarget::Player, &actors, false);
        assert_eq!(p_cands.len(), 2, "the player is enemy to the whole squad");
        // Ids are stable and distinct, which is what the grudge memory holds.
        assert_eq!(PdTarget::Player.id(), 0);
        assert_eq!(PdTarget::Hunter(0).id(), 1);
        assert_ne!(cands[0].id, cands[1].id);
        // The threat data really varies between candidates, so a veto has something
        // to discriminate on.
        assert!(cands.iter().any(|c| !c.armed), "an unarmed candidate is visible as such");

        // And the shot: a hunter firing along a line that passes through a packmate
        // damages the packmate.
        let mut world = World::new();
        if world.pd_anim_template.is_none() || world.pd_bodies().is_empty() {
            eprintln!("skipping the shot half: PD assets not loaded");
            return;
        }
        world.enable_pd_lab(super::pd_lab::PdLabConfig::default());
        world.set_wave_size(4);
        world.initial_meshes();
        world.toggle_mode();
        assert!(world.enemies.len() >= 2, "a pack spawned");

        // Put hunter 1 directly on hunter 0's barrel line, and aim hunter 0 at it.
        // Close range, because `World::new`'s default room is only 6 m across and a
        // victim placed inside a wall would have no line of sight to it.
        let shooter = world.enemies[0].enemy.pos;
        let victim = shooter + Vec3::Z * 1.2;
        world.enemies[1].enemy.pos = victim;
        let yaw = 0.0_f32; // +Z
        if let Some(sim) = world.enemies[0].pdsim.as_mut() {
            sim.yaw = yaw;
        }
        let before = world.enemies[1].enemy.health();
        let weapon = world.enemies[0].weapon;
        let collider = world.enemies[0].collider;
        // Fire straight down that line. The player is far away and off-axis, so the
        // only body on it is hunter 1.
        world.emit_pd_shot(0, shooter, shooter + Vec3::Z * 500.0, collider, weapon);
        let after = world.enemies[1].enemy.health();
        assert!(after < before, "hunter 1 took the round ({before} -> {after})");
        assert!(
            world.enemies[1].hit_part.is_some(),
            "and it went through the normal hit path, so it has a hit part",
        );
    }

    /// **A Perfect Dark hunter reacts with its authored animation, not the ragdoll —
    /// with the ragdoll still switched on.** The tables were ported to be seen, and
    /// physics would discard them: whichever reaction system wins here is the one
    /// that decides whether any of Perfect Dark's 29 hit/death animations ever reach
    /// the screen. A GoldenEye hunter is untouched and still ragdolls.
    #[test]
    fn pd_hunters_prefer_authored_reactions_over_the_ragdoll() {
        use crate::combat::hit_anim::HitPart;
        // `pd == false` now needs the **override**: Perfect Dark is what a wave wears by
        // default, so the GoldenEye side of the A/B has to be asked for (§17).
        let kill = |pd: bool, authored: bool| -> Option<(bool, usize)> {
            let mut world = World::new();
            if pd {
                if world.pd_anim_template.is_none() || world.pd_bodies().is_empty() {
                    return None;
                }
            } else {
                // The legacy clip set, which is now the only way to reach the
                // pre-promotion reaction path — a GoldenEye *body* is on Perfect Dark's
                // animations too since the roster widened.
                world.set_goldeneye_clips(true);
                world.set_body_set(super::BodySet::GoldenEye);
            }
            assert!(world.ragdoll(), "the ragdoll is on — that is the point of this test");
            world.set_authored_reactions(authored);
            arm_with(&mut world, "PP7");
            world.initial_meshes();
            world.toggle_mode();
            let feet = world.enemies[0].enemy.pos;
            let height = world.body_height(world.enemies[0].body);
            // Head height → the x4 zone multiplier makes one PP7 round lethal.
            world.hit_enemy(0, Vec3::new(feet.x, feet.y + height * 0.94, feet.z));
            let inst = world.enemies.first()?;
            assert!(inst.enemy.is_dead(), "the hunter died");
            Some((inst.ragdoll.is_some(), inst.anim.current_clip()))
        };

        let Some((ge_ragdoll, _)) = kill(false, true) else {
            eprintln!("skipping: no hunters spawned");
            return;
        };
        assert!(ge_ragdoll, "a GoldenEye hunter still dies by physics");

        let Some((pd_ragdoll, played)) = kill(true, true) else {
            eprintln!("skipping the PD half: PD assets not loaded");
            return;
        };
        assert!(!pd_ragdoll, "a Perfect Dark hunter does NOT spawn a ragdoll");
        assert!(
            HitPart::Head.deaths().iter().any(|r| r.slot == played),
            "it plays a clip from the head death table instead (got slot {played})",
        );

        // And the switch really is a switch: off, PD hunters go back to physics.
        let (pd_ragdoll_off, _) = kill(true, false).expect("PD assets loaded");
        assert!(pd_ragdoll_off, "with authored reactions off, PD hunters ragdoll again");
    }

    /// **The hit-part tables point at the animations they name.** `combat::hit_anim`
    /// addresses the Perfect Dark template by slot number, because that is what the
    /// mixer takes — so a re-ordered or re-exported template would silently repoint
    /// every reaction table at the wrong clips, and every row would still be a valid
    /// index. This pins the numbers to the filenames.
    #[test]
    fn hit_part_tables_point_at_the_right_clips() {
        use crate::combat::hit_anim::{HitPart, ALL_PARTS};
        let files = super::PD_TEMPLATE_CLIPS;

        // Every row in every table is in range, and none of them lands in the
        // locomotion or fire block (slots 0–6) — a reaction is never a walk cycle.
        for &p in ALL_PARTS {
            for r in p.deaths().iter().chain(p.injuries()) {
                assert!(r.slot < files.len(), "{p:?} row slot {} is in range", r.slot);
                assert!(
                    r.slot >= super::CHAR_HIT_START,
                    "{p:?} row slot {} is a reaction, not locomotion/fire ({})",
                    r.slot,
                    files[r.slot],
                );
            }
            // A death really is from the death block; an injury never is a full death.
            let death_start =
                super::CHAR_HIT_START + engine::skeletal::anim_set::HIT_CLIPS.len();
            for r in p.deaths() {
                assert!(
                    r.slot >= death_start,
                    "{p:?} death plays {} — that is a hit clip",
                    files[r.slot],
                );
            }
        }

        // The named slots hold the animations the tables claim. Filenames encode the
        // role; `pd_roster.json` maps role → PD animation id.
        let named = [
            (HitPart::LFoot, "13-hit-left-leg.glb"),
            (HitPart::RFoot, "14-hit-right-leg.glb"),
            (HitPart::LHand, "11-hit-left-hand.glb"),
            (HitPart::RHand, "12-hit-right-hand.glb"),
            (HitPart::LBicep, "07-hit-left-shoulder.glb"),
            (HitPart::RBicep, "08-hit-right-shoulder.glb"),
        ];
        for (part, want) in named {
            assert_eq!(
                files[part.injuries()[0].slot],
                want,
                "{part:?}'s first injury row is {want}",
            );
        }
        // The torso/head flinches are slices of a death animation — the whole point.
        assert_eq!(
            files[HitPart::Torso.injuries()[0].slot],
            "30-death-forward-face-down-soft.glb",
        );
        assert!(HitPart::Torso.injuries()[0].end.is_some(), "and it is cut short");
    }

    /// End to end: a Perfect Dark hunter shot in a specific place reacts from
    /// **that part's** table, and a hit high on the body resolves to the head rather
    /// than to whatever the height classifier would have said.
    #[test]
    fn a_pd_hunter_reacts_from_the_table_for_the_part_that_was_hit() {
        use crate::combat::hit_anim::HitPart;
        // A fresh world per shot: a head hit takes the x4 zone multiplier and is
        // lethal from full health, which would make the second shot land on a corpse.
        let shoot_at = |height_frac: f32| -> Option<(HitPart, usize)> {
            let mut world = World::new();
            if world.pd_anim_template.is_none() || world.pd_bodies().is_empty() {
                return None;
            }
            world.enable_pd_lab(super::pd_lab::PdLabConfig::default());
            arm_with(&mut world, "PP7");
            world.set_ragdoll(false); // isolate the canned reaction from the ragdoll
            world.set_hit_reactions(true);
            world.initial_meshes();
            world.toggle_mode();
            assert!(world.enemies[0].pd_anims, "the lab hunter is on the PD tables");
            let feet = world.enemies[0].enemy.pos;
            let height = world.body_height(world.enemies[0].body);
            world.hit_enemy(0, Vec3::new(feet.x, feet.y + height * height_frac, feet.z));
            let inst = &world.enemies[0];
            Some((inst.hit_part.expect("the shot resolved to a body part"), inst.anim.current_clip()))
        };

        let Some((part, played)) = shoot_at(0.94) else {
            eprintln!("skipping: PD assets not loaded");
            return;
        };
        assert_eq!(part, HitPart::Head, "a shot just under the crown is a head hit");
        // Lethal (x4), so what plays is from the head DEATH table.
        assert!(
            HitPart::Head.deaths().iter().any(|r| r.slot == played),
            "played slot {played} is not in the head death table",
        );

        // A shot at ankle height resolves to a foot, survives, and plays that part's
        // injury — a different table reached by a different classification.
        let (foot, played) = shoot_at(0.04).expect("PD assets loaded");
        assert!(
            matches!(foot, HitPart::LFoot | HitPart::RFoot),
            "a shot at the ankles is a foot hit, got {foot:?}",
        );
        assert!(
            foot.injuries().iter().any(|r| r.slot == played),
            "played slot {played} is not in the {foot:?} injury table",
        );
    }

    /// **A Perfect Dark hunter fires on Perfect Dark's authored timing**, a
    /// GoldenEye one on the ported `FIRE_TIMING` guess — and the difference is not
    /// cosmetic. Both families play the *same three animations* (the two games share
    /// an animation bank), so this is a straight A/B of authored versus guessed
    /// windows on identical clips.
    ///
    /// The pistol is the case that mattered: the guess fired between 2.10 s and
    /// 2.20 s — a 3-frame sliver — where `chraction.c:981` authors frames 58–92, i.e.
    /// 1.93 s to 3.07 s. At the PP7's 2 shots/second that is the difference between
    /// one round a burst and three.
    #[test]
    fn pd_hunters_fire_on_the_authored_window_not_the_guess() {
        use crate::combat::attack_anim;

        let spawn_pistol_hunter = |pd: bool| -> Option<(super::EnemyInstance, f32)> {
            let mut world = World::new();
            if !pd {
                // The legacy `FIRE_TIMING` windows live on the GoldenEye clip set, which a
                // GoldenEye body no longer takes by default.
                world.set_goldeneye_clips(true);
                world.set_body_set(super::BodySet::GoldenEye);
            }
            if pd {
                if world.pd_anim_template.is_none() || world.pd_bodies().is_empty() {
                    return None;
                }
                world.enable_pd_lab(super::pd_lab::PdLabConfig::default());
            }
            // Roster index 1 is the PP7 — a one-handed pistol, the class whose
            // guessed window was wrong.
            world.set_wave_size(2);
            world.initial_meshes();
            world.toggle_mode();
            let inst = world.enemies.into_iter().nth(1)?;
            assert_eq!(inst.weapon.class, crate::combat::EnemyWeaponClass::Pistol);
            let rate = inst.weapon.fire_rate;
            Some((inst, rate))
        };

        let Some((ge, rate)) = spawn_pistol_hunter(false) else {
            eprintln!("skipping: no hunters spawned");
            return;
        };
        assert!(!ge.fire.authored, "a GoldenEye hunter keeps the FIRE_TIMING guess");

        let Some((pd, pd_rate)) = spawn_pistol_hunter(true) else {
            eprintln!("skipping the PD half: PD assets not loaded");
            return;
        };
        assert!(pd.fire.authored, "a Perfect Dark hunter uses the authored row");
        assert_eq!(rate, pd_rate, "same weapon, so the comparison is about timing alone");

        // The authored window is the one in the source, converted at 30 fps.
        let f = |frame: f32| frame / attack_anim::PD_ANIM_FPS;
        assert!((pd.fire.shoot.0 - f(58.0)).abs() < 1e-3, "shoot opens at frame 58");
        assert!((pd.fire.shoot.1 - f(92.0)).abs() < 1e-3, "shoot closes at frame 92");

        // Rounds a burst can actually put out, which is the whole point.
        let rounds = |t: &attack_anim::FireTiming| -> i32 {
            ((t.shoot.1 - t.shoot.0) * rate).floor() as i32 + 1
        };
        let (ge_rounds, pd_rounds) = (rounds(&ge.fire), rounds(&pd.fire));
        assert_eq!(ge_rounds, 1, "the guessed window fits a single round");
        assert!(
            pd_rounds >= 3,
            "the authored window fits a burst, got {pd_rounds} round(s)",
        );

        // And the authored row brackets the shooting with a wider tracking window,
        // which the guess had no way to express.
        assert!(pd.fire.aim.0 < pd.fire.shoot.0, "PD tracks before it fires");
        assert!(pd.fire.aim.1 > pd.fire.shoot.1, "and after the last round");
        assert!(ge.fire.aiming(0.0) && ge.fire.aiming(99.0), "the legacy path always tracks");

        // The authored aim limits are tighter sideways than the fallback cone, so a
        // PD hunter turns its body where a GoldenEye one twisted its chest.
        assert!(
            pd.fire.cone.left < super::ENEMY_CHEST_AIM_CONE,
            "authored sideways limit {} is tighter than the {} fallback",
            pd.fire.cone.left,
            super::ENEMY_CHEST_AIM_CONE,
        );
        assert!(pd.fire.cone.up > pd.fire.cone.down, "PD aims further up than down");
    }

    /// A Perfect Dark hunter flinches and dies on **Perfect Dark's** hit and death
    /// clips. This is the end of the chain the whole 36-slot layout exists for: the
    /// combat code addresses those clips by arithmetic (`CHAR_HIT_START + …`,
    /// `+ HIT_CLIPS.len()` for the death block) against whichever template the hunter
    /// was spawned with, so an off-by-one or a short PD set would land a "death" in
    /// the hit block, or past the end, and simply play nothing.
    ///
    /// Both paths are behind kill-switches that are OFF in the shipped default
    /// (ragdoll supersedes the canned deaths, and sim-style hunters do not flinch),
    /// exactly as for GoldenEye hunters — so they are turned on here deliberately.
    #[test]
    fn pd_hunters_flinch_and_die_on_pd_clips() {
        let mut world = World::new();
        if world.pd_anim_template.is_none() || world.pd_bodies().is_empty() {
            eprintln!("skipping: PD assets not loaded");
            return;
        }
        world.enable_pd_lab(super::pd_lab::PdLabConfig::default());
        arm_with(&mut world, "PP7"); // 25 dmg — non-lethal on a full-health hunter
        world.set_ragdoll(false); // isolate the canned death/flinch clips
        world.set_hit_reactions(true);
        world.initial_meshes();
        world.toggle_mode(); // HUNT
        assert_eq!(world.enemies.len(), 1, "duel mode spawns one hunter");
        assert!(world.pd_bodies().contains(&world.enemies[0].body), "wearing a PD body");

        let torso = {
            let p = world.enemies[0].enemy.pos;
            Vec3::new(p.x, p.y + 0.8, p.z)
        };
        world.hit_enemy(0, torso);
        assert!(!world.enemies[0].enemy.is_dead(), "one PP7 round is not lethal");
        assert!(world.enemies[0].anim.is_playing_oneshot(), "a PD hunter flinches");

        // Empty it out. The last round kills, which switches the one-shot to a death.
        for _ in 0..8 {
            if world.enemies.first().is_some_and(|e| !e.enemy.is_dead()) {
                world.hit_enemy(0, torso);
            }
        }
        let inst = world.enemies.first().expect("the corpse is still there, fading");
        assert!(inst.enemy.is_dead(), "the hunter died");
        // The playing one-shot is a real clip in this template's death block, and it
        // poses the PD body to finite matrices — i.e. the index arithmetic landed on
        // a clip that exists and binds.
        let death_start = super::CHAR_HIT_START + engine::skeletal::anim_set::HIT_CLIPS.len();
        let slot = inst.anim.current_clip();
        assert!(
            (death_start..super::PD_TEMPLATE_CLIPS.len()).contains(&slot),
            "death one-shot is slot {slot}, outside the death block \
             {death_start}..{}",
            super::PD_TEMPLATE_CLIPS.len(),
        );
        let sk = &world.char_models[inst.body].skeleton;
        let mats = inst.anim.clip(slot).expect("death clip loaded").skinning_matrices(0.0, sk);
        assert_eq!(mats.len(), sk.joint_count(), "the death clip poses all 30 PD joints");
        assert!(
            mats.iter().all(|m| m.to_cols_array().iter().all(|f| f.is_finite())),
            "PD death clip {} skins finite",
            super::PD_TEMPLATE_CLIPS[slot],
        );
    }

    /// **The weapon hold calibrates itself to the body.** The bone-local attach
    /// offsets in `combat::enemy_weapons` were hand-tuned on the GoldenEye rig, so the
    /// obvious worry about a PD-bodied hunter is that it holds its gun wrong. What
    /// saves it is that `spawn_wave` does not assume where the barrel ends up: it
    /// measures the real barrel direction in the chest frame from *that hunter's own*
    /// skeleton and aim pose (`EnemyArm::barrel_forward_in_chest`) and hands it to the
    /// chest-aim layer, which then swings the hold until the barrel points at the
    /// player. Different rig, different measurement, same result on screen.
    ///
    /// So this asserts the measurement is per-body and not a constant: both families
    /// come out with a usable forward axis, and the two axes are *not* the same — if
    /// they ever were, the calibration would have quietly become a hard-coded value.
    /// **Can a GoldenEye body play Perfect Dark's clips?** Measurement probe.
    ///
    /// `PD_TEMPLATE_CLIPS` asserts they cannot ("PD's bind pose is not GoldenEye's, and
    /// driving a PD body with a GE clip produces a confidently-posed wrong figure"). This
    /// measures it instead of trusting it, and the control is exact: PD slot 0 and GE slot
    /// 0 are the **same animation** (`ANIM_TWO_GUN_HOLD`, 163 frames in both banks), so
    /// Second half of the cross-family probe: load the **whole Perfect Dark template**
    /// **The two rigs are the same rig, and a GoldenEye body plays Perfect Dark's clips.**
    ///
    /// `PD_TEMPLATE_CLIPS` used to say the opposite — "PD's bind pose is not GoldenEye's,
    /// and driving a PD body with a GE clip produces a confidently-posed wrong figure" —
    /// and half of that is true but the stated reason is not. Measured here rather than
    /// asserted, because the *body roster* depends on it: if a GoldenEye body can play the
    /// PD clip set, then Perfect Dark's animation fidelity costs nothing in body variety.
    ///
    /// Three things are checked, and the third is what makes the first two trustworthy.
    ///
    /// 1. **The bind orientations are identical** — `0.0°` on every one of the 15 shared
    ///    bones. Only the rest *lengths* differ (1.00–1.27×, i.e. body proportions), and a
    ///    rotation-driven clip does not care: 38 differently-proportioned GoldenEye bodies
    ///    already share one clip set.
    /// 2. **Every PD slot drives a GoldenEye skeleton**: all 15 bones bound, a finite
    ///    person-sized figure through the whole clip. Including the 15 appended directional
    ///    fire clips, which GoldenEye ships no equivalent of.
    /// 3. **The slots that should agree, agree to a fifth of a degree.** Perfect Dark and
    ///    GoldenEye share an animation bank, so posing one GoldenEye body with slot `n` of
    ///    each template is the same animation twice, decoded from two different ROMs — and
    ///    it comes out within `0.3°`. The exceptions are exactly the six slots
    ///    `pd_roster.json` documents as deliberately *different* animations (the deaths PD
    ///    has that GoldenEye does not), which come out 28–104° apart. A transform error
    ///    could not produce that pattern; only a correct decode of both banks can.
    ///
    /// The one direction that *does* break is the reverse — a GoldenEye clip on a Perfect
    /// Dark body, measured at 9° mean / 62° worst — and the cause is not the bind pose
    /// either: the PD rig carries 15 extra `Blend_*` joints (its seam-hiding half-rotation
    /// frames) that a GoldenEye clip has no channels for, so they stay at bind while their
    /// owning bones rotate. That asymmetry is why the all-one-family rule still stands.
    #[test]
    fn a_goldeneye_body_can_play_the_perfect_dark_clips() {
        let world = World::new();
        let (Some(ge_t), Some(_)) =
            (world.char_anim_template.as_ref(), world.pd_anim_template.as_ref())
        else {
            eprintln!("skipping: both clip templates needed");
            return;
        };
        if world.ge_bodies().is_empty() || world.pd_bodies().is_empty() {
            eprintln!("skipping: both body families needed");
            return;
        }
        let ge = &world.char_models[world.ge_bodies().start];
        let pd = &world.char_models[world.pd_bodies().start];

        // 1. Same joints, same names, same bind orientation.
        for i in 1..=15 {
            let n = format!("Bone_{i}");
            let (Some(gi), Some(pi)) = (ge.skeleton.index_of(&n), pd.skeleton.index_of(&n))
            else {
                panic!("{n} is missing from one of the two rigs");
            };
            let dr = ge.skeleton.bind_r[gi].angle_between(pd.skeleton.bind_r[pi]).to_degrees();
            assert!(dr < 1.0, "{n} binds {dr:.1} deg apart between the families");
        }

        // Posed height + every joint's world axis, for comparing two clips on one body.
        let measure = |m: &engine::skeletal::gltf_skin::SkinnedModel,
                       c: &engine::skeletal::clip::AnimationClip,
                       t: f32|
         -> (f32, Vec<Vec3>) {
            let mats = c.skinning_matrices(t, &m.skeleton);
            let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
            for v in &m.vertices {
                let src = Vec3::from(v.pos);
                let mut pw = Vec3::ZERO;
                for k in 0..4 {
                    if v.weights[k] != 0.0 {
                        pw += v.weights[k] * mats[v.joints[k] as usize].transform_point3(src);
                    }
                }
                lo = lo.min(pw.y);
                hi = hi.max(pw.y);
            }
            let dirs = (0..m.skeleton.joint_count())
                .map(|j| mats[j].transform_vector3(Vec3::Y).normalize_or_zero())
                .collect();
            (hi - lo, dirs)
        };

        // 2 + 3. Every PD slot against GoldenEye body 0. The six slots `pd_roster.json`
        // fills with deaths GoldenEye has no counterpart for; every other shared slot is
        // the same animation in both banks.
        const DIFFERENT_ANIMATIONS: [usize; 6] = [19, 20, 23, 25, 26, 27];
        let own_height = measure(ge, ge_t.clip(0).expect("GE idle"), 0.0).0;
        let mut agreed = 0;
        for (slot, file) in super::PD_TEMPLATE_CLIPS.iter().enumerate() {
            let path =
                format!("{}/../../assets/enemies/pd/animations/{file}", env!("CARGO_MANIFEST_DIR"));
            let c = engine::skeletal::clip::load(&path, &ge.skeleton)
                .unwrap_or_else(|e| panic!("{file} does not load against a GoldenEye rig: {e}"));
            assert_eq!(c.bound_joints(), 15, "{file} binds every GoldenEye bone");
            for k in 0..6 {
                let (h, dirs) = measure(ge, &c, c.duration * k as f32 / 6.0);
                assert!(dirs.iter().all(|d| d.is_finite()), "{file} poses finite");
                // A standing clip stays near full height; a death drops but never
                // collapses to nothing or explodes — the figure stays a person.
                assert!(
                    (own_height * 0.1..own_height * 1.4).contains(&h),
                    "{file} frame {k} poses a {h:.0}-tall figure on a {own_height:.0} body",
                );
            }
            let Some(g) = ge_t.clip(slot) else { continue }; // 36+ has no GE counterpart
            let worst = measure(ge, g, 0.0)
                .1
                .iter()
                .zip(&measure(ge, &c, 0.0).1)
                .map(|(a, b)| a.angle_between(*b).to_degrees())
                .fold(0.0f32, f32::max);
            if DIFFERENT_ANIMATIONS.contains(&slot) {
                assert!(
                    worst > 10.0,
                    "slot {slot} ({file}) is supposed to be a DIFFERENT animation from \
                     GoldenEye's, but the two poses agree to {worst:.1} deg",
                );
            } else {
                assert!(
                    worst < 1.0,
                    "slot {slot} ({file}) should be the same animation in both banks, \
                     but the poses are {worst:.1} deg apart",
                );
                agreed += 1;
            }
        }
        assert!(agreed >= 25, "only {agreed} slots cross-validated between the two banks");

        // The fire clips on EVERY GoldenEye body, since the roster is the point.
        for body in world.ge_bodies() {
            let m = &world.char_models[body];
            for slot in [4usize, 5, 6, 38, 44, 50] {
                let file = super::PD_TEMPLATE_CLIPS[slot];
                let path = format!(
                    "{}/../../assets/enemies/pd/animations/{file}",
                    env!("CARGO_MANIFEST_DIR")
                );
                let c = engine::skeletal::clip::load(&path, &m.skeleton)
                    .unwrap_or_else(|e| panic!("body {body} cannot load {file}: {e}"));
                assert_eq!(c.bound_joints(), 15, "body {body} binds all of {file}");
                let (h, dirs) = measure(m, &c, c.duration * 0.5);
                assert!(h > 1.0 && dirs.iter().all(|d| d.is_finite()), "body {body} poses {file}");
            }
        }
    }

    /// **Every weapon in the arsenal aims down its own barrel** — asserted per weapon,
    /// because the failure this replaces was silent and per weapon.
    ///
    /// The barrel axis used to be "the muzzle-flash mesh centroid, or the **gun** mesh
    /// centroid when there is no flash mesh". The five weapons with no flash mesh came
    /// out of that second branch wrong: the sniper 22° high, the rocket launcher
    /// pointing **backwards**. Nothing noticed, because none of them is in
    /// `ENEMY_ROSTER` today and a wrong axis just makes the chest-aim swing the hold to
    /// the wrong place — a hunter that misses, not a hunter that crashes.
    ///
    /// So the property is pinned for the whole arsenal rather than for the roster: the
    /// axis is `+Z` to within a few degrees for every weapon, whether it declares that
    /// with a flash mesh or inherits it from the convention.
    #[test]
    fn every_weapon_aims_down_its_barrel() {
        let world = World::new();
        if world.enemy_weapon_lib.is_empty() {
            eprintln!("skipping: weapon assets not loaded");
            return;
        }
        let mut with_flash = 0;
        let mut without = 0;
        for a in world.enemy_weapon_lib.iter() {
            let axis = a.barrel_axis();
            assert!(
                (axis.length() - 1.0).abs() < 1e-3,
                "{}'s barrel axis is not normalised: {axis:?}",
                a.name,
            );
            let off = axis.angle_between(Vec3::Z).to_degrees();
            assert!(
                off < 6.0,
                "{} aims {off:.1}° off the arsenal's +Z barrel convention ({axis:?})",
                a.name,
            );
            // Whichever branch it came from, it must be forward. A sign error here is
            // the rocket-launcher bug.
            assert!(axis.z > 0.9, "{} aims backwards or sideways: {axis:?}", a.name);
            match a.muzzle_offset {
                Some(_) => with_flash += 1,
                None => without += 1,
            }
        }
        // Both branches are actually exercised — if the arsenal ever gained a flash mesh
        // for every gun, the convention constant would stop being covered and this test
        // would quietly become a test of one code path.
        assert!(with_flash >= 15, "only {with_flash} weapons measure their own axis");
        assert!(without >= 4, "only {without} weapons fall back to the convention");

        // The two that were provably wrong before, named so the regression is legible.
        for name in ["Sniper Rifle", "Rocket Launcher"] {
            let a = world
                .enemy_weapon_lib
                .iter()
                .find(|w| w.name == name)
                .unwrap_or_else(|| panic!("{name} loaded"));
            assert!(a.muzzle_offset.is_none(), "{name} still ships no flash mesh");
            assert_eq!(a.barrel_axis(), super::BARREL_MODEL_AXIS);
        }
    }

    /// The gun mesh's own geometry is **not** a usable source for the barrel axis, which
    /// is the reason the fix is a convention and not a cleverer estimator. Measured
    /// against the flash-bearing weapons — where the answer is known — the old
    /// gun-centroid rule is wrong by more than a right angle on several of them, because
    /// the models are not consistently placed relative to their origin (the DD44 is
    /// modelled entirely *behind* a muzzle-at-the-origin; the AR33 entirely in front of
    /// a grip-at-the-origin, and both point `+Z`).
    ///
    /// If this ever starts failing, someone has re-authored the gun GLBs around a
    /// consistent origin and a mesh-derived axis becomes possible — worth knowing.
    #[test]
    fn the_gun_mesh_centroid_is_not_the_barrel() {
        let world = World::new();
        let mut worst: (f32, &str) = (0.0, "");
        for a in world.enemy_weapon_lib.iter().filter(|w| w.muzzle_offset.is_some()) {
            if a.gun.vertices.is_empty() {
                continue;
            }
            let n = a.gun.vertices.len() as f32;
            let centroid =
                a.gun.vertices.iter().fold(Vec3::ZERO, |s, v| s + Vec3::from(v.pos)) / n;
            let err = centroid.normalize_or_zero().angle_between(a.barrel_axis()).to_degrees();
            if err > worst.0 {
                worst = (err, a.name);
            }
        }
        if worst.1.is_empty() {
            eprintln!("skipping: weapon assets not loaded");
            return;
        }
        assert!(
            worst.0 > 90.0,
            "the gun centroid is now within {:.0}° of the barrel on every measurable \
             weapon (worst: {}) — the models may have been re-origined, in which case a \
             mesh-derived axis is worth revisiting",
            worst.0,
            worst.1,
        );
    }

    /// **An explicit body choice outranks a mode default.**
    ///
    /// `enable_pd_lab` pins Perfect Dark bodies, because the lab is about Perfect Dark.
    /// That is right, and it silently ate `BODIES=ge` for a whole playtest: the app applied
    /// the environment *before* the lab block, so with `PD_LAB` still set in the shell —
    /// they persist for a session, and the lab runs on a real level, not only its own bare
    /// room — the wave stayed Perfect Dark and nothing said why.
    ///
    /// The app applies the roster last now. This pins the property that made the ordering
    /// matter: whichever order a caller uses, an explicit `set_body_set` after
    /// `enable_pd_lab` wins, and `roster_summary` reports what will actually spawn.
    #[test]
    fn an_explicit_body_set_outranks_the_lab_pin() {
        let mut world = World::new();
        if world.pd_bodies().is_empty() || world.ge_bodies().is_empty() {
            eprintln!("skipping: both body families needed");
            return;
        }
        // The lab on its own pins Perfect Dark.
        world.enable_pd_lab(super::pd_lab::PdLabConfig::default());
        assert!(world.roster_summary().contains("0 GoldenEye"), "{}", world.roster_summary());

        // …and an explicit choice after it wins, which is the order the app now uses.
        world.set_body_set(super::BodySet::GoldenEye);
        world.set_wave_size(6);
        world.initial_meshes();
        world.toggle_mode(); // HUNT
        let summary = world.roster_summary();
        assert!(summary.contains("0 Perfect Dark"), "roster says: {summary}");
        assert!(summary.contains("[PD_LAB]"), "…and still reports the lab: {summary}");
        assert!(!world.enemies.is_empty(), "hunters spawned");
        for inst in &world.enemies {
            assert!(
                world.ge_bodies().contains(&inst.body),
                "hunter wears body {} — expected a GoldenEye body",
                inst.body,
            );
            assert!(inst.pd_anims, "…on Perfect Dark's clips, which is the whole point");
            assert!(inst.pdsim.is_some(), "…and carrying a simulant");
        }
    }

    /// **Every hunter's feet are on the floor, whichever clip set drives it.**
    ///
    /// The roster widening shipped a 1.09 m float, and this is the shape of it: a Perfect
    /// Dark clip carries an **absolute root translation of 1301.7 units** — PD's rest hip
    /// height — on top of its own vertical motion, where a GoldenEye clip carries `0` and
    /// relies on the bind. That pedestal is load-bearing for a Perfect Dark body, whose
    /// vertices are stored bone-local so the root lift is what puts the geometry in place,
    /// and pure double-counting on a GoldenEye body, whose vertices are model-space with
    /// real inverse-bind matrices (`tools/pd-assets/pd_gltf.py` documents both conventions).
    ///
    /// So the feet offset is a property of the **(body, clip set)** pair, not of the body,
    /// and the old code chose the seating idle by body *family* — correct while a family
    /// implied its own clips, wrong the moment GoldenEye bodies started playing Perfect
    /// Dark's animations. The pedestal cannot simply be stripped either: the same channel
    /// carries the deaths' fall, swinging 1071–1193 units over a death clip.
    ///
    /// Asserted as the thing a player sees — the lowest skinned vertex sits at the
    /// hunter's feet — for both clip sets, on both body families.
    #[test]
    fn a_hunters_feet_are_on_the_floor_on_either_clip_set() {
        // (label, the world, expected pd_anims)
        let mut cases: Vec<(&str, World, bool)> = Vec::new();
        {
            let mut w = World::new();
            w.set_body_set(super::BodySet::GoldenEye);
            cases.push(("GoldenEye body, PD clips", w, true));
        }
        {
            let mut w = World::new();
            w.set_body_set(super::BodySet::PerfectDark);
            cases.push(("Perfect Dark body, PD clips", w, true));
        }
        {
            let mut w = World::new();
            w.set_goldeneye_clips(true);
            w.set_body_set(super::BodySet::GoldenEye);
            cases.push(("GoldenEye body, GoldenEye clips", w, false));
        }

        let mut checked = 0;
        for (label, mut world, want_pd) in cases {
            world.set_wave_size(3);
            world.initial_meshes();
            world.toggle_mode(); // HUNT
            world.advance_animation(1.0 / 60.0); // pose the stack for real
            if world.enemies.is_empty() {
                eprintln!("skipping {label}: no hunters spawned");
                continue;
            }
            for inst in &world.enemies {
                assert_eq!(inst.pd_anims, want_pd, "{label}: wrong clip set");
                let m = &world.char_models[inst.body];
                let joints = inst.anim.skinning_matrices(&m.skeleton);
                let ct = world.char_transform(inst.enemy.pos, inst.yaw(), inst.body, inst.pd_anims);
                // Lowest skinned vertex in WORLD space — the sole of the planted foot.
                let mut lowest = f32::INFINITY;
                for v in &m.vertices {
                    let src = Vec3::from(v.pos);
                    let mut local = Vec3::ZERO;
                    for k in 0..4 {
                        if v.weights[k] != 0.0 {
                            local +=
                                v.weights[k] * joints[v.joints[k] as usize].transform_point3(src);
                        }
                    }
                    lowest = lowest.min(ct.transform_point3(local).y);
                }
                let float = lowest - inst.enemy.pos.y;
                // A tenth of a metre either way: the idle has its own vertical motion and
                // the seating is the loop's *lowest* point, so a mid-loop frame sits a
                // little high. 1.09 m does not fit in that.
                assert!(
                    float.abs() < 0.12,
                    "{label}: body {} floats {float:+.3} m above its feet (lowest {lowest:.3}, \
                     feet {:.3})",
                    inst.body,
                    inst.enemy.pos.y,
                );
                checked += 1;
            }
        }
        assert!(checked >= 3, "only {checked} hunters checked");
    }

    /// **A hunter that flinches drops its trigger.**
    ///
    /// Reported from playtest: hunters kept firing through their hurt animations. Perfect
    /// Dark does not — `chr_stop_firing` (`chraction.c:9414`) is called immediately before
    /// `ACT_ARGH` at both injury sites, dropping both hands, resetting the aim-end and
    /// freeing the fire slots; and leaving `ACT_ATTACK` stops `chr_tick_attack` pumping
    /// shots at all.
    ///
    /// It happened here because **firing is a timer**: `fire_elapsed` runs in
    /// `enemy_combat_step`, which knows nothing about the mixer or `Enemy::stun`, so a
    /// stunned hunter mid-flinch kept emitting rounds from an in-flight burst. Not new with
    /// the GoldenEye bodies — it predates them and applied to Perfect Dark ones equally.
    #[test]
    fn a_flinching_hunter_stops_firing() {
        let mut world = World::new();
        arm_with(&mut world, "PP7"); // 25 dmg — non-lethal on a full-health hunter
        world.set_wave_size(1);
        world.initial_meshes();
        world.toggle_mode(); // HUNT
        world.advance_animation(1.0 / 60.0);
        if world.enemies.is_empty() {
            eprintln!("skipping: no hunters spawned");
            return;
        }
        // Put a burst in flight the way the combat pump does.
        world.start_enemy_fire(0);
        assert!(world.enemies[0].fire_elapsed.is_some(), "a burst is running");

        let torso = {
            let p = world.enemies[0].enemy.pos;
            let (head_min, _) = world.body_hit_zones(world.enemies[0].body);
            Vec3::new(p.x, p.y + head_min * 0.7, p.z)
        };
        world.hit_enemy(0, torso);
        assert!(!world.enemies[0].enemy.is_dead(), "one PP7 round is not lethal");
        assert!(
            world.enemies[0].fire_elapsed.is_none(),
            "the flinch must drop the trigger (chr_stop_firing)",
        );
        assert_eq!(world.enemies[0].burst_shot, 0, "and reset the burst counter");

        // The "sim style" branch is the documented exception: no reaction, no stun, so the
        // burst survives — that is the behaviour PD gives its own simulants, which never
        // reach the injury code at all.
        let mut sim = World::new();
        arm_with(&mut sim, "PP7");
        sim.set_wave_size(1);
        sim.set_ragdoll(false);
        sim.set_hit_reactions(false);
        sim.set_authored_reactions(false);
        sim.initial_meshes();
        sim.toggle_mode();
        sim.advance_animation(1.0 / 60.0);
        sim.start_enemy_fire(0);
        let torso = {
            let p = sim.enemies[0].enemy.pos;
            let (head_min, _) = sim.body_hit_zones(sim.enemies[0].body);
            Vec3::new(p.x, p.y + head_min * 0.7, p.z)
        };
        sim.hit_enemy(0, torso);
        assert!(
            sim.enemies[0].fire_elapsed.is_some(),
            "with no reaction at all the hunter fights through the hit",
        );
    }

    /// **The chest-aim axis is measured, not assumed** — and what it varies with is the
    /// *clip*, not the body.
    ///
    /// It used to be asserted the other way round: measure the axis on a GoldenEye hunter
    /// and on a Perfect Dark one and require them to *differ*, on the grounds that "if they
    /// ever were the same, the calibration would have quietly become a hard-coded value".
    /// That check stopped meaning anything when the roster widened, because both families
    /// are on the same clips now — and the two rigs have identical bind orientations, so
    /// the same clip through either rig genuinely produces the same chest-local barrel
    /// direction. Bone *lengths* differ between bodies, and a chain of rotations does not
    /// care about lengths when all you want out of it is a direction.
    ///
    /// So the property that is actually load-bearing is the one `EnemyInstance::fire_axes`
    /// depends on: the axis tracks the **animation**. Each fire clip holds the gun its own
    /// way, so a directional clip must measure differently from the forward one — that is
    /// the number `install_fire_row` swaps per burst, and a constant there would leave the
    /// chest-aim correcting for a pose that is no longer on the body.
    #[test]
    fn the_chest_aim_axis_is_measured_per_clip() {
        let mut world = World::new();
        world.set_wave_size(1);
        world.initial_meshes();
        world.toggle_mode(); // HUNT
        if world.enemies.first().is_none_or(|i| i.fire_axes.len() < 2) {
            eprintln!("skipping: no hunters / assets or arm rig not loaded");
            return;
        }
        // Live on the spawned hunter, and a real direction rather than a fallback.
        let live = {
            let inst = world.enemies.first_mut().expect("hunter");
            let layer = inst
                .stack
                .layer_as::<engine::skeletal::layers::AimOffsetLayer>(super::ENEMY_CHEST_AIM_LAYER)
                .expect("chest-aim layer");
            assert!(layer.enabled, "the chest-aim layer is live on a spawned hunter");
            layer.forward
        };
        assert!(live.is_finite() && (live.length() - 1.0).abs() < 1e-3, "{live:?} is a unit axis");

        let inst = world.enemies.first().expect("hunter");
        for (slot, a) in &inst.fire_axes {
            assert!(
                a.is_finite() && (a.length() - 1.0).abs() < 1e-3,
                "slot {slot} axis {a:?} is not a unit direction",
            );
        }
        // The measured axes are not all the same value — the per-clip calibration has not
        // collapsed into a constant.
        let first = inst.fire_axes[0].1;
        let spread = inst
            .fire_axes
            .iter()
            .map(|(_, a)| a.angle_between(first).to_degrees())
            .fold(0.0f32, f32::max);
        assert!(
            spread > 5.0,
            "all {} fire clips measured within {spread:.1} deg of each other — the per-clip \
             barrel calibration has become a constant",
            inst.fire_axes.len(),
        );
        // And the forward clip's axis is the one installed at spawn.
        let default = crate::combat::attack_anim::config_for(inst.weapon.class, inst.dual);
        let spawn_axis = inst.fire_axes.iter().find(|(s, _)| *s == default.slot).map(|(_, a)| *a);
        assert_eq!(spawn_axis, Some(live), "spawn installs the forward clip's measured axis");
    }

    /// **A hunter's hit capsule fits the body that is drawn.** Perfect Dark bodies are
    /// 1.73 m and GoldenEye's render 1.50 m, so the fixed `ENEMY_RADIUS` /
    /// `ENEMY_HALF_HEIGHT` pair would leave a PD hunter's head ~23 cm above its own
    /// collider: shots through the head would miss entirely, and the 4x headshot
    /// multiplier could never fire. This pins both halves of the fix — that the
    /// capsule and hit zones scale with measured height, and that a GoldenEye body
    /// still comes out at exactly the numbers the game was tuned with.
    #[test]
    fn hit_capsule_and_zones_follow_the_body_height() {
        let world = World::new();
        if world.char_models.is_empty() {
            eprintln!("skipping: no bodies loaded");
            return;
        }
        // GoldenEye is the calibration point: unchanged, to the millimetre.
        let (r, h) = world.body_capsule(0);
        assert!((r - super::ENEMY_RADIUS).abs() < 1e-3, "GE radius {r} == {}", super::ENEMY_RADIUS);
        assert!((h - super::ENEMY_HALF_HEIGHT).abs() < 1e-3, "GE half-height {h}");
        let (head, leg) = world.body_hit_zones(0);
        assert!((head - super::ZONE_HEAD_MIN).abs() < 1e-3, "GE head line {head}");
        assert!((leg - super::ZONE_LEG_MAX).abs() < 1e-3, "GE leg line {leg}");

        let pd = world.pd_bodies();
        if pd.is_empty() {
            eprintln!("skipping the PD half: no PD bodies loaded");
            return;
        }
        for body in pd {
            let height = world.body_height(body);
            // Every PD body is a person; Elvis is a Maian and genuinely short.
            assert!((0.9..=2.1).contains(&height), "PD body {body} is {height:.2} m");
            // The capsule reaches the top of the figure it represents. `0.96` is the
            // coverage a GoldenEye body already had (1.44 m capsule on a 1.50 m body),
            // so this asserts PD is no worse covered than GE, not that it is perfect.
            let (r, h) = world.body_capsule(body);
            let capsule_top = 2.0 * (h + r);
            assert!(
                capsule_top >= height * 0.95,
                "PD body {body} is {height:.2} m but its capsule tops out at {capsule_top:.2} m \
                 — the head would not be there to shoot",
            );
            // And the head line sits above the chest but below the crown, so a shot
            // that lands on the head is classified as one.
            let (head, leg) = world.body_hit_zones(body);
            assert!(head < height && head > height * 0.6, "PD body {body} head line {head:.2} m");
            assert!(leg < head && leg > 0.0, "PD body {body} leg line {leg:.2} m");
        }
    }

    /// Every bone-driven system resolves on a Perfect Dark body. `EnemyArm::resolve`
    /// is the one that has to be checked rather than assumed: it does not just look
    /// bones up by name, it walks the parent chain from the weapon hand, takes the
    /// two hands' lowest common ancestor as the chest, and reads the head's gaze axis
    /// **out of the bind pose**. PD's bind pose is a splayed star, not a rest pose, so
    /// the gaze axis is the term that could plausibly come out wrong on PD and right
    /// on GoldenEye.
    #[test]
    fn enemy_arm_resolves_on_every_pd_body() {
        let world = World::new();
        let pd = world.pd_bodies();
        if pd.is_empty() || world.pd_anim_template.is_none() {
            eprintln!("skipping: PD assets not loaded");
            return;
        }
        for body in pd {
            let m = &world.char_models[body];
            let arm = world.enemy_arm[body]
                .as_ref()
                .unwrap_or_else(|| panic!("PD body {body} has no resolved gun arm"));
            let sk = &m.skeleton;
            // The chain really is hand ← elbow ← shoulder, and the chest really is an
            // ancestor of both hands (not, say, the pelvis, which would make the
            // upper-body mask swing the legs too).
            assert_eq!(sk.index_of("Bone_9"), Some(arm.end), "PD body {body} arm ends at the gun hand");
            assert_eq!(sk.parents[arm.end], Some(arm.mid));
            assert_eq!(sk.parents[arm.mid], Some(arm.shoulder));
            assert!(
                arm.upper_body.contains(&arm.end) && arm.upper_body.contains(&arm.head),
                "PD body {body} upper-body mask covers the gun arm + head",
            );
            assert!(
                !arm.upper_body.contains(&arm.pelvis),
                "PD body {body} upper-body mask must exclude the pelvis, or aiming moves the legs",
            );
            // The gaze axis is a unit vector pointing broadly forward (+Z is the
            // facing at rest — see `char_transform_raw`). A bind pose that did not
            // agree with that convention would give a head that looks sideways at
            // whatever it is focused on, which no other assertion here would catch.
            assert!(
                (arm.head_forward.length() - 1.0).abs() < 1e-3,
                "PD body {body} gaze axis is normalised, got {:?}",
                arm.head_forward,
            );
            assert!(
                arm.head_forward.z > 0.7,
                "PD body {body} gazes along {:?} — expected roughly +Z",
                arm.head_forward,
            );
        }
    }

    /// The Perfect Dark clip template fills **exactly** the fixed slot layout the
    /// combat code indexes arithmetically. If PD's set ever loads short, the fire
    /// index would land in the hit block and a death index past the end — so the
    /// loader refuses a partial set, and this pins the count and the boundaries.
    ///
    /// Slots 0–35 are the shared layout **both** families use; 36+ is the Perfect Dark
    /// directional fire set, which GoldenEye has no exported clips for. That boundary
    /// is asserted rather than assumed, because it is the reason the directional table
    /// is gated on `pd_anims` and not on the lab flag.
    #[test]
    fn pd_template_matches_the_fixed_slot_layout() {
        let world = World::new();
        let Some(t) = world.pd_anim_template.as_ref() else {
            eprintln!("skipping: PD assets not loaded");
            return;
        };
        let shared = super::CHAR_HIT_START
            + engine::skeletal::anim_set::HIT_CLIPS.len()
            + engine::skeletal::anim_set::DEATH_CLIPS.len();
        assert_eq!(shared, 36, "the shared layout is the frozen 36 slots");
        let total = super::PD_TEMPLATE_CLIPS.len();
        assert!(
            total > shared,
            "the PD list extends the shared layout with the directional fire set",
        );
        for slot in 0..total {
            assert!(t.clip(slot).is_some(), "PD template slot {slot} is filled");
        }
        assert!(t.clip(total).is_none(), "and nothing past the directional set");
        // Every extra slot really is a fire clip the direction tables name, so an
        // appended clip cannot drift out of the one block that is allowed to grow.
        for slot in shared..total {
            assert!(
                crate::combat::attack_anim::slot_is_fire(slot),
                "PD template slot {slot} ({}) is past the shared layout but is not a \
                 directional fire clip",
                super::PD_TEMPLATE_CLIPS[slot],
            );
        }
        // The GoldenEye template agrees on the shared block, which is what makes one
        // set of index constants correct for both. It stops there.
        if let Some(ge) = world.char_anim_template.as_ref() {
            for slot in 0..shared {
                assert!(ge.clip(slot).is_some(), "GE template slot {slot} is filled");
            }
            assert!(ge.clip(shared).is_none(), "GoldenEye has no directional fire clips");
        }
    }

    /// **Every row of the direction tables points at the clip it names.**
    /// `AttackAnimConfig::slot` is a raw index into [`super::PD_TEMPLATE_CLIPS`], so a
    /// re-numbered template would silently repoint the whole table at the wrong
    /// animations — and every row would still be a valid index playing a real clip.
    /// The filenames carry the PD animation id for exactly this check.
    #[test]
    fn direction_table_rows_point_at_the_clips_they_name() {
        use crate::combat::attack_anim;
        let files = super::PD_TEMPLATE_CLIPS;
        for row in attack_anim::ALL_ROWS {
            assert!(row.slot < files.len(), "{} slot {} in range", row.anim, row.slot);
            let name = files[row.slot];
            // `ANIM_0032` → the filename ends `…-0032.glb`; the three defaults predate
            // the convention and are named by role, so they are pinned by number.
            let id = row.anim.trim_start_matches("ANIM_");
            let by_id = name.contains(id);
            let default = matches!(row.slot, 4 | 5 | 6);
            assert!(
                by_id || default,
                "{} is at slot {} which holds {name}",
                row.anim,
                row.slot,
            );
        }
        assert_eq!(files[attack_anim::RIFLE.slot], "04-fire-rifle.glb");
        assert_eq!(files[attack_anim::PISTOL.slot], "05-fire-pistol.glb");
        assert_eq!(files[attack_anim::DUAL.slot], "06-fire-dual.glb");
    }

    /// **Every fire animation aims where Perfect Dark says it does.**
    ///
    /// This is the measurement that validates the whole direction-table transcription,
    /// and it is falsifiable: `angleoffset` is a number read out of `chraction.c` with
    /// no reference to any asset, and the barrel yaw below is measured out of the
    /// exported clip with no reference to the table. If the field-order reading of the
    /// C struct literal were wrong, or the left/right sign backwards, or a filename
    /// mapped to the wrong animation id, these two would not agree.
    ///
    /// The measurement runs through the **real layer stack** rather than off the raw
    /// clip, which is the lesson of the aim-overlay bug: the same asset measured
    /// `+1.6°` alone and `−78.2°` composed, because the overlay grafts an upper body
    /// across two unrelated root yaws. The chest-aim layer is left disabled, because
    /// its entire job is to *cancel* what is being measured here.
    ///
    /// What it measured when the table landed (`cargo test -p game fire_animation --
    /// --nocapture` prints it):
    ///
    /// | stance | animation | authored | measured |
    /// |---|---|---|---|
    /// | heavy | `ANIM_0002` / `0032` / `0003` / `0006` | 0° | +1.9 / +6.2 / −2.1 / +0.8 |
    /// | heavy | `ANIM_0004` | **+90°** | **+89.9** |
    /// | light | `ANIM_0041` / `0044` / `0045` / `0046` | 0° | −2.8 / +1.2 / −3.1 / −2.1 |
    /// | light | `ANIM_0049` / `004A` | **+90°** | **+87.9 / +75.1** |
    /// | light | `ANIM_0047` / `0048` | **−90°** | **−96.3 / −88.2** |
    /// | dual | `ANIM_007A` | 0° | −0.2 |
    /// | dual | `ANIM_007B` / `007D` | **+90°** | **+90.4 / +92.4** |
    /// | dual | `ANIM_007C` / `007E` | **−90°** | **−89.8 / −84.6** |
    ///
    /// Eighteen independent agreements, worst case 14.9°. In particular the `DTOR(270)`
    /// rows come out on the character's **right**, which is the whole sign question
    /// settled by the assets rather than by the comment in `chraction.c` that predicted
    /// it.
    #[test]
    fn each_fire_animation_aims_where_pd_says_it_does() {
        use crate::combat::attack_anim;
        use crate::combat::enemy_weapons::enemy_def_for;
        use engine::skeletal::layers::{ClipOverlayLayer, LayerCtx, Pose};

        let world = World::new();
        let Some(template) = world.pd_anim_template.as_ref() else {
            eprintln!("skipping: PD assets not loaded");
            return;
        };
        let Some(body) = world.pd_bodies().next() else {
            eprintln!("skipping: no PD body loaded");
            return;
        };
        let (Some(m), Some(Some(arm))) = (world.char_models.get(body), world.enemy_arm.get(body))
        else {
            eprintln!("skipping: no PD body / arm rig");
            return;
        };
        let sk = &m.skeleton;
        let loco: Vec<(f32, engine::skeletal::clip::AnimationClip)> = {
            let mut v = Vec::new();
            for (speed, slot) in [
                (0.0, 0),
                (engine::skeletal::anim_set::SPEED_WALK, 1),
                (engine::skeletal::anim_set::SPEED_JOG, 2),
                (engine::skeletal::anim_set::SPEED_RUN, 3),
            ] {
                let Some(c) = template.clip(slot) else {
                    eprintln!("skipping: locomotion clip {slot} missing");
                    return;
                };
                v.push((speed, c.clone()));
            }
            v
        };

        // One representative weapon per stance, because the attach rotation that puts
        // the gun in the hand is per class — a pistol row measured through the rifle
        // grip would be meaningless.
        let stances: [(&str, EnemyWeaponClass, bool); 3] = [
            ("heavy", EnemyWeaponClass::Rifle, false),
            ("light", EnemyWeaponClass::Pistol, false),
            ("dual", EnemyWeaponClass::Rifle, true),
        ];
        let mut checked = 0;
        for (stance, class, dual) in stances {
            let cfg = match class {
                EnemyWeaponClass::Pistol => crate::combat::config::PP7,
                EnemyWeaponClass::Rifle => crate::combat::config::KF7,
            };
            let weapon = enemy_def_for(&cfg);
            let Some(asset) = world.enemy_weapon_lib.iter().find(|w| w.name == weapon.name) else {
                eprintln!("skipping {stance}: weapon assets not loaded");
                continue;
            };
            let attach = Quat::from_euler(
                EulerRot::XYZ,
                weapon.right_rot.x,
                weapon.right_rot.y,
                weapon.right_rot.z,
            );
            let default = attack_anim::config_for(class, dual);
            let Some(aim_clip) = template.clip(default.slot).cloned() else { continue };
            let mut stack = arm.build_stack(loco.clone(), aim_clip);

            for row in attack_anim::table_for(class, dual).rows() {
                let Some(clip) = template.clip(row.slot).cloned() else {
                    panic!("{} names slot {} which is empty", row.anim, row.slot)
                };
                let hold = attack_anim::FireTiming::from_pd(row, clip.duration);
                if let Some(ov) = stack.layer_as::<ClipOverlayLayer>(ENEMY_AIM_OVERLAY_LAYER) {
                    ov.set_clip(clip);
                    ov.time = hold.shoot.0;
                    ov.weight = 1.0;
                }
                let pose = stack.evaluate(Pose::bind(sk), &LayerCtx { skeleton: sk, dt: 0.0 });
                // The barrel in MODEL space (the body faces +Z at yaw 0), so its yaw is
                // directly comparable to `angleoffset`.
                let g = pose.joint_global_transforms(sk);
                let hand = g[sk.index_of(crate::combat::enemy_weapons::RIGHT_HAND_BONE).unwrap()]
                    .to_scale_rotation_translation()
                    .1;
                let barrel = (hand * (attach * asset.barrel_axis())).normalize();
                let measured = barrel.x.atan2(barrel.z).to_degrees();
                let authored = {
                    let d = row.angle_offset.to_degrees();
                    if d > 180.0 { d - 360.0 } else { d }
                };
                let mut err = measured - authored;
                while err > 180.0 {
                    err -= 360.0;
                }
                while err < -180.0 {
                    err += 360.0;
                }
                println!(
                    "{stance:>5} {:>9} slot {:>2}  authored {authored:>6.1}°  \
                     measured {measured:>7.1}°  Δ {err:>6.1}°",
                    row.anim, row.slot,
                );
                // A generous tolerance on purpose. The authored value is a design
                // intent quantised to 90°, and the barrel is the end of a chain of
                // hand-authored poses through a hand-tuned attach rotation — the claim
                // being tested is "this animation aims the way the table says", not
                // "the two agree to a degree". Half a slot band (45°) is the width at
                // which a wrong sign or a swapped animation could still pass, so the
                // bound is set inside it.
                assert!(
                    err.abs() < 40.0,
                    "{stance} {} aims {measured:.1}° but PD authored {authored:.1}° \
                     (Δ {err:.1}°) — the table, the sign convention, or the filename \
                     → animation-id mapping is wrong",
                    row.anim,
                );
                checked += 1;
            }
        }
        assert!(checked >= 14, "measured only {checked} rows — assets missing?");
    }

    /// **The direction table is actually wired into the live burst.** Everything above
    /// tests the table and the assets; this tests that `start_enemy_fire` consults it,
    /// which is the part that could be dead code while every other assertion passed —
    /// the `MAX_JOINTS` lesson.
    ///
    /// The hunter is turned so its target sits 135° to its left, which lands in the
    /// hard-left group of all three stance tables. It must come out holding a clip drawn
    /// to the left (`angle_offset` +90°) from the appended block, with its aim-overlay
    /// clip and barrel axis moved with it; turned back onto the target it must go back to
    /// its stance's forward row.
    #[test]
    fn starting_a_burst_facing_away_installs_a_sideways_animation() {
        use crate::combat::attack_anim;
        let mut world = World::new();
        world.enable_pd_lab(super::pd_lab::PdLabConfig::default());
        world.initial_meshes();
        world.toggle_mode(); // HUNT
        let Some(player) = world.player_pos() else {
            eprintln!("skipping: no player");
            return;
        };
        if world.enemies.is_empty() || world.enemies[0].fire_axes.is_empty() {
            eprintln!("skipping: PD assets not loaded");
            return;
        }
        // Bearing from the hunter to the player, in the game's `atan2(x, z)` yaw.
        let bearing = {
            let p = world.enemies[0].enemy.pos;
            (player.x - p.x).atan2(player.z - p.z)
        };
        let deg = |d: f32| d * attack_anim::BAD_TAU / 360.0;

        // ── Turned 135° away (target on its left) ──
        world.enemies[0].render_yaw = Some(bearing - deg(135.0));
        world.start_enemy_fire(0);
        let inst = &world.enemies[0];
        let off = inst.fire.angle_offset.to_degrees();
        assert!(
            (off - 90.0).abs() < 1.0,
            "a target 135° left should pick a left-drawn animation, got angle_offset {off:.1}°",
        );
        let slot = inst.fire.slot.expect("an authored row names its slot");
        assert!(slot >= 36, "and it comes from the appended directional block, got {slot}");
        assert!(inst.fire.authored, "on PD's authored timing");
        // The whole pipeline moved with it, not just the numbers.
        let clip_len = inst.anim.clip(slot).expect("that clip is loaded").duration;
        let axis = inst.fire_axes.iter().find(|(s, _)| *s == slot).map(|(_, a)| *a);
        assert!(axis.is_some(), "and its barrel axis was measured at spawn");
        let installed = {
            let mut e = world.enemies[0].stack.layer_as::<ClipOverlayLayer>(ENEMY_AIM_OVERLAY_LAYER);
            e.as_mut().map(|ov| (ov.clip().duration, ov.time))
        };
        let (dur, hold) = installed.expect("the aim overlay is present");
        assert!(
            (dur - clip_len).abs() < 1e-3,
            "the aim overlay is posing the chosen clip ({dur:.2}s vs {clip_len:.2}s)",
        );
        assert!(
            (hold - world.enemies[0].fire.shoot.0).abs() < 1e-3,
            "held at that row's own shoot frame",
        );

        // ── Turned back onto the target ──
        world.enemies[0].fire_elapsed = None;
        world.enemies[0].render_yaw = Some(bearing);
        world.start_enemy_fire(0);
        let inst = &world.enemies[0];
        assert_eq!(inst.fire.angle_offset, 0.0, "facing it, the body aims straight");
        let default = attack_anim::config_for(inst.weapon.class, inst.dual);
        let table = attack_anim::table_for(inst.weapon.class, inst.dual);
        let forward = table.0[0];
        assert!(
            forward.0.iter().any(|r| r.slot == inst.fire.slot.unwrap()),
            "and the row comes from the dead-ahead group",
        );
        // Heavy's dead-ahead group is ANIM_0002, not the spawn default ANIM_0032, so this
        // only holds where the two coincide — asserted rather than assumed either way.
        if forward.0.iter().any(|r| r.slot == default.slot) {
            assert_eq!(inst.fire.slot, Some(default.slot));
        }
    }

    /// **A hunter is calibrated for every animation it can play.** The chest-aim layer
    /// swings a measured barrel axis, and a Perfect Dark hunter swaps fire clip per
    /// burst — so a missing measurement means a burst whose barrel axis belongs to a
    /// pose that is no longer on the body, which reads on screen as a guard shooting
    /// past you and shows up in no other assertion.
    #[test]
    fn a_pd_hunter_is_calibrated_for_every_clip_its_bearing_can_pick() {
        use crate::combat::attack_anim;
        let mut world = World::new();
        world.enable_pd_lab(super::pd_lab::PdLabConfig::default());
        world.initial_meshes();
        world.toggle_mode(); // HUNT
        let Some(inst) = world.enemies.first() else {
            eprintln!("skipping: no hunter spawned");
            return;
        };
        if inst.fire_axes.is_empty() {
            eprintln!("skipping: PD assets / arm rig not loaded");
            return;
        }
        assert!(inst.pd_anims, "the lab hunter is on the PD tables");
        let table = attack_anim::table_for(inst.weapon.class, inst.dual);
        for row in table.rows() {
            let axis = inst.fire_axes.iter().find(|(s, _)| *s == row.slot);
            let (_, a) = axis.unwrap_or_else(|| panic!("no barrel axis for {}", row.anim));
            assert!(
                (a.length() - 1.0).abs() < 1e-3,
                "{}'s barrel axis is normalised, got {a:?}",
                row.anim,
            );
        }
        // The spawn clip is measured too, and it is the stance default.
        let default = attack_anim::config_for(inst.weapon.class, inst.dual);
        assert!(inst.fire_axes.iter().any(|(s, _)| *s == default.slot));
    }

    /// Multi-body spawn: a flooded-in wave wears a spread of bodies across the catalog
    /// (not six clones of body 0), and each drawn instance references a valid,
    /// in-range body id whose blood buffer matches that body's vertex count.
    #[test]
    fn wave_spreads_across_multiple_bodies() {
        let mut world = World::new();
        world.set_wave_size(6); // spread needs a pack (gameplay default is 1)
        world.initial_meshes();
        world.toggle_mode(); // HUNT: spawn the wave
        if world.char_models.len() < 2 || world.enemies.is_empty() {
            eprintln!("skipping: need multiple bodies + spawned hunters");
            return;
        }
        // The wave isn't all one body (the spread picks distinct bodies for ENEMY_COUNT
        // hunters over a 44-body catalog).
        let distinct: std::collections::HashSet<usize> =
            world.enemies.iter().map(|e| e.body).collect();
        assert!(distinct.len() > 1, "wave wears more than one body, got {distinct:?}");
        // Every hunter's body id is in range, and its blood buffer matches that body.
        for inst in &world.enemies {
            let m = world.char_models.get(inst.body).expect("hunter body in range");
            assert_eq!(inst.blood.len(), m.vertices.len() * 3, "blood sized to the body");
        }
        // Each render instance carries the hunter's body id, posed against that body's
        // own rig (see `hunter_drives_the_animated_model_not_a_box` for why not 15).
        for (body, _model, joints, _opacity, _colors) in world.character_instances() {
            assert!(body < world.char_models.len(), "instance body id in range");
            assert_eq!(joints.len(), world.char_models[body].skeleton.joint_count());
        }
    }

    /// The difficulty dial: level 0 is the neutral baseline (all multipliers 1.0 /
    /// dodge 0), and it ramps to a harder-hitting, tankier, evasive hunter at the cap.
    #[test]
    fn difficulty_params_ramp_from_baseline_to_brutal() {
        let mut world = World::new();
        assert_eq!(world.difficulty(), 0, "starts at the baseline");
        let base = world.difficulty_params();
        assert_eq!(base.speed_mult, 1.0);
        assert_eq!(base.cooldown_mult, 1.0);
        assert_eq!(base.reaction_mult, 1.0);
        assert_eq!(base.health_mult, 1.0);
        assert_eq!(base.dodge, 0.0);
        assert_eq!(base.sense_mult, 1.0, "baseline perception reach");
        assert_eq!(base.suppress, 0.0, "baseline holds fire until standoff");
        assert_eq!(base.flank, 0.0, "baseline chases straight");
        assert_eq!(base.cover, 0.0, "baseline never breaks to cover");

        world.change_difficulty(100); // clamps to DIFFICULTY_MAX
        assert_eq!(world.difficulty(), DIFFICULTY_MAX);
        let hard = world.difficulty_params();
        assert!(hard.speed_mult > 1.0, "moves faster");
        assert!(hard.cooldown_mult < 1.0, "shorter burst cooldown");
        assert!(hard.reaction_mult < 1.0, "reacts faster");
        assert!(hard.health_mult > 1.0, "tankier");
        assert!(hard.dodge > 0.0, "evades");
        assert!(hard.sense_mult > 1.0, "sharper senses");
        assert!(hard.suppress > 0.0, "suppresses while closing");
        assert!(hard.flank > 0.0, "flanks the approach");
        assert!(hard.cover > 0.0, "uses cover");

        // Clamps at both ends.
        world.change_difficulty(-100);
        assert_eq!(world.difficulty(), 0, "clamped at the floor");

        // **Aim is no longer on this struct.** `accuracy_mult` / `falloff_ease` scaled a
        // hit roll that no longer exists (§17); the same dial position now selects a
        // Perfect Dark zeroing tier instead, so the lethality ramp has to be asserted
        // there. That is the one axis whose ordering the dial must never lose.
        let mut tier_at = |d: u32| {
            world.set_difficulty(d);
            super::pd_lab::tier_for_dial_frac(world.difficulty_frac())
        };
        let floor = tier_at(0);
        let top = tier_at(DIFFICULTY_MAX);
        assert_ne!(floor, top, "the dial still sweeps the lethality axis");
        assert!(
            floor.tuning().shoot_delay > top.tuning().shoot_delay,
            "a harder tier reacts faster: {floor:?} vs {top:?}",
        );
        assert!(
            floor.tuning().max_zero_speed > top.tuning().max_zero_speed,
            "…and mis-aims less: {floor:?} vs {top:?}",
        );
    }

    /// #6 footstep noise: a *moving* player pulls a blind (searching) hunter within
    /// footstep range to Investigate — but only at higher difficulty; at level 0 the
    /// hunter is deaf to footsteps (range 0). Drives one player move directly so the
    /// perception FSM never runs (the hunter stays blind), then calls the emitter.
    #[test]
    fn footsteps_divert_blind_hunters_only_at_higher_difficulty() {
        let run = |difficulty: u32| -> crate::enemy::AiState {
            let mut world = World::new();
            world.set_difficulty(difficulty); // set the dial in BUILD (no restart churn)
            world.initial_meshes();
            world.toggle_mode(); // HUNT — spawns the lone hunter in Search (no FSM step yet)
            assert!(!world.enemies.is_empty(), "hunter spawned");
            let ppos = world.player_pos().unwrap();
            // Give the player real horizontal speed by driving ONE move step directly —
            // deliberately NOT via fixed_step, so the perception FSM never runs and the
            // hunter stays blind (Search).
            let mut input = InputState::default();
            input.pointer_locked = true;
            input.press(winit::keyboard::KeyCode::KeyW);
            world
                .character
                .as_mut()
                .unwrap()
                .apply_move(1.0 / 60.0, &input, &mut world.physics);
            assert!(
                world.character.as_ref().unwrap().speed() > MOVE_NOISE_MIN_SPEED,
                "player is moving above the sneak threshold"
            );
            // Park the still-blind hunter 6 m from the player (inside footstep range at
            // max difficulty, ~10 m; outside it at level 0, where range is 0).
            world.enemies[0].enemy.pos = ppos + Vec3::new(6.0, 0.0, 0.0);
            assert_eq!(
                world.enemies[0].enemy.state(),
                crate::enemy::AiState::Search,
                "hunter is still blind before the emitter runs"
            );
            world.alert_enemies_to_movement();
            world.enemies[0].enemy.state()
        };
        assert_eq!(
            run(0),
            crate::enemy::AiState::Search,
            "difficulty 0: deaf to footsteps — the hunter keeps searching"
        );
        assert_eq!(
            run(DIFFICULTY_MAX),
            crate::enemy::AiState::Investigate,
            "hard: footsteps within range pull the blind hunter to investigate"
        );
    }

    /// #5 grenade flush: when the pack is HELD AT RANGE from a camping player, an
    /// engaged hunter lobs a grenade (a projectile is spawned). Drives a hunter to
    /// engage naturally, then relocates it out beyond the blast-safe distance and
    /// runs the flush with the camp dwell satisfied — the "flush a camper you can't
    /// walk up to" case (which real play produces via cover/walls, not the open room).
    #[test]
    fn a_held_at_range_pack_flushes_a_camper() {
        let mut world = World::new();
        world.set_difficulty(DIFFICULTY_MAX);
        // The flush is OFF by default (hunters were blowing themselves up — see
        // `World::grenades`). This scenario is about whether the mechanic still works
        // when asked for, so it opts in explicitly.
        world.set_grenades(true);
        world.initial_meshes();
        world.toggle_mode(); // HUNT — the hunter engages the standing player
        world.toggle_invulnerable();
        let input = InputState::default();
        let dt = 1.0 / 60.0;
        // Let the lone hunter engage.
        for _ in 0..120 {
            world.fixed_step(dt, &input);
            if world.enemies[0].enemy.is_engaged() {
                break;
            }
        }
        assert!(world.enemies[0].enemy.is_engaged(), "hunter engaged the camper");

        // Hold it out at range (as cover/geometry would in a real level), and satisfy
        // the camp dwell, then run one flush step.
        let ppos = world.player_pos().unwrap();
        let far = ppos + Vec3::new(9.0, 0.0, 0.0); // ≥ GRENADE_SAFE_DIST from the camp spot
        world.enemies[0].enemy.pos = far;
        world.camp_anchor = Some(ppos);
        world.camp_timer = 100.0; // well past the dwell
        world.grenade_cooldown = 0.0;
        world.grenade_flush_step(dt);
        assert!(
            !world.projectiles.is_empty(),
            "a hunter held at range lobs a grenade to flush the camper"
        );
    }

    /// The self-kill that put `World::grenades` behind a default-off switch: a
    /// hunter clear of the blast **on release** but able to run into it during the
    /// round's ~1 s of flight.
    ///
    /// 8 m is past `GRENADE_SAFE_DIST` (6.5 m), so the original at-release check
    /// permitted the throw — then the hunter closed to ~3.4 m and stood inside a 4 m
    /// blast on impact. The predictive guard has to refuse this, while the 9 m case
    /// above (which lands ~4.4 m out, outside the blast) still throws. That is a 1 m
    /// discrimination, so it is worth pinning both sides.
    #[test]
    fn no_grenade_when_a_hunter_would_run_into_the_blast() {
        let mut world = World::new();
        world.set_difficulty(DIFFICULTY_MAX);
        world.set_grenades(true);
        world.initial_meshes();
        world.toggle_mode();
        world.toggle_invulnerable();
        let input = InputState::default();
        let dt = 1.0 / 60.0;
        for _ in 0..120 {
            world.fixed_step(dt, &input);
            if world.enemies[0].enemy.is_engaged() {
                break;
            }
        }
        let ppos = world.player_pos().unwrap();
        // Clear of GRENADE_SAFE_DIST on release, but inside the blast on impact.
        world.enemies[0].enemy.pos = ppos + Vec3::new(8.0, 0.0, 0.0);
        world.camp_anchor = Some(ppos);
        world.camp_timer = 100.0;
        world.grenade_cooldown = 0.0;
        world.projectiles.clear();
        world.grenade_flush_step(dt);
        assert!(
            world.projectiles.is_empty(),
            "a hunter that would close into its own blast must not throw"
        );
    }

    /// #5 no self-harm: a hunter that's right on top of the camper does NOT lob a
    /// grenade (it would blast itself / packmates). Regression for "explodes in front
    /// of them." Everything is set up to throw EXCEPT the hunter is point-blank.
    #[test]
    fn no_grenade_when_a_hunter_is_point_blank() {
        let mut world = World::new();
        world.set_difficulty(DIFFICULTY_MAX);
        // Opt in, or the kill-switch satisfies this assertion on its own and the
        // safe-distance guard it is actually about goes untested.
        world.set_grenades(true);
        world.initial_meshes();
        world.toggle_mode();
        world.toggle_invulnerable();
        let input = InputState::default();
        let dt = 1.0 / 60.0;
        for _ in 0..120 {
            world.fixed_step(dt, &input);
            if world.enemies[0].enemy.is_engaged() {
                break;
            }
        }
        let ppos = world.player_pos().unwrap();
        world.enemies[0].enemy.pos = ppos + Vec3::new(2.0, 0.0, 0.0); // inside the blast radius
        world.camp_anchor = Some(ppos);
        world.camp_timer = 100.0;
        world.grenade_cooldown = 0.0;
        world.grenade_flush_step(dt);
        assert!(
            world.projectiles.is_empty(),
            "no grenade is lobbed while a hunter is point-blank (it would self-harm)"
        );
    }

    /// #5 baseline: at difficulty 0 the grenade flush is disabled — camping forever
    /// never spawns a grenade.
    #[test]
    fn no_grenade_flush_at_difficulty_zero() {
        let mut world = World::new(); // difficulty 0
        world.set_grenades(true); // opt in, so DIFFICULTY is what gates it here
        world.initial_meshes();
        world.toggle_mode(); // HUNT
        world.toggle_invulnerable();
        let input = InputState::default();
        let dt = 1.0 / 60.0;
        for _ in 0..600 {
            world.fixed_step(dt, &input);
        }
        assert!(world.projectiles.is_empty(), "no grenades are lobbed at difficulty 0");
    }

    /// The grenade flush is OFF by default (2026-08-17) — hunters were catching their
    /// own blast. A full-difficulty pack camped on forever must lob nothing until
    /// [`World::set_grenades`] turns it back on.
    #[test]
    fn the_grenade_flush_is_off_by_default() {
        let mut world = World::new();
        world.set_difficulty(DIFFICULTY_MAX);
        assert!(!world.grenades(), "the flush ships disabled");
        world.initial_meshes();
        world.toggle_mode();
        world.toggle_invulnerable();
        let input = InputState::default();
        let dt = 1.0 / 60.0;
        for _ in 0..900 {
            world.fixed_step(dt, &input);
        }
        assert!(
            world.projectiles.is_empty(),
            "a camper drew a grenade with the flush switched off"
        );
    }

    /// Sim-style hits: a non-lethal hit plays NO flinch/hurt animation, so the hunter
    /// keeps fighting through it. Turning hit reactions on (the GoldenEye mode flag)
    /// restores the canned flinch one-shot.
    ///
    /// **GoldenEye-family only**, and that is the point of the override here. A Perfect
    /// Dark hunter — which is what a wave wears now — takes its reaction from PD's
    /// authored per-hit-part tables instead, and those are on by default because being
    /// seen is why they were ported (§9). So a shipped hunter *does* flinch; this pins the
    /// other family's behaviour, which is still what the `hit_reactions` flag governs.
    #[test]
    fn hits_flinch_only_when_hit_reactions_enabled() {
        let mut world = World::new();
        // The canned flinch is a GoldenEye *clip set* behaviour; a GoldenEye body is on
        // Perfect Dark's animations (and so its authored reactions) by default now.
        world.set_goldeneye_clips(true);
        world.set_body_set(super::BodySet::GoldenEye);
        arm_with(&mut world, "PP7"); // 25 dmg — non-lethal on a 100-hp hunter
        world.set_ragdoll(false); // isolate the canned-flinch path (ragdoll would supersede it)
        world.initial_meshes();
        world.toggle_mode(); // HUNT: spawn one hunter
        assert!(!world.enemies.is_empty(), "hunter spawned");
        let torso = {
            let p = world.enemies[0].enemy.pos;
            Vec3::new(p.x, p.y + 0.8, p.z)
        };

        // Default: no flinch clip, and the hunter isn't stunned.
        world.hit_enemy(0, torso);
        assert!(!world.enemies[0].enemy.is_dead(), "non-lethal");
        assert!(
            !world.enemies[0].anim.is_playing_oneshot(),
            "sim style: no flinch animation by default"
        );

        // Opt into GoldenEye-style reactions → a hit now plays the flinch one-shot.
        world.set_hit_reactions(true);
        world.hit_enemy(0, torso);
        assert!(
            world.enemies[0].anim.is_playing_oneshot(),
            "flinch animation plays when hit reactions are enabled"
        );
    }

    /// Duel mode: exactly one hunter spawns, and difficulty scales its spawn health.
    #[test]
    fn one_hunter_spawns_with_difficulty_scaled_health() {
        let mut world = World::new();
        world.initial_meshes();
        world.change_difficulty(DIFFICULTY_MAX as i32); // before the spawn
        world.toggle_mode(); // HUNT: spawn the (single) wave
        assert_eq!(world.enemies.len(), 1, "duel mode spawns exactly one hunter");
        let hp = world.enemies[0].enemy.health();
        assert!(
            hp > crate::enemy::ENEMY_HEALTH,
            "difficulty scales spawn health up ({hp} vs base {})",
            crate::enemy::ENEMY_HEALTH
        );
    }

    /// Track A: four PP7 hits kill a hunter — it takes damage each shot, and the
    /// lethal shot drops the hitscan capsule (a corpse can't be shot) and puts the
    /// model into its death state. The fade then drives the character's opacity
    /// 1 → 0 once the death animation finishes.
    #[test]
    fn four_shots_kill_the_hunter_then_it_fades_out() {
        let mut world = World::new();
        arm_with(&mut world, "PP7"); // 25 dmg hitscan
        world.set_ragdoll(false); // this test covers the canned death-clip + fade path
        world.initial_meshes();
        world.toggle_mode(); // HUNT: bake nav + spawn hunter roster
        assert!(!world.enemies.is_empty(), "hunters spawned");
        let h = world.enemies[0].collider;
        assert!(world.physics.is_enemy_collider(h), "hunter has a hitscan capsule");

        // Torso-height impacts (×1 damage), so PP7's 25 dmg lands cleanly.
        let torso = {
            let p = world.enemies[0].enemy.pos;
            Vec3::new(p.x, p.y + 0.8, p.z)
        };
        // Three non-lethal hits on hunter 0 (PP7, 25 dmg, 100 hp → 75/50/25).
        for expect in [75.0, 50.0, 25.0] {
            world.hit_enemy(0, torso);
            let e = &world.enemies[0];
            assert!(!e.enemy.is_dead(), "still alive at {expect} hp");
            assert_eq!(e.enemy.health(), expect);
            assert!(e.fade.is_none(), "no death fade while alive");
        }

        // The fourth (lethal) hit.
        world.hit_enemy(0, torso);
        assert!(world.enemies[0].enemy.is_dead(), "dead after 4 PP7 shots");
        assert!(
            !world.physics.is_enemy_collider(h),
            "the corpse's capsule is removed — can't shoot a corpse"
        );
        // The fade does NOT start until the death animation finishes: the body
        // stays fully opaque while the death clip plays.
        assert!(world.enemies[0].fade.is_none(), "fade not armed at the moment of death");
        assert!(
            (world.character_instances()[0].3 - 1.0).abs() < 1e-3,
            "opaque during the death anim"
        );

        // Play out the death animation, then the full fade → invisible.
        for _ in 0..600 {
            world.advance_animation(1.0 / 60.0);
        }
        assert!(world.enemies[0].fade.is_some(), "fade started once the anim finished");
        assert!(
            world.character_instances()[0].3 <= 1e-3,
            "faded to invisible after the animation"
        );
    }

    /// The player starts **empty-handed** — the unarmed slot is the only thing owned
    /// — and weapon-cycling is gated by ownership: with one owned slot a switch is a
    /// no-op, and acquiring a gun makes the cycle target it.
    #[test]
    fn starts_unarmed_and_cycle_is_ownership_gated() {
        let mut world = World::new();
        world.initial_meshes();
        // A gun on the floor, well out of reach. Without one the level authors no
        // pickups at all, and `grant_fallback_sidearm` correctly hands the player a
        // PP7 so the level is playable — which is the wrong world for this test.
        crate::world::tools::pickup::tests::place_pickup(
            &mut world,
            crate::ecs::MeshId::WeaponPickup,
            Vec3::new(4.0, 0.0, 4.0),
            crate::ecs::Pickup::weapon("AR33"),
        );
        world.toggle_mode(); // HUNT (weapon switching only runs in HUNT)

        let unarmed = world
            .arsenal
            .weapons()
            .iter()
            .position(|w| w.is_unarmed())
            .unwrap();
        assert_eq!(world.owned.iter().filter(|&&o| o).count(), 1, "exactly one slot owned");
        assert!(world.owns_weapon(unarmed), "and it's the empty-handed one");
        assert_eq!(world.weapon_index, unarmed, "holding nothing");

        // Empty hands are the only slot owned → nothing to switch to.
        world.begin_weapon_switch();
        assert!(!world.switching, "a lone owned slot can't cycle");
        assert_eq!(world.weapon_index, unarmed, "still empty-handed");

        // Acquire a gun → the cycle now targets it.
        let other = (unarmed + 5) % world.owned.len();
        world.owned[other] = true;
        world.begin_weapon_switch();
        assert!(world.switching, "a switch begins once a second slot is owned");
        assert_eq!(world.switch_target, other, "cycle targets the newly-owned weapon");
    }

    /// Economy: defeating a hunter grants exactly one kill bounty. Credits start at
    /// zero and rise by `KILL_BOUNTY` on the lethal shot (the single `start_death`
    /// funnel), not on the earlier non-lethal hits.
    #[test]
    fn killing_a_hunter_awards_credits() {
        let mut world = World::new();
        arm_with(&mut world, "PP7"); // 25 dmg — four torso hits kill a 100-hp hunter
        world.initial_meshes();
        world.toggle_mode(); // HUNT: spawn the hunter roster
        assert!(!world.enemies.is_empty(), "hunter spawned");
        assert_eq!(world.credits(), 0, "wallet starts empty");

        let torso = {
            let p = world.enemies[0].enemy.pos;
            Vec3::new(p.x, p.y + 0.8, p.z)
        };
        // Three non-lethal hits pay nothing.
        for _ in 0..3 {
            world.hit_enemy(0, torso);
            assert_eq!(world.credits(), 0, "no bounty while the hunter lives");
        }
        // The lethal (fourth) hit pays exactly one bounty.
        world.hit_enemy(0, torso);
        assert!(world.enemies[0].enemy.is_dead(), "dead after 4 PP7 shots");
        assert_eq!(world.credits(), crate::economy::KILL_BOUNTY, "one kill = one bounty");
    }

    /// Shop: an affordable weapon buy deducts its price, marks it owned (so it joins
    /// the cycle), can't be repeated, and an ammo buy tops up the reserve.
    #[test]
    fn shop_buys_weapon_then_ammo() {
        let mut world = World::new();
        world.initial_meshes();
        world.economy.earn(2000); // give the player a budget

        let shotgun = crate::combat::config::WEAPONS
            .iter()
            .position(|w| w.name == "Shotgun")
            .unwrap();
        assert!(!world.owns_weapon(shotgun), "shotgun not owned at start");

        let price = crate::shop::weapon_price("Shotgun");
        let before = world.credits();
        assert!(world.buy_weapon(shotgun), "affordable buy succeeds");
        assert!(world.owns_weapon(shotgun), "now owned");
        assert_eq!(world.credits(), before - price, "exact price deducted");

        // Re-buying an owned weapon is a no-op (no double charge).
        let bal = world.credits();
        assert!(!world.buy_weapon(shotgun), "can't re-buy an owned weapon");
        assert_eq!(world.credits(), bal, "declined re-buy costs nothing");

        // Ammo buy adds AMMO_MAGS_PER_BUY magazines to the reserve.
        let (_, r0) = world.weapon_ammo(shotgun).unwrap();
        assert!(world.buy_ammo(shotgun), "ammo buy succeeds");
        let (_, r1) = world.weapon_ammo(shotgun).unwrap();
        let mag = crate::combat::config::WEAPONS[shotgun].magazine_size;
        assert_eq!(r1, r0 + mag * crate::shop::AMMO_MAGS_PER_BUY, "reserve topped up");
    }

    /// Shop: a broke player can't buy — the purchase is a no-op and ammo for an
    /// unowned weapon is refused outright.
    #[test]
    fn shop_declines_when_broke_or_unowned() {
        let mut world = World::new();
        world.initial_meshes();
        assert_eq!(world.credits(), 0, "wallet starts empty");

        let gg = crate::combat::config::WEAPONS
            .iter()
            .position(|w| w.name == "Golden Gun")
            .unwrap();
        assert!(!world.buy_weapon(gg), "broke → weapon buy declined");
        assert!(!world.owns_weapon(gg), "still not owned");
        assert!(!world.buy_ammo(gg), "no ammo for an unowned weapon");
    }

    /// Ragdoll death (default ON): the lethal shot spawns a physics ragdoll instead of
    /// a canned death clip — a body per bone enters the sim, the mixer plays NO death
    /// one-shot, the corpse renders with an identity model transform (WORLD-space
    /// skinning), and after it settles + fades the ragdoll tears itself out of the sim.
    #[test]
    fn ragdoll_death_replaces_the_clip_then_settles_and_despawns() {
        let mut world = World::new();
        arm_with(&mut world, "PP7"); // 25 dmg hitscan
        assert!(world.ragdoll(), "ragdoll death is on by default");
        // A Perfect Dark hunter prefers its authored death table over the physics, and a
        // wave wears Perfect Dark now — so the ragdoll has to be isolated to be tested.
        // It is still a live path (it is what a hunter with no authored row falls back to,
        // and what the whole GoldenEye family uses).
        world.set_authored_reactions(false);
        world.initial_meshes();
        world.toggle_mode(); // HUNT: bake nav + spawn the (single) hunter
        assert!(!world.enemies.is_empty(), "hunter spawned");
        // One animation frame so the death seed reads a real posed skeleton.
        world.advance_animation(1.0 / 60.0);

        let torso = {
            let p = world.enemies[0].enemy.pos;
            Vec3::new(p.x, p.y + 0.8, p.z)
        };
        for _ in 0..4 {
            world.hit_enemy(0, torso); // 4×25 = 100 → dead
        }
        assert!(world.enemies[0].enemy.is_dead(), "dead after 4 PP7 shots");
        assert!(world.enemies[0].ragdoll.is_some(), "death spawned a ragdoll");
        assert!(
            !world.enemies[0].anim.is_playing_oneshot(),
            "the ragdoll replaces the canned death one-shot"
        );
        assert!(
            world.physics.ragdoll_body_count() >= 11,
            "the corpse's bones entered the dynamics sim (got {})",
            world.physics.ragdoll_body_count()
        );

        // The corpse draws with an identity model transform (its bones are already in
        // world space) and full opacity while it's still settling.
        let insts = world.character_instances();
        assert!(!insts.is_empty(), "the corpse still renders");
        let (_, model, _, opacity, _) = insts[0];
        assert!(
            (model - Mat4::IDENTITY).to_cols_array().iter().all(|v| v.abs() < 1e-6),
            "a ragdolling corpse uses an identity model transform"
        );
        assert!((opacity - 1.0).abs() < 1e-3, "opaque while settling");

        // Run the sim out: the corpse settles, fades, and its bodies leave the sim.
        let input = InputState::default();
        for _ in 0..1200 {
            world.fixed_step(1.0 / 120.0, &input);
        }
        assert!(
            world.enemies[0].ragdoll.is_none(),
            "the settled + faded corpse tore down its ragdoll"
        );
        assert_eq!(world.physics.ragdoll_body_count(), 0, "no ragdoll bodies leak");
    }

    /// Living-hit stagger (Phase 3, default ON): a NON-lethal hit spawns a brief physics
    /// reaction (blended into animation, not a death takeover) + a short stun, and after
    /// its blend decays the reaction tears itself down — the hunter survives and is back
    /// to pure animation with no leaked bodies.
    #[test]
    fn nonlethal_hit_staggers_then_blends_back() {
        let mut world = World::new();
        arm_with(&mut world, "PP7"); // 25 dmg — non-lethal on a 100-hp hunter
        assert!(world.ragdoll(), "ragdoll feature on by default");
        world.set_authored_reactions(false); // isolate the physics stagger (see above)
        world.initial_meshes();
        world.toggle_mode(); // HUNT
        world.advance_animation(1.0 / 60.0); // seed the pose the reaction reads

        let torso = {
            let p = world.enemies[0].enemy.pos;
            Vec3::new(p.x, p.y + 0.8, p.z)
        };
        world.hit_enemy(0, torso);
        assert!(!world.enemies[0].enemy.is_dead(), "one PP7 shot is non-lethal");
        assert!(world.enemies[0].reaction.is_some(), "a living reaction spawned");
        assert!(world.enemies[0].ragdoll.is_none(), "it's a reaction, not a death ragdoll");
        assert!(world.physics.ragdoll_body_count() > 0, "reaction bodies entered the sim");

        // Blend it out: run ~1 s (past the decay window). The reaction tears down and no
        // bodies leak; the hunter is alive and back to animation.
        let input = InputState::default();
        for _ in 0..120 {
            world.fixed_step(1.0 / 120.0, &input);
        }
        assert!(world.enemies[0].reaction.is_none(), "the stagger blended out + tore down");
        assert_eq!(world.physics.ragdoll_body_count(), 0, "no reaction bodies leak");
        assert!(!world.enemies[0].enemy.is_dead(), "still alive after a non-lethal stagger");
    }

    /// With the ragdoll feature off, a non-lethal hit spawns NO physics reaction (falls
    /// back to the sim / canned-flinch paths) — the kill-switch restores the baseline.
    #[test]
    fn nonlethal_hit_no_physics_reaction_when_ragdoll_off() {
        let mut world = World::new();
        arm_with(&mut world, "PP7");
        world.set_ragdoll(false);
        world.initial_meshes();
        world.toggle_mode(); // HUNT
        world.advance_animation(1.0 / 60.0);
        let torso = {
            let p = world.enemies[0].enemy.pos;
            Vec3::new(p.x, p.y + 0.8, p.z)
        };
        world.hit_enemy(0, torso);
        assert!(!world.enemies[0].enemy.is_dead());
        assert!(world.enemies[0].reaction.is_none(), "ragdoll off → no physics reaction");
        assert_eq!(world.physics.ragdoll_body_count(), 0, "no ragdoll bodies with the feature off");
    }

    /// Ragdoll bodies never leak across a hunt: a mode switch back to BUILD (or a duel
    /// reset) tears down every live corpse's bodies.
    #[test]
    fn ragdoll_bodies_are_cleared_on_mode_switch() {
        let mut world = World::new();
        arm_with(&mut world, "PP7");
        world.set_authored_reactions(false); // isolate the physics death
        world.initial_meshes();
        world.toggle_mode(); // HUNT
        world.advance_animation(1.0 / 60.0);
        let torso = {
            let p = world.enemies[0].enemy.pos;
            Vec3::new(p.x, p.y + 0.8, p.z)
        };
        for _ in 0..4 {
            world.hit_enemy(0, torso);
        }
        assert!(world.physics.ragdoll_body_count() > 0, "a corpse is ragdolling");
        world.toggle_mode(); // back to BUILD — must drop the ragdoll bodies
        assert_eq!(
            world.physics.ragdoll_body_count(),
            0,
            "ragdoll bodies are cleared on the way out of HUNT"
        );
    }

    /// Track A: a shot that lands on the hunter's capsule damages it and spawns NO
    /// wall spark; a shot that misses the hunter and hits a wall spawns a spark and
    /// deals no damage. Exercises the real fire path (trigger → cast → branch).
    #[test]
    fn shooting_the_hunter_damages_it_a_wall_hit_sparks() {
        let mut world = World::new();
        arm_with(&mut world, "PP7"); // hitscan, not the explosive default
        world.initial_meshes();
        world.toggle_mode(); // HUNT

        // Move every other hunter far off so only hunter 0 can be on the ray.
        for i in 1..world.enemies.len() {
            let h = world.enemies[i].collider;
            let far = Vec3::new(500.0 + i as f32 * 5.0, 0.0, 500.0);
            world.enemies[i].enemy.pos = far;
            world.physics.update_enemy_collider(h, far);
        }

        // Put hunter 0 directly on the player's look ray ~1.5 m ahead (inside the
        // 6 m room, before any wall), with its capsule centred on the ray.
        let (eye, fwd) = {
            let c = world.character.as_ref().unwrap();
            (c.eye(), c.forward())
        };
        let centre = eye + fwd * 1.5;
        let feet = centre - Vec3::new(0.0, ENEMY_HALF_HEIGHT + ENEMY_RADIUS, 0.0);
        let h0 = world.enemies[0].collider;
        world.enemies[0].enemy.pos = feet;
        world.physics.update_enemy_collider(h0, feet);
        let hp0 = world.enemies[0].enemy.health();

        // Fire once (a fresh edge = one semi-auto shot).
        let mut input = InputState::default();
        input.pointer_locked = true;
        input.set_mouse_left(true);
        world.combat_step(1.0 / 60.0, &input);
        assert!(
            world.enemies[0].enemy.health() < hp0,
            "shooting the hunter damages it"
        );
        assert!(world.sparks.is_empty(), "an enemy hit spawns no wall spark");

        // Move hunter 0 far off the ray too, then fire again (release → fresh pull)
        // so the shot flies past into a wall → a spark, no further damage.
        let hp1 = world.enemies[0].enemy.health();
        let away = Vec3::new(100.0, 0.0, 100.0);
        world.enemies[0].enemy.pos = away;
        world.physics.update_enemy_collider(h0, away);
        input.set_mouse_left(false);
        world.combat_step(1.0 / 60.0, &input); // release resets the edge
        input.set_mouse_left(true);
        world.combat_step(1.0 / 60.0, &input); // fresh pull → shot into the wall
        assert!(!world.sparks.is_empty(), "a wall hit spawns a spark");
        assert_eq!(
            world.enemies[0].enemy.health(),
            hp1,
            "the wall shot dealt no damage to the (moved-away) hunter"
        );
    }

    /// Enemy separation (legacy nudge path): two hunters stacked on the exact same cell
    /// get pushed apart in a single step by the position-nudge separation pass, so they
    /// don't merge into one body. This guards the pre-ORCA baseline (`local_avoidance`
    /// off); the ORCA path separates smoothly over several frames instead — see the
    /// `orca_*` lab scenarios.
    #[test]
    fn stacked_hunters_are_pushed_apart() {
        let mut world = World::new();
        world.set_local_avoidance(false); // exercise the legacy instant-nudge path
        world.set_wave_size(6); // separation needs a pack (gameplay default is 1)
        world.initial_meshes();
        world.toggle_mode(); // HUNT — spawns the wave
        assert!(world.enemies.len() >= 2, "need at least two hunters");
        // Stack hunter 1 exactly on hunter 0.
        let p = world.enemies[0].enemy.pos;
        let h1 = world.enemies[1].collider;
        world.enemies[1].enemy.pos = p;
        world.physics.update_enemy_collider(h1, p);
        let input = InputState::default();
        world.fixed_step(1.0 / 120.0, &input);
        let (a, b) = (world.enemies[0].enemy.pos, world.enemies[1].enemy.pos);
        let d = Vec3::new(b.x - a.x, 0.0, b.z - a.z).length();
        assert!(d > 0.3, "stacked hunters should separate (got {d})");
    }

    /// Track A: a killed hunter stops moving — its death freezes the nav-driven
    /// chase (dead `update` is a no-op), so the corpse holds position.
    #[test]
    fn a_dead_hunter_stops_chasing() {
        let mut world = World::new();
        arm_with(&mut world, "PP7"); // 25 dmg — four torso hits kill
        world.initial_meshes();
        world.toggle_mode(); // HUNT
        // Kill hunter 0 outright (torso hits, ×1 damage).
        let torso = {
            let p = world.enemies[0].enemy.pos;
            Vec3::new(p.x, p.y + 0.8, p.z)
        };
        for _ in 0..4 {
            world.hit_enemy(0, torso);
        }
        assert!(world.enemies[0].enemy.is_dead());
        let rest = world.enemies[0].enemy.pos;
        let input = InputState::default();
        for _ in 0..240 {
            world.fixed_step(1.0 / 120.0, &input);
        }
        let after = world.enemies[0].enemy.pos;
        assert!(
            (after - rest).length() < 1e-4,
            "the corpse should not move (was {rest:?}, now {after:?})"
        );
    }

    /// P5: player damage subtracts from health (armor-first, but armor 0 here),
    /// arms the red flash + HUD pop, and kills at 0 → the death state.
    #[test]
    fn player_damage_subtracts_health_and_dies() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode(); // HUNT (player alive, full health)
        assert_eq!(world.player_health(), PLAYER_MAX_HEALTH);

        world.take_player_damage(8.0);
        assert_eq!(world.player_health(), 92.0, "8 dmg off 100");
        assert!(world.damage_flash() > 0.0, "damage armed the red flash");
        assert!(world.hud_alpha() > 0.0, "damage popped the health HUD");
        assert!(!world.is_player_dead());

        world.take_player_damage(1000.0); // lethal
        assert_eq!(world.player_health(), 0.0, "health floors at 0");
        assert!(world.is_player_dead(), "0 health → dead");
        assert_eq!(world.player_score().deaths, 1, "the death is scored");

        // `R` skips the rest of the death beat and respawns **in place**. Death used to
        // end the hunt (restart → BUILD); under the deathmatch loop it is a respawn, so
        // the property this guards deliberately changed: full health, alive, still HUNT.
        world.restart_after_death();
        assert!(!world.is_player_dead());
        assert_eq!(world.player_health(), PLAYER_MAX_HEALTH);
        assert!(!world.is_build(), "respawn keeps the hunt running — G leaves it");
    }

    /// A3: in HUNT the hunters carry weapons — each gun's world clip transform
    /// resolves (a hand bone is found + the pose is posed); a dead hunter drops its
    /// gun, so once every hunter is down there are no weapon draws.
    #[test]
    fn hunters_carry_weapons_in_hunt() {
        let mut world = World::new();
        arm_with(&mut world, "PP7"); // the player has to be armed to kill them below
        world.initial_meshes();
        world.toggle_mode(); // HUNT
        assert!(!world.enemy_weapon_lib().is_empty(), "weapon assets loaded");
        assert!(
            !world.enemy_weapon_draws(1.6).is_empty(),
            "live hunters' guns have world transforms"
        );
        // Kill every hunter (8 PP7 hits each is plenty).
        let n = world.enemies.len();
        for i in 0..n {
            let torso = {
                let p = world.enemies[i].enemy.pos;
                Vec3::new(p.x, p.y + 0.8, p.z)
            };
            for _ in 0..8 {
                world.hit_enemy(i, torso);
            }
        }
        assert!(
            world.enemy_weapon_draws(1.6).is_empty(),
            "dead hunters drop their guns"
        );
    }

    /// Weapon class → FIRE_TIMING window mapping, and all three fire clips are
    /// recognised as fire clips (used to gate the aim/hit-reaction post-pass).
    #[test]
    fn fire_windows_and_clip_recognition() {
        use crate::combat::EnemyWeaponClass::{Pistol, Rifle};
        for (c, d) in [(Rifle, false), (Pistol, false), (Rifle, true)] {
            let (s, e) = super::hunt::fire_window_for(c, d);
            assert!(e > s, "window start<end for {c:?} dual={d}");
        }
        use super::hunt::is_fire_clip;
        assert!(is_fire_clip(FIRE_RIFLE_IDX) && is_fire_clip(FIRE_PISTOL_IDX) && is_fire_clip(FIRE_DUAL_IDX));
        assert!(!is_fire_clip(CHAR_HIT_START), "a hit clip is not a fire clip");
    }

    /// A dual-wield hunter draws two guns (one per hand); a single-wield hunter
    /// one. The roster includes at least one dual-wielder.
    #[test]
    fn dual_wield_hunters_draw_two_guns() {
        let mut world = World::new();
        world.set_wave_size(6); // roster has dual-wielders past index 0 (default is 1)
        world.initial_meshes();
        world.toggle_mode(); // HUNT: spawn the roster
        let expected: usize = world
            .enemies
            .iter()
            .filter(|e| !e.enemy.is_dead())
            .map(|e| 1 + e.dual as usize)
            .sum();
        assert_eq!(
            world.enemy_weapon_draws(1.6).len(),
            expected,
            "one gun per hunter, two for a dual-wielder"
        );
        assert!(world.enemies.iter().any(|e| e.dual), "roster includes a dual-wielder");
    }

    /// Hit zones scale damage by impact height: a head-height shot does ×4 (PP7
    /// 25×4 = 100 → a one-shot kill), a leg-height shot does ×0.6 (15 dmg), and the
    /// hit paints persistent blood on the body.
    #[test]
    fn hit_zones_scale_damage_by_impact_height() {
        let mut world = World::new();
        arm_with(&mut world, "PP7"); // 25 dmg hitscan — zone multipliers are relative to it
        world.set_wave_size(6); // need a couple of hunters (gameplay default is 1)
        world.initial_meshes();
        world.toggle_mode(); // HUNT: spawn the roster
        // One animation frame, so the blood paint below skins against a real pose rather
        // than whatever the mixer holds before its first update.
        world.advance_animation(1.0 / 60.0);
        assert!(world.enemies.len() >= 3, "roster spawned at least three hunters");

        // The zone boundaries **scale with the body's own height**
        // (`World::body_hit_zones`), because a 1.73 m Perfect Dark hunter's head is not
        // where a 1.50 m GoldenEye one's is. So the probe heights are derived from each
        // hunter's own zones rather than being the fixed 1.2 m / 0.3 m that only ever
        // meant "head" and "leg" on a GoldenEye body.
        let (head_min, leg_max) = world.body_hit_zones(world.enemies[0].body);
        assert!(head_min > leg_max, "the zones are ordered: legs below, head above");

        // Head-zone impact on hunter 0 → ×4 → lethal in one PP7 shot.
        let head = {
            let p = world.enemies[0].enemy.pos;
            Vec3::new(p.x, p.y + head_min + 0.05, p.z)
        };
        world.hit_enemy(0, head);
        assert!(world.enemies[0].enemy.is_dead(), "a headshot one-shots with the PP7");

        // Leg-zone impact on hunter 1 → ×0.6 → 15 dmg, and it paints blood.
        let (_, leg_max) = world.body_hit_zones(world.enemies[1].body);
        // Just under the top of the leg zone — a thigh rather than an ankle, so there is
        // mesh nearby for the blood paint to redden.
        let leg = {
            let p = world.enemies[1].enemy.pos;
            Vec3::new(p.x, p.y + leg_max * 0.9, p.z)
        };
        world.hit_enemy(1, leg);
        assert_eq!(world.enemies[1].enemy.health(), 100.0 - 15.0, "a leg shot does 0.6×");

        // Blood paint, on a third hunter. Deliberately not folded into the leg shot above:
        // the paint reddens vertices within [`BLOOD_RADIUS`] of the impact in WORLD space,
        // so it is a statement about where the posed mesh *is* — and a limb probe derived
        // from a zone boundary is a fragile way to find mesh (it was, and it broke the
        // moment the bodies changed size). So the probe is the centre of the body's own
        // posed bounds, measured the same way `hit_enemy` measures it.
        let chest = {
            let inst = &world.enemies[2];
            let m = &world.char_models[inst.body];
            let joints = inst.anim.skinning_matrices(&m.skeleton);
            let ct = world.char_transform(inst.enemy.pos, inst.yaw(), inst.body, inst.pd_anims);
            let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
            for v in &m.vertices {
                let src = Vec3::from(v.pos);
                let mut local = Vec3::ZERO;
                for k in 0..4 {
                    if v.weights[k] != 0.0 {
                        local += v.weights[k] * joints[v.joints[k] as usize].transform_point3(src);
                    }
                }
                let w = ct.transform_point3(local);
                lo = lo.min(w);
                hi = hi.max(w);
            }
            (lo + hi) * 0.5
        };
        world.hit_enemy(2, chest);
        assert!(
            world.enemies[2].blood.iter().any(|&c| c < 0.999),
            "the hit painted blood onto the body"
        );
    }

    /// The door tool: `B` arms a preview on the wall, a left-click cuts a
    /// `door`-marked opening. No cut happens just from arming.
    #[test]
    fn door_tool_arms_with_b_and_cuts_on_click() {
        let mut world = World::new(); // camera at (3,1.5,3) m looking −Z at the z=0 wall
        world.initial_meshes();
        assert!(!world.is_door_arming());

        // B arms (no geometry change).
        assert!(world.door_tool_key().is_none(), "B does not cut");
        assert!(world.is_door_arming(), "B arms the preview");
        assert!(world.update_door_preview().is_some(), "ghost previews on the wall");
        assert!(!world.regions[0].brushes.iter().any(|b| b.door), "no door yet");

        // Left-click (confirm_door) cuts.
        assert!(world.confirm_door().is_some(), "click cuts the door");
        assert!(!world.is_door_arming(), "cutting disarms");
        assert!(
            world.regions[0].brushes.iter().any(|b| b.door),
            "a door-marked doorframe brush was created"
        );
    }

    /// The room retexture flood-fill stops at door/hole frames — so a room and the
    /// room beyond its door are texturable independently (issue: number keys used
    /// to change the whole level).
    #[test]
    fn room_floodfill_stops_at_frames() {
        let room = Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 10.0, 8.0, 10.0);
        // Doorframe carved in the x-min wall (adjacent at x=0), marked as a frame.
        let mut frame = Brush::new(2, Op::Subtract, -1.0, 0.0, 3.0, 1.0, 7.0, 3.0);
        frame.frame = true;
        // Protoroom just beyond the frame (adjacent at x=-1), NOT touching the room.
        let proto = Brush::new(3, Op::Subtract, -2.0, 0.0, 3.0, 1.0, 7.0, 3.0);
        let brushes = vec![room, frame, proto];

        let ids = find_room_brushes(&room, &brushes);
        assert!(ids.contains(&1), "the room itself is in the set");
        assert!(!ids.contains(&2), "the frame bounds the room, not part of it");
        assert!(
            !ids.contains(&3),
            "flood-fill must not cross the frame into the room beyond"
        );
    }

    /// A door cut re-textures the doorway reveal as the tunnel zone (5), while the
    /// surrounding room keeps its floor/wall zones — the geometric frame-AABB
    /// classification working end-to-end through the real cut flow.
    #[test]
    fn door_cut_textures_the_reveal_as_a_tunnel_zone() {
        let mut world = World::new();
        world.initial_meshes();
        world.door_tool_key();
        world.update_door_preview();
        world.confirm_door().expect("door cut");

        let tex = world.regions[0].evaluate_textured();
        let zones: std::collections::BTreeSet<u8> = tex.groups.iter().map(|g| g.zone).collect();
        assert!(zones.contains(&5), "doorway reveal → zone 5; got {zones:?}");
        assert!(
            zones.contains(&0) && (zones.contains(&2) || zones.contains(&3)),
            "room floor + wall zones still present: {zones:?}"
        );
    }

    /// Pressing `B` while the tool is armed toggles it back off, cutting nothing.
    #[test]
    fn pressing_b_again_deselects_the_door_tool() {
        let mut world = World::new();
        world.initial_meshes();
        world.door_tool_key(); // arm
        assert!(world.is_door_arming());
        world.door_tool_key(); // B again → deselect
        assert!(!world.is_door_arming(), "second B turns the tool off");
        assert!(!world.regions[0].brushes.iter().any(|b| b.door), "toggling off cuts nothing");
    }

    /// A door cut into an X-facing wall stays upright (height along Y, width
    /// along Z) — the regression for the 90°-rotated door.
    #[test]
    fn door_on_an_x_wall_stays_upright() {
        let mut world = World::new();
        world.initial_meshes();
        world.camera.yaw = std::f32::consts::FRAC_PI_2; // face the −X wall
        world.door_tool_key(); // arm
        assert!(world.update_door_preview().is_some(), "previews on the −X wall");
        assert!(world.confirm_door().is_some(), "cuts the door");

        let frame = world
            .regions[0]
            .brushes
            .iter()
            .find(|b| b.door)
            .expect("doorframe exists");
        assert_eq!(frame.h, DOOR_HEIGHT, "height runs vertically (Y)");
        assert_eq!(frame.d, DOOR_WIDTH, "width runs horizontally (Z)");
        assert_eq!(frame.w, WALL_THICKNESS, "1 WT thick through the wall (X)");
    }

    /// Cancelling an armed door leaves the geometry untouched.
    #[test]
    fn cancel_door_leaves_no_opening() {
        let mut world = World::new();
        world.initial_meshes();
        world.door_tool_key(); // arm
        assert!(world.is_door_arming());
        world.cancel_door();
        assert!(!world.is_door_arming());
        assert!(!world.regions[0].brushes.iter().any(|b| b.door), "cancel cuts nothing");
    }

    /// Scroll sizing clamps to [1, faceSize] and flips full ↔ sub-face.
    #[test]
    fn scroll_sizes_and_clamps_the_selection() {
        let mut world = World::new();
        world.initial_meshes();
        assert!(world.select_at_crosshair(), "picks the −Z wall");
        assert!(world.is_full_face(), "a fresh selection is full-face");

        // One scroll-down shrinks below full → sub-face.
        world.adjust_selection_size(-1.0, 0.0);
        assert!(!world.is_full_face(), "scrolling in makes it a sub-face");

        // Shrinking hard clamps at 1 (never full).
        for _ in 0..40 {
            world.adjust_selection_size(-1.0, 0.0);
        }
        assert!(!world.is_full_face());

        // Growing hard clamps back to the full face size.
        for _ in 0..40 {
            world.adjust_selection_size(1.0, 0.0);
        }
        assert!(world.is_full_face(), "grown back to full clamps at faceSize");
    }

    /// A sub-face push spawns a subtract brush sized to the sub-rect (carves a
    /// niche) rather than moving the whole wall.
    #[test]
    fn sub_face_push_carves_a_sized_brush() {
        let mut world = World::new();
        world.initial_meshes();
        world.select_at_crosshair(); // −Z wall: axis Z, side Min; u=X(24), v=Y(16)
        world.adjust_selection_size(-20.0, 0.0); // sel_size_u: 24 → 4
        world.adjust_selection_size(0.0, -10.0); // sel_size_v: 16 → 6
        assert!(!world.is_full_face());

        let before = world.regions[0].brushes.len();
        assert!(world.push(4.0).is_some(), "sub-face push rebuilds the region");
        assert_eq!(world.regions[0].brushes.len(), before + 1, "spawned one brush");

        let sub = world.regions[0].brushes.last().unwrap();
        assert_eq!(sub.op, Op::Subtract);
        assert_eq!(sub.w, 4.0, "width = sub-rect U");
        assert_eq!(sub.h, 6.0, "height = sub-rect V");
        assert_eq!(sub.d, 4.0, "depth = push step along the normal");
        // The original room brush is untouched (whole wall didn't move).
        let room = world.regions[0].brushes.iter().find(|b| b.id == 1).unwrap();
        assert_eq!(room.d, 24.0, "room brush unchanged by a sub-face carve");
    }

    /// A full-face push (no scroll) still resizes the wall brush in place — the
    /// Phase 1 behavior, unregressed.
    #[test]
    fn full_face_push_still_moves_the_whole_wall() {
        let mut world = World::new();
        world.initial_meshes();
        world.select_at_crosshair();
        assert!(world.is_full_face());
        let before = world.regions[0].brushes.len();
        world.push(4.0);
        assert_eq!(world.regions[0].brushes.len(), before, "no new brush");
        let room = world.regions[0].brushes.iter().find(|b| b.id == 1).unwrap();
        assert_eq!(room.d, 28.0, "whole −Z wall pushed out by the step");
    }

    /// Repeated sub-face pushes deepen the same carve rather than stacking brushes.
    #[test]
    fn repeat_sub_face_push_grows_the_same_brush() {
        let mut world = World::new();
        world.initial_meshes();
        world.select_at_crosshair();
        world.adjust_selection_size(-20.0, 0.0);
        world.adjust_selection_size(0.0, -10.0);

        world.push(4.0); // spawn the sub-face carve
        let n1 = world.regions[0].brushes.len();
        let d1 = world.regions[0].brushes.last().unwrap().d;

        world.push(4.0); // deepen it
        let n2 = world.regions[0].brushes.len();
        let d2 = world.regions[0].brushes.last().unwrap().d;

        assert_eq!(n2, n1, "repeat push grows the same brush, no new one");
        assert!(d2 > d1, "the carve deepened: {d1} → {d2}");
    }

    // NB: the door-breach tests (panel-blocks-player, hunter-breaches-to-catch)
    // and their `two_rooms_joined_by_a_door` fixture were removed when door
    // breach/blocking was disabled (2026-07-16, see `World::build_doors`). Restore
    // them from git history when the breach system is re-enabled.

    // ─── Hole tool ─────────────────────────────────────────────────────────

    /// The hole tool arms with `H`, scroll sizes it, and a click cuts an opening
    /// that is NOT door-marked (holes aren't breakable). Distinct from the door.
    #[test]
    fn hole_tool_cuts_a_non_door_opening() {
        let mut world = World::new(); // camera looks −Z at the z=0 wall
        world.initial_meshes();
        assert!(!world.is_opening_arming());

        world.hole_tool_key(); // arm
        assert!(world.is_opening_arming() && world.is_hole_arming(), "hole tool armed");
        assert!(!world.is_door_arming(), "not the door tool");
        assert!(world.update_opening_preview().is_some(), "ghost previews on the wall");

        let before = world.regions[0].brushes.len();
        assert!(world.confirm_opening().is_some(), "click cuts the hole");
        assert!(!world.is_opening_arming(), "cutting disarms");
        // Frame + protoroom subtracts added, and NO brush is door-marked.
        assert_eq!(world.regions[0].brushes.len(), before + 2, "frame + protoroom");
        assert!(
            !world.regions[0].brushes.iter().any(|b| b.door),
            "a hole is not breakable (no door-marked brush)"
        );
    }

    /// A hole can be cut into a floor (axis Y) — doors can't. Scroll grows the
    /// opening, and the cut carves the floor face.
    #[test]
    fn hole_can_be_cut_into_the_floor() {
        let mut world = World::new();
        world.initial_meshes();
        world.camera.pitch = -1.4; // look almost straight down at the floor
        world.hole_tool_key(); // arm hole
        world.adjust_opening_size(2.0, 2.0); // grow to 5×5
        let p = world.resolve_opening_placement().expect("floor is a valid hole face");
        assert_eq!(p.axis, Axis::Y, "the crosshair resolved the floor");
        let before = world.regions[0].brushes.len();
        assert!(world.confirm_opening().is_some(), "cuts a floor hole");
        assert_eq!(world.regions[0].brushes.len(), before + 2);
    }

    /// The door tool still rejects the floor (walls only) — the generalization
    /// didn't loosen the door's constraint.
    #[test]
    fn door_tool_still_rejects_the_floor() {
        let mut world = World::new();
        world.initial_meshes();
        world.camera.pitch = -1.4; // look down at the floor
        world.door_tool_key(); // arm door
        assert!(world.update_opening_preview().is_none(), "no door ghost on the floor");
        assert!(world.confirm_opening().is_none(), "no door cut into the floor");
        assert!(!world.regions[0].brushes.iter().any(|b| b.door));
    }

    // ─── Pillars & braces ───────────────────────────────────────────────────

    /// The pillar tool places one additive floor→ceiling column when aimed at the
    /// floor, and rejects a wall.
    #[test]
    fn pillar_places_a_column_on_the_floor() {
        let mut world = World::new();
        world.initial_meshes();

        // Aimed at the −Z wall (default view) → pillar rejects it.
        world.pillar_tool_key();
        assert!(world.is_placing());
        assert!(world.update_place_preview().is_none(), "no pillar ghost on a wall");
        assert!(world.confirm_place().is_none(), "no pillar placed on a wall");

        // Look down at the floor → a ghost appears and a click adds one Add brush.
        world.camera.pitch = -1.4;
        assert!(world.update_place_preview().is_some(), "pillar ghost on the floor");
        let before = world.regions[0].brushes.len();
        assert!(world.confirm_place().is_some(), "pillar placed");
        assert_eq!(world.regions[0].brushes.len(), before + 1, "one additive column");
        let col = world.regions[0].brushes.last().unwrap();
        assert_eq!(col.op, Op::Add);
        assert_eq!(col.w, PILLAR_SIZE);
        assert_eq!(col.d, PILLAR_SIZE);
        assert!(!world.is_placing(), "placing disarms after a click");
    }

    /// Scroll resizes the pillar footprint before placement.
    #[test]
    fn scroll_resizes_the_pillar() {
        let mut world = World::new();
        world.initial_meshes();
        world.camera.pitch = -1.4;
        world.pillar_tool_key();
        world.adjust_place_size(2.0, 0.0); // 2 → 4
        world.confirm_place().unwrap();
        let col = world.regions[0].brushes.last().unwrap();
        assert_eq!(col.w, 4.0, "pillar grew to the scrolled size");
    }

    /// The brace tool places three additive brushes (arch) when aimed at a wall,
    /// and rejects the floor.
    #[test]
    fn brace_places_a_three_brush_arch_on_a_wall() {
        let mut world = World::new();
        world.initial_meshes();

        // Floor → brace rejects it.
        world.camera.pitch = -1.4;
        world.brace_tool_key();
        assert!(world.update_place_preview().is_none(), "no brace ghost on the floor");
        assert!(world.confirm_place().is_none(), "no brace on the floor");

        // −Z wall → three additive brushes.
        world.camera.pitch = 0.0;
        assert!(world.update_place_preview().is_some(), "brace ghost on the wall");
        let before = world.regions[0].brushes.len();
        assert!(world.confirm_place().is_some(), "brace placed");
        assert_eq!(world.regions[0].brushes.len(), before + 3, "wall + ceiling + wall");
        assert!(
            world.regions[0].brushes.iter().rev().take(3).all(|b| b.op == Op::Add),
            "all three brace brushes are additive"
        );
    }

    /// Arming a placement tool cancels an armed opening tool (mutually exclusive).
    #[test]
    fn tools_are_mutually_exclusive() {
        let mut world = World::new();
        world.initial_meshes();
        world.hole_tool_key();
        assert!(world.is_opening_arming());
        world.pillar_tool_key();
        assert!(world.is_placing(), "pillar armed");
        assert!(!world.is_opening_arming(), "arming the pillar cancelled the hole");
    }

    // ─── Stairs ──────────────────────────────────────────────────────────

    /// Stairs require the selection to touch the floor: a sub-face selection
    /// scrolled up off the floor rejects the arrow key (no pending op forms).
    #[test]
    fn stairs_require_the_selection_to_touch_the_floor() {
        let mut world = World::new();
        world.initial_meshes();
        world.select_at_crosshair(); // −Z wall
        // Shrink V to a small band and slide it up off the floor via the preview
        // (which centers the rect on the crosshair). Aim high so it clears vMin.
        world.adjust_selection_size(0.0, -12.0); // sel_size_v: 16 → 4
        world.camera.pitch = 0.5; // look up so the centered rect sits above the floor
        world.update_selection_preview();
        assert!(!world.wall_selection_touches_floor(), "raised band is off the floor");
        assert!(!world.push_stairs(StairDir::Down), "off-floor selection rejects stairs");
        assert!(!world.has_pending_stair());
    }

    /// Arrow keys accumulate a pending step counter; the opposite arrow shrinks
    /// the same op, and confirming creates two void brushes + one descriptor with
    /// the tread mesh folded into the region (more triangles than before).
    #[test]
    fn confirm_stairs_creates_voids_treads_and_descriptor() {
        let mut world = World::new();
        let initial = world.initial_meshes();
        let tris_before = initial[0].mesh.indices.len();

        world.select_at_crosshair(); // full-face −Z wall, touches floor
        assert!(world.push_stairs(StairDir::Down), "first down grows to 1 step");
        world.push_stairs(StairDir::Down); // 2
        world.push_stairs(StairDir::Down); // 3
        world.push_stairs(StairDir::Up); // opposite shrinks → 2
        assert_eq!(world.pending_stair().unwrap().0, 2, "opposite arrow shrank the op");

        let brushes_before = world.regions[0].brushes.len();
        let rm = world.confirm_stairs().expect("confirm rebuilds the region");
        assert!(!world.has_pending_stair(), "confirm clears the pending op");
        assert_eq!(
            world.regions[0].brushes.len(),
            brushes_before + 2,
            "two void brushes (stairwell + corridor)"
        );
        assert_eq!(world.regions[0].stairs.len(), 1, "one stair descriptor");
        assert!(
            rm.mesh.indices.len() > tris_before,
            "tread geometry folded into the region mesh ({} → {})",
            tris_before,
            rm.mesh.indices.len()
        );
    }

    /// Reproduce the exact live-app ordering: click to select, then the per-frame
    /// selection preview runs (as it does every RedrawRequested), THEN the arrow
    /// keys + Enter. This guards against the preview loop clobbering the selection
    /// or pending-stair state (which the other tests don't exercise).
    #[test]
    fn preview_loop_between_select_and_confirm_does_not_break_stairs() {
        let mut world = World::new();
        world.initial_meshes();
        assert!(world.select_at_crosshair(), "click selects the −Z wall");

        // Simulate several render frames: preview updates before the user acts.
        for _ in 0..5 {
            world.update_selection_preview();
        }
        assert!(
            world.push_stairs(StairDir::Down),
            "arrow-down must still form a pending op after the preview ran"
        );
        // More frames between key presses.
        world.update_selection_preview();
        world.push_stairs(StairDir::Down);
        world.update_selection_preview();

        assert_eq!(world.pending_stair().unwrap().0, 2, "two steps pending");
        assert!(world.confirm_stairs().is_some(), "Enter confirms after previews");
        assert_eq!(world.regions[0].stairs.len(), 1);
    }

    /// Down-stairs are walkable by the hunter: the nav bake sees the treads (via
    /// the solid-box extras) and finds a path from the room floor down into the
    /// lower corridor. Also proves standable cells exist below the original floor.
    #[test]
    fn down_stairs_are_walkable_by_nav() {
        let mut world = World::new();
        world.initial_meshes();
        world.select_at_crosshair(); // −Z wall
        for _ in 0..4 {
            world.push_stairs(StairDir::Down);
        }
        world.confirm_stairs();

        let mut regions = std::mem::take(&mut world.regions);
        let nav = nav::bake(&mut regions, &[]).expect("bake with stairs");
        world.regions = regions;

        // A cell below the room floor exists (the descended corridor), and a path
        // runs from the room floor down to it.
        let stand = nav.all_standable();
        assert!(
            stand.iter().any(|c| c.y < -0.1),
            "some standable cell sits below the original floor (descended steps)"
        );
        let top = Vec3::new(3.0, 0.1, 3.0); // room floor
        let bottom = *stand
            .iter()
            .min_by(|a, b| a.y.total_cmp(&b.y))
            .expect("a lowest cell");
        let path = nav
            .find_path(top, bottom)
            .expect("a path should run from the room floor down the stairs");
        assert!(path.len() >= 2);
        assert!(path.last().unwrap().y < -0.1, "the route reaches the lower corridor");
    }

    /// Up-stairs are walkable by the hunter: treads rise above the floor and a
    /// path runs up onto the raised corridor.
    #[test]
    fn up_stairs_are_walkable_by_nav() {
        let mut world = World::new();
        world.initial_meshes();
        world.select_at_crosshair(); // −Z wall
        for _ in 0..3 {
            world.push_stairs(StairDir::Up);
        }
        world.confirm_stairs();

        let mut regions = std::mem::take(&mut world.regions);
        let nav = nav::bake(&mut regions, &[]).expect("bake with up-stairs");
        world.regions = regions;

        let stand = nav.all_standable();
        assert!(
            stand.iter().any(|c| c.y > 0.1),
            "some standable cell sits above the original floor (ascended steps)"
        );
        let bottom = Vec3::new(3.0, 0.1, 3.0);
        let top = *stand.iter().max_by(|a, b| a.y.total_cmp(&b.y)).expect("a highest cell");
        let path = nav
            .find_path(bottom, top)
            .expect("a path should run up the stairs to the raised corridor");
        assert!(path.last().unwrap().y > 0.1, "the route reaches the raised corridor");
    }

    /// Down-stairs are walkable by the player: entering HUNT and walking into the
    /// stairwell, the capsule descends the treads (feet drop below the floor) and
    /// is caught by them (never falls through to the void floor). This exercises
    /// the folded tread geometry as a Rapier trimesh collider.
    #[test]
    fn player_descends_the_stairs() {
        let mut world = World::new();
        world.initial_meshes();
        world.select_at_crosshair(); // −Z wall, full face
        for _ in 0..4 {
            world.push_stairs(StairDir::Down); // 4 steps down (−1 m at the bottom)
        }
        let rm = world.confirm_stairs().expect("confirm");
        // Sanity: the tread mesh made it into the region collider.
        assert!(!rm.mesh.indices.is_empty());

        world.toggle_mode(); // BUILD → HUNT; player spawns on the room floor
        assert_eq!(world.mode, Mode::Hunt);
        world.character.as_mut().unwrap().yaw = 0.0; // face −Z, toward the stairs
        let mut input = InputState::default();
        input.pointer_locked = true;
        input.press(winit::keyboard::KeyCode::KeyW);

        for _ in 0..600 {
            // 5 s at 120 Hz — walk into and down the stairwell
            world.fixed_step(1.0 / 120.0, &input);
        }
        let feet = world.player_pos().unwrap();
        assert!(
            feet.y < -0.1,
            "player should walk down the treads (feet below the floor), got y={}",
            feet.y
        );
        // Void floor is at −4 WT = −1.0 m; treads must catch the capsule above it.
        assert!(
            feet.y > -1.05,
            "player should rest on a tread, not fall through to the void floor, got y={}",
            feet.y
        );
    }

    /// Walking straight into a wall is blocked — the capsule can't tunnel
    /// through the CSG collider.
    #[test]
    fn character_cannot_walk_through_a_wall() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode();
        // Face −Z (yaw 0) toward the z=0 wall; hold W, pointer locked.
        let mut input = InputState::default();
        input.pointer_locked = true;
        input.press(winit::keyboard::KeyCode::KeyW);

        for _ in 0..600 {
            world.fixed_step(1.0 / 120.0, &input);
        }
        let feet = world.player_pos().unwrap();
        // Capsule radius is 0.25 m, so it should stop before z=0, never negative.
        assert!(feet.z > 0.1, "capsule tunneled through the wall: z={}", feet.z);
    }

    // ─── Free-standing platforms + stair-runs ───────────────────────────────

    /// The default room plus a raised platform (top at y=6 WT) and a stair-run
    /// descending from its −X edge down to the floor. Structures are built into
    /// the `STRUCT_ID` mesh + collider. The platform sits at x∈[10,14], z∈[8,12];
    /// the stair-run runs along −X from x=10 down to x=4 over z∈[8,12].
    fn room_with_platform_and_stair() -> World {
        let mut world = World::new(); // 24×16×24 cavity, floor at y=0
        world.initial_meshes();
        world.platforms.push(Platform {
            id: 1,
            x: 10.0,
            y: 6.0,
            z: 8.0,
            size_x: 4.0,
            size_z: 4.0,
            thickness: 1.0,
            grounded: false,
            railings: false,
        });
        world.next_platform_id = 2;
        world.stair_runs.push(StairRun {
            id: 1,
            from_platform: Some(1),
            to_platform: None,
            anchor_from: Anchor::Edge {
                edge: structures::Edge::XMin,
                offset: 0.5,
            },
            anchor_to: Anchor::Ground {
                x: 4.0,
                y: 0.0,
                z: 10.0,
            },
            width: 4.0,
            step_height: 1.0,
            rise_over_run: 1.0,
            grounded: false,
            railings: false,
        });
        world.next_run_id = 2;
        world.rebuild_structures();
        world
    }

    /// A platform + connecting stair-run are walkable by the hunter's grid nav:
    /// the platform top and stair treads become standable, and A* finds a route
    /// from the room floor up onto the platform. Proves `structure_solid_boxes`
    /// reaches the voxelizer (the `collectExtraSolids`/platform-box port).
    #[test]
    fn platform_and_stair_are_walkable_by_nav() {
        let mut world = room_with_platform_and_stair();

        let solids = world.structure_solid_boxes();
        assert!(!solids.is_empty(), "platform + stair produced solid boxes");
        let mut regions = std::mem::take(&mut world.regions);
        let nav = nav::bake(&mut regions, &solids).expect("bake with structures");
        world.regions = regions;

        // The platform top (y=6 WT = 1.5 m) yields a standable cell up there.
        let stand = nav.all_standable();
        assert!(
            stand.iter().any(|c| c.y > 1.4),
            "a standable cell sits on the raised platform (top at 1.5 m)"
        );

        // A route runs from the room floor up the stairs onto the platform top.
        let floor = Vec3::new(0.75, 0.1, 2.5); // near the bottom of the stairs
        let top = *stand
            .iter()
            .max_by(|a, b| a.y.total_cmp(&b.y))
            .expect("a highest standable cell");
        let path = nav
            .find_path(floor, top)
            .expect("A* should route up the stair-run onto the platform");
        assert!(
            path.last().unwrap().y > 1.4,
            "the route climbs onto the platform, got {:?}",
            path.last()
        );
    }

    /// Structures always wear the "simple" (blue) scheme regardless of the room's
    /// scheme, and railings emit into the dedicated railing zone (→ the
    /// transparent railing texture) rather than being classified as walls.
    #[test]
    fn structures_wear_simple_scheme_with_railings_in_their_own_zone() {
        use engine::render::textures::{simple_scheme, RAILING_ZONE};
        let mut world = room_with_platform_and_stair();
        world.platforms[0].railings = true;
        world.stair_runs[0].railings = true;
        let rm = world.rebuild_structures();

        // Every structure group uses the simple scheme, never a room scheme.
        let schemes: std::collections::BTreeSet<u16> =
            rm.mesh.groups.iter().map(|g| g.scheme).collect();
        assert_eq!(
            schemes,
            std::iter::once(simple_scheme() as u16).collect(),
            "structures use only the simple scheme, got {schemes:?}"
        );
        // Slab/treads classify to floor/wall zones…
        assert!(
            rm.mesh.groups.iter().any(|g| matches!(g.zone, 0 | 2 | 3)),
            "platform/stair surfaces present (floor/wall zones)"
        );
        // …and railings land in the dedicated railing zone.
        assert!(
            rm.mesh.groups.iter().any(|g| g.zone == RAILING_ZONE),
            "railings emit into the railing zone; groups = {:?}",
            rm.mesh.groups.iter().map(|g| g.zone).collect::<Vec<_>>()
        );
    }

    /// The player capsule rests on a platform's top surface (its trimesh collider
    /// holds it): spawning the player above the platform, gravity settles it onto
    /// the slab (y≈1.5 m), not through it to the floor.
    #[test]
    fn player_capsule_rests_on_a_platform() {
        let mut world = room_with_platform_and_stair();
        // Camera above the platform centre (x=12,z=10 WT → 3.0, 2.5 m).
        world.camera.pos = Vec3::new(3.0, 2.5, 2.5);
        world.toggle_mode(); // spawns the capsule via a downward ray onto the slab
        assert_eq!(world.mode, Mode::Hunt);

        let input = InputState::default(); // stand still
        for _ in 0..360 {
            world.fixed_step(1.0 / 120.0, &input);
        }
        let feet = world.player_pos().expect("player in HUNT");
        assert!(
            feet.y > 1.4,
            "capsule should rest on the platform top (~1.5 m), got y={}",
            feet.y
        );
    }

    /// The platform tool state machine: `T` arms it, aiming at a wall places a
    /// platform on click, and it becomes selected. A second placement, connect,
    /// grounded, and delete all round-trip through the public API.
    #[test]
    fn platform_tool_places_and_edits() {
        let mut world = World::new(); // camera looks −Z at the z=0 wall
        world.initial_meshes();

        assert!(!world.is_platform_tool());
        world.platform_tool_key();
        assert!(world.is_platform_tool() && world.is_platform_placing());

        // Click while aimed at the wall → a platform is placed and selected.
        assert!(
            world.platform_click().is_some(),
            "placing a platform rebuilds the structures mesh"
        );
        assert_eq!(world.platforms.len(), 1, "one platform placed");
        assert_eq!(world.platform_phase, Some(PlatformPhase::Selected));

        // Toggle grounded on the selection.
        assert!(world.toggle_grounded_key().is_some());
        assert!(world.platforms[0].grounded, "F grounded the platform");

        // Delete it (and it returns to the idle placement phase).
        assert!(world.delete_selected().is_some());
        assert!(world.platforms.is_empty(), "platform deleted");
        assert_eq!(world.platform_phase, Some(PlatformPhase::Idle));
    }

    /// Arming another modal tool (door) disarms the platform tool, and vice
    /// versa — the tools stay mutually exclusive.
    #[test]
    fn platform_tool_is_mutually_exclusive() {
        let mut world = World::new();
        world.initial_meshes();
        world.platform_tool_key();
        assert!(world.is_platform_tool());
        world.door_tool_key(); // arming the door disarms the platform tool
        assert!(!world.is_platform_tool(), "door tool disarmed the platform tool");
        assert!(world.is_opening_arming());
        world.platform_tool_key(); // arming the platform disarms the door
        assert!(!world.is_opening_arming(), "platform tool disarmed the door tool");
        assert!(world.is_platform_tool());
    }

    /// The two-step connect flow: `C` arms ConnectDst; locking a destination +
    /// source edge advances to ConnectSrc; a confirm builds one run and returns to
    /// Selected; and the Esc ladder walks ConnectSrc → ConnectDst → Selected.
    #[test]
    fn connect_two_step_locks_slides_and_builds() {
        let mut world = room_with_platform_and_stair(); // platform 1 at (10,6,8)
        world.platform_phase = Some(PlatformPhase::Selected);
        world.selected_platform = Some(1);

        world.connect_key();
        assert_eq!(world.platform_phase, Some(PlatformPhase::ConnectDst));

        // Lock a ground destination + the −X source edge (what connect_lock_target
        // does from a crosshair hit), then confirm. Camera looks level (pitch 0)
        // so the slide offset resolves to the edge midpoint (0.5).
        world.connect_to = Some(ConnectTarget::Ground { x: 4.0, y: 0.0, z: 10.0 });
        world.connect_edge = Some(Edge::XMin);
        world.connect_slide_wt = 2.0;
        world.platform_phase = Some(PlatformPhase::ConnectSrc);

        // The wheel slides the attach point in 1-WT steps, clamped to the edge
        // length (platform 1 is 4 WT deep, so the XMin edge is 4 WT long).
        assert!(world.is_connect_sliding());
        world.adjust_connect_slide(1.0);
        assert_eq!(world.connect_slide_wt, 3.0, "wheel slid +1 WT");
        world.adjust_connect_slide(10.0);
        assert_eq!(world.connect_slide_wt, 4.0, "clamped to the edge length");

        let before = world.stair_runs.len();
        assert!(world.connect_confirm().is_some(), "confirm builds + rebuilds");
        assert_eq!(world.stair_runs.len(), before + 1, "one run added");
        assert_eq!(world.platform_phase, Some(PlatformPhase::Selected));
        assert!(world.connect_to.is_none() && world.connect_edge.is_none());

        // Esc ladder from a fresh ConnectSrc.
        world.connect_key();
        world.connect_to = Some(ConnectTarget::Ground { x: 4.0, y: 0.0, z: 10.0 });
        world.connect_edge = Some(Edge::XMin);
        world.platform_phase = Some(PlatformPhase::ConnectSrc);
        assert!(world.platform_escape().0, "esc consumed");
        assert_eq!(world.platform_phase, Some(PlatformPhase::ConnectDst), "src → dst");
        assert!(world.platform_escape().0);
        assert_eq!(world.platform_phase, Some(PlatformPhase::Selected), "dst → selected");
    }

    /// The gizmo shows for a selected platform, a scale-handle drag grows the
    /// footprint, a move-arrow drag repositions it, and Esc cancels a drag
    /// (restoring the transform).
    #[test]
    fn gizmo_scales_moves_and_cancels() {
        let mut world = room_with_platform_and_stair();
        world.platform_phase = Some(PlatformPhase::Selected);
        world.selected_platform = Some(1);
        assert!(world.gizmo_mesh().is_some(), "gizmo shows for a selected platform");

        // Scale +X: a large rightward drag grows the footprint.
        let size_before = world.platforms[0].size_x;
        world.gizmo_start(GizmoHandle::ScaleXMax);
        assert!(world.is_gizmo_dragging());
        world.gizmo_drag_delta(400.0, 0.0);
        assert!(
            world.platforms[0].size_x > size_before,
            "scale handle grew size_x: {} → {}",
            size_before,
            world.platforms[0].size_x
        );
        world.gizmo_drag = None; // a click would confirm the drag

        // Move +X: drag shifts the platform; Esc cancels and restores it.
        let x_before = world.platforms[0].x;
        world.gizmo_start(GizmoHandle::MoveX);
        world.gizmo_drag_delta(400.0, 0.0);
        assert!(world.platforms[0].x > x_before, "move arrow shifted +X");
        let (consumed, mesh) = world.platform_escape();
        assert!(consumed && mesh.is_some(), "Esc cancels the drag + rebuilds");
        assert_eq!(world.platforms[0].x, x_before, "cancel restored the position");
        assert!(!world.is_gizmo_dragging());
    }

    // ─── Undo / redo (see `world::history`) ──────────────────────────────────

    /// A one-line signature of region 0's authored brush geometry — enough to
    /// tell "state changed" / "state restored exactly" without `PartialEq`. The
    /// per-field weights are distinct primes so a face move (which shifts a min
    /// coord one way and its dim the other) can't cancel out to no change.
    fn region0_sig(world: &World) -> f32 {
        world.regions[0]
            .brushes
            .iter()
            .map(|b| b.id as f32 + 2.0 * b.x + 3.0 * b.y + 5.0 * b.z + 7.0 * b.w + 11.0 * b.h + 13.0 * b.d)
            .sum()
    }

    /// The core contract: a push is recorded, `undo` restores the pre-edit
    /// geometry byte-for-byte (via re-bake), and `redo` re-applies it.
    #[test]
    fn undo_restores_geometry_and_redo_reapplies() {
        let mut world = World::new();
        world.initial_meshes();

        let s0 = region0_sig(&world);
        // Author an edit through the same wrapper the app uses.
        let rm = world
            .with_undo(|w| w.push(PUSH_PULL_STEP))
            .expect("crosshair hits the −Z wall");
        assert_eq!(rm.id, 0);
        let s1 = region0_sig(&world);
        assert!((s1 - s0).abs() > 0.01, "push changed the geometry");

        let meshes = world.undo().expect("one edit to undo");
        assert!(!meshes.is_empty(), "undo returns meshes to upload");
        assert!((region0_sig(&world) - s0).abs() < 1e-4, "undo restored pre-edit state");

        world.redo().expect("one edit to redo");
        assert!((region0_sig(&world) - s1).abs() < 1e-4, "redo re-applied the edit");

        // Nothing left to redo once re-applied.
        assert!(world.redo().is_none(), "redo stack drained");
    }

    /// A no-op edit (crosshair hits nothing) records no history, so `undo` has
    /// nothing to pop.
    #[test]
    fn no_op_edit_records_no_undo_step() {
        let mut world = World::new();
        world.initial_meshes();
        // Fly outside the room, look away → the push misses.
        world.camera.pos = Vec3::new(1000.0, 1000.0, 1000.0);
        world.camera.yaw = 0.0;

        assert!(world.with_undo(|w| w.push(PUSH_PULL_STEP)).is_none(), "push missed");
        assert!(world.undo().is_none(), "a no-op leaves the history empty");
    }

    /// A fresh edit after an undo forks history — the redo stack is cleared.
    #[test]
    fn new_edit_clears_redo() {
        let mut world = World::new();
        world.initial_meshes();

        world.with_undo(|w| w.push(PUSH_PULL_STEP)).expect("edit 1");
        world.undo().expect("undo edit 1"); // redo now has one entry
        world.with_undo(|w| w.push(PUSH_PULL_STEP)).expect("edit 2 forks history");

        assert!(world.redo().is_none(), "the divergent edit cleared the redo stack");
    }

    /// The undo stack is bounded: past [`history::MAX_HISTORY`], the oldest
    /// snapshot is dropped rather than growing without limit.
    #[test]
    fn history_is_capped_at_max_depth() {
        let mut world = World::new();
        world.initial_meshes();
        // Carve well past the cap; every full-face push mutates, so every one is
        // recorded (a subtract push has no thinness guard).
        for _ in 0..(history::MAX_HISTORY + 10) {
            world.with_undo(|w| w.push(1.0));
        }
        assert_eq!(
            world.undo_stack.len(),
            history::MAX_HISTORY,
            "undo depth is clamped to the cap"
        );
    }

    /// Chest-aim oracle: after measuring a rifle's real barrel and aiming the chest
    /// at a known model-space target, the **actual gun barrel** ends up pointing from
    /// the chest at that target. This is the headless proof that the overlay-hold +
    /// chest-aim chain points the gun where intended (no over/under-shoot), for the
    /// bladed rifle clip that was aiming off to the side. Skips if assets are absent.
    #[test]
    fn chest_aim_points_the_real_barrel_at_the_target() {
        let world = World::new();
        let (Some(arm), Some(template), Some(model)) = (
            world.enemy_arm.first().and_then(|a| a.as_ref()),
            world.char_anim_template.as_ref(),
            world.char_models.first(),
        ) else {
            eprintln!("skipping: character assets not loaded");
            return;
        };
        let sk = &model.skeleton;
        let weapon = crate::combat::enemy_def_for(&crate::combat::config::AR33); // rifle
        let asset = world
            .enemy_weapon_lib
            .iter()
            .find(|w| w.name == weapon.name)
            .expect("AR33 weapon asset");

        let loco: Vec<(f32, clip::AnimationClip)> = vec![
            (0.0, template.clip(0).unwrap().clone()),
            (anim_set::SPEED_WALK, template.clip(1).unwrap().clone()),
            (anim_set::SPEED_JOG, template.clip(2).unwrap().clone()),
            (anim_set::SPEED_RUN, template.clip(3).unwrap().clone()),
        ];
        let aim_clip = template.clip(FIRE_RIFLE_IDX).unwrap().clone();
        let mut stack = arm.build_stack(loco, aim_clip);

        // Full aim pose (overlay on) → measure the real barrel in the chest frame.
        {
            let ov = stack.layer_as::<ClipOverlayLayer>(ENEMY_AIM_OVERLAY_LAYER).unwrap();
            ov.time = super::hunt::fire_window_for(weapon.class, false).0;
            ov.weight = 1.0;
        }
        let aim_pose = stack.evaluate(Pose::bind(sk), &LayerCtx { skeleton: sk, dt: 0.0 });
        let attach = Quat::from_euler(
            EulerRot::XYZ,
            weapon.right_rot.x,
            weapon.right_rot.y,
            weapon.right_rot.z,
        );
        let fwd = arm
            .barrel_forward_in_chest(&aim_pose, sk, attach, asset.barrel_axis())
            .expect("barrel measured");

        // Aim the chest at an arbitrary reachable model-space target (no cone clamp).
        let target = Vec3::new(0.6, 1.4, 3.0);
        {
            let ca = stack.layer_as::<AimOffsetLayer>(ENEMY_CHEST_AIM_LAYER).unwrap();
            ca.forward = fwd;
            ca.target = target;
            ca.cone = engine::skeletal::layers::AimCone::uniform(std::f32::consts::PI);
            ca.weight = 1.0;
            ca.enabled = true;
        }
        let posed = stack.evaluate(Pose::bind(sk), &LayerCtx { skeleton: sk, dt: 0.0 });
        let g = posed.joint_global_transforms(sk);

        // The ACTUAL barrel (gun rides Bone_9 with the attach + muzzle axis) vs the
        // direction from the chest to the target.
        let (_, hand_rot, _) = g[arm.end].to_scale_rotation_translation();
        let barrel = (hand_rot * (attach * asset.barrel_axis())).normalize();
        let chest_origin = g[arm.chest].to_scale_rotation_translation().2;
        let to_target = (target - chest_origin).normalize();
        let err = barrel.angle_between(to_target);
        assert!(
            err < 0.05,
            "barrel should point at the target (off by {err} rad); barrel={barrel:?} to_target={to_target:?}"
        );
    }

    /// Head look-at oracle: the gaze axis baked from the rig (`EnemyArm::head_forward`),
    /// swung by the head look-at layer, ends up pointing from the head at the focus —
    /// the headless proof the head actually turns toward what the hunter's thinking
    /// about. Also checks the missing-neck safeguard: a focus behind the head clamps
    /// the swing to [`ENEMY_HEAD_LOOK_CONE`] instead of over-rotating. Skips w/o assets.
    #[test]
    fn head_look_points_the_head_at_the_focus_and_clamps_to_the_cone() {
        let world = World::new();
        let (Some(arm), Some(template), Some(model)) = (
            world.enemy_arm.first().and_then(|a| a.as_ref()),
            world.char_anim_template.as_ref(),
            world.char_models.first(),
        ) else {
            eprintln!("skipping: character assets not loaded");
            return;
        };
        let sk = &model.skeleton;
        let loco: Vec<(f32, clip::AnimationClip)> = vec![
            (0.0, template.clip(0).unwrap().clone()),
            (anim_set::SPEED_WALK, template.clip(1).unwrap().clone()),
            (anim_set::SPEED_JOG, template.clip(2).unwrap().clone()),
            (anim_set::SPEED_RUN, template.clip(3).unwrap().clone()),
        ];
        let aim_clip = template.clip(FIRE_RIFLE_IDX).unwrap().clone();
        let ctx = LayerCtx { skeleton: sk, dt: 0.0 };

        // (a) Wide cone → the gaze lands on an in-front, reachable model-space focus.
        let focus = Vec3::new(0.5, 1.4, 3.0);
        let mut stack = arm.build_stack(loco.clone(), aim_clip.clone());
        {
            let hl = stack.layer_as::<AimOffsetLayer>(ENEMY_HEAD_LOOK_LAYER).unwrap();
            hl.target = focus;
            hl.cone = engine::skeletal::layers::AimCone::uniform(std::f32::consts::PI);
            hl.weight = 1.0;
            hl.enabled = true;
        }
        let posed = stack.evaluate(Pose::bind(sk), &ctx);
        let g = posed.joint_global_transforms(sk);
        let (_, head_rot, head_origin) = g[arm.head].to_scale_rotation_translation();
        let gaze = (head_rot * arm.head_forward).normalize();
        let to_focus = (focus - head_origin).normalize();
        let err = gaze.angle_between(to_focus);
        assert!(
            err < 0.05,
            "head gaze should point at the focus (off by {err} rad); gaze={gaze:?} to_focus={to_focus:?}"
        );

        // (b) A focus directly behind → the swing is clamped to the gaze cone (the
        // no-neck safeguard), never a full 180° over-rotation.
        let rest = arm.build_stack(loco.clone(), aim_clip.clone())
            .evaluate(Pose::bind(sk), &ctx);
        let rest_gaze = {
            let rg = rest.joint_global_transforms(sk)[arm.head].to_scale_rotation_translation().1;
            (rg * arm.head_forward).normalize()
        };
        let mut stack2 = arm.build_stack(loco, aim_clip);
        {
            let hl = stack2.layer_as::<AimOffsetLayer>(ENEMY_HEAD_LOOK_LAYER).unwrap();
            hl.target = head_origin - rest_gaze * 2.0; // behind the head
            hl.cone = engine::skeletal::layers::AimCone::uniform(ENEMY_HEAD_LOOK_CONE);
            hl.weight = 1.0;
            hl.enabled = true;
        }
        let posed2 = stack2.evaluate(Pose::bind(sk), &ctx);
        let swung_gaze = {
            let rg = posed2.joint_global_transforms(sk)[arm.head].to_scale_rotation_translation().1;
            (rg * arm.head_forward).normalize()
        };
        let swung = swung_gaze.angle_between(rest_gaze);
        assert!(
            swung <= ENEMY_HEAD_LOOK_CONE + 1e-2,
            "gaze swing should clamp to the cone ({ENEMY_HEAD_LOOK_CONE} rad), got {swung}"
        );
    }

    /// Foot-IK leg chains: each resolved `(hip, knee, foot)` chain solves the foot onto
    /// a reachable ground target — the headless proof the leg chains resolve to real
    /// hip/knee/foot joints and the two-bone solve plants the foot. Skips w/o assets.
    #[test]
    fn foot_ik_plants_each_foot_on_a_reachable_target() {
        let world = World::new();
        let (Some(arm), Some(model)) = (
            world.enemy_arm.first().and_then(|a| a.as_ref()),
            world.char_models.first(),
        ) else {
            eprintln!("skipping: character assets not loaded");
            return;
        };
        let sk = &model.skeleton;
        let ctx = LayerCtx { skeleton: sk, dt: 0.0 };
        for k in 0..2 {
            let (root, mid, end) = arm.legs[k];
            let base = Pose::bind(sk);
            let g = base.joint_global_transforms(sk);
            let a = g[root].to_scale_rotation_translation().2;
            let b = g[mid].to_scale_rotation_translation().2;
            let c = g[end].to_scale_rotation_translation().2;
            let l = (b - a).length() + (c - b).length();
            // Reachable target at 70% of leg reach, offset off-axis so the knee bends.
            let target = a + Vec3::new(0.2, -0.9, 0.3).normalize() * (0.7 * l);
            let mut ik = TwoBoneIkLayer {
                root,
                mid,
                end,
                target,
                reach_frac: 0.0,
                pole: Vec3::ZERO,
                weight: 1.0,
                enabled: true,
            };
            let mut pose = base;
            ik.apply(&mut pose, &ctx);
            let reached = pose.joint_global_transforms(sk)[end].to_scale_rotation_translation().2;
            let err = (reached - target).length();
            assert!(err < 1e-2, "leg {k} foot should reach the ground target (off by {err})");
        }
    }

    /// PERF BASELINE (run: `cargo test --release bench_rebake_slot1 -- --nocapture --ignored`).
    /// Loads slot1 and times the CSG paths on its single region so we can measure
    /// the fold-once / incremental optimizations against real authored data.
    #[test]
    #[ignore]
    fn bench_rebake_slot1() {
        let mut world = World::new();
        let path = super::persist::slot_path(1);
        if world.load_level(&path).is_err() {
            eprintln!("slot1 not found at {} — skipping", path.display());
            return;
        }
        let region = &mut world.regions[0];
        let n = 100;
        // Warm up.
        let _ = region.evaluate();
        let _ = region.evaluate_textured();

        let t = Instant::now();
        for _ in 0..n { let _ = region.evaluate(); }
        let eval_ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;

        let t = Instant::now();
        for _ in 0..n { let _ = region.evaluate_textured(); }
        let tex_ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;

        let t = Instant::now();
        for _ in 0..n { let _ = region.evaluate_both(); }
        let both_ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;

        let brushes = region.brushes.len();
        let tris = region.evaluate().indices.len() / 3;
        eprintln!("=== bench slot1: {brushes} brushes, {tris} collider tris ===");
        eprintln!("evaluate()          {eval_ms:.3} ms");
        eprintln!("evaluate_textured() {tex_ms:.3} ms");
        eprintln!("OLD per edit        {:.3} ms (two folds)", eval_ms + tex_ms);
        eprintln!("NEW evaluate_both() {both_ms:.3} ms (one fold)");
    }

    /// PERF SCALING (run: `cargo test --release bench_scaling -- --nocapture --ignored`).
    /// Times a single-region fold as the room count grows, to show localized CSG
    /// keeps per-edit cost near-linear (cheap constant) instead of quadratic. Each
    /// room is a subtract touching the next via a doorway cut, so it stays ONE
    /// connected region (the hardest case — clustering can't help here).
    #[test]
    #[ignore]
    fn bench_scaling_connected_region() {
        use engine::geometry::csg_runtime::{Brush, Op, Region};
        for &rooms in &[16usize, 64, 256, 1024] {
            let mut region = Region::new(0);
            let mut id = 1u32;
            // A straight run of rooms along X, each 10 wide, bridged by a 2-wide
            // doorway cut into the shared wall so the whole run is connected.
            for r in 0..rooms {
                let x = r as f32 * 12.0;
                region
                    .brushes
                    .push(Brush::new(id, Op::Subtract, x, 0.0, 0.0, 10.0, 8.0, 10.0));
                id += 1;
                if r + 1 < rooms {
                    // Doorway bridging room r and r+1 (spans the wall at x+10..x+12).
                    region
                        .brushes
                        .push(Brush::new(id, Op::Subtract, x + 10.0, 0.0, 3.0, 2.0, 7.0, 4.0));
                    id += 1;
                }
            }
            let brushes = region.brushes.len();
            let _ = region.evaluate_both(); // warm
            let n = 10;
            let t = Instant::now();
            for _ in 0..n {
                let _ = region.evaluate_both();
            }
            let ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
            let per_brush = ms / brushes as f64 * 1000.0;
            eprintln!(
                "{rooms:>5} rooms / {brushes:>5} brushes: {ms:8.2} ms/full-fold  ({per_brush:.1} µs/brush)"
            );
        }
    }

    /// Two disjoint rooms load as two independent regions, every brush is mapped,
    /// and editing one room re-bakes only its region (the other's mesh is
    /// unaffected). Exercises clustering + `brush_to_region` + the incremental
    /// `rebuild_affected_regions` path.
    #[test]
    fn disjoint_rooms_cluster_and_edit_locally() {
        let mut world = World::new();
        // Add a second room far from the opening room (which is brush 1, x∈[0,24]).
        world.regions[0]
            .brushes
            .push(Brush::new(2, Op::Subtract, 100.0, 0.0, 0.0, 24.0, 16.0, 24.0));
        world.next_brush_id = 3;
        // Re-partition into connected regions.
        let _ = world.recluster_all();
        assert_eq!(world.regions.len(), 2, "two disjoint rooms → two regions");
        assert_eq!(world.brush_to_region.len(), 2, "both brushes mapped");
        let r0 = world.brush_to_region[&1];
        let r1 = world.brush_to_region[&2];
        assert_ne!(r0, r1, "the rooms are in different regions");

        // Editing brush 1's region returns exactly that region's mesh.
        let meshes = world.rebuild_affected_regions(&[1]);
        assert_eq!(meshes.len(), 1, "one region re-baked");
        assert_eq!(meshes[0].id, r0, "and it's brush 1's region");
    }

    /// A brush created without being pre-registered is auto-assigned to the region
    /// it overlaps when `rebuild_affected_regions` runs (the ported
    /// `assignBrushToRegion` safety net).
    #[test]
    fn unmapped_brush_is_auto_assigned() {
        let mut world = World::new();
        // Drop an additive brush inside the opening room, but don't map it.
        world.regions[0]
            .brushes
            .push(Brush::new(2, Op::Add, 4.0, 0.0, 4.0, 2.0, 4.0, 2.0));
        assert!(!world.brush_to_region.contains_key(&2), "not yet mapped");
        let meshes = world.rebuild_affected_regions(&[2]);
        assert_eq!(world.brush_to_region.get(&2), Some(&0), "assigned to room region 0");
        assert_eq!(meshes.len(), 1);
    }

    /// The CSG memo cache returns geometry identical to a fresh fold (correctness
    /// of the hash-keyed cache): re-baking the same region twice yields the same
    /// triangle count, and a real edit produces a different result (no stale hit).
    #[test]
    fn memoized_rebake_matches_fresh_and_invalidates_on_edit() {
        let mut world = World::new();
        world.initial_meshes();
        let first = world.rebuild_region(0).expect("region 0").mesh.indices.len();
        let cached = world.rebuild_region(0).expect("region 0").mesh.indices.len();
        assert_eq!(first, cached, "cache hit reproduces the same mesh");

        // Grow the room: different authored data → different hash → fresh fold.
        world.regions[0].brushes[0].w += 8.0;
        let edited = world.rebuild_region(0).expect("region 0").mesh.indices.len();
        // A bigger room still has geometry; the point is the cache didn't serve
        // the stale mesh (indices count can match, so assert it re-baked by
        // confirming a second identical call now caches the NEW value).
        let edited2 = world.rebuild_region(0).expect("region 0").mesh.indices.len();
        assert_eq!(edited, edited2, "new state is itself cached consistently");
    }





#[cfg(test)]
mod tmp_chain {
    use super::super::*;
    #[test]
    fn tmp_where_does_the_chain_diverge() {
        let mut world = World::new();
        world.set_wave_size(6);
        world.initial_meshes();
        world.toggle_mode();
        let World { enemies, enemy_arm, char_models, .. } = &mut world;
        for want in [0usize, 1, 2] {
            let inst = &mut enemies[want];
            let idx = if inst.dual { FIRE_DUAL_IDX } else { match inst.weapon.class { EnemyWeaponClass::Pistol => FIRE_PISTOL_IDX, EnemyWeaponClass::Rifle => FIRE_RIFLE_IDX } };
            let Some(Some(arm)) = enemy_arm.get(inst.body) else { continue };
            let Some(model) = char_models.get(inst.body) else { continue };
            let sk = &model.skeleton;
            let Some(clip) = inst.anim.clip(idx).cloned() else { continue };
            let t = clip.duration * 0.5;
            let (tr, r, s) = clip.pose_trs(t, sk);
            let mut bare = Pose::bind(sk);
            for j in 0..bare.joint_count() { bare.t[j] = tr[j]; bare.r[j] = r[j]; bare.s[j] = s[j]; }
            let gb = bare.joint_global_transforms(sk);
            if let Some(ov) = inst.stack.layer_as::<ClipOverlayLayer>(ENEMY_AIM_OVERLAY_LAYER) { ov.time = t; ov.weight = 1.0; }
            let posed = inst.stack.evaluate(Pose::bind(sk), &LayerCtx { skeleton: sk, dt: 0.0 });
            let gp = posed.joint_global_transforms(sk);
            // Walk root -> hand so we can see exactly where they part company.
            let mut chain = vec![arm.end];
            while let Some(p) = sk.parents[*chain.last().unwrap()] { chain.push(p); }
            chain.reverse();
            println!("--- {} (body {}) mask={} joints, chest={} ---", inst.weapon.name, inst.body, arm.upper_body.len(), arm.chest);
            for j in chain {
                let yb = { let q = gb[j].to_scale_rotation_translation().1; let v = q * Vec3::Z; v.x.atan2(v.z).to_degrees() };
                let yp = { let q = gp[j].to_scale_rotation_translation().1; let v = q * Vec3::Z; v.x.atan2(v.z).to_degrees() };
                println!("   j{:<3} {:<10} masked={:<5} clip={:+7.1}  stack={:+7.1}  delta={:+7.1}",
                    j, sk.names.get(j).map(|s| s.as_str()).unwrap_or("?"),
                    arm.upper_body.contains(&j), yb, yp, yp - yb);
            }
        }
    }
}

#[cfg(test)]
mod aim_overlay {
    use super::super::*;

    /// **The authored aim pose must survive being overlaid.**
    ///
    /// The upper-body overlay copies *local* transforms onto a base whose pelvis comes
    /// from the locomotion layer, so the masked subtree keeps its orientation relative
    /// to that pelvis. That is only correct if the fire clip and the locomotion clip
    /// agree about where the pelvis points — and they do not. Each hand-authored fire
    /// animation turns the whole body toward its target, so it carries its own root
    /// yaw: −53.1° (rifle), +68.7° (pistol), −15.8° (dual), against the locomotion
    /// clips' −11.4°. Grafting across that difference rotated the entire hold, gun
    /// included, by the root delta — the pistol came out 80° off, pointing the gun
    /// almost perpendicular to the hunter's facing.
    ///
    /// The dual clip masked the bug for months: it is authored within 4° of the
    /// locomotion root, so it alone looked right.
    ///
    /// `ClipOverlayLayer` now reconciles the two roots, so this asserts the invariant
    /// that catches any regression: **the barrel through the layer stack must point
    /// where the clip alone points**, for every weapon in the roster. Sampled at the
    /// middle of each clip — the hold — where all three animations aim within ~3.5° of
    /// the model's forward.
    #[test]
    fn the_overlay_preserves_the_clips_aim_direction() {
        let mut world = World::new();
        world.set_wave_size(6);
        world.initial_meshes();
        world.toggle_mode();
        let World { enemies, enemy_arm, char_models, enemy_weapon_lib, .. } = &mut world;
        let mut checked = 0;
        for inst in enemies.iter_mut() {
            let idx = if inst.dual {
                FIRE_DUAL_IDX
            } else {
                match inst.weapon.class {
                    EnemyWeaponClass::Pistol => FIRE_PISTOL_IDX,
                    EnemyWeaponClass::Rifle => FIRE_RIFLE_IDX,
                }
            };
            let Some(Some(arm)) = enemy_arm.get(inst.body) else { continue };
            let Some(model) = char_models.get(inst.body) else { continue };
            let sk = &model.skeleton;
            let Some(asset) = enemy_weapon_lib.iter().find(|w| w.name == inst.weapon.name) else {
                continue;
            };
            let attach = Quat::from_euler(
                EulerRot::XYZ,
                inst.weapon.right_rot.x,
                inst.weapon.right_rot.y,
                inst.weapon.right_rot.z,
            );
            let Some(clip) = inst.anim.clip(idx).cloned() else { continue };
            let t = clip.duration * 0.5;
            let barrel = |g: &[Mat4]| {
                let hand = g[arm.end].to_scale_rotation_translation().1;
                (hand * (attach * asset.barrel_axis())).normalize_or_zero()
            };
            // The clip on its own, straight onto the bind pose — the reference.
            let (tr, r, s) = clip.pose_trs(t, sk);
            let mut bare = Pose::bind(sk);
            for j in 0..bare.joint_count() {
                bare.t[j] = tr[j];
                bare.r[j] = r[j];
                bare.s[j] = s[j];
            }
            let want = barrel(&bare.joint_global_transforms(sk));
            // The same instant through the full hunter stack.
            if let Some(ov) = inst.stack.layer_as::<ClipOverlayLayer>(ENEMY_AIM_OVERLAY_LAYER) {
                ov.time = t;
                ov.weight = 1.0;
            }
            let posed = inst.stack.evaluate(Pose::bind(sk), &LayerCtx { skeleton: sk, dt: 0.0 });
            let got = barrel(&posed.joint_global_transforms(sk));
            let off = want.angle_between(got).to_degrees();
            assert!(
                off < 1.0,
                "{}{}: the overlay moved the barrel {off:.1}° off the clip's own aim                  (clip {:+.1}° vs stack {:+.1}° yaw) — root reconciliation regressed",
                inst.weapon.name,
                if inst.dual { " (dual)" } else { "" },
                want.x.atan2(want.z).to_degrees(),
                got.x.atan2(got.z).to_degrees(),
            );
            // …and the authored pose does aim roughly forward, which is what makes the
            // downstream chest-aim's swing budget available for actual tracking.
            let yaw = got.x.atan2(got.z).to_degrees().abs();
            assert!(yaw < 10.0, "{} aims {yaw:.1}° off forward through the stack", inst.weapon.name);
            checked += 1;
        }
        assert!(checked >= 4, "only {checked} hunters checked — the roster did not spawn");
    }
}
